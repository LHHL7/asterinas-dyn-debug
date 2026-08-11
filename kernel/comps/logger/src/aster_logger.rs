// SPDX-License-Identifier: MPL-2.0

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering},
    time::Duration,
};

use log::{Metadata, Record};
use linkme::distributed_slice;
use ostd::{
    arch::static_key::{self, StaticKeySite},
    sync::SpinLock,
    timer::Jiffies,
};

/// The logger used for Asterinas.
struct AsterLogger;

static LOGGER: AsterLogger = AsterLogger;
// Dynamic debug state protected by a single lock so rule updates and descriptor
// registrations are serialized.
static DYNDBG_STATE: SpinLock<DyndbgState> = SpinLock::new(DyndbgState::new());
const DEFAULT_DEBUG_ENABLED: bool = false;
const MAX_DYNDBG_MODULES: usize = 8192;
// 安全设计 当模块id分配用尽时，后续descriptor将被分配到UNASSIGNED_MODULE_ID上，默认禁用。
const UNASSIGNED_MODULE_ID: u32 = u32::MAX;
/// 热点计数器数组容量（与 per-CPU ring 同属 trace 基础设施）。
/// 安全设计 当descriptor数量超过上限时，超出部分不参与热点统计。
pub const MAX_HOT_SITES: usize = 4096;
const UNASSIGNED_HOT_INDEX: u32 = u32::MAX;

// Fast gates used on the hot path. Updates happen under DYNDBG_STATE lock, while
// reads are lock-free in dyndbg_should_log().
static MODULE_STATES: [ModuleState; MAX_DYNDBG_MODULES] =
    [const { ModuleState::new() }; MAX_DYNDBG_MODULES];

static DYNDBG_DESCRIPTORS_RECOMPUTED: AtomicU64 = AtomicU64::new(0);
static DYNDBG_MODULES_REPATCHED: AtomicU64 = AtomicU64::new(0);
static DYNDBG_SITES_PATCHED: AtomicU64 = AtomicU64::new(0);
static DYNDBG_PATCH_TRANSACTIONS: AtomicU64 = AtomicU64::new(0);
static DYNDBG_LAST_UPDATE_LATENCY_US: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DyndbgPatchBackend {
    PerSite = 0,
    Batch = 1,
}

static DYNDBG_PATCH_BACKEND: AtomicU8 = AtomicU8::new(DyndbgPatchBackend::Batch as u8);

impl DyndbgPatchBackend {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::PerSite,
            _ => Self::Batch,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerSite => "per_site",
            Self::Batch => "batch",
        }
    }
}

pub fn set_dyndbg_patch_backend(backend: DyndbgPatchBackend) {
    DYNDBG_PATCH_BACKEND.store(backend as u8, Ordering::Relaxed);
}

pub fn get_dyndbg_patch_backend() -> DyndbgPatchBackend {
    DyndbgPatchBackend::from_u8(DYNDBG_PATCH_BACKEND.load(Ordering::Relaxed))
}

