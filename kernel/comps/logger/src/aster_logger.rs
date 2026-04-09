// SPDX-License-Identifier: MPL-2.0

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use log::{Metadata, Record};
use ostd::{sync::SpinLock, timer::Jiffies};

/// The logger used for Asterinas.
struct AsterLogger;

static LOGGER: AsterLogger = AsterLogger;
// Dynamic debug state protected by a single lock so rule updates and descriptor
// registrations are serialized.
static DYNDBG_STATE: SpinLock<DyndbgState> = SpinLock::new(DyndbgState::new());
// Active generation of `enabled_slots` used by the fast path.
static DYNDBG_GENERATION: AtomicU64 = AtomicU64::new(0);
const DEFAULT_DEBUG_ENABLED: bool = false;

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

    // 判断记录是否匹配规则
    fn matches_record(&self, record: &Record) -> bool {
        if !self.has_any_selector() {
            return true;
        }

        selector_match(&self.file_keyword, record.file())
            && selector_match(&self.module_keyword, record.module_path())
            && selector_match(&self.function_keyword, None)
            && self.line.is_none_or(|line| record.line() == Some(line))
    }

    // 判断描述符是否匹配规则
    fn matches_descriptor(&self, descriptor: &DebugDescriptor) -> bool {
        if !self.has_any_selector() {
            return true;
        }

        selector_match(&self.file_keyword, Some(descriptor.file))
            && selector_match(&self.module_keyword, Some(descriptor.module_path))
            && selector_match(&self.function_keyword, descriptor.function)
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
    descriptors: Vec<&'static DebugDescriptor>,
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
            descriptors: Vec::new(),
            file_index: BTreeMap::new(),
            module_index: BTreeMap::new(),
            function_index: BTreeMap::new(),
            line_index: BTreeMap::new(),
        }
    }

    //注册descriptor时会将其插入到全局的descriptors列表和各个索引表中，
    //并根据rule链来设置初始enabled状态
    fn register_descriptor(&mut self, descriptor: &'static DebugDescriptor) {
        self.descriptors.push(descriptor);
        insert_index_entry(&mut self.file_index, descriptor.file, descriptor);
        insert_index_entry(&mut self.module_index, descriptor.module_path, descriptor);
        if let Some(function) = descriptor.function {
            insert_index_entry(&mut self.function_index, function, descriptor);
        }
        insert_index_entry(&mut self.line_index, descriptor.line, descriptor);

        let is_enabled = self.matches_descriptor(descriptor);
        let generation = DYNDBG_GENERATION.load(Ordering::Acquire);
        descriptor.init_enabled_slots(generation, is_enabled);
    }

    //设置新规则时 更新descriptor
    fn refresh_registered_descriptors(&mut self) {
        //先根据新规则收集出新的enabled descriptor列表
        let new_enabled = self.collect_enabled_descriptors();
        //规则计算完成后，批量写入下一代slot并原子提交generation。
        self.commit_enabled_generation(new_enabled);
    }

    // 规则精筛 最终匹配只认matches_descriptor
    fn collect_enabled_descriptors(&self) -> Vec<&'static DebugDescriptor> {
        //空规则时根据默认值决定是全开还是全关
        if self.rules.is_empty() {
            if DEFAULT_DEBUG_ENABLED {
                return self.descriptors.clone();
            }
            return Vec::new();
        }

        let mut candidates = self.collect_rule_candidates();
        candidates.retain(|descriptor| self.matches_descriptor(descriptor));
        candidates
    }

    // 索引粗筛 挑出尽可能少的候选者供精筛使用，减少matches_descriptor的调用次数提升性能
    // 获取所有规则候选集的并集
    fn collect_rule_candidates(&self) -> Vec<&'static DebugDescriptor> {
        let mut union = Vec::new();
        let mut seen = BTreeSet::new();

        for entry in &self.rules {
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

        candidates.unwrap_or_else(|| self.descriptors.clone())
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

    // 兼容旧的record-based接口
    fn matches_record(&self, record: &Record) -> bool {
        let mut enabled = DEFAULT_DEBUG_ENABLED;
        for entry in &self.rules {
            if entry.rule.matches_record(record) {
                enabled = entry.action == DyndbgRuleAction::Enable;
            }
        }
        enabled
    }

    fn commit_enabled_generation(&mut self, new_enabled: Vec<&'static DebugDescriptor>) {
        let new_ids = new_enabled
            .iter()
            .map(|descriptor| descriptor_id(descriptor))
            .collect::<BTreeSet<_>>();
        let current_generation = DYNDBG_GENERATION.load(Ordering::Relaxed);
        let next_generation = current_generation.wrapping_add(1);
        let next_slot = generation_slot(next_generation);

        // 所有descriptor先写入下一代slot，最后一次性发布generation。
        for descriptor in &self.descriptors {
            let enabled = new_ids.contains(&descriptor_id(descriptor));
            descriptor.set_enabled_slot(next_slot, enabled);
        }

        DYNDBG_GENERATION.store(next_generation, Ordering::Release);
    }
}

