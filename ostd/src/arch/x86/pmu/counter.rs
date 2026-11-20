// SPDX-License-Identifier: MPL-2.0

//! Common x86 register transactions after backend-specific allocation.

use super::{amd, intel};
use crate::pmu::{ConfigFailure, EventConfig, PmuCaps, Result};

const EVTSEL_ENABLE: u64 = 1 << 22;
const INTEL_FIXED_CTRL_STRIDE: u32 = 4;
const INTEL_FIXED_CTRL_MASK: u64 = 0xf;
const MSR_IA32_PERF_GLOBAL_OVF_CTRL: u32 = 0x390;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    IntelFixed,
    IntelGeneral,
    AmdCore,
}

#[derive(Debug)]
pub(crate) enum Caps {
    Intel(intel::Caps),
    Amd(amd::Caps),
}

impl Caps {
    pub(crate) fn summary(&self) -> PmuCaps {
        match self {
            Self::Intel(caps) => caps.summary(),
            Self::Amd(caps) => caps.summary(),
        }
    }
}

pub(crate) fn configure(
    config: EventConfig,
    caps: &Caps,
    used: &[Option<usize>],
) -> core::result::Result<(Token, u64), ConfigFailure<Token>> {
    let mut token = match caps {
        Caps::Intel(caps) => intel::allocate(config, caps, used)?,
        Caps::Amd(caps) => amd::allocate(config, caps, used)?,
    };
    if let Some(msr) = token.global_msr {
        token.saved_gate = super::read_msr(msr)? & token.global_bit;
    }
    match apply_initial_config(&mut token) {
        Ok(raw) => Ok((token, raw)),
        Err(error) => Err(ConfigFailure {
            error,
            token: Some(token),
        }),
    }
}

/// An owned x86-64 hardware counter register allocation.
#[derive(Debug)]
pub(crate) struct Token {
    pub(super) kind: Kind,
    pub(super) index: u32,
    pub(super) width_bits: u8,
    pub(super) control_msr: u32,
    pub(super) counter_msr: u32,
    pub(super) global_msr: Option<u32>,
    pub(super) global_bit: u64,
    pub(super) saved_gate: u64,
    pub(super) config_bits: u64,
    pub(super) overflow_msr: Option<u32>,
}

impl Token {
    pub(crate) fn index(&self) -> usize {
        self.index as usize
            + if self.kind == Kind::IntelFixed {
                256
            } else {
                0
            }
    }

    pub(crate) fn width_bits(&self) -> u8 {
        self.width_bits
    }

    pub(crate) fn read(&self) -> Result<u64> {
        super::read_msr(self.counter_msr)
    }

    pub(crate) fn start(&mut self) -> Result<u64> {
        let initial_raw = self.read()?;
        if self.kind != Kind::IntelFixed {
            super::write_msr(self.control_msr, self.config_bits | EVTSEL_ENABLE)?;
        }
        if let Some(global_msr) = self.global_msr {
            update_msr(global_msr, 0, self.global_bit)?;
        }
        Ok(initial_raw)
    }

    pub(crate) fn stop(&mut self) -> Result<u64> {
        self.stop_gates()?;
        self.read()
    }

    // Attempt both gates even when the first operation fails. A failed start
    // may already have enabled the local counter before the global write.
    fn stop_gates(&mut self) -> Result<()> {
        let local_result = if self.kind != Kind::IntelFixed {
            super::write_msr(self.control_msr, self.config_bits)
        } else {
            Ok(())
        };
        let global_result = if let Some(global_msr) = self.global_msr {
            update_msr(global_msr, self.global_bit, 0)
        } else {
            Ok(())
        };
        local_result.and(global_result)
    }

    pub(crate) fn release(&mut self) -> Result<()> {
        // Do not trust an upper-layer running flag after a partial start or
        // stop failure. Both stop operations must complete before restoring
        // a saved gate which could enable the counter again.
        self.stop_gates()?;
        self.program_control(0)?;
        if let Some(global_msr) = self.global_msr {
            update_msr(global_msr, self.global_bit, self.saved_gate)?;
        }
        Ok(())
    }

    fn program_control(&self, bits: u64) -> Result<()> {
        if self.kind == Kind::IntelFixed {
            let shift = self.index * INTEL_FIXED_CTRL_STRIDE;
            update_msr(self.control_msr, INTEL_FIXED_CTRL_MASK << shift, bits)?;
            super::write_msr(MSR_IA32_PERF_GLOBAL_OVF_CTRL, self.global_bit)?;
        } else {
            super::write_msr(self.control_msr, bits)?;
            if let Some(msr) = self.overflow_msr {
                super::write_msr(msr, self.global_bit)?;
            }
        }
        Ok(())
    }
}

fn apply_initial_config(token: &mut Token) -> Result<u64> {
    if let Some(global_msr) = token.global_msr {
        update_msr(global_msr, token.global_bit, 0)?;
    }

    token.program_control(token.config_bits)?;
    token.read()
}

fn update_msr(msr: u32, clear_mask: u64, set_mask: u64) -> Result<()> {
    let current = super::read_msr(msr)?;
    super::write_msr(msr, (current & !clear_mask) | set_mask)
}
