#!/bin/bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail
export PATH=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin:/root/.cargo/bin:$PATH
export RUSTC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustc
export RUSTDOC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustdoc
export CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=8
export SMP=2 MEM=4G NETDEV=none QEMU_DISPLAY=none CONSOLE=ttyS0 INITRAMFS=on OVMF=on
out=/delivery/publish/review-20261111
cd /root/asterinas/kernel/core
if [ "$1" = x86 ]; then
    export PMU_QEMU_CPU=host,pmu=on
    # Docker's device policy denies KVM. Build here, then execute the test ISO
    # in Ubuntu WSL, where /dev/kvm is accessible. /bin/false runs no guest.
    set +e
    cargo osdk test context_handoff_between_cpus --features=pmu --qemu-args='-accel kvm' --qemu-exe=/bin/false > "$out/ktest-x86-build.log" 2>&1
    rc=$?
    set -e
    grep -q 'completed successfully' "$out/ktest-x86-build.log" || { tail -60 "$out/ktest-x86-build.log"; exit "$rc"; }
    cp /root/asterinas/target/osdk/aster-core-osdk-bin.iso "$out/ktest-x86.iso"
    cp /root/ovmf/release/OVMF.fd "$out/OVMF.fd"
    echo 'x86 kernel-test image built; guest execution is pending in WSL.'
else
    export PMU_QEMU_CPU=rv64,svpbmt=true,zkr=true,sscofpmf=true,smcntrpmf=true
    timeout --kill-after=5s 600s cargo osdk test context_handoff_between_cpus --target-arch=riscv64 --scheme=riscv --features=pmu > "$out/ktest-riscv.log" 2>&1
    echo 'RISC-V kernel test completed.'
fi
