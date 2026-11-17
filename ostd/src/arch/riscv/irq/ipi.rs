// SPDX-License-Identifier: MPL-2.0

//! Inter-processor interrupts.

use spin::Once;

use crate::{cpu::PinCurrentCpu, error::Error, irq::IrqLine, prelude::Result};

/// Hardware-specific, architecture-dependent CPU ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HwCpuId(u32);

impl HwCpuId {
    pub(crate) fn read_current(_guard: &dyn PinCurrentCpu) -> Self {
        // No races because of `_guard`.
        Self(crate::arch::boot::smp::get_current_hart_id())
    }
}

pub(in crate::arch) static IPI_IRQ: Once<IrqLine> = Once::new();

/// Initializes the global IPI-related state and local state on the BSP.
///
/// # Safety
///
/// This function can only be called on the BSP and before any other
/// IPI-related function is called.
pub(in crate::arch) unsafe fn init_on_bsp() {
    let mut irq = IrqLine::alloc().unwrap();
    // SAFETY: This will be called upon an inter-processor interrupt.
    irq.on_active(|f| unsafe { crate::smp::do_inter_processor_call(f) });
    IPI_IRQ.call_once(|| irq);

    // SAFETY: Enabling the software interrupts is safe here because this
    // function cannot be called when others can perform IPI-related
    // operations. And it has no side-effects.
    unsafe { riscv::register::sie::set_ssoft() };
}

/// Initializes the IPI-related state on this AP.
///
/// # Safety
///
/// This function can only be called before any other harts can send IPIs to
/// this application hart.
pub(in crate::arch) unsafe fn init_on_ap() {
    // SAFETY: Enabling the software interrupts is safe here due to the same
    // reasons mentioned in `init_on_bsp`.
    unsafe { riscv::register::sie::set_ssoft() };
}

/// Sends a general inter-processor interrupt (IPI) to the specified CPU.
pub(crate) fn send_ipi(hw_cpu_id: HwCpuId, _guard: &dyn PinCurrentCpu) -> Result<()> {
    const XLEN: usize = usize::BITS as usize;
    const XLEN_MASK: usize = XLEN - 1;

    let hart_id = hw_cpu_id.0 as usize;
    let hart_mask_base = hart_id & !XLEN_MASK;
    let hart_mask = 1 << (hart_id & XLEN_MASK);

    let ret = sbi_rt::send_ipi(sbi_rt::HartMask::from_mask_base(hart_mask, hart_mask_base));

    if ret.error == 0 {
        crate::debug!("Successfully sent IPI to hart {}", hw_cpu_id.0);
        Ok(())
    } else {
        crate::error!(
            "Failed to send IPI to hart {}: error code {}",
            hw_cpu_id.0,
            ret.error
        );
        Err(Error::IoError)
    }
}

#[cfg(ktest)]
mod tests {
    use alloc::sync::Arc;
    use core::{
        hint::spin_loop,
        sync::atomic::{AtomicBool, Ordering},
    };

    use crate::{
        cpu::{CpuId, CpuSet, PinCurrentCpu, all_cpus},
        prelude::ktest,
        smp, task,
        timer::{Jiffies, TIMER_FREQ},
    };

    #[ktest]
    fn software_ipi_raised_during_dispatch_is_delivered() {
        let preempt_guard = task::disable_preempt();
        let Some(target) = all_cpus().find(|cpu| *cpu != preempt_guard.current_cpu()) else {
            return;
        };
        let delivered = Arc::new(AtomicBool::new(false));
        let delivered_on_target = delivered.clone();
        let reasserted = AtomicBool::new(false);

        let mut irq = super::IPI_IRQ.get().unwrap().clone();
        irq.on_active(move |_| {
            if CpuId::current_racy() != target {
                return;
            }
            if reasserted.swap(true, Ordering::Relaxed) {
                delivered_on_target.store(true, Ordering::Release);
            } else {
                // This callback runs after the IPI queue callback. Raise SSIP
                // directly to reproduce the arrival of a second notification.
                // SAFETY: This only raises the current hart's software
                // interrupt, which is handled once local IRQs are re-enabled.
                unsafe { riscv::register::sip::set_ssoft() };
            }
        });

        let deadline = Jiffies::elapsed().as_u64() + TIMER_FREQ;
        let _ = smp::inter_processor_call(&CpuSet::from(target), || {});
        while !delivered.load(Ordering::Acquire) {
            assert!(
                Jiffies::elapsed().as_u64() < deadline,
                "software IPI raised during dispatch was lost"
            );
            spin_loop();
        }
    }
}
