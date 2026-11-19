// SPDX-License-Identifier: MPL-2.0

//! System call handler for `perf_event_open(2)`.
//!
//! Implements the counting subset of the Linux `perf_event_open` ABI for
//! hardware CPU cycles and retired instructions.
//!
//! Reference: <https://man7.org/linux/man-pages/man2/perf_event_open.2.html>.

use ostd::{
    const_assert,
    cpu::{self, CpuId},
    mm::VmIo,
    pmu::{self, CountingScope, Event, EventConfig},
};

mod attributes;

use self::attributes::{
    ATTR_DISABLED, ATTR_ENABLE_ON_EXEC, ATTR_EXCLUDE_KERNEL, ATTR_EXCLUDE_USER,
};
use super::SyscallReturn;
use crate::{
    events::perf::{EventSpec, PerfContext, PerfEventFile, ReadFormat, to_error},
    fs::file::file_table::{FdFlags, FileTableInsert},
    prelude::*,
    process::{
        credentials::capabilities::CapSet,
        pid_table,
        posix_thread::{AsPosixThread, alien_access::AlienAccessMode},
    },
};

const PERF_TYPE_HARDWARE: u32 = 0;
const PERF_COUNT_HW_CPU_CYCLES: u64 = 0;
const PERF_COUNT_HW_INSTRUCTIONS: u64 = 1;
const PERF_ATTR_SIZE_VER0: u32 = 64;
const PERF_FLAG_FD_CLOEXEC: u64 = 1 << 3;

/// Opens a hardware performance counting event on a target thread context.
pub(super) fn sys_perf_event_open(
    attr_addr: Vaddr,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
    ctx: &Context,
) -> Result<SyscallReturn> {
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

    let (spec, read_format) = copy_attr(attr_addr, ctx)?.parse_counting(target_cpu)?;
    let target = authorize_target(pid, ctx)?;
    check_cpu_capabilities(target_cpu)?;
    let event_file = Arc::new(target.open(spec, read_format)?);

    let fd_flags = if flags & PERF_FLAG_FD_CLOEXEC != 0 {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };
    let file_table = ctx.thread_local.borrow_file_table();
    let fd = file_table.unwrap().lock_and_insert(event_file, fd_flags)?;
    Ok(SyscallReturn::Return(fd.into()))
}

struct AuthorizedTarget {
    context: Arc<PerfContext>,
    tid: u32,
    access_epoch: u64,
}

impl AuthorizedTarget {
    fn open(self, spec: EventSpec, read_format: ReadFormat) -> Result<PerfEventFile> {
        PerfEventFile::new(self.context, self.tid, spec, self.access_epoch, read_format)
    }
}

fn authorize_target(pid: i32, ctx: &Context) -> Result<AuthorizedTarget> {
    let effective_caps = ctx.posix_thread.credentials().effective_capset();
    if !effective_caps.intersects(CapSet::PERFMON | CapSet::SYS_ADMIN) {
        return_errno_with_message!(Errno::EPERM, "CAP_PERFMON or CAP_SYS_ADMIN is required");
    }

    if pid == 0 {
        return Ok(AuthorizedTarget {
            context: ctx.thread.perf().clone(),
            tid: ctx.posix_thread.tid(),
            access_epoch: ctx.thread.perf().access_epoch(),
        });
    }
    let thread = pid_table::pid_table_mut()
        .get_thread(pid as u32)
        .ok_or_else(|| Error::with_message(Errno::ESRCH, "the PMU target thread does not exist"))?;
    let target = thread
        .as_posix_thread()
        .ok_or_else(|| Error::with_message(Errno::ESRCH, "the PMU target is not a POSIX thread"))?;
    let access_epoch = thread.perf().access_epoch();
    if !Weak::ptr_eq(target.weak_process(), ctx.posix_thread.weak_process()) {
        // The LSM hooks may access the target process after the PID table
        // lock is released. Keep it alive throughout the check.
        let _target_process = target.weak_process().upgrade().ok_or_else(|| {
            Error::with_message(Errno::ESRCH, "the PMU target process has exited")
        })?;
        target.check_alien_access_from(ctx.posix_thread, AlienAccessMode::READ_WITH_REAL_CREDS)?;
    }
    Ok(AuthorizedTarget {
        context: thread.perf().clone(),
        tid: pid as u32,
        access_epoch,
    })
}

fn check_cpu_capabilities(target_cpu: Option<CpuId>) -> Result<()> {
    for candidate_cpu in
        cpu::all_cpus().filter(|candidate| target_cpu.is_none_or(|c| c == *candidate))
    {
        let caps = pmu::capabilities(candidate_cpu).map_err(to_error)?;
        if caps.counters() == 0 {
            return_errno_with_message!(Errno::EOPNOTSUPP, "no hardware counters on a target CPU");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// User-space `perf_event_attr` copying and validation
// ---------------------------------------------------------------------------

// Reference: Linux `include/uapi/linux/perf_event.h` (`PERF_ATTR_SIZE_VER0`).
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

impl PerfEventAttrV0 {
    fn parse_counting(&self, cpu: Option<CpuId>) -> Result<(EventSpec, ReadFormat)> {
        let event = match (self.type_, self.config) {
            (PERF_TYPE_HARDWARE, PERF_COUNT_HW_CPU_CYCLES) => Event::CpuCycles,
            (PERF_TYPE_HARDWARE, PERF_COUNT_HW_INSTRUCTIONS) => Event::Instructions,
            _ => return_errno_with_message!(
                Errno::EOPNOTSUPP,
                "only hardware cycles and instructions are supported"
            ),
        };
        if !attributes::supported_flags(self.flags)
            || !attributes::supported_sampling(self.sample_period, self.sample_type)
            || self.wakeup_events != 0
            || self.bp_type != 0
            || self.config1 != 0
        {
            return_errno_with_message!(Errno::EOPNOTSUPP, "unsupported perf counting attributes");
        }
        let read_format = ReadFormat::from_bits(self.read_format).ok_or_else(|| {
            Error::with_message(Errno::EOPNOTSUPP, "unsupported perf read format")
        })?;
        let is_user_counted = self.flags & ATTR_EXCLUDE_USER == 0;
        let is_kernel_counted = self.flags & ATTR_EXCLUDE_KERNEL == 0;
        let scope = match (is_user_counted, is_kernel_counted) {
            (true, false) => CountingScope::User,
            (false, true) => CountingScope::Kernel,
            (true, true) => CountingScope::UserAndKernel,
            (false, false) => return_errno_with_message!(
                Errno::EINVAL,
                "all requested privilege levels are excluded"
            ),
        };
        Ok((
            EventSpec {
                config: EventConfig::new(event, scope),
                cpu,
                is_initially_disabled: self.flags & ATTR_DISABLED != 0,
                should_enable_on_exec: self.flags & ATTR_ENABLE_ON_EXEC != 0,
            },
            read_format,
        ))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
struct AttrHeader {
    type_: u32,
    size: u32,
}

const ATTR_FLAGS_OFFSET: usize = 40;
const ATTR_CONFIG1_OFFSET: usize = 56;

const_assert!(size_of::<PerfEventAttrV0>() == PERF_ATTR_SIZE_VER0 as usize);
const_assert!(core::mem::offset_of!(PerfEventAttrV0, flags) == ATTR_FLAGS_OFFSET);
const_assert!(core::mem::offset_of!(PerfEventAttrV0, config1) == ATTR_CONFIG1_OFFSET);

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
