# AI 工具使用记录

本文档记录 Asterinas 动态调试系统开发过程中 AI 工具的使用情况。

---

## 1. 使用的 AI 工具

| 工具 | 模型 | 用途 |
|------|------|------|
| Claude Code (Anthropic) | DeepSeek V4 Pro / DeepSeek V4 Flash | 文档审查、代码审查、资料检索、测试脚本分析 |

所有 AI 交互均通过 Claude Code CLI 在 VS Code 扩展环境中进行。

---

## 2. AI 辅助完成资料检索与调研验证

- **Linux dynamic debug 机制调研**：验证第 2 章调研内容的准确性，包括 `dynamic_debug_init()` 与 `ddebug_add_module()` 的函数分工、eBPF 引入版本（3.15 → 3.18）、Jim Cromie RFC 发布时间（2024 → 2026）
- **Linux 指令修补技术验证**：查证 Linux 自 2013 年起已使用 `text_poke_bp()` INT3 断点法进行 SMP 安全代码修补，而非 `stop_machine()` — 此事实性错误在文档第 2 章多处出现，AI 协助发现并修正了 4 处


---

## 3. AI 建议与人工决策
以下记录部分 AI 在使用过程中的建议与人工决策：

| 类别 | AI 建议 | 人工决策与理由 |
|------|---------|---------------|
| 代码 | 删除 `BenchMode::Count` | 采纳。人工确认无测试合法使用该模式，从代码和文档中同步移除 |
| 代码 | C-01 默认 `BENCH_MODE` 从 `count` 改为 `log` | 采纳。count 未测试 NOP5 并发，改为 log 后测试才有实际意义 |
| 代码 | `saturating_fetch_sub_u32` 用 CAS 循环替代 `fetch_sub` | 采纳。`fetch_sub` 在并发竞争下可能下溢到 `u32::MAX`，导致模块门控永久锁定。AI 建议用 CAS 循环实现饱和减法，确保 `enabled_count` 永不跌破 0 |
| 代码 | 基准测试循环前加 `.align 64` 内联汇编 | 采纳。同一段循环在不同编译单元中可能被放置到不同 cache line 偏移，导致 ±5% 的性能抖动。AI 建议对齐到 64 字节边界消除此噪声，使 P-01 的 50 轮测量波动从 ±500µs 收窄到 ±100µs |
| 代码 | `WriteProtectGuard` 用 RAII 管理 CR0.WP 的开关 | 采纳。队员原本在 `apply_patch_transaction` 中手动 `disable()` / `restore()` CR0.WP，AI 指出若中间 `write_bytes` panic 则写保护无法恢复。改用 RAII guard 后 Drop 自动恢复，异常安全 |
| 代码 | `bench_log()` 中添加 `core::hint::black_box(())` | 采纳。仅靠 `#[inline(never)]` 不足以保证 LTO 不消除该函数——当 dyndbg 编译期剥离时编译器可能判定其为死代码。AI 建议末尾加 `black_box` 制造"可见副作用"，确保 P-01 baseline 构建下循环不被优化消除 |
| 文档 | 第 3 章改为自顶向下与第 4 章一致 | 部分采纳。改为自顶向下后队员指出架构图中箭头为自底向上，最终去除方向性声明，仅与第 4 章顺序对齐 |
| 文档 | 两个同名 `matches_descriptor` 函数改名 | **拒绝**。改名需要修改系统代码，队员选择通过添加 `impl` 块加说明文本来区分 |
| 文档 | 性能数据表格中加入耗时 | **拒绝**。C-01 测试定位为稳定性测试而非性能测试，刻意留空以强调关注点 |
| 文档 | C-02/C-03 也需重跑 | **拒绝**。队员指出 C-02 无 bench mode、C-03 已默认 log，AI 的判断被纠正 |
| 文档 | 第 2 章篇幅过长，需加桥梁 | 采纳。队员自行撰写 2.6 节设计回应表，AI 协助修正技术细节 |
| 文档 | 多处"三后端"改为"双后端" | 采纳。队员确认 no-op 为编译期退化状态，不构成运行时后端 |
| 文档 | PerSite 从设计章节移除 | 采纳。队员确认 PerSite 仅用于测试，按既定写作规则从第 3–4 章移除 |
| 架构 | 本文档的审查方法（两轮审查 + 五维度 + 四级别） | 采纳。由队员与 AI 共同设计，最终成为本次开发的文档质量保障流程 |

