//! # CIBOS Application Image Format (`.capp`)
//!
//! The on-disk / in-memory layout of a loadable user application, and a safe,
//! range-checked parser for it. This is the format the kernel's application
//! loader consumes to run an unprivileged program: it describes where each
//! segment of the program must be mapped in the application's virtual address
//! space, with what permissions, and which bytes back it.
//!
//! It is deliberately distinct from the CIBOS *kernel* image (`cibios::image`,
//! magic `"CIMG"`): that format carries the kernel and is parsed/verified by
//! firmware; this one carries a user application and is parsed by the booted
//! kernel's loader. Keeping them separate keeps each parser simple and its
//! trust boundary clear.
//!
//! ## Layout
//!
//! ```text
//! +-----------------------------+  offset 0
//! | AppImageHeader (32 bytes)   |
//! +-----------------------------+  offset 32
//! | AppSegment[0]   (32 bytes)  |
//! | AppSegment[1]   (32 bytes)  |
//! | ...                         |
//! | AppSegment[N-1] (32 bytes)  |
//! +-----------------------------+  offset 32 + N*32
//! | segment 0 bytes             |
//! | segment 1 bytes             |
//! | ...                         |
//! +-----------------------------+
//! ```
//!
//! Each segment names a virtual address (`vaddr`), the number of bytes present
//! in the image (`file_size`), the total number of bytes the segment occupies
//! once mapped (`mem_size`, ≥ `file_size`; the tail `mem_size - file_size` is
//! zero-filled, e.g. a `.bss`), and permission flags (read/write/execute). The
//! header records the entry virtual address the loader jumps to.
//!
//! ## Parsing
//!
//! [`AppImage`] borrows a byte slice and exposes typed, range-checked access to
//! the header, segment descriptors, and segment bodies. Like the kernel image
//! parser, it does no `unsafe` and no pointer casts: every field is decoded
//! with [`ByteReader`](crate::utils::serialization::ByteReader) and every offset
//! is validated against the slice length, so a malformed or truncated image
//! yields an [`AppImageError`] rather than undefined behavior.

use crate::types::error::SerializationError;
use crate::utils::serialization::ByteReader;

/// Application image magic: ASCII `"CAPP"` (Cibos APP), little-endian.
pub const APP_MAGIC: u32 = u32::from_le_bytes(*b"CAPP");

/// Application image format version understood by this loader.
pub const APP_VERSION: u32 = 1;

/// Encoded size of [`AppImageHeader`] in bytes.
pub const APP_HEADER_LEN: usize = 32;

/// Encoded size of one [`AppSegment`] descriptor in bytes.
pub const APP_SEGMENT_LEN: usize = 32;

/// Upper bound on the segment count, to bound parsing work on untrusted input.
pub const MAX_SEGMENTS: u32 = 16;

/// Segment permission flag: readable.
pub const SEG_FLAG_READ: u32 = 1 << 0;
/// Segment permission flag: writable.
pub const SEG_FLAG_WRITE: u32 = 1 << 1;
/// Segment permission flag: executable.
pub const SEG_FLAG_EXEC: u32 = 1 << 2;

/// Errors from parsing an application image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppImageError {
    /// The image is shorter than required for the structure being read.
    Truncated,
    /// The magic number did not match [`APP_MAGIC`].
    BadMagic,
    /// The version did not match [`APP_VERSION`].
    BadVersion,
    /// The segment count exceeded [`MAX_SEGMENTS`].
    TooManySegments,
    /// A segment's declared byte range lies outside the image.
    SegmentOutOfBounds,
    /// A segment's `file_size` exceeded its `mem_size`.
    SegmentSizeInconsistent,
}

impl From<SerializationError> for AppImageError {
    fn from(_: SerializationError) -> Self {
        AppImageError::Truncated
    }
}

