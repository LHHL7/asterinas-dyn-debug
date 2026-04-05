// SPDX-License-Identifier: MPL-2.0

use alloc::{string::String, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use log::{Metadata, Record};
use ostd::sync::SpinLock;
use ostd::timer::Jiffies;

/// The logger used for Asterinas.
struct AsterLogger;

static LOGGER: AsterLogger = AsterLogger;
// Dynamic debug state protected by a single lock so rule updates and descriptor
// registrations are serialized.
static DYNDBG_STATE: SpinLock<DyndbgState> = SpinLock::new(DyndbgState::new());

#[derive(Debug, Clone, Default)]
pub struct DyndbgRule {
    file_keyword: Option<String>,
    module_keyword: Option<String>,
    function_keyword: Option<String>,
    line: Option<u32>,
}

impl DyndbgRule {
    const fn new() -> Self {
        Self {
            file_keyword: None,
            module_keyword: None,
            function_keyword: None,
            line: None,
        }
    }

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
    rule: DyndbgRule,
    descriptors: Vec<&'static DebugDescriptor>,
}

impl DyndbgState {
    const fn new() -> Self {
        Self {
            rule: DyndbgRule::new(),
            descriptors: Vec::new(),
        }
    }
}

pub struct DebugDescriptor {
    enabled: AtomicBool,
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
            enabled: AtomicBool::new(true),
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
        self.enabled
            .store(state.rule.matches_descriptor(self), Ordering::Relaxed);
        state.descriptors.push(self);
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

pub fn dyndbg_should_log(descriptor: &'static DebugDescriptor) -> bool {
    descriptor.ensure_registered();
    descriptor.is_enabled()
}

//当rule更新时,遍历全局vec，根据新的rule来更新每个descriptor的enabled状态（遍历待优化）
fn refresh_registered_descriptors(state: &DyndbgState) {
    for descriptor in &state.descriptors {
        descriptor
            .enabled
            .store(state.rule.matches_descriptor(descriptor), Ordering::Relaxed);
    }
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
    state.rule.matches_record(record)
}

// 对外暴露快照 外部通过快照来查看和设置规则，避免直接暴露内部的Rule结构，减少耦合
#[derive(Debug, Clone, Default)]
pub struct DyndbgRuleSnapshot {
    pub file_keyword: Option<String>,
    pub module_keyword: Option<String>,
    pub function_keyword: Option<String>,
    pub line: Option<u32>,
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

pub fn get_dyndbg_rule_snapshot() -> DyndbgRuleSnapshot {
    let state = DYNDBG_STATE.lock();
    DyndbgRuleSnapshot::from(&state.rule)
}

pub fn set_dyndbg_rule(snapshot: DyndbgRuleSnapshot) {
    let mut state = DYNDBG_STATE.lock();
    state.rule = snapshot.into();
    refresh_registered_descriptors(&state);
}

pub fn clear_dyndbg_rule() {
    set_dyndbg_rule(DyndbgRuleSnapshot::default());
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
