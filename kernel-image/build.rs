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

    // Build the sample "hello" application into a .capp embedded in the kernel
    // image (x86_64 only — the app uses the x86_64 syscall ABI). This is the
    // baked-in-app pipeline in miniature: assemble a standalone user program,
    // link it at the application virtual address, objcopy to a flat binary, and
    // wrap it in a .capp via the shared AppImageBuilder. The kernel loads it
    // through loader::run_app_image.
    if arch == "x86_64" {
        build_hello_capp(&dir);
        build_hello_rs_capp(&dir);
    }
}

fn build_hello_capp(dir: &str) {
    use shared::AppImageBuilder;
    use shared::{SEG_FLAG_EXEC, SEG_FLAG_READ};
    use std::path::Path;
    use std::process::Command;

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let app_dir = format!("{dir}/apps");
    let src = format!("{app_dir}/hello.s");
    let ld = format!("{app_dir}/hello.ld");
    let obj = format!("{out_dir}/hello.o");
    let elf = format!("{out_dir}/hello.elf");
    let bin = format!("{out_dir}/hello.bin");
    let capp = format!("{out_dir}/hello.capp");
    const APP_VADDR: u64 = 0x0000_5000_0000_0000;

    println!("cargo:rerun-if-changed={src}");
    println!("cargo:rerun-if-changed={ld}");

    // Assemble (gcc as the assembler driver: handles .intel_syntax + cpp).
    run(Command::new("gcc")
        .args(["-m64", "-ffreestanding", "-nostdlib", "-c", &src, "-o", &obj]));
    // Link at the app virtual address per the app linker script.
    run(Command::new("ld").args(["-T", &ld, "-o", &elf, &obj]));
    // Flatten to raw bytes (the single R+X segment).
    run(Command::new("objcopy").args(["-O", "binary", &elf, &bin]));

    let code = std::fs::read(&bin).expect("read hello.bin");
    assert!(!code.is_empty(), "hello app flat binary is empty");
    let image = AppImageBuilder::new(APP_VADDR)
        .segment(
            APP_VADDR,
            code.len() as u32,
            SEG_FLAG_READ | SEG_FLAG_EXEC,
            &code,
        )
        .build();
    std::fs::write(&capp, &image).expect("write hello.capp");
    assert!(
        Path::new(&capp).exists(),
        "hello.capp was not produced at {capp}"
    );
}

fn run(cmd: &mut std::process::Command) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {cmd:?}: {e}"));
    assert!(status.success(), "command failed: {cmd:?} -> {status}");
}

/// Build the Rust `hello-rs` application (which uses the `cibos-app` runtime)
/// into a `.capp`. The app is a standalone freestanding crate; we compile it for
/// the bare app target with the large code model (its code/data live at the high
/// application virtual address, which needs 64-bit relocations), objcopy to a
/// flat binary, and wrap it as a single read+execute segment. This proves a real
/// Rust application — not assembly — runs in ring 3 via the syscall ABI.
fn build_hello_rs_capp(dir: &str) {
    use shared::AppImageBuilder;
    use shared::{SEG_FLAG_EXEC, SEG_FLAG_READ};
    use std::process::Command;

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let app_dir = format!("{dir}/apps/hello-rs");
    let ld = format!("{app_dir}/hello-rs.ld");
    let app_target_dir = format!("{out_dir}/hello-rs-target");
    let elf = format!(
        "{app_target_dir}/x86_64-unknown-none/release/hello-rs"
    );
    let bin = format!("{out_dir}/hello-rs.bin");
    let capp = format!("{out_dir}/hello-rs.capp");
    const APP_VADDR: u64 = 0x0000_5100_0000_0000;

    println!("cargo:rerun-if-changed={app_dir}/src/main.rs");
    println!("cargo:rerun-if-changed={ld}");
    println!("cargo:rerun-if-changed={app_dir}/Cargo.toml");

    // Compile the standalone app crate for the bare target. The large code model
    // emits 64-bit relocations (required at the high app vaddr); -Z build-std
    // rebuilds core under the same model so LTO/bitcode agree. A separate target
    // dir avoids contention with the outer build.
    let rustflags = format!(
        "-C link-arg=-T{ld} -C link-arg=-nostdlib -C relocation-model=static -C code-model=large"
    );
    // Resolve the nightly toolchain root from RUSTUP_HOME (the build-script env
    // inherits it). Invoke nightly's cargo/rustc by absolute path so the outer
    // toolchain pin cannot redirect us back to stable (build-std needs nightly).
    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| {
        format!("{}/.rustup", std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
    });
    let nightly = format!("{rustup_home}/toolchains/nightly-x86_64-unknown-linux-gnu");
    let cargo_bin = format!("{nightly}/bin/cargo");
    run(Command::new(&cargo_bin)
        .args([
            "build",
            "--release",
            "--target",
            "x86_64-unknown-none",
            "-Z",
            "build-std=core",
            "--target-dir",
            &app_target_dir,
        ])
        .current_dir(&app_dir)
        .env("RUSTUP_TOOLCHAIN", "nightly")
        // Let nightly cargo resolve its OWN rustc and sysroot (needed for
        // build-std's rust-src lookup). Clearing RUSTC and the wrapper vars is
        // essential: the outer build (and clippy) set RUSTC / RUSTC_WRAPPER /
        // RUSTC_WORKSPACE_WRAPPER, which would point build-std at the wrong
        // sysroot or run clippy on core.
        .env_remove("RUSTC")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env("RUSTFLAGS", &rustflags));

    // Flatten to raw bytes and wrap as a .capp.
    run(Command::new("objcopy").args(["-O", "binary", &elf, &bin]));
    let code = std::fs::read(&bin).expect("read hello-rs.bin");
    assert!(!code.is_empty(), "hello-rs flat binary is empty");
    let image = AppImageBuilder::new(APP_VADDR)
        .segment(
            APP_VADDR,
            code.len() as u32,
            SEG_FLAG_READ | SEG_FLAG_EXEC,
            &code,
        )
        .build();
    std::fs::write(&capp, &image).expect("write hello-rs.capp");
}
