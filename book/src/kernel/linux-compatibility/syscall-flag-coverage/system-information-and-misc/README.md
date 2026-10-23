# System Information & Misc.

<!--
Put system calls such as
uname, getrlimit, reboot, setrlimit, sysinfo, times, gettimeofday, clock_gettime,
clock_settime, getrusage, getdents, getdents64, personality, syslog,
arch_prctl, set_tid_address, and getrandom
under this category.
-->

### `arch_prctl`

Supported functionality in SCML:

```c
{{#include arch_prctl.scml}}
```

Unsupported codes:
* `ARCH_GET_CPUID` and `ARCH_SET_CPUID`

For more information,
see [the man page](https://man7.org/linux/man-pages/man2/arch_prctl.2.html).

### `getrusage`

Supported functionality in SCML:

```c
{{#include getrusage.scml}}
```

Unsupported `who` flags:
* `RUSAGE_CHILDREN`

For more information,
see [the man page](https://man7.org/linux/man-pages/man2/getrusage.2.html).

### `perf_event_open`

Available with the `pmu` feature and a supported PMU with privilege filtering.
Without this feature, the syscall returns `EOPNOTSUPP`.

```c
{{#include perf_event_open.scml}}
```

Supports per-thread hardware cycles and instructions for `pid >= 0`,
`cpu >= -1`, and `group_fd == -1`.
SCML uses `<INTEGER>` for `group_fd` because its grammar cannot express negative
literals; this does not imply support for other group descriptors.
`CAP_PERFMON` or `CAP_SYS_ADMIN` is required, including for self-monitoring.
The 64-byte attribute prefix is supported; larger structures require zero tails.
Supported attribute bits are `disabled`, `exclude_user`, `exclude_kernel`,
`exclude_hv`, `exclude_idle`, `enable_on_exec`, and `exclude_guest`.
Idle tasks and guest/hypervisor execution are outside this per-thread scope.
Excluding both user and kernel execution returns `EINVAL`.

All eight non-group combinations of time-enabled, time-running, and ID fields
are supported by `read`. The enable, disable, reset, and ID ioctls are supported.
Reset preserves elapsed times. Time enabled excludes time off the target CPU
context; counts remain readable after the target thread exits.

Inheritance (including inheritance with enable-on-exec), pinned/exclusive events,
host exclusion, sampling, groups, multiplexing, and system-wide events are
unsupported. Use standard `perf stat --no-inherit -e cycles:u,instructions:u`.
This measures the selected thread; descendants are not aggregated.

For more information, see
[the man page](https://man7.org/linux/man-pages/man2/perf_event_open.2.html).

### `getrandom`

Supported functionality in SCML:

```c
{{#include getrandom.scml}}
```

Silently-ignored flags:
* `GRND_NONBLOCK` because the underlying operation never blocks

For more information,
see [the man page](https://man7.org/linux/man-pages/man2/getrandom.2.html).

### `reboot`

Supported functionality in SCML:

```c
{{#include reboot.scml}}
```

Unsupported `op` flags:
* `LINUX_REBOOT_CMD_CAD_OFF`
* `LINUX_REBOOT_CMD_CAD_ON`
* `LINUX_REBOOT_CMD_KEXEC`
* `LINUX_REBOOT_CMD_RESTART2`
* `LINUX_REBOOT_CMD_SW_SUSPEND`

For more information,
see [the man page](https://man7.org/linux/man-pages/man2/reboot.2.html).

### `sysinfo`

Supported functionality in SCML:

```c
{{#include sysinfo.scml}}
```

Supported returned fields:
* `uptime`, rounded up to seconds when it has a fractional part
* `loads`, containing the 1-, 5-, and 15-minute load averages as fixed-point values scaled by `1 << SI_LOAD_SHIFT`
* `totalram`, `freeram`, `procs`, and `mem_unit`

The `sharedram`, `bufferram`, `totalhigh`, and `freehigh` fields are reported as zero.
The `totalswap` and `freeswap` fields are reported as zero because swap is not supported.

For more information,
see [the man page](https://man7.org/linux/man-pages/man2/sysinfo.2.html).

## POSIX clocks

### `clock_gettime`

Supported functionality in SCML:

```c
{{#include clock_gettime.scml}}
```

Unsupported predefined clock IDs:
* `CLOCK_REALTIME_ALARM`
* `CLOCK_BOOTTIME_ALARM`
* `CLOCK_TAI`

For more information,
see [the man page](https://man7.org/linux/man-pages/man2/clock_gettime.2.html).

### `clock_nanosleep`

Supported functionality in SCML:

```c
{{#include clock_nanosleep.scml}}
```

Unsupported clock IDs:
* `CLOCK_TAI`

For more information,
see [the man page](https://man7.org/linux/man-pages/man2/clock_nanosleep.2.html).
