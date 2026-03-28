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
        let (file_keyword, module_keyword) = aster_logger::get_dyndbg_rule();
        //打印过滤规则
        writeln!(
            printer,
            "file={} module={}",
            file_keyword.as_deref().unwrap_or("<none>"),
            module_keyword.as_deref().unwrap_or("<none>")
        )?;
        //用法提示
        writeln!(printer, "usage: file=<kw> +p|-p | module=<kw> +p|-p")?;

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
    //切分命令
    let mut parts = command.split_ascii_whitespace();
    //选择器
    let selector = parts
        .next()
        .ok_or_else(|| Error::with_message(Errno::EINVAL, "missing selector"))?;
    //操作
    let action = parts
        .next()
        .ok_or_else(|| Error::with_message(Errno::EINVAL, "missing action (+p/-p)"))?;

    if parts.next().is_some() {
        return_errno_with_message!(Errno::EINVAL, "unexpected trailing tokens");
    }
    //将选择器由key=value形式切分成key和value
    let (key, value) = selector
        .split_once('=')
        .ok_or_else(|| Error::with_message(Errno::EINVAL, "selector must be key=value"))?;
    if value.is_empty() {
        return_errno_with_message!(Errno::EINVAL, "selector value must not be empty");
    }

    match action {
        "+p" => enable_selector(key, value),
        "-p" => disable_selector(key),
        _ => return_errno_with_message!(Errno::EINVAL, "action must be +p or -p"),
    }
}

// +p表示启用key过滤器，-p表示禁用key对应的过滤器
// 目前file module各最多过滤一个关键字
fn enable_selector(key: &str, value: &str) -> Result<()> {
    let (file_keyword, module_keyword) = aster_logger::get_dyndbg_rule();

    match key {
        "file" => aster_logger::update_dyndbg_rule(Some(value), module_keyword.as_deref()),
        "module" => aster_logger::update_dyndbg_rule(file_keyword.as_deref(), Some(value)),
        _ => return_errno_with_message!(Errno::EINVAL, "selector key must be file or module"),
    }

    Ok(())
}

fn disable_selector(key: &str) -> Result<()> {
    let (file_keyword, module_keyword) = aster_logger::get_dyndbg_rule();

    match key {
        "file" => aster_logger::update_dyndbg_rule(None, module_keyword.as_deref()),
        "module" => aster_logger::update_dyndbg_rule(file_keyword.as_deref(), None),
        _ => return_errno_with_message!(Errno::EINVAL, "selector key must be file or module"),
    }

    Ok(())
}
