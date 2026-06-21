//! Intel 82540EM ("e1000") gigabit Ethernet driver.
//!
//! A real driver for a real, ubiquitous NIC: the e1000 is present on countless
//! physical machines and is the second [`NetDevice`] backend (after virtio-net),
//! so a non-virtio bare-metal box still has networking. It targets the e1000
//! register interface per Intel's 8254x developer manual — not any emulator;
//! QEMU's `-device e1000` merely VERIFIES it, exactly like the ATA and
//! virtio-net drivers.
//!
//! Transport model (mirrors virtio-net): a polled driver over MMIO-mapped
//! descriptor rings. RX and TX each use a ring of legacy descriptors plus a
//! parallel array of frame buffers, all in physically-contiguous,
//! identity-mapped DMA memory from the [`FrameAllocator`]. The driver does not
//! enable interrupts (IMS left clear); the kernel's spurious-IRQ handling covers
//! any stray line.

#![cfg(target_arch = "x86_64")]

use cibos_kernel::net_device::{MacAddress, NetDevice, NetDeviceError};
use cibos_kernel::sync::SpinLock;
use cibos_kernel::FrameAllocator;

// ---- Port I/O (PCI config space only; the NIC itself is MMIO) ---------------

unsafe fn outl(port: u16, val: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags));
}
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    core::arch::asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

// ---- PCI config space -------------------------------------------------------

const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;
const PCI_VENDOR_ID: u8 = 0x00;
const PCI_DEVICE_ID: u8 = 0x02;
const PCI_COMMAND: u8 = 0x04;
const PCI_BAR0: u8 = 0x10;

const INTEL_VENDOR: u16 = 0x8086;
const E1000_DEVICE_82540EM: u16 = 0x100E;

#[derive(Clone, Copy)]
struct PciAddr {
    bus: u8,
    slot: u8,
    func: u8,
}

unsafe fn pci_cfg_addr(a: PciAddr, offset: u8) -> u32 {
    0x8000_0000u32
        | ((a.bus as u32) << 16)
        | ((a.slot as u32) << 11)
        | ((a.func as u32) << 8)
        | (offset as u32 & 0xFC)
}

unsafe fn pci_read32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    outl(PCI_CONFIG_ADDRESS, pci_cfg_addr(PciAddr { bus, slot, func }, offset));
    inl(PCI_CONFIG_DATA)
}

unsafe fn pci_read16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let dword = pci_read32(bus, slot, func, offset & 0xFC);
    let shift = ((offset & 2) * 8) as u32;
    ((dword >> shift) & 0xFFFF) as u16
}

/// Enable MMIO space + bus mastering (the e1000 is a DMA-capable MMIO device).
unsafe fn pci_enable_mmio_and_busmaster(a: PciAddr) {
    let addr = pci_cfg_addr(a, PCI_COMMAND);
    outl(PCI_CONFIG_ADDRESS, addr);
    let dword = inl(PCI_CONFIG_DATA);
    // bit1 = memory space enable, bit2 = bus master enable.
    let cmd = (dword & 0xFFFF) as u16 | 0x2 | 0x4;
    let merged = (dword & 0xFFFF_0000) | cmd as u32;
    outl(PCI_CONFIG_ADDRESS, addr);
    outl(PCI_CONFIG_DATA, merged);
}

/// Find an e1000 (82540EM) on the PCI bus, returning its address and the MMIO
/// base physical address from BAR0 (a memory BAR; low bits masked off).
unsafe fn find_e1000() -> Option<(PciAddr, u64)> {
    for bus in 0u8..=255 {
        for slot in 0u8..32 {
            let vendor = pci_read16(bus, slot, 0, PCI_VENDOR_ID);
            if vendor == 0xFFFF {
                continue;
            }
            let device = pci_read16(bus, slot, 0, PCI_DEVICE_ID);
            if vendor == INTEL_VENDOR && device == E1000_DEVICE_82540EM {
                let addr = PciAddr { bus, slot, func: 0 };
                let bar0 = pci_read32(bus, slot, 0, PCI_BAR0);
                // BAR0 is a memory BAR (bit0 = 0); mask the low 4 bits for the
                // base. The e1000 register space is 128 KiB at this address.
                if bar0 & 1 == 0 {
                    let mmio = (bar0 & 0xFFFF_FFF0) as u64;
                    return Some((addr, mmio));
                }
            }
        }
    }
    None
}

