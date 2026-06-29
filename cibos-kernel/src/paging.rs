//! # Page Tables (portable model)
//!
//! The architecture-neutral model of a per-boundary virtual address space, and
//! the walk/allocate logic that turns a `(virtual, physical, permissions)`
//! mapping request into a tree of page-table frames. This is where
//! hardware-enforced isolation is *expressed*: each boundary gets its own
//! [`AddressSpace`] with its own top-level table, so one boundary's mappings are
//! physically absent from another's tables.
//!
//! ## What is portable vs architecture-specific
//!
//! The four-level radix structure (9 index bits per level, 4 KiB pages, 48-bit
//! virtual addresses) is shared by x86_64 and aarch64 and modelled here. What is
//! *not* portable — the exact bit layout of a hardware page-table entry and the
//! instruction that installs the top-level table (e.g. `mov cr3` / `msr ttbr0`)
//! — is delegated to a [`PageTableEncoder`] the architecture backend supplies.
//! So the tree-building, frame-allocation, and permission bookkeeping are
//! tested here in portable code, and only the leaf encoding crosses into the
//! kernel binary's arch glue.

use crate::error::{KernelError, KernelResult};
use crate::frame::{FrameAllocator, PhysFrame, FRAME_SIZE};

/// Number of paging levels (PML4 → PDPT → PD → PT on x86_64).
pub const LEVELS: usize = 4;
/// Index bits consumed per level.
pub const INDEX_BITS: usize = 9;
/// Entries per page-table node (`2^INDEX_BITS`).
pub const ENTRIES: usize = 1 << INDEX_BITS; // 512

/// Access permissions for a mapping. The architecture encoder translates these
/// into hardware entry bits; the portable model only records intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Readable. (Always true for a present mapping; kept explicit for clarity.)
    pub read: bool,
    /// Writable.
    pub write: bool,
    /// Executable. When false, the encoder sets the no-execute bit.
    pub execute: bool,
    /// User-accessible. When false, only the kernel (supervisor) may access the
    /// page — the basis for keeping kernel memory out of user boundaries.
    pub user: bool,
    /// Device (MMIO) memory rather than Normal cacheable RAM. When true the
    /// encoder maps the page with the architecture's device/uncached memory type
    /// (aarch64: Device-nGnRnE; x86: cache-disabled). Device registers MUST use
    /// this so the CPU does not cache, reorder, prefetch, or merge accesses to
    /// them — mapping MMIO as Normal works on lenient emulators but malfunctions
    /// on real hardware. (RISC-V has no per-PTE memory type in the base ISA;
    /// cacheability comes from platform PMAs, so this is a documented no-op there.)
    pub device: bool,
}

impl Permissions {
    /// Read-only kernel data.
    #[must_use]
    pub const fn kernel_ro() -> Self {
        Self {
            read: true,
            write: false,
            execute: false,
            user: false,
            device: false,
        }
    }

    /// Read/write kernel data.
    #[must_use]
    pub const fn kernel_rw() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
            user: false,
            device: false,
        }
    }

    /// Read/execute kernel code.
    #[must_use]
    pub const fn kernel_rx() -> Self {
        Self {
            read: true,
            write: false,
            execute: true,
            user: false,
            device: false,
        }
    }

    /// Read/write user data.
    #[must_use]
    pub const fn user_rw() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
            user: true,
            device: false,
        }
    }

    /// Read/execute user code.
    #[must_use]
    pub const fn user_rx() -> Self {
        Self {
            read: true,
            write: false,
            execute: true,
            user: true,
            device: false,
        }
    }

    /// Read/write device (MMIO) memory: uncached/Device-typed, kernel-only,
    /// non-executable. Use for memory-mapped device register windows.
    #[must_use]
    pub const fn device_rw() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
            user: false,
            device: true,
        }
    }
}

/// Encodes architecture-specific page-table entries and reads them back.
///
/// The portable model holds only physical frame addresses and [`Permissions`];
/// the architecture backend implements this trait to produce the actual entry
/// words a CPU walks. All methods are pure functions of their inputs so the
/// model can build and inspect tables without architecture state.
pub trait PageTableEncoder {
    /// Encode an entry pointing at the next-level table at `child` (an interior
    /// entry). Interior entries are present, writable, and user-accessible so
    /// that leaf permissions govern; the leaf entry's bits are authoritative.
    fn encode_table(child: PhysFrame) -> u64;

