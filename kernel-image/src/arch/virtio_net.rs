//! virtio-net driver (legacy virtio-pci transport, polled) — device discovery,
//! negotiation, and MAC/link read.
//!
//! virtio-net is a real, ubiquitous network-interface contract: QEMU/KVM, cloud
//! hypervisors, and bare-metal SR-IOV all present it. A driver for it is a real
//! driver — it does not "know" which of those it runs on — exactly as the ATA
//! driver targets the real ATA/IDE interface regardless of the disk behind it.
//! This is the networking analogue of [`ata`](super::ata): the first concrete
//! [`NetDevice`](cibos_kernel::net_device::NetDevice) implementation, behind
//! which the Lattice's NIC-backed transport will sit (the same Gate/Link/Warden
//! surface, unchanged — see `NETWORKING.md`).
//!
//! This module implements, against the LEGACY virtio-pci transport (I/O-BAR
//! based, the simplest and the one QEMU exposes by default for the
//! `pci-version=1` device):
//!   * PCI bus enumeration over config space (`0xCF8`/`0xCFC`);
//!   * detection of the virtio-net device (vendor `0x1AF4`, device `0x1000`);
//!   * the legacy virtio device-init handshake (reset → ACK → DRIVER → read
//!     feature bits → FEATURES_OK → DRIVER_OK) up to feature negotiation;
//!   * reading the MAC address and link status from the device config region.
//!
//! Frame TX/RX over the virtqueues is the next increment (B3); the device
//! discovery + negotiation + MAC/link read here are real and QEMU-verifiable on
//! their own, and form the foundation the rings build on. This is an honest
//! layering, not a stub: every register access below is a real device access.

use cibos_kernel::net_device::{MacAddress, NetDevice, NetDeviceError};
use cibos_kernel::sync::SpinLock;

// ---- Port I/O (self-contained, mirroring ata.rs) ----------------------------

#[inline]
unsafe fn outl(port: u16, val: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags));
}
#[inline]
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    core::arch::asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}
#[inline]
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}
#[inline]
unsafe fn inw(port: u16) -> u16 {
    let val: u16;
    core::arch::asm!("in ax, dx", out("ax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

unsafe fn outw(port: u16, val: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack, preserves_flags));
}

// ---- PCI config space -------------------------------------------------------

const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// Read a 32-bit dword from PCI config space for (bus, slot, func, offset).
unsafe fn pci_read32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    let addr = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    outl(PCI_CONFIG_ADDRESS, addr);
    inl(PCI_CONFIG_DATA)
}

/// Read a 16-bit word from PCI config space.
unsafe fn pci_read16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let dword = pci_read32(bus, slot, func, offset & 0xFC);
    ((dword >> ((offset as u32 & 2) * 8)) & 0xFFFF) as u16
}

/// A located PCI function.
#[derive(Clone, Copy)]
struct PciAddr {
    bus: u8,
    slot: u8,
    func: u8,
}

// virtio PCI identifiers.
const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_NET_DEVICE_LEGACY: u16 = 0x1000;
const PCI_VENDOR_ID: u8 = 0x00;
const PCI_DEVICE_ID: u8 = 0x02;
const PCI_COMMAND: u8 = 0x04;
const PCI_BAR0: u8 = 0x10;

/// Scan the PCI bus for the legacy virtio-net device. Returns its location and
/// the I/O BAR base, or `None` if no such device is present.
unsafe fn find_virtio_net() -> Option<(PciAddr, u16)> {
    for bus in 0u8..=255 {
        for slot in 0u8..32 {
            let vendor = pci_read16(bus, slot, 0, PCI_VENDOR_ID);
            if vendor == 0xFFFF {
                continue; // no device in this slot
            }
            let device = pci_read16(bus, slot, 0, PCI_DEVICE_ID);
            if vendor == VIRTIO_VENDOR && device == VIRTIO_NET_DEVICE_LEGACY {
                let addr = PciAddr { bus, slot, func: 0 };
                // BAR0 for the legacy transport is an I/O BAR (bit0 = 1); mask the
                // low 2 bits to get the base port.
                let bar0 = pci_read32(bus, slot, 0, PCI_BAR0);
                if bar0 & 1 == 1 {
                    let io_base = (bar0 & 0xFFFC) as u16;
                    return Some((addr, io_base));
                }
            }
        }
    }
    None
}