/// When `false`, the index-based candidate collection is bypassed and replaced
/// with a linear scan of all descriptors. This is an ablation hook for
/// measuring the performance contribution of the multi-dimensional index.
static DYNDBG_INDEX_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_dyndbg_index_enabled(enabled: bool) {
    DYNDBG_INDEX_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn get_dyndbg_index_enabled() -> bool {
    DYNDBG_INDEX_ENABLED.load(Ordering::Relaxed)
}

/// When `false`, candidate collection returns ALL descriptors instead of a narrowed
/// subset, simulating the fused full-recompute behavior of Linux's `ddebug_change()`.
/// This is an ablation hook for measuring the contribution of incremental recomputation.
static DYNDBG_RECOMPUTE_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_dyndbg_recompute_enabled(enabled: bool) {
    DYNDBG_RECOMPUTE_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn get_dyndbg_recompute_enabled() -> bool {
    DYNDBG_RECOMPUTE_ENABLED.load(Ordering::Relaxed)
}

struct ModuleState {
    enabled_count: AtomicU32,
}

impl ModuleState {
    const fn new() -> Self {
        Self {
            enabled_count: AtomicU32::new(0),
        }
    }
}

/// Links a [`DebugDescriptor`] to the [`StaticKeySite`] that gates its hot path.
///
/// Populated at compile time by the `dyndbg_debug!` macro and consumed at boot
/// by [`pre_register_dyndbg_keys`] to build the per-module site vectors.
#[derive(Debug, Clone, Copy)]
pub struct DyndbgKeyMapping {
    pub descriptor: &'static DebugDescriptor,
    pub static_key_site: &'static StaticKeySite,
}

struct ModuleKey {
    enabled: bool,
    static_key_sites: Vec<&'static StaticKeySite>,
}

impl ModuleKey {
    fn new() -> Self {
        Self {
            enabled: false,
            static_key_sites: Vec::new(),
        }
    }
}

//编译期收集的静态切片，init阶段完成注册操作 后续运行时无注册开销
#[distributed_slice]
pub static DYNDBG_DESCRIPTOR_REGISTRY: [&'static DebugDescriptor];

/// Distributed slice mapping each dyndbg call site to its [`StaticKeySite`].
///
/// Populated at link time by the `dyndbg_debug!` macro.  Consumed at boot by
/// `pre_register_dyndbg_keys()` to organise sites per module for batch patching.
#[distributed_slice]
pub static DYNDBG_KEY_MAPPING: [&'static DyndbgKeyMapping];


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DyndbgRuleAction {
    EnableLog,
    DisableLog,
    EnableTrace,
    DisableTrace,
    /// Flags-only rule: applies format-flag bits without touching the switch.
    KeepState,
}

/// Format flags for dyndbg log output prefixes (Linux `+f/+l/+m/+t`).
///
/// Stored per-descriptor as a bitmask (`DebugDescriptor::format_flags`).
/// Read only on the enabled (JMP) path, so the disabled NOP5 path is
/// unaffected — these flags never enter the hot disabled path.
pub const FLAG_FUNCTION: u8 = 1 << 0; // +f: function name prefix
pub const FLAG_LINE: u8 = 1 << 1;     // +l: "file:line" prefix
pub const FLAG_MODULE: u8 = 1 << 2;   // +m: module path prefix
pub const FLAG_THREAD: u8 = 1 << 3;   // +t: current task pointer prefix

#[derive(Debug, Clone, Default)]
struct DyndbgRule {
    file_keyword: Option<String>,
    module_keyword: Option<String>,
    function_keyword: Option<String>,
    line: Option<u32>,
    /// Format-flag bits to set on matched descriptors (`+f/+l/+m/+t`).
    flags_set: u8,
    /// Format-flag bits to clear on matched descriptors (`-f/-l/-m/-t`).
    flags_clear: u8,
    /// Exact format-flag value to overwrite on matched descriptors (`=fl`,
    /// Linux semantics: replaces whatever the chain produced so far).
    flags_override: Option<u8>,
}

impl DyndbgRule {
    // 判断规则是否非空
    fn has_any_selector(&self) -> bool {
        self.file_keyword.is_some()
            || self.module_keyword.is_some()
            || self.function_keyword.is_some()
            || self.line.is_some()
    }

    // 判断描述符是否匹配规则
    fn matches_descriptor(&self, descriptor: &DebugDescriptor) -> bool {
        if !self.has_any_selector() {
            return true;
        }

        selector_match(&self.file_keyword, Some(descriptor.file), Some("/"))
            && selector_match(&self.module_keyword, Some(descriptor.module_path), Some("::"))
            && selector_match(&self.function_keyword, descriptor.function_name(), None)
            && self.line.is_none_or(|line| descriptor.line == line)
    }
}

// ── 选择器匹配语义 ────────────────────────────────────────────────────────
// 三通道：精确(完整值) → 段精确(短名/多段交集) → 通配符(*/?)扫描。
// 无子串（contains）匹配——"包含"语义由段倒排索引以精确查找实现。
// 该谓词与索引收集路径（collect_by_keyword_exact_first）结果一致，
// 保证 index=on/off 两条路径输出相同候选集（消融实验前提）。

/// 判断字符串是否含通配符 `*` 或 `?`。
fn has_wildcard(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

/// 类 glob 匹配：`*` 匹配任意字符序列（含空），`?` 匹配单个字符。
/// 经典贪心两指针实现（无递归、无回溯指数级）。
fn match_wildcard(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_pi = None;
    let mut star_ti = 0usize;
    while ti < t.len() {
        //普通字符或 ? 匹配 两指针前进
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } 
        // 若为*，则记录*位置
        // pattern 指针前进，但 text 指针不动（先假设 * 匹配空串）
        else if pi < p.len() && p[pi] == b'*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } 
        // 当前匹配失败 但之前有*
        // 让那个 * 多匹配一个字符（star_ti += 1），然后重新尝试
        else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } 
        // 匹配失败且没*
        else {
            return false;
        }
    }
    // 处理 pattern 末尾的 *
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// 多段交集：keyword 按分隔符切出的每一段都必须出现在 value 的段集合中。
/// 例如 module=ext2::dir 匹配模块路径含 `ext2` 段且含 `dir` 段的所有描述符。
fn path_segments_match(keyword: &str, value: &str, sep: &str) -> bool {
    keyword
        .split(sep)
        .filter(|s| !s.is_empty())
        .all(|seg| value.split(sep).any(|v| v == seg))
}

/// 谓词：value（完整值）是否匹配 keyword。
/// `sep=None` 表示不分段（如函数名，原子值）。
fn value_matches(keyword: &str, value: &str, sep: Option<&str>) -> bool {
    if has_wildcard(keyword) {
        return match_wildcard(keyword, value);
    }
    if value == keyword {
        return true;
    }
    sep.is_some_and(|sep| path_segments_match(keyword, value, sep))
}

// 选择器为None时总是匹配（真正的match逻辑是&&）
fn selector_match(
    selector: &Option<String>,
    value: Option<&str>,
    sep: Option<&str>,
) -> bool {
    match selector {
        None => true,
        Some(needle) => value.is_some_and(|value| value_matches(needle, value, sep)),
    }
}

struct DyndbgState {
    rules: Vec<DyndbgRuleEntry>,
    module_id_by_path: BTreeMap<&'static str, u32>,
    module_keys: BTreeMap<u32, ModuleKey>,
    /// 主索引：完整路径/函数名/行号 → 描述符组（精确查找）。
    file_index: BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    module_index: BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    function_index: BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    line_index: BTreeMap<u32, Vec<&'static DebugDescriptor>>,
    /// 段倒排索引：module_path 按 `::` 切段，每段 → 描述符组。
    /// 支持 Linux 式短名（如 `module=ext2`）与多段交集（如 `module=ext2::dir`）。
    module_segment_index: BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    /// 段倒排索引：file 路径按 `/` 切段（含 basename），如 `file=dir.rs`。
    file_segment_index: BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    /// Next hotspot counter index to assign (registry order).
    hot_site_count: usize,
}

#[derive(Debug, Clone)]
struct DyndbgRuleEntry {
    rule: DyndbgRule,
    action: DyndbgRuleAction,
}

impl DyndbgState {
    const fn new() -> Self {
        Self {
            rules: Vec::new(),
            module_id_by_path: BTreeMap::new(),
            module_keys: BTreeMap::new(),
            file_index: BTreeMap::new(),
            module_index: BTreeMap::new(),
            function_index: BTreeMap::new(),
            line_index: BTreeMap::new(),
            module_segment_index: BTreeMap::new(),
            file_segment_index: BTreeMap::new(),
            hot_site_count: 0,
        }
    }

    //注册descriptor时会将其插入到各个索引表中，初始化module id
    fn register_descriptor(&mut self, descriptor: &'static DebugDescriptor) {
        insert_index_entry(&mut self.file_index, descriptor.file, descriptor);
        insert_index_entry(&mut self.module_index, descriptor.module_path, descriptor);
        if let Some(function) = descriptor.function_name() {
            insert_index_entry(&mut self.function_index, function, descriptor);
        }
        insert_index_entry(&mut self.line_index, descriptor.line, descriptor);
        // 段倒排索引：module_path 按 `::` 切段，file 按 `/` 切段。
        // split 产生的段是源 &'static str 的切片，零分配。
        for segment in descriptor.module_path.split("::") {
            insert_index_entry(&mut self.module_segment_index, segment, descriptor);
        }
        for segment in descriptor.file.split('/') {
            insert_index_entry(&mut self.file_segment_index, segment, descriptor);
        }

        let module_id = self.allocate_module_id(descriptor.module_path);
        descriptor.init_module_id(module_id);

        // 热点计数器按注册顺序分配索引（索引 == DYNDBG_DESCRIPTOR_REGISTRY 下标，
        // 查询时可直接反查描述符）。
        if self.hot_site_count < MAX_HOT_SITES {
            descriptor.init_hot_index(self.hot_site_count as u32);
            self.hot_site_count += 1;
        }
        // 初始时空规则链 默认禁用所有descriptor，避免冗余判断和触发状态迁移逻辑。
        if self.rules.is_empty() {
            descriptor.init_enabled(DEFAULT_DEBUG_ENABLED, false);
            return;
        }
        //若初始有rule链 根据rule链来设置初始enabled状态和判断是否迁移
        let (log_enabled, trace_enabled, flags) = self.matches_descriptor(descriptor);
        descriptor.init_enabled(log_enabled, trace_enabled);
        descriptor.set_format_flags(flags);
        if log_enabled || trace_enabled {
            self.apply_enabled_transition(module_id, false, true);
        }
    }

    //把编译期收集的 StaticKeySite 按模块分组。
    fn register_dyndbg_key(&mut self, mapping: &'static DyndbgKeyMapping) {
        let module_id = mapping.descriptor.module_id();
        if module_id == UNASSIGNED_MODULE_ID {
            return;
        }

        let current_enabled = module_enabled(module_id);
        let module_key = self.module_keys.entry(module_id).or_insert_with(ModuleKey::new);
        module_key.enabled = current_enabled;
        module_key.static_key_sites.push(mapping.static_key_site);

        if current_enabled {
            patch_module_sites(module_key, true);
        }
    }

    // 设置新规则时仅重算受影响的descriptor，降低规则更新成本。
    /// 重放式刷新（remove/clear 专用）：对受影响描述符重放完整规则链，
    /// 计算最终状态。链变更可能改变"最后命中者"，无法增量维护，必须重放。
    fn refresh_registered_descriptors(&mut self, affected: Vec<&'static DebugDescriptor>) {
        let tsc_start = ostd::arch::read_tsc();

        let mut seen = BTreeSet::new();
        let mut module_deltas = BTreeMap::<u32, i64>::new();

        
        for descriptor in affected {
            //去重
            if !seen.insert(descriptor_id(descriptor)) {
                continue;
            }
            DYNDBG_DESCRIPTORS_RECOMPUTED.fetch_add(1, Ordering::Relaxed);
            // 仅对受影响的 descriptor 进行独立双维度裁决
            // Log 和 Trace 各自 last-match-wins，互不干扰。
            // Format flags 无条件应用（flags 变化不影响指令修补/有效态）。
            let (log_enabled, trace_enabled, flags) = self.matches_descriptor(descriptor);
            descriptor.set_format_flags(flags);
            let (old_effective, new_effective) =
                descriptor.update_enabled(log_enabled, trace_enabled);
            if old_effective == new_effective {
                continue;
            }

            let module_id = descriptor.module_id();
            if module_state(module_id).is_none() {
                continue;
            }

            // 暂存descriptor变化（基于 effective 状态）
            let delta = if new_effective { 1 } else { -1 };
            *module_deltas.entry(module_id).or_insert(0) += delta;
        }

        // 应用模块级的变化，触发必要的指令修补。
        for (module_id, delta) in module_deltas {
            self.apply_module_delta(module_id, delta);
        }

        let tsc_end = ostd::arch::read_tsc();
        let tsc_freq = ostd::arch::tsc_freq();
        if tsc_freq > 0 && tsc_end > tsc_start {
            let elapsed_us = ((tsc_end - tsc_start) * 1_000_000) / tsc_freq;
            DYNDBG_LAST_UPDATE_LATENCY_US.store(elapsed_us, Ordering::Relaxed);
        }
    }

    /// 增量式刷新（append 专用）：新规则追加在规则链末尾，链尾必胜
    /// （last-match-wins），因此只需把新规则的动作/flags 增量应用到其
    /// 命中集上——动作只改对应维度，其余维度保持描述符当前持久状态；
    /// flags 按顺序模拟的尾部操作（set → clear → override）计算。
    ///
    /// 与重放整条链的结果等价（描述符持久状态 = 旧链重放结果，追加新规则
    /// 后目标值 = 旧值增量变换），但免去 O(链长) 的重复匹配。
    #[allow(clippy::too_many_arguments)]
    fn refresh_registered_descriptors_incremental(
        &mut self,
        affected: Vec<&'static DebugDescriptor>,
        rule: &DyndbgRule,
        action: DyndbgRuleAction,
        flags_set: u8,
        flags_clear: u8,
        flags_override: Option<u8>,
    ) {
        let tsc_start = ostd::arch::read_tsc();

        // recompute=full（消融开关）时 collect 把候选集扩大到全部描述符，
        // 候选集不等于新规则命中集——增量应用前必须按新规则过滤，只对
        // 命中者应用动作；incremental 时候选集==命中集，过滤整体跳过。
        let need_filter = !get_dyndbg_recompute_enabled();

        let mut seen = BTreeSet::new();
        let mut module_deltas = BTreeMap::<u32, i64>::new();

        for descriptor in affected {
            if !seen.insert(descriptor_id(descriptor)) {
                continue;
            }
            DYNDBG_DESCRIPTORS_RECOMPUTED.fetch_add(1, Ordering::Relaxed);

            // 计数在过滤前：recompute=full 的消融语义保持（重算全部候选）。
            if need_filter && !rule.matches_descriptor(descriptor) {
                continue;
            }

            // 增量应用：动作只改其对应维度，其余维度保持当前持久状态。
            let cur_log = descriptor.should_log_fast();
            let cur_trace = descriptor.should_trace_fast();
            let (log_enabled, trace_enabled) = match action {
                DyndbgRuleAction::EnableLog => (true, cur_trace),
                DyndbgRuleAction::DisableLog => (false, cur_trace),
                DyndbgRuleAction::EnableTrace => (cur_log, true),
                DyndbgRuleAction::DisableTrace => (cur_log, false),
                DyndbgRuleAction::KeepState => (cur_log, cur_trace),
            };
            // flags 增量：顺序模拟的尾部操作（set → clear → override）。
            let new_flags = if let Some(v) = flags_override {
                v
            } else {
                (descriptor.format_flags() | flags_set) & !flags_clear
            };
            descriptor.set_format_flags(new_flags);

            let (old_effective, new_effective) =
                descriptor.update_enabled(log_enabled, trace_enabled);
            if old_effective == new_effective {
                continue;
            }

            let module_id = descriptor.module_id();
            if module_state(module_id).is_none() {
                continue;
            }
            let delta = if new_effective { 1 } else { -1 };
            *module_deltas.entry(module_id).or_insert(0) += delta;
        }

        for (module_id, delta) in module_deltas {
            self.apply_module_delta(module_id, delta);
        }

        let tsc_end = ostd::arch::read_tsc();
        let tsc_freq = ostd::arch::tsc_freq();
        if tsc_freq > 0 && tsc_end > tsc_start {
            let elapsed_us = ((tsc_end - tsc_start) * 1_000_000) / tsc_freq;
            DYNDBG_LAST_UPDATE_LATENCY_US.store(elapsed_us, Ordering::Relaxed);
        }
    }

    // 为模块路径分配稳定且无冲突的模块ID。
    fn allocate_module_id(&mut self, module_path: &'static str) -> u32 {
        // 已存在 → 直接返回
        if let Some(module_id) = self.module_id_by_path.get(module_path) {
            return *module_id;
        }

        let next_id = self.module_id_by_path.len();
        if next_id >= MAX_DYNDBG_MODULES {
            return UNASSIGNED_MODULE_ID;
        }
        //写入新模块ID到映射表
        let module_id = next_id as u32;
        self.module_id_by_path.insert(module_path, module_id);
        module_id
    }

    // descriptor级别状态变化时的处理逻辑，更新模块计数器并在模块状态迁移时触发指令修补。
    fn apply_enabled_transition(&mut self, module_id: u32, old_enabled: bool, new_enabled: bool) {
        // 状态无变化直接返回
        if old_enabled == new_enabled {
            return;
        }

        // id无效直接返回
        let Some(module_state) = module_state(module_id) else {
            return;
        };

        let was_enabled = module_state.enabled_count.load(Ordering::Relaxed) != 0;

        // 新状态启用的话 计数器加一 否则减一(不减到0以下)
        if new_enabled {
            module_state.enabled_count.fetch_add(1, Ordering::Release);
        } else {
            saturating_fetch_sub_u32(&module_state.enabled_count);
        }

        let is_enabled = module_state.enabled_count.load(Ordering::Acquire) != 0;
        if was_enabled != is_enabled {
            self.on_module_state_transition(module_id, is_enabled);
        }
    }

    // 模块状态迁移时的处理逻辑，主要是进行指令修补。
    fn on_module_state_transition(&mut self, module_id: u32, enabled: bool) {
        let Some(module_key) = self.module_keys.get_mut(&module_id) else {
            return;
        };
        // 若模块状态没变 返回
        if module_key.enabled == enabled {
            return;
        }
        // 若模块状态迁移 则要指令修补
        module_key.enabled = enabled;
        patch_module_sites(module_key, enabled);
    }

    // 应用模块级的变化，触发必要的指令修补。
    fn apply_module_delta(&mut self, module_id: u32, delta: i64) {
        if delta == 0 {
            return;
        }

        // 拿计数器
        let Some(module_state) = module_state(module_id) else {
            return;
        };

        // 模块级更新计数器
        let was_enabled = module_state.enabled_count.load(Ordering::Relaxed) != 0;

        if delta > 0 {
            let delta_u64 = delta as u64;
            let amount = if delta_u64 > u32::MAX as u64 {
                u32::MAX
            } else {
                delta_u64 as u32
            };
            module_state.enabled_count.fetch_add(amount, Ordering::Release);
        } else {
            let amount_u64 = delta.unsigned_abs();
            let amount = if amount_u64 > u32::MAX as u64 {
                u32::MAX
            } else {
                amount_u64 as u32
            };
            saturating_fetch_sub_n_u32(&module_state.enabled_count, amount);
        }

        // 根据计数器新状态来判断是否需要修补
        let is_enabled = module_state.enabled_count.load(Ordering::Acquire) != 0;
        if was_enabled != is_enabled {
            self.on_module_state_transition(module_id, is_enabled);
        }
    }

    // 索引粗筛：挑出规则链可能影响到的descriptor并集。
    fn collect_candidates_for_rule_entries(
        &self,
        entries: &[DyndbgRuleEntry],
    ) -> Vec<&'static DebugDescriptor> {
        // If any selectorless rule exists, it can affect all descriptors.
        if entries.iter().any(|entry| !entry.rule.has_any_selector()) {
            return all_descriptors();
        }

        // Ablation: bypass candidate narrowing, return all descriptors.
        // When recompute is disabled, every rule update triggers full recomputation
        // of all descriptors, simulating Linux's fused O(n) behavior.
        if !get_dyndbg_recompute_enabled() {
            return all_descriptors();
        }

        let mut union = Vec::new();
        let mut seen = BTreeSet::new();

        for entry in entries {
            let matched = self.collect_candidates_for_rule(&entry.rule);
            for descriptor in matched {
                let id = descriptor_id(descriptor);
                if seen.insert(id) {
                    union.push(descriptor);
                }
            }
        }

        union
    }

    //单条规则来收集候选集
    //根据rule的各个selector在对应的索引表里查找匹配的descriptor列表，并取交集得到最终的候选列表
    fn collect_candidates_for_rule(&self, rule: &DyndbgRule) -> Vec<&'static DebugDescriptor> {
        // Ablation path: bypass index, linear-scan all descriptors.
        if !get_dyndbg_index_enabled() {
            return all_descriptors()
                .into_iter()
                .filter(|descriptor| rule.matches_descriptor(descriptor))
                .collect();
        }

        // Normal path: index-based candidate collection.
        let mut candidates: Option<Vec<&'static DebugDescriptor>> = None;

        if let Some(file_keyword) = &rule.file_keyword {
            let matched = collect_by_keyword_exact_first(
                &self.file_index,
                &self.file_segment_index,
                file_keyword,
                "/",
            );
            intersect_candidates(&mut candidates, matched);
        }

        if let Some(module_keyword) = &rule.module_keyword {
            let matched = collect_by_keyword_exact_first(
                &self.module_index,
                &self.module_segment_index,
                module_keyword,
                "::",
            );
            intersect_candidates(&mut candidates, matched);
        }

        if let Some(function_keyword) = &rule.function_keyword {
            let matched = collect_function_candidates(&self.function_index, function_keyword);
            intersect_candidates(&mut candidates, matched);
        }

        if let Some(line) = rule.line {
            let matched = self.line_index.get(&line).cloned().unwrap_or_default();
            intersect_candidates(&mut candidates, matched);
        }

        candidates.unwrap_or_else(all_descriptors)
    }

    // 最终的对单个描述符的裁决逻辑。
    // Log 和 Trace 是独立的两个维度，各自 last-match-wins，互不干扰。
    // Format flags 按规则链顺序增量模拟（Linux 语义）：初始 0，命中规则
    // 依次应用 set 位（+f）与 clear 位（-f），得到唯一目标值。不能做
    // 集合级累积（set/clear 合并会丢失顺序，如 -f 后 +f 结果应为设置）。
    fn matches_descriptor(&self, descriptor: &DebugDescriptor) -> (bool, bool, u8) {
        let mut log_enabled = DEFAULT_DEBUG_ENABLED;
        let mut trace_enabled = false;
        let mut flags = 0u8;
        for entry in &self.rules {
            if entry.rule.matches_descriptor(descriptor) {
                match entry.action {
                    DyndbgRuleAction::EnableLog => log_enabled = true,
                    DyndbgRuleAction::DisableLog => log_enabled = false,
                    DyndbgRuleAction::EnableTrace => trace_enabled = true,
                    DyndbgRuleAction::DisableTrace => trace_enabled = false,
                    DyndbgRuleAction::KeepState => {}
                }
                flags |= entry.rule.flags_set;
                flags &= !entry.rule.flags_clear;
                if let Some(v) = entry.rule.flags_override {
                    flags = v;
                }
            }
        }
        (log_enabled, trace_enabled, flags)
    }

}

// 获取descriptor的唯一id，这里直接使用其地址作为id，因为每个descriptor都是一个静态变量，地址唯一
fn descriptor_id(descriptor: &DebugDescriptor) -> usize {
    descriptor as *const DebugDescriptor as usize
}

// 将descriptor插入到指定的索引表里
fn insert_index_entry<K: Ord + Copy>(
    index: &mut BTreeMap<K, Vec<&'static DebugDescriptor>>,
    key: K,
    descriptor: &'static DebugDescriptor,
) {
    index.entry(key).or_default().push(descriptor);
}

//通过keyword在索引表里查找匹配的descriptor列表
/// 三通道候选收集（module/file 维度）：
/// 1. 完整值精确 → 主索引精确查找 O(log N)
/// 2. 段精确（短名/多段交集）→ 段倒排索引逐段精确查找 + 交集 O(k·log N)
/// 3. 通配符 → 主索引键 match_wildcard 扫描 O(m)
/// 结果与谓词 `value_matches` 一致（无 contains 兜底）。
fn collect_by_keyword_exact_first(
    index: &BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    segment_index: &BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    keyword: &str,
    sep: &str,
) -> Vec<&'static DebugDescriptor> {
    if !has_wildcard(keyword) {
        // 通道 1：完整值精确。
        if let Some(matched) = index.get(keyword) {
            return matched.clone();
        }
        // 通道 2：段精确——单段直接查；多段逐段查表后取交集。
        let segments: alloc::vec::Vec<&str> =
            keyword.split(sep).filter(|s| !s.is_empty()).collect();
        let mut seg_matched: Option<Vec<&'static DebugDescriptor>> = None;
        for seg in &segments {
            let Some(group) = segment_index.get(*seg) else {
                // 某段不在倒排表中 → 无描述符含该段。
                return Vec::new();
            };
            intersect_candidates(&mut seg_matched, group.clone());
        }
        if let Some(matched) = seg_matched {
            return matched;
        }
    }
    // 通道 3：通配符扫描主索引键。
    let mut matched = Vec::new();
    for (indexed_value, descriptors) in index {
        if match_wildcard(keyword, indexed_value) {
            matched.extend(descriptors.iter().copied());
        }
    }
    matched
}

/// 候选收集（function 维度）：函数名是原子值，无段概念——
/// 精确查找（O(log N)）→ 通配符扫描兜底。
fn collect_function_candidates(
    index: &BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    keyword: &str,
) -> Vec<&'static DebugDescriptor> {
    if !has_wildcard(keyword) {
        if let Some(matched) = index.get(keyword) {
            return matched.clone();
        }
        return Vec::new();
    }
    let mut matched = Vec::new();
    for (indexed_value, descriptors) in index {
        if match_wildcard(keyword, indexed_value) {
            matched.extend(descriptors.iter().copied());
        }
    }
    matched
}

