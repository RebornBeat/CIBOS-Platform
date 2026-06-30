//! virtio-blk over the virtio-MMIO transport (polled) — a real block device for
//! the platforms that present virtio devices as memory-mapped register banks
//! rather than on a PCI bus (aarch64 / riscv64 `virt`, and many real ARM/RISC-V
//! boards and hypervisors).
//!
//! This is the MMIO analogue of the PCI [`virtio_net`](super::virtio_net) driver
//! and implements the same real contract a production kernel uses:
//!   * the virtio-MMIO register interface (MagicValue/Version/DeviceID/...),
//!   * the legacy/modern device-init handshake (reset → ACK → DRIVER → features →
//!     FEATURES_OK → DRIVER_OK),
//!   * a single split virtqueue (descriptor table + avail ring + used ring), and
//!   * the virtio-blk request protocol (a 3-descriptor chain: a 16-byte request
//!     header, the data buffer, and a 1-byte status), polled to completion.
//!
//! The MMIO base address is NOT hardcoded: it is discovered from the platform
//! device tree (`virtio_mmio@...` nodes) and probed for a block device. Every
//! register access below is a real device access; there is no stub.

use cibos_kernel::block::{check_range, BlockDevice, BlockError, BLOCK_SIZE};
use cibos_kernel::sync::SpinLock;
use cibos_kernel::FrameAllocator;
use core::sync::atomic::{fence, Ordering};

// --- virtio-MMIO register offsets (virtio 1.0, section 4.2.2) ---
const MMIO_MAGIC: usize = 0x000; // R: 0x74726976 ("virt")
const MMIO_VERSION: usize = 0x004; // R: 1 = legacy, 2 = modern
const MMIO_DEVICE_ID: usize = 0x008; // R: 2 = block device
const MMIO_DEVICE_FEATURES: usize = 0x010; // R: device feature bits (windowed)
const MMIO_DEVICE_FEATURES_SEL: usize = 0x014; // W: feature bank select
const MMIO_DRIVER_FEATURES: usize = 0x020; // W: driver feature bits (windowed)
const MMIO_DRIVER_FEATURES_SEL: usize = 0x024; // W: feature bank select
const MMIO_GUEST_PAGE_SIZE: usize = 0x028; // W: legacy guest page size
const MMIO_QUEUE_SEL: usize = 0x030; // W: select queue
const MMIO_QUEUE_NUM_MAX: usize = 0x034; // R: max queue size (0 = absent)
const MMIO_QUEUE_NUM: usize = 0x038; // W: chosen queue size
const MMIO_QUEUE_ALIGN: usize = 0x03c; // W: legacy used-ring alignment
const MMIO_QUEUE_PFN: usize = 0x040; // RW: legacy queue page-frame number
const MMIO_QUEUE_NOTIFY: usize = 0x050; // W: notify a queue
const MMIO_STATUS: usize = 0x070; // RW: device status
const MMIO_CONFIG: usize = 0x100; // device-specific config (blk: capacity)

const MMIO_MAGIC_VALUE: u32 = 0x7472_6976;
const DEVICE_ID_BLOCK: u32 = 2;

// Device status bits.
const STATUS_ACK: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_FAILED: u32 = 128;

// virtio-blk request types.
const VIRTIO_BLK_T_IN: u32 = 0; // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write

// virtio-blk status (last byte of a request).
const VIRTIO_BLK_S_OK: u8 = 0;

// Split-virtqueue descriptor flags.
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

const QUEUE_ALIGN: usize = 4096; // legacy used-ring page alignment

/// One 16-byte split-virtqueue descriptor.
#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// The 16-byte virtio-blk request header.
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

#[inline]
unsafe fn mmio_r(base: usize, off: usize) -> u32 {
    core::ptr::read_volatile((base + off) as *const u32)
}

#[inline]
unsafe fn mmio_w(base: usize, off: usize, val: u32) {
    core::ptr::write_volatile((base + off) as *mut u32, val);
}

/// A split virtqueue laid out in one contiguous DMA region (legacy layout):
///   [ desc table | avail ring | pad to align | used ring ]
struct VirtQueue {
    base: usize, // phys == virt (identity-mapped)
    size: u16,
    avail_off: usize,
    used_off: usize,
}

impl VirtQueue {
    /// Legacy contiguous region size for a queue of `n` descriptors.
    fn region_bytes(n: u16) -> usize {
        let n = n as usize;
        let desc = 16 * n;
        let avail = 6 + 2 * n; // flags(2)+idx(2)+ring(2*n)+used_event(2) approx
        let used_unaligned = desc + avail;
        let used_start = used_unaligned.div_ceil(QUEUE_ALIGN) * QUEUE_ALIGN;
        let used = 6 + 8 * n; // flags(2)+idx(2)+ring(8*n)+avail_event(2)
        used_start + used
    }

    fn new(base: usize, n: u16) -> Self {
        let desc = 16 * n as usize;
        let avail = 6 + 2 * n as usize;
        let used_off = (desc + avail).div_ceil(QUEUE_ALIGN) * QUEUE_ALIGN;
        Self {
            base,
            size: n,
            avail_off: desc,
            used_off,
        }
    }

