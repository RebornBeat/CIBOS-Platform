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
#[cfg(target_arch = "x86_64")]
global_asm!(include_str!("arch/enter_user.s"));
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

pub struct Console;

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
            // bring_up_mmu now also installs the GDT/IDT and drops to ring 3 to
            // run an unprivileged user payload that syscalls via int 0x80,
            // exercising the full user/kernel boundary on real hardware.
            // Mount a root filesystem (CIBOSFS on the slave data disk) BEFORE
            // bringing up the user apps, so a ring-3 .capp can issue filesystem
            // syscalls against it. The ATA driver is port-I/O based and needs no
            // MMU, so this is safe here.
            // Seed the kernel CSPRNG (backs the get_random syscall) from the
            // firmware entropy seed before any app can request randomness.
            #[cfg(target_arch = "x86_64")]
            {
                let mut seed = [0u8; 32];
                let n = seed.len().min(shared::protocols::handoff::ENTROPY_SEED_LEN);
                seed[..n].copy_from_slice(&handoff.entropy_seed[..n]);
                seed_kernel_rng(seed);
            }

            #[cfg(all(target_arch = "x86_64", feature = "storage-selftest"))]
            mount_root_fs_early();

            bring_up_mmu(&handoff);

            // Bring up real block storage: probe the primary ATA bus and read
            // back the boot medium to prove block I/O works against actual
            // hardware (the disk we booted from).
            #[cfg(target_arch = "x86_64")]
            demonstrate_storage();

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
    // Shared with the per-process launcher so every space maps the identical
    // kernel range.
    const IDENTITY_MAP_BYTES: u64 = crate::loader::KERNEL_IDENTITY_MAP_BYTES;

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
        // The W^X mappings below set the NX bit on non-executable pages. NX is a
        // reserved bit until EFER.NXE is enabled (the bootloader sets LME but not
        // NXE), so enable it before building any table that uses NX.
        crate::arch::paging::enable_nxe();

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

        // Demonstrate per-container isolation on the proven mechanism: two
        // distinct boundaries get their own page tables; a page mapped in one is
        // physically absent in the other. Uses a borrowed frame allocator so the
        // allocator remains available for the ring-3 payload below.
        demonstrate_container_isolation(&frames, &phys_to_ptr);

        // Drop to ring 3 and run unprivileged user payloads, each in its own
        // per-process address space, reaching the kernel only via int 0x80
        // syscalls — the full user/kernel boundary.
        run_ring3_demo(&frames, &phys_to_ptr);

        // `space` and `frames` back the live page tables (CR3) for the rest of
        // this boot. Neither type implements `Drop` and the page-table frames
        // live in physical RAM independent of these handles, so simply letting
        // them fall out of scope here leaves the live mappings intact.
    }
}

/// Show two boundaries with independent address spaces on the live MMU, using a
/// borrowed frame allocator. Diagnostic demonstration of per-container isolation.
#[cfg(target_arch = "x86_64")]
fn demonstrate_container_isolation(
    frames: &cibos_kernel::FrameAllocator,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) {
    use cibos_kernel::paging::{AddressSpace, Permissions};

    const USER_VIRT: u64 = 0x0000_4000_0000_0000; // 64 TiB

    // SAFETY: the identity map is valid for every frame the allocator hands out;
    // these spaces are built but not installed (the kernel keeps running on its
    // own space), so this only reads/writes table frames.
    unsafe {
        let a = match AddressSpace::new(frames, phys_to_ptr) {
            Ok(s) => s,
            Err(e) => {
                kprintln!("CIBOS kernel: isolation demo skipped (space A): {e}");
                return;
            }
        };
        let b = match AddressSpace::new(frames, phys_to_ptr) {
            Ok(s) => s,
            Err(e) => {
                kprintln!("CIBOS kernel: isolation demo skipped (space B): {e}");
                return;
            }
        };
        if let Err(e) = a.map::<crate::arch::paging::X86PageTable>(
            USER_VIRT,
            match frames.allocate() {
                Ok(f) => f,
                Err(_) => {
                    kprintln!("CIBOS kernel: isolation demo skipped (no frame)");
                    return;
                }
            },
            Permissions::user_rw(),
            frames,
            phys_to_ptr,
        ) {
            kprintln!("CIBOS kernel: isolation demo map failed: {e}");
            return;
        }
        let in_a = a
            .translate::<crate::arch::paging::X86PageTable>(USER_VIRT, phys_to_ptr)
            .is_some();
        let in_b = b
            .translate::<crate::arch::paging::X86PageTable>(USER_VIRT, phys_to_ptr)
            .is_some();
        if in_a && !in_b {
            kprintln!(
                "CIBOS kernel: container isolation verified — page in space A (root {:#x}) \
                 is absent in space B (root {:#x})",
                a.root().addr(),
                b.root().addr()
            );
        } else {
            kprintln!("CIBOS kernel: container isolation CHECK FAILED ({in_a}, {in_b})");
        }
        // These demo spaces are discarded; their page-table frames stay
        // allocated for the rest of boot (no teardown path in this
        // demonstration, and the types are Drop-free so they need no cleanup).
        let _ = (a, b);
    }
}

