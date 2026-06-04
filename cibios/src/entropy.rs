//! # Entropy Mixing
//!
//! Firmware gathers entropy for the kernel's CSPRNG seed and passes it in the
//! handoff. The *sources* are architecture-specific and `unsafe` (timestamp and
//! cycle counters, `RDRAND` where present), so they live in the arch layer; the
//! *mixing* is portable, `unsafe`-free, and host-tested, and lives here.
//!
//! Each raw 64-bit sample is folded into a SHA-256 incremental hash under a
//! domain tag; the 32-byte digest is the seed. SHA-256 over the concatenated
//! samples whitens the (often low-quality) raw sources into a uniform seed and
//! ensures distinct sample sets yield unrelated seeds.
//!
//! A note on quality: on platforms with a true hardware RNG (`RDRAND`) the
//! samples carry real entropy. On platforms without one — the QEMU `virt`
//! machines, and many boards — the samples are counter reads with timing
//! jitter, which is *not* cryptographic-grade. A production Standard-profile
//! boot must feed this mixer a hardware TRNG; the counter path is the honest
//! best effort for development and bring-up.

use shared::crypto::hash::{Digest256, IncrementalSha256};

/// Domain separation tag, so this seed can never collide with a hash computed
/// for another purpose.
const ENTROPY_DOMAIN: &[u8] = b"CIBIOS-entropy-seed-v1";

/// Mix raw 64-bit entropy samples into a 32-byte seed.
#[must_use]
pub fn mix_entropy(samples: &[u64]) -> Digest256 {
    let mut hasher = IncrementalSha256::new();
    hasher.update(ENTROPY_DOMAIN);
    // Include the count so that, e.g., [a] and [a, 0] differ.
    hasher.update(&(samples.len() as u64).to_le_bytes());
    for sample in samples {
        hasher.update(&sample.to_le_bytes());
    }
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_same_samples() {
        let s = [0x1234_5678_9abc_def0, 0x0fed_cba9_8765_4321, 42];
        assert_eq!(mix_entropy(&s), mix_entropy(&s));
    }

    #[test]
    fn distinct_samples_diverge() {
        let a = mix_entropy(&[1, 2, 3]);
        let b = mix_entropy(&[1, 2, 4]);
        assert_ne!(a, b);
    }

    #[test]
    fn length_is_significant() {
        // [a] must not collide with [a, 0] (length is folded in).
        assert_ne!(mix_entropy(&[7]), mix_entropy(&[7, 0]));
    }

    #[test]
    fn seed_is_not_all_zero() {
        // Even an all-zero sample set yields a non-zero seed (the domain tag and
        // length are hashed), so a zeroed seed can never silently slip through.
        let seed = mix_entropy(&[0, 0, 0, 0]);
        assert!(seed.iter().any(|&b| b != 0));
    }
}
