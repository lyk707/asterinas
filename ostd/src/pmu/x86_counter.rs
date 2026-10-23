// SPDX-License-Identifier: MPL-2.0

//! x86 counter transactions, independently testable with injected MSR access.
//!
//! Regression cases and hardware-access adapters are separate so host tests
//! execute the production configuration, stop and rollback code.

use core::marker::PhantomData;

use super::{BackendKind, ConfigFailure, Event, EventConfig, PmuCaps, PmuError, Result};

pub(super) trait Registers: core::fmt::Debug {
    fn read(msr: u32) -> Result<u64>;
    fn write(msr: u32, value: u64) -> Result<()>;
}

const MSR_IA32_FIXED_CTR0: u32 = 0x0309;
const MSR_IA32_FIXED_CTR_CTRL: u32 = 0x038d;
const MSR_IA32_PERF_GLOBAL_CTRL: u32 = 0x038f;
const MSR_IA32_PERF_GLOBAL_OVF_CTRL: u32 = 0x0390;
const INTEL_FIXED_GLOBAL_SHIFT: u32 = 32;
const INTEL_FIXED_CTRL_STRIDE: u32 = 4;
const INTEL_FIXED_CTRL_MASK: u64 = 0x0f;
const MSR_AMD64_PERF_CTL0: u32 = 0xc001_0200;
const AMD_PERF_MSR_STRIDE: u32 = 2;
const MSR_AMD64_PERF_CNTR_GLOBAL_CTL: u32 = 0xc000_0301;
const MSR_AMD64_PERF_CNTR_GLOBAL_STATUS_CLR: u32 = 0xc000_0302;
const AMD_CORE_COUNTER_WIDTH_BITS: u8 = 48;
const AMD_EVTSEL_ENABLE: u64 = 1 << 22;
const AMD_EVTSEL_USR_SHIFT: u32 = 16;
const AMD_EVTSEL_OS_SHIFT: u32 = 17;
const AMD_EVENT_CPU_CYCLES: u64 = 0x0076;
const AMD_EVENT_INSTRUCTIONS: u64 = 0x00c0;

/// A declared PerfMonV2 interface must work; a fault is not legacy hardware.
pub(super) fn probe_global_control<I: Registers>(v2_leaf: Option<(u32, u32)>) -> Result<bool> {
    let Some((_, ebx)) = v2_leaf.filter(|(eax, _)| eax & 1 != 0) else {
        return Ok(false);
    };
    if ebx & 0x0f < 2 {
        return Err(PmuError::Unsupported);
    }
    I::read(MSR_AMD64_PERF_CNTR_GLOBAL_CTL)?;
    Ok(true)
}

/// Configures a stopped hardware counter token and returns its initial raw value.
pub(super) fn configure<I: Registers>(
    config: EventConfig,
    caps: PmuCaps,
    used_mask: u64,
) -> core::result::Result<(Token<I>, u64), ConfigFailure<Token<I>>> {
    let user_bit = u64::from(config.is_user_counted());
    let kernel_bit = u64::from(config.is_kernel_counted());

    let mut token = match caps.backend() {
        BackendKind::IntelFixed => {
            let index = match config.event() {
                Event::Instructions => 0,
                Event::CpuCycles => 1,
            };
            let shift = index * INTEL_FIXED_CTRL_STRIDE;
            if I::read(MSR_IA32_FIXED_CTR_CTRL)? & (INTEL_FIXED_CTRL_MASK << shift) != 0 {
                return Err(PmuError::NoCounter.into());
            }
            Token {
                io: PhantomData,
                kind: caps.backend(),
                index,
                width_bits: caps.width_bits,
                control_msr: MSR_IA32_FIXED_CTR_CTRL,
                counter_msr: MSR_IA32_FIXED_CTR0 + index,
                global_msr: Some(MSR_IA32_PERF_GLOBAL_CTRL),
                global_bit: 1u64 << (INTEL_FIXED_GLOBAL_SHIFT + index),
                saved_gate: 0,
                config_bits: (kernel_bit | (user_bit << 1)) << shift,
            }
        }
        BackendKind::AmdZen => {
            let index = config.event().index() as u32;
            let control_msr = MSR_AMD64_PERF_CTL0 + AMD_PERF_MSR_STRIDE * index;
            // A stopped configuration may still belong to another consumer.
            if I::read(control_msr)? != 0 {
                return Err(PmuError::NoCounter.into());
            }
            let event_code = match config.event() {
                Event::CpuCycles => AMD_EVENT_CPU_CYCLES,
                Event::Instructions => AMD_EVENT_INSTRUCTIONS,
            };
            Token {
                io: PhantomData,
                kind: caps.backend(),
                index,
                width_bits: AMD_CORE_COUNTER_WIDTH_BITS,
                control_msr,
                counter_msr: control_msr + 1,
                global_msr: caps
                    .has_global_control
                    .then_some(MSR_AMD64_PERF_CNTR_GLOBAL_CTL),
                global_bit: 1u64 << index,
                saved_gate: 0,
                config_bits: event_code
                    | (user_bit << AMD_EVTSEL_USR_SHIFT)
                    | (kernel_bit << AMD_EVTSEL_OS_SHIFT),
            }
        }
        BackendKind::Sbi => return Err(PmuError::Unsupported.into()),
    };

    if caps.candidates & token.mask() == 0 || used_mask & token.mask() != 0 {
        return Err(PmuError::NoCounter.into());
    }
    if let Some(global_msr) = token.global_msr {
        token.saved_gate = I::read(global_msr)? & token.global_bit;
    }

    match apply_initial_config(&mut token) {
        Ok(initial_raw) => Ok((token, initial_raw)),
        Err(error) => Err(ConfigFailure {
            error,
            token: Some(token),
        }),
    }
}