//将新匹配的descriptor列表与已有的候选列表取交集，更新候选列表
fn intersect_candidates(
    candidates: &mut Option<Vec<&'static DebugDescriptor>>,
    matched: Vec<&'static DebugDescriptor>,
) {
    match candidates {
        None => {
            *candidates = Some(matched);
        }
        Some(existing) => {
            let matched_ids = matched
                .iter()
                .map(|descriptor| descriptor_id(descriptor))
                .collect::<BTreeSet<_>>();
            existing.retain(|descriptor| matched_ids.contains(&descriptor_id(descriptor)));
        }
    }
}

fn all_descriptors() -> Vec<&'static DebugDescriptor> {
    DYNDBG_DESCRIPTOR_REGISTRY.to_vec()
}

// 模块级别的指令修补：委托给 StaticKey 通用原语。
fn patch_module_sites(module_key: &ModuleKey, enabled: bool) {
    let sites: Vec<&StaticKeySite> = module_key.static_key_sites.iter().copied().collect();
    if sites.is_empty() {
        return;
    }

    let n_sites = sites.len();

    match get_dyndbg_patch_backend() {
        DyndbgPatchBackend::Batch => {
            if enabled {
                static_key::enable_static_keys(&sites);
            } else {
                static_key::disable_static_keys(&sites);
            }
            DYNDBG_PATCH_TRANSACTIONS.fetch_add(1, Ordering::Relaxed);
        }
        DyndbgPatchBackend::PerSite => {
            for site in &sites {
                if enabled {
                    static_key::enable_static_keys(core::slice::from_ref(site));
                } else {
                    static_key::disable_static_keys(core::slice::from_ref(site));
                }
            }
            DYNDBG_PATCH_TRANSACTIONS.fetch_add(n_sites as u64, Ordering::Relaxed);
        }
    }

    DYNDBG_MODULES_REPATCHED.fetch_add(1, Ordering::Relaxed);
    DYNDBG_SITES_PATCHED.fetch_add(n_sites as u64, Ordering::Relaxed);
}

