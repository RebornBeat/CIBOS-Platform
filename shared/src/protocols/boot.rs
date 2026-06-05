//! # Bootloader → CIBIOS Handoff Protocol (legacy BIOS path)
//!
//! This is the layer *below* CIBIOS. A from-scratch bootloader (no GRUB, no
//! multiboot) loads the CIBIOS and CIBOS images into physical memory, gathers
//! the BIOS E820 memory map, builds a [`BootHandoff`], and transfers control to
//! the CIBIOS entry point. CIBIOS consumes this [`BootHandoff`], performs its
//! hardware init and isolation setup, then produces its own
//! [`HandoffData`](super::handoff::HandoffData) for CIBOS. The two contracts are
//! distinct on purpose: [`BootHandoff`] is bootloader→CIBIOS;
//! [`HandoffData`](super::handoff::HandoffData) is CIBIOS→CIBOS.
//!
//! ## Division of responsibility
//!
//! The bootloader treats the CIBOS image as an **opaque blob**. It loads the
//! `.cimg` bytes into RAM and records `(address, size)` in the handoff; it does
//! *not* parse the image, place components, verify signatures, or compute the
//! kernel entry point. CIBIOS already owns all of that (see the firmware's
//! `boot_image`: it parses the image with `ImageView`, copies each component to
//! its own `load_addr`, verifies, and jumps). So the bootloader needs only the
//! CIBIOS entry address (to jump to firmware) and the CIBOS blob location (to
//! hand to firmware) — nothing about CIBOS's internal layout.
//!
//! ## Layout and validation
//!
//! Like [`HandoffData`](super::handoff::HandoffData), the structures here are
//! `#[repr(C)]` with a leading magic and version so the consumer rejects an
//! unrecognized structure rather than interpreting garbage. These structures
//! are also written by *assembly* (`bootloader/boot/stage2.s`) and by the host
//! image tool (`tools/mkbootimage`), so the field offsets are a hard ABI: the
//! `const` offset assertions at the bottom of this file fail the build if any
//! offset drifts, which is the safeguard against silent contract breakage
//! between the Rust definition, the assembly, and the tool.
//!
//! ## Why `#[repr(C, align(8))]`
//!
//! The bootloader may run on a 32-bit (i686) or 64-bit (x86_64) CPU, and CIBIOS
//! reads the same bytes possibly on the other side of that boundary. Plain
//! `#[repr(C)]` gives these structures 4-byte alignment on i686 (where `u64`
//! aligns to 4) and 8-byte on x86_64, which can change trailing padding and
//! size. Forcing `align(8)` pins size, alignment, and every field offset to the
//! same values on both targets, so the wire layout is genuinely identical
//! regardless of which CPU wrote it. The `const` assertions are checked on every
//! target the crate builds for, including i686 — which is what surfaced this.

use core::mem::{align_of, offset_of, size_of};

/// Magic for the on-disk Boot Layout Descriptor: ASCII `"CIBOSBL1"`,
/// little-endian.
pub const BLD_MAGIC: u64 = u64::from_le_bytes(*b"CIBOSBL1");

/// Magic for the in-memory [`BootHandoff`]: ASCII `"CIBOSHO1"`, little-endian.
pub const BOOT_HANDOFF_MAGIC: u64 = u64::from_le_bytes(*b"CIBOSHO1");

/// Current Boot Layout Descriptor format version.
pub const BLD_VERSION: u32 = 1;

/// Current [`BootHandoff`] format version.
pub const BOOT_HANDOFF_VERSION: u32 = 1;

/// One physical-memory region, mirroring a BIOS INT 15h E820 entry exactly
/// (base / length / type / ACPI-3.0 extended-attribute dword = 24 bytes).
///
/// The bootloader copies E820 entries verbatim into an array of these, so the
/// layout must stay byte-identical to the E820 record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct BootMemoryRegion {
    /// Physical base address of the region.
    pub base: u64,
    /// Length of the region in bytes.
    pub length: u64,
    /// Region type (see [`BootRegionType`]).
    pub region_type: u32,
    /// ACPI 3.0 extended attributes. Bit 0 clear ⇒ ignore this entry.
    pub acpi_attributes: u32,
}

