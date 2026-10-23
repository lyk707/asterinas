# SPDX-License-Identifier: MPL-2.0
"""Test the real guest runner with fixtures; no hardware results are simulated as evidence."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "test/initramfs/src/regression/io/pmu"
BASH = os.environ.get("PMU_TEST_BASH", "bash")
VALID = "12345;;cycles:u;100000;100.00;\n67890;;instructions:u;100000;100.00;\n"
CASES = {
    "valid": (VALID, 0, 0),
    "zero_count": (VALID.replace("12345", "0"), 0, 1),
    "zero_runtime": (VALID.replace("100000", "0"), 0, 1),
    "missing_event": (VALID.splitlines()[0] + "\n", 0, 1),
    "duplicate_event": (VALID + VALID, 0, 1),
    "not_counted": (VALID.replace("12345", "<not counted>"), 0, 1),
    "not_supported": (VALID.replace("12345", "<not supported>"), 0, 1),
    "multiplexed": (VALID.replace("100.00", "50.00"), 0, 1),
    "malformed": ("1;;cycles:u\n2;;instructions:u\n", 0, 1),
    "failed_process": (VALID, 1, 1),
    "error_diagnostic": (VALID + "Error: incomplete measurement\n", 0, 1),
    "failed_diagnostic": (VALID + "FAILED: incomplete measurement\n", 0, 1),
    "missing_perf": (None, 0, 77),
    "skipped_migration": (VALID, 0, 77),
}


# Read bytes so universal-newline conversion cannot hide a broken shell argument.
for shell in SOURCE.glob("*.sh"):
    assert b"\r" not in shell.read_bytes(), f"CR byte in shell script: {shell}"


def script(path, content):
    path.write_text("#!/bin/sh\n" + content, encoding="utf-8", newline="\n")
    path.chmod(0o755)


for name, (output, status, expected) in CASES.items():
    with tempfile.TemporaryDirectory(prefix="pmu-runner-") as directory:
        fixture = Path(directory)
        for filename in ("run_pmu_tests.sh", "validate_perf_stat.sh"):
            shutil.copyfile(SOURCE / filename, fixture / filename)
        for filename in ("pmu_probe", "pmu_regression", "pmu_stress", "pmu_workload"):
            content = "exit 0\n"
            if name == "skipped_migration" and filename == "pmu_regression":
                content = '[ "${1:-}" != --migration ] || exit 77\nexit 0\n'
            script(fixture / filename, content)
        (fixture / "bin").mkdir()
        # A fallback client must never make the missing-perf case pass.
        script(fixture / "pmu_stat", "exit 0\n")
        if output is not None:
            (fixture / "fixture.csv").write_text(output, newline="\n")
            script(fixture / "bin/perf", '''[ "${1:-}" != --version ] || { echo 'TEST FIXTURE'; exit 0; }
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then cp fixture.csv "$2"; break; fi
    shift
done
exit ''' + str(status) + "\n")
        else:
            # Force command lookup failure even on Linux hosts with perf installed.
            script(fixture / "missing.sh", '''command() {
    if [ "${1:-}" = -v ] && [ "${2:-}" = perf ]; then return 1; fi
    builtin command "$@"
}
export -f command
bash ./run_pmu_tests.sh
''')
        entry = "./missing.sh" if output is None else "./run_pmu_tests.sh"
        command = 'export PATH="$PWD/bin:/usr/bin:/bin" PMU_RESULT_DIR=results; bash ' + entry
        run = subprocess.run([BASH, "-c", command], cwd=fixture, capture_output=True, text=True)
        assert run.returncode == expected, (name, run.returncode, run.stdout, run.stderr)
        if expected == 0:
            assert len(list((fixture / "results").glob("perf-*.csv"))) == 10
        print(f"PASS {name}: exit {expected}")
print(f"{len(CASES)} runner fixture tests passed; these are not PMU hardware measurements.")
