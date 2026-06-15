//! # `mkimage` — CIBOS image tool
//!
//! Wraps a flat kernel binary (produced by `objcopy -O binary` from the kernel
//! ELF) into a CIBOS image (`.cimg`) using the firmware's own image-build
//! module, and verifies images through the firmware's own parser and verifier.
//! Building the image here with the same code the firmware reads guarantees the
//! two agree on the format.
//!
//! Usage:
//! ```text
//!   mkimage build <arch> <entry_hex> <load_hex> <kernel.bin> <out.cimg>
//!   mkimage verify <image.cimg> <arch>
//! ```
//! `<arch>` is one of `x86_64`, `aarch64`, `riscv64`, `x86` (i686). Images are Lightweight
//! (unsigned); the firmware accepts them when its profile does not require a
//! signature.

use cibios::image::build::{build_unsigned, finalize_signed, ComponentInput, ImageParams};
use cibios::image::{ComponentKind, ImageView};
use cibios::{verify_image, VerificationPolicy};
use shared::crypto::backends::sphincs::{generate_keypair, SphincsPlusSigner, SIGNATURE_LEN};
use shared::crypto::SignatureSigner;
use shared::{CibosProfile, ProcessorArchitecture, SignatureAlgorithm};
use std::process::ExitCode;

fn arch_from_str(s: &str) -> Option<ProcessorArchitecture> {
    match s {
        "x86_64" => Some(ProcessorArchitecture::X86_64),
        "aarch64" => Some(ProcessorArchitecture::AArch64),
        "riscv64" => Some(ProcessorArchitecture::RiscV64),
        "x86" => Some(ProcessorArchitecture::X86),
        _ => None,
    }
}

fn parse_hex(s: &str) -> Option<u64> {
    let t = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(t, 16).ok()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("build") => cmd_build(&args),
        Some("sign") => cmd_sign(&args),
        Some("keygen") => cmd_keygen(&args),
        Some("verify") => cmd_verify(&args),
        _ => {
            eprintln!(
                "usage:\n  mkimage build  <arch> <entry_hex> <load_hex> <kernel.bin> <out.cimg>\n  mkimage sign   <arch> <entry_hex> <load_hex> <kernel.bin> <key_file> <out.cimg>\n  mkimage keygen <pub_out> <key_out>\n  mkimage verify <image.cimg> <arch> [pubkey_file]"
            );
            ExitCode::FAILURE
        }
    }
}

/// Parse an operational-profile name into a [`CibosProfile`].
fn profile_from_str(s: &str) -> Option<CibosProfile> {
    match s {
        "maximum-isolation" => Some(CibosProfile::MaximumIsolation),
        "balanced" => Some(CibosProfile::Balanced),
        "performance" => Some(CibosProfile::Performance),
        "compute" => Some(CibosProfile::Compute),
        _ => None,
    }
}

fn cmd_build(args: &[String]) -> ExitCode {
    if args.len() != 7 && args.len() != 8 {
        eprintln!(
            "build: expected <arch> <entry_hex> <load_hex> <kernel.bin> <out.cimg> [profile]"
        );
        return ExitCode::FAILURE;
    }
    let Some(arch) = arch_from_str(&args[2]) else {
        eprintln!("build: unknown arch {}", args[2]);
        return ExitCode::FAILURE;
    };
    let (Some(entry), Some(load)) = (parse_hex(&args[3]), parse_hex(&args[4])) else {
        eprintln!("build: entry/load must be hex (e.g. 0x1000000)");
        return ExitCode::FAILURE;
    };

    let body = match std::fs::read(&args[5]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("build: reading {}: {e}", args[5]);
            return ExitCode::FAILURE;
        }
    };

    // Profile to stamp into the image header. Defaults to Balanced for the
    // generic builder; the profile-aware builder passes it explicitly so the
    // image, the handoff the firmware derives from it, and the kernel's compiled
    // profile all agree (the kernel halts on a mismatch).
    let profile = match args.get(7) {
        Some(p) => match profile_from_str(p) {
            Some(pr) => pr,
            None => {
                eprintln!("build: unknown profile {p}");
                return ExitCode::FAILURE;
            }
        },
        None => CibosProfile::Balanced,
    };

    // Lightweight (unsigned) image: a single Kernel component.
    let params = ImageParams {
        architecture: arch.as_u32(),
        cibos_profile: profile.as_u32(),
        entry_point: entry,
        load_base: load,
        signature_algorithm: 0,
        signature_len: 0,
    };
    let components = [ComponentInput {
        kind: ComponentKind::Kernel,
        load_addr: load,
        body: &body,
    }];
    let image = build_unsigned(&params, &components);

    if let Err(e) = std::fs::write(&args[6], &image) {
        eprintln!("build: writing {}: {e}", args[6]);
        return ExitCode::FAILURE;
    }
    println!(
        "wrote {} ({} bytes): {} kernel component, entry {:#x}, load {:#x}",
        args[6],
        image.len(),
        body.len(),
        entry,
        load
    );
    ExitCode::SUCCESS
}

