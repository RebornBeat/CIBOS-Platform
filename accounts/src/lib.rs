//! # Accounts
//!
//! CIBOS has no traditional multi-user account system. The security principal is
//! the **isolation boundary**: running code is contained by its boundary, not by
//! a user id. What remains is *human* authentication — proving a person may
//! activate a given profile (and thus its boundary). This crate is that:
//! enrollment of profiles and verification of credentials, tying a successful
//! authentication to a [`BoundaryId`].
//!
//! Two authentication factors are supported, matching `shared`'s wired-only
//! policy — there are deliberately no wireless or biometric options:
//!
//! * **Password** — verified against a salted SHA-256 hash, compared in
//!   constant time.
//! * **Physical key device** — a wired device that signs a server-issued
//!   challenge; verified with a post-quantum (SPHINCS+) signature.
//!
//! A profile may require either factor or **both** (`PasswordAndKeyDevice`), in
//! which case both must verify.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// The key-device path verifies a SPHINCS+ signature. Use the std-side backend
// on host (the `std` feature pulls `shared/pqc-sphincs`) and the no_std portable
// verifier on bare (the `portable-pqc` feature pulls `shared/pqc-sphincs-portable`);
// both implement the same `SignatureVerifier` trait and produce identical results.
#[cfg(feature = "std")]
use shared::crypto::backends::sphincs::SphincsPlusVerifier as KeyDeviceVerifier;
#[cfg(all(not(feature = "std"), feature = "portable-pqc"))]
use shared::crypto::backends::sphincs_portable::SphincsPlusPortableVerifier as KeyDeviceVerifier;
use shared::crypto::credential::{hash_password, CredentialRecord};
use shared::crypto::hash::{digests_equal_ct, Digest256};
#[cfg(any(feature = "std", feature = "portable-pqc"))]
use shared::crypto::signature::SignatureVerifier;
use shared::types::authentication::AuthenticationFailureReason;
use shared::{AuthenticationMethod, AuthenticationOutcome, BoundaryId};

/// Compute the stored verifier hash for a password and salt.
///
/// This delegates to [`shared::crypto::credential::hash_password`] — the single
/// canonical `sha256(salt ++ password)` construction shared across CIBOS — so
/// the in-memory registry here and the on-disk [`CredentialRecord`] used by the
/// kernel login path are guaranteed byte-identical and cannot drift.
fn password_hash(salt: &[u8; 32], password: &[u8]) -> Digest256 {
    hash_password(salt, password)
}

/// The stored verifier material for a profile.
enum Verifier {
    Password {
        salt: [u8; 32],
        hash: Digest256,
    },
    KeyDevice {
        public_key: Vec<u8>,
    },
    Both {
        salt: [u8; 32],
        hash: Digest256,
        public_key: Vec<u8>,
    },
}

/// An enrolled profile: a name, the boundary it unlocks, its method, and the
/// verifier material (never the secret itself).
pub struct Profile {
    /// Human-readable profile name.
    pub name: String,
    /// The isolation boundary this profile activates.
    pub boundary: BoundaryId,
    /// The authentication method required.
    pub method: AuthenticationMethod,
    verifier: Verifier,
}

/// A credential presented at authentication time.
pub enum Credential<'a> {
    /// A password.
    Password(&'a [u8]),
    /// A key-device challenge/response: the challenge that was issued and the
    /// device's signature over it.
    KeyDevice {
        /// The challenge that was issued to the device.
        challenge: &'a [u8],
        /// The device's signature over the challenge.
        signature: &'a [u8],
    },
    /// Both factors together.
    Both {
        /// The password.
        password: &'a [u8],
        /// The issued challenge.
        challenge: &'a [u8],
        /// The device's signature over the challenge.
        signature: &'a [u8],
    },
}

/// A successful authentication: the activated boundary and the profile name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The activated boundary.
    pub boundary: BoundaryId,
    /// The profile that was unlocked.
    pub profile: String,
}

/// The registry of enrolled profiles.
#[derive(Default)]
pub struct Accounts {
    profiles: BTreeMap<u64, Profile>,
}

