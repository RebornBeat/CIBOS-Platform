//! # PS/2 Scancode Set 1 decoder
//!
//! Translates the raw byte stream from a PS/2 keyboard controller (the legacy
//! "scancode set 1" that every PC keyboard controller still produces after BIOS
//! init) into [`KeyEvent`]s. The decoder is a small state machine: it tracks the
//! shift / control / alt modifiers across make and break codes, handles the
//! `0xE0` extended-key prefix (arrows, navigation cluster), and resolves
//! printable keys to characters with the current shift state applied.
//!
//! It is deliberately hardware-free and `no_std`: the kernel's keyboard IRQ
//! handler reads one byte from the controller's data port and feeds it here; on
//! a host it is driven by a byte slice in unit tests. Only key *presses* (make
//! codes) of producing keys yield a [`KeyEvent`]; break codes update modifier
//! state and otherwise produce nothing.
//!
//! Scancode set 1 reference (make code; break code = make | 0x80):
//! `0x01` Esc, `0x0E` Backspace, `0x0F` Tab, `0x1C` Enter, `0x1D` LCtrl,
//! `0x2A` LShift, `0x36` RShift, `0x38` LAlt, plus the alphanumeric/punctuation
//! block. Extended (`0xE0`-prefixed): `0x48` Up, `0x50` Down, `0x4B` Left,
//! `0x4D` Right, `0x47` Home, `0x4F` End, `0x53` Delete, `0x1D` RCtrl, `0x38`
//! RAlt.

use crate::{Key, KeyEvent, Modifiers};

const BREAK_MASK: u8 = 0x80;
const EXTENDED_PREFIX: u8 = 0xE0;

// Make codes for the modifier keys (set 1).
const SC_LSHIFT: u8 = 0x2A;
const SC_RSHIFT: u8 = 0x36;
const SC_CTRL: u8 = 0x1D; // L; R is 0xE0,0x1D
const SC_ALT: u8 = 0x38; // L; R is 0xE0,0x38

/// A stateful PS/2 set-1 decoder.
#[derive(Debug, Default)]
pub struct ScancodeDecoder {
    shift: bool,
    ctrl: bool,
    alt: bool,
    /// Set when the previous byte was the `0xE0` extended prefix.
    extended: bool,
}

impl ScancodeDecoder {
    /// A fresh decoder with no modifiers held.
    #[must_use]
    pub const fn new() -> Self {
        ScancodeDecoder {
            shift: false,
            ctrl: false,
            alt: false,
            extended: false,
        }
    }

    /// Current modifier state.
    #[must_use]
    pub fn modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.shift,
            ctrl: self.ctrl,
            alt: self.alt,
        }
    }

    /// Feed one raw byte from the keyboard controller. Returns a [`KeyEvent`]
    /// when the byte completes a producing key press; otherwise `None` (prefix
    /// byte, modifier change, or any break code).
    pub fn push(&mut self, byte: u8) -> Option<KeyEvent> {
        if byte == EXTENDED_PREFIX {
            self.extended = true;
            return None;
        }
        let extended = self.extended;
        self.extended = false;

        let is_break = byte & BREAK_MASK != 0;
        let make = byte & !BREAK_MASK;

        // Modifier keys: update state, never emit an event.
        match (extended, make) {
            (_, SC_CTRL) => {
                self.ctrl = !is_break;
                return None;
            }
            (_, SC_ALT) => {
                self.alt = !is_break;
                return None;
            }
            (false, SC_LSHIFT) | (false, SC_RSHIFT) => {
                self.shift = !is_break;
                return None;
            }
            _ => {}
        }

        // Only presses of non-modifier keys produce events.
        if is_break {
            return None;
        }

        let key = if extended {
            decode_extended(make)?
        } else {
            decode_main(make, self.shift)?
        };
        Some(KeyEvent {
            key,
            mods: self.modifiers(),
        })
    }
}

/// Decode an extended (`0xE0`-prefixed) make code to a named key.
fn decode_extended(make: u8) -> Option<Key> {
    Some(match make {
        0x48 => Key::Up,
        0x50 => Key::Down,
        0x4B => Key::Left,
        0x4D => Key::Right,
        0x47 => Key::Home,
        0x4F => Key::End,
        0x53 => Key::Delete,
        0x1C => Key::Enter, // keypad Enter
        _ => return None,
    })
}

/// Decode a main-block make code, applying `shift` for printable characters.
fn decode_main(make: u8, shift: bool) -> Option<Key> {
    // Named keys first.
    match make {
        0x1C => return Some(Key::Enter),
        0x0E => return Some(Key::Backspace),
        0x0F => return Some(Key::Tab),
        0x01 => return Some(Key::Escape),
        0x53 => return Some(Key::Delete),
        _ => {}
    }
    let (lower, upper) = PRINTABLE.iter().find(|(code, _, _)| *code == make).map(
        |(_, l, u)| (*l, *u),
    )?;
    let c = if shift { upper } else { lower };
    Some(Key::Char(c))
}

