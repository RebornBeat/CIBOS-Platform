//! x86_64 architecture support: COM1 serial, CPUID-based detection, multiboot
//! memory map, and the jump to the kernel.

use super::{DetectOutcome, DetectResult};
use cibios::detection::DetectedHardware;
use cibios::error::FirmwareError;
#[cfg(feature = "firmware-multiboot")]
use cibios::multiboot;
use core::arch::asm;
use shared::types::hardware::{
    InputCapabilities, NetworkCapabilities, SecurityCapabilities, SensorCapabilities,
};
use shared::{HardwarePlatform, ProcessorArchitecture};

const COM1: u16 = 0x3F8;

#[cfg(feature = "firmware-multiboot")]
extern "C" {
    /// Multiboot information structure pointer, saved by the boot entry from
    /// `ebx`. Zero if not booted via multiboot.
    static multiboot_info_ptr: u64;
}

#[cfg(feature = "firmware-bootloader")]
extern "C" {
    /// `BootHandoff` pointer, saved by the bootloader boot entry from `rdi`.
    /// Zero only on a contract violation (the bootloader always sets it).
    static boot_handoff_ptr: u64;
}

unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

/// Initialize COM1 to 38400 8N1. Idempotent enough to call once at entry.
fn init_serial() {
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // enable DLAB
        outb(COM1, 0x03); // divisor low (38400)
        outb(COM1 + 1, 0x00); // divisor high
        outb(COM1 + 3, 0x03); // 8 bits, no parity, 1 stop
        outb(COM1 + 2, 0xC7); // enable + clear FIFO, 14-byte threshold
        outb(COM1 + 4, 0x0B); // RTS/DSR set
    }
}

/// Write one byte to COM1, waiting for the transmit holding register to empty.
pub fn putc(b: u8) {
    unsafe {
        // Line Status Register bit 5 (0x20) = transmit holding register empty.
        while inb(COM1 + 5) & 0x20 == 0 {}
        outb(COM1, b);
    }
}

/// Halt the processor permanently.
pub fn halt() -> ! {
    loop {
        unsafe {
            asm!("cli; hlt", options(nomem, nostack));
        }
    }
}

/// Locate the CIBOS image passed as the first multiboot module.
///
/// QEMU `-initrd <image>` (or any multiboot loader) places the image in memory
/// and records its range in the module table. We read the saved info pointer,
/// find the module table, and return the first module's bytes.
///
/// Returns `None` if no module was supplied.
#[cfg(feature = "firmware-multiboot")]
pub fn locate_image() -> Option<&'static [u8]> {
    // SAFETY: `multiboot_info_ptr` was saved by the boot entry from `ebx`.
    let info_ptr = unsafe { multiboot_info_ptr };
    if info_ptr == 0 {
        return None;
    }
    // SAFETY: the loader provides a valid info structure; 28 bytes covers the
    // flags and module-table fields.
    let header = unsafe { core::slice::from_raw_parts(info_ptr as *const u8, 28) };
    let (count, mods_addr) = match cibios::multiboot::module_table(header) {
        Ok(Some(v)) => v,
        _ => return None,
    };
    if count == 0 || mods_addr == 0 {
        return None;
    }
    // SAFETY: the module table entry is 16 bytes at `mods_addr`.
    let entry = unsafe { core::slice::from_raw_parts(mods_addr as *const u8, 16) };
    let module = match cibios::multiboot::parse_module_entry(entry) {
        Ok(m) => m,
        Err(_) => return None,
    };
    let len = module.len() as usize;
    // SAFETY: the loader placed the module body in `[start, end)`.
    let bytes = unsafe { core::slice::from_raw_parts(module.start as *const u8, len) };
    Some(bytes)
}

/// Locate the CIBOS image the from-scratch bootloader loaded.
///
/// The bootloader placed the `.cimg` blob in identity-mapped RAM and recorded
/// `(addr, size)` in the [`BootHandoff`] it passed in `rdi`. We validate the
/// handoff and return the blob bytes; CIBIOS then parses, verifies, and places
/// the image exactly as on any other path.
///
/// Returns `None` if the handoff is missing/invalid or carries no image.
#[cfg(feature = "firmware-bootloader")]
pub fn locate_image() -> Option<&'static [u8]> {
    let handoff = boot_handoff()?;
    if handoff.cibos_image_addr == 0 || handoff.cibos_image_size == 0 {
        return None;
    }
    // SAFETY: the bootloader loaded the `.cimg` into `[addr, addr+size)` in
    // identity-mapped RAM and left it there for firmware to read.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            handoff.cibos_image_addr as *const u8,
            handoff.cibos_image_size as usize,
        )
    };
    Some(bytes)
}

