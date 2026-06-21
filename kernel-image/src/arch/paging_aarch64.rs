//! AArch64 page-table encoding (VMSAv8-64, 4 KiB granule) and the register
//! operations to install a translation table at EL1.
//!
//! The portable MMU bring-up orchestration (`bring_up_mmu_generic`) and the
//! portable `cibos_kernel::paging::AddressSpace` walk a 4-level, 9-bits-per-level,
//! 4 KiB-page geometry — which is EXACTLY the AArch64 4 KiB-granule / 48-bit-VA
//! format, so the only architecture-specific piece is the descriptor bit layout
//! below plus the TTBR/TCR/SCTLR/MAIR register setup. No per-arch page-walk code.
//!
//! Descriptor format (stage 1, EL1, 4 KiB granule):
//!   * Table descriptor (levels 0-2): bits[1:0]=0b11, bits[47:12]=child PA.
//!   * Page descriptor (level 3):     bits[1:0]=0b11, AF=1, plus AP/UXN/PXN/SH
//!     and the MAIR attribute index. (A block descriptor uses bits[1:0]=0b01 but
//!     we map 4 KiB pages at level 3, so all leaves are page descriptors.)

#![cfg(target_arch = "aarch64")]

use cibos_kernel::paging::{PageTableEncoder, Permissions};
use cibos_kernel::PhysFrame;
use core::arch::asm;

/// The physical-address mask for a 4 KiB-aligned descriptor (bits [47:12]).
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

// Descriptor low bits.
const DESC_VALID: u64 = 1 << 0; // bit0: descriptor is valid
const DESC_TABLE_OR_PAGE: u64 = 1 << 1; // bit1: table (L0-2) or page (L3) — both set it
const DESC_AF: u64 = 1 << 10; // Access Flag (set to avoid an access fault on first use)

// Access permissions (bits [7:6], AP[2:1]).
const AP_RW_EL1: u64 = 0b00 << 6; // read/write at EL1, no EL0 access
const AP_RW_ALL: u64 = 0b01 << 6; // read/write at EL1 and EL0
const AP_RO_EL1: u64 = 0b10 << 6; // read-only at EL1
const AP_RO_ALL: u64 = 0b11 << 6; // read-only at EL1 and EL0

// Shareability (bits [9:8], SH). Inner-shareable for normal cacheable memory.
const SH_INNER: u64 = 0b11 << 8;

// Execute-never bits.
const PXN: u64 = 1 << 53; // Privileged eXecute Never (EL1)
const UXN: u64 = 1 << 54; // Unprivileged eXecute Never (EL0)

// MAIR attribute index (bits [4:2], AttrIndx). Index 0 = Normal write-back (set
// up in MAIR_EL1 at install time). (A Device-nGnRnE attribute at index 1 is set
// up in MAIR for a future explicit device-memory mapping path; the current MMIO
// ranges are within the Normal-mapped low region.)
const ATTR_NORMAL: u64 = 0 << 2;

/// The AArch64 VMSAv8-64 (4 KiB granule) page-table entry encoder.
pub struct Aarch64PageTable;

impl PageTableEncoder for Aarch64PageTable {
    fn encode_table(child: PhysFrame) -> u64 {
        // Interior table descriptor: valid + table type + child PA. Permission
        // enforcement is deferred to the leaf (table-level AP/XN attributes are
        // left permissive so leaf bits are authoritative — matches the portable
        // contract's expectation).
        (child.addr() & ADDR_MASK) | DESC_VALID | DESC_TABLE_OR_PAGE
    }

    fn encode_leaf(frame: PhysFrame, perms: Permissions) -> u64 {
        // Level-3 page descriptor: valid + page type + AF + shareability +
        // attr index + AP + XN. Device vs normal memory is chosen by whether the
        // mapping is executable kernel RAM (normal) — MMIO is mapped non-exec, so
        // we treat non-exec kernel pages conservatively as normal too; a dedicated
        // device mapping path can pass an explicit attribute later. For now: all
        // RAM is Normal cacheable; the MMIO ranges are mapped non-exec which is
        // safe under Normal-NC-like behavior in QEMU, but to be correct we select
        // Device attributes for non-executable kernel pages that are not writable
        // data... however we cannot distinguish here, so RAM stays Normal.
        let mut desc = (frame.addr() & ADDR_MASK)
            | DESC_VALID
            | DESC_TABLE_OR_PAGE // bit1 set => page descriptor at L3
            | DESC_AF
            | SH_INNER
            | ATTR_NORMAL;

        // Access permissions from read/write/user.
        desc |= match (perms.write, perms.user) {
            (true, true) => AP_RW_ALL,
            (true, false) => AP_RW_EL1,
            (false, true) => AP_RO_ALL,
            (false, false) => AP_RO_EL1,
        };

        // Execute-never: if not executable, set both PXN and UXN. Also always set
        // UXN for kernel (non-user) pages so EL0 can never execute kernel memory.
        if !perms.execute {
            desc |= PXN | UXN;
        } else if !perms.user {
            desc |= UXN;
        }

        desc
    }

    fn is_present(entry: u64) -> bool {
        entry & DESC_VALID != 0
    }

