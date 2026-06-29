//! Application loading and ring-3 entry (x86_64).
//!
//! Brings the pieces together: given a chunk of position-independent user code
//! and a target boundary, map a user-accessible code page and stack page into
//! that boundary's address space, point the TSS at a fresh kernel stack, and
//! drop to ring 3 at the code's entry. The user code runs unprivileged and
//! reaches the kernel only through `int 0x80` syscalls.
//!
//! ## This step vs. loading real images
//!
//! This first loader runs an in-kernel user payload (a `naked`-style routine
//! linked into the image) to prove the privilege transition and the ring-3
//! syscall path end to end. Loading an external application image — parsing its
//! format, mapping its segments with per-segment permissions, and relocating —
//! is the next layer and reuses exactly this mapping + entry path; only the
//! source of the bytes and the segment metadata change.
//!
//! The user code and stack are mapped into the *currently active* address space
//! here (the kernel's), as user-accessible pages at high virtual addresses well
//! clear of the kernel's identity map. Per-boundary spaces (one `AddressSpace`
//! per container, installed via CR3 on entry) build on
//! [`cibos_kernel::AddressSpaceManager`]; this proves the ring-3 mechanism first.

use cibos_kernel::paging::{AddressSpace, Permissions};
use cibos_kernel::{FrameAllocator, FRAME_SIZE};

use crate::arch::gdt;
use crate::arch::paging::X86PageTable;

/// Virtual address where the user code page is mapped.
pub const USER_CODE_VIRT: u64 = 0x0000_5000_0000_0000;
/// Virtual address where the user stack is mapped (one page).
pub const USER_STACK_VIRT: u64 = 0x0000_5000_0010_0000;

/// Amount of physical address space identity-mapped into every address space
/// (the kernel's and each per-process space). It must cover everything the
/// kernel executes from after a CR3 switch — kernel code/data at 16 MiB, its
/// heap and stack, the GDT/TSS/IDT, the page-table frames, and the VGA buffer
/// at `0xB8000` — so an app space can run ring-0 trap/IRQ handlers after its
/// CR3 is installed. Single source of truth shared by `bring_up_mmu` and the
/// per-process launcher below; they must map the identical range or a CR3
/// switch would fault on the first kernel access.
pub const KERNEL_IDENTITY_MAP_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB

extern "C" {
    fn enter_user_mode(entry: u64, user_stack: u64, user_code_sel: u64, user_data_sel: u64) -> !;
    /// Enter ring 3 saving the kernel context; returns the user's exit code when
    /// the user calls `exit` (which invokes `return_to_kernel`).
    fn enter_user_context(
        entry: u64,
        user_stack: u64,
        user_code_sel: u64,
        user_data_sel: u64,
        heap_base: u64,
        heap_size: u64,
    ) -> i64;
}

/// Restore the saved kernel context, returning `code` from the matching
/// [`enter_user_context`] call. Invoked by the syscall handler on `exit`.
///
/// # Safety
///
/// Must only be called while a kernel context saved by `enter_user_context` is
/// live (i.e. from within a syscall issued by the ring-3 task it launched).
pub unsafe fn return_to_kernel(code: i64) -> ! {
    extern "C" {
        fn return_to_kernel(code: i64) -> !;
    }
    return_to_kernel(code)
}

/// Sentinel `enter_user_context` "return" value meaning the lane was PARKED at a
/// trap (not exited), so its context in `USER_CTX_SAVE` is resumable. Chosen
/// distinct from any real exit code (apps exit with small non-negative codes).
#[cfg(any(feature = "ring3-resume-demo", feature = "ring3-multilane-demo"))]
pub const PARKED_SENTINEL: i64 = -0x7FFF_FFFF;

