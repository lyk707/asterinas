// SPDX-License-Identifier: MPL-2.0

//! Per-thread event ownership, cross-CPU synchronization, and scheduler accounting.
//!
//! # Lock Ordering
//!
//! The context state lock precedes the OSTD CPU-local PMU lock.
//! Synchronous IPIs are sent and waited on after dropping the state lock and
//! restoring local interrupts. The receiver rechecks the owner before touching
//! hardware. File drop never waits for an IPI.

use core::sync::atomic::{AtomicU64, Ordering};

use ostd::{
    cpu::{CpuId, CpuSet, PinCurrentCpu},
    irq::{self, DisabledLocalIrqGuard},
    pmu::{self, CounterHandle, Event, EventConfig, PmuError},
    smp,
    sync::LocalIrqDisabled,
};

use super::{activity::EventActivity, observation::ActiveTime};
use crate::prelude::*;

/// Number of hardware event slots managed per thread (`CpuCycles` and `Instructions`).
const MAX_THREAD_EVENTS: usize = 2;

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
    state: SpinLock<State, LocalIrqDisabled>,
}

impl PerfContext {
    /// Creates an empty `PerfContext` for a newly spawned thread.
    pub(crate) fn new() -> Self {
        Self {
            state: SpinLock::new(State {
                owner: None,
                has_exited: false,
                generations: [0; MAX_THREAD_EVENTS],
                records: [None, None],
            }),
        }
    }

