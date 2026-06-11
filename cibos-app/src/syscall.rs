//! Raw syscall layer: the thin `int 0x80` wrapper a CIBOS application uses to
//! enter the kernel. The register convention matches
//! [`shared::protocols::syscall`]: number in `rax`, arguments in `rdi`, `rsi`,
//! `rdx`, return value in `rax`. A negative return is a [`SyscallError`] code.
//!
//! Everything above this (console, filesystem) marshals onto these primitives;
//! this is the single place the application touches the trap instruction.

use shared::protocols::syscall::{Syscall, SyscallError};

/// Issue a syscall with up to three arguments, returning the raw `i64` result
/// (negative encodes a [`SyscallError`]).
///
/// # Safety
///
/// The arguments must satisfy the called syscall's contract (e.g. valid user
/// pointers and lengths). The kernel validates pointers against the calling
/// boundary, but passing a wrong length for a buffer is still a logic error.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn syscall3(call: Syscall, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: the caller upholds the per-syscall argument contract; this is the
    // architecture trap instruction.
    unsafe {
        core::arch::asm!(
            "int 0x80",
            in("rax") call.number(),
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            lateout("rax") ret,
            // The kernel trap preserves callee-saved registers; mark the
            // caller-saved ones the convention may clobber as clobbered.
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Non-x86_64 stub so the crate type-checks when built for other targets during
/// host tooling/tests. A real implementation is provided per architecture as
/// those trap paths are added; calling it on an unsupported arch returns
/// `NoSuchCall`.
#[cfg(not(target_arch = "x86_64"))]
#[inline]
pub unsafe fn syscall3(_call: Syscall, _a0: u64, _a1: u64, _a2: u64) -> i64 {
    SyscallError::NoSuchCall.as_return()
}

/// Convert a raw syscall return into a `Result`: `Ok(value)` for `>= 0`,
/// `Err(SyscallError)` for a recognized negative code (unrecognized negatives
/// map to [`SyscallError::IoError`] as a catch-all).
pub fn decode(ret: i64) -> Result<i64, SyscallError> {
    if ret >= 0 {
        Ok(ret)
    } else {
        Err(SyscallError::from_return(ret).unwrap_or(SyscallError::IoError))
    }
}
