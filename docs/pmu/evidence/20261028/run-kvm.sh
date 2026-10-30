#!/usr/bin/env bash
set -euo pipefail
base=/delivery/publish/review-20261028
vcpus=${PMU_VCPUS:-2}
out="$base/kvm-${vcpus}cpu"
mkdir -p "$out"
export DELIVERY_ROOT=/delivery PMU_HOST_RESULT_DIR="$out"
{
    date -Is
    uname -a
    qemu-system-x86_64 --version | head -n 1
    ls -l /dev/kvm
    grep -m 1 '^model name' /proc/cpuinfo
    cat /sys/module/kvm/parameters/enable_pmu
    echo "QEMU: -machine q35 -accel kvm -cpu host,pmu=on -smp $vcpus -m 4G"
} > "$out/host.txt"
python3 /delivery/evidence/linux-build-20261027/probe-kvm-cpu.py > "$out/cpu-probe.log"
set +e
timeout --signal=TERM --kill-after=10s 600s qemu-system-x86_64 \
    -machine q35 -accel kvm -cpu host,pmu=on -smp "$vcpus" -m 4G \
    -cdrom "$base/integrated-verification.iso" \
    -bios /root/ovmf/release/OVMF.fd \
    -display none -serial stdio -monitor none -nic none -no-reboot \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 > "$out/guest.log" 2>&1
qemu_rc=$?
set -e
tr -d '\r' < "$out/guest.log" > "$out/guest-clean.log"
guest="$out/guest-clean.log"
grep -E 'PASS:|SKIP|FAIL|PMU_VERIFY_|PMU run:|cycles:u=|;;cycles:u;|;;instructions:u;' "$guest" | tail -n 50
python3 - "$guest" "$out/status.json" "$qemu_rc" "$vcpus" <<'PY'
import json, re, sys
from pathlib import Path
text = Path(sys.argv[1]).read_text()
expected = 'PMU_VERIFY_RESULT abi=0 workload=0 probe=0 perf_version=0 perf_stat=0 suite=77'
checks = {
    'completed': expected in text,
    'no_failures': 'PMU run: failed=0 skipped=1' in text,
    'permission_boundary': 'PASS: CAP_PERFMON self/same-UID access, cross-UID denial, SYS_PTRACE authorization' in text,
    'per_target_isolation': 'PASS: concurrent dup/ioctl/read, enabled-event close/reopen and per-target isolation' in text,
    'ten_perf_rounds': len(re.findall(r'=== standard perf: round \d+ / 10 ===', text)) == 10,
    'persisted_perf_counts': len(re.findall(r'^\d+;;cycles:u;', text, re.M)) == 11 and len(re.findall(r'^\d+;;instructions:u;', text, re.M)) == 11,
    'no_panic': 'panicked at' not in text,
}
result = {'vcpus': int(sys.argv[4]), 'qemu_exit': int(sys.argv[3]), 'checks': checks,
          'passed': all(checks.values()), 'failed': 0 if checks['no_failures'] else None,
          'skipped': 1, 'migration': 'not verified; guest scheduler did not migrate on affinity change'}
Path(sys.argv[2]).write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
if not result['passed']:
    sys.exit(1)
PY
