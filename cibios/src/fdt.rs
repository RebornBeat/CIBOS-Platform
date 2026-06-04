//! # Flattened Device Tree Parser (Minimal)
//!
//! Just enough Flattened Device Tree (FDT / `.dtb`) parsing for firmware
//! hardware detection on platforms that describe themselves with a device tree
//! (the AArch64 and RISC-V `virt` machines, and most real ARM/RISC-V boards):
//! total usable memory and CPU count.
//!
//! This is pure, `unsafe`-free byte parsing over a borrowed slice, so it lives
//! in the firmware logic library and is unit-tested on the host. The
//! architecture code passes the DTB bytes (located via the pointer the platform
//! hands the kernel/firmware in a register) and receives a [`DeviceTreeInfo`].
//!
//! The format follows the Devicetree Specification: a big-endian header, a
//! structure block of tokens, and a strings block. Only the subset needed for
//! detection is interpreted; unknown properties are skipped, not rejected.

use crate::error::FirmwareError;

/// FDT magic, big-endian `0xd00dfeed`.
const FDT_MAGIC: u32 = 0xd00d_feed;

// Structure block tokens.
const FDT_BEGIN_NODE: u32 = 0x0000_0001;
const FDT_END_NODE: u32 = 0x0000_0002;
const FDT_PROP: u32 = 0x0000_0003;
const FDT_NOP: u32 = 0x0000_0004;
const FDT_END: u32 = 0x0000_0009;

/// Facts extracted from a device tree relevant to firmware detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceTreeInfo {
    /// Base physical address of the first `/memory` node.
    pub memory_base: u64,
    /// Total usable memory in bytes, summed across all `/memory` nodes.
    pub total_memory: u64,
    /// Number of CPU nodes found under `/cpus`.
    pub cpu_count: u32,
    /// `/chosen/linux,initrd-start`, if present (else 0).
    pub initrd_start: u64,
    /// `/chosen/linux,initrd-end`, if present (else 0).
    pub initrd_end: u64,
}

impl DeviceTreeInfo {
    /// The initrd byte range `(start, end)` if the device tree advertised one.
    ///
    /// This is how the AArch64 and RISC-V `virt` machines tell the firmware
    /// where a `-initrd` (the CIBOS image) was loaded.
    #[must_use]
    pub fn initrd(&self) -> Option<(u64, u64)> {
        if self.initrd_end > self.initrd_start {
            Some((self.initrd_start, self.initrd_end))
        } else {
            None
        }
    }
}

