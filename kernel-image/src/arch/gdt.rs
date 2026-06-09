//! x86_64 GDT and TSS for ring-3 (user-mode) execution.
//!
//! The bootloader's GDT has only ring-0 code/data, which is enough to run the
//! kernel but cannot host an unprivileged application. This module installs a
//! kernel-owned GDT that adds ring-3 code/data selectors and a TSS, so the
//! kernel can drop to ring 3 (via `iretq` to user code) and the CPU knows which
//! ring-0 stack to switch to when a ring-3 trap (e.g. `int 0x80`) occurs.
//!
//! ## Layout
//!
//! | index | selector | purpose                         |
//! |-------|----------|---------------------------------|
//! | 0     | 0x00     | null                            |
//! | 1     | 0x08     | ring-0 code (kernel)            |
//! | 2     | 0x10     | ring-0 data (kernel)            |
//! | 3     | 0x1B     | ring-3 code (user, RPL=3)       |
//! | 4     | 0x23     | ring-3 data (user, RPL=3)       |
//! | 5/6   | 0x28     | TSS (16-byte system descriptor) |
//!
//! Selectors carry their requested privilege level (RPL) in the low two bits, so
//! the user selectors are `index<<3 | 3`.

use core::arch::asm;
use core::mem::size_of;

/// Ring-0 code selector (matches the bootloader's 0x08).
pub const KERNEL_CODE: u16 = 0x08;
/// Ring-0 data selector.
pub const KERNEL_DATA: u16 = 0x10;
/// Ring-3 code selector (index 3, RPL 3).
pub const USER_CODE: u16 = (3 << 3) | 3;
/// Ring-3 data selector (index 4, RPL 3).
pub const USER_DATA: u16 = (4 << 3) | 3;
/// TSS selector (index 5).
pub const TSS_SELECTOR: u16 = 5 << 3;

/// Ring-0 stack used when a ring-3 trap enters the kernel. 16 KiB, 16-aligned.
const KSTACK_SIZE: usize = 16 * 1024;
#[repr(align(16))]
struct KernelStack([u8; KSTACK_SIZE]);
static mut PRIV_STACK: KernelStack = KernelStack([0; KSTACK_SIZE]);

/// A dedicated stack for fault delivery (IST1). Using an IST entry guarantees a
/// CPU exception always lands on a known-good stack regardless of the faulting
/// context's rsp — the textbook way to make a fault that occurs right after a
/// privilege transition (where the normal rsp0 path can double-fault) reliably
/// deliverable and diagnosable.
static mut FAULT_STACK: KernelStack = KernelStack([0; KSTACK_SIZE]);

/// The IST index (1-based in hardware) used by the CPU-exception gates.
pub const FAULT_IST_INDEX: u8 = 1;

/// The 64-bit Task State Segment. Only `rsp0` (the ring-0 stack pointer) and the
/// I/O-map base are meaningful here; the rest stay zero.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Tss {
    _reserved0: u32,
    rsp: [u64; 3], // rsp0..rsp2
    _reserved1: u64,
    ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    iomap_base: u16,
}

impl Tss {
    const fn new() -> Self {
        Self {
            _reserved0: 0,
            rsp: [0; 3],
            _reserved1: 0,
            ist: [0; 7],
            _reserved2: 0,
            _reserved3: 0,
            iomap_base: size_of::<Tss>() as u16, // no I/O bitmap
        }
    }
}

static mut TSS: Tss = Tss::new();

// GDT: 5 eight-byte entries (null, kcode, kdata, ucode, udata) + a 16-byte TSS
// descriptor (= two eight-byte slots), so 7 u64 slots total.
const GDT_SLOTS: usize = 7;
static mut GDT: [u64; GDT_SLOTS] = [0; GDT_SLOTS];

#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u64,
}

