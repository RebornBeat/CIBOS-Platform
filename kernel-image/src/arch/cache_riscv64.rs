//! RISC-V DMA cache maintenance via the Zicbom extension (CBO instructions).
//!
//! On a platform whose DMA is NOT cache-coherent (the virtio-mmio node lacks
//! `dma-coherent`), a driver that exposes buffers to a device must maintain the
//! data cache by hand:
//!   * CLEAN (write back) a buffer the DEVICE will READ, before notifying it, so
//!     the device sees the CPU's latest writes; and
//!   * INVALIDATE a buffer the DEVICE has WRITTEN, after completion and before the
//!     CPU reads it, so the CPU does not read stale cached data.
//!
//! The Zicbom extension provides CBO.CLEAN / CBO.INVAL / CBO.FLUSH for exactly
//! this. Availability and the cache-block size are platform facts read from the
//! DTB (`riscv,cbom-block-size`, and `zicbom` in `riscv,isa`); this module does
//! NOTHING unless the block size has been configured from the DTB — it never
//! silently assumes a value.
//!
//! HONESTY NOTE: this code is compiled and the CBO encodings are valid, but the
//! INVALIDATE semantics cannot be fully validated in the QEMU sandbox because QEMU
//! models coherent memory (the CBOs are architecturally valid but have no visible
//! effect there). On real non-coherent RISC-V hardware these are the operations
//! that make DMA correct. The driver consults [`coherent_or_maintained`] so it
//! refuses to run unsafely rather than silently corrupt data.

use core::sync::atomic::{AtomicU32, Ordering};

/// Cache-block size in bytes, from `riscv,cbom-block-size` in the DTB. 0 = not
/// configured (Zicbom maintenance unavailable / unknown).
static CBOM_BLOCK_SIZE: AtomicU32 = AtomicU32::new(0);

/// Record the DTB-reported cache-block size (call once at boot if Zicbom is
/// present). A non-zero value enables [`clean_range`] / [`invalidate_range`].
pub fn set_cbom_block_size(bytes: u32) {
    CBOM_BLOCK_SIZE.store(bytes, Ordering::Relaxed);
}

/// CBO.CLEAN on the block containing `addr` (write back dirty lines).
///
/// # Safety
/// Zicbom must be implemented (the caller gates on a non-zero block size).
#[inline]
unsafe fn cbo_clean(addr: usize) {
    // cbo.clean (rs1): misc-mem opcode 0x0F, funct3=0x2, imm/funct=0x001 in rs2-ish
    // field. Encoded via `.insn` raw form so it assembles without a target-feature.
    core::arch::asm!(".insn i 0x0F, 0x2, x0, {0}, 0x001", in(reg) addr, options(nostack, preserves_flags));
}

/// CBO.INVAL on the block containing `addr` (drop cached lines).
///
/// # Safety
/// Zicbom must be implemented (the caller gates on a non-zero block size).
#[inline]
unsafe fn cbo_inval(addr: usize) {
    core::arch::asm!(".insn i 0x0F, 0x2, x0, {0}, 0x000", in(reg) addr, options(nostack, preserves_flags));
}

/// Walk `[base, base+len)` block-by-block applying `op` (clean or invalidate).
#[inline]
unsafe fn for_each_block(base: usize, len: usize, op: impl Fn(usize)) {
    let bs = CBOM_BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    if bs == 0 || len == 0 {
        return; // maintenance unavailable or nothing to do
    }
    let start = base & !(bs - 1);
    let end = base + len;
    let mut a = start;
    while a < end {
        op(a);
        a += bs;
    }
    // Order the maintenance before/after the surrounding fence in the caller.
    core::sync::atomic::fence(Ordering::SeqCst);
}

/// Clean (write back) the cache over `[base, base+len)` before a device reads it.
/// No-op if Zicbom maintenance is not configured.
///
/// # Safety
/// `base..base+len` must be valid kernel memory.
pub unsafe fn clean_range(base: usize, len: usize) {
    for_each_block(base, len, |a| cbo_clean(a));
}

/// Invalidate the cache over `[base, base+len)` after a device wrote it, before
/// the CPU reads it. No-op if Zicbom maintenance is not configured.
///
/// # Safety
/// `base..base+len` must be valid kernel memory; the device must have finished
/// writing (the caller polls completion first).
pub unsafe fn invalidate_range(base: usize, len: usize) {
    for_each_block(base, len, |a| cbo_inval(a));
}
