//! x86_64 serial (COM1) and halt for the kernel image.

use core::arch::asm;

const COM1: u16 = 0x3F8;

unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

/// Initialize COM1 to 38400 8N1, then clear and home the VGA text console.
/// Named `init_serial` for the stable arch interface; on x86 it brings up both
/// the serial line and the on-screen VGA text console so a booted kernel is
/// visible on a monitor as well as a serial capture.
pub fn init_serial() {
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x03);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x0B);
    }
    super::vga::init();
}

/// Write one byte to both the serial console (COM1) and the VGA text console.
/// Serial first (it is the primary capture target during bring-up), then the
/// on-screen console.
pub fn putc(b: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {}
        outb(COM1, b);
    }
    super::vga::putc(b);
}

/// Mask every IRQ line on the legacy 8259 PIC pair.
///
/// At BIOS defaults the master PIC delivers IRQ0 (the periodic timer) to CPU
/// interrupt vector 0x08 — which collides with the CPU's #DF exception vector.
/// The kernel runs with interrupts disabled, so this is harmless until the first
/// time `IF` is set (e.g. entering ring 3 via `iretq` with RFLAGS.IF=1): the
/// timer then fires into vector 0x08 and looks exactly like a double fault.
///
/// Until the kernel has a real timer/APIC driver (which would remap and handle
/// these), the correct thing is to mask all PIC lines so no spurious legacy IRQ
/// is delivered. Writing 0xFF to each PIC's data port masks all eight of its
/// lines.
pub fn mask_pic() {
    const PIC1_DATA: u16 = 0x21;
    const PIC2_DATA: u16 = 0xA1;
    // SAFETY: standard 8259 PIC programming on fixed I/O ports.
    unsafe {
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}

/// Halt the processor permanently.
pub fn halt() -> ! {
    loop {
        unsafe {
            asm!("cli; hlt", options(nomem, nostack));
        }
    }
}
