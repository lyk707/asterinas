#!/bin/sh
set -u
echo PMU_VERIFY_BOOT_BEGIN
uname -a
cd /test/io/pmu || exit 1
./pmu_regression --abi-only
abi_rc=$?
./pmu_workload 1000000
workload_rc=$?
./pmu_probe
probe_rc=$?
perf --version
perf_version_rc=$?
timeout 30 perf stat --no-inherit --no-big-num -x ';' -e cycles:u,instructions:u -- ./pmu_workload 2000000
perf_stat_rc=$?
suite_rc=77
if [ "$probe_rc" -eq 0 ]; then
    export PMU_RESULT_DIR=/tmp/pmu-review-results
    sh ./run_pmu_tests.sh
    suite_rc=$?
fi
echo "PMU_VERIFY_RESULT abi=$abi_rc workload=$workload_rc probe=$probe_rc perf_version=$perf_version_rc perf_stat=$perf_stat_rc suite=$suite_rc"
echo PMU_REVIEW_PERF_LOGS_BEGIN
for result in /tmp/pmu-review-results/perf-*.csv; do
    [ -f "$result" ] || continue
    echo "PMU_REVIEW_PERF_FILE=$result"
    cat "$result"
done
echo PMU_REVIEW_PERF_LOGS_END
echo PMU_VERIFY_BOOT_END
[ "$abi_rc" -eq 0 ] && [ "$workload_rc" -eq 0 ] && [ "$perf_version_rc" -eq 0 ] || exit 1
[ "$probe_rc" -eq 0 ] || exit 77
exit "$suite_rc"