impl Accounts {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Accounts::default()
    }

    /// Enroll a password-only profile.
    pub fn enroll_password(
        &mut self,
        name: &str,
        boundary: BoundaryId,
        salt: [u8; 32],
        password: &[u8],
    ) {
        let hash = password_hash(&salt, password);
        self.profiles.insert(
            boundary.0,
            Profile {
                name: name.to_string(),
                boundary,
                method: AuthenticationMethod::Password,
                verifier: Verifier::Password { salt, hash },
            },
        );
    }

    /// Enroll a password-only profile directly from a persisted
    /// [`CredentialRecord`] (e.g. loaded from `/etc/passwd.d/<name>` in the
    /// kernel filesystem). This is the persistence counterpart of
    /// [`Self::enroll_password`]: it stores the already-hashed verifier without
    /// re-hashing, so a record written by the on-kernel login path enrolls here
    /// byte-for-byte identically.
    pub fn enroll_password_record(
        &mut self,
        name: &str,
        boundary: BoundaryId,
        record: &CredentialRecord,
    ) {
        self.profiles.insert(
            boundary.0,
            Profile {
                name: name.to_string(),
                boundary,
                method: AuthenticationMethod::Password,
                verifier: Verifier::Password {
                    salt: record.salt,
                    hash: record.hash,
                },
            },
        );
    }

    /// Export a password profile's verifier as a [`CredentialRecord`] for
    /// persistence (e.g. to write to the kernel filesystem). Returns `None` if
    /// the profile is absent or is not a password (or password+key) profile.
    /// For a `PasswordAndKeyDevice` profile this exports only the password
    /// factor's record (the key-device public key is persisted separately).
    #[must_use]
    pub fn password_record_for(&self, boundary: BoundaryId) -> Option<CredentialRecord> {
        match self.profiles.get(&boundary.0).map(|p| &p.verifier)? {
            Verifier::Password { salt, hash } | Verifier::Both { salt, hash, .. } => {
                Some(CredentialRecord {
                    salt: *salt,
                    hash: *hash,
                })
            }
            Verifier::KeyDevice { .. } => None,
        }
    }

    /// Enroll a key-device-only profile with the device's public key.
    pub fn enroll_key_device(&mut self, name: &str, boundary: BoundaryId, public_key: Vec<u8>) {
        self.profiles.insert(
            boundary.0,
            Profile {
                name: name.to_string(),
                boundary,
                method: AuthenticationMethod::PhysicalKeyDevice,
                verifier: Verifier::KeyDevice { public_key },
            },
        );
    }

    /// Enroll a profile requiring both a password and a key device.
    pub fn enroll_password_and_key_device(
        &mut self,
        name: &str,
        boundary: BoundaryId,
        salt: [u8; 32],
        password: &[u8],
        public_key: Vec<u8>,
    ) {
        let hash = password_hash(&salt, password);
        self.profiles.insert(
            boundary.0,
            Profile {
                name: name.to_string(),
                boundary,
                method: AuthenticationMethod::PasswordAndKeyDevice,
                verifier: Verifier::Both {
                    salt,
                    hash,
                    public_key,
                },
            },
        );
    }

    /// The profile for a boundary, if enrolled.
    #[must_use]
    pub fn profile(&self, boundary: BoundaryId) -> Option<&Profile> {
        self.profiles.get(&boundary.0)
    }

    /// Resolve a profile name to its boundary, if enrolled.
    #[must_use]
    pub fn find_by_name(&self, name: &str) -> Option<BoundaryId> {
        self.profiles
            .values()
            .find(|p| p.name == name)
            .map(|p| p.boundary)
    }

    /// The authentication method a profile requires, if enrolled.
    #[must_use]
    pub fn method_for(&self, boundary: BoundaryId) -> Option<AuthenticationMethod> {
        self.profiles.get(&boundary.0).map(|p| p.method)
    }

    fn check_password(salt: &[u8; 32], hash: &Digest256, password: &[u8]) -> bool {
        let presented = password_hash(salt, password);
        digests_equal_ct(hash, &presented)
    }

    fn check_key_device(public_key: &[u8], challenge: &[u8], signature: &[u8]) -> bool {
        #[cfg(any(feature = "std", feature = "portable-pqc"))]
        {
            KeyDeviceVerifier::verify(public_key, challenge, signature).is_ok()
        }
        // With no SPHINCS+ backend compiled in, the key-device factor cannot be
        // verified, so it must fail closed (never silently accept).
        #[cfg(not(any(feature = "std", feature = "portable-pqc")))]
        {
            let _ = (public_key, challenge, signature);
            false
        }
    }

    /// Authenticate a credential against the profile for `boundary`.
    ///
    /// Returns an [`AuthenticationOutcome`]: `Success { boundary }` if the
    /// presented factor(s) match the enrolled method and verify, otherwise
    /// `Failure { reason }`.
    #[must_use]
    pub fn authenticate(
        &self,
        boundary: BoundaryId,
        credential: Credential<'_>,
    ) -> AuthenticationOutcome {
        let Some(profile) = self.profiles.get(&boundary.0) else {
            return AuthenticationOutcome::Failure {
                reason: AuthenticationFailureReason::UnknownProfile,
            };
        };

        let ok = match (&profile.verifier, credential) {
            (Verifier::Password { salt, hash }, Credential::Password(pw)) => {
                Self::check_password(salt, hash, pw)
            }
            (
                Verifier::KeyDevice { public_key },
                Credential::KeyDevice {
                    challenge,
                    signature,
                },
            ) => Self::check_key_device(public_key, challenge, signature),
            (
                Verifier::Both {
                    salt,
                    hash,
                    public_key,
                },
                Credential::Both {
                    password,
                    challenge,
                    signature,
                },
            ) => {
                // Both factors required; verify each.
                Self::check_password(salt, hash, password)
                    && Self::check_key_device(public_key, challenge, signature)
            }
            // Credential kind does not match the enrolled method.
            _ => {
                return AuthenticationOutcome::Failure {
                    reason: AuthenticationFailureReason::BadSecret,
                }
            }
        };

        if ok {
            AuthenticationOutcome::Success { boundary }
        } else {
            AuthenticationOutcome::Failure {
                reason: AuthenticationFailureReason::BadSecret,
            }
        }
    }

    /// Authenticate and, on success, produce a [`Session`].
    #[must_use]
    pub fn open_session(
        &self,
        boundary: BoundaryId,
        credential: Credential<'_>,
    ) -> Option<Session> {
        match self.authenticate(boundary, credential) {
            AuthenticationOutcome::Success { boundary } => self.profiles.get(&boundary.0).map(|p| {
                Session {
                    boundary,
                    profile: p.name.clone(),
                }
            }),
            AuthenticationOutcome::Failure { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::crypto::backends::sphincs::{generate_keypair, SphincsPlusSigner};
    use shared::crypto::signature::SignatureSigner;

    const SALT: [u8; 32] = [0x11; 32];

    #[test]
    fn password_success_and_failure() {
        let mut acc = Accounts::new();
        let b = BoundaryId::new(7);
        acc.enroll_password("operator", b, SALT, b"correct horse");

        assert!(acc
            .authenticate(b, Credential::Password(b"correct horse"))
            .is_success());
        assert!(!acc
            .authenticate(b, Credential::Password(b"wrong"))
            .is_success());
    }

    #[test]
    fn unknown_profile_fails() {
        let acc = Accounts::new();
        let outcome = acc.authenticate(BoundaryId::new(99), Credential::Password(b"x"));
        assert_eq!(
            outcome,
            AuthenticationOutcome::Failure {
                reason: AuthenticationFailureReason::UnknownProfile
            }
        );
    }

    #[test]
    fn wrong_factor_kind_is_rejected() {
        let mut acc = Accounts::new();
        let b = BoundaryId::new(3);
        acc.enroll_password("p", b, SALT, b"pw");
        // Presenting a key-device credential to a password profile fails.
        let outcome = acc.authenticate(
            b,
            Credential::KeyDevice {
                challenge: b"c",
                signature: b"s",
            },
        );
        assert!(!outcome.is_success());
    }

    #[test]
    fn key_device_challenge_response() {
        // Simulate a wired key device: it holds a SPHINCS+ keypair, the profile
        // is enrolled with the public key, and the device signs a challenge.
        let (public_key, secret_key) = generate_keypair().unwrap();
        let mut acc = Accounts::new();
        let b = BoundaryId::new(5);
        acc.enroll_key_device("keyholder", b, public_key);

        let challenge = b"server-issued-nonce-12345";
        let mut signature = Vec::new();
        SphincsPlusSigner::sign(&secret_key, challenge, &mut signature).unwrap();

        assert!(acc
            .authenticate(
                b,
                Credential::KeyDevice {
                    challenge,
                    signature: &signature,
                }
            )
            .is_success());

        // A signature over a different challenge must not verify.
        assert!(!acc
            .authenticate(
                b,
                Credential::KeyDevice {
                    challenge: b"different-nonce",
                    signature: &signature,
                }
            )
            .is_success());
    }

    #[test]
    fn two_factor_requires_both() {
        let (public_key, secret_key) = generate_keypair().unwrap();
        let mut acc = Accounts::new();
        let b = BoundaryId::new(9);
        acc.enroll_password_and_key_device("admin", b, SALT, b"hunter2", public_key);

        let challenge = b"nonce";
        let mut signature = Vec::new();
        SphincsPlusSigner::sign(&secret_key, challenge, &mut signature).unwrap();

        // Both correct -> success.
        assert!(acc
            .authenticate(
                b,
                Credential::Both {
                    password: b"hunter2",
                    challenge,
                    signature: &signature,
                }
            )
            .is_success());

        // Right password, bad signature -> failure.
        assert!(!acc
            .authenticate(
                b,
                Credential::Both {
                    password: b"hunter2",
                    challenge: b"nonce",
                    signature: b"not a signature",
                }
            )
            .is_success());

        // Wrong password, good signature -> failure.
        assert!(!acc
            .authenticate(
                b,
                Credential::Both {
                    password: b"wrong",
                    challenge,
                    signature: &signature,
                }
            )
            .is_success());
    }

    #[test]
    fn open_session_carries_boundary_and_name() {
        let mut acc = Accounts::new();
        let b = BoundaryId::new(42);
        acc.enroll_password("dana", b, SALT, b"pw");
        let session = acc.open_session(b, Credential::Password(b"pw")).unwrap();
        assert_eq!(session.boundary, b);
        assert_eq!(session.profile, "dana");
        assert!(acc.open_session(b, Credential::Password(b"nope")).is_none());
    }

    #[test]
    fn credential_record_bridge_roundtrips_and_authenticates() {
        use shared::crypto::credential::CredentialRecord;

        // Enroll normally, then export the persisted record.
        let mut acc = Accounts::new();
        let b = BoundaryId::new(77);
        acc.enroll_password("erin", b, SALT, b"sunflower");
        let record = acc.password_record_for(b).expect("password record");

        // The exported record verifies the same password (one scheme).
        assert!(record.verify(b"sunflower"));
        assert!(!record.verify(b"wrong"));

        // Enrolling a *fresh* registry from that record (as the kernel would,
        // loading from disk) authenticates identically — no re-hashing.
        let mut loaded = Accounts::new();
        loaded.enroll_password_record("erin", b, &record);
        assert!(loaded
            .authenticate(b, Credential::Password(b"sunflower"))
            .is_success());
        assert!(!loaded
            .authenticate(b, Credential::Password(b"nope"))
            .is_success());

        // And a record built directly (as the login app does) enrolls and
        // authenticates the same way — proving the on-disk format and the
        // registry agree byte-for-byte.
        let direct = CredentialRecord::new(SALT, b"sunflower");
        assert_eq!(direct, record);
    }

    #[test]
    fn key_device_profile_has_no_password_record() {
        let (public_key, _secret) = generate_keypair().unwrap();
        let mut acc = Accounts::new();
        let b = BoundaryId::new(88);
        acc.enroll_key_device("kh", b, public_key);
        assert!(acc.password_record_for(b).is_none());
    }
}