// 安全的原子减法
fn saturating_fetch_sub_u32(counter: &AtomicU32) {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current == 0 {
            return;
        }
        match counter.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

// 安全的原子减法  处理一次性减amount
fn saturating_fetch_sub_n_u32(counter: &AtomicU32, amount: u32) {
    if amount == 0 {
        return;
    }

    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current == 0 {
            return;
        }

        let next = current.saturating_sub(amount);
        match counter.compare_exchange_weak(current, next, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

#[inline]
fn module_state(module_id: u32) -> Option<&'static ModuleState> {
    if module_id == UNASSIGNED_MODULE_ID {
        return None;
    }
    MODULE_STATES.get(module_id as usize)
}

#[inline]
fn module_enabled(module_id: u32) -> bool {
    module_state(module_id).is_some_and(|state| state.enabled_count.load(Ordering::Acquire) != 0)
}

#[derive(Debug)]
pub struct DebugDescriptor {
    log_enabled: AtomicBool,
    trace_enabled: AtomicBool,
    /// Format flags for log output prefixes (`+f/+l/+m/+t`), see
    /// [`FLAG_FUNCTION`] etc.  Read only on the enabled path.
    format_flags: AtomicU8,
    /// Index into the per-CPU hotspot counter array (assigned at boot in
    /// registry order).  `UNASSIGNED_HOT_INDEX` means "not counted".
    hot_index: AtomicU32,
    /// Source file path (e.g. `kernel/src/fs/ext2/dir.rs`).
    pub file: &'static str,
    /// Rust module path (e.g. `aster_kernel::fs::ext2::dir`).
    pub module_path: &'static str,
    module_id: AtomicU32,
    function: Option<fn() -> &'static str>,
    /// Source line number.
    pub line: u32,
}

impl DebugDescriptor {
    pub const fn new(
        file: &'static str,
        module_path: &'static str,
        function: Option<fn() -> &'static str>,
        line: u32,
    ) -> Self {
        Self {
            log_enabled: AtomicBool::new(DEFAULT_DEBUG_ENABLED),
            trace_enabled: AtomicBool::new(false),
            format_flags: AtomicU8::new(0),
            hot_index: AtomicU32::new(UNASSIGNED_HOT_INDEX),
            file,
            module_path,
            module_id: AtomicU32::new(UNASSIGNED_MODULE_ID),
            function,
            line,
        }
    }

    /// Set initial state for both modes (used during registration).
    fn init_enabled(&self, log_enabled: bool, trace_enabled: bool) {
        self.log_enabled.store(log_enabled, Ordering::Relaxed);
        self.trace_enabled.store(trace_enabled, Ordering::Relaxed);
    }

    fn init_module_id(&self, module_id: u32) {
        self.module_id.store(module_id, Ordering::Relaxed);
    }

    fn module_id(&self) -> u32 {
        self.module_id.load(Ordering::Acquire)
    }

    /// Assign the hotspot counter index (boot-time, registry order).
    fn init_hot_index(&self, hot_index: u32) {
        self.hot_index.store(hot_index, Ordering::Relaxed);
    }

    /// Hotspot counter index, or `UNASSIGNED_HOT_INDEX` when not counted.
    #[inline]
    pub fn hot_index(&self) -> u32 {
        self.hot_index.load(Ordering::Relaxed)
    }

    /// Atomically update both mode flags; returns the effective transition.
    ///
    /// Returns `(old_effective, new_effective)` where "effective" means
    /// *any* mode is enabled.  A change in effective state triggers the
    /// module-level counter update and potential instruction patching.
    fn update_enabled(&self, log_enabled: bool, trace_enabled: bool) -> (bool, bool) {
        let old_log = self.log_enabled.swap(log_enabled, Ordering::AcqRel);
        let old_trace = self.trace_enabled.swap(trace_enabled, Ordering::AcqRel);
        let old_effective = old_log || old_trace;
        let new_effective = log_enabled || trace_enabled;
        (old_effective, new_effective)
    }

    /// True when this descriptor is in Log mode (used by the fast-path
    /// `dyndbg_should_log` guard).
    #[inline]
    pub fn should_log_fast(&self) -> bool {
        self.log_enabled.load(Ordering::Acquire)
    }

    /// True when this descriptor is in Trace mode (used by the fast-path
    /// trace hook).
    #[inline]
    pub fn should_trace_fast(&self) -> bool {
        self.trace_enabled.load(Ordering::Acquire)
    }

    /// Overwrite the format flags with the target value computed by replaying
    /// the rule chain ([`crate::aster_logger::DyndbgState::matches_descriptor`]).
    /// Called on the control path (rule updates) — the enabled path only reads
    /// via [`Self::format_flags`].  A plain store is safe: control-path updates
    /// are serialised under the dyndbg state lock and the enabled path never
    /// writes this field.
    #[inline]
    pub fn set_format_flags(&self, flags: u8) {
        self.format_flags.store(flags, Ordering::Relaxed);
    }

    /// Read the current format flags (`+f/+l/+m/+t` bits).
    ///
    /// Only meaningful on the enabled (JMP) path; the disabled NOP5 path
    /// never executes this.
    #[inline]
    pub fn format_flags(&self) -> u8 {
        self.format_flags.load(Ordering::Relaxed)
    }

    /// Return the function name (without module path), e.g. `ext2_read`.
    #[inline]
    pub fn function_name(&self) -> Option<&'static str> {
        self.function.map(|provider| provider())
    }
}

pub fn dyndbg_should_log(descriptor: &'static DebugDescriptor) -> bool {
    // 目前仅支持了x86的static patch实现 故此时在dyndbg(x86_64)路径下模块门控为冗余
    // 但非x86的路径下依然算是优化 故暂时保留
    if !module_enabled(descriptor.module_id()) {
        return false;
    }

    descriptor.should_log_fast()
}

/// Fast-path guard for trace mode: checks module gate + descriptor trace flag.
///
/// Called from the dyndbg macro label block; when both the module gate and the
/// per-descriptor flag pass, the call site pushes a structured event into the
/// trace ring buffer.
#[inline]
pub fn dyndbg_should_trace(descriptor: &'static DebugDescriptor) -> bool {
    if !module_enabled(descriptor.module_id()) {
        return false;
    }

    descriptor.should_trace_fast()
}

/// Render the log message with the format prefixes enabled by `+f/+l/+m/+t`.
///
/// Prefix order is unified with the descriptor status listing
/// (`cat /proc/sys/kernel/dynamic_debug`): `file:line [module] function
/// [task=0x...]`.  Only called on the enabled (JMP) path — the disabled NOP5
/// path never reaches here, so the extra formatting cost is confined to
/// enabled logging.
///
/// `task_ptr` is the address of the current `ostd::Task` (a stable per-thread
/// identifier, same convention as the scheduler trace events); it is computed
/// by the caller because the logger crate must not depend on the kernel's
/// thread API.  `None` means "no thread context" (e.g. bootstrap).
pub fn format_dyndbg_log(
    descriptor: &DebugDescriptor,
    args: core::fmt::Arguments,
    task_ptr: Option<usize>,
) -> String {
    let mut buf = String::new();
    let flags = descriptor.format_flags();

    if flags & FLAG_LINE != 0 {
        buf.push_str(descriptor.file);
        buf.push(':');
        buf.push_str(&alloc::string::ToString::to_string(&descriptor.line));
        buf.push(' ');
    }
    if flags & FLAG_MODULE != 0 {
        buf.push('[');
        buf.push_str(descriptor.module_path);
        buf.push_str("] ");
    }
    if flags & FLAG_FUNCTION != 0 {
        buf.push_str(descriptor.function_name().unwrap_or("<unknown>"));
        buf.push(' ');
    }
    if flags & FLAG_THREAD != 0 {
        if let Some(task_ptr) = task_ptr {
            let _ = alloc::fmt::write(&mut buf, format_args!("[task=0x{:x}] ", task_ptr));
        }
    }
    // Formatting errors are impossible for a String sink; unwrap keeps the
    // message rendering identical to a plain log! call.
    let _ = alloc::fmt::write(&mut buf, args);
    buf
}

/// Zero-overhead literal emit: the caller site passes only the static fmt
/// string (loaded by the label block with a register-only `mov`), so the
/// disabled NOP5 path materializes nothing.  All formatting/allocation
/// happens in this dedicated stack frame, isolated from the call site.
///
/// `fmt` arrives as `&&str`: the label block loads the *address of* the
/// static `&str` slot (a thin pointer that fits a register), so the first
/// thing we do is dereference it back to the string.
#[inline(never)]
pub fn __dyndbg_label_emit_const(
    descriptor: &'static DebugDescriptor,
    fmt: &'static &'static str,
) {
    // Call sites using the literal branch are expected to pass a plain
    // message without format placeholders (inline captures are rewritten to
    // explicit arguments so they take the general branch instead).
    let fmt: &'static str = fmt;
    if descriptor.should_log_fast() {
        let __flags = descriptor.format_flags();
        if __flags == 0 {
            log::debug!("{}", fmt);
        } else {
            let __task_ptr = if __flags & crate::FLAG_THREAD != 0 {
                ostd::task::Task::current()
                    .map(|__t| (&*__t as *const _) as usize)
            } else {
                None
            };
            // format_args! here constructs in this frame (never merged with
            // caller slots), keeping the caller's disabled path instruction-free.
            log::debug!(
                "{}",
                format_dyndbg_log(descriptor, format_args!("{}", fmt), __task_ptr)
            );
        }
    }
    if descriptor.should_trace_fast() {
        crate::dyndbg_trace::push_trace_event(descriptor);
    }
}

/// asm label 块的 emit 封装（inline(never)）：格式化/分配全部在独立栈帧
/// 中进行。asm label 机制下编译器可能把 label 块与调用点共享栈槽，
/// 内联的 String 缓冲会覆盖调用点保存的寄存器——独立函数彻底隔离。
#[inline(never)]
pub fn __dyndbg_label_emit(
    descriptor: &'static DebugDescriptor,
    args: &core::fmt::Arguments,
) {
    if descriptor.should_log_fast() {
        let __flags = descriptor.format_flags();
        if __flags == 0 {
            log::debug!("{}", args);
        } else {
            let __task_ptr = if __flags & crate::FLAG_THREAD != 0 {
                ostd::task::Task::current()
                    .map(|__t| (&*__t as *const _) as usize)
            } else {
                None
            };
            log::debug!(
                "{}",
                format_dyndbg_log(descriptor, *args, __task_ptr)
            );
        }
    }
    if descriptor.should_trace_fast() {
        crate::dyndbg_trace::push_trace_event(descriptor);
    }
}

/// Single module-gate check for use in dyndbg macro label blocks.
///
/// In the x86_64 NOP5/JMP path this is always `true` (the JMP is only
/// patched in when the module is enabled).  On non-x86 architectures it
/// serves as a cheap early-exit before the per-descriptor mode checks.
#[inline]
pub fn dyndbg_module_enabled(descriptor: &DebugDescriptor) -> bool {
    module_enabled(descriptor.module_id())
}

fn pre_register_dyndbg_descriptors() {
    let mut state = DYNDBG_STATE.lock();
    for descriptor in DYNDBG_DESCRIPTOR_REGISTRY {
        state.register_descriptor(descriptor);
    }
}

fn pre_register_dyndbg_keys() {
    let mut state = DYNDBG_STATE.lock();
    for mapping in DYNDBG_KEY_MAPPING {
        state.register_dyndbg_key(mapping);
    }
}

impl log::Log for AsterLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let timestamp = Jiffies::elapsed().as_duration();
        print_logs(record, &timestamp);
    }

    fn flush(&self) {}
}

#[cfg(feature = "log_color")]
fn print_logs(record: &Record, timestamp: &Duration) {
    use owo_colors::Style;

    let secs = timestamp.as_secs();
    let millis = timestamp.subsec_millis();

    let timestamp_style = Style::new().green();
    let record_style = Style::new().default_color();
    let level_style = match record.level() {
        log::Level::Error => Style::new().red(),
        log::Level::Warn => Style::new().bright_yellow(),
        log::Level::Info => Style::new().blue(),
        log::Level::Debug => Style::new().bright_green(),
        log::Level::Trace => Style::new().bright_black(),
    };

    super::_print(format_args!(
        "{} {:<5}: {}\n",
        timestamp_style.style(format_args!("[{:>6}.{:03}]", secs, millis)),
        level_style.style(record.level()),
        record_style.style(record.args())
    ));
}

