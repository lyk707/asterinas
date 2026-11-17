// SPDX-License-Identifier: MPL-2.0

//! Symmetric Multi-Processing (SMP) support.
//!
//! This module provides a way to execute code on other processors via inter-
//! processor interrupts.
//!
//! Callers issue work with [`inter_processor_call`].
//! Remote calls are queued for
//! interrupt-context execution and return a [`PendingIpis`] handle that can be
//! waited on when the caller needs completion.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use spin::Once;

use crate::{
    arch::{irq::HwCpuId, trap::TrapFrame},
    cpu::{CpuId, CpuSet, PinCurrentCpu},
    cpu_local,
    irq::{self, DisabledLocalIrqGuard},
    prelude::Result,
    sync::SpinLock,
    util::id_set::Id,
};

/// Executes a function on other processors.
///
/// The provided function `call_fn` will be executed on all target processors
/// specified by `targets`.
/// It can also be executed on the current processor.
/// The function should be short and non-blocking, as it will be executed in
/// interrupt context with interrupts disabled.
///
/// The function `call_fn` will be executed asynchronously on the target
/// processors.
/// A call on the current processor is synchronous.
///
/// The returned [`PendingIpis`] can be used to wait until all remote target
/// processors have handled IPIs for this call.
///
/// # Panics
///
/// This function will panic if a hardware error occurs while sending an IPI to
/// a remote processor.
pub fn inter_processor_call(targets: &CpuSet, call_fn: fn()) -> PendingIpis {
    let ipi_sender = IPI_SENDER.get().unwrap();
    ipi_sender.inter_processor_call(targets, call_fn)
}

/// Runs a short, non-blocking function on `cpu` with interrupts disabled and
/// returns its result.
/// The function must not send IPIs or wait for another CPU.
///
/// # Panics
///
/// Panics if local interrupts are disabled.
///
/// # Errors
///
/// Returns an error if the destination queue is full or delivery fails before the
/// destination claims the callback. An error guarantees the callback will not run.
pub fn call_on_cpu<R: Send + 'static>(
    cpu: CpuId,
    call_fn: impl FnOnce() -> R + Send + 'static,
) -> Result<R> {
    assert!(crate::arch::irq::is_local_enabled());
    let irq_guard = irq::disable_local();
    if irq_guard.current_cpu() == cpu {
        return Ok(call_fn());
    }

    let result = Arc::new(Once::new());
    let remote_result = result.clone();
    let sender = IPI_SENDER.get().unwrap();
    sender.submit(
        cpu,
        IpiCall::Closure(Box::new(move || {
            remote_result.call_once(|| SpinLock::<Option<R>>::new(Some(call_fn())));
        })),
        &irq_guard,
    )?;
    drop(irq_guard);

    let value = result.wait().lock().take().unwrap();
    Ok(value)
}

/// Runs a short, non-blocking function with interrupts disabled on `cpu`.
///
/// A local call runs immediately.
/// A remote call returns once the destination is notified or claims the callback.
///
/// # Errors
///
/// Returns an error if the destination queue is full or delivery fails before the
/// destination claims the callback. An error guarantees the callback will not run.
pub fn call_on_cpu_async(cpu: CpuId, call_fn: fn()) -> Result<()> {
    let irq_guard = irq::disable_local();
    if irq_guard.current_cpu() == cpu {
        call_fn();
        return Ok(());
    }
    IPI_SENDER
        .get()
        .unwrap()
        .submit(cpu, IpiCall::Function(call_fn), &irq_guard)
}

/// Pending remote inter-processor calls.
pub struct PendingIpis {
    calls: BTreeMap<usize, u64>,
}

impl PendingIpis {
    pub(crate) const fn new_empty() -> Self {
        Self {
            calls: BTreeMap::new(),
        }
    }

    pub(crate) fn extend(&mut self, other: &Self) {
        for (&cpu, &sequence) in &other.calls {
            let latest = self.calls.entry(cpu).or_insert(sequence);
            *latest = (*latest).max(sequence);
        }
    }

    /// Waits until the requested remote calls have finished.
    ///
    /// # Panics
    ///
    /// Panics if local interrupts are disabled. The remote CPUs may be
    /// waiting for this processor to handle an IPI.
    pub fn wait(&self) {
        assert!(
            crate::arch::irq::is_local_enabled(),
            "waiting for remote inter-processor calls with IRQs disabled"
        );

        for (&cpu, &sequence) in &self.calls {
            let completed = LAST_COMPLETED_CALL.get_on_cpu(CpuId::new(cpu as u32));
            while completed.load(Ordering::Acquire) < sequence {
                core::hint::spin_loop();
            }
        }
    }
}

