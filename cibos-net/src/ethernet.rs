//! Ethernet II frame parse/build.

use crate::{MacAddr, NetError};

/// The fixed Ethernet II header length (dst 6 + src 6 + ethertype 2).
pub const ETH_HEADER_LEN: usize = 14;

/// A parsed view over an Ethernet II frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EthFrame<'a> {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
    /// The payload following the 14-byte header.
    pub payload: &'a [u8],
}

impl<'a> EthFrame<'a> {
    /// Parse an Ethernet II frame from `buf`.
    ///
    /// # Errors
    /// [`NetError::Truncated`] if `buf` is shorter than the 14-byte header.
    pub fn parse(buf: &'a [u8]) -> Result<Self, NetError> {
        if buf.len() < ETH_HEADER_LEN {
            return Err(NetError::Truncated);
        }
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&buf[0..6]);
        src.copy_from_slice(&buf[6..12]);
        let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
        Ok(EthFrame {
            dst,
            src,
            ethertype,
            payload: &buf[ETH_HEADER_LEN..],
        })
    }
}

/// Build an Ethernet II frame header + payload into `out`, returning the total
/// frame length written.
///
/// # Errors
/// [`NetError::BufferTooSmall`] if `out` cannot hold the header + payload.
pub fn build(
    out: &mut [u8],
    dst: MacAddr,
    src: MacAddr,
    ethertype: u16,
    payload: &[u8],
) -> Result<usize, NetError> {
    let total = ETH_HEADER_LEN + payload.len();
    if out.len() < total {
        return Err(NetError::BufferTooSmall);
    }
    out[0..6].copy_from_slice(&dst);
    out[6..12].copy_from_slice(&src);
    out[12..14].copy_from_slice(&ethertype.to_be_bytes());
    out[ETH_HEADER_LEN..total].copy_from_slice(payload);
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ethertype;

    #[test]
    fn build_then_parse_roundtrips() {
        let dst = [0xff; 6];
        let src = [0x52, 0x54, 0x00, 0xab, 0xcd, 0xef];
        let payload = [1u8, 2, 3, 4];
        let mut buf = [0u8; 64];
        let n = build(&mut buf, dst, src, ethertype::ARP, &payload).unwrap();
        assert_eq!(n, ETH_HEADER_LEN + 4);
        let f = EthFrame::parse(&buf[..n]).unwrap();
        assert_eq!(f.dst, dst);
        assert_eq!(f.src, src);
        assert_eq!(f.ethertype, ethertype::ARP);
        assert_eq!(f.payload, &payload);
    }

    #[test]
    fn parse_truncated_fails() {
        assert_eq!(EthFrame::parse(&[0u8; 10]), Err(NetError::Truncated));
    }
}