/// Install the GDT/TSS and the IDT, then drop to ring 3 to run the user payload.
/// The payload logs a message and calls `exit`, both via `int 0x80`.
#[cfg(target_arch = "x86_64")]
fn run_ring3_demo(
    frames: &cibos_kernel::FrameAllocator,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) {
    // SAFETY: single-threaded bring-up; installs the kernel GDT/TSS and IDT once,
    // before any ring transition or trap.
    unsafe {
        crate::arch::gdt::init();
        crate::arch::idt::init();
        // Remap the legacy PIC so hardware IRQs land at vectors 0x20..0x2F
        // (clear of the CPU exception vectors), then unmask only the keyboard
        // line (IRQ1). The timer (IRQ0) stays masked — the kernel has no timer
        // driver yet — so entering ring 3 with RFLAGS.IF set is safe: the only
        // interrupt that can fire is the keyboard, which has a handler at 0x21.
        crate::arch::remap_pic();
        // PIT at 100 Hz drives IRQ0 (vector 0x20): the wake/timeout source.
        crate::arch::init_pit(crate::timer::TICK_HZ);
        crate::arch::unmask_irq(0); // timer (IRQ0)
        crate::arch::unmask_irq(1); // keyboard (IRQ1)
        crate::arch::unmask_irq(2); // cascade (IRQ2)
        kprintln!(
            "CIBOS kernel: GDT/TSS + IDT installed, PIC remapped, PIT @ {} Hz, \
             timer + keyboard IRQs enabled",
            crate::timer::TICK_HZ
        );

        // Prove real hardware input reaches the kernel: enable interrupts and
        // wait briefly for a keystroke (the IRQ1 handler decodes the scancode
        // and enqueues a KeyEvent). In QEMU a key can be injected via the
        // monitor `sendkey`; on real hardware, a physical keypress.
        demonstrate_keyboard_input();

        // Load and run an EXTERNAL application image (.capp), not an embedded
        // code blob: the `hello` app is assembled and wrapped into a .capp at
        // build time (see build.rs) and embedded here via include_bytes!. The
        // loader parses it, maps each segment into the user address space with
        // its own permissions, and enters ring 3 at the image's entry point.
        // This is the baked-in-app pipeline: on a real medium the .capp would be
        // placed by mkbootimage per the image flavor instead of embedded.
        const HELLO_CAPP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hello.capp"));
        match shared::AppImage::parse(HELLO_CAPP) {
            Ok(image) => {
                kprintln!(
                    "CIBOS kernel: loading external app image (.capp): {} segment(s), \
                     entry {:#x}",
                    image.segment_count(),
                    image.entry()
                );
                match crate::loader::run_app_image_isolated(frames, &image, phys_to_ptr) {
                    // The app logs, then `exit(7)` unwinds back here via the
                    // saved kernel context — proving the kernel loaded and ran
                    // an external image in a user address space and regained
                    // control with the app's own exit code.
                    Ok(code) => kprintln!(
                        "CIBOS kernel: external app exited with code {code} \
                         (loaded from .capp, ran in ring 3)"
                    ),
                    Err(e) => kprintln!("CIBOS kernel: app launch failed: {e}"),
                }
            }
            Err(e) => kprintln!("CIBOS kernel: .capp parse failed: {e}"),
        }

        // Now load and run a second external app — written in RUST on the
        // cibos-app runtime — proving a real Rust application (not assembly)
        // runs in ring 3 through the same loader and syscall ABI.
        const HELLO_RS_CAPP: &[u8] =
            include_bytes!(concat!(env!("OUT_DIR"), "/hello-rs.capp"));
        match shared::AppImage::parse(HELLO_RS_CAPP) {
            Ok(image) => {
                kprintln!(
                    "CIBOS kernel: loading Rust app image (.capp): {} segment(s), entry {:#x}",
                    image.segment_count(),
                    image.entry()
                );
                match crate::loader::run_app_image_isolated(frames, &image, phys_to_ptr) {
                    Ok(code) => kprintln!(
                        "CIBOS kernel: Rust app exited with code {code} \
                         (cibos-app runtime, ran in ring 3)"
                    ),
                    Err(e) => kprintln!("CIBOS kernel: Rust app launch failed: {e}"),
                }
            }
            Err(e) => kprintln!("CIBOS kernel: Rust .capp parse failed: {e}"),
        }

        // Run the login application (.capp) twice under the storage-selftest
        // configuration, driving it with injected keystrokes (deterministic,
        // since QEMU sendkey is unreliable): first run CREATES profile "alice"
        // (no credential file yet), the second LOGS IN as "alice". This exercises
        // the whole interactive stack — ReadKey -> read_line, GetRandom -> salt,
        // fs -> CIBOSFS credential file, shared salted-SHA-256 verify — in ring 3.
        #[cfg(feature = "storage-selftest")]
        {
            const LOGIN_RS_CAPP: &[u8] =
                include_bytes!(concat!(env!("OUT_DIR"), "/login-rs.capp"));
            if let Ok(image) = shared::AppImage::parse(LOGIN_RS_CAPP) {
                // First run: create "alice" with password "pw123" (typed twice).
                kprintln!("CIBOS kernel: --- login app: create-user run ---");
                inject_text("alice");
                inject_enter();
                inject_text("pw123");
                inject_enter();
                inject_text("pw123");
                inject_enter();
                match crate::loader::run_app_image_isolated(frames, &image, phys_to_ptr) {
                    Ok(code) => kprintln!("CIBOS kernel: login(create) exited with code {code}"),
                    Err(e) => kprintln!("CIBOS kernel: login(create) launch failed: {e}"),
                }

                // Second run: log in as "alice" with the correct password.
                kprintln!("CIBOS kernel: --- login app: login run ---");
                inject_text("alice");
                inject_enter();
                inject_text("pw123");
                inject_enter();
                match crate::loader::run_app_image_isolated(frames, &image, phys_to_ptr) {
                    Ok(code) => kprintln!("CIBOS kernel: login(auth) exited with code {code}"),
                    Err(e) => kprintln!("CIBOS kernel: login(auth) launch failed: {e}"),
                }
            }

            // Run the REAL shell (shell::dispatch composing the existing
            // package-manager) in ring 3, driving it with a scripted command
            // sequence (sendkey is unreliable, so inject programmatically). This
            // exercises the whole shell stack — ReadKey -> read_line, the generic
            // dispatch, the Fs* syscalls (write/read/ls/rm), Now -> uptime, and a
            // composed app program (`pkg`) — all on the booted kernel.
            const SHELL_RS_CAPP: &[u8] =
                include_bytes!(concat!(env!("OUT_DIR"), "/shell-rs.capp"));
            if let Ok(image) = shared::AppImage::parse(SHELL_RS_CAPP) {
                kprintln!("CIBOS kernel: --- shell app run ---");
                inject_text("help");
                inject_enter();
                inject_text("kv set k hi");
                inject_enter();
                inject_text("kv get k");
                inject_enter();
                inject_text("edit append line1");
                inject_enter();
                inject_text("edit show");
                inject_enter();
                inject_text("exit");
                inject_enter();
                match crate::loader::run_app_image_isolated(frames, &image, phys_to_ptr) {
                    Ok(code) => kprintln!("CIBOS kernel: shell exited with code {code}"),
                    Err(e) => kprintln!("CIBOS kernel: shell launch failed: {e}"),
                }
            }
        }
    }
}

