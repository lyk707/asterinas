// SPDX-License-Identifier: MPL-2.0

//! Host fault-injection tests of the production backend transaction modules.
//! No privileged instruction is executed. This does not replace guest tests.

#![allow(dead_code)]

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
};

#[path = "../../ostd/src/pmu/types.rs"]
mod types;
use types::{
    BackendKind, ConfigFailure, CountingScope, Event, EventConfig, PmuCaps, PmuError, Result,
};
#[path = "../../ostd/src/pmu/sbi_counter.rs"]
mod sbi_counter;
#[path = "../../ostd/src/pmu/x86_counter.rs"]
mod x86_counter;

const AMD_LOCAL: u32 = 0xc001_0200;
const AMD_GLOBAL: u32 = 0xc000_0301;
const INTEL_LOCAL: u32 = 0x38d;
const INTEL_GLOBAL: u32 = 0x38f;
const ENABLE: u64 = 1 << 22;

fn caps(kind: BackendKind) -> PmuCaps {
    PmuCaps {
        backend: kind,
        counters: 2,
        has_precise_filter: true,
        width_bits: 48,
        has_global_control: true,
        candidates: if kind == BackendKind::Sbi {
            0b11000
        } else {
            0b11
        },
    }
}

fn config() -> EventConfig {
    EventConfig::new(Event::CpuCycles, CountingScope::User)
}

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
    x86_counter::configure::<Msr>(config(), caps(BackendKind::AmdZen), 0)
        .unwrap()
        .0
}

