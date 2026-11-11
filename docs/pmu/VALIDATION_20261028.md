# PMU 必要修订与 AMD KVM 验证（2026-10-28）

本次以已发布提交 `8cfb392649174235455cc8aeb8645ebbc46967f7` 为基线，
修复跨进程权限检查、无事件时的定时采样开销以及硬件释放失败的无限重试。
修订源码已重新完整编译，并在本机 AMD Ryzen 9 8945HX 的 KVM 客体中执行验证。
这是硬件虚拟化环境中的真实 PMU 计数验证；未从 U 盘启动裸机 Asterinas。

## 1. 修订内容

本次运行代码及测试共涉及 9 个文件：

| 文件 | 修改目的 |
|---|---|
| `kernel/core/src/syscall/perf_event_open.rs` | 显式目标经过跨进程访问检查 |
| `kernel/core/src/process/posix_thread/alien_access.rs` | 调整已被 PMU 使用的访问模式 lint 标记 |
| `kernel/core/src/events/perf/activity.rs`（新增） | 事件记录存续期间的 RAII 活动提示 |
| `kernel/core/src/events/perf/context.rs` | 每条记录持有活动提示 |
| `kernel/core/src/events/perf/mod.rs` | 无软件事件时跳过采样，保留独立清理 |
| `ostd/src/pmu/retirement.rs`（新增） | 有界释放重试与退避策略 |
| `ostd/src/pmu/mod.rs` | 接入退避、隔离及 CPU 本地清理提示 |
| `test/initramfs/src/regression/io/pmu/pmu_regression.c` | 客体权限允许/拒绝回归 |
| `tools/pmu/backend_tests.rs` | 活动生命周期与持续释放故障测试 |

### 1.1 跨进程监测必须经过目标访问检查

`kernel/core/src/syscall/perf_event_open.rs` 在解析显式目标 TID 后，
调用目标 POSIX 线程的 `check_alien_access_from(READ_WITH_REAL_CREDS)`。
找不到 POSIX 目标时返回 `ESRCH`，不使用 `unwrap()`。

现有 `CAP_PERFMON` / `CAP_SYS_ADMIN` 硬件监测能力检查保留。
跨进程访问还必须通过现有 alien-access LSM 钩子；同进程访问沿用该接口的豁免。
内置 capability LSM 允许满足真实凭据匹配条件的访问，或具备 `CAP_SYS_PTRACE` 的访问。