fn cmd_keygen(args: &[String]) -> ExitCode {
    if args.len() != 4 {
        eprintln!("keygen: expected <pub_out> <key_out>");
        return ExitCode::FAILURE;
    }
    let (pk, sk) = match generate_keypair() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("keygen: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&args[2], &pk) {
        eprintln!("keygen: writing {}: {e}", args[2]);
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&args[3], &sk) {
        eprintln!("keygen: writing {}: {e}", args[3]);
        return ExitCode::FAILURE;
    }
    println!(
        "wrote SPHINCS+ public key {} ({} bytes), secret key {} ({} bytes)",
        args[2],
        pk.len(),
        args[3],
        sk.len()
    );
    ExitCode::SUCCESS
}

fn cmd_sign(args: &[String]) -> ExitCode {
    if args.len() != 8 && args.len() != 9 {
        eprintln!("sign: expected <arch> <entry_hex> <load_hex> <kernel.bin> <key_file> <out.cimg> [profile]");
        return ExitCode::FAILURE;
    }
    let Some(arch) = arch_from_str(&args[2]) else {
        eprintln!("sign: unknown arch {}", args[2]);
        return ExitCode::FAILURE;
    };
    let (Some(entry), Some(load)) = (parse_hex(&args[3]), parse_hex(&args[4])) else {
        eprintln!("sign: entry/load must be hex");
        return ExitCode::FAILURE;
    };
    let body = match std::fs::read(&args[5]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("sign: reading {}: {e}", args[5]);
            return ExitCode::FAILURE;
        }
    };
    let secret_key = match std::fs::read(&args[6]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("sign: reading key {}: {e}", args[6]);
            return ExitCode::FAILURE;
        }
    };

    // Profile to stamp into the image header. Defaults to Balanced; the
    // profile-aware caller passes it explicitly so the image, the handoff the
    // firmware derives, and the kernel's compiled profile all agree (the kernel
    // halts on a mismatch). The profile arg is the 8th positional (index 8),
    // after the output path, mirroring `build`.
    let profile = match args.get(8) {
        Some(p) => match profile_from_str(p) {
            Some(pr) => pr,
            None => {
                eprintln!("sign: unknown profile {p}");
                return ExitCode::FAILURE;
            }
        },
        None => CibosProfile::Balanced,
    };

    // Reserve the SPHINCS+ signature length in the header, build the signed
    // region, sign exactly those bytes, then append the detached signature.
    let params = ImageParams {
        architecture: arch.as_u32(),
        cibos_profile: profile as u32,
        entry_point: entry,
        load_base: load,
        signature_algorithm: SignatureAlgorithm::SphincsPlus.as_u32(),
        signature_len: SIGNATURE_LEN as u32,
    };
    let components = [ComponentInput {
        kind: ComponentKind::Kernel,
        load_addr: load,
        body: &body,
    }];
    let unsigned = build_unsigned(&params, &components);

    let mut signature = Vec::new();
    if let Err(e) = SphincsPlusSigner::sign(&secret_key, &unsigned, &mut signature) {
        eprintln!("sign: {e}");
        return ExitCode::FAILURE;
    }
    if signature.len() != SIGNATURE_LEN {
        eprintln!(
            "sign: unexpected signature length {} (expected {SIGNATURE_LEN})",
            signature.len()
        );
        return ExitCode::FAILURE;
    }
    let image = finalize_signed(unsigned, &signature);

    if let Err(e) = std::fs::write(&args[7], &image) {
        eprintln!("sign: writing {}: {e}", args[7]);
        return ExitCode::FAILURE;
    }
    println!(
        "wrote signed {} ({} bytes): kernel {} bytes, SPHINCS+ signature {} bytes, entry {:#x}",
        args[7],
        image.len(),
        body.len(),
        signature.len(),
        entry
    );
    ExitCode::SUCCESS
}

fn cmd_verify(args: &[String]) -> ExitCode {
    if args.len() != 4 && args.len() != 5 {
        eprintln!("verify: expected <image.cimg> <arch> [pubkey_file]");
        return ExitCode::FAILURE;
    }
    let Some(arch) = arch_from_str(&args[3]) else {
        eprintln!("verify: unknown arch {}", args[3]);
        return ExitCode::FAILURE;
    };
    let image = match std::fs::read(&args[2]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("verify: reading {}: {e}", args[2]);
            return ExitCode::FAILURE;
        }
    };
    let pubkey = if args.len() == 5 {
        match std::fs::read(&args[4]) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("verify: reading pubkey {}: {e}", args[4]);
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    let view = match ImageView::parse(&image) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("verify: parse failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "parsed: {} component(s), entry {:#x}, load_base {:#x}",
        view.header().component_count,
        view.header().entry_point,
        view.header().load_base
    );

    // If a public key is supplied, require and check the signature (Standard
    // policy); otherwise verify hashes only (Lightweight policy).
    let policy = VerificationPolicy {
        require_signature: pubkey.is_some(),
        running_architecture: arch.as_u32(),
    };
    let key = pubkey.as_deref().unwrap_or(&[]);
    match verify_image(&image, &policy, key) {
        Ok(v) => {
            println!(
                "VERIFIED: entry {:#x}, {} component(s), signature {}",
                v.entry_point,
                v.component_count,
                if v.signature_verified {
                    "checked (SPHINCS+)"
                } else {
                    "skipped (Lightweight)"
                }
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("verify: {e}");
            ExitCode::FAILURE
        }
    }
}
