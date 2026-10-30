#!/bin/bash
set -euo pipefail
out=/delivery/publish/riscv-validation-20261030
work=/root/pmu-riscv-validation-20261030
version=${PERF_VERSION:-6.18.7}
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy
if [[ ! -d "$work/linux-$version/tools/perf" ]]; then
    if [[ $version == 6.5.6 ]]; then
        archive=/nix/store/w8cdfv0ip9q2jvyi1csc3kf4k6azqqmn-linux-6.5.6.tar.xz
    else
        archive="$work/linux-$version.tar.xz"
        curl --fail -L --retry 2 --connect-timeout 10 --max-time 180 \
            "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$version.tar.xz" -o "$archive" > "$out/perf-source-download.log" 2>&1
        curl --fail -L --connect-timeout 10 --max-time 30 \
            https://cdn.kernel.org/pub/linux/kernel/v6.x/sha256sums.asc -o "$work/kernel-sha256sums.asc" 2>> "$out/perf-source-download.log"
        (cd "$work"; grep " linux-$version.tar.xz$" kernel-sha256sums.asc | sha256sum --check) | tee "$out/perf-source-check.txt"
    fi
    tar -xJf "$archive" -C "$work"
fi
binutils=/nix/store/7770fl584xjgzyaygilpi8spskcbl58k-riscv64-unknown-linux-gnu-binutils-2.46/bin/riscv64-unknown-linux-gnu-
make -C "$work/linux-$version/tools/perf" -j4 JOBS=4 ARCH=riscv \
    CC="$work/bin/riscv64-cc" AR="${binutils}ar" LD="${binutils}ld" \
    STRIP="${binutils}strip" HOSTCC=gcc PKG_CONFIG=/bin/false WERROR=0 \
    NO_LIBELF=1 NO_LIBBPF=1 NO_LIBTRACEEVENT=1 NO_LIBPYTHON=1 NO_LIBPERL=1 \
    NO_SLANG=1 NO_GTK2=1 NO_LIBUNWIND=1 NO_LIBAUDIT=1 NO_LIBNUMA=1 NO_LIBCRYPTO=1 \
    NO_LIBBFD=1 NO_LIBDW_DWARF_UNWIND=1 NO_JVMTI=1 NO_LIBZSTD=1 NO_ZLIB=1 \
    NO_LZMA=1 NO_BPF_SKEL=1 NO_LIBPFM=1 NO_DEMANGLE=1 NO_SDT=1 NO_LIBCAP=1 NO_JEVENTS=1 \
    > "$out/perf-build.log" 2>&1
file "$work/linux-$version/tools/perf/perf" | tee "$out/perf-elf.txt"
cp "$work/linux-$version/tools/perf/perf" "$work/rootfs/bin/perf"
echo 'PASS: standard Linux perf cross-built for RISC-V'
