# riscv64 `perf` for PMU regression tests

`nixpkgs` cross-builds the RISC-V initramfs and does not cross-build
`linuxPackages.perf`, so this directory is a manual injection point.
`initramfs.nix` copies `./perf` to `/bin/perf` when it exists.

From the Asterinas repository root on a riscv64 Linux machine or chroot,
point the helper at a separate Linux source tree containing `tools/perf`:

```sh
KERNEL_SRC=/path/to/linux bash tools/pmu/build_riscv_perf.sh
```

Install a C toolchain with static libc and `readelf` on the build host.
The helper installs `perf` only after verifying that it is a riscv64 ELF
with no interpreter or shared library dependencies.

Without this file the RISC-V initramfs lacks `/bin/perf`.
After a successful PMU preflight, the runner reports `INCOMPLETE` when `perf` is missing.
It exits with status `77` if the preceding checks passed or were skipped, or status `1` if a check failed.

Boot the regression image with a PMU-capable QEMU/OpenSBI configuration:

```sh
make run_kernel TARGET_ARCH=riscv64 FEATURES=pmu SMP=1 ENABLE_KVM=0 \
    ENABLE_REGRESSION_TEST=true INCLUDE_PACKAGED_PERF=false PMU_RISCV_ICOUNT=1 \
    PMU_QEMU_CPU=rv64,svpbmt=true,zkr=true,sscofpmf=true,smcntrpmf=true
```

In the guest, run `sh /test/io/pmu/run_pmu_tests.sh`.
Exit status `77` indicates incomplete coverage, such as missing hardware or a skipped task migration.
The supported RISC-V profile is QEMU `virt` with one vCPU, `Smcntrpmf`,
instruction counting enabled, and generic OpenSBI 1.4 or later.
QEMU uses a shared instruction clock across vCPUs.
Use the x86-64 PMU guest to exercise cases requiring multiple CPUs.
The example passes `-icount shift=0` to QEMU and
`ostd.riscv_pmu=icount` to the guest.
The boot argument is the trusted launcher's assertion that QEMU counts guest
instructions.
Set it on a manual launch only when QEMU is running with `-icount`.
Without this profile, the RISC-V PMU is unavailable.
The firmware prefers a programmable counter for cycles.
On this platform, both the fixed cycle counter and its filter are private to each virtual hart.
