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
        // allowed for a present leaf; W and X follow perms. NOTE: perms.device
        // (MMIO/uncached) has NO PTE encoding in the base RISC-V ISA — memory
        // attributes come from the platform's PMAs (fixed in hardware/DT), not the
        // page table. So device is intentionally a no-op here; the Svpbmt
        // extension (if present) would add per-PTE memory types in a future step.
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

    fn encode_block_leaf(frame: PhysFrame, perms: Permissions, _level: usize) -> u64 {
        // On RISC-V a "superpage" is simply a LEAF PTE (R/W/X != 0) placed at a
        // non-final level — the encoding is IDENTICAL to a 4 KiB leaf; what makes
        // it a block is the LEVEL it sits at (L2 => 2 MiB, L1 => 1 GiB). The PPN
        // must be aligned to the superpage size (the walker guarantees this). So
        // this is the same bits as encode_leaf.
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

    fn is_block_leaf(entry: u64, _level: usize) -> bool {
        // On RISC-V a PTE is a LEAF (superpage when at a non-final level) iff it
        // is valid AND has any of R/W/X set. An interior table pointer has
        // R=W=X=0. So at an interior level, valid + (R|W|X) => block leaf.
        (entry & PTE_V != 0) && (entry & (PTE_R | PTE_W | PTE_X) != 0)
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

    fn kernel_span() -> u64 {
        // RISC-V S-mode: OpenSBI occupies the low ~2 MiB of RAM (ram_base ..
        // ram_base+0x200000) and the kernel loads just above it (0x80200000 on
        // QEMU virt) with an 8 MiB heap + stack. Reserve 32 MiB above the RAM base
        // to clear OpenSBI + kernel + heap. A per-arch constant relative to the
        // DISCOVERED ram_base, not a hardcoded platform address.
        32 * 1024 * 1024
    }

    fn min_identity_map_bytes() -> u64 {
        // Map at least the low 1 GiB so the peripherals (UART 0x10000000, PLIC
        // 0x0C000000, CLINT 0x02000000 — all below 256 MiB) are identity-mapped
        // even when RAM sits at 2 GiB. The orchestration raises this to cover all
        // of real RAM (max with ram_end), so the map spans 0..max(1 GiB, ram_end).
        1024 * 1024 * 1024
    }

    fn mmio_identity_ranges() -> &'static [(u64, u64)] {
        // No STATIC device windows on riscv64: the board-specific peripherals
        // (PLIC interrupt controller, CLINT timer/IPI, and the NS16550 UART) are
        // discovered from the DTB at boot (nodes plic@/clint@/serial@) and
        // registered into the runtime MMIO registry, which the MMU phase carves
        // from Normal and maps Device. The address source is the PLATFORM (DTB),
        // not a hardcoded constant — correct on SiFive, StarFive, etc., not just
        // QEMU virt. There is NO fallback: the DTB is always present (real firmware
        // and QEMU via OpenSBI), and a missing node is reported, not papered over.
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
