//! CIBOS network stack — from-scratch, `no_std`, no external crates.
//!
//! This is OS logic, written from scratch like the rest of CIBOS (scheduler,
//! IPC, gates, filesystem). It deliberately does NOT pull an external TCP/IP
//! crate: the workspace uses outside crates only for cryptography and a few
//! foundational primitives, never for OS logic. The scope here is the layers the
//! Lattice's NIC-backed transport needs first:
//!
//!   * Ethernet II framing (parse/build)
//!   * ARP (resolve IPv4 -> MAC, with a small cache)
//!   * IPv4 (header parse/build + checksum; fragmentation deferred honestly)
//!   * UDP (datagram parse/build)
//!   * ICMP echo (ping) for verifiability
//!
//! TCP is a separate, larger milestone (a real state machine) and is NOT here.
//!
//! The stack is driven by a frame transport the caller provides (the kernel's
//! `NetDevice`); this crate is pure protocol logic over byte slices, so it is
//! arch-independent and unit-testable on the host.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

pub mod arp;
pub mod ethernet;
pub mod icmp;
pub mod ipv4;
pub mod udp;

/// A 48-bit Ethernet MAC address.
pub type MacAddr = [u8; 6];

/// The broadcast MAC (ff:ff:ff:ff:ff:ff).
pub const MAC_BROADCAST: MacAddr = [0xFF; 6];

/// A 32-bit IPv4 address in network byte order (big-endian octets).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    /// Construct from four octets.
    #[must_use]
    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Ipv4Addr([a, b, c, d])
    }
    /// The all-zeros address (0.0.0.0).
    pub const UNSPECIFIED: Ipv4Addr = Ipv4Addr([0, 0, 0, 0]);
    /// The limited broadcast address (255.255.255.255).
    pub const BROADCAST: Ipv4Addr = Ipv4Addr([255, 255, 255, 255]);
    /// The raw octets.
    #[must_use]
    pub const fn octets(self) -> [u8; 4] {
        self.0
    }
}

/// EtherType values we handle.
pub mod ethertype {
    pub const IPV4: u16 = 0x0800;
    pub const ARP: u16 = 0x0806;
}

/// IPv4 protocol numbers we handle.
pub mod ip_proto {
    pub const ICMP: u8 = 1;
    pub const UDP: u8 = 17;
    pub const TCP: u8 = 6; // recognized; not yet implemented
}

/// Errors from parsing/building protocol data units.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetError {
    /// The input slice was shorter than the protocol requires.
    Truncated,
    /// A header field was structurally invalid (e.g. bad IHL, version).
    Malformed,
    /// The output buffer was too small to build the PDU.
    BufferTooSmall,
    /// A checksum did not validate.
    BadChecksum,
    /// The protocol/ethertype is recognized but not implemented here (e.g. TCP).
    Unsupported,
}

/// The standard internet 16-bit one's-complement checksum over `data`, folding
/// carries. Used by IPv4, ICMP, and (with a pseudo-header) UDP.
#[must_use]
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u32::from(u16::from_be_bytes([data[i], data[i + 1]]));
        i += 2;
    }
    if i < data.len() {
        // Odd trailing byte: pad with zero on the right.
        sum += u32::from(u16::from_be_bytes([data[i], 0]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Continue an in-progress checksum accumulation (for pseudo-headers). Returns
/// the running 32-bit sum; finalize with [`checksum_fold`].
#[must_use]
pub fn checksum_accumulate(mut sum: u32, data: &[u8]) -> u32 {
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u32::from(u16::from_be_bytes([data[i], data[i + 1]]));
        i += 2;
    }
    if i < data.len() {
        sum += u32::from(u16::from_be_bytes([data[i], 0]));
    }
    sum
}

/// Fold a running checksum sum to the final 16-bit one's complement.
#[must_use]
pub fn checksum_fold(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_known_vector() {
        // A classic worked example: the checksum of these bytes is 0xb1e6.
        let data = [
            0x45u8, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(checksum(&data), 0xb861);
    }

    #[test]
    fn checksum_of_valid_header_is_zero() {
        // Build a header with a correct checksum, then checksum the whole thing
        // (including the checksum field): the result must be 0.
        let mut data = [
            0x45u8, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        let c = checksum(&data);
        data[10] = (c >> 8) as u8;
        data[11] = (c & 0xFF) as u8;
        assert_eq!(checksum(&data), 0);
    }

    #[test]
    fn ipv4_addr_octets() {
        let ip = Ipv4Addr::new(10, 0, 2, 15);
        assert_eq!(ip.octets(), [10, 0, 2, 15]);
    }
}
