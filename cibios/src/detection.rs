//! # Hardware Detection (Portable Logic)
//!
//! The architecture-independent half of hardware detection: taking the raw
//! values the architecture code reads from the machine and assembling them into
//! a validated [`HardwareProfile`], plus the policy decisions that depend only
//! on those values (notably the SMT decision).
//!
//! The architecture-*dependent* half — actually executing CPUID, reading a
//! device tree, probing MMIO — lives in the binary's `arch` module because it
//! requires `unsafe`. That code fills in a [`DetectedHardware`] and hands it
//! here, keeping everything testable on the host.

use crate::error::FirmwareError;
use shared::types::error::HardwareError;
use shared::{
    CibiosProfile, CoreTopology, HardwarePlatform, HardwareProfile, MemoryRegion,
    ProcessorArchitecture,
};

use shared::types::hardware::{
    InputCapabilities, NetworkCapabilities, SecurityCapabilities, SensorCapabilities,
};

/// Raw hardware facts gathered by the architecture-specific probe code, before
/// validation. The `arch` module fills this in; [`assemble_profile`] turns it
/// into a validated [`HardwareProfile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectedHardware {
    /// Detected processor architecture.
    pub architecture: ProcessorArchitecture,
    /// Detected device class.
    pub platform: HardwarePlatform,
    /// Physical core count reported by hardware.
    pub physical_cores: u32,
    /// Logical (thread) count reported by hardware, before any SMT policy.
    pub logical_cores: u32,
    /// Total usable RAM in bytes.
    pub total_memory: u64,
    /// Raw security capability bits.
    pub security_bits: u32,
    /// Raw input capability bits.
    pub input_bits: u32,
    /// Raw sensor capability bits.
    pub sensor_bits: u32,
    /// Raw network capability bits.
    pub network_bits: u32,
}

/// The firmware's own profile, determined at build time.
///
/// A firmware built with the `handoff-cryptographic` feature is a Standard
/// firmware (it verifies signatures); without it, it is a Lightweight firmware.
/// This ties the compiled capability to the declared profile so the two cannot
/// disagree.
#[must_use]
pub const fn firmware_profile() -> CibiosProfile {
    #[cfg(feature = "handoff-cryptographic")]
    {
        CibiosProfile::Standard
    }
    #[cfg(not(feature = "handoff-cryptographic"))]
    {
        CibiosProfile::Lightweight
    }
}

/// Whether this firmware was built with the `smt-enabled` override.
///
/// SMT is otherwise off under Standard firmware and on under Lightweight. The
/// override exists for the Performance operational profile, which runs on
/// Standard firmware yet wants SMT; because firmware configures SMT before the
/// kernel image is loaded, the intent must be known at firmware build time.
#[must_use]
pub const fn smt_override_enabled() -> bool {
    cfg!(feature = "smt-enabled")
}

/// Decide the effective logical-core count after applying the SMT policy for
/// `profile`.
///
/// When `smt_forced` is set (the `smt-enabled` build override) SMT is used if
/// the hardware actually has hyperthreads, regardless of firmware profile.
/// Otherwise Standard firmware disables SMT by default (it removes a class of
/// cross-thread side channels), so the effective logical count collapses to the
/// physical count, while Lightweight firmware leaves SMT as the hardware
/// reported it.
///
/// Returns `(logical_cores, smt_enabled)`.
#[must_use]
pub const fn apply_smt_policy(
    profile: CibiosProfile,
    physical_cores: u32,
    detected_logical: u32,
    smt_forced: bool,
) -> (u32, bool) {
    if smt_forced {
        // Honor the explicit override, but never invent threads the hardware
        // does not actually have.
        let smt = detected_logical > physical_cores;
        (detected_logical, smt)
    } else if profile.smt_disabled_by_default() {
        (physical_cores, false)
    } else {
        let smt = detected_logical > physical_cores;
        (detected_logical, smt)
    }
}

/// Assemble and validate a [`HardwareProfile`] from detected values, applying
/// the SMT policy for the firmware's profile.
///
/// # Errors
///
/// Returns [`FirmwareError`] if the core topology is inconsistent (for example,
/// zero physical cores or fewer logical than physical).
pub fn assemble_profile(
    detected: &DetectedHardware,
) -> Result<HardwareProfile, FirmwareError> {
    let profile = firmware_profile();
    let (logical, smt) = apply_smt_policy(
        profile,
        detected.physical_cores,
        detected.logical_cores,
        smt_override_enabled(),
    );

    let topology = CoreTopology::new(detected.physical_cores, logical, smt).map_err(
        |e: HardwareError| FirmwareError::from(shared::SharedError::from(e)),
    )?;

    Ok(HardwareProfile {
        architecture: detected.architecture,
        platform: detected.platform,
        topology,
        total_memory: detected.total_memory,
        security: SecurityCapabilities::from_bits_truncate(detected.security_bits),
        input: InputCapabilities::from_bits_truncate(detected.input_bits),
        sensors: SensorCapabilities::from_bits_truncate(detected.sensor_bits),
        network: NetworkCapabilities::from_bits_truncate(detected.network_bits),
    })
}

