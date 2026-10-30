#!/bin/bash
set -euo pipefail
export PATH=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin:$PATH
export RUSTC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustc
export RUSTDOC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustdoc
export CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=8
cd /root/asterinas/kernel
cargo osdk build --target-arch=riscv64 --scheme=riscv --features=pmu \
    --initramfs=/root/pmu-riscv-validation-20261030/initramfs.cpio.gz \
    --kcmd-args=loglevel=error --kcmd-args=console=ttyS0 --init-args=/test/pmu_verify.sh \
    > /delivery/publish/riscv-validation-20261030/kernel-build.log 2>&1
echo 'PASS: complete RISC-V PMU kernel build'
