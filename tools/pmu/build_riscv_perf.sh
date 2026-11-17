#!/bin/bash
# SPDX-License-Identifier: MPL-2.0

# Build a riscv64 `perf` and place it at the RISC-V initramfs injection point
# (`test/initramfs/nix/perf-riscv64/perf`), which `initramfs.nix` installs as
# `/bin/perf` so the standard-perf guest acceptance can run on RISC-V.
#
# Run this on a riscv64 Linux machine, or inside a riscv64 user-mode chroot.
#
# Environment:
#   KERNEL_SRC   Linux source tree providing tools/perf
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
kernel_src=${KERNEL_SRC:?set KERNEL_SRC to a Linux source tree}
out_dir="$repo_root/test/initramfs/nix/perf-riscv64"

if [ "$(uname -m)" != "riscv64" ]; then
    echo "error: run this on riscv64; perf is not cross-built here" >&2
    exit 1
fi
if [ ! -d "$kernel_src/tools/perf" ]; then
    echo "error: $kernel_src/tools/perf not found; set KERNEL_SRC" >&2
    exit 1
fi

build_dir=$(mktemp -d)
trap 'rm -rf -- "$build_dir"' EXIT
# Counting does not need the scripting, ELF or trace event libraries.
make -C "$kernel_src/tools/perf" O="$build_dir" ARCH=riscv LDFLAGS=-static \
    NO_LIBELF=1 NO_LIBPYTHON=1 NO_LIBPERL=1 NO_LIBTRACEEVENT=1 \
    NO_SLANG=1 NO_GTK2=1 all

binary="$build_dir/perf"
elf=$(LC_ALL=C readelf -h -l -d -W "$binary")
if ! grep -Eq 'Class:[[:space:]]+ELF64' <<<"$elf" ||
    ! grep -Eq 'Machine:[[:space:]]+RISC-V' <<<"$elf" ||
    grep -Eq '^[[:space:]]*INTERP[[:space:]]|\(NEEDED\)' <<<"$elf"; then
    echo 'error: expected a static riscv64 ELF without an interpreter or shared libraries' >&2
    exit 1
fi

install -Dm0755 "$binary" "$out_dir/perf"
echo "installed static riscv64 perf to $out_dir/perf"
