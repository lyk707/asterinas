// SPDX-License-Identifier: MPL-2.0

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
};

use crate::{
    config, retirement,
    types::{PmuError, Result},
    x86_counter,
};

const AMD_LOCAL: u32 = 0xc001_0200;
const AMD_GLOBAL: u32 = 0xc000_0301;
const INTEL_LOCAL: u32 = 0x38d;
const INTEL_GLOBAL: u32 = 0x38f;
const ENABLE: u64 = 1 << 22;

const AMD_CAPS: x86_counter::Caps = x86_counter::Caps::AmdZen {
    has_global_control: true,
};
const INTEL_CAPS: x86_counter::Caps = x86_counter::Caps::IntelFixed { width_bits: 48 };

#[derive(Default)]
struct MsrState {
    values: BTreeMap<u32, u64>,
    faults: VecDeque<(bool, u32)>,
    accesses: Vec<(bool, u32, u64)>,
}

thread_local! { static MSRS: RefCell<MsrState> = RefCell::new(MsrState::default()); }

#[derive(Debug)]
struct Msr;

impl x86_counter::Registers for Msr {
    fn read(msr: u32) -> Result<u64> {
        access(false, msr, 0)
    }
    fn write(msr: u32, value: u64) -> Result<()> {
        access(true, msr, value).map(|_| ())
    }
}

fn access(write: bool, msr: u32, value: u64) -> Result<u64> {
    MSRS.with_borrow_mut(|state| {
        state.accesses.push((write, msr, value));
        if state.faults.front() == Some(&(write, msr)) {
            state.faults.pop_front();
            return Err(PmuError::HardwareFault);
        }
        if write {
            state.values.insert(msr, value);
        }
        Ok(*state.values.get(&msr).unwrap_or(&0))
    })
}

fn reset_msrs(global: u32, value: u64) {
    MSRS.with_borrow_mut(|state| {
        *state = MsrState::default();
        state.values.insert(global, value);
    });
}
fn fail_msr(write: bool, msr: u32) {
    MSRS.with_borrow_mut(|state| state.faults.push_back((write, msr)));
}
fn msr_value(msr: u32) -> u64 {
    MSRS.with_borrow(|state| *state.values.get(&msr).unwrap_or(&0))
}
fn amd() -> x86_counter::Token<Msr> {
    x86_counter::configure::<Msr>(config(), AMD_CAPS, 0)
        .unwrap()
        .0
}

#[test]
fn failed_snapshot_never_writes_or_fabricates_rollback_token() {
    for (caps, global) in [(AMD_CAPS, AMD_GLOBAL), (INTEL_CAPS, INTEL_GLOBAL)] {
        reset_msrs(global, u64::MAX);
        fail_msr(false, global);
        let failure = x86_counter::configure::<Msr>(config(), caps, 0).unwrap_err();
        assert!(failure.token.is_none());
        assert_eq!(msr_value(global), u64::MAX);
        MSRS.with_borrow(|state| assert!(state.accesses.iter().all(|(write, _, _)| !write)));
    }
}

#[test]
fn advertised_amd_v2_fault_cannot_silently_fall_back_to_legacy() {
    reset_msrs(AMD_GLOBAL, 0);
    assert!(!x86_counter::probe_global_control::<Msr>(None).unwrap());
    assert!(!x86_counter::probe_global_control::<Msr>(Some((0, 0))).unwrap());
    assert_eq!(
        x86_counter::probe_global_control::<Msr>(Some((1, 1))),
        Err(PmuError::Unsupported)
    );
    fail_msr(false, AMD_GLOBAL);
    assert_eq!(
        x86_counter::probe_global_control::<Msr>(Some((1, 6))),
        Err(PmuError::HardwareFault)
    );
    assert!(x86_counter::probe_global_control::<Msr>(Some((1, 6))).unwrap());
}

#[test]
fn x86_owned_mask_prevents_a_second_allocation() {
    reset_msrs(AMD_GLOBAL, 0);
    let failure = x86_counter::configure::<Msr>(config(), AMD_CAPS, 1).unwrap_err();
    assert_eq!(failure.error, PmuError::NoCounter);
    assert!(failure.token.is_none());
    MSRS.with_borrow(|state| assert!(state.accesses.iter().all(|(write, _, _)| !write)));
}

#[test]
fn amd_global_read_failure_does_not_skip_local_stop() {
    reset_msrs(AMD_GLOBAL, 0);
    let mut token = amd();
    token.start().unwrap();
    fail_msr(false, AMD_GLOBAL);
    assert_eq!(token.stop(), Err(PmuError::HardwareFault));
    assert_eq!(msr_value(AMD_LOCAL) & ENABLE, 0);
    token.release().unwrap();
    assert_eq!(msr_value(AMD_LOCAL), 0);
}

#[test]
fn amd_local_stop_failure_does_not_skip_global_stop() {
    reset_msrs(AMD_GLOBAL, 0);
    let mut token = amd();
    token.start().unwrap();
    fail_msr(true, AMD_LOCAL);
    assert_eq!(token.stop(), Err(PmuError::HardwareFault));
    assert_eq!(msr_value(AMD_GLOBAL) & 1, 0);
    token.release().unwrap();
}

