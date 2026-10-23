// SPDX-License-Identifier: MPL-2.0

//! x86-64 PMU backend for Intel architectural fixed counters and AMD Zen core counters.
//!
//! Register layouts and event encodings follow:
//! - Intel 64 and IA-32 Architectures Software Developer's Manual, Volume 3B,
//!   Chapter 20 ("Performance Monitoring").
//! - AMD64 Architecture Programmer's Manual, Volume 2, Section 13.2
//!   ("Core Performance Counters").

use core::arch::{asm, global_asm};

use super::{BackendKind, ConfigFailure, EventConfig, PmuCaps, PmuError, Result};
use crate::arch::{cpu::cpuid::cpuid, trap::TrapFrame};

global_asm!(include_str!("x86_msr.S"));

/// Invalid Opcode (`#UD`) exception vector on x86-64.
const X86_TRAP_UD: usize = 6;
/// General Protection (`#GP`) exception vector on x86-64.
const X86_TRAP_GP: usize = 13;

/// `CR4.PCE` (Performance-Monitoring Counter Enable, bit 8).
const CR4_PCE_BIT: usize = 1 << 8;

/// Intel `IA32_FIXED_CTR_CTRL` MSR address.
const MSR_IA32_FIXED_CTR_CTRL: u32 = 0x038d;
/// Intel `IA32_PERF_GLOBAL_CTRL` MSR address.
const MSR_IA32_PERF_GLOBAL_CTRL: u32 = 0x038f;

/// Minimum supported Intel Architectural PMU version.
const INTEL_MIN_PMU_VERSION: u32 = 2;
/// Minimum required Intel fixed counters (`INST_RETIRED.ANY` and `CPU_CLK_UNHALTED.CORE`).
const INTEL_MIN_FIXED_COUNTERS: u32 = 2;
/// Valid bit-width range for hardware performance counters.
const MIN_COUNTER_WIDTH_BITS: u8 = 40;
const MAX_COUNTER_WIDTH_BITS: u8 = 64;

/// AMD `MSR_F15H_PERF_CTL0` (Core Performance Event-Select Register 0).
const MSR_AMD64_PERF_CTL0: u32 = 0xc001_0200;
/// Standard counter bit width on AMD Zen core counters.
const AMD_CORE_COUNTER_WIDTH_BITS: u8 = 48;

/// CPUID leaf for Intel Architectural Performance Monitoring.
const CPUID_INTEL_PMU_LEAF: u32 = 0x0000_000a;
/// CPUID leaf for maximum supported extended function.
const CPUID_EXT_MAX_LEAF: u32 = 0x8000_0000;
/// CPUID leaf for extended processor signature and feature bits.
const CPUID_EXT_FEATURES_LEAF: u32 = 0x8000_0001;
/// CPUID `Fn8000_0001_ECX[PerfCtrExtCore]` (bit 23).
const CPUID_AMD_PERFCTR_EXT_CORE_BIT: u32 = 1 << 23;
/// CPUID leaf for AMD Extended Performance Monitoring and Debug (`PerfMonV2`).
const CPUID_AMD_PMU_V2_LEAF: u32 = 0x8000_0022;
/// Supported AMD Zen family range (Family 17h through Family 1Ah).
const AMD_MIN_ZEN_FAMILY: u32 = 0x17;
const AMD_MAX_ZEN_FAMILY: u32 = 0x1a;

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
pub(super) fn probe() -> Result<PmuCaps> {
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
        return probe_intel();
    }
    if &vendor == b"AuthenticAMD" {
        return probe_amd();
    }

    Err(PmuError::Unsupported)
}

/// Hardware adapter; all transaction decisions live in `x86_counter`.
#[derive(Debug)]
pub(super) struct Msr;

impl super::x86_counter::Registers for Msr {
    fn read(msr: u32) -> Result<u64> {
        read_msr(msr)
    }

