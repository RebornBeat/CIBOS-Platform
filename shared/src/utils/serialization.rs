//! # Serialization
//!
//! A minimal, allocation-free, bounds-checked byte cursor for encoding and
//! decoding wire structures that are not already fixed `#[repr(C)]` layouts —
//! signed configuration blobs, package manifests, and the like.
//!
//! All multi-byte integers are little-endian, chosen because every supported
//! architecture (x86_64, aarch64, riscv64, x86) runs little-endian in the
//! configurations the system targets, so encoding matches in-memory layout and
//! avoids per-field byte swapping.
//!
//! Every read and write is bounds-checked and returns
//! [`SerializationError`] rather than panicking, so these helpers are safe to
//! point at untrusted input (a downloaded package, a configuration file of
//! unknown provenance).

use crate::types::error::SerializationError;

/// A forward-only writer over a caller-provided byte buffer.
///
/// Tracks how many bytes have been written; never grows the buffer. A write
/// that would overflow the buffer fails with
/// [`SerializationError::BufferTooSmall`] and leaves the cursor unchanged.
pub struct ByteWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> ByteWriter<'a> {
    /// Create a writer over `buf`, starting at offset zero.
    #[must_use]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Number of bytes written so far.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Remaining free capacity in bytes.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn ensure(&self, need: usize) -> Result<(), SerializationError> {
        if self.remaining() < need {
            Err(SerializationError::BufferTooSmall {
                required: need,
                available: self.remaining(),
            })
        } else {
            Ok(())
        }
    }

    /// Write a single byte.
    ///
    /// # Errors
    /// [`SerializationError::BufferTooSmall`] if no space remains.
    pub fn put_u8(&mut self, value: u8) -> Result<(), SerializationError> {
        self.ensure(1)?;
        self.buf[self.pos] = value;
        self.pos += 1;
        Ok(())
    }

    /// Write a little-endian `u16`.
    ///
    /// # Errors
    /// [`SerializationError::BufferTooSmall`] if insufficient space remains.
    pub fn put_u16(&mut self, value: u16) -> Result<(), SerializationError> {
        self.put_bytes(&value.to_le_bytes())
    }

    /// Write a little-endian `u32`.
    ///
    /// # Errors
    /// [`SerializationError::BufferTooSmall`] if insufficient space remains.
    pub fn put_u32(&mut self, value: u32) -> Result<(), SerializationError> {
        self.put_bytes(&value.to_le_bytes())
    }

    /// Write a little-endian `u64`.
    ///
    /// # Errors
    /// [`SerializationError::BufferTooSmall`] if insufficient space remains.
    pub fn put_u64(&mut self, value: u64) -> Result<(), SerializationError> {
        self.put_bytes(&value.to_le_bytes())
    }

    /// Write a raw byte slice verbatim.
    ///
    /// # Errors
    /// [`SerializationError::BufferTooSmall`] if insufficient space remains.
    pub fn put_bytes(&mut self, bytes: &[u8]) -> Result<(), SerializationError> {
        self.ensure(bytes.len())?;
        self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
        Ok(())
    }

    /// Write a length-prefixed byte slice: a little-endian `u32` length followed
    /// by the bytes. The companion to [`ByteReader::get_length_prefixed`].
    ///
    /// # Errors
    /// [`SerializationError::BufferTooSmall`] if insufficient space remains.
    pub fn put_length_prefixed(&mut self, bytes: &[u8]) -> Result<(), SerializationError> {
        let len = u32::try_from(bytes.len()).map_err(|_| SerializationError::InvalidValue {
            field: "length prefix",
        })?;
        self.put_u32(len)?;
        self.put_bytes(bytes)
    }

    /// The bytes written so far.
    #[must_use]
    pub fn written(&self) -> &[u8] {
        &self.buf[..self.pos]
    }
}

