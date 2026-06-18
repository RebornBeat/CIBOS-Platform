//! ICMP echo (ping) request/reply parse/build, for verifiability.

use crate::{checksum, NetError};

/// ICMP message types we handle.
pub mod icmp_type {
    pub const ECHO_REPLY: u8 = 0;
    pub const ECHO_REQUEST: u8 = 8;
}

/// The fixed ICMP echo header length (type 1 + code 1 + csum 2 + id 2 + seq 2).
pub const ICMP_ECHO_HEADER_LEN: usize = 8;

/// A parsed ICMP echo message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcmpEcho<'a> {
    pub is_reply: bool,
    pub id: u16,
    pub seq: u16,
    pub payload: &'a [u8],
}

impl<'a> IcmpEcho<'a> {
    /// Parse an ICMP echo request/reply from `buf` (the IPv4 payload).
    ///
    /// # Errors
    /// [`NetError::Truncated`] if too short; [`NetError::Malformed`] if the type
    /// is not an echo request/reply; [`NetError::BadChecksum`] if the checksum
    /// fails.
    pub fn parse(buf: &'a [u8]) -> Result<Self, NetError> {
        if buf.len() < ICMP_ECHO_HEADER_LEN {
            return Err(NetError::Truncated);
        }
        let ty = buf[0];
        let is_reply = match ty {
            icmp_type::ECHO_REPLY => true,
            icmp_type::ECHO_REQUEST => false,
            _ => return Err(NetError::Malformed),
        };
        if checksum(buf) != 0 {
            return Err(NetError::BadChecksum);
        }
        let id = u16::from_be_bytes([buf[4], buf[5]]);
        let seq = u16::from_be_bytes([buf[6], buf[7]]);
        Ok(IcmpEcho {
            is_reply,
            id,
            seq,
            payload: &buf[ICMP_ECHO_HEADER_LEN..],
        })
    }
}

/// Build an ICMP echo REQUEST into `out`, returning its length.
///
/// # Errors
/// [`NetError::BufferTooSmall`] if `out` cannot hold the message.
pub fn echo_request(
    out: &mut [u8],
    id: u16,
    seq: u16,
    payload: &[u8],
) -> Result<usize, NetError> {
    build(out, icmp_type::ECHO_REQUEST, id, seq, payload)
}

/// Build an ICMP echo REPLY into `out`, returning its length.
///
/// # Errors
/// [`NetError::BufferTooSmall`] if `out` cannot hold the message.
pub fn echo_reply(out: &mut [u8], id: u16, seq: u16, payload: &[u8]) -> Result<usize, NetError> {
    build(out, icmp_type::ECHO_REPLY, id, seq, payload)
}

fn build(out: &mut [u8], ty: u8, id: u16, seq: u16, payload: &[u8]) -> Result<usize, NetError> {
    let total = ICMP_ECHO_HEADER_LEN + payload.len();
    if out.len() < total {
        return Err(NetError::BufferTooSmall);
    }
    out[0] = ty;
    out[1] = 0; // code
    out[2..4].copy_from_slice(&[0, 0]); // checksum placeholder
    out[4..6].copy_from_slice(&id.to_be_bytes());
    out[6..8].copy_from_slice(&seq.to_be_bytes());
    out[ICMP_ECHO_HEADER_LEN..total].copy_from_slice(payload);
    let csum = checksum(&out[..total]);
    out[2..4].copy_from_slice(&csum.to_be_bytes());
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_build_parse_roundtrips() {
        let mut buf = [0u8; 32];
        let n = echo_request(&mut buf, 0xABCD, 1, b"hello").unwrap();
        let e = IcmpEcho::parse(&buf[..n]).unwrap();
        assert!(!e.is_reply);
        assert_eq!(e.id, 0xABCD);
        assert_eq!(e.seq, 1);
        assert_eq!(e.payload, b"hello");
    }

    #[test]
    fn reply_is_marked_reply() {
        let mut buf = [0u8; 32];
        let n = echo_reply(&mut buf, 1, 2, b"x").unwrap();
        let e = IcmpEcho::parse(&buf[..n]).unwrap();
        assert!(e.is_reply);
    }
}