#[cfg(not(feature = "log_color"))]
fn print_logs(record: &Record, timestamp: &Duration) {
    let secs = timestamp.as_secs();
    let millis = timestamp.subsec_millis();

    super::_print(format_args!(
        "{} {:<5}: {}\n",
        format_args!("[{:>6}.{:03}]", secs, millis),
        record.level(),
        record.args()
    ));
}

// 对外暴露快照 外部通过快照来查看和设置规则，避免直接暴露内部的Rule结构，减少耦合
#[derive(Debug, Clone, Default)]
pub struct DyndbgRuleSnapshot {
    pub file_keyword: Option<String>,
    pub module_keyword: Option<String>,
    pub function_keyword: Option<String>,
    pub line: Option<u32>,
    /// Format-flag bits to set on matched descriptors (`+f/+l/+m/+t`).
    pub flags_set: u8,
    /// Format-flag bits to clear on matched descriptors (`-f/-l/-m/-t`).
    pub flags_clear: u8,
    /// Exact format-flag value to overwrite on matched descriptors (`=fl`).
    pub flags_override: Option<u8>,
}

/// Public mirror of [`DyndbgRuleAction`] for the snapshot API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DyndbgRuleActionSnapshot {
    /// Enable log output (`+p`).
    EnableLog,
    /// Disable log output (`-p`).
    DisableLog,
    /// Enable structured tracing (`+trace`).
    EnableTrace,
    /// Disable structured tracing (`-trace`).
    DisableTrace,
    /// Flags-only rule (`+f` etc.): format flags only, switch untouched.
    KeepState,
}

#[derive(Debug, Clone)]
pub struct DyndbgRuleEntrySnapshot {
    pub rule: DyndbgRuleSnapshot,
    pub action: DyndbgRuleActionSnapshot,
}

#[derive(Debug, Clone)]
pub struct DyndbgStatsSnapshot {
    pub descriptors_recomputed: u64,
    pub modules_repatched: u64,
    pub sites_patched: u64,
    pub patch_transactions: u64,
    pub last_update_latency_us: u64,
}

impl From<DyndbgRuleSnapshot> for DyndbgRule {
    fn from(snapshot: DyndbgRuleSnapshot) -> Self {
        Self {
            file_keyword: snapshot.file_keyword,
            module_keyword: snapshot.module_keyword,
            function_keyword: snapshot.function_keyword,
            line: snapshot.line,
            flags_set: snapshot.flags_set,
            flags_clear: snapshot.flags_clear,
            flags_override: snapshot.flags_override,
        }
    }
}

impl From<&DyndbgRule> for DyndbgRuleSnapshot {
    fn from(rule: &DyndbgRule) -> Self {
        Self {
            file_keyword: rule.file_keyword.clone(),
            module_keyword: rule.module_keyword.clone(),
            function_keyword: rule.function_keyword.clone(),
            line: rule.line,
            flags_set: rule.flags_set,
            flags_clear: rule.flags_clear,
            flags_override: rule.flags_override,
        }
    }
}

impl From<DyndbgRuleEntrySnapshot> for DyndbgRuleEntry {
    fn from(snapshot: DyndbgRuleEntrySnapshot) -> Self {
        Self {
            rule: snapshot.rule.into(),
            action: match snapshot.action {
                DyndbgRuleActionSnapshot::EnableLog => DyndbgRuleAction::EnableLog,
                DyndbgRuleActionSnapshot::DisableLog => DyndbgRuleAction::DisableLog,
                DyndbgRuleActionSnapshot::EnableTrace => DyndbgRuleAction::EnableTrace,
                DyndbgRuleActionSnapshot::DisableTrace => DyndbgRuleAction::DisableTrace,
                DyndbgRuleActionSnapshot::KeepState => DyndbgRuleAction::KeepState,
            },
        }
    }
}

impl From<&DyndbgRuleEntry> for DyndbgRuleEntrySnapshot {
    fn from(entry: &DyndbgRuleEntry) -> Self {
        Self {
            rule: DyndbgRuleSnapshot::from(&entry.rule),
            action: match entry.action {
                DyndbgRuleAction::EnableLog => DyndbgRuleActionSnapshot::EnableLog,
                DyndbgRuleAction::DisableLog => DyndbgRuleActionSnapshot::DisableLog,
                DyndbgRuleAction::EnableTrace => DyndbgRuleActionSnapshot::EnableTrace,
                DyndbgRuleAction::DisableTrace => DyndbgRuleActionSnapshot::DisableTrace,
                DyndbgRuleAction::KeepState => DyndbgRuleActionSnapshot::KeepState,
            },
        }
    }
}

// 获取规则链最后一条规则快照
pub fn get_dyndbg_rule_snapshot() -> DyndbgRuleSnapshot {
    let state = DYNDBG_STATE.lock();
    state
        .rules
        .last()
        .map(|entry| DyndbgRuleSnapshot::from(&entry.rule))
        .unwrap_or_default()
}

pub fn get_dyndbg_stats_snapshot() -> DyndbgStatsSnapshot {
    DyndbgStatsSnapshot {
        descriptors_recomputed: DYNDBG_DESCRIPTORS_RECOMPUTED.load(Ordering::Relaxed),
        modules_repatched: DYNDBG_MODULES_REPATCHED.load(Ordering::Relaxed),
        sites_patched: DYNDBG_SITES_PATCHED.load(Ordering::Relaxed),
        patch_transactions: DYNDBG_PATCH_TRANSACTIONS.load(Ordering::Relaxed),
        last_update_latency_us: DYNDBG_LAST_UPDATE_LATENCY_US.load(Ordering::Relaxed),
    }
}

pub fn reset_dyndbg_stats() {
    DYNDBG_DESCRIPTORS_RECOMPUTED.store(0, Ordering::Relaxed);
    DYNDBG_MODULES_REPATCHED.store(0, Ordering::Relaxed);
    DYNDBG_SITES_PATCHED.store(0, Ordering::Relaxed);
    DYNDBG_PATCH_TRANSACTIONS.store(0, Ordering::Relaxed);
    DYNDBG_LAST_UPDATE_LATENCY_US.store(0, Ordering::Relaxed);
}

//清空规则链
pub fn clear_dyndbg_rule() {
    clear_dyndbg_rules();
}

// 向规则链追加规则（增量路径：链尾必胜，免重放）
pub fn append_dyndbg_rule(snapshot: DyndbgRuleSnapshot, action: DyndbgRuleActionSnapshot) {
    let mut state = DYNDBG_STATE.lock();
    let new_entry: DyndbgRuleEntry = DyndbgRuleEntrySnapshot {
        rule: snapshot,
        action,
    }
    .into();
    let affected = state.collect_candidates_for_rule_entries(core::slice::from_ref(&new_entry));

    state.rules.push(new_entry);
    // 先取出新规则的动作/flags（传值避免与 &mut self 的借用冲突），
    // 再对命中集做增量应用。
    let (new_action, flags_set, flags_clear, flags_override) = {
        let entry = state.rules.last().unwrap();
        (
            entry.action,
            entry.rule.flags_set,
            entry.rule.flags_clear,
            entry.rule.flags_override,
        )
    };
    let new_rule = state.rules.last().unwrap().rule.clone();
    state.refresh_registered_descriptors_incremental(
        affected,
        &new_rule,
        new_action,
        flags_set,
        flags_clear,
        flags_override,
    );
}

// 获取整个规则链快照
pub fn get_dyndbg_rule_chain_snapshot() -> Vec<DyndbgRuleEntrySnapshot> {
    let state = DYNDBG_STATE.lock();
    state
        .rules
        .iter()
        .map(DyndbgRuleEntrySnapshot::from)
        .collect()
}

pub fn clear_dyndbg_rules() {
    let mut state = DYNDBG_STATE.lock();
    let old_rules = state.rules.clone();
    state.rules.clear();
    let affected = state.collect_candidates_for_rule_entries(&old_rules);
    state.refresh_registered_descriptors(affected);
}

// 通过vec的id删除规则
pub fn remove_dyndbg_rule_by_id(rule_id: usize) -> bool {
    let mut state = DYNDBG_STATE.lock();
    if rule_id >= state.rules.len() {
        return false;
    }

    let removed_entry = state.rules.remove(rule_id);
    let affected = state.collect_candidates_for_rule_entries(core::slice::from_ref(&removed_entry));
    state.refresh_registered_descriptors(affected);
    true
}

// 内部发射宏：按 descriptor 的 format flags 决定日志输出方式。
// 无 flags（默认）走原路径 log::debug!(args)，零额外开销；
// 有 flags 时拼接 +f/+l/+m/+t 前缀。仅在启用态（JMP 路径）执行。
#[doc(hidden)]
#[macro_export]
macro_rules! __dyndbg_emit_log {
    ($descriptor:expr, $($arg:tt)+) => {{
        let __flags = $descriptor.format_flags();
        if __flags == 0 {
            log::debug!($($arg)+);
        } else {
            let __task_ptr = if __flags & $crate::FLAG_THREAD != 0 {
                ostd::task::Task::current()
                    .map(|__t| (&*__t as *const _) as usize)
            } else {
                None
            };
            log::debug!(
                "{}",
                $crate::format_dyndbg_log(
                    &$descriptor,
                    format_args!($($arg)+),
                    __task_ptr,
                )
            );
        }
    }};
}

