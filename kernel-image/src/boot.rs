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
#[cfg(target_arch = "x86_64")]
global_asm!(include_str!("arch/syscall_entry.s"));
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

            // Bring up hardware-enforced isolation: build the kernel's own page
            // tables through the portable model + arch encoder and install them.
            // Surviving the CR3 switch proves the tables we built are valid
            // hardware tables, not just accounting.
            bring_up_mmu(&handoff);

            // Demonstrate the syscall transport: install the IDT and issue a
            // `log` syscall through the `int 0x80` trap gate, proving the
            // user→kernel→user round trip works on real hardware.
            demonstrate_syscall();

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

/// Build the kernel's own page tables from the handoff memory map and install
/// them, replacing the bootloader's identity map with tables we constructed via
/// the portable [`cibos_kernel::paging`] model and the architecture encoder.
///
/// This is the runtime proof of hardware-enforced isolation: we identity-map the
/// low physical memory the kernel is currently using (its image, heap, stack,
/// and the VGA buffer), switch `CR3` to our root, and keep executing. If the
/// tables were malformed the next instruction fetch would triple-fault; reaching
/// the line after the switch means the model produces valid hardware tables.
///
/// Per-boundary address spaces build directly on this: each container gets its
/// own [`cibos_kernel::AddressSpace`] with only its pages mapped, which is the
/// next step now that the mechanism is proven.
#[cfg(target_arch = "x86_64")]
fn bring_up_mmu(handoff: &HandoffData) {
    use cibos_kernel::paging::{AddressSpace, Permissions};
    use cibos_kernel::{FrameAllocator, FRAME_SIZE};
    use alloc::vec::Vec;
    use shared::MemoryRegion;

    // Reserve all physical memory below this watermark from the frame allocator:
    // it covers the firmware's low memory, the kernel image (loaded at 16 MiB),
    // its 8 MiB BSS heap, and the stack. Page-table frames are drawn from above
    // it, so building the tables cannot clobber anything in use. 64 MiB is a
    // generous bound for the current image; the 128 MiB guest leaves room.
    const RESERVED_BELOW: u64 = 64 * 1024 * 1024;
    // Identity-map this much physical address space (covers everything the
    // kernel touches in the 128 MiB guest, plus the VGA buffer at 0xB8000).
    const IDENTITY_MAP_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB

    // Collect the usable memory map the same way the kernel core does.
    let regions: Vec<MemoryRegion> = match handoff.typed_regions() {
        Ok(iter) => match iter.collect::<Result<Vec<_>, _>>() {
            Ok(v) => v,
            Err(e) => {
                kprintln!("CIBOS kernel: MMU bring-up failed (regions): {e}");
                return;
            }
        },
        Err(e) => {
            kprintln!("CIBOS kernel: MMU bring-up failed (regions): {e}");
            return;
        }
    };
    let frames = FrameAllocator::from_regions(&regions, RESERVED_BELOW);
    kprintln!(
        "CIBOS kernel: frame allocator: {} usable frame(s), {} free above {:#x}",
        frames.usable_frames(),
        frames.free_frames(),
        RESERVED_BELOW
    );

    // The bootloader installed a 0..4 GiB identity map, so physical address P is
    // currently readable/writable at virtual address P: identity is the map.
    let phys_to_ptr = |phys: u64| phys as *mut u8;

    // SAFETY: the identity map above is valid for every frame the allocator
    // hands out (all within mapped physical RAM), and we install the result only
    // after fully mapping the memory the kernel is currently executing from.
    unsafe {
        let space = match AddressSpace::new(&frames, &phys_to_ptr) {
            Ok(s) => s,
            Err(e) => {
                kprintln!("CIBOS kernel: MMU bring-up failed (root alloc): {e}");
                return;
            }
        };

        let pages = IDENTITY_MAP_BYTES / FRAME_SIZE;
        // Kernel-rwx identity map: the kernel runs in supervisor mode, so these
        // are kernel (non-user) pages. Per-boundary user spaces will map user
        // pages with restricted permissions on top of this mechanism.
        if let Err(e) = space.map_range::<crate::arch::paging::X86PageTable>(
            0,
            0,
            pages,
            Permissions {
                read: true,
                write: true,
                execute: true,
                user: false,
            },
            &frames,
            &phys_to_ptr,
        ) {
            kprintln!("CIBOS kernel: MMU bring-up failed (identity map): {e}");
            return;
        }

        kprintln!(
            "CIBOS kernel: page tables built (identity-mapped {} MiB), installing CR3 {:#x}",
            IDENTITY_MAP_BYTES / (1024 * 1024),
            space.root().addr()
        );

        // The moment of truth: switch to our tables. Execution continuing past
        // this call is the proof that the tables are valid hardware tables.
        crate::arch::paging::install(space.root());

        kprintln!(
            "CIBOS kernel: MMU online — running on kernel-built page tables (CR3 {:#x})",
            crate::arch::paging::current_root()
        );

        // Demonstrate per-container isolation on the proven mechanism: hand the
        // remaining frames to an AddressSpaceManager, give two distinct
        // boundaries their own address spaces, map a page into the first, and
        // confirm that same virtual address is absent in the second. This is the
        // hardware-isolation property — "Container A cannot reach Container B's
        // memory" — exercised on real page tables at boot.
        demonstrate_container_isolation(frames);

        // Intentionally leak the address space: it is the kernel's live page
        // table for the rest of this boot. (No teardown path here by design.)
        core::mem::forget(space);
    }
}

