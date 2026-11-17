// SPDX-License-Identifier: MPL-2.0

use crate::{retirement, types::PmuError};

#[test]
fn retired_cleanup_backs_off_then_stops_touching_faulted_hardware() {
    let mut cleanup = retirement::Cleanup::new();
    let mut attempts = Vec::new();
    for tick in 0..10_000 {
        let result = cleanup.attempt(tick, || {
            attempts.push(tick);
            Err(PmuError::HardwareFault)
        });
        assert!(result.is_none_or(|r| r == Err(PmuError::HardwareFault)));
    }
    assert_eq!(attempts, [0, 1, 3, 7, 15, 31, 63, 127]);
    assert!(cleanup.is_quarantined());
    assert!(
        cleanup
            .attempt(u64::MAX, || panic!("quarantined hardware accessed"))
            .is_none()
    );
}

#[test]
fn retired_cleanup_can_recover_without_access_during_backoff() {
    let mut cleanup = retirement::Cleanup::new();
    assert_eq!(
        cleanup.attempt(10, || Err(PmuError::HardwareFault)),
        Some(Err(PmuError::HardwareFault))
    );
    assert!(
        cleanup
            .attempt(10, || panic!("retry before deadline"))
            .is_none()
    );
    assert_eq!(cleanup.attempt(11, || Ok(())), Some(Ok(())));
    assert!(!cleanup.is_quarantined());
}

#[test]
fn retired_cleanup_deadline_saturates_without_infinite_retry() {
    let mut cleanup = retirement::Cleanup::new();
    for _ in 0..8 {
        assert_eq!(
            cleanup.attempt(u64::MAX, || Err(PmuError::HardwareFault)),
            Some(Err(PmuError::HardwareFault))
        );
    }
    assert!(cleanup.is_quarantined());
    assert!(
        cleanup
            .attempt(u64::MAX, || panic!("retry budget exhausted"))
            .is_none()
    );
}
