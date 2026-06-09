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
/// top-level init that owns the machine). [`run_user_payload_returning`] is the
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

/// Like [`run_user_payload`], but returns the user's exit code when the payload
/// calls `exit` (instead of diverging). Uses the kernel-context save/restore
/// entry so control comes back to the kernel — the basis for a real process
/// model where `exit` returns to the scheduler.
///
/// # Safety
///
/// As [`run_user_payload`]. Additionally, only one `enter_user_context` may be
/// live at a time (the saved context is a single static), which holds for the
/// current single-task bring-up.
pub unsafe fn run_user_payload_returning(
    space: &AddressSpace,
    frames: &FrameAllocator,
    code: &[u8],
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
) -> Result<i64, &'static str> {
    if code.len() as u64 > FRAME_SIZE {
        return Err("user payload exceeds one page");
    }

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

    crate::arch::paging::install(space.root());
    let user_stack_top = (USER_STACK_VIRT + FRAME_SIZE) & !0xF;

    let code = enter_user_context(
        USER_CODE_VIRT,
        user_stack_top,
        gdt::USER_CODE as u64,
        gdt::USER_DATA as u64,
    );
    Ok(code)
}
