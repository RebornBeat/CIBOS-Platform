//! # System Configuration and Profile Types
//!
//! The build-time and boot-time configuration vocabulary shared across the
//! firmware/kernel boundary.
//!
//! Three profile concepts live here, and they are distinct from the *user*
//! profiles in [`crate::types::profiles`]:
//!
//! * [`CibiosProfile`] — the firmware profile (Standard or Lightweight),
//!   selecting cryptographic versus lightweight handoff.
//! * [`CibosProfile`] — the kernel operational profile (Maximum Isolation,
//!   Balanced, Performance, Compute), selecting scheduling mechanisms and
//!   security feature set.
//! * [`HandoffMode`] — the protocol the two agree on at the boundary.
//!
//! The firmware and kernel profiles must pair correctly; [`CibiosProfile`]
//! exposes [`CibiosProfile::accepts`] encoding the documented pairing matrix.
//! All three are `#[repr(u32)]` because they are written into and read from the
//! handoff record.

use crate::types::error::SerializationError;

/// The firmware profile CIBIOS was built as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum CibiosProfile {
    /// Cryptographic verification of the CIBOS image before handoff.
    /// Pairs with Maximum Isolation, Balanced, and Performance.
    Standard = 1,
    /// Lightweight handshake, no signature verification; physical trust model.
    /// Pairs with Compute (and Performance run offline).
    Lightweight = 2,
}

impl CibiosProfile {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// The handoff mode this firmware profile uses.
    #[must_use]
    pub const fn handoff_mode(self) -> HandoffMode {
        match self {
            CibiosProfile::Standard => HandoffMode::Cryptographic,
            CibiosProfile::Lightweight => HandoffMode::Lightweight,
        }
    }

    /// Whether this firmware profile may hand off to the given kernel profile.
    ///
    /// Encodes the documented pairing matrix: cryptographic handoff (Standard)
    /// pairs with the security-bearing kernel profiles; lightweight handoff
    /// pairs only with Compute and offline Performance.
    #[must_use]
    pub const fn accepts(self, kernel: CibosProfile) -> bool {
        match self {
            CibiosProfile::Standard => matches!(
                kernel,
                CibosProfile::MaximumIsolation
                    | CibosProfile::Balanced
                    | CibosProfile::Performance
            ),
            CibiosProfile::Lightweight => {
                matches!(kernel, CibosProfile::Compute | CibosProfile::Performance)
            }
        }
    }

    /// Whether SMT is disabled by default under this firmware profile.
    #[must_use]
    pub const fn smt_disabled_by_default(self) -> bool {
        matches!(self, CibiosProfile::Standard)
    }
}

impl TryFrom<u32> for CibiosProfile {
    type Error = SerializationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(CibiosProfile::Standard),
            2 => Ok(CibiosProfile::Lightweight),
            _ => Err(SerializationError::InvalidValue {
                field: "CibiosProfile",
            }),
        }
    }
}

/// The kernel operational profile CIBOS was built as.
///
/// All profiles deliver the full quantum-like foundation (parallel pathways,
/// interference-freedom, non-determinism, application-controlled resolution).
/// They differ in which *security features* are compiled on top and which
/// scheduling mechanisms are active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum CibosProfile {
    /// Adversarial-environment profile: equal weights, RTRO, cryptographic IPC,
    /// multi-user isolation, audit logging, SMT disabled.
    MaximumIsolation = 1,
    /// General-purpose profile: weighted scheduling, anti-starvation,
    /// cryptographic IPC, SMT disabled by default.
    Balanced = 2,
    /// Responsiveness-first profile: strong weights, full fairness, SMT enabled.
    Performance = 3,
    /// Throughput-first air-gapped profile: lightweight IPC, per-lane and
    /// dynamic weights available, SMT enabled, no adversary assumed.
    Compute = 4,
}

impl CibosProfile {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Whether SMT is enabled by default under this kernel profile.
    #[must_use]
    pub const fn smt_enabled_by_default(self) -> bool {
        matches!(self, CibosProfile::Performance | CibosProfile::Compute)
    }

    /// Whether all weight classes are forced equal (a Maximum Isolation
    /// security requirement that eliminates scheduling timing side channels).
    #[must_use]
    pub const fn forces_equal_weights(self) -> bool {
        matches!(self, CibosProfile::MaximumIsolation)
    }

    /// Whether this profile uses cryptographic IPC by default.
    #[must_use]
    pub const fn cryptographic_ipc_default(self) -> bool {
        matches!(
            self,
            CibosProfile::MaximumIsolation | CibosProfile::Balanced
        )
    }

    /// Default anti-starvation threshold in milliseconds, or `None` if
    /// anti-starvation is not compiled for this profile.
    #[must_use]
    pub const fn anti_starvation_threshold_ms(self) -> Option<u32> {
        match self {
            CibosProfile::MaximumIsolation => None,
            CibosProfile::Balanced => Some(100),
            CibosProfile::Performance => Some(50),
            CibosProfile::Compute => None,
        }
    }
}

impl TryFrom<u32> for CibosProfile {
    type Error = SerializationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(CibosProfile::MaximumIsolation),
            2 => Ok(CibosProfile::Balanced),
            3 => Ok(CibosProfile::Performance),
            4 => Ok(CibosProfile::Compute),
            _ => Err(SerializationError::InvalidValue {
                field: "CibosProfile",
            }),
        }
    }
}

/// The handoff protocol the firmware and kernel agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum HandoffMode {
    /// CIBIOS verifies the CIBOS image signature before transfer.
    Cryptographic = 1,
    /// No signature verification; trust established by physical security.
    Lightweight = 2,
}

impl HandoffMode {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl TryFrom<u32> for HandoffMode {
    type Error = SerializationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(HandoffMode::Cryptographic),
            2 => Ok(HandoffMode::Lightweight),
            _ => Err(SerializationError::InvalidValue {
                field: "HandoffMode",
            }),
        }
    }
}

/// Boot-time scheduling configuration values, loaded from signed configuration
/// or falling back to the compiled profile defaults.
///
/// These map directly to the `[scheduling]` section of the documented
/// `cibos.conf`. They are advisory inputs to the kernel selector; the active
/// profile may override them (Maximum Isolation forces equal weights).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulingConfig {
    /// Weight for the System class.
    pub system_weight: u32,
    /// Weight for the User class.
    pub user_weight: u32,
    /// Weight for the Background class.
    pub background_weight: u32,
    /// Anti-starvation threshold in milliseconds (ignored if not compiled).
    pub anti_starvation_threshold_ms: u32,
}

impl SchedulingConfig {
    /// The compiled defaults for a given kernel profile.
    #[must_use]
    pub const fn defaults_for(profile: CibosProfile) -> Self {
        match profile {
            CibosProfile::MaximumIsolation => Self {
                system_weight: 1,
                user_weight: 1,
                background_weight: 1,
                anti_starvation_threshold_ms: 0,
            },
            CibosProfile::Balanced => Self {
                system_weight: 3,
                user_weight: 1,
                background_weight: 1,
                anti_starvation_threshold_ms: 100,
            },
            CibosProfile::Performance => Self {
                system_weight: 5,
                user_weight: 2,
                background_weight: 1,
                anti_starvation_threshold_ms: 50,
            },
            CibosProfile::Compute => Self {
                system_weight: 1,
                user_weight: 1,
                background_weight: 1,
                anti_starvation_threshold_ms: 0,
            },
        }
    }
}
