//! # `mkbootimage` — CIBOS bootable disk image tool
//!
//! Assembles a self-contained, directly-bootable CIBOS disk image. No GRUB, no
//! multiboot. The image is laid out in 512-byte sectors:
//!
//! ```text
//!   LBA 0      Stage 1 (MBR, 512 bytes, ends 0xAA55)
//!   LBA 1      Boot Layout Descriptor (one sector)
//!   LBA 2..    Stage 2
//!   ..         CIBIOS image (flat binary from objcopy of the cibios ELF)
//!   ..         CIBOS image (.cimg, an opaque blob to the bootloader)
//! ```
//!
//! Stage 1 reads the descriptor (LBA 1) to find Stage 2; Stage 2 reads it to
//! find CIBIOS and CIBOS, loads both, gathers the E820 map, builds the
//! `BootHandoff`, and jumps to the CIBIOS entry. The descriptor is serialized
//! directly from `shared`'s [`BootLayoutDescriptor`] via its `to_bytes`, so the
//! bytes on disk always match the contract the bootloader reads.
//!
//! Usage:
//! ```text
//!   mkbootimage --stage1 S1 --stage2 S2 --cibios CIBIOS --cibos CIBOS --out IMG
//!               [--cibios-load 0x100000] [--cibos-load 0x4000000]
//!               [--cibios-entry <addr; default = cibios-load>]
//! ```
//!
//! `--cibios` is the CIBIOS flat binary (its load address and entry default to
//! 1 MiB, matching `cibios/linker/x86_64.ld`). `--cibos` is the `.cimg` produced
//! by `mkimage`. The CIBOS load address defaults to 64 MiB, comfortably clear of
//! the firmware and the bootloader's low-memory working area.

use shared::protocols::boot::{BootLayoutDescriptor, BLD_VERSION};
use shared::BLD_MAGIC;
use std::path::PathBuf;
use std::process::ExitCode;

const SECTOR: u64 = 512;

/// Stage 1 always loads Stage 2 to this physical address (matches the constant
/// baked into `bootloader/boot/stage1.S` / `stage2.S`).
const STAGE2_LOAD_ADDR: u64 = 0x8000;
/// Default CIBIOS load address and entry (matches `cibios/linker/x86_64.ld`).
const DEFAULT_CIBIOS_LOAD: u64 = 0x0010_0000; // 1 MiB
/// Default CIBOS `.cimg` staging address (CIBIOS relocates components out of it).
const DEFAULT_CIBOS_LOAD: u64 = 0x0400_0000; // 64 MiB

/// Round `bytes` up to a whole number of 512-byte sectors.
fn sectors_for(bytes: u64) -> u64 {
    bytes.div_ceil(SECTOR)
}

struct Args {
    stage1: PathBuf,
    stage2: PathBuf,
    cibios: PathBuf,
    cibos: PathBuf,
    out: PathBuf,
    cibios_load: u64,
    cibos_load: u64,
    cibios_entry: Option<u64>,
}

fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

fn usage() {
    eprintln!(
        "usage:\n  mkbootimage --stage1 <stage1.bin> --stage2 <stage2.bin> \\\n              --cibios <cibios.bin> --cibos <cibos.cimg> --out <image.img> \\\n              [--cibios-load 0x100000] [--cibos-load 0x4000000] \\\n              [--cibios-entry <addr; default = cibios-load>]"
    );
}

fn parse_args() -> Option<Args> {
    let mut stage1 = None;
    let mut stage2 = None;
    let mut cibios = None;
    let mut cibos = None;
    let mut out = None;
    let mut cibios_load = DEFAULT_CIBIOS_LOAD;
    let mut cibos_load = DEFAULT_CIBOS_LOAD;
    let mut cibios_entry = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next();
        match flag.as_str() {
            "--stage1" => stage1 = Some(PathBuf::from(value()?)),
            "--stage2" => stage2 = Some(PathBuf::from(value()?)),
            "--cibios" => cibios = Some(PathBuf::from(value()?)),
            "--cibos" => cibos = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--cibios-load" => cibios_load = parse_u64(&value()?)?,
            "--cibos-load" => cibos_load = parse_u64(&value()?)?,
            "--cibios-entry" => cibios_entry = Some(parse_u64(&value()?)?),
            "-h" | "--help" => return None,
            other => {
                eprintln!("mkbootimage: unknown argument {other}");
                return None;
            }
        }
    }

    Some(Args {
        stage1: stage1?,
        stage2: stage2?,
        cibios: cibios?,
        cibos: cibos?,
        out: out?,
        cibios_load,
        cibos_load,
        cibios_entry,
    })
}

