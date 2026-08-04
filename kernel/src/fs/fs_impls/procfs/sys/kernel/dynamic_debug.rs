// SPDX-License-Identifier: MPL-2.0

use aster_util::printer::VmPrinter;

use crate::{
    fs::{
        file::mkmod,
        procfs::template::{FileOps, ProcFileBuilder},
        vfs::inode::Inode,
    },
    prelude::*,
};

/// Represents the inode at `/proc/sys/kernel/dynamic_debug`.
pub struct DynamicDebugFileOps;

impl DynamicDebugFileOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        //创建虚拟文件 挂载到parent目录下
        ProcFileBuilder::new(Self, mkmod!(a+r, u+w))
            .parent(parent)
            .build()
            .unwrap()
    }
}
//实现FileOps接口，支持读写 对应cat和echo命令
impl FileOps for DynamicDebugFileOps {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        let mut printer = VmPrinter::new_skip(writer, offset);
        let rules = aster_logger::get_dyndbg_rule_chain_snapshot();
        // 打印规则链（last-match-wins；flags 累积）。
        writeln!(printer, "rules={} (last-match-wins)", rules.len())?;
        for (index, entry) in rules.iter().enumerate() {
            let action_str = match entry.action {
                aster_logger::DyndbgRuleActionSnapshot::EnableLog => "+p",
                aster_logger::DyndbgRuleActionSnapshot::DisableLog => "-p",
                aster_logger::DyndbgRuleActionSnapshot::EnableTrace => "+trace",
                aster_logger::DyndbgRuleActionSnapshot::DisableTrace => "-trace",
                aster_logger::DyndbgRuleActionSnapshot::KeepState => "flags-only",
            };
            let flags_str = format_flags(
                entry.rule.flags_set,
                entry.rule.flags_clear,
                entry.rule.flags_override,
            );
            writeln!(
                printer,
                "{}: file={} module={} func={} line={} {} {}",
                index,
                entry.rule.file_keyword.as_deref().unwrap_or("*"),
                entry.rule.module_keyword.as_deref().unwrap_or("*"),
                entry.rule.function_keyword.as_deref().unwrap_or("*"),
                entry
                    .rule
                    .line
                    .map(|line| line.to_string())
                    .as_deref()
                    .unwrap_or("*"),
                action_str,
                flags_str,
            )?;
        }
        // 调试点状态列表（Linux debugfs 读取侧的对标）。
        let descriptors = aster_logger::DYNDBG_DESCRIPTOR_REGISTRY;
        writeln!(printer, "descriptors={}", descriptors.len())?;
        for descriptor in descriptors {
            let log_str = if descriptor.should_log_fast() {
                "+p"
            } else {
                "-p"
            };
            let trace_str = if descriptor.should_trace_fast() {
                "+trace"
            } else {
                "-trace"
            };
            let flags_str = format_active_flags(descriptor.format_flags());
            writeln!(
                printer,
                "{}:{} [{}] {} {} {} {}",
                descriptor.file,
                descriptor.line,
                descriptor.module_path,
                descriptor.function_name().unwrap_or("<unknown>"),
                log_str,
                trace_str,
                flags_str,
            )?;
        }
        //用法提示
        writeln!(
            printer,
            "usage: [file=<kw>] [module=<kw>] [func=<kw>] [line=<n>] <action> | del <id> | clear\n\
             action: +p|-p|+trace|-trace [+|-|=][f][l][m][t][_]  e.g. +pfl (enable log with func+line prefixes), +f (flags only), =fl (overwrite to func+line), +_ (clear all flags)"
        )?;

        Ok(printer.bytes_written())
    }

    fn write_at(&self, _offset: usize, reader: &mut VmReader) -> Result<usize> {
        const MAX_CMD_LEN: usize = 256;
        //读取命令字符串，例如file=test +p
        let (command, read_bytes) = reader.read_cstring_until_end(MAX_CMD_LEN)?;
        let command = command
            .to_str()
            .map_err(|_| Error::with_message(Errno::EINVAL, "command is not valid UTF-8"))?
            .trim();

        if command.is_empty() {
            return_errno_with_message!(Errno::EINVAL, "empty command");
        }
        //真正的命令处理
        apply_command(command)?;
        Ok(read_bytes)
    }
}

