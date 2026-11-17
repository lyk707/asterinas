// SPDX-License-Identifier: MPL-2.0

//! Inter-processor interrupts.

use spin::Once;

use crate::{cpu::PinCurrentCpu, irq::IrqLine, prelude::Result, smp};

/// Hardware-specific, architecture-dependent CPU ID.
///
/// This is the Local APIC ID in the x86_64 architecture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HwCpuId(u32);

impl HwCpuId {
    pub(crate) fn read_current(guard: &dyn PinCurrentCpu) -> Self {
        use crate::arch::kernel::apic;

        let apic = apic::get_or_init(guard);
        Self(apic.id())
    }
}

static IPI_IRQ: Once<IrqLine> = Once::new();

/// Initializes global IPI state.
pub(in crate::arch) fn init() {
    let mut irq = IrqLine::alloc().unwrap();
    // SAFETY: This will be called upon an inter-processor interrupt.
    irq.on_active(|f| unsafe { smp::do_inter_processor_call(f) });
    IPI_IRQ.call_once(|| irq);
}

/// Sends a general inter-processor interrupt (IPI) to the specified CPU.
pub(crate) fn send_ipi(hw_cpu_id: HwCpuId, guard: &dyn PinCurrentCpu) -> Result<()> {
    let irq_num = IPI_IRQ.get().unwrap().num();
    let apic = crate::arch::kernel::apic::get_or_init(guard);
    send_fixed_ipi(hw_cpu_id, irq_num, |icr| {
        // SAFETY: The CPU ID comes from the hardware topology.
        // The fixed vector is allocated for inter-processor calls.
        unsafe { apic.send_ipi(icr) }
    })
}

fn send_fixed_ipi(
    hw_cpu_id: HwCpuId,
    irq_num: u8,
    send: impl FnOnce(crate::arch::kernel::apic::Icr) -> Result<()>,
) -> Result<()> {
    use crate::arch::kernel::apic::{self, Icr};

    let icr = Icr::new(
        apic::ApicId::from(hw_cpu_id.0),
        apic::DestinationShorthand::NoShorthand,
        apic::TriggerMode::Edge,
        apic::Level::Assert,
        apic::DeliveryStatus::Idle,
        apic::DestinationMode::Physical,
        apic::DeliveryMode::Fixed,
        irq_num,
    );

    send(icr)
}

#[cfg(ktest)]
mod tests {
    use crate::prelude::*;

    #[ktest]
    fn fixed_ipi_adapter_propagates_apic_failure() {
        assert_eq!(
            super::send_fixed_ipi(super::HwCpuId(1), 0x40, |_| Err(crate::Error::IoError)),
            Err(crate::Error::IoError),
        );
    }
}
