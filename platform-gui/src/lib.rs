//! # GUI platform
//!
//! A character-cell display platform — the CIBOS equivalent of a windowed GUI,
//! at text granularity. Apps draw into a [`Surface`] (a grid of [`Cell`]s with
//! colors) and react to [`InputEvent`]s. This is deliberately a *virtual*
//! display: the [`GuiRunner`] drives an app with scripted events and hands back
//! the rendered surface, so the whole UI is inspectable and testable without a
//! real framebuffer. A hardware display driver renders the same [`Surface`] to
//! pixels later.
//!
//! A [`GuiApp`] implements `render` (paint current state) and `handle` (update
//! state from one event, returning whether to continue).

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use cibos_input::InputEvent;

pub use cibos_input;

/// A display color (a small fixed palette; `Default` means the terminal/display
/// default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// The display's default color.
    #[default]
    Default,
    /// Black.
    Black,
    /// Red.
    Red,
    /// Green.
    Green,
    /// Yellow.
    Yellow,
    /// Blue.
    Blue,
    /// Magenta.
    Magenta,
    /// Cyan.
    Cyan,
    /// White.
    White,
}

/// One display cell: a character with foreground/background colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The character shown.
    pub ch: char,
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
        }
    }
}

/// A grid of cells — the drawable surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
}

impl Surface {
    /// A new blank surface of the given size (at least 1x1).
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Surface {
            width,
            height,
            cells: vec![Cell::default(); width as usize * height as usize],
        }
    }

    /// Surface width in cells.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Surface height in cells.
    #[must_use]
    pub fn height(&self) -> u16 {
        self.height
    }

    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.width && y < self.height {
            Some(y as usize * self.width as usize + x as usize)
        } else {
            None
        }
    }

    /// Reset every cell to blank.
    pub fn clear(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
    }

    /// Set a character at `(x, y)` with default colors. Out-of-bounds is a
    /// no-op.
    pub fn put(&mut self, x: u16, y: u16, ch: char) {
        if let Some(i) = self.index(x, y) {
            self.cells[i] = Cell {
                ch,
                ..Cell::default()
            };
        }
    }

    /// Set a full cell at `(x, y)`. Out-of-bounds is a no-op.
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if let Some(i) = self.index(x, y) {
            self.cells[i] = cell;
        }
    }

    /// Write a string starting at `(x, y)`, clipped to the row. Returns the
    /// column after the last written character.
    pub fn write_str(&mut self, x: u16, y: u16, s: &str) -> u16 {
        let max = self.width.saturating_sub(x) as usize;
        let mut next = x;
        for (i, ch) in s.chars().take(max).enumerate() {
            let col = x + i as u16;
            self.put(col, y, ch);
            next = col + 1;
        }
        next
    }

    /// Write a string with explicit colors.
    pub fn write_colored(&mut self, x: u16, y: u16, s: &str, fg: Color, bg: Color) {
        let max = self.width.saturating_sub(x) as usize;
        for (i, ch) in s.chars().take(max).enumerate() {
            self.set(x + i as u16, y, Cell { ch, fg, bg });
        }
    }

    /// The cell at `(x, y)`, if in bounds.
    #[must_use]
    pub fn get(&self, x: u16, y: u16) -> Option<Cell> {
        self.index(x, y).map(|i| self.cells[i])
    }

    /// The text of row `y` (characters only), trailing blanks trimmed.
    #[must_use]
    pub fn row_text(&self, y: u16) -> String {
        if y >= self.height {
            return String::new();
        }
        let start = y as usize * self.width as usize;
        let row: String = self.cells[start..start + self.width as usize]
            .iter()
            .map(|c| c.ch)
            .collect();
        row.trim_end().to_string()
    }

    /// The whole surface as text, rows joined by newlines (trailing blanks
    /// trimmed per row).
    #[must_use]
    pub fn to_text(&self) -> String {
        (0..self.height)
            .map(|y| self.row_text(y))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Whether the app wants to keep running after handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Continue running.
    Continue,
    /// Exit the app.
    Exit,
}

/// An event-driven GUI application.
pub trait GuiApp {
    /// The app's name.
    fn name(&self) -> &str;

    /// Handle one input event, updating internal state. Return [`Flow::Exit`]
    /// to stop.
    fn handle(&mut self, event: InputEvent) -> Flow;

    /// Paint the current state onto the surface. The surface is cleared by the
    /// runner before each render.
    fn render(&self, surface: &mut Surface);
}

/// Drives a [`GuiApp`] over a virtual display.
pub struct GuiRunner {
    surface: Surface,
}

impl GuiRunner {
    /// Create a runner with a surface of the given size.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        GuiRunner {
            surface: Surface::new(width, height),
        }
    }

    fn render(&mut self, app: &dyn GuiApp) {
        self.surface.clear();
        app.render(&mut self.surface);
    }

    /// Render the initial frame, then deliver each event (rendering after each)
    /// until the app exits or events run out. Returns the final surface.
    pub fn run<I>(&mut self, app: &mut dyn GuiApp, events: I) -> Surface
    where
        I: IntoIterator<Item = InputEvent>,
    {
        self.render(app);
        for event in events {
            let flow = app.handle(event);
            self.render(app);
            if flow == Flow::Exit {
                break;
            }
        }
        self.surface.clone()
    }

    /// Like [`run`](Self::run) but returns a snapshot of the surface after every
    /// frame (initial render first, then one per handled event).
    pub fn run_capturing<I>(&mut self, app: &mut dyn GuiApp, events: I) -> Vec<Surface>
    where
        I: IntoIterator<Item = InputEvent>,
    {
        let mut frames = Vec::new();
        self.render(app);
        frames.push(self.surface.clone());
        for event in events {
            if app.handle(event) == Flow::Exit {
                break;
            }
            self.render(app);
            frames.push(self.surface.clone());
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_draw_and_read() {
        let mut s = Surface::new(10, 3);
        s.write_str(0, 0, "hello");
        s.put(0, 1, 'X');
        assert_eq!(s.row_text(0), "hello");
        assert_eq!(s.row_text(1), "X");
        assert_eq!(s.get(0, 0).unwrap().ch, 'h');
        // Out of bounds is safe.
        s.put(99, 99, 'Z');
        assert_eq!(s.to_text(), "hello\nX\n");
    }

    #[test]
    fn write_str_clips_to_width() {
        let mut s = Surface::new(3, 1);
        let end = s.write_str(0, 0, "abcdef");
        assert_eq!(end, 3);
        assert_eq!(s.row_text(0), "abc");
    }

    #[test]
    fn colored_write() {
        let mut s = Surface::new(5, 1);
        s.write_colored(0, 0, "hi", Color::Red, Color::Black);
        assert_eq!(s.get(0, 0).unwrap().fg, Color::Red);
        assert_eq!(s.get(1, 0).unwrap().bg, Color::Black);
    }
}