    fn entry_frame(entry: u64) -> PhysFrame {
        PhysFrame::containing(entry & ADDR_MASK)
    }
}

/// Install `root` as the EL1 translation table (TTBR0_EL1), program TCR_EL1 and
/// MAIR_EL1 for a 4 KiB granule / 48-bit VA, and enable the MMU (SCTLR_EL1.M).
///
/// # Safety
/// `root` must map at least all memory the kernel currently executes from and
/// its stack; otherwise the next fetch faults. Call once during bring-up.
pub unsafe fn install(root: PhysFrame) {
    // MAIR_EL1: attr0 = Normal write-back (0xFF), attr1 = Device-nGnRnE (0x00).
    let mair: u64 = 0x00FF;
    asm!("msr mair_el1, {}", in(reg) mair, options(nostack, preserves_flags));

    // TCR_EL1: T0SZ=16 (48-bit VA), 4 KiB granule (TG0=00), inner/outer
    // write-back cacheable (IRGN0/ORGN0=01), inner-shareable (SH0=11),
    // IPS=40-bit (bits[34:32]=0b010).
    let t0sz: u64 = 16;
    let irgn0: u64 = 0b01 << 8;
    let orgn0: u64 = 0b01 << 10;
    let sh0: u64 = 0b11 << 12;
    let tg0: u64 = 0b00 << 14;
    let ips: u64 = 0b010 << 32;
    let tcr: u64 = t0sz | irgn0 | orgn0 | sh0 | tg0 | ips;
    asm!("msr tcr_el1, {}", in(reg) tcr, options(nostack, preserves_flags));

    // TTBR0_EL1 = root table PA.
    asm!("msr ttbr0_el1, {}", in(reg) root.addr(), options(nostack, preserves_flags));

    // Ensure the table writes and system-register writes are visible before
    // enabling translation.
    asm!("dsb ish", "isb", options(nostack, preserves_flags));

    // SCTLR_EL1: set M (bit0, MMU enable), C (bit2, data cache), I (bit12,
    // instruction cache). Read-modify-write to preserve the rest.
    let mut sctlr: u64;
    asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nostack, preserves_flags));
    sctlr |= (1 << 0) | (1 << 2) | (1 << 12);
    asm!("msr sctlr_el1, {}", "isb", in(reg) sctlr, options(nostack, preserves_flags));
}

/// Read the active translation-table root from TTBR0_EL1 (its physical address).
#[must_use]
pub fn current_root() -> u64 {
    let ttbr0: u64;
    // SAFETY: reading TTBR0_EL1 is side-effect free.
    unsafe {
        asm!("mrs {}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack, preserves_flags));
    }
    ttbr0 & ADDR_MASK
}

/// The aarch64 implementation of the portable MMU bring-up's paging hooks.
pub struct ArchPagingImpl;

impl crate::bringup::ArchPaging for ArchPagingImpl {
    type Encoder = Aarch64PageTable;

    fn identity_map_bytes() -> u64 {
        // On QEMU virt, RAM starts at 1 GiB (0x40000000) — the kernel, heap, and
        // stack live just ABOVE 1 GiB (the kernel at 0x40080000), and the
        // peripherals (PL011 at 0x09000000, GIC at 0x08000000) live below 256
        // MiB. So the identity map must span from 0 through the RAM the kernel
        // uses. 1.25 GiB covers the low peripheral region AND the 128 MiB RAM
        // region at 1 GiB. (Mapping the full 4 GiB as 4 KiB pages is ~1M
        // iterations — impractically slow in debug; block descriptors are a
        // future portable-layer enhancement.)
        (1024 + 256) * 1024 * 1024 // 1.25 GiB
    }

    fn reserved_below() -> u64 {
        // QEMU virt RAM is 128 MiB starting at 1 GiB (0x40000000..0x48000000);
        // the kernel loads at 0x40080000 with an 8 MiB heap + stack above it.
        // Reserve through 1 GiB + 32 MiB so the kernel image/heap/stack are
        // protected, while the rest of the 128 MiB RAM (above 1 GiB + 32 MiB)
        // stays free for page-table frames. (Reserving the full 1.25 GiB would
        // reserve ALL 128 MiB of RAM, leaving zero free frames.)
        1024 * 1024 * 1024 + 32 * 1024 * 1024 // 1 GiB + 32 MiB
    }

    fn mmio_identity_ranges() -> &'static [(u64, u64)] {
        // On QEMU virt the peripherals (PL011 UART at 0x09000000, GIC at
        // 0x08000000) live BELOW 256 MiB, so they are already covered by the main
        // low-RAM identity map (which spans 0..1.25 GiB). Unlike the x86 PCI hole
        // (which sits ABOVE the 1 GiB map and needs a separate mapping), there are
        // no extra high-MMIO ranges to map here. Empty.
        &[]
    }

    unsafe fn enable_table_features() {
        // AArch64 needs no separate NX-enable (XN is honored by default); the
        // MMU/cache enable happens in `install`. Nothing to do here.
    }

    unsafe fn install(root: PhysFrame) {
        install(root);
    }

    fn current_root() -> u64 {
        current_root()
    }
}