impl core::fmt::Display for AppImageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            AppImageError::Truncated => "application image truncated",
            AppImageError::BadMagic => "bad application image magic",
            AppImageError::BadVersion => "unsupported application image version",
            AppImageError::TooManySegments => "too many segments",
            AppImageError::SegmentOutOfBounds => "segment range outside image",
            AppImageError::SegmentSizeInconsistent => "segment file_size exceeds mem_size",
        };
        f.write_str(s)
    }
}

/// The application image header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppImageHeader {
    /// Must equal [`APP_MAGIC`].
    pub magic: u32,
    /// Must equal [`APP_VERSION`].
    pub version: u32,
    /// Number of [`AppSegment`] descriptors following the header.
    pub segment_count: u32,
    /// Reserved; always zero. Keeps the entry `u64` 8-byte aligned.
    pub _reserved: u32,
    /// Virtual address of the program entry point (the loader jumps here).
    pub entry: u64,
    /// Total image length in bytes (header + descriptors + bodies). A
    /// self-describing length lets the loader sanity-check the buffer it was
    /// handed without a separate size channel.
    pub image_len: u64,
}

impl AppImageHeader {
    /// Parse and validate the header at the start of `bytes`.
    ///
    /// # Errors
    ///
    /// [`AppImageError`] on truncation, bad magic, or bad version.
    pub fn parse(bytes: &[u8]) -> Result<Self, AppImageError> {
        if bytes.len() < APP_HEADER_LEN {
            return Err(AppImageError::Truncated);
        }
        let mut r = ByteReader::new(&bytes[..APP_HEADER_LEN]);
        let header = AppImageHeader {
            magic: r.get_u32()?,
            version: r.get_u32()?,
            segment_count: r.get_u32()?,
            _reserved: r.get_u32()?,
            entry: r.get_u64()?,
            image_len: r.get_u64()?,
        };
        if header.magic != APP_MAGIC {
            return Err(AppImageError::BadMagic);
        }
        if header.version != APP_VERSION {
            return Err(AppImageError::BadVersion);
        }
        if header.segment_count > MAX_SEGMENTS {
            return Err(AppImageError::TooManySegments);
        }
        Ok(header)
    }
}

/// One loadable segment of an application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSegment {
    /// Virtual address the segment is mapped at (page-aligned by the loader).
    pub vaddr: u64,
    /// Offset of the segment's bytes within the image.
    pub file_offset: u64,
    /// Number of bytes present in the image for this segment.
    pub file_size: u64,
    /// Total bytes the segment occupies once mapped. Bytes beyond `file_size`
    /// are zero-filled (e.g. `.bss`). Must be ≥ `file_size`.
    pub mem_size: u64,
    /// Permission flags ([`SEG_FLAG_READ`] / [`SEG_FLAG_WRITE`] /
    /// [`SEG_FLAG_EXEC`]).
    pub flags: u32,
    /// Reserved; always zero.
    pub _reserved: u32,
}

impl AppSegment {
    /// Parse one descriptor from the start of `bytes`.
    ///
    /// # Errors
    ///
    /// [`AppImageError::Truncated`] if fewer than [`APP_SEGMENT_LEN`] bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, AppImageError> {
        if bytes.len() < APP_SEGMENT_LEN {
            return Err(AppImageError::Truncated);
        }
        let mut r = ByteReader::new(&bytes[..APP_SEGMENT_LEN]);
        // 32-byte descriptor: vaddr(8) file_offset(8) file_size(4) mem_size(4)
        // flags(4) reserved(4). file_size/mem_size are u32 (a segment fits in
        // 4 GiB), which keeps the descriptor exactly APP_SEGMENT_LEN bytes.
        Ok(AppSegment {
            vaddr: r.get_u64()?,
            file_offset: r.get_u64()?,
            file_size: r.get_u32()? as u64,
            mem_size: r.get_u32()? as u64,
            flags: r.get_u32()?,
            _reserved: r.get_u32()?,
        })
    }

    /// Whether the segment should be readable.
    #[must_use]
    pub fn readable(&self) -> bool {
        self.flags & SEG_FLAG_READ != 0
    }
    /// Whether the segment should be writable.
    #[must_use]
    pub fn writable(&self) -> bool {
        self.flags & SEG_FLAG_WRITE != 0
    }
    /// Whether the segment should be executable.
    #[must_use]
    pub fn executable(&self) -> bool {
        self.flags & SEG_FLAG_EXEC != 0
    }
}