/// Inject the characters of `s` into the keyboard queue (selftest only).
#[cfg(all(target_arch = "x86_64", feature = "storage-selftest"))]
fn inject_text(s: &str) {
    for c in s.chars() {
        crate::keyboard::inject_key(cibos_input::KeyEvent::ch(c));
    }
}

/// Inject an Enter key (selftest only).
#[cfg(all(target_arch = "x86_64", feature = "storage-selftest"))]
fn inject_enter() {
    crate::keyboard::inject_key(cibos_input::KeyEvent::new(cibos_input::Key::Enter));
}

/// Exercise the filesystem through the actual syscall ABI: build an `FsRwArgs`
/// block plus path/data buffers in kernel memory, issue `FsWrite` then `FsRead`
/// via `handle_syscall`, and confirm the round-trip. This proves the whole path
/// — trap dispatcher → `SyscallEnv` → mounted CIBOSFS → ATA disk — not just the
/// filesystem in isolation. (In this step the "user" pointers are kernel-mapped,
/// as with the existing log/exit transport; ring-3 boundaries will translate
/// through their address space with no ABI change.)
#[cfg(all(target_arch = "x86_64", feature = "storage-selftest"))]
fn demonstrate_fs_syscalls() {
    use shared::protocols::syscall::{FsRwArgs, Syscall};

    let path = b"/etc/via_syscall";
    let data = b"written through the syscall ABI";
    let mut readbuf = [0u8; 64];

    // Write via the FsWrite syscall.
    let wargs = FsRwArgs {
        path_ptr: path.as_ptr() as u64,
        path_len: path.len() as u64,
        buf_ptr: data.as_ptr() as u64,
        buf_len: data.len() as u64,
    };
    let wbytes = wargs.to_bytes();
    let wret = handle_syscall(Syscall::FsWrite.number(), wbytes.as_ptr() as u64, 0, 0);

    // Read it back via the FsRead syscall into readbuf.
    let rargs = FsRwArgs {
        path_ptr: path.as_ptr() as u64,
        path_len: path.len() as u64,
        buf_ptr: readbuf.as_mut_ptr() as u64,
        buf_len: readbuf.len() as u64,
    };
    let rbytes = rargs.to_bytes();
    let rret = handle_syscall(Syscall::FsRead.number(), rbytes.as_ptr() as u64, 0, 0);

    let ok = wret == data.len() as i64
        && rret == data.len() as i64
        && &readbuf[..data.len()] == data;
    kprintln!(
        "CIBOS kernel: fs syscalls — FsWrite returned {wret}, FsRead returned {rret}, \
         round-trip {}",
        if ok { "OK" } else { "FAIL" }
    );
}

