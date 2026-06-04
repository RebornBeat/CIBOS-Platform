//! # Error Hierarchy
//!
//! The root error taxonomy for the entire CIBIOS/CIBOS/HIP system.
//!
//! ## Design
//!
//! This module is deliberately `no_std` *and* allocation-free. Error values
//! carry `&'static str` context rather than owned strings, so the error type
//! is usable on the barest bare-metal target (firmware before an allocator
//! exists) as well as in the fully-`std` application layer. Every error
//! implements [`core::error::Error`] (stable since Rust 1.81), so the standard
//! `?` propagation and `source()` chaining work uniformly across `no_std` and
//! `std` crates.
//!
//! ## Layering
//!
//! [`SharedError`] is the cross-cutting root. Each downstream crate defines its
//! own domain error (`FirmwareError` in CIBIOS, `KernelError` in CIBOS, and so
//! on) that wraps [`SharedError`] through a `From` conversion. This gives every
//! layer a precise local error vocabulary while preserving a single, uniform
//! chain back to the shared root — which is exactly the hierarchy the system
//! documentation describes.
//!
//! ## Why not `thiserror`?
//!
//! `thiserror` depends on `std`. Firmware and kernel code cannot link `std`, so
//! the `Display`/`Error` implementations here are written by hand. They are
//! mechanical and complete.

use core::fmt;

/// Convenient result alias for fallible operations returning a [`SharedError`].
pub type SharedResult<T> = Result<T, SharedError>;

/// The cross-cutting root error for the CIBIOS/CIBOS/HIP system.
///
/// Downstream crates wrap this in their own domain error type. Each variant
/// corresponds to a coherent failure domain rather than a single call site,
/// keeping the taxonomy small enough to reason about while still distinguishing
/// the categories that callers actually branch on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SharedError {
    /// Hardware initialization, detection, or capability failures.
    Hardware(HardwareError),
    /// Isolation boundary establishment or enforcement failures.
    Isolation(IsolationError),
    /// Authentication and credential verification failures.
    Authentication(AuthenticationError),
    /// Cryptographic operation failures (signing, verification, KEM, hashing).
    Crypto(CryptoError),
    /// Image, component, or integrity verification failures.
    Verification(VerificationError),
    /// Configuration loading, parsing, or validation failures.
    Config(ConfigError),
    /// Serialization or deserialization of wire/handoff structures.
    Serialization(SerializationError),
    /// Protocol-level failures (handoff, IPC, authentication exchange).
    Protocol(ProtocolError),
    /// Resource exhaustion or unavailability.
    Resource(ResourceError),
}

impl fmt::Display for SharedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SharedError::Hardware(e) => write!(f, "hardware error: {e}"),
            SharedError::Isolation(e) => write!(f, "isolation error: {e}"),
            SharedError::Authentication(e) => write!(f, "authentication error: {e}"),
            SharedError::Crypto(e) => write!(f, "crypto error: {e}"),
            SharedError::Verification(e) => write!(f, "verification error: {e}"),
            SharedError::Config(e) => write!(f, "configuration error: {e}"),
            SharedError::Serialization(e) => write!(f, "serialization error: {e}"),
            SharedError::Protocol(e) => write!(f, "protocol error: {e}"),
            SharedError::Resource(e) => write!(f, "resource error: {e}"),
        }
    }
}

impl core::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            SharedError::Hardware(e) => Some(e),
            SharedError::Isolation(e) => Some(e),
            SharedError::Authentication(e) => Some(e),
            SharedError::Crypto(e) => Some(e),
            SharedError::Verification(e) => Some(e),
            SharedError::Config(e) => Some(e),
            SharedError::Serialization(e) => Some(e),
            SharedError::Protocol(e) => Some(e),
            SharedError::Resource(e) => Some(e),
        }
    }
}

// --- From conversions so `?` lifts domain errors into the root automatically ---

impl From<HardwareError> for SharedError {
    fn from(e: HardwareError) -> Self {
        SharedError::Hardware(e)
    }
}
impl From<IsolationError> for SharedError {
    fn from(e: IsolationError) -> Self {
        SharedError::Isolation(e)
    }
}
impl From<AuthenticationError> for SharedError {
    fn from(e: AuthenticationError) -> Self {
        SharedError::Authentication(e)
    }
}
impl From<CryptoError> for SharedError {
    fn from(e: CryptoError) -> Self {
        SharedError::Crypto(e)
    }
}
impl From<VerificationError> for SharedError {
    fn from(e: VerificationError) -> Self {
        SharedError::Verification(e)
    }
}
impl From<ConfigError> for SharedError {
    fn from(e: ConfigError) -> Self {
        SharedError::Config(e)
    }
}
impl From<SerializationError> for SharedError {
    fn from(e: SerializationError) -> Self {
        SharedError::Serialization(e)
    }
}
impl From<ProtocolError> for SharedError {
    fn from(e: ProtocolError) -> Self {
        SharedError::Protocol(e)
    }
}
impl From<ResourceError> for SharedError {
    fn from(e: ResourceError) -> Self {
        SharedError::Resource(e)
    }
}

