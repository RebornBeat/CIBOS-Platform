//! The on-kernel [`Console`] backend.
//!
//! [`SyscallConsole`] implements the shared [`cibos_console::Console`] trait on
//! top of this runtime's syscall primitives: `write_line` logs through the
//! `Log` syscall, `read_line` reads a line through `ReadKey`, and `read_secret`
//! reads a line with the echo masked. This is the kernel counterpart to
//! `platform-cli`'s `StdConsole` — it is what lets the *existing* line-oriented
//! applications (the shell, `login::run_login`, CLI tools) run unchanged in
//! ring 3: they depend only on the `Console` trait, and this supplies the
//! backend.

use alloc::string::String;
use cibos_console::Console;

/// A [`Console`] backed by the kernel syscall interface (Log + ReadKey).
///
/// Zero-sized: it holds no state, since all I/O goes through syscalls. Construct
/// with [`SyscallConsole::new`] and pass `&console` to any `Console` consumer.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyscallConsole;

impl SyscallConsole {
    /// A new syscall-backed console.
    #[must_use]
    pub const fn new() -> Self {
        SyscallConsole
    }
}

impl Console for SyscallConsole {
    fn write_line(&self, line: &str) {
        crate::console::println(line);
    }

    fn read_line(&self) -> Option<String> {
        // A blocking line read with echo; never returns end-of-input on the
        // kernel console (the keyboard is always present), so this is `Some`.
        Some(crate::input::read_line(false))
    }

    fn read_secret(&self) -> Option<String> {
        // Same line editor, but echo each character as `*`.
        Some(crate::input::read_line(true))
    }
}