#[test]
fn failed_snapshot_never_writes_or_fabricates_rollback_token() {
    for (kind, global) in [
        (BackendKind::AmdZen, AMD_GLOBAL),
        (BackendKind::IntelFixed, INTEL_GLOBAL),
    ] {
        reset_msrs(global, u64::MAX);
        fail_msr(false, global);
        let failure = x86_counter::configure::<Msr>(config(), caps(kind), 0).unwrap_err();
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
    let failure =
        x86_counter::configure::<Msr>(config(), caps(BackendKind::AmdZen), 1).unwrap_err();
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
    let failure =
        x86_counter::configure::<Msr>(config(), caps(BackendKind::AmdZen), 0).unwrap_err();
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
    let (mut token, _) =
        x86_counter::configure::<Msr>(config(), caps(BackendKind::IntelFixed), 0).unwrap();
    token.start().unwrap();
    token.stop().unwrap();
    token.release().unwrap();
    assert_eq!(msr_value(INTEL_LOCAL), 0x03);
    assert_eq!(msr_value(INTEL_GLOBAL), saved);
}

#[test]
fn amd_legacy_never_accesses_perfmon_v2_registers() {
    reset_msrs(AMD_GLOBAL, 0);
    let mut legacy = caps(BackendKind::AmdZen);
    legacy.has_global_control = false;
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
    let failure =
        x86_counter::configure::<Msr>(config(), caps(BackendKind::AmdZen), 0).unwrap_err();
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

struct SbiState {
    index: usize,
    info: usize,
    mapped: bool,
    running: bool,
    raw: u64,
    old_reset_semantics: bool,
    read_fault: bool,
    faults: VecDeque<(usize, isize)>,
    calls: Vec<(usize, [usize; 5])>,
}
impl Default for SbiState {
    fn default() -> Self {
        Self {
            index: 3,
            info: 0xc03 | (47 << 12),
            mapped: false,
            running: false,
            raw: 10,
            old_reset_semantics: false,
            read_fault: false,
            faults: VecDeque::new(),
            calls: Vec::new(),
        }
    }
}
thread_local! { static SBI: RefCell<SbiState> = RefCell::new(SbiState::default()); }
#[derive(Debug)]
struct Sbi;
impl sbi_counter::Firmware for Sbi {
    fn call(
        eid: usize,
        fid: usize,
        args: [usize; 5],
    ) -> core::result::Result<usize, sbi_counter::SbiError> {
        assert_eq!(eid, sbi_counter::SBI_EXT_PMU);
        SBI.with_borrow_mut(|state| {
            state.calls.push((fid, args));
            if state.faults.front().is_some_and(|(id, _)| *id == fid) {
                return Err(sbi_counter::SbiError(state.faults.pop_front().unwrap().1));
            }
            match fid {
                1 => Ok(state.info),
                2 => {
                    state.mapped = true;
                    Ok(state.index)
                }
                3 if !state.mapped => Err(sbi_counter::SbiError(-3)),
                3 if state.running => Err(sbi_counter::SbiError(-7)),
                3 => {
                    state.running = true;
                    Ok(0)
                }
                4 if !state.mapped => Err(sbi_counter::SbiError(-3)),
                4 => {
                    let was_running = state.running;
                    state.running = false;
                    if was_running {
                        state.raw += 5;
                    }
                    if args[2] == 1 && (was_running || state.old_reset_semantics) {
                        state.mapped = false;
                    }
                    if was_running {
                        Ok(0)
                    } else {
                        Err(sbi_counter::SbiError(-8))
                    }
                }
                _ => panic!("unexpected SBI function {fid}"),
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
fn sbi() -> sbi_counter::Token<Sbi> {
    sbi_counter::configure::<Sbi>(config(), caps(BackendKind::Sbi), 0)
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
            assert!(!state.mapped && !state.running);
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
        SBI.with_borrow(|state| assert!(!state.mapped && !state.running));
    }
}

#[test]
fn failed_reset_retains_ownership_and_retries_cleanup() {
    reset_sbi();
    let mut token = sbi();
    SBI.with_borrow_mut(|state| state.faults.push_back((4, -1)));
    assert!(token.release().is_err());
    SBI.with_borrow(|state| assert!(state.mapped && state.running));
    token.release().unwrap();
    SBI.with_borrow(|state| assert!(!state.mapped && !state.running));
}

#[test]
fn failed_cleanup_start_does_not_forget_mapping() {
    reset_sbi();
    let mut token = sbi();
    SBI.with_borrow_mut(|state| state.faults.push_back((3, -1)));
    assert!(token.release().is_err());
    SBI.with_borrow(|state| assert!(state.mapped));
    token.release().unwrap();
}

#[test]
fn metadata_and_csr_failures_keep_a_cleanup_token() {
    for fault in 0..3 {
        reset_sbi();
        SBI.with_borrow_mut(|state| match fault {
            0 => state.faults.push_back((1, -1)),
            1 => state.info = 0xc01 | (47 << 12),
            _ => state.read_fault = true,
        });
        let failure =
            sbi_counter::configure::<Sbi>(config(), caps(BackendKind::Sbi), 0).unwrap_err();
        let mut token = failure
            .token
            .expect("successful CONFIG_MATCHING must retain ownership");
        token.release().unwrap();
        SBI.with_borrow(|state| assert!(!state.mapped));
    }
}

#[test]
fn invalid_sbi_identity_is_rejected_before_narrowing_or_shift() {
    for index in [64, usize::MAX, (1usize << 32) + 3, 2] {
        reset_sbi();
        SBI.with_borrow_mut(|state| state.index = index);
        let failure =
            sbi_counter::configure::<Sbi>(config(), caps(BackendKind::Sbi), 0).unwrap_err();
        assert_eq!(failure.error, PmuError::HardwareFault);
        assert!(failure.token.is_none());
    }
}

#[test]
fn sbi_never_offers_an_owned_counter_and_sets_requested_filters() {
    reset_sbi();
    SBI.with_borrow_mut(|state| state.index = 4);
    let mut token = sbi_counter::configure::<Sbi>(config(), caps(BackendKind::Sbi), 1 << 3)
        .unwrap()
        .0;
    SBI.with_borrow(|state| {
        let args = state.calls[0].1;
        assert_eq!(args[1], 1 << 4);
        assert_eq!(args[2], (1 << 7) | (1 << 6) | (1 << 4) | (1 << 3));
        assert_eq!(args[3], 1);
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
    for csr in [0xc00, 0xc02, 0xc03, 0xc1f] {
        let info = sbi_counter::CounterInfo::decode(csr | (63 << 12)).unwrap();
        assert!(!info.has_filter(false, false));
        assert!(info.has_filter(true, true));
        assert_eq!(info.has_filter(true, false), csr == 0xc00 || csr == 0xc02);
        assert_eq!(info.has_filter(false, true), csr != 0xc00 && csr != 0xc02);
    }
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

#[path = "../../kernel/core/src/events/perf/attributes.rs"]
mod attributes;
#[path = "../../kernel/core/src/events/perf/observation.rs"]
mod observation;
