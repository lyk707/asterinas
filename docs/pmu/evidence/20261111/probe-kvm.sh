#!/bin/bash
# SPDX-License-Identifier: MPL-2.0
out=$(dirname "$0")
{
    uname -a
    printf 'CPUs exposing SVM: '
    grep -c ' svm ' /proc/cpuinfo
    cat /sys/module/kvm/parameters/enable_pmu
    ls -l /dev/kvm
    timeout 3 qemu-system-x86_64 -accel kvm -cpu host,pmu=on -m 256 \
        -display none -serial none -monitor none -nodefaults
    printf 'KVM probe exit=%s\n' "$?"
} > "$out/amd-kvm-environment.txt" 2>&1
cat "$out/amd-kvm-environment.txt"
