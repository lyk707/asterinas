#!/bin/bash
# SPDX-License-Identifier: MPL-2.0

# Build a riscv64 `perf` and place it at the RISC-V initramfs injection point
# (`test/initramfs/nix/perf-riscv64/perf`), which `initramfs.nix` installs as
# `/bin/perf` so the standard-perf guest acceptance can run on RISC-V.
#
# Run this on a riscv64 Linux machine, or inside a riscv64 user-mode chroot.
# Cross-building perf is deliberate manual work: `nixpkgs` does not cross-build
# `linuxPackages.perf`, and perf links against the target's libelf/libbfd.
#
# Environment:
#   KERNEL_SRC   kernel source tree providing `tools/perf` (default: this repo)
#   PERF_STATIC  set to 1 for a static binary when the guest has no dynamic loader
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
kernel_src=${KERNEL_SRC:-"$repo_root"}
out_dir="$repo_root/test/initramfs/nix/perf-riscv64"

if [ "$(uname -m)" != "riscv64" ]; then
	echo "error: run this on riscv64; perf is not cross-built here" >&2
	exit 1
fi
if [ ! -d "$kernel_src/tools/perf" ]; then
	echo "error: $kernel_src/tools/perf not found; set KERNEL_SRC" >&2
	exit 1
fi

make -C "$kernel_src/tools/perf" ARCH=riscv NO_LIBTRACEEVENT=1 \
	prefix=/usr ${PERF_STATIC:+LDFLAGS=-static} install

mkdir -p "$out_dir"
cp "$kernel_src/tools/perf/perf" "$out_dir/perf"
chmod 755 "$out_dir/perf"
echo "installed $(stat -c '%s' "$out_dir/perf") bytes to $out_dir/perf"
echo "verify with: file $out_dir/perf"
