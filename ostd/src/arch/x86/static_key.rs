// SPDX-License-Identifier: MPL-2.0

//! Generic static-key primitive for zero-overhead runtime branching.
//!
//! A static key lets kernel code guard a path with a compile-time-embedded
//! NOP5/JMP slot: when the key is *disabled* the CPU executes a 5-byte NOP;
//! when *enabled* the NOP is patched to a `JMP rel32` that jumps into the
//! guarded block.  The patch runs under the SMP-safe PatchRendezvous protocol
//! provided by [`super::static_patch`].
//!
//! # Architecture support
//!
//! | Arch     | Disabled path            | Enabled path        |
//! |----------|--------------------------|---------------------|
//! | x86_64   | NOP5 (5-byte multi-nop)  | JMP rel32           |
//! | riscv64  | AtomicBool load+branch   | (same, no patching) |
//! | loong64  | AtomicBool load+branch   | (same, no patching) |
//!
//! # Example
//!
//! ```ignore
//! use ostd::static_key_branch;
//!
//! fn hot_path() {
//!     static_key_branch!(MY_FEATURE => {
//!         do_expensive_work();
//!     });
//! }
//!
//! // Enable later:
//! ostd::arch::static_key::enable_static_keys(&[...]);
//! ```

use core::sync::atomic::{AtomicBool, Ordering};

use linkme::distributed_slice;
use super::static_patch::{self, PatchInstruction, PatchRequest};

/// A compile-time-registered static branch site.
///
/// Created by the [`static_key_branch!`] macro.  Each instance owns:
/// - the address of the 5-byte instruction slot (NOP5 when disabled),
/// - the address of the "enabled" code block (JMP target),
/// - a runtime flag tracking the current state.
#[derive(Debug)]
pub struct StaticKeySite {
    /// Address of the 5-byte patch slot in `.text` (stored as a function
    /// pointer to avoid const-eval pointer-to-int restrictions).
    instruction_site: unsafe extern "C" fn() -> bool,
    /// Address to jump to when enabled.
    jump_target: unsafe extern "C" fn() -> bool,
    /// Whether the key is currently enabled.
    enabled: AtomicBool,
    /// Human-readable tag for grouping / finding sites (e.g. "dyndbg", "sched_trace").
    pub tag: &'static str,
}

impl StaticKeySite {
    /// Create a static-key site from the two function pointers provided by the
    /// macro.  The pointer-to-`usize` conversion is deferred to patch time so
    /// that the `static` initialiser stays valid in const context.
    #[doc(hidden)]
    pub const fn new(
        instruction_site: unsafe extern "C" fn() -> bool,
        jump_target: unsafe extern "C" fn() -> bool,
        tag: &'static str,
    ) -> Self {
        Self {
            instruction_site,
            jump_target,
            enabled: AtomicBool::new(false),
            tag,
        }
    }

    /// Query the current enabled state (non-hot-path; the hot path uses the
    /// patched instruction slot directly on x86_64).
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

/// Find all registered [`StaticKeySite`]s with the given tag.
pub fn find_sites_by_tag(tag: &str) -> alloc::vec::Vec<&'static StaticKeySite> {
    STATIC_KEY_SITE_REGISTRY
        .iter()
        .filter(|s| s.tag == tag)
        .copied()
        .collect()
}

