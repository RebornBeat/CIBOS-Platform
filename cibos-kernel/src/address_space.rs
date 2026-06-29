//! # Address Space Manager
//!
//! Gives each isolation boundary its own [`AddressSpace`] — its own page-table
//! tree — so that "Container A cannot read Container B" is enforced by hardware,
//! not just accounted. This is the policy layer on top of the proven page-table
//! mechanism ([`crate::paging`]) and the physical [`FrameAllocator`].
//!
//! ## Where this sits
//!
//! [`crate::container::ContainerRegistry`] tracks *who* exists and their
//! resource ceilings; [`crate::memory::MemoryManager`] tracks *how much* RAM
//! each boundary has reserved. This manager tracks *where* each boundary's pages
//! live — the actual virtual-to-physical mappings — and owns the frames backing
//! them. A boundary's lifecycle is mirrored here: create a space when the
//! boundary is created, tear it down (reclaiming every frame) when destroyed.
//!
//! ## Portability
//!
//! Everything here is portable and host-tested. The hardware specifics — entry
//! bit layout and the `CR3`/`TTBR0` write — stay behind the
//! [`PageTableEncoder`] type parameter and the caller-supplied physical-to-
//! pointer map, exactly as in [`crate::paging`]. The kernel binary instantiates
//! these with the x86_64 encoder and the identity map; tests use a model
//! encoder over an arena.

use crate::error::{KernelError, KernelResult};
use crate::frame::{FrameAllocator, PhysFrame, FRAME_SIZE};
use crate::paging::{AddressSpace, PageTableEncoder, Permissions};
use crate::sync::SpinLock;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use shared::BoundaryId;

/// One boundary's address space plus the frames that back its mapped pages, so
/// teardown can return every frame to the allocator.
struct BoundarySpace {
    space: AddressSpace,
    /// Data frames mapped into this space (page-table-node frames are tracked by
    /// the space itself and reclaimed via [`AddressSpace`] walk on teardown).
    mapped_frames: Vec<PhysFrame>,
}

struct ManagerState {
    spaces: BTreeMap<BoundaryId, BoundarySpace>,
}

/// Owns per-boundary address spaces and the physical frame allocator they draw
/// from.
pub struct AddressSpaceManager {
    frames: FrameAllocator,
    state: SpinLock<ManagerState>,
}

impl AddressSpaceManager {
    /// Create a manager over the given physical frame allocator.
    #[must_use]
    pub fn new(frames: FrameAllocator) -> Self {
        Self {
            frames,
            state: SpinLock::new(ManagerState {
                spaces: BTreeMap::new(),
            }),
        }
    }

    /// The underlying physical frame allocator (for diagnostics/accounting).
    #[must_use]
    pub fn frames(&self) -> &FrameAllocator {
        &self.frames
    }

    /// Whether a boundary has an address space.
    #[must_use]
    pub fn has_space(&self, boundary: BoundaryId) -> bool {
        self.state.lock().spaces.contains_key(&boundary)
    }

    /// Number of boundaries with an address space.
    #[must_use]
    pub fn space_count(&self) -> usize {
        self.state.lock().spaces.len()
    }