/// A parsed, structurally-validated view over an application image.
pub struct AppImage<'a> {
    bytes: &'a [u8],
    header: AppImageHeader,
}

impl<'a> AppImage<'a> {
    /// Parse and structurally validate an application image from `bytes`.
    ///
    /// Validates the magic, version, segment-count bound, that the descriptor
    /// table fits after the header, and that every segment's body range lies
    /// within the image and has `file_size <= mem_size`.
    ///
    /// # Errors
    ///
    /// [`AppImageError`] on any structural problem.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, AppImageError> {
        let header = AppImageHeader::parse(bytes)?;

        // The descriptor table must fit after the header.
        let descriptors_len = (header.segment_count as usize)
            .checked_mul(APP_SEGMENT_LEN)
            .ok_or(AppImageError::Truncated)?;
        let descriptors_end = APP_HEADER_LEN
            .checked_add(descriptors_len)
            .ok_or(AppImageError::Truncated)?;
        if descriptors_end > bytes.len() {
            return Err(AppImageError::Truncated);
        }

        let view = AppImage { bytes, header };

        // Validate every segment's declared range up front so later accessors
        // and the loader can trust them.
        for i in 0..header.segment_count {
            let seg = view.segment(i)?;
            if seg.file_size > seg.mem_size {
                return Err(AppImageError::SegmentSizeInconsistent);
            }
            if seg.file_size > 0 {
                let start = seg.file_offset as usize;
                let end = start
                    .checked_add(seg.file_size as usize)
                    .ok_or(AppImageError::SegmentOutOfBounds)?;
                if end > bytes.len() {
                    return Err(AppImageError::SegmentOutOfBounds);
                }
            }
        }

        Ok(view)
    }

    /// The parsed header.
    #[must_use]
    pub fn header(&self) -> &AppImageHeader {
        &self.header
    }

    /// The entry-point virtual address.
    #[must_use]
    pub fn entry(&self) -> u64 {
        self.header.entry
    }

    /// The number of segments.
    #[must_use]
    pub fn segment_count(&self) -> u32 {
        self.header.segment_count
    }

    /// Decode the `index`-th segment descriptor.
    ///
    /// # Errors
    ///
    /// [`AppImageError`] if `index` is out of range or the descriptor is
    /// truncated.
    pub fn segment(&self, index: u32) -> Result<AppSegment, AppImageError> {
        if index >= self.header.segment_count {
            return Err(AppImageError::SegmentOutOfBounds);
        }
        let start = APP_HEADER_LEN + (index as usize) * APP_SEGMENT_LEN;
        AppSegment::parse(&self.bytes[start..])
    }

    /// Borrow the body bytes of `seg` (length `file_size`), validating the
    /// range against the image. Returns an empty slice for a zero-`file_size`
    /// segment (e.g. a pure `.bss`).
    ///
    /// # Errors
    ///
    /// [`AppImageError::SegmentOutOfBounds`] if the range is out of bounds.
    pub fn segment_body(&self, seg: &AppSegment) -> Result<&'a [u8], AppImageError> {
        if seg.file_size == 0 {
            return Ok(&[]);
        }
        let start = seg.file_offset as usize;
        let end = start
            .checked_add(seg.file_size as usize)
            .ok_or(AppImageError::SegmentOutOfBounds)?;
        if end > self.bytes.len() {
            return Err(AppImageError::SegmentOutOfBounds);
        }
        Ok(&self.bytes[start..end])
    }
}