/// A sender that carries necessary information to send inter-processor interrupts.
///
/// The purpose of exporting this type is to enable the users to check whether
/// [`IPI_SENDER`] has been initialized.
pub(crate) struct IpiSender {
    hw_cpu_ids: Box<[HwCpuId]>,
}

/// The [`IpiSender`] singleton.
pub(crate) static IPI_SENDER: Once<IpiSender> = Once::new();

impl IpiSender {
    /// Executes a function on other processors.
    ///
    /// See [`inter_processor_call`] for details.
    /// The purpose of exporting this
    /// method is to enable callers to check whether [`IPI_SENDER`] has been
    /// initialized.
    pub(crate) fn inter_processor_call(&self, targets: &CpuSet, call_fn: fn()) -> PendingIpis {
        let irq_guard = irq::disable_local();
        let this_cpu_id = irq_guard.current_cpu();

        let mut call_on_self = false;
        let mut pending_ipis = PendingIpis::new_empty();
        for cpu_id in targets.iter() {
            if cpu_id == this_cpu_id {
                call_on_self = true;
                continue;
            }
            // Existing callers can submit mandatory work with IRQs disabled.
            // Admission cannot wait for space on the destination.
            let sequence = CALL_QUEUES
                .get_on_cpu(cpu_id)
                .lock()
                .push(IpiCall::Function(call_fn));
            pending_ipis.calls.insert(cpu_id.as_usize(), sequence);
        }
        for cpu_id in targets.iter() {
            if cpu_id == this_cpu_id {
                continue;
            }
            self.send(cpu_id, &irq_guard)
                .expect("failed to send inter-processor interrupt");
        }
        if call_on_self {
            // Execute the function synchronously.
            call_fn();
        }
        pending_ipis
    }

    fn submit(&self, cpu: CpuId, call: IpiCall, irq_guard: &DisabledLocalIrqGuard) -> Result<()> {
        let queue = CALL_QUEUES.get_on_cpu(cpu);
        enqueue_and_signal(queue, call, || self.send(cpu, irq_guard))
    }

    fn send(&self, cpu: CpuId, irq_guard: &DisabledLocalIrqGuard) -> Result<()> {
        let hw_cpu_id = self.hw_cpu_ids[cpu.as_usize()];
        crate::arch::irq::send_ipi(hw_cpu_id, irq_guard)
    }
}

enum IpiCall {
    Function(fn()),
    Closure(Box<dyn FnOnce() + Send>),
}

const MAX_PENDING_CALLS: usize = 64;
const CALLS_PER_INTERRUPT: usize = 32;

struct QueuedCall {
    sequence: u64,
    call: IpiCall,
}

struct CallQueue {
    calls: VecDeque<QueuedCall>,
    last_sequence: u64,
}

impl CallQueue {
    const fn new() -> Self {
        Self {
            calls: VecDeque::new(),
            last_sequence: 0,
        }
    }

    fn push(&mut self, call: IpiCall) -> u64 {
        self.last_sequence = self
            .last_sequence
            .checked_add(1)
            .expect("IPI sequence exhausted");
        let sequence = self.last_sequence;
        self.calls.push_back(QueuedCall { sequence, call });
        sequence
    }
}

cpu_local! {
    static CALL_QUEUES: SpinLock<CallQueue> = SpinLock::new(CallQueue::new());
    static LAST_COMPLETED_CALL: AtomicU64 = AtomicU64::new(0);
    static RETRY_IPI_DELIVERY: AtomicBool = AtomicBool::new(false);
}

fn enqueue_and_signal(
    queue: &SpinLock<CallQueue>,
    call: IpiCall,
    signal: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let sequence = {
        let mut queue = queue.lock();
        if queue.calls.len() >= MAX_PENDING_CALLS {
            return Err(crate::Error::NotEnoughResources);
        }
        queue.push(call)
    };

    if let Err(error) = signal() {
        let cancelled = {
            let mut queue = queue.lock();
            queue
                .calls
                .binary_search_by_key(&sequence, |call| call.sequence)
                .ok()
                .and_then(|index| queue.calls.remove(index))
        };
        if cancelled.is_some() {
            return Err(error);
        }
        // The receiver already claimed this call, possibly on an earlier IPI.
    }
    Ok(())
}

