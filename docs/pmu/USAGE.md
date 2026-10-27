# Build and test PMU counting

This guide builds the kernel and initramfs from source.
Prebuilt system images are not required.
The tested platform is an AMD Ryzen 9 8945HX running WSL2 and Docker with KVM.
Intel and RISC-V hardware acceptance remain separate work.

## Requirements

- An x86-64 Linux environment, including WSL2 with working nested virtualization.
- Docker running Linux containers and access to `asterinas/dev:0.19.0-20261012`.
- Network access for the first Docker, Cargo, Git and Nix dependency downloads.
- Additional disk space for dependencies, the Nix store and build artifacts.
- For hardware measurements: a usable `/dev/kvm` and PMU virtualization.
  Firmware virtualization alone does not establish these conditions.

The development image supplies the build tools, QEMU and UEFI firmware.
The Rust version is pinned by `rust-toolchain.toml` to `nightly-2026-07-21`.
The measurements below use two virtual CPUs and 4 GiB of guest memory;
the host also needs resources for Docker and compilation.

## Enter the development environment

Run these commands in a Linux shell, from the repository root.
WSL users should keep their checkout in the Linux filesystem.

```bash
docker info
grep -m 1 '^flags' /proc/cpuinfo | tr ' ' '\n' | grep -E '^(svm|vmx)$'
ls -l /dev/kvm
docker run -it --name asterinas-pmu-dev --device /dev/kvm \
    --mount "type=bind,source=$PWD,target=/root/asterinas" \
    --workdir /root/asterinas asterinas/dev:0.19.0-20261012 bash
```

The named container retains its dependency cache.
Use `docker start -ai asterinas-pmu-dev` to reenter it.
For compilation alone, `/dev/kvm` is unnecessary and its device mapping may be omitted.
Hardware measurements require a container created with that mapping.

## Build

Inside the container:

```bash
cd /root/asterinas
export PATH=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin:$PATH
export RUSTC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustc
export RUSTDOC=/root/.rustup/toolchains/nightly-2026-07-21-x86_64-unknown-linux-gnu/bin/rustdoc
bash tools/pmu/check.sh host
make kernel TARGET_ARCH=x86_64 FEATURES=pmu \
    ENABLE_REGRESSION_TEST=true ENABLE_PMU_TESTS=true
```

The explicit compiler paths avoid the development image's inconsistent rustup target metadata,
while retaining the pinned compiler version.
The kernel build produces `target/osdk/asterinas-osdk-bin.iso`.
Do not enable Cargo offline mode on a new environment without the required dependencies.

`ENABLE_PMU_TESTS=true` includes target-architecture standard perf.
The initramfs retains the perf package's own Nix store directory,
which its wrapper needs to execute the hidden binary.
The existing manual RISC-V perf injection route is also retained;
see [its instructions](../../test/initramfs/nix/perf-riscv64/README.md).
RISC-V cross-building of standard perf has not been validated here.

## Boot and measure

Inside the development container, confirm access before starting QEMU:

```bash
test -c /dev/kvm && test -r /dev/kvm && test -w /dev/kvm
cat /sys/module/kvm/parameters/enable_pmu
make run_kernel TARGET_ARCH=x86_64 FEATURES=pmu \
    ENABLE_REGRESSION_TEST=true ENABLE_PMU_TESTS=true \
    ENABLE_KVM=1 PMU_QEMU_CPU=host,pmu=on \
    SMP=2 MEM=4G QEMU_DISPLAY=none CONSOLE=ttyS0 NETDEV=none
```

Resolve any failed device check before continuing.
The Makefile may omit KVM if the device is missing, despite `ENABLE_KVM=1`.
Check that the resulting QEMU command includes `-accel kvm` and `-cpu host,pmu=on`.
On AMD, the project's default Intel CPU model is unsuitable for this measurement.

In the Asterinas guest shell:

```sh
cd /test/io/pmu
./pmu_probe
perf --version
perf stat --no-inherit --no-big-num -x ';' \
    -e cycles:u,instructions:u -- ./pmu_workload 2000000
sh ./run_pmu_tests.sh
suite_rc=$?
echo "PMU suite exit=$suite_rc"
```

`--no-inherit` matches the implementation's unsupported event-inheritance boundary.
The probe must return a positive hardware count.
The runner checks regression, concurrency, migration and ten standard perf rounds.
Exit 77 means incomplete prerequisites or skipped coverage and must not count as a pass.
The current scheduler does not migrate an existing thread on `sched_setaffinity`,
so the migration test reports a skip when the observed CPU never changes.

Preserve the guest output and the runner's CSV files before guest shutdown.
CSV files in the guest's temporary filesystem are not automatically exported to the host.
QEMU process exit alone is not a test verdict.

Recorded measurements and their limits are in
[the validation report](VALIDATION_20261027.md).