/// Read and validate the `BootHandoff` the bootloader passed in `rdi`.
#[cfg(feature = "firmware-bootloader")]
fn boot_handoff() -> Option<&'static shared::BootHandoff> {
    // SAFETY: the bootloader saved its `rdi` argument here; it points at a
    // valid, aligned `BootHandoff` in identity-mapped RAM that outlives boot.
    let ptr = unsafe { boot_handoff_ptr };
    if ptr == 0 {
        return None;
    }
    // SAFETY: `ptr` is a valid, aligned `BootHandoff` per the boot contract.
    let handoff = unsafe { &*(ptr as *const shared::BootHandoff) };
    if handoff.is_valid() {
        Some(handoff)
    } else {
        None
    }
}

/// Gather entropy for the kernel CSPRNG seed.
///
/// Mixes timestamp-counter samples (with loop-induced jitter between reads) and,
/// when the CPU advertises it, `RDRAND` outputs. On QEMU and modern hardware
/// `RDRAND` is present and contributes real entropy; the TSC jitter is the
/// portable fallback.
pub fn gather_entropy() -> [u8; 32] {
    let mut samples = [0u64; 20];
    let mut n = 0usize;

    // Timestamp-counter reads separated by a small, data-dependent busy loop to
    // introduce timing jitter between samples.
    for _ in 0..12 {
        let t = rdtsc();
        samples[n] = t;
        n += 1;
        let mut acc = 0u64;
        for i in 0..(t & 0x3f) {
            acc = acc.wrapping_add(i ^ t);
        }
        core::hint::black_box(acc);
    }

    // RDRAND if supported (CPUID.01h:ECX bit 30).
    let rdrand = (core::arch::x86_64::__cpuid(1).ecx & (1 << 30)) != 0;
    if rdrand {
        for _ in 0..8 {
            if n >= samples.len() {
                break;
            }
            if let Some(v) = rdrand64() {
                samples[n] = v;
                n += 1;
            }
        }
    }

    cibios::entropy::mix_entropy(&samples[..n])
}

/// Read the timestamp counter.
fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Read a 64-bit `RDRAND` value, or `None` if the hardware reported no random
/// data was ready.
fn rdrand64() -> Option<u64> {
    let value: u64;
    let ok: u8;
    unsafe {
        asm!(
            "rdrand {v}",
            "setc {ok}",
            v = out(reg) value,
            ok = out(reg_byte) ok,
            options(nomem, nostack),
        );
    }
    if ok != 0 {
        Some(value)
    } else {
        None
    }
}

/// Detect hardware via CPUID and the multiboot memory map.
pub fn detect() -> DetectOutcome {
    init_serial();

    // CPUID leaf 1: feature flags and logical processor count.
    let cpuid1 = core::arch::x86_64::__cpuid(1);
    let logical = ((cpuid1.ebx >> 16) & 0xff).max(1);
    let rdrand = (cpuid1.ecx & (1 << 30)) != 0;

    let mut security = SecurityCapabilities::empty();
    if rdrand {
        security |= SecurityCapabilities::HARDWARE_RNG;
    }

    // Primary usable RAM: from the multiboot map or the BootHandoff E820 map,
    // depending on the boot source.
    let (memory_base, total_memory) = read_memory_map()?;

    let hardware = DetectedHardware {
        architecture: ProcessorArchitecture::X86_64,
        // FLAG: platform class is a heuristic. x86_64 firmware assumes a
        // desktop/server-class machine; refine if targeting a specific device.
        platform: HardwarePlatform::Desktop,
        physical_cores: logical,
        logical_cores: logical,
        total_memory,
        security_bits: security.bits(),
        input_bits: InputCapabilities::KEYBOARD.bits() | InputCapabilities::POINTER.bits(),
        sensor_bits: SensorCapabilities::empty().bits(),
        network_bits: NetworkCapabilities::ETHERNET.bits(),
    };

    Ok(DetectResult {
        hardware,
        memory_base,
        memory_length: total_memory,
    })
}

