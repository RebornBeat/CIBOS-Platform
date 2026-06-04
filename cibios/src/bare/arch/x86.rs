//! x86 (32-bit) architecture support — for legacy-BIOS, older hardware.
//!
//! Mirrors the x86_64 module's surface (COM1 serial, CPUID detection, multiboot
//! memory map, kernel jump) but stays in 32-bit protected mode and avoids
//! features absent on old CPUs: entropy relies on `RDTSC` jitter (present since
//! the Pentium), using `RDRAND` only when CPUID advertises it.

use super::{DetectOutcome, DetectResult};
use cibios::detection::DetectedHardware;
use cibios::error::FirmwareError;
use cibios::multiboot;
use core::arch::asm;
use shared::types::hardware::{
    InputCapabilities, NetworkCapabilities, SecurityCapabilities, SensorCapabilities,
};
use shared::{HardwarePlatform, ProcessorArchitecture};

const COM1: u16 = 0x3F8;

extern "C" {
    /// Multiboot information structure pointer, saved by the boot entry from
    /// `ebx` (32-bit). Zero if not booted via multiboot.
    static multiboot_info_ptr: u32;
}

unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

/// Initialize COM1 to 38400 8N1.
fn init_serial() {
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x03);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x0B);
    }
}

/// Write one byte to COM1, waiting for the transmit holding register to empty.
pub fn putc(b: u8) {
    unsafe {
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
pub fn locate_image() -> Option<&'static [u8]> {
    // SAFETY: saved by the boot entry from `ebx`.
    let info_ptr = unsafe { core::ptr::read(core::ptr::addr_of!(multiboot_info_ptr)) };
    if info_ptr == 0 {
        return None;
    }
    // SAFETY: the loader provides a valid info structure; 28 bytes covers the
    // flags and module-table fields.
    let header = unsafe { core::slice::from_raw_parts(info_ptr as *const u8, 28) };
    let (count, mods_addr) = match multiboot::module_table(header) {
        Ok(Some(v)) => v,
        _ => return None,
    };
    if count == 0 || mods_addr == 0 {
        return None;
    }
    // SAFETY: the module table entry is 16 bytes at `mods_addr`.
    let entry = unsafe { core::slice::from_raw_parts(mods_addr as *const u8, 16) };
    let module = match multiboot::parse_module_entry(entry) {
        Ok(m) => m,
        Err(_) => return None,
    };
    let len = module.len() as usize;
    // SAFETY: the loader placed the module body in `[start, end)`.
    let bytes = unsafe { core::slice::from_raw_parts(module.start as *const u8, len) };
    Some(bytes)
}

/// Gather entropy for the kernel CSPRNG seed: `RDTSC` jitter, plus `RDRAND` when
/// the CPU advertises it (rare on the old hardware this target serves).
pub fn gather_entropy() -> [u8; 32] {
    let mut samples = [0u64; 20];
    let mut n = 0usize;

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

    // RDRAND if supported (CPUID.01h:ECX bit 30). Absent on pre-Ivy-Bridge.
    let rdrand = (core::arch::x86::__cpuid(1).ecx & (1 << 30)) != 0;
    if rdrand {
        for _ in 0..8 {
            if n >= samples.len() {
                break;
            }
            if let Some(v) = rdrand32() {
                samples[n] = u64::from(v);
                n += 1;
            }
        }
    }

    cibios::entropy::mix_entropy(&samples[..n])
}

/// Read the timestamp counter (present since the Pentium).
fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Read a 32-bit `RDRAND` value, or `None` if no random data was ready.
fn rdrand32() -> Option<u32> {
    let value: u32;
    let ok: u8;
    unsafe {
        asm!(
            "rdrand {v:e}",
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

    let cpuid1 = core::arch::x86::__cpuid(1);
    let logical = ((cpuid1.ebx >> 16) & 0xff).max(1);
    let rdrand = (cpuid1.ecx & (1 << 30)) != 0;

    let mut security = SecurityCapabilities::empty();
    if rdrand {
        security |= SecurityCapabilities::HARDWARE_RNG;
    }

    let (memory_base, total_memory) = read_multiboot_memory()?;

    let hardware = DetectedHardware {
        architecture: ProcessorArchitecture::X86,
        // FLAG: 32-bit x86 firmware assumes an older desktop/laptop.
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

/// Read the multiboot memory map, returning `(base, total)`.
fn read_multiboot_memory() -> Result<(u64, u64), FirmwareError> {
    let info_addr = unsafe { core::ptr::read(core::ptr::addr_of!(multiboot_info_ptr)) };
    if info_addr == 0 {
        return Err(FirmwareError::BootFailure {
            phase: "multiboot info pointer missing",
        });
    }
    let info = info_addr as *const u8;
    let flags = unsafe { read_u32_le(info) };
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

unsafe fn read_u32_le(p: *const u8) -> u32 {
    u32::from_le_bytes([
        core::ptr::read(p),
        core::ptr::read(p.add(1)),
        core::ptr::read(p.add(2)),
        core::ptr::read(p.add(3)),
    ])
}

/// Transfer control to the loaded kernel at `entry`, passing the physical
/// address of the handoff structure in `eax`. (The 32-bit kernel entry reads
/// the handoff pointer from `eax`, mirroring the x86_64 `rdi` convention.)
///
/// # Safety
///
/// `entry` must be the verified kernel entry point and `handoff_ptr` must point
/// to a valid handoff structure that outlives the call. Never returns.
pub unsafe fn jump_to_kernel(entry: u64, handoff_ptr: u64) -> ! {
    asm!(
        "mov eax, {handoff}",
        "jmp {entry}",
        handoff = in(reg) handoff_ptr as u32,
        entry = in(reg) entry as u32,
        options(noreturn),
    );
}
