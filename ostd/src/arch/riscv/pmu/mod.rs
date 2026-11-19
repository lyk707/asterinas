// SPDX-License-Identifier: MPL-2.0

//! RISC-V SBI PMU adapter with guarded CSR reads.

use core::arch::{asm, global_asm};

pub(crate) use self::counter::Caps;
use self::counter::{CounterInfo, HW_COUNTERS, SbiError, SbiResult, StopMode};
use crate::{
    arch::{boot::DEVICE_TREE, trap::TrapFrame},
    pmu::{ConfigFailure, Event, EventConfig, PmuError, Result},
};

mod counter;

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

pub(crate) fn probe() -> Result<Caps> {
    if !sbi_rt::probe_extension(sbi_rt::Pmu).is_available() {
        return Err(PmuError::Unsupported);
    }
    let total = sbi_rt::pmu_num_counters();
    let mut infos = alloc::vec::Vec::new();
    infos
        .try_reserve_exact(total)
        .map_err(|_| PmuError::HardwareFault)?;
    for index in 0..total {
        let info = match sbi_result(sbi_rt::pmu_counter_get_info(index)) {
            Ok(value) => Some(CounterInfo::decode(value)?),
            // A sparse SBI index is not a hardware counter.
            Err(SbiError(-3)) => None,
            Err(error) => return Err(error.into()),
        };
        infos.push(info);
    }
    restrict_user_counter_access()?;
    Ok(Caps::new(
        infos,
        has_counter_extension("smcntrpmf"),
        has_counter_extension("sscofpmf"),
    ))
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
pub(crate) struct Sbi;

impl Sbi {
    fn configure(base: usize, mask: usize, flags: usize, event: Event) -> SbiResult<usize> {
        let event_idx = match event {
            Event::CpuCycles => 1,
            Event::Instructions => 2,
        };
        sbi_result(sbi_rt::pmu_counter_config_matching(
            base, mask, flags, event_idx, 0,
        ))
    }

    fn start(index: usize) -> SbiResult<()> {
        sbi_result(sbi_rt::pmu_counter_start(index, 1, 0usize, 0)).map(|_| ())
    }

    fn stop(index: usize, mode: StopMode) -> SbiResult<()> {
        sbi_result(sbi_rt::pmu_counter_stop(
            index,
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

pub(crate) type Token = counter::Token;

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
    /// The index must be in `0..32` and select a permitted PMU CSR.
    /// The caller must pin the CPU, disable local IRQs, and supply an aligned
    /// pointer writable for one `u64`.
    fn __pmu_read_counter_csr(index: u32, output: *mut u64) -> u32;
    fn __pmu_csr_sites();
    fn __pmu_csr_sites_end();
    fn __pmu_csr_failed();
}

// Filtering capabilities are hart-local, just like the SBI inventory.
fn has_counter_extension(name: &str) -> bool {
    let Some(tree) = DEVICE_TREE.get() else {
        return false;
    };
    let hart = crate::arch::boot::smp::get_current_hart_id() as usize;
    let Some(cpu) = tree
        .cpus()
        .find(|cpu| cpu.property("reg").and_then(|reg| reg.as_usize()) == Some(hart))
    else {
        return false;
    };
    if let Some(list) = cpu.property("riscv,isa-extensions") {
        list.value
            .split(|&byte| byte == 0)
            .any(|ext| ext == name.as_bytes())
    } else {
        cpu.property("riscv,isa")
            .and_then(|isa| isa.as_str())
            .is_some_and(|isa| isa.split('_').any(|ext| ext == name))
    }
}