/// Map the given user code bytes and a stack into `space` as user-accessible
/// pages, then drop to ring 3 at the code entry. Does not return: the user code
/// must `exit` (which the kernel handles) or run forever.
///
/// This diverging entry suits a task that never returns to the kernel (e.g. a
/// top-level init that owns the machine). [`run_app_image_isolated`] is the
/// variant used by the process model, where each app runs in its own address
/// space and `exit` unwinds back to the kernel. Kept as a documented alternative.
///
/// `code` must be position-independent and fit in one page; `phys_to_ptr` is the
/// active identity map.
///
/// # Safety
///
/// `space` must be the currently installed address space (so the new user
/// mappings are live), the GDT/TSS must be initialised, and `code` must be valid
/// ring-3-safe machine code that only escapes via `int 0x80`.
#[allow(dead_code)]
pub unsafe fn run_user_payload(
    space: &AddressSpace,
    frames: &FrameAllocator,
    code: &[u8],
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<(), &'static str> {
    if code.len() as u64 > FRAME_SIZE {
        return Err("user payload exceeds one page");
    }

    // Allocate and populate the code page (user-executable, read-only).
    let code_frame = frames.allocate().map_err(|_| "no frame for user code")?;
    let code_ptr = phys_to_ptr(code_frame.addr());
    core::ptr::write_bytes(code_ptr, 0, FRAME_SIZE as usize);
    core::ptr::copy_nonoverlapping(code.as_ptr(), code_ptr, code.len());

    space
        .map::<X86PageTable>(
            USER_CODE_VIRT,
            code_frame,
            Permissions::user_rx(),
            frames,
            phys_to_ptr,
        )
        .map_err(|_| "map user code failed")?;

    // Allocate and map the user stack page (user read/write).
    let stack_frame = frames.allocate().map_err(|_| "no frame for user stack")?;
    let stack_ptr = phys_to_ptr(stack_frame.addr());
    core::ptr::write_bytes(stack_ptr, 0, FRAME_SIZE as usize);
    space
        .map::<X86PageTable>(
            USER_STACK_VIRT,
            stack_frame,
            Permissions::user_rw(),
            frames,
            phys_to_ptr,
        )
        .map_err(|_| "map user stack failed")?;

    // The TLB may hold stale entries for these freshly-mapped addresses; reload
    // CR3 to flush (we are still on `space`'s root).
    crate::arch::paging::install(space.root());

    // Top of the user stack (grows down), 16-byte aligned.
    let user_stack_top = (USER_STACK_VIRT + FRAME_SIZE) & !0xF;

    // Ensure the TSS ring-0 stack is set so the syscall trap from ring 3 lands
    // on a valid kernel stack (gdt::init already set it; reaffirm defensively).
    // entry is the code page virtual base.
    enter_user_mode(
        USER_CODE_VIRT,
        user_stack_top,
        gdt::USER_CODE as u64,
        gdt::USER_DATA as u64,
    );
}

/// Map every segment of a parsed application image into `space` with each
/// segment's own permissions, copying the file bytes in and zero-filling the
/// `mem_size - file_size` tail (e.g. `.bss`). Returns the image entry virtual
/// address on success; the caller enters ring 3 there.
///
/// Each segment is mapped page-by-page from fresh zeroed frames, so a segment
/// whose `vaddr`/`file_size` are not page-aligned is handled correctly: bytes
/// are written at the right intra-page offset and the surrounding bytes stay
/// zero. Segments must not overlap a page already mapped in `space` (the
/// underlying `map` rejects a double mapping), which a well-formed image with
/// page-disjoint segments satisfies.
///
/// # Safety
///
/// `space` must be a valid address space whose tables are reachable through
/// `phys_to_ptr` (the active identity map), and `frames` must hand out frames
/// that `phys_to_ptr` maps to writable `FRAME_SIZE` regions. The image's
/// segment permissions are trusted to describe ring-3-safe code/data; the
/// caller is responsible for having validated the image (`AppImage::parse`).
pub unsafe fn load_app_image(
    space: &AddressSpace,
    frames: &FrameAllocator,
    image: &shared::AppImage,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<u64, &'static str> {
    use shared::AppSegment;

    let perms_of = |seg: &AppSegment| Permissions {
        read: seg.readable(),
        write: seg.writable(),
        execute: seg.executable(),
        user: true,
        device: false,
    };

    for i in 0..image.segment_count() {
        let seg = image.segment(i).map_err(|_| "bad segment descriptor")?;
        let body = image.segment_body(&seg).map_err(|_| "bad segment body")?;
        let perms = perms_of(&seg);

        // Page-aligned span covering [vaddr, vaddr + mem_size).
        let seg_start = seg.vaddr;
        let seg_end = seg
            .vaddr
            .checked_add(seg.mem_size)
            .ok_or("segment end overflow")?;
        let first_page = seg_start & !(FRAME_SIZE - 1);
        // Number of pages spanned (ceil of the unaligned end minus aligned start).
        let span = seg_end - first_page;
        let page_count = span.div_ceil(FRAME_SIZE);

        for p in 0..page_count {
            let page_virt = first_page + p * FRAME_SIZE;

            // Fresh zeroed frame for this page (zero-fill covers .bss tails and
            // any sub-page padding around the copied bytes).
            let frame = frames
                .allocate_zeroed(phys_to_ptr)
                .map_err(|_| "no frame for app segment")?;

            // Copy the portion of the segment's file bytes that lands in this
            // page, if any. The file bytes occupy [seg_start, seg_start+file_size)
            // in virtual space; intersect that with this page.
            let file_end = seg_start + seg.file_size;
            let page_lo = page_virt;
            let page_hi = page_virt + FRAME_SIZE;
            let copy_lo = core::cmp::max(seg_start, page_lo);
            let copy_hi = core::cmp::min(file_end, page_hi);
            if copy_hi > copy_lo {
                let dst = phys_to_ptr(frame.addr());
                let dst_off = (copy_lo - page_lo) as usize;
                let src_off = (copy_lo - seg_start) as usize;
                let n = (copy_hi - copy_lo) as usize;
                core::ptr::copy_nonoverlapping(body.as_ptr().add(src_off), dst.add(dst_off), n);
            }

            space
                .map::<X86PageTable>(page_virt, frame, perms, frames, phys_to_ptr)
                .map_err(|_| "map app segment page failed")?;
        }
    }

    Ok(image.entry())
}

