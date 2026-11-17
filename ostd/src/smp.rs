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

use alloc::{boxed::Box, collections::VecDeque, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

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
/// Returns an error if the destination queue is full or a remote IPI cannot be sent.
/// The function is not executed in those cases.
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
/// A remote call returns after the IPI is sent.
///
/// # Errors
///
/// Returns an error if the destination queue is full or a remote IPI cannot be sent.
/// The function is not executed in those cases.
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
    cpus: CpuSet,
}

impl PendingIpis {
    pub(crate) fn new_empty() -> Self {
        Self {
            cpus: CpuSet::new_empty(),
        }
    }

    fn add(&mut self, cpu_id: CpuId) {
        self.cpus.add(cpu_id);
    }

    pub(crate) fn extend(&mut self, other: &Self) {
        for cpu_id in other.cpus.iter() {
            self.add(cpu_id);
        }
    }

    /// Waits until all pending remote processors have handled their IPIs.
    ///
    /// # Panics
    ///
    /// This method panics if local IRQs are disabled.
    /// Waiting for remote IPIs
    /// with local IRQs disabled can deadlock if one of the remote processors is
    /// also waiting for this processor to handle an IPI.
    pub fn wait(&self) {
        assert!(
            crate::arch::irq::is_local_enabled(),
            "waiting for remote inter-processor calls with IRQs disabled"
        );

        for cpu_id in self.cpus.iter() {
            // Wait until there are no pending IPIs on the target CPU.
            //
            // Note that if new IPIs arrive to that CPU in the meantime, we
            // will also wait for them. This is fine because there usually
            // aren't too many IPIs in common cases.
            while HAS_PENDING_IPIS.get_on_cpu(cpu_id).load(Ordering::Acquire) {
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
            self.enqueue(cpu_id, IpiCall::Function(call_fn));
            pending_ipis.add(cpu_id);
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

    fn enqueue(&self, cpu: CpuId, call: IpiCall) {
        let mut queue = CALL_QUEUES.get_on_cpu(cpu).lock();
        Self::push(cpu, &mut queue, call);
    }

    fn submit(&self, cpu: CpuId, call: IpiCall, irq_guard: &DisabledLocalIrqGuard) -> Result<()> {
        Self::signal_and_enqueue(cpu, call, || self.send(cpu, irq_guard))
    }

    fn signal_and_enqueue(
        cpu: CpuId,
        call: IpiCall,
        signal: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        const MAX_PENDING_CALLS: usize = 64;
        let mut queue = CALL_QUEUES.get_on_cpu(cpu).lock();
        if queue.len() >= MAX_PENDING_CALLS {
            return Err(crate::Error::NotEnoughResources);
        }
        // The receiver cannot inspect the queue before successful delivery
        // is committed. A failed send leaves no callback to run on a later IPI.
        signal()?;
        Self::push(cpu, &mut queue, call);
        Ok(())
    }

    fn push(cpu: CpuId, queue: &mut VecDeque<IpiCall>, call: IpiCall) {
        queue.push_back(call);
        // Set the flag while holding the queue lock, as the receiver clears it there.
        HAS_PENDING_IPIS
            .get_on_cpu(cpu)
            .store(true, Ordering::Release);
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

cpu_local! {
    static CALL_QUEUES: SpinLock<VecDeque<IpiCall>> = SpinLock::new(VecDeque::new());
    static HAS_PENDING_IPIS: AtomicBool = AtomicBool::new(false);
}

/// Handles inter-processor calls.
///
/// # Safety
///
/// This function must be called from an IRQ handler that can be triggered by
/// inter-processor interrupts.
pub(crate) unsafe fn do_inter_processor_call(_trapframe: &TrapFrame) {
    let this_cpu_id = CpuId::current_racy();
    handle_ipi_batch(
        CALL_QUEUES.get_on_cpu(this_cpu_id),
        HAS_PENDING_IPIS.get_on_cpu(this_cpu_id),
    );
}

fn handle_ipi_batch(queue: &SpinLock<VecDeque<IpiCall>>, pending: &AtomicBool) {
    // The IRQ handler is the only consumer on this CPU.
    // Bound this invocation to the calls already queued on entry.
    // Later enqueues have their own IPI pending after the current acknowledgement.
    let batch_len = queue.lock().len();
    for _ in 0..batch_len {
        let call = queue.lock().pop_front().unwrap();
        match call {
            IpiCall::Function(call_fn) => call_fn(),
            IpiCall::Closure(call_fn) => call_fn(),
        }
    }

    let queue = queue.lock();
    if queue.is_empty() {
        pending.store(false, Ordering::Release);
    }
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

    use super::{Arc, AtomicBool, Box, IpiCall, SpinLock, VecDeque, handle_ipi_batch};
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
    fn ipi_handler_leaves_new_calls_for_the_next_interrupt() {
        let queue = Arc::new(SpinLock::new(VecDeque::new()));
        let pending = AtomicBool::new(true);
        let producer = queue.clone();
        queue.lock().push_back(IpiCall::Closure(Box::new(move || {
            producer.lock().push_back(IpiCall::Function(|| {}));
        })));

        handle_ipi_batch(&queue, &pending);
        assert_eq!(queue.lock().len(), 1);
        assert!(pending.load(Ordering::Acquire));

        handle_ipi_batch(&queue, &pending);
        assert!(queue.lock().is_empty());
        assert!(!pending.load(Ordering::Acquire));
    }

    #[ktest]
    fn failed_ipi_does_not_leave_callback_queued() {
        if cpu::num_cpus() < 2 {
            return;
        }
        let irq_guard = crate::irq::disable_local();
        let target_cpu = cpu::all_cpus()
            .find(|cpu_id| *cpu_id != irq_guard.current_cpu())
            .unwrap();
        let before = IPI_CALL_COUNT.load(Ordering::Relaxed);
        assert!(
            super::IpiSender::signal_and_enqueue(
                target_cpu,
                IpiCall::Function(count_ipi_call),
                || Err(crate::Error::IoError),
            )
            .is_err()
        );
        drop(irq_guard);

        let called_on = super::call_on_cpu(target_cpu, cpu::CpuId::current_racy).unwrap();
        assert_eq!(called_on, target_cpu);
        assert_eq!(IPI_CALL_COUNT.load(Ordering::Relaxed), before);
    }
}
