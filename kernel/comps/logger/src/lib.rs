// SPDX-License-Identifier: MPL-2.0

//! The logger implementation for Asterinas.
//!
//! This logger now has the most basic logging functionality, controls the output
//! based on the globally set log level. Different log levels will be represented
//! with different colors if enabling `log_color` feature.
//!
//! This logger guarantees _atomicity_ under concurrency: messages are always
//! printed in their entirety without being mixed with messages generated
//! concurrently on other cores.
//!
//! IRQs are disabled while printing. So do not print long log messages.
#![no_std]
#![deny(unsafe_code)]

extern crate alloc;

use component::{init_component, ComponentInitError};

mod aster_logger;
mod console;
#[allow(unsafe_code)]
pub mod dyndbg_trace;
/// Kernel log ring backing `/dev/kmsg` (the dmesg channel).
pub mod kmsg;
//对外暴露接口 方便控制接口调用
pub use aster_logger::{
    append_dyndbg_rule, clear_dyndbg_rule, clear_dyndbg_rules, dyndbg_module_enabled,
    dyndbg_should_log, dyndbg_should_trace, format_dyndbg_log,
    __dyndbg_label_emit,
    __dyndbg_label_emit_const,
    get_dyndbg_index_enabled, get_dyndbg_recompute_enabled, get_dyndbg_patch_backend,
    get_dyndbg_rule_chain_snapshot, get_dyndbg_rule_snapshot,
    remove_dyndbg_rule_by_id, get_dyndbg_stats_snapshot, reset_dyndbg_stats,
    set_dyndbg_index_enabled, set_dyndbg_recompute_enabled,
    set_dyndbg_patch_backend,
    DebugDescriptor, DYNDBG_DESCRIPTOR_REGISTRY, DYNDBG_KEY_MAPPING,
    DyndbgKeyMapping, DyndbgPatchBackend, DyndbgStatsSnapshot,
    DyndbgRuleActionSnapshot,
    DyndbgRuleEntrySnapshot,
    DyndbgRuleSnapshot,
    FLAG_FUNCTION, FLAG_LINE, FLAG_MODULE, FLAG_THREAD,
};
#[doc(hidden)]
pub use linkme::distributed_slice;
pub use console::_print;

#[init_component]
fn init() -> Result<(), ComponentInitError> {
    aster_logger::init();
    Ok(())
}