/// Map a user stack of `pages` pages into `space` (user read/write) at
/// `stack_base`, returning the initial (top-of-stack, 16-byte-aligned) pointer.
///
/// # Safety
///
/// As [`load_app_image`].
pub unsafe fn map_user_stack(
    space: &AddressSpace,
    frames: &FrameAllocator,
    stack_base: u64,
    pages: u64,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<u64, &'static str> {
    for p in 0..pages {
        let frame = frames
            .allocate_zeroed(phys_to_ptr)
            .map_err(|_| "no frame for user stack")?;
        space
            .map::<X86PageTable>(
                stack_base + p * FRAME_SIZE,
                frame,
                Permissions::user_rw(),
                frames,
                phys_to_ptr,
            )
            .map_err(|_| "map user stack page failed")?;
    }
    // Top of the stack (grows down), 16-byte aligned.
    Ok((stack_base + pages * FRAME_SIZE) & !0xF)
}

/// Run an application image in its **own fresh address space** — the real
/// process model.
///
/// Unlike a single shared address space (where two runs collide on the same
/// user vaddrs), this builds a brand-new
/// [`AddressSpace`] per call: it identity-maps the kernel range into it (so
/// ring-0 trap/IRQ handlers execute after the CR3 switch), maps the app's
/// segments, stack, and heap, installs the space, drops to ring 3, and on the
/// app's `exit` restores the caller's CR3. Each process is therefore isolated
/// and re-runnable — the same app, or different apps, can run back-to-back
/// without vaddr collisions, and one process cannot reach another's pages.
///
/// Returns the app's exit code.
///
/// # Safety
///
/// As [`load_app_image`]. Additionally: `kernel_root` must be the address space
/// to return to (the caller's installed CR3), and only one `enter_user_context`
/// may be live at a time (the kernel-context save/restore is a single slot).
pub unsafe fn run_app_image_isolated(
    frames: &FrameAllocator,
    image: &shared::AppImage,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<i64, &'static str> {
    use cibos_kernel::paging::Permissions;

    // The CR3 to restore when the app exits (the caller's space — the kernel's).
    let kernel_root = crate::arch::paging::current_root();

    // Fresh per-process space.
    let space = AddressSpace::new(frames, phys_to_ptr).map_err(|_| "no frame for app space root")?;

    // Identity-map the kernel range into the new space so ring-0 execution
    // (the syscall/IRQ handlers, the kernel stack, the page tables themselves)
    // continues to work after we switch CR3 to this space. Kernel pages:
    // supervisor-only, read/write/execute — identical to the kernel's own map.
    let kernel_pages = KERNEL_IDENTITY_MAP_BYTES / FRAME_SIZE;
    space
        .map_range::<X86PageTable>(
            0,
            0,
            kernel_pages,
            Permissions {
                read: true,
                write: true,
                execute: true,
                user: false,
                device: false,
            },
            frames,
            phys_to_ptr,
        )
        .map_err(|_| "map kernel range into app space failed")?;

    // Map the app's segments (each with its own permissions), its stack, and a
    // heap — exactly as the shared-space path does, but into this private space.
    let entry = load_app_image(&space, frames, image, phys_to_ptr)?;

    let app_base = entry & !0xFFFF_FFFF; // 4 GiB-align the app's region
    let stack_base = app_base + 0x0010_0000; // +1 MiB
    let stack_top = map_user_stack(&space, frames, stack_base, 1, phys_to_ptr)?;

    const HEAP_PAGES: u64 = 64; // 256 KiB
    let heap_base = app_base + 0x0020_0000; // +2 MiB
    map_user_stack(&space, frames, heap_base, HEAP_PAGES, phys_to_ptr)?;
    let heap_size = HEAP_PAGES * FRAME_SIZE;

    // Switch to the app's space, run it in ring 3, then restore the kernel's.
    crate::arch::paging::install(space.root());
    let code = enter_user_context(
        entry,
        stack_top,
        gdt::USER_CODE as u64,
        gdt::USER_DATA as u64,
        heap_base,
        heap_size,
    );
    // Back from ring 3 (the app called `exit`): restore the caller's address
    // space before returning so the rest of the kernel runs on its own CR3.
    crate::arch::paging::install(cibos_kernel::PhysFrame::containing(kernel_root));

    Ok(code)
}

