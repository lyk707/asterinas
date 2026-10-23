// SPDX-License-Identifier: MPL-2.0

//! Conservative observation deadlines for modular hardware counters.

// A platform assumption, not an architectural limit: supported systems must
// retire fewer than 1024 instructions/cycles per nanosecond. In particular,
// instructions can retire several times faster than the clock frequency.
const MAX_INCREMENTS_PER_NS: u128 = 1024;

/// Enabled time on the target task's scheduled context, excluding sleep and
/// the temporary freezes used by control/read operations.
#[derive(Debug, Default)]
pub(super) struct ActiveTime {
    total: u64,
    since: Option<u64>,
}

impl ActiveTime {
    pub(super) fn total(&self) -> u64 {
        self.total
    }
    pub(super) fn is_active(&self) -> bool {
        self.since.is_some()
    }

    pub(super) fn update(&mut self, now: u64, active: bool) -> Option<()> {
        if let Some(since) = self.since {
            self.total = self.total.checked_add(now.checked_sub(since)?)?;
        }
        self.since = active.then_some(now);
        Some(())
    }
}

pub(super) fn interval_is_safe(width_bits: u8, last_ns: u64, now_ns: u64) -> bool {
    if !(32..=64).contains(&width_bits) {
        return false;
    }
    let max_gap_ns = (1u128 << width_bits) / MAX_INCREMENTS_PER_NS;
    now_ns
        .checked_sub(last_ns)
        .is_some_and(|gap| u128::from(gap) < max_gap_ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_time_excludes_sleep_disabled_time_and_read_freezes() {
        let mut time = ActiveTime::default();
        time.update(100, true).unwrap();
        time.update(120, false).unwrap();
        time.update(1000, false).unwrap(); // Sleeping/disabled/frozen.
        assert_eq!(time.total(), 20);
        time.update(2000, true).unwrap();
        time.update(2010, true).unwrap(); // Sampling does not double count.
        time.update(2020, false).unwrap();
        assert_eq!(time.total(), 40);
    }

    #[test]
    fn time_overflow_and_backward_clock_do_not_saturate_silently() {
        let mut time = ActiveTime {
            total: u64::MAX,
            since: Some(10),
        };
        assert!(time.update(11, true).is_none());
        assert_eq!(time.total(), u64::MAX);
        let mut time = ActiveTime::default();
        time.update(10, true).unwrap();
        assert!(time.update(9, false).is_none());
        assert_eq!(time.total(), 0);
    }

    #[test]
    fn full_wrap_deadline_and_clock_regression_fail_closed() {
        let deadline = (1u64 << 32) / 1024;
        assert!(interval_is_safe(32, 10, 10 + deadline - 1));
        assert!(!interval_is_safe(32, 10, 10 + deadline));
        assert!(!interval_is_safe(32, 10, 9));
    }

    #[test]
    fn sixty_four_bit_width_and_invalid_metadata() {
        assert!(interval_is_safe(64, 0, 1_000_000_000));
        for width in [0, 31, 65, 255] {
            assert!(!interval_is_safe(width, 0, 0));
        }
    }
}
