// SPDX-License-Identifier: MPL-2.0

//! Per-thread hardware performance counting subsystem.
//!
//! Coordinates per-CPU PMU initialization, timer/IPI sampling callbacks, and
//! error translation between [`ostd::pmu`] and Linux `errno` codes.

pub(crate) mod attributes;
mod context;
mod file;
mod observation;

pub(crate) use context::{EventSpec, PerfContext};
pub(crate) use file::PerfEventFile;
use ostd::{cpu::CpuId, pmu::PmuError};

use crate::{prelude::*, thread::Thread};

/// Initializes the hardware PMU and registers the periodic sampling callback on the current CPU.
pub(crate) fn init_on_cpu() {
    match ostd::pmu::init_on_cpu() {
        Ok(caps) => info!(
            "[pmu] cpu={:?} backend={:?} counters={} width={}b",
            CpuId::current_racy(),
            caps.backend(),
            caps.counters(),
            caps.width_bits(),
        ),
        Err(err) => warn!(
            "[pmu] cpu={:?} unavailable: {:?}",
            CpuId::current_racy(),
            err
        ),
    }
    ostd::timer::register_callback_on_cpu(sync_current);
}

/// Synchronizes retired counters and samples the current thread's active PMU events.
///
/// Used both as a local timer callback and as a cross-CPU IPI handler. Never
/// blocks, touches user memory, sends nested IPIs, or acquires sleeping locks.
pub(super) fn sync_current() {
    ostd::pmu::collect_retired();
    if let Some(thread) = Thread::current() {
        thread.perf().on_tick();
    }
}

/// Converts a low-level [`PmuError`] into a kernel [`Error`] with a Linux `Errno`.
pub(super) fn to_error(err: PmuError) -> Error {
    let (errno, message) = match err {
        PmuError::Unsupported => (Errno::EOPNOTSUPP, "no supported hardware PMU"),
        PmuError::UnsupportedFilter => (
            Errno::EOPNOTSUPP,
            "PMU privilege filtering is not guaranteed",
        ),
        PmuError::InvalidConfig => (Errno::EINVAL, "invalid PMU configuration"),
        PmuError::NoCounter => (Errno::EBUSY, "PMU counter resource is occupied"),
        PmuError::Overflow => (
            Errno::EOVERFLOW,
            "PMU counter or observation interval overflow",
        ),
        PmuError::WrongCpu | PmuError::InvalidHandle | PmuError::HardwareFault => (
            Errno::EIO,
            "PMU register, firmware, or handle operation failed",
        ),
    };
    Error::with_message(errno, message)
}
