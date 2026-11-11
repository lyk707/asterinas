#!/bin/bash
# SPDX-License-Identifier: MPL-2.0

# Run from any directory. No privileged actions and no automatic downloads.
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
    lint)
        # PMU code sits behind a non-default feature, so `make check` and the
        # stock workspace Clippy never compile it. Check both architectures
        # with the feature on, then confirm the feature-off build still works.
        RUSTFLAGS="-Dwarnings" cargo osdk clippy --features="pmu" -- --no-deps
        RUSTFLAGS="-Dwarnings" OSDK_TARGET_ARCH=riscv64 \
            cargo osdk clippy --features="pmu" -- --no-deps
        make kernel TARGET_ARCH=x86_64 FEATURES= ENABLE_REGRESSION_TEST=true
        ;;
    scml)
        bash tools/sctrace.sh \
            book/src/kernel/linux-compatibility/syscall-flag-coverage/system-information-and-misc/perf_event_open.scml \
            -- echo 'PMU SCML validation'
        ;;
    format)
        # Format only the Rust files touched by this PMU implementation.
        rustfmt --edition 2024 --config skip_children=true \
            ostd/src/pmu/*.rs \
            kernel/core/src/events/perf/*.rs \
            kernel/core/src/syscall/perf_event_open.rs \
            kernel/core/src/events/mod.rs kernel/core/src/thread/mod.rs \
            kernel/core/src/init.rs kernel/core/src/process/execve.rs \
            kernel/core/src/syscall/mod.rs kernel/core/src/syscall/arch/generic.rs \
            ostd/src/lib.rs ostd/src/arch/x86/trap/mod.rs \
            ostd/src/arch/riscv/trap/mod.rs ostd/src/arch/riscv/cpu/extension.rs \
            tools/pmu/arithmetic_tests.rs tools/pmu/backend_tests.rs
        ;;
    *) echo 'usage: tools/pmu/check.sh {host|x86|riscv|default|lint|scml|format}' >&2; exit 2;;
esac
