# riscv64 `perf` for PMU regression tests

`nixpkgs` cross-builds the RISC-V initramfs and does not cross-build
`linuxPackages.perf`, so this directory is a manual injection point.
`initramfs.nix` copies `./perf` to `/bin/perf` when it exists.

From the Asterinas repository root on a riscv64 Linux machine or chroot,
point the helper at a separate Linux source tree containing `tools/perf`:

```sh
KERNEL_SRC=/path/to/linux bash tools/pmu/build_riscv_perf.sh
```

The helper copies the built binary into this directory.
Use `PERF_STATIC=1` if the guest cannot supply the required dynamic libraries.

Without this file the RISC-V initramfs simply lacks `/bin/perf`; the regression
scripts then report the standard-perf acceptance as `SKIP`, never as `PASS`.

Boot the regression image with a PMU-capable QEMU/OpenSBI configuration:

```sh
make run_kernel TARGET_ARCH=riscv64 FEATURES=pmu SMP=2 \
    ENABLE_REGRESSION_TEST=true ENABLE_PMU_TESTS=false \
    PMU_QEMU_CPU=rv64,svpbmt=true,zkr=true,sscofpmf=true,smcntrpmf=true
```

In the guest, run `sh /test/io/pmu/run_pmu_tests.sh`.
Exit status `77` indicates incomplete coverage, such as missing hardware or a skipped task migration.
RISC-V per-thread cycles require a programmable counter because fixed cycle counters can be shared by harts.
