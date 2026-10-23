// SPDX-License-Identifier: MPL-2.0

//! Per-thread event ownership, cross-CPU synchronization, and scheduler accounting.
//!
//! # Lock Ordering
//!
//! 1. [`PerfContext::operation`] (`Mutex<()>`)
//! 2. [`PerfContext::state`] (`SpinLock<State, LocalIrqDisabled>`)
//! 3. OSTD CPU-local PMU lock (`ostd::pmu::LOCAL`)
//!
//! Scheduler, timer, exit, and IPI paths acquire only locks (2) and (3).
//! Synchronous IPIs are sent and waited on only after dropping `state` and
//! restoring local interrupts. File drop never blocks.

use core::sync::atomic::{AtomicU64, Ordering};

use ostd::{
    cpu::{CpuId, CpuSet, PinCurrentCpu},
    irq::{self, DisabledLocalIrqGuard},
    pmu::{self, CounterHandle, EventConfig, PmuError},
    sync::LocalIrqDisabled,
};

use super::observation::ActiveTime;
use crate::prelude::*;

/// Number of hardware event slots managed per thread (`CpuCycles` and `Instructions`).
const MAX_THREAD_EVENTS: usize = 2;
/// Default counter width in bits before a hardware handle is bound.
const DEFAULT_COUNTER_WIDTH_BITS: u8 = 64;

/// Global monotonically increasing `perf_event` ID generator.
static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(1);

/// Specification for creating a per-thread hardware counting event.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EventSpec {
    pub config: EventConfig,
    pub cpu: Option<CpuId>,
    pub is_initially_disabled: bool,
    pub should_enable_on_exec: bool,
}

/// Handle identifying an allocated event slot within a [`PerfContext`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Ticket {
    pub slot: usize,
    pub generation: u64,
    pub id: u64,
}

/// Point-in-time counter and timing values returned to `read(2)`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Snapshot {
    pub count: u64,
    pub enabled_ns: u64,
    pub running_ns: u64,
}

/// Control operations supported by `ioctl(2)`.
#[derive(Clone, Copy, Debug)]
pub(super) enum Operation {
    Enable,
    Disable,
    Reset,
}

/// Per-thread PMU context shared by the monitored [`Thread`] and any open [`PerfEventFile`]s.
///
/// [`Thread`]: crate::thread::Thread
/// [`PerfEventFile`]: super::PerfEventFile
#[derive(Debug)]
pub(crate) struct PerfContext {
    operation: Mutex<()>,
    state: SpinLock<State, LocalIrqDisabled>,
}

impl PerfContext {
    /// Creates an empty `PerfContext` for a newly spawned thread.
    pub(crate) fn new() -> Self {
        Self {
            operation: Mutex::new(()),
            state: SpinLock::new(State {
                owner: None,
                is_paused: false,
                has_exited: false,
                generations: [0; MAX_THREAD_EVENTS],
                records: [None, None],
            }),
        }
    }