// ---- e1000 register offsets (bytes from the MMIO base) ----------------------

const REG_CTRL: usize = 0x0000; // Device Control
const REG_STATUS: usize = 0x0008; // Device Status
const REG_EERD: usize = 0x0014; // EEPROM Read
const REG_ICR: usize = 0x00C0; // Interrupt Cause Read
const REG_IMC: usize = 0x00D8; // Interrupt Mask Clear
const REG_RCTL: usize = 0x0100; // Receive Control
const REG_TCTL: usize = 0x0400; // Transmit Control
const REG_RDBAL: usize = 0x2800; // RX Descriptor Base Low
const REG_RDBAH: usize = 0x2804; // RX Descriptor Base High
const REG_RDLEN: usize = 0x2808; // RX Descriptor Length
const REG_RDH: usize = 0x2810; // RX Descriptor Head
const REG_RDT: usize = 0x2818; // RX Descriptor Tail
const REG_TDBAL: usize = 0x3800; // TX Descriptor Base Low
const REG_TDBAH: usize = 0x3804; // TX Descriptor Base High
const REG_TDLEN: usize = 0x3808; // TX Descriptor Length
const REG_TDH: usize = 0x3810; // TX Descriptor Head
const REG_TDT: usize = 0x3818; // TX Descriptor Tail

// CTRL bits.
const CTRL_SLU: u32 = 1 << 6; // Set Link Up

// STATUS bits.
const STATUS_LU: u32 = 1 << 1; // Link Up

// RCTL bits.
const RCTL_EN: u32 = 1 << 1; // Receiver Enable
const RCTL_BAM: u32 = 1 << 15; // Broadcast Accept Mode
const RCTL_SECRC: u32 = 1 << 26; // Strip Ethernet CRC
// Buffer size: bits 16-17 = 00 with no BSEX => 2048 bytes.

// TCTL bits.
const TCTL_EN: u32 = 1 << 1; // Transmit Enable
const TCTL_PSP: u32 = 1 << 3; // Pad Short Packets

// Legacy TX descriptor CMD bits.
const TXD_CMD_EOP: u8 = 1 << 0; // End Of Packet
const TXD_CMD_IFCS: u8 = 1 << 1; // Insert FCS
const TXD_CMD_RS: u8 = 1 << 3; // Report Status
// TX descriptor STATUS bits.
const TXD_STAT_DD: u8 = 1 << 0; // Descriptor Done

// RX descriptor STATUS bits.
const RXD_STAT_DD: u8 = 1 << 0; // Descriptor Done
const RXD_STAT_EOP: u8 = 1 << 1; // End Of Packet

const RING_LEN: usize = 32; // descriptors per ring (power of two)
const RX_BUF_LEN: usize = 2048; // matches RCTL buffer-size 2048
const TX_BUF_LEN: usize = 2048;

/// One legacy RX descriptor (16 bytes): addr(u64), len(u16), csum(u16),
/// status(u8), errors(u8), special(u16).
#[repr(C)]
#[derive(Clone, Copy)]
struct RxDesc {
    addr: u64,
    len: u16,
    csum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

/// One legacy TX descriptor (16 bytes): addr(u64), len(u16), cso(u8), cmd(u8),
/// status(u8), css(u8), special(u16).
#[repr(C)]
#[derive(Clone, Copy)]
struct TxDesc {
    addr: u64,
    len: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

/// A discovered + initialized e1000 device.
pub struct E1000 {
    mmio: usize, // identity-mapped MMIO base (phys == virt)
    mac: MacAddress,
    rx_ring: usize, // phys/virt base of the RX descriptor ring
    tx_ring: usize, // phys/virt base of the TX descriptor ring
    rx_bufs: usize, // base of the RX buffer array
    tx_bufs: usize, // base of the TX buffer array
    rx_cur: SpinLock<usize>,
    tx_cur: SpinLock<usize>,
    lock: SpinLock<()>,
}

impl E1000 {
    #[inline]
    unsafe fn read_reg(&self, off: usize) -> u32 {
        ((self.mmio + off) as *const u32).read_volatile()
    }
    #[inline]
    unsafe fn write_reg(&self, off: usize, val: u32) {
        ((self.mmio + off) as *mut u32).write_volatile(val);
    }

