// SPDX-License-Identifier: MPL-2.0

//! Lock-free ring buffer for dyndbg structured tracing.
//!
//! When a dyndbg descriptor is in Trace mode, every hit pushes a compact
//! [`TraceEvent`] into a global ring buffer — zero string allocation,
//! zero branching overhead in the disabled path (NOP5).
//!
//! # Usage
//!
//! ```ignore
//! // Enable trace for a module:
//! //   echo module=ext2 +trace > /proc/sys/kernel/dynamic_debug
//!
//! // Later, snapshot the events:
//! for event in aster_logger::dyndbg_trace::snapshot_events() {
//!     log::info!("id=0x{:x} cpu={} tsc={}", event.descriptor_id, event.cpu, event.tsc);
//! }
//! ```

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Maximum number of buffered events (power of 2 for mask-based wrap).
const RING_CAPACITY: usize = 1024;
const RING_MASK: usize = RING_CAPACITY - 1;

/// A single trace event recorded when a dyndbg call site is hit in Trace mode.
///
/// 12 bytes (packed).  The `descriptor_id` is the address of the
/// [`super::DebugDescriptor`], which can be mapped back to file/module/line
/// via the descriptor registry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TraceEvent {
    /// Unique identifier of the dyndbg descriptor (its static address).
    pub descriptor_id: u64,
    /// TSC timestamp at capture time.
    pub tsc: u64,
    /// CPU where the event occurred.
    pub cpu: u32,
}

struct RingBuffer {
    events: [TraceEvent; RING_CAPACITY],
    /// Monotonically-increasing write index (modulo RING_CAPACITY).
    head: AtomicUsize,
}

impl RingBuffer {
    const fn new() -> Self {
        const EMPTY: TraceEvent = TraceEvent {
            descriptor_id: 0,
            tsc: 0,
            cpu: 0,
        };
        Self {
            events: [EMPTY; RING_CAPACITY],
            head: AtomicUsize::new(0),
        }
    }

    fn push(&self, event: TraceEvent) {
        let idx = self.head.fetch_add(1, Ordering::Relaxed) & RING_MASK;
        #[allow(unsafe_code)]
        // SAFETY: idx < RING_CAPACITY due to mask; fetch_add gives each
        // producer a unique slot so no write-write races.
        unsafe {
            let ptr = self.events.as_ptr().add(idx) as *mut TraceEvent;
            core::ptr::write_volatile(ptr, event);
        }
    }

    fn snapshot(&self) -> alloc::vec::Vec<TraceEvent> {
        let head = self.head.load(Ordering::Relaxed);
        let start = if head > RING_CAPACITY {
            head - RING_CAPACITY
        } else {
            0
        };
        (start..head)
            .map(|i| {
                let idx = i & RING_MASK;
                #[allow(unsafe_code)]
                // SAFETY: idx < RING_CAPACITY due to mask.
                unsafe {
                    let ptr = self.events.as_ptr().add(idx);
                    core::ptr::read_volatile(ptr)
                }
            })
            .collect()
    }
}

/// Global ring buffer shared across all CPUs.
static TRACE_RING: RingBuffer = RingBuffer::new();

/// Cumulative count for quick inspection without snapshotting.
static TRACE_EVENT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Push a trace event into the ring buffer (called from the dyndbg macro
/// hot path when the descriptor is in Trace mode).
///
/// # Lock-freedom
///
/// Uses `fetch_add` for slot allocation + `write_volatile` for the store.
/// No locks, no CAS loops.  Safe for concurrent producers on different CPUs.
#[inline(always)]
pub fn push_trace_event(descriptor_id: u64) {
    let event = TraceEvent {
        descriptor_id,
        tsc: ostd::arch::read_tsc(),
        cpu: u32::from(ostd::cpu::CpuId::current_racy()),
    };
    TRACE_RING.push(event);
    TRACE_EVENT_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Return a read-only snapshot of all buffered events (oldest first).
pub fn snapshot_events() -> alloc::vec::Vec<TraceEvent> {
    TRACE_RING.snapshot()
}

/// Number of events recorded since boot (or last reset).
pub fn event_count() -> u64 {
    TRACE_EVENT_COUNT.load(Ordering::Relaxed)
}

/// Reset the event counter (does not clear the ring buffer).
pub fn reset_event_count() {
    TRACE_EVENT_COUNT.store(0, Ordering::Relaxed);
}

/// Reset both the ring buffer and event counter.
pub fn reset() {
    TRACE_RING.head.store(0, Ordering::Relaxed);
    TRACE_EVENT_COUNT.store(0, Ordering::Relaxed);
}
