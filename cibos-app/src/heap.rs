//! A small, real heap allocator for CIBOS applications.
//!
//! The kernel maps a writable heap region for each application and hands its
//! base and size to `_start` (see [`crate::rt`]); the application installs that
//! region with [`init`], after which `alloc`/`Box`/`Vec`/`String` work. This is
//! a genuine free-list allocator — it splits blocks on allocation and coalesces
//! adjacent free blocks on deallocation, so long-running programs that allocate
//! and free repeatedly do not fragment the heap into uselessness. Allocation is
//! a linear first-fit scan: correct and dependency-free, which is what an
//! on-kernel runtime needs.
//!
//! Layout of an allocation: each free block begins with a [`Block`] header
//! (`size`, `next`). On allocation, the chosen block's base is recorded and the
//! returned payload is aligned; the 8 bytes immediately *before* the payload
//! store the block base, so [`Heap::dealloc`] recovers the block in O(1).
//!
//! Single-threaded: CIBOS applications are currently single-threaded, so the
//! allocator uses a non-reentrant interior-mutability cell rather than a lock.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::mem::{align_of, size_of};
use core::ptr;

/// A free-list node stored at the start of each free block. `size` is the total
/// block size in bytes, including this header.
#[repr(C)]
struct Block {
    size: usize,
    next: *mut Block,
}

const BLOCK_HDR: usize = size_of::<Block>();
const WORD: usize = size_of::<usize>();
/// Minimum alignment/granularity (enough to place a `Block` header).
const MIN_ALIGN: usize = align_of::<Block>();

#[inline]
fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

struct Heap {
    head: *mut Block,
    initialized: bool,
}

impl Heap {
    const fn new() -> Self {
        Heap {
            head: ptr::null_mut(),
            initialized: false,
        }
    }

    /// Install `[base, base+size)` as a single free block.
    ///
    /// # Safety
    ///
    /// The region must be valid, writable, and unused for the program lifetime.
    unsafe fn init(&mut self, base: usize, size: usize) {
        if self.initialized || size < BLOCK_HDR {
            return;
        }
        let block = base as *mut Block;
        unsafe {
            (*block).size = size;
            (*block).next = ptr::null_mut();
        }
        self.head = block;
        self.initialized = true;
    }

    /// First-fit allocation. Returns an aligned payload with the owning block's
    /// base stashed in the word just before it, or null if the heap is full.
    unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(MIN_ALIGN);
        let want = align_up(layout.size().max(1), MIN_ALIGN);

        let mut prev: *mut Block = ptr::null_mut();
        let mut cur = self.head;
        while !cur.is_null() {
            let base = cur as usize;
            let cur_size = unsafe { (*cur).size };
            let next = unsafe { (*cur).next };
            // Payload must clear the block header AND leave a word before it for
            // the stashed base; align the payload start.
            let min_payload = base + BLOCK_HDR + WORD;
            let payload = align_up(min_payload, align);
            let used_end = payload + want;
            if used_end <= base + cur_size {
                let used = used_end - base;
                let leftover = cur_size - used;
                unsafe {
                    if leftover >= BLOCK_HDR {
                        // Split: keep `used` here, free the tail remainder.
                        let nb = (base + used) as *mut Block;
                        (*nb).size = leftover;
                        (*nb).next = next;
                        if prev.is_null() {
                            self.head = nb;
                        } else {
                            (*prev).next = nb;
                        }
                        (*cur).size = used;
                    } else {
                        // Consume the whole block.
                        if prev.is_null() {
                            self.head = next;
                        } else {
                            (*prev).next = next;
                        }
                    }
                    // Stash the block base in the word before the payload.
                    *((payload - WORD) as *mut usize) = base;
                }
                return payload as *mut u8;
            }
            prev = cur;
            cur = next;
        }
        ptr::null_mut()
    }

    /// Free a payload from [`alloc`](Heap::alloc), recovering its block via the
    /// stashed base and coalescing adjacent free blocks.
    unsafe fn dealloc(&mut self, payload: *mut u8) {
        if payload.is_null() {
            return;
        }
        let p = payload as usize;
        let base = unsafe { *((p - WORD) as *const usize) };
        let block = base as *mut Block;
        unsafe {
            // `(*block).size` still holds the block's true size from alloc.
            (*block).next = self.head;
            self.head = block;
            self.coalesce();
        }
    }

    /// Merge physically-adjacent free blocks (first block's end == second's
    /// start). O(n^2) over the free list; fine for small application heaps.
    unsafe fn coalesce(&mut self) {
        let mut a = self.head;
        while !a.is_null() {
            let a_end = a as usize + unsafe { (*a).size };
            let mut prev: *mut Block = ptr::null_mut();
            let mut b = self.head;
            let mut restarted = false;
            while !b.is_null() {
                if b != a && b as usize == a_end {
                    unsafe {
                        (*a).size += (*b).size;
                        let bnext = (*b).next;
                        if prev.is_null() {
                            self.head = bnext;
                        } else {
                            (*prev).next = bnext;
                        }
                    }
                    prev = ptr::null_mut();
                    b = self.head;
                    restarted = true;
                    continue;
                }
                prev = b;
                b = unsafe { (*b).next };
            }
            let _ = restarted;
            a = unsafe { (*a).next };
        }
    }
}

