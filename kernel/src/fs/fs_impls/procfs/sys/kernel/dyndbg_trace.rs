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

/// Represents the inode at `/proc/sys/kernel/dyndbg_trace`.
pub struct DyndbgTraceFileOps;

impl DyndbgTraceFileOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        ProcFileBuilder::new(Self, mkmod!(a+r, u+w))
            .parent(parent)
            .build()
            .unwrap()
    }
}

impl FileOps for DyndbgTraceFileOps {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        let mut printer = VmPrinter::new_skip(writer, offset);
        let events = aster_logger::dyndbg_trace::snapshot_events();
        let count = aster_logger::dyndbg_trace::event_count();

        writeln!(printer, "events={} (total recorded since boot/last reset)", count)?;

        if events.is_empty() {
            writeln!(printer, "(no events)")?;
        }

        for event in &events {
            // Resolve descriptor_id back to file/line/function.
            let desc_info = resolve_descriptor(event.descriptor_id);
            writeln!(
                printer,
                "cpu={} tsc={} {}",
                event.cpu, event.tsc, desc_info,
            )?;
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
            aster_logger::dyndbg_trace::reset();
            return Ok(read_bytes);
        }

        return_errno_with_message!(Errno::EINVAL, "unknown command");
    }
}

/// Map a descriptor address back to `file:line (function)` for display.
fn resolve_descriptor(descriptor_id: u64) -> alloc::string::String {
    for desc in aster_logger::DYNDBG_DESCRIPTOR_REGISTRY {
        if (desc as *const _ as *const () as u64) == descriptor_id {
            let func = desc.function_name().unwrap_or("<unknown>");
            return alloc::format!("{}:{} ({})", desc.file, desc.line, func);
        }
    }
    alloc::format!("id=0x{:x} (unknown)", descriptor_id)
}