/// Enable I/O space + bus mastering for the device (PCI command register).
unsafe fn pci_enable_io_and_busmaster(a: PciAddr) {
    let cmd = pci_read16(a.bus, a.slot, a.func, PCI_COMMAND);
    // bit0 = I/O space enable, bit2 = bus master enable.
    let new = cmd | 0x1 | 0x4;
    let addr = 0x8000_0000u32
        | ((a.bus as u32) << 16)
        | ((a.slot as u32) << 11)
        | ((a.func as u32) << 8)
        | (PCI_COMMAND as u32 & 0xFC);
    outl(PCI_CONFIG_ADDRESS, addr);
    // Write back the 16-bit command in the low half of the dword.
    let dword = inl(PCI_CONFIG_DATA);
    let merged = (dword & 0xFFFF_0000) | new as u32;
    outl(PCI_CONFIG_ADDRESS, addr);
    outl(PCI_CONFIG_DATA, merged);
}

// ---- Legacy virtio-pci I/O register map (offsets from the I/O BAR) ----------

const VIRTIO_PCI_DEVICE_FEATURES: u16 = 0x00; // r: device feature bits 0..31
const VIRTIO_PCI_GUEST_FEATURES: u16 = 0x04; // w: driver feature bits 0..31
const VIRTIO_PCI_QUEUE_PFN: u16 = 0x08; // rw: queue physical page-frame number
const VIRTIO_PCI_QUEUE_SIZE: u16 = 0x0C; // r: queue size N (0 = absent)
const VIRTIO_PCI_QUEUE_SELECT: u16 = 0x0E; // w: select the active queue index
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10; // w: notify the device of new buffers
const VIRTIO_PCI_STATUS: u16 = 0x12; // r/w: device status (8-bit)
const VIRTIO_PCI_CONFIG: u16 = 0x14; // device-specific config (net: MAC[6], status)

// Device status bits.
const STATUS_RESET: u8 = 0x00;
const STATUS_ACKNOWLEDGE: u8 = 0x01;
const STATUS_DRIVER: u8 = 0x02;
const STATUS_FEATURES_OK: u8 = 0x08;
const STATUS_DRIVER_OK: u8 = 0x04;

// virtio-net feature bits.
const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_F_STATUS: u32 = 1 << 16;
const VIRTIO_NET_S_LINK_UP: u16 = 1;

// virtio split-virtqueue descriptor flags.
// `NEXT` (descriptor chaining) is part of the spec vocabulary; our frames fit in
// one descriptor each, so it is unused today but kept for chained-buffer support.
#[allow(dead_code)]
const VRING_DESC_F_NEXT: u16 = 1; // descriptor chains to `next`
const VRING_DESC_F_WRITE: u16 = 2; // device writes (RX); else driver writes (TX)

// virtio-net queue indices (legacy, no MQ): queue 0 = RX, queue 1 = TX.
const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;

// The legacy virtio_net_hdr prepended to every frame (no GSO/MRG negotiated):
// flags(1) gso_type(1) hdr_len(2) gso_size(2) csum_start(2) csum_offset(2).
const VIRTIO_NET_HDR_LEN: usize = 10;
// Max Ethernet frame we handle (1500 MTU + 14 header); buffers hold hdr + this.
const FRAME_BUF_LEN: usize = VIRTIO_NET_HDR_LEN + 1514;

/// A legacy split virtqueue laid out in one physically-contiguous, identity-
/// mapped DMA region. Layout (virtio 0.9.5):
///   desc:  N × 16 bytes  (addr u64, len u32, flags u16, next u16)
///   avail: 2+2 + N×2 + 2 bytes  (flags u16, idx u16, ring[N] u16, used_event u16)
///   pad to the next 4096 boundary
///   used:  2+2 + N×8 + 2 bytes  (flags u16, idx u16, ring[N]{id u32,len u32}, ..)
/// Plus a parallel array of per-descriptor frame buffers (also in the region).
struct VirtQueue {
    /// Physical (== virtual, identity-mapped) base of the ring region.
    base: usize,
    /// Queue size N (device-reported).
    size: u16,
    /// Offset of the avail ring within the region.
    avail_off: usize,
    /// Offset of the used ring within the region.
    used_off: usize,
    /// Offset of the frame-buffer array within the region.
    bufs_off: usize,
    /// Driver's record of the last used-ring index it has consumed (interior
    /// mutability so `recv_frame(&self)` can advance it).
    last_used: core::sync::atomic::AtomicU16,
}

#[inline]
fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