fn apply_command(command: &str) -> Result<()> {
    if command == "clear" {
        aster_logger::clear_dyndbg_rules();
        return Ok(());
    }

    if let Some(rule_id) = command.strip_prefix("del ") {
        return delete_rule(rule_id.trim());
    }

    // Last token is the action (optionally combined with format flags, Linux
    // style: `+pfl`, `+f`, `-fl`, `+_`, `+trace`/`-trace`), previous tokens
    // are selectors.
    let mut parts = command.split_ascii_whitespace().collect::<Vec<_>>();
    if parts.is_empty() {
        return_errno_with_message!(Errno::EINVAL, "empty command");
    }

    let action_token = parts
        .pop()
        .ok_or_else(|| Error::with_message(Errno::EINVAL, "missing action (+p/-p)"))?;
    let selectors = parts;

    let parsed = parse_action(action_token)?;
    append_rule(&selectors, parsed)
}

/// Result of parsing one action token: an optional log/trace switch plus the
/// format-flag operation it carries.
struct ParsedAction {
    action: Option<aster_logger::DyndbgRuleActionSnapshot>,
    flags_set: u8,
    flags_clear: u8,
    flags_override: Option<u8>,
}

/// Parse a Linux-style action token into an optional log/trace switch and
/// format-flag bits.
///
/// Grammar: `[+|-|=][p][f][l][m][t][_]`, e.g.
/// - `+pfl` — enable log output and add function + line prefixes
/// - `+f` — flags only: add the function prefix (switch untouched)
/// - `-fl` — remove function + line prefixes (switch untouched)
/// - `=fl` — overwrite: replace the flags with exactly f+l (Linux semantics)
/// - `+_` — clear all format flags
/// - `-p` / `+trace` / `-trace` — plain switches, unchanged from before
///
/// `+` sets bits, `-` clears them, `=` overwrites the whole flag set.  The
/// `p` switch follows last-match-wins semantics on the rule chain; the
/// flag operation is replayed in chain order by the filter engine.
fn parse_action(token: &str) -> Result<ParsedAction> {
    // 保留独立的 trace 动作词（trace 不是单字符 flag）。
    match token {
        "+trace" => {
            return Ok(ParsedAction {
                action: Some(aster_logger::DyndbgRuleActionSnapshot::EnableTrace),
                flags_set: 0,
                flags_clear: 0,
                flags_override: None,
            })
        }
        "-trace" => {
            return Ok(ParsedAction {
                action: Some(aster_logger::DyndbgRuleActionSnapshot::DisableTrace),
                flags_set: 0,
                flags_clear: 0,
                flags_override: None,
            })
        }
        _ => {}
    }

    // 逐字符解析 先拿出操作符 + - =
    let mut chars = token.chars();
    let op = chars
        .next()
        .ok_or_else(|| Error::with_message(Errno::EINVAL, "empty action"))?;
    if op != '+' && op != '-' && op != '=' {
        return_errno_with_message!(
            Errno::EINVAL,
            "action must start with +, -, or ="
        );
    }
    let clearing = op == '-';
    let overwriting = op == '=';

    // 然后解析后续的标志字符
    let mut action = None;
    let mut set = 0u8;
    let mut clear = 0u8;
    let mut override_flags: Option<u8> = None;
    for c in chars {
        let bit = match c {
            'p' => {
                action = Some(if clearing {
                    aster_logger::DyndbgRuleActionSnapshot::DisableLog
                } else {
                    aster_logger::DyndbgRuleActionSnapshot::EnableLog
                });
                continue;
            }
            'f' => aster_logger::FLAG_FUNCTION,
            'l' => aster_logger::FLAG_LINE,
            'm' => aster_logger::FLAG_MODULE,
            't' => aster_logger::FLAG_THREAD,
            '_' => {
                // `+_`/`=_`: clear all format flags (Linux semantics).
                if clearing {
                    return_errno_with_message!(Errno::EINVAL, "'_' cannot be cleared");
                }
                if overwriting {
                    override_flags = Some(0);
                } else {
                    // `+_`: clear all flags (Linux semantics).
                    clear = u8::MAX;
                }
                continue;
            }
            _ => {
                return_errno_with_message!(
                    Errno::EINVAL,
                    "unknown flag character in action"
                )
            }
        };
        if overwriting {
            override_flags = Some(override_flags.unwrap_or(0) | bit);
        } else if clearing {
            clear |= bit;
        } else {
            set |= bit;
        }
    }

    Ok(ParsedAction {
        action,
        flags_set: set,
        flags_clear: clear,
        flags_override: override_flags,
    })
}

