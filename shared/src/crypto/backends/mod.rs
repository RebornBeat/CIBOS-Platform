//! # Cryptographic Backends
//!
//! Concrete implementations of the [`super::signature`] and [`super::kem`]
//! traits, each behind its own Cargo feature. When a feature is off the
//! corresponding submodule is not compiled at all, so a default build pulls in
//! no cryptographic backend code and the trait-level dispatch reports the
//! algorithm as unavailable.
//!
//! Backend selection by deployment path:
//! * `pqc-sphincs` — SPHINCS+ verifier/signer, the hash-based root-of-trust
//!   for boot/firmware signing.
//! * `pqc-mldsa` — ML-DSA (Dilithium) verifier/signer for channel signatures.
//! * `pqc-mlkem` — ML-KEM (Kyber) key encapsulation for channel confidentiality.

#[cfg(feature = "pqc-sphincs")]
pub mod sphincs;

#[cfg(feature = "pqc-mldsa")]
pub mod mldsa;

#[cfg(feature = "pqc-mlkem")]
pub mod mlkem;