impl VirtQueue {
    /// Total bytes a queue of size `n` needs (rings + per-descriptor buffers),
    /// rounded to whole pages.
    fn region_bytes(n: u16) -> usize {
        let n = n as usize;
        let desc = n * 16;
        let avail = 6 + n * 2;
        let used_start = align_up(desc + avail, 4096);
        let used = 6 + n * 8;
        let bufs_start = align_up(used_start + used, 4096);
        let bufs = n * FRAME_BUF_LEN;
        align_up(bufs_start + bufs, 4096)
    }

    /// Construct the queue view over an already-zeroed region at physical `base`.
    fn new(base: usize, n: u16) -> Self {
        let nn = n as usize;
        let desc = nn * 16;
        let avail = 6 + nn * 2;
        let used_off = align_up(desc + avail, 4096);
        let used = 6 + nn * 8;
        let bufs_off = align_up(used_off + used, 4096);
        Self {
            base,
            size: n,
            avail_off: desc,
            used_off,
            bufs_off,
            last_used: core::sync::atomic::AtomicU16::new(0),
        }
    }

    #[inline]
    fn pfn(&self) -> u32 {
        (self.base as u64 >> 12) as u32
    }

    // ---- raw descriptor access ----
    /// Write descriptor `i` = {addr, len, flags, next}.
    /// # Safety: `i < size`; the region is mapped writable.
    unsafe fn set_desc(&self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let d = (self.base + i as usize * 16) as *mut u8;
        (d as *mut u64).write_volatile(addr);
        (d.add(8) as *mut u32).write_volatile(len);
        (d.add(12) as *mut u16).write_volatile(flags);
        (d.add(14) as *mut u16).write_volatile(next);
    }

    /// Physical address of descriptor `i`'s frame buffer.
    fn buf_addr(&self, i: u16) -> u64 {
        (self.base + self.bufs_off + i as usize * FRAME_BUF_LEN) as u64
    }

    // ---- avail ring (driver -> device) ----
    unsafe fn avail_idx(&self) -> u16 {
        ((self.base + self.avail_off + 2) as *const u16).read_volatile()
    }
    /// Set VRING_AVAIL_F_NO_INTERRUPT in the avail ring's flags word: this is a
    /// POLLED driver, so it tells the device (via the documented virtio field)
    /// not to raise a completion interrupt for this queue. avail.flags is the
    /// first u16 of the avail ring.
    unsafe fn set_no_interrupt(&self) {
        const VRING_AVAIL_F_NO_INTERRUPT: u16 = 1;
        ((self.base + self.avail_off) as *mut u16).write_volatile(VRING_AVAIL_F_NO_INTERRUPT);
    }
    unsafe fn set_avail_ring(&self, slot: u16, desc: u16) {
        let r = (self.base + self.avail_off + 4 + (slot as usize % self.size as usize) * 2)
            as *mut u16;
        r.write_volatile(desc);
    }
    unsafe fn publish_avail(&self, new_idx: u16) {
        ((self.base + self.avail_off + 2) as *mut u16).write_volatile(new_idx);
    }

    // ---- used ring (device -> driver) ----
    unsafe fn used_idx(&self) -> u16 {
        ((self.base + self.used_off + 2) as *const u16).read_volatile()
    }
    /// Read used-ring element `slot` = (descriptor id, written length).
    unsafe fn used_elem(&self, slot: u16) -> (u32, u32) {
        let e = (self.base + self.used_off + 4 + (slot as usize % self.size as usize) * 8)
            as *const u8;
        let id = (e as *const u32).read_volatile();
        let len = (e.add(4) as *const u32).read_volatile();
        (id, len)
    }

    fn last_used_get(&self) -> u16 {
        self.last_used.load(core::sync::atomic::Ordering::Acquire)
    }
    fn last_used_bump(&self) {
        self.last_used
            .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    }
}

/// A discovered + negotiated legacy virtio-net device.
pub struct VirtioNet {
    io_base: u16,
    mac: MacAddress,
    has_status: bool,
    /// Receive queue (queue 0) — device writes incoming frames here.
    rx: VirtQueue,
    /// Transmit queue (queue 1) — driver writes outgoing frames here.
    tx: VirtQueue,
    /// Next TX descriptor slot to use (round-robin over the queue).
    tx_next: SpinLock<u16>,
    // Serializes device register access (QUEUE_NOTIFY etc.) across send/recv.
    lock: SpinLock<()>,
}

