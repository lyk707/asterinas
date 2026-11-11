// SPDX-License-Identifier: MPL-2.0

//! CPU-local PMU resources for counting CPU cycles and retired instructions.
//!
//! A [`CounterHandle`] represents one exclusive hardware counter allocation on
//! a specific CPU. All hardware register operations require a
//! [`DisabledLocalIrqGuard`] on the owning CPU; dropping a handle from another
//! CPU queues deferred cleanup via an inter-processor call.
//!
//! The upper kernel layer owns per-task attribution and must stop and release
//! active handles before migrating a task across CPUs.

mod accounting;
mod retirement;
#[cfg(target_arch = "riscv64")]
mod sbi_counter;
mod types;
#[cfg(target_arch = "x86_64")]
mod x86_counter;

use core::sync::atomic::{AtomicBool, Ordering};

use retirement::Cleanup;
use types::ConfigFailure;
pub use types::{BackendKind, CountingScope, Event, EventConfig, PmuCaps, PmuError, Result};
#[cfg(target_arch = "x86_64")]
#[path = "x86.rs"]
mod backend;
#[cfg(target_arch = "riscv64")]
#[path = "riscv.rs"]
mod backend;
#[cfg(not(any(target_arch = "x86_64", target_arch = "riscv64")))]
#[path = "unsupported.rs"]
mod backend;

#[cfg(any(target_arch = "x86_64", target_arch = "riscv64"))]
pub(crate) use backend::recover_register_fault;

use crate::{
    cpu::{CpuId, CpuSet, PinCurrentCpu},
    cpu_local,
    irq::{self, DisabledLocalIrqGuard},
    sync::{LocalIrqDisabled, SpinLock},
    timer::Jiffies,
};

/// The maximum number of concurrent counter allocations managed per CPU.
const MAX_CPU_COUNTERS: usize = 2;

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
    let mut local = LOCAL.get_on_cpu(irq_guard.current_cpu()).lock();
    *local.caps.get_or_insert_with(backend::probe)
}

/// Returns the cached PMU capabilities of `cpu` without cross-CPU register access.
pub fn capabilities(cpu: CpuId) -> Result<PmuCaps> {
    LOCAL
        .get_on_cpu(cpu)
        .lock()
        .caps
        .unwrap_or(Err(PmuError::Unsupported))
}

/// Allocates and configures a stopped hardware counter on the current CPU.
pub fn configure(config: EventConfig, irq_guard: &DisabledLocalIrqGuard) -> Result<CounterHandle> {
    let cpu = irq_guard.current_cpu();
    let mut local = LOCAL.get_on_cpu(cpu).lock();
    let caps = local.caps.unwrap_or(Err(PmuError::Unsupported))?;
    if !caps.has_precise_filter() {
        return Err(PmuError::UnsupportedFilter);
    }

    collect_locked(&mut local, cpu);
    // Cleanup may have exhausted its retries and quarantined this CPU's PMU.
    local.caps.unwrap_or(Err(PmuError::Unsupported))?;

    let slot = config.event().index();
    if local.records[slot].is_some() {
        return Err(PmuError::NoCounter);
    }

    let generation = local.generations[slot]
        .checked_add(1)
        .ok_or(PmuError::NoCounter)?;
    local.generations[slot] = generation;

    let used_mask = local
        .records
        .iter()
        .flatten()
        .fold(0u64, |mask, record| mask | record.token.mask());

    let (token, initial_raw) = match backend::configure(config, caps, used_mask) {
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

/// Clears the cumulative total of a healthy counter on the current CPU.
pub fn reset(handle: &CounterHandle, irq_guard: &DisabledLocalIrqGuard) -> Result<()> {
    with_record(handle, irq_guard, |record| {
        if let Some(err) = record.fault {
            return Err(err);
        }
        let raw = record.token.read()?;
        record.last = raw & accounting::mask(record.token.width_bits());
        record.total = 0;
        Ok(())
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
pub fn collect_retired() {
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
                return;
            }
        }
        // Never wait in Drop: it can execute under a file-table lock or with IRQs disabled.
        let _ = crate::smp::inter_processor_call(&CpuSet::from(self.cpu), collect_retired);
    }
}

// ---------------------------------------------------------------------------
// Internal state and helpers
// ---------------------------------------------------------------------------

cpu_local! {
    static NEEDS_COLLECTION: AtomicBool = AtomicBool::new(false);
    static LOCAL: SpinLock<LocalPmu, LocalIrqDisabled> = SpinLock::new(LocalPmu {
        caps: None,
        generations: [0; MAX_CPU_COUNTERS],
        records: [None, None],
    });
}

struct LocalPmu {
    caps: Option<Result<PmuCaps>>,
    generations: [u64; MAX_CPU_COUNTERS],
    records: [Option<Record>; MAX_CPU_COUNTERS],
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