/// A builder that serializes an application image, used by the host image tool
/// to emit `.capp` files. Gated to `std` builds: the bare firmware/kernel link
/// of `shared` does not pull in `alloc`, and images are *built* by host tooling
/// and only *parsed* (alloc-free, via [`AppImage`]) on the device. Sharing this
/// one writer with the parser keeps the emitted bytes from ever drifting from
/// what [`AppImage`] accepts.
#[cfg(any(feature = "std", test))]
pub struct AppImageBuilder {
    entry: u64,
    /// (vaddr, mem_size, flags, body bytes) for each segment, in order.
    segments: alloc::vec::Vec<(u64, u32, u32, alloc::vec::Vec<u8>)>,
}

#[cfg(any(feature = "std", test))]
impl AppImageBuilder {
    /// Start a builder for a program entered at virtual address `entry`.
    #[must_use]
    pub fn new(entry: u64) -> Self {
        Self {
            entry,
            segments: alloc::vec::Vec::new(),
        }
    }

    /// Append a segment mapped at `vaddr` with `flags`, backed by `body`, and
    /// occupying `mem_size` bytes once mapped (`mem_size >= body.len()`; the
    /// tail is zero-filled). Panics in debug if `mem_size < body.len()`.
    #[must_use]
    pub fn segment(mut self, vaddr: u64, mem_size: u32, flags: u32, body: &[u8]) -> Self {
        debug_assert!(mem_size as usize >= body.len(), "mem_size < file_size");
        self.segments.push((vaddr, mem_size, flags, body.to_vec()));
        self
    }

