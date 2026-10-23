// SPDX-License-Identifier: MPL-2.0

//! Fallback PMU backend for architectures without hardware counter support.

use super::{ConfigFailure, EventConfig, PmuCaps, PmuError, Result};

/// A placeholder counter token on unsupported architectures.
#[derive(Debug)]
pub(super) struct Token;

pub(super) fn probe() -> Result<PmuCaps> {
    Err(PmuError::Unsupported)
}

pub(super) fn configure(
    _config: EventConfig,
    _caps: PmuCaps,
    _used_mask: u64,
) -> core::result::Result<(Token, u64), ConfigFailure<Token>> {
    Err(PmuError::Unsupported.into())
}

impl Token {
    pub(super) fn mask(&self) -> u64 {
        0
    }

    pub(super) fn width_bits(&self) -> u8 {
        64
    }

    pub(super) fn read(&self) -> Result<u64> {
        Err(PmuError::Unsupported)
    }

    pub(super) fn start(&mut self) -> Result<u64> {
        Err(PmuError::Unsupported)
    }

    pub(super) fn stop(&mut self) -> Result<u64> {
        Err(PmuError::Unsupported)
    }

    pub(super) fn release(&mut self) -> Result<()> {
        Err(PmuError::Unsupported)
    }
}
