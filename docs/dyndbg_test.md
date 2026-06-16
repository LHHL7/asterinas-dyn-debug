# Asterinas Dynamic Debug System 测试文档

## 1. 文档目标

本文档为 Asterinas Dynamic Debug System 的**最终测试报告**，覆盖以下验证维度：

1. 功能正确性（F 系列）
2. 增量重算正确性（I-01）与索引消融（I-02）
3. 性能基准 —— 微基准 + 真实 workload（P 系列，含 P-01R）
4. Batch Patch Transaction 效果（P-02）
5. SMP 并发稳定性（C 系列）
6. 系统边界与已知限制

本文档同时作为：

- 测试执行指南（编译命令 + 测试命令）
- Benchmark 执行规范（参数约束 + 判定标准）

结构化测试数据存放于 `results/` 目录，详见第 9 节。

---

## 2. 测试环境

### 2.1 硬件环境

| 项目 | 内容 |
| --- | --- |
| 设备 | 远程服务器（Docker 容器 `asterinas-dev`） |
| CPU | Intel(R) Core(TM) i9-7900X @ 3.30GHz（10 核 20 线程，1 Socket） |
| 内存 | 46 GiB（Docker 可用 46.7 GiB） |
| 存储 | 222 GB NVMe SSD |
| 系统 | Ubuntu 24.04.4 LTS，kernel 6.14.0-27-generic |

### 2.2 虚拟化环境

| 项目 | 内容 |
| --- | --- |
| QEMU 版本 | 通过 `make run_kernel`（Docker 容器），版本待补 |
| 内存 | 16G（`MEM=16G`） |
| 编译模式 | release（`RELEASE=1`，I 系列除外） |

**通用固定参数**：`LOG_LEVEL=debug SYSCALL_INFO=off`。

- `LOG_LEVEL=debug`：必须，`dyndbg_debug!` 走 `log::debug!`，低于此级别不会触发日志输出。
- `SYSCALL_INFO=off`：关闭 `info!` 级别的 syscall 入口/出口追踪（`kernel/src/syscall/mod.rs`），避免每次系统调用产生海量 `info` 日志污染串口、干扰 benchmark 计时。**不影响 `dyndbg_debug!`（走 `debug` 级别）。**

**SMP 和 RELEASE 按测试类别区分**：

| 测试系列 | SMP | RELEASE | 原因 |
|----------|-----|---------|------|
| F（功能） | 1（默认） | 1 | 单线程串行，release 即可 |
| I-01（增量重算） | 1（默认） | **不可用** | stats 计数器依赖非 release 构建的断言/检查 |
| I-02（索引消融） | 1（默认） | 1 | 使用 uptime 计时 + 原子计数器，无 debug 断言依赖 |
| P（性能） | **1** | 1 | 单线程基准，多核引入调度抖动干扰计时 |
| C（并发） | **4** | 1 | 多核并发 patching 是 C 系列的核心测试目标 |

**同构建分组**：以下用例共用同一编译命令，可通过 `/test/dyndbg/run_all.sh` 一键执行：

```bash
make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G
# guest 内：
/test/dyndbg/run_all.sh
```

| 用例 | 测试内容 |
|------|---------|
| F-01~F-08 | 功能正确性 |
| I-02 | 多维度索引消融 |
| P-01（static） | 微基准 static path |
| P-01R（static） | 真实 workload static path |
| P-02 | batch patch 开销 |

**需单独编译启动的用例**：

| 用例 | 编译命令差异 |
|------|------------|
| I-01 | 不可使用 `RELEASE=1` |
| P-01（baseline） | `NO_DEFAULT_FEATURES=1 FEATURES=cvm_guest` |
| P-01（branch） | `NO_DEFAULT_FEATURES=1 FEATURES=cvm_guest,branchdbg` |
| P-01R（baseline/branch） | 同上 |
| C-01~C-03 | `SMP=4` |

`LOG_LEVEL=debug` 对 I 系列非强制——I-01 只读 `/proc/sys/kernel/dyndbg_stats`，以 `warn` 级别运行可压低 DEBUG 噪声、减少串口阻塞，不影响统计结果。

测试结果收集说明：`tools/dyndbg/*.sh` 会被打包进 initramfs，guest 启动后可直接在 shell 里执行 `/test/dyndbg/*.sh`。当前内核不支持 9P，因此结果会优先尝试写到 `/results`，失败后回退到 `/ext2/results`。测试结束后，使用 `tools/dyndbg/collect_results.sh` 把 ext2 镜像里的结果同步回宿主机的 `results/` 目录。

同步顺序：guest 里先执行 `sync`，再 `umount /ext2`，最后在宿主机单独开一个终端运行 `sh tools/dyndbg/collect_results.sh`。否则 loop-mount 可能看不到刚写入的 `results/`。收集脚本会自动把旧 CSV 表头重写成统一的新 schema，避免宿主 `results.csv` 残留旧标题。

宿主机收集后的结果会落到源码树下的 `results/` 目录。

收集结果的示例：

```bash
make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G

# 同步结果
sync；umount /ext2

# guest 测试结束并卸载 ext2 后，把 ext2 镜像里的结果收集回宿主机 results/
sh tools/dyndbg/collect_results.sh
```

