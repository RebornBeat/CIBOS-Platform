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

    fn set_handler(&mut self, handler: u64, selector: u16, type_attr: u8, ist: u8) {
        self.offset_low = (handler & 0xFFFF) as u16;
        self.offset_mid = ((handler >> 16) & 0xFFFF) as u16;
        self.offset_high = ((handler >> 32) & 0xFFFF_FFFF) as u32;
        self.selector = selector;
        // IST index occupies the low 3 bits of this byte (0 = use rsp0/legacy).
        self.ist = ist & 0x7;
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

    // Install CPU-exception handlers (vectors 0..=19, skipping 15 which is
    // reserved) so faults are reported instead of triple-faulting through empty
    // gates. DPL=0 interrupt gates.
    const GATE_INT64_DPL0: u8 = 0x8E;
    macro_rules! set_fault {
        ($vec:literal, $sym:ident) => {{
            extern "C" {
                fn $sym();
            }
            IDT[$vec].set_handler(
                $sym as *const () as u64,
                cs,
                GATE_INT64_DPL0,
                crate::arch::gdt::FAULT_IST_INDEX,
            );
        }};
    }
    set_fault!(0, cibos_fault_0);
    set_fault!(1, cibos_fault_1);
    set_fault!(2, cibos_fault_2);
    set_fault!(3, cibos_fault_3);
    set_fault!(4, cibos_fault_4);
    set_fault!(5, cibos_fault_5);
    set_fault!(6, cibos_fault_6);
    set_fault!(7, cibos_fault_7);
    set_fault!(8, cibos_fault_8);
    set_fault!(9, cibos_fault_9);
    set_fault!(10, cibos_fault_10);
    set_fault!(11, cibos_fault_11);
    set_fault!(12, cibos_fault_12);
    set_fault!(13, cibos_fault_13);
    set_fault!(14, cibos_fault_14);
    set_fault!(16, cibos_fault_16);
    set_fault!(17, cibos_fault_17);
    set_fault!(18, cibos_fault_18);
    set_fault!(19, cibos_fault_19);

    let handler = syscall_trap_entry as *const () as u64;
    IDT[VECTOR_SYSCALL].set_handler(handler, cs, GATE_INT64_DPL3, 0);

    // Keyboard IRQ: the PIC is remapped so IRQ1 arrives at vector 0x21. A DPL=0
    // interrupt gate (ring 3 cannot invoke it directly; only the hardware line
    // does). The handler runs on the current stack via the legacy rsp0 path.
    extern "C" {
        fn keyboard_irq_entry();
    }
    const VECTOR_KEYBOARD: usize = 0x21;
    IDT[VECTOR_KEYBOARD].set_handler(
        keyboard_irq_entry as *const () as u64,
        cs,
        GATE_INT64_DPL0,
        0,
    );

    let ptr = IdtPointer {
        limit: (size_of::<[IdtEntry; IDT_LEN]>() - 1) as u16,
        base: core::ptr::addr_of!(IDT) as u64,
    };
    asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
}

/// Reporter called by the assembly fault stub: print the vector and faulting
/// RIP, then the stub halts. `#[no_mangle]` so the asm can `call` it.
#[no_mangle]
pub extern "C" fn cibos_fault_report(vector: u64, error_code: u64, rip: u64) {
    use core::fmt::Write;
    let mut console = crate::boot::Console;
    let _ = writeln!(
        console,
        "CIBOS kernel: [FAULT] vector {vector} err={error_code:#x} at RIP {rip:#x} — halting"
    );
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
