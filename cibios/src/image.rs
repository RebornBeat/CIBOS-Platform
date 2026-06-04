//! # CIBOS Image Format
//!
//! The on-media layout of a CIBOS image and a safe parser for it.
//!
//! ## Layout
//!
//! ```text
//! +-------------------------------+  offset 0
//! | ImageHeader                   |
//! +-------------------------------+
//! | ComponentDescriptor[0]        |
//! | ComponentDescriptor[1]        |
//! | ...                           |
//! | ComponentDescriptor[N-1]      |
//! +-------------------------------+
//! | component 0 bytes             |
//! | component 1 bytes             |
//! | ...                           |
//! +-------------------------------+  offset = signed_region_len
//! | signature bytes               |
//! +-------------------------------+
//! ```
//!
//! Everything from offset 0 up to `signed_region_len` is covered by the
//! detached signature that follows it. The header records the architecture, the
//! kernel profile, the entry point, and the signature parameters; each component
//! descriptor records where its bytes live and their SHA-256 hash.
//!
//! ## Parsing
//!
//! [`ImageView`] borrows a byte slice and exposes typed, range-checked access to
//! the header, descriptors, and component bodies. No `unsafe`, no pointer
//! casts: every field is decoded with [`shared::utils::serialization::ByteReader`]
//! and every offset is validated against the slice length, so a malformed or
//! truncated image yields a [`FirmwareError`] rather than undefined behavior.

use crate::error::FirmwareError;
use shared::crypto::hash::{sha256, Digest256};
use shared::utils::serialization::ByteReader;

/// Image magic: ASCII "CIMG" (CIbos iMaGe), little-endian.
pub const IMAGE_MAGIC: u32 = 0x474D_4943;

/// Image format version understood by this firmware.
pub const IMAGE_VERSION: u32 = 1;

/// Encoded size of [`ImageHeader`] in bytes.
pub const HEADER_LEN: usize = 64;

/// Encoded size of one [`ComponentDescriptor`] in bytes.
pub const DESCRIPTOR_LEN: usize = 64;

/// Upper bound on component count, to bound parsing work on untrusted input.
pub const MAX_COMPONENTS: u32 = 32;

/// The kind of a CIBOS image component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ComponentKind {
    /// The kernel executable.
    Kernel = 1,
    /// The async runtime, if shipped as a separate component.
    Runtime = 2,
    /// An initial RAM filesystem.
    InitRamfs = 3,
    /// Embedded configuration.
    Config = 4,
}

impl ComponentKind {
    /// Decode from a `u32` discriminant.
    fn from_u32(v: u32) -> Result<Self, FirmwareError> {
        match v {
            1 => Ok(ComponentKind::Kernel),
            2 => Ok(ComponentKind::Runtime),
            3 => Ok(ComponentKind::InitRamfs),
            4 => Ok(ComponentKind::Config),
            _ => Err(FirmwareError::MalformedImage {
                detail: "unknown component kind",
            }),
        }
    }
}

/// The fixed-size image header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageHeader {
    /// Must equal [`IMAGE_MAGIC`].
    pub magic: u32,
    /// Must equal [`IMAGE_VERSION`].
    pub version: u32,
    /// Target architecture, as a [`shared::ProcessorArchitecture`] discriminant.
    pub architecture: u32,
    /// Target kernel profile, as a [`shared::CibosProfile`] discriminant.
    pub cibos_profile: u32,
    /// Number of component descriptors following the header.
    pub component_count: u32,
    /// Signature algorithm, as a [`shared::SignatureAlgorithm`] discriminant.
    pub signature_algorithm: u32,
    /// Kernel entry point address.
    pub entry_point: u64,
    /// Base load address for the image.
    pub load_base: u64,
    /// Number of leading bytes covered by the signature.
    pub signed_region_len: u64,
    /// Length of the trailing signature in bytes.
    pub signature_len: u32,
    /// Reserved, must be zero.
    pub _reserved: u32,
}

impl ImageHeader {
    fn parse(bytes: &[u8]) -> Result<Self, FirmwareError> {
        if bytes.len() < HEADER_LEN {
            return Err(FirmwareError::MalformedImage {
                detail: "image shorter than header",
            });
        }
        let mut r = ByteReader::new(&bytes[..HEADER_LEN]);
        // Field order defines the wire format; keep it in lockstep with the
        // builder in tests / tooling.
        let header = ImageHeader {
            magic: read_u32(&mut r)?,
            version: read_u32(&mut r)?,
            architecture: read_u32(&mut r)?,
            cibos_profile: read_u32(&mut r)?,
            component_count: read_u32(&mut r)?,
            signature_algorithm: read_u32(&mut r)?,
            entry_point: read_u64(&mut r)?,
            load_base: read_u64(&mut r)?,
            signed_region_len: read_u64(&mut r)?,
            signature_len: read_u32(&mut r)?,
            _reserved: read_u32(&mut r)?,
        };
        Ok(header)
    }
}

