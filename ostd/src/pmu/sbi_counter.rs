// SPDX-License-Identifier: MPL-2.0

//! SBI counter ownership and transactions, independent of the `ecall` adapter.
//!
//! `stop` captures the final value but keeps the firmware mapping for a later
//! restart.
//! `release` must confirm a successful STOP+RESET before relinquishing ownership.
//! `SBI_ERR_ALREADY_STOPPED` alone does not confirm a reset.

use core::marker::PhantomData;

use super::{BackendKind, ConfigFailure, Event, EventConfig, PmuCaps, PmuError, Result};

// OpenSBI v1.4 implements the Sscofpmf and Smcntrpmf filters used here.
// https://github.com/riscv-software-src/opensbi/blob/v1.4/lib/sbi/sbi_pmu.c#L520-L615
pub(super) fn supports_filter_contract(implementation: usize, version: usize) -> bool {
    implementation == 1 && version >= 0x0001_0004
}

pub(super) fn supports_qemu_profile(
    is_qemu_virt: bool,
    has_fixed_filter: bool,
    num_cpus: usize,
    cmdline: &str,
) -> bool {
    // Sscofpmf alone also appears on QEMU versions that ignore mode filters.
    // QEMU's instruction clock is global across vCPUs.
    // Use a single vCPU, and require the trusted launcher to enable -icount.
    // https://github.com/qemu/qemu/blob/v10.2.1/accel/tcg/icount-common.c
    is_qemu_virt
        && has_fixed_filter
        && num_cpus == 1
        && cmdline
            .split_ascii_whitespace()
            .any(|arg| arg == "ostd.riscv_pmu=icount")
}

// SBI PMU error codes and configuration flags:
// https://github.com/riscv-non-isa/riscv-sbi-doc/blob/v3.0/src/ext-pmu.adoc
const ALREADY_STARTED: isize = -7;
const ALREADY_STOPPED: isize = -8;
const MINH: usize = 1 << 7;
const SINH: usize = 1 << 6;
const UINH: usize = 1 << 5;
const VSINH: usize = 1 << 4;
const VUINH: usize = 1 << 3;
const CYCLE_CSR: u32 = 0xc00;
const INSTRET_CSR: u32 = 0xc02;
pub(super) const HW_COUNTERS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SbiError(pub(super) isize);

impl From<SbiError> for PmuError {
    fn from(error: SbiError) -> Self {
        match error.0 {
            -2 => Self::Unsupported,
            -3 => Self::InvalidConfig,
            _ => Self::HardwareFault,
        }
    }
}

pub(super) type SbiResult<T> = core::result::Result<T, SbiError>;

pub(super) trait Firmware: core::fmt::Debug {
    fn configure(mask: u32, flags: usize, event: Event) -> SbiResult<usize>;
    fn start(index: u32) -> SbiResult<()>;
    fn stop(index: u32, mode: StopMode) -> SbiResult<()>;
    fn read_counter(csr: u32) -> Result<u64>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StopMode {
    KeepMapping,
    ResetMapping,
}

#[derive(Debug)]
pub(super) struct Caps {
    infos: [Option<CounterInfo>; HW_COUNTERS],
    programmable: u32,
    fixed_cycles: u32,
    fixed_instructions: u32,
    width_bits: u8,
}

impl Caps {
    /// Discovers counters after the platform and filtering contract have been verified.
    pub(super) fn probe(
        infos: [Option<CounterInfo>; HW_COUNTERS],
        has_hpm_filter: bool,
    ) -> Result<Self> {
        let mut caps = Self {
            infos,
            programmable: 0,
            fixed_cycles: 0,
            fixed_instructions: 0,
            width_bits: 64,
        };
        for (index, info) in infos.into_iter().enumerate() {
            // In OpenSBI the hardware index is the low five bits of the CSR.
            let Some(info) = info.filter(|info| info.csr == CYCLE_CSR + index as u32) else {
                continue;
            };
            let bit = 1 << index;
            match info.csr {
                CYCLE_CSR => caps.fixed_cycles = bit,
                INSTRET_CSR => caps.fixed_instructions = bit,
                csr if csr > INSTRET_CSR && has_hpm_filter => caps.programmable |= bit,
                _ => continue,
            }
            caps.width_bits = caps.width_bits.min(info.width_bits);
        }
        if caps.fixed_cycles == 0 || caps.fixed_instructions == 0 {
            return Err(PmuError::HardwareFault);
        }
        Ok(caps)
    }