说明：正式测试在上述远程服务器 Docker 容器（`asterinas-dev`）内执行。

### 2.3 编译环境

| 项目 | 内容 |
| --- | --- |
| Rust 版本 | nightly-2025-12-06（见 `rust-toolchain.toml`） |
| 编译模式 | release（`RELEASE=1`），I 系列除外 |
| 内存 | `MEM=16G` |

所有测试通过 `make run_kernel` 构建并启动 QEMU guest，无需单独 `cargo build`。

**三组 P 系列编译命令**：

```bash
# baseline（dyndbg 编译期消除）
make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G \
  NO_DEFAULT_FEATURES=1 FEATURES=”cvm_guest”

# branch（branch gate，无 static patch）
make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G \
  NO_DEFAULT_FEATURES=1 FEATURES=”cvm_guest,branchdbg”

# static（static patch 站点存在，运行时 -p 禁用）
make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G
```

说明：`NO_DEFAULT_FEATURES=1` 关闭默认 features（含 `dyndbg`），再通过 `FEATURES` 精确指定要启用的 feature。static build 不传额外参数，走默认 `cvm_guest + dyndbg`。

---

## 3. 当前实现语义说明（必读）

### 3.1 命令语法

selectors 在前，action 在最后：

```text
module=mm +p
file=mm/ func=alloc_ +p
line=123 -p
```

### 3.2 Selector 语义

- file/module/func：substring contains
- line：精确匹配

可用示例：

```text
file=mm/
func=alloc_
```

不可用示例（当前实现不支持）：

```text
file=mm/*.rs
func=alloc_*
```

### 3.3 module selector 语义

module 匹配的是 Rust 的 module_path，而不是 log target。

### 3.4 Rule Chain 语义

- 多规则参与匹配
- 最终由最后命中规则决定（last-match-wins）

### 3.5 Static Patch 语义

- NOP5 <-> JMP rel32
- disabled fast path 为纯 NOP 流

### 3.6 架构假设

- 当前只验证 x86_64
- 依赖 x86 I-Cache coherence
- 其他架构未实现显式 I-Cache flush

### 3.7 已知限制

- module id 数量有限
- 超限后进入 UNASSIGNED_MODULE_ID，对应日志点永久禁用
- 未覆盖项：非 x86 I-Cache flush、CPU hotplug、模块卸载/重载、嵌套 patch 事务

---

## 3.8 测试用例清单（Case List）

说明：以下用例为测试执行的主清单，执行时可在各小节内补充更细化步骤。

| Case ID | 目标 | 输入/操作 | 预期结果 | 备注 |
| --- | --- | --- | --- | --- |
| F-01 | Module selector 基础匹配 | module=mm +p | 仅 module_path 含 mm 的日志输出 | 使用真实 module_path 子串 |
| F-02 | File selector 子串匹配 | file=mm/ +p | 仅 file 路径含 mm/ 输出 | 禁用通配符语法 |
| F-03 | Function selector 子串匹配 | func=alloc_ +p | 仅函数名含 alloc_ 输出 | 函数名为 module_path::fn |
| F-04 | Line selector 精确匹配 | line=123 +p | 仅行号 123 输出 | 行号必须存在 |
| F-05 | 规则冲突 last-match-wins | module=mm +p; func=alloc -p | alloc 不输出，其他 mm 输出 | 交换顺序验证覆盖 |
| F-06 | 动态切换即时性 | clear; module=mm +p; clear | enable 立即生效，clear 立即失效 | 避免规则叠加 |
| F-07 | 非法输入鲁棒性 | ++++; line=abc; func=== | 返回错误且不崩溃 | 记录 errno |
| F-08 | 多层规则覆盖 | module=mm +p; file=mm/ +p; func=alloc -p; line=123 +p | last-match-wins 生效 | 多层冲突覆盖 |
| I-01 | 增量重算正确性与延迟 | module=sched +p | 记录 descriptors_recomputed、modules_repatched、sites_patched、last_update_latency_us，从计数和延迟两个维度验证增量重算优于全量重算 | 依赖 stats 接口 |
| I-02 | 多维度索引消融 | 同一内核 index=on/off 对比，line/file/func/module 四维度 `-p` | index=on 候选收集延迟低于 index=off（file/func/module 16-36%），line 接近（BTreeMap 点查 vs 线性扫描在当前规模下持平） | 使用 `-p` 消除 patching 噪声，仅测量候选收集路径 |
| P-01 | Static fast path（微基准） | dyndbg_bench 紧凑循环，三构建对比 | `baseline ≈ static < branch` | TSC 计时，禁止串口输出 |
| P-01R | Static fast path（真实 workload） | 6 类 syscall 场景，三构建对比 | `baseline ≈ static < branch` | 覆盖 FS/IPC/进程/fd 子系统 |
| P-02 | Batch patch 开销 | 同一内核下 per-site vs batch | batch patch 更快 | 单次脚本自动跑两轮 |
| C-01 | 并发 patch 稳定性 | 规则切换并发（module=$MODULE_KEY +p） | 无 panic/crash/死锁 | 验证规则切换稳定性 |
| C-02 | 高频 patch 压测 | 1e5 次 enable/disable | 系统稳定 | 记录耗时 |
| C-03 | Patch Storm（真实场景混合负载） | 多 CPU 混合规则操作，默认 `BENCH_MODE=log` | 无死锁/非法指令/崩溃 | 覆盖真实打印路径 |

