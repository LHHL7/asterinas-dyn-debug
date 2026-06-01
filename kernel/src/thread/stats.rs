// SPDX-License-Identifier: MPL-2.0

use aster_util::per_cpu_counter::PerCpuCounter;
use spin::Once;

pub(super) static CONTEXT_SWITCH_COUNTER: Once<PerCpuCounter> = Once::new();
pub(super) static PAGE_FAULT_COUNTER: Once<PerCpuCounter> = Once::new();

/// Counts the number of context switches ever happened across all CPUs.
pub fn collect_context_switch_count() -> usize {
    CONTEXT_SWITCH_COUNTER.get().unwrap().sum_all_cpus()
}

/// Counts the number of resolved user page faults across all CPUs.
pub fn collect_page_fault_count() -> usize {
    PAGE_FAULT_COUNTER.get().unwrap().sum_all_cpus()
}
