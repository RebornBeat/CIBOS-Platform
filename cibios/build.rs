//! Emit the architecture linker script for the CIBIOS firmware binary, scoped
//! to this crate so it does not affect other bare binaries in the workspace.
//! Also validate the firmware feature combination at build time.

fn main() {
    validate_features();

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "none" {
        return; // host builds (lib + tests) need no linker script
    }
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let script = match arch.as_str() {
        "x86_64" => "x86_64.ld",
        "x86" => "x86.ld",
        "aarch64" => "aarch64.ld",
        "riscv64" => "riscv64.ld",
        _ => return,
    };
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker/{script}");
    println!("cargo:rerun-if-changed=linker/{script}");
}

/// Reject illegal firmware feature combinations at compile time.
///
/// A firmware image is Standard *xor* Lightweight: it either verifies the CIBOS
/// signature (`handoff-cryptographic`) or it does not (`handoff-lightweight` /
/// absence). Compiling both verifiers-present and physical-trust into one image
/// is contradictory, so we fail the build with a clear message rather than
/// produce an ambiguous firmware.
fn validate_features() {
    let cryptographic = std::env::var_os("CARGO_FEATURE_HANDOFF_CRYPTOGRAPHIC").is_some();
    let lightweight = std::env::var_os("CARGO_FEATURE_HANDOFF_LIGHTWEIGHT").is_some();
    if cryptographic && lightweight {
        panic!(
            "CIBIOS feature conflict: `handoff-cryptographic` and \
             `handoff-lightweight` are mutually exclusive (a firmware is Standard \
             xor Lightweight). If you enabled a `profile-*` bundle, build the \
             firmware with `--no-default-features` and exactly one handoff mode."
        );
    }

    // A firmware image is entered exactly one way on x86-family hardware: by a
    // multiboot loader (QEMU bring-up) or by the from-scratch CIBOS bootloader
    // (bare metal / USB). The two boot entries are different assembly and
    // different image-acquisition logic, so compiling both into one image is
    // contradictory. (On aarch64/riscv64 both flags are inert — those targets
    // always boot via the platform device tree.)
    let multiboot = std::env::var_os("CARGO_FEATURE_FIRMWARE_MULTIBOOT").is_some();
    let bootloader = std::env::var_os("CARGO_FEATURE_FIRMWARE_BOOTLOADER").is_some();
    if multiboot && bootloader {
        panic!(
            "CIBIOS feature conflict: `firmware-multiboot` and \
             `firmware-bootloader` are mutually exclusive (a firmware image has \
             exactly one boot entry). Build the bare-metal path with \
             `--no-default-features --features firmware-bootloader[,handoff-...]`."
        );
    }
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let bare = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "none";
    if bare
        && matches!(arch.as_str(), "x86_64" | "x86")
        && !multiboot
        && !bootloader
    {
        panic!(
            "CIBIOS x86/x86_64 firmware needs a boot entry: enable exactly one of \
             `firmware-multiboot` (QEMU `-kernel`) or `firmware-bootloader` \
             (bare metal). The default features include `firmware-multiboot`; if \
             you built with `--no-default-features`, add one explicitly."
        );
    }
}