    unsafe fn set_desc(&self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let d = (self.base + 16 * i as usize) as *mut Desc;
        core::ptr::write_volatile(
            d,
            Desc {
                addr,
                len,
                flags,
                next,
            },
        );
    }

    unsafe fn avail_idx(&self) -> u16 {
        core::ptr::read_volatile((self.base + self.avail_off + 2) as *const u16)
    }

    unsafe fn set_avail_ring(&self, slot: u16, desc: u16) {
        let ring = (self.base + self.avail_off + 4) as *mut u16;
        core::ptr::write_volatile(ring.add((slot % self.size) as usize), desc);
    }

    unsafe fn publish_avail(&self, new_idx: u16) {
        // Ensure descriptor + ring writes are visible before the index bump.
        fence(Ordering::SeqCst);
        core::ptr::write_volatile((self.base + self.avail_off + 2) as *mut u16, new_idx);
    }

    unsafe fn used_idx(&self) -> u16 {
        core::ptr::read_volatile((self.base + self.used_off + 2) as *const u16)
    }

    fn pfn(&self) -> u32 {
        (self.base as u64 / 4096) as u32
    }
}

/// A virtio-blk device on the MMIO transport.
pub struct VirtioBlkMmio {
    base: usize,
    capacity: u64, // in 512-byte sectors
    queue: VirtQueue,
    // Bounce/request scratch: header(16) + status(1), kept in a DMA region.
    req_region: usize,
    lock: SpinLock<()>,
}

impl VirtioBlkMmio {
    /// Probe the virtio-MMIO slot at `base` for a block device and initialise it.
    /// Returns `None` if the slot is empty or is not a block device.
    ///
    /// # Safety
    /// `base` must be a mapped virtio-MMIO register window (Device memory). The
    /// identity map must make phys == virt for the DMA regions allocated here.
    pub unsafe fn probe(base: usize, frames: &FrameAllocator) -> Option<Self> {
        if mmio_r(base, MMIO_MAGIC) != MMIO_MAGIC_VALUE {
            return None;
        }
        let version = mmio_r(base, MMIO_VERSION);
        if mmio_r(base, MMIO_DEVICE_ID) != DEVICE_ID_BLOCK {
            return None; // empty slot or a different device
        }
        // Only the legacy (version 1) MMIO transport is implemented here, which is
        // what QEMU `virt` presents by default. Modern (v2) uses a different queue
        // attach path; reject it explicitly rather than mis-driving it.
        if version != 1 {
            return None;
        }

        // --- device-init handshake ---
        mmio_w(base, MMIO_STATUS, 0); // reset
        let mut status = STATUS_ACK;
        mmio_w(base, MMIO_STATUS, status);
        status |= STATUS_DRIVER;
        mmio_w(base, MMIO_STATUS, status);

        // Feature negotiation: accept none of the optional features (the base
        // read/write protocol needs no feature bit), but perform the handshake.
        mmio_w(base, MMIO_DEVICE_FEATURES_SEL, 0);
        let _devf = mmio_r(base, MMIO_DEVICE_FEATURES);
        mmio_w(base, MMIO_DRIVER_FEATURES_SEL, 0);
        mmio_w(base, MMIO_DRIVER_FEATURES, 0);
        status |= STATUS_FEATURES_OK;
        mmio_w(base, MMIO_STATUS, status);
        if mmio_r(base, MMIO_STATUS) & STATUS_FEATURES_OK == 0 {
            mmio_w(base, MMIO_STATUS, STATUS_FAILED);
            return None;
        }

        // Legacy guest page size (the device needs it to interpret QUEUE_PFN).
        mmio_w(base, MMIO_GUEST_PAGE_SIZE, 4096);

        // --- queue 0 setup ---
        mmio_w(base, MMIO_QUEUE_SEL, 0);
        let qmax = mmio_r(base, MMIO_QUEUE_NUM_MAX);
        if qmax == 0 {
            return None;
        }
        let qsize = core::cmp::min(qmax, 128) as u16;
        mmio_w(base, MMIO_QUEUE_NUM, qsize as u32);
        mmio_w(base, MMIO_QUEUE_ALIGN, QUEUE_ALIGN as u32);

        let bytes = VirtQueue::region_bytes(qsize);
        let pages = (bytes as u64).div_ceil(cibos_kernel::FRAME_SIZE);
        let first = frames.allocate_contiguous(pages).ok()?;
        let qbase = first.addr() as usize; // identity-mapped
        core::ptr::write_bytes(qbase as *mut u8, 0, bytes);
        let queue = VirtQueue::new(qbase, qsize);
        mmio_w(base, MMIO_QUEUE_PFN, queue.pfn());

        // A small DMA region for the request header (16) + status (1).
        let req_pages = 1u64;
        let req_first = frames.allocate_contiguous(req_pages).ok()?;
        let req_region = req_first.addr() as usize;
        core::ptr::write_bytes(req_region as *mut u8, 0, 4096);

        // capacity (sectors) is the first u64 of the blk config space.
        let cap_lo = mmio_r(base, MMIO_CONFIG) as u64;
        let cap_hi = mmio_r(base, MMIO_CONFIG + 4) as u64;
        let capacity = (cap_hi << 32) | cap_lo;

        // DRIVER_OK — the device is live.
        status |= STATUS_DRIVER_OK;
        mmio_w(base, MMIO_STATUS, status);

        Some(Self {
            base,
            capacity,
            queue,
            req_region,
            lock: SpinLock::new(()),
        })
    }

