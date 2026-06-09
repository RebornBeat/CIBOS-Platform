//! # `shared` — CIBIOS / CIBOS / HIP Common Foundation
//!
//! The foundational crate every other crate in the workspace builds on. It
//! defines the cross-cutting type vocabulary, the cryptographic abstractions
//! and backends, the firmware↔kernel protocols, and small utility helpers.
//!
//! ## `no_std`
//!
//! This crate is `#![no_std]` for real builds and links `alloc`. The standard
//! library is pulled in only under `cfg(test)` so unit tests can run on the
//! host, and only behind the `std` feature for the build-time signing tooling.
//! Nothing in the default build requires an operating system, which is what
//! lets firmware (CIBIOS) and the kernel (CIBOS) depend on it directly.
//!
//! ## Module map
//!
//! * [`types`] — the data vocabulary: errors, hardware description, isolation
//!   boundaries, authentication, user profiles, system/operational profiles,
//!   and a `no_std` monotonic clock. Types crossing the handoff carry stable
//!   `#[repr(u32)]`/`#[repr(C)]` representations.
//! * [`crypto`] — hashing (always present) plus signature, KEM, and the
//!   feature-gated SPHINCS+, ML-DSA, and ML-KEM backends. The trait layer
//!   always compiles; algorithms are selected by Cargo features, which is how
//!   the no-crypto / classical / post-quantum deployment paths share one type
//!   system.
//! * [`protocols`] — the firmware→kernel [`protocols::handoff`] contract, the
//!   [`protocols::ipc`] kernel/runtime interface (`KernelInterface`) and
//!   channel vocabulary, and the [`protocols::authentication`] exchange.
//! * [`utils`] — a bounds-checked serialization cursor and shared validators.
//!
//! ## Error model
//!
//! Every fallible operation returns a typed error rooted at
//! [`types::error::SharedError`]. Downstream crates wrap this in their own
//! domain error via `From`, preserving a single uniform chain. Errors are
//! allocation-free (`&'static str` context) so they work before an allocator
//! exists.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(any(feature = "std", test))]
extern crate alloc;

pub mod crypto;
pub mod protocols;
pub mod types;
pub mod utils;

// ---------------------------------------------------------------------------
// Curated re-exports: the most frequently used items, surfaced at the crate
// root so downstream crates can `use shared::{SharedError, HandoffData, ...}`
// without reaching through the full module path. The modules remain public for
// everything else.
// ---------------------------------------------------------------------------

pub use types::error::{SharedError, SharedResult};

pub use types::hardware::{
    CoreTopology, HardwarePlatform, HardwareProfile, MemoryRegion, MemoryRegionKind,
    ProcessorArchitecture,
};

pub use types::isolation::{
    BoundaryConfiguration, BoundaryId, ChannelId, LaneId, ResourceLimits, ResourceUsage,
    WeightClass,
};

pub use types::authentication::{
    AuthenticationMethod, AuthenticationOutcome, AuthenticationRequest, CredentialFormat,
    KeyDeviceInterface,
};

pub use types::profiles::{
    ActiveProfile, ProfileCapabilities, ProfileConfiguration, ProfileId, ProfileName,
};

pub use types::config::{CibiosProfile, CibosProfile, HandoffMode, SchedulingConfig};

pub use types::time::Monotonic;

pub use crypto::{
    sha256, sha3_256, sha3_512, sha512, Digest256, Digest512, KemAlgorithm, KeyEncapsulation,
    SignatureAlgorithm, SignatureVerifier,
};

pub use protocols::app_image::{
    AppImage, AppImageError, AppImageHeader, AppSegment, APP_MAGIC, APP_VERSION, MAX_SEGMENTS,
    SEG_FLAG_EXEC, SEG_FLAG_READ, SEG_FLAG_WRITE,
};
#[cfg(feature = "std")]
pub use protocols::app_image::AppImageBuilder;

pub use protocols::boot::{
    BootHandoff, BootLayoutDescriptor, BootMemoryRegion, BootRegionType, BLD_MAGIC, BLD_VERSION,
    BOOT_HANDOFF_MAGIC, BOOT_HANDOFF_VERSION,
};

pub use protocols::handoff::{DecodedHandoff, HandoffData, HANDOFF_MAGIC, HANDOFF_VERSION};

pub use protocols::ipc::{ChannelDirection, ChannelTerms, KernelInterface, WaitResource};
