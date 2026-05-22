# Asterinas Dynamic Debug System 测试文档

## 1. 文档目标

本文档用于验证 Asterinas Dynamic Debug System 的：

1. 功能正确性
2. 运行时动态切换正确性
3. Static Patch 热路径优化效果
4. Batch Patch Transaction 效果
5. SMP-safe Runtime Patch 稳定性
6. 系统边界与已知限制

本文档同时作为：

- 测试执行指南
- Benchmark 执行规范
- 数据记录模板
- 最终测试报告基础

---

## 2. 测试环境

### 2.1 硬件环境

| 项目 | 内容 |
| --- | --- |
| 设备名 | WIN-49IU2922R58 |
| CPU | Intel(R) Core(TM) i5-10500H @ 2.50GHz |
| 核心数 | 未提供（后补） |
| 内存 | 16.0 GB（可用 15.8 GB） |
| 存储 | 477 GB SATA SSD；932 GB HDD（WDC WD10SPZX-22Z10T1） |
| 显卡 | NVIDIA GeForce GTX 1650 Ti（4 GB）；Intel UHD Graphics（128 MB） |
| 宿主机系统 | Windows 64-bit + WSL（docker-desktop） |

### 2.2 虚拟化环境

| 项目 | 内容 |
| --- | --- |
| QEMU 版本 | 通过 `make run_kernel`（WSL + docker-desktop），版本待补 |
| SMP 核数 | 4 |
| 启动参数 | `make run_kernel ENABLE_KVM=0 SMP=4 INITRAMFS_SKIP_GZIP=1` |

推荐固定参数：

```bash
qemu-system-x86_64 -smp 4 -m 4G
```

说明：正式数据测试的服务器环境信息留空，后续补充。

### 2.3 编译环境

| 项目 | 内容 |
| --- | --- |
| Rust 版本 | |
| 编译模式 | release |
| 编译参数 | |

建议：

```bash
cargo build --release
```

### 2.4 测试 Commit

| 阶段 | Commit | 说明 |
| --- | --- | --- |
| baseline | | 原始日志系统 |
| descriptor | | descriptor fast path |
| module gate | | module-level gate |
| static patch | | call-site static patch |
| batch patch | | batch transaction |

建议打 tag：

```bash
git tag phase1-descriptor
git tag phase2-module-gate
git tag phase3-static-patch
git tag phase4-batch-patch
```

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
| I-01 | 增量重算正确性 | module=sched +p | 仅相关 descriptor 变化 | 依赖 stats 接口 |
| P-01 | Disabled fast path | 多阶段基线对比 | branches/branch-misses 下降 | 禁止串口输出 |
| P-02 | Batch patch 开销 | 单事务 vs batch | batch patch 更快 | 使用旧 commit 对照 |
| P-03 | descriptor 规模扩展 | 100/1000/10000 | overhead 平稳可控 | 记录 update latency |
| P-04 | Patch Binary Verification | 校验 NOP5/JMP | 指令字节符合预期 | GDB/QEMU | 
| P-05 | Module-level patch reduction | 多 descriptor 变化 | modules_repatched 保持低 | 依赖 stats 接口 |
| C-01 | 并发 patch 稳定性 | CPU0 日志 + CPU1 规则切换 | 无 panic/crash/死锁 | 运行 5-10 分钟 |
| C-02 | 高频 patch 压测 | 1e5 次 enable/disable | 系统稳定 | 记录耗时 |
| C-03 | Patch Storm | 多 CPU 混合操作 | 无死锁/非法指令 | 并发 add/del/clear |

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
| P-01 | 6.1 Disabled Fast Path Benchmark |
| P-02 | 6.2 Batch Patch Transaction Benchmark |
| P-03 | 6.3 Descriptor Scale Benchmark |
| P-04 | 6.4 Patch Binary Verification |
| P-05 | 6.5 Module-level Patch Reduction |
| C-01 | 7.1 Logging + Patch 并发 |
| C-02 | 7.2 高频 Patch Stress |
| C-03 | 7.3 Patch Storm |

---

## 4. 功能正确性测试

### 4.1 Module Selector 测试 (F-01)

**目标**：验证 module selector 是否正确控制目标日志。

