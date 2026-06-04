//! # Notepad
//!
//! A minimal single-line text field for the GUI platform, demonstrating
//! keyboard and pointer input rendered to a cell [`Surface`].
//!
//! Keys: printable characters insert at the cursor; `Backspace`/`Delete` remove;
//! `Left`/`Right`/`Home`/`End` move the cursor; `Enter` or `Escape` exit. A
//! primary pointer tap on the text row moves the cursor to that column.
//!
//! The display is four rows: a title bar, the text, a caret row marking the
//! cursor column, and a hint line.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use platform_gui::{Color, Flow, GuiApp, Surface};
use cibos_input::{InputEvent, Key, PointerAction};

const TEXT_ROW: u16 = 1;
const CARET_ROW: u16 = 2;

/// The notepad app.
#[derive(Default)]
pub struct Notepad {
    chars: Vec<char>,
    cursor: usize,
}

impl Notepad {
    /// Create an empty notepad.
    #[must_use]
    pub fn new() -> Self {
        Notepad::default()
    }

    /// The current text.
    #[must_use]
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// The current cursor position (character index).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }
}

impl GuiApp for Notepad {
    fn name(&self) -> &str {
        "notepad"
    }

    fn handle(&mut self, event: InputEvent) -> Flow {
        match event {
            InputEvent::Key(k) => match k.key {
                Key::Char(c) => self.insert(c),
                Key::Backspace => self.backspace(),
                Key::Delete => self.delete(),
                Key::Left => self.cursor = self.cursor.saturating_sub(1),
                Key::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
                Key::Home => self.cursor = 0,
                Key::End => self.cursor = self.chars.len(),
                Key::Enter | Key::Escape => return Flow::Exit,
                Key::Tab | Key::Up | Key::Down => {}
            },
            InputEvent::Pointer(p) => {
                if p.action == PointerAction::Press && p.y == TEXT_ROW {
                    self.cursor = (p.x as usize).min(self.chars.len());
                }
            }
        }
        Flow::Continue
    }

    fn render(&self, surface: &mut Surface) {
        surface.write_colored(0, 0, "CIBOS Notepad", Color::White, Color::Blue);
        surface.write_str(0, TEXT_ROW, &self.text());
        // Caret row: a '^' under the cursor column.
        surface.put(self.cursor as u16, CARET_ROW, '^');
        surface.write_str(0, 3, "Enter/Esc: quit  arrows: move");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_gui::GuiRunner;
    use cibos_input::{KeyEvent, Pointer};

    fn key(k: Key) -> InputEvent {
        InputEvent::Key(KeyEvent::new(k))
    }
    fn ch(c: char) -> InputEvent {
        InputEvent::Key(KeyEvent::ch(c))
    }

    #[test]
    fn typing_and_backspace() {
        let mut app = Notepad::new();
        let mut runner = GuiRunner::new(40, 4);
        let surface = runner.run(
            &mut app,
            [ch('h'), ch('i'), key(Key::Backspace), ch('y')],
        );
        assert_eq!(app.text(), "hy");
        assert_eq!(surface.row_text(TEXT_ROW), "hy");
    }

    #[test]
    fn cursor_movement_and_insert() {
        let mut app = Notepad::new();
        let mut runner = GuiRunner::new(40, 4);
        runner.run(
            &mut app,
            [ch('h'), ch('i'), key(Key::Left), ch('X')],
        );
        // h i, cursor at 2; Left -> 1; insert X at 1 -> "hXi"
        assert_eq!(app.text(), "hXi");
        assert_eq!(app.cursor(), 2);
    }

    #[test]
    fn home_end_delete() {
        let mut app = Notepad::new();
        let mut runner = GuiRunner::new(40, 4);
        runner.run(
            &mut app,
            [ch('a'), ch('b'), ch('c'), key(Key::Home), key(Key::Delete)],
        );
        assert_eq!(app.text(), "bc"); // deleted 'a' at start
        assert_eq!(app.cursor(), 0);
    }

    #[test]
    fn pointer_tap_moves_cursor() {
        let mut app = Notepad::new();
        let mut runner = GuiRunner::new(40, 4);
        runner.run(
            &mut app,
            [
                ch('h'),
                ch('e'),
                ch('l'),
                ch('l'),
                ch('o'),
                InputEvent::Pointer(Pointer::tap(1, TEXT_ROW)),
                ch('X'),
            ],
        );
        // tap at col 1 -> cursor 1; insert X -> "hXello"
        assert_eq!(app.text(), "hXello");
    }

    #[test]
    fn enter_exits_before_consuming_rest() {
        let mut app = Notepad::new();
        let mut runner = GuiRunner::new(40, 4);
        // 'a', Enter (exit), 'b' should never be processed.
        runner.run(&mut app, [ch('a'), key(Key::Enter), ch('b')]);
        assert_eq!(app.text(), "a");
    }

    #[test]
    fn caret_row_marks_cursor() {
        let mut app = Notepad::new();
        let mut runner = GuiRunner::new(40, 4);
        let surface = runner.run(&mut app, [ch('a'), ch('b')]);
        // cursor at col 2 -> caret at col 2 on the caret row
        assert_eq!(surface.get(2, CARET_ROW).unwrap().ch, '^');
    }
}
