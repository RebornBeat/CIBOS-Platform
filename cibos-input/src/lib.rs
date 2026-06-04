//! # Input event model
//!
//! A platform-independent input vocabulary shared by every CIBOS platform that
//! has a human at it — the GUI platform today, the mobile (touch) platform
//! later. Hardware drivers (a PS/2 or USB keyboard, a touch panel) translate
//! their raw reports into these events; apps only ever see [`InputEvent`].
//!
//! Keyboard events carry a [`Key`] and [`Modifiers`]. Pointer events cover both
//! mouse and touch: a touch is simply a [`Pointer`] with the primary button.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![no_std]

/// A key on the keyboard. Printable keys carry their character; the rest are
/// named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character (already case-resolved by the driver).
    Char(char),
    /// Return/Enter.
    Enter,
    /// Backspace (delete left).
    Backspace,
    /// Forward delete.
    Delete,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
}

/// Modifier-key state at the time of a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Shift held.
    pub shift: bool,
    /// Control held.
    pub ctrl: bool,
    /// Alt held.
    pub alt: bool,
}

impl Modifiers {
    /// No modifiers.
    #[must_use]
    pub const fn none() -> Self {
        Modifiers {
            shift: false,
            ctrl: false,
            alt: false,
        }
    }

    /// Control held alone.
    #[must_use]
    pub const fn ctrl() -> Self {
        Modifiers {
            shift: false,
            ctrl: true,
            alt: false,
        }
    }
}

/// A keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// Which key.
    pub key: Key,
    /// Modifier state.
    pub mods: Modifiers,
}

impl KeyEvent {
    /// A key event with no modifiers.
    #[must_use]
    pub const fn new(key: Key) -> Self {
        KeyEvent {
            key,
            mods: Modifiers::none(),
        }
    }

    /// A printable-character key event with no modifiers.
    #[must_use]
    pub const fn ch(c: char) -> Self {
        KeyEvent::new(Key::Char(c))
    }
}

/// Pointer buttons (touch maps to `Primary`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Left mouse button / single touch.
    Primary,
    /// Right mouse button / long-press.
    Secondary,
    /// Middle button.
    Middle,
}

/// What a pointer is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerAction {
    /// Button/touch went down.
    Press,
    /// Button/touch released.
    Release,
    /// Moved (button state unchanged).
    Move,
}

/// A pointer (mouse or touch) event, in display cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pointer {
    /// Column.
    pub x: u16,
    /// Row.
    pub y: u16,
    /// Press / release / move.
    pub action: PointerAction,
    /// Which button (primary for touch).
    pub button: Button,
}

impl Pointer {
    /// A primary-button press at `(x, y)` — the common "click" / "tap".
    #[must_use]
    pub const fn tap(x: u16, y: u16) -> Self {
        Pointer {
            x,
            y,
            action: PointerAction::Press,
            button: Button::Primary,
        }
    }
}

/// Any input event delivered to an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// A keyboard event.
    Key(KeyEvent),
    /// A pointer/touch event.
    Pointer(Pointer),
}

impl From<KeyEvent> for InputEvent {
    fn from(k: KeyEvent) -> Self {
        InputEvent::Key(k)
    }
}

impl From<Pointer> for InputEvent {
    fn from(p: Pointer) -> Self {
        InputEvent::Pointer(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors() {
        assert_eq!(KeyEvent::ch('a').key, Key::Char('a'));
        assert_eq!(KeyEvent::new(Key::Enter).mods, Modifiers::none());
        assert!(Modifiers::ctrl().ctrl);
        let p = Pointer::tap(3, 4);
        assert_eq!((p.x, p.y, p.action, p.button), (3, 4, PointerAction::Press, Button::Primary));
    }

    #[test]
    fn into_input_event() {
        let e: InputEvent = KeyEvent::ch('z').into();
        assert!(matches!(e, InputEvent::Key(_)));
        let e: InputEvent = Pointer::tap(1, 1).into();
        assert!(matches!(e, InputEvent::Pointer(_)));
    }
}
