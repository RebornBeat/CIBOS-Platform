//! # Lockscreen
//!
//! A mobile PIN lock screen: a numeric keypad that gates the device behind a
//! PIN. A PIN is just a digit-only password, so it verifies through the same
//! [`Accounts`] path as any other password — tying a correct PIN to the
//! profile's isolation boundary.
//!
//! It is a [`TouchApp`]: taps on the keypad enter digits, `C` clears, and `E`
//! submits. On the correct PIN the screen unlocks; wrong PINs decrement the
//! remaining attempts.
//!
//! Keypad layout (each button is 4 cells wide, 1 tall, starting at row 2):
//!
//! ```text
//!  1  2  3
//!  4  5  6
//!  7  8  9
//!  C  0  E
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use accounts::{Accounts, Credential};
use platform_gui::{Color, Flow, Surface};
use platform_mobile::{Gesture, TouchApp};
use shared::BoundaryId;

const KEYPAD_TOP: u16 = 2;
const BUTTON_WIDTH: u16 = 4;

/// A key on the PIN pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadKey {
    /// A digit 0-9.
    Digit(u8),
    /// Clear the entry.
    Clear,
    /// Submit the entry.
    Enter,
}

/// Map a tap at `(x, y)` to a keypad key, if it lands on a button.
#[must_use]
pub fn button_at(x: u16, y: u16) -> Option<PadKey> {
    if y < KEYPAD_TOP {
        return None;
    }
    let row = y - KEYPAD_TOP;
    let col = x / BUTTON_WIDTH;
    if row > 3 || col > 2 {
        return None;
    }
    if row < 3 {
        Some(PadKey::Digit((row * 3 + col + 1) as u8))
    } else {
        match col {
            0 => Some(PadKey::Clear),
            1 => Some(PadKey::Digit(0)),
            _ => Some(PadKey::Enter),
        }
    }
}

/// Lock state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    /// Awaiting the PIN.
    Locked,
    /// The last attempt was wrong.
    Wrong,
    /// Unlocked successfully.
    Unlocked,
    /// Too many failed attempts.
    LockedOut,
}

/// The PIN lock screen.
pub struct PinLock {
    accounts: Accounts,
    boundary: BoundaryId,
    entered: String,
    state: LockState,
    attempts_left: u32,
}

impl PinLock {
    /// Create a lock screen for `boundary`, allowing `max_attempts` tries.
    #[must_use]
    pub fn new(accounts: Accounts, boundary: BoundaryId, max_attempts: u32) -> Self {
        PinLock {
            accounts,
            boundary,
            entered: String::new(),
            state: LockState::Locked,
            attempts_left: max_attempts,
        }
    }

    /// The current lock state.
    #[must_use]
    pub fn state(&self) -> LockState {
        self.state
    }

    /// Remaining attempts.
    #[must_use]
    pub fn attempts_left(&self) -> u32 {
        self.attempts_left
    }

    /// Apply a keypad key; returns whether the screen has resolved (unlocked or
    /// locked out).
    pub fn press(&mut self, key: PadKey) -> bool {
        match key {
            PadKey::Digit(d) => {
                if self.state != LockState::Unlocked && self.state != LockState::LockedOut {
                    self.entered.push((b'0' + d) as char);
                    if self.state == LockState::Wrong {
                        self.state = LockState::Locked;
                    }
                }
            }
            PadKey::Clear => self.entered.clear(),
            PadKey::Enter => {
                let ok = self
                    .accounts
                    .open_session(self.boundary, Credential::Password(self.entered.as_bytes()))
                    .is_some();
                self.entered.clear();
                if ok {
                    self.state = LockState::Unlocked;
                    return true;
                }
                self.attempts_left = self.attempts_left.saturating_sub(1);
                self.state = if self.attempts_left == 0 {
                    LockState::LockedOut
                } else {
                    LockState::Wrong
                };
                return self.state == LockState::LockedOut;
            }
        }
        false
    }
}

impl TouchApp for PinLock {
    fn name(&self) -> &str {
        "lockscreen"
    }

    fn on_gesture(&mut self, gesture: Gesture) -> Flow {
        if let Gesture::Tap { x, y } = gesture {
            if let Some(key) = button_at(x, y) {
                if self.press(key) {
                    return Flow::Exit; // unlocked or locked out
                }
            }
        }
        Flow::Continue
    }

