// SPDX-License-Identifier: MPL-2.0

//! Minimal x86_64 static patching primitives.
//!
//! This module intentionally keeps a very small surface: patch one fixed
//! 5-byte instruction slot as either `NOP5` or `JMP rel32`.

use core::{
    hint::spin_loop,
    ptr,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence},
};

use crate::{
    cpu::{CpuId, CpuSet, num_cpus},
    irq,
    sync::SpinLock,
};

/// Fixed slot size for mini static patching.
pub const PATCH_SLOT_SIZE: usize = 5;

/// The instruction to write into a 5-byte patch slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchInstruction {
    /// 5-byte NOP: `0F 1F 44 00 00`.
    Nop5,
    /// 5-byte near jump: `E9 <rel32>`.
    JmpRel32 {
        /// Absolute address of jump target.
        target: usize,
    },
}

/// A single patch request within a batch transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchRequest {
    /// Address of the 5-byte instruction slot to patch.
    pub instruction_address: usize,
    /// Instruction to write into the patch slot.
    pub instruction: PatchInstruction,
}

/// Errors returned by mini static patch operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchError {
    /// The site address is not usable.
    InvalidSiteAddress,
    /// The target is out of `rel32` range for `JMP`.
    Rel32OutOfRange,
    /// The local IRQs are disabled, so SMP rendezvous cannot proceed safely.
    IrqsDisabled,
    /// SMP is active but the IPI sender is not ready.
    SmpNotReady,
}

struct PatchRendezvous {
    arrived: AtomicUsize,
    done: AtomicUsize,
    release: AtomicBool,
}

impl PatchRendezvous {
    const fn new() -> Self {
        Self {
            arrived: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            release: AtomicBool::new(false),
        }
    }