/// Handles inter-processor calls.
///
/// # Safety
///
/// This function must be called from an IRQ handler that can be triggered by
/// inter-processor interrupts.
pub(crate) unsafe fn do_inter_processor_call(_trapframe: &TrapFrame) {
    let irq_guard = irq::disable_local();
    drain_calls_on_current_cpu(&irq_guard);
}

pub(crate) fn retry_pending_ipi(irq_guard: &DisabledLocalIrqGuard) {
    if RETRY_IPI_DELIVERY
        .get_on_cpu(irq_guard.current_cpu())
        .load(Ordering::Relaxed)
    {
        drain_calls_on_current_cpu(irq_guard);
    }
}

fn drain_calls_on_current_cpu(irq_guard: &DisabledLocalIrqGuard) {
    let cpu = irq_guard.current_cpu();
    let needs_followup = handle_ipi_batch(
        CALL_QUEUES.get_on_cpu(cpu),
        LAST_COMPLETED_CALL.get_on_cpu(cpu),
    );
    // Rearm after a bounded batch even if several incoming IPIs coalesced.
    // The next local timer interrupt can drain another batch if rearming fails.
    let retry = needs_followup && IPI_SENDER.get().unwrap().send(cpu, irq_guard).is_err();
    RETRY_IPI_DELIVERY
        .get_on_cpu(cpu)
        .store(retry, Ordering::Relaxed);
}

fn handle_ipi_batch(queue: &SpinLock<CallQueue>, completed: &AtomicU64) -> bool {
    // Both the IPI and timer fallback run with local IRQs disabled, so only
    // one consumer can execute at a time. Defer calls added after entry.
    let last_at_entry = queue.lock().last_sequence;
    for _ in 0..CALLS_PER_INTERRUPT {
        let Some(QueuedCall { sequence, call }) = queue
            .lock()
            .calls
            .pop_front_if(|pending| pending.sequence <= last_at_entry)
        else {
            break;
        };
        match call {
            IpiCall::Function(call_fn) => call_fn(),
            IpiCall::Closure(call_fn) => call_fn(),
        }
        completed.store(sequence, Ordering::Release);
    }
    !queue.lock().calls.is_empty()
}

pub(super) fn init() {
    IPI_SENDER.call_once(|| {
        let hw_cpu_ids = crate::boot::smp::construct_hw_cpu_id_mapping();
        IpiSender { hw_cpu_ids }
    });
}

