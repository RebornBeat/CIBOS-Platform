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
#[cfg(all(target_arch = "x86_64", any(feature = "ring3-resume-demo", feature = "ring3-multilane-demo")))]
global_asm!(include_str!("arch/resume_user.s"));
#[cfg(target_arch = "aarch64")]
global_asm!(include_str!("boot/aarch64.s"));
#[cfg(target_arch = "aarch64")]
global_asm!(include_str!("arch/vectors_aarch64.s"));
#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("boot/riscv64.s"));
#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("arch/vectors_riscv64.s"));
#[cfg(target_arch = "x86")]
global_asm!(include_str!("boot/x86.s"));

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
        use ::core::fmt::Write as _;
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
pub extern "C" fn kernel_entry(handoff_ptr: u64, dtb_ptr: u64) -> ! {
    arch::init_serial();
    // Stash the DTB pointer (firmware/QEMU passes a Flattened Device Tree
    // describing the real platform: RAM base/size, device addresses). On the
    // self-boot path this lets us read the actual layout at runtime instead of
    // using compiled-in constants — so the kernel works on QEMU AND real hardware
    // without knowing which. x86_64 has no DTB (its layout comes from the BIOS
    // handoff), so the pointer is simply unused there.
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    {
        DTB_PTR.store(dtb_ptr, core::sync::atomic::Ordering::Relaxed);
    }
    // If the firmware DTB reports a different console UART base than the early
    // bootstrap default, switch to it now so the rest of boot (and real boards)
    // use the discovered address. On QEMU virt this is the same address; on a
    // real board whose UART lives elsewhere, this is what makes output appear.
    #[cfg(target_arch = "aarch64")]
    {
        if let Some(base) = dtb_device_base(b"pl011") {
            arch::set_uart_base(base as usize);
        }
    }
    let _ = dtb_ptr;
    // Phase: early_traps — install fault/trap vectors (and enable FP where the
    // arch needs it, done in the boot asm) so any fault is REPORTED, not silent.
    // Driven through the per-arch bring-up contract: identical call on every
    // arch, no target_arch branching in the boot flow.
    Arch.early_traps();
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

            // Portable in-kernel IPC demo: two cooperative lanes exchange
            // messages over a bounded channel, driven by the single selector's
            // Catch-and-Release loop. This is arch-independent (pure cibos-kernel
            // Rust), so it proves the canonical channel model on EVERY arch —
            // including aarch64/riscv64, which do not yet run the ring-3 syscall
            // path. The sender's second send parks on a full buffer until the
            // receiver drains a slot (back-pressure), then resumes.
            #[cfg(feature = "channel-demo")]
            demonstrate_kernel_channel(&mut kernel);

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
            // Per-arch bring-up phases, driven through the contract in canonical
            // order. Each call is identical on every arch; an arch that has not
            // built a phase reports Skipped(reason) honestly. No target_arch
            // branching in this control flow.
            let st = Arch.seed_entropy(&handoff.entropy_seed);
            if let PhaseStatus::Skipped(r) | PhaseStatus::Failed(r) = st {
                kprintln!("CIBOS kernel: entropy seed {} ({r})", st.label());
            }

            let st = Arch.mount_root_fs();
            if let PhaseStatus::Failed(r) = st {
                kprintln!("CIBOS kernel: root FS mount FAILED ({r})");
            }

            // MMU phase: on x86_64 this also owns the frame allocator and, within
            // its scope, probes the NIC and drops to ring 3.
            let st = Arch.bring_up_mmu(&handoff);
            if let PhaseStatus::Skipped(r) = st {
                kprintln!("CIBOS kernel: MMU bring-up skipped ({r})");
            }

            let _ = Arch.verify_storage();

            // Production GUI surface: the kernel GUI runner (a real display
            // driver — see `crate::gui`/`crate::arch::vga`) renders a
            // `platform-gui` cell-grid Surface to the screen and drives a GuiApp
            // from the live keyboard. The driver is always-compiled production
            // code; the `gui-demo` feature selects which app the boot surface
            // launches (here, the notepad). A different image can launch a
            // different GuiApp on the same runner.
            #[cfg(all(target_arch = "x86_64", feature = "gui-demo"))]
            {
                kprintln!("CIBOS kernel: starting GUI surface (notepad)");
                let mut app = notepad::Notepad::new();
                crate::gui::run_gui_app(&mut app);
                kprintln!("CIBOS kernel: GUI surface exited");
            }

            // IPC demo: open a local channel and round-trip a message through
            // the *real* syscall dispatch path (OpenChannel -> ChannelSend ->
            // ChannelRecv against KernelSyscallEnv). Proves the Track 2 ABI works
            // end to end on the booted kernel, including bounded back-pressure.
            #[cfg(all(target_arch = "x86_64", feature = "channel-demo"))]
            demonstrate_channel();

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
    // The usable RAM base differs by platform. x86 has no DTB (its layout comes
    // from the BIOS handoff). On aarch64/riscv64 the firmware/QEMU passes a DTB
    // describing the REAL platform RAM — read it at runtime so the same kernel is
    // correct on QEMU virt AND real boards; fall back to the conventional QEMU
    // virt base/size only if no DTB was passed or it could not be parsed.
    #[cfg(target_arch = "x86_64")]
    let (ram_base, ram_length): (u64, u64) = (0x0010_0000, 0x0800_0000); // 1 MiB, 128 MiB
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
    let (ram_base, ram_length): (u64, u64) = (0x0010_0000, 0x0800_0000);
    #[cfg(target_arch = "aarch64")]
    let fallback: (u64, u64) = (0x4000_0000, 0x0800_0000); // 1 GiB, 128 MiB
    #[cfg(target_arch = "riscv64")]
    let fallback: (u64, u64) = (0x8000_0000, 0x0800_0000); // 2 GiB, 128 MiB
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    let (ram_base, ram_length): (u64, u64) = match dtb_ram_region() {
        Some((base, size)) if size > 0 => {
            kprintln!(
                "CIBOS kernel: platform RAM from DTB: base {:#x}, size {} MiB",
                base,
                size / (1024 * 1024)
            );
            (base, size)
        }
        _ => {
            kprintln!("CIBOS kernel: no DTB RAM region; using platform fallback");
            fallback
        }
    };
    let regions = [MemoryRegion {
        base: ram_base,
        length: ram_length,
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
        ram_length,
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
    bring_up_mmu_generic::<crate::arch::paging::ArchPagingImpl>(handoff);
}

/// Portable MMU bring-up orchestration, generic over the architecture's paging
/// hooks ([`crate::bringup::ArchPaging`]). This single function builds the
/// kernel's page tables, identity-maps low RAM + the arch's device-MMIO ranges,
/// installs the tables, and proves per-container isolation — identically on every
/// architecture. Only the `P` hooks (entry encoder, register ops, MMIO ranges)
/// differ per arch. After the MMU is online it runs the post-MMU phases (NIC +
/// ring-3) which still borrow the frame allocator this function owns.
fn bring_up_mmu_generic<P: crate::bringup::ArchPaging>(handoff: &HandoffData) {
    use cibos_kernel::paging::{AddressSpace, Permissions};
    use cibos_kernel::{FrameAllocator, FRAME_SIZE};
    use alloc::vec::Vec;
    use shared::MemoryRegion;

    // Reserve all physical memory below this watermark from the frame allocator
    // so building the tables cannot clobber the kernel image, heap, or stack.
    // Arch-supplied: on the PC, RAM starts at 0 and the kernel is low; on QEMU
    // virt, RAM (and the kernel) start at 1 GiB, so the watermark must clear it.
    let reserved_below: u64 = P::reserved_below();
    // Identity-map this much physical address space (arch-supplied; covers
    // everything the kernel touches plus any low framebuffer).
    let identity_map_bytes: u64 = P::identity_map_bytes();

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
    let frames = FrameAllocator::from_regions(&regions, reserved_below);
    kprintln!(
        "CIBOS kernel: frame allocator: {} usable frame(s), {} free above {:#x}",
        frames.usable_frames(),
        frames.free_frames(),
        reserved_below
    );

    // The bootloader/firmware installed a low identity map, so physical address P
    // is currently readable/writable at virtual address P: identity is the map.
    let phys_to_ptr = |phys: u64| phys as *mut u8;

    // SAFETY: the identity map above is valid for every frame the allocator
    // hands out (all within mapped physical RAM), and we install the result only
    // after fully mapping the memory the kernel is currently executing from.
    unsafe {
        // Enable any entry features the arch needs before building tables that
        // use them (x86: EFER.NXE so the NX bit is honored).
        P::enable_table_features();

        let space = match AddressSpace::new(&frames, &phys_to_ptr) {
            Ok(s) => s,
            Err(e) => {
                kprintln!("CIBOS kernel: MMU bring-up failed (root alloc): {e}");
                return;
            }
        };

        let pages = identity_map_bytes / FRAME_SIZE;
        // Kernel-rwx identity map: the kernel runs in supervisor mode, so these
        // are kernel (non-user) pages. Per-boundary user spaces will map user
        // pages with restricted permissions on top of this mechanism.
        if let Err(e) = space.map_range::<P::Encoder>(
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

        // Map the arch's device-MMIO ranges (kernel-rw, NON-EXECUTABLE) so
        // MMIO-BAR drivers can reach their registers. The main identity map only
        // covers low RAM; device MMIO lives high (x86 PCI hole at 0xFEB.., ARM
        // GIC/UART, RISC-V PLIC/UART). Real hardware register space — any OS maps it.
        for &(base, len) in P::mmio_identity_ranges() {
            let mmio_pages = len / FRAME_SIZE;
            if let Err(e) = space.map_range::<P::Encoder>(
                base,
                base,
                mmio_pages,
                Permissions {
                    read: true,
                    write: true,
                    execute: false,
                    user: false,
                },
                &frames,
                &phys_to_ptr,
            ) {
                kprintln!("CIBOS kernel: MMU bring-up failed (MMIO map {base:#x}): {e}");
                return;
            }
        }

        kprintln!(
            "CIBOS kernel: page tables built (identity-mapped {} MiB), installing root {:#x}",
            identity_map_bytes / (1024 * 1024),
            space.root().addr()
        );

        // The moment of truth: switch to our tables. Execution continuing past
        // this call is the proof that the tables are valid hardware tables.
        P::install(space.root());

        kprintln!(
            "CIBOS kernel: MMU online — running on kernel-built page tables (root {:#x})",
            P::current_root()
        );

        // Demonstrate per-container isolation on the proven mechanism: two
        // distinct boundaries get their own page tables; a page mapped in one is
        // physically absent in the other. Uses a borrowed frame allocator so the
        // allocator remains available for the ring-3 payload below.
        #[cfg(target_arch = "x86_64")]
        verify_container_isolation(&frames, &phys_to_ptr);

        // Post-MMU phases that borrow the frame allocator this function owns.
        // These are x86-specific TODAY (NIC drivers + ring-3 entry); they will be
        // generalized the same way (shared orchestration + arch hooks) as the
        // per-arch sweep reaches them. Until then they run only where built.
        #[cfg(target_arch = "x86_64")]
        {
            // Probe for a NIC now that the MMU is online: the device's virtqueue
            // DMA addresses must be stable under the final page tables, so this
            // runs AFTER the switch. Production path, like ATA storage.
            let _nic_present = probe_nic_at_boot(&frames);

            // Drop to ring 3 and run unprivileged user payloads, each in its own
            // per-process address space, reaching the kernel only via int 0x80
            // syscalls — the full user/kernel boundary.
            start_ring3_runtime(&frames, &phys_to_ptr);
        }

        // `space` and `frames` back the live page tables for the rest of this
        // boot. Neither type implements `Drop` and the page-table frames live in
        // physical RAM independent of these handles, so letting them fall out of
        // scope here leaves the live mappings intact.
        let _ = &frames;
    }
}

/// Show two boundaries with independent address spaces on the live MMU, using a
/// borrowed frame allocator. Diagnostic demonstration of per-container isolation.
#[cfg(target_arch = "x86_64")]
fn verify_container_isolation(
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
/// Bring up the ring-3 user runtime: install the GDT/TSS and IDT, remap the PIC,
/// start the PIT, enable hardware IRQs, then load and run the system's `.capp`
/// applications (the interactive login→shell surface, server services, etc.).
/// This is the production path from kernel bring-up into user space. Opt-in
/// `#[cfg]` blocks add verification routines (resume/multilane/etc.) without
/// changing the production flow.
fn start_ring3_runtime(
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

        // Enable interrupts and confirm the keyboard line is live: the IRQ1
        // handler decodes scancodes into KeyEvents that the input syscalls
        // consume. A physical keypress drives this on real hardware.
        arm_keyboard_input();

        // Install the channel + Lattice (Gate/Link/Warden) handle table for the
        // production runtime, backed by a scheduler that drives back-pressure
        // wakeups. This makes the IPC syscalls (OpenChannel/Channel*, the
        // cross-boundary handshake) and the net syscalls (GateBind/GateConnect/
        // Link*/Warden*/GateProbe) available to every `.capp` in a NORMAL boot —
        // not only inside a demo. The demos install their own table when they run
        // their own selector; this is the always-on production install.
        #[cfg(not(feature = "ring3-multilane-demo"))]
        {
            let sched = alloc::sync::Arc::new(cibos_kernel::Scheduler::new(
                1,
                multilane_seed(),
                cibos_kernel::compiled_profile().unwrap_or(shared::CibosProfile::Balanced),
            ));
            install_channel_table(sched);
            // Now that the Lattice handle table exists AND the NIC is installed,
            // prove a remote Link round-trips through the Link API over the NIC.
            #[cfg(feature = "virtio-net-demo")]
            lattice_remote_link_selfcheck();
        }

        // Ring-3 park/resume demonstration (proves the per-lane context
        // mechanism: trap saves full user state -> park -> resume from the trap
        // point). Install the context-saving syscall stub for the demo, then
        // restore the default stub so the normal app flow below is unaffected.
        #[cfg(feature = "ring3-resume-demo")]
        {
            crate::arch::idt::set_ctx_saving_syscall_vector();
            crate::loader::run_resume_demo(frames, phys_to_ptr);
            crate::arch::idt::set_default_syscall_vector();
        }

        // Ring-3 multilane demonstration (selector-owned Ring3Table): two ring-3
        // lanes, the canonical Scheduler picks the next ready one, a lane that
        // yields parks and another runs, then the parked lane resumes. Same
        // vector-swap discipline so the normal app flow below is unaffected.
        #[cfg(feature = "ring3-multilane-demo")]
        {
            crate::arch::idt::set_ctx_saving_syscall_vector();
            crate::ring3::run_multilane_demo(frames, phys_to_ptr, multilane_seed());
            crate::arch::idt::set_default_syscall_vector();
        }

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
        // runs in ring 3 through the same loader and syscall ABI. Opt-in via
        // `app-hello` (the `--with-apps` flavor flag).
        #[cfg(feature = "app-hello")]
        {
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
        }

        // Run the login application (.capp) twice under the storage-selftest
        // configuration, driving it with injected keystrokes (deterministic,
        // since QEMU sendkey is unreliable): first run CREATES profile "alice"
        // (no credential file yet), the second LOGS IN as "alice". This exercises
        // the whole interactive stack — ReadKey -> read_line, GetRandom -> salt,
        // fs -> CIBOSFS credential file, shared salted-SHA-256 verify — in ring 3.
        #[cfg(all(feature = "storage-selftest", feature = "app-login"))]
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

                // Second run: authenticate as "alice". The login `.capp` returns
                // 0 when access is GRANTED and 1 when DENIED, so its exit code is
                // the session gate: the shell is launched ONLY on a granted login.
                kprintln!("CIBOS kernel: --- login app: login run ---");
                inject_text("alice");
                inject_enter();
                inject_text("pw123");
                inject_enter();
                let login_granted =
                    match crate::loader::run_app_image_isolated(frames, &image, phys_to_ptr) {
                        Ok(0) => {
                            kprintln!("CIBOS kernel: login GRANTED — starting session");
                            true
                        }
                        Ok(code) => {
                            kprintln!(
                                "CIBOS kernel: login DENIED (code {code}) — no session"
                            );
                            false
                        }
                        Err(e) => {
                            kprintln!("CIBOS kernel: login(auth) launch failed: {e}");
                            false
                        }
                    };

                // Run the REAL shell (shell::dispatch composing the existing apps)
                // as the user's session — but ONLY if login was granted. This is
                // the gated boot->login->session flow: a denied login never
                // reaches the shell. The session is driven here with injected
                // commands (deterministic; sendkey is unreliable); a live boot
                // reads the real keyboard. Opt-in via `app-shell`.
                #[cfg(feature = "app-shell")]
                if login_granted {
                    const SHELL_RS_CAPP: &[u8] =
                        include_bytes!(concat!(env!("OUT_DIR"), "/shell-rs.capp"));
                    if let Ok(image) = shared::AppImage::parse(SHELL_RS_CAPP) {
                        kprintln!("CIBOS kernel: --- shell session (user: alice) ---");
                        inject_text("store browse");
                        inject_enter();
                        inject_text("store install welcome");
                        inject_enter();
                        inject_text("store installed");
                        inject_enter();
                        inject_text("exit");
                        inject_enter();
                        match crate::loader::run_app_image_isolated(frames, &image, phys_to_ptr) {
                            Ok(code) => {
                                kprintln!("CIBOS kernel: shell session ended (code {code})")
                            }
                            Err(e) => kprintln!("CIBOS kernel: shell launch failed: {e}"),
                        }
                    }
                }
            }
        }

        // LIVE interactive session (no injected commands): a real person types
        // the profile/password into login-rs, and on a GRANTED login, the shell
        // commands into shell-rs. Same gated boot->login->session flow as the
        // injected selftest above, but driven entirely by the live keyboard
        // (blocking ReadKey -> IRQ1). Opt-in via `interactive-session`.
        #[cfg(all(target_arch = "x86_64", feature = "interactive-session"))]
        {
            run_interactive_session(frames, phys_to_ptr);
        }
    }
}

/// Run the live login -> gated shell session on the real keyboard (no injection).
/// login-rs and shell-rs are the same `.capp`s the injected selftest runs; here
/// they read live keystrokes. The shell starts ONLY on a granted login.
#[cfg(all(target_arch = "x86_64", feature = "interactive-session"))]
fn run_interactive_session(
    frames: &cibos_kernel::FrameAllocator,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) {
    const LOGIN_RS_CAPP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/login-rs.capp"));
    const SHELL_RS_CAPP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shell-rs.capp"));

    kprintln!("");
    kprintln!("CIBOS kernel: === live interactive session ===");
    kprintln!("CIBOS kernel: type at the prompts below (real keyboard).");

    let Ok(login_image) = shared::AppImage::parse(LOGIN_RS_CAPP) else {
        kprintln!("CIBOS kernel: interactive: login image parse failed");
        return;
    };

    // Run the login gate on the live keyboard. A granted login returns 0.
    // SAFETY: single-threaded bring-up; GDT/TSS+IDT installed, IRQs live, and the
    // frame allocator + identity phys map are valid (same preconditions as the
    // injected selftest's launches).
    let granted = match unsafe {
        crate::loader::run_app_image_isolated(frames, &login_image, phys_to_ptr)
    } {
        Ok(0) => {
            kprintln!("CIBOS kernel: login GRANTED — starting your session");
            true
        }
        Ok(code) => {
            kprintln!("CIBOS kernel: login DENIED (code {code})");
            false
        }
        Err(e) => {
            kprintln!("CIBOS kernel: login launch failed: {e}");
            false
        }
    };

    // The shell is the user's session — launched ONLY on a granted login.
    if granted {
        if let Ok(shell_image) = shared::AppImage::parse(SHELL_RS_CAPP) {
            kprintln!("CIBOS kernel: --- shell session ---");
            // SAFETY: as above.
            match unsafe {
                crate::loader::run_app_image_isolated(frames, &shell_image, phys_to_ptr)
            } {
                Ok(code) => kprintln!("CIBOS kernel: shell session ended (code {code})"),
                Err(e) => kprintln!("CIBOS kernel: shell launch failed: {e}"),
            }
        }
    }
}
#[cfg(all(target_arch = "x86_64", feature = "storage-selftest", feature = "app-login"))]
fn inject_text(s: &str) {
    for c in s.chars() {
        crate::keyboard::inject_key(cibos_input::KeyEvent::ch(c));
    }
}

/// Inject an Enter key (selftest only).
#[cfg(all(target_arch = "x86_64", feature = "storage-selftest", feature = "app-login"))]
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
#[cfg(all(target_arch = "x86_64", any(feature = "storage-selftest", feature = "interactive-session")))]
fn mount_root_fs_early() {
    use cibos_kernel::fs::{Fs, FsError};
    // SAFETY: single-threaded bring-up; probes the primary slave ATA ports.
    let Some(data) = (unsafe { crate::arch::ata::AtaDisk::probe(crate::arch::ata::Device::Slave) })
    else {
        kprintln!("CIBOS kernel: no data disk on the slave (root fs not mounted)");
        return;
    };
    let sectors = data.sectors();

    // Persistent mode: try to MOUNT an existing CIBOSFS first, so data written in
    // a prior boot survives. Only FORMAT (and seed /etc) when the disk has no
    // valid filesystem yet (first boot / wiped medium). A Live profile would
    // instead always format; that choice belongs to the boot profile (TODO 5a:
    // surface it as a profile flag — for now the kernel is Persistent).
    match Fs::mount(data) {
        Ok(fs) => {
            *ROOT_FS.lock() = Some(fs);
            kprintln!(
                "CIBOS kernel: root filesystem mounted (CIBOSFS, persistent) — {} sectors",
                sectors
            );
        }
        Err(FsError::BadSuperblock) => {
            // Unformatted medium: format and seed the base directories. Re-probe
            // because `mount` consumed the device.
            let Some(data) =
                (unsafe { crate::arch::ata::AtaDisk::probe(crate::arch::ata::Device::Slave) })
            else {
                kprintln!("CIBOS kernel: data disk vanished after mount attempt");
                return;
            };
            kprintln!(
                "CIBOS kernel: data disk unformatted — formatting CIBOSFS ({} sectors)",
                sectors
            );
            match Fs::format(data, 64).and_then(|mut fs| {
                fs.mkdir(b"/etc")?;
                fs.mkdir(b"/apps")?;
                // Seed the local package repository baked on the medium: a `/repo`
                // directory holding package files the package manager can install
                // without any network. The contents here must match what the
                // shell `.capp` declares the package's hash to be (it computes
                // sha256 over the same bytes), so integrity verification passes.
                fs.mkdir(b"/repo")?;
                fs.write_file(b"/repo/welcome", b"welcome to cibos")?;
                Ok(fs)
            }) {
                Ok(fs) => {
                    *ROOT_FS.lock() = Some(fs);
                    kprintln!(
                        "CIBOS kernel: root filesystem formatted (CIBOSFS), /etc /apps /repo ready"
                    );
                }
                Err(e) => kprintln!("CIBOS kernel: root fs format failed: {:?}", e),
            }
        }
        Err(e) => kprintln!("CIBOS kernel: root fs mount failed: {:?}", e),
    }
}

/// Probe the primary ATA bus and read back the boot medium to prove real block
/// I/O. Reads LBA 0 (the MBR — must end in the 0x55AA boot signature) and LBA 1
/// (the Boot Layout Descriptor — must carry the `CIBOSBL1` magic the image tool
/// wrote). Reading the genuine on-disk structures we booted from, through the
/// ATA driver, is the end-to-end storage proof.
/// Probe for a NIC at boot — PRODUCTION path, always run on x86_64 (exactly like
/// the ATA storage probe). Tries each supported NIC driver in turn and reports
/// the first present device. virtio-net is the first driver; e1000 (also a real,
/// standardized interface) is the next. QEMU/cloud/bare-metal all present one of
/// these — this is real hardware discovery, not a demo.
///
/// Returns whether a NIC was found, so the caller can later bind it under the
/// Lattice's NIC-backed transport.
#[cfg(target_arch = "x86_64")]
/// Send a real ARP request for the QEMU/LAN gateway and poll for the reply, to
/// verify a NIC's TX and RX paths end to end against the actual device. Real
/// frames only; honest reporting. Used by the boot NIC self-check (demo only).
#[cfg(all(target_arch = "x86_64", feature = "virtio-net-demo"))]
fn nic_arp_selfcheck(nic: &dyn cibos_kernel::net_device::NetDevice, label: &str) {
    let m = nic.mac();
    // Addressing: IP 10.0.2.15 (standard QEMU user-net guest), gateway 10.0.2.2.
    let our_ip = [10u8, 0, 2, 15];
    let gw_ip = [10u8, 0, 2, 2];
    let mut arp = [0u8; 42]; // 14 Ethernet + 28 ARP
    for b in arp.iter_mut().take(6) {
        *b = 0xFF; // dst broadcast
    }
    arp[6..12].copy_from_slice(&m); // src MAC
    arp[12] = 0x08;
    arp[13] = 0x06; // EtherType ARP
    arp[14] = 0x00;
    arp[15] = 0x01; // HTYPE Ethernet
    arp[16] = 0x08;
    arp[17] = 0x00; // PTYPE IPv4
    arp[18] = 6; // HLEN
    arp[19] = 4; // PLEN
    arp[20] = 0x00;
    arp[21] = 0x01; // OPER request
    arp[22..28].copy_from_slice(&m); // sender HW
    arp[28..32].copy_from_slice(&our_ip); // sender IP
    arp[38..42].copy_from_slice(&gw_ip); // target IP

    match nic.send_frame(&arp) {
        Ok(()) => kprintln!("CIBOS kernel: {label} TX: ARP request sent"),
        Err(e) => kprintln!("CIBOS kernel: {label} TX: {:?}", e),
    }

    let mut rxbuf = [0u8; 1514];
    let mut got_reply = false;
    'rx: for _ in 0..2_000_000u64 {
        if let Ok(Some(len)) = nic.recv_frame(&mut rxbuf) {
            if len >= 42 {
                let ethertype = u16::from_be_bytes([rxbuf[12], rxbuf[13]]);
                let oper = u16::from_be_bytes([rxbuf[20], rxbuf[21]]);
                if ethertype == 0x0806 && oper == 2 {
                    kprintln!(
                        "CIBOS kernel: {label} RX: ARP reply — gw {}.{}.{}.{} is at {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                        rxbuf[28], rxbuf[29], rxbuf[30], rxbuf[31],
                        rxbuf[22], rxbuf[23], rxbuf[24], rxbuf[25], rxbuf[26], rxbuf[27]
                    );
                    got_reply = true;
                    break 'rx;
                }
            }
        }
        core::hint::spin_loop();
    }
    if !got_reply {
        kprintln!("CIBOS kernel: {label} RX: no ARP reply within budget");
    }
}

