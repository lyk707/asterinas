# Asterinas 性能事件计数支持设计与验证报告

> 2026-10-27 实测补充：AMD / WSL / KVM 已得到非零硬件计数，
> 计数回归、并发及十轮标准 perf 无失败；跨 CPU 迁移受调度器限制而跳过。
> 本轮实际执行范围与原始证据见 [实测记录](VALIDATION_20261027.md)，
> 源码构建与运行步骤见 [使用说明](USAGE.md)。
> 下文较早的构建与静态检查描述不代表本轮全部重新执行。


1.  第一章 背景、问题与范围
2.  第二章 总体设计
3.  第三章 用户接口与请求解析
4.  第四章 线程事件模型与创建流程
5.  第五章 PMU 资源模型与架构适配
6.  第六章 调度同步与累计规则
7.  第七章 事件控制与生命周期
8.  第八章 设计取舍与集成验证

## 摘要

本文说明 Asterinas 中线程级硬件性能事件计数的设计。实现以 Linux `perf_event_open` 的基础计数接口为对接目标，当前关注 `cycles` 与 `instructions` 两类事件，通过 fd 完成启停、读取、重置和关闭，并将计数结果正确归属到目标线程。

整体采用四层结构：系统调用层解析 Linux ABI 并创建 fd；线程事件层通过 `PerfContext`、`Record` 与 `Ticket` 管理逻辑事件；OSTD 通过 `CounterHandle` 和每 CPU 的资源记录管理临时硬件分配；架构后端分别处理 Intel、AMD Zen 和 RISC-V SBI PMU 的能力探测、事件配置、计数器启停、读取与释放。线程事件与硬件分配分开，使一个逻辑事件可以跨越多次调度和 CPU 迁移累计，而不把硬件计数器永久绑定给线程。

本轮已在 AMD / WSL / KVM 中完成 x86-64 PMU 内核构建和客体计数验证。
主机侧 34 项 Rust 测试与 14 项 runner 夹具通过，客体回归、并发、任务隔离及十轮标准 perf 无失败；
跨 CPU 迁移受当前调度器限制而跳过，完整套件为 `failed=0 skipped=1`。
本轮未重新执行双架构 Clippy、完整 RISC-V 构建或全仓库检查，具体证据见 [实测记录](VALIDATION_20261027.md)。

### 本文的核心对象

| 对象                              | 主要职责                                                         |
|-----------------------------------|------------------------------------------------------------------|
| `PerfEventFile`                   | 将用户 fd 与事件上下文、Ticket、读数格式连接起来                 |
| `PerfContext` / 线程 `Record`     | 管理目标线程的事件状态、累计值、故障和生命周期                   |
| `Ticket`                          | 通过槽位、代数和 ID 定位一条逻辑事件，避免槽位复用后的旧引用误用 |
| `CounterHandle`                   | 标识某 CPU 上的一次临时硬件资源分配                              |
| OSTD 资源 `Record` / 后端 `Token` | 保存分配状态、累计基准以及可用于操作硬件或 SBI 的信息            |

### 当前验证状态

| 本轮已完成 | 仍未完成 |
|---|---|
| x86-64 PMU 完整构建、34 项 Rust 测试、14 项 runner 夹具 | 本轮重新执行双架构 Clippy、完整 RISC-V 构建和全仓库检查 |
| AMD KVM 客体中的非零 cycles/instructions 和十轮标准 perf | Intel / RISC-V 客体硬件验收与原生 Linux 性能开销测量 |
| 计数生命周期、远程读取、退出、并发和任务隔离回归 | 实际跨 CPU 迁移，以及停止/释放硬件故障注入 |

## 第一章 背景、问题与范围

### 1.1 为什么需要性能事件接口

程序运行时间变长，可能是执行的指令更多，也可能是缓存访问、分支预测或调度等待出了问题。只记录开始和结束时间，通常无法区分这些原因。性能分析需要进一步观察：程序执行了多少条指令、消耗了多少个 CPU 周期，以及这些开销发生在哪里。

Linux 通过 perf 工具和内核性能事件子系统提供这类观察能力。本次工作的目标，是在 Asterinas 中建立其中最基础的一条链路：用户态提出计数请求，内核将请求关联到目标线程，驱动 CPU 的性能监测硬件累计事件，最后通过文件描述符把结果交还用户态。

本文先说明 perf 和 PMU 的关系，再以一次 `cycles:u` 计数为例，逐步说明各个对象何时产生、保存什么信息，以及它们如何共同完成计数。

### 1.2 perf 是什么

通常所说的 perf 包含两个相互配合的部分：用户态的 `perf` 命令行工具，以及 Linux 内核中的性能事件机制。命令行工具负责选择分析对象、配置事件、收集和展示结果；内核负责权限检查、事件管理、调度同步和访问底层事件来源。\[1\]

perf 能观察的对象包括 CPU 硬件事件、内核维护的软件事件、静态跟踪点，以及通过 kprobe、uprobe 等机制建立的动态探测事件。因此，perf 的能力既包括 PMU 硬件计数，也包括操作系统和程序执行过程的观察。\[1\]\[2\]

完整 perf 的主要能力可以按下表理解。具体事件和功能能否使用，还取决于处理器、内核配置、权限以及相关数据是否可用。

| 能力               | 解决的问题                                     | 典型用法或机制                                                                      |
|--------------------|------------------------------------------------|-------------------------------------------------------------------------------------|
| 事件发现与配置     | 当前平台能观察哪些事件，怎样选择事件和执行态   | `perf list`；硬件、软件、tracepoint、raw 等事件；`u`、`k` 等过滤条件                |
| 累计计数           | 某段运行期间，事件一共发生了多少次             | `perf stat`；周期数、指令数、缓存或分支相关计数；按可用事件计算指标                 |
| 执行采样           | 程序运行开销主要集中在哪里                     | `perf record` 记录样本；可按配置记录指令地址、调用栈等信息                          |
| 热点分析与代码定位 | 哪些函数或指令消耗较多，两个版本的热点有何变化 | `perf report`、`perf top`、`perf annotate`、`perf diff`                             |
| 内核与应用行为跟踪 | 调度、系统调用或指定探测点在何时发生           | `perf sched`、`perf trace`、`perf probe`、`perf script`                             |
| 专项性能分析       | 锁竞争、内存访问、缓存行共享等问题出现在哪里   | `perf lock`、`perf mem`、`perf c2c` 等专项工具                                      |
| 多种监测范围       | 观察一个线程、一组任务、指定 CPU 或整个系统    | 线程/进程选择、CPU 选择、全系统与 cgroup 等监测方式                                 |
| 事件协同与数据传输 | 多个事件怎样一起运行，如何传送计数和样本       | 事件组、任务继承、计数器分时复用；`read`、`mmap` 环形缓冲区及部分平台的辅助跟踪数据 |

上表概括了 perf 的主要能力。\[1\]–\[4\]\[6\] 本次实现其中的**线程级硬件事件简单计数**。例如，用户可以请求统计某线程在用户态执行期间的 CPU 周期数；函数热点、调用栈和行为跟踪等分析需要另外的机制支持。

### 1.3 PMU 是什么，它与 perf 如何配合

PMU 是 Performance Monitoring Unit，即性能监测单元。本文主要讨论 CPU 核心的 PMU。它提供事件选择、执行态过滤和硬件计数器等能力：软件先指定要观察的事件，硬件随后在事件发生时自动增加计数值。

例如，选择周期事件后，计数器按该处理器对此事件的定义累计 CPU 周期；选择指令事件后，计数器累计已经退休的架构指令。这里的“退休”指指令已完成并提交，不能简单等同于流水线里曾经执行过的所有推测操作。两类事件的具体硬件实现可能随处理器而异。\[2\]

perf 提供面向程序的性能分析接口，PMU 提供其中一部分底层硬件数据。用户态不能直接把“监测线程 A”写进 PMU：线程是操作系统管理的对象，PMU 计数器则属于 CPU。内核需要在线程运行时配置计数器，在线程切出时停止并保存结果，才能将硬件计数归属到正确的线程。

这也是本次设计的核心问题：**逻辑事件属于线程，硬件计数器属于 CPU，两者的寿命和归属不同。**

### 1.4 本次完成的范围

本次以 Linux 的 `perf_event_open` 计数接口为对接目标。用户通过它创建一个事件并获得 fd，之后使用 `ioctl` 控制事件、使用 `read` 读取累计值，最后关闭 fd。

| 项目       | 本版设计范围                                                                      |
|------------|-----------------------------------------------------------------------------------|
| 事件种类   | `cycles`、`instructions`；每个线程每类最多一条事件                                |
| 监测对象   | 当前线程，或通过正数 TID 指定的目标线程                                           |
| CPU 条件   | 不限 CPU，或仅在线程运行于指定 CPU 时计数                                         |
| 执行态     | 用户态、内核态，或二者同时计数；要求排除 hypervisor                               |
| 用户接口   | `perf_event_open`；ENABLE、DISABLE、RESET、ID ioctl；非组格式 `read`；关闭事件    |
| 生命周期   | 与线程调度配合；线程迁移后继续累计；目标退出后保留最终结果；支持 `enable_on_exec` |
| 权限       | 本版要求调用者具有 CAP_PERFMON 或 CAP_SYS_ADMIN                                   |
| 本次不实现 | 采样、调用栈、`mmap` 样本缓冲区、事件组、全系统监测、任务继承、计数器分时复用等   |

采用 Linux 的参数和返回布局，是为了让用户程序能够使用熟悉的计数接口。但支持这个接口的一个子集，与完整支持标准 Linux perf 工具，是两个不同的验收目标。现有 `pmu_stat` 是验证本次链路的用户态程序；标准 `perf stat` 是否可直接运行，需要另外检查其依赖和实际调用行为。

当前设计按统一的线程事件接口组织多架构 PMU 后端：x86-64 侧覆盖 Intel 固定计数器与 AMD Zen 事件计数，RISC-V 侧通过 SBI PMU 接口适配计数器操作。三类后端共享上层的事件配置、资源句柄和生命周期管理，但硬件资源发现、事件编码、启停、读取、过滤与释放方式由各自后端负责。编译与静态检查通过只能证明代码通过对应检查；具体 CPU、固件和虚拟化环境中的硬件行为仍需结合客体实测验证。

### 1.5 本设计需要解决的问题

从用户态传入“统计一个线程的周期数”，到得到一份可解释的结果，中间至少需要解决四个问题。

首先是接口语义。用户使用 Linux 的事件编号、属性布局和控制命令，内核需要解释这些输入，并明确拒绝尚未实现的请求。

其次是归属。用户选择的是线程，真正执行计数的是 CPU。线程会睡眠、切换和迁移；内核需要把多个 CPU 上的运行片段归到同一个事件，并在目标不运行时停止该事件的硬件片段。

再次是对象寿命。目标可以先退出，调用者稍后再读取；文件可以被复制引用，一次 close 也不一定销毁事件；硬件资源则应在每次运行片段结束后及时归还。这几件事需要分别建模。

最后是结果有效性。资源不足、权限过滤无法保证、寄存器访问失败和观测间隔过长，都可能让一个看似正常的数值失去意义。设计需要同时说明正常路径和失败后如何停止、报告与回收。

后文的分层和类型围绕这四个问题展开。

## 第二章 总体设计

### 2.1 贯穿全文的例子

假设一个用户线程希望统计自己执行一段计算时的用户态 CPU 周期数。它先创建一个禁用的事件，准备好后启用，执行计算，再禁用并读取结果。

下面是说明调用顺序的示意代码，省略错误处理；`workload()` 表示被测代码。

``` c
struct perf_event_attr attr = {0};
attr.type = PERF_TYPE_HARDWARE;
attr.size = 64;
attr.config = PERF_COUNT_HW_CPU_CYCLES;
attr.disabled = 1;
attr.exclude_kernel = 1;
attr.exclude_hv = 1;

int fd = syscall(SYS_perf_event_open, &attr, 0, -1, -1,
                 PERF_FLAG_FD_CLOEXEC);
ioctl(fd, PERF_EVENT_IOC_RESET, 0);
ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
workload();
ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);

uint64_t count;
read(fd, &count, sizeof(count));
close(fd);
```