    fn render(&self, surface: &mut Surface) {
        let status = match self.state {
            LockState::Locked => "Enter PIN",
            LockState::Wrong => "Wrong PIN",
            LockState::Unlocked => "Unlocked",
            LockState::LockedOut => "Locked out",
        };
        let color = match self.state {
            LockState::Wrong | LockState::LockedOut => Color::Red,
            LockState::Unlocked => Color::Green,
            LockState::Locked => Color::White,
        };
        surface.write_colored(0, 0, status, color, Color::Default);
        // Masked PIN entry on row 1.
        let mask = "*".repeat(self.entered.len());
        surface.write_str(0, 1, &mask);
        // Keypad labels.
        let labels = [
            ["1", "2", "3"],
            ["4", "5", "6"],
            ["7", "8", "9"],
            ["C", "0", "E"],
        ];
        for (r, row) in labels.iter().enumerate() {
            for (c, label) in row.iter().enumerate() {
                surface.write_str(c as u16 * BUTTON_WIDTH, KEYPAD_TOP + r as u16, label);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cibos_input::Pointer;
    use platform_mobile::MobileRunner;

    const SALT: [u8; 32] = [0x33; 32];

    fn lock_with_pin(pin: &str) -> PinLock {
        let mut acc = Accounts::new();
        let b = BoundaryId::new(1);
        acc.enroll_password("phone", b, SALT, pin.as_bytes());
        PinLock::new(acc, b, 3)
    }

    #[test]
    fn button_mapping() {
        assert_eq!(button_at(0, 2), Some(PadKey::Digit(1)));
        assert_eq!(button_at(8, 2), Some(PadKey::Digit(3)));
        assert_eq!(button_at(4, 4), Some(PadKey::Digit(8)));
        assert_eq!(button_at(0, 5), Some(PadKey::Clear));
        assert_eq!(button_at(4, 5), Some(PadKey::Digit(0)));
        assert_eq!(button_at(8, 5), Some(PadKey::Enter));
        assert_eq!(button_at(0, 0), None); // title area
        assert_eq!(button_at(99, 99), None);
    }

    #[test]
    fn correct_pin_unlocks() {
        let mut lock = lock_with_pin("1234");
        for d in [1u8, 2, 3, 4] {
            assert!(!lock.press(PadKey::Digit(d)));
        }
        assert!(lock.press(PadKey::Enter)); // resolves
        assert_eq!(lock.state(), LockState::Unlocked);
    }

    #[test]
    fn wrong_pin_decrements_attempts() {
        let mut lock = lock_with_pin("1234");
        for d in [9u8, 9, 9, 9] {
            lock.press(PadKey::Digit(d));
        }
        lock.press(PadKey::Enter);
        assert_eq!(lock.state(), LockState::Wrong);
        assert_eq!(lock.attempts_left(), 2);
    }

    #[test]
    fn lockout_after_max_attempts() {
        let mut lock = lock_with_pin("1234");
        for _ in 0..3 {
            lock.press(PadKey::Digit(0));
            lock.press(PadKey::Enter);
        }
        assert_eq!(lock.state(), LockState::LockedOut);
        assert_eq!(lock.attempts_left(), 0);
    }

    #[test]
    fn clear_resets_entry() {
        let mut lock = lock_with_pin("12");
        lock.press(PadKey::Digit(9));
        lock.press(PadKey::Clear);
        lock.press(PadKey::Digit(1));
        lock.press(PadKey::Digit(2));
        assert!(lock.press(PadKey::Enter));
        assert_eq!(lock.state(), LockState::Unlocked);
    }

    #[test]
    fn unlocks_over_the_mobile_runner() {
        // Drive the lock screen with taps through the touch runner, proving the
        // gesture path: tap 1,2,3,4 then Enter.
        let mut lock = lock_with_pin("1234");
        let mut runner = MobileRunner::new(16, 6);
        let tap = |x, y| {
            [
                Pointer::tap(x, y),
                Pointer {
                    x,
                    y,
                    action: cibos_input::PointerAction::Release,
                    button: cibos_input::Button::Primary,
                },
            ]
        };
        let mut taps = Vec::new();
        // digits 1,2,3,4 are at row 2 col0, row2 col1, row2 col2, row3 col0
        for (x, y) in [(0, 2), (4, 2), (8, 2), (0, 3)] {
            taps.extend(tap(x, y));
        }
        taps.extend(tap(8, 5)); // Enter
        let screen = runner.run(&mut lock, taps);
        assert_eq!(lock.state(), LockState::Unlocked);
        assert!(screen.row_text(0).contains("Unlocked"));
    }
}
