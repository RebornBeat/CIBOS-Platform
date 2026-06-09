//! # Physical Frame Allocator
//!
//! Hands out and reclaims 4 KiB physical page frames from the usable regions the
//! firmware reported. This is the foundation of hardware-enforced isolation: the
//! page-table machinery ([`crate::paging`]) draws the frames it needs — for both
//! the page-table nodes themselves and the pages it maps into a boundary's
//! address space — from here.
//!
//! ## Design
//!
//! The allocator is a portable, `no_std`, allocation-light bitmap over the
//! usable physical address space. It is deliberately simple and deterministic:
//! a fixed frame size, a single contiguous frame-number space, and a bitmap with
//! one bit per frame (1 = allocated). Frames outside any usable region, and
//! frames below a caller-supplied watermark (the kernel image, boot structures,
//! and the firmware's identity-mapped low memory), start marked allocated so
//! they are never handed out.
//!
//! Policy and accounting live here in tested code; turning a returned
//! [`PhysFrame`] into a hardware mapping is the architecture backend's job.

use crate::error::{KernelError, KernelResult};
use crate::sync::SpinLock;
use alloc::vec;
use alloc::vec::Vec;
use shared::{MemoryRegion, MemoryRegionKind};

/// Page frame size in bytes (4 KiB; the x86_64 / aarch64 / riscv64 base page).
pub const FRAME_SIZE: u64 = 4096;

/// A physical 4 KiB frame, identified by its base physical address (always
/// frame-aligned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysFrame(u64);

impl PhysFrame {
    /// The frame containing `addr` (rounds down to the frame boundary).
    #[must_use]
    pub const fn containing(addr: u64) -> Self {
        PhysFrame(addr & !(FRAME_SIZE - 1))
    }

    /// The frame with the given frame index (`index * FRAME_SIZE`).
    #[must_use]
    pub const fn from_index(index: u64) -> Self {
        PhysFrame(index * FRAME_SIZE)
    }

    /// The base physical address of this frame.
    #[must_use]
    pub const fn addr(self) -> u64 {
        self.0
    }

    /// The frame index (`addr / FRAME_SIZE`).
    #[must_use]
    pub const fn index(self) -> u64 {
        self.0 / FRAME_SIZE
    }
}

struct AllocState {
    /// One bit per frame, indexed by frame number; 1 = allocated/unavailable.
    bitmap: Vec<u64>,
    /// Number of frames the bitmap covers (frames `0..frame_count`).
    frame_count: u64,
    /// Frames currently allocated (including the initially-reserved ones).
    allocated: u64,
    /// Frames that are usable RAM at all (the ceiling on what can be free).
    usable: u64,
    /// Lowest frame index eligible for allocation (a search hint).
    next_hint: u64,
}

/// A bitmap physical frame allocator over the usable memory map.
pub struct FrameAllocator {
    state: SpinLock<AllocState>,
}

impl FrameAllocator {
    /// Build an allocator from the firmware memory map.
    ///
    /// Every frame fully contained in a [`MemoryRegionKind::Usable`] region and
    /// at or above `reserved_below` starts free; all other frames (non-usable
    /// regions, partially-covered frames, and everything below `reserved_below`)
    /// start allocated so they are never handed out. `reserved_below` must cover
    /// the kernel image, the boot/page-table structures, and the firmware's
    /// identity-mapped low memory.
    #[must_use]
    pub fn from_regions(regions: &[MemoryRegion], reserved_below: u64) -> Self {
        // Highest usable address determines the bitmap size.
        let max_end = regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| r.end())
            .max()
            .unwrap_or(0);
        let frame_count = max_end / FRAME_SIZE;
        let words = frame_count.div_ceil(64) as usize;

        // Start with everything allocated, then free the usable frames.
        let mut bitmap = vec![u64::MAX; words];
        let mut usable = 0u64;

        let reserved_frame = reserved_below.div_ceil(FRAME_SIZE);

        for region in regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
        {
            // Frames fully inside the region: [ceil(base), floor(end)).
            let first = region.base.div_ceil(FRAME_SIZE);
            let last = region.end() / FRAME_SIZE; // exclusive
            let mut f = first;
            while f < last {
                usable += 1;
                if f >= reserved_frame {
                    // Mark free.
                    bitmap[(f / 64) as usize] &= !(1u64 << (f % 64));
                }
                f += 1;
            }
        }

        // Count how many are actually free vs allocated for the accounting.
        let mut allocated = 0u64;
        for f in 0..frame_count {
            if bitmap[(f / 64) as usize] & (1u64 << (f % 64)) != 0 {
                allocated += 1;
            }
        }

