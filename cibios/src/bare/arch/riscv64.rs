//! RISC-V 64 architecture support: SBI console output, device-tree-based
//! detection, and the jump to the kernel. Targets the QEMU `virt` machine in
//! S-mode under OpenSBI.

use super::{DetectOutcome, DetectResult};
use cibios::detection::DetectedHardware;
use cibios::error::FirmwareError;
use cibios::fdt;
use core::arch::asm;
use shared::types::hardware::{
    InputCapabilities, NetworkCapabilities, SecurityCapabilities, SensorCapabilities,
};
use shared::{HardwarePlatform, ProcessorArchitecture};

extern "C" {
    /// Device tree blob pointer, saved by the boot entry from `a1`.
    static dtb_ptr: u64;
}

/// SBI legacy `console_putchar` (extension id 0x01). Available in S-mode under
/// OpenSBI, so it needs no knowledge of the UART's MMIO address.
fn sbi_putchar(ch: u8) {
    unsafe {
        asm!(
            "ecall",
            in("a7") 1usize,        // EID = 0x01 (legacy console putchar)
            in("a0") ch as usize,
            lateout("a0") _,
            options(nomem, nostack),
        );
    }
}

/// Write one byte to the SBI console.
pub fn putc(b: u8) {
    sbi_putchar(b);
}

/// Halt the processor permanently.
pub fn halt() -> ! {
    loop {
        unsafe {
            asm!("wfi", options(nomem, nostack));
        }
    }
}

/// Detect hardware from the device tree the platform passed in `a1`.
pub fn detect() -> DetectOutcome {
    let info = parse_device_tree()?;
    let cores = info.cpu_count.max(1);

    let hardware = DetectedHardware {
        architecture: ProcessorArchitecture::RiscV64,
        // FLAG: platform class heuristic for the virt machine.
        platform: HardwarePlatform::Embedded,
        physical_cores: cores,
        logical_cores: cores,
        total_memory: info.total_memory,
        security_bits: SecurityCapabilities::empty().bits(),
        input_bits: InputCapabilities::empty().bits(),
        sensor_bits: SensorCapabilities::empty().bits(),
        network_bits: NetworkCapabilities::empty().bits(),
    };

    Ok(DetectResult {
        hardware,
        memory_base: info.memory_base,
        memory_length: info.total_memory,
    })
}

fn parse_device_tree() -> Result<fdt::DeviceTreeInfo, FirmwareError> {
    let addr = unsafe { core::ptr::read(core::ptr::addr_of!(dtb_ptr)) };
    if addr == 0 {
        return Err(FirmwareError::BootFailure {
            phase: "device tree pointer missing",
        });
    }
    let base = addr as *const u8;
    let totalsize = unsafe {
        u32::from_be_bytes([
            core::ptr::read(base.add(4)),
            core::ptr::read(base.add(5)),
            core::ptr::read(base.add(6)),
            core::ptr::read(base.add(7)),
        ])
    } as usize;
    if totalsize == 0 || totalsize > 0x10_0000 {
        return Err(FirmwareError::MalformedImage {
            detail: "implausible device tree size",
        });
    }
    let dtb = unsafe { core::slice::from_raw_parts(base, totalsize) };
    fdt::parse(dtb)
}

/// Transfer control to the kernel at `entry`, passing the handoff structure
/// pointer in `a0`.
///
/// # Safety
///
/// `entry` must be the verified kernel entry and `handoff_ptr` a valid handoff
/// structure. Never returns.
/// Locate the CIBOS image for this target.
///
/// FLAG: image acquisition on this architecture is not yet wired (a fixed load
/// address or a storage driver). Returns `None` until then; the x86_64 path
/// (multiboot module) is the reference implementation.
/// Gather entropy for the kernel CSPRNG seed.
///
/// Reads the `time` CSR via `rdtime` repeatedly with loop-induced jitter between samples and
/// mixes them. NOTE: the QEMU `virt` machine for this architecture exposes no
/// hardware RNG, so this is counter jitter only — adequate for development, not
/// cryptographic-grade. A production Standard-profile boot must supply a TRNG.
pub fn gather_entropy() -> [u8; 32] {
    let mut samples = [0u64; 16];
    for (i, slot) in samples.iter_mut().enumerate() {
        let t = read_counter();
        *slot = t;
        let mut acc = 0u64;
        for k in 0..((t & 0x3f) + i as u64) {
            acc = acc.wrapping_add(k ^ t);
        }
        core::hint::black_box(acc);
    }
    cibios::entropy::mix_entropy(&samples)
}

/// Read the architecture's free-running counter.
fn read_counter() -> u64 {
    let v: u64;
    unsafe {
        asm!("rdtime {v}", v = out(reg) v, options(nomem, nostack));
    }
    v
}

pub fn locate_image() -> Option<&'static [u8]> {
    // SAFETY: the boot entry saved the DTB pointer the platform passed in a
    // register. A null pointer means no device tree (and so no initrd).
    let dtb_addr = unsafe { core::ptr::read(core::ptr::addr_of!(dtb_ptr)) };
    if dtb_addr == 0 {
        return None;
    }
    // Read the FDT header (40 bytes) to learn the total blob size.
    // SAFETY: the platform provides a valid device tree at this address.
    let header = unsafe { core::slice::from_raw_parts(dtb_addr as *const u8, 40) };
    let totalsize = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if totalsize < 40 {
        return None;
    }
    // SAFETY: the device tree occupies `totalsize` bytes from its base.
    let dtb = unsafe { core::slice::from_raw_parts(dtb_addr as *const u8, totalsize) };
    let info = cibios::fdt::parse(dtb).ok()?;
    let (start, end) = info.initrd()?;
    let len = (end - start) as usize;
    // SAFETY: the loader placed the initrd (the CIBOS image) in `[start, end)`.
    let bytes = unsafe { core::slice::from_raw_parts(start as *const u8, len) };
    Some(bytes)
}

pub unsafe fn jump_to_kernel(entry: u64, handoff_ptr: u64) -> ! {
    // The kernel's `_start` expects the handoff pointer in a0 (RISC-V arg0).
    // Pin it to a0 explicitly and let the jump register be any other register,
    // so the allocator cannot alias `{entry}` onto a0 and have the handoff load
    // clobber the jump target (see the x86_64 note for the failure this
    // prevents).
    asm!(
        "jr {entry}",
        entry = in(reg) entry,
        in("a0") handoff_ptr,
        options(noreturn),
    );
}
