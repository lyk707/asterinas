#!/bin/bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail
export PATH=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin:/root/.cargo/bin:$PATH
export RUSTC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustc
export RUSTDOC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustdoc
export CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=8
out=/delivery/publish/review-20261111
work=/root/pmu-review-20261111-riscv
cached=/root/pmu-riscv-validation-20261030
mkdir -p "$work/tests"
cp -a "$cached/rootfs" "$work/"
cd /root/asterinas
make -C test/initramfs/src/regression/io/pmu CC="$cached/bin/riscv64-cc" \
    OUTPUT_DIR="$work/tests" BUILD_DIR="$work" TEST_PLATFORM=asterinas \
    C_FLAGS='-std=gnu11 -O2 -Wall -Wextra -Werror -D__asterinas__' > "$out/riscv-clients.log" 2>&1
cp -a "$work/tests/io/pmu/." "$work/rootfs/test/io/pmu/"
cp "$out/guest-verify.sh" "$work/rootfs/test/pmu_verify.sh"
chmod +x "$work/rootfs/test/pmu_verify.sh"
cd "$work/rootfs"
find . -print0 | cpio --null -o --format=newc --quiet | gzip -1 > "$work/initramfs.cpio.gz"
cd /root/asterinas/kernel
cargo osdk build --target-arch=riscv64 --scheme=riscv --features=pmu \
    --initramfs="$work/initramfs.cpio.gz" --kcmd-args=loglevel=error \
    --kcmd-args=console=ttyS0 --init-args=/test/pmu_verify.sh > "$out/riscv-kernel.log" 2>&1
{
    uname -a
    qemu-system-riscv64 --version | head -1
    sha256sum "$work/rootfs/bin/perf" /root/asterinas/target/osdk/asterinas-osdk-bin.elf "$work/initramfs.cpio.gz"
} > "$out/riscv-environment.txt"
timeout --signal=TERM --kill-after=5s 600s qemu-system-riscv64 \
    -machine virt -accel tcg -cpu rv64,svpbmt=true,zkr=true,sscofpmf=true,smcntrpmf=true \
    -m 2G -smp 2 -display none -serial stdio -monitor none -nic none -no-reboot \
    -kernel /root/asterinas/target/osdk/asterinas-osdk-bin.elf -initrd "$work/initramfs.cpio.gz" \
    -append 'SHELL=/bin/sh LOGNAME=root HOME=/ USER=root PATH=/bin:/test/io/pmu loglevel=error console=ttyS0 -- /test/pmu_verify.sh' \
    > "$out/riscv-guest.log" 2>&1
tail -12 "$out/riscv-guest.log"