// ---- Ring-3 park/resume demonstration (feature: ring3-resume-demo) ----------
//
// Proves the per-lane ring-3 context mechanism end to end: launch a ring-3
// payload that traps via `yield`; the context-saving trap stub saves its FULL
// register state into the lane's `SavedUserContext` (pointed to by the kernel-
// set CURRENT_USER_CTX), the handler PARKS it (returns to the kernel); the
// kernel then resumes the *same parked context* via `resume_user_context`, and
// the payload continues from exactly the instruction after `yield`, logs, and
// exits. This is the load-bearing prerequisite for live multi-context (`spawn`)
// and cross-boundary channel enforcement (see TRACK2-LIVE-CONTEXT-DESIGN.md).
//
// The demo uses ONE lane (one parked context at a time), but the mechanism is
// arbitrary-lane by construction: the trap saves into *CURRENT_USER_CTX and
// `resume_*` take the context pointer as an argument, so the selector-owned
// Ring3Lane table (step 3) reuses this asm unchanged.
#[cfg(all(target_arch = "x86_64", feature = "ring3-resume-demo"))]
pub unsafe fn run_resume_demo(
    frames: &FrameAllocator,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) {
    use crate::arch::ring3_ctx::SavedUserContext;

    extern "C" {
        fn enter_user_context(
            entry: u64,
            user_stack: u64,
            user_code_sel: u64,
            user_data_sel: u64,
            heap_base: u64,
            heap_size: u64,
        ) -> i64;
        fn resume_user_context(
            ctx: *const SavedUserContext,
            kctx: *mut crate::boot::KernelReturnContext,
        ) -> i64;
    }

    // Tiny ring-3 payload (assembled from resume_payload2.s; see the design
    // note). It: yield (rax=3,int 0x80) -> [resume point] -> log(msg) ->
    // exit(0). Position-independent (msg reached via PC-relative lea).
    const PAYLOAD: &[u8] = &[
        0x48, 0xc7, 0xc0, 0x03, 0x00, 0x00, 0x00, // mov rax, 3   (Yield)
        0x48, 0x31, 0xff, // xor rdi, rdi
        0x48, 0x31, 0xf6, // xor rsi, rsi
        0x48, 0x31, 0xd2, // xor rdx, rdx
        0xcd, 0x80, // int 0x80   <- traps, saved + parked here
        0x48, 0xc7, 0xc0, 0x01, 0x00, 0x00, 0x00, // mov rax, 1   (Log)
        0x48, 0x8d, 0x3d, 0x20, 0x00, 0x00, 0x00, // lea rdi, [rip+0x20] (msg)
        0x48, 0xc7, 0xc6, 0x3c, 0x00, 0x00, 0x00, // mov rsi, 0x3c (len 60)
        0x48, 0x31, 0xd2, // xor rdx, rdx
        0xcd, 0x80, // int 0x80   (log)
        0x48, 0xc7, 0xc0, 0x02, 0x00, 0x00, 0x00, // mov rax, 2   (Exit)
        0x48, 0x31, 0xff, // xor rdi, rdi  (code 0)
        0xcd, 0x80, // int 0x80   (exit)
        0xeb, 0xfe, // 1: jmp 1b
        0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00, // nop padding to align msg @ 0x40
        // msg @ offset 0x40 (60 bytes): "  [ring3] resumed after park, continued from the trap point\n"
        0x20, 0x20, 0x5b, 0x72, 0x69, 0x6e, 0x67, 0x33, 0x5d, 0x20, 0x72, 0x65,
        0x73, 0x75, 0x6d, 0x65, 0x64, 0x20, 0x61, 0x66, 0x74, 0x65, 0x72, 0x20,
        0x70, 0x61, 0x72, 0x6b, 0x2c, 0x20, 0x63, 0x6f, 0x6e, 0x74, 0x69, 0x6e,
        0x75, 0x65, 0x64, 0x20, 0x66, 0x72, 0x6f, 0x6d, 0x20, 0x74, 0x68, 0x65,
        0x20, 0x74, 0x72, 0x61, 0x70, 0x20, 0x70, 0x6f, 0x69, 0x6e, 0x74, 0x0a,
    ];

    kprintln!("CIBOS kernel: ring-3 park/resume demo starting");

    // Fresh per-lane address space (the lane's cr3), kernel range identity-mapped
    // so the trap/handler run after the CR3 switch.
    let kernel_root = crate::arch::paging::current_root();
    let space = match AddressSpace::new(frames, phys_to_ptr) {
        Ok(s) => s,
        Err(_) => {
            kprintln!("  resume demo: no frame for space root — skipping");
            return;
        }
    };
    let kernel_pages = KERNEL_IDENTITY_MAP_BYTES / FRAME_SIZE;
    if space
        .map_range::<X86PageTable>(
            0,
            0,
            kernel_pages,
            Permissions { read: true, write: true, execute: true, user: false, device: false },
            frames,
            phys_to_ptr,
        )
        .is_err()
    {
        kprintln!("  resume demo: kernel identity map failed — skipping");
        return;
    }

    // Map the payload code page (user rx) and a user stack page (user rw).
    if run_user_payload_map(&space, frames, PAYLOAD, phys_to_ptr).is_err() {
        kprintln!("  resume demo: payload map failed — skipping");
        return;
    }
    let user_stack_top = (USER_STACK_VIRT + FRAME_SIZE) & !0xF;

    // The lane's saved context lives here (one lane for the demo). A static is
    // used so its address is stable for CURRENT_USER_CTX; the selector-owned
    // table will hold one of these per lane.
    static mut LANE_CTX: SavedUserContext = SavedUserContext::empty();
    static mut KRET: crate::boot::KernelReturnContext =
        crate::boot::KernelReturnContext::zeroed();

    // Point CURRENT_USER_CTX at this lane's context so the trap saves into it.
    crate::boot::CURRENT_USER_CTX = core::ptr::addr_of_mut!(LANE_CTX);

    // Switch to the lane's space and launch it. enter_user_context returns when
    // the payload parks (PARKED_SENTINEL via return_to_kernel) or exits.
    crate::arch::paging::install(space.root());
    let r1 = enter_user_context(
        USER_CODE_VIRT,
        user_stack_top,
        gdt::USER_CODE as u64,
        gdt::USER_DATA as u64,
        0,
        0,
    );

    if r1 != PARKED_SENTINEL {
        crate::arch::paging::install(cibos_kernel::PhysFrame::containing(kernel_root));
        kprintln!("  resume demo: expected park, got {} — aborting", r1);
        return;
    }
    kprintln!("  lane parked at trap (full user context saved); kernel back in control");

    // Resume the SAME parked context. Mark resumed so its exit unwinds to the
    // resume_user_context call site (return_to_saved_kernel).
    crate::boot::mark_lane_resumed();
    let code = resume_user_context(
        core::ptr::addr_of!(LANE_CTX),
        core::ptr::addr_of_mut!(KRET),
    );

    // Back from ring 3 (the resumed lane exited): restore the kernel's space.
    crate::arch::paging::install(cibos_kernel::PhysFrame::containing(kernel_root));
    kprintln!("  lane resumed from the trap point and exited (code {})", code);
    kprintln!("CIBOS kernel: ring-3 park/resume demo OK");
}