fn read_or_die(path: &PathBuf, label: &str) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("mkbootimage: reading {label} {}: {e}", path.display());
            None
        }
    }
}

fn main() -> ExitCode {
    let Some(args) = parse_args() else {
        usage();
        return ExitCode::FAILURE;
    };

    let Some(stage1) = read_or_die(&args.stage1, "stage1") else {
        return ExitCode::FAILURE;
    };
    let Some(stage2) = read_or_die(&args.stage2, "stage2") else {
        return ExitCode::FAILURE;
    };
    let Some(cibios) = read_or_die(&args.cibios, "cibios") else {
        return ExitCode::FAILURE;
    };
    let Some(cibos) = read_or_die(&args.cibos, "cibos") else {
        return ExitCode::FAILURE;
    };

    // Stage 1 is the MBR and must be exactly one sector ending in 0xAA55.
    if stage1.len() != SECTOR as usize {
        eprintln!(
            "mkbootimage: stage1 must be exactly {SECTOR} bytes (got {})",
            stage1.len()
        );
        return ExitCode::FAILURE;
    }
    if stage1[510] != 0x55 || stage1[511] != 0xAA {
        eprintln!("mkbootimage: stage1 is missing the 0xAA55 boot signature at offset 510");
        return ExitCode::FAILURE;
    }
    // Stage 2 must fit in the real-mode segment window Stage 1 loads it into.
    if stage2.len() > 32 * 1024 {
        eprintln!(
            "mkbootimage: stage2 is {} bytes; the real-mode load window is 32 KiB",
            stage2.len()
        );
        return ExitCode::FAILURE;
    }

    // --- Lay out the image in sectors. ---
    // LBA 0: Stage 1.  LBA 1: descriptor.  LBA 2: Stage 2.  Then CIBIOS, CIBOS.
    let stage2_lba = 2u64;
    let stage2_sectors = sectors_for(stage2.len() as u64);

    let cibios_lba = stage2_lba + stage2_sectors;
    let cibios_sectors = sectors_for(cibios.len() as u64);

    let cibos_lba = cibios_lba + cibios_sectors;
    let cibos_sectors = sectors_for(cibos.len() as u64);

    let cibios_entry = args.cibios_entry.unwrap_or(args.cibios_load);

    let descriptor = BootLayoutDescriptor {
        magic: BLD_MAGIC,
        version: BLD_VERSION,
        _pad0: 0,
        stage2_lba,
        stage2_sectors: stage2_sectors as u32,
        _pad1: 0,
        stage2_load_addr: STAGE2_LOAD_ADDR,
        cibios_lba,
        cibios_sectors: cibios_sectors as u32,
        _pad2: 0,
        cibios_load_addr: args.cibios_load,
        cibios_entry,
        cibos_lba,
        cibos_sectors: cibos_sectors as u32,
        _pad3: 0,
        cibos_load_addr: args.cibos_load,
        cibos_size: cibos.len() as u64,
    };

    // --- Assemble the image. Each region is padded up to its sector count. ---
    let total_sectors = cibos_lba + cibos_sectors;
    let mut image = vec![0u8; (total_sectors * SECTOR) as usize];

    let put = |image: &mut [u8], lba: u64, bytes: &[u8]| {
        let off = (lba * SECTOR) as usize;
        image[off..off + bytes.len()].copy_from_slice(bytes);
    };

    put(&mut image, 0, &stage1);
    put(&mut image, 1, &descriptor.to_bytes());
    put(&mut image, stage2_lba, &stage2);
    put(&mut image, cibios_lba, &cibios);
    put(&mut image, cibos_lba, &cibos);

    if let Err(e) = std::fs::write(&args.out, &image) {
        eprintln!("mkbootimage: writing {}: {e}", args.out.display());
        return ExitCode::FAILURE;
    }

    println!("wrote {} ({} bytes, {total_sectors} sectors)", args.out.display(), image.len());
    println!("  stage1  : LBA 0, 1 sector");
    println!("  layout  : LBA 1, 1 sector");
    println!(
        "  stage2  : LBA {stage2_lba}, {stage2_sectors} sector(s), load {STAGE2_LOAD_ADDR:#x}"
    );
    println!(
        "  cibios  : LBA {cibios_lba}, {cibios_sectors} sector(s), load {:#x}, entry {cibios_entry:#x}",
        args.cibios_load
    );
    println!(
        "  cibos   : LBA {cibos_lba}, {cibos_sectors} sector(s), load {:#x}, {} bytes",
        args.cibos_load,
        cibos.len()
    );
    ExitCode::SUCCESS
}
