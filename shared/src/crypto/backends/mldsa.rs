//! # ML-DSA Backend
//!
//! Lattice-based post-quantum signatures (ML-DSA / Dilithium, FIPS 204). Used
//! for channel/message authenticity on profiles that sign IPC, where the larger
//! throughput and smaller signatures relative to SPHINCS+ matter and the
//! lattice security assumption is acceptable.
//!
//! Wraps `pqcrypto-mldsa` using the `mldsa65` parameter set (NIST security
//! category 3). Verification is `no_std`; signing is gated behind `std`.
//!
//! Compiled only when the `pqc-mldsa` feature is enabled.

use crate::crypto::signature::{SignatureAlgorithm, SignatureVerifier};
use crate::types::error::CryptoError;

use pqcrypto_mldsa::mldsa65 as md;
use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _, SecretKey as _};

/// Public key length for `mldsa65`, in bytes (FIPS 204 ML-DSA-65).
pub const PUBLIC_KEY_LEN: usize = 1952;
/// Secret key length for `mldsa65`, in bytes.
pub const SECRET_KEY_LEN: usize = 4032;
/// Maximum signature length for `mldsa65`, in bytes.
pub const SIGNATURE_LEN: usize = 3309;

/// ML-DSA signature verifier.
pub struct MlDsaVerifier;

impl SignatureVerifier for MlDsaVerifier {
    const ALGORITHM: SignatureAlgorithm = SignatureAlgorithm::MlDsa;
    const PUBLIC_KEY_LEN: usize = PUBLIC_KEY_LEN;
    const SIGNATURE_MAX_LEN: usize = SIGNATURE_LEN;

    fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        if public_key.len() != PUBLIC_KEY_LEN {
            return Err(CryptoError::InvalidKeyLength {
                expected: PUBLIC_KEY_LEN,
                actual: public_key.len(),
            });
        }

        let pk = md::PublicKey::from_bytes(public_key).map_err(|_| {
            CryptoError::InvalidKeyLength {
                expected: PUBLIC_KEY_LEN,
                actual: public_key.len(),
            }
        })?;
        let sig = md::DetachedSignature::from_bytes(signature).map_err(|_| {
            CryptoError::InvalidSignatureLength {
                expected: SIGNATURE_LEN,
                actual: signature.len(),
            }
        })?;

        match md::verify_detached_signature(&sig, message, &pk) {
            Ok(()) => Ok(()),
            Err(_) => Err(CryptoError::SignatureInvalid),
        }
    }
}

/// ML-DSA signer (build-time tooling only).
#[cfg(feature = "std")]
pub struct MlDsaSigner;

#[cfg(feature = "std")]
impl crate::crypto::signature::SignatureSigner for MlDsaSigner {
    const ALGORITHM: SignatureAlgorithm = SignatureAlgorithm::MlDsa;
    const SECRET_KEY_LEN: usize = SECRET_KEY_LEN;

    fn sign(
        secret_key: &[u8],
        message: &[u8],
        out: &mut alloc::vec::Vec<u8>,
    ) -> Result<(), CryptoError> {
        if secret_key.len() != SECRET_KEY_LEN {
            return Err(CryptoError::InvalidKeyLength {
                expected: SECRET_KEY_LEN,
                actual: secret_key.len(),
            });
        }
        let sk = md::SecretKey::from_bytes(secret_key).map_err(|_| {
            CryptoError::InvalidKeyLength {
                expected: SECRET_KEY_LEN,
                actual: secret_key.len(),
            }
        })?;
        let sig = md::detached_sign(message, &sk);
        out.extend_from_slice(sig.as_bytes());
        Ok(())
    }
}

/// Generate a fresh ML-DSA keypair as `(public_key, secret_key)` byte vectors.
/// Build-time tooling only.
///
/// # Errors
///
/// Infallible at the wrapper level; returns `Result` for API symmetry.
#[cfg(feature = "std")]
pub fn generate_keypair() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), CryptoError> {
    let (pk, sk) = md::keypair();
    Ok((pk.as_bytes().to_vec(), sk.as_bytes().to_vec()))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::crypto::signature::{SignatureSigner, SignatureVerifier};
    use alloc::vec::Vec;

    #[test]
    fn sign_verify_roundtrip() {
        let (pk, sk) = generate_keypair().expect("keypair");
        assert_eq!(pk.len(), PUBLIC_KEY_LEN);
        assert_eq!(sk.len(), SECRET_KEY_LEN);

        let msg = b"channel handshake transcript";
        let mut sig = Vec::new();
        MlDsaSigner::sign(&sk, msg, &mut sig).expect("sign");

        MlDsaVerifier::verify(&pk, msg, &sig).expect("verify ok");

        let mut bad_sig = sig.clone();
        bad_sig[0] ^= 0xFF;
        assert!(MlDsaVerifier::verify(&pk, msg, &bad_sig).is_err());

        assert!(MlDsaVerifier::verify(&[0u8; 4], msg, &sig).is_err());
    }
}