/// (make code, unshifted char, shifted char) for the printable main block of a
/// US QWERTY layout, scancode set 1.
const PRINTABLE: &[(u8, char, char)] = &[
    // Number row.
    (0x02, '1', '!'),
    (0x03, '2', '@'),
    (0x04, '3', '#'),
    (0x05, '4', '$'),
    (0x06, '5', '%'),
    (0x07, '6', '^'),
    (0x08, '7', '&'),
    (0x09, '8', '*'),
    (0x0A, '9', '('),
    (0x0B, '0', ')'),
    (0x0C, '-', '_'),
    (0x0D, '=', '+'),
    // Top letter row.
    (0x10, 'q', 'Q'),
    (0x11, 'w', 'W'),
    (0x12, 'e', 'E'),
    (0x13, 'r', 'R'),
    (0x14, 't', 'T'),
    (0x15, 'y', 'Y'),
    (0x16, 'u', 'U'),
    (0x17, 'i', 'I'),
    (0x18, 'o', 'O'),
    (0x19, 'p', 'P'),
    (0x1A, '[', '{'),
    (0x1B, ']', '}'),
    // Home letter row.
    (0x1E, 'a', 'A'),
    (0x1F, 's', 'S'),
    (0x20, 'd', 'D'),
    (0x21, 'f', 'F'),
    (0x22, 'g', 'G'),
    (0x23, 'h', 'H'),
    (0x24, 'j', 'J'),
    (0x25, 'k', 'K'),
    (0x26, 'l', 'L'),
    (0x27, ';', ':'),
    (0x28, '\'', '"'),
    (0x29, '`', '~'),
    (0x2B, '\\', '|'),
    // Bottom letter row.
    (0x2C, 'z', 'Z'),
    (0x2D, 'x', 'X'),
    (0x2E, 'c', 'C'),
    (0x2F, 'v', 'V'),
    (0x30, 'b', 'B'),
    (0x31, 'n', 'N'),
    (0x32, 'm', 'M'),
    (0x33, ',', '<'),
    (0x34, '.', '>'),
    (0x35, '/', '?'),
    // Space.
    (0x39, ' ', ' '),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_plain_letter() {
        let mut d = ScancodeDecoder::new();
        // 'a' make = 0x1E.
        assert_eq!(d.push(0x1E), Some(KeyEvent::ch('a')));
        // 'a' break = 0x9E: no event.
        assert_eq!(d.push(0x9E), None);
    }

    #[test]
    fn shift_capitalizes_then_releases() {
        let mut d = ScancodeDecoder::new();
        assert_eq!(d.push(SC_LSHIFT), None); // shift down
        let ev = d.push(0x1E).unwrap(); // 'a' with shift
        assert_eq!(ev.key, Key::Char('A'));
        assert!(ev.mods.shift);
        assert_eq!(d.push(SC_LSHIFT | BREAK_MASK), None); // shift up
        assert_eq!(d.push(0x1E), Some(KeyEvent::ch('a'))); // lowercase again
    }

    #[test]
    fn shifted_number_is_symbol() {
        let mut d = ScancodeDecoder::new();
        d.push(SC_RSHIFT);
        assert_eq!(d.push(0x02).unwrap().key, Key::Char('!')); // shift+1
    }

    #[test]
    fn ctrl_modifier_is_carried() {
        let mut d = ScancodeDecoder::new();
        d.push(SC_CTRL);
        let ev = d.push(0x2E).unwrap(); // 'c'
        assert_eq!(ev.key, Key::Char('c'));
        assert!(ev.mods.ctrl); // Ctrl+C visible to the consumer
        d.push(SC_CTRL | BREAK_MASK);
        assert!(!d.push(0x2E).unwrap().mods.ctrl);
    }

    #[test]
    fn named_keys() {
        let mut d = ScancodeDecoder::new();
        assert_eq!(d.push(0x1C), Some(KeyEvent::new(Key::Enter)));
        assert_eq!(d.push(0x0E), Some(KeyEvent::new(Key::Backspace)));
        assert_eq!(d.push(0x0F), Some(KeyEvent::new(Key::Tab)));
        assert_eq!(d.push(0x01), Some(KeyEvent::new(Key::Escape)));
        assert_eq!(d.push(0x39), Some(KeyEvent::ch(' ')));
    }

    #[test]
    fn extended_arrows_and_delete() {
        let mut d = ScancodeDecoder::new();
        // Up arrow: 0xE0, 0x48.
        assert_eq!(d.push(EXTENDED_PREFIX), None);
        assert_eq!(d.push(0x48), Some(KeyEvent::new(Key::Up)));
        // Right arrow.
        d.push(EXTENDED_PREFIX);
        assert_eq!(d.push(0x4D), Some(KeyEvent::new(Key::Right)));
        // Extended Delete (0xE0,0x53).
        d.push(EXTENDED_PREFIX);
        assert_eq!(d.push(0x53), Some(KeyEvent::new(Key::Delete)));
        // Extended break (e.g. 0xE0, 0xC8 — Up release) yields nothing.
        d.push(EXTENDED_PREFIX);
        assert_eq!(d.push(0x48 | BREAK_MASK), None);
    }

    #[test]
    fn right_ctrl_is_extended_and_carries() {
        let mut d = ScancodeDecoder::new();
        // Right Ctrl: 0xE0, 0x1D.
        d.push(EXTENDED_PREFIX);
        assert_eq!(d.push(SC_CTRL), None);
        assert!(d.modifiers().ctrl);
    }

    #[test]
    fn unknown_codes_are_ignored() {
        let mut d = ScancodeDecoder::new();
        assert_eq!(d.push(0x7E), None); // unmapped make code
    }
}
