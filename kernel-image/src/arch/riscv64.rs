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
