//! # Firmware → Kernel Handoff Protocol
//!
//! The single data structure CIBIOS writes and CIBOS reads at the moment
//! control transfers from firmware to kernel. This is the most layout-sensitive
//! type in the system: it is produced by one independently-compiled binary and
//! consumed by another, so its representation is fixed, versioned, and
//! self-validating.
//!
//! ## Naming
//!
//! There is exactly one handoff type, [`HandoffData`]. (Earlier drafts referred
//! to both `HandoffProtocol` and `HandoffData`; this module is the single
//! source of truth and uses [`HandoffData`] throughout.)
//!
//! ## Layout and validation
//!
//! [`HandoffData`] is `#[repr(C)]` with a leading magic number and version so
//! the kernel can reject a structure it does not understand rather than
//! interpreting garbage. Variable-length data (the memory map) is carried as a
//! fixed-capacity array plus a count, which keeps the whole structure
//! POD-copyable and free of pointers that would not survive the address-space
//! transition.
//!
//! The kernel calls [`HandoffData::validate`] immediately on receipt. Only
//! after validation succeeds does it trust any field.

use crate::types::config::{CibiosProfile, CibosProfile, HandoffMode};
use crate::types::error::ProtocolError;
use crate::types::hardware::{
    CoreTopology, HardwarePlatform, MemoryRegion, MemoryRegionKind, ProcessorArchitecture,
};

/// Magic number identifying a valid handoff structure: ASCII "CIBH" (CIBios
/// Handoff), little-endian.
pub const HANDOFF_MAGIC: u32 = 0x4842_4943;

/// The handoff protocol version this build speaks. The kernel rejects any
/// version it does not recognize.
pub const HANDOFF_VERSION: u32 = 1;

/// Maximum number of memory regions carried in the handoff memory map.
pub const MAX_MEMORY_REGIONS: usize = 64;

/// Length of the entropy seed handed to the kernel, in bytes.
pub const ENTROPY_SEED_LEN: usize = 32;

/// The complete firmware→kernel handoff structure.
///
/// `#[repr(C)]` for a stable, predictable layout across the two binaries. All
/// fields are plain data; there are no pointers, because a pointer valid in the
/// firmware's view of memory may not be valid in the kernel's.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct HandoffData {
    /// Must equal [`HANDOFF_MAGIC`]. First field so a wrong structure is caught
    /// immediately.
    pub magic: u32,
    /// Must equal [`HANDOFF_VERSION`].
    pub version: u32,
    /// Processor architecture, as [`ProcessorArchitecture`] discriminant.
    pub architecture: u32,
    /// Device class, as [`HardwarePlatform`] discriminant.
    pub platform: u32,
    /// Firmware profile, as [`CibiosProfile`] discriminant.
    pub cibios_profile: u32,
    /// Kernel profile, as [`CibosProfile`] discriminant.
    pub cibos_profile: u32,
    /// Handoff mode, as [`HandoffMode`] discriminant.
    pub handoff_mode: u32,
    /// Number of physical cores.
    pub physical_cores: u32,
    /// Number of logical cores (after SMT configuration).
    pub logical_cores: u32,
    /// Whether SMT is enabled (`0` = no, `1` = yes).
    pub smt_enabled: u32,
    /// Total usable RAM in bytes.
    pub total_memory: u64,
    /// Number of valid entries in [`Self::memory_regions`].
    pub memory_region_count: u32,
    /// Padding to keep the following array 8-byte aligned and the layout
    /// explicit. Always zero.
    pub _reserved: u32,
    /// The physical memory map established by firmware.
    pub memory_regions: [HandoffMemoryRegion; MAX_MEMORY_REGIONS],
    /// Entropy seed gathered by firmware from the hardware RNG, for the kernel
    /// to seed its own CSPRNG before its entropy sources are up.
    pub entropy_seed: [u8; ENTROPY_SEED_LEN],
}

