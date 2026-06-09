//! Bare-metal firmware entry, panic handler, and boot orchestration.
//!
//! Compiled only for `target_os = "none"`. This module wires the architecture
//! boot assembly to the Rust entry, provides a serial [`Console`] and a panic
//! handler that reports through it, and runs the firmware boot sequence by
//! calling into the portable, host-tested logic in the `cibios` library.

use cibios::detection::assemble_profile;
use cibios::image::ImageView;
use cibios::{verify_image, VerificationPolicy};
use core::arch::global_asm;
use core::fmt::{self, Write};

pub mod arch;

// Pull in the architecture boot entry. Each defines `_start`, sets up the
// stack, clears BSS, and calls `cibios_entry`.
//
// On x86/x86_64 the entry differs by boot source: the multiboot entry starts in
// 32-bit protected mode and performs the long-mode transition itself (QEMU
// `-kernel`); the bootloader entry is reached already in the final CPU mode by
// the from-scratch CIBOS bootloader, which leaves the `BootHandoff` pointer in
// the first-argument register. ARM/RISC-V always boot via the platform device
// tree, so their entry does not vary.
#[cfg(all(target_arch = "x86_64", feature = "firmware-multiboot"))]
global_asm!(include_str!("boot/x86_64.s"));
#[cfg(all(target_arch = "x86_64", feature = "firmware-bootloader"))]
global_asm!(include_str!("boot/x86_64_bootloader.s"));
#[cfg(all(target_arch = "x86", feature = "firmware-multiboot"))]
global_asm!(include_str!("boot/x86.s"));
#[cfg(all(target_arch = "x86", feature = "firmware-bootloader"))]
global_asm!(include_str!("boot/x86_bootloader.s"));
#[cfg(target_arch = "aarch64")]
global_asm!(include_str!("boot/aarch64.s"));
#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("boot/riscv64.s"));

/// A zero-sized serial console implementing [`core::fmt::Write`] over the
/// architecture's byte output.
pub struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            arch::putc(byte);
        }
        Ok(())
    }
}

/// Print a formatted line to the serial console. Errors are ignored — there is
/// nowhere better to report a failed debug print during boot.
macro_rules! kprintln {
    ($($arg:tt)*) => {{
        let mut console = $crate::bare::Console;
        let _ = ::core::writeln!(console, $($arg)*);
    }};
}

/// The Rust entry point, called by the architecture boot assembly once the
/// stack is set up and BSS is cleared. Never returns.
///
/// # Safety
///
/// Called exactly once, from the boot assembly, in the correct CPU mode with a
/// valid stack. Not to be called from Rust.
#[no_mangle]
pub extern "C" fn cibios_entry() -> ! {
    run();
    arch::halt();
}

/// The firmware boot sequence.
///
/// Detects hardware, assembles and validates a hardware profile, and reports
/// readiness. The subsequent step — locating the CIBOS image on boot media,
/// then [`boot_image`] — is reached once an image source is wired for the
/// target (a multiboot module on x86_64, or a known load address / storage
/// driver on the others).
fn run() {
    kprintln!("CIBIOS v{} starting", env!("CARGO_PKG_VERSION"));

    let detect = match arch::detect() {
        Ok(d) => d,
        Err(e) => {
            kprintln!("[FATAL] hardware detection failed: {}", e);
            return;
        }
    };

    kprintln!(
        "detected: {} core(s), {} MiB RAM at {:#x}",
        detect.hardware.physical_cores,
        detect.memory_length / (1024 * 1024),
        detect.memory_base
    );

    let profile = match assemble_profile(&detect.hardware) {
        Ok(p) => p,
        Err(e) => {
            kprintln!("[FATAL] hardware profile assembly failed: {}", e);
            return;
        }
    };

    kprintln!(
        "profile: {:?} on {:?}, {} logical context(s), SMT {}",
        profile.architecture,
        profile.platform,
        profile.topology.logical_cores,
        if profile.topology.smt_enabled {
            "on"
        } else {
            "off"
        }
    );

    kprintln!(
        "firmware profile: {:?}",
        cibios::detection::firmware_profile()
    );

    // Acquire the CIBOS image for this target and boot it. On x86_64 the image
    // arrives as the first multiboot module (QEMU `-initrd`); other targets
    // will use a fixed load address or storage driver.
    match arch::locate_image() {
        Some(image) => {
            kprintln!("CIBOS image found ({} bytes); booting", image.len());
            // Standard firmware verifies the image against the compiled-in
            // SPHINCS+ root public key; Lightweight needs no key.
            boot_image(image, &detect, trusted_root_key());
            kprintln!("[FATAL] boot_image returned; halting");
        }
        None => {
            kprintln!("no CIBOS image source for this target; CIBIOS idle");
        }
    }
}