/// A descriptor locating one component within the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentDescriptor {
    /// Component kind discriminant.
    pub kind: u32,
    /// Byte offset from image start to the component body.
    pub offset: u64,
    /// Length of the component body in bytes.
    pub length: u64,
    /// Address this component should be loaded at.
    pub load_addr: u64,
    /// SHA-256 of the component body.
    pub hash: Digest256,
}

impl ComponentDescriptor {
    fn parse(bytes: &[u8]) -> Result<Self, FirmwareError> {
        if bytes.len() < DESCRIPTOR_LEN {
            return Err(FirmwareError::MalformedImage {
                detail: "descriptor truncated",
            });
        }
        let mut r = ByteReader::new(&bytes[..DESCRIPTOR_LEN]);
        let kind = read_u32(&mut r)?;
        let _reserved = read_u32(&mut r)?;
        let offset = read_u64(&mut r)?;
        let length = read_u64(&mut r)?;
        let load_addr = read_u64(&mut r)?;
        let mut hash = [0u8; 32];
        r.get_into(&mut hash)
            .map_err(|_| FirmwareError::MalformedImage {
                detail: "descriptor hash truncated",
            })?;
        Ok(ComponentDescriptor {
            kind,
            offset,
            length,
            load_addr,
            hash,
        })
    }

    /// The component kind, decoded.
    ///
    /// # Errors
    /// [`FirmwareError::MalformedImage`] if the kind discriminant is unknown.
    pub fn component_kind(&self) -> Result<ComponentKind, FirmwareError> {
        ComponentKind::from_u32(self.kind)
    }
}

/// A borrowed, validated view over a CIBOS image byte slice.
pub struct ImageView<'a> {
    bytes: &'a [u8],
    header: ImageHeader,
}

impl<'a> ImageView<'a> {
    /// Parse and structurally validate an image from `bytes`.
    ///
    /// Validates the magic, version, component-count bound, and that the
    /// declared signed region and descriptor table fit within the slice. Does
    /// *not* perform hashing or signature verification — see
    /// [`Self::verify_component_hashes`] and the `verification` module.
    ///
    /// # Errors
    /// [`FirmwareError::MalformedImage`] on any structural problem.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FirmwareError> {
        let header = ImageHeader::parse(bytes)?;
        if header.magic != IMAGE_MAGIC {
            return Err(FirmwareError::MalformedImage {
                detail: "bad image magic",
            });
        }
        if header.version != IMAGE_VERSION {
            return Err(FirmwareError::MalformedImage {
                detail: "unsupported image version",
            });
        }
        if header.component_count > MAX_COMPONENTS {
            return Err(FirmwareError::MalformedImage {
                detail: "too many components",
            });
        }

        // The descriptor table must fit after the header.
        let descriptors_len = (header.component_count as usize)
            .checked_mul(DESCRIPTOR_LEN)
            .ok_or(FirmwareError::MalformedImage {
                detail: "descriptor table size overflow",
            })?;
        let descriptors_end =
            HEADER_LEN
                .checked_add(descriptors_len)
                .ok_or(FirmwareError::MalformedImage {
                    detail: "descriptor table end overflow",
                })?;
        if descriptors_end > bytes.len() {
            return Err(FirmwareError::MalformedImage {
                detail: "descriptor table exceeds image",
            });
        }

        // The signed region plus signature must fit within the slice.
        let signed = header.signed_region_len as usize;
        let sig_end = signed
            .checked_add(header.signature_len as usize)
            .ok_or(FirmwareError::MalformedImage {
                detail: "signature region overflow",
            })?;
        if signed < descriptors_end || sig_end > bytes.len() {
            return Err(FirmwareError::MalformedImage {
                detail: "signed region inconsistent with image size",
            });
        }

