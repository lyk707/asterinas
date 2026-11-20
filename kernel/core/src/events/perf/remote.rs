// SPDX-License-Identifier: MPL-2.0

//! Run bounded PMU requests on the owner CPU's existing timer path.

use ostd::{
    cpu::{CpuId, PinCurrentCpu},
    cpu_local, irq,
    sync::{LocalIrqDisabled, Waiter},
    task::atomic_mode,
};

use crate::prelude::*;

const MAX_QUEUED_REQUESTS: usize = 32;
const MAX_REQUESTS_PER_TICK: usize = 8;

type Request = Box<dyn FnOnce() + Send>;

cpu_local! {
    static REQUESTS: SpinLock<VecDeque<Request>, LocalIrqDisabled> =
        SpinLock::new(VecDeque::new());
}

/// Queues an operation and waits in task context for its own result.
///
/// The operation runs with local IRQs disabled. The caller must release any
/// lock that it may acquire before waiting.
pub(super) fn call_on_cpu<T: Send + 'static>(
    cpu: CpuId,
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    atomic_mode::might_sleep();
    let (waiter, waker) = Waiter::new_pair();
    let result = Arc::new(SpinLock::<Option<T>, LocalIrqDisabled>::new(None));
    let remote_result = result.clone();
    let request = Box::new(move || {
        let value = operation();
        *remote_result.lock() = Some(value);
        waker.wake_up();
    });

    {
        let mut requests = REQUESTS.get_on_cpu(cpu).lock();
        if requests.len() == MAX_QUEUED_REQUESTS {
            return_errno_with_message!(Errno::EBUSY, "the PMU owner CPU request queue is full");
        }
        requests
            .try_reserve(1)
            .map_err(|_| Error::with_message(Errno::ENOMEM, "cannot queue the PMU request"))?;
        requests.push_back(request);
    }

    waiter.wait();
    let value = result.lock().take().unwrap();
    Ok(value)
}

/// Runs a bounded batch without holding the queue lock across a callback.
pub(super) fn service_requests() {
    let irq_guard = irq::disable_local();
    let requests = REQUESTS.get_on_cpu(irq_guard.current_cpu());
    for _ in 0..MAX_REQUESTS_PER_TICK {
        let request = requests.lock().pop_front();
        let Some(request) = request else { break };
        request();
    }
}