/// Validate a firmware-reported memory map: regions must be non-empty, sorted by
/// base, and non-overlapping. Firmware builds this list from the platform memory
/// map; validating it here keeps the check in portable, tested code.
///
/// # Errors
///
/// Returns [`FirmwareError::MalformedImage`]-adjacent
/// [`FirmwareError`] via shared isolation errors if the map is inconsistent.
pub fn validate_memory_map(regions: &[MemoryRegion]) -> Result<(), FirmwareError> {
    use shared::types::error::{IsolationError, SharedError};

    let mut previous_end: Option<u64> = None;
    for region in regions {
        if region.length == 0 {
            return Err(FirmwareError::from(SharedError::from(
                IsolationError::BoundarySetupFailed {
                    detail: "zero-length memory region",
                },
            )));
        }
        // Detect base+length overflow.
        let end = region.base.checked_add(region.length).ok_or_else(|| {
            FirmwareError::from(SharedError::from(IsolationError::BoundarySetupFailed {
                detail: "memory region end overflow",
            }))
        })?;
        if let Some(prev) = previous_end {
            if region.base < prev {
                return Err(FirmwareError::from(SharedError::from(
                    IsolationError::BoundarySetupFailed {
                        detail: "memory regions overlap or are unsorted",
                    },
                )));
            }
        }
        previous_end = Some(end);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::MemoryRegionKind;

    fn detected(physical: u32, logical: u32) -> DetectedHardware {
        DetectedHardware {
            architecture: ProcessorArchitecture::X86_64,
            platform: HardwarePlatform::Desktop,
            physical_cores: physical,
            logical_cores: logical,
            total_memory: 8 * 1024 * 1024 * 1024,
            security_bits: SecurityCapabilities::HARDWARE_RNG.bits(),
            input_bits: InputCapabilities::KEYBOARD.bits(),
            sensor_bits: 0,
            network_bits: NetworkCapabilities::ETHERNET.bits(),
        }
    }

    #[test]
    fn smt_policy_per_profile() {
        // Standard disables SMT by default: 4 physical / 8 logical -> 4/false.
        assert_eq!(
            apply_smt_policy(CibiosProfile::Standard, 4, 8, false),
            (4, false)
        );
        // The smt-enabled override forces it on even under Standard firmware
        // (this is the Performance operational profile's case).
        assert_eq!(
            apply_smt_policy(CibiosProfile::Standard, 4, 8, true),
            (8, true)
        );
        // The override cannot invent hyperthreads the hardware lacks.
        assert_eq!(
            apply_smt_policy(CibiosProfile::Standard, 4, 4, true),
            (4, false)
        );
        // Lightweight keeps it: 4/8 stays 8/true.
        assert_eq!(
            apply_smt_policy(CibiosProfile::Lightweight, 4, 8, false),
            (8, true)
        );
        // No SMT present: 4/4 stays 4/false under Lightweight.
        assert_eq!(
            apply_smt_policy(CibiosProfile::Lightweight, 4, 4, false),
            (4, false)
        );
    }

    #[test]
    fn assemble_is_consistent_with_profile() {
        let profile = assemble_profile(&detected(4, 8)).expect("assemble");
        // The compiled firmware_profile() governs SMT. Either way the topology
        // is internally consistent, which is the invariant we assert.
        assert_eq!(profile.topology.physical_cores, 4);
        assert!(profile.topology.logical_cores >= profile.topology.physical_cores);
        assert!(profile.security.contains(SecurityCapabilities::HARDWARE_RNG));
    }

    #[test]
    fn zero_cores_rejected() {
        assert!(assemble_profile(&detected(0, 0)).is_err());
    }

    #[test]
    fn memory_map_validation() {
        let good = [
            MemoryRegion {
                base: 0,
                length: 0x1000,
                kind: MemoryRegionKind::FirmwareReserved,
            },
            MemoryRegion {
                base: 0x1000,
                length: 0x10_0000,
                kind: MemoryRegionKind::Usable,
            },
        ];
        assert!(validate_memory_map(&good).is_ok());

        let overlapping = [
            MemoryRegion {
                base: 0,
                length: 0x2000,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                base: 0x1000,
                length: 0x1000,
                kind: MemoryRegionKind::Usable,
            },
        ];
        assert!(validate_memory_map(&overlapping).is_err());

        let zero = [MemoryRegion {
            base: 0,
            length: 0,
            kind: MemoryRegionKind::Usable,
        }];
        assert!(validate_memory_map(&zero).is_err());
    }
}