    /// Encode a leaf entry mapping a 4 KiB page at `frame` with `perms`.
    fn encode_leaf(frame: PhysFrame, perms: Permissions) -> u64;

    /// Encode a BLOCK (huge-page) leaf at interior `level` (0 = top-level table),
    /// mapping a larger, naturally-aligned region directly without a final-level
    /// table. For the shared 4-level / 9-bits geometry: a leaf at level
    /// `LEVELS-2` maps 2 MiB, at `LEVELS-3` maps 1 GiB. `frame` must be aligned to
    /// that block size. Each architecture encodes its block-descriptor form
    /// (x86: PS bit; aarch64: block descriptor bits[1:0]=0b01; riscv64: a leaf PTE
    /// placed at a non-final level). Used to identity-map large real-hardware RAM
    /// cheaply (far fewer page-table frames than 4 KiB pages).
    fn encode_block_leaf(frame: PhysFrame, perms: Permissions, level: usize) -> u64;

    /// Whether an entry is present (bit set by both `encode_*`).
    fn is_present(entry: u64) -> bool;

    /// Whether `entry`, found at interior `level` (0 = top), is a BLOCK/huge leaf
    /// (maps memory directly) rather than a pointer to a next-level table. Lets
    /// the walker stop at a 2 MiB/1 GiB block instead of descending into it as a
    /// table. For a normal 4 KiB-only mapping this is always false at interior
    /// levels. (x86: PS bit; aarch64: block descriptor bits[1:0]==0b01; riscv64: a
    /// leaf PTE — any of R/W/X set — at a non-final level.)
    fn is_block_leaf(entry: u64, level: usize) -> bool;

    /// The physical frame an entry points at (interior child or leaf page).
    fn entry_frame(entry: u64) -> PhysFrame;
}

/// A per-boundary virtual address space: the root page-table frame plus the
/// logic to map and unmap pages into it.
///
/// The `AddressSpace` does not itself touch hardware; it allocates page-table
/// frames from a [`FrameAllocator`] and writes entries through a caller-supplied
/// physical-to-pointer map (the identity map on the booted kernel). Installing
/// the space on the CPU (writing its [`root`](Self::root) to `cr3`/`ttbr0`) is
/// the architecture backend's final step.
pub struct AddressSpace {
    root: PhysFrame,
}

