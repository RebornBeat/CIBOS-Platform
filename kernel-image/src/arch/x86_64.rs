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

/// Remap the legacy 8259 PIC pair so hardware IRQs are delivered to interrupt
/// vectors `0x20..=0x2F` instead of the BIOS-default `0x08..=0x0F` (which
/// collide with CPU exception vectors), then mask every line.
///
/// This is the correct fix for the IRQ0↔#DF collision that [`mask_pic`] worked
/// around: after remapping, individual lines can be unmasked safely (their
/// vectors no longer alias exceptions). The standard ICW1..ICW4 init sequence is
/// followed; after it, OCW1 masks all lines until the kernel unmasks the ones it
/// services.
pub fn remap_pic() {
    const PIC1_CMD: u16 = 0x20;
    const PIC1_DATA: u16 = 0x21;
    const PIC2_CMD: u16 = 0xA0;
    const PIC2_DATA: u16 = 0xA1;
    const ICW1_INIT: u8 = 0x11; // init + ICW4 present
    const ICW4_8086: u8 = 0x01; // 8086/88 mode
    const MASTER_OFFSET: u8 = 0x20; // IRQ0..7 -> 0x20..0x27
    const SLAVE_OFFSET: u8 = 0x28; // IRQ8..15 -> 0x28..0x2F

    // SAFETY: standard 8259 programming on fixed I/O ports during bring-up.
    unsafe {
        // ICW1: begin init (both PICs).
        outb(PIC1_CMD, ICW1_INIT);
        outb(PIC2_CMD, ICW1_INIT);
        // ICW2: vector offsets.
        outb(PIC1_DATA, MASTER_OFFSET);
        outb(PIC2_DATA, SLAVE_OFFSET);
        // ICW3: tell master the slave is on IRQ2 (bit 2); tell slave its id (2).
        outb(PIC1_DATA, 0x04);
        outb(PIC2_DATA, 0x02);
        // ICW4: 8086 mode.
        outb(PIC1_DATA, ICW4_8086);
        outb(PIC2_DATA, ICW4_8086);
        // OCW1: mask all lines for now.
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}

/// Unmask a single IRQ line on the 8259 PIC pair (read-modify-write of the
/// interrupt mask register). `line` 0..=7 is on the master (port 0x21), 8..=15
/// on the slave (port 0xA1). Call after [`remap_pic`] and after the IDT has a
/// handler for the corresponding vector (`0x20 + line`). A masked bit is 1, so
/// unmasking clears the bit.
pub fn unmask_irq(line: u8) {
    // SAFETY: PIC data-port read/modify/write during bring-up.
    unsafe {
        if line < 8 {
            let cur = inb(0x21);
            outb(0x21, cur & !(1u8 << line));
        } else {
            let cur = inb(0xA1);
            outb(0xA1, cur & !(1u8 << (line - 8)));
        }
    }
}

/// Program PIT channel 0 (the system timer) as a periodic rate generator at
/// `hz` ticks per second, so IRQ0 fires `hz` times a second. After
/// [`remap_pic`], IRQ0 is delivered to vector `0x20`; the caller must install a
/// handler there and `unmask_irq(0)` to receive ticks.
///
/// The PIT input clock is ~1.193182 MHz; the 16-bit divisor is
/// `1193182 / hz` (clamped to the representable range). Mode 2 (rate generator)
/// gives an even periodic tick suitable for a scheduler/timeout source.
pub fn init_pit(hz: u32) {
    const PIT_CH0: u16 = 0x40;
    const PIT_CMD: u16 = 0x43;
    const PIT_INPUT_HZ: u32 = 1_193_182;
    // Command: channel 0, access lo/hi byte, mode 2 (rate generator), binary.
    const CMD_CH0_MODE2: u8 = 0b0011_0100;

    let hz = hz.clamp(19, PIT_INPUT_HZ); // <19 Hz overflows the 16-bit divisor
    let divisor = (PIT_INPUT_HZ / hz) as u16;
    // SAFETY: standard PIT programming on fixed I/O ports during bring-up.
    unsafe {
        outb(PIT_CMD, CMD_CH0_MODE2);
        outb(PIT_CH0, (divisor & 0xFF) as u8); // low byte
        outb(PIT_CH0, (divisor >> 8) as u8); // high byte
    }
}

/// Signal end-of-interrupt to the PIC for a vector in the remapped range
/// `0x20..=0x2F`. For lines on the slave (vector >= 0x28) both PICs must be
/// acknowledged.
///
/// # Safety
///
/// Call exactly once at the end of servicing a hardware IRQ.
pub unsafe fn pic_eoi(vector: u8) {
    const PIC1_CMD: u16 = 0x20;
    const PIC2_CMD: u16 = 0xA0;
    const EOI: u8 = 0x20;
    if vector >= 0x28 {
        outb(PIC2_CMD, EOI);
    }
    outb(PIC1_CMD, EOI);
}

/// Read one byte from the PS/2 keyboard controller data port (0x60).
///
/// # Safety
///
/// Should be read in response to a keyboard IRQ (data is ready); reading at
/// other times returns whatever the controller last latched.
pub unsafe fn read_keyboard_data() -> u8 {
    inb(0x60)
}

/// Halt the processor permanently.
pub fn halt() -> ! {
    loop {
        unsafe {
            asm!("cli; hlt", options(nomem, nostack));
        }
    }
}
