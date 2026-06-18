//! ARP (Address Resolution Protocol) — resolve IPv4 -> MAC, with a small cache.

use crate::{Ipv4Addr, MacAddr, NetError};

/// ARP packet length for IPv4-over-Ethernet (htype/ptype/hlen/plen/oper + 2x
/// (HW+proto) addresses = 28 bytes).
pub const ARP_LEN: usize = 28;

/// ARP operation codes.
pub mod oper {
    pub const REQUEST: u16 = 1;
    pub const REPLY: u16 = 2;
}

/// A parsed ARP packet (IPv4-over-Ethernet).
#[derive(Clone, Copy, Debug)]
pub struct ArpPacket {
    pub oper: u16,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

impl ArpPacket {
    /// Parse an ARP packet from `buf`.
    ///
    /// # Errors
    /// [`NetError::Truncated`] if too short; [`NetError::Malformed`] if it is not
    /// IPv4-over-Ethernet (htype 1, ptype 0x0800, hlen 6, plen 4).
    pub fn parse(buf: &[u8]) -> Result<Self, NetError> {
        if buf.len() < ARP_LEN {
            return Err(NetError::Truncated);
        }
        let htype = u16::from_be_bytes([buf[0], buf[1]]);
        let ptype = u16::from_be_bytes([buf[2], buf[3]]);
        let hlen = buf[4];
        let plen = buf[5];
        if htype != 1 || ptype != 0x0800 || hlen != 6 || plen != 4 {
            return Err(NetError::Malformed);
        }
        let oper = u16::from_be_bytes([buf[6], buf[7]]);
        let mut sender_mac = [0u8; 6];
        let mut target_mac = [0u8; 6];
        sender_mac.copy_from_slice(&buf[8..14]);
        let sender_ip = Ipv4Addr([buf[14], buf[15], buf[16], buf[17]]);
        target_mac.copy_from_slice(&buf[18..24]);
        let target_ip = Ipv4Addr([buf[24], buf[25], buf[26], buf[27]]);
        Ok(ArpPacket {
            oper,
            sender_mac,
            sender_ip,
            target_mac,
            target_ip,
        })
    }

    /// Serialize this ARP packet into `out` (28 bytes).
    ///
    /// # Errors
    /// [`NetError::BufferTooSmall`] if `out` is shorter than 28 bytes.
    pub fn build(&self, out: &mut [u8]) -> Result<usize, NetError> {
        if out.len() < ARP_LEN {
            return Err(NetError::BufferTooSmall);
        }
        out[0..2].copy_from_slice(&1u16.to_be_bytes()); // htype Ethernet
        out[2..4].copy_from_slice(&0x0800u16.to_be_bytes()); // ptype IPv4
        out[4] = 6; // hlen
        out[5] = 4; // plen
        out[6..8].copy_from_slice(&self.oper.to_be_bytes());
        out[8..14].copy_from_slice(&self.sender_mac);
        out[14..18].copy_from_slice(&self.sender_ip.0);
        out[18..24].copy_from_slice(&self.target_mac);
        out[24..28].copy_from_slice(&self.target_ip.0);
        Ok(ARP_LEN)
    }
}

/// Build an ARP REQUEST asking "who has `target_ip`?" from `(sender_mac,
/// sender_ip)`.
#[must_use]
pub fn request(sender_mac: MacAddr, sender_ip: Ipv4Addr, target_ip: Ipv4Addr) -> ArpPacket {
    ArpPacket {
        oper: oper::REQUEST,
        sender_mac,
        sender_ip,
        target_mac: [0u8; 6],
        target_ip,
    }
}

/// Build an ARP REPLY answering `request` from `(our_mac, our_ip)`.
#[must_use]
pub fn reply(request: &ArpPacket, our_mac: MacAddr, our_ip: Ipv4Addr) -> ArpPacket {
    ArpPacket {
        oper: oper::REPLY,
        sender_mac: our_mac,
        sender_ip: our_ip,
        target_mac: request.sender_mac,
        target_ip: request.sender_ip,
    }
}

/// A tiny fixed-capacity ARP cache (IPv4 -> MAC). No allocation; LRU-ish by
/// simple round-robin replacement. Capacity is small by design — a host talks to
/// few peers (gateway, a handful of local addresses).
pub struct ArpCache {
    entries: [Option<(Ipv4Addr, MacAddr)>; Self::CAP],
    next: usize,
}

impl ArpCache {
    const CAP: usize = 16;

    /// A new, empty cache.
    #[must_use]
    pub const fn new() -> Self {
        ArpCache {
            entries: [None; Self::CAP],
            next: 0,
        }
    }

    /// Insert or update `ip -> mac`.
    pub fn insert(&mut self, ip: Ipv4Addr, mac: MacAddr) {
        // Update in place if present.
        for e in self.entries.iter_mut().flatten() {
            if e.0 == ip {
                e.1 = mac;
                return;
            }
        }
        // Otherwise place at the round-robin slot.
        self.entries[self.next] = Some((ip, mac));
        self.next = (self.next + 1) % Self::CAP;
    }

    /// Look up the MAC for `ip`, if cached.
    #[must_use]
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<MacAddr> {
        self.entries
            .iter()
            .flatten()
            .find(|e| e.0 == ip)
            .map(|e| e.1)
    }
}

impl Default for ArpCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_build_parse_roundtrips() {
        let smac = [0x52, 0x54, 0x00, 0xab, 0xcd, 0xef];
        let sip = Ipv4Addr::new(10, 0, 2, 15);
        let tip = Ipv4Addr::new(10, 0, 2, 2);
        let req = request(smac, sip, tip);
        let mut buf = [0u8; 28];
        req.build(&mut buf).unwrap();
        let parsed = ArpPacket::parse(&buf).unwrap();
        assert_eq!(parsed.oper, oper::REQUEST);
        assert_eq!(parsed.sender_ip, sip);
        assert_eq!(parsed.target_ip, tip);
        assert_eq!(parsed.sender_mac, smac);
    }

    #[test]
    fn reply_targets_the_requester() {
        let req = request([1, 2, 3, 4, 5, 6], Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(2, 2, 2, 2));
        let our_mac = [9, 9, 9, 9, 9, 9];
        let rep = reply(&req, our_mac, Ipv4Addr::new(2, 2, 2, 2));
        assert_eq!(rep.oper, oper::REPLY);
        assert_eq!(rep.sender_mac, our_mac);
        assert_eq!(rep.target_mac, req.sender_mac);
        assert_eq!(rep.target_ip, req.sender_ip);
    }

    #[test]
    fn cache_insert_and_lookup() {
        let mut c = ArpCache::new();
        let ip = Ipv4Addr::new(10, 0, 2, 2);
        let mac = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
        assert_eq!(c.lookup(ip), None);
        c.insert(ip, mac);
        assert_eq!(c.lookup(ip), Some(mac));
        // Update in place.
        let mac2 = [1, 1, 1, 1, 1, 1];
        c.insert(ip, mac2);
        assert_eq!(c.lookup(ip), Some(mac2));
    }
}
