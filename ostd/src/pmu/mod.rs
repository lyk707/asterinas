// SPDX-License-Identifier: MPL-2.0

//! CPU-local PMU resources for counting CPU cycles and retired instructions.
//!
//! A [`CounterHandle`] represents one exclusive hardware counter allocation on
//! a specific CPU.
//! All hardware register operations require a [`DisabledLocalIrqGuard`] on the owning CPU.
//! Dropping a handle retires the allocation on its owner CPU. A remote drop
//! leaves cleanup to the owner CPU timer or its next allocation.
//!
//! The upper kernel layer owns per-task attribution and must stop and release
//! active handles before migrating a task across CPUs.

mod accounting;
mod retirement;
mod types;

use core::sync::atomic::{AtomicBool, Ordering};

use retirement::Cleanup;
pub(crate) use types::ConfigFailure;
pub use types::{BackendKind, CountingScope, Event, EventConfig, PmuCaps, PmuError, Result};

#[cfg(any(target_arch = "x86_64", target_arch = "riscv64"))]
use crate::arch::pmu as backend;
#[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64")))]
#[path = "unsupported.rs"]
mod backend;

use crate::{
    cpu::{CpuId, PinCurrentCpu},
    cpu_local,
    irq::{self, DisabledLocalIrqGuard},
    sync::{LocalIrqDisabled, SpinLock},
    timer::Jiffies,
};

/// The number of logical event slots managed per CPU.
const MAX_EVENTS: usize = 2;

/// An exclusive CPU-local hardware counter allocation.
#[must_use]
#[derive(Debug)]
pub struct CounterHandle {
    cpu: CpuId,
    slot: usize,
    generation: u64,
}

/// Initializes and caches the PMU capabilities of the current CPU.
pub fn init_on_cpu() -> Result<PmuCaps> {
    let irq_guard = irq::disable_local();
    let (caps, should_register_cleanup) = {
        let mut local = LOCAL.get_on_cpu(irq_guard.current_cpu()).lock();
        let first_probe = local.caps.is_none();
        let caps = local
            .caps
            .get_or_insert_with(backend::probe)
            .as_ref()
            .map(|caps| caps.summary())
            .map_err(|&error| error);
        (caps, first_probe && caps.is_ok())
    };
    if should_register_cleanup {
        crate::timer::register_callback_on_cpu(collect_retired);
    }
    caps
}

/// Returns the cached PMU capabilities of `cpu` without cross-CPU register access.
pub fn capabilities(cpu: CpuId) -> Result<PmuCaps> {
    LOCAL
        .get_on_cpu(cpu)
        .lock()
        .capabilities()
        .map(|caps| caps.summary())
}

/// Allocates and configures a stopped hardware counter on the current CPU.
pub fn configure(config: EventConfig, irq_guard: &DisabledLocalIrqGuard) -> Result<CounterHandle> {
    let cpu = irq_guard.current_cpu();
    let mut local = LOCAL.get_on_cpu(cpu).lock();
    collect_locked(&mut local, cpu);
    // Cleanup may have exhausted its retries and quarantined this CPU's PMU.
    local.capabilities()?;

    let slot = match config.event() {
        Event::CpuCycles => 0,
        Event::Instructions => 1,
    };
    if local.records[slot].is_some() {
        return Err(PmuError::NoCounter);
    }

    let generation = local.generations[slot]
        .checked_add(1)
        .ok_or(PmuError::NoCounter)?;
    local.generations[slot] = generation;

    let used = local
        .records
        .each_ref()
        .map(|record| record.as_ref().map(|record| record.token.index()));

    let (token, initial_raw) = match backend::configure(config, local.capabilities()?, &used) {
        Ok(configured) => configured,
        Err(failure) => {
            if let Some(token) = failure.token {
                local.records[slot] = Some(Record {
                    generation,
                    token,
                    is_running: false,
                    is_retired: true,
                    cleanup: Cleanup::new(),
                    fault: Some(failure.error),
                    total: 0,
                    last: 0,
                });
                collect_locked(&mut local, cpu);
            } else if failure.error == PmuError::HardwareFault {
                // Firmware gave no trustworthy resource identity; mark CPU PMU faulted.
                local.caps = Some(Err(failure.error));
            }
            return Err(failure.error);
        }
    };

    let width_bits = token.width_bits();
    local.records[slot] = Some(Record {
        generation,
        token,
        is_running: false,
        is_retired: false,
        cleanup: Cleanup::new(),
        fault: None,
        total: 0,
        last: initial_raw & accounting::mask(width_bits),
    });

    Ok(CounterHandle {
        cpu,
        slot,
        generation,
    })
}

