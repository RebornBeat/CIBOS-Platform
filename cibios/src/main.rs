//! CIBIOS firmware binary entry.
//!
//! On a bare-metal target (`target_os = "none"`) this pulls in the `bare`
//! module: the architecture boot assembly, the panic handler, and the boot
//! orchestration, all of which call into the `cibios` library for portable
//! logic. On the host it is a small stub so the workspace builds and the
//! library test suite runs.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(target_os = "none")]
mod bare;

#[cfg(not(target_os = "none"))]
fn main() {
    println!(
        "CIBIOS firmware logic is provided as the `cibios` library and verified by `cargo test`.\n\
         Build for a bare-metal target (x86_64-unknown-none, aarch64-unknown-none,\n\
         riscv64gc-unknown-none-elf) to produce a firmware image."
    );
}
