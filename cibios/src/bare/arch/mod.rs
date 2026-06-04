//! Architecture abstraction for the bare-metal firmware binary.
//!
//! Each supported architecture provides the same small surface — byte-level
//! serial output, a halt, and hardware detection — implemented with the
//! `unsafe` operations (port I/O, MMIO, CPUID, `ecall`) that the portable logic
//! library is forbidden from using. The portable logic in the `cibios` library
//! consumes the [`DetectResult`] this layer produces.

use cibios::detection::DetectedHardware;
use cibios::error::FirmwareError;

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::{detect, gather_entropy, halt, jump_to_kernel, locate_image, putc};

#[cfg(target_arch = "x86")]
mod x86;
#[cfg(target_arch = "x86")]
pub use x86::{detect, gather_entropy, halt, jump_to_kernel, locate_image, putc};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::{detect, gather_entropy, halt, jump_to_kernel, locate_image, putc};

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub use riscv64::{detect, gather_entropy, halt, jump_to_kernel, locate_image, putc};

/// The result of architecture-specific hardware detection: the assembled raw
/// facts plus the primary usable memory region (base and length) for the
/// handoff memory map.
#[derive(Debug, Clone, Copy)]
pub struct DetectResult {
    /// Raw detected hardware facts.
    pub hardware: DetectedHardware,
    /// Base physical address of primary usable RAM.
    pub memory_base: u64,
    /// Length in bytes of primary usable RAM.
    pub memory_length: u64,
}

/// Convenience alias for detection results.
pub type DetectOutcome = Result<DetectResult, FirmwareError>;
