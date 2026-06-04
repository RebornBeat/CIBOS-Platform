//! # User Profile Types
//!
//! A *profile* is one user's completely isolated environment: its own storage,
//! its own applications, its own configuration. There is intentionally only one
//! kind of profile — there is no "work" versus "personal" distinction and no
//! graded privilege tiers. Profiles differ only in their boolean capability
//! grants (for example, whether the profile may install applications) and in
//! which authentication method unlocks them.
//!
//! At runtime a profile is realized as an isolation boundary
//! ([`crate::types::isolation::BoundaryId`]); this module describes the
//! *persistent* profile definition that the system stores and that the kernel's
//! profile manager loads when a profile is unlocked.
//!
//! Profile names use a fixed-capacity [`heapless::String`] so this module needs
//! no allocator and remains usable across the whole `no_std`/`std` range.

use crate::types::authentication::AuthenticationMethod;
use crate::types::error::ConfigError;
use crate::types::isolation::BoundaryId;
use heapless::String as HeaplessString;

/// Maximum length, in bytes, of a profile display name.
pub const MAX_PROFILE_NAME: usize = 64;

/// A profile display name with a fixed maximum capacity.
pub type ProfileName = HeaplessString<MAX_PROFILE_NAME>;

/// Persistent identifier for a user profile.
///
/// Stable across reboots, unlike the runtime [`BoundaryId`] a profile is given
/// when it is activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct ProfileId(pub u64);

impl ProfileId {
    /// Construct a profile identifier from a raw value.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        ProfileId(raw)
    }

    /// The raw underlying value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Boolean capability grants for a profile.
///
/// These are *grants*, not security levels: a profile either may or may not do
/// each of these things. Every profile is equally and fully isolated regardless
/// of which grants it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileCapabilities {
    /// May install new applications into this profile.
    pub install_applications: bool,
    /// May create, modify, or delete *other* profiles (administrative).
    pub manage_profiles: bool,
    /// May initiate firmware/OS updates.
    pub perform_updates: bool,
    /// May access networking through the network container.
    pub network_access: bool,
}

impl ProfileCapabilities {
    /// A standard non-administrative profile: can install apps and use the
    /// network, cannot manage other profiles or perform system updates.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            install_applications: true,
            manage_profiles: false,
            perform_updates: false,
            network_access: true,
        }
    }

    /// An administrative profile: all grants enabled.
    #[must_use]
    pub const fn administrative() -> Self {
        Self {
            install_applications: true,
            manage_profiles: true,
            perform_updates: true,
            network_access: true,
        }
    }

    /// A locked-down profile: no installation, no network, no administration.
    #[must_use]
    pub const fn restricted() -> Self {
        Self {
            install_applications: false,
            manage_profiles: false,
            perform_updates: false,
            network_access: false,
        }
    }
}

impl Default for ProfileCapabilities {
    fn default() -> Self {
        Self::standard()
    }
}

/// The persistent definition of a user profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileConfiguration {
    /// Stable identifier.
    pub id: ProfileId,
    /// Human-readable display name.
    pub display_name: ProfileName,
    /// How this profile is unlocked.
    pub authentication: AuthenticationMethod,
    /// Whether this profile's storage is encrypted at rest.
    pub storage_encrypted: bool,
    /// Capability grants.
    pub capabilities: ProfileCapabilities,
}

impl ProfileConfiguration {
    /// Construct a profile configuration, validating the display name.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::ValidationFailed`] if `display_name` is empty or
    /// exceeds [`MAX_PROFILE_NAME`] bytes.
    pub fn new(
        id: ProfileId,
        display_name: &str,
        authentication: AuthenticationMethod,
        storage_encrypted: bool,
        capabilities: ProfileCapabilities,
    ) -> Result<Self, ConfigError> {
        if display_name.is_empty() {
            return Err(ConfigError::ValidationFailed {
                field: "display_name",
                reason: "must not be empty",
            });
        }
        let name = ProfileName::try_from(display_name).map_err(|()| {
            ConfigError::ValidationFailed {
                field: "display_name",
                reason: "exceeds maximum length",
            }
        })?;
        Ok(Self {
            id,
            display_name: name,
            authentication,
            storage_encrypted,
            capabilities,
        })
    }
}

/// A profile that has been unlocked and bound to a runtime isolation boundary.
///
/// This pairs the persistent profile identity with the live boundary the kernel
/// created for the active session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveProfile {
    /// The persistent profile configuration.
    pub configuration: ProfileConfiguration,
    /// The runtime boundary assigned to this session.
    pub boundary: BoundaryId,
}