    pub(super) fn summary(&self) -> PmuCaps {
        PmuCaps {
            backend: BackendKind::Sbi,
            counters: 2,
            width_bits: self.width_bits,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CounterInfo {
    pub(super) csr: u32,
    pub(super) width_bits: u8,
}

impl CounterInfo {
    pub(super) fn decode(value: usize) -> Result<Self> {
        // Firmware counters, reserved bits, time, and invalid widths must not
        // reach the CSR reader or shift/mask arithmetic.
        if value & !0x3ffff != 0 {
            return Err(PmuError::HardwareFault);
        }
        let csr = (value & 0xfff) as u32;
        let width_bits = ((value >> 12) & 0x3f) as u8 + 1;
        if !(0xc00..=0xc1f).contains(&csr) || csr == 0xc01 || !(32..=64).contains(&width_bits) {
            return Err(PmuError::HardwareFault);
        }
        Ok(Self { csr, width_bits })
    }
}

/// Owns the mapping from successful CONFIG_MATCHING until successful RESET.
#[derive(Debug)]
pub(super) struct Token<I: Firmware> {
    index: u32,
    info: CounterInfo,
    has_mapping: bool,
    io: PhantomData<I>,
}

pub(super) fn configure<I: Firmware>(
    config: EventConfig,
    caps: &Caps,
    used_mask: u64,
) -> core::result::Result<(Token<I>, u64), ConfigFailure<Token<I>>> {
    // OpenSBI tries programmable matches first and otherwise uses the event's
    // fixed counter. Prefer fixed instret even with a free programmable slot so
    // instructions cannot consume the last programmable cycles counter.
    // https://github.com/riscv-software-src/opensbi/blob/v1.7/lib/sbi/sbi_pmu.c#L671-L740
    let (candidates, fixed_index) = match config.event() {
        Event::CpuCycles => (caps.programmable | caps.fixed_cycles, 0),
        Event::Instructions => (caps.fixed_instructions, 2),
    };
    let available = candidates & !(used_mask as u32);
    // Never ask firmware to match if its fixed fallback would write an owned counter.
    if available == 0 || used_mask & (1 << fixed_index) != 0 {
        return Err(PmuError::NoCounter.into());
    }
    let mut flags = MINH | VSINH | VUINH;
    if !config.is_user_counted() {
        flags |= UINH;
    }
    if !config.is_kernel_counted() {
        flags |= SINH;
    }
    let index = I::configure(available, flags, config.event()).map_err(PmuError::from)?;
    // Validate before narrowing or shifting; a malformed firmware result must
    // not alias a valid counter. With no trusted identity OSTD faults this PMU.
    if index >= HW_COUNTERS {
        return Err(PmuError::HardwareFault.into());
    }
    if available & (1u32 << index) == 0 {
        return Err(PmuError::HardwareFault.into());
    }
    let mut token = Token {
        index: index as u32,
        info: caps.infos[index].unwrap(),
        has_mapping: true,
        io: PhantomData,
    };
    // OpenSBI can leave fixed counters running at boot and after matching.
    // Establish the stopped state without resetting the mapping.
    let result = if token.info.csr <= INSTRET_CSR {
        token.stop()
    } else {
        token.read()
    };
    match result {
        Ok(initial_raw) => Ok((token, initial_raw)),
        Err(error) => Err(ConfigFailure {
            error,
            token: Some(token),
        }),
    }
}

impl<I: Firmware> Token<I> {
    pub(super) fn mask(&self) -> u64 {
        1u64 << self.index
    }

    pub(super) fn width_bits(&self) -> u8 {
        self.info.width_bits
    }

    pub(super) fn read(&self) -> Result<u64> {
        if !self.has_mapping {
            return Err(PmuError::InvalidHandle);
        }
        I::read_counter(self.info.csr)
    }

    pub(super) fn start(&mut self) -> Result<u64> {
        let initial_raw = self.read()?;
        I::start(self.index).map_err(PmuError::from)?;
        Ok(initial_raw)
    }

    pub(super) fn stop(&mut self) -> Result<u64> {
        if !self.has_mapping {
            return Err(PmuError::InvalidHandle);
        }
        match I::stop(self.index, StopMode::KeepMapping) {
            Ok(()) | Err(SbiError(ALREADY_STOPPED)) => self.read(),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn release(&mut self) -> Result<()> {
        if !self.has_mapping {
            return Ok(());
        }
        // OpenSBI versions differ on whether an already-stopped RESET removes
        // the mapping before reporting ALREADY_STOPPED. Avoid that ambiguous
        // operation entirely: establish running state before STOP+RESET.
        // This counter is still exclusively owned, IRQs are disabled, and the
        // logical final count was captured by stop(). Cleanup is not counted.
        // https://github.com/riscv-software-src/opensbi/blob/v1.7/lib/sbi/sbi_pmu.c#L484-L527
        match I::start(self.index) {
            Ok(()) | Err(SbiError(ALREADY_STARTED)) => {}
            Err(error) => return Err(error.into()),
        }
        I::stop(self.index, StopMode::ResetMapping).map_err(PmuError::from)?;
        // Only a confirmed successful STOP+RESET ends ownership. Any failure
        // leaves a token which collect_retired can retry, even after restart.
        self.has_mapping = false;
        Ok(())
    }
}
