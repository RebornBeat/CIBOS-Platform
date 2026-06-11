//! # Kernel CSPRNG
//!
//! A deterministic, seed-driven pseudo-random generator used by the
//! weighted-entropy selector. It is a hash-counter construction: each 32-byte
//! output block is `SHA-256(key || counter)`, with the counter incremented per
//! block. The key is the entropy seed firmware gathered and passed in the
//! handoff record.
//!
//! Properties that matter here:
//!
//! * **Deterministic from the seed** — identical seeds produce identical
//!   streams, which is what makes the scheduler's behavior reproducible in
//!   tests while remaining unpredictable in production (where the seed comes
//!   from the hardware RNG).
//! * **Forward output only** — there is no way to run the counter backward to
//!   recover earlier scheduling decisions from a later state without the key.
//! * **No allocation** — fixed buffers only, so it runs in the kernel's hot
//!   path without touching the allocator.
//!
//! This drives *scheduling* entropy. It deliberately reuses the audited SHA-256
//! in `shared` rather than introducing a separate stream cipher.

use shared::crypto::hash::{sha256, Digest256};

/// A SHA-256 hash-counter pseudo-random generator.
pub struct Csprng {
    key: [u8; 32],
    counter: u64,
    block: Digest256,
    block_pos: usize,
}

impl Csprng {
    /// Seed the generator from a 32-byte seed (the handoff entropy seed).
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let mut rng = Csprng {
            key: seed,
            counter: 0,
            block: [0u8; 32],
            block_pos: 32, // force a refill on first use
        };
        rng.refill();
        rng
    }

    /// Generate the next output block as `SHA-256(key || counter)`.
    fn refill(&mut self) {
        let mut input = [0u8; 40];
        input[..32].copy_from_slice(&self.key);
        input[32..].copy_from_slice(&self.counter.to_le_bytes());
        self.block = sha256(&input);
        self.counter = self.counter.wrapping_add(1);
        self.block_pos = 0;
    }

    /// Produce the next byte of the stream.
    pub fn next_u8(&mut self) -> u8 {
        if self.block_pos >= 32 {
            self.refill();
        }
        let b = self.block[self.block_pos];
        self.block_pos += 1;
        b
    }

    /// Fill `out` with stream bytes. Used by the `get_random` syscall.
    pub fn fill_bytes(&mut self, out: &mut [u8]) {
        for b in out.iter_mut() {
            *b = self.next_u8();
        }
    }

    /// Produce the next `u64` of the stream (little-endian from 8 bytes).
    pub fn next_u64(&mut self) -> u64 {
        let mut bytes = [0u8; 8];
        for b in &mut bytes {
            *b = self.next_u8();
        }
        u64::from_le_bytes(bytes)
    }

    /// Produce a value in `0..bound` (uniform via rejection sampling, so there
    /// is no modulo bias). `bound` must be non-zero.
    ///
    /// # Panics
    ///
    /// Panics if `bound` is zero.
    pub fn next_bounded(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "bound must be non-zero");
        // Rejection sampling: discard values in the final, short interval so the
        // remaining range is an exact multiple of `bound`.
        let zone = u64::MAX - (u64::MAX % bound);
        loop {
            let v = self.next_u64();
            if v < zone {
                return v % bound;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_from_seed() {
        let mut a = Csprng::from_seed([1u8; 32]);
        let mut b = Csprng::from_seed([1u8; 32]);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Csprng::from_seed([1u8; 32]);
        let mut b = Csprng::from_seed([2u8; 32]);
        // Extremely unlikely to match across 16 draws if seeds differ.
        let mut any_diff = false;
        for _ in 0..16 {
            if a.next_u64() != b.next_u64() {
                any_diff = true;
            }
        }
        assert!(any_diff);
    }

    #[test]
    fn bounded_in_range() {
        let mut r = Csprng::from_seed([7u8; 32]);
        for _ in 0..1000 {
            let v = r.next_bounded(10);
            assert!(v < 10);
        }
    }

    #[test]
    fn bounded_distribution_is_reasonable() {
        // Over many draws each bucket should be hit; this guards against a
        // generator that is stuck or badly biased.
        let mut r = Csprng::from_seed([42u8; 32]);
        let mut counts = [0u32; 8];
        for _ in 0..8000 {
            counts[r.next_bounded(8) as usize] += 1;
        }
        for c in counts {
            // Expected ~1000 each; allow a wide tolerance to avoid flakiness.
            assert!(c > 700 && c < 1300, "bucket count {c} out of expected range");
        }
    }
}