    /// Creates and attaches a new hardware event to this thread context.
    pub(super) fn create(self: &Arc<Self>, spec: EventSpec) -> Result<Ticket> {
        let id = NEXT_EVENT_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                cur.checked_add(1)
            })
            .map_err(|_| Error::with_message(Errno::ENOSPC, "PMU event ID space exhausted"))?;

        self.on_owner(move |state, irq_guard| {
            if state.has_exited {
                return_errno_with_message!(Errno::ESRCH, "the target thread has exited");
            }
            let slot = match spec.config.event() {
                Event::CpuCycles => 0,
                Event::Instructions => 1,
            };
            if let Some(mut closed) = state.records[slot].take_if(|record| record.is_closed) {
                closed.stop(read_now_ns(), irq_guard);
            }
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
            let mut record = Record {
                _activity: EventActivity::new(),
                ticket,
                spec,
                is_enabled,
                is_closed: false,
                fault: None,
                base_count: 0,
                enabled_time: ActiveTime::default(),
                base_running_ns: 0,
                live: None,
            };
            if state.owner.is_some() {
                record.sync(read_now_ns(), true, irq_guard);
                record.check_fault()?;
            }
            state.records[slot] = Some(record);
            Ok(ticket)
        })
    }

    /// Samples the requested event on its owner CPU without stopping it.
    pub(super) fn read(self: &Arc<Self>, ticket: Ticket) -> Result<Snapshot> {
        self.on_owner(move |state, irq_guard| {
            let task_active = state.owner.is_some() && !state.has_exited;
            let record = Self::record_mut(state, ticket)?;
            let now_ns = read_now_ns();
            record.update_enabled(now_ns, task_active);
            record.sample(now_ns, irq_guard);
            record.snapshot()
        })
    }

    /// Executes an `Enable`, `Disable`, or `Reset` control operation on `ticket`.
    pub(super) fn control(self: &Arc<Self>, ticket: Ticket, operation: Operation) -> Result<()> {
        self.on_owner(move |state, irq_guard| {
            let now_ns = read_now_ns();
            let has_exited = state.has_exited;
            let task_active = state.owner.is_some() && !has_exited;
            let record = Self::record_mut(state, ticket)?;
            match operation {
                Operation::Disable => {
                    record.disable(now_ns);
                    record.stop(now_ns, irq_guard);
                }
                Operation::Enable => {
                    if has_exited {
                        return_errno_with_message!(Errno::ESRCH, "the target thread has exited");
                    }
                    record.check_fault()?;
                    record.is_enabled = true;
                    if task_active {
                        record.sync(now_ns, true, irq_guard);
                    }
                }
                Operation::Reset => {
                    record.reset(now_ns, irq_guard)?;
                }
            }
            record.check_fault()
        })
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

    /// Samples active counters on timer ticks and processes pending closes on IPIs.
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
        for record in state.records.iter_mut().flatten() {
            if record.spec.should_enable_on_exec && !record.is_closed {
                record.spec.should_enable_on_exec = false;
                record.is_enabled = true;
            }
        }
        if state.owner == Some(irq_guard.current_cpu()) {
            Self::sync(&mut state, &irq_guard);
        }
    }

    fn on_owner<T: Send + 'static, F>(self: &Arc<Self>, mut operation: F) -> T
    where
        F: FnOnce(&mut State, &DisabledLocalIrqGuard) -> T + Send + 'static,
    {
        loop {
            let cpu = {
                let irq_guard = irq::disable_local();
                let mut state = self.state.lock();
                match state.owner {
                    Some(cpu) if cpu != irq_guard.current_cpu() => cpu,
                    _ => return operation(&mut state, &irq_guard),
                }
            };
            let context = self.clone();
            match smp::call_on_cpu(cpu, move || {
                let irq_guard = irq::disable_local();
                let mut state = context.state.lock();
                // A scheduled-out context has no live hardware to sample.
                // Retry only when ownership moved to a different CPU.
                if state.owner.is_some_and(|owner| owner != cpu) {
                    Err(operation)
                } else {
                    Ok(operation(&mut state, &irq_guard))
                }
            }) {
                Ok(result) => return result,
                Err(retry) => operation = retry,
            }
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
                record.sync(now_ns, !state.has_exited, irq_guard);
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
    has_exited: bool,
    generations: [u64; MAX_THREAD_EVENTS],
    records: [Option<Record>; MAX_THREAD_EVENTS],
}

#[derive(Debug)]
struct Record {
    _activity: EventActivity,
    ticket: Ticket,
    spec: EventSpec,
    is_enabled: bool,
    is_closed: bool,
    fault: Option<PmuError>,
    base_count: u64,
    enabled_time: ActiveTime,
    base_running_ns: u64,
    live: Option<RunningSegment>,
}

#[derive(Debug)]
struct RunningSegment {
    handle: CounterHandle,
    width_bits: u8,
    since_ns: u64,
    last_observed_ns: u64,
    total: u64,
    count_at_reset: u64,
}

impl RunningSegment {
    fn check_interval(&self, now_ns: u64) -> core::result::Result<(), PmuError> {
        if super::observation::interval_is_safe(self.width_bits, self.last_observed_ns, now_ns) {
            Ok(())
        } else {
            Err(PmuError::Overflow)
        }
    }

    fn count(&self) -> core::result::Result<u64, PmuError> {
        self.total
            .checked_sub(self.count_at_reset)
            .ok_or(PmuError::Overflow)
    }
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

    fn disable(&mut self, now_ns: u64) {
        self.update_enabled(now_ns, false);
        self.is_enabled = false;
    }

    fn sync(&mut self, now_ns: u64, task_active: bool, irq_guard: &DisabledLocalIrqGuard) {
        self.update_enabled(now_ns, task_active);
        if task_active
            && self.is_enabled
            && !self.is_closed
            && self.fault.is_none()
            && self
                .spec
                .cpu
                .is_none_or(|cpu| cpu == irq_guard.current_cpu())
        {
            self.start(now_ns, irq_guard);
            self.sample(now_ns, irq_guard);
        } else {
            self.stop(now_ns, irq_guard);
        }
    }

    fn start(&mut self, now_ns: u64, irq_guard: &DisabledLocalIrqGuard) {
        if self.live.is_some() {
            return;
        }
        let result = (|| {
            let handle = pmu::configure(self.spec.config, irq_guard)?;
            let width_bits = pmu::counter_width(&handle, irq_guard)?;
            pmu::start(&handle, irq_guard)?;
            Ok(RunningSegment {
                handle,
                width_bits,
                since_ns: now_ns,
                last_observed_ns: now_ns,
                total: 0,
                count_at_reset: 0,
            })
        })();
        match result {
            Ok(segment) => self.live = Some(segment),
            Err(err) => self.fault = Some(err),
        }
    }

    fn sample(&mut self, now_ns: u64, irq_guard: &DisabledLocalIrqGuard) {
        let Some(segment) = self.live.as_mut() else {
            return;
        };
        match segment
            .check_interval(now_ns)
            .and_then(|()| pmu::read(&segment.handle, irq_guard))
        {
            Ok(total) => {
                segment.total = total;
                segment.last_observed_ns = now_ns;
            }
            Err(err) => {
                self.fault.get_or_insert(err);
                self.stop(now_ns, irq_guard);
            }
        }
    }

    fn stop(&mut self, now_ns: u64, irq_guard: &DisabledLocalIrqGuard) {
        let Some(segment) = self.live.take() else {
            return;
        };
        let interval_res = segment.check_interval(now_ns);
        let stop_res = pmu::stop(&segment.handle, irq_guard);
        let release_res = pmu::release(segment.handle, irq_guard);

        let result = interval_res.and(stop_res).and_then(|total| {
            let count = total
                .checked_sub(segment.count_at_reset)
                .ok_or(PmuError::Overflow)?;
            self.base_count = self
                .base_count
                .checked_add(count)
                .ok_or(PmuError::Overflow)?;
            let elapsed_ns = now_ns
                .checked_sub(segment.since_ns)
                .ok_or(PmuError::Overflow)?;
            self.base_running_ns = self
                .base_running_ns
                .checked_add(elapsed_ns)
                .ok_or(PmuError::Overflow)?;
            release_res
        });
        if let Err(err) = result {
            self.fault.get_or_insert(err);
        }
    }

    fn reset(&mut self, now_ns: u64, irq_guard: &DisabledLocalIrqGuard) -> Result<()> {
        self.check_fault()?;
        self.sample(now_ns, irq_guard);
        self.check_fault()?;
        // RESET affects the count, not the enabled or running times.
        self.base_count = 0;
        if let Some(segment) = self.live.as_mut() {
            segment.count_at_reset = segment.total;
        }
        Ok(())
    }

    fn check_fault(&self) -> Result<()> {
        self.fault.map_or(Ok(()), |err| Err(super::to_error(err)))
    }

    fn snapshot(&self) -> Result<Snapshot> {
        self.check_fault()?;
        let mut count = self.base_count;
        let mut running_ns = self.base_running_ns;
        if let Some(segment) = self.live.as_ref() {
            let overflow = || super::to_error(PmuError::Overflow);
            let live_ns = segment
                .last_observed_ns
                .checked_sub(segment.since_ns)
                .ok_or_else(overflow)?;
            count = count
                .checked_add(segment.count().map_err(super::to_error)?)
                .ok_or_else(overflow)?;
            running_ns = running_ns.checked_add(live_ns).ok_or_else(overflow)?;
        }

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

fn notify_async(cpu: Option<CpuId>) {
    if let Some(target_cpu) = cpu {
        let _ = smp::inter_processor_call(&CpuSet::from(target_cpu), super::sync_current);
    }
}
