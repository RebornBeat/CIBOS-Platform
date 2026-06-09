//! x86_64 page-table encoding and installation.
//!
//! Implements [`cibos_kernel::PageTableEncoder`] with the real x86_64 4-level
//! (PML4 → PDPT → PD → PT) entry bit layout, and installs a built address space
//! by writing its root frame to `CR3`. The portable model in `cibos-kernel`
//! builds the table tree and decides permissions; this is the thin architecture
//! glue that turns those decisions into the exact words the CPU's page-table
//! walker reads, and the instruction that activates them.

use cibos_kernel::paging::{PageTableEncoder, Permissions};
use cibos_kernel::PhysFrame;
use core::arch::asm;

// x86_64 page-table entry bits.
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const NO_EXECUTE: u64 = 1 << 63;
/// Physical address field of an entry (bits 12..=51).
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// The x86_64 page-table entry encoder.
pub struct X86PageTable;

impl PageTableEncoder for X86PageTable {
    fn encode_table(child: PhysFrame) -> u64 {
        // Interior entries are present + writable + user so the leaf's bits are
        // authoritative for the final permission (the CPU ANDs the path's
        // writable/user bits, so interiors must be permissive). Execute is
        // governed at the leaf via NX.
        (child.addr() & ADDR_MASK) | PRESENT | WRITABLE | USER
    }

    fn encode_leaf(frame: PhysFrame, perms: Permissions) -> u64 {
        let mut e = (frame.addr() & ADDR_MASK) | PRESENT;
        if perms.write {
            e |= WRITABLE;
        }
        if perms.user {
            e |= USER;
        }
        if !perms.execute {
            e |= NO_EXECUTE;
        }
        e
    }

    fn is_present(entry: u64) -> bool {
        entry & PRESENT != 0
    }

    fn entry_frame(entry: u64) -> PhysFrame {
        PhysFrame::containing(entry & ADDR_MASK)
    }
}

/// Install `root` (a PML4 physical frame) as the active address space by
/// writing it to `CR3`, flushing the TLB.
///
/// # Safety
///
/// `root` must point at a valid, fully-populated PML4 that maps at least all
/// memory the kernel is currently executing from and its stack; otherwise the
/// next instruction fetch faults. The NX bit must be enabled in EFER (it is,
/// under the bootloader/long-mode setup) for the leaf NX bits to take effect.
pub unsafe fn install(root: PhysFrame) {
    asm!("mov cr3, {}", in(reg) root.addr(), options(nostack, preserves_flags));
}

/// Read the currently active page-table root from `CR3` (its physical address).
#[must_use]
pub fn current_root() -> u64 {
    let cr3: u64;
    // SAFETY: reading CR3 is side-effect free.
    unsafe {
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }
    cr3 & ADDR_MASK
}
