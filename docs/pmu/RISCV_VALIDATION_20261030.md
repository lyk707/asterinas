# RISC-V QEMU + OpenSBI 客体验证（2026-10-30）

本轮在 AMD 主机的 Docker 开发容器中，以 QEMU TCG 模拟 RISC-V，
启动完整的 RISC-V 星绽内核并执行真实客体程序、SBI 调用和 PMU 系统调用。
结果为 `failed=0 skipped=1`，标准 perf 十轮通过，唯一跳过项是实际跨核迁移。
这是模拟客体的功能验证，不是 RISC-V 实体处理器的硬件性能实测。

## 1. 环境

| 项目 | 实际配置 |
|---|---|
| 主机 | AMD Ryzen 9 8945HX / WSL2 / Docker |
| 开发镜像 | `asterinas/dev:0.19.0-20261012` |
| 模拟器 | `qemu-system-riscv64` 10.2.1，`-accel tcg` |
| 固件 | QEMU 默认 OpenSBI 1.7，SBI 3.0，包含 PMU 扩展 |
| 机器 | `virt`，2 hart，2 GiB RAM |
| CPU | `rv64,svpbmt=true,zkr=true,sscofpmf=true,smcntrpmf=true` |
| Rust | `nightly-2026-07-21`，`riscv64imac-unknown-none-elf` |
| 客体客户端 | GCC 15.2.0 交叉编译，RISC-V ELF，glibc 2.42 |
| 客体 shell | 缓存中的 RISC-V BusyBox 1.37.0 及其 Nix 运行时闭包 |
| 标准 perf | 官方 Linux 6.18.7 源码交叉编译，无事件解析或统计逻辑补丁 |

`Platform PMU Device: ---` 表示没有额外的平台 PMU 驱动，不能单独据此判断 SBI PMU 不可用。
同一固件的 `Standard SBI Extensions` 包含 `pmu`，修复后的内核实际探测到：

```text
[pmu] cpu=CpuId(0) backend=Sbi counters=2 width=64b
[pmu] cpu=CpuId(1) backend=Sbi counters=2 width=64b
```

## 2. 本次发现并修复的问题

相对 2026-10-28 修订源码，本轮运行代码只修改两个文件：

- `ostd/src/pmu/riscv.rs`：在当前 hart 的 PMU 初始化中清除 `scounteren` 的
  cycle、instret、HPM 用户态访问权限，保留原有 time 权限，然后回读确认。
- `test/initramfs/src/regression/io/pmu/pmu_regression.c`：增加 RISC-V 专用回归，
  要求活跃 perf 事件存在时，子进程直接读取 cycle、instret、hpmcounter3 均收到 `SIGILL`。

原实现只检查 `scounteren`，发现固件允许直接读取便返回 `Unsupported`。
OpenSBI 1.7 的默认状态因此使两个 hart 的 PMU 都不可用。
修复由内核主动收紧访问权限，不放宽已有的隔离检查；若回读发现硬件没有遵守限制，仍拒绝启用。
初始化发生在该 CPU 禁用本地中断、用户任务启动之前。

新增的直读拒绝测试与受控 perf 计数测试同时通过，说明本轮环境中
“用户态禁止直接访问、内核通过 SBI/CSR 受控计数”的路径可以共同工作。

## 3. 构建与测试结果

| 检查 | 结果 |
|---|---|
| RISC-V PMU 内核构建 | `cargo osdk build --target-arch=riscv64 --scheme=riscv --features=pmu ...` 通过 |
| RISC-V PMU Clippy | `OSDK_TARGET_ARCH=riscv64 cargo osdk clippy --features=pmu -- --no-deps` 通过 |
| 主机测试 | 40 项 Rust 测试、14 项 runner 夹具，以及 C/ABI、双架构汇编检查通过 |
| 客体 PMU probe | 返回非零模拟计数 |
| 计数回归 | 属性、控制、8 种 read 布局、三种计数执行态、睡眠计时、远程读取、退出冻结通过 |
| 文件生命周期 | fork、dup、CLOEXEC、最终关闭、close/reopen 及资源复用通过 |
| 权限 | 无能力拒绝、CAP_PERFMON 自身/同 UID 允许、不同 UID 拒绝、SYS_PTRACE 授权通过 |
| RISC-V 直读隔离 | 活跃事件下 U-mode cycle/instret/HPM CSR 读取均被拒绝 |
| 并发和任务隔离 | dup/ioctl/read 并发、启用事件关闭重开及按目标线程隔离通过 |
| 标准 perf | 独立命令及随后 10 轮全部通过 |
| 实际跨核迁移 | SKIP；当前调度器没有按 affinity 变更迁移既有线程 |

