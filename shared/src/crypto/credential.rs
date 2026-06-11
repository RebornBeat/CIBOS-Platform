//! Password credentials — the one definition of how CIBOS stores and checks a
//! password, shared by the host account registry and the on-kernel login
//! application so a credential written by one verifies under the other.
//!
//! The scheme is intentionally simple and `no_std`: a per-profile 32-byte salt
//! and the SHA-256 of `salt ++ password`, compared in constant time. This is the
//! same construction the `accounts` crate uses; keeping it here means there is a
//! single implementation rather than two that could drift apart.
//!
//! ## On-disk record
//!
//! [`CredentialRecord`] is a fixed 68-byte layout written to a file in the
//! kernel filesystem (e.g. `/etc/passwd.d/<name>`):
//!
//! ```text
//!   magic   u32   "CPW1" little-endian
//!   version u32   1
//!   salt    [u8; 32]
//!   hash    [u8; 32]   sha256(salt ++ password)
//! ```

use crate::crypto::hash::{digests_equal_ct, Digest256};

/// Salt length in bytes.
pub const SALT_LEN: usize = 32;
/// Encoded [`CredentialRecord`] length in bytes.
pub const CREDENTIAL_RECORD_LEN: usize = 4 + 4 + SALT_LEN + 32;
/// Record magic: ASCII `"CPW1"`.
pub const CREDENTIAL_MAGIC: u32 = u32::from_le_bytes(*b"CPW1");
/// Record version.
pub const CREDENTIAL_VERSION: u32 = 1;

/// Compute the stored verifier for `password` under `salt`: `sha256(salt ++
/// password)`. This is the canonical construction used everywhere in CIBOS.
#[must_use]
pub fn hash_password(salt: &[u8; SALT_LEN], password: &[u8]) -> Digest256 {
    // Hash incrementally so any password length works without allocation. Feeding
    // salt then password is exactly `sha256(salt ++ password)`.
    let mut h = crate::crypto::hash::IncrementalSha256::new();
    h.update(salt);
    h.update(password);
    h.finalize()
}

/// Verify `password` against a stored `(salt, hash)` in constant time.
#[must_use]
pub fn verify_password(salt: &[u8; SALT_LEN], stored: &Digest256, password: &[u8]) -> bool {
    let computed = hash_password(salt, password);
    digests_equal_ct(stored, &computed)
}

/// A password credential record (salt + verifier hash) with a fixed on-disk
/// encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialRecord {
    /// Per-profile random salt.
    pub salt: [u8; SALT_LEN],
    /// `sha256(salt ++ password)`.
    pub hash: Digest256,
}

impl CredentialRecord {
    /// Build a record for `password` under `salt`.
    #[must_use]
    pub fn new(salt: [u8; SALT_LEN], password: &[u8]) -> Self {
        let hash = hash_password(&salt, password);
        CredentialRecord { salt, hash }
    }

    /// Verify `password` against this record (constant time).
    #[must_use]
    pub fn verify(&self, password: &[u8]) -> bool {
        verify_password(&self.salt, &self.hash, password)
    }

    /// Encode to the fixed [`CREDENTIAL_RECORD_LEN`]-byte layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; CREDENTIAL_RECORD_LEN] {
        let mut b = [0u8; CREDENTIAL_RECORD_LEN];
        b[0..4].copy_from_slice(&CREDENTIAL_MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&CREDENTIAL_VERSION.to_le_bytes());
        b[8..8 + SALT_LEN].copy_from_slice(&self.salt);
        b[8 + SALT_LEN..CREDENTIAL_RECORD_LEN].copy_from_slice(&self.hash);
        b
    }

    /// Decode from bytes. Returns `None` on a bad magic/version/length.
    #[must_use]
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < CREDENTIAL_RECORD_LEN {
            return None;
        }
        let magic = u32::from_le_bytes(b[0..4].try_into().ok()?);
        let version = u32::from_le_bytes(b[4..8].try_into().ok()?);
        if magic != CREDENTIAL_MAGIC || version != CREDENTIAL_VERSION {
            return None;
        }
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&b[8..8 + SALT_LEN]);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&b[8 + SALT_LEN..CREDENTIAL_RECORD_LEN]);
        Some(CredentialRecord { salt, hash })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash::sha256;

    #[test]
    fn hash_matches_salt_plus_password_construction() {
        let salt = [7u8; SALT_LEN];
        // Reference: sha256(salt ++ password) assembled directly.
        let mut buf = alloc::vec::Vec::new();
        buf.extend_from_slice(&salt);
        buf.extend_from_slice(b"correct horse");
        let reference = sha256(&buf);
        assert_eq!(hash_password(&salt, b"correct horse"), reference);
    }

    #[test]
    fn verify_accepts_correct_rejects_wrong() {
        let salt = [3u8; SALT_LEN];
        let rec = CredentialRecord::new(salt, b"hunter2");
        assert!(rec.verify(b"hunter2"));
        assert!(!rec.verify(b"hunter3"));
        assert!(!rec.verify(b""));
    }

    #[test]
    fn record_roundtrips_through_bytes() {
        let rec = CredentialRecord::new([9u8; SALT_LEN], b"swordfish");
        let bytes = rec.to_bytes();
        assert_eq!(bytes.len(), CREDENTIAL_RECORD_LEN);
        let back = CredentialRecord::from_bytes(&bytes).unwrap();
        assert_eq!(back, rec);
        assert!(back.verify(b"swordfish"));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = CredentialRecord::new([1u8; SALT_LEN], b"x").to_bytes();
        bytes[0] ^= 0xFF;
        assert_eq!(CredentialRecord::from_bytes(&bytes), None);
    }
}