/// Starts or resumes a stopped counter on the current CPU.
pub fn start(handle: &CounterHandle, irq_guard: &DisabledLocalIrqGuard) -> Result<()> {
    with_record(handle, irq_guard, |record| {
        if let Some(err) = record.fault {
            return Err(err);
        }
        if record.is_running {
            return Ok(());
        }

        match record.token.start() {
            Ok(initial_raw) => {
                record.last = initial_raw & accounting::mask(record.token.width_bits());
                record.is_running = true;
                Ok(())
            }
            Err(err) => {
                record.fault = Some(err);
                Err(err)
            }
        }
    })
}

/// Reads a non-consuming cumulative count from a counter on the current CPU.
pub fn read(handle: &CounterHandle, irq_guard: &DisabledLocalIrqGuard) -> Result<u64> {
    with_record(handle, irq_guard, observe)
}

/// Stops the hardware counter and returns its final cumulative count.
pub fn stop(handle: &CounterHandle, irq_guard: &DisabledLocalIrqGuard) -> Result<u64> {
    with_record(handle, irq_guard, |record| {
        if !record.is_running {
            return record.fault.map_or(Ok(record.total), Err);
        }
        // A software observation error must not skip the hardware stop.
        let result = record.token.stop().and_then(|raw| {
            record.is_running = false;
            if let Some(err) = record.fault {
                return Err(err);
            }
            accounting::accumulate(
                &mut record.total,
                &mut record.last,
                raw,
                record.token.width_bits(),
            )
            .ok_or(PmuError::Overflow)
        });
        if let Err(err) = result {
            record.fault.get_or_insert(err);
        }
        result
    })
}

/// Releases a counter allocation on the current CPU, stopping it first if needed.
pub fn release(handle: CounterHandle, irq_guard: &DisabledLocalIrqGuard) -> Result<()> {
    if irq_guard.current_cpu() != handle.cpu {
        return Err(PmuError::WrongCpu);
    }

    let result = {
        let mut local = LOCAL.get_on_cpu(handle.cpu).lock();
        let record = local.records[handle.slot]
            .as_mut()
            .ok_or(PmuError::InvalidHandle)?;
        if record.generation != handle.generation {
            return Err(PmuError::InvalidHandle);
        }

        record.is_retired = true;
        let release_res = record
            .cleanup
            .attempt(Jiffies::elapsed().as_u64(), || record.token.release())
            .unwrap_or(Err(PmuError::HardwareFault));
        if release_res.is_ok() {
            local.records[handle.slot] = None;
        } else {
            record.fault = Some(PmuError::HardwareFault);
        }
        refresh_collection_state(&mut local, handle.cpu);
        release_res
    };

    drop(handle);
    result
}

/// Returns the hardware bit width of a valid counter allocation.
pub fn counter_width(handle: &CounterHandle, irq_guard: &DisabledLocalIrqGuard) -> Result<u8> {
    with_record(handle, irq_guard, |record| Ok(record.token.width_bits()))
}

/// Retries deferred cleanup of retired counter tokens on the current CPU.
fn collect_retired() {
    // This is an advisory fast path. In task context, migration can at most
    // defer cleanup until the next owner-CPU timer callback; configure also
    // collects under LOCAL's lock before allocating a counter.
    if !NEEDS_COLLECTION
        .get_on_cpu(CpuId::current_racy())
        .load(Ordering::Acquire)
    {
        return;
    }
    let irq_guard = irq::disable_local();
    let cpu = irq_guard.current_cpu();
    collect_locked(&mut LOCAL.get_on_cpu(cpu).lock(), cpu);
}

