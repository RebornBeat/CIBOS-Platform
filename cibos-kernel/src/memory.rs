//! # Memory Manager
//!
//! Tracks the physical memory the firmware reported in the handoff map and the
//! per-boundary reservations made against it.
//!
//! This is the kernel's memory *accounting*, not its page-table machinery. It
//! answers "how much usable RAM exists, and how much has each isolation
//! boundary reserved" — the questions the isolation model needs — and enforces
//! that the sum of reservations never exceeds the usable total. Mapping pages
//! into a boundary's address space is architecture glue handled in the kernel
//! binary; the policy and accounting live here, in portable, tested code.

use crate::error::{KernelError, KernelResult};
use crate::sync::SpinLock;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use shared::{BoundaryId, MemoryRegion, MemoryRegionKind};

struct MemState {
    regions: Vec<MemoryRegion>,
    total_usable: u64,
    reserved: u64,
    by_boundary: BTreeMap<BoundaryId, u64>,
}

/// Physical memory accounting for the kernel.
pub struct MemoryManager {
    state: SpinLock<MemState>,
}

impl MemoryManager {
    /// Build a manager from the firmware memory map, summing the usable regions.
    #[must_use]
    pub fn from_regions(regions: &[MemoryRegion]) -> Self {
        let usable: Vec<MemoryRegion> = regions
            .iter()
            .copied()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .collect();
        let total_usable = usable.iter().map(|r| r.length).fold(0u64, u64::saturating_add);
        Self {
            state: SpinLock::new(MemState {
                regions: usable,
                total_usable,
                reserved: 0,
                by_boundary: BTreeMap::new(),
            }),
        }
    }

    /// Total usable RAM in bytes.
    #[must_use]
    pub fn total_usable(&self) -> u64 {
        self.state.lock().total_usable
    }

    /// Currently unreserved RAM in bytes.
    #[must_use]
    pub fn available(&self) -> u64 {
        let s = self.state.lock();
        s.total_usable.saturating_sub(s.reserved)
    }

    /// Number of usable regions.
    #[must_use]
    pub fn region_count(&self) -> usize {
        self.state.lock().regions.len()
    }

    /// Reserve `bytes` for `boundary`, adding to any existing reservation.
    ///
    /// # Errors
    ///
    /// [`KernelError::LimitExceeded`] if the reservation would exceed the usable
    /// total.
    pub fn reserve(&self, boundary: BoundaryId, bytes: u64) -> KernelResult<()> {
        let mut s = self.state.lock();
        let new_reserved = s.reserved.saturating_add(bytes);
        if new_reserved > s.total_usable {
            return Err(KernelError::LimitExceeded { resource: "memory" });
        }
        s.reserved = new_reserved;
        *s.by_boundary.entry(boundary).or_insert(0) += bytes;
        Ok(())
    }

    /// Release all memory reserved by `boundary` (called when it is destroyed).
    pub fn release_all(&self, boundary: BoundaryId) {
        let mut s = self.state.lock();
        if let Some(amount) = s.by_boundary.remove(&boundary) {
            s.reserved = s.reserved.saturating_sub(amount);
        }
    }

    /// Current reservation for `boundary`.
    #[must_use]
    pub fn reservation(&self, boundary: BoundaryId) -> u64 {
        self.state.lock().by_boundary.get(&boundary).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> Vec<MemoryRegion> {
        alloc::vec![
            MemoryRegion {
                base: 0,
                length: 0x10_0000,
                kind: MemoryRegionKind::FirmwareReserved,
            },
            MemoryRegion {
                base: 0x10_0000,
                length: 0x4000_0000, // 1 GiB usable
                kind: MemoryRegionKind::Usable,
            },
        ]
    }

    #[test]
    fn sums_only_usable_regions() {
        let mm = MemoryManager::from_regions(&regions());
        assert_eq!(mm.total_usable(), 0x4000_0000);
        assert_eq!(mm.region_count(), 1);
        assert_eq!(mm.available(), 0x4000_0000);
    }

    #[test]
    fn reserve_and_release() {
        let mm = MemoryManager::from_regions(&regions());
        let b = BoundaryId::new(1);
        mm.reserve(b, 0x1000_0000).unwrap(); // 256 MiB
        assert_eq!(mm.reservation(b), 0x1000_0000);
        assert_eq!(mm.available(), 0x3000_0000);
        mm.release_all(b);
        assert_eq!(mm.reservation(b), 0);
        assert_eq!(mm.available(), 0x4000_0000);
    }

    #[test]
    fn over_reservation_rejected() {
        let mm = MemoryManager::from_regions(&regions());
        let b = BoundaryId::new(1);
        let r = mm.reserve(b, 0x4000_0000 + 1);
        assert!(matches!(r, Err(KernelError::LimitExceeded { resource: "memory" })));
        // Failed reservation does not change accounting.
        assert_eq!(mm.available(), 0x4000_0000);
    }

    #[test]
    fn reservations_accumulate() {
        let mm = MemoryManager::from_regions(&regions());
        let b = BoundaryId::new(1);
        mm.reserve(b, 0x1000_0000).unwrap();
        mm.reserve(b, 0x1000_0000).unwrap();
        assert_eq!(mm.reservation(b), 0x2000_0000);
    }
}
