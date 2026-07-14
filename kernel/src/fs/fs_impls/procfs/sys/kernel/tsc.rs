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

/// Represents the inode at `/proc/sys/kernel/tsc`.
/// Provides userspace access to the CPU Time Stamp Counter for
/// high-precision benchmarking.  Reading this file returns the
/// current TSC value and the calibrated TSC frequency so that
/// shell scripts can convert TSC deltas to wall-clock time.
pub struct TscFileOps;

impl TscFileOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        ProcFileBuilder::new(Self, mkmod!(a+r))
            .parent(parent)
            .build()
            .unwrap()
    }
}

impl FileOps for TscFileOps {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        let mut printer = VmPrinter::new_skip(writer, offset);

        let tsc = ostd::arch::read_tsc();
        let freq = ostd::arch::tsc_freq();

        // Format: "tsc_freq=<hz> tsc=<current_tsc>"
        // The caller computes duration as (tsc_end - tsc_start) * 1_000_000 / tsc_freq
        // to obtain microseconds.
        writeln!(printer, "{} {}", freq, tsc)?;

        Ok(printer.bytes_written())
    }
}
