//! # CIBIOS — Complete Isolation Basic Input/Output System
//!
//! The firmware that replaces a conventional BIOS/UEFI in the CIBIOS/CIBOS/HIP
//! system. CIBIOS initializes hardware, locates and verifies the CIBOS image,
//! constructs the handoff record, and transfers control to the kernel.
//!
//! ## Library vs. binary
//!
//! This crate is split for testability. The **library** (this module tree)
//! contains the portable, `unsafe`-free firmware *logic*: image parsing and
//! verification, handoff construction, and the assembly of a hardware profile
//! from already-detected values. All of it is unit-tested on the host.
//!
//! The **binary** (`main.rs` and the architecture/boot glue it pulls in) is the
//! bare-metal entry: assembly entry points, MMIO device access, and the panic
//! handler. That code requires a real target and is exercised in QEMU, not by
//! the host test suite. It calls into this library for everything portable.
//!
//! ## `unsafe`
//!
//! The library forbids `unsafe`. Every hardware-touching operation that needs
//! `unsafe` lives in the binary, behind the architecture abstraction, so the
//! verifiable core stays verifiable.

#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod detection;
pub mod entropy;
pub mod error;
pub mod fdt;
pub mod handoff;
pub mod image;
pub mod multiboot;
pub mod verification;

pub use detection::{assemble_profile, firmware_profile, DetectedHardware};
pub use error::{FirmwareError, FirmwareResult};
pub use handoff::build_handoff;
pub use image::{ComponentDescriptor, ComponentKind, ImageHeader, ImageView};
pub use verification::{verify_image, VerificationPolicy, VerifiedImage};
