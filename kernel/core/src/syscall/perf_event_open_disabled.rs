// SPDX-License-Identifier: MPL-2.0

use super::SyscallReturn;
use crate::prelude::*;

/// Returns `EOPNOTSUPP` when the kernel is built without the `pmu` feature.
pub(super) fn sys_perf_event_open(
    _attr_addr: Vaddr,
    _pid: i32,
    _cpu: i32,
    _group_fd: i32,
    _flags: u64,
    _ctx: &Context,
) -> Result<SyscallReturn> {
    return_errno_with_message!(Errno::EOPNOTSUPP, "hardware PMU counting is not enabled");
}