#[cfg(ktest)]
mod test {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        Arc, AtomicU64, Box, CALLS_PER_INTERRUPT, CallQueue, IpiCall, SpinLock, enqueue_and_signal,
        handle_ipi_batch,
    };
    use crate::{
        cpu::{self, PinCurrentCpu},
        prelude::ktest,
        task,
    };

    static IPI_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn count_ipi_call() {
        IPI_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[ktest]
    fn pending_ipis_waits_for_all_cpus() {
        let before = IPI_CALL_COUNT.load(Ordering::Relaxed);

        super::inter_processor_call(&cpu::CpuSet::new_full(), count_ipi_call).wait();

        assert_eq!(
            IPI_CALL_COUNT.load(Ordering::Relaxed) - before,
            cpu::num_cpus()
        );
    }

    #[ktest]
    fn inter_processor_call_runs_on_current_cpu() {
        let preempt_guard = task::disable_preempt();
        let before = IPI_CALL_COUNT.load(Ordering::Relaxed);

        super::inter_processor_call(
            &cpu::CpuSet::from(preempt_guard.current_cpu()),
            count_ipi_call,
        )
        .wait();

        assert_eq!(IPI_CALL_COUNT.load(Ordering::Relaxed) - before, 1);
    }

    #[ktest]
    fn pending_ipis_waits_for_remote_cpu() {
        if cpu::num_cpus() < 2 {
            return;
        }

        let preempt_guard = task::disable_preempt();
        let target_cpu = cpu::all_cpus()
            .find(|cpu_id| *cpu_id != preempt_guard.current_cpu())
            .unwrap();
        let before = IPI_CALL_COUNT.load(Ordering::Relaxed);

        super::inter_processor_call(&cpu::CpuSet::from(target_cpu), count_ipi_call).wait();

        assert_eq!(IPI_CALL_COUNT.load(Ordering::Relaxed) - before, 1);
    }

    #[ktest]
    fn pending_ipis_can_be_extended() {
        if cpu::num_cpus() < 2 {
            return;
        }

        let preempt_guard = task::disable_preempt();
        let target_cpu = cpu::all_cpus()
            .find(|cpu_id| *cpu_id != preempt_guard.current_cpu())
            .unwrap();
        let target_cpus = cpu::CpuSet::from(target_cpu);
        let before = IPI_CALL_COUNT.load(Ordering::Relaxed);

        let mut pending_ipis = super::PendingIpis::new_empty();
        pending_ipis.extend(&super::inter_processor_call(&target_cpus, count_ipi_call));
        pending_ipis.extend(&super::inter_processor_call(&target_cpus, count_ipi_call));
        pending_ipis.wait();

        assert_eq!(IPI_CALL_COUNT.load(Ordering::Relaxed) - before, 2);
    }

    #[ktest]
    fn ipi_handler_defers_new_calls_and_publishes_completed_sequence() {
        let queue = Arc::new(SpinLock::new(CallQueue::new()));
        let completed = AtomicU64::new(0);
        let producer = queue.clone();
        let sequence = queue.lock().push(IpiCall::Closure(Box::new(move || {
            producer.lock().push(IpiCall::Function(|| {}));
        })));

        assert!(handle_ipi_batch(&queue, &completed));
        assert_eq!(queue.lock().calls.len(), 1);
        assert_eq!(completed.load(Ordering::Acquire), sequence);

        assert!(!handle_ipi_batch(&queue, &completed));
        assert_eq!(completed.load(Ordering::Acquire), sequence + 1);
    }

    #[ktest]
    fn ipi_handler_limits_legacy_batch() {
        let queue = SpinLock::new(CallQueue::new());
        let completed = AtomicU64::new(0);
        for _ in 0..=CALLS_PER_INTERRUPT {
            queue.lock().push(IpiCall::Function(|| {}));
        }

        assert!(handle_ipi_batch(&queue, &completed));
        assert_eq!(
            completed.load(Ordering::Acquire),
            CALLS_PER_INTERRUPT as u64
        );
        assert!(!handle_ipi_batch(&queue, &completed));
        assert_eq!(
            completed.load(Ordering::Acquire),
            CALLS_PER_INTERRUPT as u64 + 1
        );
    }

    #[ktest]
    fn ipi_rearms_a_coalesced_batch() {
        if cpu::num_cpus() < 2 {
            return;
        }
        let irq_guard = crate::irq::disable_local();
        let target = cpu::all_cpus()
            .find(|cpu_id| *cpu_id != irq_guard.current_cpu())
            .unwrap();
        let before = IPI_CALL_COUNT.load(Ordering::Relaxed);
        let count = CALLS_PER_INTERRUPT * 2 + 1;
        let sequence = {
            let mut queue = super::CALL_QUEUES.get_on_cpu(target).lock();
            for _ in 0..count {
                queue.push(IpiCall::Function(count_ipi_call));
            }
            queue.last_sequence
        };
        super::IPI_SENDER
            .get()
            .unwrap()
            .send(target, &irq_guard)
            .unwrap();
        drop(irq_guard);

        let deadline = crate::timer::Jiffies::elapsed().as_u64() + crate::timer::TIMER_FREQ;
        let completed = super::LAST_COMPLETED_CALL.get_on_cpu(target);
        while completed.load(Ordering::Acquire) < sequence {
            assert!(crate::timer::Jiffies::elapsed().as_u64() < deadline);
            core::hint::spin_loop();
        }
        assert_eq!(IPI_CALL_COUNT.load(Ordering::Relaxed) - before, count);
    }

    #[ktest]
    fn failed_ipi_cancels_only_unclaimed_calls() {
        let queue = SpinLock::new(CallQueue::new());
        let completed = AtomicU64::new(0);
        let failed = || Err(crate::Error::IoError);

        assert!(enqueue_and_signal(&queue, IpiCall::Function(|| {}), failed).is_err());
        assert!(queue.lock().calls.is_empty());

        assert!(
            enqueue_and_signal(&queue, IpiCall::Function(|| {}), || {
                assert!(!handle_ipi_batch(&queue, &completed));
                failed()
            })
            .is_ok()
        );
        assert_eq!(completed.load(Ordering::Acquire), 2);
    }
}