这里 `pid=0` 选择当前调用线程，`cpu=-1` 表示不限制其运行 CPU，`group_fd=-1` 表示创建独立事件。由于 `read_format` 为零，读出的结果只有一个 64 位累计值。

整个过程分成三个阶段：

1.  **创建事件。** 把用户请求转换成内部配置，在目标线程中建立一条逻辑记录，并返回 fd。
2.  **执行计数。** 目标线程实际运行且事件启用时，在当前 CPU 上申请计数器；切出时结算并释放，切入后重新申请。
3.  **取回结果与结束。** 通过 fd 找到原事件，取得累计快照；最后一个文件引用销毁时关闭事件。

创建 fd 并不总意味着立即占用一个硬件计数器。示例创建时事件处于禁用状态，内核先保存请求，等到满足运行条件后才申请硬件资源。

### 2.2 四层分别负责什么

用户态是调用方。内核实现分成以下四层，后文按事件的实际经历展开，不要求读者先记住全部类型。

| 层次          | 负责的问题                                  | 主要抽象                                                                    | 主要位置                                     |
|---------------|---------------------------------------------|-----------------------------------------------------------------------------|----------------------------------------------|
| 系统调用适配  | 如何理解用户参数、检查权限并返回 fd         | `PerfEventAttrV0`、`AttrHeader`                                             | `kernel/core/src/syscall/perf_event_open.rs` |
| 线程事件管理  | 事件属于谁，如何控制、随调度累计并读取      | `EventSpec`、`PerfContext`、`Record`、`Ticket`、`PerfEventFile`、`Snapshot` | `kernel/core/src/events/perf/`               |
| OSTD 资源管理 | 当前 CPU 上哪些资源可用，如何安全分配和归还 | `EventConfig`、`PmuCaps`、`CounterHandle`、`LocalPmu`                       | `ostd/src/pmu/mod.rs`                        |
| 架构后端      | 如何把统一事件配置变成寄存器操作            | 后端 `Token`、事件编码、MSR 访问封装                                        | `ostd/src/pmu/x86.rs`、`x86_msr.S`           |

系统调用层处理 Linux ABI，线程事件层处理线程策略，OSTD 层管理 CPU 资源，架构后端处理硬件差异。这样，线程调度逻辑不需要知道 AMD 的 MSR 编号，后端也不需要知道用户 fd 或目标 TID。

### 2.3 先看对象的归属关系

``` mermaid
flowchart TD
    F["调用者的文件描述符表"] --> P["PerfEventFile"]
    T["目标线程 Thread"] --> C["PerfContext"]
    P -->|"context 引用"| C
    P -->|"ticket 定位"| R["线程事件 Record"]
    C -->|"State.records 保存"| R
    R -->|"live 保存当前分配"| H["CounterHandle"]
    H -->|"CPU、槽位和代数定位"| L["LocalPmu 中的资源记录"]
    L -->|"token 保存硬件信息"| B["架构后端 Token"]
```

图中有两条长期关系：目标线程拥有自己的监测上下文，事件文件引用这个上下文中的某条事件。`CounterHandle` 以下是临时硬件关系，会随线程切换而建立或撤销。

fd 放在调用者的文件表里，事件放在目标线程的上下文里。二者可以属于不同线程：例如父线程持有 fd，监测子线程。下面先解释用户请求怎样建立这两条长期关系，再解释硬件关系怎样随运行变化。

### 2.4 对象寿命与引用关系

本设计区分事件的逻辑存在、硬件片段的运行，以及文件对象的内存寿命。

| 关系                 | 开始与结束                                                  | 期间保留什么                                               |
|----------------------|-------------------------------------------------------------|------------------------------------------------------------|
| 逻辑事件             | `create()` 建立记录；最后的文件对象销毁后关闭记录并完成清理 | 配置、事件身份、累计值、启用和错误状态                     |
| 硬件运行片段         | 满足运行条件时分配并启动；切出、禁用、冻结或退出时停止结算  | 当前 CPU 上的句柄、资源记录及后端 Token                    |
| 文件对象及上下文引用 | 返回 fd 前建立文件引用；最后引用释放后触发 Drop             | 指向原上下文和原事件的访问关系；目标退出后仍可读取最终记录 |

目标退出会结束运行，却不必立即删除事件记录。禁用会停止硬件片段，却仍允许读取累计值。上下文的 `Arc` 保证内存仍可访问，是否还能启用或运行则由 `exited`、`closed`、`fault` 等状态判断。

引用方向也因此可以保持清楚：Thread 与 PerfEventFile 共同引用 PerfContext，上下文中的记录持有临时句柄；记录没有反向持有 PerfEventFile 的强引用。文件销毁可主动通知上下文关闭事件，不需要等待目标线程对象消失。

### 2.5 模块依赖与计数约束

系统调用适配依赖线程事件管理和 OSTD 配置类型；线程事件管理通过 OSTD 的公开资源接口操作硬件；OSTD 在编译期选择架构后端。OSTD 不反向查找内核 Thread 或文件表，因此更换硬件后端不要求重写 fd 和线程事件的语义。

后续设计应维持以下约束：

1.  **操作必须指向原事件。** 文件通过上下文与 Ticket 选择逻辑事件；CPU 操作通过句柄的 CPU、槽位和代数选择本次资源分配。
2.  **一个运行片段只结算一次。** 周期观测更新片段内部累计值，片段结束时再加入线程累计值。
3.  **硬件操作在所属 CPU 上执行。** 远程请求通过同步通知协调，不能在调用者 CPU 上代读另一颗 CPU 的寄存器。
4.  **失败后仍保留清理依据。** 已取得的 Token 必须能被追踪；清理未完成的资源不能被重新当作空闲资源。
5.  **不把失败变成有效计数。** 不支持或发生错误时返回明确错误，不能用零代替失败结果。

这些约束分别落实到后文的凭据校验、调度结算、冻结同步和退役资源回收中。

## 第三章 用户接口与请求解析

### 3.1 系统调用入口接收哪些信息

用户态通过原有系统调用框架进入内核。x86-64 分发表需要接入：

``` rust
SYS_PERF_EVENT_OPEN = 298 => sys_perf_event_open(args[..5]);
```

接入位置是 `kernel/core/src/syscall/arch/x86.rs`。通用架构表 `kernel/core/src/syscall/arch/generic.rs` 使用编号 241。编号由目标架构的 ABI 决定，处理函数可以共享。

内核处理函数的签名如下，位置是 `kernel/core/src/syscall/perf_event_open.rs`：

``` rust
pub(super) fn sys_perf_event_open(
    attr_addr: Vaddr,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
    ctx: &Context,
) -> Result<SyscallReturn>;
```

| 输入，按函数形参顺序列出 | 输入含义                         | 本次处理后的去向                                  |
|--------------------------|----------------------------------|---------------------------------------------------|
| `attr_addr`              | 用户态属性结构的地址             | 经受控用户内存访问复制为 `PerfEventAttrV0`        |
| `pid`                    | 要监测哪个线程                   | 解析为目标 `Arc<PerfContext>`；另存 TID 供展示    |
| `cpu`                    | 是否限制计数 CPU                 | `-1` 转为 `None`，有效 CPU 编号转为 `Some(CpuId)` |
| `group_fd`               | 是否加入一个事件组               | 本版只接受 `-1`，不建立组关系                     |
| `flags`                  | 本次创建调用的标志               | 本版接受 CLOEXEC，并转换为文件表的 fd 标志        |
| `ctx`                    | 原有框架提供的当前系统调用上下文 | 用于访问用户地址空间、调用线程、凭据和文件表      |

前五项来自用户请求，最后的 `ctx` 由内核提供。它与后文线程持有的 `PerfContext` 是两种不同对象。

本次复用原有的系统调用入口、寄存器参数提取和分发宏。在 x86-64 上，调用号来自 RAX，五个参数依次来自 RDI、RSI、RDX、R10、R8；分发宏按位置将它们交给处理函数。这里新增的是 perf 调用的接入和处理逻辑。

### 3.2 用户属性布局：`PerfEventAttrV0` 与 `AttrHeader`

内核收到的 `attr_addr` 只是地址。要知道这个地址后的字节分别表示什么，必须先遵守用户程序使用的 ABI 布局。因此在 `kernel/core/src/syscall/perf_event_open.rs` 定义两个接收结构：

``` rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
struct PerfEventAttrV0 {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
struct AttrHeader {
    type_: u32,
    size: u32,
}
```

`AttrHeader` 只读前 8 字节，以便先知道用户声明的长度；`PerfEventAttrV0` 对应基础 64 字节属性。`#[repr(C)]` 保持 C 布局，`Pod` 配合现有用户内存接口进行按值复制。

| 字段              | 本次如何使用                                       |
|-------------------|----------------------------------------------------|
| `type_`、`config` | 确认是硬件事件，并选择 cycles 或 instructions      |
| `size`            | 校验用户提供的结构长度                             |
| `flags`           | 解释 disabled、执行态过滤、enable_on_exec 等属性位 |
| `read_format`     | 决定读数时追加时间和事件 ID 的哪些字段             |
| 其余字段          | 保留既定位置，检查用户是否请求了本版尚未支持的功能 |

所以，结构体保留采样字段，不代表本版已经支持采样；删除这些字段会改变后续字段的字节偏移，使同一份用户参数被错误解释。

`sample_type` 是这条规则的唯一例外：标准 `perf stat` 即使在纯计数模式下也会设置 `PERF_SAMPLE_IDENTIFIER`，该位只影响样本记录的布局，而计数事件不产生样本。因此本版接受 `sample_type` 为零或恰好等于 `PERF_SAMPLE_IDENTIFIER`，其余任何采样位都仍然表示真实的采样请求，返回 `EOPNOTSUPP`。

`copy_attr()` 通过 `ctx.user_space()` 读取用户内存：先读头部并检查长度，再读基础结构，比较两次读取的 `size`；超过 64 字节的扩展部分要求全零。长度不支持时尝试回写支持的长度并返回 E2BIG。地址访问失败则返回相应错误。

这里的输出是内核自己持有的属性值 `attr`。后续操作使用复制结果，用户地址不会被直接当成可信内核指针。

### 3.3 `Event` 和 `EventConfig`：只表达硬件要统计什么

复制完成后，系统调用先检查请求是否属于支持范围。通过检查后，需要把 Linux 的数字编号和排除位转换成内部更直接的表达。

为此在 `ostd/src/pmu/mod.rs` 定义：

``` rust
pub enum Event {
    /// CPU clock cycles, as defined by the processor.
    CpuCycles,
    /// Retired architectural instructions.
    Instructions,
}
```

``` rust
pub struct EventConfig {
    event: Event,
    scope: CountingScope,
}

pub enum CountingScope {
    /// Count only user-mode (CPL3 / U-mode) execution.
    User,
    /// Count only kernel-mode (CPL0 / S-mode) execution.
    Kernel,
    /// Count execution in both modes.
    UserAndKernel,
}
```

`Event` 给事件命名，`CountingScope` 给出执行态范围，`EventConfig` 把两者组合起来。
用枚举而不是两个布尔字段，是为了让「两个执行态都排除」这种无意义配置无法被构造。

示例中的 `cycles:u` 对应 `EventConfig::new(Event::CpuCycles, CountingScope::User)`。转换代码在
`kernel/core/src/syscall/perf_event_open.rs`：

``` rust
let is_user_counted = attr.flags & ATTR_EXCLUDE_USER == 0;
let is_kernel_counted = attr.flags & ATTR_EXCLUDE_KERNEL == 0;
let scope = match (is_user_counted, is_kernel_counted) {
    (true, false) => CountingScope::User,
    (false, true) => CountingScope::Kernel,
    (true, true) => CountingScope::UserAndKernel,
    (false, false) => {
        return_errno_with_message!(Errno::EINVAL, "all requested privilege levels are excluded");
    }
};
let config = EventConfig::new(event, scope);
```

