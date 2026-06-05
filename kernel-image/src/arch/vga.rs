//! VGA text-mode console for the kernel image (x86 PC / BIOS).
//!
//! On a BIOS boot the display starts in VGA text mode 3 (80×25, 16 colors) with
//! a character buffer memory-mapped at physical `0xB8000`. Each cell is two
//! bytes: an ASCII code point and an attribute (foreground in the low nibble,
//! background in the high nibble). No mode-setting is required — the firmware
//! left the card in text mode — so this is the simplest real on-screen output
//! a freshly booted kernel can produce, complementing the serial console.
//!
//! This is a kernel-internal driver, deliberately minimal: a moving cursor,
//! newline handling, and hardware scroll when the cursor passes the last row.
//! It is `no_std` and lock-free on its own; serialization with the rest of the
//! console output is provided by the caller (the kernel writes the console
//! behind its existing lock). It is compiled only for x86 targets.

use core::arch::asm;

/// VGA text framebuffer physical address. Identity-mapped by both the multiboot
/// path (CIBIOS maps low memory) and the from-scratch bootloader path (Stage 2
/// identity-maps 0..4 GiB), so this address is directly addressable at boot.
const VGA_BUFFER: usize = 0xB8000;
/// Text-mode dimensions for mode 3.
const WIDTH: usize = 80;
const HEIGHT: usize = 25;

/// VGA text attribute. Light grey on black is the conventional console default.
const DEFAULT_ATTR: u8 = 0x07;

/// CRT controller I/O ports, used to move the hardware cursor.
const CRTC_INDEX: u16 = 0x3D4;
const CRTC_DATA: u16 = 0x3D5;

/// Current cursor column and row. Single-threaded at the point of use (the
/// kernel console holds a lock around all output), so plain statics suffice.
static mut COL: usize = 0;
static mut ROW: usize = 0;

unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

/// Pointer to cell `(col, row)` in the VGA text buffer.
fn cell_ptr(col: usize, row: usize) -> *mut u16 {
    (VGA_BUFFER as *mut u16).wrapping_add(row * WIDTH + col)
}

/// Write a space-filled, default-attribute blank to every cell and home the
/// cursor. Called once at console init so the screen does not show BIOS
/// leftovers.
pub fn init() {
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            // SAFETY: cell_ptr stays within the 80*25 buffer at 0xB8000, which
            // is identity-mapped and writable in text mode.
            unsafe {
                cell_ptr(col, row).write_volatile(blank());
            }
        }
    }
    // SAFETY: single-threaded console init; statics are not aliased.
    unsafe {
        COL = 0;
        ROW = 0;
    }
    update_hw_cursor(0, 0);
}

/// A blank cell: space with the default attribute.
fn blank() -> u16 {
    (u16::from(DEFAULT_ATTR) << 8) | u16::from(b' ')
}

/// Compose a cell from a byte and the default attribute.
fn cell(byte: u8) -> u16 {
    (u16::from(DEFAULT_ATTR) << 8) | u16::from(byte)
}

/// Write one byte to the VGA console, advancing the cursor and scrolling as
/// needed. Handles `\n` (newline), `\r` (carriage return), and `\t` (tab to the
/// next 8-column stop). Other control bytes are rendered as spaces to avoid
/// emitting glyphs for them.
pub fn putc(byte: u8) {
    // SAFETY: the console is written behind the kernel's lock, so the cursor
    // statics are not concurrently accessed; all writes stay within the buffer.
    unsafe {
        match byte {
            b'\n' => newline(),
            b'\r' => COL = 0,
            b'\t' => {
                let next = (COL + 8) & !7;
                while COL < next && COL < WIDTH {
                    cell_ptr(COL, ROW).write_volatile(blank());
                    COL += 1;
                }
                if COL >= WIDTH {
                    newline();
                }
            }
            0x20..=0x7E => {
                cell_ptr(COL, ROW).write_volatile(cell(byte));
                COL += 1;
                if COL >= WIDTH {
                    newline();
                }
            }
            // Non-printable: show a space so layout is preserved without glyphs.
            _ => {
                cell_ptr(COL, ROW).write_volatile(blank());
                COL += 1;
                if COL >= WIDTH {
                    newline();
                }
            }
        }
        update_hw_cursor(COL, ROW);
    }
}

/// Advance to the start of the next line, scrolling if at the bottom.
///
/// # Safety
///
/// Caller holds the console lock; the cursor statics are not aliased.
unsafe fn newline() {
    COL = 0;
    if ROW + 1 >= HEIGHT {
        scroll();
    } else {
        ROW += 1;
    }
}

/// Scroll the screen up by one row, blanking the new bottom row. Leaves the
/// cursor row at the last line.
///
/// # Safety
///
/// Caller holds the console lock; reads and writes stay within the buffer.
unsafe fn scroll() {
    for row in 1..HEIGHT {
        for col in 0..WIDTH {
            let v = cell_ptr(col, row).read_volatile();
            cell_ptr(col, row - 1).write_volatile(v);
        }
    }
    for col in 0..WIDTH {
        cell_ptr(col, HEIGHT - 1).write_volatile(blank());
    }
    ROW = HEIGHT - 1;
}

/// Move the hardware cursor to `(col, row)` via the CRT controller, so the
/// blinking cursor tracks where the next character lands.
fn update_hw_cursor(col: usize, row: usize) {
    let pos = (row * WIDTH + col) as u16;
    // SAFETY: standard VGA CRTC register programming on fixed I/O ports.
    unsafe {
        outb(CRTC_INDEX, 0x0F);
        outb(CRTC_DATA, (pos & 0xFF) as u8);
        outb(CRTC_INDEX, 0x0E);
        outb(CRTC_DATA, ((pos >> 8) & 0xFF) as u8);
    }
}
