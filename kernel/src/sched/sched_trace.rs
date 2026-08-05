// SPDX-License-Identifier: MPL-2.0

//! Zero-overhead scheduler tracing via [`static_key_branch`].
//!
//! Demonstrates the StaticKey primitive (independent of dyndbg): when disabled,
//! the trace call site is a single NOP5 instruction; when enabled at runtime,
//! the NOP5 is patched to a JMP that enters a lock-free per-CPU ring buffer
//! recording context-switch events.
//!
//! # Usage
//!
//! ```ignore
//! use crate::sched::sched_trace;
//!
//! sched_trace::enable();
//! // ... run workload ...
//! let (events, _lost) = sched_trace::snapshot_events();
//! for event in events {
//!     log::info!("cpu={} ts={} task=0x{:x}", event.cpu, event.tsc, event.task_ptr);
//! }
//! sched_trace::disable();
//! ```

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ostd::arch::static_key::{self, StaticKeySite};
use ostd::cpu::CpuId;
use ostd::cpu_local;

/// Maximum number of buffered events per CPU (power of 2 for mask-based wrap).
const RING_CAPACITY: usize = 256;
const RING_MASK: usize = RING_CAPACITY - 1;

/// A single context-switch event recorded by the trace hook.
#[derive(Debug, Clone, Copy)]
pub struct SchedTraceEvent {
    /// CPU where the event occurred.
    pub cpu: u32,
    /// TSC timestamp at capture time.
    pub tsc: u64,
    /// Address of the `Arc<Task>` that was picked (useful as a unique task id).
    pub task_ptr: usize,
}

struct RingBuffer {
    events: [SchedTraceEvent; RING_CAPACITY],
    /// Monotonically-increasing write index (modulo RING_CAPACITY).
    head: AtomicUsize,
    /// Snapshot watermark: events with index < `snapshot_head` have already
    /// been consumed by the previous snapshot.
    snapshot_head: AtomicUsize,
}

impl RingBuffer {
    const fn new() -> Self {
        const EMPTY: SchedTraceEvent = SchedTraceEvent {
            cpu: 0,
            tsc: 0,
            task_ptr: 0,
        };
        Self {
            events: [EMPTY; RING_CAPACITY],
            head: AtomicUsize::new(0),
            snapshot_head: AtomicUsize::new(0),
        }
    }

    fn push(&self, event: SchedTraceEvent) {
        let idx = self.head.fetch_add(1, Ordering::Relaxed) & RING_MASK;
        #[allow(unsafe_code)]
        // SAFETY: idx < RING_CAPACITY due to mask; this CPU is the only
        // producer of its own ring (accessed via `get_with`), so no
        // write-write races.
        unsafe {
            let ptr = self.events.as_ptr().add(idx) as *mut SchedTraceEvent;
            core::ptr::write_volatile(ptr, event);
        }
    }

    /// Drain events produced since the previous snapshot into `out`.
    ///
    /// Returns the number of events lost to ring overwrite since the previous
    /// snapshot.
    fn drain_since(&self, out: &mut alloc::vec::Vec<SchedTraceEvent>) -> usize {
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
            // this slot.
            unsafe {
                let ptr = self.events.as_ptr().add(idx);
                out.push(core::ptr::read_volatile(ptr));
            }
        }
        self.snapshot_head.store(head, Ordering::Relaxed);

        lost
    }

    fn reset(&self) {
        self.head.store(0, Ordering::Relaxed);
        self.snapshot_head.store(0, Ordering::Relaxed);
    }
}

cpu_local! {
    static RING: RingBuffer = RingBuffer::new();
}

/// Cumulative count for quick inspection without draining.
static EVENT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Events lost to ring overwrite since boot (or last reset).
static LOST_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Number of events recorded since boot (or last reset).
pub fn event_count() -> u64 {
    EVENT_COUNT.load(Ordering::Relaxed)
}

/// Number of events lost to ring overwrite since boot (or last reset).
pub fn lost_count() -> u64 {
    LOST_EVENTS.load(Ordering::Relaxed)
}

/// Reset the event counter.
pub fn reset_event_count() {
    EVENT_COUNT.store(0, Ordering::Relaxed);
}

/// Drain all events produced since the previous drain, merged across all CPUs
/// and sorted by TSC (global time order).
///
/// Consuming is destructive: each event is returned exactly once across
/// successive calls.  Returns `(events, lost_this_read)`; the second element
/// is also accumulated into the global [`lost_count`].
pub fn snapshot_events() -> (alloc::vec::Vec<SchedTraceEvent>, u64) {
    let mut events = alloc::vec::Vec::new();
    let mut lost_this_read = 0u64;
    for raw_cpu in 0..ostd::cpu::num_cpus() {
        let ring = RING.get_on_cpu(CpuId::new(raw_cpu as u32));
        let lost = ring.drain_since(&mut events);
        if lost > 0 {
            lost_this_read += lost as u64;
            LOST_EVENTS.fetch_add(lost as u64, Ordering::Relaxed);
        }
    }
    events.sort_by_key(|e| e.tsc);
    (events, lost_this_read)
}

// ── Site management ────────────────────────────────────────────────────

/// Enable scheduler tracing (NOP5 → JMP, one SMP transaction).
pub fn enable() {
    let sites = static_key::find_sites_by_tag("SCHED_TRACE");
    if !sites.is_empty() {
        static_key::enable_static_keys(&sites);
    }
}

/// Disable scheduler tracing (JMP → NOP5, one SMP transaction).
pub fn disable() {
    let sites = static_key::find_sites_by_tag("SCHED_TRACE");
    if !sites.is_empty() {
        static_key::disable_static_keys(&sites);
    }
}

/// Return the [`StaticKeySite`] reference if it has been initialised.
pub fn site() -> Option<&'static StaticKeySite> {
    static_key::find_sites_by_tag("SCHED_TRACE").into_iter().next()
}

// ── Hot-path call site ─────────────────────────────────────────────────

/// Called from [`super::sched_class::PerCpuClassRqSet::pick_next_entity`] on
/// every context switch.
///
/// **Cost when disabled:** single NOP5 instruction — statistically
/// indistinguishable from no instrumentation.
///
/// **Cost when enabled:** one TSC read + lock-free per-CPU ring-buffer push.
#[inline(always)]
pub fn trace_pick_next(task_ptr: usize) {
    ostd::static_key_branch!(SCHED_TRACE => {
        let event = SchedTraceEvent {
            cpu: u32::from(CpuId::current_racy()),
            tsc: ostd::arch::read_tsc(),
            task_ptr,
        };
        let irq_guard = ostd::irq::disable_local();
        RING.get_with(&irq_guard).push(event);
        EVENT_COUNT.fetch_add(1, Ordering::Relaxed);
    });
}