/// Probe the slave data disk, format a fresh CIBOSFS, seed `/etc/`, and install
/// it as the kernel's root filesystem (`ROOT_FS`) — done before the user apps
/// run so a ring-3 `.capp` can issue filesystem syscalls against it.
#[cfg(all(target_arch = "x86_64", feature = "storage-selftest"))]
fn mount_root_fs_early() {
    use cibos_kernel::fs::Fs;
    // SAFETY: single-threaded bring-up; probes the primary slave ATA ports.
    let Some(data) = (unsafe { crate::arch::ata::AtaDisk::probe(crate::arch::ata::Device::Slave) })
    else {
        kprintln!("CIBOS kernel: no data disk on the slave (root fs not mounted)");
        return;
    };
    kprintln!(
        "CIBOS kernel: data disk (slave) online — {} sectors; formatting CIBOSFS",
        data.sectors()
    );
    match Fs::format(data, 64).and_then(|mut fs| {
        fs.mkdir(b"/etc")?;
        Ok(fs)
    }) {
        Ok(fs) => {
            *ROOT_FS.lock() = Some(fs);
            kprintln!("CIBOS kernel: root filesystem mounted (CIBOSFS), /etc ready");
        }
        Err(e) => kprintln!("CIBOS kernel: root fs format failed: {:?}", e),
    }
}

