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

/// Install the exception vector table: point VBAR_EL1 at `cibos_vectors` so any
/// synchronous exception, IRQ, FIQ, or SError is reported (and, during bring-up,
/// halts) instead of vanishing to a garbage address. The x86 equivalent is the
/// IDT install + fault reporter. Call once, early in `kernel_entry`, before any
/// code that might fault.
///
/// # Safety
/// Writes the EL1 vector base register; call once during single-threaded
/// bring-up.
pub unsafe fn install_exception_vectors() {
    extern "C" {
        static cibos_vectors: u8;
    }
    let base = core::ptr::addr_of!(cibos_vectors) as u64;
    asm!("msr vbar_el1, {0}", "isb", in(reg) base, options(nostack, preserves_flags));
}

/// Reporter called by every exception vector entry (see `vectors_aarch64.s`).
/// `kind` is the vector index (0=Sync/SP0 .. 15=AArch32 SError); `esr` is
/// ESR_EL1 (exception syndrome), `elr` the faulting PC (ELR_EL1), `far` the
/// faulting address (FAR_EL1). Prints a diagnostic line; the asm stub then halts.
/// `#[no_mangle]` so the asm can `call` it.
#[no_mangle]
pub extern "C" fn cibos_aarch64_exception(kind: u64, esr: u64, elr: u64, far: u64) {
    use core::fmt::Write;
    // ESR_EL1 exception class is bits [31:26].
    let ec = (esr >> 26) & 0x3F;
    let mut console = crate::boot::Console;
    let _ = writeln!(
        console,
        "CIBOS kernel: [AArch64 EXCEPTION] kind={kind} EC={ec:#04x} ESR={esr:#x} ELR={elr:#x} FAR={far:#x} — halting"
    );
}
