// SPDX-License-Identifier: MPL-2.0

//! Per-thread hardware performance counting backed by OSTD's CPU-local PMU.

mod activity;
mod context;
mod observation;
mod remote;

use ostd::pmu::PmuError;

pub use self::context::{EventSpec, Operation, PerfContext, Snapshot, Ticket};
use crate::{prelude::*, thread::Thread};

/// Probes the CPU-local PMU before its timer callbacks begin running.
pub(crate) fn init_on_cpu() {
    let _ = ostd::pmu::init_on_cpu();
    ostd::timer::register_callback_on_cpu(sync_current);
}

fn sync_current() {
    remote::service_requests();
    if activity::has_events()
        && let Some(thread) = Thread::current()
    {
        thread.perf().on_tick();
    }
}

fn to_error(error: PmuError) -> Error {
    let (errno, message) = match error {
        PmuError::Unsupported => (Errno::EOPNOTSUPP, "no supported hardware PMU"),
        PmuError::UnsupportedEvent => (Errno::EOPNOTSUPP, "PMU event is unavailable"),
        PmuError::UnsupportedFilter => {
            (Errno::EOPNOTSUPP, "PMU privilege filtering is unavailable")
        }
        PmuError::InvalidConfig => (Errno::EINVAL, "invalid PMU configuration"),
        PmuError::NoCounter => (Errno::EBUSY, "PMU counter resource is occupied"),
        PmuError::Overflow => (
            Errno::EOVERFLOW,
            "PMU count or observation interval overflow",
        ),
        PmuError::WrongCpu | PmuError::InvalidHandle | PmuError::HardwareFault => {
            (Errno::EIO, "PMU register or ownership operation failed")
        }
    };
    Error::with_message(errno, message)
}