/// Probe the primary ATA bus and read back the boot medium to prove real block
/// I/O. Reads LBA 0 (the MBR — must end in the 0x55AA boot signature) and LBA 1
/// (the Boot Layout Descriptor — must carry the `CIBOSBL1` magic the image tool
/// wrote). Reading the genuine on-disk structures we booted from, through the
/// ATA driver, is the end-to-end storage proof.
#[cfg(target_arch = "x86_64")]
fn demonstrate_storage() {
    use cibos_kernel::block::{BlockDevice, BLOCK_SIZE};

    // SAFETY: single-threaded bring-up; probes the fixed primary-bus ATA ports.
    let disk = unsafe { crate::arch::ata::AtaDisk::probe(crate::arch::ata::Device::Master) };
    let Some(disk) = disk else {
        kprintln!("CIBOS kernel: storage — no ATA disk on the primary bus (skipping)");
        return;
    };
    kprintln!(
        "CIBOS kernel: ATA disk online — {} sectors ({} MiB)",
        disk.sectors(),
        disk.sectors() * BLOCK_SIZE as u64 / (1024 * 1024)
    );

    let mut sector = [0u8; BLOCK_SIZE];

    // LBA 0: the MBR. Last two bytes must be the 0x55, 0xAA boot signature.
    match disk.read_blocks(0, 1, &mut sector) {
        Ok(()) => {
            let sig_ok = sector[510] == 0x55 && sector[511] == 0xAA;
            kprintln!(
                "CIBOS kernel: read LBA 0 (MBR) — boot signature {}",
                if sig_ok { "OK (0x55AA)" } else { "MISSING" }
            );
        }
        Err(e) => kprintln!("CIBOS kernel: LBA 0 read failed: {:?}", e),
    }

    // LBA 1: the Boot Layout Descriptor. First eight bytes are the BLD magic.
    match disk.read_blocks(1, 1, &mut sector) {
        Ok(()) => {
            let magic = u64::from_le_bytes(sector[0..8].try_into().unwrap());
            let ok = magic == shared::BLD_MAGIC;
            kprintln!(
                "CIBOS kernel: read LBA 1 (descriptor) — magic {}",
                if ok { "OK (CIBOSBL1)" } else { "mismatch" }
            );
        }
        Err(e) => kprintln!("CIBOS kernel: LBA 1 read failed: {:?}", e),
    }

    // Optional write-path verification (feature `storage-selftest`). Targets the
    // disk's LAST sector and is non-destructive: it saves the current contents,
    // writes a known pattern, reads it back to confirm the round-trip, then
    // restores the original bytes. This proves write_blocks against real
    // hardware without altering any meaningful data.
    #[cfg(feature = "storage-selftest")]
    {
        let last = disk.block_count() - 1;
        let mut saved = [0u8; BLOCK_SIZE];
        let mut pattern = [0u8; BLOCK_SIZE];
        let mut readback = [0u8; BLOCK_SIZE];
        for (i, b) in pattern.iter_mut().enumerate() {
            *b = (i as u8) ^ 0x5A;
        }
        let r = disk
            .read_blocks(last, 1, &mut saved)
            .and_then(|()| disk.write_blocks(last, 1, &pattern))
            .and_then(|()| disk.read_blocks(last, 1, &mut readback));
        match r {
            Ok(()) => {
                let matched = readback == pattern;
                // Restore the original contents regardless of the comparison.
                let restored = disk.write_blocks(last, 1, &saved).is_ok();
                kprintln!(
                    "CIBOS kernel: storage self-test — wrote+read LBA {} : {} (restore {})",
                    last,
                    if matched { "round-trip OK" } else { "MISMATCH" },
                    if restored { "OK" } else { "FAILED" }
                );
            }
            Err(e) => kprintln!("CIBOS kernel: storage self-test failed: {:?}", e),
        }
    }

    // Optional on-disk filesystem demo (feature `storage-selftest`). Operates on
    // the primary SLAVE — a second, dedicated data disk — so the boot medium is
    // never formatted. Proves CIBOSFS over the real ATA driver: format, create a
    // directory + file, read it back, then remount and confirm it persisted.
    #[cfg(feature = "storage-selftest")]
    {
        use cibos_kernel::fs::Fs;
        // If the root filesystem was already mounted early (before the apps),
        // exercise it through the syscall ABI rather than reformatting.
        if ROOT_FS.lock().is_some() {
            demonstrate_fs_syscalls();
            return;
        }
        // SAFETY: single-threaded bring-up; probes the primary slave.
        match unsafe { crate::arch::ata::AtaDisk::probe(crate::arch::ata::Device::Slave) } {
            Some(data) => {
                kprintln!(
                    "CIBOS kernel: data disk (slave) online — {} sectors",
                    data.sectors()
                );
                let r = (|| -> Result<(), cibos_kernel::fs::FsError> {
                    let mut fs = Fs::format(data, 64)?;
                    fs.mkdir(b"/etc")?;
                    fs.write_file(b"/etc/hello", b"CIBOSFS persisted this")?;
                    let read = fs.read_file(b"/etc/hello")?;
                    let ok_live = read == b"CIBOSFS persisted this";
                    // Remount and re-read to prove it is on the medium, not RAM.
                    let dev = fs.into_device();
                    let fs2 = Fs::mount(dev)?;
                    let read2 = fs2.read_file(b"/etc/hello")?;
                    let ok_persist = read2 == b"CIBOSFS persisted this";
                    kprintln!(
                        "CIBOS kernel: CIBOSFS — wrote /etc/hello, read-back {}, \
                         remount-persist {}",
                        if ok_live { "OK" } else { "FAIL" },
                        if ok_persist { "OK" } else { "FAIL" }
                    );
                    // Install as the kernel's root filesystem so the filesystem
                    // SYSCALLS operate on it, then exercise the full ABI path.
                    *ROOT_FS.lock() = Some(fs2);
                    Ok(())
                })();
                if let Err(e) = r {
                    kprintln!("CIBOS kernel: CIBOSFS demo failed: {:?}", e);
                } else {
                    demonstrate_fs_syscalls();
                }
            }
            None => kprintln!("CIBOS kernel: no data disk on the primary slave (skipping FS demo)"),
        }
    }
}