**步骤**：

```text
module=mm +p
```

**预期**：
- module_path 含 mm 的日志输出
- 其他模块日志不输出

**实际结果**：待填写

---

### 4.2 File Selector 测试 (F-02)

**步骤**：

```text
file=mm/ +p
```

**预期**：仅 file 路径包含 mm/ 的日志输出

**实际结果**：待填写

---

### 4.3 Function Selector 测试 (F-03)

**步骤**：

```text
func=alloc_ +p
```

**预期**：函数名包含 alloc_ 的日志输出

**实际结果**：待填写

---

### 4.4 Line Selector 测试 (F-04)

**步骤**：

```text
line=123 +p
```

**预期**：仅行号精确匹配的日志输出

**实际结果**：待填写

---

### 4.5 Rule Chain 冲突测试（last-match-wins）(F-05)

**步骤**：

```text
module=mm +p
func=alloc -p
```

**预期**：
- mm::alloc 不输出
- 其他 mm 输出

交换顺序验证覆盖关系。

**实际结果**：待填写

---

### 4.6 动态 Enable/Disable 测试 (F-06)

注意：规则链是追加式，连续 +p / -p 会形成多条规则。

**正确切换流程**：
1. clear
2. module=mm +p
3. 触发日志，确认输出
4. clear 或 del <id>
5. 触发日志，确认停止

**实际结果**：待填写

---

### 4.7 非法输入鲁棒性测试 (F-07)

**测试输入**：

```text
++++
line=abc
func===
+ ???
```

**预期**：返回错误、不 panic、不 crash。

**实际结果**：待填写

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
- line=123 的日志输出
- alloc 相关但非 line=123 的日志不输出
- 其他 mm/ 路径日志输出

**实际结果**：待填写

建议使用脚本：`tools/dyndbg/functional.sh`

快速参数档：

```bash
ITERS=200000 tools/dyndbg/functional.sh
```

## 5. 增量更新测试

### 5.1 增量重算正确性 (I-01)

**前置要求**：增加统计接口，例如：

- 读取路径：`/proc/sys/kernel/dyndbg_stats`
- 重置命令：`echo reset > /proc/sys/kernel/dyndbg_stats`

```rust
struct DynDbgStats {
    descriptors_recomputed: usize,
    modules_repatched: usize,
    sites_patched: usize,
}
```

**步骤**：
1. `echo reset > /proc/sys/kernel/dyndbg_stats`
2. `echo "+p" > /proc/sys/kernel/dynamic_debug`
3. `cat /proc/sys/kernel/dyndbg_stats`
4. `echo reset > /proc/sys/kernel/dyndbg_stats`
5. `echo "module=sched +p" > /proc/sys/kernel/dynamic_debug`
6. `cat /proc/sys/kernel/dyndbg_stats`

**预期**：
- selectorless 规则触发全量重算
- module 规则触发显著更小的重算规模

建议使用脚本：`tools/dyndbg/incremental.sh`

快速参数档：

```bash
tools/dyndbg/incremental.sh
```

**实际结果**：待填写

---

## 6. 性能测试

注意：性能测试禁止真实串口输出，否则 IO 开销会掩盖 fast path 结果。
建议用计数器递增或空路径模拟。

### 6.0 Benchmark 约束与判定标准

固定条件：

- 固定 QEMU 参数与 CPU 核数
- release 构建
- 固定迭代次数，建议 warmup 1 次
- 每项至少 3 轮取均值

判定标准（建议范围）：

- disabled path cycles 不高于 baseline 的 1.1x
- static patch 相比 descriptor 应降低 branches/branch-misses
- 若趋势相反，标记为 review 并记录原因

### 6.1 Disabled Fast Path Benchmark (P-01)

| 阶段 | disabled path |
| --- | --- |
| baseline | call |
| descriptor | atomic + branch |
| module gate | module branch |
| static patch | nop5 |

指标：cycles / instructions / branches / branch-misses / elapsed

建议使用脚本：`tools/dyndbg/perf.sh`

可选：设置 `PERF=1` 以尝试采集 perf 计数器；如不可用则仅输出 elapsed。

快速参数档（禁用 perf）：

