// SPDX-License-Identifier: MPL-2.0

//! Zero-overhead scheduler tracing via [`static_key_branch`].
//!
//! Demonstrates the StaticKey primitive (independent of dyndbg): when disabled,
//! the trace call site is a NOP5 instruction with no measurable overhead; when
//! enabled via [`enable`], the instruction is patched to a JMP that enters the
//! trace block.
//!
//! # Usage
//!
//! ```ignore
//! use crate::sched::sched_trace;
//!
//! // Enable at runtime:
//! sched_trace::enable();
//! // ... run workload ...
//! let count = sched_trace::trace_count();
//! sched_trace::disable();
//! ```

use core::sync::atomic::{AtomicU64, Ordering};

use ostd::arch::static_key::{self, StaticKeySite};

/// Number of times the trace call site has been entered since boot.
static TRACE_COUNT: AtomicU64 = AtomicU64::new(0);

/// Get the current trace counter value.
pub fn trace_count() -> u64 {
    TRACE_COUNT.load(Ordering::Relaxed)
}

/// Reset the trace counter to zero.
pub fn reset_trace_count() {
    TRACE_COUNT.store(0, Ordering::Relaxed);
}

// ── Site management ────────────────────────────────────────────────────

/// Enable scheduler tracing (NOP5 → JMP, one SMP transaction).
pub fn enable() {
    let sites = static_key::find_sites_by_tag("sched_trace");
    if !sites.is_empty() {
        static_key::enable_static_keys(&sites);
    }
}

/// Disable scheduler tracing (JMP → NOP5, one SMP transaction).
pub fn disable() {
    let sites = static_key::find_sites_by_tag("sched_trace");
    if !sites.is_empty() {
        static_key::disable_static_keys(&sites);
    }
}

/// Return the [`StaticKeySite`] reference if it has been initialised.
pub fn site() -> Option<&'static StaticKeySite> {
    static_key::find_sites_by_tag("sched_trace").into_iter().next()
}

// ── Hot-path call site ─────────────────────────────────────────────────

/// Called from the scheduler hot path (e.g. `pick_next_entity`).
///
/// **Cost when disabled:** single NOP5 instruction on x86_64 — statistically
/// indistinguishable from no instrumentation (TOST equivalence, δ < 0.05 ns/call).
#[inline(always)]
pub fn trace_pick_next() {
    ostd::static_key_branch!(SCHED_TRACE => {
        TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
    });
}