上面只摘录配置生成部分；其执行前提是事件类型和编号已校验。`exclude_user=0` 变成
「计入用户态」，内核态独立转换，不能从一个过滤位推断另一个过滤位；
两个都排除时直接以 `EINVAL` 拒绝，不构造事件配置。

这个抽象放在 OSTD，是因为它是上层事件管理和底层资源接口共同使用的配置契约。它不携带 pid、fd 或 Linux 属性长度，后端只需要理解要统计的事件及执行态。

### 3.4 `EventSpec`：补上线程事件的运行规则

“统计用户态周期数”还没有说明事件是否一创建就启用、是否限制 CPU。于是线程事件层在 `kernel/core/src/events/perf/context.rs` 定义：

``` rust
pub(crate) struct EventSpec {
    pub config: EventConfig,
    pub cpu: Option<CpuId>,
    pub is_initially_disabled: bool,
    pub should_enable_on_exec: bool,
}
```

| 字段             | 作用                                                             |
|------------------|------------------------------------------------------------------|
| `config`         | 传给底层的事件与执行态配置                                       |
| `cpu`            | `None` 表示允许跟随线程迁移；`Some(cpu)` 表示只在这颗 CPU 上统计 |
| `disabled`       | 决定事件创建时的逻辑启用状态                                     |
| `enable_on_exec` | 是否在目标成功 exec 后启用                                       |

`EventConfig` 负责计数内容，`EventSpec` 在其上增加事件管理规则。这样硬件接口不必承担“等到 exec 再启用”等线程策略。

`EventSpec` 也无需重复保存所有系统调用参数：`pid` 将被转换成目标上下文引用；`group_fd` 已被限制为 `-1`；调用参数 `flags` 里的 CLOEXEC 将交给文件表管理；`read_format` 将保存在事件文件中。每个参数都在对应层落实。

### 3.5 目标选择与上下文引用

内核接下来需要把配置交给实际的目标线程。系统调用中的选择代码为：

``` rust
    let (target, target_tid) = if pid == 0 {
        (ctx.thread.perf().clone(), ctx.posix_thread.tid())
    } else {
        let thread = pid_table::pid_table_mut().get_thread(pid as u32)
            .ok_or_else(|| Error::with_message(Errno::ESRCH, "the PMU target does not exist"))?;
        (thread.perf().clone(), pid as u32)
    };
```

`pid=0` 从当前调用上下文取线程；正数则按 TID 查目标线程。本版不支持 `pid=-1` 的全系统监测。

这里的 `target` 是 `Arc<PerfContext>`。`Arc` 表示共享同一个对象的引用；`clone()` 增加引用，不复制一份新的线程监测状态。线程与事件文件可以共同持有原上下文，目标退出后，文件仍能访问保留的最终结果。

另存的 `target_tid` 用于诊断展示。后续控制和读取通过上下文引用定位，不会每次拿 TID 重新查线程，因此不会因 TID 被复用而监测到另一线程。

至此，系统调用已经得到两项核心结果：**监测谁的 `target`，以及怎样监测的 `EventSpec`。** 下一步是在这个上下文中建立事件，再为用户创建操作入口。

## 第四章 线程事件模型与创建流程

### 4.1 线程级管理对象：`PerfContext` 与 `State`

`PerfContext` 是一个线程的全部 PMU 事件的管理入口，定义于 `kernel/core/src/events/perf/context.rs`：

``` rust
pub(crate) struct PerfContext {
    operation: Mutex<()>,
    state: SpinLock<State, LocalIrqDisabled>,
}
```

本次在原有 `Thread` 中增加 `perf: Arc<PerfContext>`，并在 `Thread::new()` 中初始化。接入位置是 `kernel/core/src/thread/mod.rs`，初始化表达式为：

``` rust
perf: Arc::new(crate::events::perf::PerfContext::new()),
```

上下文随线程建立，不必等到第一次 `perf_event_open` 才创建。这样，调度器、退出路径和事件文件都可以通过同一个入口管理该线程的事件。

上下文内部用 `State` 保存状态，同样定义于 `context.rs`：

``` rust
struct State {
    owner: Option<CpuId>,
    paused: bool,
    exited: bool,
    generations: [u64; 2],
    records: [Option<Record>; 2],
}
```

| 字段          | 含义                                                |
|---------------|-----------------------------------------------------|
| `owner`       | 目标线程当前的调度归属 CPU；没有运行归属时为 `None` |
| `paused`      | 用户控制或读取期间是否暂时冻结事件                  |
| `exited`      | 目标线程是否已经退出                                |
| `generations` | 每个事件槽位已经使用到哪一代                        |
| `records`     | 保存这个线程的逻辑事件记录                          |

初始状态是：

``` rust
owner: None, paused: false, exited: false, generations: [0; 2],
records: [None, None],
```

`records` 的类型是 `[Option<Record>; 2]`，即两个可空槽位组成的数组。`records` 是字段名，下一节的 `Record` 是每个非空槽位中保存的结构类型。初始化的 `[None, None]` 表示两条事件都还不存在，也没有为它们分配硬件计数器。

本版约定 cycles 使用逻辑槽位 0，instructions 使用逻辑槽位 1。因此一个线程可以同时保存两类事件，但不能再创建第二条同类事件。这是当前实现的容量限制。

### 4.2 `Ticket`：定位一条逻辑事件

一个上下文中可能有多个事件，文件对象需要知道自己控制的是哪一条。`context.rs` 为此定义事件凭据：

``` rust
pub(super) struct Ticket {
    pub slot: usize,
    pub generation: u64,
    pub id: u64,
}
```

`slot` 指出事件在哪个槽位；`generation` 区分这个槽位前后创建的不同事件；`id` 是对外返回的全局事件标识。

只保存槽位号会有一个问题：旧事件关闭后，同一槽位可能放入新事件。如果旧操作仍只凭槽位号查找，就可能碰到新记录。附带代数后，查找必须同时确认记录仍属于这一代。

本版的内部查找检查槽位、代数及关闭状态；`id` 用于 ID ioctl 和读数标识。因此，真正定位事件需要 **`PerfContext` 引用加上 `Ticket`**；`Ticket` 自己既不包含目标线程引用，也不包含 CPU 编号。

### 4.3 线程 `Record`：保存事件从创建到关闭的状态

逻辑事件记录的结构定义于 `kernel/core/src/events/perf/context.rs`：

``` rust
struct Record {
    ticket: Ticket,
    spec: EventSpec,
    is_enabled: bool,
    is_closed: bool,
    fault: Option<PmuError>,
    base_count: u64,
    live_count: u64,
    enabled_time: ActiveTime,
    base_running_ns: u64,
    live: Option<CounterHandle>,
    live_since_ns: u64,
    last_observed_ns: u64,
    width_bits: u8,
}
```

| 字段组                        | 保存的内容                                    | 为什么需要                                 |
|-------------------------------|-----------------------------------------------|--------------------------------------------|
| `ticket`、`spec`               | 身份与创建要求                                | 后续控制需要确认对象，调度需要知道计数条件 |
| `is_enabled`、`is_closed`、`fault`  | 当前启用、关闭和错误状态                      | 创建要求不足以表达运行后的状态变化         |
| `base_count`                   | 已结算运行片段的总计数                        | 线程切出后仍须保留结果，并跨多次运行累计   |
| `enabled_time`                 | 逻辑启用时间与计时起点                        | 区分事件被启用多久与真正占用硬件多久       |
| `base_running_ns`、`live_since_ns` | 实际计数时间及片段起点                        | 记录硬件真正参与计数的时间                 |
| `live`                        | 当前片段的 `CounterHandle`，未运行时为 `None` | 连接长期逻辑事件与临时 CPU 资源            |
| `last_observed_ns`、`width_bits` | 最近观测时刻和计数宽度                        | 检查观测间隔是否足以避免遗漏回绕           |

`spec.is_initially_disabled` 描述创建时的要求，`is_enabled` 描述当前状态。用户执行 ENABLE 后，改变的是当前状态，而不是重新生成一份请求。

`base_count` 保存已经结算到线程层的结果，`live_count` 保存当前运行片段已观测到的计数。片段结束时 `live_count` 并入 `base_count`；所以读取前还需要结算当前片段。`enabled_time` 是 `ActiveTime`（定义于 `observation.rs`），其 `update()` 返回 `None` 表示观测间隔过长、无法保证累计正确，此时记录 `Overflow` 故障。

### 4.4 创建记录并分配凭据

该方法定义于 `context.rs`，输入 `EventSpec`，返回 `Ticket`，并将新记录放入 `state.records[slot]`。

``` rust
    pub(super) fn create(&self, spec: EventSpec) -> Result<Ticket> {
        let id = NEXT_EVENT_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |cur| cur.checked_add(1))
            .map_err(|_| Error::with_message(Errno::ENOSPC, "PMU event ID space exhausted"))?;

        let ticket = self.with_frozen(|state, _now_ns| {
            if state.has_exited {
                return_errno_with_message!(Errno::ESRCH, "the target thread has exited");
            }
            let slot = spec.config.event().index();
            if state.records[slot].is_some() {
                return_errno_with_message!(
                    Errno::EBUSY,
                    "one event of each type per thread is supported"
                );
            }
            let generation = state.generations[slot]
                .checked_add(1)
                .ok_or_else(|| Error::with_message(Errno::ENOSPC, "PMU generation exhausted"))?;
            state.generations[slot] = generation;

            let is_enabled = !spec.is_initially_disabled;
            let ticket = Ticket { slot, generation, id };
            state.records[slot] = Some(Record {
                ticket,
                spec,
                is_enabled,
                is_closed: false,
                fault: None,
                base_count: 0,
                live_count: 0,
                enabled_time: ActiveTime::default(),
                base_running_ns: 0,
                live: None,
                live_since_ns: 0,
                last_observed_ns: 0,
                width_bits: DEFAULT_COUNTER_WIDTH_BITS,
            });
            Ok(ticket)
        })?;
```

创建过程先分配事件 ID，再在冻结状态下检查目标是否退出、事件槽位是否空闲。随后增加该槽位代数，生成 `Ticket`，并建立初始 `Record`。示例中的事件初始禁用，所以 `is_enabled=false`、`live=None`，累计值为零。

如果创建的是已启用事件，并且目标此时正在允许运行的 CPU 上执行，解除冻结时可能立即启动硬件。代码在创建后检查这种即时启动错误，失败则撤销新事件，不把失败包装成正常的零计数。

注意这不是将 `EventSpec` 的内存直接“变成”`Record`。它是读取创建要求，再生成一条额外包含状态、累计值和资源引用的新记录。

### 4.5 `PerfEventFile`：用户 fd 对应的文件对象

用户态后续只持有一个整数 fd，无法直接操作内核记录。为将原有文件接口接到事件管理上，本次在 `kernel/core/src/events/perf/file.rs` 定义：

``` rust
pub(crate) struct PerfEventFile {
    common: FileCommon,
    context: Arc<PerfContext>,
    ticket: Ticket,
    read_format: u64,
    target_tid: Tid,
}
```

| 字段          | 作用                         |
|---------------|------------------------------|
| `common`      | 接入原有文件框架的公共信息   |
| `context`     | 引用实际目标线程的监测上下文 |
| `ticket`      | 指向这个上下文中的某一条事件 |
| `read_format` | 保存读数的用户态返回格式     |
| `target_tid`  | 保留用于诊断展示的线程编号   |

构造函数也在 `file.rs`：

``` rust
    pub(crate) fn new(context: Arc<PerfContext>, target_tid: Tid, spec: EventSpec,
        read_format: u64) -> Result<Self> {
        // Allocate the path before attaching the event; no fallible work after
        // create, so a successful attachment always acquires an owning file.
        let path = AnonInodeFs::new_path(|_| "anon_inode:[perf_event]".to_string());
        let common = FileCommon::new(path, AccessMode::O_RDWR, StatusFlags::empty());
        let ticket = context.create(spec)?;
        Ok(Self { common, context, ticket, read_format, target_tid })
    }
```