Case 与章节对应关系：

| Case ID | 对应小节 |
| --- | --- |
| F-01 | 4.1 Module Selector 测试 |
| F-02 | 4.2 File Selector 测试 |
| F-03 | 4.3 Function Selector 测试 |
| F-04 | 4.4 Line Selector 测试 |
| F-05 | 4.5 Rule Chain 冲突测试 |
| F-06 | 4.6 动态 Enable/Disable 测试 |
| F-07 | 4.7 非法输入鲁棒性测试 |
| F-08 | 4.8 多层规则覆盖测试 |
| I-01 | 5.1 增量重算正确性 |
| I-02 | 5.2 多维度索引消融 |
| P-01 | 6.1 Disabled Fast Path Benchmark |
| P-01R | 6.1.1 Real Workload Supplement |
| P-02 | 6.2 Batch Patch Transaction Benchmark |
| C-01 | 7.1 规则切换并发 |
| C-02 | 7.2 高频 Patch Stress |
| C-03 | 7.3 Patch Storm |

---

## 4. 功能正确性测试

F 系列与 P 系列 static 共用同一 build（默认 features，dyndbg 启用）：

```bash
make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G
```

F 系列主要是验证 selector 语义和规则链行为，不是性能压测；因此正式执行时不需要很大的 `ITERS`。建议默认使用 `ITERS=100`，既能覆盖每个测例的规则操作和日志输出链路，又不会让验证过程过长。

建议使用脚本：`/test/dyndbg/functional.sh`

F-04 与 F-08 的 `line` 选择器由 initramfs 构建时生成的 `/etc/dyndbg_line.txt` 提供；脚本运行时优先读取该文件，读不到时回退到稳定默认值。这样即使 guest 内没有源码树，也能保持 line 选择稳定。

快速参数档：

```bash
# 1) guest 中运行功能测试快速档
ITERS=10 /test/dyndbg/functional.sh
```

正式命令：

```bash
# guest 中运行功能测试正式档
/test/dyndbg/functional.sh


# 需要手工指定 line 时可覆盖：
# LINE_KEY=<new_line> ITERS=100 /test/dyndbg/functional.sh
```

### 4.1 Module Selector 测试 (F-01)

**目标**：验证 module selector 是否正确控制目标日志。

**步骤**：

```text
module=mm +p
```

**预期**：
- 目标 descriptor 被命中并通过 bench 判定为 enabled
- 其他模块不受该 selector 影响


---

### 4.2 File Selector 测试 (F-02)

**步骤**：

```text
file=mm/ +p
```

**预期**：
- 目标 descriptor 被命中并通过 bench 判定为 enabled
- file selector 对应的目标范围生效


---

### 4.3 Function Selector 测试 (F-03)

**步骤**：

```text
func=alloc_ +p
```

**预期**：
- 目标 descriptor 被命中并通过 bench 判定为 enabled
- function selector 对应的目标范围生效


---

### 4.4 Line Selector 测试 (F-04)

**步骤**：

```text
line=123 +p
```

**预期**：
- 目标 descriptor 被命中并通过 bench 判定为 enabled
- line selector 对应的精确行号生效


---

### 4.5 Rule Chain 冲突测试（last-match-wins）(F-05)

**步骤**：

```text
module=mm +p
func=alloc -p
```

**预期**：
- 通过 bench 的 enabled/disabled 状态验证 last-match-wins 规则
- 交换顺序后，最终命中的规则决定结果

交换顺序验证覆盖关系。


---

### 4.6 动态 Enable/Disable 测试 (F-06)

**预期**：
- enable 后 bench 判定为 enabled
- clear 后 bench 判定为 disabled
- 连续切换时规则状态与 bench 结果保持一致


---

### 4.7 非法输入鲁棒性测试 (F-07)

**测试输入**：

```text
++++
line=abc
func===
+ ???
```

**预期**：
- 非法输入不会新增规则
- 不 panic、不 crash


---

### 4.8 多层规则覆盖测试 (F-08)

**测试目标**：验证多层规则下 last-match-wins 语义稳定。

**步骤**：

```text
module=mm +p
file=mm/ +p
func=alloc -p
line=123 +p
```

**预期**：
- 通过 bench 状态验证最终规则覆盖结果
- last-match-wins 在多层规则下保持稳定




## 5. 增量更新测试

**I 系列不可使用 `RELEASE=1`**（stats 计数器依赖非 release 构建的断言/检查）。

```bash
make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off MEM=16G
```

### 5.1 增量重算正确性 (I-01)

**前置要求**：增加统计接口，例如：

- 读取路径：`/proc/sys/kernel/dyndbg_stats`
- 重置命令：`echo reset > /proc/sys/kernel/dyndbg_stats`

```rust
struct DynDbgStats {
    descriptors_recomputed: usize,
    modules_repatched: usize,
    sites_patched: usize,
    last_update_latency_us: u64,
}
```