/// Read a big-endian `u32` at `offset`, bounds-checked.
fn be_u32(bytes: &[u8], offset: usize) -> Result<u32, FirmwareError> {
    let end = offset
        .checked_add(4)
        .ok_or(FirmwareError::MalformedImage {
            detail: "fdt offset overflow",
        })?;
    if end > bytes.len() {
        return Err(FirmwareError::MalformedImage {
            detail: "fdt truncated",
        });
    }
    Ok(u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

/// Parse a device tree blob, extracting memory size and CPU count.
///
/// # Errors
///
/// Returns [`FirmwareError::MalformedImage`] if the blob is truncated, has a bad
/// magic, or is structurally inconsistent.
pub fn parse(dtb: &[u8]) -> Result<DeviceTreeInfo, FirmwareError> {
    if be_u32(dtb, 0)? != FDT_MAGIC {
        return Err(FirmwareError::MalformedImage {
            detail: "bad fdt magic",
        });
    }
    let off_struct = be_u32(dtb, 8)? as usize;
    let off_strings = be_u32(dtb, 12)? as usize;
    let size_struct = be_u32(dtb, 36)? as usize;

    let struct_end = off_struct
        .checked_add(size_struct)
        .ok_or(FirmwareError::MalformedImage {
            detail: "fdt struct size overflow",
        })?;
    if struct_end > dtb.len() || off_strings > dtb.len() {
        return Err(FirmwareError::MalformedImage {
            detail: "fdt block out of range",
        });
    }

    let mut info = DeviceTreeInfo::default();

    // Root #address-cells / #size-cells (defaults per spec: 2 and 1, but the
    // common root value is 2/2; we read them from the root node when present).
    let mut root_address_cells: u32 = 2;
    let mut root_size_cells: u32 = 1;

    let mut cursor = off_struct;
    let mut depth: i32 = 0;
    // Name of the node we're currently inside, at each depth, simplified to the
    // current node only (enough for memory/cpu detection).
    let mut current_is_memory = false;
    let mut current_is_chosen = false;
    let mut in_cpus = false;
    let mut cpus_depth: i32 = -1;

    loop {
        if cursor + 4 > struct_end {
            return Err(FirmwareError::MalformedImage {
                detail: "fdt struct overran",
            });
        }
        let token = be_u32(dtb, cursor)?;
        cursor += 4;

        match token {
            FDT_BEGIN_NODE => {
                depth += 1;
                // Node name: null-terminated string, then pad to 4 bytes.
                let name_start = cursor;
                let mut i = name_start;
                while i < struct_end && dtb[i] != 0 {
                    i += 1;
                }
                if i >= struct_end {
                    return Err(FirmwareError::MalformedImage {
                        detail: "fdt node name unterminated",
                    });
                }
                let name = &dtb[name_start..i];
                cursor = align4(i + 1);

                current_is_memory = name_starts_with(name, b"memory");
                current_is_chosen = name_eq(name, b"chosen");
                if name_eq(name, b"cpus") {
                    in_cpus = true;
                    cpus_depth = depth;
                } else if in_cpus && depth == cpus_depth + 1 && name_starts_with(name, b"cpu") {
                    // A CPU node directly under /cpus.
                    info.cpu_count = info.cpu_count.saturating_add(1);
                }
            }
            FDT_END_NODE => {
                if in_cpus && depth == cpus_depth {
                    in_cpus = false;
                    cpus_depth = -1;
                }
                depth -= 1;
                current_is_memory = false;
                current_is_chosen = false;
                if depth < 0 {
                    return Err(FirmwareError::MalformedImage {
                        detail: "fdt unbalanced node end",
                    });
                }
            }
            FDT_PROP => {
                let len = be_u32(dtb, cursor)? as usize;
                let nameoff = be_u32(dtb, cursor + 4)? as usize;
                cursor += 8;
                let val_start = cursor;
                let val_end =
                    val_start
                        .checked_add(len)
                        .ok_or(FirmwareError::MalformedImage {
                            detail: "fdt prop len overflow",
                        })?;
                if val_end > struct_end {
                    return Err(FirmwareError::MalformedImage {
                        detail: "fdt prop overran",
                    });
                }
                let pname = cstr_at(dtb, off_strings + nameoff)?;

                // Root cells (root node is at depth 1).
                if depth == 1 && pname == b"#address-cells" && len == 4 {
                    root_address_cells = be_u32(dtb, val_start)?;
                } else if depth == 1 && pname == b"#size-cells" && len == 4 {
                    root_size_cells = be_u32(dtb, val_start)?;
                } else if current_is_memory && pname == b"reg" {
                    let (base, size) = parse_reg(
                        dtb,
                        val_start,
                        len,
                        root_address_cells,
                        root_size_cells,
                    )?;
                    if info.total_memory == 0 {
                        info.memory_base = base;
                    }
                    info.total_memory = info.total_memory.saturating_add(size);
                } else if current_is_chosen && pname == b"linux,initrd-start" {
                    info.initrd_start = read_prop_int(dtb, val_start, len)?;
                } else if current_is_chosen && pname == b"linux,initrd-end" {
                    info.initrd_end = read_prop_int(dtb, val_start, len)?;
                }

                cursor = align4(val_end);
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => {
                return Err(FirmwareError::MalformedImage {
                    detail: "fdt unknown token",
                });
            }
        }
    }

    Ok(info)
}

/// Parse a `reg` property, returning `(base_of_first_entry, total_size)`.
fn parse_reg(
    dtb: &[u8],
    start: usize,
    len: usize,
    address_cells: u32,
    size_cells: u32,
) -> Result<(u64, u64), FirmwareError> {
    let entry_cells = address_cells as usize + size_cells as usize;
    if entry_cells == 0 {
        return Ok((0, 0));
    }
    let entry_bytes = entry_cells * 4;
    if entry_bytes == 0 || !len.is_multiple_of(entry_bytes) {
        return Err(FirmwareError::MalformedImage {
            detail: "fdt reg length not a multiple of entry size",
        });
    }
    let mut total: u64 = 0;
    let mut first_base: u64 = 0;
    let mut off = start;
    let entries = len / entry_bytes;
    for i in 0..entries {
        let base = read_cells(dtb, off, address_cells)?;
        off += address_cells as usize * 4;
        let size = read_cells(dtb, off, size_cells)?;
        off += size_cells as usize * 4;
        if i == 0 {
            first_base = base;
        }
        total = total.saturating_add(size);
    }
    Ok((first_base, total))
}

/// Read `cells` big-endian u32 words as a single u64 (cells is 1 or 2).
fn read_cells(dtb: &[u8], offset: usize, cells: u32) -> Result<u64, FirmwareError> {
    let mut value: u64 = 0;
    for i in 0..cells as usize {
        let word = be_u32(dtb, offset + i * 4)? as u64;
        value = (value << 32) | word;
    }
    Ok(value)
}

/// Read an integer device-tree property that may be encoded as one cell (u32)
/// or two cells (u64), as the `linux,initrd-*` properties are.
fn read_prop_int(dtb: &[u8], start: usize, len: usize) -> Result<u64, FirmwareError> {
    match len {
        4 => Ok(u64::from(be_u32(dtb, start)?)),
        8 => read_cells(dtb, start, 2),
        _ => Err(FirmwareError::MalformedImage {
            detail: "unexpected initrd property length",
        }),
    }
}

/// Read a null-terminated string from the strings block.
fn cstr_at(bytes: &[u8], offset: usize) -> Result<&[u8], FirmwareError> {
    if offset >= bytes.len() {
        return Err(FirmwareError::MalformedImage {
            detail: "fdt string offset out of range",
        });
    }
    let mut i = offset;
    while i < bytes.len() && bytes[i] != 0 {
        i += 1;
    }
    Ok(&bytes[offset..i])
}

const fn align4(n: usize) -> usize {
    (n + 3) & !3
}

fn name_starts_with(name: &[u8], prefix: &[u8]) -> bool {
    name.len() >= prefix.len() && &name[..prefix.len()] == prefix
}

fn name_eq(name: &[u8], other: &[u8]) -> bool {
    name == other
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    // Build a minimal but valid FDT with a root (#address-cells=2,
    // #size-cells=2), a /memory node with reg = <0x0 0x40000000> (1 GiB), and a
    // /cpus node with two cpu@N children.
    fn build_dtb() -> Vec<u8> {
        // Strings block: collect property names with their offsets.
        let mut strings: Vec<u8> = Vec::new();
        let mut add_str = |s: &str, strings: &mut Vec<u8>| -> u32 {
            let off = strings.len() as u32;
            strings.extend_from_slice(s.as_bytes());
            strings.push(0);
            off
        };
        let off_ac = add_str("#address-cells", &mut strings);
        let off_sc = add_str("#size-cells", &mut strings);
        let off_reg = add_str("reg", &mut strings);
        let off_devtype = add_str("device_type", &mut strings);
        let off_initrd_start = add_str("linux,initrd-start", &mut strings);
        let off_initrd_end = add_str("linux,initrd-end", &mut strings);

        // Structure block.
        let mut s: Vec<u8> = Vec::new();
        let push_u32 = |v: u32, s: &mut Vec<u8>| s.extend_from_slice(&v.to_be_bytes());
        let push_node_name = |name: &str, s: &mut Vec<u8>| {
            s.extend_from_slice(name.as_bytes());
            s.push(0);
            while s.len() % 4 != 0 {
                s.push(0);
            }
        };
        let push_prop =
            |nameoff: u32, val: &[u8], s: &mut Vec<u8>| {
                s.extend_from_slice(&FDT_PROP.to_be_bytes());
                s.extend_from_slice(&(val.len() as u32).to_be_bytes());
                s.extend_from_slice(&nameoff.to_be_bytes());
                s.extend_from_slice(val);
                while s.len() % 4 != 0 {
                    s.push(0);
                }
            };

        // root
        push_u32(FDT_BEGIN_NODE, &mut s);
        push_node_name("", &mut s);
        push_prop(off_ac, &2u32.to_be_bytes(), &mut s);
        push_prop(off_sc, &2u32.to_be_bytes(), &mut s);

        // /memory@0 with reg=<0 0x40000000>
        push_u32(FDT_BEGIN_NODE, &mut s);
        push_node_name("memory@0", &mut s);
        push_prop(off_devtype, b"memory\0", &mut s);
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x4000_0000u64.to_be_bytes()); // address (2 cells)
        reg.extend_from_slice(&0x4000_0000u64.to_be_bytes()); // size (2 cells) = 1 GiB
        push_prop(off_reg, &reg, &mut s);
        push_u32(FDT_END_NODE, &mut s);

        // /cpus { cpu@0; cpu@1; }
        push_u32(FDT_BEGIN_NODE, &mut s);
        push_node_name("cpus", &mut s);
        push_u32(FDT_BEGIN_NODE, &mut s);
        push_node_name("cpu@0", &mut s);
        push_u32(FDT_END_NODE, &mut s);
        push_u32(FDT_BEGIN_NODE, &mut s);
        push_node_name("cpu@1", &mut s);
        push_u32(FDT_END_NODE, &mut s);
        push_u32(FDT_END_NODE, &mut s); // end /cpus

        // /chosen { linux,initrd-start = <...>; linux,initrd-end = <...>; }
        push_u32(FDT_BEGIN_NODE, &mut s);
        push_node_name("chosen", &mut s);
        push_prop(off_initrd_start, &0x4800_0000u64.to_be_bytes(), &mut s);
        push_prop(off_initrd_end, &0x4806_3000u64.to_be_bytes(), &mut s);
        push_u32(FDT_END_NODE, &mut s); // end /chosen

        push_u32(FDT_END_NODE, &mut s); // end root
        push_u32(FDT_END, &mut s);

        // Assemble: header (40 bytes) + struct + strings.
        let header_len = 40usize;
        let off_struct = header_len;
        let off_strings = header_len + s.len();
        let total = off_strings + strings.len();

        let mut dtb = Vec::new();
        let mut ph = |v: u32, dtb: &mut Vec<u8>| dtb.extend_from_slice(&v.to_be_bytes());
        ph(FDT_MAGIC, &mut dtb);
        ph(total as u32, &mut dtb);
        ph(off_struct as u32, &mut dtb);
        ph(off_strings as u32, &mut dtb);
        ph(0, &mut dtb); // off_mem_rsvmap
        ph(17, &mut dtb); // version
        ph(16, &mut dtb); // last_comp_version
        ph(0, &mut dtb); // boot_cpuid_phys
        ph(strings.len() as u32, &mut dtb); // size_dt_strings
        ph(s.len() as u32, &mut dtb); // size_dt_struct
        dtb.extend_from_slice(&s);
        dtb.extend_from_slice(&strings);
        dtb
    }

    #[test]
    fn parses_memory_and_cpus() {
        let dtb = build_dtb();
        let info = parse(&dtb).expect("parse");
        assert_eq!(info.memory_base, 0x4000_0000);
        assert_eq!(info.total_memory, 0x4000_0000); // 1 GiB
        assert_eq!(info.cpu_count, 2);
    }

    #[test]
    fn parses_initrd_from_chosen() {
        let dtb = build_dtb();
        let info = parse(&dtb).expect("parse");
        assert_eq!(info.initrd_start, 0x4800_0000);
        assert_eq!(info.initrd_end, 0x4806_3000);
        assert_eq!(info.initrd(), Some((0x4800_0000, 0x4806_3000)));
    }

    #[test]
    fn no_initrd_yields_none() {
        // A device tree without /chosen initrd properties reports no initrd.
        let info = DeviceTreeInfo {
            memory_base: 0,
            total_memory: 0x1000,
            cpu_count: 1,
            initrd_start: 0,
            initrd_end: 0,
        };
        assert_eq!(info.initrd(), None);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut dtb = build_dtb();
        dtb[0] = 0;
        assert!(parse(&dtb).is_err());
    }

    #[test]
    fn rejects_truncated() {
        let dtb = build_dtb();
        assert!(parse(&dtb[..20]).is_err());
    }
}