// ===========================================================================
// Hardware
// ===========================================================================

/// Failures in hardware detection, initialization, or capability negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HardwareError {
    /// A required processor architecture feature was absent.
    UnsupportedArchitecture {
        /// Human-readable description of the missing capability.
        detail: &'static str,
    },
    /// Hardware initialization assembly returned a non-zero status code.
    InitializationFailed {
        /// Subsystem that failed to initialize.
        subsystem: &'static str,
        /// Raw status code returned by the low-level routine.
        code: i32,
    },
    /// Insufficient physical memory to proceed.
    InsufficientMemory {
        /// Bytes required.
        required: u64,
        /// Bytes available.
        available: u64,
    },
    /// A hardware random number generator was requested but is unavailable.
    RandomNumberGeneratorUnavailable,
    /// SMT configuration could not be applied as requested.
    SmtConfigurationFailed {
        /// Description of the failure.
        detail: &'static str,
    },
    /// A storage device read or write failed.
    StorageFailure {
        /// Description of the failure.
        detail: &'static str,
    },
    /// A required peripheral or controller was not found.
    PeripheralMissing {
        /// Name of the missing peripheral.
        name: &'static str,
    },
}

impl fmt::Display for HardwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HardwareError::UnsupportedArchitecture { detail } => {
                write!(f, "unsupported architecture feature: {detail}")
            }
            HardwareError::InitializationFailed { subsystem, code } => {
                write!(f, "initialization of {subsystem} failed with code {code}")
            }
            HardwareError::InsufficientMemory {
                required,
                available,
            } => write!(
                f,
                "insufficient memory: {required} bytes required, {available} available"
            ),
            HardwareError::RandomNumberGeneratorUnavailable => {
                write!(f, "hardware random number generator unavailable")
            }
            HardwareError::SmtConfigurationFailed { detail } => {
                write!(f, "SMT configuration failed: {detail}")
            }
            HardwareError::StorageFailure { detail } => {
                write!(f, "storage failure: {detail}")
            }
            HardwareError::PeripheralMissing { name } => {
                write!(f, "required peripheral missing: {name}")
            }
        }
    }
}

impl core::error::Error for HardwareError {}

// ===========================================================================
// Isolation
// ===========================================================================

/// Failures in establishing or enforcing isolation boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IsolationError {
    /// A memory isolation boundary could not be established.
    BoundarySetupFailed {
        /// Description of the failure.
        detail: &'static str,
    },
    /// A cross-boundary access was attempted and refused.
    AccessDenied {
        /// Description of the denied access.
        detail: &'static str,
    },
    /// A lane memory region could not be reserved.
    LaneRegionUnavailable,
    /// The requested isolation level is not supported in this configuration.
    UnsupportedLevel {
        /// Description of the unsupported level.
        detail: &'static str,
    },
    /// An attempt to share mutable state across a boundary was detected.
    SharedStateViolation {
        /// Description of the violation.
        detail: &'static str,
    },
}

impl fmt::Display for IsolationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IsolationError::BoundarySetupFailed { detail } => {
                write!(f, "isolation boundary setup failed: {detail}")
            }
            IsolationError::AccessDenied { detail } => {
                write!(f, "cross-boundary access denied: {detail}")
            }
            IsolationError::LaneRegionUnavailable => {
                write!(f, "lane memory region unavailable")
            }
            IsolationError::UnsupportedLevel { detail } => {
                write!(f, "unsupported isolation level: {detail}")
            }
            IsolationError::SharedStateViolation { detail } => {
                write!(f, "shared mutable state violation: {detail}")
            }
        }
    }
}

impl core::error::Error for IsolationError {}

// ===========================================================================
// Authentication
// ===========================================================================

/// Failures in authentication and credential verification.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthenticationError {
    /// Provided credentials did not verify.
    InvalidCredentials,
    /// No authentication device was present when one was required.
    DeviceNotPresent,
    /// The authentication device was present but could not be read.
    DeviceReadFailure {
        /// Description of the failure.
        detail: &'static str,
    },
    /// The credential format was not recognized.
    UnsupportedCredentialFormat,
    /// The referenced profile does not exist.
    ProfileNotFound,
    /// Authentication is not authorized for the requested operation.
    Unauthorized {
        /// Description of the unauthorized operation.
        detail: &'static str,
    },
}

