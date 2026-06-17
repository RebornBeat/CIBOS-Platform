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
/// next instruction fetch faults. For leaf NX bits to be honored rather than
/// faulting as reserved bits, [`enable_nxe`] must have been called first.
pub unsafe fn install(root: PhysFrame) {
    asm!("mov cr3, {}", in(reg) root.addr(), options(nostack, preserves_flags));
}

/// Enable EFER.NXE (no-execute enable, bit 11) so the NX bit (bit 63) in
/// page-table entries is honored. Until this is set, NX is a *reserved* bit and
/// any entry with it set faults the page walk; the bootloader enables LME but
/// not NXE, so the kernel must do this before installing tables that use NX
/// (the W^X user/stack pages depend on it).
///
/// # Safety
///
/// Modifies the EFER MSR; call once during single-threaded bring-up before
/// installing page tables that set NX.
pub unsafe fn enable_nxe() {
    const IA32_EFER: u32 = 0xC000_0080;
    const NXE: u64 = 1 << 11;
    let (mut lo, hi): (u32, u32);
    asm!("rdmsr", in("ecx") IA32_EFER, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    let efer = (u64::from(hi) << 32) | u64::from(lo);
    let efer = efer | NXE;
    lo = efer as u32;
    let hi = (efer >> 32) as u32;
    asm!("wrmsr", in("ecx") IA32_EFER, in("eax") lo, in("edx") hi, options(nomem, nostack, preserves_flags));
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

/// The active page-table root as a [`PhysFrame`] (for adopting the current space
/// to map additional pages into it, e.g. a `spawn`ed lane's stack).
#[cfg(feature = "ring3-multilane-demo")]
#[must_use]
pub fn current_root_frame() -> cibos_kernel::PhysFrame {
    cibos_kernel::PhysFrame::containing(current_root())
}