**步骤**：
1. 以 `warn` 级别启动 guest，避免 `+p` 触发大量 DEBUG 输出干扰测试执行；I-01 只依赖统计接口，不依赖控制台打印。
2. `echo reset > /proc/sys/kernel/dyndbg_stats`
3. `echo "+p" > /proc/sys/kernel/dynamic_debug`
4. `cat /proc/sys/kernel/dyndbg_stats`
5. `echo reset > /proc/sys/kernel/dyndbg_stats`
6. `echo "module=sched +p" > /proc/sys/kernel/dynamic_debug`
7. `cat /proc/sys/kernel/dyndbg_stats`

**预期**：
- selectorless 规则触发全量重算
- module 规则触发显著更小的重算规模
- `descriptors_recomputed`、`modules_repatched`、`sites_patched` 都会被记录，其中 `descriptors_recomputed` 是主判据，后两项是辅助观测值
- `last_update_latency_us` 记录最近一次规则更新（含重算 + patch）的端到端 TSC 延迟（微秒），增量重算延迟应显著低于全量重算（典型值：全量 ~2500μs vs 增量 ~300μs，约 9x）
- 采用 `warn` 仅降低日志噪声，不改变 `descriptors_recomputed`、`modules_repatched`、`sites_patched` 的统计值，也不改变 I-01 的测试结论
建议使用脚本：`/test/dyndbg/incremental.sh`

快速参数档：

```bash
# I-01 直接运行即可，不需要额外参数
/test/dyndbg/incremental.sh
```

说明：CSV 会输出 full / module 的 descriptor、module、site、latency_us 统计字段（含 `full_latency_us` 和 `module_latency_us`），同时从计数和延迟两个维度衡量增量重算效果；I-01 的主判据是 descriptor 重算数量是否比 full 更小，延迟作为辅助判据。

### 5.2 多维度索引消融 (I-02)

**目标**：通过关闭索引（退化为线性扫描），测量多维度索引对候选收集阶段的加速贡献。

**原理**：
- 系统维护 file/module/function/line 四维 BTreeMap 索引，规则更新时通过索引定位受影响 descriptor 集合
- 通过 `/proc/sys/kernel/dyndbg_bench` 的 `index=on|off` 开关，可将候选收集切换为全量 descriptor 线性扫描
- 与 I-01 不同，I-02 测量的是"候选收集"阶段的纯粹差异，而非 descriptor 重算粒度
- 使用 `-p`（disable）替代 `+p`，消除静态修补（NOP↔JMP）噪声：所有 descriptor 默认禁用，`-p` 保持禁用态，不触发状态迁移，无指令修补

**索引实现细节**：
- `file_index` / `module_index` / `function_index`：遍历全部 BTreeMap key 做子串匹配（`contains`），复杂度 O(num_unique_keys)，非真正 O(log N) 点查
- `line_index`：真正 BTreeMap `get()` 点查，复杂度 O(log N)
- 消融路径（`index=off`）：`all_descriptors().filter(rule.matches_descriptor)`，复杂度 O(all_descriptors)

**前置要求**：
- 内核编译时包含 `dyndbg` feature（默认）
- `/proc/sys/kernel/dyndbg_bench` 支持 `index=on|off` 控制
- `dyndbg_bench` 模块包含 65 个 descriptor，覆盖 four selector 维度

**步骤**：
1. 以默认 `LOG_LEVEL=debug` 编译启动（`-p` 不触发日志输出，无干扰）
2. guest 内执行 `/test/dyndbg/index_ablation.sh`
3. 脚本自动在 `index=on` 和 `index=off` 下各跑四组 selector：`line=N -p`、`file=dyndbg_bench.rs -p`、`func=bench_log_0 -p`、`module=dyndbg_bench -p`，每轮 `INDEX_ITERS` 次（默认 10000）
4. 每次迭代执行 `selector -p` + `clear`，规则从空状态开始，清除累积效应

**预期**：
- `sites_patched` 全部为 0，确认无 patching 噪声混入
- `descriptors_recomputed` 在 index=on 和 index=off 下相等，确认工作负载一致
- `file` / `func` / `module` 三个 key-scan 维度：`index=on` elapsed 低于 `index=off`（16-36%），收益来自将扫描从全体 descriptor（284）缩减为唯一 key 集合
- `line` 维度（真 BTreeMap 点查）：两者接近，当前 284-descriptor 规模下 BTreeMap 指针跳转的 cache 开销抵消了 O(log N) 优势
- 随 descriptor 规模增长，全部维度的索引收益将线性放大

**建议使用脚本**：`/test/dyndbg/index_ablation.sh`

```bash
# 快速参数档：
INDEX_ITERS=1000 /test/dyndbg/index_ablation.sh
# 正式档：
/test/dyndbg/index_ablation.sh
```

结果 CSV 路径：`results/index_ablation/results.csv`


---

## 6. 性能测试

注意：性能测试禁止真实串口输出，否则 IO 开销会掩盖 fast path 结果。
建议优先使用 `mode=count`、关闭真实 `log` 输出，或只保留极少量日志用于功能验证。

### 6.0 Benchmark 约束与判定标准

固定条件：

