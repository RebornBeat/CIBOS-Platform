//! # Cryptography
//!
//! The cryptographic layer for CIBIOS/CIBOS, organized so that the *abstractions*
//! always compile while concrete *algorithms* are feature-selected.
//!
//! * [`hash`] — always present. SHA-2/SHA-3 wrappers used for image integrity,
//!   configuration hashing, and as building blocks.
//! * [`signature`] — the `SignatureVerifier` / `SignatureSigner` traits and the
//!   [`signature::SignatureAlgorithm`] identifier. Always compiles.
//! * [`kem`] — the `KeyEncapsulation` trait and [`kem::KemAlgorithm`] identifier.
//!   Always compiles.
//! * [`backends`] — concrete implementations, each behind a Cargo feature.
//!
//! This structure is what lets the no-crypto, classical, and post-quantum
//! deployment paths described in the Convergent Architecture white paper share
//! a single type system: code is written against the traits, and a build
//! selects which algorithms physically exist.

pub mod backends;
pub mod hash;
pub mod kem;
pub mod signature;

pub use hash::{
    digests_equal_ct, sha256, sha3_256, sha3_512, sha512, Digest256, Digest512,
    IncrementalSha256, DIGEST_256_LEN, DIGEST_512_LEN,
};
pub use kem::{KemAlgorithm, KeyEncapsulation};
pub use signature::{verify_with, SignatureAlgorithm, SignatureVerifier};

#[cfg(feature = "std")]
pub use signature::SignatureSigner;