```bash
ITERS=200000 RUNS=1 WARMUP=0 PERF=0 tools/dyndbg/perf.sh
```

---

### 6.2 Batch Patch Transaction Benchmark (P-02)

需对比：
- per-site transaction（旧 commit）
- batch transaction（当前）

记录：patch time 与事务次数。

建议使用脚本：`tools/dyndbg/patch_bench.sh`

快速参数档：

```bash
PATCH_ITERS=300 tools/dyndbg/patch_bench.sh
```

---

### 6.3 Descriptor Scale Benchmark (P-03)

| descriptors | overhead | update latency |
| --- | --- | --- |
| 100 | | |
| 1000 | | |
| 10000 | | |

建议使用脚本：`tools/dyndbg/scale.sh`

快速参数档（需手动标注 DESCRIPTORS）：

```bash
DESCRIPTORS=100 UPDATE_ITERS=1 tools/dyndbg/scale.sh
```

---

### 6.4 Patch Binary Verification (P-04)

**测试目标**：验证 NOP5 <-> JMP rel32 的指令级切换确实发生。

**方法建议**（二选一）：

1) QEMU + GDB 读取 `__dyndbg_site_*` 处 5 字节。
2) 在调试接口中导出 patch site 地址后读取内存。

**预期**：

- disable 时为 `0F 1F 44 00 00`
- enable 时为 `E9 <rel32>`

建议使用脚本：`tools/dyndbg/patch_verify.sh`

如需记录字节值，可设置 `BYTES_DISABLED` / `BYTES_ENABLED` 手动填入。

快速参数档（需 GDB 手工读取字节）：

```bash
KEEP_STATE=1 tools/dyndbg/patch_verify.sh
```

**实际结果**：待填写

---

### 6.5 Module-level Patch Reduction (P-05)

**测试目标**：验证模块级聚合后 patch 次数减少。

**步骤**：

```text
echo reset > /proc/sys/kernel/dyndbg_stats
module=mm +p
cat /proc/sys/kernel/dyndbg_stats
```

**预期**：

- modules_repatched 显著小于 sites_patched
- 同一模块多 descriptor 变化仅触发一次 modules_repatched

**实际结果**：待填写

建议使用脚本：`tools/dyndbg/patch_reduction.sh`

快速参数档：

```bash
tools/dyndbg/patch_reduction.sh
```

## 7. 并发与稳定性测试

### 7.1 Logging + Patch 并发 (C-01)

- CPU0：高频日志
- CPU1：循环 module=mm +p / clear

持续数分钟，验证无 panic / crash / 死锁。

建议使用脚本：`tools/dyndbg/concurrency.sh`

快速参数档：

```bash
LOG_ITERS=200000 TOGGLE_ITERS=2000 DMESG_CHECK=0 tools/dyndbg/concurrency.sh
```

可选：设置 `DMESG_CHECK=1` 自动扫描 dmesg 关键字。

### 7.2 高频 Patch Stress (C-02)

执行 100000 次 enable/disable，系统保持稳定。

建议使用脚本：`tools/dyndbg/stress.sh`

可选：设置 `DMESG_CHECK=1` 自动扫描 dmesg 关键字。

快速参数档：

```bash
TOGGLE_ITERS=20000 DMESG_CHECK=0 tools/dyndbg/stress.sh
```

---

### 7.3 Patch Storm (C-03)

**测试目标**：多 CPU 混合规则操作下验证稳定性。

**测试方法**：

- CPU0: 高频 log
- CPU1: module=mm +p / clear
- CPU2: add/del rule
- CPU3: func/file/line 混合切换

持续运行，验证无死锁/非法指令/崩溃。

建议使用脚本：`tools/dyndbg/patch_storm.sh`

可选：设置 `DMESG_CHECK=1` 自动扫描 dmesg 关键字。

快速参数档：

```bash
STORM_ITERS=2000 LOG_ITERS=200000 CLEAR_INTERVAL=200 SLEEP_US=0 DMESG_CHECK=0 \
    tools/dyndbg/patch_storm.sh
```

---

## 8. 指令级分析

对照 disabled path 演化，验证：
- 分支消除
- 读内存减少
- 禁用路径最小化

---

## 9. 最终测试总结

统一结论表：

