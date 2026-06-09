//! # Syscall ABI
//!
//! The contract between a CIBOS application (running in an isolated user
//! boundary) and the kernel. Like the boot contract in [`crate::protocols::boot`],
//! this is the single source of truth both sides bind to: the application's
//! runtime issues these numbers with this register convention, and the kernel's
//! trap dispatcher decodes them the same way.
//!
//! ## Why a narrow ABI
//!
//! The rich application surface (channels, lanes, timers, the network and
//! filesystem facades) is expressed in the SDK's `System` API. That API does not
//! need one syscall per method; it needs a small set of primitives the SDK
//! marshals onto. This module defines that primitive set. It starts deliberately
//! minimal — the operations needed to load an application, let it run, log, and
//! exit — and grows as the transport carries more of `System`. Keeping the raw
//! ABI small keeps the trusted trap path small.
//!
//! ## Register convention (x86_64)
//!
//! A user→kernel trap (`int 0x80` / `syscall`) passes:
//!
//! | role          | register |
//! |---------------|----------|
//! | syscall number| `rax`    |
//! | argument 0    | `rdi`    |
//! | argument 1    | `rsi`    |
//! | argument 2    | `rdx`    |
//! | return value  | `rax`    |
//!
//! Other architectures map the same six logical slots (number + up to three
//! args + return) onto their own ABI registers in the arch trap glue; this
//! module is architecture-neutral and only fixes the *logical* contract.
//!
//! Pointers passed in arguments are **user virtual addresses** in the calling
//! boundary's address space; the kernel validates and translates them before
//! use. A negative return value (as a two's-complement [`i64`]) is an error
//! code from [`SyscallError`]; zero or positive is success.

/// ABI version. Bumped on any incompatible change to numbers or convention.
pub const SYSCALL_ABI_VERSION: u32 = 1;

/// Syscall numbers. Stable identifiers placed in the number register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Syscall {
    /// `log(ptr: *const u8, len: usize) -> 0`. Write `len` bytes of UTF-8 from
    /// the user buffer at `ptr` to the kernel console (serial + screen). The
    /// kernel bounds-checks and translates `ptr` within the caller's space.
    Log = 1,
    /// `exit(code: u64) -> !`. Terminate the calling application; control returns
    /// to the kernel, which tears down the boundary. Does not return to user.
    Exit = 2,
    /// `yield_now() -> 0`. Voluntarily yield the CPU back to the scheduler.
    Yield = 3,
    /// `now() -> u64`. Monotonic nanoseconds since boot, in the return register.
    Now = 4,
}

impl Syscall {
    /// Decode a raw syscall number, or `None` if unknown.
    #[must_use]
    pub const fn from_number(n: u64) -> Option<Self> {
        match n {
            1 => Some(Syscall::Log),
            2 => Some(Syscall::Exit),
            3 => Some(Syscall::Yield),
            4 => Some(Syscall::Now),
            _ => None,
        }
    }

    /// The raw number for this syscall.
    #[must_use]
    pub const fn number(self) -> u64 {
        self as u64
    }
}

/// Error codes returned (as negated [`i64`]) in the return register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum SyscallError {
    /// Unknown syscall number.
    NoSuchCall = 1,
    /// A pointer argument was outside the caller's mapped memory.
    BadAddress = 2,
    /// An argument was invalid (e.g. a length that overflows the buffer).
    InvalidArgument = 3,
    /// The operation is not permitted for this boundary.
    NotPermitted = 4,
}

impl SyscallError {
    /// Encode as the negative return value the ABI uses for errors.
    #[must_use]
    pub const fn as_return(self) -> i64 {
        -(self as i64)
    }

    /// Decode an error from a (negative) return value, or `None` if it is not a
    /// recognized error or is a success value (>= 0).
    #[must_use]
    pub const fn from_return(v: i64) -> Option<Self> {
        match v {
            -1 => Some(SyscallError::NoSuchCall),
            -2 => Some(SyscallError::BadAddress),
            -3 => Some(SyscallError::InvalidArgument),
            -4 => Some(SyscallError::NotPermitted),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_roundtrip() {
        for s in [Syscall::Log, Syscall::Exit, Syscall::Yield, Syscall::Now] {
            assert_eq!(Syscall::from_number(s.number()), Some(s));
        }
        assert_eq!(Syscall::from_number(0), None);
        assert_eq!(Syscall::from_number(999), None);
    }

    #[test]
    fn error_codes_roundtrip() {
        for e in [
            SyscallError::NoSuchCall,
            SyscallError::BadAddress,
            SyscallError::InvalidArgument,
            SyscallError::NotPermitted,
        ] {
            assert!(e.as_return() < 0);
            assert_eq!(SyscallError::from_return(e.as_return()), Some(e));
        }
        // Success values are not errors.
        assert_eq!(SyscallError::from_return(0), None);
        assert_eq!(SyscallError::from_return(42), None);
    }
}
