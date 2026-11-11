# Review follow-up evidence

These files record the 2026-11-11 validation of the changes above `41a868e`.
See [the review report](../../REVIEW_20261111.md) for conclusions and limits.

- `ktest-riscv.log`: the new PMU context handoff test running on two QEMU harts.
- `riscv-guest.log`: the full guest regression and all twenty original perf CSV outputs.
- `perf-results.csv`: those outputs combined by scope, round and event.
- `host.log`: 40 Rust unit tests, 16 runner fixtures, C ABI and assembly checks.
- `clippy-*-ktest.log`: checks for both architectures, including the new kernel test.
- `ktest-x86-build.log`: successful x86 test-image compilation only. The build harness
  deliberately used `/bin/false` as its QEMU executable because it could not run KVM
  in that container; this is not a successful x86 test execution.
- `amd-kvm-environment.txt`: a separate WSL KVM probe failed, and no CPU exposed SVM.
- `source-sha256.txt`: 1,754 source and build inputs compared with the compiled tree;
  Windows text line endings are normalized to LF.
- `summary.json`: machine-readable results, explicitly marking incomplete acceptance.

The shell scripts record the local build/validation harness. `/delivery` is the
original workstation's mounted workspace. The RISC-V run reuses the cached runtime
and perf 6.18.7 from the [previous validation](../../RISCV_VALIDATION_20261030.md),
then rebuilds the C clients and kernel from the current sources.
For portable setup and commands, use [USAGE.md](../../USAGE.md).

The RISC-V handoff test drives PMU hooks across CPUs; actual scheduler migration
still returns SKIP in the guest regression. RISC-V counts are simulated by QEMU.
No system images, compiler caches or dependency binaries are included here.
