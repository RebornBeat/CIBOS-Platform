//! # SpinLock
//!
//! A minimal spinlock providing `Sync` interior mutability for kernel data
//! structures that are reached through shared (`&self`) references — notably the
//! scheduler state, which the async runtime touches via an
//! `Arc<dyn KernelInterface>`.
//!
//! This is the kernel's only hand-written synchronization primitive and the one
//! place the kernel uses `unsafe`. The implementation is the textbook
//! acquire/release spinlock: a single `AtomicBool` gates a `bias` that hands out
//! exactly one `&mut T` at a time. A [`SpinGuard`] releases the lock on drop.
//!
//! Note on the HIP "no global locks" principle: that principle concerns the
//! *lane execution* path, where isolation and triggering replace shared locks.
//! The scheduler's own bookkeeping, mutated from multiple execution contexts, is
//! a kernel-internal data structure; guarding it with a short-held lock is an
//! implementation detail, not shared state between lanes. A per-core,
//! lock-free run-queue design is a possible later optimization.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// A spinlock guarding a value of type `T`.
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// Safe to share across execution contexts: access to the inner value is
// serialized by the atomic flag, and a guard hands out the only live reference.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Create a new spinlock wrapping `value`.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Acquire the lock, spinning until it is free, and return a guard.
    pub fn lock(&self) -> SpinGuard<'_, T> {
        // Acquire: spin until we flip the flag from false to true.
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Hint to the CPU that we are in a spin-wait loop.
            core::hint::spin_loop();
        }
        SpinGuard { lock: self }
    }
}

/// RAII guard that releases the [`SpinLock`] when dropped.
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: holding the guard means we hold the lock, so we are the sole
        // accessor of the inner value for the guard's lifetime.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above; exclusive access is guaranteed by the held lock.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        // Release: publish our writes and free the lock.
        self.lock.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_grants_mutable_access() {
        let lock = SpinLock::new(0u32);
        {
            let mut g = lock.lock();
            *g += 5;
        }
        assert_eq!(*lock.lock(), 5);
    }

    #[test]
    fn guard_releases_on_drop() {
        let lock = SpinLock::new(1u32);
        let g = lock.lock();
        assert_eq!(*g, 1);
        drop(g);
        // Re-acquire succeeds (would deadlock if release failed).
        let g2 = lock.lock();
        assert_eq!(*g2, 1);
    }
}
