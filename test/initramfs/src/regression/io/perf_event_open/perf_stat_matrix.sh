#!/bin/sh

# SPDX-License-Identifier: MPL-2.0

set -eu

if [ "$#" -ne 2 ] || [ ! -x "$1" ]; then
    printf 'Usage: %s <executable workload> <new results directory>\n' "$0" >&2
    exit 2
fi

workload=$1
results_dir=$2
LC_ALL=C
export LC_ALL

if ! command -v perf >/dev/null 2>&1; then
    printf 'FAIL: perf is required for this test\n' >&2
    exit 1
fi

check_counts() {
    awk -F, -v requested="$1" -v test_case="$2" -v round="$3" '
        BEGIN {
            n = split(requested, events, ",")
            for (i = 1; i <= n; i++)
                expected[events[i]] = 1
        }
        /^[[:space:]]*($|#)/ { next }
        {
            event = $3
            if (NF < 5 || !(event in expected) || (event in counts) ||
                $1 !~ /^[0-9]+$/ || $1 <= 0 ||
                $4 !~ /^[0-9]+$/ || $4 <= 0) {
                invalid = 1
                next
            }
            counts[event] = $1
            runtime[event] = $4
            running[event] = $5
        }
        END {
            if (invalid)
                exit 1
            for (i = 1; i <= n; i++)
                if (!(events[i] in counts))
                    exit 1
            for (i = 1; i <= n; i++) {
                event = events[i]
                printf "%s,%s,%s,%s,%s,%s\n", test_case, round, event,
                    counts[event], runtime[event], running[event]
            }
        }
    ' "$4"
}

mkdir "$results_dir"
{
    uname -a
    perf version
} > "$results_dir/environment.txt"
printf 'case,instructions,cycles\n' > "$results_dir/cases.csv"
printf 'case,round,event,count,runtime_ns,running_pct\n' > "$results_dir/counts.csv"

rounds=10
test_case=0
runs=0

for instructions in '' instructions:u instructions:k instructions; do
    for cycles in '' cycles:u cycles:k cycles; do
        events="$instructions"
        if [ -n "$cycles" ]; then
            events="${events:+$events,}$cycles"
        fi
        [ -n "$events" ] || continue

        test_case=$((test_case + 1))
        printf '%d,%s,%s\n' "$test_case" "$instructions" "$cycles" >> "$results_dir/cases.csv"

        round=1
        while [ "$round" -le "$rounds" ]; do
            raw="$results_dir/case-$test_case-round-$round.csv"
            if ! perf stat --no-inherit --no-big-num -x, -e "$events" -o "$raw" -- "$workload" ||
               ! check_counts "$events" "$test_case" "$round" "$raw" >> "$results_dir/counts.csv"; then
                printf 'FAIL: case %d (%s), round %d, see %s\n' \
                    "$test_case" "$events" "$round" "$raw" >&2
                exit 1
            fi
            runs=$((runs + 1))
            round=$((round + 1))
        done
        printf 'PASS: %s (%d/%d)\n' "$events" "$rounds" "$rounds"
    done
done

if [ "$test_case" -ne 15 ] || [ "$runs" -ne 150 ]; then
    printf 'FAIL: ran %d cases and %d perf stat runs\n' "$test_case" "$runs" >&2
    exit 1
fi
printf 'PASS: %d cases, %d perf stat runs\n' "$test_case" "$runs"