/// E820 region-type values. These are the BIOS-defined constants, copied
/// verbatim from the firmware's E820 map; they are independent of the system's
/// internal `MemoryRegionKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum BootRegionType {
    /// Free RAM available for use.
    Usable = 1,
    /// Reserved; must not be used.
    Reserved = 2,
    /// ACPI tables; reclaimable after ACPI is parsed.
    AcpiReclaimable = 3,
    /// ACPI non-volatile storage; must be preserved.
    AcpiNvs = 4,
    /// Memory that failed testing; unusable.
    BadMemory = 5,
}

impl BootMemoryRegion {
    /// Interpret the raw `region_type` as a [`BootRegionType`], if recognized.
    #[must_use]
    pub fn classify(&self) -> Option<BootRegionType> {
        match self.region_type {
            1 => Some(BootRegionType::Usable),
            2 => Some(BootRegionType::Reserved),
            3 => Some(BootRegionType::AcpiReclaimable),
            4 => Some(BootRegionType::AcpiNvs),
            5 => Some(BootRegionType::BadMemory),
            _ => None,
        }
    }

    /// True if this region is usable RAM that the BIOS did not flag invalid via
    /// the ACPI extended-attribute bit.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.region_type == BootRegionType::Usable as u32 && (self.acpi_attributes & 1) != 0
    }
}

/// The structure the bootloader hands to CIBIOS.
///
/// On x86_64 the bootloader passes a physical pointer to this in `RDI` (the
/// System V first integer argument) with the CPU already in 64-bit long mode
/// under an identity-mapped page table (`page_table_root`). On i686 the
/// bootloader passes the pointer as the first cdecl stack argument in 32-bit
/// protected mode with a flat GDT and paging disabled.
///
/// All fields are plain data and the structure is self-contained except for the
/// one unavoidable pointer, `memory_regions_ptr`, which is a *physical* address
/// the bootloader leaves identity-mapped for CIBIOS to read.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(8))]
pub struct BootHandoff {
    /// Must equal [`BOOT_HANDOFF_MAGIC`]. First field so a wrong structure is
    /// caught immediately.
    pub magic: u64,
    /// Must equal [`BOOT_HANDOFF_VERSION`].
    pub version: u32,
    /// Reserved flag bits (currently zero).
    pub flags: u32,
    /// BIOS boot drive number (the `DL` value at power-on).
    pub boot_drive: u32,
    /// Number of [`BootMemoryRegion`] entries at `memory_regions_ptr`.
    pub memory_region_count: u32,
    /// Physical pointer to the `memory_region_count`-long region array.
    pub memory_regions_ptr: u64,
    /// Physical address where the CIBOS kernel image (`.cimg`) was loaded.
    pub cibos_image_addr: u64,
    /// Size of the CIBOS kernel image in bytes.
    pub cibos_image_size: u64,
    /// Physical address where the CIBIOS image was loaded.
    pub cibios_image_addr: u64,
    /// Size of the CIBIOS image in bytes.
    pub cibios_image_size: u64,
    /// Physical address of the PML4 the bootloader installed (x86_64). Zero on
    /// i686, where the bootloader leaves paging disabled.
    pub page_table_root: u64,
    /// Physical address where Stage 2 was loaded (so CIBIOS may reclaim it).
    pub stage2_addr: u64,
    /// Size of Stage 2 in bytes.
    pub stage2_size: u64,
}

impl BootHandoff {
    /// Validate magic and version. CIBIOS calls this before trusting any field.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.magic == BOOT_HANDOFF_MAGIC && self.version == BOOT_HANDOFF_VERSION
    }
}

