#!/usr/bin/env bash
set -euo pipefail
export PATH=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin:$PATH
export RUSTC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustc
export RUSTDOC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustdoc
export CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=8
export HTTPS_PROXY=http://host.docker.internal:12000
export HTTP_PROXY="$HTTPS_PROXY" https_proxy="$HTTPS_PROXY" http_proxy="$HTTPS_PROXY"
export NIX_CONFIG='substitute = false'
out=/delivery/publish/review-20261028
mkdir -p "$out"
cd /root/asterinas
if [[ ${PMU_SKIP_STATIC:-0} != 1 ]]; then
bash tools/pmu/check.sh host > "$out/host.log" 2>&1
echo 'PASS: host PMU checks'
rustfmt --edition 2024 --check --config skip_children=true \
    ostd/src/pmu/*.rs kernel/core/src/events/perf/*.rs \
    kernel/core/src/syscall/perf_event_open.rs \
    kernel/core/src/process/posix_thread/alien_access.rs tools/pmu/backend_tests.rs > "$out/rustfmt.log" 2>&1
echo 'PASS: PMU Rust formatting'
clang-format --dry-run --Werror test/initramfs/src/regression/io/pmu/pmu_regression.c \
    > "$out/c-format.log" 2>&1
echo 'PASS: updated C regression formatting'
fi
make kernel TARGET_ARCH=x86_64 FEATURES=pmu ENABLE_REGRESSION_TEST=true ENABLE_PMU_TESTS=true \
    > "$out/kernel.log" 2>&1
echo 'PASS: integrated x86_64 PMU kernel build'
work=/root/pmu-review-20261028-initramfs
mkdir -p "$work"
cd "$work"
gzip -dc /root/asterinas/test/initramfs/build/initramfs.cpio.gz | cpio -idu --quiet
cp /delivery/evidence/linux-build-20261027/guest-init.sh init
cp /delivery/publish/review-20261028/guest-verify.sh test/pmu_verify.sh
find . -print0 | cpio --null -o --format=newc --quiet > /root/pmu-review-20261028.cpio
cd /root/asterinas/kernel
export PMU_QEMU_CPU=host,pmu=on SMP=2 MEM=4G NETDEV=none QEMU_DISPLAY=none CONSOLE=ttyS0
export INITRAMFS=on OVMF=on
cargo osdk build --features=pmu --initramfs=/root/pmu-review-20261028.cpio \
    --kcmd-args=loglevel=error --kcmd-args=earlycon --kcmd-args=console=ttyS0 \
    --init-args=/test/pmu_verify.sh > "$out/verification-image.log" 2>&1
cp /root/asterinas/target/osdk/asterinas-osdk-bin.iso "$out/integrated-verification.iso"
echo 'PASS: integrated verification image prepared'