fn delete_rule(rule_id: &str) -> Result<()> {
    if rule_id.is_empty() {
        return_errno_with_message!(Errno::EINVAL, "missing rule id");
    }

    let rule_id = rule_id
        .parse::<usize>()
        .map_err(|_| Error::with_message(Errno::EINVAL, "rule id must be a valid usize"))?;

    if !aster_logger::remove_dyndbg_rule_by_id(rule_id) {
        return_errno_with_message!(Errno::EINVAL, "rule id out of range");
    }

    Ok(())
}

fn parse_selector(selector: &str) -> Result<(&str, &str)> {
    let (key, value) = selector
        .split_once('=')
        .ok_or_else(|| Error::with_message(Errno::EINVAL, "selector must be key=value"))?;
    if value.is_empty() {
        return_errno_with_message!(Errno::EINVAL, "selector value must not be empty");
    }

    Ok((key, value))
}

/// Render format-flag bits as a readable `+f/+l/+m/+t` / `=fl` string for `cat`.
fn format_flags(set: u8, clear: u8, override_flags: Option<u8>) -> alloc::string::String {
    use aster_logger::{FLAG_FUNCTION, FLAG_LINE, FLAG_MODULE, FLAG_THREAD};

    // `=fl` rules render as an exact overwrite target.
    if let Some(v) = override_flags {
        let mut s = alloc::string::String::from('=');
        if v == 0 {
            s.push('_');
            return s;
        }
        for (bit, ch) in [
            (FLAG_FUNCTION, 'f'),
            (FLAG_LINE, 'l'),
            (FLAG_MODULE, 'm'),
            (FLAG_THREAD, 't'),
        ] {
            if v & bit != 0 {
                s.push(ch);
            }
        }
        return s;
    }

    let mut s = alloc::string::String::new();
    if set != 0 {
        s.push('+');
        for (bit, ch) in [
            (FLAG_FUNCTION, 'f'),
            (FLAG_LINE, 'l'),
            (FLAG_MODULE, 'm'),
            (FLAG_THREAD, 't'),
        ] {
            if set & bit != 0 {
                s.push(ch);
            }
        }
    }
    if clear == u8::MAX {
        s.push_str("+_");
    } else if clear != 0 {
        s.push('-');
        for (bit, ch) in [
            (FLAG_FUNCTION, 'f'),
            (FLAG_LINE, 'l'),
            (FLAG_MODULE, 'm'),
            (FLAG_THREAD, 't'),
        ] {
            if clear & bit != 0 {
                s.push(ch);
            }
        }
    }
    if s.is_empty() {
        s.push('-');
    }
    s
}

/// Render the *currently effective* format flags of a descriptor as a
/// `+fl` / `-` string (used by the descriptor status listing).
fn format_active_flags(flags: u8) -> alloc::string::String {
    use aster_logger::{FLAG_FUNCTION, FLAG_LINE, FLAG_MODULE, FLAG_THREAD};

    let mut s = alloc::string::String::new();
    if flags != 0 {
        s.push('+');
        for (bit, ch) in [
            (FLAG_FUNCTION, 'f'),
            (FLAG_LINE, 'l'),
            (FLAG_MODULE, 'm'),
            (FLAG_THREAD, 't'),
        ] {
            if flags & bit != 0 {
                s.push(ch);
            }
        }
    } else {
        s.push('-');
    }
    s
}

fn append_rule(selectors: &[&str], parsed: ParsedAction) -> Result<()> {
    let mut rule = aster_logger::DyndbgRuleSnapshot::default();
    for selector in selectors {
        let (key, value) = parse_selector(selector)?;
        match key {
            "file" => rule.file_keyword = Some(value.to_string()),
            "module" => rule.module_keyword = Some(value.to_string()),
            "func" => rule.function_keyword = Some(value.to_string()),
            "line" => {
                let line = value
                    .parse::<u32>()
                    .map_err(|_| Error::with_message(Errno::EINVAL, "line must be a valid u32"))?;
                rule.line = Some(line);
            }
            _ => {
                return_errno_with_message!(
                    Errno::EINVAL,
                    "selector key must be file/module/func/line"
                )
            }
        }
    }
    rule.flags_set = parsed.flags_set;
    rule.flags_clear = parsed.flags_clear;
    rule.flags_override = parsed.flags_override;

    match parsed.action {
        Some(action) => aster_logger::append_dyndbg_rule(rule, action),
        // Flags-only rule: switch untouched.
        None => aster_logger::append_dyndbg_rule(
            rule,
            aster_logger::DyndbgRuleActionSnapshot::KeepState,
        ),
    }

    Ok(())
}
