// SPDX-License-Identifier: MPL-2.0

//! SBI counter ownership and transactions, independent of the `ecall` adapter.
//!
//! `stop` captures the final value but keeps the firmware mapping for a later
//! restart. `release` must confirm a successful STOP+RESET before relinquishing
//! ownership. SBI_ERR_ALREADY_STOPPED alone does not confirm a reset.

use core::marker::PhantomData;

use super::{ConfigFailure, Event, EventConfig, PmuCaps, PmuError, Result};

pub(super) const SBI_EXT_BASE: usize = 0x10;
pub(super) const SBI_EXT_PMU: usize = 0x504d55;
pub(super) const GET_INFO: usize = 1;
/// OpenSBI 1.4 and later implement both architectural privilege filters.
pub(super) fn supports_filter_contract(implementation: usize, version: usize) -> bool {
    implementation == 1 && version >= 0x0001_0004
}
const CONFIG_MATCHING: usize = 2;
const START: usize = 3;
const STOP: usize = 4;
const STOP_RESET: usize = 1;
const ALREADY_STARTED: isize = -7;
const ALREADY_STOPPED: isize = -8;
const MINH: usize = 1 << 7;
const SINH: usize = 1 << 6;
const UINH: usize = 1 << 5;
const VSINH: usize = 1 << 4;
const VUINH: usize = 1 << 3;

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

pub(super) trait Firmware: core::fmt::Debug {
    fn call(eid: usize, fid: usize, args: [usize; 5]) -> core::result::Result<usize, SbiError>;
    fn read_counter(csr: u32) -> Result<u64>;
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

    pub(super) fn has_filter(self, has_fixed_filter: bool, has_hpm_filter: bool) -> bool {
        if self.csr == 0xc00 || self.csr == 0xc02 {
            has_fixed_filter
        } else {
            has_hpm_filter
        }
    }
}

/// Owns the mapping from successful CONFIG_MATCHING until successful RESET.
#[derive(Debug)]
pub(super) struct Token<I: Firmware> {
    index: u32,
    info: Option<CounterInfo>,
    has_mapping: bool,
    io: PhantomData<I>,
}

pub(super) fn configure<I: Firmware>(
    config: EventConfig,
    caps: PmuCaps,
    used_mask: u64,
) -> core::result::Result<(Token<I>, u64), ConfigFailure<Token<I>>> {
    let available = caps.candidates & !used_mask;
    if available == 0 {
        return Err(PmuError::NoCounter.into());
    }
    let mut flags = MINH | VSINH | VUINH;
    if !config.is_user_counted() {
        flags |= UINH;
    }
    if !config.is_kernel_counted() {
        flags |= SINH;
    }
    let event = match config.event() {
        Event::CpuCycles => 1,
        Event::Instructions => 2,
    };
    let index = I::call(
        SBI_EXT_PMU,
        CONFIG_MATCHING,
        [0, available as usize, flags, event, 0],
    )
    .map_err(PmuError::from)?;
    // Validate before narrowing or shifting; a malformed firmware result must
    // not alias a valid counter. With no trusted identity OSTD faults this PMU.
    if index >= 64 || available & (1u64 << index) == 0 {
        return Err(PmuError::HardwareFault.into());
    }
    let mut token = Token {
        index: index as u32,
        info: None,
        has_mapping: true,
        io: PhantomData,
    };
    // Record ownership before either metadata lookup or the first CSR access.
    let result = (|| {
        let value = I::call(SBI_EXT_PMU, GET_INFO, [index, 0, 0, 0, 0]).map_err(PmuError::from)?;
        token.info = Some(CounterInfo::decode(value)?);
        token.read()
    })();
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
        // A successful configure always supplies validated metadata. Failed
        // configurations are cleanup-only tokens and never expose a handle.
        self.info.map_or(0, |info| info.width_bits)
    }

    pub(super) fn read(&self) -> Result<u64> {
        if !self.has_mapping {
            return Err(PmuError::InvalidHandle);
        }
        I::read_counter(self.info.ok_or(PmuError::InvalidHandle)?.csr)
    }

    pub(super) fn start(&mut self) -> Result<u64> {
        let initial_raw = self.read()?;
        I::call(SBI_EXT_PMU, START, [self.index as usize, 1, 0, 0, 0]).map_err(PmuError::from)?;
        Ok(initial_raw)
    }

    pub(super) fn stop(&mut self) -> Result<u64> {
        if !self.has_mapping {
            return Err(PmuError::InvalidHandle);
        }
        match I::call(SBI_EXT_PMU, STOP, [self.index as usize, 1, 0, 0, 0]) {
            Ok(_) | Err(SbiError(ALREADY_STOPPED)) => self.read(),
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
        match I::call(SBI_EXT_PMU, START, [self.index as usize, 1, 0, 0, 0]) {
            Ok(_) | Err(SbiError(ALREADY_STARTED)) => {}
            Err(error) => return Err(error.into()),
        }
        I::call(
            SBI_EXT_PMU,
            STOP,
            [self.index as usize, 1, STOP_RESET, 0, 0],
        )
        .map_err(PmuError::from)?;
        // Only a confirmed successful STOP+RESET ends ownership. Any failure
        // leaves a token which collect_retired can retry, even after restart.
        self.has_mapping = false;
        Ok(())
    }
}
