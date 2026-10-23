// SPDX-License-Identifier: MPL-2.0

//! The deliberately limited counting ABI, shared with host regression tests.

pub(crate) const ATTR_DISABLED: u64 = 1 << 0;
pub(crate) const ATTR_EXCLUDE_USER: u64 = 1 << 4;
pub(crate) const ATTR_EXCLUDE_KERNEL: u64 = 1 << 5;
pub(crate) const ATTR_ENABLE_ON_EXEC: u64 = 1 << 12;

const ATTR_EXCLUDE_HV: u64 = 1 << 6;
const ATTR_EXCLUDE_IDLE: u64 = 1 << 7;
const ATTR_EXCLUDE_GUEST: u64 = 1 << 20;

// Per-thread counting never includes the idle task or a guest VM. S-mode
// excludes firmware M-mode. There is no supported hypervisor execution mode.
const SUPPORTED: u64 = ATTR_DISABLED
    | ATTR_EXCLUDE_USER
    | ATTR_EXCLUDE_KERNEL
    | ATTR_EXCLUDE_HV
    | ATTR_EXCLUDE_IDLE
    | ATTR_ENABLE_ON_EXEC
    | ATTR_EXCLUDE_GUEST;

pub(crate) const fn supported_flags(flags: u64) -> bool {
    flags & !SUPPORTED == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inheritance_is_rejected_even_with_enable_on_exec() {
        assert!(!supported_flags(1 << 1));
        assert!(!supported_flags(
            (1 << 1) | ATTR_DISABLED | ATTR_ENABLE_ON_EXEC
        ));
    }

    #[test]
    fn unsupported_modes_are_not_silently_accepted() {
        for bit in 0..64 {
            assert_eq!(supported_flags(1 << bit), SUPPORTED & (1 << bit) != 0);
        }
        assert!(!supported_flags(1 << 2)); // pinned
        assert!(!supported_flags(1 << 19)); // exclude_host
    }

    #[test]
    fn no_inherit_perf_stat_flags_are_supported() {
        assert!(supported_flags(
            ATTR_DISABLED | ATTR_ENABLE_ON_EXEC | ATTR_EXCLUDE_KERNEL | ATTR_EXCLUDE_GUEST
        ));
        assert!(supported_flags(0));
    }
}
