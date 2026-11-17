#!/bin/sh
# SPDX-License-Identifier: MPL-2.0

# QEMU's init script powers off without propagating the command's exit status.
trap 'status=$?; printf "PMU acceptance exit: %s\n" "$status"' EXIT

# Check the physical parent and each ancestor. An untrusted user must not
# be able to replace them after the check. A trusted sticky directory (such
# as /tmp) also prevents users from renaming entries they do not own.
trusted_parent() (
    CDPATH= cd -P -- "$1" || exit 1
    parent=$(pwd -P) || exit 1
    uid=$(id -u) || exit 1
    path=$parent
    while :; do
        [ ! -L "$path" ] || exit 1
        metadata=$(stat -c '%u %a' -- "$path") || exit 1
        owner=${metadata% *}
        mode=${metadata#* }
        [ "$owner" = 0 ] || [ "$owner" = "$uid" ] || exit 1
        [ "$((0$mode & 0022))" -eq 0 ] ||
            [ "$((0$mode & 01000))" -ne 0 ] || exit 1
        [ "$path" != / ] || break
        path=${path%/*}
        path=${path:-/}
    done
    printf '%s\n' "$parent"
)

create_results() (
    umask 077
    if [ -n "${PMU_RESULT_DIR:-}" ]; then
        name=$(basename -- "$PMU_RESULT_DIR") || exit 1
        case "$name" in ''|.|..|/) exit 1;; esac
        parent=$(dirname -- "$PMU_RESULT_DIR") || exit 1
        parent=$(trusted_parent "$parent") || exit 1
        results="${parent%/}/$name"
        mkdir -m 0700 -- "$results" || exit 1
        printf '%s\n' "$results"
    else
        parent=$(trusted_parent "${TMPDIR:-/tmp}") || exit 1
        mktemp -d "${parent%/}/pmu-results.XXXXXX"
    fi
)

# Guest acceptance: regression, concurrency, migration, and both perf scopes x10.
# 77 means incomplete platform prerequisites; it must never be counted as PASS.
set -u
[ "$#" -eq 0 ] || exit 2
cd "$(dirname "$0")" || exit 1
./pmu_probe
case $? in
    0) ;;
    77) echo 'INCOMPLETE: PMU prerequisite unavailable.'; exit 77;;
    *) echo 'FAIL: PMU preflight failed.'; exit 1;;
esac
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
run ./pmu_regression
run ./pmu_stress
run ./pmu_regression --migration

if ! command -v perf >/dev/null 2>&1; then
    echo 'INCOMPLETE: standard Linux perf is required; pmu_stat is a separate smoke client.'
    [ "$failed" -eq 0 ] || exit 1
    exit 77
fi
umask 077
if ! results=$(create_results); then
    echo 'FAIL: cannot create a new private PMU results directory under a trusted parent.' >&2
    exit 1
fi
{
    uname -a
    perf --version
} > "$results/environment.txt" 2>&1
rounds_per_scope=10
run_scope() {
    scope=$1
    case "$scope" in
        user) events=cycles:u,instructions:u; prefix=perf;;
        all) events=cycles,instructions; prefix=perf-all;;
    esac
    round=1
    while [ "$round" -le "$rounds_per_scope" ]; do
        echo "=== standard perf ($scope): round $round / $rounds_per_scope ==="
        log="$results/$prefix-$round.csv"
        set -- perf stat --no-inherit --no-big-num -x ';' -e "$events" \
            -o "$log" -- ./pmu_workload 2000000
        {
            printf 'Arguments:'
            printf ' [%s]' "$@"
            printf '\n'
        } >> "$results/environment.txt"
        if LC_ALL=C "$@" > "$results/workload-$scope-$round.txt" 2>&1; then
            if [ ! -s "$log" ] || ! sh ./validate_perf_stat.sh "$log" "$scope"; then
                failed=$((failed + 1))
            fi
        else
            failed=$((failed + 1))
            echo "FAIL: perf round $round; inspect $results"
        fi
        round=$((round + 1))
    done
}

set -- user all
for scope in "$@"; do
    run_scope "$scope"
done
printf 'failed=%s\nskipped=%s\nrounds=%s\nrounds_per_scope=%s\n' \
    "$failed" "$skipped" "$(( $# * rounds_per_scope ))" "$rounds_per_scope" > "$results/status.txt"
echo "PMU run: failed=$failed skipped=$skipped logs=$results"
[ "$failed" -eq 0 ] || exit 1
[ "$skipped" -eq 0 ] || exit 77