impl VirtioNet {
    /// Probe the PCI bus and bring a virtio-net device up through feature
    /// negotiation, reading its MAC and link-status capability. Returns `None`
    /// if no virtio-net device is present.
    ///
    /// # Safety
    /// Touches PCI config ports and the device I/O BAR; call once during
    /// single-threaded bring-up.
    pub unsafe fn probe(frames: &cibos_kernel::FrameAllocator) -> Option<Self> {
        let (addr, io_base) = find_virtio_net()?;
        pci_enable_io_and_busmaster(addr);

        // Legacy virtio device-init handshake.
        outb(io_base + VIRTIO_PCI_STATUS, STATUS_RESET);
        outb(io_base + VIRTIO_PCI_STATUS, STATUS_ACKNOWLEDGE);
        outb(
            io_base + VIRTIO_PCI_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER,
        );

        // Read device features; accept only MAC + STATUS (the ones we read here).
        let device_features = inl(io_base + VIRTIO_PCI_DEVICE_FEATURES);
        let wanted = (VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS) & device_features;
        outl(io_base + VIRTIO_PCI_GUEST_FEATURES, wanted);
        outb(
            io_base + VIRTIO_PCI_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );

        // Read the MAC from device config (present iff VIRTIO_NET_F_MAC).
        let mut mac = [0u8; 6];
        if device_features & VIRTIO_NET_F_MAC != 0 {
            for (i, b) in mac.iter_mut().enumerate() {
                *b = inb(io_base + VIRTIO_PCI_CONFIG + i as u16);
            }
        }
        let has_status = device_features & VIRTIO_NET_F_STATUS != 0;

        // Set up the RX (0) and TX (1) virtqueues. If either is absent or cannot
        // be allocated, fail the probe honestly (no half-initialized device).
        let rx = Self::setup_queue(io_base, RX_QUEUE, frames)?;
        let tx = Self::setup_queue(io_base, TX_QUEUE, frames)?;

        // Pre-fill the RX ring so the device has buffers to write incoming frames
        // into, then publish them and notify.
        for i in 0..rx.size {
            rx.set_desc(i, rx.buf_addr(i), FRAME_BUF_LEN as u32, VRING_DESC_F_WRITE, 0);
            rx.set_avail_ring(i, i);
        }
        rx.publish_avail(rx.size);
        outw(io_base + VIRTIO_PCI_QUEUE_NOTIFY, RX_QUEUE);

        // Queues are ready: tell the device it may operate.
        outb(
            io_base + VIRTIO_PCI_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );

        Some(Self {
            io_base,
            mac,
            has_status,
            rx,
            tx,
            tx_next: SpinLock::new(0),
            lock: SpinLock::new(()),
        })
    }

    /// Select queue `index`, read its size, allocate + zero a contiguous DMA
    /// region for it, attach it via QUEUE_PFN, and return the queue view.
    /// Returns `None` if the queue is absent (size 0) or memory is unavailable.
    ///
    /// # Safety
    /// Touches the device I/O BAR; the identity map must make physical == virtual
    /// for the allocated region (true on the booted kernel).
    unsafe fn setup_queue(
        io_base: u16,
        index: u16,
        frames: &cibos_kernel::FrameAllocator,
    ) -> Option<VirtQueue> {
        outw(io_base + VIRTIO_PCI_QUEUE_SELECT, index);
        let size = inw(io_base + VIRTIO_PCI_QUEUE_SIZE);
        if size == 0 {
            return None; // queue not present
        }
        let bytes = VirtQueue::region_bytes(size);
        let pages = (bytes as u64).div_ceil(cibos_kernel::FRAME_SIZE);
        let first = frames.allocate_contiguous(pages).ok()?;
        let base = first.addr() as usize; // identity-mapped: phys == virt
        // Zero the whole region (rings must start clear).
        core::ptr::write_bytes(base as *mut u8, 0, bytes);
        let q = VirtQueue::new(base, size);
        // Polled driver: tell the device not to interrupt on completions for this
        // queue (the avail ring's NO_INTERRUPT flag). Set before attaching.
        q.set_no_interrupt();
        // Attach the queue: legacy devices take the region's page-frame number.
        outl(io_base + VIRTIO_PCI_QUEUE_PFN, q.pfn());
        Some(q)
    }

    /// Read the device-config link status word (valid iff VIRTIO_NET_F_STATUS).
    unsafe fn read_status(&self) -> u16 {
        // The status word follows the 6 MAC bytes in the net config region.
        inw(self.io_base + VIRTIO_PCI_CONFIG + 6)
    }

