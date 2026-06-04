//! # Handoff Construction
//!
//! Assembles the [`HandoffData`] record CIBIOS passes to CIBOS, from the
//! verified image and the detected hardware, then validates it before it is
//! used.
//!
//! The firmware's own profile and handoff mode come from
//! [`crate::detection::firmware_profile`]; the kernel profile comes from the
//! verified image header. [`build_handoff`] constructs the record and then runs
//! [`HandoffData::validate`] on it, so a firmware/kernel profile pairing the
//! system forbids (for example, a Standard firmware asked to launch a Compute
//! kernel) is rejected here, in the firmware, before any transfer of control.

use crate::detection::firmware_profile;
use crate::error::FirmwareError;
use crate::verification::VerifiedImage;
use shared::protocols::handoff::ENTROPY_SEED_LEN;
use shared::types::error::SharedError;
use shared::{
    CibosProfile, DecodedHandoff, HandoffData, HardwareProfile, MemoryRegion,
};

/// Build and validate the handoff record.
///
/// * `hardware` — the assembled hardware profile.
/// * `verified` — the result of verifying the CIBOS image; supplies the kernel
///   profile the image declares.
/// * `memory_regions` — the platform memory map to pass to the kernel.
/// * `entropy_seed` — entropy gathered by firmware to seed the kernel CSPRNG.
///
/// Returns both the raw [`HandoffData`] (to be written to the handoff memory
/// region) and its validated [`DecodedHandoff`] view.
///
/// # Errors
///
/// Returns [`FirmwareError`] if the image's declared kernel profile is invalid,
/// if there are too many memory regions, or if the firmware/kernel profile
/// pairing is not permitted.
pub fn build_handoff(
    hardware: &HardwareProfile,
    verified: &VerifiedImage,
    memory_regions: &[MemoryRegion],
    entropy_seed: [u8; ENTROPY_SEED_LEN],
) -> Result<(HandoffData, DecodedHandoff), FirmwareError> {
    let cibios_profile = firmware_profile();
    let cibos_profile = CibosProfile::try_from(verified.cibos_profile).map_err(|e| {
        FirmwareError::from(SharedError::from(e))
    })?;
    let handoff_mode = cibios_profile.handoff_mode();

    let handoff = HandoffData::new(
        hardware.architecture,
        hardware.platform,
        cibios_profile,
        cibos_profile,
        handoff_mode,
        hardware.topology,
        hardware.total_memory,
        memory_regions,
        entropy_seed,
    )
    .map_err(|e| FirmwareError::from(SharedError::from(e)))?;

    // Validate before trusting: this enforces the profile-pairing matrix and
    // topology consistency. A forbidden pairing fails here, in firmware.
    let decoded = handoff
        .validate()
        .map_err(|e| FirmwareError::from(SharedError::from(e)))?;

    Ok((handoff, decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detection::{assemble_profile, DetectedHardware};
    use shared::protocols::handoff::ENTROPY_SEED_LEN;
    use shared::{HardwarePlatform, MemoryRegionKind, ProcessorArchitecture};

    fn hardware() -> HardwareProfile {
        let detected = DetectedHardware {
            architecture: ProcessorArchitecture::X86_64,
            platform: HardwarePlatform::Desktop,
            physical_cores: 4,
            logical_cores: 4,
            total_memory: 8 * 1024 * 1024 * 1024,
            security_bits: 0,
            input_bits: 0,
            sensor_bits: 0,
            network_bits: 0,
        };
        assemble_profile(&detected).unwrap()
    }

    fn verified_for(profile: CibosProfile) -> VerifiedImage {
        VerifiedImage {
            entry_point: 0x10_0000,
            load_base: 0x10_0000,
            cibos_profile: profile.as_u32(),
            component_count: 1,
            signature_verified: false,
        }
    }

    fn regions() -> [MemoryRegion; 1] {
        [MemoryRegion {
            base: 0x10_0000,
            length: 0x1000_0000,
            kind: MemoryRegionKind::Usable,
        }]
    }

    #[test]
    fn builds_for_compatible_pairing() {
        // firmware_profile() depends on the build's features. Choose a kernel
        // profile every firmware profile accepts: Performance is accepted by
        // both Standard and Lightweight.
        let result = build_handoff(
            &hardware(),
            &verified_for(CibosProfile::Performance),
            &regions(),
            [3u8; ENTROPY_SEED_LEN],
        );
        let (_, decoded) = result.expect("handoff builds");
        assert_eq!(decoded.cibos_profile, CibosProfile::Performance);
        assert_eq!(decoded.architecture, ProcessorArchitecture::X86_64);
    }

    #[test]
    fn rejects_invalid_kernel_profile_discriminant() {
        let mut v = verified_for(CibosProfile::Performance);
        v.cibos_profile = 999; // not a valid CibosProfile
        let result = build_handoff(&hardware(), &v, &regions(), [0u8; ENTROPY_SEED_LEN]);
        assert!(result.is_err());
    }
}