- 固定 QEMU 参数与 CPU 核数
- release 构建
- 固定迭代次数，建议 warmup 1 次
- 每项至少 3 轮取均值

三向对比的执行原则：

- baseline、branch、patch 必须分别编译、分别启动、分别采样
- 同一次 guest 启动只测一个 backend，不在脚本内部自动切换 backend
- `perf.sh` 通过 `BACKEND_MODE=baseline|branch|static` 指定当前启动对应的 backend
- `workload.sh` 通过 `WORKLOAD_MODE=baseline|branch|static` 指定当前启动对应的 workload 采样模式

说明：baseline、branch、patch 这三组不是在同一次 guest 里切 backend，而是要分别编译、分别启动三次，再分别执行测试命令。

判定标准（建议范围）：

- static path cycles 不高于 baseline 的 1.1x
- static patch 相比 descriptor 应降低 branches/branch-misses
- 若趋势相反，标记为 review 并记录原因

#### 三组构建的消融关系

P 系列的三组构建并非三种并列的编译配置，而是构成一条干净的**模块消融链**（module-level ablation chain）。其中 branch 的定位不是"另一个功能选项"，而是**为了量化静态指令修补优化贡献而刻意保留的系统中间态**。

三组构建包含的组件差异：

| 构建 | 描述符 + 静态注册 | 规则链 + 索引 + 增量重算 | 模块级门控 | Static Patch（NOP↔JMP） | 批量修补事务 |
|------|:---:|:---:|:---:|:---:|:---:|
| baseline | | | | | |
| branch | ✅ | ✅ | ✅ | | |
| static (patch build) | ✅ | ✅ | ✅ | ✅ | ✅ |

三组构建的角色定位：

| 构建 | 消融角色 |
|------|---------|
| baseline | **绝对下限**：编译期完全消除，所有调用点直接消失 |
| branch | **静态修补优化的消融对照**：保留描述符 + 运行时过滤引擎 + 模块门控，但用软件分支判断替代指令修补。与 static 的唯一差异是 static patch 的有无 |
| static（patch build） | **最终实现的性能基线**：完整系统，static patch 站点运行时常驻但被规则禁用 |

消融链：

```
baseline ──(+描述符+过滤引擎+模块门控)──→ branch ──(+static patch+批量修补)──→ static
   ↑                                         ↑                                    ↑
   编译期消除                           软件分支判断                        硬件 NOP/JMP 短路
```

核心判据：

- `baseline ≈ static < branch` 成立 → static patch 有效消除了分支判断开销，将禁用路径压至接近编译期消除
- `static - branch` 的差值是 **static patch + 批量修补的整体贡献**（模块门控在两组中均存在，已从差值中抵消）
- `branch - baseline` 的差值是**描述符注册 + 运行时过滤引擎 + 模块门控的固有开销**（无硬件指令修补时的纯软件路径成本）

### 6.1 Disabled Fast Path Benchmark (P-01)


指标：elapsed（必选）+ task_clock_ms / context_switches / page_faults（软指标）+ cycles / instructions / branches / branch-misses（硬件 PMU，可选）

说明（当前仓库与内核能力）：

- `cycles/instructions/branches/branch-misses` 依赖 `perf stat` 与 `perf_event_open` 路径。
- 当前 Asterinas 兼容性文档标注 `perf_event_open` 为不支持项，因此在 guest 内通常无法稳定采集硬件 PMU 计数。
- `tools/dyndbg/perf.sh` 已支持在无 `perf` 场景下退化采集软指标：`task_clock_ms`（基于 `/proc/self/stat` 的 utime+stime）、`context_switches`（`/proc/stat` 的 `ctxt` 差值）、`page_faults`（`/proc/stat` 的 `page_faults` 差值）。
- `RESULT` 和 CSV 只保留软指标列：`elapsed_ms`、`task_clock_ms`、`context_switches`、`page_faults`。

与题目要求的对应关系：

- 性能基线：`mode=log` 的 static path 作为动态禁用下的主基线；`mode=count` 仅作为无日志的辅助 sanity baseline
- 动态调试对比：同一测试路径在大量调试语句被动态禁用时的 static path 数据
- `ENABLE_LOG=1` 不属于正式对照，只用于功能验证

当前实现说明：

- `mode=log` 和 `mode=count` 仍通过同一组 procfs / userspace harness 触发，因此它们适合做相对比较，不适合声称为“纯 kernel 内部 loop”
- `ENABLE_LOG=1` 只适合功能验证或可视化观察，不应混入正式 P-01 性能结论
- 如果后续要做真正的 enabled_noprint / enabled_print 分离，需要新增 kernel benchmark 入口或单独的 enabled-no-print 计数路径

三次编译 / 三次启动的推荐顺序：

1. baseline build：编译期关闭 `dyndbg`，让调用点在编译期直接消失。
2. branch build：关闭 `dyndbg`，开启 `branchdbg`，保留 descriptor + 运行时 branch gate。
3. patch build：默认构建，保留原始 static patch backend。

对应命令示例：

