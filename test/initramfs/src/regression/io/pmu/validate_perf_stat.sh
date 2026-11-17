#!/bin/sh
# SPDX-License-Identifier: MPL-2.0

# Validate one LC_ALL=C, semicolon-separated standard perf stat result.
# A successful process exit alone is not counting evidence.
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || exit 2
case "${2:-user}" in
    user) suffix=:u;;
    all) suffix=;;
    *) exit 2;;
esac
awk -F ';' -v suffix="$suffix" '
BEGIN { cycles = "cycles" suffix; instructions = "instructions" suffix }
function trim(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
function positive(s) { return s ~ /^[0-9]+([.][0-9]+)?$/ && s + 0 > 0 }
tolower($0) ~ /<not counted>|<not supported>|error|failed/ { bad = 1 }
{
    event = trim($3)
    if (event != cycles && event != instructions) next
    seen[event]++
    value = trim($1); runtime = trim($4); percent = trim($5)
    if (NF < 5 || !positive(value) || !positive(runtime) || !positive(percent)) bad = 1
    if (percent + 0 < 99.99 || percent + 0 > 100.01) bad = 1
}
END {
    if (seen[cycles] != 1 || seen[instructions] != 1 || bad) {
        print "FAIL: perf output must contain two positive, fully scheduled hardware events" > "/dev/stderr"
        exit 1
    }
}' "$1"