独立 probe 返回 **6,096,625 cycles**；独立 perf 命令返回
**14,060,650 cycles:u / 14,059,982 instructions:u**。
这些数值属于 QEMU 模拟的 PMU，不能据此推导实体 CPU 的频率、IPC 或性能。

```text
perf version 6.18.7
PASS: RISC-V U-mode cycle/instret/HPM CSR reads denied with active perf event
PASS: concurrent dup/ioctl/read, enabled-event close/reopen and per-target isolation
SKIP migration: scheduler stayed on CPU 1 (requested 0); migration is unverified
PMU run: failed=0 skipped=1 logs=/tmp/pmu-review-results
PMU_VERIFY_RESULT abi=0 workload=0 probe=0 perf_version=0 perf_stat=0 suite=77
PMU_VERIFY_INIT_EXIT=77
```

QEMU 正常关机退出码为 0；测试结论由客体标记及每轮 CSV 决定。
77 仍表示有未覆盖项目，不能称为完整验收全部通过。
每轮 CSV 均保留两项指定事件的正计数、正运行时间和 100% 调度比例，已导出到主机证据目录。

## 4. perf 版本与构建路径

最初使用缓存中的 Linux perf 6.5.6。它在该客体中没有保留指定的
`cycles:u,instructions:u`，而是输出默认事件集合，严格验收因此失败。
结合其事件分组源码，缺失 PMU sysfs 描述是该行为的原因判断。
本轮改用官方 Linux 6.18.7 后，同一命令正常输出两项指定事件并通过原有验收脚本；
没有放宽脚本、伪造 sysfs 描述或修改 perf 的统计逻辑。

6.18.7 源码从 `cdn.kernel.org` 下载，按该目录发布的 SHA-256 校验通过。
构建关闭了非本项目范围的 ELF 分析、追踪、BPF、Python/Perl 扩展及厂商事件 JSON，
保留通用硬件事件解析和 `perf stat`。完整命令及构建日志在证据目录中。

标准 Nix 全回归 initramfs 构建因基础依赖构建失败未完成。
本轮改为使用已经缓存的 RISC-V GCC、glibc、BusyBox 与生产 PMU C 测试，
制作最小 initramfs；因此不能把本轮结论写成“整个 Nix/RISC-V 发行环境构建通过”。
用于 perf 构建的 flex、bison 仅安装在开发容器内。

## 5. 复现入口与证据

[evidence/20261030-riscv](evidence/20261030-riscv/) 保存：
固件探测、修复前后日志、内核和 perf 构建、RISC-V Clippy、主机测试、
全部 perf CSV、最终结果及构建输入校验和。

该目录中的 `prepare-rootfs.sh`、`build-kernel.sh`、`build-perf.sh`、`run-guest.sh`
是本次实际使用的脚本，记录了本机的 Nix store 和 `/delivery` 路径。
它们依赖对应缓存，不是适用于任意电脑的自动安装脚本；新环境需先准备等价的
RISC-V 工具链、BusyBox/运行库及标准 perf。
内核构建命令和 QEMU 参数可从脚本直接核对。

本轮没有 RISC-V 实体硬件、Intel PMU、客体硬件故障注入或真实跨核迁移的验收结果。
前一轮 AMD KVM 实测仍见 [VALIDATION_20261028.md](VALIDATION_20261028.md)。
