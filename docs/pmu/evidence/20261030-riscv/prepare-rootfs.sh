#!/bin/bash
set -euo pipefail
out=/delivery/publish/riscv-validation-20261030
work=/root/pmu-riscv-validation-20261030
mkdir -p "$work/bin" "$work/rootfs" "$work/tests"
ln -sfn /nix/store/7770fl584xjgzyaygilpi8spskcbl58k-riscv64-unknown-linux-gnu-binutils-2.46/bin/riscv64-unknown-linux-gnu-ld "$work/bin/ld"
ln -sfn /nix/store/7770fl584xjgzyaygilpi8spskcbl58k-riscv64-unknown-linux-gnu-binutils-2.46/bin/riscv64-unknown-linux-gnu-as "$work/bin/as"
cat > "$work/bin/riscv64-cc" <<'EOF'
#!/bin/sh
exec /nix/store/f37gbyh8cb94s6i44hfvs6n49mi8as4c-riscv64-unknown-linux-gnu-gcc-15.2.0/bin/riscv64-unknown-linux-gnu-gcc \
    -B/root/pmu-riscv-validation-20261030/bin/ \
    -B/nix/store/bwya0sg1l6ad7d5m6c0362bckbvzspzl-glibc-riscv64-unknown-linux-gnu-2.42-67/lib/ \
    -isystem /nix/store/zv7j3q91jy0vdk39161068ygr1s8fcfb-glibc-riscv64-unknown-linux-gnu-2.42-67-dev/include \
    -L/nix/store/aqprh3a5qbzghmshf8mypi4an5fxxrv1-riscv64-unknown-linux-gnu-gcc-15.2.0-lib/lib \
    -Wl,-rpath,/nix/store/bwya0sg1l6ad7d5m6c0362bckbvzspzl-glibc-riscv64-unknown-linux-gnu-2.42-67/lib:/nix/store/aqprh3a5qbzghmshf8mypi4an5fxxrv1-riscv64-unknown-linux-gnu-gcc-15.2.0-lib/lib \
    -Wl,--dynamic-linker=/nix/store/bwya0sg1l6ad7d5m6c0362bckbvzspzl-glibc-riscv64-unknown-linux-gnu-2.42-67/lib/ld-linux-riscv64-lp64d.so.1 \
    "$@"
EOF
chmod +x "$work/bin/riscv64-cc"
cd /root/asterinas
make -C test/initramfs/src/regression/io/pmu CC="$work/bin/riscv64-cc" \
    OUTPUT_DIR="$work/tests" BUILD_DIR="$work" TEST_PLATFORM=asterinas \
    C_FLAGS='-std=gnu11 -O2 -Wall -Wextra -Werror -D__asterinas__' > "$out/cross-tests.log" 2>&1
rootfs="$work/rootfs"
mkdir -p "$rootfs"/{bin,sbin,dev,proc,sys,tmp,root,etc,test/io/pmu,nix/store}
busybox=/nix/store/7pqc1ni9mxqxdr8lz9v4irvbng3nkq68-busybox-riscv64-unknown-linux-gnu-1.37.0
while read -r path; do cp -a "$path" "$rootfs/nix/store/"; done < <(nix-store -qR "$busybox")
cp -a /nix/store/aqprh3a5qbzghmshf8mypi4an5fxxrv1-riscv64-unknown-linux-gnu-gcc-15.2.0-lib "$rootfs/nix/store/"
cp "$busybox/bin/busybox" "$rootfs/bin/busybox"
for app in sh mount uname timeout cat echo printf poweroff sleep mkdir mktemp rm grep sed awk cut head tail wc sort tr date ls kill; do
    ln -sfn busybox "$rootfs/bin/$app"
done
cp -a "$work/tests/io/pmu/." "$rootfs/test/io/pmu/"
cp /delivery/evidence/linux-build-20261027/guest-init.sh "$rootfs/init"
cp /delivery/publish/review-20261028/guest-verify.sh "$rootfs/test/pmu_verify.sh"
chmod +x "$rootfs/init" "$rootfs/test/pmu_verify.sh"
find "$work/tests" -type f -executable -exec file {} \; > "$out/cross-test-elf.txt"
cd "$rootfs"
find . -print0 | cpio --null -o --format=newc --quiet | gzip -1 > "$work/initramfs.cpio.gz"
echo 'PASS: RISC-V regression clients and minimal guest rootfs prepared'