    /// Read a 16-bit EEPROM word via the EERD register (bit0 = start; bit4 =
    /// done; data in bits 16-31; address in bits 8-15).
    /// Read a 16-bit EEPROM word via the EERD register (bit0 = start; bit4 =
    /// done; data in bits 16-31; address in bits 8-15). Returns `None` if the
    /// EEPROM does not signal "done" within a bounded budget — a real e1000
    /// always responds, so a timeout means this is not a functioning e1000 (or
    /// has no EEPROM), and the caller must NOT spin forever on real hardware.
    unsafe fn eeprom_read(&self, word_addr: u8) -> Option<u16> {
        self.write_reg(REG_EERD, 1 | ((word_addr as u32) << 8));
        for _ in 0..200_000u32 {
            let v = self.read_reg(REG_EERD);
            if v & (1 << 4) != 0 {
                return Some((v >> 16) as u16);
            }
            core::hint::spin_loop();
        }
        None
    }

    /// Probe the PCI bus for an e1000, map its registers (identity-mapped),
    /// read the MAC from EEPROM, set up the RX/TX rings, and enable the device.
    ///
    /// # Safety
    /// Touches PCI config ports + the device MMIO BAR; allocates DMA memory.
    /// Call once during single-threaded bring-up, after the MMU is online.
    pub unsafe fn probe(frames: &FrameAllocator) -> Option<Self> {
        let (addr, mmio_phys) = find_e1000()?;
        pci_enable_mmio_and_busmaster(addr);
        let mmio = mmio_phys as usize; // identity map: phys == virt

        // Allocate DMA regions: RX ring, TX ring, RX bufs, TX bufs.
        let ring_bytes = RING_LEN * 16;
        let rx_ring_pages = (ring_bytes as u64).div_ceil(cibos_kernel::FRAME_SIZE);
        let rx_ring = frames.allocate_contiguous(rx_ring_pages).ok()?.addr() as usize;
        let tx_ring = frames.allocate_contiguous(rx_ring_pages).ok()?.addr() as usize;
        let rx_buf_pages =
            ((RING_LEN * RX_BUF_LEN) as u64).div_ceil(cibos_kernel::FRAME_SIZE);
        let rx_bufs = frames.allocate_contiguous(rx_buf_pages).ok()?.addr() as usize;
        let tx_buf_pages =
            ((RING_LEN * TX_BUF_LEN) as u64).div_ceil(cibos_kernel::FRAME_SIZE);
        let tx_bufs = frames.allocate_contiguous(tx_buf_pages).ok()?.addr() as usize;

        let dev = Self {
            mmio,
            mac: [0u8; 6],
            rx_ring,
            tx_ring,
            rx_bufs,
            tx_bufs,
            rx_cur: SpinLock::new(0),
            tx_cur: SpinLock::new(0),
            lock: SpinLock::new(()),
        };

        // Disable interrupts on the device (we poll); clear pending causes.
        dev.write_reg(REG_IMC, 0xFFFF_FFFF);
        let _ = dev.read_reg(REG_ICR);

        // Read MAC from EEPROM (words 0,1,2 -> 6 bytes, little-endian per word).
        // A real e1000 responds; if the EEPROM read times out, this is not a
        // functioning e1000 — bail out (the caller falls through to "no NIC")
        // rather than spin forever or trust a bogus device match.
        let mut mac = [0u8; 6];
        for i in 0..3 {
            let w = dev.eeprom_read(i as u8)?;
            mac[i * 2] = (w & 0xFF) as u8;
            mac[i * 2 + 1] = (w >> 8) as u8;
        }

        // ---- RX ring setup ----
        core::ptr::write_bytes(rx_ring as *mut u8, 0, ring_bytes);
        for i in 0..RING_LEN {
            let d = (rx_ring + i * 16) as *mut RxDesc;
            (*d).addr = (rx_bufs + i * RX_BUF_LEN) as u64;
            (*d).status = 0;
        }
        dev.write_reg(REG_RDBAL, (rx_ring as u64 & 0xFFFF_FFFF) as u32);
        dev.write_reg(REG_RDBAH, (rx_ring as u64 >> 32) as u32);
        dev.write_reg(REG_RDLEN, ring_bytes as u32);
        dev.write_reg(REG_RDH, 0);
        dev.write_reg(REG_RDT, (RING_LEN - 1) as u32);
        dev.write_reg(
            REG_RCTL,
            RCTL_EN | RCTL_BAM | RCTL_SECRC, // 2048-byte buffers (size bits 0)
        );

        // ---- TX ring setup ----
        core::ptr::write_bytes(tx_ring as *mut u8, 0, ring_bytes);
        for i in 0..RING_LEN {
            let d = (tx_ring + i * 16) as *mut TxDesc;
            (*d).addr = (tx_bufs + i * TX_BUF_LEN) as u64;
            (*d).status = TXD_STAT_DD; // mark free
        }
        dev.write_reg(REG_TDBAL, (tx_ring as u64 & 0xFFFF_FFFF) as u32);
        dev.write_reg(REG_TDBAH, (tx_ring as u64 >> 32) as u32);
        dev.write_reg(REG_TDLEN, ring_bytes as u32);
        dev.write_reg(REG_TDH, 0);
        dev.write_reg(REG_TDT, 0);
        dev.write_reg(REG_TCTL, TCTL_EN | TCTL_PSP);

        // Bring the link up.
        let ctrl = dev.read_reg(REG_CTRL);
        dev.write_reg(REG_CTRL, ctrl | CTRL_SLU);

        Some(Self { mac, ..dev })
    }
}

impl NetDevice for E1000 {
    fn mac(&self) -> MacAddress {
        self.mac
    }

