//! # Key Encapsulation
//!
//! The key-encapsulation abstraction used to establish shared secrets for
//! channel confidentiality on profiles that encrypt IPC.
//!
//! As with signatures, the trait layer always compiles and concrete backends
//! (ML-KEM / Kyber) are feature-gated. Key encapsulation is inherently a
//! two-party, allocation-touching operation, so unlike signature *verification*
//! it is expected to run in the kernel (which has an allocator), not in the
//! pre-allocator firmware phase.

use crate::types::error::{CryptoError, SerializationError};

/// Identifier for a key-encapsulation algorithm, stable on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum KemAlgorithm {
    /// ML-KEM-768 (Kyber), NIST security category 3.
    MlKem768 = 1,
}

impl KemAlgorithm {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Whether this algorithm is post-quantum (all current KEMs here are).
    #[must_use]
    pub const fn is_post_quantum(self) -> bool {
        matches!(self, KemAlgorithm::MlKem768)
    }
}

impl TryFrom<u32> for KemAlgorithm {
    type Error = SerializationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(KemAlgorithm::MlKem768),
            _ => Err(SerializationError::InvalidValue {
                field: "KemAlgorithm",
            }),
        }
    }
}

/// A key-encapsulation mechanism.
///
/// The KEM flow:
/// 1. The receiver generates a keypair and publishes its public key.
/// 2. The sender *encapsulates* against that public key, producing a ciphertext
///    and a shared secret.
/// 3. The receiver *decapsulates* the ciphertext with its secret key, recovering
///    the same shared secret.
///
/// Buffers are caller-provided fixed slices sized to the algorithm constants,
/// keeping the trait usable without forcing allocation at the call site.
pub trait KeyEncapsulation {
    /// Which algorithm this implements.
    const ALGORITHM: KemAlgorithm;
    /// Public key length in bytes.
    const PUBLIC_KEY_LEN: usize;
    /// Secret key length in bytes.
    const SECRET_KEY_LEN: usize;
    /// Ciphertext length in bytes.
    const CIPHERTEXT_LEN: usize;
    /// Shared secret length in bytes.
    const SHARED_SECRET_LEN: usize;

    /// Encapsulate against `public_key`, writing the ciphertext to `ciphertext`
    /// and the derived shared secret to `shared_secret`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::KeyEncapsulationFailed`] on failure, or a length
    /// error if any buffer is mis-sized.
    fn encapsulate(
        public_key: &[u8],
        ciphertext: &mut [u8],
        shared_secret: &mut [u8],
    ) -> Result<(), CryptoError>;

    /// Decapsulate `ciphertext` with `secret_key`, writing the recovered shared
    /// secret to `shared_secret`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::KeyEncapsulationFailed`] on failure, or a length
    /// error if any buffer is mis-sized.
    fn decapsulate(
        secret_key: &[u8],
        ciphertext: &[u8],
        shared_secret: &mut [u8],
    ) -> Result<(), CryptoError>;
}
