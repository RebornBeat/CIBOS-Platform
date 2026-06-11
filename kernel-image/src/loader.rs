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

/// Map the given user code bytes and a stack into `space` as user-accessible
/// pages, then drop to ring 3 at the code entry. Does not return: the user code
/// must `exit` (which the kernel handles) or run forever.
///
/// This diverging entry suits a task that never returns to the kernel (e.g. a
/// top-level init that owns the machine). [`run_app_image`] is the
/// variant used by the current demo and the basis for the process model, where
/// `exit` unwinds back to the kernel. Kept as a documented alternative.
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

/// Load a parsed application image into `space`, map a user stack, install the
/// space, and enter ring 3 at the image's entry point. Returns the user's exit
/// code when it calls `exit`.
///
/// This is the external-image counterpart to [`run_user_payload`]: the
/// program comes from a `.capp` (its segments and entry are described by the
/// image) rather than a single embedded code blob, so each segment lands with
/// its own permissions. The user stack is mapped within the app's own address
/// region (derived from its entry) so distinct apps get distinct stacks.
///
/// # Safety
///
/// As [`load_app_image`]. Only one `enter_user_context` may be live at a time
/// (the kernel-context save/restore is a single slot).
pub unsafe fn run_app_image(
    space: &AddressSpace,
    frames: &FrameAllocator,
    image: &shared::AppImage,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<i64, &'static str> {
    let entry = load_app_image(space, frames, image, phys_to_ptr)?;
    // Place the user stack at a fixed offset within this app's own address
    // region (derived from its entry), so two apps loaded at different base
    // addresses get distinct, non-overlapping stacks. One page below a 1 MiB
    // boundary above the entry's 4 GiB-aligned base.
    let app_base = entry & !0xFFFF_FFFF; // 4 GiB-align the app's region
    let stack_base = app_base + 0x0010_0000; // +1 MiB
    let stack_top = map_user_stack(space, frames, stack_base, 1, phys_to_ptr)?;

    // Map a heap region for the application (so it can use `alloc`) and pass its
    // base/size to `_start` via the entry registers. Placed above the stack
    // within the app's own region so distinct apps get distinct heaps. 64 pages
    // = 256 KiB, enough for the shell/login working set.
    const HEAP_PAGES: u64 = 64;
    let heap_base = app_base + 0x0020_0000; // +2 MiB
    map_user_stack(space, frames, heap_base, HEAP_PAGES, phys_to_ptr)?;
    let heap_size = HEAP_PAGES * FRAME_SIZE;

    crate::arch::paging::install(space.root());
    let code = enter_user_context(
        entry,
        stack_top,
        gdt::USER_CODE as u64,
        gdt::USER_DATA as u64,
        heap_base,
        heap_size,
    );
    Ok(code)
}