    fn link_up(&self) -> bool {
        // SAFETY: reads the device STATUS register on the mapped MMIO BAR.
        let status = unsafe { self.read_reg(REG_STATUS) };
        status & STATUS_LU != 0
    }

    fn send_frame(&self, frame: &[u8]) -> Result<(), NetDeviceError> {
        if frame.is_empty() || frame.len() > TX_BUF_LEN {
            return Err(NetDeviceError::TooLarge);
        }
        let _g = self.lock.lock();
        let mut cur = self.tx_cur.lock();
        let i = *cur;
        // SAFETY: ring/buf bases are identity-mapped DMA; `i < RING_LEN`.
        unsafe {
            let d = (self.tx_ring + i * 16) as *mut TxDesc;
            // Wait for the slot to be free (DD set) — bounded.
            let mut spins = 0u32;
            while (*d).status & TXD_STAT_DD == 0 {
                spins += 1;
                if spins > 1_000_000 {
                    return Err(NetDeviceError::Busy);
                }
                core::hint::spin_loop();
            }
            let buf = (self.tx_bufs + i * TX_BUF_LEN) as *mut u8;
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buf, frame.len());
            (*d).addr = (self.tx_bufs + i * TX_BUF_LEN) as u64;
            (*d).len = frame.len() as u16;
            (*d).cmd = TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS;
            (*d).status = 0;
            let next = (i + 1) % RING_LEN;
            *cur = next;
            // Advance the tail so the device transmits.
            self.write_reg(REG_TDT, next as u32);
            // Poll for completion (DD set again) — bounded.
            let mut spins = 0u32;
            while (*d).status & TXD_STAT_DD == 0 {
                spins += 1;
                if spins > 1_000_000 {
                    return Err(NetDeviceError::Busy);
                }
                core::hint::spin_loop();
            }
            Ok(())
        }
    }

    fn recv_frame(&self, buf: &mut [u8]) -> Result<Option<usize>, NetDeviceError> {
        let _g = self.lock.lock();
        let mut cur = self.rx_cur.lock();
        let i = *cur;
        // SAFETY: ring/buf bases are identity-mapped DMA; `i < RING_LEN`.
        unsafe {
            let d = (self.rx_ring + i * 16) as *mut RxDesc;
            if (*d).status & RXD_STAT_DD == 0 {
                return Ok(None); // nothing received in this slot
            }
            // We only handle single-descriptor frames (EOP set, frame <= buf).
            let eop = (*d).status & RXD_STAT_EOP != 0;
            let len = (*d).len as usize;
            let mut out = None;
            if eop && len > 0 && len <= buf.len() {
                let src = (self.rx_bufs + i * RX_BUF_LEN) as *const u8;
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), len);
                out = Some(len);
            }
            // Recycle this descriptor: clear status, hand it back via the tail.
            (*d).status = 0;
            let next = (i + 1) % RING_LEN;
            *cur = next;
            // The tail points at the last descriptor the driver owns; set it to
            // the slot we just freed so the device can refill it.
            self.write_reg(REG_RDT, i as u32);
            if out.is_some() {
                Ok(out)
            } else if len > buf.len() {
                Err(NetDeviceError::TooLarge)
            } else {
                Ok(None)
            }
        }
    }
}