        Ok(ImageView { bytes, header })
    }

    /// The parsed header.
    #[must_use]
    pub fn header(&self) -> &ImageHeader {
        &self.header
    }

    /// Decode the `index`-th component descriptor.
    ///
    /// # Errors
    /// [`FirmwareError::MalformedImage`] if `index` is out of range or the
    /// descriptor is malformed.
    pub fn descriptor(&self, index: u32) -> Result<ComponentDescriptor, FirmwareError> {
        if index >= self.header.component_count {
            return Err(FirmwareError::MalformedImage {
                detail: "component index out of range",
            });
        }
        let start = HEADER_LEN + (index as usize) * DESCRIPTOR_LEN;
        ComponentDescriptor::parse(&self.bytes[start..])
    }

    /// Borrow the body bytes of the component described by `desc`, validating
    /// that the declared range lies within the image.
    ///
    /// # Errors
    /// [`FirmwareError::MalformedImage`] if the range is out of bounds.
    pub fn component_body(
        &self,
        desc: &ComponentDescriptor,
    ) -> Result<&'a [u8], FirmwareError> {
        let start = desc.offset as usize;
        let end = start
            .checked_add(desc.length as usize)
            .ok_or(FirmwareError::MalformedImage {
                detail: "component range overflow",
            })?;
        if end > self.bytes.len() {
            return Err(FirmwareError::MalformedImage {
                detail: "component body exceeds image",
            });
        }
        Ok(&self.bytes[start..end])
    }

    /// The leading byte range covered by the signature.
    ///
    /// # Errors
    /// [`FirmwareError::MalformedImage`] if the region is inconsistent (already
    /// checked in [`Self::parse`], re-checked here defensively).
    pub fn signed_region(&self) -> Result<&'a [u8], FirmwareError> {
        let end = self.header.signed_region_len as usize;
        if end > self.bytes.len() {
            return Err(FirmwareError::MalformedImage {
                detail: "signed region exceeds image",
            });
        }
        Ok(&self.bytes[..end])
    }

    /// The trailing detached signature bytes.
    ///
    /// # Errors
    /// [`FirmwareError::MalformedImage`] if the signature range is inconsistent.
    pub fn signature(&self) -> Result<&'a [u8], FirmwareError> {
        let start = self.header.signed_region_len as usize;
        let end = start + self.header.signature_len as usize;
        if end > self.bytes.len() {
            return Err(FirmwareError::MalformedImage {
                detail: "signature exceeds image",
            });
        }
        Ok(&self.bytes[start..end])
    }

    /// Visit each component in order, passing its descriptor and body to `f`.
    ///
    /// This is the basis for *placement*: the firmware copies each component's
    /// body to its `load_addr`. Kept `alloc`-free and `unsafe`-free so the
    /// iteration logic is host-tested; the architecture layer supplies the
    /// closure that performs the actual physical copy.
    ///
    /// # Errors
    /// Propagates any [`FirmwareError`] from descriptor decoding, body bounds
    /// checking, or the closure.
    pub fn for_each_component<F>(&self, mut f: F) -> Result<(), FirmwareError>
    where
        F: FnMut(&ComponentDescriptor, &'a [u8]) -> Result<(), FirmwareError>,
    {
        for index in 0..self.header.component_count {
            let desc = self.descriptor(index)?;
            let body = self.component_body(&desc)?;
            f(&desc, body)?;
        }
        Ok(())
    }

    /// Verify every component body against the SHA-256 hash in its descriptor.
    ///
    /// This is integrity checking, independent of the signature: it catches
    /// corruption even on the Lightweight profile that does not verify
    /// signatures. Uses constant-time digest comparison.
    ///
    /// # Errors
    /// [`FirmwareError::ComponentHashMismatch`] for the first component whose
    /// content does not match, or [`FirmwareError::MalformedImage`] on a bad
    /// descriptor.
    pub fn verify_component_hashes(&self) -> Result<(), FirmwareError> {
        use shared::crypto::hash::digests_equal_ct;
        for index in 0..self.header.component_count {
            let desc = self.descriptor(index)?;
            let body = self.component_body(&desc)?;
            let actual = sha256(body);
            if !digests_equal_ct(&actual, &desc.hash) {
                return Err(FirmwareError::ComponentHashMismatch { index });
            }
        }
        Ok(())
    }
}

fn read_u32(r: &mut ByteReader<'_>) -> Result<u32, FirmwareError> {
    r.get_u32().map_err(|_| FirmwareError::MalformedImage {
        detail: "unexpected end of header/descriptor",
    })
}

fn read_u64(r: &mut ByteReader<'_>) -> Result<u64, FirmwareError> {
    r.get_u64().map_err(|_| FirmwareError::MalformedImage {
        detail: "unexpected end of header/descriptor",
    })
}

/// Image construction helpers, available with `std`. Used by the host test
/// suite and by build-time image-signing tooling. Never compiled into firmware.
#[cfg(feature = "std")]
pub mod build {
    use super::{
        ComponentKind, DESCRIPTOR_LEN, HEADER_LEN, IMAGE_MAGIC, IMAGE_VERSION,
    };
    use shared::crypto::hash::sha256;
    use shared::utils::serialization::ByteWriter;
    use std::vec::Vec;

