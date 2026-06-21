//! RISC-V 64 serial (SBI legacy console) and halt for the kernel image.

use core::arch::asm;

/// Output one byte via the SBI legacy `console_putchar` call (EID 0x01). The
/// firmware (OpenSBI) owns the UART, so the kernel needs no MMIO address.
fn sbi_putchar(ch: u8) {
    unsafe {
        asm!(
            "ecall",
            in("a7") 1usize,        // legacy console_putchar extension id
            in("a0") ch as usize,
            lateout("a0") _,
            options(nostack),
        );
    }
}

/// Nothing to initialize: OpenSBI has the console ready.
pub fn init_serial() {}

/// Write one byte through SBI.
pub fn putc(b: u8) {
    sbi_putchar(b);
}

/// Halt the hart permanently.
pub fn halt() -> ! {
    loop {
        unsafe {
            asm!("wfi", options(nomem, nostack));
        }
    }
}

/// Install the S-mode trap vector: point `stvec` at `cibos_riscv_trap` so any
/// trap (exception or interrupt) is reported (and, during bring-up, halts)
/// instead of vanishing. The x86 equivalent is the IDT install + fault reporter;
/// the aarch64 equivalent is VBAR_EL1. Call once, early in `kernel_entry`.
///
/// # Safety
/// Writes the S-mode trap vector register; call once during single-threaded
/// bring-up.
pub unsafe fn install_trap_vector() {
    extern "C" {
        fn cibos_riscv_trap();
    }
    // stvec: Direct mode (low 2 bits = 0), base = handler address.
    let base = cibos_riscv_trap as *const () as usize;
    asm!("csrw stvec, {0}", in(reg) base, options(nostack, preserves_flags));
}

/// Reporter called by the S-mode trap stub (see `vectors_riscv64.s`). `scause`
/// is the trap cause (top bit = interrupt vs exception), `sepc` the faulting PC,
/// `stval` the trap value (e.g. faulting address). Prints a diagnostic line; the
/// stub then halts. `#[no_mangle]` so the asm can `call` it.
#[no_mangle]
pub extern "C" fn cibos_riscv_trap_report(scause: usize, sepc: usize, stval: usize) {
    use core::fmt::Write;
    let is_interrupt = (scause >> (usize::BITS - 1)) & 1 == 1;
    let code = scause & !(1usize << (usize::BITS - 1));
    let mut console = crate::boot::Console;
    let _ = writeln!(
        console,
        "CIBOS kernel: [RISC-V TRAP] {} cause={} sepc={:#x} stval={:#x} — halting",
        if is_interrupt { "interrupt" } else { "exception" },
        code,
        sepc,
        stval
    );
}