impl AddressSpace {
    /// Create a fresh, empty address space, allocating and zeroing its root
    /// table.
    ///
    /// # Errors
    ///
    /// Propagates frame-allocation failure.
    ///
    /// # Safety
    ///
    /// `phys_to_ptr` must map any allocated frame's physical address to a
    /// writable pointer to `FRAME_SIZE` mapped bytes.
    pub unsafe fn new(
        frames: &FrameAllocator,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> KernelResult<Self> {
        let root = frames.allocate_zeroed(phys_to_ptr)?;
        Ok(Self { root })
    }

    /// The root page-table frame (its physical address goes in `cr3`/`ttbr0`).
    #[must_use]
    pub fn root(&self) -> PhysFrame {
        self.root
    }

    /// Adopt an EXISTING root table (do not allocate a new one) so additional
    /// pages can be mapped into an already-installed address space. The dual of
    /// [`new`](Self::new): `new` creates a fresh space; `adopt` wraps the current
    /// one. Used when a `spawn` adds a lane stack to the caller's live space
    /// (same boundary -> same space). The caller is responsible for `root` being
    /// a valid PML4/root frame for the intended space.
    #[must_use]
    pub fn adopt(root: PhysFrame) -> Self {
        Self { root }
    }

    /// Split a 48-bit virtual address into its four 9-bit level indices,
    /// most-significant level first (`[PML4, PDPT, PD, PT]`).
    fn indices(virt: u64) -> [usize; LEVELS] {
        let mut idx = [0usize; LEVELS];
        // Level 0 is the top table; its index is bits 47..39, etc.
        for (level, slot) in idx.iter_mut().enumerate() {
            let shift = 12 + INDEX_BITS * (LEVELS - 1 - level);
            *slot = ((virt >> shift) & ((1 << INDEX_BITS) - 1)) as usize;
        }
        idx
    }

    /// Map a single 4 KiB `virt` page to physical `frame` with `perms`.
    ///
    /// Allocates interior tables as needed. Fails if the mapping already exists
    /// (no silent remap) or on frame exhaustion.
    ///
    /// # Errors
    ///
    /// [`KernelError::LimitExceeded`] on frame exhaustion;
    /// [`KernelError::InvalidState`] if `virt` or `frame` is misaligned or the
    /// page is already mapped.
    ///
    /// # Safety
    ///
    /// `phys_to_ptr` must map any frame's physical address to a writable pointer
    /// to `FRAME_SIZE` mapped bytes; the returned pointers must stay valid for
    /// the duration of the call.
    pub unsafe fn map<E: PageTableEncoder>(
        &self,
        virt: u64,
        frame: PhysFrame,
        perms: Permissions,
        frames: &FrameAllocator,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> KernelResult<()> {
        if !virt.is_multiple_of(FRAME_SIZE) {
            return Err(KernelError::InvalidState {
                reason: "unaligned virtual address",
            });
        }
        let idx = Self::indices(virt);
        let mut table = self.root;

        // Walk/allocate the interior levels (all but the last).
        for (level, &index) in idx.iter().enumerate().take(LEVELS - 1) {
            let entry_ptr = (phys_to_ptr(table.addr()) as *mut u64).add(index);
            let entry = core::ptr::read(entry_ptr);
            if E::is_present(entry) {
                // If this interior slot is a BLOCK/huge leaf (not a table), we must
                // NOT descend into the mapped frame as if it were a page table —
                // that would corrupt mapped memory. A 4 KiB mapping cannot share a
                // slot already occupied by a larger block; reject it.
                if E::is_block_leaf(entry, level) {
                    return Err(KernelError::InvalidState {
                        reason: "address covered by an existing large-page block",
                    });
                }
                table = E::entry_frame(entry);
            } else {
                let child = frames.allocate_zeroed(phys_to_ptr)?;
                core::ptr::write(entry_ptr, E::encode_table(child));
                table = child;
            }
        }

        // Leaf level.
        let leaf_index = idx[LEVELS - 1];
        let leaf_ptr = (phys_to_ptr(table.addr()) as *mut u64).add(leaf_index);
        if E::is_present(core::ptr::read(leaf_ptr)) {
            return Err(KernelError::InvalidState {
                reason: "page already mapped",
            });
        }
        core::ptr::write(leaf_ptr, E::encode_leaf(frame, perms));
        Ok(())
    }

    /// Map a contiguous range of `count` pages starting at `virt`→`phys`.
    ///
    /// # Errors
    ///
    /// Propagates [`map`](Self::map)'s errors. Partial progress is left in place
    /// on error (the caller tears down the whole space on failure).
    ///
    /// # Safety
    ///
    /// As [`map`](Self::map).
    /// Map a single naturally-aligned 2 MiB block at `virt` to `frame` using a
    /// block/huge leaf at level `LEVELS-2` (one level above the 4 KiB leaf). Both
    /// `virt` and `frame` must be 2 MiB-aligned. This places ONE entry instead of
    /// 512 4 KiB entries plus an L3 table — the basis for cheaply mapping large
    /// real-hardware RAM.
    ///
    /// # Safety
    /// Same contract as [`map`](Self::map): `phys_to_ptr` must map table frames to
    /// readable pointers, and the caller installs the result only after the
    /// kernel's own memory is mapped.
    pub unsafe fn map_block_2m<E: PageTableEncoder>(
        &self,
        virt: u64,
        frame: PhysFrame,
        perms: Permissions,
        frames: &FrameAllocator,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> KernelResult<()> {
        const BLOCK_2M: u64 = FRAME_SIZE * ENTRIES as u64; // 4 KiB * 512 = 2 MiB
        if !virt.is_multiple_of(BLOCK_2M) || !frame.addr().is_multiple_of(BLOCK_2M) {
            return Err(KernelError::InvalidState {
                reason: "unaligned 2 MiB block",
            });
        }
        let idx = Self::indices(virt);
        let mut table = self.root;

        // Walk/allocate interior levels DOWN TO the block level (LEVELS-2). For a
        // 4-level tree that is levels 0..=1 (top, then the L1 that holds the 2 MiB
        // block leaf at its index).
        for &index in idx.iter().take(LEVELS - 2) {
            let entry_ptr = (phys_to_ptr(table.addr()) as *mut u64).add(index);
            let entry = core::ptr::read(entry_ptr);
            if E::is_present(entry) {
                table = E::entry_frame(entry);
            } else {
                let child = frames.allocate_zeroed(phys_to_ptr)?;
                core::ptr::write(entry_ptr, E::encode_table(child));
                table = child;
            }
        }

        // Place the block leaf at level LEVELS-2.
        let block_index = idx[LEVELS - 2];
        let block_ptr = (phys_to_ptr(table.addr()) as *mut u64).add(block_index);
        if E::is_present(core::ptr::read(block_ptr)) {
            return Err(KernelError::InvalidState {
                reason: "2 MiB slot already mapped",
            });
        }
        core::ptr::write(block_ptr, E::encode_block_leaf(frame, perms, LEVELS - 2));
        Ok(())
    }

    /// Map `count` consecutive 4 KiB pages starting at (`virt`, `phys`) with
    /// `perms`. Uses 2 MiB block mappings for the naturally-aligned bulk (one
    /// entry per 2 MiB instead of 512 4 KiB entries plus an L3 table) and 4 KiB
    /// pages for any unaligned head/tail, so mapping large real-hardware RAM stays
    /// cheap in page-table frames while remaining exactly correct at the edges.
    ///
    /// # Safety
    /// Same contract as [`map`](Self::map): `phys_to_ptr` must map table frames to
    /// readable pointers, and the result is installed only after the kernel's own
    /// memory is mapped.
    pub unsafe fn map_range<E: PageTableEncoder>(
        &self,
        virt: u64,
        phys: u64,
        count: u64,
        perms: Permissions,
        frames: &FrameAllocator,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> KernelResult<()> {
        // Use 2 MiB block mappings for the naturally-aligned bulk (one entry per
        // 2 MiB instead of 512 4 KiB entries + an L3 table), falling back to 4 KiB
        // pages for any unaligned head/tail. This keeps page-table frame usage and
        // iteration count low when mapping large real-hardware RAM, while staying
        // exactly correct for unaligned edges. Behavior for small/unaligned ranges
        // is identical to the all-4 KiB path.
        const PAGES_PER_2M: u64 = ENTRIES as u64; // 512
        let mut i = 0u64;
        while i < count {
            let v = virt + i * FRAME_SIZE;
            let p = phys + i * FRAME_SIZE;
            let remaining = count - i;
            if v.is_multiple_of(FRAME_SIZE * PAGES_PER_2M)
                && p.is_multiple_of(FRAME_SIZE * PAGES_PER_2M)
                && remaining >= PAGES_PER_2M
            {
                self.map_block_2m::<E>(v, PhysFrame::containing(p), perms, frames, phys_to_ptr)?;
                i += PAGES_PER_2M;
            } else {
                self.map::<E>(v, PhysFrame::containing(p), perms, frames, phys_to_ptr)?;
                i += 1;
            }
        }
        Ok(())
    }

    /// Translate a virtual address to its mapped physical address, or `None` if
    /// unmapped. Used by tests and by the kernel to audit a boundary's space.
    ///
    /// # Safety
    ///
    /// `phys_to_ptr` must map table frames to readable pointers.
    #[must_use]
    pub unsafe fn translate<E: PageTableEncoder>(
        &self,
        virt: u64,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> Option<u64> {
        let idx = Self::indices(virt);
        let mut table = self.root;
        for (level, &index) in idx.iter().enumerate().take(LEVELS - 1) {
            let entry = core::ptr::read((phys_to_ptr(table.addr()) as *const u64).add(index));
            if !E::is_present(entry) {
                return None;
            }
            // A block/huge leaf at this interior level maps memory directly; stop
            // here and add the in-block offset (the bits below this level's span).
            if E::is_block_leaf(entry, level) {
                // Span covered by one entry at `level`: 4 KiB << (9 * (levels-1-level)).
                let shift = 12 + INDEX_BITS * (LEVELS - 1 - level);
                let block_size = 1u64 << shift;
                let base = E::entry_frame(entry).addr();
                return Some(base | (virt & (block_size - 1)));
            }
            table = E::entry_frame(entry);
        }
        let leaf = core::ptr::read(
            (phys_to_ptr(table.addr()) as *const u64).add(idx[LEVELS - 1]),
        );
        if !E::is_present(leaf) {
            return None;
        }
        let page = E::entry_frame(leaf).addr();
        Some(page | (virt & (FRAME_SIZE - 1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{MemoryRegion, MemoryRegionKind};

    // A test encoder mirroring the x86_64 entry layout closely enough to
    // exercise the portable walk: present=bit0, write=bit1, user=bit2,
    // NX=bit63, frame in bits 12..48.
    struct TestEncoder;
    const PRESENT: u64 = 1 << 0;
    const WRITE: u64 = 1 << 1;
    const USER: u64 = 1 << 2;
    const NX: u64 = 1 << 63;
    const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    impl PageTableEncoder for TestEncoder {
        fn encode_table(child: PhysFrame) -> u64 {
            (child.addr() & ADDR_MASK) | PRESENT | WRITE | USER
        }
        fn encode_leaf(frame: PhysFrame, perms: Permissions) -> u64 {
            let mut e = (frame.addr() & ADDR_MASK) | PRESENT;
            if perms.write {
                e |= WRITE;
            }
            if perms.user {
                e |= USER;
            }
            if !perms.execute {
                e |= NX;
            }
            e
        }
        fn encode_block_leaf(frame: PhysFrame, perms: Permissions, _level: usize) -> u64 {
            // Test encoder: a block leaf is a leaf with bit 7 (PS) set, so tests
            // can distinguish a block mapping from a 4 KiB page mapping.
            const PS: u64 = 1 << 7;
            let mut e = (frame.addr() & ADDR_MASK) | PRESENT | PS;
            if perms.write {
                e |= WRITE;
            }
            if perms.user {
                e |= USER;
            }
            if !perms.execute {
                e |= NX;
            }
            e
        }
        fn is_present(entry: u64) -> bool {
            entry & PRESENT != 0
        }
        fn is_block_leaf(entry: u64, _level: usize) -> bool {
            const PS: u64 = 1 << 7;
            (entry & PRESENT != 0) && (entry & PS != 0)
        }
        fn entry_frame(entry: u64) -> PhysFrame {
            PhysFrame::containing(entry & ADDR_MASK)
        }
    }

    // Identity map for the test: physical address == pointer into a big backing
    // buffer. We back the low physical range with a Vec and treat its base as
    // physical 0 is not safe; instead we allocate frames from a region that maps
    // into a leaked buffer. Simpler: use a fixed-size arena and an offset.
    struct Arena {
        base: *mut u8,
        len: usize,
    }
    impl Arena {
        fn new(len: usize) -> Self {
            let v = alloc::vec![0u8; len].into_boxed_slice();
            let base = alloc::boxed::Box::into_raw(v) as *mut u8;
            Arena { base, len }
        }
        fn ptr_for(&self, phys: u64) -> *mut u8 {
            assert!((phys as usize) < self.len, "phys {phys:#x} out of arena");
            // SAFETY: bounds-checked above.
            unsafe { self.base.add(phys as usize) }
        }
    }

    fn setup(arena_len: usize) -> (FrameAllocator, Arena) {
        // Usable region [0, arena_len); reserve nothing below so frame 0 is
        // usable, but we keep frame 0 out by reserving the first frame so a
        // zero root is distinguishable.
        let regions = alloc::vec![MemoryRegion {
            base: 0,
            length: arena_len as u64,
            kind: MemoryRegionKind::Usable,
        }];
        let fa = FrameAllocator::from_regions(&regions, FRAME_SIZE);
        (fa, Arena::new(arena_len))
    }

    #[test]
    fn index_split_is_correct() {
        // virt with distinct indices at each level.
        // PML4=1, PDPT=2, PD=3, PT=4, offset=0
        let virt = (1u64 << 39) | (2u64 << 30) | (3u64 << 21) | (4u64 << 12);
        let idx = AddressSpace::indices(virt);
        assert_eq!(idx, [1, 2, 3, 4]);
    }

    #[test]
    fn map_then_translate_roundtrips() {
        let (fa, arena) = setup(2 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        // SAFETY: the arena backs all frames the allocator hands out.
        unsafe {
            let space = AddressSpace::new(&fa, &p2p).unwrap();
            let virt = 0x4000_0000u64; // 1 GiB
            let frame = fa.allocate().unwrap();
            space
                .map::<TestEncoder>(virt, frame, Permissions::user_rw(), &fa, &p2p)
                .unwrap();
            let got = space.translate::<TestEncoder>(virt, &p2p).unwrap();
            assert_eq!(got, frame.addr());
            // Offset within the page is preserved.
            let got2 = space.translate::<TestEncoder>(virt + 0x123, &p2p).unwrap();
            assert_eq!(got2, frame.addr() + 0x123);
        }
    }

    #[test]
    fn unmapped_translates_to_none() {
        let (fa, arena) = setup(1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let space = AddressSpace::new(&fa, &p2p).unwrap();
            assert!(space.translate::<TestEncoder>(0x8000_0000, &p2p).is_none());
        }
    }

    #[test]
    fn double_map_is_rejected() {
        let (fa, arena) = setup(2 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let space = AddressSpace::new(&fa, &p2p).unwrap();
            let virt = 0x4000_0000u64;
            let f1 = fa.allocate().unwrap();
            let f2 = fa.allocate().unwrap();
            space
                .map::<TestEncoder>(virt, f1, Permissions::user_rw(), &fa, &p2p)
                .unwrap();
            let again = space.map::<TestEncoder>(virt, f2, Permissions::user_rw(), &fa, &p2p);
            assert!(matches!(
                again,
                Err(KernelError::InvalidState {
                    reason: "page already mapped"
                })
            ));
        }
    }

    #[test]
    fn unaligned_virt_is_rejected() {
        let (fa, arena) = setup(1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let space = AddressSpace::new(&fa, &p2p).unwrap();
            let f = fa.allocate().unwrap();
            let r = space.map::<TestEncoder>(0x4000_0123, f, Permissions::user_rw(), &fa, &p2p);
            assert!(matches!(
                r,
                Err(KernelError::InvalidState {
                    reason: "unaligned virtual address"
                })
            ));
        }
    }

    #[test]
    fn two_spaces_are_independent() {
        // The core isolation property: a mapping in space A is absent in space B.
        let (fa, arena) = setup(4 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let a = AddressSpace::new(&fa, &p2p).unwrap();
            let b = AddressSpace::new(&fa, &p2p).unwrap();
            assert_ne!(a.root(), b.root());
            let virt = 0x4000_0000u64;
            let fa_frame = fa.allocate().unwrap();
            a.map::<TestEncoder>(virt, fa_frame, Permissions::user_rw(), &fa, &p2p)
                .unwrap();
            // Same virtual address is mapped in A but not in B.
            assert!(a.translate::<TestEncoder>(virt, &p2p).is_some());
            assert!(b.translate::<TestEncoder>(virt, &p2p).is_none());
        }
    }

    #[test]
    fn map_range_maps_all_pages() {
        let (fa, arena) = setup(4 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let space = AddressSpace::new(&fa, &p2p).unwrap();
            let virt = 0x8000_0000u64;
            let phys = 0x20_0000u64;
            space
                .map_range::<TestEncoder>(virt, phys, 4, Permissions::kernel_rw(), &fa, &p2p)
                .unwrap();
            for i in 0..4u64 {
                let got = space
                    .translate::<TestEncoder>(virt + i * FRAME_SIZE, &p2p)
                    .unwrap();
                assert_eq!(got, phys + i * FRAME_SIZE);
            }
        }
    }

    #[test]
    fn map_range_uses_2mib_block_when_aligned() {
        // A 2 MiB-aligned range of exactly 512 pages must be mapped as a single
        // 2 MiB block leaf at level LEVELS-2 (not 512 4 KiB pages). Verify by
        // walking to the L2 entry and asserting it is a block leaf, and that
        // translation across the whole 2 MiB region is correct.
        let (fa, arena) = setup(8 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let space = AddressSpace::new(&fa, &p2p).unwrap();
            let virt = 0x4000_0000u64; // 1 GiB, 2 MiB-aligned
            let phys = 0x20_0000u64; // 2 MiB, 2 MiB-aligned
            let pages = ENTRIES as u64; // 512 pages = 2 MiB
            space
                .map_range::<TestEncoder>(virt, phys, pages, Permissions::kernel_rw(), &fa, &p2p)
                .unwrap();

            // Walk to the L2 (level LEVELS-2) entry and confirm it is a block leaf.
            let idx = AddressSpace::indices(virt);
            let mut table = space.root();
            for &index in idx.iter().take(LEVELS - 2) {
                let e = core::ptr::read((p2p(table.addr()) as *const u64).add(index));
                assert!(TestEncoder::is_present(e), "interior must be present");
                assert!(
                    !TestEncoder::is_block_leaf(e, 0),
                    "interior above block level must be a table, not a block"
                );
                table = TestEncoder::entry_frame(e);
            }
            let l2 = core::ptr::read((p2p(table.addr()) as *const u64).add(idx[LEVELS - 2]));
            assert!(
                TestEncoder::is_block_leaf(l2, LEVELS - 2),
                "the 2 MiB-aligned range must be mapped as a block leaf at L2"
            );

            // Translation must be correct across the whole 2 MiB block, including
            // an offset partway in (proving the block-aware translate path).
            assert_eq!(space.translate::<TestEncoder>(virt, &p2p).unwrap(), phys);
            let off = 0x1F_F000u64; // last 4 KiB within the 2 MiB block
            assert_eq!(
                space.translate::<TestEncoder>(virt + off, &p2p).unwrap(),
                phys + off
            );
        }
    }

    #[test]
    fn map_range_falls_back_to_4k_when_unaligned() {
        // A range that is NOT 2 MiB-aligned (or shorter than a block) must use
        // 4 KiB pages — the L2 entry must be a TABLE, not a block leaf.
        let (fa, arena) = setup(8 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let space = AddressSpace::new(&fa, &p2p).unwrap();
            let virt = 0x4000_0000u64 + FRAME_SIZE; // 1 GiB + 4 KiB: NOT 2 MiB-aligned
            let phys = 0x20_0000u64 + FRAME_SIZE;
            space
                .map_range::<TestEncoder>(virt, phys, 8, Permissions::kernel_rw(), &fa, &p2p)
                .unwrap();
            let idx = AddressSpace::indices(virt);
            let mut table = space.root();
            for &index in idx.iter().take(LEVELS - 2) {
                let e = core::ptr::read((p2p(table.addr()) as *const u64).add(index));
                table = TestEncoder::entry_frame(e);
            }
            let l2 = core::ptr::read((p2p(table.addr()) as *const u64).add(idx[LEVELS - 2]));
            assert!(
                !TestEncoder::is_block_leaf(l2, LEVELS - 2),
                "unaligned range must map via a 4 KiB table, not a block"
            );
            // And translation is still correct.
            for i in 0..8u64 {
                assert_eq!(
                    space
                        .translate::<TestEncoder>(virt + i * FRAME_SIZE, &p2p)
                        .unwrap(),
                    phys + i * FRAME_SIZE
                );
            }
        }
    }

    #[test]
    fn map_4k_into_existing_block_is_rejected() {
        // After a 2 MiB block covers a region, a 4 KiB map() into that same region
        // must ERROR (not descend into the block frame as a table and corrupt it).
        let (fa, arena) = setup(8 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let space = AddressSpace::new(&fa, &p2p).unwrap();
            let virt = 0x4000_0000u64; // 2 MiB-aligned
            let phys = 0x20_0000u64;
            space
                .map_block_2m::<TestEncoder>(
                    virt,
                    PhysFrame::containing(phys),
                    Permissions::kernel_rw(),
                    &fa,
                    &p2p,
                )
                .unwrap();
            // A 4 KiB map into the SAME 2 MiB region must be rejected.
            let err = space.map::<TestEncoder>(
                virt + FRAME_SIZE,
                PhysFrame::containing(0x40_0000),
                Permissions::kernel_rw(),
                &fa,
                &p2p,
            );
            assert!(err.is_err(), "4 KiB map into an existing block must be rejected");
        }
    }
}
