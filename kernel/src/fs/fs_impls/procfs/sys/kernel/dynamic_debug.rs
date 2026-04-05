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
        let rule = aster_logger::get_dyndbg_rule_snapshot();
        //打印过滤规则
        writeln!(
            printer,
            "file={} module={} func={} line={}",
            rule.file_keyword.as_deref().unwrap_or("<none>"),
            rule.module_keyword.as_deref().unwrap_or("<none>"),
            rule.function_keyword.as_deref().unwrap_or("<none>"),
            rule.line
                .map(|line| line.to_string())
                .as_deref()
                .unwrap_or("<none>")
        )?;
        //用法提示
        writeln!(
            printer,
            "usage: [file=<kw>] [module=<kw>] [func=<kw>] [line=<n>] +p|-p"
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
        "+p" => enable_selectors(&selectors),
        "-p" => disable_selectors(&selectors),
        _ => return_errno_with_message!(Errno::EINVAL, "action must be +p or -p"),
    }
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

fn enable_selectors(selectors: &[&str]) -> Result<()> {
    if selectors.is_empty() {
        return_errno_with_message!(Errno::EINVAL, "+p requires at least one selector");
    }

    let mut rule = aster_logger::get_dyndbg_rule_snapshot();
    for selector in selectors {
        let (key, value) = parse_selector(selector)?;
        match key {
            "file" => rule.file_keyword = Some(value.to_string()),
            "module" => rule.module_keyword = Some(value.to_string()),
            "func" => rule.function_keyword = Some(value.to_string()),
            "line" => {
                let line = value.parse::<u32>().map_err(|_| {
                    Error::with_message(Errno::EINVAL, "line must be a valid u32")
                })?;
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

    aster_logger::set_dyndbg_rule(rule);

    Ok(())
}

fn disable_selectors(selectors: &[&str]) -> Result<()> {
    if selectors.is_empty() {
        aster_logger::clear_dyndbg_rule();
        return Ok(());
    }

    let mut rule = aster_logger::get_dyndbg_rule_snapshot();
    for selector in selectors {
        let (key, _value) = parse_selector(selector)?;
        match key {
            "file" => rule.file_keyword = None,
            "module" => rule.module_keyword = None,
            "func" => rule.function_keyword = None,
            "line" => rule.line = None,
            _ => {
                return_errno_with_message!(
                    Errno::EINVAL,
                    "selector key must be file/module/func/line"
                )
            }
        }
    }

    aster_logger::set_dyndbg_rule(rule);

    Ok(())
}
