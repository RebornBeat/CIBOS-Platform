//! UDP datagram parse/build, including the IPv4 pseudo-header checksum.

use crate::{checksum_accumulate, checksum_fold, ip_proto, Ipv4Addr, NetError};

/// The fixed UDP header length (src port 2 + dst port 2 + length 2 + csum 2).
pub const UDP_HEADER_LEN: usize = 8;

/// A parsed UDP datagram view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpDatagram<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: &'a [u8],
}

impl<'a> UdpDatagram<'a> {
    /// Parse a UDP datagram from `buf` (the L4 payload of an IPv4 packet).
    /// `src`/`dst` are the IPv4 addresses, needed to validate the checksum when
    /// present (a zero checksum field means "not computed" and is accepted).
    ///
    /// # Errors
    /// [`NetError::Truncated`] if too short; [`NetError::Malformed`] if the
    /// length field is inconsistent; [`NetError::BadChecksum`] if a present
    /// checksum fails.
    pub fn parse(buf: &'a [u8], src: Ipv4Addr, dst: Ipv4Addr) -> Result<Self, NetError> {
        if buf.len() < UDP_HEADER_LEN {
            return Err(NetError::Truncated);
        }
        let src_port = u16::from_be_bytes([buf[0], buf[1]]);
        let dst_port = u16::from_be_bytes([buf[2], buf[3]]);
        let length = u16::from_be_bytes([buf[4], buf[5]]) as usize;
        if length < UDP_HEADER_LEN || length > buf.len() {
            return Err(NetError::Malformed);
        }
        let csum = u16::from_be_bytes([buf[6], buf[7]]);
        if csum != 0 {
            // Validate over the pseudo-header + UDP header + payload.
            let sum = pseudo_header_sum(src, dst, length as u16);
            if checksum_fold(checksum_accumulate(sum, &buf[..length])) != 0 {
                return Err(NetError::BadChecksum);
            }
        }
        Ok(UdpDatagram {
            src_port,
            dst_port,
            payload: &buf[UDP_HEADER_LEN..length],
        })
    }
}

/// The IPv4 pseudo-header partial sum (src, dst, protocol, UDP length) for the
/// UDP checksum.
fn pseudo_header_sum(src: Ipv4Addr, dst: Ipv4Addr, udp_len: u16) -> u32 {
    let mut ph = [0u8; 12];
    ph[0..4].copy_from_slice(&src.0);
    ph[4..8].copy_from_slice(&dst.0);
    ph[8] = 0;
    ph[9] = ip_proto::UDP;
    ph[10..12].copy_from_slice(&udp_len.to_be_bytes());
    checksum_accumulate(0, &ph)
}

/// Build a UDP datagram (header + payload) into `out`, computing the checksum
/// over the IPv4 pseudo-header. Returns the datagram length.
///
/// # Errors
/// [`NetError::BufferTooSmall`] if `out` cannot hold header + payload.
pub fn build(
    out: &mut [u8],
    src: Ipv4Addr,
    dst: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Result<usize, NetError> {
    let total = UDP_HEADER_LEN + payload.len();
    if out.len() < total || total > u16::MAX as usize {
        return Err(NetError::BufferTooSmall);
    }
    out[0..2].copy_from_slice(&src_port.to_be_bytes());
    out[2..4].copy_from_slice(&dst_port.to_be_bytes());
    out[4..6].copy_from_slice(&(total as u16).to_be_bytes());
    out[6..8].copy_from_slice(&[0, 0]); // checksum placeholder
    out[UDP_HEADER_LEN..total].copy_from_slice(payload);
    // Checksum: pseudo-header + UDP header + payload.
    let sum = pseudo_header_sum(src, dst, total as u16);
    let mut csum = checksum_fold(checksum_accumulate(sum, &out[..total]));
    // A computed checksum of 0 is transmitted as 0xFFFF (0 means "none").
    if csum == 0 {
        csum = 0xFFFF;
    }
    out[6..8].copy_from_slice(&csum.to_be_bytes());
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_then_parse_roundtrips_with_checksum() {
        let src = Ipv4Addr::new(10, 0, 2, 15);
        let dst = Ipv4Addr::new(10, 0, 2, 2);
        let payload = b"ping";
        let mut buf = [0u8; 64];
        let n = build(&mut buf, src, dst, 5000, 53, payload).unwrap();
        let d = UdpDatagram::parse(&buf[..n], src, dst).unwrap();
        assert_eq!(d.src_port, 5000);
        assert_eq!(d.dst_port, 53);
        assert_eq!(d.payload, payload);
    }

    #[test]
    fn corrupted_payload_fails_checksum() {
        let src = Ipv4Addr::new(10, 0, 2, 15);
        let dst = Ipv4Addr::new(10, 0, 2, 2);
        let mut buf = [0u8; 64];
        let n = build(&mut buf, src, dst, 1, 2, b"data").unwrap();
        buf[UDP_HEADER_LEN] ^= 0xFF; // corrupt payload
        assert_eq!(
            UdpDatagram::parse(&buf[..n], src, dst),
            Err(NetError::BadChecksum)
        );
    }
}
