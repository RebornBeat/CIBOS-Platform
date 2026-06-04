//! # Multiboot1 Memory Map Parser (Minimal)
//!
//! The x86_64 counterpart to [`crate::fdt`]: when CIBIOS is loaded by a
//! multiboot1 loader (such as QEMU `-kernel`), the loader passes a pointer to a
//! Multiboot Information structure, which contains the BIOS memory map. This
//! module extracts the total available RAM and the base of the first available
//! region from the memory-map entries.
//!
//! Pure, `unsafe`-free parsing over a borrowed slice of the memory-map bytes,
//! so it lives in the logic library and is host-tested. The architecture code
//! reads the `mmap_addr`/`mmap_length` fields from the info structure (those
//! reads *are* `unsafe`, in the arch layer) and passes the memory-map bytes
//! here.
//!
//! Each memory-map entry has the layout (little-endian):
//! ```text
//!   u32 size       // size of the entry NOT including this field
//!   u64 base_addr
//!   u64 length
//!   u32 type        // 1 = available RAM
//! ```

use crate::error::FirmwareError;
use shared::utils::serialization::ByteReader;

/// Memory-map entry type for "available RAM".
const MMAP_TYPE_AVAILABLE: u32 = 1;

/// Multiboot info `flags` bit 3: the boot modules fields are valid.
const MB_FLAG_MODS: u32 = 1 << 3;

/// A boot module's byte range, as described by the multiboot module table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultibootModule {
    /// Physical start address of the module.
    pub start: u64,
    /// Physical end address (exclusive) of the module.
    pub end: u64,
}

impl MultibootModule {
    /// Length of the module in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the module is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// From the leading bytes of a Multiboot Information structure, return the
/// module table location `(count, addr)` if the loader supplied modules.
///
/// The info structure begins with `u32 flags`; bit 3 indicates the
/// `mods_count` (offset 20) and `mods_addr` (offset 24) fields are valid.
///
/// # Errors
///
/// [`FirmwareError::MalformedImage`] if the header is too short to read.
pub fn module_table(info_header: &[u8]) -> Result<Option<(u32, u32)>, FirmwareError> {
    if info_header.len() < 28 {
        return Err(FirmwareError::MalformedImage {
            detail: "multiboot info header truncated",
        });
    }
    let mut r = ByteReader::new(info_header);
    let flags = r.get_u32().map_err(|_| FirmwareError::MalformedImage {
        detail: "multiboot flags truncated",
    })?;
    if flags & MB_FLAG_MODS == 0 {
        return Ok(None);
    }
    // Skip to offset 20 (mods_count): we've read 4 bytes (flags); skip 16.
    let _ = r.get_slice(16);
    let mods_count = r.get_u32().map_err(|_| FirmwareError::MalformedImage {
        detail: "mods_count truncated",
    })?;
    let mods_addr = r.get_u32().map_err(|_| FirmwareError::MalformedImage {
        detail: "mods_addr truncated",
    })?;
    if mods_count == 0 {
        return Ok(None);
    }
    Ok(Some((mods_count, mods_addr)))
}

/// Parse one multiboot module table entry (`u32 mod_start`, `u32 mod_end`, then
/// a string pointer and reserved word we ignore).
///
/// # Errors
///
/// [`FirmwareError::MalformedImage`] if the entry is truncated or describes an
/// empty/inverted range.
pub fn parse_module_entry(entry: &[u8]) -> Result<MultibootModule, FirmwareError> {
    if entry.len() < 8 {
        return Err(FirmwareError::MalformedImage {
            detail: "multiboot module entry truncated",
        });
    }
    let mut r = ByteReader::new(entry);
    let start = r.get_u32().map_err(|_| FirmwareError::MalformedImage {
        detail: "mod_start truncated",
    })? as u64;
    let end = r.get_u32().map_err(|_| FirmwareError::MalformedImage {
        detail: "mod_end truncated",
    })? as u64;
    if end <= start {
        return Err(FirmwareError::MalformedImage {
            detail: "multiboot module range empty or inverted",
        });
    }
    Ok(MultibootModule { start, end })
}

/// Facts extracted from a multiboot memory map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MultibootMemInfo {
    /// Base physical address of the first available region above 1 MiB.
    pub memory_base: u64,
    /// Total available RAM in bytes (sum of all type-1 regions).
    pub total_memory: u64,
}

