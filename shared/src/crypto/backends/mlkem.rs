//! # ML-KEM Backend
//!
//! Lattice-based post-quantum key encapsulation (ML-KEM / Kyber, FIPS 203).
//! Establishes the shared secret used to key channel confidentiality on
//! profiles that encrypt IPC.
//!
//! Wraps `pqcrypto-mlkem` using the `mlkem768` parameter set (NIST security
//! category 3). Compiled only when the `pqc-mlkem` feature is enabled.

use crate::crypto::kem::{KemAlgorithm, KeyEncapsulation};
use crate::types::error::CryptoError;

use pqcrypto_mlkem::mlkem768 as mk;
use pqcrypto_traits::kem::{
    Ciphertext as _, PublicKey as _, SecretKey as _, SharedSecret as _,
};

/// Public key length for `mlkem768`, in bytes (FIPS 203 ML-KEM-768).
pub const PUBLIC_KEY_LEN: usize = 1184;
/// Secret key length for `mlkem768`, in bytes.
pub const SECRET_KEY_LEN: usize = 2400;
/// Ciphertext length for `mlkem768`, in bytes.
pub const CIPHERTEXT_LEN: usize = 1088;
/// Shared secret length for `mlkem768`, in bytes.
pub const SHARED_SECRET_LEN: usize = 32;

/// ML-KEM key-encapsulation mechanism.
pub struct MlKem768;

impl KeyEncapsulation for MlKem768 {
    const ALGORITHM: KemAlgorithm = KemAlgorithm::MlKem768;
    const PUBLIC_KEY_LEN: usize = PUBLIC_KEY_LEN;
    const SECRET_KEY_LEN: usize = SECRET_KEY_LEN;
    const CIPHERTEXT_LEN: usize = CIPHERTEXT_LEN;
    const SHARED_SECRET_LEN: usize = SHARED_SECRET_LEN;

    fn encapsulate(
        public_key: &[u8],
        ciphertext: &mut [u8],
        shared_secret: &mut [u8],
    ) -> Result<(), CryptoError> {
        if public_key.len() != PUBLIC_KEY_LEN {
            return Err(CryptoError::InvalidKeyLength {
                expected: PUBLIC_KEY_LEN,
                actual: public_key.len(),
            });
        }
        if ciphertext.len() != CIPHERTEXT_LEN {
            return Err(CryptoError::KeyEncapsulationFailed {
                detail: "ciphertext buffer wrong size",
            });
        }
        if shared_secret.len() != SHARED_SECRET_LEN {
            return Err(CryptoError::KeyEncapsulationFailed {
                detail: "shared secret buffer wrong size",
            });
        }

        let pk = mk::PublicKey::from_bytes(public_key).map_err(|_| {
            CryptoError::InvalidKeyLength {
                expected: PUBLIC_KEY_LEN,
                actual: public_key.len(),
            }
        })?;

        let (ss, ct) = mk::encapsulate(&pk);
        ciphertext.copy_from_slice(ct.as_bytes());
        shared_secret.copy_from_slice(ss.as_bytes());
        Ok(())
    }

    fn decapsulate(
        secret_key: &[u8],
        ciphertext: &[u8],
        shared_secret: &mut [u8],
    ) -> Result<(), CryptoError> {
        if secret_key.len() != SECRET_KEY_LEN {
            return Err(CryptoError::InvalidKeyLength {
                expected: SECRET_KEY_LEN,
                actual: secret_key.len(),
            });
        }
        if shared_secret.len() != SHARED_SECRET_LEN {
            return Err(CryptoError::KeyEncapsulationFailed {
                detail: "shared secret buffer wrong size",
            });
        }

        let sk = mk::SecretKey::from_bytes(secret_key).map_err(|_| {
            CryptoError::InvalidKeyLength {
                expected: SECRET_KEY_LEN,
                actual: secret_key.len(),
            }
        })?;
        let ct = mk::Ciphertext::from_bytes(ciphertext).map_err(|_| {
            CryptoError::KeyEncapsulationFailed {
                detail: "malformed ciphertext",
            }
        })?;

        let ss = mk::decapsulate(&ct, &sk);
        shared_secret.copy_from_slice(ss.as_bytes());
        Ok(())
    }
}

/// Generate a fresh ML-KEM keypair as `(public_key, secret_key)` byte vectors.
/// Available with `std` (the kernel side allocates these during channel setup).
///
/// # Errors
///
/// Infallible at the wrapper level; returns `Result` for API symmetry.
#[cfg(feature = "std")]
pub fn generate_keypair() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), CryptoError> {
    let (pk, sk) = mk::keypair();
    Ok((pk.as_bytes().to_vec(), sk.as_bytes().to_vec()))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn encapsulate_decapsulate_agree() {
        let (pk, sk) = generate_keypair().expect("keypair");
        assert_eq!(pk.len(), PUBLIC_KEY_LEN);
        assert_eq!(sk.len(), SECRET_KEY_LEN);

        let mut ct = [0u8; CIPHERTEXT_LEN];
        let mut ss_sender = [0u8; SHARED_SECRET_LEN];
        MlKem768::encapsulate(&pk, &mut ct, &mut ss_sender).expect("encapsulate");

        let mut ss_receiver = [0u8; SHARED_SECRET_LEN];
        MlKem768::decapsulate(&sk, &ct, &mut ss_receiver).expect("decapsulate");

        // Both parties derive the identical shared secret.
        assert_eq!(ss_sender, ss_receiver);

        // A corrupted ciphertext must not yield the sender's secret.
        let mut bad_ct = ct;
        bad_ct[0] ^= 0xFF;
        let mut ss_bad = [0u8; SHARED_SECRET_LEN];
        MlKem768::decapsulate(&sk, &bad_ct, &mut ss_bad).expect("decapsulate runs");
        assert_ne!(ss_sender, ss_bad);
    }
}
