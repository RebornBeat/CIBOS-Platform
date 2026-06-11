//! Console output and process exit for a CIBOS application.
//!
//! [`print`] / [`println`] write to the kernel console (serial + screen) via the
//! `Log` syscall; [`exit`] terminates the application via the `Exit` syscall.
//! All are `no_std` and alloc-free — they write borrowed byte slices directly.

use crate::syscall::syscall3;
use shared::protocols::syscall::Syscall;

/// Maximum bytes written per `Log` syscall (the kernel bounds this too); longer
/// output is split across calls.
const LOG_CHUNK: usize = 4096;

/// Write raw bytes to the kernel console.
pub fn write(bytes: &[u8]) {
    for chunk in bytes.chunks(LOG_CHUNK) {
        // SAFETY: `chunk` is a valid readable slice; we pass its pointer and
        // length, which the kernel validates against this boundary.
        unsafe {
            syscall3(Syscall::Log, chunk.as_ptr() as u64, chunk.len() as u64, 0);
        }
    }
}

/// Write a string slice to the kernel console.
pub fn print(s: &str) {
    write(s.as_bytes());
}

/// Write a string slice followed by a newline.
pub fn println(s: &str) {
    print(s);
    write(b"\n");
}

/// Terminate the application with `code`. Does not return.
pub fn exit(code: u64) -> ! {
    // SAFETY: Exit never returns to the caller; the kernel tears down the
    // boundary. The loop satisfies the `!` return type if the kernel ever did.
    unsafe {
        syscall3(Syscall::Exit, code, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// A [`core::fmt::Write`] adapter so applications can use `write!`/`writeln!`
/// against the console without an allocator.
pub struct Console;

impl core::fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        print(s);
        Ok(())
    }
}
