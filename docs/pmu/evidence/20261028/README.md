# 2026-10-28 verification evidence

The guest log contains the new permission regression, the existing suite and
all ten exported perf CSV files. `source-sha256.txt` matches the compiled tree.
`build.sh`, `run-kvm.sh` and `guest-verify.sh` record this local validation harness;
their `/delivery` paths refer to the original workstation's delivery directory.
For a new environment, use the portable instructions in ../../USAGE.md.
No kernel image is included here. Hardware fault injection ran in host unit tests,
not by forcing a real guest MSR or SBI failure. See ../../VALIDATION_20261028.md.