/// Global-allocator wrapper providing single-threaded interior mutability.
pub struct AppAlloc {
    heap: UnsafeCell<Heap>,
}

// SAFETY: CIBOS applications are single-threaded; no concurrent access occurs.
unsafe impl Sync for AppAlloc {}

impl AppAlloc {
    const fn new() -> Self {
        AppAlloc {
            heap: UnsafeCell::new(Heap::new()),
        }
    }
}

unsafe impl GlobalAlloc for AppAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let heap = unsafe { &mut *self.heap.get() };
        unsafe { heap.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        let heap = unsafe { &mut *self.heap.get() };
        unsafe { heap.dealloc(ptr) }
    }
}

/// The process global allocator (bare target only; on the host, std's allocator
/// is used so the crate's own tests can run).
#[cfg(target_os = "none")]
#[global_allocator]
static ALLOCATOR: AppAlloc = AppAlloc::new();

/// On the host, a plain static instance used by the unit tests (not registered
/// as the global allocator).
#[cfg(not(target_os = "none"))]
static ALLOCATOR: AppAlloc = AppAlloc::new();

/// Install the application heap region. Call once at startup before any
/// allocation (the runtime entry does this).
///
/// # Safety
///
/// `base..base+size` must be a valid, writable, otherwise-unused region for the
/// program's lifetime.
pub unsafe fn init(base: usize, size: usize) {
    let heap = unsafe { &mut *ALLOCATOR.heap.get() };
    unsafe { heap.init(base, size) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a `Heap` over a heap-allocated backing buffer (host test only).
    fn with_heap(bytes: usize, f: impl FnOnce(&mut Heap, usize)) {
        // A large, well-aligned backing region from the host allocator.
        let mut backing = alloc::vec![0u8; bytes + 64];
        let base = {
            let raw = backing.as_mut_ptr() as usize;
            align_up(raw, MIN_ALIGN)
        };
        let usable = bytes;
        let mut heap = Heap::new();
        unsafe { heap.init(base, usable) };
        f(&mut heap, usable);
        // keep `backing` alive
        core::hint::black_box(&backing);
    }

    #[test]
    fn alloc_then_dealloc_reuses_space() {
        with_heap(4096, |heap, _| {
            let l = Layout::from_size_align(64, 8).unwrap();
            let a = unsafe { heap.alloc(l) };
            assert!(!a.is_null());
            // Write through the pointer to prove it is usable memory.
            unsafe { core::ptr::write_bytes(a, 0xAB, 64) };
            unsafe { heap.dealloc(a) };
            // After free+coalesce the next same-size alloc should succeed again.
            let b = unsafe { heap.alloc(l) };
            assert!(!b.is_null());
        });
    }

    #[test]
    fn many_allocs_until_full_then_free_all() {
        with_heap(8192, |heap, _| {
            let l = Layout::from_size_align(128, 8).unwrap();
            let mut ptrs = alloc::vec::Vec::new();
            loop {
                let p = unsafe { heap.alloc(l) };
                if p.is_null() {
                    break;
                }
                unsafe { core::ptr::write_bytes(p, 0x11, 128) };
                ptrs.push(p);
            }
            assert!(ptrs.len() > 10, "should fit many 128B blocks in 8 KiB");
            for p in &ptrs {
                unsafe { heap.dealloc(*p) };
            }
            // Coalescing should restore a large contiguous block: a big alloc now
            // succeeds.
            let big = Layout::from_size_align(4096, 8).unwrap();
            let b = unsafe { heap.alloc(big) };
            assert!(!b.is_null(), "heap should be coalesced back to one big block");
        });
    }

    #[test]
    fn respects_alignment() {
        with_heap(4096, |heap, _| {
            for &align in &[16usize, 32, 64, 256] {
                let l = Layout::from_size_align(48, align).unwrap();
                let p = unsafe { heap.alloc(l) };
                assert!(!p.is_null());
                assert_eq!(p as usize % align, 0, "payload must be {align}-aligned");
            }
        });
    }

    #[test]
    fn distinct_allocations_do_not_overlap() {
        with_heap(4096, |heap, _| {
            let l = Layout::from_size_align(64, 8).unwrap();
            let a = unsafe { heap.alloc(l) } as usize;
            let b = unsafe { heap.alloc(l) } as usize;
            assert!(a != 0 && b != 0);
            // The two 64-byte payloads must not overlap.
            assert!(a + 64 <= b || b + 64 <= a);
        });
    }
}
