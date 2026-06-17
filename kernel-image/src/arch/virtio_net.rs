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
const VIRTIO_PCI_STATUS: u16 = 0x12; // r/w: device status (8-bit)
const VIRTIO_PCI_CONFIG: u16 = 0x14; // device-specific config (net: MAC[6], status)

// Device status bits.
const STATUS_RESET: u8 = 0x00;
const STATUS_ACKNOWLEDGE: u8 = 0x01;
const STATUS_DRIVER: u8 = 0x02;
const STATUS_FEATURES_OK: u8 = 0x08;
// DRIVER_OK is asserted once the virtqueues are set up (the TX/RX increment).
#[allow(dead_code)]
const STATUS_DRIVER_OK: u8 = 0x04;

// virtio-net feature bits.
const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_F_STATUS: u32 = 1 << 16;
const VIRTIO_NET_S_LINK_UP: u16 = 1;

/// A discovered + negotiated legacy virtio-net device.
pub struct VirtioNet {
    io_base: u16,
    mac: MacAddress,
    has_status: bool,
    // Serializes device register access once the TX/RX rings are driven (B3);
    // held now only by set_driver_ok.
    #[allow(dead_code)]
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
    pub unsafe fn probe() -> Option<Self> {
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

        Some(Self {
            io_base,
            mac,
            has_status,
            lock: SpinLock::new(()),
        })
    }

    /// Read the device-config link status word (valid iff VIRTIO_NET_F_STATUS).
    unsafe fn read_status(&self) -> u16 {
        // The status word follows the 6 MAC bytes in the net config region.
        inw(self.io_base + VIRTIO_PCI_CONFIG + 6)
    }

    /// Signal DRIVER_OK — the device may begin operation. (Called once the
    /// virtqueues are set up; exposed for the TX/RX increment.)
    ///
    /// # Safety
    /// Touches the device status register.
    #[allow(dead_code)]
    pub unsafe fn set_driver_ok(&self) {
        let _g = self.lock.lock();
        outb(
            self.io_base + VIRTIO_PCI_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
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

    fn send_frame(&self, _frame: &[u8]) -> Result<(), NetDeviceError> {
        // TX over the transmit virtqueue is the next increment (B3). Until the
        // rings are wired, report Busy honestly rather than silently dropping —
        // there is no fake success here.
        Err(NetDeviceError::Busy)
    }

    fn recv_frame(&self, _buf: &mut [u8]) -> Result<Option<usize>, NetDeviceError> {
        // RX over the receive virtqueue is the next increment (B3). No frame is
        // available until the rings are wired.
        Ok(None)
    }
}