/// Map a raw payload + a user stack into `space` (helper for the resume demo).
/// Mirrors the mapping in `run_user_payload` but does not enter ring 3 (the
/// caller drives entry via `enter_user_context` for park/resume).
#[cfg(all(target_arch = "x86_64", feature = "ring3-resume-demo"))]
unsafe fn run_user_payload_map(
    space: &AddressSpace,
    frames: &FrameAllocator,
    code: &[u8],
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<(), &'static str> {
    if code.len() as u64 > FRAME_SIZE {
        return Err("payload exceeds one page");
    }
    let code_frame = frames.allocate().map_err(|_| "no frame for code")?;
    let code_ptr = phys_to_ptr(code_frame.addr());
    core::ptr::write_bytes(code_ptr, 0, FRAME_SIZE as usize);
    core::ptr::copy_nonoverlapping(code.as_ptr(), code_ptr, code.len());
    space
        .map::<X86PageTable>(USER_CODE_VIRT, code_frame, Permissions::user_rx(), frames, phys_to_ptr)
        .map_err(|_| "map code failed")?;

    let stack_frame = frames.allocate().map_err(|_| "no frame for stack")?;
    let stack_ptr = phys_to_ptr(stack_frame.addr());
    core::ptr::write_bytes(stack_ptr, 0, FRAME_SIZE as usize);
    space
        .map::<X86PageTable>(USER_STACK_VIRT, stack_frame, Permissions::user_rw(), frames, phys_to_ptr)
        .map_err(|_| "map stack failed")?;
    crate::arch::paging::install(space.root());
    Ok(())
}