/// Build a code/data segment descriptor with the given DPL and exec flag. In
/// 64-bit mode base/limit are ignored; the access and flag bits carry meaning.
fn segment(dpl: u8, executable: bool) -> u64 {
    // Access byte: present(7) | dpl(6:5) | desc-type=1(4) | exec(3) | rw(1).
    let mut access: u64 = 1 << 7 | ((dpl as u64) << 5) | (1 << 4) | (1 << 1);
    if executable {
        access |= 1 << 3;
    }
    // Flags nibble (bits 52..55 of the descriptor): granularity(55) | DB(54) |
    // long-mode L(53). For 64-bit code, L=1, DB=0; data segments leave L=0.
    let flags: u64 = if executable {
        (1 << 7) | (1 << 5) // G | L
    } else {
        1 << 7 // G
    };
    (access << 40) | (flags << 48) | 0x0000_0000_0000_FFFF
}

/// Install the kernel GDT (with ring-3 selectors and the TSS), reload the data
/// segment registers, and load the task register.
///
/// # Safety
///
/// Single-threaded bring-up only; replaces the active GDT. Must run before any
/// ring transition.
pub unsafe fn init() {
    // Point the TSS ring-0 stack at the top of the private stack.
    let stack_top = core::ptr::addr_of!(PRIV_STACK.0) as u64 + KSTACK_SIZE as u64;
    TSS.rsp[0] = stack_top;

    // IST1: dedicated fault-delivery stack. CPU-exception gates reference this
    // (via their IST field) so exceptions always land on a known-good stack.
    let fault_top = core::ptr::addr_of!(FAULT_STACK.0) as u64 + KSTACK_SIZE as u64;
    TSS.ist[0] = fault_top;

    GDT[0] = 0;
    GDT[1] = segment(0, true); // kernel code
    GDT[2] = segment(0, false); // kernel data
    GDT[3] = segment(3, true); // user code
    GDT[4] = segment(3, false); // user data

    // 16-byte TSS descriptor across slots 5 and 6.
    let tss_addr = core::ptr::addr_of!(TSS) as u64;
    let limit = (size_of::<Tss>() - 1) as u64;
    let type_access: u64 = (1 << 7) | (0x9); // present | type=0x9 (available 64-bit TSS)
    let low = (limit & 0xFFFF)
        | ((tss_addr & 0xFF_FFFF) << 16)
        | (type_access << 40)
        | (((limit >> 16) & 0xF) << 48)
        | (((tss_addr >> 24) & 0xFF) << 56);
    let high = (tss_addr >> 32) & 0xFFFF_FFFF;
    GDT[5] = low;
    GDT[6] = high;

    let ptr = GdtPointer {
        limit: (size_of::<[u64; GDT_SLOTS]>() - 1) as u16,
        base: core::ptr::addr_of!(GDT) as u64,
    };
    asm!("lgdt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));

    // Reload data segment registers with the kernel data selector and reload CS
    // via a far return.
    asm!(
        "mov ax, {data:x}",
        "mov ds, ax",
        "mov es, ax",
        "mov ss, ax",
        "mov fs, ax",
        "mov gs, ax",
        // Reload CS: push new selector + a label, then a far return (retfq).
        "lea rax, [rip + 2f]",
        "push {kcode}",
        "push rax",
        "retfq",
        "2:",
        data = in(reg) KERNEL_DATA as u64,
        kcode = in(reg) KERNEL_CODE as u64,
        out("rax") _,
        options(preserves_flags),
    );

    // Load the task register with the TSS selector.
    asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
}

/// The ring-0 stack top currently configured in the TSS (`rsp0`).
//
// Used by the forthcoming per-task process model (each task sets its own ring-0
// stack before entering ring 3); unused while the ring-3 demo uses the single
// init-time stack, so allowed dead code for now.
#[allow(dead_code)]
#[must_use]
pub fn kernel_stack_top() -> u64 {
    // SAFETY: single-threaded read of a static set during init.
    unsafe { TSS.rsp[0] }
}

/// Update the TSS ring-0 stack pointer (called before entering ring 3 so a trap
/// returns onto a known-good kernel stack).
///
/// # Safety
///
/// `stack_top` must point at the top of a valid, mapped ring-0 stack.
#[allow(dead_code)]
pub unsafe fn set_kernel_stack(stack_top: u64) {
    TSS.rsp[0] = stack_top;
}