        Self {
            state: SpinLock::new(AllocState {
                bitmap,
                frame_count,
                allocated,
                usable,
                next_hint: reserved_frame,
            }),
        }
    }

    /// Allocate one free frame.
    ///
    /// # Errors
    ///
    /// [`KernelError::LimitExceeded`] if no free frame remains.
    pub fn allocate(&self) -> KernelResult<PhysFrame> {
        let mut s = self.state.lock();
        let count = s.frame_count;
        let start = s.next_hint;
        // Search from the hint to the end, then wrap to the beginning.
        for f in (start..count).chain(0..start) {
            let word = (f / 64) as usize;
            let bit = 1u64 << (f % 64);
            if s.bitmap[word] & bit == 0 {
                s.bitmap[word] |= bit;
                s.allocated += 1;
                s.next_hint = f + 1;
                return Ok(PhysFrame::from_index(f));
            }
        }
        Err(KernelError::LimitExceeded {
            resource: "physical frames",
        })
    }

    /// Allocate one free frame and zero it through the given identity map.
    ///
    /// `phys_to_ptr` maps a physical address to a writable pointer valid in the
    /// current address space (on the booted kernel this is the identity map the
    /// bootloader installed). Used for page-table nodes, which must be zeroed
    /// before use.
    ///
    /// # Errors
    ///
    /// Propagates [`allocate`](Self::allocate)'s error.
    ///
    /// # Safety
    ///
    /// `phys_to_ptr` must return a pointer to `FRAME_SIZE` writable, mapped bytes
    /// for the returned frame.
    pub unsafe fn allocate_zeroed(
        &self,
        phys_to_ptr: impl Fn(u64) -> *mut u8,
    ) -> KernelResult<PhysFrame> {
        let frame = self.allocate()?;
        let ptr = phys_to_ptr(frame.addr());
        core::ptr::write_bytes(ptr, 0, FRAME_SIZE as usize);
        Ok(frame)
    }

    /// Return a frame to the free pool. Double-frees are ignored (idempotent).
    pub fn free(&self, frame: PhysFrame) {
        let mut s = self.state.lock();
        let f = frame.index();
        if f >= s.frame_count {
            return;
        }
        let word = (f / 64) as usize;
        let bit = 1u64 << (f % 64);
        if s.bitmap[word] & bit != 0 {
            s.bitmap[word] &= !bit;
            s.allocated = s.allocated.saturating_sub(1);
            if f < s.next_hint {
                s.next_hint = f;
            }
        }
    }

    /// Total usable frames (free + allocated, excluding non-RAM).
    #[must_use]
    pub fn usable_frames(&self) -> u64 {
        self.state.lock().usable
    }

    /// Currently free frames available for allocation.
    #[must_use]
    pub fn free_frames(&self) -> u64 {
        let s = self.state.lock();
        // Free = usable frames that are not marked allocated. Frames marked
        // allocated include the reserved-below region, which is not part of
        // `usable`, so compute against the bitmap directly.
        let mut free = 0u64;
        for f in 0..s.frame_count {
            if s.bitmap[(f / 64) as usize] & (1u64 << (f % 64)) == 0 {
                free += 1;
            }
        }
        free
    }

    /// Whether the given frame is currently allocated.
    #[must_use]
    pub fn is_allocated(&self, frame: PhysFrame) -> bool {
        let s = self.state.lock();
        let f = frame.index();
        if f >= s.frame_count {
            return true;
        }
        s.bitmap[(f / 64) as usize] & (1u64 << (f % 64)) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> Vec<MemoryRegion> {
        alloc::vec![
            MemoryRegion {
                base: 0,
                length: 0x10_0000, // first 1 MiB: firmware-reserved
                kind: MemoryRegionKind::FirmwareReserved,
            },
            MemoryRegion {
                base: 0x10_0000,
                length: 0x100_0000, // 16 MiB usable starting at 1 MiB
                kind: MemoryRegionKind::Usable,
            },
        ]
    }

    #[test]
    fn counts_usable_frames() {
        // 16 MiB / 4 KiB = 4096 usable frames; none reserved below 1 MiB.
        let fa = FrameAllocator::from_regions(&regions(), 0x10_0000);
        assert_eq!(fa.usable_frames(), 0x100_0000 / FRAME_SIZE);
        assert_eq!(fa.free_frames(), 0x100_0000 / FRAME_SIZE);
    }

    #[test]
    fn reserved_below_is_not_handed_out() {
        // Reserve the first 2 MiB (1 MiB hole + first 1 MiB of usable).
        let fa = FrameAllocator::from_regions(&regions(), 0x20_0000);
        let frame = fa.allocate().expect("frame");
        assert!(
            frame.addr() >= 0x20_0000,
            "allocated below the watermark: {:#x}",
            frame.addr()
        );
    }

    #[test]
    fn allocate_free_roundtrip() {
        let fa = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let before = fa.free_frames();
        let a = fa.allocate().unwrap();
        let b = fa.allocate().unwrap();
        assert_ne!(a, b);
        assert!(fa.is_allocated(a));
        assert_eq!(fa.free_frames(), before - 2);
        fa.free(a);
        assert!(!fa.is_allocated(a));
        assert_eq!(fa.free_frames(), before - 1);
        // Double-free is a no-op.
        fa.free(a);
        assert_eq!(fa.free_frames(), before - 1);
    }

    #[test]
    fn frames_are_aligned_and_in_range() {
        let fa = FrameAllocator::from_regions(&regions(), 0x10_0000);
        for _ in 0..16 {
            let f = fa.allocate().unwrap();
            assert_eq!(f.addr() % FRAME_SIZE, 0);
            assert!(f.addr() >= 0x10_0000);
            assert!(f.addr() < 0x110_0000);
        }
    }

    #[test]
    fn exhaustion_is_reported() {
        // Tiny usable region: exactly 4 frames.
        let regions = alloc::vec![MemoryRegion {
            base: 0x10_0000,
            length: 4 * FRAME_SIZE,
            kind: MemoryRegionKind::Usable,
        }];
        let fa = FrameAllocator::from_regions(&regions, 0x10_0000);
        for _ in 0..4 {
            fa.allocate().unwrap();
        }
        assert!(matches!(
            fa.allocate(),
            Err(KernelError::LimitExceeded {
                resource: "physical frames"
            })
        ));
    }

    #[test]
    fn freed_frame_is_reused() {
        let fa = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let a = fa.allocate().unwrap();
        fa.free(a);
        // The hint moves back, so the next allocation reuses the freed frame.
        let b = fa.allocate().unwrap();
        assert_eq!(a, b);
    }
}
