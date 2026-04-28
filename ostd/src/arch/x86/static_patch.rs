// SPDX-License-Identifier: MPL-2.0

//! Minimal x86_64 static patching primitives.
//!
//! This module intentionally keeps a very small surface: patch one fixed
//! 5-byte instruction slot as either `NOP5` or `JMP rel32`.

use core::{ptr, sync::atomic::{Ordering, fence}};

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

/// Errors returned by mini static patch operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchError {
    /// The site address is not usable.
    InvalidSiteAddress,
    /// The target is out of `rel32` range for `JMP`.
    Rel32OutOfRange,
}

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
    if site_address == 0 {
        return Err(PatchError::InvalidSiteAddress);
    }

    // Ensure previous writes are globally visible before code modification.
    fence(Ordering::SeqCst);

    // 在siteaddress上写指令
    match instruction {
        PatchInstruction::Nop5 => {
            write_nop5(site_address);
        }
        PatchInstruction::JmpRel32 { target } => {
            write_jmp_rel32(site_address, target)?;
        }
    }

    // Serialize after patching to avoid stale fetch windows on local CPU.
    // cpu可能缓存旧指令
    fence(Ordering::SeqCst);

    Ok(())
}

fn write_nop5(site_address: usize) {
    const NOP5: [u8; PATCH_SLOT_SIZE] = [0x0F, 0x1F, 0x44, 0x00, 0x00];
    write_bytes(site_address, &NOP5);
}

fn write_jmp_rel32(site_address: usize, target: usize) -> Result<(), PatchError> {
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
    write_bytes(site_address, &bytes);

    Ok(())
}

fn write_bytes(site_address: usize, bytes: &[u8; PATCH_SLOT_SIZE]) {
    let ptr = site_address as *mut u8;
    for (offset, byte) in bytes.iter().enumerate() {
        // SAFETY: Caller guarantees the slot is a valid writable code location.
        // write_volatile保证写操作不被编译器消除
        unsafe { ptr::write_volatile(ptr.add(offset), *byte) };
    }
}
