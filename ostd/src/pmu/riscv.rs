// SPDX-License-Identifier: MPL-2.0

//! RISC-V SBI PMU adapter with guarded CSR reads.

use core::arch::{asm, global_asm};

pub(super) use super::sbi_counter::Caps;
use super::{
    ConfigFailure, Event, EventConfig, PmuError, Result,
    sbi_counter::{self, CounterInfo, Firmware, HW_COUNTERS, SbiError, SbiResult, StopMode},
};
use crate::arch::{
    boot::DEVICE_TREE,
    cpu::extension::{self, IsaExtensions},
    trap::TrapFrame,
};

global_asm!(include_str!("riscv_csr.S"));

/// Called only for kernel-mode illegal instructions by the trap handler.
pub(crate) fn recover_register_fault(frame: &mut TrapFrame) -> bool {
    let start = __pmu_csr_sites as *const () as usize;
    let end = __pmu_csr_sites_end as *const () as usize;
    // The assembly table has one equally sized entry per counter.
    // Recover only at the CSR read at the start of each entry.
    let sites = start..end;
    if sites.contains(&frame.sepc) && (frame.sepc - start).is_multiple_of(sites.len() / HW_COUNTERS)
    {
        frame.sepc = __pmu_csr_failed as *const () as usize;
        true
    } else {
        false
    }
}

pub(super) fn probe() -> Result<Caps> {
    if !sbi_rt::probe_extension(sbi_rt::Pmu).is_available() {
        return Err(PmuError::Unsupported);
    }
    let implementation = sbi_rt::get_sbi_impl_id();
    let version = sbi_rt::get_sbi_impl_version();
    // SBI filter flags are hints. Advertise only the audited OpenSBI contract
    // and actual ISA filtering extensions; a build feature is not evidence.
    if !sbi_counter::supports_filter_contract(implementation, version)
        || !sbi_counter::supports_qemu_profile(
            is_qemu_virt(),
            extension::has_extensions(IsaExtensions::SMCNTRPMF),
            crate::cpu::num_cpus(),
            &crate::boot::boot_info().kernel_cmdline,
        )
    {
        return Err(PmuError::UnsupportedFilter);
    }
    let total = sbi_rt::pmu_num_counters().min(HW_COUNTERS);
    let mut infos = [None; HW_COUNTERS];
    for (index, info) in infos.iter_mut().take(total).enumerate() {
        *info = sbi_result(sbi_rt::pmu_counter_get_info(index))
            .ok()
            .and_then(|value| CounterInfo::decode(value).ok());
    }
    let caps = Caps::probe(infos, extension::has_extensions(IsaExtensions::SSCOFPMF))?;
    restrict_user_counter_access()?;
    Ok(caps)
}

fn is_qemu_virt() -> bool {
    // QEMU virt stores the cycle value, mode filter, and inhibit bits in each
    // vCPU's CPURISCVState, so even its fixed cycle counter is hart-local.
    // https://github.com/qemu/qemu/blob/v10.2.1/target/riscv/cpu.h
    // https://github.com/qemu/qemu/blob/v10.2.1/hw/riscv/virt.c
    DEVICE_TREE
        .get()
        .and_then(|tree| tree.find_node("/"))
        .and_then(|node| node.property("model"))
        .and_then(|model| model.as_str())
        == Some("riscv-virtio,qemu")
}

fn sbi_result(ret: sbi_rt::SbiRet) -> SbiResult<usize> {
    if ret.error == 0 {
        Ok(ret.value)
    } else {
        Err(SbiError(ret.error as isize))
    }
}

fn restrict_user_counter_access() -> Result<()> {
    const PERF_COUNTER_BITS: usize = !(1usize << 1);
    let scounteren: usize;
    // Revoke access before publishing the capabilities and preserve the time bit.
    // SAFETY: This is a hart-local S-mode register and the caller has IRQs disabled.
    unsafe {
        asm!(
            "csrrc zero, scounteren, {mask}",
            "csrr {remaining}, scounteren",
            mask = in(reg) PERF_COUNTER_BITS,
            remaining = out(reg) scounteren,
            options(nomem, nostack),
        )
    };
    if scounteren & PERF_COUNTER_BITS != 0 {
        return Err(PmuError::Unsupported);
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct Sbi;

impl Firmware for Sbi {
    fn configure(mask: u32, flags: usize, event: Event) -> SbiResult<usize> {
        let event_idx = match event {
            Event::CpuCycles => 1,
            Event::Instructions => 2,
        };
        sbi_result(sbi_rt::pmu_counter_config_matching(
            0,
            mask as usize,
            flags,
            event_idx,
            0,
        ))
    }

    fn start(index: u32) -> SbiResult<()> {
        sbi_result(sbi_rt::pmu_counter_start(index as usize, 1, 0usize, 0)).map(|_| ())
    }

    fn stop(index: u32, mode: StopMode) -> SbiResult<()> {
        sbi_result(sbi_rt::pmu_counter_stop(
            index as usize,
            1,
            usize::from(mode == StopMode::ResetMapping),
        ))
        .map(|_| ())
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
    caps: &Caps,
    used_mask: u64,
) -> core::result::Result<(Token, u64), ConfigFailure<Token>> {
    sbi_counter::configure::<Sbi>(config, caps, used_mask)
}

unsafe extern "C" {
    /// # Safety
    ///
    /// The index must be in `0..32` and select a permitted PMU CSR.
    /// The caller must pin the CPU, disable local IRQs, and supply an aligned
    /// pointer writable for one `u64`.
    fn __pmu_read_counter_csr(index: u32, output: *mut u64) -> u32;
    fn __pmu_csr_sites();
    fn __pmu_csr_sites_end();
    fn __pmu_csr_failed();
}