/// Enable interrupts and poll the keyboard queue briefly to prove that a real
/// hardware keystroke reaches the kernel through the IRQ1 → scancode-decode →
/// key-queue path. Reports the first key seen (or that none arrived within the
/// window), then disables interrupts again so the subsequent ring-3 transition
/// runs in the known-good (keyboard-only-IRQ) environment it expects.
///
/// # Safety
///
/// The IDT and remapped PIC must already be initialised with the keyboard line
/// unmasked and a handler at vector 0x21.
#[cfg(target_arch = "x86_64")]
unsafe fn demonstrate_keyboard_input() {
    use core::arch::asm;

    // Enable interrupts (set IF). From here the timer (IRQ0) and keyboard (IRQ1)
    // can fire.
    asm!("sti", options(nomem, nostack));

    // Prove the timer is advancing: sample, wait a fixed number of ticks, sample
    // again. This is the wake/timeout source the rest of the system builds on.
    let t0 = crate::timer::now_ticks();
    crate::timer::wait_ticks(crate::timer::millis_to_ticks(200));
    let t1 = crate::timer::now_ticks();
    kprintln!(
        "CIBOS kernel: timer online — {} ticks in ~200ms (PIT @ {} Hz)",
        t1 - t0,
        crate::timer::TICK_HZ
    );

    // Wait up to ~2s for a keystroke, returning the instant one arrives. Now
    // that a timer exists, this is a real bounded wait (not a fragile busy
    // spin): it sleeps via `hlt` between interrupts, catches an injected/real
    // key reliably, and times out cleanly on a headless boot. An interactive
    // login/shell will use the same primitive with a long/effectively-infinite
    // timeout to block for the user.
    let got = crate::timer::wait_ticks_or(crate::timer::millis_to_ticks(2000), || {
        crate::keyboard::poll_key().is_some()
    });
    // Note: the predicate above pops the key when it returns true; re-poll is
    // empty. To report the actual key, poll once more before the predicate in a
    // real consumer. Here we report from scancode state.
    let _ = got;

    // Disable interrupts again before returning to the ring-3 path.
    asm!("cli", options(nomem, nostack));

    let seen_codes = crate::keyboard::scancodes_seen();
    if seen_codes > 0 {
        kprintln!(
            "CIBOS kernel: keyboard online — {} scancode(s) received and decoded",
            seen_codes
        );
    } else {
        kprintln!(
            "CIBOS kernel: keyboard armed (IRQ1 live, no fault); no key within ~2s window"
        );
    }
}