```bash
# 1) baseline build + baseline run
make run_kernel ENABLE_KVM=0 SMP=4 INITRAMFS_SKIP_GZIP=1 LOG_LEVEL=debug SYSCALL_INFO=off NO_DEFAULT_FEATURES=1 FEATURES=cvm_guest

# guest 内只跑 baseline backend
快速档：
ITERS=200 BACKEND_MODE=baseline /test/dyndbg/perf.sh
正式档：
BACKEND_MODE=baseline /test/dyndbg/perf.sh

# 2) branch build + branch run
make run_kernel ENABLE_KVM=0 SMP=4 INITRAMFS_SKIP_GZIP=1 LOG_LEVEL=debug SYSCALL_INFO=off NO_DEFAULT_FEATURES=1 FEATURES="cvm_guest,branchdbg"

# guest 内只跑 branch backend
快速档：
ITERS=200 BACKEND_MODE=branch /test/dyndbg/perf.sh
正式档：
BACKEND_MODE=branch /test/dyndbg/perf.sh

# 3) patch build + static run
make run_kernel ENABLE_KVM=0 SMP=4 INITRAMFS_SKIP_GZIP=1 LOG_LEVEL=debug SYSCALL_INFO=off

# guest 内只跑 patch backend（静态 patch 站点存在，运行时规则禁用）
快速档：
ITERS=200 BACKEND_MODE=static /test/dyndbg/perf.sh
正式档：
BACKEND_MODE=static /test/dyndbg/perf.sh
```

### 6.1.1 Real Workload Supplement (P-01R)

为了避免 P-01 只停留在单次 `count` vs `log` 的合成循环，建议补一组真实文件系统 workload，对比同一内核热点路径在“无规则/低干扰”与“规则存在但动态禁用”下的端到端耗时。当前最合适的 3 个场景是：

| workload | 主要内核路径 | 说明 |
| --- | --- | --- |
| create_delete | `open` / `close` / `unlink` | 小文件创建后立即删除，适合观察 VFS + ext2 的基础路径开销 |
| rename | `rename` | 单路径重命名，适合观察目录项查找和重命名路径 |
| mkdir_rmdir | `mkdir` / `rmdir` | 目录创建/删除，适合观察目录操作路径 |

对应脚本：`/test/dyndbg/workload.sh`

说明：

- baseline、branch、patch 必须分开构建、分开启动。
- baseline build：`dyndbg` feature 关闭，站点在编译期直接移除。
- branch build：`branchdbg` feature 开启，保留 descriptor + branch gate。
- patch build：默认构建，站点存在但运行时 `-p` 禁用。
- `workload.sh` 现在支持三种正式模式：`WORKLOAD_MODE=baseline`、`WORKLOAD_MODE=branch`、`WORKLOAD_MODE=static`。
- 正式测试建议在三次构建里分别运行对应脚本，再在宿主机侧汇总对比。
- static path 通过同一批 syscall 热点文件上的动态 debug 规则触发，例如 `file=open.rs -p`、`file=rename.rs -p`、`file=mkdir.rs -p`、`file=rmdir.rs -p`。
- branch path 与 static path 使用同一组 `-p` 规则，主要用于比较“descriptor + branch gate”相对 patch 的开销。
- 这组测试更接近真实 workload 的端到端效果，适合作为 P-01 的补充，不替代 P-01 的 fast-path 微基准。

结果字段：

- `workload_mode`：`baseline` / `branch` / `static`
- `elapsed_ms`：当前模式下的平均耗时
- `task_clock_ms`：当前模式下的平均任务时钟
- `context_switches`：当前模式下的平均上下文切换数
- `page_faults`：当前模式下的平均页故障数

三向对比建议：

1. 先在 baseline build 里运行 `WORKLOAD_MODE=baseline`，采 baseline。
2. 再在 branch build 里运行 `WORKLOAD_MODE=branch`，采 branch 组数据。
3. 最后在 patch build 里运行 `WORKLOAD_MODE=static`，采 patch 组数据。

构建示例：

```bash
# baseline build: sites compiled out
# baseline build: no dynamic debug sites (no-site baseline)
# Use NO_DEFAULT_FEATURES to disable default features (which include dyndbg).
make run_kernel ENABLE_KVM=0 SMP=4 INITRAMFS_SKIP_GZIP=1 LOG_LEVEL=debug SYSCALL_INFO=off NO_DEFAULT_FEATURES=1 FEATURES=cvm_guest

# branch-based build: branch conditional instrumentation compiled in
# Enable `branchdbg` feature while leaving `dyndbg` off.
make run_kernel ENABLE_KVM=0 SMP=4 INITRAMFS_SKIP_GZIP=1 LOG_LEVEL=debug SYSCALL_INFO=off NO_DEFAULT_FEATURES=1 FEATURES="cvm_guest,branchdbg"

# patch-based build: static patch sites (original dyndbg)
make run_kernel ENABLE_KVM=0 SMP=4 INITRAMFS_SKIP_GZIP=1 LOG_LEVEL=debug SYSCALL_INFO=off
```

对应 workload 运行命令：

