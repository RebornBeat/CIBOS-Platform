//! Architecture-specific console output and halt for the kernel image.
//!
//! Compiled only for `target_os = "none"`. Each backend provides `init_serial`,
//! `putc`, and `halt`. On x86_64 the console is dual: COM1 serial (the bring-up
//! capture target) and the VGA text console (on-screen output via `vga`). On
//! aarch64/riscv64 the console targets the QEMU `virt` serial defaults so the
//! kernel can prove liveness; an on-screen framebuffer console for those is a
//! later step.

#[cfg(target_arch = "x86_64")]
mod vga;
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
