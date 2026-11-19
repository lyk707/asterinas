// SPDX-License-Identifier: MPL-2.0

//! SBI counter inventory and single-event matching.

use alloc::vec::Vec;

use crate::pmu::{BackendKind, ConfigFailure, EventConfig, PmuCaps, PmuError, Result};

const ALREADY_STARTED: isize = -7;
const ALREADY_STOPPED: isize = -8;
const MINH: usize = 1 << 7;
const SINH: usize = 1 << 6;
const UINH: usize = 1 << 5;
const VSINH: usize = 1 << 4;
const VUINH: usize = 1 << 3;
pub(crate) const HW_COUNTERS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SbiError(pub(crate) isize);

impl From<SbiError> for PmuError {
    fn from(error: SbiError) -> Self {
        match error.0 {
            -2 => Self::Unsupported,
            -3 => Self::InvalidConfig,
            _ => Self::HardwareFault,
        }
    }
}
pub(crate) type SbiResult<T> = core::result::Result<T, SbiError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopMode {
    KeepMapping,
    ResetMapping,
}

#[derive(Debug)]
pub(crate) struct Caps {
    // SBI indices are retained, including firmware counters and sparse entries.
    infos: Vec<Option<CounterInfo>>,
    has_fixed_filter: bool,
    has_hpm_filter: bool,
}

impl Caps {
    pub(crate) fn new(
        infos: Vec<Option<CounterInfo>>,
        has_fixed_filter: bool,
        has_hpm_filter: bool,
    ) -> Self {
        Self {
            infos,
            has_fixed_filter,
            has_hpm_filter,
        }
    }

    pub(crate) fn summary(&self) -> PmuCaps {
        let mut counters = 0;
        let mut width_bits = 64;
        for info in self.infos.iter().flatten() {
            if let CounterInfo::Hardware { width, .. } = info {
                counters += 1;
                width_bits = width_bits.min(*width);
            }
        }
        PmuCaps {
            backend: BackendKind::Sbi,
            counters,
            width_bits: if counters == 0 { 0 } else { width_bits },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CounterInfo {
    Hardware { csr: u32, width: u8 },
    Firmware,
}

impl CounterInfo {
    pub(crate) fn decode(value: usize) -> Result<Self> {
        if value >> (usize::BITS - 1) != 0 {
            return Ok(Self::Firmware);
        }
        if value & !0x3ffff != 0 {
            return Err(PmuError::HardwareFault);
        }
        let csr = (value & 0xfff) as u32;
        let width = ((value >> 12) & 0x3f) as u8 + 1;
        Ok(Self::Hardware { csr, width })
    }
}

#[derive(Debug)]
pub(crate) struct Token {
    index: usize,
    csr: u32,
    width: u8,
    has_mapping: bool,
}

pub(crate) fn configure(
    config: EventConfig,
    caps: &Caps,
    used: &[Option<usize>],
) -> core::result::Result<(Token, u64), ConfigFailure<Token>> {
    let mut flags = MINH | VSINH | VUINH;
    if !config.is_user_counted() {
        flags |= UINH;
    }
    if !config.is_kernel_counted() {
        flags |= SINH;
    }
    let mut had_candidate = false;
    let mut had_available = false;
    let mut occupied = false;
    let mut hardware = false;
    // SBI masks are XLEN bits wide. Enumerate all windows rather than truncating
    // the discovered inventory to 32 or 64 entries.
    for base in (0..caps.infos.len()).step_by(usize::BITS as usize) {
        let mut mask = 0usize;
        for (offset, info) in caps.infos[base..]
            .iter()
            .take(usize::BITS as usize)
            .enumerate()
        {
            let Some(CounterInfo::Hardware { csr, .. }) = info else {
                continue;
            };
            if !(0xc00..=0xc1f).contains(csr) || *csr == 0xc01 {
                continue;
            }
            hardware = true;
            let can_filter = if *csr <= 0xc02 {
                caps.has_fixed_filter
            } else {
                caps.has_hpm_filter
            };
            if !can_filter {
                continue;
            }
            had_candidate = true;
            if used.contains(&Some(base + offset)) {
                occupied = true;
                continue;
            }
            mask |= 1usize << offset;
        }
        if mask == 0 {
            continue;
        }
        had_available = true;
        let index = match super::Sbi::configure(base, mask, flags, config.event()) {
            Ok(index) => index,
            Err(SbiError(-2)) => continue,
            Err(error) => return Err(PmuError::from(error).into()),
        };
        // A response outside the offered set has no trustworthy resource identity.
        if index < base
            || index - base >= usize::BITS as usize
            || mask & (1usize << (index - base)) == 0
        {
            return Err(PmuError::HardwareFault.into());
        }
        let Some(CounterInfo::Hardware { csr, width }) = caps.infos[index] else {
            return Err(PmuError::HardwareFault.into());
        };
        let mut token = Token {
            index,
            csr,
            width,
            has_mapping: true,
        };
        // Normalize the state even for fixed counters left running by firmware.
        return match token.stop() {
            Ok(raw) => Ok((token, raw)),
            Err(error) => Err(ConfigFailure {
                error,
                token: Some(token),
            }),
        };
    }
    let error = if hardware && !had_candidate {
        PmuError::UnsupportedFilter
    } else if occupied || (had_candidate && !had_available) {
        PmuError::NoCounter
    } else {
        PmuError::UnsupportedEvent
    };
    Err(error.into())
}

impl Token {
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    pub(crate) fn width_bits(&self) -> u8 {
        self.width
    }

    pub(crate) fn read(&self) -> Result<u64> {
        if !self.has_mapping {
            return Err(PmuError::InvalidHandle);
        }
        super::Sbi::read_counter(self.csr)
    }

    pub(crate) fn start(&mut self) -> Result<u64> {
        let initial_raw = self.read()?;
        super::Sbi::start(self.index).map_err(PmuError::from)?;
        Ok(initial_raw)
    }

    pub(crate) fn stop(&mut self) -> Result<u64> {
        if !self.has_mapping {
            return Err(PmuError::InvalidHandle);
        }
        match super::Sbi::stop(self.index, StopMode::KeepMapping) {
            Ok(()) | Err(SbiError(ALREADY_STOPPED)) => self.read(),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn release(&mut self) -> Result<()> {
        if !self.has_mapping {
            return Ok(());
        }
        // OpenSBI versions differ on whether an already-stopped RESET removes
        // the mapping before reporting ALREADY_STOPPED. Avoid that ambiguous
        // operation entirely: establish running state before STOP+RESET.
        // This counter is still exclusively owned, IRQs are disabled, and the
        // logical final count was captured by stop(). Cleanup is not counted.
        // https://github.com/riscv-software-src/opensbi/blob/v1.7/lib/sbi/sbi_pmu.c#L484-L527
        match super::Sbi::start(self.index) {
            Ok(()) | Err(SbiError(ALREADY_STARTED)) => {}
            Err(error) => return Err(error.into()),
        }
        super::Sbi::stop(self.index, StopMode::ResetMapping).map_err(PmuError::from)?;
        // Only a confirmed successful STOP+RESET ends ownership. Any failure
        // leaves a token which collect_retired can retry, even after restart.
        self.has_mapping = false;
        Ok(())
    }
}
