// SPDX-License-Identifier: MPL-2.0

//! Hardware-backed context handoff tests. Run with two PMU-capable CPUs.
//!
//! These drive the scheduler hooks on real CPUs but do not migrate a task in
//! the scheduler. The user-space migration regression remains a separate test.

use core::hint::black_box;

use ostd::{
    cpu::{CpuId, CpuSet, all_cpus},
    irq,
    pmu::{self, CountingScope, Event, EventConfig},
    prelude::ktest,
    smp::inter_processor_call,
    sync::LocalIrqDisabled,
};

use super::{EventSpec, PerfContext};
use crate::prelude::*;

// IPI callbacks take function pointers, so publish the test's context here.
static CONTEXT: SpinLock<Option<Arc<PerfContext>>, LocalIrqDisabled> = SpinLock::new(None);

fn run_segment() {
    let context = CONTEXT.lock().as_ref().unwrap().clone();
    let irq_guard = irq::disable_local();
    context.on_schedule_in();
    let mut value = 1u64;
    for i in 0..1000 {
        value = black_box(value.wrapping_mul(17).wrapping_add(i));
    }
    black_box(value);
    context.on_tick();
    context.on_schedule_out(&irq_guard);
}

fn on_cpu(cpu: CpuId, callback: fn()) {
    inter_processor_call(&CpuSet::from(cpu), callback).wait();
}

#[ktest]
fn context_handoff_between_cpus() {
    let mut cpus = all_cpus();
    let cpu_a = cpus.next().unwrap();
    let cpu_b = cpus
        .next()
        .expect("PMU context handoff test requires SMP >= 2");

    component::init_all(
        component::InitStage::Bootstrap,
        component::parse_metadata!(),
    )
    .unwrap();
    for cpu in [cpu_a, cpu_b] {
        on_cpu(cpu, || {
            pmu::init_on_cpu().expect("PMU context handoff test requires hardware counters");
        });
    }

    // Repeat with a CPU filter to prove that an ineligible destination does
    // not count, while returning to the original CPU resumes the same event.
    for filter in [None, Some(cpu_a)] {
        let context = Arc::new(PerfContext::new());
        let tickets = [Event::CpuCycles, Event::Instructions].map(|event| {
            context
                .create(EventSpec {
                    config: EventConfig::new(event, CountingScope::Kernel),
                    cpu: filter,
                    is_initially_disabled: false,
                    should_enable_on_exec: false,
                })
                .unwrap()
        });
        *CONTEXT.lock() = Some(context.clone());
        let mut previous = tickets.map(|ticket| context.read(ticket).unwrap());

        for cpu in [cpu_a, cpu_b, cpu_a, cpu_b, cpu_a] {
            on_cpu(cpu, run_segment);
            for (ticket, before) in tickets.into_iter().zip(&mut previous) {
                let after = context.read(ticket).unwrap();
                if filter.is_none_or(|allowed| allowed == cpu) {
                    assert!(after.count > before.count);
                    assert!(after.running_ns > before.running_ns);
                } else {
                    assert_eq!(after.count, before.count);
                    assert_eq!(after.running_ns, before.running_ns);
                }
                *before = after;
            }

            // Both slots must be reusable immediately after schedule-out.
            // Another context's work must not appear in the first context.
            let other = Arc::new(PerfContext::new());
            let other_tickets = [Event::CpuCycles, Event::Instructions].map(|event| {
                other
                    .create(EventSpec {
                        config: EventConfig::new(event, CountingScope::Kernel),
                        cpu: None,
                        is_initially_disabled: false,
                        should_enable_on_exec: false,
                    })
                    .unwrap()
            });
            *CONTEXT.lock() = Some(other.clone());
            on_cpu(cpu, run_segment);
            for ticket in other_tickets {
                assert!(other.read(ticket).unwrap().count > 0);
                other.close(ticket);
            }
            for (ticket, before) in tickets.into_iter().zip(previous) {
                let after = context.read(ticket).unwrap();
                assert_eq!(after.count, before.count);
                assert_eq!(after.running_ns, before.running_ns);
            }
            *CONTEXT.lock() = Some(context.clone());
        }

        for ticket in tickets {
            context.close(ticket);
        }
        *CONTEXT.lock() = None;
    }
}