    /// One component to place into an image.
    pub struct ComponentInput<'a> {
        /// Component kind.
        pub kind: ComponentKind,
        /// Address the component loads at.
        pub load_addr: u64,
        /// Raw component body.
        pub body: &'a [u8],
    }

    /// Parameters for building an image (everything except the components).
    pub struct ImageParams {
        /// Target architecture discriminant.
        pub architecture: u32,
        /// Target kernel profile discriminant.
        pub cibos_profile: u32,
        /// Kernel entry point.
        pub entry_point: u64,
        /// Image load base.
        pub load_base: u64,
        /// Signature algorithm discriminant.
        pub signature_algorithm: u32,
        /// Length the trailing signature will occupy.
        pub signature_len: u32,
    }

    /// Build the unsigned image bytes: header, descriptor table, and component
    /// bodies, with each descriptor's SHA-256 filled in. The returned vector
    /// has exactly `signed_region_len` bytes; the caller signs all of them and
    /// appends the signature with [`finalize_signed`].
    #[must_use]
    pub fn build_unsigned(params: &ImageParams, components: &[ComponentInput<'_>]) -> Vec<u8> {
        let count = components.len();
        let bodies_offset = HEADER_LEN + count * DESCRIPTOR_LEN;

        // Compute per-component absolute offsets.
        let mut offsets = Vec::with_capacity(count);
        let mut cursor = bodies_offset;
        for c in components {
            offsets.push(cursor);
            cursor += c.body.len();
        }
        let signed_region_len = cursor;

        let mut image = vec![0u8; signed_region_len];

        // Header.
        {
            let (head, _) = image.split_at_mut(HEADER_LEN);
            let mut w = ByteWriter::new(head);
            w.put_u32(IMAGE_MAGIC).unwrap();
            w.put_u32(IMAGE_VERSION).unwrap();
            w.put_u32(params.architecture).unwrap();
            w.put_u32(params.cibos_profile).unwrap();
            w.put_u32(count as u32).unwrap();
            w.put_u32(params.signature_algorithm).unwrap();
            w.put_u64(params.entry_point).unwrap();
            w.put_u64(params.load_base).unwrap();
            w.put_u64(signed_region_len as u64).unwrap();
            w.put_u32(params.signature_len).unwrap();
            w.put_u32(0).unwrap(); // _reserved
        }

        // Descriptors.
        for (i, c) in components.iter().enumerate() {
            let start = HEADER_LEN + i * DESCRIPTOR_LEN;
            let hash = sha256(c.body);
            let slice = &mut image[start..start + DESCRIPTOR_LEN];
            let mut w = ByteWriter::new(slice);
            w.put_u32(c.kind as u32).unwrap();
            w.put_u32(0).unwrap(); // _reserved
            w.put_u64(offsets[i] as u64).unwrap();
            w.put_u64(c.body.len() as u64).unwrap();
            w.put_u64(c.load_addr).unwrap();
            w.put_bytes(&hash).unwrap();
        }

        // Bodies.
        for (i, c) in components.iter().enumerate() {
            let start = offsets[i];
            image[start..start + c.body.len()].copy_from_slice(c.body);
        }

        image
    }

    /// Append a detached signature to an unsigned image, producing the final
    /// signed image bytes.
    #[must_use]
    pub fn finalize_signed(mut unsigned: Vec<u8>, signature: &[u8]) -> Vec<u8> {
        unsigned.extend_from_slice(signature);
        unsigned
    }
}

#[cfg(all(test, feature = "std"))]
mod placement_tests {
    use super::build::{ComponentInput, ImageParams};
    use super::{ComponentKind, ImageView};

    #[test]
    fn for_each_component_visits_bodies_at_load_addresses() {
        let kernel_body = b"kernel component bytes".to_vec();
        let config_body = b"config".to_vec();
        let params = ImageParams {
            architecture: shared::ProcessorArchitecture::X86_64.as_u32(),
            cibos_profile: shared::CibosProfile::Balanced as u32,
            entry_point: 0x0100_0000,
            load_base: 0x0100_0000,
            signature_algorithm: 0,
            signature_len: 0,
        };
        let components = [
            ComponentInput {
                kind: ComponentKind::Kernel,
                load_addr: 0x0100_0000,
                body: &kernel_body,
            },
            ComponentInput {
                kind: ComponentKind::Config,
                load_addr: 0x0200_0000,
                body: &config_body,
            },
        ];
        let image = super::build::build_unsigned(&params, &components);
        let view = ImageView::parse(&image).expect("parse");

        // Collect the placement plan and check it matches what we put in.
        let mut seen: Vec<(u64, Vec<u8>, u32)> = Vec::new();
        view.for_each_component(|desc, body| {
            seen.push((desc.load_addr, body.to_vec(), desc.kind));
            Ok(())
        })
        .expect("iterate");

        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, 0x0100_0000);
        assert_eq!(seen[0].1, kernel_body);
        assert_eq!(seen[0].2, ComponentKind::Kernel as u32);
        assert_eq!(seen[1].0, 0x0200_0000);
        assert_eq!(seen[1].1, config_body);
    }
}
