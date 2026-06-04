//! # Authentication Exchange Protocol
//!
//! The message envelopes for the authentication handshake between the platform
//! UI (which collects the secret) and the kernel authentication subsystem
//! (which verifies it).
//!
//! The flow is deliberately minimal:
//!
//! 1. The UI asks to authenticate a profile; the kernel issues an
//!    [`AuthChallenge`] containing a fresh random nonce.
//! 2. The UI gathers the secret (password and/or key-device material), forms an
//!    [`AuthResponse`] bound to that nonce, and submits it.
//! 3. The kernel verifies and returns an
//!    [`crate::types::authentication::AuthenticationOutcome`].
//!
//! The nonce makes a captured response useless on a later exchange (replay
//! protection). Secret *material* never appears in these types — the response
//! carries only a verifier output (a signature over the nonce, or a
//! password-derived proof), never the secret itself.

use crate::types::authentication::{AuthenticationMethod, AuthenticationRequest};
use crate::types::error::ProtocolError;
use crate::types::isolation::BoundaryId;

/// Length of the authentication challenge nonce, in bytes.
pub const AUTH_NONCE_LEN: usize = 32;

/// Maximum length of a response proof, in bytes. Sized to hold the largest
/// supported signature proof (an ML-DSA signature) plus framing.
pub const MAX_AUTH_PROOF_LEN: usize = 4096;

/// A challenge issued by the kernel to begin an authentication exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthChallenge {
    /// The profile the exchange will authenticate.
    pub target: BoundaryId,
    /// A fresh random nonce the response must be bound to.
    pub nonce: [u8; AUTH_NONCE_LEN],
    /// The method the profile requires, so the UI knows what to collect.
    pub required_method: AuthenticationMethod,
}

impl AuthChallenge {
    /// Construct a challenge.
    #[must_use]
    pub const fn new(
        target: BoundaryId,
        nonce: [u8; AUTH_NONCE_LEN],
        required_method: AuthenticationMethod,
    ) -> Self {
        Self {
            target,
            nonce,
            required_method,
        }
    }
}

/// A proof of possession submitted in response to an [`AuthChallenge`].
///
/// The proof bytes are a fixed-capacity buffer plus a length, so the type is
/// `Copy` and allocation-free. The interpretation of the proof depends on the
/// method: for a key device it is a signature over the challenge nonce; for a
/// password it is a KDF-derived proof bound to the nonce. The raw secret is
/// never carried.
#[derive(Debug, Clone, Copy)]
pub struct AuthResponse {
    /// The profile being authenticated (must match the challenge).
    pub target: BoundaryId,
    /// The nonce from the challenge, echoed back for binding.
    pub nonce: [u8; AUTH_NONCE_LEN],
    /// The method used to produce the proof.
    pub method: AuthenticationMethod,
    /// Number of valid bytes in [`Self::proof`].
    pub proof_len: u32,
    /// The proof bytes (signature or derived proof), zero-padded.
    pub proof: [u8; MAX_AUTH_PROOF_LEN],
}

impl AuthResponse {
    /// Construct a response, copying `proof` into the fixed buffer.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::MessageTooLarge`] if `proof` exceeds
    /// [`MAX_AUTH_PROOF_LEN`].
    pub fn new(
        target: BoundaryId,
        nonce: [u8; AUTH_NONCE_LEN],
        method: AuthenticationMethod,
        proof: &[u8],
    ) -> Result<Self, ProtocolError> {
        if proof.len() > MAX_AUTH_PROOF_LEN {
            return Err(ProtocolError::MessageTooLarge {
                size: proof.len(),
                maximum: MAX_AUTH_PROOF_LEN,
            });
        }
        let mut buf = [0u8; MAX_AUTH_PROOF_LEN];
        buf[..proof.len()].copy_from_slice(proof);
        Ok(Self {
            target,
            nonce,
            method,
            proof_len: proof.len() as u32,
            proof: buf,
        })
    }

    /// The valid proof bytes.
    #[must_use]
    pub fn proof_bytes(&self) -> &[u8] {
        &self.proof[..self.proof_len as usize]
    }

    /// Whether this response is well-formed against the given challenge: same
    /// target, same nonce, and a method satisfying what the challenge required.
    #[must_use]
    pub fn matches_challenge(&self, challenge: &AuthChallenge) -> bool {
        self.target == challenge.target
            && self.nonce == challenge.nonce
            && method_satisfies(self.method, challenge.required_method)
    }

    /// Convert the request form for downstream verification.
    #[must_use]
    pub fn as_request(&self) -> AuthenticationRequest {
        AuthenticationRequest {
            target: self.target,
            method: self.method,
            interface: None,
        }
    }
}

/// Whether a response `method` satisfies the `required` method. An exact match
/// always satisfies; a combined requirement is satisfied only by the combined
/// method.
#[must_use]
pub const fn method_satisfies(
    provided: AuthenticationMethod,
    required: AuthenticationMethod,
) -> bool {
    matches!(
        (provided, required),
        (AuthenticationMethod::Password, AuthenticationMethod::Password)
            | (
                AuthenticationMethod::PhysicalKeyDevice,
                AuthenticationMethod::PhysicalKeyDevice
            )
            | (
                AuthenticationMethod::PasswordAndKeyDevice,
                AuthenticationMethod::PasswordAndKeyDevice
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_binds_to_challenge() {
        let target = BoundaryId::new(7);
        let nonce = [9u8; AUTH_NONCE_LEN];
        let challenge =
            AuthChallenge::new(target, nonce, AuthenticationMethod::PhysicalKeyDevice);

        let resp = AuthResponse::new(
            target,
            nonce,
            AuthenticationMethod::PhysicalKeyDevice,
            &[1, 2, 3, 4],
        )
        .unwrap();

        assert!(resp.matches_challenge(&challenge));
        assert_eq!(resp.proof_bytes(), &[1, 2, 3, 4]);

        // Wrong nonce breaks the binding.
        let mut bad = resp;
        bad.nonce[0] ^= 1;
        assert!(!bad.matches_challenge(&challenge));
    }

    #[test]
    fn oversized_proof_rejected() {
        let r = AuthResponse::new(
            BoundaryId::new(1),
            [0u8; AUTH_NONCE_LEN],
            AuthenticationMethod::Password,
            &[0u8; MAX_AUTH_PROOF_LEN + 1],
        );
        assert!(r.is_err());
    }

    #[test]
    fn method_satisfaction() {
        assert!(method_satisfies(
            AuthenticationMethod::Password,
            AuthenticationMethod::Password
        ));
        assert!(!method_satisfies(
            AuthenticationMethod::Password,
            AuthenticationMethod::PasswordAndKeyDevice
        ));
    }
}
