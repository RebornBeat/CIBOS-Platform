//! Keyboard input for a CIBOS application, over the `ReadKey` syscall.
//!
//! [`read_key`] blocks until a key is available (the kernel sleeps the CPU and
//! wakes on the keyboard IRQ); [`poll_key`] returns immediately. The returned
//! [`KeyCode`]/[`KeyMods`] mirror the kernel's input model. [`read_char`] is a
//! convenience for the common case of wanting the next printable character.

use crate::syscall::syscall3;
use shared::protocols::syscall::{decode_key, KeyCode, KeyMods, Syscall};

pub use shared::protocols::syscall::{KeyCode as Code, KeyMods as Mods};

/// Block until a key event is available and return it. Returns `None` only if
/// the kernel reports no input device or the (long) internal wait elapses.
pub fn read_key() -> Option<(KeyCode, KeyMods)> {
    // SAFETY: ReadKey takes a single scalar (blocking flag) and returns a packed
    // value; no pointers are involved.
    let ret = unsafe { syscall3(Syscall::ReadKey, 1, 0, 0) };
    decode_key(ret)
}

/// Return a key event if one is already buffered, without blocking.
pub fn poll_key() -> Option<(KeyCode, KeyMods)> {
    // SAFETY: as `read_key`, with the non-blocking flag.
    let ret = unsafe { syscall3(Syscall::ReadKey, 0, 0, 0) };
    decode_key(ret)
}

/// Block until the next printable character and return it, skipping non-printable
/// keys (arrows, etc.). Returns `None` if input is unavailable.
pub fn read_char() -> Option<char> {
    while let Some((code, _)) = read_key() {
        match code {
            KeyCode::Char(c) => return Some(c),
            KeyCode::Enter => return Some('\n'),
            _ => continue,
        }
    }
    None
}

/// Read a line of input from the keyboard, echoing it to the console and
/// handling Backspace. Returns the line (without the trailing newline) when
/// Enter is pressed. If `mask` is set, typed characters echo as `*` (for
/// passwords). Uses the application heap.
pub fn read_line(mask: bool) -> alloc::string::String {
    use alloc::string::String;
    let mut line = String::new();
    while let Some((code, _mods)) = read_key() {
        match code {
            KeyCode::Enter => {
                crate::console::write(b"\n");
                return line;
            }
            KeyCode::Backspace => {
                if line.pop().is_some() {
                    // Erase the last echoed character: backspace, space, backspace.
                    crate::console::write(b"\x08 \x08");
                }
            }
            KeyCode::Char(c) => {
                line.push(c);
                if mask {
                    crate::console::write(b"*");
                } else {
                    let mut buf = [0u8; 4];
                    crate::console::write(c.encode_utf8(&mut buf).as_bytes());
                }
            }
            _ => {} // ignore navigation keys for a simple line editor
        }
    }
    line
}
