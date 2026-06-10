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

const MAX_CMD_LEN: usize = 32;

/// Represents the inode at `/proc/sys/kernel/dyndbg_stats`.
pub struct DyndbgStatsFileOps;

impl DyndbgStatsFileOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        ProcFileBuilder::new(Self, mkmod!(a+r, u+w))
            .parent(parent)
            .build()
            .unwrap()
    }
}

impl FileOps for DyndbgStatsFileOps {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        let mut printer = VmPrinter::new_skip(writer, offset);
        let stats = aster_logger::get_dyndbg_stats_snapshot();

        writeln!(
            printer,
            "descriptors_recomputed={}",
            stats.descriptors_recomputed
        )?;
        writeln!(printer, "modules_repatched={}", stats.modules_repatched)?;
        writeln!(printer, "sites_patched={}", stats.sites_patched)?;
        writeln!(printer, "patch_transactions={}", stats.patch_transactions)?;
        writeln!(
            printer,
            "last_update_latency_us={}",
            stats.last_update_latency_us
        )?;
        writeln!(printer, "usage: reset")?;

        Ok(printer.bytes_written())
    }

    fn write_at(&self, _offset: usize, reader: &mut VmReader) -> Result<usize> {
        let (command, read_bytes) = reader.read_cstring_until_end(MAX_CMD_LEN)?;
        let command = command
            .to_str()
            .map_err(|_| Error::with_message(Errno::EINVAL, "command is not valid UTF-8"))?
            .trim();

        if command.is_empty() {
            return_errno_with_message!(Errno::EINVAL, "empty command");
        }

        if command == "reset" {
            aster_logger::reset_dyndbg_stats();
            return Ok(read_bytes);
        }

        return_errno_with_message!(Errno::EINVAL, "unknown command");
    }
}
