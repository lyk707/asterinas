// SPDX-License-Identifier: MPL-2.0

use std::{cell::RefCell, collections::VecDeque};

use crate::{
    config, retirement, sbi_counter,
    types::{CountingScope, Event, EventConfig, PmuError, Result},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SbiCall {
    Configure(u32, usize, Event),
    Start(u32),
    Stop(u32, bool),
}

struct SbiState {
    forced_index: Option<usize>,
    mapped: u32,
    running: u32,
    raw: u64,
    has_fixed_filter: bool,
    fixed_cycle_filter: usize,
    old_reset_semantics: bool,
    read_fault: bool,
    faults: VecDeque<(SbiCall, isize)>,
    calls: Vec<SbiCall>,
}

impl Default for SbiState {
    fn default() -> Self {
        Self {
            forced_index: None,
            mapped: 0,
            running: 0,
            raw: 10,
            has_fixed_filter: false,
            fixed_cycle_filter: 0,
            old_reset_semantics: false,
            read_fault: false,
            faults: VecDeque::new(),
            calls: Vec::new(),
        }
    }
}

impl SbiState {
    fn record_call(&mut self, call: SbiCall) -> sbi_counter::SbiResult<()> {
        self.calls.push(call);
        if self
            .faults
            .front()
            .is_some_and(|(failed, _)| *failed == call)
        {
            return Err(sbi_counter::SbiError(self.faults.pop_front().unwrap().1));
        }
        Ok(())
    }
}

thread_local! { static SBI: RefCell<SbiState> = RefCell::new(SbiState::default()); }

#[derive(Debug)]
struct Sbi;

impl sbi_counter::Firmware for Sbi {
    fn configure(mask: u32, flags: usize, event: Event) -> sbi_counter::SbiResult<usize> {
        SBI.with_borrow_mut(|state| {
            state.record_call(SbiCall::Configure(mask, flags, event))?;
            let programmable = mask & !0b111 & !state.mapped;
            let index = state.forced_index.unwrap_or_else(|| {
                if programmable != 0 {
                    programmable.trailing_zeros() as usize
                } else if event == Event::CpuCycles {
                    0
                } else {
                    2
                }
            });
            // OpenSBI may write the fixed cycle filter even outside the mask.
            if index == 0 && state.has_fixed_filter {
                state.fixed_cycle_filter = flags;
            }
            if index < 32 {
                state.mapped |= 1 << index;
            }
            Ok(index)
        })
    }

    fn start(index: u32) -> sbi_counter::SbiResult<()> {
        SBI.with_borrow_mut(|state| {
            state.record_call(SbiCall::Start(index))?;
            let bit = 1 << index;
            if state.mapped & bit == 0 {
                return Err(sbi_counter::SbiError(-3));
            }
            if state.running & bit != 0 {
                return Err(sbi_counter::SbiError(-7));
            }
            state.running |= bit;
            Ok(())
        })
    }

    fn stop(index: u32, mode: sbi_counter::StopMode) -> sbi_counter::SbiResult<()> {
        SBI.with_borrow_mut(|state| {
            let reset = mode == sbi_counter::StopMode::ResetMapping;
            state.record_call(SbiCall::Stop(index, reset))?;
            let bit = 1 << index;
            if state.mapped & bit == 0 {
                return Err(sbi_counter::SbiError(-3));
            }
            let was_running = state.running & bit != 0;
            state.running &= !bit;
            if was_running {
                state.raw += 5;
            }
            if reset && (was_running || state.old_reset_semantics) {
                state.mapped &= !bit;
            }
            if was_running {
                Ok(())
            } else {
                Err(sbi_counter::SbiError(-8))
            }
        })
    }

    fn read_counter(_csr: u32) -> Result<u64> {
        SBI.with_borrow(|state| {
            if state.read_fault {
                Err(PmuError::HardwareFault)
            } else {
                Ok(state.raw)
            }
        })
    }
}

fn reset_sbi() {
    SBI.with_borrow_mut(|state| *state = SbiState::default());
}

fn counter_infos(
    indices: &[usize],
) -> [Option<sbi_counter::CounterInfo>; sbi_counter::HW_COUNTERS] {
    let mut infos = [None; sbi_counter::HW_COUNTERS];
    for &index in indices {
        infos[index] = sbi_counter::CounterInfo::decode((0xc00 + index) | (47 << 12)).ok();
    }
    infos
}

fn sbi_caps() -> sbi_counter::Caps {
    sbi_counter::Caps::probe(counter_infos(&[0, 2, 3, 4]), true).unwrap()
}

fn sbi() -> sbi_counter::Token<Sbi> {
    sbi_counter::configure::<Sbi>(config(), &sbi_caps(), 0)
        .unwrap()
        .0
}

#[test]
fn stopped_release_works_with_both_opensbi_reset_behaviors() {
    for old in [false, true] {
        reset_sbi();
        SBI.with_borrow_mut(|state| state.old_reset_semantics = old);
        let mut token = sbi();
        assert_eq!(token.start().unwrap(), 10);
        let final_raw = token.stop().unwrap();
        assert_eq!(final_raw, 15);
        token.release().unwrap();
        assert_eq!(final_raw, 15); // Cleanup does not change the observed final value.
        SBI.with_borrow(|state| {
            assert_eq!((state.mapped, state.running), (0, 0));
            assert_eq!(state.raw, 20);
        });
        assert_eq!(token.read(), Err(PmuError::InvalidHandle));
        let calls = SBI.with_borrow(|state| state.calls.len());
        token.release().unwrap();
        SBI.with_borrow(|state| assert_eq!(state.calls.len(), calls));
    }
}

#[test]
fn never_started_and_running_sbi_counters_can_be_released() {
    for start in [false, true] {
        reset_sbi();
        let mut token = sbi();
        if start {
            token.start().unwrap();
        }
        token.release().unwrap();
        SBI.with_borrow(|state| assert_eq!((state.mapped, state.running), (0, 0)));
    }
}

#[test]
fn failed_reset_retains_ownership_and_retries_cleanup() {
    reset_sbi();
    let mut token = sbi();
    SBI.with_borrow_mut(|state| state.faults.push_back((SbiCall::Stop(3, true), -1)));
    assert!(token.release().is_err());
    SBI.with_borrow(|state| assert_ne!(state.mapped & state.running, 0));
    token.release().unwrap();
    SBI.with_borrow(|state| assert_eq!((state.mapped, state.running), (0, 0)));
}

#[test]
fn failed_cleanup_start_does_not_forget_mapping() {
    reset_sbi();
    let mut token = sbi();
    SBI.with_borrow_mut(|state| state.faults.push_back((SbiCall::Start(3), -1)));
    assert!(token.release().is_err());
    SBI.with_borrow(|state| assert_ne!(state.mapped, 0));
    token.release().unwrap();
}

#[test]
fn csr_read_failure_keeps_a_cleanup_token() {
    reset_sbi();
    SBI.with_borrow_mut(|state| state.read_fault = true);
    let failure = sbi_counter::configure::<Sbi>(config(), &sbi_caps(), 0).unwrap_err();
    let mut token = failure
        .token
        .expect("successful CONFIG_MATCHING owns the counter");
    assert_eq!(token.width_bits(), 48);
    token.release().unwrap();
    SBI.with_borrow(|state| assert_eq!(state.mapped, 0));
}

#[test]
fn invalid_sbi_identity_is_rejected_before_narrowing_or_shift() {
    for index in [64, usize::MAX, (1usize << 32) + 3, 2] {
        reset_sbi();
        SBI.with_borrow_mut(|state| state.forced_index = Some(index));
        let failure = sbi_counter::configure::<Sbi>(config(), &sbi_caps(), 0).unwrap_err();
        assert_eq!(failure.error, PmuError::HardwareFault);
        assert!(failure.token.is_none());
    }
}

#[test]
fn sbi_never_offers_an_owned_counter_and_sets_requested_filters() {
    reset_sbi();
    SBI.with_borrow_mut(|state| state.forced_index = Some(4));
    let mut token = sbi_counter::configure::<Sbi>(config(), &sbi_caps(), 1 << 3)
        .unwrap()
        .0;
    SBI.with_borrow(|state| {
        assert_eq!(
            state.calls[0],
            SbiCall::Configure(
                (1 << 4) | 1,
                (1 << 7) | (1 << 6) | (1 << 4) | (1 << 3),
                Event::CpuCycles
            )
        );
    });
    assert_eq!(token.mask(), 1 << 4);
    assert_eq!(token.width_bits(), 48);
    token.release().unwrap();
}

#[test]
fn sbi_metadata_rejects_unsafe_csr_width_and_reserved_fields() {
    for info in [
        0xc01 | (63 << 12),
        0xb00 | (63 << 12),
        0xc20 | (63 << 12),
        0xc03 | (30 << 12),
        0xc03 | (1 << 18),
        usize::MAX,
    ] {
        assert!(sbi_counter::CounterInfo::decode(info).is_err());
    }
    assert_eq!(
        sbi_counter::CounterInfo::decode(0xc03 | (31 << 12))
            .unwrap()
            .width_bits,
        32
    );
    assert_eq!(
        sbi_counter::CounterInfo::decode(0xc03 | (63 << 12))
            .unwrap()
            .width_bits,
        64
    );
}

#[test]
fn filter_capability_requires_both_audited_firmware_and_matching_isa() {
    assert!(!sbi_counter::supports_filter_contract(0, usize::MAX));
    assert!(!sbi_counter::supports_filter_contract(1, 0x10003));
    assert!(sbi_counter::supports_filter_contract(1, 0x10004));

    let cmdline = "console=hvc0 ostd.riscv_pmu=icount";
    assert!(!sbi_counter::supports_qemu_profile(false, true, 1, cmdline));
    assert!(!sbi_counter::supports_qemu_profile(true, false, 1, cmdline));
    assert!(!sbi_counter::supports_qemu_profile(
        true,
        true,
        1,
        "console=hvc0"
    ));
    assert!(!sbi_counter::supports_qemu_profile(
        true,
        true,
        1,
        "ostd.riscv_pmu=icount-no"
    ));
    assert!(!sbi_counter::supports_qemu_profile(true, true, 2, cmdline));
    assert!(sbi_counter::supports_qemu_profile(true, true, 1, cmdline));
    assert!(sbi_counter::Caps::probe(counter_infos(&[3, 4]), true).is_err());
    assert!(sbi_counter::Caps::probe(counter_infos(&[0, 2]), false).is_ok());
}

#[test]
fn fixed_instruction_counter_is_stopped_before_first_start() {
    reset_sbi();
    SBI.with_borrow_mut(|state| state.running = 1 << 2);
    let caps = sbi_counter::Caps::probe(counter_infos(&[0, 2, 3]), true).unwrap();
    let config = EventConfig::new(Event::Instructions, CountingScope::User);
    let (mut token, raw) = sbi_counter::configure::<Sbi>(config, &caps, 0).unwrap();
    assert_eq!(raw, 15);
    SBI.with_borrow(|state| assert_eq!(state.calls.last(), Some(&SbiCall::Stop(2, false))));
    assert_eq!(token.start().unwrap(), raw);
    token.release().unwrap();
}

#[test]
fn one_programmable_counter_can_count_with_fixed_instret_in_both_orders() {
    for order in [
        [Event::CpuCycles, Event::Instructions],
        [Event::Instructions, Event::CpuCycles],
    ] {
        reset_sbi();
        let caps = sbi_counter::Caps::probe(counter_infos(&[0, 2, 3]), true).unwrap();
        let mut used = 0;
        for event in order {
            let config = EventConfig::new(event, CountingScope::User);
            let (mut token, _) = sbi_counter::configure::<Sbi>(config, &caps, used).unwrap();
            token.start().unwrap();
            let expected_bit = if event == Event::CpuCycles {
                1 << 3
            } else {
                1 << 2
            };
            assert_eq!(token.mask(), expected_bit);
            used |= token.mask();
        }
        SBI.with_borrow(|state| assert_eq!(u64::from(state.running), used));
    }
}

#[test]
fn fixed_cycle_fallback_requires_an_available_counter() {
    reset_sbi();
    SBI.with_borrow_mut(|state| {
        state.has_fixed_filter = true;
        state.forced_index = Some(0);
    });
    let caps = sbi_caps();
    let failure = sbi_counter::configure::<Sbi>(config(), &caps, 1).unwrap_err();
    assert_eq!(failure.error, PmuError::NoCounter);
    SBI.with_borrow(|state| assert!(state.calls.is_empty()));

    let (mut token, _) = sbi_counter::configure::<Sbi>(config(), &caps, 0).unwrap();
    assert_eq!(token.mask(), 1);
    token.release().unwrap();
}

#[test]
fn stopped_sbi_counter_can_restart_without_losing_mapping() {
    reset_sbi();
    let mut token = sbi();
    token.start().unwrap();
    assert_eq!(token.stop().unwrap(), 15);
    assert_eq!(token.stop().unwrap(), 15);
    assert_eq!(token.start().unwrap(), 15);
    assert_eq!(token.stop().unwrap(), 20);
    token.release().unwrap();
}

#[test]
fn permanent_sbi_release_fault_preserves_mapping_and_bounds_ecalls() {
    reset_sbi();
    let mut token = sbi();
    let mut cleanup = retirement::Cleanup::new();
    SBI.with_borrow_mut(|state| {
        state.calls.clear();
        state
            .faults
            .extend(std::iter::repeat_n((SbiCall::Stop(3, true), -1), 8));
    });
    for tick in 0..10_000 {
        let _ = cleanup.attempt(tick, || token.release());
    }
    assert!(cleanup.is_quarantined());
    SBI.with_borrow(|state| {
        assert_ne!(state.mapped, 0);
        assert_eq!(state.calls.len(), 16); // START + failed STOP|RESET, eight attempts.
    });
}
