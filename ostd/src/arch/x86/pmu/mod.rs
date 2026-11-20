// SPDX-License-Identifier: MPL-2.0

//! x86-64 PMU backend for Intel architectural fixed counters and AMD Zen core counters.
//!
//! Register layouts and event encodings follow:
//! - Intel 64 and IA-32 Architectures Software Developer's Manual, Volume 3B,
//!   Chapter 20 ("Performance Monitoring").
//! - AMD64 Architecture Programmer's Manual, Volume 2, Section 13.2
//!   ("Core Performance Counters").

use core::arch::{asm, global_asm};

pub(crate) use self::counter::Caps;
use crate::{
    arch::trap::TrapFrame,
    pmu::{ConfigFailure, EventConfig, PmuError, Result},
};

mod amd;
mod counter;
mod intel;

global_asm!(include_str!("x86_msr.S"));

/// Invalid Opcode (`#UD`) exception vector on x86-64.
const X86_TRAP_UD: usize = 6;
/// General Protection (`#GP`) exception vector on x86-64.
const X86_TRAP_GP: usize = 13;

/// `CR4.PCE` (Performance-Monitoring Counter Enable, bit 8).
const CR4_PCE_BIT: usize = 1 << 8;

/// Recovers from `#GP` or `#UD` faults triggered at the PMU MSR access sites.
pub(crate) fn recover_register_fault(frame: &mut TrapFrame) -> bool {
    if !matches!(frame.trap_num, X86_TRAP_UD | X86_TRAP_GP) {
        return false;
    }

    if frame.rip == __pmu_rdmsr_site as *const () as usize {
        frame.rip = __pmu_rdmsr_failed as *const () as usize;
        true
    } else if frame.rip == __pmu_wrmsr_site as *const () as usize {
        frame.rip = __pmu_wrmsr_failed as *const () as usize;
        true
    } else {
        false
    }
}

/// Probes the current x86-64 CPU for a supported hardware PMU interface.
pub(crate) fn probe() -> Result<Caps> {
    crate::arch::if_tdx_enabled!({
        return Err(PmuError::Unsupported);
    });

    let cr4: usize;
    // SAFETY: Reading CR4 at CPL0 has no side effects and cannot fault.
    unsafe {
        asm!("mov {}, cr4", out(reg) cr4, options(nostack, nomem));
    }
    // Reject platforms where user-space `rdpmc` is globally enabled in CR4.
    if cr4 & CR4_PCE_BIT != 0 {
        return Err(PmuError::Unsupported);
    }

    let base_leaf = cpuid(0, 0).ok_or(PmuError::Unsupported)?;
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&base_leaf.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&base_leaf.edx.to_le_bytes());
    vendor[8..12].copy_from_slice(&base_leaf.ecx.to_le_bytes());

    if &vendor == b"GenuineIntel" {
        return intel::probe().map(Caps::Intel);
    }
    if &vendor == b"AuthenticAMD" {
        return amd::probe().map(Caps::Amd);
    }

    Err(PmuError::Unsupported)
}

pub(crate) use counter::Token;

pub(crate) fn configure(
    config: EventConfig,
    caps: &Caps,
    used: &[Option<usize>],
) -> core::result::Result<(Token, u64), ConfigFailure<Token>> {
    counter::configure(config, caps, used)
}

unsafe extern "C" {
    /// # Safety
    ///
    /// The MSR must be a PMU register on the current CPU.
    /// The caller must serialize access to that register and supply an aligned
    /// pointer writable for one `u64`.
    fn __pmu_read_msr(msr: u32, value: *mut u64) -> u32;
    /// # Safety
    ///
    /// The MSR must be a PMU register on the current CPU.
    /// The value must obey its register and ownership constraints.
    /// The caller must serialize access to the register.
    fn __pmu_write_msr(msr: u32, value: u64) -> u32;
    fn __pmu_rdmsr_site();
    fn __pmu_rdmsr_failed();
    fn __pmu_wrmsr_site();
    fn __pmu_wrmsr_failed();
}

fn read_msr(msr: u32) -> Result<u64> {
    let mut value = 0u64;
    // SAFETY: `__pmu_read_msr` writes one `u64` to `&mut value`. Only architectural
    // PMU MSRs are passed, and `#GP`/`#UD` faults at `__pmu_rdmsr_site` are caught
    // by `recover_register_fault`.
    let status = unsafe { __pmu_read_msr(msr, &mut value) };
    if status == 0 {
        Ok(value)
    } else {
        Err(PmuError::HardwareFault)
    }
}

fn write_msr(msr: u32, value: u64) -> Result<()> {
    // SAFETY: Only validated PMU control/overflow values are written with local
    // IRQs disabled and the CPU-local PMU lock held. Faults at `__pmu_wrmsr_site`
    // are caught by `recover_register_fault`.
    let status = unsafe { __pmu_write_msr(msr, value) };
    if status == 0 {
        Ok(())
    } else {
        Err(PmuError::HardwareFault)
    }
}

// Leaf bounds must be read on this CPU rather than cached across heterogeneous CPUs.
fn cpuid(leaf: u32, subleaf: u32) -> Option<core::arch::x86_64::CpuidResult> {
    let base = if leaf >= 0x8000_0000 { 0x8000_0000 } else { 0 };
    let max = core::arch::x86_64::__cpuid_count(base, 0).eax;
    (leaf <= max).then(|| core::arch::x86_64::__cpuid_count(leaf, subleaf))
}
