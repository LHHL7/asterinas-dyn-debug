// SPDX-License-Identifier: MPL-2.0

//! Static-key fallback for RISC-V.
//!
//! RISC-V does not yet have an instruction-patching backend, so every
//! [`StaticKeySite`] degrades to an `AtomicBool` guard.  The
//! `static_key_branch!` macro emits a normal conditional branch instead of
//! a NOP5 slot.

use core::sync::atomic::{AtomicBool, Ordering};

use linkme::distributed_slice;

/// A compile-time-registered static branch site (software-only fallback).
#[derive(Debug)]
pub struct StaticKeySite {
    enabled: AtomicBool,
    /// Human-readable tag for grouping / finding sites.
    pub tag: &'static str,
}

impl StaticKeySite {
    /// Create a fallback site (no instruction addresses needed).
    #[doc(hidden)]
    pub const fn new_fallback(tag: &'static str) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            tag,
        }
    }

    /// Query current enabled state.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

/// Find all registered sites with the given tag.
pub fn find_sites_by_tag(tag: &str) -> alloc::vec::Vec<&'static StaticKeySite> {
    STATIC_KEY_SITE_REGISTRY
        .iter()
        .filter(|s| s.tag == tag)
        .copied()
        .collect()
}

/// Distributed slice (empty on this architecture unless software-mode sites
/// are registered).
#[distributed_slice]
pub static STATIC_KEY_SITE_REGISTRY: [&'static StaticKeySite];

/// Enable a batch of static keys — toggles the software flag only.
pub fn enable_static_keys(sites: &[&'static StaticKeySite]) {
    for s in sites {
        s.enabled.store(true, Ordering::Release);
    }
}

/// Disable a batch of static keys — toggles the software flag only.
pub fn disable_static_keys(sites: &[&'static StaticKeySite]) {
    for s in sites {
        s.enabled.store(false, Ordering::Release);
    }
}

/// Software-only fallback (no instruction patching on RISC-V).
#[doc(hidden)]
#[macro_export]
macro_rules! _static_key_branch_impl {
    ($key:ident => $enabled_block:block) => {
        static STATIC_KEY_SITE: $crate::arch::static_key::StaticKeySite =
            $crate::arch::static_key::StaticKeySite::new_fallback(stringify!($key));
        #[$crate::distributed_slice($crate::arch::static_key::STATIC_KEY_SITE_REGISTRY)]
        static STATIC_KEY_SITE_ENTRY: &$crate::arch::static_key::StaticKeySite =
            &STATIC_KEY_SITE;
        if STATIC_KEY_SITE.is_enabled() {
            $enabled_block
        }
    };
}

/// Boot-time observable: logs the number of registered sites.
pub fn init_static_keys() {
    log::info!(
        "[static_key] {} site(s) registered (software fallback)",
        STATIC_KEY_SITE_REGISTRY.len()
    );
}