/// Map a payload + stack for ONE ring-3 lane at caller-chosen virtual addresses
/// (so multiple lanes coexist in one space). Returns the lane's stack top.
/// Used by the selector-owned Ring3Table multilane demo.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub unsafe fn map_lane(
    space: &AddressSpace,
    frames: &FrameAllocator,
    code: &[u8],
    code_virt: u64,
    stack_virt: u64,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<u64, &'static str> {
    if code.len() as u64 > FRAME_SIZE {
        return Err("payload exceeds one page");
    }
    let code_frame = frames.allocate().map_err(|_| "no frame for code")?;
    let code_ptr = phys_to_ptr(code_frame.addr());
    core::ptr::write_bytes(code_ptr, 0, FRAME_SIZE as usize);
    core::ptr::copy_nonoverlapping(code.as_ptr(), code_ptr, code.len());
    space
        .map::<X86PageTable>(code_virt, code_frame, Permissions::user_rx(), frames, phys_to_ptr)
        .map_err(|_| "map code failed")?;

    let stack_frame = frames.allocate().map_err(|_| "no frame for stack")?;
    let stack_ptr = phys_to_ptr(stack_frame.addr());
    core::ptr::write_bytes(stack_ptr, 0, FRAME_SIZE as usize);
    space
        .map::<X86PageTable>(stack_virt, stack_frame, Permissions::user_rw(), frames, phys_to_ptr)
        .map_err(|_| "map stack failed")?;
    Ok((stack_virt + FRAME_SIZE) & !0xF)
}

/// Build a fresh per-boundary address space with the kernel range identity-mapped
/// (so traps/handlers run after the CR3 switch). Returns the space.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub unsafe fn new_lane_space(
    frames: &FrameAllocator,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<AddressSpace, &'static str> {
    let space = AddressSpace::new(frames, phys_to_ptr).map_err(|_| "no frame for space root")?;
    let kernel_pages = KERNEL_IDENTITY_MAP_BYTES / FRAME_SIZE;
    space
        .map_range::<X86PageTable>(
            0,
            0,
            kernel_pages,
            Permissions { read: true, write: true, execute: true, user: false, device: false },
            frames,
            phys_to_ptr,
        )
        .map_err(|_| "kernel identity map failed")?;
    Ok(space)
}