    /// Submit one read or write request for `count` sectors at `lba` and poll the
    /// used ring until the device completes it. `data` is the identity-mapped DMA
    /// buffer (read: device-writable; write: device-readable).
    ///
    /// # Safety
    /// `data` must be `count * BLOCK_SIZE` bytes and identity-mapped.
    unsafe fn request(&self, write: bool, lba: u64, data: usize, len: u32) -> Result<(), BlockError> {
        let _g = self.lock.lock();

        // Build the request header.
        let hdr = self.req_region as *mut BlkReqHeader;
        core::ptr::write_volatile(
            hdr,
            BlkReqHeader {
                req_type: if write { VIRTIO_BLK_T_OUT } else { VIRTIO_BLK_T_IN },
                reserved: 0,
                sector: lba,
            },
        );
        let status_ptr = (self.req_region + 16) as *mut u8;
        core::ptr::write_volatile(status_ptr, 0xff); // sentinel != OK

        // 3-descriptor chain: header (R) -> data (R or W) -> status (W).
        // Descriptor 0: header, device-readable.
        self.queue.set_desc(0, self.req_region as u64, 16, VRING_DESC_F_NEXT, 1);
        // Descriptor 1: data. For a READ the device WRITES the buffer.
        let data_flags = VRING_DESC_F_NEXT | if write { 0 } else { VRING_DESC_F_WRITE };
        self.queue.set_desc(1, data as u64, len, data_flags, 2);
        // Descriptor 2: status byte, device-writable.
        self.queue
            .set_desc(2, (self.req_region + 16) as u64, 1, VRING_DESC_F_WRITE, 0);

        // Publish descriptor 0 (head of the chain) into the avail ring.
        let idx = self.queue.avail_idx();
        self.queue.set_avail_ring(idx, 0);
        self.queue.publish_avail(idx.wrapping_add(1));

        // On a non-coherent platform (RISC-V without DMA coherency), clean (write
        // back) the buffers the DEVICE will READ — the request header, the ring
        // region, and, for a WRITE, the data — so the device sees current data.
        // No-op when cache maintenance is not configured (coherent platforms).
        #[cfg(target_arch = "riscv64")]
        {
            crate::arch::cache_riscv64::clean_range(self.req_region, 17);
            crate::arch::cache_riscv64::clean_range(self.queue.base, 4096);
            if write {
                crate::arch::cache_riscv64::clean_range(data, len as usize);
            }
        }

        // Notify the device.
        mmio_w(self.base, MMIO_QUEUE_NOTIFY, 0);

        // Poll the used ring for completion (bounded spin).
        let target = idx.wrapping_add(1);
        let mut spins: u64 = 0;
        while self.queue.used_idx() != target {
            spins += 1;
            if spins > 100_000_000 {
                return Err(BlockError::Timeout);
            }
            core::hint::spin_loop();
        }
        fence(Ordering::SeqCst);

        // The device has written the used ring and the status byte, and for a READ
        // the data buffer. Invalidate those before the CPU reads them, so we do not
        // see stale cached data. No-op on coherent platforms.
        #[cfg(target_arch = "riscv64")]
        {
            crate::arch::cache_riscv64::invalidate_range(self.queue.base, 4096);
            crate::arch::cache_riscv64::invalidate_range(self.req_region, 17);
            if !write {
                crate::arch::cache_riscv64::invalidate_range(data, len as usize);
            }
        }

        match core::ptr::read_volatile(status_ptr) {
            VIRTIO_BLK_S_OK => Ok(()),
            _ => Err(BlockError::DeviceError),
        }
    }
}

impl BlockDevice for VirtioBlkMmio {
    fn block_count(&self) -> u64 {
        self.capacity
    }

    fn read_blocks(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), BlockError> {
        check_range(self.capacity, lba, count, buf.len())?;
        // The buffer must be identity-mapped for DMA; kernel heap/stack are. We
        // pass its address straight through (phys == virt).
        let len = count * BLOCK_SIZE as u32;
        // SAFETY: buf is exactly len bytes (checked) and identity-mapped.
        unsafe { self.request(false, lba, buf.as_mut_ptr() as usize, len) }
    }

    fn write_blocks(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), BlockError> {
        check_range(self.capacity, lba, count, buf.len())?;
        let len = count * BLOCK_SIZE as u32;
        // SAFETY: buf is exactly len bytes (checked) and identity-mapped.
        unsafe { self.request(true, lba, buf.as_ptr() as usize, len) }
    }
}