    fn write(msr: u32, value: u64) -> Result<()> {
        write_msr(msr, value)
    }
}

pub(super) type Token = super::x86_counter::Token<Msr>;

pub(super) fn configure(
    config: EventConfig,
    caps: PmuCaps,
    used_mask: u64,
) -> core::result::Result<(Token, u64), ConfigFailure<Token>> {
    super::x86_counter::configure::<Msr>(config, caps, used_mask)
}

// ---------------------------------------------------------------------------
// Internal probing and MSR helpers
// ---------------------------------------------------------------------------

unsafe extern "C" {
    fn __pmu_read_msr(msr: u32, value: *mut u64) -> u32;
    fn __pmu_write_msr(msr: u32, value: u64) -> u32;
    fn __pmu_rdmsr_site();
    fn __pmu_rdmsr_failed();
    fn __pmu_wrmsr_site();
    fn __pmu_wrmsr_failed();
}

fn probe_intel() -> Result<PmuCaps> {
    let pmu_leaf = cpuid(CPUID_INTEL_PMU_LEAF, 0).ok_or(PmuError::Unsupported)?;
    let version = pmu_leaf.eax & 0xff;
    let fixed_count = pmu_leaf.edx & 0x1f;
    let width_bits = ((pmu_leaf.edx >> 5) & 0xff) as u8;

    if version < INTEL_MIN_PMU_VERSION
        || fixed_count < INTEL_MIN_FIXED_COUNTERS
        || !(MIN_COUNTER_WIDTH_BITS..=MAX_COUNTER_WIDTH_BITS).contains(&width_bits)
    {
        return Err(PmuError::Unsupported);
    }

    read_msr(MSR_IA32_FIXED_CTR_CTRL)?;
    read_msr(MSR_IA32_PERF_GLOBAL_CTRL)?;

    Ok(PmuCaps {
        backend: BackendKind::IntelFixed,
        counters: 2,
        has_precise_filter: true,
        width_bits,
        has_global_control: true,
        candidates: 0b11,
    })
}

fn probe_amd() -> Result<PmuCaps> {
    let sig_eax = cpuid(1, 0).ok_or(PmuError::Unsupported)?.eax;
    let base_family = (sig_eax >> 8) & 0x0f;
    let family = if base_family == 0x0f {
        base_family + ((sig_eax >> 20) & 0xff)
    } else {
        base_family
    };

    let ext_max = cpuid(CPUID_EXT_MAX_LEAF, 0)
        .ok_or(PmuError::Unsupported)?
        .eax;
    if ext_max < CPUID_EXT_FEATURES_LEAF {
        return Err(PmuError::Unsupported);
    }

    let ext_features = cpuid(CPUID_EXT_FEATURES_LEAF, 0).ok_or(PmuError::Unsupported)?;
    if !(AMD_MIN_ZEN_FAMILY..=AMD_MAX_ZEN_FAMILY).contains(&family)
        || ext_features.ecx & CPUID_AMD_PERFCTR_EXT_CORE_BIT == 0
    {
        return Err(PmuError::Unsupported);
    }

    read_msr(MSR_AMD64_PERF_CTL0)?;
    read_msr(MSR_AMD64_PERF_CTL0 + 2)?;

    let v2_leaf = if ext_max >= CPUID_AMD_PMU_V2_LEAF {
        let leaf = cpuid(CPUID_AMD_PMU_V2_LEAF, 0).ok_or(PmuError::Unsupported)?;
        Some((leaf.eax, leaf.ebx))
    } else {
        None
    };
    let has_global_control = super::x86_counter::probe_global_control::<Msr>(v2_leaf)?;

    Ok(PmuCaps {
        backend: BackendKind::AmdZen,
        counters: 2,
        has_precise_filter: true,
        width_bits: AMD_CORE_COUNTER_WIDTH_BITS,
        has_global_control,
        candidates: 0b11,
    })
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
