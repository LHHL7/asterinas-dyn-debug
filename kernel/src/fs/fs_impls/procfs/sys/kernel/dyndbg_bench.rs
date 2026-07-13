// SPDX-License-Identifier: MPL-2.0

use core::sync::atomic::{AtomicU64, Ordering};

use aster_util::printer::VmPrinter;
use ostd::sync::SpinLock;

use crate::{
    fs::{
        file::mkmod,
        procfs::template::{FileOps, ProcFileBuilder},
        vfs::inode::Inode,
    },
    prelude::*,
};

const MAX_CMD_LEN: usize = 128;

static BENCH_COUNTER: AtomicU64 = AtomicU64::new(0);
static BENCH_STATE: SpinLock<BenchState> = SpinLock::new(BenchState::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchMode {
    Log,
    LogBatch,
}

impl BenchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::LogBatch => "log_batch",
        }
    }
}

//最后一次测试结果
struct BenchState {
    last_mode: BenchMode,
    //循环次数
    last_iters: u64,
    //耗时（微秒）
    last_duration_us: u64,
}

impl BenchState {
    const fn new() -> Self {
        Self {
            last_mode: BenchMode::Log,
            last_iters: 0,
            last_duration_us: 0,
        }
    }
}

/// Represents the inode at `/proc/sys/kernel/dyndbg_bench`.
pub struct DyndbgBenchFileOps;

impl DyndbgBenchFileOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        ProcFileBuilder::new(Self, mkmod!(a+r, u+w))
            .parent(parent)
            .build()
            .unwrap()
    }
}

//实现FileOps接口，支持读写 对应cat和echo命令
impl FileOps for DyndbgBenchFileOps {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        let mut printer = VmPrinter::new_skip(writer, offset);
        let state = BENCH_STATE.lock();
        let counter = BENCH_COUNTER.load(Ordering::Relaxed);

        writeln!(printer, "last_mode={}", state.last_mode.as_str())?;
        writeln!(printer, "last_iters={}", state.last_iters)?;
        writeln!(printer, "last_duration_us={}", state.last_duration_us)?;
        writeln!(printer, "backend={}", aster_logger::get_dyndbg_patch_backend().as_str())?;
        writeln!(printer, "index={}", if aster_logger::get_dyndbg_index_enabled() { "on" } else { "off" })?;
        writeln!(printer, "recompute={}", if aster_logger::get_dyndbg_recompute_enabled() { "incremental" } else { "full" })?;
        writeln!(printer, "counter={}", counter)?;
        writeln!(
            printer,
            "usage: backend=per_site|batch index=on|off recompute=incremental|full mode=log|log_batch iters=<n>"
        )?;

        Ok(printer.bytes_written())
    }

    fn write_at(&self, _offset: usize, reader: &mut VmReader) -> Result<usize> {
        let (command, read_bytes) = reader.read_cstring_until_end(MAX_CMD_LEN)?;
        let command = command
            .to_str()
            .map_err(|_| Error::with_message(Errno::EINVAL, "command is not valid UTF-8"))?
            .trim();

        if command.is_empty() {
            return_errno_with_message!(Errno::EINVAL, "empty command");
        }

        run_bench(command)?;
        Ok(read_bytes)
    }
}