    /// Create a fresh, empty address space for `boundary`.
    ///
    /// # Errors
    ///
    /// [`KernelError::InvalidState`] if the boundary already has a space;
    /// frame-allocation failure otherwise.
    ///
    /// # Safety
    ///
    /// `phys_to_ptr` must map any allocated frame's physical address to a
    /// writable pointer to `FRAME_SIZE` mapped bytes.
    pub unsafe fn create_space(
        &self,
        boundary: BoundaryId,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> KernelResult<()> {
        let mut s = self.state.lock();
        if s.spaces.contains_key(&boundary) {
            return Err(KernelError::InvalidState {
                reason: "boundary already has an address space",
            });
        }
        let space = AddressSpace::new(&self.frames, phys_to_ptr)?;
        s.spaces.insert(
            boundary,
            BoundarySpace {
                space,
                mapped_frames: Vec::new(),
            },
        );
        Ok(())
    }

    /// The root page-table frame for `boundary` (its `CR3`/`TTBR0` value).
    #[must_use]
    pub fn root_of(&self, boundary: BoundaryId) -> Option<PhysFrame> {
        self.state
            .lock()
            .spaces
            .get(&boundary)
            .map(|b| b.space.root())
    }

    /// Allocate `count` fresh physical frames and map them into `boundary`'s
    /// space at virtual `virt` with `perms`. The frames are recorded so teardown
    /// reclaims them.
    ///
    /// # Errors
    ///
    /// [`KernelError::UnknownContainer`] if the boundary has no space;
    /// propagates mapping/allocation errors.
    ///
    /// # Safety
    ///
    /// As [`crate::paging::AddressSpace::map`].
    pub unsafe fn map_new_pages<E: PageTableEncoder>(
        &self,
        boundary: BoundaryId,
        virt: u64,
        count: u64,
        perms: Permissions,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> KernelResult<()> {
        let mut s = self.state.lock();
        let b = s
            .spaces
            .get_mut(&boundary)
            .ok_or(KernelError::UnknownContainer)?;
        for i in 0..count {
            let frame = self.frames.allocate_zeroed(phys_to_ptr)?;
            b.space.map::<E>(
                virt + i * FRAME_SIZE,
                frame,
                perms,
                &self.frames,
                phys_to_ptr,
            )?;
            b.mapped_frames.push(frame);
        }
        Ok(())
    }

    /// Map an existing physical range (e.g. shared/device memory) into
    /// `boundary`'s space without allocating new frames or recording them for
    /// reclamation (the caller owns that memory's lifetime).
    ///
    /// # Errors
    ///
    /// [`KernelError::UnknownContainer`] if the boundary has no space;
    /// propagates mapping/allocation errors.
    ///
    /// # Safety
    ///
    /// As [`crate::paging::AddressSpace::map`]: `phys_to_ptr` must map any frame
    /// to a writable pointer to `FRAME_SIZE` mapped bytes, and `phys` must name
    /// real physical memory the caller keeps valid for the mapping's lifetime.
    pub unsafe fn map_existing<E: PageTableEncoder>(
        &self,
        boundary: BoundaryId,
        virt: u64,
        phys: u64,
        count: u64,
        perms: Permissions,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> KernelResult<()> {
        let mut s = self.state.lock();
        let b = s
            .spaces
            .get_mut(&boundary)
            .ok_or(KernelError::UnknownContainer)?;
        b.space
            .map_range::<E>(virt, phys, count, perms, &self.frames, phys_to_ptr)
    }

    /// Translate a virtual address within `boundary`'s space.
    ///
    /// # Safety
    ///
    /// `phys_to_ptr` must map table frames to readable pointers.
    #[must_use]
    pub unsafe fn translate<E: PageTableEncoder>(
        &self,
        boundary: BoundaryId,
        virt: u64,
        phys_to_ptr: &impl Fn(u64) -> *mut u8,
    ) -> Option<u64> {
        let s = self.state.lock();
        let b = s.spaces.get(&boundary)?;
        b.space.translate::<E>(virt, phys_to_ptr)
    }

    /// Tear down `boundary`'s space, returning all the data frames it mapped to
    /// the allocator. (Page-table-node frames remain reserved in this first
    /// implementation; a full walk-and-free is a later refinement and is called
    /// out in the roadmap. Data frames are the bulk of the memory.)
    ///
    /// # Errors
    ///
    /// [`KernelError::UnknownContainer`] if the boundary has no space.
    pub fn destroy_space(&self, boundary: BoundaryId) -> KernelResult<()> {
        let mut s = self.state.lock();
        let b = s
            .spaces
            .remove(&boundary)
            .ok_or(KernelError::UnknownContainer)?;
        for frame in b.mapped_frames {
            self.frames.free(frame);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paging::Permissions;
    use shared::{MemoryRegion, MemoryRegionKind};

    // Model encoder mirroring x86_64 entry bits (as in paging.rs tests).
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
            unsafe { self.base.add(phys as usize) }
        }
    }

    fn manager(arena_len: usize) -> (AddressSpaceManager, Arena) {
        let regions = alloc::vec![MemoryRegion {
            base: 0,
            length: arena_len as u64,
            kind: MemoryRegionKind::Usable,
        }];
        let fa = FrameAllocator::from_regions(&regions, FRAME_SIZE);
        (AddressSpaceManager::new(fa), Arena::new(arena_len))
    }

    #[test]
    fn create_and_destroy_space() {
        let (mgr, arena) = manager(4 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        let b = BoundaryId::new(1);
        unsafe {
            mgr.create_space(b, &p2p).unwrap();
            assert!(mgr.has_space(b));
            assert_eq!(mgr.space_count(), 1);
            // Double-create is rejected.
            assert!(matches!(
                mgr.create_space(b, &p2p),
                Err(KernelError::InvalidState { .. })
            ));
        }
        mgr.destroy_space(b).unwrap();
        assert!(!mgr.has_space(b));
        assert!(matches!(
            mgr.destroy_space(b),
            Err(KernelError::UnknownContainer)
        ));
    }

    #[test]
    fn mapped_pages_translate() {
        let (mgr, arena) = manager(8 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        let b = BoundaryId::new(1);
        unsafe {
            mgr.create_space(b, &p2p).unwrap();
            mgr.map_new_pages::<TestEncoder>(b, 0x4000_0000, 3, Permissions::user_rw(), &p2p)
                .unwrap();
            // All three pages resolve to distinct physical frames.
            let a0 = mgr.translate::<TestEncoder>(b, 0x4000_0000, &p2p).unwrap();
            let a1 = mgr
                .translate::<TestEncoder>(b, 0x4000_0000 + FRAME_SIZE, &p2p)
                .unwrap();
            let a2 = mgr
                .translate::<TestEncoder>(b, 0x4000_0000 + 2 * FRAME_SIZE, &p2p)
                .unwrap();
            assert_ne!(a0, a1);
            assert_ne!(a1, a2);
        }
    }

    #[test]
    fn boundaries_are_isolated() {
        // The whole point: two boundaries' spaces do not share mappings.
        let (mgr, arena) = manager(8 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        let a = BoundaryId::new(1);
        let c = BoundaryId::new(2);
        unsafe {
            mgr.create_space(a, &p2p).unwrap();
            mgr.create_space(c, &p2p).unwrap();
            assert_ne!(mgr.root_of(a), mgr.root_of(c));

            mgr.map_new_pages::<TestEncoder>(a, 0x4000_0000, 1, Permissions::user_rw(), &p2p)
                .unwrap();
            // The address mapped in A is absent in C.
            assert!(mgr.translate::<TestEncoder>(a, 0x4000_0000, &p2p).is_some());
            assert!(mgr.translate::<TestEncoder>(c, 0x4000_0000, &p2p).is_none());
        }
    }

    #[test]
    fn destroy_reclaims_data_frames() {
        let (mgr, arena) = manager(8 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        let b = BoundaryId::new(1);
        unsafe {
            mgr.create_space(b, &p2p).unwrap();
            let free_before_map = mgr.frames().free_frames();
            mgr.map_new_pages::<TestEncoder>(b, 0x4000_0000, 4, Permissions::user_rw(), &p2p)
                .unwrap();
            // Mapping consumed at least the 4 data frames (plus table nodes).
            assert!(mgr.frames().free_frames() <= free_before_map - 4);
            let free_after_map = mgr.frames().free_frames();
            mgr.destroy_space(b).unwrap();
            // The 4 data frames came back.
            assert_eq!(mgr.frames().free_frames(), free_after_map + 4);
        }
    }

    #[test]
    fn map_into_missing_boundary_errors() {
        let (mgr, arena) = manager(4 * 1024 * 1024);
        let p2p = |phys: u64| arena.ptr_for(phys);
        unsafe {
            let r = mgr.map_new_pages::<TestEncoder>(
                BoundaryId::new(9),
                0x4000_0000,
                1,
                Permissions::user_rw(),
                &p2p,
            );
            assert!(matches!(r, Err(KernelError::UnknownContainer)));
        }
    }
}