impl fmt::Display for AuthenticationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthenticationError::InvalidCredentials => write!(f, "invalid credentials"),
            AuthenticationError::DeviceNotPresent => {
                write!(f, "authentication device not present")
            }
            AuthenticationError::DeviceReadFailure { detail } => {
                write!(f, "authentication device read failure: {detail}")
            }
            AuthenticationError::UnsupportedCredentialFormat => {
                write!(f, "unsupported credential format")
            }
            AuthenticationError::ProfileNotFound => write!(f, "profile not found"),
            AuthenticationError::Unauthorized { detail } => {
                write!(f, "unauthorized: {detail}")
            }
        }
    }
}

impl core::error::Error for AuthenticationError {}

// ===========================================================================
// Crypto
// ===========================================================================

/// Failures in cryptographic operations.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoError {
    /// A signature failed to verify.
    SignatureInvalid,
    /// Signing failed.
    SigningFailed {
        /// Description of the failure.
        detail: &'static str,
    },
    /// Key encapsulation or decapsulation failed.
    KeyEncapsulationFailed {
        /// Description of the failure.
        detail: &'static str,
    },
    /// A key had an unexpected or invalid length.
    InvalidKeyLength {
        /// Expected length in bytes.
        expected: usize,
        /// Actual length in bytes.
        actual: usize,
    },
    /// A signature had an unexpected or invalid length.
    InvalidSignatureLength {
        /// Expected length in bytes.
        expected: usize,
        /// Actual length in bytes.
        actual: usize,
    },
    /// The requested algorithm is not compiled into this build.
    AlgorithmUnavailable {
        /// Name of the unavailable algorithm.
        algorithm: &'static str,
    },
    /// Insufficient entropy was available to complete the operation.
    InsufficientEntropy,
    /// A hash comparison did not match the expected value.
    HashMismatch,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CryptoError::SignatureInvalid => write!(f, "signature verification failed"),
            CryptoError::SigningFailed { detail } => write!(f, "signing failed: {detail}"),
            CryptoError::KeyEncapsulationFailed { detail } => {
                write!(f, "key encapsulation failed: {detail}")
            }
            CryptoError::InvalidKeyLength { expected, actual } => write!(
                f,
                "invalid key length: expected {expected} bytes, got {actual}"
            ),
            CryptoError::InvalidSignatureLength { expected, actual } => write!(
                f,
                "invalid signature length: expected {expected} bytes, got {actual}"
            ),
            CryptoError::AlgorithmUnavailable { algorithm } => {
                write!(f, "cryptographic algorithm unavailable: {algorithm}")
            }
            CryptoError::InsufficientEntropy => write!(f, "insufficient entropy"),
            CryptoError::HashMismatch => write!(f, "hash mismatch"),
        }
    }
}

impl core::error::Error for CryptoError {}

// ===========================================================================
// Verification
// ===========================================================================

/// Failures in image, component, or integrity verification.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VerificationError {
    /// The image format could not be parsed.
    MalformedImage {
        /// Description of the malformation.
        detail: &'static str,
    },
    /// A component within the image failed verification.
    ComponentVerificationFailed {
        /// Name of the failing component.
        component: &'static str,
    },
    /// The overall image integrity hash did not match.
    IntegrityMismatch,
    /// The image signature did not match the expected signing key.
    SignatureMismatch,
    /// The image version is incompatible with the verifier.
    IncompatibleVersion {
        /// Version the verifier expected.
        expected: u32,
        /// Version found in the image.
        found: u32,
    },
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerificationError::MalformedImage { detail } => {
                write!(f, "malformed image: {detail}")
            }
            VerificationError::ComponentVerificationFailed { component } => {
                write!(f, "component verification failed: {component}")
            }
            VerificationError::IntegrityMismatch => write!(f, "image integrity mismatch"),
            VerificationError::SignatureMismatch => write!(f, "image signature mismatch"),
            VerificationError::IncompatibleVersion { expected, found } => write!(
                f,
                "incompatible image version: expected {expected}, found {found}"
            ),
        }
    }
}

impl core::error::Error for VerificationError {}

// ===========================================================================
// Config
// ===========================================================================