```bash
# baseline build
快速档：
WORKLOAD_MODE=baseline ITERS=10  /test/dyndbg/workload.sh
正式档：
WORKLOAD_MODE=baseline /test/dyndbg/workload.sh

# branch build
快速档：
WORKLOAD_MODE=branch  ITERS=10  /test/dyndbg/workload.sh
正式档：
WORKLOAD_MODE=branch  /test/dyndbg/workload.sh

# patch build
快速档：
WORKLOAD_MODE=static  ITERS=10  /test/dyndbg/workload.sh
正式档：
WORKLOAD_MODE=static  /test/dyndbg/workload.sh
```

---

### 6.2 Batch Patch Transaction Benchmark (P-02)

Scope：

- 测量 runtime rule update 的端到端延迟
- 包括 procfs dispatch、命令解析、selector recompute 和 patch transaction
- 在同一内核、同一次 guest 启动、同一次脚本执行中，对比 `backend=per_site` 与 `backend=batch`
- 该项用于补充说明系统开销来源，不替代 P-01 的性能基线要求

对比维度：

- per-site patch backend
- batch patch backend

记录：patch time、`modules_repatched`、`sites_patched`。

建议使用脚本：`/test/dyndbg/patch_bench.sh`


```bash
# 快速参数档：
PATCH_ITERS=20 /test/dyndbg/patch_bench.sh
# 正式档：
/test/dyndbg/patch_bench.sh  # 脚本内部自动跑 per_site 和 batch 两轮
```

用途：

- 验证 patch transaction 正常
- 验证 stats 正常
- 验证无死锁/异常
- 验证同一实现下的 per-site 与 batch 后端差异

三种模式：

- `PATCH_MODE=enable` 观察纯 enable 更新
- `PATCH_MODE=clear` 观察纯 clear 更新
- `PATCH_MODE=toggle` 观察 enable + clear 成对更新

说明：默认推荐 `PATCH_MODE=toggle`，因为它最接近当前批量 transaction 的常见更新形态。

---

## 7. 并发与稳定性测试

**只有 C 系列需要多核**（并发 patching 是核心测试目标）：

```bash
make run_kernel SMP=4 LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G
```

### 7.1 规则切换并发 (C-01)

- CPU0：高频count
- CPU1：循环 module=mm +p / clear 规则切换
 
目标：验证规则切换在并发条件下的稳定性（无 panic/oops、无死锁、patch 事务一致性）。

本测试的定位是“规则切换稳定性”，因此主测使用 `BENCH_MODE=count`（不做真实打印输出），以避免 IO/串口干扰并专注于内核侧的并发路径和一致性行为。

建议使用脚本：`/test/dyndbg/concurrency.sh`

快速参数档：

```bash
# 并发测试：使用 count 模式避免日志输出污染结果，同时保留 dmesg 检查
LOG_ITERS=50 TOGGLE_ITERS=20 /test/dyndbg/concurrency.sh
```
正式档：
```bash
/test/dyndbg/concurrency.sh
```

说明：CSV 会包含 `bench_mode`、`duration_us`、`dmesg_status`、`dmesg_hits`、`status` 等字段；以无 panic/oops 为通过标准。

### 7.2 高频 Patch Stress (C-02)

执行多次 enable/disable，系统保持稳定。

建议使用脚本：`/test/dyndbg/stress.sh`

快速参数档：

```bash
# 高频压测：启用 dmesg 检查。若需更高置信度，可把 TOGGLE_ITERS 值提高到 500 或以上。
TOGGLE_ITERS=50 /test/dyndbg/stress.sh
```
正式档：
```bash
/test/dyndbg/stress.sh
```

---

### 7.3 Patch Storm (C-03)

**测试目标**：多 CPU 混合规则操作下验证真实场景稳定性。

**测试方法**：

- CPU0: 高频 log
- CPU1: module=mm +p / clear
- CPU2: add/del rule
- CPU3: func/file/line 混合切换

持续运行，验证无死锁/非法指令/崩溃。

建议使用脚本：`/test/dyndbg/patch_storm.sh`


默认 `BENCH_MODE=log`，以覆盖真实打印路径下的稳定性；如需降噪或做辅助对照，可临时切到 `BENCH_MODE=count`。

快速参数档：

```bash
# Patch storm：启用 dmesg 检查，保持较小迭代以便快速验收
BENCH_MODE=log STORM_ITERS=20 LOG_ITERS=50 CLEAR_INTERVAL=10  \
    /test/dyndbg/patch_storm.sh
```

正式档：
```bash
/test/dyndbg/patch_storm.sh
```

---

## 8. 最终测试总结

### 8.1 功能正确性（F 系列）

| Case | 结果 | 说明 |
|------|------|------|
| F-01~F-08 | 全部 pass | selector 语义、last-match-wins、动态切换、鲁棒性全部通过 |

### 8.2 增量重算与索引消融（I 系列）

| Case | 结果 | 说明 |
|------|------|------|
| I-01 | pass | 全量 284 descriptors / 154 modules；module=sched 仅重算 65 descriptors / 2 modules，缩减 ~77% |
| I-02 | pass | file/func/module 三维 index=on 快 16-36%；line 维度接近（BTreeMap 点查 cache 开销抵消）；全部 sites_patched=0 确认无 patching 噪声 |

### 8.3 性能基准（P 系列）

