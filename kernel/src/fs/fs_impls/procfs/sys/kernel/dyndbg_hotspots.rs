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
/// Number of hottest sites shown by `cat`.
const TOP_N: usize = 10;

/// Represents the inode at `/proc/sys/kernel/dyndbg_hotspots`.
pub struct DyndbgHotspotsFileOps;

impl DyndbgHotspotsFileOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        ProcFileBuilder::new(Self, mkmod!(a+r, u+w))
            .parent(parent)
            .build()
            .unwrap()
    }
}

impl FileOps for DyndbgHotspotsFileOps {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        let mut printer = VmPrinter::new_skip(writer, offset);
        let hot = aster_logger::dyndbg_trace::hot_top(TOP_N);
        writeln!(
            printer,
            "top={} (sites with trace hits, counts summed across CPUs; reset with: reset)",
            TOP_N
        )?;
        if hot.is_empty() {
            writeln!(printer, "(no trace hits recorded; enable with +trace)")?;
        }
        for (rank, (idx, count)) in hot.iter().enumerate() {
            // The hotspot index equals the subscript of
            // `DYNDBG_DESCRIPTOR_REGISTRY` (assigned at boot in registry order).
            if let Some(descriptor) = aster_logger::DYNDBG_DESCRIPTOR_REGISTRY.get(*idx) {
                writeln!(
                    printer,
                    "{}. {}:{} [{}] {} count={}",
                    rank + 1,
                    descriptor.file,
                    descriptor.line,
                    descriptor.module_path,
                    descriptor.function_name().unwrap_or("<unknown>"),
                    count,
                )?;
            }
        }
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
            aster_logger::dyndbg_trace::reset_hot();
            return Ok(read_bytes);
        }

        return_errno_with_message!(Errno::EINVAL, "unknown command");
    }
}