/// Build two per-boundary address spaces and prove they are isolated, on the
/// live MMU. Diagnostic/bring-up demonstration of the [`AddressSpaceManager`].
#[cfg(target_arch = "x86_64")]
fn demonstrate_container_isolation(frames: cibos_kernel::FrameAllocator) {
    use cibos_kernel::paging::Permissions;
    use cibos_kernel::AddressSpaceManager;
    use shared::BoundaryId;

    let phys_to_ptr = |phys: u64| phys as *mut u8;
    let mgr = AddressSpaceManager::new(frames);

    let a = BoundaryId::new(1);
    let b = BoundaryId::new(2);
    // A user virtual address well clear of the kernel's identity-mapped range.
    const USER_VIRT: u64 = 0x0000_4000_0000_0000; // 64 TiB

    // SAFETY: the identity map installed above is valid for every frame the
    // allocator hands out; these spaces are built but not installed (the kernel
    // keeps running on its own space), so this only reads/writes table frames.
    unsafe {
        if let Err(e) = mgr.create_space(a, &phys_to_ptr) {
            kprintln!("CIBOS kernel: isolation demo skipped (space A): {e}");
            return;
        }
        if let Err(e) = mgr.create_space(b, &phys_to_ptr) {
            kprintln!("CIBOS kernel: isolation demo skipped (space B): {e}");
            return;
        }
        if let Err(e) = mgr.map_new_pages::<crate::arch::paging::X86PageTable>(
            a,
            USER_VIRT,
            1,
            Permissions::user_rw(),
            &phys_to_ptr,
        ) {
            kprintln!("CIBOS kernel: isolation demo map failed: {e}");
            return;
        }

        let in_a = mgr
            .translate::<crate::arch::paging::X86PageTable>(a, USER_VIRT, &phys_to_ptr)
            .is_some();
        let in_b = mgr
            .translate::<crate::arch::paging::X86PageTable>(b, USER_VIRT, &phys_to_ptr)
            .is_some();

        match (in_a, in_b) {
            (true, false) => kprintln!(
                "CIBOS kernel: container isolation verified — page mapped in boundary {} \
                 is absent in boundary {} (separate page tables, roots {:#x} / {:#x})",
                a.raw(),
                b.raw(),
                mgr.root_of(a).map(|f| f.addr()).unwrap_or(0),
                mgr.root_of(b).map(|f| f.addr()).unwrap_or(0),
            ),
            other => kprintln!(
                "CIBOS kernel: container isolation CHECK FAILED (in_a, in_b) = {other:?}"
            ),
        }
    }

    // Keep the manager alive for the rest of boot.
    core::mem::forget(mgr);
}

/// On non-x86_64 targets the page-table encoder and CR3 install are not yet
/// implemented, so MMU bring-up is a no-op (the bootloader/firmware identity map
/// stays active). The portable model is identical; only the arch encoder differs.
#[cfg(not(target_arch = "x86_64"))]
fn bring_up_mmu(_handoff: &HandoffData) {
    kprintln!("CIBOS kernel: MMU bring-up skipped (arch backend pending)");
}