/// Exercise the NIC-backed UDP transport end to end: send a DNS query for a name
/// to QEMU user-net's built-in resolver (10.0.2.3:53) and poll for the response.
/// Proves the whole stack — ARP resolve, Ethernet/IPv4/UDP build, NIC TX, NIC RX,
/// IPv4/UDP parse — works against a real service. Demo-only; honest reporting.
#[cfg(all(target_arch = "x86_64", feature = "virtio-net-demo"))]
fn net_stack_udp_selfcheck() {
    use cibos_net::Ipv4Addr;
    // A minimal DNS query for "a.root-servers.net" type A, id 0x1234. (12-byte
    // header + QNAME + qtype + qclass.)
    let query: &[u8] = &[
        0x12, 0x34, // id
        0x01, 0x00, // flags: standard query, recursion desired
        0x00, 0x01, // QDCOUNT = 1
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // AN/NS/AR = 0
        0x01, b'a', 0x0c, b'r', b'o', b'o', b't', b'-', b's', b'e', b'r', b'v', b'e', b'r', b's',
        0x03, b'n', b'e', b't', 0x00, // QNAME
        0x00, 0x01, // QTYPE = A
        0x00, 0x01, // QCLASS = IN
    ];
    let dns = Ipv4Addr::new(10, 0, 2, 3);
    match crate::net_stack::udp_send_to(dns, 53, 5353, query) {
        Ok(_) => kprintln!("CIBOS kernel: net-stack UDP: DNS query sent to 10.0.2.3:53"),
        Err(e) => {
            kprintln!("CIBOS kernel: net-stack UDP: send failed: {:?}", e);
            return;
        }
    }
    let mut resp = [0u8; 512];
    for _ in 0..3_000_000u64 {
        match crate::net_stack::poll_udp(5353, &mut resp) {
            Ok(Some((src, sport, len))) => {
                kprintln!(
                    "CIBOS kernel: net-stack UDP: DNS reply from {}.{}.{}.{}:{} ({} bytes) — STACK OK",
                    src.0[0], src.0[1], src.0[2], src.0[3], sport, len
                );
                return;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        core::hint::spin_loop();
    }
    kprintln!("CIBOS kernel: net-stack UDP: no DNS reply within budget");
}

/// Exercise a REMOTE Lattice Link end to end: create a remote Link (UDP flow to
/// the QEMU DNS resolver) and round-trip a DNS query THROUGH the Link API
/// (link_send / link_recv), proving the Lattice's byte transport now rides the
/// NIC — the Gate/Link/Warden surface is unchanged; only the backing transport
/// widened. Demo-only; honest reporting.
#[cfg(all(target_arch = "x86_64", feature = "virtio-net-demo"))]
fn lattice_remote_link_selfcheck() {
    use cibos_kernel::SyscallEnv;
    // No NIC -> nothing to route over; skip cleanly (loopback Links still work).
    if !nic_present() {
        return;
    }
    let boundary = shared::BoundaryId(0x900);
    // Create the remote Link in the channel/Lattice table (handle in the same
    // space as local Links).
    let handle = {
        let mut guard = CHANNEL_TABLE.lock();
        let Some(table) = guard.as_mut() else {
            kprintln!("CIBOS kernel: lattice remote Link: no handle table");
            return;
        };
        table.connect_remote(boundary.0, cibos_net::Ipv4Addr::new(10, 0, 2, 3), 53, 5400)
    };
    // A minimal DNS query for "a.root-servers.net" type A, id 0x4321.
    let query: &[u8] = &[
        0x43, 0x21, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, b'a', 0x0c,
        b'r', b'o', b'o', b't', b'-', b's', b'e', b'r', b'v', b'e', b'r', b's', 0x03, b'n', b'e',
        b't', 0x00, 0x00, 0x01, 0x00, 0x01,
    ];
    let env = KernelSyscallEnv;
    match env.link_send(boundary, handle, query) {
        Ok(()) => kprintln!("CIBOS kernel: lattice remote Link: query sent via link_send"),
        Err(e) => {
            kprintln!("CIBOS kernel: lattice remote Link: link_send failed: {:?}", e);
            return;
        }
    }
    for _ in 0..3_000_000u64 {
        match env.link_recv(boundary, handle) {
            Ok(bytes) if !bytes.is_empty() => {
                kprintln!(
                    "CIBOS kernel: lattice remote Link: reply via link_recv ({} bytes) — REMOTE LINK OK",
                    bytes.len()
                );
                return;
            }
            Ok(_) => {}
            Err(_) => {} // WouldBlock while polling
        }
        core::hint::spin_loop();
    }
    kprintln!("CIBOS kernel: lattice remote Link: no reply within budget");
}

/// Probe for a NIC at boot (production path, like the ATA storage probe). Tries
/// each supported driver in turn — virtio-net first (cloud/VM/SR-IOV), then the
/// e1000 (ubiquitous physical NIC) — and installs the FIRST present device into
/// the `NIC` kernel-global so the Lattice's transport can use it. Honestly
/// reports when no supported NIC is found (loopback-only networking). Runs after
/// the MMU is online (the drivers' DMA addresses must be stable).
#[cfg(target_arch = "x86_64")]
fn probe_nic_at_boot(frames: &cibos_kernel::FrameAllocator) -> bool {
    use cibos_kernel::net_device::NetDevice;
    // SAFETY: single-threaded bring-up; touches PCI config ports + the device
    // BARs once, and allocates DMA memory from `frames`.
    // 1) virtio-net.
    if let Some(nic) = unsafe { crate::arch::virtio_net::VirtioNet::probe(frames) } {
        let m = nic.mac();
        kprintln!(
            "CIBOS kernel: NIC: virtio-net MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, link {}",
            m[0], m[1], m[2], m[3], m[4], m[5],
            if nic.link_up() { "up" } else { "down" }
        );
        #[cfg(feature = "virtio-net-demo")]
        {
            kprintln!("CIBOS kernel: virtio-net RX/TX virtqueues set up; DRIVER_OK asserted");
            nic_arp_selfcheck(&nic, "virtio-net");
        }
        *NIC.lock() = Some(alloc::boxed::Box::new(nic));
        crate::net_stack::configure(m);
        #[cfg(feature = "virtio-net-demo")]
        net_stack_udp_selfcheck();
        return true;
    }
    // 2) e1000 (Intel 82540EM) — for non-virtio bare metal.
    if let Some(nic) = unsafe { crate::arch::e1000::E1000::probe(frames) } {
        let m = nic.mac();
        kprintln!(
            "CIBOS kernel: NIC: e1000 MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, link {}",
            m[0], m[1], m[2], m[3], m[4], m[5],
            if nic.link_up() { "up" } else { "down" }
        );
        #[cfg(feature = "virtio-net-demo")]
        nic_arp_selfcheck(&nic, "e1000");
        *NIC.lock() = Some(alloc::boxed::Box::new(nic));
        crate::net_stack::configure(m);
        return true;
    }
    // No supported NIC present. Honest report; networking falls back to loopback.
    kprintln!("CIBOS kernel: NIC: no supported NIC found (loopback only)");
    false
}

#[cfg(target_arch = "x86_64")]
fn verify_storage() {
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

/// IPC self-test: open a local channel and round-trip a message through the
/// real syscall ABI (`OpenChannel` -> `ChannelSend` -> `ChannelRecv`), then
/// prove bounded back-pressure (`WouldBlock` on a full buffer). Drives
/// [`handle_syscall`] directly with ABI registers — the same entry the ring-3
/// trap stub uses — so this exercises number decoding, argument marshalling, the
/// user-buffer copy, and the kernel channel table end to end.
/// Portable in-kernel channel demo, runnable on EVERY architecture.
///
/// Unlike [`demonstrate_channel`] (x86_64-only, which drives the ring-3 syscall
/// ABI), this exercises the canonical cross-lane IPC model entirely inside the
/// kernel: two cooperative lanes share a bounded [`Channel`] and are driven by
/// the single selector's `run_until_idle` loop. It proves the Catch-and-Release
/// model — bounded buffer, back-pressure parking, and resume-on-signal — works
/// on aarch64/riscv64/i686 as well as x86_64, since it is pure `cibos-kernel`
/// Rust with no arch-specific dependency.
#[cfg(feature = "channel-demo")]
fn demonstrate_kernel_channel(kernel: &mut Kernel) {
    use alloc::sync::Arc;
    use shared::protocols::ipc::{ChannelDirection, ChannelTerms};

    kprintln!("CIBOS kernel: --- in-kernel channel IPC demo (portable) ---");

    // Capacity 1, max 64 bytes: the second send must park until the receiver
    // drains the first message (proves back-pressure across lanes).
    let terms = match ChannelTerms::new("kdemo", ChannelDirection::Bidirectional, 64, 1) {
        Ok(t) => t,
        Err(_) => {
            kprintln!("CIBOS kernel: channel terms rejected (demo skipped)");
            return;
        }
    };
    let channel = Arc::new(kernel.create_channel(&terms));

    let tx = Arc::clone(&channel);
    let _ = kernel.spawn_with_lane(WeightClass::System, move |lane| async move {
        // First message goes into the single free slot.
        match tx.send(lane, alloc::vec![b'p', b'i', b'n', b'g']).await {
            Ok(()) => kprintln!("CIBOS kernel:   tx lane: sent 'ping'"),
            Err(_) => kprintln!("CIBOS kernel:   tx lane: send 1 failed"),
        }
        // Second message parks here (buffer full) until the rx lane drains a
        // slot, then resumes — Catch-and-Release back-pressure, no busy-wait.
        match tx.send(lane, alloc::vec![b'p', b'o', b'n', b'g']).await {
            Ok(()) => kprintln!("CIBOS kernel:   tx lane: sent 'pong' (after back-pressure)"),
            Err(_) => kprintln!("CIBOS kernel:   tx lane: send 2 failed"),
        }
    });

    let rx = Arc::clone(&channel);
    let _ = kernel.spawn_with_lane(WeightClass::User, move |lane| async move {
        for _ in 0..2 {
            match rx.recv(lane).await {
                Ok(msg) => {
                    let n = msg.len();
                    // Show the bytes if they are the printable demo payloads.
                    if msg.as_slice() == b"ping" {
                        kprintln!("CIBOS kernel:   rx lane: received 'ping' ({n} bytes)");
                    } else if msg.as_slice() == b"pong" {
                        kprintln!("CIBOS kernel:   rx lane: received 'pong' ({n} bytes)");
                    } else {
                        kprintln!("CIBOS kernel:   rx lane: received {n} bytes");
                    }
                }
                Err(_) => kprintln!("CIBOS kernel:   rx lane: recv failed"),
            }
        }
    });

    kprintln!("CIBOS kernel: in-kernel channel demo lanes spawned");
}

#[cfg(all(target_arch = "x86_64", feature = "channel-demo"))]
fn demonstrate_channel() {
    use shared::protocols::syscall::{Syscall, SyscallError};

    kprintln!("CIBOS kernel: --- local channel IPC demo ---");

    // Open a channel with capacity 1 message of up to 64 bytes.
    let handle = handle_syscall(Syscall::OpenChannel.number(), 1, 64, 0);
    if handle < 0 {
        kprintln!("CIBOS kernel: channel demo — open failed ({})", handle);
        return;
    }
    kprintln!("CIBOS kernel: channel opened (handle {})", handle);

    // Send "ping" through the channel. The payload lives on the kernel stack;
    // its address serves as the user pointer (copy_from_user reads kernel-mapped
    // memory in this pre-ring-3 step).
    let msg = b"ping";
    let send = handle_syscall(
        Syscall::ChannelSend.number(),
        handle as u64,
        msg.as_ptr() as u64,
        msg.len() as u64,
    );
    kprintln!(
        "CIBOS kernel: channel send -> {}",
        if send == 0 { "OK" } else { "ERR" }
    );

    // A second send must hit back-pressure (capacity 1, buffer full) -> WouldBlock.
    let send2 = handle_syscall(
        Syscall::ChannelSend.number(),
        handle as u64,
        msg.as_ptr() as u64,
        msg.len() as u64,
    );
    let blocked = SyscallError::from_return(send2) == Some(SyscallError::WouldBlock);
    kprintln!(
        "CIBOS kernel: channel send (full) -> {}",
        if blocked {
            "WouldBlock (back-pressure OK)"
        } else {
            "UNEXPECTED"
        }
    );

    // Receive into a stack buffer and report what came back.
    let mut buf = [0u8; 64];
    let n = handle_syscall(
        Syscall::ChannelRecv.number(),
        handle as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    );
    if n >= 0 {
        let got = &buf[..n as usize];
        kprintln!(
            "CIBOS kernel: channel recv -> {} byte(s): {}",
            n,
            core::str::from_utf8(got).unwrap_or("<non-utf8>")
        );
    } else {
        kprintln!("CIBOS kernel: channel recv failed ({})", n);
    }

    // After draining, a further recv must report WouldBlock (empty but open).
    let n2 = handle_syscall(
        Syscall::ChannelRecv.number(),
        handle as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    );
    let empty = SyscallError::from_return(n2) == Some(SyscallError::WouldBlock);
    kprintln!(
        "CIBOS kernel: channel recv (empty) -> {}",
        if empty {
            "WouldBlock (drained OK)"
        } else {
            "UNEXPECTED"
        }
    );
    kprintln!("CIBOS kernel: local channel IPC demo exited");
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
unsafe fn arm_keyboard_input() {
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
pub(crate) struct KernelSyscallEnv;

/// The Flattened Device Tree pointer the firmware/QEMU passed at boot (0 if
/// none). Read at runtime to discover the real platform layout (RAM, devices)
/// instead of using compiled-in constants. Only populated on arches that receive
/// a DTB (aarch64/riscv64); x86_64 gets its layout from the BIOS handoff.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
static DTB_PTR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Parse the firmware DTB (if present and valid) and return the primary RAM
/// region `(base, size)`. Returns `None` if no DTB was passed or it could not be
/// parsed — callers then fall back to the platform's conventional layout. This is
/// the mechanism that lets the same kernel boot on QEMU and real boards.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn dtb_ram_region() -> Option<(u64, u64)> {
    let ptr = DTB_PTR.load(core::sync::atomic::Ordering::Relaxed);
    if ptr == 0 {
        return None;
    }
    // SAFETY: read the FDT header to learn the blob's total size, then bound a
    // slice to it. The firmware placed a readable FDT here (or 0, handled above).
    let header = unsafe { core::slice::from_raw_parts(ptr as *const u8, 40) };
    let total = cibos_dtb::DeviceTree::totalsize_at(header)?;
    if total < 40 || total > 16 * 1024 * 1024 {
        return None; // implausible size; ignore
    }
    let blob = unsafe { core::slice::from_raw_parts(ptr as *const u8, total) };
    let dt = cibos_dtb::DeviceTree::new(blob).ok()?;
    dt.ram_region().ok()
}

/// Parse the firmware DTB (if present) and return the base address of the first
/// device node whose name starts with `prefix` (e.g. `b"pl011"` for the PL011
/// UART). Returns `None` if no DTB was passed or the device is absent. Lets the
/// kernel discover peripheral addresses at runtime instead of hardcoding them.
/// (aarch64 today; riscv64's console goes through OpenSBI/SBI calls, so it has no
/// MMIO peripheral to discover yet — this will extend there when a PLIC/CLINT
/// driver lands.)
#[cfg(target_arch = "aarch64")]
fn dtb_device_base(prefix: &[u8]) -> Option<u64> {
    let ptr = DTB_PTR.load(core::sync::atomic::Ordering::Relaxed);
    if ptr == 0 {
        return None;
    }
    // SAFETY: read the FDT header for the total size, then bound a slice to it.
    let header = unsafe { core::slice::from_raw_parts(ptr as *const u8, 40) };
    let total = cibos_dtb::DeviceTree::totalsize_at(header)?;
    if total < 40 || total > 16 * 1024 * 1024 {
        return None;
    }
    let blob = unsafe { core::slice::from_raw_parts(ptr as *const u8, total) };
    let dt = cibos_dtb::DeviceTree::new(blob).ok()?;
    dt.device_base(prefix).ok()
}
/// disk). When mounted, the filesystem syscalls operate on it; when absent they
/// report `NotPermitted`. Guarded by a spinlock so the trap path and any setup
/// code do not race.
#[cfg(target_arch = "x86_64")]
static ROOT_FS: cibos_kernel::sync::SpinLock<
    Option<cibos_kernel::fs::Fs<crate::arch::ata::AtaDisk>>,
> = cibos_kernel::sync::SpinLock::new(None);

/// The network interface discovered at boot, stored so the Lattice's NIC-backed
/// transport can use it (mirrors the `ROOT_FS` kernel-global pattern). Held as a
/// boxed `NetDevice` trait object so any driver (virtio-net, e1000, …) installs
/// uniformly. `None` when no supported NIC is present (loopback-only networking).
#[cfg(target_arch = "x86_64")]
static NIC: cibos_kernel::sync::SpinLock<
    Option<alloc::boxed::Box<dyn cibos_kernel::net_device::NetDevice + Send>>,
> = cibos_kernel::sync::SpinLock::new(None);

/// Run `f` against the installed NIC if one is present, returning its result (or
/// `None` if no NIC is installed). The Lattice's NIC-backed transport uses this
/// to send/receive frames over whatever driver was bound at boot. Brief lock
/// hold; never hold across blocking work.
///
/// Wired into the Lattice transport in N5; defined now as the storage seam.
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
pub fn with_nic<R>(
    f: impl FnOnce(&dyn cibos_kernel::net_device::NetDevice) -> R,
) -> Option<R> {
    let guard = NIC.lock();
    guard.as_ref().map(|nic| f(nic.as_ref()))
}

/// Whether a NIC is currently installed (a real interface was found at boot).
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
#[must_use]
pub fn nic_present() -> bool {
    NIC.lock().is_some()
}

/// The kernel CSPRNG backing the `get_random` syscall, seeded from the firmware
/// entropy seed in the handoff at bring-up. `None` until seeded.
#[cfg(target_arch = "x86_64")]
static KERNEL_RNG: cibos_kernel::sync::SpinLock<Option<cibos_kernel::entropy::Csprng>> =
    cibos_kernel::sync::SpinLock::new(None);

/// A boundary-aware handle table over the CANONICAL `Channel` (the real channel
/// system: terms, sender/receiver waiters, KernelInterface back-pressure). Maps
/// `(boundary, handle) -> Channel`. Both endpoints of a cross-boundary channel
/// register a handle pointing at the SAME `Channel` (the canonical handle is a
/// cheap Arc-backed clone), so bytes sent by one boundary are received by the
/// other THROUGH THE KERNEL — never via shared user memory. This unifies the
/// syscall channel path onto the canonical Channel (no separate LocalChannel).
#[cfg(target_arch = "x86_64")]
struct ChannelHandleTable {
    next_handle: u64,
    /// (boundary, handle) -> the shared canonical channel.
    handles: alloc::collections::BTreeMap<(u64, u64), cibos_kernel::channel::Channel>,
    /// The registry that mints channels and runs the request/accept handshake.
    registry: cibos_kernel::channel::ChannelRegistry,
    /// Accepted requests awaiting the requester's outcome poll: request_id ->
    /// (requester boundary, the handle minted for the requester). Populated by
    /// `accept_channel`, consumed by `poll_channel_outcome`. This is how the
    /// REQUESTER learns its endpoint handle after the target accepted.
    accepted: alloc::collections::BTreeMap<u64, (u64, u64)>,
    /// The kernel-side Lattice: Gate registry (bind/connect/accept/Warden) whose
    /// Links are canonical Channels registered in `handles` like any other.
    gates: cibos_kernel::gate::GateRegistry,
    /// Monotonic source of ChannelIds for Lattice Links (each Link is one Channel).
    next_channel_id: u64,
    /// Remote Links: (boundary, handle) -> a NIC-backed UDP flow. A Link handle
    /// resolves to EITHER a local Channel (in `handles`) or a remote UDP flow
    /// here; link_send/link_recv dispatch on which. This is how the Lattice's
    /// byte transport widens to the NIC without any ABI/surface change.
    remote_links: alloc::collections::BTreeMap<(u64, u64), crate::net_stack::RemoteLink>,
    /// The scheduler used as the channels' KernelInterface for back-pressure
    /// wakeups (the SAME selector that dispatches the ring-3 lanes).
    kernel: alloc::sync::Arc<dyn shared::KernelInterface>,
}

#[cfg(target_arch = "x86_64")]
impl ChannelHandleTable {
    fn new(kernel: alloc::sync::Arc<dyn shared::KernelInterface>) -> Self {
        Self {
            next_handle: 0,
            handles: alloc::collections::BTreeMap::new(),
            registry: cibos_kernel::channel::ChannelRegistry::new(),
            accepted: alloc::collections::BTreeMap::new(),
            gates: cibos_kernel::gate::GateRegistry::new(),
            next_channel_id: 0x1000_0000,
            remote_links: alloc::collections::BTreeMap::new(),
            kernel,
        }
    }

    /// Register `channel` under `boundary`, returning the new handle.
    #[cfg_attr(not(feature = "ring3-multilane-demo"), allow(dead_code))]
    fn register(&mut self, boundary: u64, channel: cibos_kernel::channel::Channel) -> u64 {
        let handle = self.next_handle;
        self.next_handle += 1;
        self.handles.insert((boundary, handle), channel);
        handle
    }

    /// Resolve a `(boundary, handle)` to its channel (a cheap clone-handle).
    fn resolve(&self, boundary: u64, handle: u64) -> Option<cibos_kernel::channel::Channel> {
        self.handles.get(&(boundary, handle)).cloned()
    }

    /// Create a REMOTE Link for `boundary`: a NIC-backed UDP flow to
    /// `(remote_ip, remote_port)` listening on `local_port`, minting a handle in
    /// the same handle space as local Links. link_send/link_recv on this handle
    /// route over the NIC. This is the kernel-internal entry the remote-gate
    /// addressing model calls; the Gate/Link/Warden surface is unchanged.
    #[cfg_attr(not(feature = "virtio-net-demo"), allow(dead_code))]
    fn connect_remote(
        &mut self,
        boundary: u64,
        remote_ip: cibos_net::Ipv4Addr,
        remote_port: u16,
        local_port: u16,
    ) -> u64 {
        let handle = self.next_handle;
        self.next_handle += 1;
        self.remote_links.insert(
            (boundary, handle),
            crate::net_stack::RemoteLink {
                local_port,
                remote_ip,
                remote_port,
            },
        );
        handle
    }

    /// Resolve a `(boundary, handle)` to a remote Link, if it is one.
    fn resolve_remote(&self, boundary: u64, handle: u64) -> Option<crate::net_stack::RemoteLink> {
        self.remote_links.get(&(boundary, handle)).copied()
    }
}

/// The kernel's channel handle table (boundary-aware, over the canonical Channel).
/// Mirrors the `ROOT_FS` static pattern (spinlock-guarded, brief locks — never
/// held across a wait). Installed once the scheduler is available.
#[cfg(target_arch = "x86_64")]
static CHANNEL_TABLE: cibos_kernel::sync::SpinLock<Option<ChannelHandleTable>> =
    cibos_kernel::sync::SpinLock::new(None);

/// A handle to the kernel's syscall environment (a unit struct). Lets in-kernel
/// demonstrations exercise the SAME `SyscallEnv` methods the ring-3 dispatch
/// calls (e.g. the cross-boundary channel handshake), without a ring-3 trap.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
#[must_use]
pub fn kernel_syscall_env() -> KernelSyscallEnv {
    KernelSyscallEnv
}

/// Install the channel handle table, backed by `kernel` (the selector's
/// scheduler) for back-pressure wakeups. Idempotent for a run.
#[cfg(target_arch = "x86_64")]
pub fn install_channel_table(kernel: alloc::sync::Arc<dyn shared::KernelInterface>) {
    *CHANNEL_TABLE.lock() = Some(ChannelHandleTable::new(kernel));
}

/// Tear down the channel handle table after a run.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub fn clear_channel_table() {
    *CHANNEL_TABLE.lock() = None;
}

/// Seed the kernel CSPRNG from the firmware entropy seed. Called once at boot.
#[cfg(target_arch = "x86_64")]
fn seed_kernel_rng(seed: [u8; 32]) {
    *KERNEL_RNG.lock() = Some(cibos_kernel::entropy::Csprng::from_seed(seed));
}

/// Draw a 32-byte entropy seed from the kernel RNG for the multilane selector
/// (the Scheduler seeds its weighted-entropy CSPRNG from this). Falls back to a
/// fixed seed if the RNG is unavailable — the demo's correctness does not depend
/// on the seed value, only on the selector's Ready/Stalled mechanics.
#[cfg(target_arch = "x86_64")]
pub fn multilane_seed() -> [u8; 32] {
    let mut seed = [0x5Au8; 32];
    if let Some(rng) = KERNEL_RNG.lock().as_mut() {
        rng.fill_bytes(&mut seed);
    }
    seed
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

    fn sleep_nanos(&self, nanos: u64) -> Result<(), shared::protocols::syscall::SyscallError> {
        // Back the sleep with the PIT monotonic millisecond counter (10 ms
        // resolution). Busy-wait until the deadline: the timer IRQ advances the
        // tick counter, so `hlt` between checks lets the CPU idle until the next
        // interrupt rather than spinning hot. A future cooperative version will
        // yield the lane to the scheduler instead of waiting in-kernel.
        let millis = nanos / 1_000_000;
        if millis == 0 {
            return Ok(());
        }
        let start = crate::timer::now_millis();
        while crate::timer::now_millis().wrapping_sub(start) < millis {
            // SAFETY: enable interrupts and halt until the next (timer) IRQ, so
            // the monotonic counter keeps advancing while we wait.
            unsafe {
                core::arch::asm!("sti; hlt", options(nomem, nostack));
            }
        }
        Ok(())
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
        // True blocking read: sleep the CPU with `hlt` until a key actually
        // arrives, however long the user pauses (no deadline) — required for a
        // live interactive session. The injected selftest path never reaches here
        // because it pre-fills the queue, so `poll_key` above returns immediately.
        // SAFETY: interrupts are enabled and the keyboard IRQ is live by the time
        // user code runs, so a keystroke will eventually wake the `hlt`.
        unsafe {
            crate::timer::wait_for(crate::keyboard::has_key);
        }
        if let Some(ev) = crate::keyboard::poll_key() {
            return map(ev);
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

    fn open_channel(
        &self,
        boundary: shared::BoundaryId,
        capacity: usize,
        max_message_bytes: usize,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use shared::protocols::ipc::{ChannelDirection, ChannelTerms};
        use shared::protocols::syscall::SyscallError;

        let mut guard = CHANNEL_TABLE.lock();
        let table = guard.as_mut().ok_or(SyscallError::NotPermitted)?;

        // A same-boundary channel: build canonical terms and mint a real Channel
        // (the same kind cross-boundary accept produces), registered under the
        // caller's boundary. `OpenChannel` is the intra-boundary convenience; the
        // cross-boundary path goes through request/accept (see request_channel).
        let terms = ChannelTerms::new(
            "syscall",
            ChannelDirection::Bidirectional,
            max_message_bytes as u32,
            capacity.max(1) as u32,
        )
        .map_err(|_| SyscallError::InvalidArgument)?;
        let channel = table.registry.create(&terms, table.kernel.clone());
        Ok(table.register(boundary.0, channel))
    }

    fn channel_send(
        &self,
        boundary: shared::BoundaryId,
        handle: u64,
        data: &[u8],
    ) -> Result<(), shared::protocols::syscall::SyscallError> {
        use cibos_kernel::channel::SendStep;
        use shared::protocols::syscall::SyscallError;

        // Resolve (caller boundary, handle) -> the shared canonical channel under
        // a brief lock, then release before try_send (which takes the channel's
        // own lock and may register a back-pressure wait via the scheduler).
        let channel = {
            let guard = CHANNEL_TABLE.lock();
            let table = guard.as_ref().ok_or(SyscallError::NotFound)?;
            table.resolve(boundary.0, handle).ok_or(SyscallError::NotFound)?
        };

        // The sending lane is whichever ring-3 lane issued the syscall.
        let lane = current_syscall_lane();
        match channel.try_send(lane, data) {
            SendStep::Sent => Ok(()),
            // Full buffer: the lane is registered to wait (Catch-and-Release);
            // surface WouldBlock so a cooperative caller parks/retries.
            SendStep::Full => Err(SyscallError::WouldBlock),
            SendStep::Closed => Err(SyscallError::NotFound),
            SendStep::TooLarge => Err(SyscallError::InvalidArgument),
        }
    }

    fn channel_recv(
        &self,
        boundary: shared::BoundaryId,
        handle: u64,
    ) -> Result<alloc::vec::Vec<u8>, shared::protocols::syscall::SyscallError> {
        use cibos_kernel::channel::RecvStep;
        use shared::protocols::syscall::SyscallError;

        let channel = {
            let guard = CHANNEL_TABLE.lock();
            let table = guard.as_ref().ok_or(SyscallError::NotFound)?;
            table.resolve(boundary.0, handle).ok_or(SyscallError::NotFound)?
        };

        let lane = current_syscall_lane();
        match channel.try_recv(lane) {
            RecvStep::Message(bytes) => Ok(bytes),
            // Empty but open: the lane is registered to wait; surface WouldBlock.
            RecvStep::Empty => Err(SyscallError::WouldBlock),
            RecvStep::Closed => Err(SyscallError::NotFound),
        }
    }

    fn spawn(
        &self,
        boundary: shared::BoundaryId,
        entry: u64,
        arg: u64,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;

        // With the selector-owned Ring3Table installed (multilane), a ring-3
        // `spawn` creates a NEW cooperative lane in the CALLER'S boundary,
        // starting at `entry` (an address already mapped in the caller's space)
        // with a freshly-mapped stack. This is the join of roadmap 2a (the spawn
        // syscall) and 2b (the cooperative multi-context loop).
        #[cfg(feature = "ring3-multilane-demo")]
        {
            // Distinct stack per spawned lane (above the two seed lanes' region).
            static SPAWN_STACK_NEXT: core::sync::atomic::AtomicU64 =
                core::sync::atomic::AtomicU64::new(0x0000_5000_0200_0000);
            let stack_virt =
                SPAWN_STACK_NEXT.fetch_add(0x10_0000, core::sync::atomic::Ordering::SeqCst);

            // SAFETY: booted kernel, identity phys map, frames published for the
            // demo run. Maps into the current (caller's) space.
            let stack_top = match unsafe { crate::loader::map_spawn_stack(stack_virt) } {
                Ok(top) => top,
                Err(_) => return Err(SyscallError::NotPermitted),
            };

            let mut guard = crate::ring3::RING3_TABLE.lock();
            let Some(table) = guard.as_mut() else {
                return Err(SyscallError::NotPermitted);
            };
            let lane =
                table.spawn_lane(entry, stack_top, arg, boundary, shared::WeightClass::User);
            // `arg` is now marshaled into the new lane's rdi (see spawn_lane).
            #[allow(clippy::needless_return)]
            return Ok(lane.0 & (i64::MAX as u64));
        }

        // HONEST BOUNDARY (no multilane table): without the cooperative loop
        // installed there is no live ring-3 lane surface to spawn onto, so this
        // reports NotPermitted rather than fabricating a lane that runs nothing.
        #[cfg(not(feature = "ring3-multilane-demo"))]
        {
            let _ = (boundary, entry, arg);
            Err(SyscallError::NotPermitted)
        }
    }

    fn request_channel(
        &self,
        requester: shared::BoundaryId,
        target: shared::BoundaryId,
        terms: &shared::protocols::ipc::ChannelTerms,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use shared::protocols::ipc::ChannelRequest;
        use shared::protocols::syscall::SyscallError;
        let guard = CHANNEL_TABLE.lock();
        let table = guard.as_ref().ok_or(SyscallError::NotPermitted)?;
        let req = ChannelRequest { target, terms: terms.clone() };
        Ok(table.registry.request(requester, &req))
    }

    fn poll_channel_request(
        &self,
        target: shared::BoundaryId,
        out: &mut [u8],
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use shared::protocols::ipc::{ChannelRequestWire, ChannelTermsWire, CHANNEL_REQUEST_WIRE_LEN};
        use shared::protocols::syscall::SyscallError;
        let guard = CHANNEL_TABLE.lock();
        let table = guard.as_ref().ok_or(SyscallError::NotPermitted)?;
        let (id, requester, terms) = table.registry.poll(target).ok_or(SyscallError::NotFound)?;
        let wire = ChannelRequestWire {
            requester: requester.0,
            terms: ChannelTermsWire::from_terms(&terms),
        };
        let bytes = wire.to_bytes();
        if out.len() < CHANNEL_REQUEST_WIRE_LEN {
            return Err(SyscallError::InvalidArgument);
        }
        out[..CHANNEL_REQUEST_WIRE_LEN].copy_from_slice(&bytes);
        Ok(id)
    }

    fn accept_channel(
        &self,
        target: shared::BoundaryId,
        request_id: u64,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let mut guard = CHANNEL_TABLE.lock();
        let table = guard.as_mut().ok_or(SyscallError::NotPermitted)?;
        // Accept WHOLESALE: create the one canonical channel; register a handle
        // for BOTH the target (returned now) and the requester (stored for its
        // outcome poll). Both handles point at the SAME channel.
        let kernel = table.kernel.clone();
        let (channel, requester) = table
            .registry
            .accept(request_id, target, kernel)
            .ok_or(SyscallError::NotPermitted)?;
        let target_handle = table.register(target.0, channel.clone());
        let requester_handle = table.register(requester.0, channel);
        table.accepted.insert(request_id, (requester.0, requester_handle));
        Ok(target_handle)
    }

    fn reject_channel(
        &self,
        target: shared::BoundaryId,
        request_id: u64,
    ) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let guard = CHANNEL_TABLE.lock();
        let table = guard.as_ref().ok_or(SyscallError::NotPermitted)?;
        if table.registry.reject(request_id, target) {
            Ok(())
        } else {
            Err(SyscallError::NotFound)
        }
    }

    fn poll_channel_outcome(
        &self,
        requester: shared::BoundaryId,
        request_id: u64,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let mut guard = CHANNEL_TABLE.lock();
        let table = guard.as_mut().ok_or(SyscallError::NotPermitted)?;
        // Accepted: hand the requester its stored handle (consume the record).
        if let Some(&(req_boundary, handle)) = table.accepted.get(&request_id) {
            if req_boundary == requester.0 {
                table.accepted.remove(&request_id);
                return Ok(handle);
            }
            // A different boundary cannot claim this outcome.
            return Err(SyscallError::NotPermitted);
        }
        // Still pending -> WouldBlock; otherwise rejected/unknown -> NotFound.
        if table.registry.is_pending(request_id) {
            Err(SyscallError::WouldBlock)
        } else {
            Err(SyscallError::NotFound)
        }
    }

    fn gate_bind(
        &self,
        owner: shared::BoundaryId,
        gate: u16,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use cibos_kernel::gate::GateError;
        use shared::protocols::syscall::SyscallError;
        let mut guard = CHANNEL_TABLE.lock();
        let table = guard.as_mut().ok_or(SyscallError::NotPermitted)?;
        match table.gates.bind(owner, gate) {
            Ok(()) => Ok(u64::from(gate)), // the listener handle IS the gate number
            Err(GateError::Blocked) | Err(GateError::AlreadyBound) => Err(SyscallError::NotPermitted),
            Err(_) => Err(SyscallError::InvalidArgument),
        }
    }

    fn gate_connect(
        &self,
        from: shared::BoundaryId,
        gate: u16,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use cibos_kernel::gate::GateError;
        use shared::protocols::syscall::SyscallError;
        let mut guard = CHANNEL_TABLE.lock();
        let table = guard.as_mut().ok_or(SyscallError::NotPermitted)?;
        // Mint a fresh ChannelId for this Link and connect.
        let cid = shared::ChannelId::new(table.next_channel_id);
        table.next_channel_id += 1;
        let kernel = table.kernel.clone();
        let link = match table.gates.connect(from, gate, kernel, cid) {
            Ok(l) => l,
            Err(GateError::Blocked) => return Err(SyscallError::NotPermitted),
            Err(GateError::Refused) => return Err(SyscallError::NotFound),
            Err(_) => return Err(SyscallError::InvalidArgument),
        };
        // Register the connector's half as a Link handle in this boundary.
        Ok(table.register(from.0, link))
    }

    fn gate_accept(
        &self,
        owner: shared::BoundaryId,
        gate: u16,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use cibos_kernel::gate::GateError;
        use shared::protocols::syscall::SyscallError;
        let mut guard = CHANNEL_TABLE.lock();
        let table = guard.as_mut().ok_or(SyscallError::NotPermitted)?;
        let (link, _from) = match table.gates.accept(owner, gate) {
            Ok(v) => v,
            Err(GateError::WouldBlock) => return Err(SyscallError::WouldBlock),
            Err(GateError::NotOwner) => return Err(SyscallError::NotPermitted),
            Err(_) => return Err(SyscallError::NotFound),
        };
        // Register the listener's half as a Link handle in the owner's boundary.
        Ok(table.register(owner.0, link))
    }

    fn link_send(
        &self,
        boundary: shared::BoundaryId,
        handle: u64,
        data: &[u8],
    ) -> Result<(), shared::protocols::syscall::SyscallError> {
        use cibos_kernel::channel::SendStep;
        use shared::protocols::syscall::SyscallError;
        // Resolve the handle to a local Channel-backed Link or a remote
        // NIC-backed Link. Same ABI; the kernel dispatches on which.
        let (link, remote) = {
            let guard = CHANNEL_TABLE.lock();
            let table = guard.as_ref().ok_or(SyscallError::NotPermitted)?;
            match table.resolve(boundary.0, handle) {
                Some(ch) => (Some(ch), None),
                None => (
                    None,
                    Some(
                        table
                            .resolve_remote(boundary.0, handle)
                            .ok_or(SyscallError::NotFound)?,
                    ),
                ),
            }
        };
        if let Some(rl) = remote {
            // Remote Link: send one UDP datagram over the NIC.
            return match rl.send(data) {
                Ok(_) => Ok(()),
                Err(crate::net_stack::TransportError::Net(_)) => {
                    Err(SyscallError::InvalidArgument)
                }
                Err(_) => Err(SyscallError::WouldBlock),
            };
        }
        let link = link.ok_or(SyscallError::NotFound)?;
        match link.try_send(current_syscall_lane(), data) {
            SendStep::Sent => Ok(()),
            SendStep::Full => Err(SyscallError::WouldBlock),
            SendStep::Closed => Err(SyscallError::NotFound),
            SendStep::TooLarge => Err(SyscallError::InvalidArgument),
        }
    }

    fn link_recv(
        &self,
        boundary: shared::BoundaryId,
        handle: u64,
    ) -> Result<alloc::vec::Vec<u8>, shared::protocols::syscall::SyscallError> {
        use cibos_kernel::channel::RecvStep;
        use shared::protocols::syscall::SyscallError;
        let (link, remote) = {
            let guard = CHANNEL_TABLE.lock();
            let table = guard.as_ref().ok_or(SyscallError::NotPermitted)?;
            match table.resolve(boundary.0, handle) {
                Some(ch) => (Some(ch), None),
                None => (
                    None,
                    Some(
                        table
                            .resolve_remote(boundary.0, handle)
                            .ok_or(SyscallError::NotFound)?,
                    ),
                ),
            }
        };
        if let Some(rl) = remote {
            // Remote Link: poll the NIC for one datagram from our peer.
            let mut buf = [0u8; 1500];
            return match rl.recv(&mut buf) {
                Ok(Some(len)) => Ok(buf[..len].to_vec()),
                Ok(None) => Err(SyscallError::WouldBlock),
                Err(_) => Err(SyscallError::WouldBlock),
            };
        }
        let link = link.ok_or(SyscallError::NotFound)?;
        match link.try_recv(current_syscall_lane()) {
            RecvStep::Message(bytes) => Ok(bytes),
            RecvStep::Empty => Err(SyscallError::WouldBlock),
            RecvStep::Closed => Err(SyscallError::NotFound),
        }
    }

    fn link_close(
        &self,
        boundary: shared::BoundaryId,
        handle: u64,
    ) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let link = {
            let guard = CHANNEL_TABLE.lock();
            let table = guard.as_ref().ok_or(SyscallError::NotPermitted)?;
            table.resolve(boundary.0, handle).ok_or(SyscallError::NotFound)?
        };
        link.close();
        Ok(())
    }

    fn warden_set(
        &self,
        _boundary: shared::BoundaryId,
        gate: u16,
        allow: bool,
    ) -> Result<(), shared::protocols::syscall::SyscallError> {
        use shared::protocols::syscall::SyscallError;
        let guard = CHANNEL_TABLE.lock();
        let table = guard.as_ref().ok_or(SyscallError::NotPermitted)?;
        if allow {
            table.gates.warden_allow(gate);
        } else {
            table.gates.warden_deny(gate);
        }
        Ok(())
    }

    fn gate_probe(
        &self,
        _boundary: shared::BoundaryId,
        gate: u16,
    ) -> Result<u64, shared::protocols::syscall::SyscallError> {
        use cibos_kernel::gate::GateState;
        use shared::protocols::syscall::SyscallError;
        let guard = CHANNEL_TABLE.lock();
        let table = guard.as_ref().ok_or(SyscallError::NotPermitted)?;
        Ok(match table.gates.probe(gate) {
            GateState::Closed => 0,
            GateState::Open => 1,
            GateState::Blocked => 2,
        })
    }
}

/// Bridge from the architecture trap stub to the portable dispatcher. Called by
/// `cibos_syscall_handler` with the ABI registers; returns the value for `rax`.
#[cfg(target_arch = "x86_64")]
pub fn handle_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    use cibos_kernel::{dispatch_syscall, SyscallOutcome, SyscallRequest};

    // Attribute the syscall to the RUNNING ring-3 lane's real boundary when a
    // selector-owned lane is active; otherwise (normal .capp / single-resume /
    // kernel paths) attribute to the system boundary. This is what makes the
    // dispatcher's boundary-aware calls (open_channel, spawn) enforce the real
    // security principal instead of a stand-in.
    let boundary = {
        #[cfg(feature = "ring3-multilane-demo")]
        {
            let lane = active_lane();
            if lane.0 != 0 {
                crate::ring3::RING3_TABLE
                    .lock()
                    .as_ref()
                    .and_then(|t| t.boundary_of(lane))
                    .unwrap_or(shared::BoundaryId::SYSTEM)
            } else {
                shared::BoundaryId::SYSTEM
            }
        }
        #[cfg(not(feature = "ring3-multilane-demo"))]
        {
            shared::BoundaryId::SYSTEM
        }
    };

    let req = SyscallRequest {
        number,
        arg0,
        arg1,
        arg2,
        boundary,
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

// ---- Per-lane ring-3 context: park + resume (the live-context prerequisite) --
//
// The context-saving trap stub (`user_ctx_trap_entry` in resume_user.s) saves
// the trapped lane's FULL register state into `USER_CTX_SAVE` before calling
// `handle_user_trap`. This is the load-bearing mechanism the live-context design
// note identifies as the shared prerequisite for `spawn` and cross-boundary
// channels: a ring-3 lane the kernel can park and resume exactly where it
// trapped.
// ---- Per-lane ring-3 context: park + resume (the live-context prerequisite) --
//
// The context-saving trap stub (`user_ctx_trap_entry` in resume_user.s) saves
// the trapped lane's FULL register state into `*CURRENT_USER_CTX` before calling
// `handle_user_trap`. This is the load-bearing mechanism the live-context design
// note identifies as the shared prerequisite for `spawn` and cross-boundary
// channels. Shared by both the single-lane resume demo and the multilane
// (selector-owned table) demo.

#[cfg(all(target_arch = "x86_64", any(feature = "ring3-resume-demo", feature = "ring3-multilane-demo")))]
extern "C" {
    /// Set by the kernel to point at the running lane's `SavedUserContext`; the
    /// trap stub saves into `*CURRENT_USER_CTX`. Switching lanes repoints this.
    pub static mut CURRENT_USER_CTX: *mut arch::ring3_ctx::SavedUserContext;
    /// Unwind a resumed lane's `exit` to the `resume_user_context` call site.
    fn return_to_saved_kernel(code: i64) -> !;
}

/// The kernel-side register snapshot a `resume_user_context` saves so a resumed
/// lane's `exit` can unwind back to the kernel. Caller-owned (one per in-flight
/// resume) — mirrors `enter_user.s`'s KERNEL_CTX but without the single-global
/// limitation, so nested/sequential resumes stay correct. Layout (8 quadwords):
/// rsp, rbx, rbp, r12, r13, r14, r15, return_addr.
#[cfg(all(target_arch = "x86_64", any(feature = "ring3-resume-demo", feature = "ring3-multilane-demo")))]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KernelReturnContext {
    slots: [u64; 8],
}

#[cfg(all(target_arch = "x86_64", any(feature = "ring3-resume-demo", feature = "ring3-multilane-demo")))]
impl KernelReturnContext {
    pub const fn zeroed() -> Self {
        Self { slots: [0; 8] }
    }
}

/// Point `CURRENT_USER_CTX` at `ctx` (the running lane's context). The selector
/// calls this before resuming each lane, so a trap saves back into that lane.
///
/// # Safety
/// `ctx` must outlive the lane's run (the Ring3Table owns it for the demo).
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub unsafe fn set_current_user_ctx(ctx: *mut arch::ring3_ctx::SavedUserContext) {
    CURRENT_USER_CTX = ctx;
}

/// In the multilane loop EVERY lane is entered via `resume_user_context`, so
/// every lane's `exit` must unwind through `return_to_saved_kernel`. The table
/// calls this before resuming a lane; `handle_user_trap` then routes that lane's
/// `Exit` accordingly. `LaneId` recorded for the boundary lookup (step 4).
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
static ACTIVE_LANE_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub fn set_active_lane(lane: shared::LaneId) {
    ACTIVE_LANE_ID.store(lane.0, core::sync::atomic::Ordering::SeqCst);
    // Every multilane lane is entered via resume_user_context: route its exit.
    LANE_WAS_RESUMED.store(true, core::sync::atomic::Ordering::SeqCst);
}

/// Clear the active-lane record (0 = none) once a lane returns to the selector,
/// so syscalls issued outside any ring-3 lane attribute to the system boundary.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub fn clear_active_lane() {
    ACTIVE_LANE_ID.store(0, core::sync::atomic::Ordering::SeqCst);
}

/// The lane the selector is currently running (0 = none). Read by the trap so
/// per-lane decisions (boundary, exit routing) use the right lane. Used by
/// step 4 to read the lane's real boundary instead of hardcoded SYSTEM.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
#[must_use]
pub fn active_lane() -> shared::LaneId {
    shared::LaneId(ACTIVE_LANE_ID.load(core::sync::atomic::Ordering::SeqCst))
}

/// The lane id to attribute a channel syscall to: the running ring-3 lane under
/// the multilane selector, or lane 0 otherwise (single-run paths). Channels use
/// this as the waiter id for back-pressure.
#[cfg(target_arch = "x86_64")]
fn current_syscall_lane() -> shared::LaneId {
    #[cfg(feature = "ring3-multilane-demo")]
    {
        let l = active_lane();
        if l.0 != 0 {
            return l;
        }
    }
    shared::LaneId(0)
}

/// Whether the next `Yield` should PARK the lane (true once, for the single-lane
/// resume demo). The multilane loop parks on every yield via its own handler.
#[cfg(all(target_arch = "x86_64", feature = "ring3-resume-demo"))]
static PARK_NEXT_YIELD: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(true);

/// Set true once a lane has been resumed via `resume_user_context`, so its
/// `exit` unwinds through `return_to_saved_kernel` (the per-resume kernel
/// context) rather than the original `enter_user_context` slot.
#[cfg(all(target_arch = "x86_64", any(feature = "ring3-resume-demo", feature = "ring3-multilane-demo")))]
static LANE_WAS_RESUMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Mark that a lane has been resumed, so its `exit` routes through
/// `return_to_saved_kernel`. Called just before `resume_user_context`.
#[cfg(all(target_arch = "x86_64", feature = "ring3-resume-demo"))]
pub fn mark_lane_resumed() {
    LANE_WAS_RESUMED.store(true, core::sync::atomic::Ordering::SeqCst);
}

/// Rust handler for the context-saving trap path. Mirrors `handle_syscall` but
/// adds the park decision. On `Yield` the lane parks (its full context is saved
/// in `*CURRENT_USER_CTX`); the kernel resumes it later. A lane entered via
/// `resume_user_context` routes its `Exit` through `return_to_saved_kernel`.
#[cfg(all(target_arch = "x86_64", any(feature = "ring3-resume-demo", feature = "ring3-multilane-demo")))]
pub fn handle_user_trap(number: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    use core::sync::atomic::Ordering;
    use shared::protocols::syscall::Syscall;

    // Single-lane demo: park only on the FIRST yield (one round-trip proof).
    #[cfg(feature = "ring3-resume-demo")]
    if number == Syscall::Yield.number() && PARK_NEXT_YIELD.swap(false, Ordering::SeqCst) {
        // SAFETY: reached from a ring-3 trap; the user context is already saved.
        unsafe { crate::loader::return_to_kernel(crate::loader::PARKED_SENTINEL) }
    }

    // Multilane demo: EVERY yield parks the lane and returns to the selector,
    // which then dispatches the next ready lane. The lane was entered via
    // resume_user_context, so we unwind through ITS saved kernel frame
    // (return_to_saved_kernel) — NOT return_to_kernel (whose KERNEL_CTX is unset
    // on this path). The PARKED_SENTINEL tells the selector the lane parked.
    #[cfg(feature = "ring3-multilane-demo")]
    if number == Syscall::Yield.number() {
        // SAFETY: the lane was entered via resume_user_context (ACTIVE_KERNEL_CTX
        // points at its saved frame); the user context is saved in
        // *CURRENT_USER_CTX by the trap stub.
        unsafe { return_to_saved_kernel(crate::loader::PARKED_SENTINEL) }
    }

    // A lane entered via resume_user_context must unwind its exit to that call
    // site (return_to_saved_kernel), not the original enter_user_context slot.
    if number == Syscall::Exit.number() && LANE_WAS_RESUMED.load(Ordering::SeqCst) {
        // SAFETY: a resume_user_context is live, so ACTIVE_KERNEL_CTX points at
        // its saved frame.
        unsafe { return_to_saved_kernel(arg0 as i64) }
    }

    // Otherwise: ordinary syscall semantics (resumes the same lane inline).
    handle_syscall(number, arg0, arg1, arg2)
}

// ===========================================================================
// Per-arch bring-up contract implementations.
//
// These wire the canonical bring-up phases to each architecture. x86_64
// delegates to the existing, verified functions (no behavior change — just
// relocation behind the contract). aarch64/riscv64 implement early_traps (they
// have vectors) and report the rest as Skipped("pending: ...") honestly, to be
// filled in — by implementing the SAME method — as each phase is built.
// ===========================================================================

use crate::bringup::{ArchBringUp, PhaseStatus};

/// The architecture bring-up implementation for the target the kernel is built
/// for. `kernel_entry` drives the canonical sequence through this single value,
/// so the boot control flow carries no `target_arch` branching.
pub(crate) struct Arch;

#[cfg(target_arch = "x86_64")]
impl ArchBringUp for Arch {
    fn early_traps(&self) {
        // x86_64 installs its IDT later (inside the ring-3 bring-up under the
        // MMU phase); the PIC/serial are already live from the firmware handoff,
        // so there is no separate early-trap install here. Faults before the IDT
        // are caught by the firmware's environment.
    }

    fn seed_entropy(&self, seed: &[u8]) -> PhaseStatus {
        let mut buf = [0u8; 32];
        let n = buf.len().min(seed.len());
        buf[..n].copy_from_slice(&seed[..n]);
        seed_kernel_rng(buf);
        PhaseStatus::Done
    }

    fn mount_root_fs(&self) -> PhaseStatus {
        #[cfg(any(feature = "storage-selftest", feature = "interactive-session"))]
        {
            mount_root_fs_early();
            PhaseStatus::Done
        }
        #[cfg(not(any(feature = "storage-selftest", feature = "interactive-session")))]
        {
            PhaseStatus::Skipped("no storage feature enabled")
        }
    }

    fn bring_up_mmu(&self, handoff: &HandoffData) -> PhaseStatus {
        // This phase also owns the frame allocator and, within its scope, probes
        // the NIC and drops to ring 3 (they borrow the allocator it owns).
        bring_up_mmu(handoff);
        PhaseStatus::Done
    }

    fn verify_storage(&self) -> PhaseStatus {
        verify_storage();
        PhaseStatus::Done
    }
}

#[cfg(target_arch = "aarch64")]
impl ArchBringUp for Arch {
    fn early_traps(&self) {
        // FP/SIMD is enabled in boot/aarch64.s; install the exception vectors
        // (VBAR_EL1) so faults are reported.
        // SAFETY: single-threaded bring-up; sets VBAR_EL1 once.
        unsafe {
            arch::install_exception_vectors();
        }
    }
    fn seed_entropy(&self, _seed: &[u8]) -> PhaseStatus {
        PhaseStatus::Skipped("pending: aarch64 RNG path")
    }
    fn mount_root_fs(&self) -> PhaseStatus {
        PhaseStatus::Skipped("pending: aarch64 block driver")
    }
    fn bring_up_mmu(&self, handoff: &HandoffData) -> PhaseStatus {
        // The SAME portable orchestration as x86_64, parameterized by the aarch64
        // paging hooks (VMSAv8-64 encoder + TTBR/TCR/SCTLR install).
        bring_up_mmu_generic::<crate::arch::paging_aarch64::ArchPagingImpl>(handoff);
        PhaseStatus::Done
    }
    fn verify_storage(&self) -> PhaseStatus {
        PhaseStatus::Skipped("pending: aarch64 block driver")
    }
}

#[cfg(target_arch = "riscv64")]
impl ArchBringUp for Arch {
    fn early_traps(&self) {
        // Install the S-mode trap vector (stvec) so traps are reported.
        // SAFETY: single-threaded bring-up; sets stvec once.
        unsafe {
            arch::install_trap_vector();
        }
    }
    fn seed_entropy(&self, _seed: &[u8]) -> PhaseStatus {
        PhaseStatus::Skipped("pending: riscv64 RNG path")
    }
    fn mount_root_fs(&self) -> PhaseStatus {
        PhaseStatus::Skipped("pending: riscv64 block driver")
    }
    fn bring_up_mmu(&self, handoff: &HandoffData) -> PhaseStatus {
        // The SAME portable orchestration as x86_64/aarch64, parameterized by the
        // riscv64 Sv48 paging hooks (encoder + satp install).
        bring_up_mmu_generic::<crate::arch::paging_riscv64::ArchPagingImpl>(handoff);
        PhaseStatus::Done
    }
    fn verify_storage(&self) -> PhaseStatus {
        PhaseStatus::Skipped("pending: riscv64 block driver")
    }
}

#[cfg(target_arch = "x86")]
impl ArchBringUp for Arch {
    fn early_traps(&self) {
        // i686: 32-bit IDT pending; serial is live. No early-trap install yet.
    }
    fn seed_entropy(&self, _seed: &[u8]) -> PhaseStatus {
        PhaseStatus::Skipped("pending: i686 RNG path")
    }
    fn mount_root_fs(&self) -> PhaseStatus {
        PhaseStatus::Skipped("pending: i686 block driver")
    }
    fn bring_up_mmu(&self, _handoff: &HandoffData) -> PhaseStatus {
        PhaseStatus::Skipped("pending: i686 32-bit paging")
    }
    fn verify_storage(&self) -> PhaseStatus {
        PhaseStatus::Skipped("pending: i686 block driver")
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("CIBOS kernel PANIC: {info}");
    arch::halt();
}
