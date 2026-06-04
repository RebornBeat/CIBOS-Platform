//! # Authentication Types
//!
//! Types describing how a user authenticates to unlock a profile.
//!
//! The system supports exactly two authentication methods, by deliberate
//! scope decision:
//!
//! 1. **Password** — a secret the user knows.
//! 2. **Physical key device** — a secret the user *has*, read over any wired
//!    physical connection (USB-A, USB-C, or a device's charging port acting as
//!    a data connection). The device carries cryptographic key material that
//!    unlocks the profile.
//!
//! There is deliberately **no** biometric authentication and **no** wireless
//! authentication (NFC, Bluetooth, Wi-Fi). Wireless authentication expands the
//! attack surface, and biometrics are out of scope. A profile may use either
//! method, or require both.
//!
//! These types are shared between the kernel (which performs verification) and
//! the platform/SDK layers (which present the authentication UI), so they live
//! in `shared`. Secret *material* itself is never represented by these types;
//! they describe method, format, and outcome only.

use crate::types::error::AuthenticationError;
use crate::types::isolation::BoundaryId;

/// The method by which a profile is unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum AuthenticationMethod {
    /// A password known to the user.
    Password = 1,
    /// A physical key device connected over a wired interface.
    PhysicalKeyDevice = 2,
    /// Both a password *and* a physical key device are required.
    PasswordAndKeyDevice = 3,
}

impl AuthenticationMethod {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Whether this method requires a physical key device to be present.
    #[must_use]
    pub const fn requires_key_device(self) -> bool {
        matches!(
            self,
            AuthenticationMethod::PhysicalKeyDevice | AuthenticationMethod::PasswordAndKeyDevice
        )
    }

    /// Whether this method requires a password.
    #[must_use]
    pub const fn requires_password(self) -> bool {
        matches!(
            self,
            AuthenticationMethod::Password | AuthenticationMethod::PasswordAndKeyDevice
        )
    }
}

impl TryFrom<u32> for AuthenticationMethod {
    type Error = AuthenticationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(AuthenticationMethod::Password),
            2 => Ok(AuthenticationMethod::PhysicalKeyDevice),
            3 => Ok(AuthenticationMethod::PasswordAndKeyDevice),
            _ => Err(AuthenticationError::UnsupportedCredentialFormat),
        }
    }
}

/// The physical interface over which a key device is connected.
///
/// All options are wired. There is no wireless variant by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum KeyDeviceInterface {
    /// USB Type-A port.
    UsbA = 1,
    /// USB Type-C port.
    UsbC = 2,
    /// A charging port operating as a wired data connection.
    ChargingPort = 3,
}

impl KeyDeviceInterface {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Format of credential material stored on a key device or derived from a
/// password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum CredentialFormat {
    /// Ed25519 keypair material (classical).
    Ed25519 = 1,
    /// ML-DSA (Dilithium) keypair material (post-quantum).
    MlDsa = 2,
    /// A password-derived key (via the configured KDF).
    PasswordDerived = 3,
}

impl CredentialFormat {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl TryFrom<u32> for CredentialFormat {
    type Error = AuthenticationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(CredentialFormat::Ed25519),
            2 => Ok(CredentialFormat::MlDsa),
            3 => Ok(CredentialFormat::PasswordDerived),
            _ => Err(AuthenticationError::UnsupportedCredentialFormat),
        }
    }
}

/// A request to authenticate against a specific profile.
///
/// This carries no secret material — only the identity of the target profile
/// and the method being attempted. The actual secret is handled by the kernel's
/// authentication subsystem through channels that never expose it to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticationRequest {
    /// The boundary (profile) the user is attempting to unlock.
    pub target: BoundaryId,
    /// The method being attempted.
    pub method: AuthenticationMethod,
    /// If a key device is involved, the interface it is connected on.
    pub interface: Option<KeyDeviceInterface>,
}

/// The outcome of an authentication attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationOutcome {
    /// Authentication succeeded; the named boundary may be activated.
    Success {
        /// The authenticated boundary (profile).
        boundary: BoundaryId,
    },
    /// Authentication failed.
    Failure {
        /// Why it failed.
        reason: AuthenticationFailureReason,
    },
}

impl AuthenticationOutcome {
    /// Convenience: whether the outcome was a success.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, AuthenticationOutcome::Success { .. })
    }

    /// Convert a failure outcome into an [`AuthenticationError`]. Returns `Ok`
    /// with the boundary on success.
    ///
    /// # Errors
    ///
    /// Returns the corresponding [`AuthenticationError`] when this outcome is a
    /// failure.
    pub const fn into_result(self) -> Result<BoundaryId, AuthenticationError> {
        match self {
            AuthenticationOutcome::Success { boundary } => Ok(boundary),
            AuthenticationOutcome::Failure { reason } => Err(reason.into_error()),
        }
    }
}

/// Why an authentication attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AuthenticationFailureReason {
    /// The supplied secret did not match.
    BadSecret = 1,
    /// A required key device was not present.
    KeyDeviceMissing = 2,
    /// The key device was present but could not be read.
    KeyDeviceUnreadable = 3,
    /// The named profile does not exist.
    UnknownProfile = 4,
}

impl AuthenticationFailureReason {
    /// Map this reason to the corresponding [`AuthenticationError`].
    #[must_use]
    pub const fn into_error(self) -> AuthenticationError {
        match self {
            AuthenticationFailureReason::BadSecret => AuthenticationError::InvalidCredentials,
            AuthenticationFailureReason::KeyDeviceMissing => AuthenticationError::DeviceNotPresent,
            AuthenticationFailureReason::KeyDeviceUnreadable => {
                AuthenticationError::DeviceReadFailure {
                    detail: "key device present but unreadable",
                }
            }
            AuthenticationFailureReason::UnknownProfile => AuthenticationError::ProfileNotFound,
        }
    }
}
