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
use alloc::vec::Vec;

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

/// The filesystem surface a line-oriented application uses.
///
/// This is the exact set of operations the shell's command dispatcher needs,
/// over `&str` paths (UTF-8). It is implemented twice — by the host SDK's
/// `Filesystem` (development/tests) and by the on-kernel `cibos-app` runtime
/// (syscall-backed) — so the *same* application logic runs in both places.
pub trait ShellFs {
    /// Create or overwrite the file at `path` with `data`; `true` on success.
    fn write(&self, path: &str, data: &[u8]) -> bool;
    /// Read the whole file at `path`, or `None` if it does not exist.
    fn read(&self, path: &str) -> Option<Vec<u8>>;
    /// List the immediate child names under directory `path`.
    ///
    /// Contract: returns the IMMEDIATE CHILD NAMES of the directory — bare names,
    /// not full paths, and not recursive. Both backends honor this: the kernel
    /// `SyscallFs` returns CIBOSFS `list_dir` names directly; the host SDK derives
    /// child names from its flat key space.
    fn list(&self, path: &str) -> Vec<String>;
    /// Delete the file at `path`; `true` if it was removed.
    fn delete(&self, path: &str) -> bool;
    /// Whether `path` exists (as a file or a directory).
    ///
    /// The default treats `path` as existing if it can be read as a file OR if it
    /// has any child entries (so directories are detected too). Backends with a
    /// cheaper or more precise existence probe — the kernel `SyscallFs` and the
    /// host SDK both do — override this.
    fn exists(&self, path: &str) -> bool {
        self.read(path).is_some() || !self.list(path).is_empty()
    }
    /// Ensure a directory at `path` exists. On a hierarchical filesystem this
    /// creates the directory (needed before writing files beneath it); on a flat
    /// key-value backend it is a no-op (the default), since such backends key on
    /// the full path and have no real directories. `true` if the directory now
    /// exists (or the backend needs none).
    fn mkdir(&self, path: &str) -> bool {
        let _ = path;
        true
    }
}

/// The minimal system surface a line-oriented application uses: a filesystem,
/// a monotonic clock, and its resource limits. This is everything the shell's
/// synchronous command dispatcher touches — deliberately no spawn, channels, or
/// networking, which belong to the async host runtime and are replaced by the
/// kernel's own process model.
pub trait ShellSystem {
    /// The filesystem handle type this system hands out.
    type Fs: ShellFs;

    /// Obtain a filesystem handle.
    fn filesystem(&self) -> Self::Fs;

    /// Monotonic time since boot, in nanoseconds.
    fn now_nanos(&self) -> u64;

    /// The resource limits granted to this application.
    fn resource_limits(&self) -> shared::ResourceLimits;
}

