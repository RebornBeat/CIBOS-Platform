//! # CIBOS bare application runtime (`cibos-app`)
//!
//! The minimal `no_std` library a ring-3 CIBOS application (`.capp`) links
//! against to reach the kernel. It exposes exactly the primitives the kernel's
//! syscall ABI provides today, with no allocator and no `std`:
//!
//! * [`console`] — write to the kernel console (`Log`) and [`console::exit`].
//! * [`fs`] — read/write/mkdir/exists against the kernel's filesystem (`Fs*`).
//! * [`syscall`] — the raw `int 0x80` layer the above marshal onto.
//!
//! This is deliberately separate from the rich `std` `cibos-sdk`: a real
//! application that runs *on the kernel* (rather than in-process on a host)
//! needs only these syscall-backed primitives, and keeping the on-device
//! runtime tiny keeps the trusted surface small. As the syscall ABI grows
//! (channels, timers), this runtime grows to match, and higher-level `System`
//! ergonomics can be rebuilt on top without `std`.
//!
//! Applications provide their own entry point (`_start`) and panic handler when
//! built as a freestanding `.capp`; this library does not impose either, so it
//! can also be unit-tested on the host.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

pub mod console;
pub mod console_backend;
pub mod fs;
pub mod heap;
pub mod input;
pub mod rand;
pub mod syscall;

// The freestanding runtime entry (_start + panic handler) is only meaningful on
// the bare application target.
#[cfg(target_os = "none")]
pub mod rt;

pub use shared::protocols::syscall::SyscallError;

pub use console_backend::SyscallConsole;
pub use cibos_console::Console;

#[cfg(test)]
mod tests {
    use super::syscall::decode;
    use shared::protocols::syscall::SyscallError;

    #[test]
    fn decode_success_values() {
        assert_eq!(decode(0), Ok(0));
        assert_eq!(decode(31), Ok(31));
    }

    #[test]
    fn decode_known_errors() {
        assert_eq!(decode(SyscallError::NotFound.as_return()), Err(SyscallError::NotFound));
        assert_eq!(decode(SyscallError::IoError.as_return()), Err(SyscallError::IoError));
        assert_eq!(
            decode(SyscallError::BadAddress.as_return()),
            Err(SyscallError::BadAddress)
        );
    }

    #[test]
    fn decode_unknown_negative_is_ioerror() {
        // A negative code with no mapping falls back to IoError.
        assert_eq!(decode(-9999), Err(SyscallError::IoError));
    }
}
