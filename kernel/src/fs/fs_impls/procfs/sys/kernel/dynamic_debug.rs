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
        // 打印规则链（last-match-wins）。
        writeln!(printer, "rules={} (last-match-wins)", rules.len())?;
        for (index, entry) in rules.iter().enumerate() {
            let action_str = match entry.action {
                aster_logger::DyndbgRuleActionSnapshot::EnableLog => "+p",
                aster_logger::DyndbgRuleActionSnapshot::DisableLog => "-p",
                aster_logger::DyndbgRuleActionSnapshot::EnableTrace => "+trace",
                aster_logger::DyndbgRuleActionSnapshot::DisableTrace => "-trace",
            };
            writeln!(
                printer,
                "{}: file={} module={} func={} line={} {}",
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
            )?;
        }
        //用法提示
        writeln!(
            printer,
            "usage: [file=<kw>] [module=<kw>] [func=<kw>] [line=<n>] +p|-p|+trace|-trace | del <id> | clear"
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

    // Last token is action, previous tokens are selectors.
    let mut parts = command.split_ascii_whitespace().collect::<Vec<_>>();
    if parts.is_empty() {
        return_errno_with_message!(Errno::EINVAL, "empty command");
    }

    let action = parts
        .pop()
        .ok_or_else(|| Error::with_message(Errno::EINVAL, "missing action (+p/-p)"))?;
    let selectors = parts;

    match action {
        "+p" => append_rule(&selectors, aster_logger::DyndbgRuleActionSnapshot::EnableLog),
        "-p" => append_rule(&selectors, aster_logger::DyndbgRuleActionSnapshot::DisableLog),
        "+trace" => append_rule(&selectors, aster_logger::DyndbgRuleActionSnapshot::EnableTrace),
        "-trace" => append_rule(&selectors, aster_logger::DyndbgRuleActionSnapshot::DisableTrace),
        _ => return_errno_with_message!(Errno::EINVAL, "action must be +p, -p, +trace, or -trace"),
    }
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

fn append_rule(
    selectors: &[&str],
    action: aster_logger::DyndbgRuleActionSnapshot,
) -> Result<()> {
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

    aster_logger::append_dyndbg_rule(rule, action);

    Ok(())
}
