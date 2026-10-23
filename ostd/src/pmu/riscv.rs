// SPDX-License-Identifier: MPL-2.0

//! RISC-V SBI PMU adapter with guarded CSR reads.

use core::arch::{asm, global_asm};

use super::{
    BackendKind, ConfigFailure, EventConfig, PmuCaps, PmuError, Result,
    sbi_counter::{self, CounterInfo, Firmware, GET_INFO, SBI_EXT_BASE, SBI_EXT_PMU, SbiError},
};
use crate::arch::{
    cpu::extension::{IsaExtensions, has_extensions},
    trap::TrapFrame,
};

global_asm!(include_str!("riscv_csr.S"));

/// Called only for kernel-mode illegal instructions by the trap handler.
pub(crate) fn recover_register_fault(frame: &mut TrapFrame) -> bool {
    let start = __pmu_csr_sites as *const () as usize;
    let end = __pmu_csr_sites_end as *const () as usize;
    // Each uncompressed entry has one CSR read and one jump. Recover only at
    // a read, never at a jump or an unrelated illegal instruction.
    if (start..end).contains(&frame.sepc) && (frame.sepc - start).is_multiple_of(8) {
        frame.sepc = __pmu_csr_failed as *const () as usize;
        true
    } else {
        false
    }
}

pub(super) fn probe() -> Result<PmuCaps> {
    let scounteren: usize;
    // SAFETY: S-mode may read its counter-access control register.
    unsafe { asm!("csrr {}, scounteren", out(reg) scounteren, options(nomem, nostack)) };
    // Allow user time reads, but never expose PMU data from other tasks.
    if scounteren & !(1 << 1) != 0 {
        return Err(PmuError::Unsupported);
    }
    if Sbi::call(SBI_EXT_BASE, 3, [SBI_EXT_PMU, 0, 0, 0, 0])? == 0 {
        return Err(PmuError::Unsupported);
    }
    let implementation = Sbi::call(SBI_EXT_BASE, 1, [0; 5])?;
    let version = Sbi::call(SBI_EXT_BASE, 2, [0; 5])?;
    // SBI filter flags are hints. Advertise only the audited OpenSBI contract
    // and actual ISA filtering extensions; a build feature is not evidence.
    if !sbi_counter::supports_filter_contract(implementation, version) {
        return Err(PmuError::UnsupportedFilter);
    }
    let fixed_filter = has_extensions(IsaExtensions::SMCNTRPMF);
    let hpm_filter = has_extensions(IsaExtensions::SSCOFPMF);
    let total = Sbi::call(SBI_EXT_PMU, 0, [0; 5])?.min(64);
    let mut candidates = 0u64;
    let mut width_bits = 64;
    for index in 0..total {
        let Ok(value) = Sbi::call(SBI_EXT_PMU, GET_INFO, [index, 0, 0, 0, 0]) else {
            continue;
        };
        let Ok(info) = CounterInfo::decode(value) else {
            continue;
        };
        if info.has_filter(fixed_filter, hpm_filter) {
            candidates |= 1u64 << index;
            width_bits = width_bits.min(info.width_bits);
        }
    }
    if candidates.count_ones() < 2 {
        return Err(PmuError::UnsupportedFilter);
    }
    Ok(PmuCaps {
        backend: BackendKind::Sbi,
        counters: 2,
        has_precise_filter: true,
        width_bits,
        has_global_control: false,
        candidates,
    })
}

#[derive(Debug)]
pub(super) struct Sbi;

impl Firmware for Sbi {
    fn call(eid: usize, fid: usize, args: [usize; 5]) -> core::result::Result<usize, SbiError> {
        let error: isize;
        let value: usize;
        // SAFETY: Base and PMU calls use the SBI ABI and no shared memory.
        // a0/a1 are the only registers changed by firmware.
        unsafe {
            asm!("ecall",
                inlateout("a0") args[0] => error, inlateout("a1") args[1] => value,
                in("a2") args[2], in("a3") args[3], in("a4") args[4],
                in("a6") fid, in("a7") eid, options(nomem, nostack),
            );
        }
        if error == 0 {
            Ok(value)
        } else {
            Err(SbiError(error))
        }
    }

    fn read_counter(csr: u32) -> Result<u64> {
        if !(0xc00..=0xc1f).contains(&csr) || csr == 0xc01 {
            return Err(PmuError::HardwareFault);
        }
        let mut value = 0;
        // SAFETY: The index selects a read-only CSR; value is a valid aligned
        // output pointer. Illegal accesses return an error through exact-site
        // recovery in the kernel trap handler.
        let status = unsafe { __pmu_read_counter_csr(csr - 0xc00, &mut value) };
        if status == 0 {
            Ok(value)
        } else {
            Err(PmuError::HardwareFault)
        }
    }
}

pub(super) type Token = sbi_counter::Token<Sbi>;

pub(super) fn configure(
    config: EventConfig,
    caps: PmuCaps,
    used_mask: u64,
) -> core::result::Result<(Token, u64), ConfigFailure<Token>> {
    sbi_counter::configure::<Sbi>(config, caps, used_mask)
}

unsafe extern "C" {
    fn __pmu_read_counter_csr(index: u32, output: *mut u64) -> u32;
    fn __pmu_csr_sites();
    fn __pmu_csr_sites_end();
    fn __pmu_csr_failed();
}
