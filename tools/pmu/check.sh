#!/bin/bash
# SPDX-License-Identifier: MPL-2.0

# Run from any directory.
set -euo pipefail
pmu_root=$(cd "$(dirname "$0")/../.." && pwd)
pmu_output=${PMU_CHECK_OUTPUT:-"$pmu_root/target/pmu-check"}
mkdir -p "$pmu_output"
cd "$pmu_root"

case "${1:-host}" in
    host)
        make -C test/initramfs/src/regression/io/pmu \
            OUTPUT_DIR="$pmu_output/test" BUILD_DIR="$pmu_output" \
            TEST_PLATFORM=linux C_FLAGS='-std=gnu11 -O2 -Wall -Wextra -Werror'
        "$pmu_output/test/io/pmu/pmu_regression" --abi-only
        "$pmu_output/test/io/pmu/pmu_stat" --help
        "$pmu_output/test/io/pmu/pmu_workload" 1000
        rustc --edition=2024 --test tools/pmu/arithmetic_tests.rs -o "$pmu_output/arithmetic_tests"
        "$pmu_output/arithmetic_tests"
        rustc --edition=2024 --test tools/pmu/backend_tests.rs -o "$pmu_output/backend_tests"
        "$pmu_output/backend_tests"
        python3 tools/pmu/runner_tests.py
        python3 tools/pmu/assembly_tests.py
        echo 'Host tests cover production logic with injected hardware access, not guest hardware execution.'
        ;;
    x86)
        make kernel TARGET_ARCH=x86_64 FEATURES=pmu ENABLE_REGRESSION_TEST=true
        ;;
    riscv)
        make kernel TARGET_ARCH=riscv64 FEATURES=pmu ENABLE_REGRESSION_TEST=true
        ;;
    default)
        make kernel TARGET_ARCH=x86_64 FEATURES= ENABLE_REGRESSION_TEST=true
        ;;
    guest)
        if [[ ! -c /dev/kvm || ! -r /dev/kvm || ! -w /dev/kvm ]]; then
            echo 'INCOMPLETE: x86 PMU guest acceptance requires access to /dev/kvm.' >&2
            exit 77
        fi
        : > qemu.log
        make run_kernel AUTO_TEST=pmu TARGET_ARCH=x86_64 FEATURES=pmu \
            ENABLE_KVM=1 PMU_QEMU_CPU=host,pmu=on SMP=2 RELEASE=1 \
            INCLUDE_PACKAGED_PERF=true
        status=$(sed -n -e 's/\r$//' -e 's/^PMU acceptance exit: \([0-9][0-9]*\)$/\1/p' qemu.log)
        case "$status" in
            0) echo 'PASS: PMU guest acceptance completed.';;
            77) echo 'INCOMPLETE: PMU guest acceptance skipped one or more checks.' >&2; exit 77;;
            *) echo 'FAIL: PMU guest acceptance did not complete successfully; see qemu.log.' >&2; exit 1;;
        esac
        ;;
    lint)
        # PMU code sits behind a non-default feature, so `make check` and the
        # stock workspace Clippy never compile it. Check both architectures
        # with PMU and its in-kernel tests enabled.
        RUSTFLAGS="-Dwarnings" cargo osdk clippy --ktests --features=pmu -- --no-deps
        RUSTFLAGS="-Dwarnings" OSDK_TARGET_ARCH=riscv64 \
            cargo osdk clippy --ktests --features=pmu -- --no-deps
        ;;
    scml)
        local_scml="$pmu_root/book/src/kernel/linux-compatibility/syscall-flag-coverage"
        PMU_BOOK_SCML="$local_scml/system-information-and-misc/perf_event_open.scml" \
            PMU_IOCTL_SCML="$local_scml/file-descriptor-and-io-control/ioctl.scml" \
            cargo test --manifest-path tools/sctrace/Cargo.toml
        ;;
    format)
        bash tools/format_all.sh
        # The standalone test harnesses are outside the Cargo workspace.
        rustfmt --edition 2024 --config skip_children=true \
            tools/pmu/arithmetic_tests.rs tools/pmu/backend_tests.rs tools/pmu/backend_tests/*.rs
        ;;
    *) echo 'usage: tools/pmu/check.sh {host|x86|riscv|default|guest|lint|scml|format}' >&2; exit 2;;
esac