// Unified `dyndbg_debug!` macro: choose backend at compile time via features.
#[macro_export]
macro_rules! dyndbg_debug {
    // Zero-overhead literal branch: constant message without arguments.
    // The format string lives in a static symbol that the label block loads
    // with a register-only `mov` (no stack writes), so the disabled NOP5
    // path executes ZERO extra instructions — `nopl; ret`, identical to the
    // compiled-out baseline.  Arguments are constructed inside the emit
    // function's own stack frame (never merged with host slots).
    ($s:literal) => {{
        #[cfg(feature = "dyndbg")]
        {
            fn __dyndbg_function_name() -> &'static str {
                fn __dyndbg_fn_marker() {}
                let full = core::any::type_name_of_val(&__dyndbg_fn_marker);
                let path = full
                    .strip_suffix("::__dyndbg_fn_marker")
                    .and_then(|p| p.strip_suffix("::__dyndbg_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__dyndbg_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            #[cfg(target_arch = "x86_64")]
            {
                static __DYNDBG_SITE_FMT: &str = $s;
                #[allow(unsafe_code)]
                // SAFETY: Same structure as the general branch below — an
                // exact 5-byte patch slot plus a register-only label block.
                // The block loads the static fmt/descriptor addresses into
                // registers and calls the emit helper; no stack writes in
                // this frame, so no slot-merge hazard, and the disabled path
                // materializes nothing at all.
                unsafe {
                    core::arch::asm!(
                        concat!(
                            ".ifndef \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\"\n",
                            ".weak \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\"\n",
                            "\"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\":\n",
                            ".endif\n",
                            ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                            ".if 0\n",
                            "jmp {0}\n",
                            ".endif\n",
                        ),
                        label {
                            #[allow(unsafe_code)]
                            // SAFETY: This inner asm only defines a global symbol at the
                            // debug block entry for patching targets.
                            unsafe {
                                core::arch::asm!(
                                    concat!(
                                        ".ifndef \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\"\n",
                                        ".weak \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\"\n",
                                        "\"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\":\n",
                                        ".endif\n",
                                    ),
                                    options(nomem, nostack)
                                );
                            }
                            #[allow(unsafe_code)]
                            // SAFETY: Register-only call; the static addresses
                            // are materialized as immediates inside the block.
                            // clobber_abi models the call on the main path.
                            unsafe {
                                core::arch::asm!(
                                    "call {emit}",
                                    emit = in(reg) $crate::__dyndbg_label_emit_const as *const (),
                                    in("rdi") &DESCRIPTOR,
                                    in("rsi") &__DYNDBG_SITE_FMT,
                                    options()
                                );
                            }
                        },
                        clobber_abi("sysv64"),
                        options()
                    );
                }
            }
                // SAFETY: The following extern symbols are declared to refer to
                // the labels emitted by the asm block above. They are not real
                // functions to be called by C; we only take their addresses for
                // patch registration and must ensure the asm defines them.
                unsafe extern "C" {
                    #[link_name = concat!(
                        "__dyndbg_site_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!()
                    )]
                    fn __dyndbg_site() -> bool;
                    #[link_name = concat!(
                        "__dyndbg_target_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!()
                    )]
                    fn __dyndbg_target() -> bool;
                }

                static STATIC_KEY_SITE: ostd::arch::static_key::StaticKeySite =
                    ostd::arch::static_key::StaticKeySite::new(
                        __dyndbg_site,
                        __dyndbg_target,
                        "dyndbg",
                    );
                #[ostd::distributed_slice(ostd::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
                static STATIC_KEY_REG_ENTRY: &ostd::arch::static_key::StaticKeySite =
                    &STATIC_KEY_SITE;

                static DYNDBG_KEY_MAPPING: $crate::DyndbgKeyMapping =
                    $crate::DyndbgKeyMapping {
                        descriptor: &DESCRIPTOR,
                        static_key_site: &STATIC_KEY_SITE,
                    };
                #[$crate::distributed_slice($crate::DYNDBG_KEY_MAPPING)]
                static DYNDBG_KEY_MAPPING_ENTRY: &'static $crate::DyndbgKeyMapping =
                    &DYNDBG_KEY_MAPPING;

            #[cfg(not(target_arch = "x86_64"))]
            {
                if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                    $crate::__dyndbg_label_emit_const(&DESCRIPTOR, $s);
                }
            }

        }

        // Branch-based backend: same static-fmt path (no patching).
        #[cfg(all(not(feature = "dyndbg"), feature = "branchdbg"))]
        {
            fn __branch_function_name() -> &'static str {
                fn __branch_fn_marker() {}
                let full = core::any::type_name_of_val(&__branch_fn_marker);
                let path = full
                    .strip_suffix("::__branch_fn_marker")
                    .and_then(|p| p.strip_suffix("::__branch_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__branch_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                $crate::__dyndbg_label_emit_const(&DESCRIPTOR, $s);
            }
        }

        // No-op backend (neither feature enabled)
        #[cfg(not(any(feature = "dyndbg", feature = "branchdbg")))]
        {
            #[cfg(target_arch = "x86_64")]
            {
                #[allow(unsafe_code)]
                // SAFETY: See the general branch: empty asm aligns LLVM's
                // optimization behavior with the static-patch path.
                unsafe {
                    core::arch::asm!("", options());
                }
            }
        }
    }};

    // General branch: dynamic arguments need format_args! materialized in the
    // NORMAL path (label-block construction triggers the asm-goto slot-merge
    // bug), which costs the disabled path a few instructions per site.
    ($($arg:tt)+) => {{
        // Patch-based backend (static patch site + descriptor)
        #[cfg(feature = "dyndbg")]
        {
            // 获取当前函数完整名称（包含模块路径）
            fn __dyndbg_function_name() -> &'static str {
                fn __dyndbg_fn_marker() {}
                let full = core::any::type_name_of_val(&__dyndbg_fn_marker);
                // "crate::mod::user_fn::__dyndbg_function_name::__dyndbg_fn_marker"
                // -> strip the two internal items -> "crate::mod::user_fn" -> "user_fn".
                // func matching is atomic (no segment channel under the
                // three-channel engine), so the descriptor stores the short
                // function name; partial names need the wildcard channel.
                let path = full
                    .strip_suffix("::__dyndbg_fn_marker")
                    .and_then(|p| p.strip_suffix("::__dyndbg_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__dyndbg_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            #[cfg(target_arch = "x86_64")]
            {
                // Materialize the Arguments in the NORMAL path: LLVM treats
                // asm-goto label blocks as unreachable cold code and merges
                // their stack slots with live host slots, so constructing the
                // Arguments inside the label block corrupted the host frame
                // (e.g. symlink-target String in lookup_from_parent got
                // overwritten -> free(rodata) -> heap corruption). With
                // normal-path liveness the temporary can never merge, and the
                // label block reads it via the register operand below.
                let __dyndbg_args = &format_args!($($arg)+);
                #[allow(unsafe_code)]
                    // SAFETY: The inline asm emits an exact 5-byte patch slot at the
                    // call-site and declares a possible branch target used only by
                    // static patching. The label block only performs a register-only
                    // call (no stack writes in this frame): rdi/rsi arrive via the
                    // outer asm's input operands (callbr live-in semantics).
                    unsafe {
                        core::arch::asm!(
                        concat!(
                            ".ifndef \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\"\n",
                            ".weak \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\"\n",
                            "\"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\":\n",
                            ".endif\n",
                            ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                            ".if 0\n",
                            "jmp {0}\n",
                            ".endif\n",
                        ),
                        label {
                            #[allow(unsafe_code)]
                            // SAFETY: This inner asm only defines a global symbol at the
                            // debug block entry for patching targets.
                            unsafe {
                                core::arch::asm!(
                                    concat!(
                                        ".ifndef \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\"\n",
                                        ".weak \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\"\n",
                                        "\"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\":\n",
                                        ".endif\n",
                                    ),
                                    options(nomem, nostack)
                                );
                            }
                            // SAFETY: JMP is only patched in when the module is
                            // enabled, so a module-gate check is redundant here.
                            // The call is register-only: the address operand is
                            // materialized into a scratch register inside the
                            // block; no stack slots are touched in this frame.
                            #[allow(unsafe_code)]
                            unsafe {
                                core::arch::asm!(
                                    "call {emit}",
                                    emit = in(reg) $crate::__dyndbg_label_emit as *const (),
                                    in("rdi") &DESCRIPTOR,
                                    // rsi = args: reloaded from the normal-path
                                    // stack slot inside the block; no callee-saved
                                    // register carries it across the site.
                                    in("rsi") __dyndbg_args,
                                    options()
                                );
                            }
                        },
                        // clobber_abi models the label-block call's register
                        // clobbers on the MAIN path: LLVM treats asm-goto label
                        // blocks as unreachable, so without this the rejoin path
                        // reused caller-saved registers that the runtime call
                        // had clobbered.  The args pointer stays in its stack
                        // slot on the normal path (the inner asm reloads it),
                        // keeping the disabled NOP5 path free of the extra
                        // register save/restore a callee-saved carrier would
                        // cost.
                        clobber_abi("sysv64"),
                        options()
                    );
                }

                // SAFETY: The following extern symbols are declared to refer to
                // the labels emitted by the asm block above. They are not real
                // functions to be called by C; we only take their addresses for
                // patch registration and must ensure the asm defines them.
                unsafe extern "C" {
                    #[link_name = concat!(
                        "__dyndbg_site_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!()
                    )]
                    fn __dyndbg_site() -> bool;
                    #[link_name = concat!(
                        "__dyndbg_target_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!()
                    )]
                    fn __dyndbg_target() -> bool;
                }

                static STATIC_KEY_SITE: ostd::arch::static_key::StaticKeySite =
                    ostd::arch::static_key::StaticKeySite::new(
                        __dyndbg_site,
                        __dyndbg_target,
                        "dyndbg",
                    );
                #[ostd::distributed_slice(ostd::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
                static STATIC_KEY_REG_ENTRY: &ostd::arch::static_key::StaticKeySite =
                    &STATIC_KEY_SITE;

                static DYNDBG_KEY_MAPPING: $crate::DyndbgKeyMapping =
                    $crate::DyndbgKeyMapping {
                        descriptor: &DESCRIPTOR,
                        static_key_site: &STATIC_KEY_SITE,
                    };
                #[$crate::distributed_slice($crate::DYNDBG_KEY_MAPPING)]
                static DYNDBG_KEY_MAPPING_ENTRY: &'static $crate::DyndbgKeyMapping =
                    &DYNDBG_KEY_MAPPING;
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                    $crate::__dyndbg_label_emit(&DESCRIPTOR, &format_args!($($arg)+));
                }
            }
        }

        // Branch-based backend: descriptor + fast runtime check (no patching)
        #[cfg(all(not(feature = "dyndbg"), feature = "branchdbg"))]
        {
            fn __branch_function_name() -> &'static str {
                fn __branch_fn_marker() {}
                let full = core::any::type_name_of_val(&__branch_fn_marker);
                let path = full
                    .strip_suffix("::__branch_fn_marker")
                    .and_then(|p| p.strip_suffix("::__branch_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__branch_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                $crate::__dyndbg_label_emit(&DESCRIPTOR, &format_args!($($arg)+));
            }
        }

        // No-op backend (neither feature enabled)
        #[cfg(not(any(feature = "dyndbg", feature = "branchdbg")))]
        {
            // Align compiler optimization behavior with the dyndbg backend:
            // an empty asm block with the same options tells LLVM this code
            // does not access memory or stack, matching the NOP5 path.
            #[cfg(target_arch = "x86_64")]
            {
                #[allow(unsafe_code)]
                // SAFETY: This empty asm block generates no instructions; it only
                // provides optimization hints so the no-op path is not penalized
                // relative to the static-patch path.
                unsafe {
                    core::arch::asm!("", options());
                }
            }
        }
    }};
}