它先准备匿名文件的公共信息，再调用 `context.create(spec)` 得到凭据，最后将上下文和凭据一起保存到文件对象。成功建立事件后，构造函数中不再安排可能失败的初始化工作，使事件能够立即取得对应的文件持有者。

因此，实际调用顺序是 `PerfEventFile::new()` 内部调用 `PerfContext::create()`，由后者产生 `Record` 和 `Ticket`。`PerfEventFile` 通过引用和凭据访问记录，两者不会合并成一个对象。

### 4.6 插入文件表，返回 fd

系统调用取得 `target` 和配置后，执行以下创建与返回代码：

``` rust
    let event_file = Arc::new(PerfEventFile::new(target, target_tid, EventSpec {
        config, cpu, disabled: attr.flags & ATTR_DISABLED != 0,
        enable_on_exec: attr.flags & ATTR_ENABLE_ON_EXEC != 0,
    }, attr.read_format)?);
    let fd_flags = if flags & PERF_FLAG_FD_CLOEXEC != 0 { FdFlags::CLOEXEC } else { FdFlags::empty() };
    let file_table = ctx.thread_local.borrow_file_table();
    let fd = file_table.unwrap().write().insert(event_file, fd_flags);
    Ok(SyscallReturn::Return(fd.into()))
```

这里 `fd_flags` 使用调用参数 `flags`，不是属性结构中的 `attr.flags`。例如 `PERF_FLAG_FD_CLOEXEC = 1 << 3`，其数值 8 来自 Linux 接口约定，表示 exec 时自动关闭该描述符，由原有文件表机制处理。

到这一步，示例中的长期关系已经建立：调用线程的文件表中有一个 fd，fd 引用 `PerfEventFile`；文件对象持有目标上下文和凭据；上下文的槽位中保存一条初始禁用的 cycles 记录。

从字段关系看，`EventConfig` 保存到 `EventSpec.config`，EventSpec 再保存到线程 `Record.spec`；PerfEventFile 则保存上下文引用和 Ticket。配置值按字段组合，文件对象通过引用访问记录。读者可以据此沿字段找到原请求，而不必将创建过程理解为几个对象依次替换彼此。

## 第五章 PMU 资源模型与架构适配

### 5.1 启用事件与启动硬件是两个时刻

用户通过 fd 执行 ENABLE 后，文件对象将它转换成内部 `Operation::Enable`，由 `PerfContext` 更新事件状态。事件启用后，只有目标线程正在合适的 CPU 上运行，才有条件启动硬件。

`PerfContext::sync()` 使用以下条件决定记录是否应该运行：

``` rust
let run = !state.paused && !state.exited && record.enabled && !record.closed
    && record.fault.is_none()
    && record.spec.cpu.is_none_or(|cpu| cpu == guard.current_cpu());
```

例如事件已启用，但线程正在睡眠，就先保留逻辑状态；线程在指定 CPU 之外运行时，也不应累计该事件。`State.owner` 表示实际运行归属，`EventSpec.cpu` 表示允许的计数 CPU，两者含义不同。

### 5.2 `PmuCaps`：先知道当前 CPU 能提供什么

每颗 CPU 初始化时，`kernel/core/src/events/perf/mod.rs` 调用 `ostd::pmu::init_on_cpu()`。OSTD 通过架构后端的 `probe()` 探测并缓存能力，相关抽象定义于 `ostd/src/pmu/mod.rs`：

``` rust
pub enum BackendKind {
    /// Intel architectural fixed counters 0 and 1.
    IntelFixed,
    /// AMD Zen core performance counters.
    AmdZen,
    /// RISC-V SBI PMU with a trusted privilege-filter contract.
    Sbi,
}
```

``` rust
pub struct PmuCaps {
    /// The register interface.
    pub backend: BackendKind,
    /// Managed resources; other hardware counters are left alone.
    pub counters: u8,
    /// Whether user/kernel filtering and firmware exclusion are supported.
    pub precise_filter: bool,
    pub(super) width: u8,
    pub(super) global_control: bool,
    pub(super) candidates: u64,
}
```

`backend` 标记使用哪类硬件接口；`counters` 是本模块管理的资源数；`width` 是计数器宽度；`precise_filter` 表示能否满足本版要求的执行态过滤；其他字段描述全局门控与候选资源。

本版探测 x86 CPU 厂商和相应能力，并尝试访问相关 MSR。成功与失败都会缓存。系统调用创建事件前查询这些缓存，确认目标可能使用的 CPU 支持所需功能；实际运行时仍可能因资源被占用而分配失败。

这样，“平台支持这个功能”和“此刻有一个空闲计数器”分别判断，不能在打开事件时仅凭能力探测承诺后续必然成功。

### 5.3 `CounterHandle`：代表当前 CPU 上的一次分配

当目标线程可以计数时，线程 `Record` 调用 OSTD 申请资源。OSTD 返回的硬件句柄定义于 `ostd/src/pmu/mod.rs`：

``` rust
pub struct CounterHandle {
    cpu: CpuId,
    slot: usize,
    generation: u64,
}
```

`cpu` 指出资源属于哪颗 CPU，`slot` 指出在该 CPU 软件资源表中的位置，`generation` 区分该位置前后两次分配。

它与 `Ticket` 分工不同：`Ticket` 定位线程中长期存在的逻辑事件；`CounterHandle` 定位 CPU 上本次分配的资源。线程从 CPU 0 迁移到 CPU 1 后，仍使用原事件凭据，但必须在 CPU 1 上申请新句柄。

句柄没有 TID 和累计总数，也没有 MSR 地址。线程归属由调用它的 `PerfContext` 保证，硬件信息则由下一节的 CPU 资源记录保存。

### 5.4 `LocalPmu` 和 OSTD 的 `Record`：管理 CPU 资源

OSTD 在每颗 CPU 上维护一套本地资源表。两个结构都定义于 `ostd/src/pmu/mod.rs`：

``` rust
struct LocalPmu {
    caps: Option<Result<PmuCaps>>,
    generations: [u64; 2],
    records: [Option<Record>; 2],
}
```

``` rust
struct Record {
    generation: u64,
    token: backend::Token,
    running: bool,
    retired: bool,
    fault: Option<PmuError>,
    total: u64,
    last: u64,
}
```

`LocalPmu` 保存当前 CPU 的能力和至多两条受管资源记录。这里的 `Record` 与第四章的线程事件 `Record` 同名，但在不同模块中。

| 对比项         | `kernel/.../perf/context.rs` 的 `Record` | `ostd/src/pmu/mod.rs` 的 `Record`           |
|----------------|------------------------------------------|---------------------------------------------|
| 归属           | 目标线程                                 | 某颗 CPU                                    |
| 典型寿命       | 事件创建到关闭                           | 一次硬件资源分配                            |
| 配置与硬件关系 | `spec` 保存请求，`live` 保存当前句柄     | `token` 保存真正的硬件资源信息              |
| 累计值         | `count` 跨线程多次运行累计               | `total` 累计本次分配，`last` 保存最近原始值 |
| 线程迁移后     | 原记录继续存在                           | 旧 CPU 记录被释放，新 CPU 重新建立记录      |

将二者分开，是为了允许一个线程事件跨越多次调度和多个 CPU，而不把某个 CPU 的寄存器永久绑定给这个线程。

### 5.5 从线程 `Record::start()` 进入 OSTD

线程层的启动方法定义于 `kernel/core/src/events/perf/context.rs`：

``` rust
    fn start(&mut self, now: u64, guard: &DisabledLocalIrqGuard) {
        if self.live.is_some() || self.fault.is_some() { return; }
        let result = (|| {
            let handle = pmu::configure(self.spec.config, guard)?;
            // Drop is a local, nonblocking release if either of these calls fails.
            let width = pmu::counter_width(&handle, guard)?;
            pmu::start(&handle, guard)?;
            self.width = width;
            self.live = Some(handle);
            self.live_since = now;
            self.last_observed = now;
            Ok(())
        })();
        if let Err(err) = result { self.fault = Some(err); }
    }
```

它把 `self.spec.config` 交给 `pmu::configure()`，得到句柄，再获取宽度并调用 `pmu::start()`。成功后，将句柄存进线程记录的 `live`，记录片段开始时间。失败时保存 `fault`。

`guard` 是关闭本地中断的保护凭据，提供当前 CPU 身份并约束这一段操作的执行环境。资源操作仍必须发生在其所属 CPU；远程操作需要通知所属 CPU 执行。

OSTD 的 `configure(config, guard)` 定义于 `ostd/src/pmu/mod.rs`。它依次检查配置与缓存能力、清理可回收资源、选择空闲逻辑槽位、更新分配代数，再调用 `backend::configure()`。后端返回 `Token` 后，OSTD 将它装入 CPU 记录，读取初始原始值作为 `last`，最后返回 `CounterHandle`。

这里先保存 Token 再执行可能失败的读取，是为了始终保留已配置资源的清理依据。配置过程中部分成功又失败时，通过同文件定义的 `ConfigFailure` 带回资源：

``` rust
struct ConfigFailure {
    error: PmuError,
    token: Option<backend::Token>,
}
```

有待回滚的资源就保留 `token` 并标记待清理；清理尚未成功时，不能把它当成空闲槽位重新分配。

### 5.6 `Token`：保存可以真正操作硬件的信息

架构后端将统一配置翻译成硬件信息。x86 的 `Token` 定义于 `ostd/src/pmu/x86.rs`：

``` rust
pub(super) struct Token {
    kind: BackendKind,
    index: u32,
    width: u8,
    control: u32,
    counter: u32,
    global: Option<u32>,
    global_bit: u64,
    // Restore only our own gate bit, preserving every unrelated counter.
    saved_gate: u64,
    config: u64,
}
```

| 字段                     | 用途                                     |
|--------------------------|------------------------------------------|
| `kind`、`index`、`width` | 确定后端类型、实际硬件资源编号和计数宽度 |
| `control`、`counter`     | 保存控制寄存器和计数寄存器的 MSR 编号    |
| `global`、`global_bit`   | 如有全局门控，记录其寄存器和本资源对应位 |
| `saved_gate`             | 释放时恢复本资源原有的门控状态           |
| `config`                 | 已编码的事件选择和执行态过滤             |

上层用句柄定位 CPU 记录，CPU 记录持有 Token，Token 才知道要访问哪个寄存器。这个分工将硬件细节留在后端。

相同的逻辑事件，在不同接口中的编号可以不同。按当前 x86 后端的映射：

| 事件         | Linux hardware `config` | 软件逻辑槽位 | Intel 固定计数器编号 | AMD 本版计数器编号 / 事件码 |
|--------------|-------------------------|--------------|----------------------|-----------------------------|
| cycles       | 0                       | 0            | 1                    | 0 / `0x76`                  |
| instructions | 1                       | 1            | 0                    | 1 / `0xc0`                  |

因此，线程 `records[0]` 表示 cycles，并不能推出“访问硬件计数器 0”。后端负责完成这次映射。

以 AMD 分支为例，后端按事件选择 MSR，并把用户态、内核态过滤编码进去。下列代码摘自 `backend::configure()`，省略外围分支和事务清理：

``` rust
let index = config.event.index() as u32;
let control = 0xc001_0200 + 2 * index;
let event = match config.event { Event::CpuCycles => 0x76, Event::Instructions => 0xc0 };
// Token 中保存：control、counter: control + 1，
// 以及 config: event | (user << 16) | (kernel << 17)。
```

这些地址与事件码是本版后端的具体适配内容；CPU 提供寄存器和计数电路，内核新增代码负责选择、配置、启停、读取和释放它们。

### 5.7 资源启动与硬件门控

OSTD 对外的启动函数位于 `ostd/src/pmu/mod.rs`：