/// Install a space's page tables as the active CR3 (exposed for the multilane
/// demo, which maps all lanes into one space then switches to it).
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub unsafe fn install_space(space: &AddressSpace) {
    crate::arch::paging::install(space.root());
}

// ---- spawn syscall support (feature: ring3-multilane-demo) ------------------
//
// `KernelSyscallEnv::spawn` must map a fresh stack for the new lane into the
// CALLER'S currently-installed address space (same boundary -> same space). The
// frame allocator is a local in `start_ring3_runtime`; we expose it to the syscall
// path for the demo's duration via a raw pointer (set before the run, cleared
// after), mirroring how RING3_TABLE is installed. phys_to_ptr on the booted
// kernel is the identity map, so it is reconstructed here rather than stored.

#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
static SPAWN_FRAMES: core::sync::atomic::AtomicPtr<FrameAllocator> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Publish the frame allocator for the spawn syscall (demo-run lifetime).
///
/// # Safety
/// `frames` must outlive the run (it is a local in `start_ring3_runtime`, which spans
/// the whole demo). Must be cleared with `clear_spawn_frames` before it drops.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub unsafe fn set_spawn_frames(frames: &FrameAllocator) {
    SPAWN_FRAMES.store(
        (frames as *const FrameAllocator) as *mut FrameAllocator,
        core::sync::atomic::Ordering::SeqCst,
    );
}

/// Stop exposing the frame allocator to the spawn syscall.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub fn clear_spawn_frames() {
    SPAWN_FRAMES.store(core::ptr::null_mut(), core::sync::atomic::Ordering::SeqCst);
}

/// Map ONLY a payload's code page (no stack) at `code_virt` — for a `spawn`
/// target whose stack the spawn syscall maps separately.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub unsafe fn map_lane_code(
    space: &AddressSpace,
    frames: &FrameAllocator,
    code: &[u8],
    code_virt: u64,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<(), &'static str> {
    if code.len() as u64 > FRAME_SIZE {
        return Err("payload exceeds one page");
    }
    let code_frame = frames.allocate().map_err(|_| "no frame for code")?;
    let code_ptr = phys_to_ptr(code_frame.addr());
    core::ptr::write_bytes(code_ptr, 0, FRAME_SIZE as usize);
    core::ptr::copy_nonoverlapping(code.as_ptr(), code_ptr, code.len());
    space
        .map::<X86PageTable>(code_virt, code_frame, Permissions::user_rx(), frames, phys_to_ptr)
        .map_err(|_| "map code failed")?;
    Ok(())
}

/// Map a fresh user stack page at `stack_virt` into the CURRENT address space
/// (the caller's live space, adopted from `cr3`). Returns the 16-byte-aligned
/// stack top. Used by the spawn syscall to give a new lane its own stack.
///
/// # Safety
/// Must run on the booted kernel with the identity phys map and a frame
/// allocator published via `set_spawn_frames`.
#[cfg(all(target_arch = "x86_64", feature = "ring3-multilane-demo"))]
pub unsafe fn map_spawn_stack(stack_virt: u64) -> Result<u64, &'static str> {
    let frames_ptr = SPAWN_FRAMES.load(core::sync::atomic::Ordering::SeqCst);
    if frames_ptr.is_null() {
        return Err("spawn frames unavailable");
    }
    let frames = &*frames_ptr;
    let phys_to_ptr = |phys: u64| phys as *mut u8;

    // Adopt the current space (same boundary as the caller) and map one rw page.
    let space = AddressSpace::adopt(crate::arch::paging::current_root_frame());
    let stack_frame = frames.allocate().map_err(|_| "no frame for spawn stack")?;
    let stack_ptr = phys_to_ptr(stack_frame.addr());
    core::ptr::write_bytes(stack_ptr, 0, FRAME_SIZE as usize);
    space
        .map::<X86PageTable>(stack_virt, stack_frame, Permissions::user_rw(), frames, &phys_to_ptr)
        .map_err(|_| "map spawn stack failed")?;
    Ok((stack_virt + FRAME_SIZE) & !0xF)
}
