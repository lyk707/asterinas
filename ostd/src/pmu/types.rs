// SPDX-License-Identifier: MPL-2.0

//! Shared PMU configuration and error types.

/// A hardware performance event to count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    /// Processor clock cycles.
    CpuCycles,
    /// Retired architectural instructions.
    Instructions,
}

impl Event {
    /// Returns the logical per-CPU slot index used by this event.
    pub const fn index(self) -> usize {
        match self {
            Self::CpuCycles => 0,
            Self::Instructions => 1,
        }
    }
}

/// The privilege levels a counting event observes.
///
/// "Count nothing" is deliberately not representable: a configuration that
/// excludes both levels is meaningless and is rejected before this type exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CountingScope {
    /// Count only user-mode (CPL3 / U-mode) execution.
    User,
    /// Count only kernel-mode (CPL0 / S-mode) execution.
    Kernel,
    /// Count execution in both modes.
    UserAndKernel,
}

impl CountingScope {
    /// Builds a scope from the two privilege filters, rejecting "exclude all".
    ///
    /// Returns `None` when both levels are excluded.
    pub const fn from_filters(include_user: bool, include_kernel: bool) -> Option<Self> {
        match (include_user, include_kernel) {
            (true, true) => Some(Self::UserAndKernel),
            (true, false) => Some(Self::User),
            (false, true) => Some(Self::Kernel),
            (false, false) => None,
        }
    }

    /// Returns whether user-mode execution is counted.
    pub const fn counts_user(self) -> bool {
        matches!(self, Self::User | Self::UserAndKernel)
    }

    /// Returns whether kernel-mode execution is counted.
    pub const fn counts_kernel(self) -> bool {
        matches!(self, Self::Kernel | Self::UserAndKernel)
    }
}

/// A hardware event and its requested privilege-level filter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventConfig {
    event: Event,
    scope: CountingScope,
}

impl EventConfig {
    /// Creates a new event configuration.
    pub const fn new(event: Event, scope: CountingScope) -> Self {
        Self { event, scope }
    }

    /// Returns the hardware event to measure.
    pub const fn event(&self) -> Event {
        self.event
    }

    /// Returns the privilege levels this event observes.
    pub const fn scope(&self) -> CountingScope {
        self.scope
    }

    /// Returns whether execution in user mode (CPL3 / U-mode) is counted.
    pub const fn is_user_counted(&self) -> bool {
        self.scope.counts_user()
    }

    /// Returns whether execution in kernel mode (CPL0 / S-mode) is counted.
    pub const fn is_kernel_counted(&self) -> bool {
        self.scope.counts_kernel()
    }
}

/// The detected hardware or firmware PMU interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    /// Intel architectural PMU (fixed counters 0 and 1).
    IntelFixed,
    /// AMD Zen core performance counters.
    AmdZen,
    /// RISC-V SBI PMU extension with hardware CSR readout.
    Sbi,
}

/// The PMU capabilities cached for one CPU during initialization.
#[derive(Clone, Copy, Debug)]
pub struct PmuCaps {
    pub(super) backend: BackendKind,
    pub(super) counters: u8,
    pub(super) has_precise_filter: bool,
    pub(super) width_bits: u8,
    #[cfg_attr(not(target_arch = "x86_64"), expect(dead_code))]
    pub(super) has_global_control: bool,
    pub(super) candidates: u64,
}

impl PmuCaps {
    /// Returns the detected register or firmware interface kind.
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }

    /// Returns the number of managed counters on this CPU.
    pub const fn counters(&self) -> u8 {
        self.counters
    }

    /// Returns whether hardware privilege-level filtering is supported.
    pub const fn has_precise_filter(&self) -> bool {
        self.has_precise_filter
    }

    /// Returns the counter bit width.
    pub const fn width_bits(&self) -> u8 {
        self.width_bits
    }
}

/// A failure returned by a PMU operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PmuError {
    /// No supported PMU is exposed on this CPU.
    Unsupported,
    /// The platform cannot guarantee the requested privilege filter.
    UnsupportedFilter,
    /// Neither user nor kernel execution was requested.
    InvalidConfig,
    /// No managed counter resource is free for the event.
    NoCounter,
    /// A handle was accessed from a CPU other than its owner.
    WrongCpu,
    /// The counter allocation has ended or is in an invalid state.
    InvalidHandle,
    /// A hardware register access or firmware call failed.
    HardwareFault,
    /// A counter accumulator overflowed or the observation interval was too long.
    Overflow,
}

/// A specialized result type for PMU operations.
pub type Result<T> = core::result::Result<T, PmuError>;

/// A failed configuration which may still own a resource requiring cleanup.
#[derive(Debug)]
pub(super) struct ConfigFailure<T> {
    pub(super) error: PmuError,
    pub(super) token: Option<T>,
}

impl<T> From<PmuError> for ConfigFailure<T> {
    fn from(error: PmuError) -> Self {
        Self { error, token: None }
    }
}