| 目标 | 结果 | 备注 |
| --- | --- | --- |
| selector 精确匹配 | | |
| last-match-wins | | |
| runtime toggle | | |
| disabled fast path 最小化 | | |
| static patch 生效 | | |
| batch patch 生效 | | |
| SMP 并发稳定 | | |

---

## 10. 执行流程建议

1. 补全环境信息
2. 给 milestone commit 打 tag
3. 实现必要测试辅助接口（stats）
4. 按文档逐项执行
5. 回填数据
6. 汇总成正式报告

---

## 11. 辅助工具与脚本

### 11.1 dyndbg_bench

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
- line selector 需要从 `dyndbg_bench.rs` 中取实际行号。

### 11.2 测试脚本（tools/dyndbg）

脚本位于 `tools/dyndbg/`，建议在目标系统内执行：

```bash
chmod +x tools/dyndbg/*.sh
```

- 功能测试：`tools/dyndbg/functional.sh`
- 并发压力：`tools/dyndbg/concurrency.sh`
- 性能基线：`tools/dyndbg/perf.sh`
- 增量重算：`tools/dyndbg/incremental.sh`
- Patch 基准：`tools/dyndbg/patch_bench.sh`
- 规模扩展：`tools/dyndbg/scale.sh`
- 高频压测：`tools/dyndbg/stress.sh`
- Patch 字节验证：`tools/dyndbg/patch_verify.sh`
- Patch 降低验证：`tools/dyndbg/patch_reduction.sh`
- Patch 风暴：`tools/dyndbg/patch_storm.sh`
- 一键执行：`tools/dyndbg/run_all.sh`

可选环境变量：

- `MODULE_KEY` / `FILE_KEY` / `FUNC_KEY` / `LINE_KEY`
- `ITERS` / `TOGGLE_ITERS` / `LOG_ITERS`
- `ENABLE_LOG` / `RUN_COUNT`
- `RUNS` / `WARMUP` / `CLEAR_INTERVAL` / `SLEEP_US` / `KEEP_STATE`
- `RESULTS_DIR` / `RUN_ID` / `COMMIT` / `PHASE` / `DESCRIPTORS`
- `PERF` / `PERF_EVENTS` / `DMESG_CHECK` / `BYTES_DISABLED` / `BYTES_ENABLED`

run_all.sh 额外变量：

- `RUN_FUNCTIONAL` / `RUN_INCREMENTAL` / `RUN_PERF`
- `RUN_PATCH_BENCH` / `RUN_SCALE` / `RUN_PATCH_VERIFY`
- `RUN_PATCH_REDUCTION` / `RUN_CONCURRENCY` / `RUN_STRESS` / `RUN_PATCH_STORM`

快速参数档：

```bash
ITERS=200000 RUNS=1 WARMUP=0 LOG_ITERS=200000 TOGGLE_ITERS=2000 STORM_ITERS=2000 \
PATCH_ITERS=300 UPDATE_ITERS=1 PERF=0 DMESG_CHECK=0 RUN_PATCH_VERIFY=0 \
tools/dyndbg/run_all.sh
```

说明：

- `run_all.sh` 默认将结果写入 `results/<RUN_ID>/...`。

### 11.4 结构化输出与结果目录

所有脚本输出统一的结构化结果行：

```text
RESULT key=value key=value ...
```

并写入 CSV（默认目录 `results/`）：

```
results/
    functional/results.csv
    concurrency/results.csv
    perf/results.csv
    incremental/results.csv
    patch_bench/results.csv
    scale/results.csv
    stress/results.csv
    patch_verify/results.csv
    patch_reduction/results.csv
    patch_storm/results.csv
```

建议在每次测试前设置：

```text
COMMIT=<git-sha>
PHASE=<baseline|descriptor|module_gate|static_patch|batch_patch>
RUN_ID=<custom-id>
```

说明：

- 如果 perf/dmesg/字节信息不可用，对应字段会记录为 `na` 或 `unknown`。

### 11.3 dyndbg_stats

路径：`/proc/sys/kernel/dyndbg_stats`

读取输出字段：

- descriptors_recomputed
- modules_repatched
- sites_patched

重置统计：

```text
reset
```
