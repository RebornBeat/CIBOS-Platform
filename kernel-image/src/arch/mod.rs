//! Architecture-specific serial output and halt for the kernel image.
//!
//! Compiled only for `target_os = "none"`. Each backend provides `init_serial`,
//! `putc`, and `halt`. Serial targets the QEMU `virt`/PC defaults so the kernel
//! can prove liveness over the console.

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::{halt, init_serial, putc};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::{halt, init_serial, putc};

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub use riscv64::{halt, init_serial, putc};