// The memory-region array (`memory_regions_ptr`/`memory_region_count`) and the
// CIBOS image blob (`cibos_image_addr`/`cibos_image_size`) are raw physical
// addresses. Reconstructing slices from them requires `core::slice::from_raw_parts`,
// which is `unsafe`; this crate is `#![forbid(unsafe_code)]`, so — exactly as with
// the firmware→kernel `HandoffData` — the *consumer* (the CIBIOS arch layer, which
// already performs raw reads of multiboot data) does that reconstruction against
// the addresses and counts validated here.

/// The on-disk descriptor at LBA 1 of the boot medium.
///
/// Stage 1 reads it to find Stage 2; Stage 2 reads it to find CIBIOS and CIBOS.
/// Written by `tools/mkbootimage` directly from this type so the bytes on disk
/// always match the contract the bootloader reads.
///
/// Note the asymmetry between CIBIOS and CIBOS fields: CIBIOS has an
/// `entry` (the loader jumps to firmware, so it needs the firmware entry),
/// while CIBOS does not (CIBIOS, not the loader, parses the `.cimg` and computes
/// the kernel entry). CIBOS therefore carries an exact byte `size` so the
/// opaque blob is handed over with its true length even when the sector count
/// rounds up.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(8))]
pub struct BootLayoutDescriptor {
    /// Must equal [`BLD_MAGIC`].
    pub magic: u64,
    /// Must equal [`BLD_VERSION`].
    pub version: u32,
    /// Padding to keep the following `u64` fields 8-byte aligned. Always zero.
    pub _pad0: u32,
    /// LBA where Stage 2 begins.
    pub stage2_lba: u64,
    /// Stage 2 length in 512-byte sectors.
    pub stage2_sectors: u32,
    /// Padding. Always zero.
    pub _pad1: u32,
    /// Physical address Stage 1 loads Stage 2 to (conventionally `0x8000`).
    pub stage2_load_addr: u64,
    /// LBA where the CIBIOS image begins.
    pub cibios_lba: u64,
    /// CIBIOS length in 512-byte sectors.
    pub cibios_sectors: u32,
    /// Padding. Always zero.
    pub _pad2: u32,
    /// Physical address Stage 2 loads CIBIOS to.
    pub cibios_load_addr: u64,
    /// Physical entry point of CIBIOS (the address the loader jumps to).
    pub cibios_entry: u64,
    /// LBA where the CIBOS image begins.
    pub cibos_lba: u64,
    /// CIBOS length in 512-byte sectors.
    pub cibos_sectors: u32,
    /// Padding. Always zero.
    pub _pad3: u32,
    /// Physical address Stage 2 loads CIBOS to.
    pub cibos_load_addr: u64,
    /// Exact CIBOS image size in bytes (the sector count may round up).
    pub cibos_size: u64,
}

impl BootLayoutDescriptor {
    /// Encoded size in bytes (equal to `size_of::<Self>()`, pinned by the
    /// offset assertions below).
    pub const ENCODED_LEN: usize = 104;

