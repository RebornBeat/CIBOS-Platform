//! # `cibos-console` — the line-oriented Console seam
//!
//! A single trait, [`Console`], that decouples line-oriented applications (the
//! shell, the login gate, CLI tools) from *where* their I/O goes. The same
//! application code runs against:
//!
//! * a host console backed by `std` stdin/stdout (development), or a capture
//!   console for tests — both live in `platform-cli`;
//! * an on-kernel console backed by the `Log` and `ReadKey` syscalls — lives in
//!   the kernel-side `cibos-app` runtime.
//!
//! Keeping the trait here, in a tiny `no_std` crate, is what lets the *existing*
//! applications and the existing `login::run_login` run unchanged on the booted
//! kernel: they depend only on this trait, not on a particular (std) backend.
//! This is the seam the architecture always intended — see `platform-cli`'s own
//! note that "applications written against the `Console` trait do not change"
//! when the on-device console arrives.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::string::String;

/// A line-oriented console: write a line, read a line.
///
/// Not required to be `Send`/`Sync`: CLI tasks hold the console by shared
/// reference on a single-threaded executor (host) or run synchronously in one
/// ring-3 task (kernel).
pub trait Console {
    /// Write a line of output, followed by a newline.
    fn write_line(&self, line: &str);

    /// Read a line of input, or `None` at end of input.
    fn read_line(&self) -> Option<String>;

    /// Read a line of *secret* input (e.g. a password), suppressing echo where
    /// the backend supports it (a masked TTY, or the kernel console echoing
    /// `*`). The default implementation falls back to [`Console::read_line`], so
    /// existing backends keep working unchanged; backends that can hide input
    /// override this. The login gate uses it for passwords.
    fn read_secret(&self) -> Option<String> {
        self.read_line()
    }
}
