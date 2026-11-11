#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
set -u
echo PMU_REVIEW_BOOT_BEGIN
uname -a
perf --version
cd /test/io/pmu || exit 1
export PMU_RESULT_DIR=/tmp/pmu-review-results
sh ./run_pmu_tests.sh
suite_rc=$?
echo "PMU_REVIEW_SUITE_EXIT=$suite_rc"
for result in "$PMU_RESULT_DIR"/*.csv "$PMU_RESULT_DIR"/status.txt; do
    [ -f "$result" ] || continue
    echo "PMU_REVIEW_FILE=$result"
    cat "$result"
done
echo PMU_REVIEW_BOOT_END
exit "$suite_rc"