| Case | 结果 | 说明 |
|------|------|------|
| P-01 | pass | baseline=9355μs < static=9450μs (+1.0%) < branch=9625μs (+2.9%)，符合 `baseline ≈ static < branch` |
| P-02 | pass | batch=5.09s vs per_site=5.29s，batch 快 ~3.7%；130000 sites 仅 4000 batch transactions |

### 8.4 并发稳定性（C 系列）

| Case | 结果 | 说明 |
|------|------|------|
| C-01~C-03 | 全部 pass | 无 panic/oops/死锁，dmesg 干净 |

### 8.5 汇总

| 维度 | 结论 |
|------|------|
| selector 精确匹配 | ✅ 通过 |
| last-match-wins 规则链 | ✅ 通过 |
| runtime 动态切换 | ✅ 通过 |
| static fast path 最小化 | ✅ static 仅比 baseline 高 ~1% |
| static patch 生效 | ✅ `baseline ≈ static < branch` 趋势成立 |
| batch patch 生效 | ✅ batch 事务数减少 97%，延迟降低 ~3.7% |
| 多维度索引加速候选收集 | ✅ file/func/module 索引加速 16-36%；line 接近（当前规模下 BTreeMap cache 开销与线性扫描持平） |
| SMP 并发稳定 | ✅ 无 panic/crash/死锁，dmesg 干净 |

---

## 9. 辅助工具与脚本

### 9.1 dyndbg_bench

路径：`/proc/sys/kernel/dyndbg_bench`

写入命令：

```text
mode=log iters=1000000
mode=count iters=1000000
```

读取输出字段：

- last_mode
- last_iters
- last_duration_us
- counter

说明：

- `mode=log` 会触发 `dyndbg_debug!` 调用，用于测量禁用/启用路径。
- `mode=count` 仅做计数，适合作为循环基线。
- 默认 selector 可用 `module=dyndbg_bench`、`file=dyndbg_bench.rs`、`func=bench_log`。
- line selector 由 initramfs 生成的 `/etc/dyndbg_line.txt` 提供；若缺失则回退到稳定默认值。

### 9.2 测试脚本（/test/dyndbg）

脚本位于 guest 内的 `/test/dyndbg/`，建议在 guest shell 内执行：

```bash
chmod +x /test/dyndbg/*.sh
```

- 功能测试：`/test/dyndbg/functional.sh`
- 并发压力：`/test/dyndbg/concurrency.sh`
- 性能基线：`/test/dyndbg/perf.sh`
- 增量重算：`/test/dyndbg/incremental.sh`
- 索引消融：`/test/dyndbg/index_ablation.sh`
- Patch 基准：`/test/dyndbg/patch_bench.sh`
- 高频压测：`/test/dyndbg/stress.sh`
- Patch 风暴：`/test/dyndbg/patch_storm.sh`
- 一键执行：`/test/dyndbg/run_all.sh`

可选环境变量：

- `MODULE_KEY` / `FILE_KEY` / `FUNC_KEY` / `LINE_KEY`
- `ITERS` / `TOGGLE_ITERS` / `LOG_ITERS`
- `ENABLE_LOG` / `RUN_COUNT`
- `RUNS` / `WARMUP` / `CLEAR_INTERVAL` / `SLEEP_US` / `KEEP_STATE`
- `RESULTS_DIR` / `RUN_ID` / `COMMIT` / `PHASE` / `DESCRIPTORS`
- `PERF` / `PERF_EVENTS` / `DMESG_CHECK` / `BYTES_DISABLED` / `BYTES_ENABLED`

run_all.sh 额外变量（仅覆盖同构建用例：F / I-02 / P-01 static / P-01R static / P-02）：

- `RUN_FUNCTIONAL` / `RUN_INDEX_ABLATION`
- `RUN_PERF` / `RUN_WORKLOAD`
- `RUN_PATCH_BENCH`

快速参数档：

```bash
ITERS=10 RUNS=1 INDEX_ITERS=100 PATCH_ITERS=100 /test/dyndbg/run_all.sh
```

说明：

- `run_all.sh` 默认将结果写入 `results/<RUN_ID>/...`。

### 9.3 dyndbg_stats

路径：`/proc/sys/kernel/dyndbg_stats`

读取输出字段：

- descriptors_recomputed
- modules_repatched
- sites_patched

重置统计：

```text
reset
```


### 9.4 结构化输出与结果目录

所有脚本输出统一的结构化结果行：

```text
RESULT key=value key=value ...
```

并写入 CSV（默认目录为 guest 内的 `/ext2/results/`；如果 9P 可用则会切换到 `/results/`）：

```
results/
    functional/results.csv
    concurrency/results.csv
    perf/results.csv
    patch_bench/results.csv
    scale/results.csv
    stress/results.csv
    patch_storm/results.csv
```

建议在每次测试前设置：

```text
RUN_ID=<custom-id>
```

说明：

- 如果 perf/dmesg/字节信息不可用，对应字段会记录为 `na` 或 `unknown`。
- 由于当前内核不支持 9P，共享结果目录会回退为 ext2 镜像；如需把结果同步到宿主机源码树下的 `results/`，在 guest 退出后运行 `tools/dyndbg/collect_results.sh`。
- `functional.sh` 的结构化结果会使用 `status=pass|fail`，并附带 `expected` / `actual`，不再只表示 `executed`。