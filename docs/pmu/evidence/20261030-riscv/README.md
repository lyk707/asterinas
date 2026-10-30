# RISC-V verification evidence, 2026-10-30

`guest.log` is the successful QEMU 10.2.1 / OpenSBI 1.7 / perf 6.18.7 run.
The perf CSV files were extracted from the guest serial log and checked without
loosening the guest validator. Earlier failed runs are retained separately.
The shell scripts record workstation-specific Nix and /delivery paths; they
require the corresponding cached dependencies, not merely an arbitrary checkout.
No kernel image, compiler cache or Linux source archive is included here.
For results and limitations, see ../../RISCV_VALIDATION_20261030.md.