// 为需要独立 site 标识的场景提供带后缀版本，避免宏展开后符号重名。
#[macro_export]
macro_rules! dyndbg_debug_site {
    // Zero-overhead literal branch (site variant): constant message
    // without arguments; same zero-instruction disabled path as dyndbg_debug!.
    // The format string lives in a static symbol that the label block loads
    // with a register-only `mov` (no stack writes), so the disabled NOP5
    // path executes ZERO extra instructions — `nopl; ret`, identical to the
    // compiled-out baseline.  Arguments are constructed inside the emit
    // function's own stack frame (never merged with host slots).
    ($site:literal, $s:literal) => {{
        #[cfg(feature = "dyndbg")]
        {
            fn __dyndbg_function_name() -> &'static str {
                fn __dyndbg_fn_marker() {}
                let full = core::any::type_name_of_val(&__dyndbg_fn_marker);
                let path = full
                    .strip_suffix("::__dyndbg_fn_marker")
                    .and_then(|p| p.strip_suffix("::__dyndbg_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__dyndbg_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            #[cfg(target_arch = "x86_64")]
            {
                static __DYNDBG_SITE_FMT: &str = $s;
                #[allow(unsafe_code)]
                // SAFETY: Same structure as the general branch below — an
                // exact 5-byte patch slot plus a register-only label block.
                // The block loads the static fmt/descriptor addresses into
                // registers and calls the emit helper; no stack writes in
                // this frame, so no slot-merge hazard, and the disabled path
                // materializes nothing at all.
                unsafe {
                    core::arch::asm!(
                        concat!(
                            ".ifndef \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
 "_",
 $site,
 "\"\n",
                            ".weak \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
 "_",
 $site,
 "\"\n",
                            "\"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
 "_",
 $site,
 "\":\n",
                            ".endif\n",
                            ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                            ".if 0\n",
                            "jmp {0}\n",
                            ".endif\n",
                        ),
                        label {
                            #[allow(unsafe_code)]
                            // SAFETY: This inner asm only defines a global symbol at the
                            // debug block entry for patching targets.
                            unsafe {
                                core::arch::asm!(
                                    concat!(
                                        ".ifndef \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
 "_",
 $site,
 "\"\n",
                                        ".weak \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
 "_",
 $site,
 "\"\n",
                                        "\"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
 "_",
 $site,
 "\":\n",
                                        ".endif\n",
                                    ),
                                    options(nomem, nostack)
                                );
                            }
                            #[allow(unsafe_code)]
                            // SAFETY: Register-only call; the static addresses
                            // are materialized as immediates inside the block.
                            // clobber_abi models the call on the main path.
                            unsafe {
                                core::arch::asm!(
                                    "call {emit}",
                                    emit = in(reg) $crate::__dyndbg_label_emit_const as *const (),
                                    in("rdi") &DESCRIPTOR,
                                    in("rsi") &__DYNDBG_SITE_FMT,
                                    options()
                                );
                            }
                        },
                        clobber_abi("sysv64"),
                        options()
                    );
                }
            }
                // SAFETY: The following extern symbols are declared to refer to
                // the labels emitted by the asm block above. They are not real
                // functions to be called by C; we only take their addresses for
                // patch registration and must ensure the asm defines them.
                unsafe extern "C" {
                    #[link_name = concat!(
                        "__dyndbg_site_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!(), "_", $site)]
                    fn __dyndbg_site() -> bool;
                    #[link_name = concat!(
                        "__dyndbg_target_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!(), "_", $site)]
                    fn __dyndbg_target() -> bool;
                }

                static STATIC_KEY_SITE: ostd::arch::static_key::StaticKeySite =
                    ostd::arch::static_key::StaticKeySite::new(
                        __dyndbg_site,
                        __dyndbg_target,
                        "dyndbg",
                    );
                #[ostd::distributed_slice(ostd::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
                static STATIC_KEY_REG_ENTRY: &ostd::arch::static_key::StaticKeySite =
                    &STATIC_KEY_SITE;

                static DYNDBG_KEY_MAPPING: $crate::DyndbgKeyMapping =
                    $crate::DyndbgKeyMapping {
                        descriptor: &DESCRIPTOR,
                        static_key_site: &STATIC_KEY_SITE,
                    };
                #[$crate::distributed_slice($crate::DYNDBG_KEY_MAPPING)]
                static DYNDBG_KEY_MAPPING_ENTRY: &'static $crate::DyndbgKeyMapping =
                    &DYNDBG_KEY_MAPPING;

            #[cfg(not(target_arch = "x86_64"))]
            {
                if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                    $crate::__dyndbg_label_emit_const(&DESCRIPTOR, $s);
                }
            }

        }

        // Branch-based backend: same static-fmt path (no patching).
        #[cfg(all(not(feature = "dyndbg"), feature = "branchdbg"))]
        {
            fn __branch_function_name() -> &'static str {
                fn __branch_fn_marker() {}
                let full = core::any::type_name_of_val(&__branch_fn_marker);
                let path = full
                    .strip_suffix("::__branch_fn_marker")
                    .and_then(|p| p.strip_suffix("::__branch_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__branch_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                $crate::__dyndbg_label_emit_const(&DESCRIPTOR, $s);
            }
        }

        // No-op backend (neither feature enabled)
        #[cfg(not(any(feature = "dyndbg", feature = "branchdbg")))]
        {
            #[cfg(target_arch = "x86_64")]
            {
                #[allow(unsafe_code)]
                // SAFETY: See the general branch: empty asm aligns LLVM's
                // optimization behavior with the static-patch path.
                unsafe {
                    core::arch::asm!("", options());
                }
            }
        }
    }};

    ($site:literal, $($arg:tt)+) => {{
        #[cfg(feature = "dyndbg")]
        {
            fn __dyndbg_function_name() -> &'static str {
                fn __dyndbg_fn_marker() {}
                let full = core::any::type_name_of_val(&__dyndbg_fn_marker);
                // "crate::mod::user_fn::__dyndbg_function_name::__dyndbg_fn_marker"
                // -> strip the two internal items -> "crate::mod::user_fn" -> "user_fn".
                // func matching is atomic (no segment channel under the
                // three-channel engine), so the descriptor stores the short
                // function name; partial names need the wildcard channel.
                let path = full
                    .strip_suffix("::__dyndbg_fn_marker")
                    .and_then(|p| p.strip_suffix("::__dyndbg_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__dyndbg_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            #[cfg(target_arch = "x86_64")]
            {
                // See dyndbg_debug!: Arguments must be materialized in the
                // NORMAL path — label-block stack slots get merged with live
                // host slots (LLVM treats asm-goto targets as unreachable
                // cold code) and the JMP path would corrupt the host frame.
                let __dyndbg_args = &format_args!($($arg)+);
                #[allow(unsafe_code)]
                    // SAFETY: The inline asm emits an exact 5-byte patch slot at the
                    // call-site and declares a possible branch target used only by
                    // static patching. The label block only performs a register-only
                    // call (no stack writes in this frame): rdi/rsi arrive via the
                    // outer asm's input operands (callbr live-in semantics).
                    unsafe {
                        core::arch::asm!(
                        concat!(
                            ".ifndef \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "_",
                            $site,
                            "\"\n",
                            ".weak \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "_",
                            $site,
                            "\"\n",
                            "\"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "_",
                            $site,
                            "\":\n",
                            ".endif\n",
                            ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                            ".if 0\n",
                            "jmp {0}\n",
                            ".endif\n",
                        ),
                        label {
                            #[allow(unsafe_code)]
                            // SAFETY: This inner asm only defines a global symbol at the
                            // debug block entry for patching targets.
                            unsafe {
                                core::arch::asm!(
                                    concat!(
                                        ".ifndef \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "_",
                                        $site,
                                        "\"\n",
                                        ".weak \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "_",
                                        $site,
                                        "\"\n",
                                        "\"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "_",
                                        $site,
                                        "\":\n",
                                        ".endif\n",
                                    ),
                                    options(nomem, nostack)
                                );
                            }
                            // SAFETY: JMP is only patched in when the module is
                            // enabled, so a module-gate check is redundant here.
                            // The call is register-only: the address operand is
                            // materialized into a scratch register inside the
                            // block; no stack slots are touched in this frame.
                            #[allow(unsafe_code)]
                            unsafe {
                                core::arch::asm!(
                                    "call {emit}",
                                    emit = in(reg) $crate::__dyndbg_label_emit as *const (),
                                    in("rdi") &DESCRIPTOR,
                                    // rsi = args: reloaded from the normal-path
                                    // stack slot inside the block; no callee-saved
                                    // register carries it across the site.
                                    in("rsi") __dyndbg_args,
                                    options()
                                );
                            }
                        },
                        // clobber_abi models the label-block call's register
                        // clobbers on the MAIN path: LLVM treats asm-goto label
                        // blocks as unreachable, so without this the rejoin path
                        // reused caller-saved registers that the runtime call
                        // had clobbered.  The args pointer stays in its stack
                        // slot on the normal path (the inner asm reloads it),
                        // keeping the disabled NOP5 path free of the extra
                        // register save/restore a callee-saved carrier would
                        // cost.
                        clobber_abi("sysv64"),
                        options()
                    );
                }

                // SAFETY: The following extern symbols are declared to refer to
                // the labels emitted by the asm block above. They are not real
                // functions to be called by C; we only take their addresses for
                // patch-site registration.
                unsafe extern "C" {
                    #[link_name = concat!(
                        "__dyndbg_site_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!(),
                        "_",
                        $site,
                    )]
                    fn __dyndbg_site() -> bool;
                    #[link_name = concat!(
                        "__dyndbg_target_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!(),
                        "_",
                        $site,
                    )]
                    fn __dyndbg_target() -> bool;
                }

                static STATIC_KEY_SITE: ostd::arch::static_key::StaticKeySite =
                    ostd::arch::static_key::StaticKeySite::new(
                        __dyndbg_site,
                        __dyndbg_target,
                        "dyndbg",
                    );
                #[ostd::distributed_slice(ostd::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
                static STATIC_KEY_REG_ENTRY: &ostd::arch::static_key::StaticKeySite =
                    &STATIC_KEY_SITE;

                static DYNDBG_KEY_MAPPING: $crate::DyndbgKeyMapping =
                    $crate::DyndbgKeyMapping {
                        descriptor: &DESCRIPTOR,
                        static_key_site: &STATIC_KEY_SITE,
                    };
                #[$crate::distributed_slice($crate::DYNDBG_KEY_MAPPING)]
                static DYNDBG_KEY_MAPPING_ENTRY: &'static $crate::DyndbgKeyMapping =
                    &DYNDBG_KEY_MAPPING;
            }
        }
        #[cfg(all(not(feature = "dyndbg"), feature = "branchdbg"))]
        {
            fn __branch_function_name() -> &'static str {
                fn __branch_fn_marker() {}
                let full = core::any::type_name_of_val(&__branch_fn_marker);
                let path = full
                    .strip_suffix("::__branch_fn_marker")
                    .and_then(|p| p.strip_suffix("::__branch_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__branch_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                $crate::__dyndbg_label_emit(&DESCRIPTOR, &format_args!($($arg)+));
            }
        }
        #[cfg(not(any(feature = "dyndbg", feature = "branchdbg")))]
        {
            #[cfg(target_arch = "x86_64")]
            {
                #[allow(unsafe_code)]
                // SAFETY: empty asm for compiler optimization alignment only.
                unsafe {
                    core::arch::asm!("", options());
                }
            }
        }
    }};
}

// 同样为需要自定义函数标签的场景提供函数版本宏
#[macro_export]
macro_rules! dyndbg_debug_func {
    // Zero-overhead literal branch (func variant): constant message
    // without arguments; same zero-instruction disabled path as dyndbg_debug!.
    // The format string lives in a static symbol that the label block loads
    // with a register-only `mov` (no stack writes), so the disabled NOP5
    // path executes ZERO extra instructions — `nopl; ret`, identical to the
    // compiled-out baseline.  Arguments are constructed inside the emit
    // function's own stack frame (never merged with host slots).
    ($func:expr, $s:literal) => {{
        #[cfg(feature = "dyndbg")]
        {
            fn __dyndbg_function_name() -> &'static str {
                fn __dyndbg_fn_marker() {}
                let full = core::any::type_name_of_val(&__dyndbg_fn_marker);
                let path = full
                    .strip_suffix("::__dyndbg_fn_marker")
                    .and_then(|p| p.strip_suffix("::__dyndbg_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__dyndbg_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            #[cfg(target_arch = "x86_64")]
            {
                static __DYNDBG_SITE_FMT: &str = $s;
                #[allow(unsafe_code)]
                // SAFETY: Same structure as the general branch below — an
                // exact 5-byte patch slot plus a register-only label block.
                // The block loads the static fmt/descriptor addresses into
                // registers and calls the emit helper; no stack writes in
                // this frame, so no slot-merge hazard, and the disabled path
                // materializes nothing at all.
                unsafe {
                    core::arch::asm!(
                        concat!(
                            ".ifndef \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\"\n",
                            ".weak \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\"\n",
                            "\"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\":\n",
                            ".endif\n",
                            ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                            ".if 0\n",
                            "jmp {0}\n",
                            ".endif\n",
                        ),
                        label {
                            #[allow(unsafe_code)]
                            // SAFETY: This inner asm only defines a global symbol at the
                            // debug block entry for patching targets.
                            unsafe {
                                core::arch::asm!(
                                    concat!(
                                        ".ifndef \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\"\n",
                                        ".weak \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\"\n",
                                        "\"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\":\n",
                                        ".endif\n",
                                    ),
                                    options(nomem, nostack)
                                );
                            }
                            #[allow(unsafe_code)]
                            // SAFETY: Register-only call; the static addresses
                            // are materialized as immediates inside the block.
                            // clobber_abi models the call on the main path.
                            unsafe {
                                core::arch::asm!(
                                    "call {emit}",
                                    emit = in(reg) $crate::__dyndbg_label_emit_const as *const (),
                                    in("rdi") &DESCRIPTOR,
                                    in("rsi") &__DYNDBG_SITE_FMT,
                                    options()
                                );
                            }
                        },
                        clobber_abi("sysv64"),
                        options()
                    );
                }
            }
                // SAFETY: The following extern symbols are declared to refer to
                // the labels emitted by the asm block above. They are not real
                // functions to be called by C; we only take their addresses for
                // patch registration and must ensure the asm defines them.
                unsafe extern "C" {
                    #[link_name = concat!(
                        "__dyndbg_site_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!()
                    )]
                    fn __dyndbg_site() -> bool;
                    #[link_name = concat!(
                        "__dyndbg_target_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!()
                    )]
                    fn __dyndbg_target() -> bool;
                }

                static STATIC_KEY_SITE: ostd::arch::static_key::StaticKeySite =
                    ostd::arch::static_key::StaticKeySite::new(
                        __dyndbg_site,
                        __dyndbg_target,
                        "dyndbg",
                    );
                #[ostd::distributed_slice(ostd::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
                static STATIC_KEY_REG_ENTRY: &ostd::arch::static_key::StaticKeySite =
                    &STATIC_KEY_SITE;

                static DYNDBG_KEY_MAPPING: $crate::DyndbgKeyMapping =
                    $crate::DyndbgKeyMapping {
                        descriptor: &DESCRIPTOR,
                        static_key_site: &STATIC_KEY_SITE,
                    };
                #[$crate::distributed_slice($crate::DYNDBG_KEY_MAPPING)]
                static DYNDBG_KEY_MAPPING_ENTRY: &'static $crate::DyndbgKeyMapping =
                    &DYNDBG_KEY_MAPPING;

            #[cfg(not(target_arch = "x86_64"))]
            {
                if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                    $crate::__dyndbg_label_emit_const(&DESCRIPTOR, $s);
                }
            }

        }

        // Branch-based backend: same static-fmt path (no patching).
        #[cfg(all(not(feature = "dyndbg"), feature = "branchdbg"))]
        {
            fn __branch_function_name() -> &'static str {
                fn __branch_fn_marker() {}
                let full = core::any::type_name_of_val(&__branch_fn_marker);
                let path = full
                    .strip_suffix("::__branch_fn_marker")
                    .and_then(|p| p.strip_suffix("::__branch_function_name"))
                    .unwrap_or(full);
                path.rsplit_once("::").map(|(_, f)| f).unwrap_or(path)
            }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__branch_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                $crate::__dyndbg_label_emit_const(&DESCRIPTOR, $s);
            }
        }

        // No-op backend (neither feature enabled)
        #[cfg(not(any(feature = "dyndbg", feature = "branchdbg")))]
        {
            #[cfg(target_arch = "x86_64")]
            {
                #[allow(unsafe_code)]
                // SAFETY: See the general branch: empty asm aligns LLVM's
                // optimization behavior with the static-patch path.
                unsafe {
                    core::arch::asm!("", options());
                }
            }
        }
    }};

    ($func:expr, $($arg:tt)+) => {{
        #[cfg(feature = "dyndbg")]
        {
            fn __dyndbg_function_name() -> &'static str { $func }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__dyndbg_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            #[cfg(target_arch = "x86_64")]
            {
                // Materialize the Arguments in the NORMAL path: LLVM treats
                // asm-goto label blocks as unreachable cold code and merges
                // their stack slots with live host slots, so constructing the
                // Arguments inside the label block corrupted the host frame.
                let __dyndbg_args = &format_args!($($arg)+);
                #[allow(unsafe_code)]
                // SAFETY: The inline asm emits an exact 5-byte patch slot at the
                // call-site and declares a possible branch target used only by
                // static patching. The label block only performs a register-only
                // call (no stack writes in this frame): rdi/rsi arrive via the
                // outer asm's input operands (callbr live-in semantics).
                unsafe {
                    core::arch::asm!(
                        concat!(
                            ".ifndef \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\"\n",
                            ".weak \"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\"\n",
                            "\"__dyndbg_site_",
                            module_path!(),
                            "_",
                            line!(),
                            "_",
                            column!(),
                            "\":\n",
                            ".endif\n",
                            ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                            ".if 0\n",
                            "jmp {0}\n",
                            ".endif\n",
                        ),
                        label {
                            #[allow(unsafe_code)]
                            // SAFETY: This inner asm only defines a global symbol at the
                            // debug block entry for patching targets.
                            unsafe {
                                core::arch::asm!(
                                    concat!(
                                        ".ifndef \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\"\n",
                                        ".weak \"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\"\n",
                                        "\"__dyndbg_target_",
                                        module_path!(),
                                        "_",
                                        line!(),
                                        "_",
                                        column!(),
                                        "\":\n",
                                        ".endif\n",
                                    ),
                                    options(nomem, nostack)
                                );
                            }
                            // SAFETY: JMP is only patched in when the module is
                            // enabled, so a module-gate check is redundant here.
                            // The call is register-only: the address operand is
                            // materialized into a scratch register inside the
                            // block; no stack slots are touched in this frame.
                            #[allow(unsafe_code)]
                            unsafe {
                                core::arch::asm!(
                                    "call {emit}",
                                    emit = in(reg) $crate::__dyndbg_label_emit as *const (),
                                    in("rdi") &DESCRIPTOR,
                                    // rsi = args: reloaded from the normal-path
                                    // stack slot inside the block; no callee-saved
                                    // register carries it across the site.
                                    in("rsi") __dyndbg_args,
                                    options()
                                );
                            }
                        },
                        // clobber_abi models the label-block call's register
                        // clobbers on the MAIN path: LLVM treats asm-goto label
                        // blocks as unreachable, so without this the rejoin path
                        // reused caller-saved registers that the runtime call
                        // had clobbered.  The args pointer stays in its stack
                        // slot on the normal path (the inner asm reloads it),
                        // keeping the disabled NOP5 path free of the extra
                        // register save/restore a callee-saved carrier would
                        // cost.
                        clobber_abi("sysv64"),
                        options()
                    );
                }

                // SAFETY: The following extern symbols are declared to refer to
                // the labels emitted by the asm block above. They are not real
                // functions to be called by C; we only take their addresses for
                // patch registration and must ensure the asm defines them.
                unsafe extern "C" {
                    #[link_name = concat!(
                        "__dyndbg_site_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!()
                    )]
                    fn __dyndbg_site() -> bool;
                    #[link_name = concat!(
                        "__dyndbg_target_",
                        module_path!(),
                        "_",
                        line!(),
                        "_",
                        column!()
                    )]
                    fn __dyndbg_target() -> bool;
                }

                static STATIC_KEY_SITE: ostd::arch::static_key::StaticKeySite =
                    ostd::arch::static_key::StaticKeySite::new(
                        __dyndbg_site,
                        __dyndbg_target,
                        "dyndbg",
                    );
                #[ostd::distributed_slice(ostd::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
                static STATIC_KEY_REG_ENTRY: &ostd::arch::static_key::StaticKeySite =
                    &STATIC_KEY_SITE;

                static DYNDBG_KEY_MAPPING: $crate::DyndbgKeyMapping =
                    $crate::DyndbgKeyMapping {
                        descriptor: &DESCRIPTOR,
                        static_key_site: &STATIC_KEY_SITE,
                    };
                #[$crate::distributed_slice($crate::DYNDBG_KEY_MAPPING)]
                static DYNDBG_KEY_MAPPING_ENTRY: &'static $crate::DyndbgKeyMapping =
                    &DYNDBG_KEY_MAPPING;
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                    $crate::__dyndbg_label_emit(&DESCRIPTOR, &format_args!($($arg)+));
                }
            }
        }

        #[cfg(all(not(feature = "dyndbg"), feature = "branchdbg"))]
        {
            fn __branch_function_name() -> &'static str { $func }
            static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
                file!(),
                module_path!(),
                Some(__branch_function_name),
                line!(),
            );
            #[$crate::distributed_slice($crate::DYNDBG_DESCRIPTOR_REGISTRY)]
            static DYNDBG_DESCRIPTOR_ENTRY: &'static $crate::DebugDescriptor = &DESCRIPTOR;
            if $crate::dyndbg_module_enabled(&DESCRIPTOR) {
                $crate::__dyndbg_label_emit(&DESCRIPTOR, &format_args!($($arg)+));
            }
        }

        #[cfg(not(any(feature = "dyndbg", feature = "branchdbg")))]
        {
            #[cfg(target_arch = "x86_64")]
            {
                #[allow(unsafe_code)]
                // SAFETY: empty asm for compiler optimization alignment only.
                unsafe {
                    core::arch::asm!("", options());
                }
            }
        }
    }};
}

pub(super) fn init() {
    pre_register_dyndbg_descriptors();
    pre_register_dyndbg_keys();
    ostd::logger::inject_logger(&LOGGER);
}