/// The SPHINCS+ root public key the Standard firmware verifies images against.
///
/// Compiled into the firmware image from `keys/trusted_root.pub` so the trust
/// anchor travels with the firmware and cannot be supplied at runtime. The
/// Lightweight firmware does not verify signatures and uses an empty key.
#[cfg(feature = "handoff-cryptographic")]
fn trusted_root_key() -> &'static [u8] {
    // 32-byte SPHINCS+ (sphincs-sha2-128f-simple) public key.
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../keys/trusted_root.pub"))
}

/// Lightweight firmware performs no signature verification, so there is no
/// trusted key to supply.
#[cfg(not(feature = "handoff-cryptographic"))]
fn trusted_root_key() -> &'static [u8] {
    &[]
}

/// Verify a CIBOS image, place its components, build the handoff, and transfer
/// control to the kernel.
///
/// Reached once the image bytes have been located for the target. Returns only
/// on failure (a successful call jumps to the kernel and never returns).
fn boot_image(
    image: &[u8],
    detect: &arch::DetectResult,
    trusted_root_key: &[u8],
) {
    use cibios::handoff::build_handoff;
    use shared::{MemoryRegion, MemoryRegionKind};

    let profile = match assemble_profile(&detect.hardware) {
        Ok(p) => p,
        Err(e) => {
            kprintln!("[FATAL] profile assembly failed: {}", e);
            return;
        }
    };

    let require_signature = cibios::detection::firmware_profile()
        == shared::CibiosProfile::Standard;
    let policy = VerificationPolicy {
        require_signature,
        running_architecture: detect.hardware.architecture.as_u32(),
    };

    let verified = match verify_image(image, &policy, trusted_root_key) {
        Ok(v) => v,
        Err(e) => {
            kprintln!("[FATAL] image verification failed: {}", e);
            return;
        }
    };
    kprintln!(
        "image verified (signature {}), entry {:#x}",
        if verified.signature_verified {
            "checked"
        } else {
            "skipped"
        },
        verified.entry_point
    );

    // Place each component at its load address before constructing the handoff.
    let view = match ImageView::parse(image) {
        Ok(v) => v,
        Err(e) => {
            kprintln!("[FATAL] image parse failed: {}", e);
            return;
        }
    };
    let placement = view.for_each_component(|desc, body| {
        // SAFETY: load_addr lies in identity-mapped RAM established at boot;
        // copying the verified body there stages the component for execution.
        unsafe {
            core::ptr::copy_nonoverlapping(
                body.as_ptr(),
                desc.load_addr as *mut u8,
                body.len(),
            );
        }
        Ok(())
    });
    if let Err(e) = placement {
        kprintln!("[FATAL] component placement failed: {}", e);
        return;
    }
    kprintln!("components placed");

    // FLAG: entropy seeding. A real Standard-profile boot fills this from the
    // hardware RNG; zeroed here until per-arch RNG reads are wired.
    // Gather entropy from the platform for the kernel CSPRNG seed. Replaces the
    // previously-zeroed seed; see `arch::gather_entropy` for source quality.
    let entropy_seed = arch::gather_entropy();

    let regions = [MemoryRegion {
        base: detect.memory_base,
        length: detect.memory_length,
        kind: MemoryRegionKind::Usable,
    }];

    let (handoff, _decoded) =
        match build_handoff(&profile, &verified, &regions, entropy_seed) {
            Ok(h) => h,
            Err(e) => {
                kprintln!("[FATAL] handoff construction failed: {}", e);
                return;
            }
        };

    kprintln!("handoff built; transferring control to CIBOS");
    let handoff_ptr = core::ptr::addr_of!(handoff) as u64;
    unsafe {
        arch::jump_to_kernel(verified.entry_point, handoff_ptr);
    }
}

/// Panic handler for the firmware binary: report through the serial console and
/// halt. There is no unwinding on bare metal.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kprintln!("[PANIC] {}", info);
    arch::halt()
}