``` rust
pub fn start(handle: &CounterHandle, irq_guard: &DisabledLocalIrqGuard) -> Result<()> {
    with_record(handle, irq_guard, |record| {
        if let Some(err) = record.fault {
            return Err(err);
        }
        if record.is_running {
            return Ok(());
        }

        // `Token::start` reads the baseline before opening the gates and hands
        // it back, so the baseline cannot race with the first counted event.
        match record.token.start() {
            Ok(initial_raw) => {
                record.last = initial_raw & accounting::mask(record.token.width_bits());
                record.is_running = true;
                Ok(())
            }
            Err(err) => {
                record.fault = Some(err);
                Err(err)
            }
        }
    })
}
```

`with_record()` 首先确认当前 CPU 与句柄一致，再检查槽位、分配代数和退役状态。有效后调用 `record.token.start()`；该方法先读取计数基准，再打开门控，并把基准一并返回，因此基准与首次计数之间不存在竞态。

x86 后端的 Token 方法如下，定义于 `ostd/src/pmu/x86_counter.rs`。`start` 与 `stop` 都返回 `Result<u64>`：前者交回启动前的计数基准，后者交回停止时读到的原始值。停止顺序是先关 local 门控再关 global 门控，且两者都会尝试、结果合并，因为一次失败的启动可能已经打开了 local 门控。释放时的多步清理和失败回滚不能用一条简单的早退语句概括，具体契约见 5.10：

``` rust
impl<I: Registers> Token<I> {
    pub(super) fn mask(&self) -> u64 { 1u64 << self.index }
    pub(super) fn width_bits(&self) -> u8 { self.width_bits }
    pub(super) fn read(&self) -> Result<u64> { I::read(self.counter_msr) }

    pub(super) fn start(&mut self) -> Result<u64> {
        let initial_raw = self.read()?;
        if self.kind == BackendKind::AmdZen {
            I::write(self.control_msr, self.config_bits | AMD_EVTSEL_ENABLE)?;
        }
        if let Some(global_msr) = self.global_msr {
            update_msr::<I>(global_msr, 0, self.global_bit)?;
        }
        Ok(initial_raw)
    }

    pub(super) fn stop(&mut self) -> Result<u64> {
        self.stop_gates()?;
        self.read()
    }

    // Attempt both gates even when the first write fails: a failed start may
    // already have enabled the local counter before the global write.
    fn stop_gates(&mut self) -> Result<()> {
        let local_result = if self.kind == BackendKind::AmdZen {
            I::write(self.control_msr, self.config_bits)
        } else {
            Ok(())
        };
        let global_result = if let Some(global_msr) = self.global_msr {
            update_msr::<I>(global_msr, self.global_bit, 0)
        } else {
            Ok(())
        };
        local_result.and(global_result)
    }

    // `release()` stops the event, clears only the state owned by this token,
    // and restores the prior gate state; its failure path must keep enough
    // state for cleanup or retry. See §5.10.
}
```

于是，启动链路可以明确到三个不同职责：线程 `Record::start()` 决定为本事件开始一个运行片段；OSTD `pmu::start()` 检查资源并维护软件状态；后端 `Token::start()` 修改硬件启用位和门控位。

启动后由硬件自动累计事件，内核不需要为每条指令执行一次软件加一。

### 5.8 MSR 访问与异常恢复

后端通过 `rd()`、`wr()` 封装访问 MSR。实际读写指令位于本次接入的 `ostd/src/pmu/x86_msr.S`；下面是读取部分：

``` asm
__pmu_read_msr:
    mov ecx, edi
.global __pmu_rdmsr_site
__pmu_rdmsr_site:
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [rsi], rax
    xor eax, eax
    ret
.global __pmu_rdmsr_failed
__pmu_rdmsr_failed:
    mov eax, 1
    ret
```

调用者传入 MSR 编号，汇编把编号放入 ECX，用 CPU 已提供的 `RDMSR` 指令读出 EDX:EAX，再组成 64 位值。这里新增的是内核访问封装及失败恢复，不是制造寄存器或实现 CPU 指令。

若已限定的 PMU 访问指令触发 \#GP 或 \#UD，`ostd/src/arch/x86/trap/mod.rs` 接入 `recover_register_fault()`，将返回位置改到失败入口，使 Rust 层得到 `HardwareFault`。恢复只覆盖明确标记的 PMU 读写位置，不能吞掉任意内核异常。

### 5.9 OSTD 资源接口的输入、结果与所有权

上层依赖下表中的统一接口，具体定义位置均为 `ostd/src/pmu/mod.rs`。表中“当前分配”指某个 CounterHandle 代表的一次 CPU 资源分配，和线程长期事件的累计值分开。

| 接口                            | 输入与前提                                  | 结果及状态变化                                                       |
|---------------------------------|---------------------------------------------|----------------------------------------------------------------------|
| `init_on_cpu()`                 | 在待初始化 CPU 上调用                       | 探测并缓存能力或错误                                                 |
| `capabilities(cpu)`             | CPU 编号                                    | 读取缓存，不由调用者远程访问该 CPU 的寄存器                          |
| `configure(config, guard)`      | 当前 CPU 的保护凭据、合法事件配置、可用资源 | 返回停止状态的句柄，建立初始基准；失败时保留需要清理的资源           |
| `start(&handle, guard)`         | 句柄有效，当前 CPU 与资源归属一致           | 借用句柄，启动或恢复当前分配                                         |
| `read(&handle, guard)`          | 同样满足 CPU 和身份校验                     | 更新并返回当前分配累计值，保留句柄及累计值                           |
| `stop(&handle, guard)`          | 同样满足 CPU 和身份校验                     | 停止硬件并返回当前分配累计值，句柄仍归调用方持有                     |
| `reset(&handle, guard)`         | 当前分配已停止且无错误                      | 清零这一分配的软件累计值并重建基准                                   |
| `release(handle, guard)`        | 按值交出句柄，在所属 CPU 上处理             | 尝试停止并归还资源，消耗调用方对句柄的所有权；未清理完的资源继续受管 |
| `counter_width(&handle, guard)` | 有效句柄及对应 CPU 的保护凭据               | 返回该资源的硬件计数宽度                                             |

接口采用句柄借用还是按值接收，明确了“继续使用这次分配”与“结束这次分配”的区别。它们均不负责查找 TID、解释 Linux `read_format` 或改变线程的逻辑启用状态。

### 5.10 停止、释放与失败回滚契约

停止和释放不是同一个动作。`stop()` 结束当前计数片段并结算其增量，但句柄仍代表这次分配；`release()` 则结束这次分配的所有权，并尝试把硬件资源恢复到可再次分配的状态。两者必须保持清晰的顺序。

正常路径应按以下顺序执行：

1.  在资源所属 CPU 上停止硬件计数，避免停止之后继续产生新的增量。
2.  读取最终原始计数，并按计数器位宽计算最后一段差值。
3.  更新 OSTD 本地记录的累计值和 `running` 状态，再由线程层把该片段的累计值加入逻辑事件总数。
4.  释放时清除本事件启用状态和本事件拥有的溢出/门控状态；只恢复自己修改过的控制位，不能覆盖其他计数器或固件管理的无关状态。
5.  确认硬件已停止且资源状态可追踪后，才将槽位标记为空闲。若清理未完成，保留 Token、代数和故障状态，等待所属 CPU 重试清理。

AMD Zen 后端需要特别注意事件选择 MSR 的 enable 位。停止操作必须确认该事件不再计数；释放还需清除本事件的控制配置，并按实现约定处理溢出状态。不能仅将软件中的 `running=false` 就视为硬件已经停止。若寄存器写入失败，资源记录不能直接丢弃。

RISC-V 后端的停止和释放通过其 SBI PMU/计数器接口完成。固件调用失败时，内核不能假设计数器已经停止，也不能立即把相应计数器编号重新分配。应保留可重试的资源状态，并将失败返回上层。对依赖特权级过滤的配置，还必须确认固件和硬件确实提供所请求的过滤语义；无法保证时应返回不支持，而不是退化为不受限计数。

故障处理的关键规则是：**尽力停止、保留清理凭据、避免重复结算、禁止过早复用资源，并向上层保留明确错误。** 如果 `stop()` 失败，`release()` 不应因为一次早期错误返回就遗失清理依据；实现需要继续执行可安全尝试的关闭步骤，或将资源标记为待清理。只有在资源状态确认安全后才能回收槽位。

这套契约需要通过可控故障场景验证：配置中途失败、启动失败、停止失败、读取最终计数失败、释放失败，以及清理重试。编译和 Clippy 可以检查类型与代码质量，但无法单独证明寄存器或 SBI 调用失败时的硬件状态。

### 5.11 三类后端的适配边界

| 维度       | x86-64 Intel                         | x86-64 AMD Zen                                        | RISC-V                                           |
|------------|--------------------------------------|-------------------------------------------------------|--------------------------------------------------|
| 事件来源   | 架构固定计数器                       | 核心性能监测计数器                                    | SBI PMU 管理的硬件计数器                         |
| 资源发现   | CPUID 与固定计数器能力               | CPU 厂商/型号与 PMU 能力                              | SBI PMU 能力查询及计数器信息                     |
| 配置与启停 | 固定计数器控制与全局门控             | 事件选择/计数器寄存器与 enable 位                     | SBI 配置、启动、停止接口及平台允许的计数器访问   |
| 过滤       | 依赖固定计数器支持的执行态过滤       | 依赖事件选择寄存器和硬件能力                          | 依赖 SBI/硬件所能保证的特权级过滤语义            |
| 主要风险   | 门控位恢复和其他计数器状态保护       | enable 位未清除、寄存器访问失败、释放时错误覆盖无关位 | 固件调用失败、计数器分配状态不一致、过滤能力不足 |
| 共用契约   | 统一的配置、句柄校验、累计和错误表达 | 统一的配置、句柄校验、累计和错误表达                  | 统一的配置、句柄校验、累计和错误表达             |

上层不应根据架构类型直接操作寄存器或 SBI 调用。架构差异集中在后端，OSTD 负责统一资源身份和生命周期，线程事件层负责逻辑累计与调度同步。这样既保留统一接口，也能针对每个后端单独测试停止、释放和错误恢复。

## 第六章 调度同步与累计规则

### 6.1 逻辑状态与硬件运行状态

线程事件用第四章的字段共同表达状态，没有另外定义一套状态枚举。下表描述常见情况；“启用但未运行”还可能同时带有冻结、CPU 不匹配等条件。

| 情况               | 线程记录中的体现                           | 硬件关系与允许的后续操作                             |
|--------------------|--------------------------------------------|------------------------------------------------------|
| 初始禁用           | `enabled=false`，无运行片段                | 可 ENABLE、RESET、读取或关闭                         |
| 已启用但目标未运行 | `enabled=true`，`live=None`                | 保留请求，等待调度到符合条件的 CPU                   |
| 正在计数           | 满足 sync 条件，`live=Some(handle)`        | 观测当前片段；切出或控制操作时停止结算               |
| 暂时冻结           | 上下文 `paused=true`，同步后各片段均已停止 | 在冻结状态下控制或制作快照，之后按条件恢复           |
| 目标已退出         | 上下文 `exited=true`，最终片段已结算       | 保留最终结果供读取，拒绝重新启用                     |
| 记录故障           | `fault=Some(error)`                        | 向上报告错误，不按正常事件继续启动；相关资源仍须清理 |
| 已请求关闭         | `closed=true`                              | 停止或清理尚在用资源，无在用片段后清除记录           |

因此 ENABLE 表达的是允许计数，`live` 表达的是当前确实有一次硬件分配。区分二者，才能解释线程睡眠期间为什么事件仍存在、启用时间和运行时间为什么可能不同。

### 6.2 调度路径的接入

如果只在打开事件时配置一次 PMU，随后一直读寄存器，线程切出后的其他程序也可能被累计。为把结果归到目标线程，本次在 `kernel/core/src/thread/mod.rs` 的原有调度回调中接入：

``` rust
// pre_schedule_handler：旧线程切出前
#[cfg(feature = "pmu")]
if let Some(thread) = task.as_thread() {
    thread.perf().on_schedule_out(irq_guard);
}

// post_schedule_handler：新线程切入后
#[cfg(feature = "pmu")]
if let Some(thread) = task.as_thread() {
    thread.perf().on_schedule_in();
}
```

