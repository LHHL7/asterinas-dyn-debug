// SPDX-License-Identifier: MPL-2.0

use aster_util::printer::VmPrinter;
use ostd::sync::SpinLock;
use aster_logger::DebugDescriptor;
use aster_logger::dyndbg_trace::TraceEvent;

use crate::{
    fs::{
        file::mkmod,
        procfs::template::{FileOps, ProcFileBuilder},
        vfs::inode::Inode,
    },
    prelude::*,
};

const MAX_CMD_LEN: usize = 32;

/// Events drained by the snapshot at the start of a `cat`-style read.
///
/// A procfs read is served over multiple `read_at` calls (one per page).
/// The ring drain in [`snapshot_events`] is destructive, so the snapshot must
/// be taken exactly once per read session (offset == 0) and re-served on
/// subsequent calls; otherwise every event beyond the first page is drained
/// and silently dropped.
struct PendingSnapshot {
    events: alloc::vec::Vec<TraceEvent>,
    lost_this_read: u64,
}

/// Represents the inode at `/proc/sys/kernel/dyndbg_trace`.
pub struct DyndbgTraceFileOps {
    /// Snapshot for the in-flight read session, shared across `read_at` calls
    /// (the FileOps instance is created once per inode, not per open).
    pending: SpinLock<Option<PendingSnapshot>>,
}

impl DyndbgTraceFileOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        ProcFileBuilder::new(
            Self {
                pending: SpinLock::new(None),
            },
            mkmod!(a+r, u+w),
        )
        .parent(parent)
        .build()
        .unwrap()
    }
}

impl FileOps for DyndbgTraceFileOps {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        let mut printer = VmPrinter::new_skip(writer, offset);

        // Drain the ring before reading the counters: the loss accumulated by
        // this snapshot only becomes visible in `lost_count()` after the
        // drain, and the summary must agree with the snapshot line below.
        let mut pending = self.pending.lock();
        if offset == 0 {
            let (events, lost_this_read) = aster_logger::dyndbg_trace::snapshot_events();
            *pending = Some(PendingSnapshot {
                events,
                lost_this_read,
            });
        }
        let count = aster_logger::dyndbg_trace::event_count();
        let lost = aster_logger::dyndbg_trace::lost_count();

        writeln!(printer, "events={}", count)?;
        writeln!(printer, "lost={}", lost)?;

        if let Some(snapshot) = pending.as_ref() {
            writeln!(
                printer,
                "snapshot: {} events, {} lost (this read)",
                snapshot.events.len(),
                snapshot.lost_this_read
            )?;

            if snapshot.events.is_empty() {
                writeln!(printer, "(no events)")?;
            }

            for event in &snapshot.events {
                // Resolve descriptor_id back to file/line/function.
                let desc_info = resolve_descriptor(event.descriptor_id);
                writeln!(
                    printer,
                    "cpu={} tsc={} {}",
                    event.cpu, event.tsc, desc_info,
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
            aster_logger::dyndbg_trace::reset();
            return Ok(read_bytes);
        }

        return_errno_with_message!(Errno::EINVAL, "unknown command");
    }
}

/// Map a descriptor address back to `file:line [module] function` for display
/// (same order/format as the `cat dynamic_debug` status listing).
fn resolve_descriptor(descriptor_id: u64) -> alloc::string::String {
    for desc in aster_logger::DYNDBG_DESCRIPTOR_REGISTRY {
        // The registry is a slice of references, so each item is
        // `&&DebugDescriptor`; deref once before taking the address, otherwise
        // the comparison hits the slot address inside the registry array
        // instead of the descriptor itself.
        let desc_addr = *desc as *const DebugDescriptor as *const () as u64;
        if desc_addr == descriptor_id {
            let func = desc.function_name().unwrap_or("<unknown>");
            return alloc::format!(
                "{}:{} [{}] {}",
                desc.file,
                desc.line,
                desc.module_path,
                func
            );
        }
    }
    alloc::format!("id=0x{:x} (unknown)", descriptor_id)
}
