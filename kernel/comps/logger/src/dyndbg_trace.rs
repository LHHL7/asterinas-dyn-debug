// SPDX-License-Identifier: MPL-2.0

//! Lock-free per-CPU ring buffers for dyndbg structured tracing.
//!
//! When a dyndbg descriptor is in Trace mode, every hit pushes a compact
//! [`TraceEvent`] into the ring buffer of the *current CPU* — zero string
//! allocation, zero branching overhead in the disabled path (NOP5), and no
//! cross-CPU contention on the write index (each CPU owns its own `head`).
//!
//! # Usage
//!
//! ```ignore
//! // Enable trace for a module:
//! //   echo module=ext2 +trace > /proc/sys/kernel/dynamic_debug
//!
//! // Later, snapshot the events:
//! let (events, lost) = aster_logger::dyndbg_trace::snapshot_events();
//! for event in events {
//!     log::info!("id=0x{:x} cpu={} tsc={}", event.descriptor_id, event.cpu, event.tsc);
//! }
//! ```
//!
//! # Multi-CPU semantics
//!
//! - Producers write only to their own CPU's ring (via `CpuId::current_racy`),
//!   so `fetch_add` on `head` never contends across CPUs.
//! - [`snapshot_events`] drains the events produced since the *previous*
//!   snapshot on every CPU and merges them sorted by TSC, restoring a global
//!   time order.  Events lost to ring overwrite between snapshots are counted
//!   in [`lost_count`].

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ostd::cpu::{local::CpuLocal, CpuId};
use ostd::cpu_local;

/// Maximum number of buffered events per CPU (power of 2 for mask-based wrap).
const RING_CAPACITY: usize = 1024;
const RING_MASK: usize = RING_CAPACITY - 1;

/// A single trace event recorded when a dyndbg call site is hit in Trace mode.
///
/// The `descriptor_id` is the address of the
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
    /// Snapshot watermark: events with index < `snapshot_head` have already
    /// been consumed by the previous snapshot; only
    /// `[snapshot_head, head)` is drained next time.
    snapshot_head: AtomicUsize,
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
            snapshot_head: AtomicUsize::new(0),
        }
    }

    fn push(&self, event: TraceEvent) {
        let idx = self.head.fetch_add(1, Ordering::Relaxed) & RING_MASK;
        #[allow(unsafe_code)]
        // SAFETY: idx < RING_CAPACITY due to mask; this CPU is the only
        // producer of its own ring (accessed via `get_with`), so no
        // write-write races.
        unsafe {
            let ptr = self.events.as_ptr().add(idx) as *mut TraceEvent;
            core::ptr::write_volatile(ptr, event);
        }
    }

    /// Drain events produced since the previous snapshot into `out`.
    ///
    /// Returns the number of events lost to ring overwrite since the previous
    /// snapshot (produced count exceeding the ring capacity).
    fn drain_since(&self, out: &mut alloc::vec::Vec<TraceEvent>) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let watermark = self.snapshot_head.load(Ordering::Relaxed);
        let produced = head.saturating_sub(watermark);
        let available = produced.min(RING_CAPACITY);
        let lost = produced - available;

        for i in 0..available {
            let idx = (watermark + i) & RING_MASK;
            #[allow(unsafe_code)]
            // SAFETY: idx < RING_CAPACITY due to mask; the slot was written by
            // `write_volatile` and is read before any possible overwrite of
            // this slot (overwrites only affect slots below `head - CAP`).
            unsafe {
                let ptr = self.events.as_ptr().add(idx);
                out.push(core::ptr::read_volatile(ptr));
            }
        }
        // Advance the watermark; overwritten events are counted as lost above.
        self.snapshot_head.store(head, Ordering::Relaxed);

        lost
    }

    fn reset(&self) {
        self.head.store(0, Ordering::Relaxed);
        self.snapshot_head.store(0, Ordering::Relaxed);
    }
}

/// One ring buffer per CPU, stored in the `.cpu_local` section.
///
/// `RingBuffer` is `Sync` (only atomic interior mutation), so remote-CPU
/// access via [`CpuLocal::get_on_cpu`] is allowed for snapshotting.
cpu_local! {
    static TRACE_RING: RingBuffer = RingBuffer::new();
}

/// Cumulative count for quick inspection without snapshotting.
static TRACE_EVENT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Events lost to ring overwrite since boot (or last reset).
static TRACE_LOST_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Push a trace event into the current CPU's ring buffer (called from the
/// dyndbg macro hot path when the descriptor is in Trace mode).
///
/// # Lock-freedom
///
/// Uses `fetch_add` for slot allocation + `write_volatile` for the store.
/// No locks, no CAS loops, no cross-CPU contention — each CPU owns its ring.
/// IRQs are temporarily disabled only to obtain the CPU-local reference.
#[inline(always)]
pub fn push_trace_event(descriptor_id: u64) {
    let event = TraceEvent {
        descriptor_id,
        tsc: ostd::arch::read_tsc(),
        cpu: u32::from(CpuId::current_racy()),
    };
    let irq_guard = ostd::irq::disable_local();
    TRACE_RING.get_with(&irq_guard).push(event);
    TRACE_EVENT_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Return a snapshot of all events produced since the previous snapshot,
/// merged across all CPUs and sorted by TSC (global time order).
///
/// Consuming is destructive: each event is returned exactly once across
/// successive calls.  Returns `(events, lost_this_read)` where the second
/// element is the number of events lost to ring overwrite *during this
/// snapshot interval*; it is also accumulated into the global [`lost_count`].
pub fn snapshot_events() -> (alloc::vec::Vec<TraceEvent>, u64) {
    let mut events = alloc::vec::Vec::new();
    let mut lost_this_read = 0u64;
    for raw_cpu in 0..ostd::cpu::num_cpus() {
        let ring = TRACE_RING.get_on_cpu(CpuId::new(raw_cpu as u32));
        let lost = ring.drain_since(&mut events);
        if lost > 0 {
            lost_this_read += lost as u64;
            TRACE_LOST_EVENTS.fetch_add(lost as u64, Ordering::Relaxed);
        }
    }
    events.sort_by_key(|e| e.tsc);
    (events, lost_this_read)
}

/// Number of events recorded since boot (or last reset).
pub fn event_count() -> u64 {
    TRACE_EVENT_COUNT.load(Ordering::Relaxed)
}

/// Number of events lost to ring overwrite since boot (or last reset).
pub fn lost_count() -> u64 {
    TRACE_LOST_EVENTS.load(Ordering::Relaxed)
}

/// Reset the event counter (does not clear the ring buffers).
pub fn reset_event_count() {
    TRACE_EVENT_COUNT.store(0, Ordering::Relaxed);
}

/// Reset all ring buffers, the event counter and the lost counter.
pub fn reset() {
    for raw_cpu in 0..ostd::cpu::num_cpus() {
        let ring = TRACE_RING.get_on_cpu(CpuId::new(raw_cpu as u32));
        ring.reset();
    }
    TRACE_EVENT_COUNT.store(0, Ordering::Relaxed);
    TRACE_LOST_EVENTS.store(0, Ordering::Relaxed);
}