线程切出时，`on_schedule_out()` 停止当前运行片段，把结果加到线程记录并释放 CPU 资源，然后清除 `owner`。线程切入时，`on_schedule_in()` 设置新的 CPU 归属，再检查是否需要开始计数。

调度框架仍由 Asterinas 原有机制提供；这里增加的是 PMU 状态同步调用。

### 6.3 停止、结算与释放的顺序

线程层的 `Record::stop()` 位于 `kernel/core/src/events/perf/context.rs`。它取出 `live` 句柄，先检查观测间隔，再停止硬件片段、取得该片段累计值、把 `live_count` 并入 `base_count`、累加 `base_running_ns`，最后归还句柄代表的资源。

其中 OSTD 停止接口的代码为：

``` rust
pub fn stop(handle: &CounterHandle, irq_guard: &DisabledLocalIrqGuard) -> Result<u64> {
    with_record(handle, irq_guard, |record| {
        if !record.is_running {
            return record.fault.map_or(Ok(record.total), Err);
        }
        // A software observation error must not skip the hardware stop.
        let result = record.token.stop().and_then(|raw| {
            record.is_running = false;
            if let Some(err) = record.fault {
                return Err(err);
            }
            accounting::accumulate(
                &mut record.total,
                &mut record.last,
                raw,
                record.token.width_bits(),
            )
            .ok_or(PmuError::Overflow)
        });
        if let Err(err) = result {
            record.fault.get_or_insert(err);
        }
        result
    })
}
```

它先调用后端停止方法取得原始值，再用 `accounting::accumulate()` 结算；即使软件已经记录了错误，也仍会尝试停止硬件。归还时 `pmu::release(handle, guard)` 按值接收句柄，结束本次分配。

例如线程先在 CPU 0 上运行，得到片段计数 1200；切出时将 1200 加到线程记录。后来迁到 CPU 1，重新申请计数器，得到片段计数 800。最终同一逻辑事件的累计值为 2000，原 Ticket 不变，两次 CounterHandle 不同。

这里的数字只用于解释软件累计关系，不是实验结果。

### 6.4 原始计数、差值与回绕

硬件计数寄存器可能原本不为零，也可能只有 40 或 48 位。本版通过基准与差值统计一次分配期间的增量，而不要求将计数寄存器统一写零。

差值累计定义于 `ostd/src/pmu/accounting.rs`：

``` rust
pub(super) const fn mask(width: u8) -> u64 {
    if width == 64 { u64::MAX } else { (1u64 << width) - 1 }
}
```

``` rust
pub(super) fn accumulate(total: &mut u64, last: &mut u64, raw: u64, width: u8) -> Option<u64> {
    let raw = raw & mask(width);
    let delta = raw.wrapping_sub(*last) & mask(width);
    *total = total.checked_add(delta)?;
    *last = raw;
    Some(*total)
}
```

例如 8 位计数器从 250 变为 5，按位宽计算得到增量 11。这个方法要求两次观测之间的真实增量小于一整圈；若已经绕过一圈或多圈，仅凭首尾值无法恢复完整增量。

因此，`kernel/core/src/events/perf/mod.rs` 注册 CPU 定时回调，对当前线程定期观测。这里的观测用于维护累计正确性，不是 perf 的执行地址或调用栈采样功能。

当前实现还使用“每纳秒最多 1024 次增量”的平台上界假设检查间隔。按此假设，40 位计数器的观测间隔需小于约 1.07 秒；间隔过长就报告 Overflow。该假设需要在目标平台验收，不能代替硬件与调度条件验证。

OSTD `total` 保存本次分配的累计值，线程层只在片段结束时将其加入 `count`。定期观测更新前者，不能把每次观测得到的片段累计值反复加到后者。

### 6.5 资源退役与异常清理

正常路径显式释放资源。`CounterHandle` 的 `Drop` 还提供兜底：标记对应资源记录为退役；如果就在所属 CPU 上，尝试立即清理，否则发送 IPI 请求所属 CPU 清理。

远程路径可以在锁保护下标记软件状态，实际寄存器操作仍在资源所属 CPU 上执行。清理失败的 Token 保持可追踪并等待重试，不能丢弃后将资源误报为空闲。

这使正常返回、配置部分失败和异常退出都围绕同一套资源归属规则处理。

### 6.6 CPU 迁移与远程同步

线程事件的归属是逻辑线程，而硬件资源始终属于某颗 CPU。线程迁移时必须结束旧 CPU 上的运行片段，再在新 CPU 上建立新的分配，不能把旧 `CounterHandle` 搬到新 CPU 使用。

典型迁移过程如下：

1.  旧 CPU 的调度切出回调停止事件，读取最后增量并将该片段结算到线程事件 `count`。
2.  旧 CPU 上的资源进入释放流程；`owner` 清空，原句柄不再作为活动分配使用。
3.  新 CPU 的切入回调设置新的 `owner`，根据事件是否启用、是否受 CPU 限制以及是否处于故障状态决定是否重新启动。
4.  新 CPU 分配新的句柄和后端 Token，但线程事件的 Ticket、ID 与累计总数保持不变。

如果 ENABLE、DISABLE、RESET 或 READ 由另一个 CPU 发起，调用方不能直接访问目标 CPU 的本地 PMU 状态。冻结流程需要通知资源所属 CPU 完成停止和结算，再在一致状态下执行控制或生成快照。IPI 回调不得持有会造成等待环的锁；等待远程确认期间也不能一直持有回调需要获取的状态锁。

迁移和远程同步验收要特别覆盖以下竞态：控制请求与切出同时发生、读取与定时观测同时发生、退出与释放同时发生，以及远程清理重试时目标 CPU 再次进入调度。验收重点不仅是最终计数，还包括没有跨 CPU 访问资源、没有重复累计、没有永久暂停事件和没有把待清理槽位重新分配。

## 第七章 事件控制与生命周期

### 7.1 控制入口与 `Operation`

用户执行 `ioctl(fd, command, 0)` 时，原有系统调用和文件表找到 `PerfEventFile`，再调用本次实现的文件方法。该方法将命令转换成内部操作：

``` rust
pub(super) enum Operation { Enable, Disable, Reset }
```

`Operation` 定义于 `context.rs`，文件层随后调用 `self.context.control(self.ticket, operation)`。同一个上下文引用和 Ticket 会一直指向创建时的事件。

| 操作    | 线程事件层的行为                           |
|---------|--------------------------------------------|
| ENABLE  | 修改逻辑启用状态，满足运行条件时启动硬件   |
| DISABLE | 先停止并结算当前片段，再设为禁用           |
| RESET   | 结算后清零事件 `count`，保留启用与运行时间 |
| ID      | 返回 `ticket.id`，不重新查目标线程         |

用户 RESET 的对象是长期事件计数；OSTD 另有对一次硬件分配的 `reset()` 接口，两者所在层次和作用范围不同。

### 7.2 `Snapshot`：给文件层一份已结算的结果

`read(fd, ...)` 同样先通过原有文件框架找到事件文件。文件对象调用 `PerfContext::read(ticket)`，得到定义于 `context.rs` 的快照：

``` rust
pub(super) struct Snapshot {
    pub count: u64,
    pub enabled_ns: u64,
    pub running_ns: u64,
}
```

`Snapshot` 只携带本次读数需要的值，不携带可操作的硬件资源。先形成快照，再退出 PMU 锁并写入用户缓冲区，可避免用户内存访问过程长期持有 PMU 锁或冻结目标线程。

返回布局由文件中的 `read_format` 决定：

| 条件       | 返回顺序中的字段              |
|------------|-------------------------------|
| 始终存在   | `count`，8 字节               |
| bit 0 设置 | 随后追加 `enabled_ns`，8 字节 |
| bit 1 设置 | 随后追加 `running_ns`，8 字节 |
| bit 2 设置 | 最后追加事件 `id`，8 字节     |

`read_format=0` 返回 8 字节计数；`read_format=7` 依次返回 count、enabled_ns、running_ns、id，共 32 字节。读取不会消耗累计值，也不会自动清零或按时间比例缩放。

### 7.3 冻结同步与并发边界

若调用者读取时目标正在其他 CPU 上计数，调用者不能直接在自己的 CPU 上读取目标的 MSR。同时，线程记录里的 `count` 还可能未包含正在运行的片段。

为此，`context.rs` 的 `with_frozen()` 将操作分成以下步骤：

1.  获取 `operation` 互斥锁，串行化需要冻结的控制和读取。
2.  在状态锁内设置 `paused=true`，记录目标的 CPU 归属，然后释放状态锁。
3.  通知所属 CPU 同步，停止并结算运行片段；没有运行归属时无需访问远程硬件。
4.  在状态锁内执行控制或生成 Snapshot。
5.  清除暂停标记，再通知同步，根据当前状态恢复应运行的事件。

操作即使失败也必须执行恢复步骤，避免一次失败让其他事件一直暂停。

`PerfContext` 使用两种锁，是因为用户控制路径可以串行等待，调度、定时和 IPI 回调则不能依赖可睡眠的互斥锁。锁顺序是 operation Mutex、上下文 state 锁、OSTD 本地 PMU 锁；等待 IPI 时释放 state 锁，回调不获取 operation Mutex。

一次读取的结果边界，是这次冻结同步完成并结算片段后的状态。冻结解除后事件可能继续增长，用户得到的是该时刻的快照。计数边界按实际启停与调度挂钩位置定义；包含内核态时，还需要在验收中确认这些位置所包含的内核执行开销。

### 7.4 线程退出、exec 与关闭 fd

线程退出时，`Thread::exit()` 接入 `perf.on_exit()`：停止最后一个片段、结束时间累计并标记退出。只要文件仍引用上下文，最终记录继续存在，父线程可以等目标退出后再读数。

成功 exec 的路径在 `kernel/core/src/process/execve.rs` 调用 `on_exec()`，启用设置了 `enable_on_exec` 的事件。它沿用原上下文。若包含内核态，计数起点按该挂钩位置解释，不能说成只统计新程序进入用户态之后的执行。

关闭 fd 时先移除原有文件表中的引用；最后一个文件对象引用销毁后执行：

``` rust
impl Drop for PerfEventFile {
    fn drop(&mut self) { self.context.close(self.ticket); }
}
```

此时上下文将对应记录标记关闭，无在用句柄时可以立即清除槽位；仍有硬件片段则通知所属 CPU 清理。`dup` 等产生的额外引用可能延后对象销毁，所以一次 `close(fd)` 不一定立即销毁事件。

### 7.5 错误如何向上传递

资源层在 `ostd/src/pmu/mod.rs` 定义 `PmuError`，使硬件失败不会被当作普通计数结果：

``` rust
pub enum PmuError {
    /// No supported PMU is exposed.
    Unsupported,
    /// The platform cannot guarantee the requested filter.
    UnsupportedFilter,
    /// Neither user nor kernel execution was requested.
    InvalidConfig,
    /// No managed resource is free for the event.
    NoCounter,
    /// A handle was used from another CPU.
    WrongCpu,
    /// The allocation has ended or the state is invalid.
    InvalidHandle,
    /// A register access or firmware operation failed.
    HardwareFault,
    /// A logical total overflowed or the observation interval was too long.
    Overflow,
}
```

线程层把它转换成内核 Errno，例如资源不足对应 EBUSY，不支持的功能或过滤对应 EOPNOTSUPP，累计或观测溢出对应 EOVERFLOW。硬件或无效资源操作等错误通常转换为 EIO。

输入方面，本版检查属性布局、支持的事件和选项、目标存在性、CPU 编号和能力，并要求调用者具有 CAP_PERFMON 或 CAP_SYS_ADMIN。不支持的请求应明确失败，不能悄悄按另一种含义计数。

### 7.6 失败路径、状态保留与重试

故障路径的处理目标不是保证所有硬件操作都成功，而是保证失败之后软件状态仍然真实、资源仍可追踪，并且错误能够传回用户态。不同阶段的处理重点如下：

