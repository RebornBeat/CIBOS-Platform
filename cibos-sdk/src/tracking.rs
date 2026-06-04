//! Host memory accounting (feature `host-memory-tracking`).
//!
//! A counting global allocator that wraps the system allocator and maintains the
//! process's live and peak allocated-byte totals. On the host development
//! transport the application is a single in-process container, so the process
//! heap is that container's memory, and these totals back
//! [`container::memory_usage`](crate::container::memory_usage).
//!
//! This is intentionally opt-in: defining a `#[global_allocator]` is a
//! process-wide decision, so a binary that enables this feature must not install
//! its own. The wrapper is transparent (it forwards every operation to
//! [`System`]); only the byte counters are added. `realloc` and `alloc_zeroed`
//! are left to the `GlobalAlloc` defaults, which route through this type's
//! `alloc`/`dealloc`, so they are counted consistently.
//!
//! In a production CIBOS deployment the kernel tracks container memory directly;
//! this module exists so the host transport can report the same figures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// A transparent counting wrapper around the system allocator.
struct TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            let live = ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            // Raise the high-water mark if this allocation set a new peak.
            let mut peak = PEAK.load(Ordering::Relaxed);
            while live > peak {
                match PEAK.compare_exchange_weak(
                    peak,
                    live,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(observed) => peak = observed,
                }
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static GLOBAL: TrackingAllocator = TrackingAllocator;

/// Bytes currently allocated across the process (the host container).
#[must_use]
pub(crate) fn allocated_bytes() -> usize {
    ALLOCATED.load(Ordering::Relaxed)
}

/// The high-water mark of allocated bytes since the process started.
#[must_use]
pub(crate) fn peak_bytes() -> usize {
    PEAK.load(Ordering::Relaxed)
}
