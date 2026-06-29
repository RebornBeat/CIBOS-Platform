//! CIBOS flattened-device-tree (DTB/FDT) parser — from scratch, `no_std`, no
//! external crates.
//!
//! Both QEMU and real boards' firmware pass a Flattened Device Tree describing
//! the platform: where RAM is, where the UART/interrupt-controller live, how many
//! CPUs, etc. Parsing it at runtime is what lets the kernel work on QEMU AND real
//! hardware WITHOUT compiled-in platform constants — the bare-metal-first
//! guarantee. This parser extracts exactly what the kernel bring-up needs (RAM
//! base+size, the primary UART base) and is deliberately small; it is not a
//! general-purpose FDT library.
//!
//! FDT binary layout (all multi-byte fields big-endian):
//!   * Header: magic 0xd00dfeed, total size, offset to the struct block, offset
//!     to the strings block, ... (see [`FDT_MAGIC`]).
//!   * Struct block: a token stream — BEGIN_NODE(name), PROP(len, nameoff, data),
//!     END_NODE, NOP, END.
//!   * Strings block: null-terminated property names referenced by PROP.nameoff.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

/// The FDT header magic (big-endian 0xd00dfeed).
pub const FDT_MAGIC: u32 = 0xd00d_feed;

// Struct-block tokens.
const FDT_BEGIN_NODE: u32 = 0x0000_0001;
const FDT_END_NODE: u32 = 0x0000_0002;
const FDT_PROP: u32 = 0x0000_0003;
const FDT_NOP: u32 = 0x0000_0004;
const FDT_END: u32 = 0x0000_0009;

/// Errors from parsing a device tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DtbError {
    /// The blob did not start with the FDT magic.
    BadMagic,
    /// The blob ended before a structure we needed was complete.
    Truncated,
    /// A required node or property was not found.
    NotFound,
}

/// A read-only view over a flattened device tree in memory.
pub struct DeviceTree<'a> {
    blob: &'a [u8],
    struct_off: usize,
    struct_size: usize,
    strings_off: usize,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    let hi = be32(b, at)? as u64;
    let lo = be32(b, at + 4)? as u64;
    Some((hi << 32) | lo)
}

impl<'a> DeviceTree<'a> {
    /// Parse the FDT header of `blob`.
    ///
    /// # Errors
    /// [`DtbError::BadMagic`] if the magic is wrong; [`DtbError::Truncated`] if
    /// the header is incomplete.
    pub fn new(blob: &'a [u8]) -> Result<Self, DtbError> {
        if be32(blob, 0).ok_or(DtbError::Truncated)? != FDT_MAGIC {
            return Err(DtbError::BadMagic);
        }
        let struct_off = be32(blob, 8).ok_or(DtbError::Truncated)? as usize;
        let strings_off = be32(blob, 12).ok_or(DtbError::Truncated)? as usize;
        let struct_size = be32(blob, 36).ok_or(DtbError::Truncated)? as usize;
        Ok(DeviceTree {
            blob,
            struct_off,
            struct_size,
            strings_off,
        })
    }

    /// Construct from a raw pointer (the value firmware passes in a register).
    ///
    /// The total size is read from the header so the slice is bounded. Returns
    /// `None` on a bad magic/size. This is the one place a raw firmware pointer
    /// is turned into a safe slice; callers pass the register value directly.
    ///
    /// # Safety
    /// `ptr` must point at a readable FDT blob the firmware placed and left
    /// mapped. Expressed as a normal fn taking `usize` to keep the crate
    /// `forbid(unsafe_code)`; the caller's `unsafe` is the slice construction it
    /// does before calling [`new`](Self::new).
    pub fn totalsize_at(blob_header: &[u8]) -> Option<usize> {
        if be32(blob_header, 0)? != FDT_MAGIC {
            return None;
        }
        Some(be32(blob_header, 4)? as usize)
    }