    /// Validate magic and version.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.magic == BLD_MAGIC && self.version == BLD_VERSION
    }

    /// Serialize into the on-disk byte layout (little-endian, fields in
    /// declared order). The declared order is the `#[repr(C)]` order, and the
    /// offset assertions at the bottom of this module guarantee the resulting
    /// bytes land at the offsets the bootloader assembly reads. Used by
    /// `tools/mkbootimage` so the bytes on disk cannot drift from this type.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::ENCODED_LEN] {
        use crate::utils::serialization::ByteWriter;
        let mut buf = [0u8; Self::ENCODED_LEN];
        {
            let mut w = ByteWriter::new(&mut buf);
            // Every put_* here fits within ENCODED_LEN, so unwrap cannot fail;
            // a layout change would trip the const assertions, not panic here.
            w.put_u64(self.magic).unwrap();
            w.put_u32(self.version).unwrap();
            w.put_u32(self._pad0).unwrap();
            w.put_u64(self.stage2_lba).unwrap();
            w.put_u32(self.stage2_sectors).unwrap();
            w.put_u32(self._pad1).unwrap();
            w.put_u64(self.stage2_load_addr).unwrap();
            w.put_u64(self.cibios_lba).unwrap();
            w.put_u32(self.cibios_sectors).unwrap();
            w.put_u32(self._pad2).unwrap();
            w.put_u64(self.cibios_load_addr).unwrap();
            w.put_u64(self.cibios_entry).unwrap();
            w.put_u64(self.cibos_lba).unwrap();
            w.put_u32(self.cibos_sectors).unwrap();
            w.put_u32(self._pad3).unwrap();
            w.put_u64(self.cibos_load_addr).unwrap();
            w.put_u64(self.cibos_size).unwrap();
        }
        buf
    }

    /// Parse from the on-disk byte layout. The inverse of [`to_bytes`].
    ///
    /// [`to_bytes`]: BootLayoutDescriptor::to_bytes
    ///
    /// # Errors
    ///
    /// Returns [`SerializationError`] if `bytes` is shorter than
    /// [`ENCODED_LEN`].
    ///
    /// [`SerializationError`]: crate::types::error::SerializationError
    /// [`ENCODED_LEN`]: BootLayoutDescriptor::ENCODED_LEN
    pub fn from_bytes(
        bytes: &[u8],
    ) -> Result<Self, crate::types::error::SerializationError> {
        use crate::utils::serialization::ByteReader;
        let mut r = ByteReader::new(bytes);
        Ok(Self {
            magic: r.get_u64()?,
            version: r.get_u32()?,
            _pad0: r.get_u32()?,
            stage2_lba: r.get_u64()?,
            stage2_sectors: r.get_u32()?,
            _pad1: r.get_u32()?,
            stage2_load_addr: r.get_u64()?,
            cibios_lba: r.get_u64()?,
            cibios_sectors: r.get_u32()?,
            _pad2: r.get_u32()?,
            cibios_load_addr: r.get_u64()?,
            cibios_entry: r.get_u64()?,
            cibos_lba: r.get_u64()?,
            cibos_sectors: r.get_u32()?,
            _pad3: r.get_u32()?,
            cibos_load_addr: r.get_u64()?,
            cibos_size: r.get_u64()?,
        })
    }
}

// ---------------------------------------------------------------------------
// Hard ABI: these fail the build if any offset or size drifts from the values
// the assembly (`bootloader/boot/stage1.s`, `stage2.s`) and the host image tool
// (`tools/mkbootimage`) are written against. Treat a failure here as a contract
// break to be reconciled across all three, never as an assertion to relax.
// ---------------------------------------------------------------------------

