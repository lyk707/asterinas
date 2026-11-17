// SPDX-License-Identifier: MPL-2.0

//! Host fault-injection tests of the production backend transaction modules.
//! No privileged instruction is executed.
//! This does not replace guest tests.

#[path = "../../ostd/src/pmu/types.rs"]
#[allow(dead_code)]
mod types;
use types::{BackendKind, ConfigFailure, Event, EventConfig, PmuCaps, PmuError, Result};
#[path = "../../kernel/core/src/events/perf/activity.rs"]
mod activity;
#[path = "../../ostd/src/pmu/retirement.rs"]
mod retirement;
#[path = "../../ostd/src/pmu/sbi_counter.rs"]
#[allow(dead_code)]
mod sbi_counter;
#[path = "../../ostd/src/pmu/x86_counter.rs"]
#[allow(dead_code)]
mod x86_counter;

#[path = "../../kernel/core/src/events/perf/attributes.rs"]
mod attributes;
#[path = "../../kernel/core/src/events/perf/observation.rs"]
mod observation;

#[path = "backend_tests/activity_tests.rs"]
mod activity_tests;
#[path = "backend_tests/msr_tests.rs"]
mod msr_tests;
#[path = "backend_tests/retirement_tests.rs"]
mod retirement_tests;
#[path = "backend_tests/sbi_tests.rs"]
mod sbi_tests;

fn config() -> types::EventConfig {
    types::EventConfig::new(types::Event::CpuCycles, types::CountingScope::User)
}