/// Install the IDT and exercise the syscall trap path: issue a `log` syscall
/// via `int 0x80` and confirm it returns success. Bring-up demonstration of the
/// syscall transport.
#[cfg(target_arch = "x86_64")]
fn demonstrate_syscall() {
    use core::arch::asm;
    use shared::protocols::syscall::Syscall;

    // SAFETY: single-threaded bring-up; installs the kernel IDT once.
    unsafe {
        crate::arch::idt::init();
    }
    kprintln!("CIBOS kernel: IDT installed, syscall gate at vector 0x80");

    // Issue a `log` syscall the way an application would: number in rax, args in
    // rdi/rsi, `int 0x80`, result in rax.
    let msg = b"  [syscall] hello from a log() trap\n";
    let ret: i64;
    // SAFETY: the IDT is installed and the buffer is a valid kernel pointer; the
    // trap stub preserves all registers except rax.
    unsafe {
        asm!(
            "int 0x80",
            inout("rax") Syscall::Log.number() => ret,
            in("rdi") msg.as_ptr() as u64,
            in("rsi") msg.len() as u64,
            in("rdx") 0u64,
            clobber_abi("C"),
        );
    }
    if ret == 0 {
        kprintln!("CIBOS kernel: syscall transport verified — log() trap returned 0");
    } else {
        kprintln!("CIBOS kernel: syscall transport CHECK FAILED — log() returned {ret}");
    }
}

/// On non-x86_64 targets the syscall trap entry is not yet implemented.
#[cfg(not(target_arch = "x86_64"))]
fn demonstrate_syscall() {
    kprintln!("CIBOS kernel: syscall transport skipped (arch trap entry pending)");
}

/// The kernel's [`SyscallEnv`]: how the portable syscall dispatcher reaches
/// kernel services and the caller's memory.
///
/// In this first transport step the trap is issued by the kernel itself
/// (`int 0x80` from supervisor code), so a "user" pointer is a kernel-mapped
/// address and `copy_from_user` reads it directly after a basic non-null/length
/// sanity check. Once applications run in their own ring-3 address spaces,
/// `copy_from_user` will translate through the calling boundary's
/// [`cibos_kernel::AddressSpaceManager`] before copying — the dispatcher and ABI
/// are unchanged by that.
#[cfg(target_arch = "x86_64")]
struct KernelSyscallEnv;

#[cfg(target_arch = "x86_64")]
impl cibos_kernel::SyscallEnv for KernelSyscallEnv {
    fn copy_from_user(
        &self,
        _boundary: shared::BoundaryId,
        ptr: u64,
        len: usize,
        out: &mut [u8],
    ) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        if ptr == 0 {
            return Err(SyscallError::BadAddress);
        }
        if ptr.checked_add(len as u64).is_none() {
            return Err(SyscallError::InvalidArgument);
        }
        // SAFETY: in this step the caller is supervisor code and `ptr` is a
        // kernel-mapped address in the active identity map. The length is
        // bounded by the dispatcher (<= MAX_LOG_LEN) before we get here.
        unsafe {
            core::ptr::copy_nonoverlapping(ptr as *const u8, out.as_mut_ptr(), len);
        }
        Ok(())
    }

    fn console_write(&self, bytes: &[u8]) {
        for &b in bytes {
            crate::arch::putc(b);
        }
    }

    fn now_nanos(&self) -> u64 {
        // A real monotonic clock is wired with the timer subsystem; for now the
        // syscall reports zero rather than a fabricated value.
        0
    }
}

/// Bridge from the architecture trap stub to the portable dispatcher. Called by
/// `cibos_syscall_handler` with the ABI registers; returns the value for `rax`.
#[cfg(target_arch = "x86_64")]
pub fn handle_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    use cibos_kernel::{dispatch_syscall, SyscallOutcome, SyscallRequest};
    let req = SyscallRequest {
        number,
        arg0,
        arg1,
        arg2,
        // Until ring-3 boundaries issue traps, attribute syscalls to the system
        // boundary. The dispatcher does not rely on this for the current calls.
        boundary: shared::BoundaryId::SYSTEM,
    };
    match dispatch_syscall(&req, &KernelSyscallEnv) {
        SyscallOutcome::Return(v) => v,
        SyscallOutcome::Yield => 0,
        // No ring-3 task to tear down yet; report the exit code as the return.
        SyscallOutcome::Exit(code) => code as i64,
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("CIBOS kernel PANIC: {info}");
    arch::halt();
}
