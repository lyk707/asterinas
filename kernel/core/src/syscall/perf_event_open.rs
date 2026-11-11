// SPDX-License-Identifier: MPL-2.0

//! System call handler for `perf_event_open(2)`.
//!
//! Implements the counting subset of the Linux `perf_event_open` ABI for
//! hardware CPU cycles and retired instructions.
//!
//! Reference: <https://man7.org/linux/man-pages/man2/perf_event_open.2.html>.

#[cfg(feature = "pmu")]
use ostd::{const_assert, mm::VmIo};

use super::SyscallReturn;
#[cfg(feature = "pmu")]
use crate::fs::file::file_table::FdFlags;
use crate::prelude::*;

#[cfg(feature = "pmu")]
const PERF_TYPE_HARDWARE: u32 = 0;
#[cfg(feature = "pmu")]
const PERF_COUNT_HW_CPU_CYCLES: u64 = 0;
#[cfg(feature = "pmu")]
const PERF_COUNT_HW_INSTRUCTIONS: u64 = 1;
#[cfg(feature = "pmu")]
const PERF_ATTR_SIZE_VER0: u32 = 64;
#[cfg(feature = "pmu")]
const PERF_FLAG_FD_CLOEXEC: u64 = 1 << 3;

#[cfg(feature = "pmu")]
use crate::events::perf::attributes::{
    self, ATTR_DISABLED, ATTR_ENABLE_ON_EXEC, ATTR_EXCLUDE_KERNEL, ATTR_EXCLUDE_USER,
};

#[cfg(feature = "pmu")]
const SUPPORTED_READ_FORMAT_MASK: u64 = 0x07;

/// Opens a hardware performance counting event on a target thread context.
#[cfg(feature = "pmu")]
pub(super) fn sys_perf_event_open(
    attr_addr: Vaddr,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
    ctx: &Context,
) -> Result<SyscallReturn> {
    use ostd::{
        cpu::{self, CpuId},
        pmu::{self, CountingScope, Event, EventConfig},
    };

    use crate::{
        events::perf::{EventSpec, PerfEventFile},
        process::{
            credentials::capabilities::CapSet,
            pid_table,
            posix_thread::{AsPosixThread, alien_access::AlienAccessMode},
        },
    };

    if flags & !PERF_FLAG_FD_CLOEXEC != 0 || pid < -1 || cpu < -1 || group_fd < -1 {
        return_errno_with_message!(Errno::EINVAL, "invalid perf_event_open arguments");
    }
    if pid == -1 && cpu == -1 {
        return_errno_with_message!(
            Errno::EINVAL,
            "system-wide perf_event_open requires a valid CPU"
        );
    }
    if pid == -1 || group_fd != -1 {
        return_errno_with_message!(
            Errno::EOPNOTSUPP,
            "system-wide events and event groups are unsupported"
        );
    }

    let target_cpu =
        if cpu == -1 {
            None
        } else {
            Some(CpuId::try_from(cpu as usize).map_err(|_| {
                Error::with_message(Errno::EINVAL, "PMU CPU number is out of range")
            })?)
        };

    let attr = copy_attr(attr_addr, ctx)?;

    if attr.type_ != PERF_TYPE_HARDWARE
        || (attr.config != PERF_COUNT_HW_CPU_CYCLES && attr.config != PERF_COUNT_HW_INSTRUCTIONS)
    {
        return_errno_with_message!(
            Errno::EOPNOTSUPP,
            "only hardware cycles and instructions are supported"
        );
    }

    // Inheritance is not implemented. Accepting it with enable_on_exec would
    // silently lose counts from descendants; standard perf must use --no-inherit.
    if !attributes::supported_flags(attr.flags)
        || !attributes::supported_sampling(attr.sample_period, attr.sample_type)
        || attr.read_format & !SUPPORTED_READ_FORMAT_MASK != 0
        || attr.wakeup_events != 0
        || attr.bp_type != 0
        || attr.config1 != 0
    {
        return_errno_with_message!(Errno::EOPNOTSUPP, "unsupported perf counting attributes");
    }

    let event = if attr.config == PERF_COUNT_HW_CPU_CYCLES {
        Event::CpuCycles
    } else {
        Event::Instructions
    };
    let is_user_counted = attr.flags & ATTR_EXCLUDE_USER == 0;
    let is_kernel_counted = attr.flags & ATTR_EXCLUDE_KERNEL == 0;
    let scope = match (is_user_counted, is_kernel_counted) {
        (true, false) => CountingScope::User,
        (false, true) => CountingScope::Kernel,
        (true, true) => CountingScope::UserAndKernel,
        (false, false) => {
            return_errno_with_message!(
                Errno::EINVAL,
                "all requested privilege levels are excluded"
            );
        }
    };
    let config = EventConfig::new(event, scope);

    let effective_caps = ctx.posix_thread.credentials().effective_capset();
    if !effective_caps.intersects(CapSet::PERFMON | CapSet::SYS_ADMIN) {
        return_errno_with_message!(Errno::EPERM, "CAP_PERFMON or CAP_SYS_ADMIN is required");
    }

    let (target_context, target_tid) = if pid == 0 {
        (ctx.thread.perf().clone(), ctx.posix_thread.tid())
    } else {
        let thread = pid_table::pid_table_mut()
            .get_thread(pid as u32)
            .ok_or_else(|| {
                Error::with_message(Errno::ESRCH, "the PMU target thread does not exist")
            })?;
        // CAP_PERFMON authorizes hardware monitoring, while alien-access hooks
        // independently authorize access to this target. This intentionally
        // applies a stricter cross-process policy than Linux's CAP_PERFMON bypass.
        thread
            .as_posix_thread()
            .ok_or_else(|| {
                Error::with_message(Errno::ESRCH, "the PMU target is not a POSIX thread")
            })?
            .check_alien_access_from(ctx.posix_thread, AlienAccessMode::READ_WITH_REAL_CREDS)?;
        (thread.perf().clone(), pid as u32)
    };

    for candidate_cpu in
        cpu::all_cpus().filter(|candidate| target_cpu.is_none_or(|c| c == *candidate))
    {
        pmu::capabilities(candidate_cpu).map_err(|err| match err {
            pmu::PmuError::NoCounter => {
                Error::with_message(Errno::EBUSY, "PMU counter is occupied")
            }
            pmu::PmuError::HardwareFault => {
                Error::with_message(Errno::EIO, "PMU probe register access failed")
            }
            _ => Error::with_message(
                Errno::EOPNOTSUPP,
                "PMU or precise filtering unavailable on a target CPU",
            ),
        })?;
    }

    let spec = EventSpec {
        config,
        cpu: target_cpu,
        is_initially_disabled: attr.flags & ATTR_DISABLED != 0,
        should_enable_on_exec: attr.flags & ATTR_ENABLE_ON_EXEC != 0,
    };
    let event_file = Arc::new(PerfEventFile::new(
        target_context,
        target_tid,
        spec,
        attr.read_format,
    )?);

    let fd_flags = if flags & PERF_FLAG_FD_CLOEXEC != 0 {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };
    let file_table = ctx.thread_local.borrow_file_table();
    let fd = file_table.unwrap().write().insert(event_file, fd_flags);
    Ok(SyscallReturn::Return(fd.into()))
}