/// Distributed slice collecting every [`StaticKeySite`] across all crates.
///
/// Populated at link time by the `static_key_branch!` macro.  The slice is
/// read-only after boot; all mutation goes through [`enable_static_keys`] and
/// [`disable_static_keys`].
#[distributed_slice]
pub static STATIC_KEY_SITE_REGISTRY: [&'static StaticKeySite];

/// Enable a batch of static keys in one SMP-safe transaction.
///
/// Each site whose `enabled` flag is currently `false` is patched from NOP5
/// to `JMP rel32`.  Sites that are already enabled are silently skipped.
/// Errors are logged but not propagated so callers get a uniform API across
/// architectures.
pub fn enable_static_keys(sites: &[&'static StaticKeySite]) {
    let requests: alloc::vec::Vec<PatchRequest> = sites
        .iter()
        .filter(|s| !s.enabled.swap(true, Ordering::AcqRel))
        .map(|s| PatchRequest {
            instruction_address: s.instruction_site as usize,
            instruction: PatchInstruction::JmpRel32 {
                target: s.jump_target as usize,
            },
        })
        .collect();
    if requests.is_empty() {
        return;
    }
    if let Err(e) = static_patch::patch_5byte_slots(&requests) {
        log::warn!("[static_key] enable failed: {:?}", e);
    }
}

/// Disable a batch of static keys in one SMP-safe transaction.
///
/// Each site whose `enabled` flag is currently `true` is patched back from
/// `JMP rel32` to NOP5.  Sites that are already disabled are silently skipped.
/// Errors are logged but not propagated.
pub fn disable_static_keys(sites: &[&'static StaticKeySite]) {
    let requests: alloc::vec::Vec<PatchRequest> = sites
        .iter()
        .filter(|s| s.enabled.swap(false, Ordering::AcqRel))
        .map(|s| PatchRequest {
            instruction_address: s.instruction_site as usize,
            instruction: PatchInstruction::Nop5,
        })
        .collect();
    if requests.is_empty() {
        return;
    }
    if let Err(e) = static_patch::patch_5byte_slots(&requests) {
        log::warn!("[static_key] disable failed: {:?}", e);
    }
}

/// Called once at boot to log the static-key registry size.
///
/// The sites are fully initialised at compile time via `#[distributed_slice]`;
/// this function is a lightweight boot-time observable.  A future `build.rs`
/// integrity check can compare the observed count against a compile-time
/// expected value to detect linkme mis-aggregation.
pub fn init_static_keys() {
    log::info!(
        "[static_key] {} site(s) registered",
        STATIC_KEY_SITE_REGISTRY.len()
    );
}

/// Emit a zero-overhead static branch at the call site.
///
/// On x86_64 this expands to a 5-byte NOP slot (disabled path) whose address
/// is registered so that [`enable_static_keys`]/[`disable_static_keys`] can
/// later patch it to a `JMP rel32`.  On non-x86 architectures the macro falls
/// back to an `AtomicBool` load + conditional branch.
///
/// # Example
///
/// ```ignore
/// static_key_branch!(VERBOSE_TRACE => {
///     log::trace!("expensive trace data: {:?}", data);
/// });
/// ```
/// Implementation of [`static_key_branch`].  See the re-export in `ostd::lib`.
#[doc(hidden)]
#[macro_export]
macro_rules! _static_key_branch_impl {
    ($key:ident => $enabled_block:block) => {
        // ── x86_64: NOP5 slot + runtime patching ──────────────────────
        #[cfg(target_arch = "x86_64")]
        {
            #[allow(unsafe_code)]
            // SAFETY: The inline asm emits a 5-byte patch slot and a label
            // for the enabled block.  The `.if 0` / `jmp` construct is
            // seen by the assembler for rel32-range validation but never
            // emitted into the binary.  The label block is normal Rust.
            unsafe {
                core::arch::asm!(
                    concat!(
                        ".globl \"__static_key_site_",
                        stringify!($key),
                        "_",
                        file!(),
                        "_",
                        line!(),
                        "_",
                        column!(),
                        "\"\n",
                        "\"__static_key_site_",
                        stringify!($key),
                        "_",
                        file!(),
                        "_",
                        line!(),
                        "_",
                        column!(),
                        "\":\n",
                        ".byte 0x0f, 0x1f, 0x44, 0x00, 0x00\n",
                        ".if 0\n",
                        "jmp {0}\n",
                        ".endif\n",
                    ),
                    label {
                        #[allow(unsafe_code)]
                        // SAFETY: This inner asm only defines a global
                        // symbol at the entry of the enabled block for
                        // use as a JMP target by static patching.
                        unsafe {
                            core::arch::asm!(
                                concat!(
                                    ".globl \"__static_key_target_",
                                    stringify!($key),
                                    "_",
                                    file!(),
                                    "_",
                                    line!(),
                                    "_",
                                    column!(),
                                    "\"\n",
                                    "\"__static_key_target_",
                                    stringify!($key),
                                    "_",
                                    file!(),
                                    "_",
                                    line!(),
                                    "_",
                                    column!(),
                                    "\":\n",
                                ),
                                options(nomem, nostack)
                            );
                        }
                        $enabled_block
                    },
                    options(nomem, nostack, preserves_flags)
                );
            }

            // Declare extern symbols for the labels emitted above so we
            // can take their addresses for the StaticKeySite registration.
            // SAFETY: the asm block guarantees these symbols exist.
            unsafe extern "C" {
                #[link_name = concat!(
                    "__static_key_site_",
                    stringify!($key),
                    "_",
                    file!(),
                    "_",
                    line!(),
                    "_",
                    column!()
                )]
                fn __static_key_site() -> bool;
                #[link_name = concat!(
                    "__static_key_target_",
                    stringify!($key),
                    "_",
                    file!(),
                    "_",
                    line!(),
                    "_",
                    column!()
                )]
                fn __static_key_target() -> bool;
            }

            static STATIC_KEY_SITE: $crate::arch::static_key::StaticKeySite =
                $crate::arch::static_key::StaticKeySite::new(
                    __static_key_site,
                    __static_key_target,
                    stringify!($key),
                );
            #[$crate::distributed_slice($crate::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
            static STATIC_KEY_SITE_ENTRY: &$crate::arch::static_key::StaticKeySite =
                &STATIC_KEY_SITE;
        }

        // ── Non-x86 fallback: AtomicBool + branch ─────────────────────
        #[cfg(not(target_arch = "x86_64"))]
        {
            static STATIC_KEY_SITE: $crate::arch::static_key::StaticKeySite =
                $crate::arch::static_key::StaticKeySite::new_fallback(
                    stringify!($key),
                );
            #[$crate::distributed_slice($crate::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
            static STATIC_KEY_SITE_ENTRY: &$crate::arch::static_key::StaticKeySite =
                &STATIC_KEY_SITE;
            if STATIC_KEY_SITE.is_enabled() {
                $enabled_block
            }
        }
    };
}
