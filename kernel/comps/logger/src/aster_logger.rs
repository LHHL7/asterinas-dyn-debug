// SPDX-License-Identifier: MPL-2.0

use alloc::string::String;
use core::time::Duration;

use log::{Metadata, Record};
use ostd::sync::SpinLock;
use ostd::timer::Jiffies;

/// The logger used for Asterinas.
struct AsterLogger;

static LOGGER: AsterLogger = AsterLogger;
//全局过滤规则，spinlock自旋锁保护
static DYNDBG_RULE: SpinLock<DyndbgRule> = SpinLock::new(DyndbgRule::new());

//新增过滤规则（文件名和模块名）
#[derive(Debug, Default)]
struct DyndbgRule {
    file_keyword: Option<String>,
    module_keyword: Option<String>,
}

impl DyndbgRule {
    const fn new() -> Self {
        Self {
            file_keyword: None,
            module_keyword: None,
        }
    }
}

impl log::Log for AsterLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        //增加分支单独处理debug模式，进行动态过滤
        if record.level() == log::Level::Debug && !dyndbg_match(record) {
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

fn dyndbg_match(record: &Record) -> bool {
    let rule = DYNDBG_RULE.lock();
    let file_keyword = rule.file_keyword.as_deref();
    let module_keyword = rule.module_keyword.as_deref();
    //利用record提供的元数据进行匹配
    let file_matched = file_keyword
        .map(|needle| record.file().is_some_and(|file| file.contains(needle)))
        .unwrap_or(false);
    let module_matched = module_keyword
        .map(|needle| {
            record
                .module_path()
                .is_some_and(|module_path| module_path.contains(needle))
        })
        .unwrap_or(false);

    match (file_keyword, module_keyword) {
        (None, None) => true, 
        // 默认为true代表放行所有 debug 日志，与原来一致
        // 默认为false代表封锁所有 debug 日志，按需通过规则放行
        _ => file_matched || module_matched,
    }
}
//运行时调用，更新过滤规则
pub fn update_dyndbg_rule(file_keyword: Option<&str>, module_keyword: Option<&str>) {
    let mut rule = DYNDBG_RULE.lock();
    rule.file_keyword = file_keyword.map(String::from);
    rule.module_keyword = module_keyword.map(String::from);
}
//运行时调用，获取当前过滤规则

pub fn get_dyndbg_rule() -> (Option<String>, Option<String>) {
    let rule = DYNDBG_RULE.lock();
    (rule.file_keyword.clone(), rule.module_keyword.clone())
}

//清空过滤规则
pub fn clear_dyndbg_rule() {
    update_dyndbg_rule(None, None);
}

pub(super) fn init() {
    ostd::logger::inject_logger(&LOGGER);
}