    /// Serialize to the on-disk `.capp` byte layout (parseable by [`AppImage`]).
    #[must_use]
    pub fn build(self) -> alloc::vec::Vec<u8> {
        use crate::utils::serialization::ByteWriter;

        let n = self.segments.len();
        let header_and_table = APP_HEADER_LEN + n * APP_SEGMENT_LEN;
        let bodies_len: usize = self.segments.iter().map(|(_, _, _, b)| b.len()).sum();
        let total = header_and_table + bodies_len;

        let mut buf = alloc::vec![0u8; total];

        // Header.
        {
            let mut w = ByteWriter::new(&mut buf[..APP_HEADER_LEN]);
            // These writes fit APP_HEADER_LEN exactly; unwrap is safe.
            w.put_u32(APP_MAGIC).unwrap();
            w.put_u32(APP_VERSION).unwrap();
            w.put_u32(n as u32).unwrap();
            w.put_u32(0).unwrap();
            w.put_u64(self.entry).unwrap();
            w.put_u64(total as u64).unwrap();
        }

        // Descriptors and bodies.
        let mut body_cursor = header_and_table;
        for (i, (vaddr, mem_size, flags, body)) in self.segments.iter().enumerate() {
            let dstart = APP_HEADER_LEN + i * APP_SEGMENT_LEN;
            {
                let mut w = ByteWriter::new(&mut buf[dstart..dstart + APP_SEGMENT_LEN]);
                w.put_u64(*vaddr).unwrap();
                w.put_u64(body_cursor as u64).unwrap();
                w.put_u32(body.len() as u32).unwrap();
                w.put_u32(*mem_size).unwrap();
                w.put_u32(*flags).unwrap();
                w.put_u32(0).unwrap();
            }
            buf[body_cursor..body_cursor + body.len()].copy_from_slice(body);
            body_cursor += body.len();
        }

        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::serialization::ByteWriter;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Build a minimal valid image with the given segments (vaddr, bytes, memsz,
    /// flags) and entry. Mirrors what the host `mkappimage` tool will emit.
    fn build(entry: u64, segs: &[(u64, &[u8], u64, u32)]) -> Vec<u8> {
        let header_and_table = APP_HEADER_LEN + segs.len() * APP_SEGMENT_LEN;
        let mut bodies_len = 0usize;
        for (_, body, _, _) in segs {
            bodies_len += body.len();
        }
        let total = header_and_table + bodies_len;
        let mut buf = vec![0u8; total];

        // Header.
        {
            let mut w = ByteWriter::new(&mut buf[..APP_HEADER_LEN]);
            w.put_u32(APP_MAGIC).unwrap();
            w.put_u32(APP_VERSION).unwrap();
            w.put_u32(segs.len() as u32).unwrap();
            w.put_u32(0).unwrap();
            w.put_u64(entry).unwrap();
            w.put_u64(total as u64).unwrap();
        }

        // Descriptors + bodies.
        let mut body_cursor = header_and_table;
        for (i, (vaddr, body, memsz, flags)) in segs.iter().enumerate() {
            let dstart = APP_HEADER_LEN + i * APP_SEGMENT_LEN;
            {
                let mut w = ByteWriter::new(&mut buf[dstart..dstart + APP_SEGMENT_LEN]);
                w.put_u64(*vaddr).unwrap();
                w.put_u64(body_cursor as u64).unwrap();
                w.put_u32(body.len() as u32).unwrap();
                w.put_u32(*memsz as u32).unwrap();
                w.put_u32(*flags).unwrap();
                w.put_u32(0).unwrap(); // reserved
            }
            buf[body_cursor..body_cursor + body.len()].copy_from_slice(body);
            body_cursor += body.len();
        }
        buf
    }

    #[test]
    fn builder_output_parses_back() {
        let code = [0xb8u8, 0x01, 0x00, 0x00, 0x00]; // mov eax,1
        let img = AppImageBuilder::new(0x5000_0000_0000)
            .segment(
                0x5000_0000_0000,
                code.len() as u32,
                SEG_FLAG_READ | SEG_FLAG_EXEC,
                &code,
            )
            .segment(0x5000_0001_0000, 0x4000, SEG_FLAG_READ | SEG_FLAG_WRITE, &[7, 8, 9])
            .build();

        let view = AppImage::parse(&img).unwrap();
        assert_eq!(view.entry(), 0x5000_0000_0000);
        assert_eq!(view.segment_count(), 2);
        let s0 = view.segment(0).unwrap();
        assert!(s0.executable() && s0.readable() && !s0.writable());
        assert_eq!(view.segment_body(&s0).unwrap(), &code);
        let s1 = view.segment(1).unwrap();
        assert!(s1.writable() && !s1.executable());
        assert_eq!(s1.mem_size, 0x4000);
        assert_eq!(view.segment_body(&s1).unwrap(), &[7, 8, 9]);
    }

    #[test]
    fn magic_is_ascii_tag() {
        assert_eq!(&APP_MAGIC.to_le_bytes(), b"CAPP");
    }

    #[test]
    fn round_trips_a_two_segment_image() {
        let code = [0x90u8, 0x90, 0xcc]; // nop; nop; int3 — arbitrary bytes
        let data = [1u8, 2, 3, 4];
        let img = build(
            0x5000_0000_0000,
            &[
                (0x5000_0000_0000, &code, code.len() as u64, SEG_FLAG_READ | SEG_FLAG_EXEC),
                (0x5000_0001_0000, &data, 0x2000, SEG_FLAG_READ | SEG_FLAG_WRITE),
            ],
        );
        let view = AppImage::parse(&img).unwrap();
        assert_eq!(view.entry(), 0x5000_0000_0000);
        assert_eq!(view.segment_count(), 2);

        let s0 = view.segment(0).unwrap();
        assert_eq!(s0.vaddr, 0x5000_0000_0000);
        assert!(s0.readable() && s0.executable() && !s0.writable());
        assert_eq!(view.segment_body(&s0).unwrap(), &code);

        let s1 = view.segment(1).unwrap();
        assert!(s1.readable() && s1.writable() && !s1.executable());
        assert_eq!(s1.file_size, 4);
        assert_eq!(s1.mem_size, 0x2000); // bss tail beyond the 4 file bytes
        assert_eq!(view.segment_body(&s1).unwrap(), &data);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut img = build(0, &[(0, &[0u8], 1, SEG_FLAG_READ)]);
        img[0] ^= 0xff;
        assert_eq!(AppImage::parse(&img).err(), Some(AppImageError::BadMagic));
    }

    #[test]
    fn rejects_truncation() {
        let img = build(0, &[(0, &[1u8, 2, 3], 3, SEG_FLAG_READ)]);
        let truncated = &img[..img.len() - 1];
        assert_eq!(
            AppImage::parse(truncated).err(),
            Some(AppImageError::SegmentOutOfBounds)
        );
        let header_cut = &img[..APP_HEADER_LEN - 1];
        assert_eq!(AppImage::parse(header_cut).err(), Some(AppImageError::Truncated));
    }

    #[test]
    fn rejects_too_many_segments() {
        // Forge a header claiming more than MAX_SEGMENTS.
        let mut buf = vec![0u8; APP_HEADER_LEN];
        let mut w = ByteWriter::new(&mut buf);
        w.put_u32(APP_MAGIC).unwrap();
        w.put_u32(APP_VERSION).unwrap();
        w.put_u32(MAX_SEGMENTS + 1).unwrap();
        w.put_u32(0).unwrap();
        w.put_u64(0).unwrap();
        w.put_u64(APP_HEADER_LEN as u64).unwrap();
        assert_eq!(AppImage::parse(&buf).err(), Some(AppImageError::TooManySegments));
    }

    #[test]
    fn rejects_segment_out_of_bounds() {
        // Hand-build a header + one descriptor whose body runs past the image.
        let mut buf = vec![0u8; APP_HEADER_LEN + APP_SEGMENT_LEN];
        {
            let mut w = ByteWriter::new(&mut buf[..APP_HEADER_LEN]);
            w.put_u32(APP_MAGIC).unwrap();
            w.put_u32(APP_VERSION).unwrap();
            w.put_u32(1).unwrap();
            w.put_u32(0).unwrap();
            w.put_u64(0).unwrap();
            w.put_u64((APP_HEADER_LEN + APP_SEGMENT_LEN) as u64).unwrap();
        }
        {
            let mut w = ByteWriter::new(&mut buf[APP_HEADER_LEN..]);
            w.put_u64(0x4000).unwrap(); // vaddr
            w.put_u64(APP_HEADER_LEN as u64 + APP_SEGMENT_LEN as u64).unwrap(); // file_offset at EOF
            w.put_u32(16).unwrap(); // file_size 16 — runs past the image
            w.put_u32(16).unwrap(); // mem_size
            w.put_u32(SEG_FLAG_READ).unwrap();
            w.put_u32(0).unwrap(); // reserved
        }
        assert_eq!(
            AppImage::parse(&buf).err(),
            Some(AppImageError::SegmentOutOfBounds)
        );
    }

    #[test]
    fn rejects_filesize_gt_memsize() {
        let mut buf = vec![0u8; APP_HEADER_LEN + APP_SEGMENT_LEN + 16];
        let total = buf.len() as u64;
        {
            let mut w = ByteWriter::new(&mut buf[..APP_HEADER_LEN]);
            w.put_u32(APP_MAGIC).unwrap();
            w.put_u32(APP_VERSION).unwrap();
            w.put_u32(1).unwrap();
            w.put_u32(0).unwrap();
            w.put_u64(0).unwrap();
            w.put_u64(total).unwrap();
        }
        {
            let mut w = ByteWriter::new(&mut buf[APP_HEADER_LEN..APP_HEADER_LEN + APP_SEGMENT_LEN]);
            w.put_u64(0x4000).unwrap();
            w.put_u64((APP_HEADER_LEN + APP_SEGMENT_LEN) as u64).unwrap();
            w.put_u32(16).unwrap(); // file_size 16
            w.put_u32(8).unwrap(); // mem_size 8 < file_size — inconsistent
            w.put_u32(SEG_FLAG_READ).unwrap();
            w.put_u32(0).unwrap(); // reserved
        }
        assert_eq!(
            AppImage::parse(&buf).err(),
            Some(AppImageError::SegmentSizeInconsistent)
        );
    }
}
