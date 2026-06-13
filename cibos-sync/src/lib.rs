//! # `cibos-sync` — no_std synchronization for CIBOS applications
//!
//! A single primitive, [`Mutex`], with the same surface as
//! [`std::sync::Mutex`] (its [`lock`](Mutex::lock) returns a `Result` so call
//! sites that write `m.lock().unwrap()` are identical). This lets a CIBOS
//! application keep its shared state behind a lock and reuse the *exact same*
//! `process_command` logic whether it runs:
//!
//! * on the host, where state is shared into an async worker, or
//! * in a ring-3 `.capp`, single-threaded and `no_std`.
//!
//! The lock is spin-based and cannot be poisoned, so its error type is
//! [`Infallible`] and `lock()` always returns `Ok`. On a single-threaded ring-3
//! task it never actually contends; the atomic flag only guards against
//! re-entrant access (which would be a caller bug).
//!
//! This crate is pure `no_std` (atomics + `UnsafeCell`, no syscalls), so it
//! links cleanly into both host and bare-metal builds.

#![no_std]
#![warn(missing_docs)]

use core::cell::UnsafeCell;
use core::convert::Infallible;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// A spin-based mutual-exclusion lock with a [`std::sync::Mutex`]-shaped API.
#[derive(Debug)]
pub struct Mutex<T: ?Sized> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the lock serializes all access to `value`; sharing across threads is
// sound when `T: Send`.
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}

impl<T> Mutex<T> {
    /// Create a new mutex wrapping `value`.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquire the lock, returning a guard. Mirrors `std::sync::Mutex::lock`:
    /// the `Result` is always `Ok` (this lock cannot be poisoned), so
    /// `lock().unwrap()` is the idiomatic call.
    ///
    /// # Errors
    ///
    /// Never returns `Err`; the error type is [`Infallible`].
    pub fn lock(&self) -> Result<MutexGuard<'_, T>, Infallible> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Ok(MutexGuard { lock: self })
    }
}

/// RAII guard that releases the [`Mutex`] when dropped.
#[derive(Debug)]
pub struct MutexGuard<'a, T: ?Sized> {
    lock: &'a Mutex<T>,
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: holding the guard means we hold the lock, so we are the sole
        // accessor of the inner value for the guard's lifetime.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above; exclusive access is guaranteed by the held lock.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    #[test]
    fn lock_guards_access_and_unlocks() {
        let m = Mutex::new(Vec::<u32>::new());
        m.lock().unwrap().push(1);
        m.lock().unwrap().push(2);
        let g = m.lock().unwrap();
        assert_eq!(&*g, &[1, 2]);
    }
}
