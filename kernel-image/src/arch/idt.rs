//! x86_64 interrupt descriptor table and the syscall trap gate.
//!
//! Installs a minimal IDT with one entry — vector `0x80`, the syscall gate — and
//! the assembly trap stub that saves the caller's registers, marshals the
//! syscall number and arguments per the [`shared::protocols::syscall`] ABI into
//! a [`cibos_kernel::SyscallRequest`], calls the portable dispatcher, and
//! returns the result in `rax`.
//!
//! This first step uses the `int 0x80` software-interrupt path: it proves the
//! trap → dispatch → return round trip on real hardware. Ring-3 user/supervisor
//! separation (a TSS, ring-3 selectors, and `iretq` to user code) builds on this
//! same IDT and is the next layer; the dispatcher and ABI do not change.

use core::arch::asm;
use core::mem::size_of;

/// A 64-bit IDT gate descriptor (16 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    const fn empty() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            zero: 0,
        }
    }

    fn set_handler(&mut self, handler: u64, selector: u16, type_attr: u8) {
        self.offset_low = (handler & 0xFFFF) as u16;
        self.offset_mid = ((handler >> 16) & 0xFFFF) as u16;
        self.offset_high = ((handler >> 32) & 0xFFFF_FFFF) as u32;
        self.selector = selector;
        self.ist = 0;
        self.type_attr = type_attr;
        self.zero = 0;
    }
}

/// The IDT pointer loaded by `lidt`.
#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

/// 256 gates; only the syscall vector is populated for now.
const IDT_LEN: usize = 256;
const VECTOR_SYSCALL: usize = 0x80;

// Gate type/attribute byte: present | DPL=3 | 64-bit interrupt gate (0x8E with
// DPL=3 => 0xEE). DPL=3 lets ring-3 issue `int 0x80`; until ring-3 exists this
// is harmless and forward-compatible.
const GATE_INT64_DPL3: u8 = 0xEE;

static mut IDT: [IdtEntry; IDT_LEN] = [IdtEntry::empty(); IDT_LEN];

/// Install the IDT and the syscall gate. Reads the current `CS` so the gate's
/// selector matches whatever the bootloader/long-mode GDT established.
///
/// # Safety
///
/// Must be called once during single-threaded kernel bring-up before any trap
/// can occur. Installs a process-wide IDT.
pub unsafe fn init() {
    let cs: u16;
    asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags));

    let handler = syscall_trap_entry as usize as u64;
    IDT[VECTOR_SYSCALL].set_handler(handler, cs, GATE_INT64_DPL3);

    let ptr = IdtPointer {
        limit: (size_of::<[IdtEntry; IDT_LEN]>() - 1) as u16,
        base: core::ptr::addr_of!(IDT) as u64,
    };
    asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
}

extern "C" {
    /// The assembly trap stub (defined in `syscall_entry.s`).
    fn syscall_trap_entry();
}

/// The Rust trap handler the assembly stub tail-calls with the saved argument
/// registers. Returns the value to place in the caller's `rax`.
///
/// `number`=rax, `arg0`=rdi, `arg1`=rsi, `arg2`=rdx at trap time (the ABI).
#[no_mangle]
pub extern "C" fn cibos_syscall_handler(number: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    crate::boot::handle_syscall(number, arg0, arg1, arg2)
}