这是本实现明确采用的保守策略，比 Linux 的 CAP_PERFMON 豁免更严格。
不能把它描述为逐项复制 Linux 权限语义；参见
[Linux v6.16 的 perf_check_permission](https://github.com/torvalds/linux/blob/v6.16/kernel/events/core.c#L12521-L12548)。

新增客体回归实际验证：无监测能力拒绝；仅 CAP_PERFMON 的隐式自身、显式自身、
同 UID 跨进程访问允许；不同 UID 目标拒绝；补充 CAP_SYS_PTRACE 后允许该目标。
本轮验证的是内置访问策略，未安装额外的自定义 LSM 模块。

### 1.2 空闲定时采样与延迟硬件清理解耦

新增 `kernel/core/src/events/perf/activity.rs`，由每条软件事件记录持有 RAII 活动标记。
记录真正移除时才减少全局计数，覆盖创建回滚、关闭后的延迟移除以及上下文销毁。

`sync_current()` 仍先服务延迟硬件清理。软件记录数为零时，跳过
`Thread::current()`、Arc 克隆及 `PerfContext::on_tick()` 的锁操作。
OSTD 为每个 CPU 保存独立的清理提示；没有待清理 token 时也跳过 PMU 本地锁。
因此软件事件全部移除后，释放失败的硬件 token 仍可继续清理。

这些原子值只用作快速路径提示；实际记录、所有权和退役状态仍受原有锁保护。
本次保留调度切入/切出的 CPU 归属跟踪，避免改变首次远程安装事件的行为。
未测量整体 CPU 开销百分比，不能将静态路径减少换算为已测得的性能收益。

### 1.3 释放失败退避与资源隔离

新增 `ostd/src/pmu/retirement.rs`，直接释放和延迟清理共用同一重试预算。
首次失败后分别等待 1、2、4、8、16、32、64 个系统 tick，
累计 8 次失败后停止访问该 token。正常时钟下，最早在首次失败后的 127 tick 耗尽预算。
`Drop`、新配置和 timer 的重复调用不会重置预算或绕过退避。

预算耗尽后保留 token 及其资源占用，并将该 CPU 的 PMU 能力标记为 `HardwareFault`。
禁止新分配；原有句柄仍可执行停表和释放。释放未确认的计数器不会被重新分配。
清理提示只覆盖仍允许重试的 token，隔离资源不会触发永久的周期性硬件访问。

**已知限制：** 无法确认释放的槽位保持占用，且该 CPU 的 PMU 不会在本次启动中自动恢复。
当前 `cpu=-1` 的能力预检仍遍历全部 CPU，因此任一 CPU 隔离后，新的线程模式 open
也可能直接返回 `EIO`。这保留了原有预检语义；未在本次改为延迟报告错误。
若硬件持续拒绝停表，软件无法保证物理计数器已停止，只能保留所有权并拒绝复用。

## 2. 验证环境与结果

| 项目 | 本次结果 |
|---|---|
| 主机 CPU | AMD Ryzen 9 8945HX |
| WSL 内核 | 6.18.33.2-microsoft-standard-WSL2 |
| 开发镜像 | asterinas/dev:0.19.0-20261012 |
| QEMU | 10.2.1，显式 `-accel kvm -cpu host,pmu=on` |
| 客体资源 | 2 vCPU、4 GiB RAM |
| QMP / 主机 PMU | KVM enabled=true，pmu=true，perfctr-core=true，enable_pmu=Y |
| Rust 主机测试 | 40 项通过：4 项算术、36 项后端/属性/计时/活动/清理测试 |
| 测试脚本夹具 | 14 项通过；另有 C 编译、ABI 及双架构汇编布局检查 |
| 格式检查 | 修改的 Rust / C 文件通过 |
| x86-64 PMU 完整内核构建 | 通过 |
| PMU 开启时 Clippy | `cargo osdk clippy --features=pmu -- --no-deps` 通过 |
| PMU 关闭时完整内核构建 | 通过 |
| 客体权限回归 | 新增的允许/拒绝分支全部通过 |
| 客体计数与并发回归 | 通过，含远程读取、退出、close/reopen、dup/ioctl/read 和任务隔离 |
| 标准 perf | 独立命令通过，随后 10 轮通过；每轮 CSV 已导出到客体串口日志 |
| 套件结果 | `failed=0 skipped=1`；跳过的是尚未实现的实际跨核迁移 |

独立 probe 读到 **4,002,382 cycles**；独立标准 perf 命令读到
**8,850,921 cycles / 20,260,921 instructions**。这些是本轮观测值，不是固定期望值。

客体最终标记：

```text
PASS: CAP_PERFMON self/same-UID access, cross-UID denial, SYS_PTRACE authorization
PASS: concurrent dup/ioctl/read, enabled-event close/reopen and per-target isolation
PMU run: failed=0 skipped=1 logs=/tmp/pmu-review-results
PMU_VERIFY_RESULT abi=0 workload=0 probe=0 perf_version=0 perf_stat=0 suite=77
PMU_VERIFY_INIT_EXIT=77
```

QEMU 通过 debug-exit 退出，进程退出码为 33；测试结论取客体结果标记。
77 表示仍有未覆盖项目，不能记为整个验收套件全部通过。

## 3. 故障验证的边界

新增主机测试直接包含生产的退避模块和 x86/SBI token 实现，
以注入的 MSR / SBI 访问错误验证持久故障、瞬态恢复、预算耗尽、时钟上界和资源所有权保留。
持久 SBI 故障在模拟的 10,000 tick 中只执行 8 次释放尝试、16 次 SBI 调用；
持久 AMD 故障也只执行 8 次释放尝试。
另有跨线程的活动标记生命周期及回滚测试。

上述故障注入是主机单元测试，**没有在 KVM 客体中人为制造实际 MSR/SBI 停表故障**。
本机 KVM 验证覆盖正常硬件计数、真实系统调用权限边界和并发生命周期。
本轮未执行 Intel/RISC-V 硬件验收、RISC-V 完整构建或完整仓库 lint。

## 4. 可审计证据

本轮日志、QMP 信息、结果汇总及构建输入校验和位于
[evidence/20261028](evidence/20261028/)。
源码与镜像构建输入经过校验；后续关闭 PMU 的构建未替换本轮已验证的独立验证 ISO。
源码使用方法见 [USAGE.md](USAGE.md)。
