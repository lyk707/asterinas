#!/bin/sh
# SPDX-License-Identifier: MPL-2.0

# Guest acceptance: regression, concurrency, migration, and standard perf x10.
# 77 means incomplete platform prerequisites; it must never be counted as PASS.
set -u
[ "$#" -eq 0 ] || { [ "$#" -eq 1 ] && [ "$1" = --migration ]; } || exit 2
cd "$(dirname "$0")" || exit 1
failed=0
skipped=0
run() {
    "$@"
    code=$?
    case "$code" in
        0) ;;
        77) skipped=$((skipped + 1));;
        *) failed=$((failed + 1)); echo "FAIL: $* (exit $code)";;
    esac
}
run ./pmu_probe
run ./pmu_regression
run ./pmu_stress
run ./pmu_regression --migration

if ! command -v perf >/dev/null 2>&1; then
    echo 'INCOMPLETE: standard Linux perf is required; pmu_stat is a separate smoke client.'
    [ "$failed" -eq 0 ] || exit 1
    exit 77
fi
results=${PMU_RESULT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/pmu-results.XXXXXX")} || exit 1
mkdir -p "$results" || exit 1
{
    uname -a
    perf --version
    echo 'Command: perf stat --no-inherit --no-big-num -x ; -e cycles:u,instructions:u -- ./pmu_workload 2000000'
} > "$results/environment.txt" 2>&1
round=1
while [ "$round" -le 10 ]; do
    echo "=== standard perf: round $round / 10 ==="
    log="$results/perf-$round.csv"
    if LC_ALL=C perf stat --no-inherit --no-big-num -x ';' -e cycles:u,instructions:u \
        -o "$log" -- ./pmu_workload 2000000 > "$results/workload-$round.txt" 2>&1; then
        if [ ! -s "$log" ] || ! sh ./validate_perf_stat.sh "$log"; then
            failed=$((failed + 1))
        fi
    else
        failed=$((failed + 1))
        echo "FAIL: perf round $round; inspect $results"
    fi
    round=$((round + 1))
done
printf 'failed=%s\nskipped=%s\nrounds=10\n' "$failed" "$skipped" > "$results/status.txt"
echo "PMU run: failed=$failed skipped=$skipped logs=$results"
[ "$failed" -eq 0 ] || exit 1
[ "$skipped" -eq 0 ] || exit 77
