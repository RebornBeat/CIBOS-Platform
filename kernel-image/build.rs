//! Emit the architecture linker script for the CIBOS kernel image, scoped to
//! this crate. The script depends on the boot path: the `self-boot` feature
//! links for standalone QEMU boot (multiboot/QEMU load addresses), while
//! without it the image links for the CIBIOS handoff path (loaded by CIBIOS at
//! a higher address, clear of the firmware).

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "none" {
        return; // host stub build needs no linker script
    }
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let self_boot = std::env::var("CARGO_FEATURE_SELF_BOOT").is_ok();

    let script = match (arch.as_str(), self_boot) {
        ("x86_64", true) => "x86_64.ld",
        ("x86_64", false) => "x86_64_handoff.ld",
        ("aarch64", true) => "aarch64.ld",
        ("aarch64", false) => "aarch64_handoff.ld",
        ("riscv64", true) => "riscv64.ld",
        ("riscv64", false) => "riscv64_handoff.ld",
        _ => return,
    };
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker/{script}");
    println!("cargo:rerun-if-changed=linker/{script}");
}