| 失败位置                                 | 必须保留的信息                   | 后续处理原则                                                    |
|------------------------------------------|----------------------------------|-----------------------------------------------------------------|
| 能力探测失败                             | 缓存的探测错误                   | 创建或启动时返回不支持/硬件错误，不把能力未知当成支持           |
| `configure` 尚未取得硬件资源             | 事件配置和错误原因               | 不创建有效运行片段；可安全回滚后释放逻辑记录                    |
| `configure` 已取得 Token，但初始读取失败 | Token、CPU、槽位、代数和清理状态 | 不丢弃 Token；尝试关闭硬件，清理失败则保留待清理记录            |
| `start` 写寄存器或调用 SBI 失败          | 当前分配的身份与故障状态         | 不能仅凭软件标志认定硬件未启动；执行安全停止/回滚并禁止正常累计 |
| `stop` 失败                              | Token 和原始错误                 | 继续尝试可安全执行的停止步骤；不重复结算，不提前释放槽位        |
| `release` 失败                           | 退役状态、Token 和待清理标志     | 不将资源报告为空闲；由所属 CPU 的清理路径重试                   |
| 线程退出或文件关闭时发生失败             | 最终事件记录和资源清理依据       | 逻辑事件可标记关闭，但物理资源清理完成前仍须保持可追踪          |
| 观测间隔过长或累计溢出                   | 已结算值、最后观测值和故障状态   | 返回溢出错误，不用截断值伪装成有效结果                          |

这些规则要求线程层和 OSTD 层分别维护自己的状态。线程 `Record.fault` 说明逻辑事件不能再正常运行；OSTD 资源记录的 `fault/retired` 则说明硬件分配可能需要清理。两者不能互相替代：逻辑事件发生错误，并不意味着底层资源已经释放。

恢复操作还要避免重复结算。一次片段的最终增量只能加入线程 `count` 一次；清理重试只能处理资源状态，不能再次把同一片段的累计值加到事件总数。对已经关闭的事件，重试清理也不能使旧 Ticket 重新指向槽位中新建的事件。

## 第八章 设计取舍与集成验证

### 8.1 为什么采用这些抽象

各个类型的引入对应不同的状态归属和生命周期。下面集中列出设计选择及其替代方案，便于评审讨论。

本设计将配置、凭据和资源身份保留为字段中的值类型，只有需要共同持有上下文或文件对象时才使用共享引用。引入一个类型的依据，是它承担了独立的接口约束、状态或生命周期。本文不为尚未实现的采样和事件组提前增加对应对象。

| 设计选择                                | 要满足的需求                                           | 替代做法及其问题                                                           |
|-----------------------------------------|--------------------------------------------------------|----------------------------------------------------------------------------|
| Linux ABI 结构与内部配置分开            | 接收稳定的用户字节布局，让内部接口只处理支持的计数内容 | 后端直接接收整份 Linux 属性，会将 ABI 版本和大量无关字段带入硬件层         |
| `EventConfig` 与 `EventSpec` 分开       | 复用硬件计数配置，将线程运行规则留在线程层             | 在硬件接口中处理 exec、目标选择等策略，会扩大 OSTD 职责                    |
| 每线程一个 `PerfContext`                | 让计数与调度、迁移、退出对应                           | 只在进程中保存一份状态，无法直接区分多个线程同时在不同 CPU 上运行          |
| 文件持有 `Arc<PerfContext>` 和 `Ticket` | 长期指向原事件，目标退出后仍能读取                     | 每次按 TID 查目标，会遇到退出和编号复用问题                                |
| 凭据带代数                              | 槽位复用后拒绝旧引用                                   | 只保存数组下标，无法区分前后两次创建或分配                                 |
| 线程记录与 CPU 资源记录分开             | 同一事件跨多个运行片段和 CPU 累计                      | 事件永久绑定一个硬件计数器，会使迁移、资源共享和释放难以协调               |
| 文件接口承载控制和读数                  | 复用 fd 的查找、引用和关闭机制                         | 为启停、读数、关闭分别增加专用系统调用，会重复对象管理                     |
| 冻结后生成快照                          | 读取仍在运行的目标时结算一致结果                       | 仅返回线程 `count` 会遗漏当前片段；在调用者 CPU 直接读目标寄存器则归属错误 |
| 编译期选择后端                          | 复用上层接口并隔离架构细节                             | 在每个上层方法中散布 Intel、AMD、SBI 分支，会重复资源和状态逻辑            |

本版用固定两个槽位，降低简单计数的管理复杂度，代价是同类事件不能重复创建，也不支持计数器分时复用。未来扩展这些功能时，需要重新设计事件容器、资源仲裁和时间统计，不能仅增加一个事件枚举值。

### 8.2 本次新增与原有能力的边界

| 范围           | 复用的基础能力                                        | 本次实现或接入的内容                                                   |
|----------------|-------------------------------------------------------|------------------------------------------------------------------------|
| 用户态进入内核 | 系统调用入口、用户寄存器上下文、通用分发宏            | perf_event_open 分发表接入、参数解释和请求校验                         |
| 用户内存访问   | 用户地址空间的受控读写接口                            | perf 属性布局、复制流程和支持范围检查                                  |
| 文件框架       | 文件表、匿名文件基础、read/ioctl/close 分派、引用管理 | PerfEventFile 及其事件控制、读数和销毁行为                             |
| 线程与调度     | Thread、切换回调、退出和 exec 框架                    | 每线程 PerfContext、事件状态与累计、相应路径中的同步调用               |
| OSTD           | CPU 本地存储、中断保护、锁、定时与 IPI 基础机制       | PMU 能力与资源表、CounterHandle、差值累计、清理及错误接口              |
| x86 平台       | CPU 的 PMU 电路、CPUID 与 MSR 指令、原有异常框架      | Intel/AMD PMU 探测、事件映射、寄存器配置、访问封装和限定位置的异常恢复 |
| RISC-V 平台    | SBI PMU 固件接口与平台提供的计数器能力                | SBI PMU 探测、计数器配置、启停读取、释放与错误转换                     |

`kernel/` 和 `ostd/` 是代码职责边界。内核层负责系统调用、线程事件和文件对象；OSTD 层负责 CPU 本地资源、架构能力探测和硬件操作接口；架构后端负责把统一配置映射到具体寄存器或固件调用。底层 PMU 电路由处理器提供，内核和 OSTD 负责安全地配置、启停、读取及释放它。

### 8.3 当前缺口与本次不覆盖的功能

x86 后端通过 `probe()` 限定支持的厂商、处理器能力和运行环境。Intel 分支使用架构固定计数器，AMD 分支使用受支持的 Zen PMU 资源；虚拟机是否暴露相应 PMU 也会影响运行。RISC-V 后端通过 SBI PMU 与相应计数器接口完成能力探测、配置、启停、读取和释放。三类后端都应遵守统一的资源契约，但不能假设它们的硬件控制方式相同。

后端通过 `ostd/src/pmu/mod.rs` 的统一接口和条件编译选择，上层使用共同的 `probe/configure`、句柄和计数接口。平台验收需要分别确认事件编码、用户态/内核态过滤、计数宽度、启动与停止语义、资源释放，以及访问失败后的恢复行为。

| 扩展或验收项                      | 为什么需要                                     | 当前状态与后续落点                                                                              |
|-----------------------------------|------------------------------------------------|-------------------------------------------------------------------------------------------------|
| 标准 Linux `perf stat` 端到端运行 | 验证现有内核接口能否被标准用户态工具实际使用   | 需要将匹配架构的 `perf` 放入客体环境，核对依赖、事件选择和实际 ABI 请求，并执行真实工作负载     |
| 客体中的生命周期与故障注入        | 证明调度、迁移、退出和错误路径在目标系统中正确 | 需验证停止、释放、回滚、重试、目标退出后读数以及 CPU 迁移；仅编译和主机单元测试不能覆盖这些行为 |
| 定量性能报告                      | 记录计数值与重复测量结果，便于复核             | 需要填入实际运行数据、CPU/虚拟化条件、命令、退出码和原始日志；不能用空模板代替测量              |
| 执行采样                          | 将事件开销定位到地址或调用栈                   | 需要溢出通知、样本格式、缓冲区、映射与读取策略；定时维护计数差值不提供这些能力                  |
| 事件组及分时复用                  | 协调多个事件，处理硬件资源不足                 | 固定两槽位不足以承载，需要事件容器、组调度、资源仲裁和相应返回语义                              |
| 任务继承                          | 自动监测派生线程或子进程                       | 需要在线程创建路径设计事件复制、关联、累计和关闭关系                                            |

当前实现聚焦简单线程硬件事件计数。上述扩展功能不属于本次已完成范围；同样，编译和静态检查通过不等于端到端兼容性或所有故障路径均已验证。

### 8.4 如何验收这条链路

验收应同时检查 ABI、归属和资源生命周期。下表列出需要保留的检查证据；实际执行结果应以对应架构的构建日志、客体测试日志和测量数据为准。

| 方面       | 检查内容                                                                         |
|------------|----------------------------------------------------------------------------------|
| 接口与布局 | 两类事件、执行态过滤、属性长度、读格式与缓冲区长度；不支持参数的错误返回         |
| 基础计数   | 工作负载下得到合理非零结果；读取不清零；禁用后不再累计；RESET 清零计数并保留时间 |
| 线程归属   | 目标睡眠或切出后，其他线程执行不应计入目标；指定 CPU 条件生效                    |
| 迁移与累计 | 多次切入切出及跨 CPU 迁移后仍累计到同一逻辑事件                                  |
| 对象寿命   | 目标退出后可读最终结果；最后引用释放后关闭事件；槽位复用拒绝旧代数               |
| 资源和错误 | 同类事件冲突、资源占用、硬件访问失败、过长观测间隔及清理重试                     |
| 平台验证   | AMD、Intel 与 RISC-V 分别记录环境与结果；虚拟机测试记录 PMU/SBI 暴露条件         |

当前验证记录见本节末尾的「当前自动化检查结果」：`tools/pmu/check.sh` 的各子命令均在
WSL 中通过，但那是本项目的本地记录，不是上游 CI 的结果，也不替代客体内的硬件计数与故障路径实测。

x86-64 侧已确认 `perf` 7.1.4 能在 initramfs 的 rootfs 内启动（在宿主上 `chroot` 到该
initramfs 后执行 `/bin/perf --version` 返回正常）。该二进制是动态链接的，因此 initramfs
构建时会把包自身的 store 路径一并装入客人可见的 `/nix/store`，否则 `bin/perf` 这个
wrapper 会指向缺失的文件。

仍需补齐的端到端证据包括：标准 Linux `perf` 是否能在两种架构的客体中真正计数
cycles/instructions、启停/RESET/关闭与读取语义、CPU 迁移和目标退出后的最终结果，
以及 AMD 与 RISC-V 后端在停止或释放失败时的回滚行为。定量报告模板中的空白测量项应在真实测试完成后填写，不能以模板或编译通过记录代替。

#### 较早自动化检查记录

下表是提交前的本地记录：x86-64 宿主机上的 WSL2 + Docker（nix 工具链），
被测代码为本次提交的父版本。它说明构建与静态检查通过，但**不是可复核的 CI 记录**——
本次没有接入上游 CI，GitHub 上也不会出现对应检查结果。要复核请按本节末尾的命令重跑，
并把输出绑定到具体提交 SHA。

| 检查项                     | 当前记录                              | 结果能证明什么                             |
|----------------------------|---------------------------------------|--------------------------------------------|
| 环境检查                   | 通过                                  | 检查脚本所需工具链与环境可用               |
| 主机测试（`host`）         | 14 项 runner 夹具、35 项 Rust 测试通过 | 被覆盖的主机侧逻辑和测试辅助流程符合预期   |
| SCML（`scml`）             | 通过                                  | 相关文档语法检查通过                       |
| rustfmt（`format`）        | 通过                                  | Rust 代码格式检查通过                      |
| C format                   | 通过                                  | C 代码格式检查通过                         |
| x86-64 PMU 构建（`x86`）   | 通过                                  | 该配置下内核可完成编译                     |
| RISC-V PMU 构建（`riscv`） | 通过                                  | 该配置下内核可完成编译                     |
| 默认构建（`default`）      | 通过                                  | PMU 关闭时默认构建未被该功能破坏           |
| x86-64 Clippy（`lint`）    | 通过                                  | 对应配置的 Clippy 检查通过                 |
| RISC-V Clippy（`lint`）    | 通过                                  | 对应配置的 Clippy 检查通过                 |
| `make check`（含 typos）   | 通过                                  | 全仓库检查通过                             |

