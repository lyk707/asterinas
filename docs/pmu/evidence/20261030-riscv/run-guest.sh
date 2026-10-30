#!/bin/bash
set -uo pipefail
out=/delivery/publish/riscv-validation-20261030
work=/root/pmu-riscv-validation-20261030
tag=${PMU_RUN_TAG:-initial}
timeout --signal=TERM --kill-after=5s 600 qemu-system-riscv64 \
    -machine virt -accel tcg \
    -cpu rv64,svpbmt=true,zkr=true,sscofpmf=true,smcntrpmf=true \
    -m 2G -smp 2 -display none -serial stdio -monitor none -nic none -no-reboot \
    -kernel /root/asterinas/target/osdk/asterinas-osdk-bin.elf \
    -initrd "$work/initramfs.cpio.gz" \
    -append 'SHELL=/bin/sh LOGNAME=root HOME=/ USER=root PATH=/bin:/test/io/pmu loglevel=error console=ttyS0 -- /test/pmu_verify.sh' \
    > "$out/guest-$tag.log" 2>&1
result=$?
echo "QEMU process exit=$result"
tail -n 40 "$out/guest-$tag.log"
exit "$result"
