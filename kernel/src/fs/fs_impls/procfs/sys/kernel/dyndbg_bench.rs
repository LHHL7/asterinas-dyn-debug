// SPDX-License-Identifier: MPL-2.0

use core::sync::atomic::{AtomicU64, Ordering};

use aster_util::printer::VmPrinter;
use ostd::{sync::SpinLock, timer::Jiffies};

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
    Count,
}

impl BenchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Count => "count",
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
            last_mode: BenchMode::Count,
            last_iters: 0,
            last_duration_us: 0,
        }
    }
}

/// Represents the inode at `/proc/sys/kernel/dyndbg_bench`.
pub struct DyndbgBenchFileOps;

impl DyndbgBenchFileOps {
    pub fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        //创建虚拟文件 挂载到parent目录下
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
        writeln!(printer, "counter={}", counter)?;
        writeln!(
            printer,
            "usage: mode=log|count iters=<n> (e.g., mode=log iters=1000000)"
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
    let mut mode = None;
    let mut iters = None;

    for token in command.split_ascii_whitespace() {
        if let Some(value) = token.strip_prefix("mode=") {
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

    let mode = mode.ok_or_else(|| Error::with_message(Errno::EINVAL, "missing mode"))?;
    let iters = iters.ok_or_else(|| Error::with_message(Errno::EINVAL, "missing iters"))?;
    //真正执行基准测试
    execute_bench(mode, iters)
}

// 解析模式参数，支持log和count两种模式
fn parse_mode(value: &str) -> Result<BenchMode> {
    match value {
        "log" => Ok(BenchMode::Log),
        "count" => Ok(BenchMode::Count),
        _ => return_errno_with_message!(Errno::EINVAL, "mode must be log or count"),
    }
}

fn execute_bench(mode: BenchMode, iters: u64) -> Result<()> {
    let start = Jiffies::elapsed().as_duration();

    match mode {
        BenchMode::Log => {
            for _ in 0..iters {
                bench_log();
            }
        }
        BenchMode::Count => {
            for _ in 0..iters {
                BENCH_COUNTER.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    let end = Jiffies::elapsed().as_duration();
    let elapsed = end.checked_sub(start).unwrap_or_default();
    let elapsed_us = elapsed.as_micros() as u64;

    let mut state = BENCH_STATE.lock();
    state.last_mode = mode;
    state.last_iters = iters;
    state.last_duration_us = elapsed_us;

    Ok(())
}

fn bench_log() {
    aster_logger::dyndbg_debug!("dyndbg bench log");
}