/// Parse multiboot memory-map bytes (the region the info structure's
/// `mmap_addr`/`mmap_length` point to).
///
/// # Errors
///
/// Returns [`FirmwareError::MalformedImage`] if an entry is truncated or its
/// self-described size is implausible.
pub fn parse_memory_map(mmap: &[u8]) -> Result<MultibootMemInfo, FirmwareError> {
    let mut info = MultibootMemInfo::default();
    let mut r = ByteReader::new(mmap);
    let mut base_set = false;

    while r.remaining() >= 4 {
        let entry_size = r.get_u32().map_err(|_| FirmwareError::MalformedImage {
            detail: "multiboot mmap entry size truncated",
        })? as usize;
        // The size field excludes itself; the body must be at least 20 bytes
        // (u64 base + u64 length + u32 type).
        if entry_size < 20 {
            return Err(FirmwareError::MalformedImage {
                detail: "multiboot mmap entry too small",
            });
        }
        if r.remaining() < entry_size {
            return Err(FirmwareError::MalformedImage {
                detail: "multiboot mmap entry truncated",
            });
        }
        let base = r.get_u64().map_err(|_| FirmwareError::MalformedImage {
            detail: "mmap base truncated",
        })?;
        let length = r.get_u64().map_err(|_| FirmwareError::MalformedImage {
            detail: "mmap length truncated",
        })?;
        let kind = r.get_u32().map_err(|_| FirmwareError::MalformedImage {
            detail: "mmap type truncated",
        })?;

        // Skip any extra bytes beyond the 20-byte body this parser understands.
        let consumed = 20usize;
        if entry_size > consumed {
            let _ = r.get_slice(entry_size - consumed);
        }

        if kind == MMAP_TYPE_AVAILABLE {
            info.total_memory = info.total_memory.saturating_add(length);
            // Record the base of the first available region at or above 1 MiB,
            // skipping the low-memory hole below it.
            if !base_set && base >= 0x10_0000 {
                info.memory_base = base;
                base_set = true;
            }
        }
    }

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::utils::serialization::ByteWriter;
    use std::vec::Vec;

    fn entry(base: u64, length: u64, kind: u32) -> Vec<u8> {
        let mut buf = [0u8; 24];
        let n = {
            let mut w = ByteWriter::new(&mut buf);
            w.put_u32(20).unwrap(); // size excludes itself
            w.put_u64(base).unwrap();
            w.put_u64(length).unwrap();
            w.put_u32(kind).unwrap();
            w.position()
        };
        buf[..n].to_vec()
    }

    #[test]
    fn sums_available_regions() {
        let mut mmap = Vec::new();
        // Low memory below 1 MiB (available) — counted in total, not base.
        mmap.extend_from_slice(&entry(0x0, 0x9_FC00, 1));
        // Reserved region — ignored.
        mmap.extend_from_slice(&entry(0xF_0000, 0x1_0000, 2));
        // Main RAM above 1 MiB.
        mmap.extend_from_slice(&entry(0x10_0000, 0x7FF0_0000, 1));

        let info = parse_memory_map(&mmap).expect("parse");
        assert_eq!(info.memory_base, 0x10_0000);
        assert_eq!(info.total_memory, 0x9_FC00 + 0x7FF0_0000);
    }

    #[test]
    fn rejects_tiny_entry() {
        let mut buf = [0u8; 8];
        let n = {
            let mut w = ByteWriter::new(&mut buf);
            w.put_u32(4).unwrap(); // implausibly small
            w.put_u32(0).unwrap();
            w.position()
        };
        assert!(parse_memory_map(&buf[..n]).is_err());
    }

    #[test]
    fn module_table_found_when_flag_set() {
        let mut buf = [0u8; 28];
        {
            let mut w = ByteWriter::new(&mut buf);
            w.put_u32(MB_FLAG_MODS).unwrap(); // flags: modules present
            for _ in 0..4 {
                w.put_u32(0).unwrap(); // skip mem/boot device/cmdline (offsets 4..20)
            }
            w.put_u32(1).unwrap(); // mods_count @20
            w.put_u32(0x9000).unwrap(); // mods_addr @24
        }
        assert_eq!(module_table(&buf).unwrap(), Some((1, 0x9000)));
    }

    #[test]
    fn module_table_absent_when_flag_clear() {
        let mut buf = [0u8; 28];
        {
            let mut w = ByteWriter::new(&mut buf);
            w.put_u32(0).unwrap(); // no flags
        }
        assert_eq!(module_table(&buf).unwrap(), None);
    }

    #[test]
    fn parses_module_entry_range() {
        let mut buf = [0u8; 16];
        {
            let mut w = ByteWriter::new(&mut buf);
            w.put_u32(0x10_0000).unwrap(); // mod_start
            w.put_u32(0x14_0000).unwrap(); // mod_end
            w.put_u32(0).unwrap(); // string
            w.put_u32(0).unwrap(); // reserved
        }
        let m = parse_module_entry(&buf).unwrap();
        assert_eq!(m.start, 0x10_0000);
        assert_eq!(m.end, 0x14_0000);
        assert_eq!(m.len(), 0x4_0000);
    }

    #[test]
    fn rejects_inverted_module_range() {
        let mut buf = [0u8; 8];
        {
            let mut w = ByteWriter::new(&mut buf);
            w.put_u32(0x20_0000).unwrap();
            w.put_u32(0x10_0000).unwrap(); // end < start
        }
        assert!(parse_module_entry(&buf).is_err());
    }
}