// 取generation的最低位作为当前使用的slot index，0或1
#[inline]
const fn generation_slot(generation: u64) -> usize {
    (generation & 1) as usize
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

pub struct DebugDescriptor {
    enabled_slots: [AtomicBool; 2],
    registered: AtomicBool,
    file: &'static str,
    module_path: &'static str,
    function: Option<&'static str>,
    line: u32,
}

impl DebugDescriptor {
    pub const fn new(
        file: &'static str,
        module_path: &'static str,
        function: Option<&'static str>,
        line: u32,
    ) -> Self {
        Self {
            enabled_slots: [
                AtomicBool::new(DEFAULT_DEBUG_ENABLED),
                AtomicBool::new(DEFAULT_DEBUG_ENABLED),
            ],
            registered: AtomicBool::new(false),
            file,
            module_path,
            function,
            line,
        }
    }

    //descriptor首次注册时，根据全局rule来store其enabled状态，并插入全局vec中
    //此时registered状态会被置为true，后续再次调用ensure_registered时会直接返回，保证不会重复注册
    fn ensure_registered(&'static self) {
        if self
            .registered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let mut state = DYNDBG_STATE.lock();
        state.register_descriptor(self);
    }

    //初始化两个槽位
    fn init_enabled_slots(&self, generation: u64, enabled: bool) {
        let slot = generation_slot(generation);
        self.enabled_slots[slot].store(enabled, Ordering::Relaxed);
        self.enabled_slots[slot ^ 1].store(enabled, Ordering::Relaxed);
    }

    //写入对应的槽位
    fn set_enabled_slot(&self, slot: usize, enabled: bool) {
        self.enabled_slots[slot].store(enabled, Ordering::Relaxed);
    }

    fn is_enabled(&self) -> bool {
        loop {
            let generation_before = DYNDBG_GENERATION.load(Ordering::Acquire);
            let slot = generation_slot(generation_before);
            let enabled = self.enabled_slots[slot].load(Ordering::Relaxed);
            let generation_after = DYNDBG_GENERATION.load(Ordering::Acquire);
            if generation_before == generation_after {
                return enabled;
            }
        }
    }
}

pub fn dyndbg_should_log(descriptor: &'static DebugDescriptor) -> bool {
    descriptor.ensure_registered();
    descriptor.is_enabled()
}

impl log::Log for AsterLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        // For legacy `log::debug!` callsites, keep record-based matching.
        if record.level() == log::Level::Debug && !dyndbg_match_record(record) {
            return;
        }

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

fn dyndbg_match_record(record: &Record) -> bool {
    let state = DYNDBG_STATE.lock();
    state.matches_record(record)
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
    state.rules.clear();
    state.rules.push(DyndbgRuleEntry {
        rule: snapshot.into(),
        action: DyndbgRuleAction::Enable,
    });
    state.refresh_registered_descriptors();
}

//清空规则链
pub fn clear_dyndbg_rule() {
    clear_dyndbg_rules();
}

// 向规则链追加规则
pub fn append_dyndbg_rule(snapshot: DyndbgRuleSnapshot, enabled: bool) {
    let mut state = DYNDBG_STATE.lock();
    state.rules.push(
        DyndbgRuleEntrySnapshot {
            rule: snapshot,
            enabled,
        }
        .into(),
    );
    state.refresh_registered_descriptors();
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
    state.rules.clear();
    state.refresh_registered_descriptors();
}

// 通过vec的id删除规则
pub fn remove_dyndbg_rule_by_id(rule_id: usize) -> bool {
    let mut state = DYNDBG_STATE.lock();
    if rule_id >= state.rules.len() {
        return false;
    }

    state.rules.remove(rule_id);
    state.refresh_registered_descriptors();
    true
}

//引入新宏
#[macro_export]
macro_rules! dyndbg_debug {
    ($($arg:tt)+) => {{
        static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
            file!(),
            module_path!(),
            None,
            line!(),
        );
        if $crate::dyndbg_should_log(&DESCRIPTOR) {
            log::debug!($($arg)+);
        }
    }};
}
//引入新宏 其静态变量需要function name作为参数传入 其针对调试函数的场景(后续看看能否与dyndbg_debug宏合并)
#[macro_export]
macro_rules! dyndbg_debug_func {
    ($func:expr, $($arg:tt)+) => {{
        static DESCRIPTOR: $crate::DebugDescriptor = $crate::DebugDescriptor::new(
            file!(),
            module_path!(),
            Some($func),
            line!(),
        );
        if $crate::dyndbg_should_log(&DESCRIPTOR) {
            log::debug!($($arg)+);
        }
    }};
}

pub(super) fn init() {
    ostd::logger::inject_logger(&LOGGER);
}
