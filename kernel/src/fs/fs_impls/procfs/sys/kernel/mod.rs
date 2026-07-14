// SPDX-License-Identifier: MPL-2.0

use aster_util::slot_vec::SlotVec;
use ostd::sync::RwMutexUpgradeableGuard;

use crate::{
    fs::{
        file::mkmod,
        procfs::{
            ProcDir,
            sys::kernel::{
                cap_last_cap::CapLastCapFileOps, dynamic_debug::DynamicDebugFileOps,
                dyndbg_bench::DyndbgBenchFileOps, dyndbg_stats::DyndbgStatsFileOps,
                pid_max::PidMaxFileOps, tsc::TscFileOps, yama::YamaDirOps,
            },
            template::{
                DirOps, ProcDirBuilder, lookup_child_from_table, populate_children_from_table,
            },
        },
        vfs::inode::Inode,
    },
    prelude::*,
};

mod cap_last_cap;
mod dynamic_debug;
mod dyndbg_bench;
mod dyndbg_stats;
mod pid_max;
mod tsc;
mod yama;

/// Represents the inode at `/proc/sys/kernel`.
pub struct KernelDirOps;

impl KernelDirOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        // Reference:
        // <https://elixir.bootlin.com/linux/v6.16.5/source/kernel/sysctl.c#L1765>
        // <https://elixir.bootlin.com/linux/v6.16.5/source/fs/proc/proc_sysctl.c#L978>
        ProcDirBuilder::new(Self, mkmod!(a+rx))
            .parent(parent)
            .build()
            .unwrap()
    }
    #[expect(clippy::type_complexity)]
    const STATIC_ENTRIES: &'static [(&'static str, fn(Weak<dyn Inode>) -> Arc<dyn Inode>)] = &[
        ("cap_last_cap", CapLastCapFileOps::new_inode),
        ("dynamic_debug", DynamicDebugFileOps::new_inode),
        ("dyndbg_bench", DyndbgBenchFileOps::new_inode),
        ("dyndbg_stats", DyndbgStatsFileOps::new_inode),
        ("pid_max", PidMaxFileOps::new_inode),
        ("tsc", TscFileOps::new_inode),
        ("yama", YamaDirOps::new_inode),
    ];
}

impl DirOps for KernelDirOps {
    fn lookup_child(&self, dir: &ProcDir<Self>, name: &str) -> Result<Arc<dyn Inode>> {
        let mut cached_children = dir.cached_children().write();
        //从静态表中查找子节点 并调用对应的构造函数创建inode
        if let Some(child) =
            lookup_child_from_table(name, &mut cached_children, Self::STATIC_ENTRIES, |f| {
                (f)(dir.this_weak().clone())
            })
        {
            return Ok(child);
        }

        return_errno_with_message!(Errno::ENOENT, "the file does not exist");
    }

    fn populate_children<'a>(
        &self,
        dir: &'a ProcDir<Self>,
    ) -> RwMutexUpgradeableGuard<'a, SlotVec<(String, Arc<dyn Inode>)>> {
        let mut cached_children = dir.cached_children().write();

        populate_children_from_table(&mut cached_children, Self::STATIC_ENTRIES, |f| {
            (f)(dir.this_weak().clone())
        });

        cached_children.downgrade()
    }
}