复现方式：在仓库根目录依次运行

``` sh
tools/pmu/check.sh host    # 主机侧事务与故障注入测试
tools/pmu/check.sh lint    # 双架构 Clippy + 关闭 PMU 的构建
tools/pmu/check.sh x86     # x86-64 PMU 构建
tools/pmu/check.sh riscv   # RISC-V PMU 构建
tools/pmu/check.sh default # 关闭 PMU 的默认构建
tools/pmu/check.sh scml    # SCML 语法校验
tools/pmu/check.sh format  # 仅格式化本次涉及的 Rust 文件
make check                 # 全仓库格式、Clippy、typos 检查
```

`tools/pmu/check.sh` 是本项目新增的检查入口，不是上游 Asterinas 的脚本；
早期交付包中的 `run_checks.sh` 只是把上述命令串起来的汇总脚本，两者等价。
仓库内的入口以 `tools/pmu/check.sh` 为准。

以上结果不应被扩大解释为真实硬件计数、标准 `perf` 工具运行、跨 CPU 迁移或硬件故障回滚已经通过。
客体验收尚未完成：两架构的成功十轮 `perf stat` 输出、目标线程归属、CPU 迁移和停止/释放失败回滚
都还需要在带 PMU 的 KVM 或真实硬件上取得，并按「提交 SHA + 架构 + 镜像/工具链/固件版本 + 完整命令 +
退出码 + 原始日志」的形式保存。

### 8.5 分阶段集成与验证

集成可以沿依赖关系逐步展开，每阶段先验证本层契约，再进入上层。下表说明推荐的集成顺序，不新增本文之外的完成声明。

| 阶段             | 接入内容                                                      | 进入下一阶段前确认                                         |
|------------------|---------------------------------------------------------------|------------------------------------------------------------|
| 硬件与资源接口   | 每 CPU 探测、x86 配置与启停读取、句柄校验和释放               | 支持范围明确，错误可报告，资源能够安全归还                 |
| 线程事件模型     | PerfContext、Record、Ticket，以及调度、定时、退出和 exec 接入 | 片段归属正确，切换与迁移不破坏累计，目标退出后状态完整     |
| 文件与系统调用   | PerfEventFile、属性解析、权限检查、分发表、fd 和读格式        | ABI 输入输出及失败路径明确，控制和文件寿命正确             |
| 端到端与平台验收 | 用户态工作负载、跨线程控制、回归用例和各平台记录              | 结果与工作负载对应；测试结论标明代码版本、CPU 和虚拟化条件 |

接口应补充 rustdoc，记录参数前提、返回值、CPU 归属和资源转移要求；面向实现的代码注释则解释容易被破坏的约束，例如冻结失败也须恢复、先登记 Token 再执行可失败读取，以及片段累计只能结算一次。

## 附录 A 一次实际监测的调用关系

前文使用当前线程的手动启停例子。现有 `pmu_stat` 测试使用父线程监测子线程的方式：先 fork 并让子线程等待，父线程对 child 创建初始禁用、设置 `enable_on_exec` 的事件，然后放行子线程。

创建调用位置为 `test/initramfs/src/regression/io/pmu/pmu_stat.c`：

``` c
events[i].fd = pmu_open(events[i].code, events[i].scope, child, 1);
```

这里 child 对应子进程的初始线程，不是 `pid=0`。目标成功 exec 时启用；运行中随调度结算；退出时保留最终记录；父线程随后读取并关闭 fd。

| 阶段     | 主要调用关系                                                          | 保留下来的对象或结果                                |
|----------|-----------------------------------------------------------------------|-----------------------------------------------------|
| 创建     | `sys_perf_event_open` → `PerfEventFile::new` → `PerfContext::create`  | 子线程上下文中的 Record/Ticket；父线程文件表中的 fd |
| 启用     | ENABLE 或 `on_exec` → 事件状态更新 → `sync`                           | 逻辑事件变为 enabled                                |
| 开始片段 | 线程 `Record::start` → `pmu::configure/start` → 后端配置与 Token 启动 | 线程 live 中的句柄；CPU 资源表中的 Token            |
| 结束片段 | 线程 `Record::stop` → `pmu::stop/release` → 后端停止与清理            | 片段增量加入线程 count；资源归还                    |
| 读取     | 文件 `read` → `PerfContext::read` → 冻结结算 → Snapshot               | 用户缓冲区中的计数与可选时间、ID                    |
| 关闭     | 最后文件引用销毁 → `PerfEventFile::drop` → `context.close`            | 逻辑记录关闭，尚在用的硬件资源完成清理              |

## 附录 B 源码位置索引

以下路径均相对于仓库根目录。源码摘录用于说明对象职责和调用关系；标为示意或省略部分的代码不能直接替换完整实现。具体行为以对应文件中的实现为准，运行结论则以各架构的构建日志、客体测试日志和定量测量记录为准。

| 文件                                              | 定义或接入的内容                                                                                                  |
|---------------------------------------------------|-------------------------------------------------------------------------------------------------------------------|
| `kernel/core/src/syscall/mod.rs`                  | 原有参数提取、处理函数分发                                                                                        |
| `kernel/core/src/syscall/arch/x86.rs`             | x86-64 系统调用 298 的接入                                                                                        |
| `kernel/core/src/syscall/arch/generic.rs`         | 通用架构系统调用 241 的分发表位置                                                                                 |
| `kernel/core/src/syscall/perf_event_open.rs`      | AttrHeader、PerfEventAttrV0、copy_attr、系统调用处理与返回 fd                                                     |
| `kernel/core/src/events/perf/context.rs`          | EventSpec、Ticket、PerfContext、State、线程 Record、Operation、Snapshot，以及创建、控制、读取、调度和退出方法     |
| `kernel/core/src/events/perf/file.rs`             | PerfEventFile、构造、read、ioctl 和 Drop                                                                          |
| `kernel/core/src/events/perf/mod.rs`              | 每 CPU 初始化、定时与通知同步辅助、错误转换                                                                       |
| `kernel/core/src/thread/mod.rs`                   | perf 字段、线程创建、调度和退出接入                                                                               |
| `kernel/core/src/process/execve.rs`               | 成功 exec 的 on_exec 接入                                                                                         |
| `kernel/core/src/init.rs`                         | 每 CPU PMU 初始化的接入位置                                                                                       |
| `ostd/src/pmu/mod.rs`                             | Event、EventConfig、BackendKind、PmuCaps、CounterHandle、LocalPmu、CPU Record、ConfigFailure、PmuError 和资源接口 |
| `ostd/src/pmu/accounting.rs`                      | mask 与 accumulate，处理位宽和差值累计                                                                            |
| `ostd/src/pmu/x86.rs`                             | x86 probe/configure、Token、MSR 封装与异常恢复判断                                                                |
| `ostd/src/pmu/x86_msr.S`                          | MSR 读写指令及失败返回入口                                                                                        |
| `ostd/src/pmu/riscv.rs`                           | RISC-V SBI PMU 能力探测、计数器配置、启停、读取与释放适配                                                         |
| `ostd/src/arch/x86/trap/mod.rs`                   | 限定 PMU 访问位置的异常恢复接入                                                                                   |
| `test/initramfs/src/regression/io/pmu/pmu_stat.c` | 用户态父线程监测子线程的测试入口                                                                                  |

## 参考资料

\[1\] [perf(1)：Linux 性能分析工具总览](https://man7.org/linux/man-pages/man1/perf.1.html)。用于区分用户工具、内核事件机制及主要分析能力。

\[2\] [perf_event_open(2)：性能事件系统调用](https://man7.org/linux/man-pages/man2/perf_event_open.2.html)。用于对照参数、计数与采样、属性布局和事件文件接口。

\[3\] [perf-list(1)：事件种类与过滤条件](https://man7.org/linux/man-pages/man1/perf-list.1.html)。用于说明事件选择、原始硬件事件和执行态修饰符。

\[4\] [perf-record(1)](https://man7.org/linux/man-pages/man1/perf-record.1.html) 与 [perf-stat(1)](https://man7.org/linux/man-pages/man1/perf-stat.1.html)。用于区分执行采样与累计统计。

\[5\] [Linux 内核：Perf events and tool security](https://www.kernel.org/doc/html/latest/admin-guide/perf-security.html)。用于对照 Linux 的性能事件权限背景；本文实现采用其自身受限策略。

\[6\] [perf-report(1)](https://man7.org/linux/man-pages/man1/perf-report.1.html)、[perf-sched(1)](https://man7.org/linux/man-pages/man1/perf-sched.1.html)、[perf-lock(1)](https://man7.org/linux/man-pages/man1/perf-lock.1.html)、[perf-c2c(1)](https://man7.org/linux/man-pages/man1/perf-c2c.1.html)。用于说明热点、调度、锁与缓存行共享分析。

文档组织与设计论证参考以下两篇 Asterinas 设计提案。它们用于指导本文的问题陈述、对象关系、生命周期、取舍与集成安排；PMU 类型和函数以实际实现代码为依据。

\[7\] [Introduce a device registration framework，Issue \#3743](https://github.com/asterinas/asterinas/issues/3743)。参考其先介绍 Linux 背景与范围，再解释对象关系、不同生命周期、定义位置和分阶段集成的组织方式。

\[8\] [Introducing a Structural Reimplementation Design of overlayfs，Issue \#3760](https://github.com/asterinas/asterinas/issues/3760)。参考其以核心对象说明状态和引用、逐项解释字段职责、讨论替代方案，并单列缺口与非目标的写法。

## 2026-10-28 审查修订补充

跨进程事件现在额外经过 `READ_WITH_REAL_CREDS` 的 alien-access LSM 检查。
此策略比 Linux 的 CAP_PERFMON 豁免更严格，不能只凭监测能力越过目标访问策略。
无软件事件时跳过定时线程采样，硬件延迟清理使用独立的 CPU 本地提示。
释放失败按指数间隔重试，累计失败 8 次后保留 token 并隔离该 CPU 的 PMU，
禁止将释放未确认的槽位重新分配；该 CPU 的 PMU 在本次启动中不自动恢复。

修订后通过 40 项 Rust 主机测试、14 项 runner 夹具、x86-64 PMU 开启与关闭的完整构建、
PMU Clippy，以及 AMD KVM 客体中的权限边界、计数、并发和十轮标准 perf。
套件仍为 `failed=0 skipped=1`，实际跨 CPU 迁移尚未验证。
详细变更、重试边界、故障注入与硬件验证的区别见
[本轮修订与验证记录](VALIDATION_20261028.md)。

## 2026-10-30 RISC-V 客体验证补充

已在 QEMU 10.2.1 / OpenSBI 1.7 中启动 RISC-V 星绽内核，完成 SBI PMU
计数、权限、并发和标准 perf 6.18.7 十轮验证。后端初始化现在主动清除
`scounteren` 的 cycle/instret/HPM 用户态访问权限，并回读确认；保留 time 的原有权限。
新增回归确认活跃 perf 事件下，用户态直接读取这些 PMU CSR 会收到 `SIGILL`。

本轮为 TCG 模拟客体功能验证，不能替代 RISC-V 实体 PMU 性能实测。
套件仍有实际跨核迁移一项跳过；标准 Nix 全回归 initramfs 路线本轮未构建成功，
使用的是缓存工具链和运行库制作的最小测试 initramfs。
环境、源码变化、版本兼容性和原始证据见
[RISC-V 验证记录](RISCV_VALIDATION_20261030.md)。