/// A forward-only reader over a byte buffer.
///
/// Every read advances the cursor and is bounds-checked against the end of the
/// buffer, returning [`SerializationError::UnexpectedEnd`] on underrun.
pub struct ByteReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    /// Create a reader over `buf`, starting at offset zero.
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Number of bytes consumed so far.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Number of bytes still available to read.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn ensure(&self, need: usize) -> Result<(), SerializationError> {
        if self.remaining() < need {
            Err(SerializationError::UnexpectedEnd)
        } else {
            Ok(())
        }
    }

    /// Read a single byte.
    ///
    /// # Errors
    /// [`SerializationError::UnexpectedEnd`] if no bytes remain.
    pub fn get_u8(&mut self) -> Result<u8, SerializationError> {
        self.ensure(1)?;
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    /// Read a little-endian `u16`.
    ///
    /// # Errors
    /// [`SerializationError::UnexpectedEnd`] if insufficient bytes remain.
    pub fn get_u16(&mut self) -> Result<u16, SerializationError> {
        let mut b = [0u8; 2];
        self.get_into(&mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    /// Read a little-endian `u32`.
    ///
    /// # Errors
    /// [`SerializationError::UnexpectedEnd`] if insufficient bytes remain.
    pub fn get_u32(&mut self) -> Result<u32, SerializationError> {
        let mut b = [0u8; 4];
        self.get_into(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Read a little-endian `u64`.
    ///
    /// # Errors
    /// [`SerializationError::UnexpectedEnd`] if insufficient bytes remain.
    pub fn get_u64(&mut self) -> Result<u64, SerializationError> {
        let mut b = [0u8; 8];
        self.get_into(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Read exactly `out.len()` bytes into `out`.
    ///
    /// # Errors
    /// [`SerializationError::UnexpectedEnd`] if insufficient bytes remain.
    pub fn get_into(&mut self, out: &mut [u8]) -> Result<(), SerializationError> {
        self.ensure(out.len())?;
        out.copy_from_slice(&self.buf[self.pos..self.pos + out.len()]);
        self.pos += out.len();
        Ok(())
    }

    /// Borrow the next `len` bytes without copying, advancing the cursor.
    ///
    /// # Errors
    /// [`SerializationError::UnexpectedEnd`] if fewer than `len` bytes remain.
    pub fn get_slice(&mut self, len: usize) -> Result<&'a [u8], SerializationError> {
        self.ensure(len)?;
        let s = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(s)
    }

    /// Read a length-prefixed byte slice written by
    /// [`ByteWriter::put_length_prefixed`].
    ///
    /// # Errors
    /// [`SerializationError::UnexpectedEnd`] if the prefix or the body is
    /// truncated.
    pub fn get_length_prefixed(&mut self) -> Result<&'a [u8], SerializationError> {
        let len = self.get_u32()? as usize;
        self.get_slice(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrip() {
        let mut buf = [0u8; 64];
        let mut w = ByteWriter::new(&mut buf);
        w.put_u8(0xAB).unwrap();
        w.put_u16(0x1234).unwrap();
        w.put_u32(0xDEAD_BEEF).unwrap();
        w.put_u64(0x0102_0304_0506_0708).unwrap();
        w.put_length_prefixed(b"hello").unwrap();
        let n = w.position();

        let mut r = ByteReader::new(&buf[..n]);
        assert_eq!(r.get_u8().unwrap(), 0xAB);
        assert_eq!(r.get_u16().unwrap(), 0x1234);
        assert_eq!(r.get_u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.get_u64().unwrap(), 0x0102_0304_0506_0708);
        assert_eq!(r.get_length_prefixed().unwrap(), b"hello");
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn write_overflow_is_error_not_panic() {
        let mut buf = [0u8; 2];
        let mut w = ByteWriter::new(&mut buf);
        assert!(w.put_u32(1).is_err());
        // Cursor unchanged after failed write.
        assert_eq!(w.position(), 0);
    }

    #[test]
    fn read_underrun_is_error_not_panic() {
        let buf = [0u8; 2];
        let mut r = ByteReader::new(&buf);
        assert!(r.get_u32().is_err());
    }

    #[test]
    fn truncated_length_prefix_rejected() {
        // Claims 100 bytes follow, but only 3 are present.
        let mut buf = [0u8; 7];
        {
            let mut w = ByteWriter::new(&mut buf);
            w.put_u32(100).unwrap();
            w.put_bytes(&[1, 2, 3]).unwrap();
        }
        let mut r = ByteReader::new(&buf);
        assert!(r.get_length_prefixed().is_err());
    }
}
