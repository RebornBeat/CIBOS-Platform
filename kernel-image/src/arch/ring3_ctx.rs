//! Per-lane saved ring-3 (user) CPU context.
//!
//! This is the symmetric partner of the kernel-context save in `enter_user.s`.
//! Where `enter_user_context` saves the *kernel's* callee-saved registers so a
//! user `exit` can longjmp back, a [`SavedUserContext`] captures a *user* lane's
//! full architectural state at a trap, so the kernel (and, later, the selector)
//! can park that lane and resume it exactly where it trapped — the load-bearing
//! mechanism behind live ring-3 multi-context (`spawn`) and cross-boundary
//! channel enforcement (per the live-context design note).
//!
//! The field order is fixed and mirrored in `resume_user.s`; do not reorder
//! without updating the assembly offsets there.

/// A complete ring-3 register snapshot sufficient to resume a trapped lane via
/// `iretq`. The 15 general-purpose registers (all except RSP, which travels in
/// the `iretq` frame) plus the interrupt-return frame fields the CPU pops when
/// returning to ring 3: RIP, CS, RFLAGS, RSP, SS.
///
/// `#[repr(C)]` with explicit field order so the assembly in `resume_user.s`
/// can index fields by byte offset (`OFF_*` below).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SavedUserContext {
    // --- General-purpose registers (offset 0..120) ---
    pub r15: u64, // 0
    pub r14: u64, // 8
    pub r13: u64, // 16
    pub r12: u64, // 24
    pub r11: u64, // 32
    pub r10: u64, // 40
    pub r9: u64,  // 48
    pub r8: u64,  // 56
    pub rbp: u64, // 64
    pub rdi: u64, // 72
    pub rsi: u64, // 80
    pub rdx: u64, // 88
    pub rcx: u64, // 96
    pub rbx: u64, // 104
    pub rax: u64, // 112
    // --- Interrupt-return frame (offset 120..160) ---
    pub rip: u64,    // 120
    pub cs: u64,     // 128
    pub rflags: u64, // 136
    pub rsp: u64,    // 144
    pub ss: u64,     // 152
}

impl SavedUserContext {
    /// A zeroed context (all registers 0). Real contexts are filled by the trap
    /// save path; this exists for table initialisation.
    pub const fn empty() -> Self {
        Self {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            r11: 0,
            r10: 0,
            r9: 0,
            r8: 0,
            rbp: 0,
            rdi: 0,
            rsi: 0,
            rdx: 0,
            rcx: 0,
            rbx: 0,
            rax: 0,
            rip: 0,
            cs: 0,
            rflags: 0,
            rsp: 0,
            ss: 0,
        }
    }

    /// True once this context holds a real trapped frame (RIP and a user CS).
    /// A freshly `empty()` context reports `false`. (Used by the selector-owned
    /// Ring3Lane table to distinguish parked-resumable lanes from fresh slots.)
    #[allow(dead_code)]
    pub fn is_live(&self) -> bool {
        self.rip != 0 && self.cs != 0
    }
}

/// Compile-time guard that the field offsets the assembly in `resume_user.s`
/// hard-codes (its `OFF_*` constants) match this struct's layout. Checked on
/// every build (including the bare target where the asm actually runs); a drift
/// fails compilation rather than corrupting a resumed lane at runtime.
const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(SavedUserContext, r15) == 0);
    assert!(offset_of!(SavedUserContext, r11) == 32);
    assert!(offset_of!(SavedUserContext, r8) == 56);
    assert!(offset_of!(SavedUserContext, rbp) == 64);
    assert!(offset_of!(SavedUserContext, rdi) == 72);
    assert!(offset_of!(SavedUserContext, rax) == 112);
    assert!(offset_of!(SavedUserContext, rip) == 120);
    assert!(offset_of!(SavedUserContext, cs) == 128);
    assert!(offset_of!(SavedUserContext, rflags) == 136);
    assert!(offset_of!(SavedUserContext, rsp) == 144);
    assert!(offset_of!(SavedUserContext, ss) == 152);
    assert!(core::mem::size_of::<SavedUserContext>() == 160);
};

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn layout_is_stable() {
        // 20 u64 fields, packed, 8-byte aligned. The assembly in resume_user.s
        // depends on this exact size and alignment.
        assert_eq!(size_of::<SavedUserContext>(), 20 * 8);
        assert_eq!(align_of::<SavedUserContext>(), 8);
    }

    #[test]
    fn field_offsets_match_asm() {
        // These offsets are hard-coded in resume_user.s; a mismatch here means
        // the assembly would load the wrong registers.
        let base = core::ptr::addr_of!(EMPTY) as usize;
        let off = |p: *const u64| p as usize - base;
        assert_eq!(off(core::ptr::addr_of!(EMPTY.r15)), 0);
        assert_eq!(off(core::ptr::addr_of!(EMPTY.rax)), 112);
        assert_eq!(off(core::ptr::addr_of!(EMPTY.rip)), 120);
        assert_eq!(off(core::ptr::addr_of!(EMPTY.cs)), 128);
        assert_eq!(off(core::ptr::addr_of!(EMPTY.rflags)), 136);
        assert_eq!(off(core::ptr::addr_of!(EMPTY.rsp)), 144);
        assert_eq!(off(core::ptr::addr_of!(EMPTY.ss)), 152);
    }
    static EMPTY: SavedUserContext = SavedUserContext::empty();

    #[test]
    fn empty_is_not_live() {
        assert!(!SavedUserContext::empty().is_live());
        let mut c = SavedUserContext::empty();
        c.rip = 0x4000_0000;
        c.cs = 0x1b;
        assert!(c.is_live());
    }
}
