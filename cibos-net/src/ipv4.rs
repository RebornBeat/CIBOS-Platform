//! IPv4 header parse/build. Fragmentation is deferred honestly (single-fragment
//! datagrams only); options are not emitted (IHL fixed at 5).

use crate::{checksum, Ipv4Addr, NetError};

/// The minimum IPv4 header length (no options), 20 bytes.
pub const IPV4_HEADER_LEN: usize = 20;

/// A parsed IPv4 datagram view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Packet<'a> {
    pub protocol: u8,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub ttl: u8,
    /// The L4 payload following the header.
    pub payload: &'a [u8],
}

impl<'a> Ipv4Packet<'a> {
    /// Parse an IPv4 datagram from `buf`.
    ///
    /// # Errors
    /// [`NetError::Truncated`] if too short; [`NetError::Malformed`] if version
    /// != 4 or IHL/total-length are inconsistent; [`NetError::BadChecksum`] if
    /// the header checksum fails.
    pub fn parse(buf: &'a [u8]) -> Result<Self, NetError> {
        if buf.len() < IPV4_HEADER_LEN {
            return Err(NetError::Truncated);
        }
        let version = buf[0] >> 4;
        let ihl = (buf[0] & 0x0F) as usize * 4;
        if version != 4 || ihl < IPV4_HEADER_LEN || buf.len() < ihl {
            return Err(NetError::Malformed);
        }
        let total_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        if total_len < ihl || total_len > buf.len() {
            return Err(NetError::Malformed);
        }
        // Validate the header checksum (over the IHL bytes).
        if checksum(&buf[..ihl]) != 0 {
            return Err(NetError::BadChecksum);
        }
        let ttl = buf[8];
        let protocol = buf[9];
        let src = Ipv4Addr([buf[12], buf[13], buf[14], buf[15]]);
        let dst = Ipv4Addr([buf[16], buf[17], buf[18], buf[19]]);
        Ok(Ipv4Packet {
            protocol,
            src,
            dst,
            ttl,
            payload: &buf[ihl..total_len],
        })
    }
}

/// Build an IPv4 header (no options, IHL=5) for `payload` into `out`, returning
/// the total datagram length (header + payload). `identification` is the IP ID;
/// `ttl` the time-to-live; the Don't-Fragment flag is set and fragmentation is
/// not performed (single-datagram only).
///
/// # Errors
/// [`NetError::BufferTooSmall`] if `out` cannot hold header + payload.
pub fn build(
    out: &mut [u8],
    src: Ipv4Addr,
    dst: Ipv4Addr,
    protocol: u8,
    ttl: u8,
    identification: u16,
    payload: &[u8],
) -> Result<usize, NetError> {
    let total = IPV4_HEADER_LEN + payload.len();
    if out.len() < total || total > u16::MAX as usize {
        return Err(NetError::BufferTooSmall);
    }
    out[0] = 0x45; // version 4, IHL 5
    out[1] = 0; // DSCP/ECN
    out[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    out[4..6].copy_from_slice(&identification.to_be_bytes());
    out[6..8].copy_from_slice(&0x4000u16.to_be_bytes()); // DF set, frag offset 0
    out[8] = ttl;
    out[9] = protocol;
    out[10..12].copy_from_slice(&[0, 0]); // checksum placeholder
    out[12..16].copy_from_slice(&src.0);
    out[16..20].copy_from_slice(&dst.0);
    let csum = checksum(&out[..IPV4_HEADER_LEN]);
    out[10..12].copy_from_slice(&csum.to_be_bytes());
    out[IPV4_HEADER_LEN..total].copy_from_slice(payload);
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ip_proto;

    #[test]
    fn build_then_parse_roundtrips() {
        let src = Ipv4Addr::new(10, 0, 2, 15);
        let dst = Ipv4Addr::new(10, 0, 2, 2);
        let payload = [0xde, 0xad, 0xbe, 0xef];
        let mut buf = [0u8; 64];
        let n = build(&mut buf, src, dst, ip_proto::UDP, 64, 0x1234, &payload).unwrap();
        let p = Ipv4Packet::parse(&buf[..n]).unwrap();
        assert_eq!(p.src, src);
        assert_eq!(p.dst, dst);
        assert_eq!(p.protocol, ip_proto::UDP);
        assert_eq!(p.ttl, 64);
        assert_eq!(p.payload, &payload);
    }

    #[test]
    fn bad_checksum_detected() {
        let mut buf = [0u8; 24];
        build(
            &mut buf,
            Ipv4Addr::new(1, 2, 3, 4),
            Ipv4Addr::new(5, 6, 7, 8),
            ip_proto::ICMP,
            64,
            1,
            &[0, 0, 0, 0],
        )
        .unwrap();
        buf[12] ^= 0xFF; // corrupt the source address after checksum was set
        assert_eq!(Ipv4Packet::parse(&buf), Err(NetError::BadChecksum));
    }

    #[test]
    fn rejects_non_v4() {
        let mut buf = [0u8; 20];
        buf[0] = 0x65; // version 6
        assert_eq!(Ipv4Packet::parse(&buf), Err(NetError::Malformed));
    }
}
