//! # Hashing
//!
//! Thin, allocation-free wrappers over the `sha2` and `sha3` crates (both
//! `no_std`). These are the only cryptographic primitives that are *always*
//! compiled in, because hashing is needed everywhere — image integrity in the
//! handoff, configuration signing, and as a building block for the signature
//! backends — and the implementations are small and dependency-light.
//!
//! All functions are pure: input bytes in, fixed-size digest out. They allocate
//! nothing and are safe to call in firmware before an allocator exists.

use sha2::{Digest, Sha256, Sha512};
use sha3::{Sha3_256, Sha3_512};

/// Length of a SHA-256 / SHA3-256 digest in bytes.
pub const DIGEST_256_LEN: usize = 32;
/// Length of a SHA-512 / SHA3-512 digest in bytes.
pub const DIGEST_512_LEN: usize = 64;

/// A 256-bit digest.
pub type Digest256 = [u8; DIGEST_256_LEN];
/// A 512-bit digest.
pub type Digest512 = [u8; DIGEST_512_LEN];

/// Compute the SHA-256 digest of `data`.
#[must_use]
pub fn sha256(data: &[u8]) -> Digest256 {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let out = hasher.finalize();
    let mut digest = [0u8; DIGEST_256_LEN];
    digest.copy_from_slice(&out);
    digest
}

/// Compute the SHA-512 digest of `data`.
#[must_use]
pub fn sha512(data: &[u8]) -> Digest512 {
    let mut hasher = Sha512::new();
    hasher.update(data);
    let out = hasher.finalize();
    let mut digest = [0u8; DIGEST_512_LEN];
    digest.copy_from_slice(&out);
    digest
}

/// Compute the SHA3-256 digest of `data`.
#[must_use]
pub fn sha3_256(data: &[u8]) -> Digest256 {
    let mut hasher = Sha3_256::new();
    hasher.update(data);
    let out = hasher.finalize();
    let mut digest = [0u8; DIGEST_256_LEN];
    digest.copy_from_slice(&out);
    digest
}

/// Compute the SHA3-512 digest of `data`.
#[must_use]
pub fn sha3_512(data: &[u8]) -> Digest512 {
    let mut hasher = Sha3_512::new();
    hasher.update(data);
    let out = hasher.finalize();
    let mut digest = [0u8; DIGEST_512_LEN];
    digest.copy_from_slice(&out);
    digest
}

/// Incremental SHA-256 hasher, for hashing data that arrives in pieces (for
/// example, a multi-component OS image read block by block during boot).
///
/// Wraps the streaming `sha2` API without allocating.
pub struct IncrementalSha256 {
    inner: Sha256,
}

impl IncrementalSha256 {
    /// Begin a new incremental hash.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Sha256::new(),
        }
    }

    /// Feed another chunk of data into the hash.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalize and produce the digest, consuming the hasher.
    #[must_use]
    pub fn finalize(self) -> Digest256 {
        let out = self.inner.finalize();
        let mut digest = [0u8; DIGEST_256_LEN];
        digest.copy_from_slice(&out);
        digest
    }
}

impl Default for IncrementalSha256 {
    fn default() -> Self {
        Self::new()
    }
}

/// Constant-time comparison of two 256-bit digests.
///
/// Avoids the early-exit timing leak of a naive `==`. Use this when comparing a
/// computed digest against an expected value where an attacker might observe
/// timing (for example, integrity checks on attacker-supplied images).
#[must_use]
pub fn digests_equal_ct(a: &Digest256, b: &Digest256) -> bool {
    let mut diff: u8 = 0;
    for i in 0..DIGEST_256_LEN {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known-answer test vectors for the empty input and "abc".
    // These are the canonical published digests for each algorithm.

    #[test]
    fn sha256_empty() {
        let d = sha256(b"");
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            d,
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55,
            ]
        );
    }

    #[test]
    fn sha256_abc() {
        let d = sha256(b"abc");
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            d,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn sha3_256_abc() {
        let d = sha3_256(b"abc");
        // SHA3-256("abc") = 3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532
        assert_eq!(
            d,
            [
                0x3a, 0x98, 0x5d, 0xa7, 0x4f, 0xe2, 0x25, 0xb2, 0x04, 0x5c, 0x17, 0x2d, 0x6b, 0xd3,
                0x90, 0xbd, 0x85, 0x5f, 0x08, 0x6e, 0x3e, 0x9d, 0x52, 0x5b, 0x46, 0xbf, 0xe2, 0x45,
                0x11, 0x43, 0x15, 0x32,
            ]
        );
    }

    #[test]
    fn incremental_matches_oneshot() {
        let mut h = IncrementalSha256::new();
        h.update(b"ab");
        h.update(b"c");
        assert_eq!(h.finalize(), sha256(b"abc"));
    }

    #[test]
    fn constant_time_compare() {
        let a = sha256(b"abc");
        let b = sha256(b"abc");
        let c = sha256(b"abd");
        assert!(digests_equal_ct(&a, &b));
        assert!(!digests_equal_ct(&a, &c));
    }

    #[test]
    fn sha512_empty_len() {
        let d = sha512(b"");
        assert_eq!(d.len(), DIGEST_512_LEN);
        // First 8 bytes of SHA-512("") = cf83e1357eefb8bd
        assert_eq!(&d[..8], &[0xcf, 0x83, 0xe1, 0x35, 0x7e, 0xef, 0xb8, 0xbd]);
    }
}