/// An owned x86-64 hardware counter register allocation.
#[derive(Debug)]
pub(super) struct Token<I: Registers> {
    io: PhantomData<I>,
    kind: BackendKind,
    index: u32,
    width_bits: u8,
    control_msr: u32,
    counter_msr: u32,
    global_msr: Option<u32>,
    global_bit: u64,
    saved_gate: u64,
    config_bits: u64,
}

impl<I: Registers> Token<I> {
    pub(super) fn mask(&self) -> u64 {
        1u64 << self.index
    }

    pub(super) fn width_bits(&self) -> u8 {
        self.width_bits
    }

    pub(super) fn read(&self) -> Result<u64> {
        I::read(self.counter_msr)
    }

    pub(super) fn start(&mut self) -> Result<u64> {
        let initial_raw = self.read()?;
        if self.kind == BackendKind::AmdZen {
            I::write(self.control_msr, self.config_bits | AMD_EVTSEL_ENABLE)?;
        }
        if let Some(global_msr) = self.global_msr {
            update_msr::<I>(global_msr, 0, self.global_bit)?;
        }
        Ok(initial_raw)
    }

    pub(super) fn stop(&mut self) -> Result<u64> {
        self.stop_gates()?;
        self.read()
    }

    // Attempt both gates even when the first operation fails. A failed start
    // may already have enabled the local counter before the global write.
    fn stop_gates(&mut self) -> Result<()> {
        let local_result = if self.kind == BackendKind::AmdZen {
            I::write(self.control_msr, self.config_bits)
        } else {
            Ok(())
        };
        let global_result = if let Some(global_msr) = self.global_msr {
            update_msr::<I>(global_msr, self.global_bit, 0)
        } else {
            Ok(())
        };
        local_result.and(global_result)
    }

    pub(super) fn release(&mut self) -> Result<()> {
        // Do not trust an upper-layer running flag after a partial start or
        // stop failure. Both stop operations must complete before restoring
        // a saved gate which could enable the counter again.
        self.stop_gates()?;
        if self.kind == BackendKind::IntelFixed {
            let shift = self.index * INTEL_FIXED_CTRL_STRIDE;
            update_msr::<I>(self.control_msr, INTEL_FIXED_CTRL_MASK << shift, 0)?;
            I::write(MSR_IA32_PERF_GLOBAL_OVF_CTRL, self.global_bit)?;
        } else {
            I::write(self.control_msr, 0)?;
            if self.global_msr.is_some() {
                I::write(MSR_AMD64_PERF_CNTR_GLOBAL_STATUS_CLR, self.global_bit)?;
            }
        }
        if let Some(global_msr) = self.global_msr {
            update_msr::<I>(global_msr, self.global_bit, self.saved_gate)?;
        }
        Ok(())
    }
}

fn apply_initial_config<I: Registers>(token: &mut Token<I>) -> Result<u64> {
    if let Some(global_msr) = token.global_msr {
        update_msr::<I>(global_msr, token.global_bit, 0)?;
    }

    if token.kind == BackendKind::IntelFixed {
        let shift = token.index * INTEL_FIXED_CTRL_STRIDE;
        update_msr::<I>(
            token.control_msr,
            INTEL_FIXED_CTRL_MASK << shift,
            token.config_bits,
        )?;
        I::write(MSR_IA32_PERF_GLOBAL_OVF_CTRL, token.global_bit)?;
    } else {
        I::write(token.control_msr, token.config_bits)?;
        if token.global_msr.is_some() {
            I::write(MSR_AMD64_PERF_CNTR_GLOBAL_STATUS_CLR, token.global_bit)?;
        }
    }

    token.read()
}

fn update_msr<I: Registers>(msr: u32, clear_mask: u64, set_mask: u64) -> Result<()> {
    let current = I::read(msr)?;
    I::write(msr, (current & !clear_mask) | set_mask)
}
