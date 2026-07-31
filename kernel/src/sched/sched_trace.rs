// SPDX-License-Identifier: MPL-2.0

//! Zero-overhead scheduler tracing via [`static_key_branch`].
//!
//! Demonstrates the StaticKey primitive (independent of dyndbg): when disabled,
//! the trace call site is a single NOP5 instruction; when enabled at runtime,
//! the NOP5 is patched to a JMP that enters a lock-free ring buffer recording
//! context-switch events.
//!
//! # Usage
//!
//! ```ignore
//! use crate::sched::sched_trace;
//!
//! sched_trace::enable();
//! // ... run workload ...
//! for event in sched_trace::snapshot_events() {
//!     log::info!("cpu={} ts={} task=0x{:x}", event.cpu, event.tsc, event.task_ptr);
//! }
//! sched_trace::disable();
//! ```

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ostd::arch::static_key::{self, StaticKeySite};

/// Maximum number of buffered events (power of 2 for mask-based wrap).
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
        }
    }

    fn push(&self, event: SchedTraceEvent) {
        let idx = self.head.fetch_add(1, Ordering::Relaxed) & RING_MASK;
        #[allow(unsafe_code)]
        // SAFETY: idx < RING_CAPACITY due to mask; fetch_add gives each
        // producer a unique slot so no write-write races.
        unsafe {
            let ptr = self.events.as_ptr().add(idx) as *mut SchedTraceEvent;
            core::ptr::write_volatile(ptr, event);
        }
    }

    fn drain(&self) -> alloc::vec::Vec<SchedTraceEvent> {
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
static RING: RingBuffer = RingBuffer::new();

/// Cumulative count for quick inspection without draining.
static EVENT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Number of events recorded since boot (or last reset).
pub fn event_count() -> u64 {
    EVENT_COUNT.load(Ordering::Relaxed)
}

/// Reset the event counter.
pub fn reset_event_count() {
    EVENT_COUNT.store(0, Ordering::Relaxed);
}

/// Drain all buffered events (oldest first).
pub fn snapshot_events() -> alloc::vec::Vec<SchedTraceEvent> {
    RING.drain()
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
/// **Cost when enabled:** one TSC read + lock-free ring-buffer push.
#[inline(always)]
pub fn trace_pick_next(task_ptr: usize) {
    ostd::static_key_branch!(SCHED_TRACE => {
        let event = SchedTraceEvent {
            cpu: u32::from(ostd::cpu::CpuId::current_racy()),
            tsc: ostd::arch::read_tsc(),
            task_ptr,
        };
        RING.push(event);
        EVENT_COUNT.fetch_add(1, Ordering::Relaxed);
    });
}