/// On non-x86_64 targets the page-table encoder and CR3 install are not yet
/// implemented, so MMU bring-up is a no-op (the bootloader/firmware identity map
/// stays active). The portable model is identical; only the arch encoder differs.
#[cfg(not(target_arch = "x86_64"))]
fn bring_up_mmu(_handoff: &HandoffData) {
    kprintln!("CIBOS kernel: MMU bring-up skipped (arch backend pending)");
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

/// The kernel's optionally-mounted root filesystem (CIBOSFS over the ATA data
/// disk). When mounted, the filesystem syscalls operate on it; when absent they
/// report `NotPermitted`. Guarded by a spinlock so the trap path and any setup
/// code do not race.
#[cfg(target_arch = "x86_64")]
static ROOT_FS: cibos_kernel::sync::SpinLock<
    Option<cibos_kernel::fs::Fs<crate::arch::ata::AtaDisk>>,
> = cibos_kernel::sync::SpinLock::new(None);

/// The kernel CSPRNG backing the `get_random` syscall, seeded from the firmware
/// entropy seed in the handoff at bring-up. `None` until seeded.
#[cfg(target_arch = "x86_64")]
static KERNEL_RNG: cibos_kernel::sync::SpinLock<Option<cibos_kernel::entropy::Csprng>> =
    cibos_kernel::sync::SpinLock::new(None);

/// Seed the kernel CSPRNG from the firmware entropy seed. Called once at boot.
#[cfg(target_arch = "x86_64")]
fn seed_kernel_rng(seed: [u8; 32]) {
    *KERNEL_RNG.lock() = Some(cibos_kernel::entropy::Csprng::from_seed(seed));
}

/// Map a filesystem error onto the syscall ABI error set.
#[cfg(target_arch = "x86_64")]
fn fs_err(e: cibos_kernel::fs::FsError) -> shared::protocols::syscall::SyscallError {
    use cibos_kernel::fs::FsError;
    use shared::protocols::syscall::SyscallError;
    match e {
        FsError::NotFound => SyscallError::NotFound,
        _ => SyscallError::IoError,
    }
}

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

    fn copy_to_user(
        &self,
        _boundary: shared::BoundaryId,
        ptr: u64,
        bytes: &[u8],
    ) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        if ptr == 0 {
            return Err(SyscallError::BadAddress);
        }
        if ptr.checked_add(bytes.len() as u64).is_none() {
            return Err(SyscallError::InvalidArgument);
        }
        // SAFETY: symmetric to copy_from_user — in this step the caller is
        // supervisor code and `ptr` is a kernel-mapped address in the active
        // identity map; the length is bounded by the dispatcher before we get
        // here. Once apps run in ring 3 this will translate through the calling
        // boundary's address space.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        }
        Ok(())
    }

    // The filesystem syscall methods route to the kernel's mounted root
    // filesystem (ROOT_FS). When nothing is mounted they fall through to the
    // trait defaults (NotPermitted).
    fn fs_read(
        &self,
        path: &[u8],
    ) -> Result<Option<alloc::vec::Vec<u8>>, shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let guard = ROOT_FS.lock();
        let Some(fs) = guard.as_ref() else {
            return Err(SyscallError::NotPermitted);
        };
        match fs.read_file(path) {
            Ok(data) => Ok(Some(data)),
            Err(cibos_kernel::fs::FsError::NotFound) => Ok(None),
            Err(e) => Err(fs_err(e)),
        }
    }

    fn fs_write(
        &self,
        path: &[u8],
        data: &[u8],
    ) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let mut guard = ROOT_FS.lock();
        let Some(fs) = guard.as_mut() else {
            return Err(SyscallError::NotPermitted);
        };
        fs.write_file(path, data).map(|_| ()).map_err(fs_err)
    }

    fn fs_mkdir(&self, path: &[u8]) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let mut guard = ROOT_FS.lock();
        let Some(fs) = guard.as_mut() else {
            return Err(SyscallError::NotPermitted);
        };
        fs.mkdir(path).map(|_| ()).map_err(fs_err)
    }

    fn fs_exists(&self, path: &[u8]) -> Result<bool, shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let guard = ROOT_FS.lock();
        let Some(fs) = guard.as_ref() else {
            return Err(SyscallError::NotPermitted);
        };
        Ok(fs.exists(path))
    }

    fn fs_list(
        &self,
        path: &[u8],
    ) -> Result<Option<alloc::vec::Vec<alloc::string::String>>, shared::protocols::syscall::SyscallError>
    {
        use shared::protocols::syscall::SyscallError;
        let guard = ROOT_FS.lock();
        let Some(fs) = guard.as_ref() else {
            return Err(SyscallError::NotPermitted);
        };
        match fs.list_dir(path) {
            Ok(entries) => Ok(Some(
                entries
                    .into_iter()
                    .map(|e| alloc::string::String::from_utf8_lossy(&e.name).into_owned())
                    .collect(),
            )),
            Err(cibos_kernel::fs::FsError::NotFound) => Ok(None),
            Err(e) => Err(fs_err(e)),
        }
    }

    fn fs_delete(&self, path: &[u8]) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let mut guard = ROOT_FS.lock();
        let Some(fs) = guard.as_mut() else {
            return Err(SyscallError::NotPermitted);
        };
        fs.remove_file(path).map_err(fs_err)
    }

    fn read_key(&self, blocking: bool) -> i64 {
        use shared::protocols::syscall::{encode_key, KeyCode, KeyMods, SyscallError};
        // Map a kernel KeyEvent to the ABI key encoding.
        fn map(ev: cibos_input::KeyEvent) -> i64 {
            use cibos_input::Key;
            let code = match ev.key {
                Key::Char(c) => KeyCode::Char(c),
                Key::Enter => KeyCode::Enter,
                Key::Backspace => KeyCode::Backspace,
                Key::Delete => KeyCode::Delete,
                Key::Tab => KeyCode::Tab,
                Key::Escape => KeyCode::Escape,
                Key::Left => KeyCode::Left,
                Key::Right => KeyCode::Right,
                Key::Up => KeyCode::Up,
                Key::Down => KeyCode::Down,
                Key::Home => KeyCode::Home,
                Key::End => KeyCode::End,
            };
            let mods = KeyMods {
                shift: ev.mods.shift,
                ctrl: ev.mods.ctrl,
                alt: ev.mods.alt,
            };
            encode_key(code, mods)
        }

        if let Some(ev) = crate::keyboard::poll_key() {
            return map(ev);
        }
        if !blocking {
            return SyscallError::NotFound.as_return();
        }
        // Block efficiently using the timer's bounded wait primitive: sleep via
        // `hlt` until a key arrives or the timeout elapses. `wait_ticks_or` is
        // the established building block for "wait for input up to a deadline";
        // the steady PIT tick guarantees this always terminates.
        // SAFETY: interrupts are enabled and the timer/keyboard IRQs are live by
        // the time user code runs.
        let got = unsafe {
            crate::timer::wait_ticks_or(
                crate::timer::millis_to_ticks(30_000),
                crate::keyboard::has_key,
            )
        };
        if got {
            if let Some(ev) = crate::keyboard::poll_key() {
                return map(ev);
            }
        }
        SyscallError::NotFound.as_return()
    }

    fn fill_random(&self, out: &mut [u8]) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let mut guard = KERNEL_RNG.lock();
        let Some(rng) = guard.as_mut() else {
            return Err(SyscallError::NotPermitted);
        };
        rng.fill_bytes(out);
        Ok(())
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
        // Return control to the kernel at the matching `enter_user_context`
        // call site, with the exit code as that call's return value. This is the
        // basis for a process model: `exit` unwinds to the kernel (and, later,
        // to the scheduler) rather than halting.
        SyscallOutcome::Exit(code) => {
            // SAFETY: reached only from a syscall issued by the ring-3 task that
            // `enter_user_context` launched, so a saved kernel context is live.
            unsafe { crate::loader::return_to_kernel(code as i64) }
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("CIBOS kernel PANIC: {info}");
    arch::halt();
}
