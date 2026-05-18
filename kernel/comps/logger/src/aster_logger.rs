// SPDX-License-Identifier: MPL-2.0

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Duration,
};

use log::{Metadata, Record};
use linkme::distributed_slice;
use ostd::{sync::SpinLock, timer::Jiffies};

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

// Fast gates used on the hot path. Updates happen under DYNDBG_STATE lock, while
// reads are lock-free in dyndbg_should_log().
static MODULE_STATES: [ModuleState; MAX_DYNDBG_MODULES] =
    [const { ModuleState::new() }; MAX_DYNDBG_MODULES];

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

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct PatchSite {
    instruction_address: usize,
    metadata: PatchSiteMetadata,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct PatchSiteMetadata {
    jump_target: usize,
    descriptor_address: usize,
}

struct ModuleKey {
    enabled: bool,
    patch_sites: Vec<PatchSite>,
}

impl ModuleKey {
    fn new() -> Self {
        Self {
            enabled: false,
            patch_sites: Vec::new(),
        }
    }
}

//编译期收集的静态切片，init阶段完成注册操作 后续运行时无注册开销
#[distributed_slice]
pub static DYNDBG_DESCRIPTOR_REGISTRY: [&'static DebugDescriptor];

#[distributed_slice]
pub static DYNDBG_PATCH_SITE_REGISTRY: [&'static DyndbgPatchSiteRegistration];

// 现在注册patchsite信息需要site和target
#[derive(Debug, Clone, Copy)]
pub struct DyndbgPatchSiteRegistration {
    pub descriptor: &'static DebugDescriptor,
    pub instruction_site: unsafe extern "C" fn() -> bool,
    pub jump_target: unsafe extern "C" fn() -> bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DyndbgRuleAction {
    Enable,
    Disable,
}

#[derive(Debug, Clone, Default)]
pub struct DyndbgRule {
    file_keyword: Option<String>,
    module_keyword: Option<String>,
    function_keyword: Option<String>,
    line: Option<u32>,
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

        selector_match(&self.file_keyword, Some(descriptor.file))
            && selector_match(&self.module_keyword, Some(descriptor.module_path))
            && selector_match(&self.function_keyword, descriptor.function_name())
            && self.line.is_none_or(|line| descriptor.line == line)
    }
}

// value包含keyword则匹配，当selector为None时总是匹配（真正的match逻辑是&&）
fn selector_match(selector: &Option<String>, value: Option<&str>) -> bool {
    match selector {
        None => true,
        Some(needle) => value.is_some_and(|value| value.contains(needle)),
    }
}

struct DyndbgState {
    rules: Vec<DyndbgRuleEntry>,
    module_id_by_path: BTreeMap<&'static str, u32>,
    module_keys: BTreeMap<u32, ModuleKey>,
    file_index: BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    module_index: BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    function_index: BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    line_index: BTreeMap<u32, Vec<&'static DebugDescriptor>>,
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

        let module_id = self.allocate_module_id(descriptor.module_path);
        descriptor.init_module_id(module_id);
        // 初始时空规则链 默认禁用所有descriptor，避免冗余判断和触发状态迁移逻辑。
        if self.rules.is_empty() {
            descriptor.init_enabled(DEFAULT_DEBUG_ENABLED);
            return;
        }
        //若初始有rule链 根据rule链来设置初始enabled状态和判断是否迁移
        let is_enabled = self.matches_descriptor(descriptor);
        descriptor.init_enabled(is_enabled);
        if is_enabled {
            self.apply_enabled_transition(module_id, false, true);
        }
    }

    //把编译期收集的信息存入module key
    fn register_patch_site_registration(&mut self, registration: &'static DyndbgPatchSiteRegistration) {
        let module_id = registration.descriptor.module_id();
        if module_id == UNASSIGNED_MODULE_ID {
            return;
        }

        // 构造patchsite
        let instruction_address = registration.instruction_site as usize;
        let jump_target = registration.jump_target as usize;
        let patch_site = PatchSite {
            instruction_address,
            metadata: PatchSiteMetadata {
                jump_target,
                descriptor_address: descriptor_id(registration.descriptor),
            },
        };

        // 存入module key里
        let current_enabled = module_enabled(module_id);
        let module_key = self.module_keys.entry(module_id).or_insert_with(ModuleKey::new);
        module_key.enabled = current_enabled;
        module_key.patch_sites.push(patch_site);

        if current_enabled {
            patch_module_sites(module_key, true);
        }
    }

    // 设置新规则时仅重算受影响的descriptor，降低规则更新成本。
    fn refresh_registered_descriptors(&mut self, affected: Vec<&'static DebugDescriptor>) {
        let mut seen = BTreeSet::new();
        let mut module_deltas = BTreeMap::<u32, i64>::new();

        //去重
        for descriptor in affected {
            if !seen.insert(descriptor_id(descriptor)) {
                continue;
            }
            //仅对受影响的descriptor进行裁决和更新enabled状态 避免全量更新的性能问题
            let enabled = self.matches_descriptor(descriptor);
            let old_enabled = descriptor.swap_enabled(enabled);
            if old_enabled == enabled {
                continue;
            }

            let module_id = descriptor.module_id();
            if module_state(module_id).is_none() {
                continue;
            }

            // 暂存descriptor变化
            let delta = if enabled { 1 } else { -1 };
            *module_deltas.entry(module_id).or_insert(0) += delta;
        }

        // 应用模块级的变化，触发必要的指令修补。
        for (module_id, delta) in module_deltas {
            self.apply_module_delta(module_id, delta);
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
        let mut candidates: Option<Vec<&'static DebugDescriptor>> = None;

        if let Some(file_keyword) = &rule.file_keyword {
            let matched = collect_by_keyword(&self.file_index, file_keyword);
            intersect_candidates(&mut candidates, matched);
        }

        if let Some(module_keyword) = &rule.module_keyword {
            let matched = collect_by_keyword(&self.module_index, module_keyword);
            intersect_candidates(&mut candidates, matched);
        }

        if let Some(function_keyword) = &rule.function_keyword {
            let matched = collect_by_keyword(&self.function_index, function_keyword);
            intersect_candidates(&mut candidates, matched);
        }

        if let Some(line) = rule.line {
            let matched = self.line_index.get(&line).cloned().unwrap_or_default();
            intersect_candidates(&mut candidates, matched);
        }

        candidates.unwrap_or_else(all_descriptors)
    }

    //最终的对单个描述符的裁决逻辑，所有rule过一遍，后面规则优先级高于前面规则
    fn matches_descriptor(&self, descriptor: &DebugDescriptor) -> bool {
        let mut enabled = DEFAULT_DEBUG_ENABLED;
        for entry in &self.rules {
            if entry.rule.matches_descriptor(descriptor) {
                enabled = entry.action == DyndbgRuleAction::Enable;
            }
        }
        enabled
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
fn collect_by_keyword(
    index: &BTreeMap<&'static str, Vec<&'static DebugDescriptor>>,
    keyword: &str,
) -> Vec<&'static DebugDescriptor> {
    let mut matched = Vec::new();
    for (indexed_value, descriptors) in index {
        if indexed_value.contains(keyword) {
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

// 模块级别的指令修补
#[cfg(target_arch = "x86_64")]
fn patch_module_sites(module_key: &ModuleKey, enabled: bool) {
    use ostd::arch::static_patch::{patch_5byte_slots, PatchInstruction, PatchRequest};

    let mut requests = Vec::new();
    for site in &module_key.patch_sites {
        // 0是占位符，还没填充真实地址 则跳过
        if site.instruction_address == 0 {
            continue;
        }

        // 生成要修补的指令。
        let instruction = if enabled {
            if site.metadata.jump_target == 0 {
                continue;
            }
            PatchInstruction::JmpRel32 {
                target: site.metadata.jump_target,
            }
        } else {
            PatchInstruction::Nop5
        };

        requests.push(PatchRequest {
            instruction_address: site.instruction_address,
            instruction,
        });
    }

    if requests.is_empty() {
        return;
    }

    if let Err(error) = patch_5byte_slots(&requests) {
        log::warn!(
            "dyndbg batch patch failed: module_enabled={}, sites={}, error={:?}",
            enabled,
            requests.len(),
            error
        );
    }
}

// 非x86架构暂未实现，提供空实现  
#[cfg(not(target_arch = "x86_64"))]
fn patch_module_sites(_module_key: &ModuleKey, _enabled: bool) {}

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
    enabled: AtomicBool,
    file: &'static str,
    module_path: &'static str,
    module_id: AtomicU32,
    function: Option<fn() -> &'static str>,
    line: u32,
}

impl DebugDescriptor {
    pub const fn new(
        file: &'static str,
        module_path: &'static str,
        function: Option<fn() -> &'static str>,
        line: u32,
    ) -> Self {
        Self {
            enabled: AtomicBool::new(DEFAULT_DEBUG_ENABLED),
            file,
            module_path,
            module_id: AtomicU32::new(UNASSIGNED_MODULE_ID),
            function,
            line,
        }
    }

    fn init_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn init_module_id(&self, module_id: u32) {
        self.module_id.store(module_id, Ordering::Relaxed);
    }

    fn module_id(&self) -> u32 {
        self.module_id.load(Ordering::Acquire)
    }

    fn swap_enabled(&self, enabled: bool) -> bool {
        self.enabled.swap(enabled, Ordering::AcqRel)
    }

    #[inline]
    fn should_log_fast(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    #[inline]
    fn function_name(&self) -> Option<&'static str> {
        self.function.map(|provider| provider())
    }
}

pub fn dyndbg_should_log(descriptor: &'static DebugDescriptor) -> bool {
    if !module_enabled(descriptor.module_id()) {
        return false;
    }

    descriptor.should_log_fast()
}

fn pre_register_dyndbg_descriptors() {
    let mut state = DYNDBG_STATE.lock();
    for descriptor in DYNDBG_DESCRIPTOR_REGISTRY {
        state.register_descriptor(descriptor);
    }
}

fn pre_register_dyndbg_patch_sites() {
    let mut state = DYNDBG_STATE.lock();
    for registration in DYNDBG_PATCH_SITE_REGISTRY {
        state.register_patch_site_registration(registration);
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
}

#[derive(Debug, Clone)]
pub struct DyndbgRuleEntrySnapshot {
    pub rule: DyndbgRuleSnapshot,
    pub enabled: bool,
}

impl From<DyndbgRuleSnapshot> for DyndbgRule {
    fn from(snapshot: DyndbgRuleSnapshot) -> Self {
        Self {
            file_keyword: snapshot.file_keyword,
            module_keyword: snapshot.module_keyword,
            function_keyword: snapshot.function_keyword,
            line: snapshot.line,
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
        }
    }
}

impl From<DyndbgRuleEntrySnapshot> for DyndbgRuleEntry {
    fn from(snapshot: DyndbgRuleEntrySnapshot) -> Self {
        Self {
            rule: snapshot.rule.into(),
            action: if snapshot.enabled {
                DyndbgRuleAction::Enable
            } else {
                DyndbgRuleAction::Disable
            },
        }
    }
}

impl From<&DyndbgRuleEntry> for DyndbgRuleEntrySnapshot {
    fn from(entry: &DyndbgRuleEntry) -> Self {
        Self {
            rule: DyndbgRuleSnapshot::from(&entry.rule),
            enabled: entry.action == DyndbgRuleAction::Enable,
        }
    }
}

// Backward-compatible API.
pub fn update_dyndbg_rule(file_keyword: Option<&str>, module_keyword: Option<&str>) {
    let snapshot = DyndbgRuleSnapshot {
        file_keyword: file_keyword.map(String::from),
        module_keyword: module_keyword.map(String::from),
        function_keyword: None,
        line: None,
    };
    set_dyndbg_rule(snapshot);
}

// Backward-compatible API.
pub fn get_dyndbg_rule() -> (Option<String>, Option<String>) {
    let snapshot = get_dyndbg_rule_snapshot();
    (snapshot.file_keyword, snapshot.module_keyword)
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

// 清空规则链，新设置一条规则
pub fn set_dyndbg_rule(snapshot: DyndbgRuleSnapshot) {
    let mut state = DYNDBG_STATE.lock();
    let old_rules = state.rules.clone();

    state.rules.clear();
    let new_entry = DyndbgRuleEntry {
        rule: snapshot.into(),
        action: DyndbgRuleAction::Enable,
    };
    state.rules.push(new_entry.clone());

    let mut affected = state.collect_candidates_for_rule_entries(&old_rules);
    affected.extend(state.collect_candidates_for_rule_entries(core::slice::from_ref(
        &new_entry,
    )));
    state.refresh_registered_descriptors(affected);
}

//清空规则链
pub fn clear_dyndbg_rule() {
    clear_dyndbg_rules();
}

// 向规则链追加规则
pub fn append_dyndbg_rule(snapshot: DyndbgRuleSnapshot, enabled: bool) {
    let mut state = DYNDBG_STATE.lock();
    let new_entry: DyndbgRuleEntry = DyndbgRuleEntrySnapshot {
        rule: snapshot,
        enabled,
    }
    .into();
    let affected = state.collect_candidates_for_rule_entries(core::slice::from_ref(&new_entry));

    state.rules.push(new_entry);
    state.refresh_registered_descriptors(affected);
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

//引入新宏
#[macro_export]
macro_rules! dyndbg_debug {
    ($($arg:tt)+) => {{
        // 获取当前函数完整名称（包含模块路径）
        fn __dyndbg_function_name() -> &'static str {
            fn __dyndbg_fn_marker() {}
            let type_name = core::any::type_name_of_val(&__dyndbg_fn_marker);
            type_name
                .strip_suffix("::__dyndbg_fn_marker")
                .unwrap_or(type_name)
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
            // 调用点静态补丁槽：NOP5 <-> JMP debug path。
            #[allow(unsafe_code)]
            unsafe {
                // SAFETY: The inline asm emits an exact 5-byte patch slot at the
                // call-site and declares a possible branch target used only by
                // static patching. The label block is normal Rust code and falls
                // through to the original execution path.
                core::arch::asm!(
                    concat!(
                        ".globl \"__dyndbg_site_",
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
                        ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                        ".if 0\n",
                        "jmp {0}\n",
                        ".endif\n",
                    ),
                    label {
                        #[allow(unsafe_code)]
                        unsafe {
                            // SAFETY: This only defines a global symbol at the
                            // debug block entry for patching targets.
                            core::arch::asm!(
                                concat!(
                                    ".globl \"__dyndbg_target_",
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
                                ),
                                options(nomem, nostack)
                            );
                        }
                        if $crate::dyndbg_should_log(&DESCRIPTOR) {
                            log::debug!($($arg)+);
                        }
                    },
                    options(nomem, nostack, preserves_flags)
                );
            }

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
            // 编译期生成patch site信息
            static DYNDBG_PATCH_SITE: $crate::DyndbgPatchSiteRegistration =
                $crate::DyndbgPatchSiteRegistration {
                    descriptor: &DESCRIPTOR,
                    instruction_site: __dyndbg_site,
                    jump_target: __dyndbg_target,
                };
            // 收集到全局分布式切片里，init阶段完成注册 后续运行时无注册开销
            #[$crate::distributed_slice($crate::DYNDBG_PATCH_SITE_REGISTRY)]
            static DYNDBG_PATCH_SITE_ENTRY: &'static $crate::DyndbgPatchSiteRegistration =
                &DYNDBG_PATCH_SITE;
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            if $crate::dyndbg_should_log(&DESCRIPTOR) {
                log::debug!($($arg)+);
            }
        }
    }};
}
// 手动指定函数名的覆盖宏，用于需要自定义函数标签的场景。
#[macro_export]
macro_rules! dyndbg_debug_func {
    ($func:expr, $($arg:tt)+) => {{
        fn __dyndbg_function_name() -> &'static str {
            $func
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
            #[allow(unsafe_code)]
            unsafe {
                // SAFETY: The inline asm emits an exact 5-byte patch slot at the
                // call-site and declares a possible branch target used only by
                // static patching. The label block is normal Rust code and falls
                // through to the original execution path.
                core::arch::asm!(
                    concat!(
                        ".globl \"__dyndbg_site_",
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
                        ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                        ".if 0\n",
                        "jmp {0}\n",
                        ".endif\n",
                    ),
                    label {
                        #[allow(unsafe_code)]
                        unsafe {
                            // SAFETY: This only defines a global symbol at the
                            // debug block entry for patching targets.
                            core::arch::asm!(
                                concat!(
                                    ".globl \"__dyndbg_target_",
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
                                ),
                                options(nomem, nostack)
                            );
                        }
                        if $crate::dyndbg_should_log(&DESCRIPTOR) {
                            log::debug!($($arg)+);
                        }
                    },
                    options(nomem, nostack, preserves_flags)
                );
            }

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

            static DYNDBG_PATCH_SITE: $crate::DyndbgPatchSiteRegistration =
                $crate::DyndbgPatchSiteRegistration {
                    descriptor: &DESCRIPTOR,
                    instruction_site: __dyndbg_site,
                    jump_target: __dyndbg_target,
                };
            #[$crate::distributed_slice($crate::DYNDBG_PATCH_SITE_REGISTRY)]
            static DYNDBG_PATCH_SITE_ENTRY: &'static $crate::DyndbgPatchSiteRegistration =
                &DYNDBG_PATCH_SITE;
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            if $crate::dyndbg_should_log(&DESCRIPTOR) {
                log::debug!($($arg)+);
            }
        }
    }};
}

pub(super) fn init() {
    pre_register_dyndbg_descriptors();
    pre_register_dyndbg_patch_sites();
    ostd::logger::inject_logger(&LOGGER);
}
