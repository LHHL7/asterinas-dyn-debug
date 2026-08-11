// SPDX-License-Identifier: MPL-2.0

//! Read-only backing store for `/dev/kmsg` (the `dmesg` channel).
//!
//! Every chunk printed through [`crate::console::_print`] is mirrored into a
//! fixed-size ring buffer here.  Readers (the mem device in the kernel) drain
//! it consume-on-read style, matching Linux's `/dev/kmsg` semantics: bytes are
//! delivered to exactly one reader, and a reader that falls behind skips the
//! bytes that were overwritten by the ring wrap.
//!
//! All operations are lock-protected, allocation-free and sleep-free, so they
//! are safe to call while the console device lock is held and under low-memory
//! conditions (the heap allocator itself logs via `_print`).

use ostd::mm::{FallibleVmWrite, VmReader};
use ostd::sync::SpinLock;

/// Ring capacity in bytes.
const RING_CAPACITY: usize = 512 * 1024;

struct Ring {
    buf: [u8; RING_CAPACITY],
    /// Total number of bytes appended.
    written: usize,
    /// Sequence number of the oldest retained byte.
    kept_start: usize,
    /// Sequence number of the next byte to deliver to readers.
    read_pos: usize,
}

static KMSG_RING: SpinLock<Ring> = SpinLock::new(Ring {
    buf: [0; RING_CAPACITY],
    written: 0,
    kept_start: 0,
    read_pos: 0,
});

/// Mirror `bytes` into the ring, overwriting the oldest retained bytes if the
/// ring fills up.
pub fn append(bytes: &[u8]) {
    let mut ring = KMSG_RING.lock();
    for &b in bytes {
        let idx = ring.written % RING_CAPACITY;
        ring.buf[idx] = b;
        ring.written += 1;
        if ring.written - ring.kept_start > RING_CAPACITY {
            ring.kept_start += 1;
        }
    }
    if ring.read_pos < ring.kept_start {
        // Unread bytes were overwritten; drop them from the read window.
        ring.read_pos = ring.kept_start;
    }
}

/// Drain up to `out.len()` of the oldest not-yet-read bytes into `out`.
///
/// Returns the number of bytes written; `0` means the ring is drained (EOF
/// for a `dmesg`-style reader).
pub fn read_into(out: &mut [u8]) -> usize {
    let mut ring = KMSG_RING.lock();
    let avail = ring.written - ring.read_pos;
    let n = avail.min(out.len());
    for i in 0..n {
        out[i] = ring.buf[(ring.read_pos + i) % RING_CAPACITY];
    }
    ring.read_pos += n;
    n
}

/// Drop all retained bytes (`dmesg -c`).
pub fn clear() {
    let mut ring = KMSG_RING.lock();
    ring.kept_start = ring.written;
    ring.read_pos = ring.written;
}

/// Copy all retained bytes into `writer`, then drop them
/// (`syslog(SYSLOG_ACTION_READ_CLEAR)`, used by `dmesg -c`).
pub fn read_all_clear(
    writer: &mut ostd::mm::VmWriter,
) -> core::result::Result<usize, ostd::Error> {
    let n = read_all(writer)?;
    clear();
    Ok(n)
}

/// Number of retained bytes (`syslog(SYSLOG_ACTION_SIZE_BUFFER)`).
pub fn size_now() -> usize {
    let ring = KMSG_RING.lock();
    ring.written - ring.kept_start
}

/// Non-consuming copy of all retained bytes into `writer`
/// (`syslog(SYSLOG_ACTION_READ_ALL)`): unlike [`read_into`], the ring is left
/// intact so every `dmesg` invocation sees the same snapshot.
pub fn read_all(writer: &mut ostd::mm::VmWriter) -> core::result::Result<usize, ostd::Error> {
    let mut ring = KMSG_RING.lock();
    let avail = ring.written - ring.kept_start;
    let n = avail.min(writer.avail());

    let start_pos = ring.kept_start % RING_CAPACITY;
    let seg1 = n.min(RING_CAPACITY - start_pos);
    let mut written = writer
        .write_fallible(&mut ostd::mm::VmReader::from(&ring.buf[start_pos..start_pos + seg1]))
        .map_err(|(e, _)| e)?;
    if written < n {
        written += writer
            .write_fallible(&mut ostd::mm::VmReader::from(&ring.buf[..n - written]))
            .map_err(|(e, _)| e)?;
    }
    Ok(written)
}
