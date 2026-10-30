# riscv64 `perf` for PMU guest acceptance

The 2026-10-30 QEMU/OpenSBI validation cross-built standard perf 6.18.7 and
ran all ten rounds successfully in a minimal guest rootfs. Perf 6.5.6 fell
back to default events in this guest and did not pass the explicit-event
acceptance. See [the validation report](../../../../docs/pmu/RISCV_VALIDATION_20261030.md)
for the working compiler/runtime environment and its limits. The full Nix
cross-initramfs path below remains separate from that tested minimal rootfs.

`nixpkgs` cross-builds the RISC-V initramfs and does not cross-build
`linuxPackages.perf`, so this directory is a manual injection point.
`initramfs.nix` copies `./perf` to `/bin/perf` when it exists.

Build the binary on a riscv64 Linux machine (or with a riscv64 QEMU user-mode
chroot) from the kernel tree matching the target, then place it here:

```sh
make -C tools/perf ARCH=riscv NO_LIBTRACEEVENT=1 prefix=/usr install
cp tools/perf/perf test/initramfs/nix/perf-riscv64/perf
```

`tools/pmu/build_riscv_perf.sh` automates the same step.
The resulting binary must run on the target rootfs, so prefer a static build
(`LDFLAGS=-static`) when the guest lacks a dynamic loader for perf's libraries.

Without this file the RISC-V initramfs simply lacks `/bin/perf`; the regression
scripts then report the standard-perf acceptance as `SKIP`, never as `PASS`.

The PMU acceptance build can also select target-architecture perf with
`ENABLE_PMU_TESTS=true`. That cross build has not been validated on RISC-V.
For this manual injection route, leave `ENABLE_PMU_TESTS=false` (the default)
to avoid requesting the unverified cross build. The x86-64 acceptance uses
`ENABLE_PMU_TESTS=true` and preserves the perf wrapper's own Nix store path.
