//! On-screen GUI runner: renders a `platform-gui` [`Surface`] to the VGA text
//! console and drives a [`GuiApp`] with real keyboard input.
//!
//! The GUI is character-cell granularity by design (a `Surface` is a grid of
//! `Cell { ch, fg, bg }`), and VGA text mode is exactly a character-cell display
//! (80×25, 16 colors), so "rendering" is a direct **blit**: each surface cell
//! becomes one VGA `u16` (ASCII byte + attribute). This is the hardware display
//! driver the platform-gui docs anticipate — the same `Surface` the host runner
//! renders virtually is painted to the screen here.
//!
//! The loop mirrors the host `GuiRunner`: clear → `render` → blit → wait for an
//! input event → `handle`, until the app returns [`Flow::Exit`]. Input is the
//! existing PS/2 keyboard (`keyboard::poll_key`), wrapped as an
//! [`InputEvent::Key`]. Only built for x86 BIOS targets (where VGA text exists).

#![cfg(target_arch = "x86_64")]

use cibos_input::InputEvent;
use platform_gui::{Color, Flow, GuiApp, Surface};

use crate::arch::vga;

/// Map a GUI [`Color`] to a VGA 4-bit color code.
///
/// VGA text colors: 0 black, 1 blue, 2 green, 3 cyan, 4 red, 5 magenta, 6 brown,
/// 7 light grey, … 15 white. We map the 8 named GUI colors to their VGA
/// equivalents; `Default` uses light grey for foreground / black for background
/// (handled by the caller, which knows whether it is composing fg or bg).
fn vga_color(c: Color, default_code: u8) -> u8 {
    match c {
        Color::Default => default_code,
        Color::Black => 0,
        Color::Blue => 1,
        Color::Green => 2,
        Color::Cyan => 3,
        Color::Red => 4,
        Color::Magenta => 5,
        Color::Yellow => 14, // bright brown == yellow
        Color::White => 15,
    }
}

/// Compose a VGA attribute byte from a cell's foreground/background colors.
fn attr(fg: Color, bg: Color) -> u8 {
    // Default fg = light grey (7), default bg = black (0).
    let fg = vga_color(fg, 7) & 0x0F;
    let bg = vga_color(bg, 0) & 0x0F;
    (bg << 4) | fg
}

/// Blit a whole [`Surface`] onto the VGA text buffer.
fn blit(surface: &Surface) {
    let w = surface.width();
    let h = surface.height();
    for y in 0..h {
        for x in 0..w {
            if let Some(cell) = surface.get(x, y) {
                // VGA cells are single-byte code points; map non-ASCII to '?'.
                let ch = if (cell.ch as u32) < 0x80 {
                    cell.ch as u8
                } else {
                    b'?'
                };
                vga::put_cell(x as usize, y as usize, ch, attr(cell.fg, cell.bg));
            }
        }
    }
}

/// Run a [`GuiApp`] on screen until it exits.
///
/// Creates a surface sized to the VGA console, then loops: render the app into
/// the surface, blit it, block for the next key, and feed it to the app. Returns
/// when the app returns [`Flow::Exit`].
pub fn run_gui_app(app: &mut dyn GuiApp) {
    let mut surface = Surface::new(vga::width() as u16, vga::height() as u16);

    // Initial paint.
    surface.clear();
    app.render(&mut surface);
    blit(&surface);

    loop {
        // Block until a key is available (the keyboard fills its queue from the
        // IRQ1 handler). Yield to the CPU between polls.
        let ev = loop {
            if let Some(k) = crate::keyboard::poll_key() {
                break k;
            }
            core::hint::spin_loop();
        };

        match app.handle(InputEvent::Key(ev)) {
            Flow::Exit => break,
            Flow::Continue => {}
        }

        surface.clear();
        app.render(&mut surface);
        blit(&surface);
    }
}
