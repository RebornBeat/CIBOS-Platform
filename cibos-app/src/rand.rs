//! Randomness and time for a CIBOS application.
//!
//! [`fill`] draws cryptographically-random bytes from the kernel CSPRNG via the
//! `GetRandom` syscall (used for salts, nonces, tokens); [`now_nanos`] reads the
//! monotonic clock via the `Now` syscall.

use crate::syscall::{decode, syscall3};
use shared::protocols::syscall::{Syscall, SyscallError};

/// Fill `buf` with cryptographically-random bytes from the kernel CSPRNG.
///
/// # Errors
///
/// A kernel error if no entropy source is available.
pub fn fill(buf: &mut [u8]) -> Result<(), SyscallError> {
    if buf.is_empty() {
        return Ok(());
    }
    // SAFETY: buf is a valid writable slice; the kernel validates it and writes
    // at most buf.len() bytes.
    let ret = unsafe { syscall3(Syscall::GetRandom, buf.as_mut_ptr() as u64, buf.len() as u64, 0) };
    decode(ret).map(|_| ())
}

/// A fresh 32-byte random salt from the kernel CSPRNG.
///
/// # Errors
///
/// As [`fill`].
pub fn salt32() -> Result<[u8; 32], SyscallError> {
    let mut s = [0u8; 32];
    fill(&mut s)?;
    Ok(s)
}

/// Monotonic nanoseconds since boot (from the `Now` syscall).
#[must_use]
pub fn now_nanos() -> u64 {
    // SAFETY: Now takes no pointer arguments and returns a scalar.
    let ret = unsafe { syscall3(Syscall::Now, 0, 0, 0) };
    if ret < 0 {
        0
    } else {
        ret as u64
    }
}

/// Cooperatively sleep for at least `nanos` nanoseconds via the `Sleep` syscall.
pub fn sleep_nanos(nanos: u64) {
    // The duration u64 is carried in arg0 (low 32 bits) and arg1 (high 32).
    let lo = nanos & 0xFFFF_FFFF;
    let hi = nanos >> 32;
    // SAFETY: Sleep takes two scalar arguments and no pointers.
    let _ = unsafe { syscall3(Syscall::Sleep, lo, hi, 0) };
}

/// Cooperatively sleep for at least `millis` milliseconds.
pub fn sleep_millis(millis: u64) {
    sleep_nanos(millis.saturating_mul(1_000_000));
}