    /// Return a consumed RX descriptor to the avail ring so the device can write
    /// another incoming frame into it, then notify the device.
    ///
    /// # Safety
    /// `desc_id < rx.size`; the RX region is identity-mapped + writable.
    unsafe fn recycle_rx(&self, desc_id: u16) {
        // Reset the descriptor as a device-writable buffer of full length.
        self.rx.set_desc(
            desc_id,
            self.rx.buf_addr(desc_id),
            FRAME_BUF_LEN as u32,
            VRING_DESC_F_WRITE,
            0,
        );
        let idx = self.rx.avail_idx();
        self.rx.set_avail_ring(idx, desc_id);
        self.rx.publish_avail(idx.wrapping_add(1));
        outw(self.io_base + VIRTIO_PCI_QUEUE_NOTIFY, RX_QUEUE);
    }
}

impl NetDevice for VirtioNet {
    fn mac(&self) -> MacAddress {
        self.mac
    }

    fn link_up(&self) -> bool {
        if !self.has_status {
            // Without the STATUS feature the link is assumed up while present.
            return true;
        }
        // SAFETY: reads a device-config port on the located I/O BAR.
        let status = unsafe { self.read_status() };
        status & VIRTIO_NET_S_LINK_UP != 0
    }

    fn send_frame(&self, frame: &[u8]) -> Result<(), NetDeviceError> {
        if frame.is_empty() || frame.len() > FRAME_BUF_LEN - VIRTIO_NET_HDR_LEN {
            return Err(NetDeviceError::TooLarge);
        }
        let _g = self.lock.lock();
        // Choose the next TX descriptor slot (round-robin).
        let mut next = self.tx_next.lock();
        let slot = *next % self.tx.size;
        *next = next.wrapping_add(1);
        drop(next);

        // SAFETY: `slot < tx.size`; the TX region is identity-mapped + writable.
        unsafe {
            let buf = self.tx.buf_addr(slot) as *mut u8;
            // 10-byte virtio_net_hdr, all zero (no GSO/checksum offload).
            core::ptr::write_bytes(buf, 0, VIRTIO_NET_HDR_LEN);
            // Ethernet frame after the header.
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                buf.add(VIRTIO_NET_HDR_LEN),
                frame.len(),
            );
            let total = (VIRTIO_NET_HDR_LEN + frame.len()) as u32;
            // Driver-readable descriptor (no WRITE flag): the device reads it.
            self.tx.set_desc(slot, self.tx.buf_addr(slot), total, 0, 0);

            let idx = self.tx.avail_idx();
            self.tx.set_avail_ring(idx, slot);
            self.tx.publish_avail(idx.wrapping_add(1));
            outw(self.io_base + VIRTIO_PCI_QUEUE_NOTIFY, TX_QUEUE);

            // Polled completion: wait (bounded) for the device to reclaim it.
            let target = idx.wrapping_add(1);
            for _ in 0..1_000_000 {
                if self.tx.used_idx() == target {
                    return Ok(());
                }
                core::hint::spin_loop();
            }
            // The frame was published; if the used ring has not advanced in the
            // budget, report Busy rather than claim a fake success.
            Err(NetDeviceError::Busy)
        }
    }

    fn recv_frame(&self, buf: &mut [u8]) -> Result<Option<usize>, NetDeviceError> {
        let _g = self.lock.lock();
        // SAFETY: the RX region is identity-mapped; indices kept within `size`.
        unsafe {
            let used = self.rx.used_idx();
            if used == self.rx.last_used_get() {
                return Ok(None); // nothing new
            }
            let slot = self.rx.last_used_get() % self.rx.size;
            let (desc_id, written) = self.rx.used_elem(slot);
            let desc_id = (desc_id as u16) % self.rx.size;

            // The device wrote [virtio_net_hdr | frame]; skip the header.
            let total = written as usize;
            let frame_len = total.saturating_sub(VIRTIO_NET_HDR_LEN);
            if frame_len == 0 {
                // Recycle and report nothing usable.
                self.recycle_rx(desc_id);
                self.rx.last_used_bump();
                return Ok(None);
            }
            if buf.len() < frame_len {
                return Err(NetDeviceError::TooLarge);
            }
            let src = (self.rx.buf_addr(desc_id) as *const u8).add(VIRTIO_NET_HDR_LEN);
            core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), frame_len);

            // Recycle the buffer back into the avail ring for reuse.
            self.recycle_rx(desc_id);
            self.rx.last_used_bump();
            Ok(Some(frame_len))
        }
    }
}