/// Failures in configuration loading, parsing, or validation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigError {
    /// The configuration source could not be read.
    ReadFailed {
        /// Description of the failure.
        detail: &'static str,
    },
    /// The configuration could not be parsed.
    ParseFailed {
        /// Description of the failure.
        detail: &'static str,
    },
    /// A configuration value failed validation.
    ValidationFailed {
        /// Name of the offending field.
        field: &'static str,
        /// Description of why validation failed.
        reason: &'static str,
    },
    /// The configuration signature did not verify.
    SignatureInvalid,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::ReadFailed { detail } => write!(f, "config read failed: {detail}"),
            ConfigError::ParseFailed { detail } => write!(f, "config parse failed: {detail}"),
            ConfigError::ValidationFailed { field, reason } => {
                write!(f, "config validation failed for '{field}': {reason}")
            }
            ConfigError::SignatureInvalid => write!(f, "config signature invalid"),
        }
    }
}

impl core::error::Error for ConfigError {}

// ===========================================================================
// Serialization
// ===========================================================================

/// Failures in serializing or deserializing wire / handoff structures.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SerializationError {
    /// The buffer was too small to hold the serialized form.
    BufferTooSmall {
        /// Bytes required.
        required: usize,
        /// Bytes available.
        available: usize,
    },
    /// The input bytes were too short to deserialize.
    UnexpectedEnd,
    /// A field held a value outside its permitted range or set.
    InvalidValue {
        /// Name of the offending field.
        field: &'static str,
    },
    /// A magic number or version tag did not match.
    BadMagic,
}

impl fmt::Display for SerializationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SerializationError::BufferTooSmall {
                required,
                available,
            } => write!(
                f,
                "buffer too small: {required} bytes required, {available} available"
            ),
            SerializationError::UnexpectedEnd => write!(f, "unexpected end of input"),
            SerializationError::InvalidValue { field } => {
                write!(f, "invalid value for field '{field}'")
            }
            SerializationError::BadMagic => write!(f, "bad magic number"),
        }
    }
}

impl core::error::Error for SerializationError {}

// ===========================================================================
// Protocol
// ===========================================================================

/// Failures at the protocol layer (handoff, IPC, authentication exchange).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProtocolError {
    /// The handoff data structure was rejected by the receiver.
    HandoffRejected {
        /// Description of why it was rejected.
        detail: &'static str,
    },
    /// A protocol version mismatch was detected.
    VersionMismatch {
        /// Version the local side speaks.
        local: u32,
        /// Version the remote side speaks.
        remote: u32,
    },
    /// A channel could not be established.
    ChannelEstablishmentFailed {
        /// Description of the failure.
        detail: &'static str,
    },
    /// A message exceeded the negotiated maximum size.
    MessageTooLarge {
        /// Size of the message in bytes.
        size: usize,
        /// Negotiated maximum in bytes.
        maximum: usize,
    },
    /// A channel was closed by the peer.
    ChannelClosed,
    /// The peer was not authorized for the requested channel.
    Unauthorized,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::HandoffRejected { detail } => {
                write!(f, "handoff rejected: {detail}")
            }
            ProtocolError::VersionMismatch { local, remote } => write!(
                f,
                "protocol version mismatch: local {local}, remote {remote}"
            ),
            ProtocolError::ChannelEstablishmentFailed { detail } => {
                write!(f, "channel establishment failed: {detail}")
            }
            ProtocolError::MessageTooLarge { size, maximum } => write!(
                f,
                "message too large: {size} bytes exceeds maximum {maximum}"
            ),
            ProtocolError::ChannelClosed => write!(f, "channel closed"),
            ProtocolError::Unauthorized => write!(f, "unauthorized channel request"),
        }
    }
}

impl core::error::Error for ProtocolError {}

// ===========================================================================
// Resource
// ===========================================================================

/// Failures from resource exhaustion or unavailability.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceError {
    /// A capacity limit was reached.
    CapacityExceeded {
        /// Name of the resource.
        resource: &'static str,
    },
    /// A resource was requested that does not exist.
    NotFound {
        /// Name of the resource.
        resource: &'static str,
    },
    /// A resource was temporarily unavailable.
    Unavailable {
        /// Name of the resource.
        resource: &'static str,
    },
    /// A memory allocation limit was exceeded.
    MemoryLimitExceeded {
        /// Bytes requested.
        requested: u64,
        /// Bytes permitted.
        limit: u64,
    },
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceError::CapacityExceeded { resource } => {
                write!(f, "capacity exceeded for {resource}")
            }
            ResourceError::NotFound { resource } => write!(f, "resource not found: {resource}"),
            ResourceError::Unavailable { resource } => {
                write!(f, "resource unavailable: {resource}")
            }
            ResourceError::MemoryLimitExceeded { requested, limit } => write!(
                f,
                "memory limit exceeded: requested {requested} bytes, limit {limit}"
            ),
        }
    }
}

impl core::error::Error for ResourceError {}