impl Drop for CounterHandle {
    fn drop(&mut self) {
        let irq_guard = irq::disable_local();
        {
            let mut local = LOCAL.get_on_cpu(self.cpu).lock();
            let Some(record) = local.records[self.slot].as_mut() else {
                return;
            };
            if record.generation != self.generation {
                return;
            }
            record.is_retired = true;
            refresh_collection_state(&mut local, self.cpu);
            if self.cpu == irq_guard.current_cpu() {
                collect_locked(&mut local, self.cpu);
            }
        }
        // Never wait in Drop: it can execute under a file-table lock or with IRQs disabled.
        // A local timer tick or the next allocation can retry cleanup.
    }
}

cpu_local! {
    static NEEDS_COLLECTION: AtomicBool = AtomicBool::new(false);
    static LOCAL: SpinLock<LocalPmu, LocalIrqDisabled> = SpinLock::new(LocalPmu {
        caps: None,
        generations: [0; MAX_EVENTS],
        records: [None, None],
    });
}

struct LocalPmu {
    caps: Option<Result<backend::Caps>>,
    generations: [u64; MAX_EVENTS],
    records: [Option<Record>; MAX_EVENTS],
}

impl LocalPmu {
    fn capabilities(&self) -> Result<&backend::Caps> {
        self.caps
            .as_ref()
            .ok_or(PmuError::Unsupported)?
            .as_ref()
            .map_err(|&error| error)
    }
}

#[derive(Debug)]
struct Record {
    generation: u64,
    token: backend::Token,
    is_running: bool,
    is_retired: bool,
    cleanup: Cleanup,
    fault: Option<PmuError>,
    total: u64,
    last: u64,
}

fn with_record<T>(
    handle: &CounterHandle,
    irq_guard: &DisabledLocalIrqGuard,
    operation_fn: impl FnOnce(&mut Record) -> Result<T>,
) -> Result<T> {
    if irq_guard.current_cpu() != handle.cpu {
        return Err(PmuError::WrongCpu);
    }
    let mut local = LOCAL.get_on_cpu(handle.cpu).lock();
    let record = local.records[handle.slot]
        .as_mut()
        .ok_or(PmuError::InvalidHandle)?;
    if record.generation != handle.generation || record.is_retired {
        return Err(PmuError::InvalidHandle);
    }
    operation_fn(record)
}

fn observe(record: &mut Record) -> Result<u64> {
    if let Some(err) = record.fault {
        return Err(err);
    }
    if !record.is_running {
        return Ok(record.total);
    }

    let result = record.token.read().and_then(|raw| {
        accounting::accumulate(
            &mut record.total,
            &mut record.last,
            raw,
            record.token.width_bits(),
        )
        .ok_or(PmuError::Overflow)
    });
    if let Err(err) = result {
        record.fault = Some(err);
    }
    result
}

fn collect_locked(local: &mut LocalPmu, cpu: CpuId) {
    let now = Jiffies::elapsed().as_u64();
    for slot in &mut local.records {
        if let Some(record) = slot.as_mut()
            && record.is_retired
            && matches!(
                record.cleanup.attempt(now, || record.token.release()),
                Some(Ok(()))
            )
        {
            *slot = None;
        }
    }
    refresh_collection_state(local, cpu);
}

// Called under LOCAL's lock, including when a remote Drop retires a token.
fn refresh_collection_state(local: &mut LocalPmu, cpu: CpuId) {
    let mut needs_collection = false;
    for record in local.records.iter().flatten().filter(|r| r.is_retired) {
        if record.cleanup.is_quarantined() {
            // An unconfirmed release cannot make the counter reusable. Keep
            // its token reserved, reject new allocations, and let existing
            // handles continue through stop/release to settle their resources.
            local.caps = Some(Err(PmuError::HardwareFault));
        } else {
            needs_collection = true;
        }
    }
    NEEDS_COLLECTION
        .get_on_cpu(cpu)
        .store(needs_collection, Ordering::Release);
}
