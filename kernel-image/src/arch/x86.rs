//! 32-bit x86 (i686) serial console and halt for the kernel image.
//!
//! The legacy x86 path targets COM1 (0x3F8) via port I/O — the same UART the
//! 64-bit backend drives — using the identical `in`/`out` instructions, which
//! are available in 32-bit protected mode. This is serial-only for now (the
//! bring-up/liveness capture target); an on-screen VGA text console for i686 can
//! be added the same way the x86_64 backend layers `vga` on top, as a later
//! step. Keeping this minimal mirrors the aarch64/riscv64 backends.

use core::arch::asm;

/// COM1 base I/O port (the standard PC serial port).
const COM1: u16 = 0x3F8;

/// Write one byte to an I/O port.
unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

/// Read one byte from an I/O port.
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

/// Initialize COM1: 115200 8N1, FIFO on (identical sequence to the 64-bit path).
pub fn init_serial() {
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // enable DLAB
        outb(COM1, 0x03); //     divisor low (115200)
        outb(COM1 + 1, 0x00); // divisor high
        outb(COM1 + 3, 0x03); // 8 bits, no parity, one stop bit
        outb(COM1 + 2, 0xC7); // enable + clear FIFO, 14-byte threshold
        outb(COM1 + 4, 0x0B); // RTS/DSR set
    }
}

/// Write one byte to COM1, waiting for the transmit-holding register to empty.
pub fn putc(b: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {}
        outb(COM1, b);
    }
}

/// Halt the processor permanently (interrupts disabled).
pub fn halt() -> ! {
    loop {
        unsafe {
            asm!("cli; hlt", options(nomem, nostack));
        }
    }
}
