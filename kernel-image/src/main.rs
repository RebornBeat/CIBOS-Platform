//! # `cibos-kernel` image — the bootable CIBOS kernel
//!
//! On a bare target (`*-unknown-none`) this is a `no_std`/`no_main` binary: the
//! architecture boot assembly calls [`boot::kernel_entry`], which brings up the
//! heap and boots [`cibos_kernel::Kernel`] from the CIBIOS handoff. On the host
//! it compiles to a small stub so the crate participates in normal workspace
//! builds. See `QEMU.md` for boot commands.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(target_os = "none")]
extern crate alloc;

#[cfg(target_os = "none")]
mod arch;
#[cfg(target_os = "none")]
#[macro_use]
mod boot;
#[cfg(all(target_os = "none", target_arch = "x86_64"))]
mod loader;
#[cfg(all(target_os = "none", target_arch = "x86_64", feature = "ring3-multilane-demo"))]
mod ring3;
#[cfg(all(target_os = "none", target_arch = "x86_64"))]
mod keyboard;
#[cfg(all(target_os = "none", target_arch = "x86_64", feature = "gui-demo"))]
mod gui;
#[cfg(all(target_os = "none", target_arch = "x86_64"))]
mod timer;
#[cfg(all(target_os = "none", target_arch = "x86_64"))]
mod net_stack;

#[cfg(not(target_os = "none"))]
fn main() {
    println!(
        "cibos-kernel is a bare-metal image; build it for a *-unknown-none target. See QEMU.md."
    );
}
