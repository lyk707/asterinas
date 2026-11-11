// SPDX-License-Identifier: MPL-2.0

//! Counter width masking and modular wrap-around arithmetic.
//!
//! This module is independent of the underlying register or firmware interface
//! so that counter accumulation logic can be unit-tested on the host.

/// The maximum bit width of a hardware performance counter.
const MAX_COUNTER_WIDTH_BITS: u8 = 64;

/// Returns a bitmask for a hardware counter of `width_bits` bits.
pub(super) const fn mask(width_bits: u8) -> u64 {
    if width_bits >= MAX_COUNTER_WIDTH_BITS {
        u64::MAX
    } else {
        (1u64 << width_bits) - 1
    }
}

/// Accumulates one hardware counter observation into `total`.
///
/// The caller must sample the counter before a full `2^width_bits` wrap occurs.
/// Returns `None` if the 64-bit software accumulator overflows.
pub(super) fn accumulate(total: &mut u64, last: &mut u64, raw: u64, width_bits: u8) -> Option<u64> {
    let counter_mask = mask(width_bits);
    let masked_raw = raw & counter_mask;
    let delta = masked_raw.wrapping_sub(*last) & counter_mask;

    *total = total.checked_add(delta)?;
    *last = masked_raw;
    Some(*total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_the_hardware_width() {
        let (mut total, mut last) = (10, 250);
        assert_eq!(accumulate(&mut total, &mut last, 5, 8), Some(21));
        assert_eq!(accumulate(&mut total, &mut last, 5, 8), Some(21));
    }

    #[test]
    fn wraps_at_forty_eight_bit_width() {
        let width_bits = 48;
        let max_val = (1u64 << width_bits) - 1;
        let (mut total, mut last) = (100, max_val - 9);
        assert_eq!(accumulate(&mut total, &mut last, 15, width_bits), Some(125));
        assert_eq!(last, 15);
    }

    #[test]
    fn handles_sixty_four_bit_counters_without_shifting_by_sixty_four() {
        let (mut total, mut last) = (0, u64::MAX - 2);
        assert_eq!(accumulate(&mut total, &mut last, 3, 64), Some(6));
    }

    #[test]
    fn logical_overflow_is_an_error() {
        let (mut total, mut last) = (u64::MAX, 0);
        assert_eq!(accumulate(&mut total, &mut last, 1, 48), None);
    }
}
