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
RISC-V support is limited to QEMU `virt` with one vCPU, `Smcntrpmf`,
generic OpenSBI 1.4 or newer, and the documented
[`ostd.riscv_pmu=icount`](../../kernel-parameters.md#ostdriscv_pmu) launch profile.

```c
{{#include perf_event_open.scml}}
```

Supports per-thread hardware cycles and instructions for `pid >= 0`,
`cpu >= -1`, and `group_fd == -1`.
`CAP_PERFMON` or `CAP_SYS_ADMIN` is required, including for self-monitoring.
Monitoring a thread in another process also requires the alien-access check using real credentials.
Changes to the target's real, effective, or saved UID or GID freeze all existing events for that thread.
Their descriptors can still read the accumulated values.
Re-enabling a revoked event returns `EACCES` while the target thread is alive.
Enabling an event after the target thread exits returns `ESRCH`.
An `execve` that preserves these IDs allows the event to continue or start if `enable_on_exec` is set.
Sustained CPU migration can return `EAGAIN` from an open, read, or event control operation.
A descriptor opened with `PERF_FLAG_FD_CLOEXEC` is closed when its holder executes a new program.
The declared attribute `size` may be `0` (the original 64-byte prefix) or `64` through `PAGE_SIZE`.
Bytes after the 64-byte prefix must be zero.
An unsupported size or a nonzero extension returns `E2BIG` and writes `64` to `size` if writeback succeeds.
Supported attribute bits are `disabled`, `exclude_user`, `exclude_kernel`,
`exclude_hv`, `exclude_idle`, `enable_on_exec`, and `exclude_guest`.
Counting requires `sample_period` to be zero.
The counting subset accepts zero or `PERF_SAMPLE_IDENTIFIER` in `sample_type`.
Standard `perf stat` can set `PERF_SAMPLE_IDENTIFIER` even when counting without sampling.
Idle tasks and guest/hypervisor execution are outside this per-thread scope.
Excluding both user and kernel execution returns `EINVAL`.

All eight non-group combinations of time-enabled, time-running, and ID fields
are supported by `read`.
The `PERF_EVENT_IOC_ENABLE`, `PERF_EVENT_IOC_DISABLE`, and
`PERF_EVENT_IOC_RESET` ioctls require argument `0`.
`PERF_EVENT_IOC_ID` writes the event ID as a `u64` through its output pointer.
Reset preserves elapsed times.
Enabled time accumulates while the enabled event's thread is scheduled on any CPU.
For an event restricted to a CPU, running time and counts only advance on that CPU.
Counts remain readable after the target thread exits.

Inheritance (including inheritance with enable-on-exec), pinned/exclusive events,
host exclusion, sampling, groups, multiplexing, and system-wide events are
unsupported.
A thread can have one cycles event and one instructions event open at a time,
regardless of privilege filter or whether the event is enabled.

For example, in an Asterinas guest with the PMU test tools:

```sh
perf stat --no-inherit -e cycles:u,instructions:u -- /test/io/pmu/pmu_workload 2000000
```

Substitute another workload or select an existing thread with `-t TID`.
The tool measures the selected thread without aggregating its descendants.

From the repository root in the development container,
run `bash tools/pmu/check.sh host`, `scml`, and `lint` for the host checks.
Run `bash tools/pmu/check.sh guest` on an x86-64 KVM host
with a working virtual PMU and `/dev/kvm` mapped into the container.
Guest acceptance returns `77` when a required check cannot run.

For RISC-V, build a static `perf` on a riscv64 Linux machine or chroot
with a C toolchain, static libc, `readelf`, and a separate Linux source tree:

```sh
KERNEL_SRC=/path/to/linux bash tools/pmu/build_riscv_perf.sh
make run_kernel TARGET_ARCH=riscv64 FEATURES=pmu SMP=1 ENABLE_KVM=0 \
    ENABLE_REGRESSION_TEST=true INCLUDE_PACKAGED_PERF=false PMU_RISCV_ICOUNT=1 \
    PMU_QEMU_CPU=rv64,svpbmt=true,zkr=true,sscofpmf=true,smcntrpmf=true
```

The helper validates and installs `test/initramfs/nix/perf-riscv64/perf`.
When present, the initramfs includes it as `/bin/perf`.
The launch command enables QEMU instruction counting with `-icount shift=0`
and passes `ostd.riscv_pmu=icount` to the guest.
On a manual launch, set the boot argument only when `-icount` is enabled.
Run `sh /test/io/pmu/run_pmu_tests.sh` in the guest.
The supported single-vCPU profile skips migration and reports `77`.
Check `status.txt` for `failed=0`, `skipped=1`, and `rounds=20`
after all remaining checks, including standard `perf`, complete successfully.
A missing `/bin/perf` also produces an incomplete result.

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
