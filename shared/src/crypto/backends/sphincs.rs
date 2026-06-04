//! # SPHINCS+ Backend
//!
//! Hash-based post-quantum signatures, used as the system's root of trust for
//! boot and firmware verification. SPHINCS+ rests only on the security of its
//! underlying hash function — the most conservative ("old math") assumption
//! available — which is why the white paper selects it for the trust anchor.
//!
//! This backend wraps `pqcrypto-sphincsplus` using the
//! `sphincssha2128fsimple` parameter set (NIST security category 1, the "fast"
//! variant). Verification is exposed for `no_std` consumers; signing is gated
//! behind `std` because it is a build-time/tooling operation.
//!
//! Compiled only when the `pqc-sphincs` feature is enabled.

use crate::crypto::signature::{SignatureAlgorithm, SignatureVerifier};
use crate::types::error::CryptoError;

use pqcrypto_sphincsplus::sphincssha2128fsimple as sp;
use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _, SecretKey as _};

/// Public key length for `sphincssha2128fsimple`, in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Secret key length for `sphincssha2128fsimple`, in bytes.
pub const SECRET_KEY_LEN: usize = 64;
/// Signature length for `sphincssha2128fsimple`, in bytes.
pub const SIGNATURE_LEN: usize = 17088;

/// SPHINCS+ signature verifier.
pub struct SphincsPlusVerifier;

impl SignatureVerifier for SphincsPlusVerifier {
    const ALGORITHM: SignatureAlgorithm = SignatureAlgorithm::SphincsPlus;
    const PUBLIC_KEY_LEN: usize = PUBLIC_KEY_LEN;
    const SIGNATURE_MAX_LEN: usize = SIGNATURE_LEN;

    fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        if public_key.len() != PUBLIC_KEY_LEN {
            return Err(CryptoError::InvalidKeyLength {
                expected: PUBLIC_KEY_LEN,
                actual: public_key.len(),
            });
        }
        if signature.len() != SIGNATURE_LEN {
            return Err(CryptoError::InvalidSignatureLength {
                expected: SIGNATURE_LEN,
                actual: signature.len(),
            });
        }

        let pk = sp::PublicKey::from_bytes(public_key)
            .map_err(|_| CryptoError::InvalidKeyLength {
                expected: PUBLIC_KEY_LEN,
                actual: public_key.len(),
            })?;
        let sig = sp::DetachedSignature::from_bytes(signature).map_err(|_| {
            CryptoError::InvalidSignatureLength {
                expected: SIGNATURE_LEN,
                actual: signature.len(),
            }
        })?;

        match sp::verify_detached_signature(&sig, message, &pk) {
            Ok(()) => Ok(()),
            Err(_) => Err(CryptoError::SignatureInvalid),
        }
    }
}

/// SPHINCS+ signer (build-time tooling only).
#[cfg(feature = "std")]
pub struct SphincsPlusSigner;

#[cfg(feature = "std")]
impl crate::crypto::signature::SignatureSigner for SphincsPlusSigner {
    const ALGORITHM: SignatureAlgorithm = SignatureAlgorithm::SphincsPlus;
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
        let sk = sp::SecretKey::from_bytes(secret_key).map_err(|_| {
            CryptoError::InvalidKeyLength {
                expected: SECRET_KEY_LEN,
                actual: secret_key.len(),
            }
        })?;
        let sig = sp::detached_sign(message, &sk);
        out.extend_from_slice(sig.as_bytes());
        Ok(())
    }
}

/// Generate a fresh SPHINCS+ keypair, returning `(public_key, secret_key)` as
/// owned byte vectors. Build-time tooling only.
///
/// # Errors
///
/// Currently infallible at the wrapper level, but returns `Result` for API
/// symmetry and forward compatibility.
#[cfg(feature = "std")]
pub fn generate_keypair() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), CryptoError> {
    let (pk, sk) = sp::keypair();
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

        let msg = b"CIBOS kernel image v1";
        let mut sig = Vec::new();
        SphincsPlusSigner::sign(&sk, msg, &mut sig).expect("sign");
        assert_eq!(sig.len(), SIGNATURE_LEN);

        // Correct signature verifies.
        SphincsPlusVerifier::verify(&pk, msg, &sig).expect("verify ok");

        // Tampered message fails.
        let bad_msg = b"CIBOS kernel image v2";
        assert!(SphincsPlusVerifier::verify(&pk, bad_msg, &sig).is_err());

        // Tampered signature fails.
        let mut bad_sig = sig.clone();
        bad_sig[0] ^= 0xFF;
        assert!(SphincsPlusVerifier::verify(&pk, msg, &bad_sig).is_err());

        // Wrong-length key is rejected with a length error, not a panic.
        assert!(SphincsPlusVerifier::verify(&[0u8; 4], msg, &sig).is_err());
    }
}