    // 重置状态
    fn reset(&self) {
        self.arrived.store(0, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.release.store(false, Ordering::Relaxed);
    }
}

static PATCH_LOCK: SpinLock<()> = SpinLock::new(());
static PATCH_RENDEZVOUS: PatchRendezvous = PatchRendezvous::new();

// 预留5字节槽位
/// Executes the x86_64 dyndbg patch-site stub for one call site.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn dyndbg_patch_site<const LINE: u32, const COLUMN: u32>() -> bool {
    // SAFETY: The stub is a tiny no-op function whose first five bytes are the
    // patch slot. It is only used on x86_64, where the stub is defined below.
    unsafe { dyndbg_patch_site_stub::<LINE, COLUMN>() }
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
/// Defines the per-call-site dyndbg patch stub.
pub unsafe extern "C" fn dyndbg_patch_site_stub<const LINE: u32, const COLUMN: u32>() -> bool {
    core::arch::naked_asm!(
        "nop",
        "nop",
        "nop",
        "nop",
        "nop",
        "xor eax, eax",
        "ret",
    );
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
pub fn dyndbg_patch_site<const LINE: u32, const COLUMN: u32>() -> bool {
    let _ = (LINE, COLUMN);
    false
}

/// Patches one 5-byte slot at `site_address`.
/// 在siteaddress上写指令
///
/// This API is intentionally safe to call from upper layers. It must only be
/// used for addresses that are known to refer to a writable executable kernel
/// text slot prepared for this purpose.
pub fn patch_5byte_slot(site_address: usize, instruction: PatchInstruction) -> Result<(), PatchError> {
    let request = PatchRequest {
        instruction_address: site_address,
        instruction,
    };
    patch_5byte_slots(core::slice::from_ref(&request))
}

/// Patches multiple 5-byte slots as one transaction.
///
/// This function defines the patch transaction boundary. The current
/// implementation still writes each site sequentially, but the orchestration
/// (SMP rendezvous, text sync, serialization) runs only once for the whole
/// batch so it can be upgraded to a true SMP-safe transaction later.
pub fn patch_5byte_slots(requests: &[PatchRequest]) -> Result<(), PatchError> {
    if requests.is_empty() {
        return Ok(());
    }

    validate_patch_requests(requests)?;
    apply_patch_transaction(requests)
}

fn validate_patch_requests(requests: &[PatchRequest]) -> Result<(), PatchError> {
    for request in requests {
        if request.instruction_address == 0 {
            return Err(PatchError::InvalidSiteAddress);
        }
        let _ = encode_instruction(request.instruction_address, request.instruction)?;
    }
    Ok(())
}

fn apply_patch_transaction(requests: &[PatchRequest]) -> Result<(), PatchError> {
    let _patch_guard = PATCH_LOCK.lock();

    let mut target_count = 0usize;
    if num_cpus() > 1 && !crate::IN_BOOTSTRAP_CONTEXT.load(Ordering::Relaxed) {
        // 检查本地中断开关
        if !crate::arch::irq::is_local_enabled() {
            return Err(PatchError::IrqsDisabled);
        }

        //检查IPI发送器是否初始化完成
        if crate::smp::IPI_SENDER.get().is_none() {
            return Err(PatchError::SmpNotReady);
        }

        //获取主cpu与其它远程cpu集合
        let current_cpu = CpuId::current_racy();
        let mut target_cpus = CpuSet::new_full();
        target_cpus.remove(current_cpu);
        target_count = target_cpus.count();

        if target_count > 0 {
            PATCH_RENDEZVOUS.reset();
            // 主cpu向其它cpu发送IPI请求 并等待
            crate::smp::inter_processor_call(&target_cpus, patch_ipi_wait);
            while PATCH_RENDEZVOUS.arrived.load(Ordering::Acquire) < target_count {
                spin_loop();
            }
        }
    }

    // Ensure previous writes are globally visible before code modification.
    fence(Ordering::SeqCst);

    // 原子地写入指令字节，保证不会被中断打断
    {
        // 关中断并关闭写保护以便修改代码
        // 这里的guard会在作用域结束时自动恢复中断和写保护状态
        let _irq_guard = irq::disable_local();
        let _wp_guard = WriteProtectGuard::disable();
        for request in requests {
            let bytes = encode_instruction(request.instruction_address, request.instruction)?;
            write_bytes(request.instruction_address, &bytes);
        }
    }

    // Serialize after patching to avoid stale fetch windows on local CPU.
    // cpu可能缓存旧指令
    fence(Ordering::SeqCst);

    // 写完指令 释放其他 CPU + 等待它们全部退出等待
    if target_count > 0 {
        PATCH_RENDEZVOUS.release.store(true, Ordering::Release);
        while PATCH_RENDEZVOUS.done.load(Ordering::Acquire) < target_count {
            spin_loop();
        }
    }

    Ok(())
}
// 生成指令字节
fn encode_instruction(
    site_address: usize,
    instruction: PatchInstruction,
) -> Result<[u8; PATCH_SLOT_SIZE], PatchError> {
    match instruction {
        PatchInstruction::Nop5 => Ok([0x0F, 0x1F, 0x44, 0x00, 0x00]),
        PatchInstruction::JmpRel32 { target } => encode_jmp_rel32(site_address, target),
    }
}

// 生成完整跳转指令

fn encode_jmp_rel32(site_address: usize, target: usize) -> Result<[u8; PATCH_SLOT_SIZE], PatchError> {
    // 下条指令地址
    let next_ip = site_address
        .checked_add(PATCH_SLOT_SIZE)
        .ok_or(PatchError::Rel32OutOfRange)?;

    // 计算偏移量
    let rel = (target as i128) - (next_ip as i128);
    if rel < i32::MIN as i128 || rel > i32::MAX as i128 {
        return Err(PatchError::Rel32OutOfRange);
    }

    let rel32 = rel as i32;
    let mut bytes = [0u8; PATCH_SLOT_SIZE];
    bytes[0] = 0xE9;
    bytes[1..].copy_from_slice(&rel32.to_le_bytes());
    Ok(bytes)
}

fn write_bytes(site_address: usize, bytes: &[u8; PATCH_SLOT_SIZE]) {
    let ptr = site_address as *mut u8;
    for (offset, byte) in bytes.iter().enumerate() {
        // SAFETY: Caller guarantees the slot is a valid writable code location.
        // write_volatile保证写操作不被编译器消除
        unsafe { ptr::write_volatile(ptr.add(offset), *byte) };
    }
}

fn patch_ipi_wait() {
    PATCH_RENDEZVOUS.arrived.fetch_add(1, Ordering::Release);
    while !PATCH_RENDEZVOUS.release.load(Ordering::Acquire) {
        spin_loop();
    }
    PATCH_RENDEZVOUS.done.fetch_add(1, Ordering::Release);
}

struct WriteProtectGuard {
    cr0: x86_64::registers::control::Cr0Flags,
}

impl WriteProtectGuard {
    // 关闭cr0的写保护位并将原来的cr0值保存在guard中以便恢复
    fn disable() -> Self {
        use x86_64::registers::control::{Cr0, Cr0Flags};

        let cr0 = Cr0::read();
        let mut new_cr0 = cr0;
        new_cr0.remove(Cr0Flags::WRITE_PROTECT);

        // SAFETY: Disabling write protection is required for patching, and
        // the caller ensures it is done with local IRQs disabled.
        unsafe { Cr0::write(new_cr0) };

        Self { cr0 }
    }
}

impl Drop for WriteProtectGuard {
    fn drop(&mut self) {
        // SAFETY: Restoring the previous CR0 value puts write protection back.
        unsafe { x86_64::registers::control::Cr0::write(self.cr0) };
    }
}
