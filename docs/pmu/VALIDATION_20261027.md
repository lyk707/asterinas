# PMU build and hardware validation, 2026-10-27

The x86-64 PMU kernel was built and booted with real KVM acceleration on an
AMD Ryzen 9 8945HX, using WSL Ubuntu 22.04 and
`6.18.33.2-microsoft-standard-WSL2`.
The guest used two virtual CPUs, 4 GiB of memory and `host,pmu=on`.
QMP confirmed KVM enabled, AMD family 19h, `perfctr-core=true` and `pmu=true`.

## Measurements from the tested delivery

Two runs used the same compiled verification image:

| Check | Ubuntu QEMU 6.2 | Development-image QEMU 10.2.1 |
|---|---|---|
| Probe cycles:u | 4,008,263 | 4,006,739 |
| Standard perf cycles:u | 8,630,570 | 8,538,634 |
| Standard perf instructions:u | 20,262,549 | 20,285,139 |
| Regression and concurrency | Passed | Passed |
| Standard perf rounds | 10, no failures | 10, no failures |
| Cross-CPU migration | Skipped | Skipped |
| Suite summary | failed=0, skipped=1 | failed=0, skipped=1 |

The standard command was:

```sh
perf stat --no-inherit --no-big-num -x ';' \
    -e cycles:u,instructions:u -- ./pmu_workload 2000000
```

These observations are not a controlled performance comparison or an overhead benchmark.
The guest returned:

```text
PMU_VERIFY_RESULT abi=0 workload=0 probe=0 perf_version=0 perf_stat=0 suite=77
PMU_VERIFY_INIT_EXIT=77
```

Regression covered attribute errors, user-memory faults, control and read layouts,
IDs and descriptor lifetime, privilege scopes, sleep accounting, remote reads,
target exit, fork/CLOEXEC, capability checks and per-thread CPU filtering.
Concurrency covered dup/ioctl/read and repeated close/reopen.
These earlier measurements did not include the remote branch's additional
per-target isolation test; the integrated-version results below are separate.

The scheduler's existing TODO in `kernel/core/src/syscall/sched_affinity.rs`
states that affinity changes do not migrate an existing thread.
The migration test observed no CPU change and correctly returned 77.
This is a scheduler prerequisite limitation, not proof of a PMU migration defect
or proof that PMU migration handling is already correct.

## Integration with the existing branch

The final implementation and optional target perf packaging were synchronized
onto the existing `pmu/perf-event` branch.
The branch's per-target isolation test, lint entry, revised design documentation,
SCML examples and manual RISC-V perf injection route were preserved.

The integrated tree was rebuilt successfully in the same development image.
The host checks passed 34 Rust tests and 14 runner fixtures, together with C and
assembly checks; PMU Rust formatting and the updated C formatting passed.
An initial isolation run exposed an existing test setup error: `pmu_open`
creates disabled events, but the per-target test never enabled them.
The test now enables each event before releasing its target threads.
The test was retained and rerun, rather than removed or marked successful.

The rebuilt KVM guest then passed regression, concurrency, per-target isolation
and ten standard perf rounds, with `failed=0 skipped=1`.
The only skip remained cross-CPU migration.
The probe reported 4,009,768 cycles;
the separate standard perf command reported 8,916,812 cycles and
20,260,921 instructions.
See [the integrated evidence](evidence/20261027/integrated/), including the exact
source-file checksums, build log, QMP record and guest log.


## Evidence and limits

Original delivery evidence is under [evidence/20261027](evidence/20261027/):

- `host.log`: four arithmetic and thirty backend/attribute/timing Rust tests,
  fourteen runner fixtures, C build/ABI checks and assembly-layout checks.
- `kvm-after-reboot/`: Ubuntu QEMU 6.2 host, QMP, guest and status records.
- `kvm-docker-after-reboot/`: development-image QEMU 10.2.1 records.
- `kvm-validation-summary.json`: machine-readable results and tested ISO checksum.

The ten-round CSV files were checked in the guest but not exported before shutdown.
The saved logs contain round completion, runner totals and the separate perf count lines.
The referenced original ISO is not committed to this repository.

Intel hardware, RISC-V SBI guest acceptance, cross-CPU migration,
actual hardware stop/release failure injection and native-Linux performance overhead
remain outside these completed measurements.
Earlier design-report statements about other builds and whole-tree checks
are historical records and were not all rerun for this integration.

For building without a prebuilt system image, see [the usage guide](USAGE.md).