fn run_bench(command: &str) -> Result<()> {
    // 检查命令格式，解析出mode和iters参数
    let mut backend = None;
    let mut index_enabled = None;
    let mut recompute_enabled = None;
    let mut mode = None;
    let mut iters = None;

    for token in command.split_ascii_whitespace() {
        if let Some(value) = token.strip_prefix("backend=") {
            backend = Some(parse_backend(value)?);
        } else if let Some(value) = token.strip_prefix("index=") {
            index_enabled = Some(parse_index(value)?);
        } else if let Some(value) = token.strip_prefix("recompute=") {
            recompute_enabled = Some(parse_recompute(value)?);
        } else if let Some(value) = token.strip_prefix("mode=") {
            mode = Some(parse_mode(value)?);
        } else if let Some(value) = token.strip_prefix("iters=") {
            let parsed = value
                .parse::<u64>()
                .map_err(|_| Error::with_message(Errno::EINVAL, "iters must be a valid u64"))?;
            if parsed == 0 {
                return_errno_with_message!(Errno::EINVAL, "iters must be greater than 0");
            }
            iters = Some(parsed);
        } else {
            return_errno_with_message!(Errno::EINVAL, "unknown token");
        }
    }

    if let Some(backend) = backend {
        aster_logger::set_dyndbg_patch_backend(backend);
    }

    if let Some(enabled) = index_enabled {
        aster_logger::set_dyndbg_index_enabled(enabled);
    }

    if let Some(enabled) = recompute_enabled {
        aster_logger::set_dyndbg_recompute_enabled(enabled);
    }

    if mode.is_none() && iters.is_none() {
        return Ok(());
    }

    let mode = mode.ok_or_else(|| Error::with_message(Errno::EINVAL, "missing mode"))?;
    let iters = iters.ok_or_else(|| Error::with_message(Errno::EINVAL, "missing iters"))?;
    //真正执行基准测试
    execute_bench(mode, iters)
}

fn parse_backend(value: &str) -> Result<aster_logger::DyndbgPatchBackend> {
    match value {
        "per_site" | "persite" => Ok(aster_logger::DyndbgPatchBackend::PerSite),
        "batch" => Ok(aster_logger::DyndbgPatchBackend::Batch),
        _ => return_errno_with_message!(Errno::EINVAL, "backend must be per_site or batch"),
    }
}

// 解析索引开关参数，支持on和off两种模式
fn parse_index(value: &str) -> Result<bool> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => return_errno_with_message!(Errno::EINVAL, "index must be on or off"),
    }
}

// 解析增量重算开关参数，支持incremental（增量）和full（全量）两种模式
fn parse_recompute(value: &str) -> Result<bool> {
    match value {
        "incremental" => Ok(true),
        "full" => Ok(false),
        _ => return_errno_with_message!(Errno::EINVAL, "recompute must be incremental or full"),
    }
}

// 解析模式参数，支持log和log_batch两种模式
fn parse_mode(value: &str) -> Result<BenchMode> {
    match value {
        "log" => Ok(BenchMode::Log),
        "log_batch" => Ok(BenchMode::LogBatch),
        _ => return_errno_with_message!(Errno::EINVAL, "mode must be log or log_batch"),
    }
}

fn execute_bench(mode: BenchMode, iters: u64) -> Result<()> {
    let tsc_start = ostd::arch::read_tsc();
    let tsc_freq = ostd::arch::tsc_freq();

    // Force 64-byte cache-line alignment so the benchmark loop sits at
    // the same alignment across all build configurations.
    #[cfg(target_arch = "x86_64")]
    {
        #[allow(unsafe_code)]
        // SAFETY: `.align 64` emits only alignment padding; no side effects.
        unsafe {
            core::arch::asm!(".align 64", options(nomem, nostack, preserves_flags));
        }
    }

    match mode {
        BenchMode::Log => {
            for _ in 0..iters {
                core::hint::black_box(bench_log());
            }
        }
        BenchMode::LogBatch => {
            for _ in 0..iters {
                core::hint::black_box(bench_log_batch());
            }
        }
    }

    let tsc_end = ostd::arch::read_tsc();
    let elapsed_us = if tsc_freq > 0 && tsc_end > tsc_start {
        ((tsc_end - tsc_start) * 1_000_000) / tsc_freq
    } else {
        0 // TSC unavailable, fallback to 0 (should not happen on supported archs)
    };

    let mut state = BENCH_STATE.lock();
    state.last_mode = mode;
    state.last_iters = iters;
    state.last_duration_us = elapsed_us;

    Ok(())
}

#[inline(never)]
fn bench_log() {
    aster_logger::dyndbg_debug!("dyndbg bench log");
    // Prevent LTO from eliminating this function when call sites are disabled.
    core::hint::black_box(());
}

// Move bench callsites to a dedicated module to keep this file small.
mod bench_sites;
use bench_sites::bench_log_batch;
