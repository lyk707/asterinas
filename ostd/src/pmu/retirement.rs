// SPDX-License-Identifier: MPL-2.0

//! Bounded hardware-release retries. Quarantined tokens must remain reserved.

use super::Result;

/// Includes the initial release attempt; failures are separated by 1, 2, 4, ... ticks.
const MAX_RELEASE_ATTEMPTS: u8 = 8;

#[derive(Debug)]
pub(super) struct Cleanup {
    failures: u8,
    next_attempt: u64,
}

impl Cleanup {
    pub(super) const fn new() -> Self {
        Self {
            failures: 0,
            next_attempt: 0,
        }
    }

    pub(super) fn is_quarantined(&self) -> bool {
        self.failures == MAX_RELEASE_ATTEMPTS
    }

    /// Returns `None` without touching hardware during backoff or after quarantine.
    pub(super) fn attempt(
        &mut self,
        now: u64,
        release: impl FnOnce() -> Result<()>,
    ) -> Option<Result<()>> {
        if self.is_quarantined() || now < self.next_attempt {
            return None;
        }
        let result = release();
        if result.is_err() {
            self.failures += 1;
            self.next_attempt = now.saturating_add(1 << (self.failures - 1));
        }
        Some(result)
    }
}