/// A memory region in the handoff map. A `#[repr(C)]` mirror of
/// [`MemoryRegion`] using only POD fields (the `kind` is stored as its `u32`
/// discriminant).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct HandoffMemoryRegion {
    /// Physical base address.
    pub base: u64,
    /// Length in bytes.
    pub length: u64,
    /// Region kind, as [`MemoryRegionKind`] discriminant.
    pub kind: u32,
    /// Padding to 8-byte alignment. Always zero.
    pub _reserved: u32,
}

impl HandoffMemoryRegion {
    /// An all-zero region used to pad the fixed-capacity array.
    pub const ZERO: HandoffMemoryRegion = HandoffMemoryRegion {
        base: 0,
        length: 0,
        kind: 0,
        _reserved: 0,
    };

    /// Build a handoff region from a typed [`MemoryRegion`].
    #[must_use]
    pub const fn from_region(region: MemoryRegion) -> Self {
        Self {
            base: region.base,
            length: region.length,
            kind: region.kind.as_u32(),
            _reserved: 0,
        }
    }

    /// Convert back to a typed [`MemoryRegion`], validating the kind.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::HandoffRejected`] if the kind discriminant is
    /// not a known [`MemoryRegionKind`].
    pub fn to_region(self) -> Result<MemoryRegion, ProtocolError> {
        let kind = MemoryRegionKind::try_from(self.kind).map_err(|_| {
            ProtocolError::HandoffRejected {
                detail: "invalid memory region kind",
            }
        })?;
        Ok(MemoryRegion {
            base: self.base,
            length: self.length,
            kind,
        })
    }
}