const _: () = {
    // BootMemoryRegion mirrors the 24-byte E820 record exactly.
    assert!(size_of::<BootMemoryRegion>() == 24);
    assert!(align_of::<BootMemoryRegion>() == 8);
    assert!(offset_of!(BootMemoryRegion, base) == 0);
    assert!(offset_of!(BootMemoryRegion, length) == 8);
    assert!(offset_of!(BootMemoryRegion, region_type) == 16);
    assert!(offset_of!(BootMemoryRegion, acpi_attributes) == 20);

    // BootHandoff — matches the HO_* offsets in stage2.s.
    assert!(size_of::<BootHandoff>() == 88);
    assert!(align_of::<BootHandoff>() == 8);
    assert!(offset_of!(BootHandoff, magic) == 0);
    assert!(offset_of!(BootHandoff, version) == 8);
    assert!(offset_of!(BootHandoff, flags) == 12);
    assert!(offset_of!(BootHandoff, boot_drive) == 16);
    assert!(offset_of!(BootHandoff, memory_region_count) == 20);
    assert!(offset_of!(BootHandoff, memory_regions_ptr) == 24);
    assert!(offset_of!(BootHandoff, cibos_image_addr) == 32);
    assert!(offset_of!(BootHandoff, cibos_image_size) == 40);
    assert!(offset_of!(BootHandoff, cibios_image_addr) == 48);
    assert!(offset_of!(BootHandoff, cibios_image_size) == 56);
    assert!(offset_of!(BootHandoff, page_table_root) == 64);
    assert!(offset_of!(BootHandoff, stage2_addr) == 72);
    assert!(offset_of!(BootHandoff, stage2_size) == 80);

    // BootLayoutDescriptor — matches the BLD_* offsets in stage1.s / stage2.s.
    assert!(size_of::<BootLayoutDescriptor>() == 104);
    assert!(align_of::<BootLayoutDescriptor>() == 8);
    assert!(offset_of!(BootLayoutDescriptor, magic) == 0);
    assert!(offset_of!(BootLayoutDescriptor, version) == 8);
    assert!(offset_of!(BootLayoutDescriptor, stage2_lba) == 16);
    assert!(offset_of!(BootLayoutDescriptor, stage2_sectors) == 24);
    assert!(offset_of!(BootLayoutDescriptor, stage2_load_addr) == 32);
    assert!(offset_of!(BootLayoutDescriptor, cibios_lba) == 40);
    assert!(offset_of!(BootLayoutDescriptor, cibios_sectors) == 48);
    assert!(offset_of!(BootLayoutDescriptor, cibios_load_addr) == 56);
    assert!(offset_of!(BootLayoutDescriptor, cibios_entry) == 64);
    assert!(offset_of!(BootLayoutDescriptor, cibos_lba) == 72);
    assert!(offset_of!(BootLayoutDescriptor, cibos_sectors) == 80);
    assert!(offset_of!(BootLayoutDescriptor, cibos_load_addr) == 88);
    assert!(offset_of!(BootLayoutDescriptor, cibos_size) == 96);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magics_are_ascii_tags() {
        assert_eq!(&BLD_MAGIC.to_le_bytes(), b"CIBOSBL1");
        assert_eq!(&BOOT_HANDOFF_MAGIC.to_le_bytes(), b"CIBOSHO1");
    }

    #[test]
    fn usable_region_requires_attr_bit() {
        let r = BootMemoryRegion {
            base: 0,
            length: 0x1000,
            region_type: 1,
            acpi_attributes: 1,
        };
        assert!(r.is_usable());
        let bad = BootMemoryRegion {
            acpi_attributes: 0,
            ..r
        };
        assert!(!bad.is_usable());
        let reserved = BootMemoryRegion {
            region_type: 2,
            ..r
        };
        assert!(!reserved.is_usable());
    }

    #[test]
    fn region_classify_covers_known_types() {
        let mk = |t: u32| BootMemoryRegion {
            base: 0,
            length: 1,
            region_type: t,
            acpi_attributes: 1,
        };
        assert_eq!(mk(1).classify(), Some(BootRegionType::Usable));
        assert_eq!(mk(2).classify(), Some(BootRegionType::Reserved));
        assert_eq!(mk(3).classify(), Some(BootRegionType::AcpiReclaimable));
        assert_eq!(mk(4).classify(), Some(BootRegionType::AcpiNvs));
        assert_eq!(mk(5).classify(), Some(BootRegionType::BadMemory));
        assert_eq!(mk(99).classify(), None);
    }

    #[test]
    fn handoff_validates_magic_and_version() {
        let h = BootHandoff {
            magic: BOOT_HANDOFF_MAGIC,
            version: BOOT_HANDOFF_VERSION,
            flags: 0,
            boot_drive: 0x80,
            memory_region_count: 0,
            memory_regions_ptr: 0,
            cibos_image_addr: 0,
            cibos_image_size: 0,
            cibios_image_addr: 0,
            cibios_image_size: 0,
            page_table_root: 0,
            stage2_addr: 0,
            stage2_size: 0,
        };
        assert!(h.is_valid());
        assert!(!BootHandoff { magic: 0, ..h }.is_valid());
        assert!(!BootHandoff { version: 99, ..h }.is_valid());
    }

    #[test]
    fn descriptor_validates_magic_and_version() {
        let d = BootLayoutDescriptor {
            magic: BLD_MAGIC,
            version: BLD_VERSION,
            _pad0: 0,
            stage2_lba: 2,
            stage2_sectors: 16,
            _pad1: 0,
            stage2_load_addr: 0x8000,
            cibios_lba: 18,
            cibios_sectors: 64,
            _pad2: 0,
            cibios_load_addr: 0x10_0000,
            cibios_entry: 0x10_0000,
            cibos_lba: 82,
            cibos_sectors: 128,
            _pad3: 0,
            cibos_load_addr: 0x400_0000,
            cibos_size: 65_536,
        };
        assert!(d.is_valid());
        assert!(!BootLayoutDescriptor { magic: 0, ..d }.is_valid());
    }

    #[test]
    fn descriptor_round_trips_through_bytes() {
        let d = BootLayoutDescriptor {
            magic: BLD_MAGIC,
            version: BLD_VERSION,
            _pad0: 0,
            stage2_lba: 2,
            stage2_sectors: 16,
            _pad1: 0,
            stage2_load_addr: 0x8000,
            cibios_lba: 18,
            cibios_sectors: 64,
            _pad2: 0,
            cibios_load_addr: 0x10_0000,
            cibios_entry: 0x10_0000,
            cibos_lba: 82,
            cibos_sectors: 128,
            _pad3: 0,
            cibos_load_addr: 0x400_0000,
            cibos_size: 65_536,
        };
        let bytes = d.to_bytes();
        assert_eq!(bytes.len(), BootLayoutDescriptor::ENCODED_LEN);
        let back = BootLayoutDescriptor::from_bytes(&bytes).expect("parse");
        // Round-trip equality across every field (compare the encodings, since
        // the struct has padding fields).
        assert_eq!(back.to_bytes(), bytes);
        assert_eq!(back.cibios_entry, 0x10_0000);
        assert_eq!(back.cibos_size, 65_536);
    }

    #[test]
    fn descriptor_bytes_match_repr_c_offsets() {
        // The serialized bytes must place each field at exactly the offset the
        // bootloader assembly reads. Spot-check the offsets the asm depends on.
        let d = BootLayoutDescriptor {
            magic: BLD_MAGIC,
            version: BLD_VERSION,
            _pad0: 0,
            stage2_lba: 0x1122_3344_5566_7788,
            stage2_sectors: 0xAABB_CCDD,
            _pad1: 0,
            stage2_load_addr: 0x8000,
            cibios_lba: 0,
            cibios_sectors: 0,
            _pad2: 0,
            cibios_load_addr: 0,
            cibios_entry: 0x0010_0000,
            cibos_lba: 0,
            cibos_sectors: 0,
            _pad3: 0,
            cibos_load_addr: 0,
            cibos_size: 0,
        };
        let b = d.to_bytes();
        // magic at 0
        assert_eq!(&b[0..8], &BLD_MAGIC.to_le_bytes());
        // version at 8
        assert_eq!(&b[8..12], &BLD_VERSION.to_le_bytes());
        // stage2_lba at 16
        assert_eq!(&b[16..24], &0x1122_3344_5566_7788u64.to_le_bytes());
        // stage2_sectors at 24
        assert_eq!(&b[24..28], &0xAABB_CCDDu32.to_le_bytes());
        // stage2_load_addr at 32
        assert_eq!(&b[32..40], &0x8000u64.to_le_bytes());
        // cibios_entry at 64
        assert_eq!(&b[64..72], &0x0010_0000u64.to_le_bytes());
    }
}