/// Read the primary usable-RAM `(base, total)` from the multiboot information
/// structure pointed to by `multiboot_info_ptr`.
#[cfg(feature = "firmware-multiboot")]
fn read_memory_map() -> Result<(u64, u64), FirmwareError> {
    let info_addr = unsafe { core::ptr::read(core::ptr::addr_of!(multiboot_info_ptr)) };
    if info_addr == 0 {
        return Err(FirmwareError::BootFailure {
            phase: "multiboot info pointer missing",
        });
    }
    // The multiboot info structure is in identity-mapped low memory.
    let info = info_addr as *const u8;
    let flags = unsafe { read_u32_le(info) };
    // Bit 6 indicates mmap_length/mmap_addr are valid.
    if flags & (1 << 6) == 0 {
        return Err(FirmwareError::BootFailure {
            phase: "multiboot memory map unavailable",
        });
    }
    let mmap_length = unsafe { read_u32_le(info.add(44)) } as usize;
    let mmap_addr = unsafe { read_u32_le(info.add(48)) } as usize;
    let mmap = unsafe { core::slice::from_raw_parts(mmap_addr as *const u8, mmap_length) };
    let mem = multiboot::parse_memory_map(mmap)?;
    Ok((mem.memory_base, mem.total_memory))
}

#[cfg(feature = "firmware-multiboot")]
unsafe fn read_u32_le(p: *const u8) -> u32 {
    u32::from_le_bytes([
        core::ptr::read(p),
        core::ptr::read(p.add(1)),
        core::ptr::read(p.add(2)),
        core::ptr::read(p.add(3)),
    ])
}

/// Read the primary usable-RAM `(base, total)` from the [`BootHandoff`] E820
/// map the bootloader passed.
///
/// Uses the same semantics as the multiboot parser: `total` is the sum of all
/// usable regions, and `base` is the base of the first usable region at or
/// above 1 MiB (skipping the low-memory hole).
#[cfg(feature = "firmware-bootloader")]
fn read_memory_map() -> Result<(u64, u64), FirmwareError> {
    let handoff = boot_handoff().ok_or(FirmwareError::BootFailure {
        phase: "boot handoff missing or invalid",
    })?;
    if handoff.memory_regions_ptr == 0 || handoff.memory_region_count == 0 {
        return Err(FirmwareError::BootFailure {
            phase: "boot handoff memory map empty",
        });
    }
    // SAFETY: the bootloader left `memory_region_count` `BootMemoryRegion`
    // values at `memory_regions_ptr` in identity-mapped RAM.
    let regions = unsafe {
        core::slice::from_raw_parts(
            handoff.memory_regions_ptr as *const shared::BootMemoryRegion,
            handoff.memory_region_count as usize,
        )
    };

    let mut total: u64 = 0;
    let mut base: u64 = 0;
    let mut base_set = false;
    for region in regions {
        if region.is_usable() {
            total = total.saturating_add(region.length);
            if !base_set && region.base >= 0x10_0000 {
                base = region.base;
                base_set = true;
            }
        }
    }
    if !base_set {
        return Err(FirmwareError::BootFailure {
            phase: "boot handoff has no usable RAM above 1 MiB",
        });
    }
    Ok((base, total))
}

/// Transfer control to the loaded kernel at `entry`, passing the physical
/// address of the handoff structure in `rdi` (System V first argument).
///
/// # Safety
///
/// `entry` must be the verified kernel entry point and `handoff_ptr` must point
/// to a valid handoff structure that outlives the call. Never returns.
pub unsafe fn jump_to_kernel(entry: u64, handoff_ptr: u64) -> ! {
    // The kernel's `_start` expects the handoff pointer in RDI (System V arg0).
    // Pin `handoff_ptr` to RDI with an explicit register constraint and let the
    // jump target use any *other* register. A previous version wrote
    // `mov rdi, {handoff}` with `{entry}` as a generic `reg` operand; the
    // allocator was free to place `{entry}` in RDI too, so the `mov` clobbered
    // the entry address and `jmp {entry}` jumped to the handoff pointer (a stack
    // address) instead of the kernel — triple-faulting. Constraining the
    // registers explicitly removes that aliasing.
    asm!(
        "jmp {entry}",
        entry = in(reg) entry,
        in("rdi") handoff_ptr,
        options(noreturn),
    );
}