impl HandoffData {
    /// Construct a handoff structure from typed components (firmware side).
    ///
    /// The caller supplies the typed values; this packs them into the stable
    /// representation, fills the magic/version, and copies the memory map.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::HandoffRejected`] if more than
    /// [`MAX_MEMORY_REGIONS`] regions are supplied.
    // The handoff record is, by its nature, the full system description that
    // firmware assembles from several independent sources (architecture probe,
    // build-time profile selection, memory map, hardware RNG). Bundling these
    // distinct inputs into an artificial parameter struct purely to satisfy the
    // argument-count lint would obscure rather than clarify the call site.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        architecture: ProcessorArchitecture,
        platform: HardwarePlatform,
        cibios_profile: CibiosProfile,
        cibos_profile: CibosProfile,
        handoff_mode: HandoffMode,
        topology: CoreTopology,
        total_memory: u64,
        regions: &[MemoryRegion],
        entropy_seed: [u8; ENTROPY_SEED_LEN],
    ) -> Result<Self, ProtocolError> {
        if regions.len() > MAX_MEMORY_REGIONS {
            return Err(ProtocolError::HandoffRejected {
                detail: "too many memory regions",
            });
        }

        let mut memory_regions = [HandoffMemoryRegion::ZERO; MAX_MEMORY_REGIONS];
        for (slot, region) in memory_regions.iter_mut().zip(regions.iter()) {
            *slot = HandoffMemoryRegion::from_region(*region);
        }

        Ok(Self {
            magic: HANDOFF_MAGIC,
            version: HANDOFF_VERSION,
            architecture: architecture.as_u32(),
            platform: platform.as_u32(),
            cibios_profile: cibios_profile.as_u32(),
            cibos_profile: cibos_profile.as_u32(),
            handoff_mode: handoff_mode.as_u32(),
            physical_cores: topology.physical_cores,
            logical_cores: topology.logical_cores,
            smt_enabled: u32::from(topology.smt_enabled),
            total_memory,
            memory_region_count: regions.len() as u32,
            _reserved: 0,
            memory_regions,
            entropy_seed,
        })
    }

    /// Validate the structure on receipt (kernel side).
    ///
    /// Checks the magic, version, region count, and the consistency of the
    /// firmware/kernel profile pairing. Returns a [`DecodedHandoff`] with all
    /// fields converted to their typed forms only if every check passes.
    ///
    /// # Errors
    ///
    /// Returns a [`ProtocolError`] describing the first failed check.
    pub fn validate(&self) -> Result<DecodedHandoff, ProtocolError> {
        if self.magic != HANDOFF_MAGIC {
            return Err(ProtocolError::HandoffRejected {
                detail: "bad magic number",
            });
        }
        if self.version != HANDOFF_VERSION {
            return Err(ProtocolError::VersionMismatch {
                local: HANDOFF_VERSION,
                remote: self.version,
            });
        }
        if self.memory_region_count as usize > MAX_MEMORY_REGIONS {
            return Err(ProtocolError::HandoffRejected {
                detail: "memory region count exceeds capacity",
            });
        }

        let architecture =
            ProcessorArchitecture::try_from(self.architecture).map_err(|_| {
                ProtocolError::HandoffRejected {
                    detail: "invalid architecture",
                }
            })?;
        let platform = HardwarePlatform::try_from(self.platform).map_err(|_| {
            ProtocolError::HandoffRejected {
                detail: "invalid platform",
            }
        })?;
        let cibios_profile = CibiosProfile::try_from(self.cibios_profile).map_err(|_| {
            ProtocolError::HandoffRejected {
                detail: "invalid cibios profile",
            }
        })?;
        let cibos_profile = CibosProfile::try_from(self.cibos_profile).map_err(|_| {
            ProtocolError::HandoffRejected {
                detail: "invalid cibos profile",
            }
        })?;
        let handoff_mode = HandoffMode::try_from(self.handoff_mode).map_err(|_| {
            ProtocolError::HandoffRejected {
                detail: "invalid handoff mode",
            }
        })?;

        // The firmware and kernel profiles must form a valid pair, and the
        // handoff mode must be the one that pairing implies.
        if !cibios_profile.accepts(cibos_profile) {
            return Err(ProtocolError::HandoffRejected {
                detail: "incompatible firmware/kernel profile pairing",
            });
        }
        if cibios_profile.handoff_mode() != handoff_mode {
            return Err(ProtocolError::HandoffRejected {
                detail: "handoff mode does not match firmware profile",
            });
        }

        let topology = CoreTopology::new(
            self.physical_cores,
            self.logical_cores,
            self.smt_enabled != 0,
        )
        .map_err(|_| ProtocolError::HandoffRejected {
            detail: "inconsistent core topology",
        })?;

        Ok(DecodedHandoff {
            architecture,
            platform,
            cibios_profile,
            cibos_profile,
            handoff_mode,
            topology,
            total_memory: self.total_memory,
            entropy_seed: self.entropy_seed,
        })
    }

    /// Iterate the valid memory regions, converting each to its typed form.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::HandoffRejected`] if any region has an invalid
    /// kind discriminant.
    pub fn typed_regions(
        &self,
    ) -> Result<MemoryRegionIter<'_>, ProtocolError> {
        let count = self.memory_region_count as usize;
        if count > MAX_MEMORY_REGIONS {
            return Err(ProtocolError::HandoffRejected {
                detail: "memory region count exceeds capacity",
            });
        }
        Ok(MemoryRegionIter {
            regions: &self.memory_regions[..count],
            index: 0,
        })
    }
}

/// The validated, typed view of a [`HandoffData`], produced by
/// [`HandoffData::validate`]. The kernel works with this rather than the raw
/// `#[repr(C)]` structure.
#[derive(Debug, Clone, Copy)]
pub struct DecodedHandoff {
    /// Processor architecture.
    pub architecture: ProcessorArchitecture,
    /// Device class.
    pub platform: HardwarePlatform,
    /// Firmware profile.
    pub cibios_profile: CibiosProfile,
    /// Kernel profile.
    pub cibos_profile: CibosProfile,
    /// Negotiated handoff mode.
    pub handoff_mode: HandoffMode,
    /// Core topology.
    pub topology: CoreTopology,
    /// Total usable RAM in bytes.
    pub total_memory: u64,
    /// Entropy seed from firmware.
    pub entropy_seed: [u8; ENTROPY_SEED_LEN],
}