/// Returns `EOPNOTSUPP` when the kernel is built without the `pmu` feature.
#[cfg(not(feature = "pmu"))]
pub(super) fn sys_perf_event_open(
    _attr_addr: Vaddr,
    _pid: i32,
    _cpu: i32,
    _group_fd: i32,
    _flags: u64,
    _ctx: &Context,
) -> Result<SyscallReturn> {
    return_errno_with_message!(Errno::EOPNOTSUPP, "hardware PMU counting is not enabled");
}

// ---------------------------------------------------------------------------
// User-space `perf_event_attr` copying and validation
// ---------------------------------------------------------------------------

// Reference: Linux `include/uapi/linux/perf_event.h` (`PERF_ATTR_SIZE_VER0`).
#[cfg(feature = "pmu")]
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
struct PerfEventAttrV0 {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
}

#[cfg(feature = "pmu")]
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
struct AttrHeader {
    type_: u32,
    size: u32,
}

#[cfg(feature = "pmu")]
const ATTR_FLAGS_OFFSET: usize = 40;
#[cfg(feature = "pmu")]
const ATTR_CONFIG1_OFFSET: usize = 56;

#[cfg(feature = "pmu")]
const_assert!(size_of::<PerfEventAttrV0>() == PERF_ATTR_SIZE_VER0 as usize);
#[cfg(feature = "pmu")]
const_assert!(core::mem::offset_of!(PerfEventAttrV0, flags) == ATTR_FLAGS_OFFSET);
#[cfg(feature = "pmu")]
const_assert!(core::mem::offset_of!(PerfEventAttrV0, config1) == ATTR_CONFIG1_OFFSET);

#[cfg(feature = "pmu")]
fn copy_attr(addr: Vaddr, ctx: &Context) -> Result<PerfEventAttrV0> {
    let user_space = ctx.user_space();
    let header: AttrHeader = user_space.read_val(addr)?;
    let size = if header.size == 0 {
        PERF_ATTR_SIZE_VER0 as usize
    } else {
        header.size as usize
    };

    if !(PERF_ATTR_SIZE_VER0 as usize..=PAGE_SIZE).contains(&size) {
        return report_attr_size(addr, ctx);
    }

    let attr: PerfEventAttrV0 = user_space.read_val(addr)?;
    if attr.size != header.size {
        return_errno_with_message!(Errno::EINVAL, "the attribute size changed while copying");
    }

    if size > PERF_ATTR_SIZE_VER0 as usize {
        let tail_addr = addr
            .checked_add(PERF_ATTR_SIZE_VER0 as usize)
            .ok_or_else(|| Error::with_message(Errno::EFAULT, "attribute address overflow"))?;
        let mut tail = vec![0u8; size - PERF_ATTR_SIZE_VER0 as usize];
        user_space.read_bytes(tail_addr, &mut tail)?;
        if tail.iter().any(|&byte| byte != 0) {
            return report_attr_size(addr, ctx);
        }
    }
    Ok(attr)
}

#[cfg(feature = "pmu")]
fn report_attr_size(addr: Vaddr, ctx: &Context) -> Result<PerfEventAttrV0> {
    let size_addr = addr
        .checked_add(core::mem::offset_of!(PerfEventAttrV0, size))
        .ok_or_else(|| Error::with_message(Errno::EFAULT, "attribute address overflow"))?;
    ctx.user_space()
        .write_val(size_addr, &PERF_ATTR_SIZE_VER0)?;
    return_errno_with_message!(
        Errno::E2BIG,
        "unsupported perf_event_attr size or extension"
    );
}
