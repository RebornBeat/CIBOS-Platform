//! AArch64 serial (PL011 on QEMU `virt`) and halt for the kernel image.

use core::arch::asm;

/// PL011 UART base on the QEMU `virt` machine. FLAG: confirm against the board's
/// device tree for non-QEMU hardware.
const UART0: usize = 0x0900_0000;
const UARTDR: usize = 0x00;
const UARTFR: usize = 0x18;
const UARTFR_TXFF: u8 = 1 << 5;

unsafe fn mmio_write_u8(addr: usize, val: u8) {
    core::ptr::write_volatile(addr as *mut u8, val);
}

unsafe fn mmio_read_u8(addr: usize) -> u8 {
    core::ptr::read_volatile(addr as *const u8)
}

/// QEMU's PL011 is usable without initialization.
pub fn init_serial() {}

/// Write one byte to the UART, waiting for transmit FIFO space.
pub fn putc(b: u8) {
    unsafe {
        while mmio_read_u8(UART0 + UARTFR) & UARTFR_TXFF != 0 {}
        mmio_write_u8(UART0 + UARTDR, b);
    }
}

/// Halt the processor permanently.
pub fn halt() -> ! {
    loop {
        unsafe {
            asm!("wfe", options(nomem, nostack));
        }
    }
}