    /// Creates and attaches a new hardware event to this thread context.
    pub(super) fn create(&self, spec: EventSpec) -> Result<Ticket> {
        let id = NEXT_EVENT_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                cur.checked_add(1)
            })
            .map_err(|_| Error::with_message(Errno::ENOSPC, "PMU event ID space exhausted"))?;

        let ticket = self.with_frozen(|state, _now_ns| {
            if state.has_exited {
                return_errno_with_message!(Errno::ESRCH, "the target thread has exited");
            }
            let slot = spec.config.event().index();
            if state.records[slot].is_some() {
                return_errno_with_message!(
                    Errno::EBUSY,
                    "one event of each type per thread is supported"
                );
            }
            let generation = state.generations[slot]
                .checked_add(1)
                .ok_or_else(|| Error::with_message(Errno::ENOSPC, "PMU generation exhausted"))?;
            state.generations[slot] = generation;

            let is_enabled = !spec.is_initially_disabled;
            let ticket = Ticket {
                slot,
                generation,
                id,
            };
            state.records[slot] = Some(Record {
                ticket,
                spec,
                is_enabled,
                is_closed: false,
                fault: None,
                base_count: 0,
                live_count: 0,
                enabled_time: ActiveTime::default(),
                base_running_ns: 0,
                live: None,
                live_since_ns: 0,
                last_observed_ns: 0,
                width_bits: DEFAULT_COUNTER_WIDTH_BITS,
            });
            Ok(ticket)
        })?;

        if let Err(err) = self.check_fault(ticket) {
            self.close(ticket);
            return Err(err);
        }
        Ok(ticket)
    }

    /// Reads a snapshot after settling counters on their current owner CPU.
    pub(super) fn read(&self, ticket: Ticket) -> Result<Snapshot> {
        // Pause is visible to schedule-in on every CPU. A single IPI followed
        // by a live snapshot could otherwise sample a stale owner after migration.
        self.with_frozen(|state, now_ns| Self::record_mut(state, ticket)?.snapshot(now_ns))
    }

    /// Executes an `Enable`, `Disable`, or `Reset` control operation on `ticket`.
    pub(super) fn control(&self, ticket: Ticket, operation: Operation) -> Result<()> {
        self.with_frozen(|state, now_ns| {
            let has_exited = state.has_exited;
            let record = Self::record_mut(state, ticket)?;
            match operation {
                Operation::Disable => record.disable(now_ns),
                Operation::Enable => {
                    if has_exited {
                        return_errno_with_message!(Errno::ESRCH, "the target thread has exited");
                    }
                    if let Some(err) = record.fault {
                        return Err(super::to_error(err));
                    }
                    record.enable(now_ns);
                }
                Operation::Reset => {
                    if let Some(err) = record.fault {
                        return Err(super::to_error(err));
                    }
                    // Linux PERF_EVENT_IOC_RESET clears only the count, preserving times.
                    record.base_count = 0;
                    record.live_count = 0;
                }
            }
            Ok(())
        })?;
        self.check_fault(ticket)
    }

    /// Marks `ticket` closed when its [`PerfEventFile`] is dropped.
    ///
    /// [`PerfEventFile`]: super::PerfEventFile
    pub(super) fn close(&self, ticket: Ticket) {
        let owner_cpu = {
            let mut state = self.state.lock();
            let Ok(record) = Self::record_mut(&mut state, ticket) else {
                return;
            };
            record.is_closed = true;
            if record.live.is_none() {
                state.records[ticket.slot] = None;
            }
            state.owner
        };
        // Never wait in close: it may run under a file-table lock or with IRQs disabled.
        notify_async(owner_cpu);
    }

    /// Stops and settles all active counters before the thread is scheduled out.
    pub(crate) fn on_schedule_out(&self, irq_guard: &DisabledLocalIrqGuard) {
        let mut state = self.state.lock();
        if state.records.iter().any(Option::is_some) {
            let now_ns = read_now_ns();
            for slot in &mut state.records {
                if let Some(record) = slot.as_mut() {
                    record.update_enabled(now_ns, false);
                    record.stop(now_ns, irq_guard);
                    if record.is_closed {
                        *slot = None;
                    }
                }
            }
        }
        state.owner = None;
    }

    /// Starts or resumes eligible counters after the thread is scheduled in on the current CPU.
    pub(crate) fn on_schedule_in(&self) {
        let irq_guard = irq::disable_local();
        let mut state = self.state.lock();
        state.owner = Some(irq_guard.current_cpu());
        Self::sync(&mut state, &irq_guard);
    }

    /// Periodically samples active counters on timer ticks and synchronous IPIs.
    pub(crate) fn on_tick(&self) {
        let irq_guard = irq::disable_local();
        let mut state = self.state.lock();
        if state.owner == Some(irq_guard.current_cpu()) {
            Self::sync(&mut state, &irq_guard);
        }
    }

    /// Stops all counters and freezes final snapshots when the thread exits.
    pub(crate) fn on_exit(&self) {
        let irq_guard = irq::disable_local();
        let mut state = self.state.lock();
        state.has_exited = true;
        if !state.records.iter().any(Option::is_some) {
            return;
        }
        let now_ns = read_now_ns();
        for record in state.records.iter_mut().flatten() {
            record.stop(now_ns, &irq_guard);
            record.disable(now_ns);
        }
    }

    /// Activates `enable_on_exec` events after `execve(2)` succeeds.
    pub(crate) fn on_exec(&self) {
        let irq_guard = irq::disable_local();
        let mut state = self.state.lock();
        if !state.records.iter().any(Option::is_some) {
            return;
        }
        let now_ns = read_now_ns();
        for record in state.records.iter_mut().flatten() {
            if record.spec.should_enable_on_exec && !record.is_closed {
                record.spec.should_enable_on_exec = false;
                record.enable(now_ns);
            }
        }
        if state.owner == Some(irq_guard.current_cpu()) {
            Self::sync(&mut state, &irq_guard);
        }
    }

    fn with_frozen<T>(&self, operation_fn: impl FnOnce(&mut State, u64) -> Result<T>) -> Result<T> {
        let _operation_guard = self.operation.lock();
        let owner_cpu = {
            let mut state = self.state.lock();
            state.is_paused = true;
            state.owner
        };
        notify_and_wait(owner_cpu);

        let (result, resume_cpu) = {
            let mut state = self.state.lock();
            debug_assert!(state.records.iter().flatten().all(|r| r.live.is_none()));
            for slot in &mut state.records {
                if slot
                    .as_ref()
                    .is_some_and(|r| r.is_closed && r.live.is_none())
                {
                    *slot = None;
                }
            }
            let result = operation_fn(&mut state, read_now_ns());
            state.is_paused = false;
            (result, state.owner)
        };
        notify_and_wait(resume_cpu);
        result
    }

    fn check_fault(&self, ticket: Ticket) -> Result<()> {
        let mut state = self.state.lock();
        match Self::record_mut(&mut state, ticket)?.fault {
            Some(err) => Err(super::to_error(err)),
            None => Ok(()),
        }
    }

    fn record_mut(state: &mut State, ticket: Ticket) -> Result<&mut Record> {
        state.records[ticket.slot]
            .as_mut()
            .filter(|record| record.ticket.generation == ticket.generation && !record.is_closed)
            .ok_or_else(|| Error::with_message(Errno::EBADF, "the PMU event no longer exists"))
    }

    fn sync(state: &mut State, irq_guard: &DisabledLocalIrqGuard) {
        if !state.records.iter().any(Option::is_some) {
            return;
        }
        let now_ns = read_now_ns();
        for slot in &mut state.records {
            if let Some(record) = slot.as_mut() {
                record.update_enabled(now_ns, !state.is_paused && !state.has_exited);
                let should_run = !state.is_paused
                    && !state.has_exited
                    && record.is_enabled
                    && !record.is_closed
                    && record.fault.is_none()
                    && record
                        .spec
                        .cpu
                        .is_none_or(|cpu| cpu == irq_guard.current_cpu());
                if should_run {
                    record.start(now_ns, irq_guard);
                    record.sample(now_ns, irq_guard);
                } else {
                    record.stop(now_ns, irq_guard);
                }
                if record.is_closed && record.live.is_none() {
                    *slot = None;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal state and per-event accounting record
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct State {
    owner: Option<CpuId>,
    is_paused: bool,
    has_exited: bool,
    generations: [u64; MAX_THREAD_EVENTS],
    records: [Option<Record>; MAX_THREAD_EVENTS],
}

#[derive(Debug)]
struct Record {
    ticket: Ticket,
    spec: EventSpec,
    is_enabled: bool,
    is_closed: bool,
    fault: Option<PmuError>,
    base_count: u64,
    live_count: u64,
    enabled_time: ActiveTime,
    base_running_ns: u64,
    live: Option<CounterHandle>,
    live_since_ns: u64,
    last_observed_ns: u64,
    width_bits: u8,
}

impl Record {
    fn update_enabled(&mut self, now_ns: u64, task_active: bool) {
        if self
            .enabled_time
            .update(now_ns, task_active && self.is_enabled && !self.is_closed)
            .is_none()
        {
            self.fault.get_or_insert(PmuError::Overflow);
        }
    }

    fn enable(&mut self, _now_ns: u64) {
        // The next sync starts time on the scheduled target, not on the reader.
        self.is_enabled = true;
    }

    fn disable(&mut self, now_ns: u64) {
        self.update_enabled(now_ns, false);
        self.is_enabled = false;
    }

    fn check_interval(&self, now_ns: u64) -> core::result::Result<(), PmuError> {
        if super::observation::interval_is_safe(self.width_bits, self.last_observed_ns, now_ns) {
            Ok(())
        } else {
            Err(PmuError::Overflow)
        }
    }

    fn start(&mut self, now_ns: u64, irq_guard: &DisabledLocalIrqGuard) {
        if self.live.is_some() || self.fault.is_some() {
            return;
        }
        let result = (|| {
            let handle = pmu::configure(self.spec.config, irq_guard)?;
            let width_bits = pmu::counter_width(&handle, irq_guard)?;
            pmu::start(&handle, irq_guard)?;
            self.width_bits = width_bits;
            self.live = Some(handle);
            self.live_count = 0;
            self.live_since_ns = now_ns;
            self.last_observed_ns = now_ns;
            Ok(())
        })();
        if let Err(err) = result {
            self.fault = Some(err);
        }
    }

    fn sample(&mut self, now_ns: u64, irq_guard: &DisabledLocalIrqGuard) {
        let Some(handle) = self.live.as_ref() else {
            return;
        };
        match self
            .check_interval(now_ns)
            .and_then(|()| pmu::read(handle, irq_guard))
        {
            Ok(segment_count) => {
                self.live_count = segment_count;
                self.last_observed_ns = now_ns;
            }
            Err(err) => {
                self.fault.get_or_insert(err);
                self.stop(now_ns, irq_guard);
            }
        }
    }

    fn stop(&mut self, now_ns: u64, irq_guard: &DisabledLocalIrqGuard) {
        let Some(handle) = self.live.take() else {
            return;
        };
        let interval_res = self.check_interval(now_ns);
        let stop_res = pmu::stop(&handle, irq_guard);
        let release_res = pmu::release(handle, irq_guard);

        let result = interval_res.and(stop_res).and_then(|segment_count| {
            self.base_count = self
                .base_count
                .checked_add(segment_count)
                .ok_or(PmuError::Overflow)?;
            self.live_count = 0;
            let elapsed_ns = now_ns
                .checked_sub(self.live_since_ns)
                .ok_or(PmuError::Overflow)?;
            self.base_running_ns = self
                .base_running_ns
                .checked_add(elapsed_ns)
                .ok_or(PmuError::Overflow)?;
            self.live_since_ns = 0;
            release_res
        });
        if let Err(err) = result {
            self.fault.get_or_insert(err);
        }
    }

    fn snapshot(&mut self, now_ns: u64) -> Result<Snapshot> {
        let settle_ns = if self.live.is_some() {
            self.last_observed_ns.max(now_ns)
        } else {
            now_ns
        };
        self.update_enabled(settle_ns, self.enabled_time.is_active());

        if let Some(err) = self.fault {
            return Err(super::to_error(err));
        }

        let count = self
            .base_count
            .checked_add(self.live_count)
            .ok_or_else(|| super::to_error(PmuError::Overflow))?;
        let active_running_ns = if self.live.is_some() {
            self.last_observed_ns.saturating_sub(self.live_since_ns)
        } else {
            0
        };
        let running_ns = self
            .base_running_ns
            .checked_add(active_running_ns)
            .ok_or_else(|| super::to_error(PmuError::Overflow))?;

        Ok(Snapshot {
            count,
            enabled_ns: self.enabled_time.total(),
            running_ns,
        })
    }
}

fn read_now_ns() -> u64 {
    aster_time::read_monotonic_time()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

fn notify_and_wait(cpu: Option<CpuId>) {
    if let Some(target_cpu) = cpu {
        let pending =
            ostd::smp::inter_processor_call(&CpuSet::from(target_cpu), super::sync_current);
        pending.wait();
    }
}

fn notify_async(cpu: Option<CpuId>) {
    if let Some(target_cpu) = cpu {
        let _ = ostd::smp::inter_processor_call(&CpuSet::from(target_cpu), super::sync_current);
    }
}