    /// Read a NUL-terminated property name from the strings block at `nameoff`.
    fn prop_name(&self, nameoff: usize) -> &'a [u8] {
        let start = self.strings_off + nameoff;
        let mut end = start;
        while end < self.blob.len() && self.blob[end] != 0 {
            end += 1;
        }
        &self.blob[start..end]
    }

    /// Find the first node whose name begins with `prefix` (e.g. `b"memory@"`)
    /// and return the raw bytes of its `reg` property, if present. Used to read
    /// the RAM region and device MMIO bases.
    ///
    /// `reg` is a list of (address, size) pairs; the cell sizes come from the
    /// node's parent `#address-cells`/`#size-cells`. For the platforms we target
    /// (QEMU virt, typical boards) these are 2/2 (64-bit), which we assume here
    /// and document as a limitation.
    fn node_reg(&self, prefix: &[u8]) -> Option<&'a [u8]> {
        let mut pos = self.struct_off;
        let end = self.struct_off + self.struct_size;
        let mut in_target = false;
        while pos < end {
            let token = be32(self.blob, pos)?;
            pos += 4;
            match token {
                FDT_BEGIN_NODE => {
                    // Node name follows, NUL-terminated, padded to 4 bytes.
                    let name_start = pos;
                    let mut e = name_start;
                    while e < self.blob.len() && self.blob[e] != 0 {
                        e += 1;
                    }
                    let name = &self.blob[name_start..e];
                    in_target = name.starts_with(prefix);
                    pos = (e + 1 + 3) & !3; // skip name + NUL, align to 4
                }
                FDT_PROP => {
                    let len = be32(self.blob, pos)? as usize;
                    let nameoff = be32(self.blob, pos + 4)? as usize;
                    let data_start = pos + 8;
                    pos = (data_start + len + 3) & !3;
                    if in_target && self.prop_name(nameoff) == b"reg" {
                        return self.blob.get(data_start..data_start + len);
                    }
                }
                FDT_END_NODE => {
                    in_target = false;
                }
                FDT_NOP => {}
                FDT_END => break,
                _ => return None,
            }
        }
        None
    }

    /// The primary RAM region as `(base, size)`, read from the `/memory@*` node's
    /// `reg` (first address/size pair, 64-bit cells).
    ///
    /// # Errors
    /// [`DtbError::NotFound`] if no memory node/reg is present.
    pub fn ram_region(&self) -> Result<(u64, u64), DtbError> {
        let reg = self.node_reg(b"memory").ok_or(DtbError::NotFound)?;
        let base = be64(reg, 0).ok_or(DtbError::Truncated)?;
        let size = be64(reg, 8).ok_or(DtbError::Truncated)?;
        Ok((base, size))
    }

    /// The base address of a device node whose name starts with `prefix` (e.g.
    /// `b"pl011"`, `b"serial"`, `b"uart"`), from its `reg` first address cell.
    ///
    /// # Errors
    /// [`DtbError::NotFound`] if no such node/reg is present.
    pub fn device_base(&self, prefix: &[u8]) -> Result<u64, DtbError> {
        let reg = self.node_reg(prefix).ok_or(DtbError::NotFound)?;
        be64(reg, 0).ok_or(DtbError::Truncated)
    }

    /// The `(base, size)` of the first device node whose name starts with
    /// `prefix`, read from its `reg` property (assuming 2 address cells + 2 size
    /// cells, the standard for the 64-bit `virt` platforms). Used to register a
    /// discovered device's MMIO window for Device mapping, so the window comes
    /// from the platform's DTB rather than a hardcoded board constant.
    pub fn device_reg(&self, prefix: &[u8]) -> Result<(u64, u64), DtbError> {
        let reg = self.node_reg(prefix).ok_or(DtbError::NotFound)?;
        let base = be64(reg, 0).ok_or(DtbError::Truncated)?;
        // size may be absent on some nodes; default to a single page if so.
        let size = be64(reg, 8).unwrap_or(0x1000);
        Ok((base, size))
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::vec::Vec;

    /// Build a minimal valid FDT with one /memory@X node carrying a reg of
    /// (base, size), to exercise the parser without a real blob.
    fn synth_fdt(base: u64, size: u64) -> Vec<u8> {
        // Layout: header (40 bytes) + struct block + strings block.
        let mut strings = Vec::new();
        let reg_name_off = strings.len() as u32;
        strings.extend_from_slice(b"reg\0");

        let mut st = Vec::new();
        // root node
        st.extend_from_slice(&FDT_BEGIN_NODE.to_be_bytes());
        st.extend_from_slice(b"\0\0\0\0"); // empty root name, padded
                                           // memory@... node
        st.extend_from_slice(&FDT_BEGIN_NODE.to_be_bytes());
        let name = b"memory@0\0";
        st.extend_from_slice(name);
        while st.len() % 4 != 0 {
            st.push(0);
        }
        // reg prop
        st.extend_from_slice(&FDT_PROP.to_be_bytes());
        st.extend_from_slice(&16u32.to_be_bytes()); // len = 2x u64
        st.extend_from_slice(&reg_name_off.to_be_bytes());
        st.extend_from_slice(&base.to_be_bytes());
        st.extend_from_slice(&size.to_be_bytes());
        st.extend_from_slice(&FDT_END_NODE.to_be_bytes()); // end memory
        st.extend_from_slice(&FDT_END_NODE.to_be_bytes()); // end root
        st.extend_from_slice(&FDT_END.to_be_bytes());

        let header_len = 40usize;
        let struct_off = header_len;
        let strings_off = header_len + st.len();
        let total = strings_off + strings.len();

        let mut blob = Vec::new();
        blob.extend_from_slice(&FDT_MAGIC.to_be_bytes()); // 0
        blob.extend_from_slice(&(total as u32).to_be_bytes()); // 4 totalsize
        blob.extend_from_slice(&(struct_off as u32).to_be_bytes()); // 8
        blob.extend_from_slice(&(strings_off as u32).to_be_bytes()); // 12
        blob.extend_from_slice(&0u32.to_be_bytes()); // 16 mem_rsvmap
        blob.extend_from_slice(&17u32.to_be_bytes()); // 20 version
        blob.extend_from_slice(&16u32.to_be_bytes()); // 24 last_comp
        blob.extend_from_slice(&0u32.to_be_bytes()); // 28 boot_cpuid
        blob.extend_from_slice(&(strings.len() as u32).to_be_bytes()); // 32 size_strings
        blob.extend_from_slice(&(st.len() as u32).to_be_bytes()); // 36 size_struct
        blob.extend_from_slice(&st);
        blob.extend_from_slice(&strings);
        blob
    }

    #[test]
    fn parses_ram_region() {
        let blob = synth_fdt(0x4000_0000, 0x0800_0000);
        let dt = DeviceTree::new(&blob).unwrap();
        assert_eq!(dt.ram_region().unwrap(), (0x4000_0000, 0x0800_0000));
    }

    #[test]
    fn riscv_layout() {
        let blob = synth_fdt(0x8000_0000, 0x0800_0000);
        let dt = DeviceTree::new(&blob).unwrap();
        assert_eq!(dt.ram_region().unwrap(), (0x8000_0000, 0x0800_0000));
    }

    #[test]
    fn bad_magic_rejected() {
        let bad = [0u8; 40];
        assert_eq!(DeviceTree::new(&bad).err(), Some(DtbError::BadMagic));
    }
}