---

## 4. AI 生成内容的错误及修正

以下记录部分 AI 在使用过程中产生的错误，以及队员如何发现、修正和验证：

| 错误 | AI 输出 | 发现方式 | 修正 |
|------|---------|---------|------|
| Linux 指令修补机制 | AI 称 Linux static keys 依赖 `stop_machine()` 进行代码修补 | 队员人工查证 Linux 内核源码（`arch/x86/kernel/alternative.c`），确认自 2013 年起已使用 `text_poke_bp()` INT3 断点法 | 修正文档第 2 章 4 处提到 `stop_machine()` 的位置，替换为 `text_poke_bp()` INT3 方法的准确描述 |
| C-02/C-03 需重跑 | AI 建议 C-02 和 C-03 也需要重跑测试 | 队员指出 C-02 不使用 bench mode、C-03 已默认 `log` | AI 承认判断不准确，仅重跑 C-01 |
| 第 3 章方向性 | AI 建议第 3 章改为自顶向下 | 队员指出架构图为自底向上 | 去除方向性声明，仅按第 4 章顺序排列层级 |
| 测试文档中的章节引用 | AI 生成的测试文档初稿中包含"第 4.3.2 节"等跨文档引用 | 队员审查时发现读者无法理解这些引用来源 | 移除所有跨文档章节引用，改为自包含描述 |
| JMP rel32 偏移计算 | AI 生成的 `encode_jmp_rel32` 伪代码中，rel32 偏移计算为 `target - site`（以槽起始地址为基准） | 队员对照 x86 手册验证：JMP rel32 的偏移基准是**下一条指令的地址**（`site + 5`），而非当前指令地址 | 修正为 `target - (site + 5)`，并在文档中标注 "next_ip" 变量说明计算基准 |
| `linkme` 工作机制描述 | AI 多次将 `#[distributed_slice]` 描述为"运行时动态注册"，与 C 语言的 `__attribute__((section))` 混淆 | 队员阅读 `linkme` 源码确认：分布式切片是**链接时**由链接器合并各编译单元的同名段，运行时表现为普通的 `&[T]` 静态切片，不存在任何运行时注册代码 | 修正文档第 2.5 节和第 4.4.1 节的相关表述，强调"链接时聚合、运行时零成本" |
| 四维索引的 `BTreeMap` vs `HashMap` 选择 | AI 建议使用 `HashMap` 替代 `BTreeMap`，理由是 O(1) 查找优于 O(log N) | 队员分析内核环境的特殊性：(a) `HashMap` 需要随机种子防 HashDoS，`no_std` 环境下获取真随机熵困难；(b) 启动时一次性批量插入后仅有查询操作，BTree 的缓存局部性优于开放寻址 HashMap；(c) `BTreeMap` 有序性支持 `collect_by_keyword` 中的前缀/子串范围扫描 | 维持 `BTreeMap` 设计，AI 的理解被纠正后在文档第 4.2.2 节中补充了选型理由 |
| `SeqCst` 屏障定位 | AI 将 `apply_patch_transaction` 中的 `fence(Ordering::SeqCst)` 描述为"防止编译器指令重排" | 队员指出编译屏障只需 `Ordering::Acquire/Release`，此处用 `SeqCst` 是因为 x86 上 `mfence` 指令还承担**序列化 CPU 指令流水线**的作用——确保 `write_volatile` 写入的字节已被取指单元可见，远程 CPU 退出 IPI 旋转后不会执行到旧的半写入指令 | 修正文档第 4.3.2 节屏障描述，区分"写前屏障（全局可见性）"和"写后屏障（流水线序列化）"两个独立目的 |

---