#[test]
fn partial_start_can_be_released_without_upper_running_flag() {
    reset_msrs(AMD_GLOBAL, 0x80);
    let mut token = amd();
    fail_msr(true, AMD_GLOBAL);
    assert_eq!(token.start(), Err(PmuError::HardwareFault));
    assert_ne!(msr_value(AMD_LOCAL) & ENABLE, 0);
    token.release().unwrap();
    assert_eq!(msr_value(AMD_LOCAL), 0);
    assert_eq!(msr_value(AMD_GLOBAL), 0x80);
}

#[test]
fn failed_cleanup_never_restores_an_enabled_gate() {
    reset_msrs(AMD_GLOBAL, 0x81);
    let mut token = amd();
    token.start().unwrap();
    fail_msr(true, AMD_LOCAL);
    assert!(token.release().is_err());
    assert_eq!(msr_value(AMD_GLOBAL), 0x80);
    token.release().unwrap();
    assert_eq!(msr_value(AMD_LOCAL), 0);
    assert_eq!(msr_value(AMD_GLOBAL), 0x81);
}

#[test]
fn failed_configuration_retains_real_rollback_state() {
    reset_msrs(AMD_GLOBAL, 0x81);
    fail_msr(true, AMD_LOCAL);
    let failure = x86_counter::configure::<Msr>(config(), AMD_CAPS, 0).unwrap_err();
    let mut token = failure.token.unwrap();
    assert_eq!(msr_value(AMD_GLOBAL), 0x80);
    token.release().unwrap();
    assert_eq!(msr_value(AMD_GLOBAL), 0x81);
}

#[test]
fn intel_release_preserves_other_counter_fields_and_gates() {
    let saved = (1 << 33) | (1 << 32) | 0x80;
    reset_msrs(INTEL_GLOBAL, saved);
    MSRS.with_borrow_mut(|state| {
        state.values.insert(INTEL_LOCAL, 0x03);
    });
    let (mut token, _) = x86_counter::configure::<Msr>(config(), INTEL_CAPS, 0).unwrap();
    token.start().unwrap();
    token.stop().unwrap();
    token.release().unwrap();
    assert_eq!(msr_value(INTEL_LOCAL), 0x03);
    assert_eq!(msr_value(INTEL_GLOBAL), saved);
}

#[test]
fn amd_legacy_never_accesses_perfmon_v2_registers() {
    reset_msrs(AMD_GLOBAL, 0);
    let legacy = x86_counter::Caps::AmdZen {
        has_global_control: false,
    };
    let (mut token, _) = x86_counter::configure::<Msr>(config(), legacy, 0).unwrap();
    token.start().unwrap();
    token.stop().unwrap();
    token.release().unwrap();
    MSRS.with_borrow(|state| {
        assert!(
            state
                .accesses
                .iter()
                .all(|(_, msr, _)| !(0xc000_0300..=0xc000_0302).contains(msr))
        )
    });
}

#[test]
fn stopped_foreign_amd_configuration_is_not_overwritten() {
    reset_msrs(AMD_GLOBAL, 0);
    MSRS.with_borrow_mut(|state| {
        state.values.insert(AMD_LOCAL, 0xc0);
    });
    let failure = x86_counter::configure::<Msr>(config(), AMD_CAPS, 0).unwrap_err();
    assert_eq!(failure.error, PmuError::NoCounter);
    assert!(failure.token.is_none());
    assert_eq!(msr_value(AMD_LOCAL), 0xc0);
}

#[test]
fn stop_reads_after_disabling_and_start_returns_baseline() {
    reset_msrs(AMD_GLOBAL, 0);
    let mut token = amd();
    MSRS.with_borrow_mut(|state| {
        state.values.insert(AMD_LOCAL + 1, 40);
    });
    assert_eq!(token.start().unwrap(), 40);
    MSRS.with_borrow_mut(|state| {
        state.values.insert(AMD_LOCAL + 1, 100);
        state.accesses.clear();
    });
    assert_eq!(token.stop().unwrap(), 100);
    MSRS.with_borrow(|state| assert_eq!(state.accesses.last().unwrap().1, AMD_LOCAL + 1));
}

#[test]
fn permanent_amd_release_fault_is_bounded_by_production_cleanup_policy() {
    reset_msrs(AMD_GLOBAL, 0);
    let mut token = amd();
    let mask = token.mask();
    let mut cleanup = retirement::Cleanup::new();
    MSRS.with_borrow_mut(|state| {
        state.accesses.clear();
        state
            .faults
            .extend(std::iter::repeat_n((true, AMD_LOCAL), 8));
    });
    for tick in 0..10_000 {
        let _ = cleanup.attempt(tick, || token.release());
    }
    assert!(cleanup.is_quarantined());
    assert_eq!(token.mask(), mask); // Unconfirmed ownership is retained.
    MSRS.with_borrow(|state| {
        assert_eq!(
            state
                .accesses
                .iter()
                .filter(|(write, msr, _)| *write && *msr == AMD_LOCAL)
                .count(),
            8
        );
        assert!(state.accesses.len() <= 24);
    });
}
