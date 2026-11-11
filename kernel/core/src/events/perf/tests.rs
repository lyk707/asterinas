// SPDX-License-Identifier: MPL-2.0

//! Hardware-backed context handoff tests. Run with two PMU-capable CPUs.
//!
//! These drive the scheduler hooks on real CPUs but do not migrate a task in
//! the scheduler. The user-space migration regression remains a separate test.

use core::hint::black_box;

use ostd::{
    cpu::{CpuId, all_cpus},
    irq,
    pmu::{self, CountingScope, Event, EventConfig},
    prelude::ktest,
    smp::call_on_cpu,
};

use super::{EventSpec, Operation, PerfContext, Ticket};
use crate::prelude::*;

fn burn_cpu() {
    let mut value = 1u64;
    for i in 0..1000 {
        value = black_box(value.wrapping_mul(17).wrapping_add(i));
    }
    black_box(value);
}

fn run_segment(cpu: CpuId, context: &Arc<PerfContext>) {
    let context = context.clone();
    call_on_cpu(cpu, move || {
        let irq_guard = irq::disable_local();
        context.on_schedule_in();
        burn_cpu();
        context.on_tick();
        context.on_schedule_out(&irq_guard);
    });
}

fn create_events(context: &Arc<PerfContext>, cpu: Option<CpuId>) -> [Ticket; 2] {
    [Event::CpuCycles, Event::Instructions].map(|event| {
        context
            .create(EventSpec {
                config: EventConfig::new(event, CountingScope::Kernel),
                cpu,
                is_initially_disabled: false,
                should_enable_on_exec: false,
            })
            .unwrap()
    })
}

fn unavailable(reason: &str) {
    assert_ne!(
        option_env!("ASTERINAS_REQUIRE_PMU_KTEST"),
        Some("1"),
        "{reason}"
    );
    warn!("PMU handoff skipped: {reason}");
}

#[ktest]
fn context_handoff_between_cpus() {
    let mut cpus = all_cpus();
    let cpu_a = cpus.next().unwrap();
    let Some(cpu_b) = cpus.next() else {
        unavailable("two CPUs are required");
        return;
    };

    component::init_all(
        component::InitStage::Bootstrap,
        component::parse_metadata!(),
    )
    .unwrap();
    for cpu in [cpu_a, cpu_b] {
        if call_on_cpu(cpu, pmu::init_on_cpu).is_err() {
            unavailable("hardware PMU counters are required on both CPUs");
            return;
        }
    }

    // Repeat with a CPU filter to prove that an ineligible destination does
    // not count, while returning to the original CPU resumes the same event.
    for filter in [None, Some(cpu_a)] {
        let context = Arc::new(PerfContext::new());
        let tickets = create_events(&context, filter);
        let mut previous = tickets.map(|ticket| context.read(ticket).unwrap());

        for cpu in [cpu_a, cpu_b, cpu_a, cpu_b, cpu_a] {
            run_segment(cpu, &context);
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
            let other_tickets = create_events(&other, None);
            run_segment(cpu, &other);
            for ticket in other_tickets {
                assert!(other.read(ticket).unwrap().count > 0);
                other.close(ticket);
            }
            for (ticket, before) in tickets.into_iter().zip(previous) {
                let after = context.read(ticket).unwrap();
                assert_eq!(after.count, before.count);
                assert_eq!(after.running_ns, before.running_ns);
            }
        }

        for ticket in tickets {
            context.close(ticket);
        }
    }

    let remote = if CpuId::current_racy() == cpu_a {
        cpu_b
    } else {
        cpu_a
    };
    check_live_remote_reads(remote);
}

fn check_live_remote_reads(cpu: CpuId) {
    let context = Arc::new(PerfContext::new());
    let tickets = create_events(&context, None);
    let owner_context = context.clone();
    call_on_cpu(cpu, move || owner_context.on_schedule_in());

    let segment_starts = || {
        let state = context.state.lock();
        state
            .records
            .each_ref()
            .map(|record| record.as_ref().unwrap().live.as_ref().unwrap().since_ns)
    };
    let initial_starts = segment_starts();
    let first = tickets.map(|ticket| context.read(ticket).unwrap());
    call_on_cpu(cpu, burn_cpu);
    let second = tickets.map(|ticket| context.read(ticket).unwrap());
    assert_eq!(segment_starts(), initial_starts);
    for (before, after) in first.into_iter().zip(second) {
        assert!(after.count > before.count);
        assert!(after.running_ns >= before.running_ns);
    }

    context.control(tickets[0], Operation::Reset).unwrap();
    assert_eq!(segment_starts(), initial_starts);
    assert!(context.read(tickets[1]).unwrap().count >= second[1].count);

    let owner_context = context.clone();
    call_on_cpu(cpu, move || {
        owner_context.on_schedule_out(&irq::disable_local())
    });
    for ticket in tickets {
        context.close(ticket);
    }
}