/// Iterator over the typed memory regions of a [`HandoffData`].
pub struct MemoryRegionIter<'a> {
    regions: &'a [HandoffMemoryRegion],
    index: usize,
}

impl Iterator for MemoryRegionIter<'_> {
    type Item = Result<MemoryRegion, ProtocolError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.regions.len() {
            return None;
        }
        let region = self.regions[self.index];
        self.index += 1;
        Some(region.to_region())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_regions() -> [MemoryRegion; 2] {
        [
            MemoryRegion {
                base: 0x10_0000,
                length: 0x100_0000,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                base: 0x0,
                length: 0x10_0000,
                kind: MemoryRegionKind::FirmwareReserved,
            },
        ]
    }

    #[test]
    fn roundtrip_valid_handoff() {
        let topology = CoreTopology::new(4, 4, false).unwrap();
        let regions = sample_regions();
        let handoff = HandoffData::new(
            ProcessorArchitecture::X86_64,
            HardwarePlatform::Desktop,
            CibiosProfile::Standard,
            CibosProfile::Balanced,
            HandoffMode::Cryptographic,
            topology,
            8 * 1024 * 1024 * 1024,
            &regions,
            [7u8; ENTROPY_SEED_LEN],
        )
        .expect("construct");

        let decoded = handoff.validate().expect("validate");
        assert_eq!(decoded.architecture, ProcessorArchitecture::X86_64);
        assert_eq!(decoded.cibos_profile, CibosProfile::Balanced);
        assert_eq!(decoded.handoff_mode, HandoffMode::Cryptographic);
        assert_eq!(decoded.entropy_seed, [7u8; ENTROPY_SEED_LEN]);

        let collected: Result<heapless::Vec<MemoryRegion, 8>, _> =
            handoff.typed_regions().unwrap().collect();
        let collected = collected.expect("regions decode");
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].kind, MemoryRegionKind::Usable);
    }

    #[test]
    fn rejects_bad_magic() {
        let topology = CoreTopology::new(1, 1, false).unwrap();
        let mut handoff = HandoffData::new(
            ProcessorArchitecture::AArch64,
            HardwarePlatform::Mobile,
            CibiosProfile::Lightweight,
            CibosProfile::Compute,
            HandoffMode::Lightweight,
            topology,
            1024,
            &[],
            [0u8; ENTROPY_SEED_LEN],
        )
        .unwrap();
        handoff.magic = 0xDEAD_BEEF;
        assert!(handoff.validate().is_err());
    }

    #[test]
    fn rejects_incompatible_pairing() {
        // Standard firmware must NOT accept the Compute kernel profile.
        let topology = CoreTopology::new(1, 1, false).unwrap();
        let handoff = HandoffData::new(
            ProcessorArchitecture::X86_64,
            HardwarePlatform::Server,
            CibiosProfile::Standard,
            CibosProfile::Compute,
            HandoffMode::Cryptographic,
            topology,
            1024,
            &[],
            [0u8; ENTROPY_SEED_LEN],
        )
        .unwrap();
        assert!(handoff.validate().is_err());
    }

    #[test]
    fn rejects_mode_mismatch() {
        let topology = CoreTopology::new(1, 1, false).unwrap();
        let mut handoff = HandoffData::new(
            ProcessorArchitecture::X86_64,
            HardwarePlatform::Desktop,
            CibiosProfile::Standard,
            CibosProfile::Balanced,
            HandoffMode::Cryptographic,
            topology,
            1024,
            &[],
            [0u8; ENTROPY_SEED_LEN],
        )
        .unwrap();
        // Tamper the mode so it no longer matches the Standard firmware profile.
        handoff.handoff_mode = HandoffMode::Lightweight.as_u32();
        assert!(handoff.validate().is_err());
    }
}
