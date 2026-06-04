//! Bare-metal kernel entry, global allocator, panic handler, and boot
//! orchestration.
//!
//! Compiled only for `target_os = "none"`. The architecture boot assembly sets
//! up a stack, clears BSS, and calls [`kernel_entry`] with the handoff pointer
//! in the first-argument register (`rdi`/`x0`/`a0`) — exactly where CIBIOS's
//! `jump_to_kernel` leaves it. The entry brings up the heap, obtains the
//! handoff, boots [`Kernel::from_handoff`], runs an initial system lane, and
//! drives the scheduler to idle, reporting each step over the serial console.

use crate::arch;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::global_asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr::NonNull;
use linked_list_allocator::Heap;

use cibos_kernel::sync::SpinLock;
use cibos_kernel::Kernel;
use shared::protocols::handoff::HandoffData;
#[cfg(feature = "self-boot")]
use shared::protocols::handoff::ENTROPY_SEED_LEN;
use shared::WeightClass;

// Architecture boot entry. Each defines `_start`, sets up the stack, clears
// BSS, and calls `kernel_entry`, preserving the handoff pointer argument.
#[cfg(all(target_arch = "x86_64", feature = "self-boot"))]
global_asm!(include_str!("boot/x86_64_selfboot.s"));
#[cfg(all(target_arch = "x86_64", not(feature = "self-boot")))]
global_asm!(include_str!("boot/x86_64_handoff.s"));
#[cfg(target_arch = "aarch64")]
global_asm!(include_str!("boot/aarch64.s"));
#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("boot/riscv64.s"));

// ---------------------------------------------------------------------------
// Global allocator: a linked-list heap over a static region, guarded by the
// kernel's spinlock.
// ---------------------------------------------------------------------------

/// Size of the kernel's initial heap (8 MiB). Lives in BSS, zeroed at boot.
const HEAP_SIZE: usize = 8 * 1024 * 1024;
static mut KERNEL_HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

struct LockedHeap(SpinLock<Heap>);

// SAFETY: all access to the inner heap is serialized by the spinlock.
unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0
            .lock()
            .allocate_first_fit(layout)
            .map_or(core::ptr::null_mut(), |p| p.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(nn) = NonNull::new(ptr) {
            self.0.lock().deallocate(nn, layout);
        }
    }
}

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap(SpinLock::new(Heap::empty()));

/// Initialize the global heap from the static region. Called once, before any
/// allocation.
fn init_heap() {
    // SAFETY: called exactly once at boot, before any allocation; the region is
    // a unique static of HEAP_SIZE bytes.
    unsafe {
        let start = core::ptr::addr_of_mut!(KERNEL_HEAP) as *mut u8;
        ALLOCATOR.0.lock().init(start, HEAP_SIZE);
    }
}

// ---------------------------------------------------------------------------
// Serial console.
// ---------------------------------------------------------------------------

struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            arch::putc(byte);
        }
        Ok(())
    }
}

macro_rules! kprintln {
    ($($arg:tt)*) => {{
        let mut console = $crate::boot::Console;
        let _ = ::core::writeln!(console, $($arg)*);
    }};
}

// ---------------------------------------------------------------------------
// Entry and boot sequence.
// ---------------------------------------------------------------------------

/// The Rust entry point, called by the architecture boot assembly with the
/// handoff pointer in the first-argument register.
///
/// # Safety
///
/// Called exactly once from the boot assembly, in the correct CPU mode, with a
/// valid stack. `handoff_ptr` is either a valid, aligned [`HandoffData`] (the
/// CIBIOS path) or ignored (the `self-boot` path).
#[no_mangle]
pub extern "C" fn kernel_entry(handoff_ptr: u64) -> ! {
    arch::init_serial();
    kprintln!("CIBOS kernel: entry");

    init_heap();
    kprintln!("CIBOS kernel: heap online ({} bytes)", HEAP_SIZE);

    let handoff = obtain_handoff(handoff_ptr);

    // Kernel-side self-enforcing profile check (ADR-007): a binary compiled as
    // one operational profile refuses a handoff claiming another. Defense in
    // depth on top of the firmware's pairing check. Skipped when the image was
    // built without a profile bundle (host tooling / generic QEMU self-boot).
    if let Some(compiled) = cibos_kernel::compiled_profile() {
        if handoff.cibos_profile != compiled.as_u32() {
            kprintln!(
                "CIBOS kernel: profile mismatch — built as {:?} but handoff claims \
                 profile id {} — halting",
                compiled,
                handoff.cibos_profile
            );
            arch::halt();
        }
    }

    match Kernel::from_handoff(&handoff, 256) {
        Ok(mut kernel) => {
            kprintln!(
                "CIBOS kernel: handoff accepted, {} bytes usable across {} region(s)",
                kernel.memory().total_usable(),
                kernel.memory().region_count()
            );

            // Spawn an initial system lane that proves the scheduler runs.
            let _ = kernel.spawn(WeightClass::System, async {
                kprintln!("CIBOS kernel: init lane running");
            });

            let polls = kernel.run_until_idle();
            kprintln!("CIBOS kernel: scheduler idle after {polls} poll(s)");
            kprintln!("CIBOS kernel: boot complete");
        }
        Err(_) => {
            kprintln!("CIBOS kernel: handoff REJECTED — halting");
        }
    }

    arch::halt();
}

/// Obtain the handoff record. Under `self-boot` we synthesize one (for
/// standalone QEMU); otherwise we read the one CIBIOS placed at `ptr`.
#[cfg(feature = "self-boot")]
fn obtain_handoff(_ptr: u64) -> HandoffData {
    synth_handoff()
}

/// Read the handoff CIBIOS passed by pointer.
///
/// SAFETY: on the CIBIOS boot path, firmware placed a valid, aligned
/// `HandoffData` at this address and left it mapped.
#[cfg(not(feature = "self-boot"))]
fn obtain_handoff(ptr: u64) -> HandoffData {
    unsafe { core::ptr::read(ptr as *const HandoffData) }
}

/// Synthesize a minimal, valid handoff for standalone QEMU boot. Values are
/// nominal for the memory accounting; the heap is the static region above, not
/// this map.
#[cfg(feature = "self-boot")]
fn synth_handoff() -> HandoffData {
    use shared::{
        CibiosProfile, CibosProfile, CoreTopology, HandoffMode, HardwarePlatform, MemoryRegion,
        MemoryRegionKind, ProcessorArchitecture,
    };

    let topology = CoreTopology::new(1, 1, false).expect("valid topology");
    let regions = [MemoryRegion {
        base: 0x0010_0000,
        length: 0x0800_0000, // 128 MiB nominal
        kind: MemoryRegionKind::Usable,
    }];

    // Match the synthesized handoff to the compiled profile so a self-boot image
    // built for a specific profile passes its own boot-time profile check. With
    // no profile bundle compiled in, default to Balanced/Standard.
    let cibos_profile = cibos_kernel::compiled_profile().unwrap_or(CibosProfile::Balanced);
    let (cibios_profile, mode) = match cibos_profile {
        CibosProfile::Compute => (CibiosProfile::Lightweight, HandoffMode::Lightweight),
        _ => (CibiosProfile::Standard, HandoffMode::Cryptographic),
    };

    HandoffData::new(
        ProcessorArchitecture::current().expect("supported architecture"),
        HardwarePlatform::Desktop,
        cibios_profile,
        cibos_profile,
        mode,
        topology,
        0x0800_0000,
        &regions,
        [0x42u8; ENTROPY_SEED_LEN],
    )
    .expect("synthesized handoff is valid")
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("CIBOS kernel PANIC: {info}");
    arch::halt();
}
