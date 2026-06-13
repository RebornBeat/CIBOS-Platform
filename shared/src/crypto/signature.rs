//! # Signature Schemes
//!
//! The signature abstraction the system verifies against.
//!
//! Verification and signing are split deliberately:
//!
//! * [`SignatureVerifier`] is `no_std` and allocation-free. Firmware (CIBIOS)
//!   verifies the CIBOS image signature with it; the kernel verifies update
//!   packages and configuration with it. This is the trait that must work on
//!   bare metal.
//! * `SignatureSigner` produces signatures and is only needed by `std` build
//!   tooling (the image builder, the package signer). It is gated behind the
//!   `std` feature so it never pulls allocation into a firmware build.
//!
//! Concrete backends (Ed25519, SPHINCS+, ML-DSA) are feature-gated in
//! [`super::backends`]. The trait layer here always compiles, so code can be
//! written against it regardless of which backends a given build includes.

use crate::types::error::{CryptoError, SerializationError};

/// Identifier for a signature algorithm, stable on the wire and in handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum SignatureAlgorithm {
    /// Ed25519 (classical, fast, small).
    Ed25519 = 1,
    /// SPHINCS+ (hash-based, post-quantum, root-of-trust choice for boot).
    SphincsPlus = 2,
    /// ML-DSA / Dilithium (lattice-based, post-quantum, channel signatures).
    MlDsa = 3,
}

impl SignatureAlgorithm {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Whether this algorithm is post-quantum.
    #[must_use]
    pub const fn is_post_quantum(self) -> bool {
        matches!(
            self,
            SignatureAlgorithm::SphincsPlus | SignatureAlgorithm::MlDsa
        )
    }
}

impl TryFrom<u32> for SignatureAlgorithm {
    type Error = SerializationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(SignatureAlgorithm::Ed25519),
            2 => Ok(SignatureAlgorithm::SphincsPlus),
            3 => Ok(SignatureAlgorithm::MlDsa),
            _ => Err(SerializationError::InvalidValue {
                field: "SignatureAlgorithm",
            }),
        }
    }
}

/// A `no_std`, allocation-free signature *verifier*.
///
/// Implementors verify a detached signature over a message using a public key,
/// all supplied as byte slices. Lengths are validated against the algorithm's
/// expected sizes; mismatches yield [`CryptoError::InvalidKeyLength`] or
/// [`CryptoError::InvalidSignatureLength`] rather than panicking.
pub trait SignatureVerifier {
    /// Which algorithm this verifier implements.
    const ALGORITHM: SignatureAlgorithm;
    /// Expected public key length in bytes.
    const PUBLIC_KEY_LEN: usize;
    /// Maximum signature length in bytes (some schemes are variable-length).
    const SIGNATURE_MAX_LEN: usize;

    /// Verify `signature` over `message` under `public_key`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::SignatureInvalid`] if verification fails, or a
    /// length error if the key or signature is malformed.
    fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError>;
}

/// A signature *signer*. Only available with the `std` feature, since signing
/// is a build-time/tooling operation that runs with a full standard library.
#[cfg(feature = "std")]
pub trait SignatureSigner {
    /// Which algorithm this signer implements.
    const ALGORITHM: SignatureAlgorithm;
    /// Expected secret key length in bytes.
    const SECRET_KEY_LEN: usize;

    /// Sign `message` with `secret_key`, appending the signature bytes to
    /// `out`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::SigningFailed`] on any signing error, or
    /// [`CryptoError::InvalidKeyLength`] if the secret key is malformed.
    fn sign(
        secret_key: &[u8],
        message: &[u8],
        out: &mut alloc::vec::Vec<u8>,
    ) -> Result<(), CryptoError>;
}

/// Verify a signature given a runtime-selected algorithm, dispatching to the
/// compiled-in backend. Returns [`CryptoError::AlgorithmUnavailable`] if the
/// requested algorithm's backend was not compiled into this build.
///
/// This is the entry point firmware and kernel use when the algorithm is read
/// from a handoff/header field rather than known statically.
///
/// # Errors
///
/// Propagates the verifier's error, or returns
/// [`CryptoError::AlgorithmUnavailable`] when the backend is absent.
pub fn verify_with(
    algorithm: SignatureAlgorithm,
    public_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    match algorithm {
        SignatureAlgorithm::SphincsPlus => {
            // Prefer the std backend when present; fall back to the no_std
            // portable verifier (bare firmware/kernel) when only that is compiled
            // in. Both produce identical results for the same key/message/sig.
            #[cfg(feature = "pqc-sphincs")]
            {
                super::backends::sphincs::SphincsPlusVerifier::verify(
                    public_key, message, signature,
                )
            }
            #[cfg(all(not(feature = "pqc-sphincs"), feature = "pqc-sphincs-portable"))]
            {
                super::backends::sphincs_portable::SphincsPlusPortableVerifier::verify(
                    public_key, message, signature,
                )
            }
            #[cfg(all(not(feature = "pqc-sphincs"), not(feature = "pqc-sphincs-portable")))]
            {
                let _ = (public_key, message, signature);
                Err(CryptoError::AlgorithmUnavailable {
                    algorithm: "SPHINCS+",
                })
            }
        }
        SignatureAlgorithm::MlDsa => {
            #[cfg(feature = "pqc-mldsa")]
            {
                super::backends::mldsa::MlDsaVerifier::verify(public_key, message, signature)
            }
            #[cfg(not(feature = "pqc-mldsa"))]
            {
                let _ = (public_key, message, signature);
                Err(CryptoError::AlgorithmUnavailable {
                    algorithm: "ML-DSA",
                })
            }
        }
        SignatureAlgorithm::Ed25519 => {
            // Ed25519 backend is not part of the default feature set; it can be
            // added behind a `classical-crypto` feature when a deployment path
            // requires it. Absent that, report unavailability honestly.
            let _ = (public_key, message, signature);
            Err(CryptoError::AlgorithmUnavailable {
                algorithm: "Ed25519",
            })
        }
    }
}
