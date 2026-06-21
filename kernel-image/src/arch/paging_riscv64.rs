//! RISC-V 64 page-table encoding (Sv48, 4 KiB pages) and the `satp` register
//! operations to install a translation table in S-mode.
//!
//! The portable MMU orchestration and `cibos_kernel::paging` walk a 4-level,
//! 9-bits-per-level, 4 KiB-page geometry. RISC-V **Sv48** is exactly that
//! (4 levels, 9 bits each, 4 KiB pages, 48-bit VA) — so RISC-V uses Sv48 (not the
//! 3-level Sv39) to match the shared page-walk with no per-arch walk code. The
//! only architecture-specific pieces are the PTE bit layout below and the `satp`
//! setup.
//!
//! Sv48 PTE format (64-bit): bits[0]=V (valid), [1]=R, [2]=W, [3]=X, [4]=U,
//! [5]=G, [6]=A (accessed), [7]=D (dirty); the physical page number occupies
//! bits[53:10] (PPN), where PPN = PA >> 12. An interior (non-leaf) PTE has
//! R=W=X=0 (a pointer to the next-level table); a leaf PTE has at least one of
//! R/W/X set.

#![cfg(target_arch = "riscv64")]

use cibos_kernel::paging::{PageTableEncoder, Permissions};
use cibos_kernel::PhysFrame;
use core::arch::asm;

// PTE flag bits.
const PTE_V: u64 = 1 << 0; // Valid
const PTE_R: u64 = 1 << 1; // Readable
const PTE_W: u64 = 1 << 2; // Writable
const PTE_X: u64 = 1 << 3; // eXecutable
const PTE_U: u64 = 1 << 4; // User-accessible
const PTE_A: u64 = 1 << 6; // Accessed (set to avoid a fault on first access)
const PTE_D: u64 = 1 << 7; // Dirty (set so writes don't fault)

/// Convert a physical frame address to the PTE's PPN field (bits [53:10]).
/// PPN = PA >> 12, then shifted left 10 into the PTE.
fn ppn_field(frame: PhysFrame) -> u64 {
    (frame.addr() >> 12) << 10
}

/// Recover the physical frame address from a PTE's PPN field.
fn frame_from_pte(entry: u64) -> PhysFrame {
    let ppn = (entry >> 10) & ((1u64 << 44) - 1);
    PhysFrame::containing(ppn << 12)
}

/// The RISC-V Sv48 (4 KiB page) page-table entry encoder.
pub struct Sv48PageTable;

impl PageTableEncoder for Sv48PageTable {
    fn encode_table(child: PhysFrame) -> u64 {
        // Interior PTE: valid, R=W=X=0 (pointer to next level), PPN = child.
        ppn_field(child) | PTE_V
    }

    fn encode_leaf(frame: PhysFrame, perms: Permissions) -> u64 {
        // Leaf PTE: valid + A + D + the requested R/W/X/U. Reads are always
        // allowed for a present leaf; W and X follow perms.
        let mut pte = ppn_field(frame) | PTE_V | PTE_R | PTE_A | PTE_D;
        if perms.write {
            pte |= PTE_W;
        }
        if perms.execute {
            pte |= PTE_X;
        }
        if perms.user {
            pte |= PTE_U;
        }
        pte
    }

    fn is_present(entry: u64) -> bool {
        entry & PTE_V != 0
    }

    fn entry_frame(entry: u64) -> PhysFrame {
        frame_from_pte(entry)
    }
}

/// Install `root` as the active translation table via `satp` in Sv48 mode and
/// fence to apply it. MODE=9 (Sv48) in satp bits[63:60]; PPN = root >> 12.
///
/// # Safety
/// `root` must map at least all memory the kernel currently executes from and
/// its stack; otherwise the next fetch faults. Call once during bring-up.
pub unsafe fn install(root: PhysFrame) {
    const SATP_MODE_SV48: u64 = 9 << 60;
    let ppn = root.addr() >> 12;
    let satp = SATP_MODE_SV48 | ppn;
    // sfence.vma before+after is conservative; the write to satp plus an
    // sfence.vma makes the new translation active.
    asm!(
        "sfence.vma",
        "csrw satp, {0}",
        "sfence.vma",
        in(reg) satp,
        options(nostack, preserves_flags)
    );
}

/// Read the active translation-table root from `satp` (its physical address).
#[must_use]
pub fn current_root() -> u64 {
    let satp: u64;
    // SAFETY: reading satp is side-effect free.
    unsafe {
        asm!("csrr {0}, satp", out(reg) satp, options(nomem, nostack, preserves_flags));
    }
    // PPN is bits[43:0]; PA = PPN << 12.
    (satp & ((1u64 << 44) - 1)) << 12
}

/// The riscv64 implementation of the portable MMU bring-up's paging hooks.
pub struct ArchPagingImpl;

impl crate::bringup::ArchPaging for ArchPagingImpl {
    type Encoder = Sv48PageTable;

    fn identity_map_bytes() -> u64 {
        // QEMU virt RAM starts at 0x80000000 (2 GiB); OpenSBI occupies the low
        // part and the kernel loads above it. Identity-map through the RAM the
        // kernel uses. 2.25 GiB covers 0..0x90000000 (the low devices, OpenSBI,
        // and the kernel+heap region just above 2 GiB).
        (2048 + 256) * 1024 * 1024 // 2.25 GiB
    }

    fn reserved_below() -> u64 {
        // QEMU virt RV64 RAM starts at 2 GiB (0x80000000); OpenSBI sits at the
        // base and the kernel is loaded above it. Reserve through 2 GiB + 32 MiB
        // so the frame allocator never hands out OpenSBI's or the kernel's memory
        // for page tables; frames come from RAM above that.
        2u64 * 1024 * 1024 * 1024 + 32 * 1024 * 1024 // 2 GiB + 32 MiB
    }

    fn mmio_identity_ranges() -> &'static [(u64, u64)] {
        // QEMU virt RV64 peripherals (UART 0x10000000, PLIC 0x0C000000, CLINT
        // 0x02000000) all live below 2 GiB and are covered by the main identity
        // map. No extra high-MMIO ranges.
        &[]
    }

    unsafe fn enable_table_features() {
        // RISC-V needs no separate feature enable before building tables; XWR
        // permission bits are honored directly. Nothing to do here.
    }

    unsafe fn install(root: PhysFrame) {
        install(root);
    }

    fn current_root() -> u64 {
        current_root()
    }
}
