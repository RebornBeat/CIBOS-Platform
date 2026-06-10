//! ATA PIO block driver (legacy IDE / PATA, primary bus, polled).
//!
//! Drives a disk on the legacy primary ATA bus using programmed I/O — the
//! universal, real-hardware storage path: legacy IDE controllers expose it
//! directly, and most SATA/AHCI controllers present a legacy IDE-compatible
//! interface on these same fixed ports. It needs no PCI enumeration and no DMA,
//! which makes it the right first storage driver; AHCI and NVMe (PCI + MMIO +
//! command queues) layer on later behind the same [`BlockDevice`] trait.
//!
//! This implements LBA28 (28-bit addressing, up to 128 GiB) reads and writes of
//! 512-byte sectors against the master device on the primary bus
//! (I/O ports `0x1F0..=0x1F7`, control `0x3F6`). Access is serialized by a lock
//! since the controller registers are shared global state. The driver polls the
//! status register (no IRQs); a bounded spin guards against a missing or wedged
//! device so a read/write fails with [`BlockError::Timeout`] rather than
//! hanging.

use cibos_kernel::block::{check_range, BlockDevice, BlockError, BLOCK_SIZE};
use cibos_kernel::sync::SpinLock;

// Primary bus I/O ports.
const DATA: u16 = 0x1F0; // 16-bit data register
const FEATURES: u16 = 0x1F1; // write: features; read: error
const SECCOUNT: u16 = 0x1F2;
const LBA_LO: u16 = 0x1F3;
const LBA_MID: u16 = 0x1F4;
const LBA_HI: u16 = 0x1F5;
const DRIVE: u16 = 0x1F6; // drive/head select
const STATUS_CMD: u16 = 0x1F7; // read: status; write: command
const CTRL: u16 = 0x3F6; // device control / alternate status

// Status register bits.
const SR_ERR: u8 = 0x01;
const SR_DRQ: u8 = 0x08; // data request ready
const SR_DF: u8 = 0x20; // device fault
const SR_BSY: u8 = 0x80;

// Commands.
const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_CACHE_FLUSH: u8 = 0xE7;
const CMD_IDENTIFY: u8 = 0xEC;

// Drive-select base for LBA mode (bit 6 = LBA, bits 7/5 set per the legacy
// spec); bit 4 selects master (0) vs slave (1): master 0xE0, slave 0xF0.
const DRIVE_LBA_MASTER: u8 = 0xE0;
const DRIVE_LBA_SLAVE: u8 = 0xF0;

/// Which device on the primary ATA bus to address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Slave is used by the storage-selftest FS demo (a data disk)
pub enum Device {
    /// Primary master (the typical boot disk).
    Master,
    /// Primary slave (a second attached disk).
    Slave,
}

impl Device {
    fn select_base(self) -> u8 {
        match self {
            Device::Master => DRIVE_LBA_MASTER,
            Device::Slave => DRIVE_LBA_SLAVE,
        }
    }
}

// Bounded poll iterations before declaring a timeout. Generous; each iteration
// is a port read.
const POLL_LIMIT: u32 = 10_000_000;

/// A handle to a device on the primary ATA bus.
pub struct AtaDisk {
    lock: SpinLock<()>,
    sectors: u64,
    select: u8,
}

impl AtaDisk {
    /// Probe `device` on the primary bus with IDENTIFY. Returns a disk handle
    /// and its sector count, or `None` if no ATA device responds there.
    ///
    /// # Safety
    ///
    /// Touches fixed ATA I/O ports; call once per device during single-threaded
    /// bring-up.
    pub unsafe fn probe(device: Device) -> Option<AtaDisk> {
        let select = device.select_base();
        // Select the device, LBA mode, and issue IDENTIFY with LBA/count zeroed.
        outb(DRIVE, select);
        io_wait();
        outb(SECCOUNT, 0);
        outb(LBA_LO, 0);
        outb(LBA_MID, 0);
        outb(LBA_HI, 0);
        outb(STATUS_CMD, CMD_IDENTIFY);

        // Status 0 => no device on this bus.
        let st = inb(STATUS_CMD);
        if st == 0 {
            return None;
        }
        // Wait for BSY to clear; if LBA_MID/HI become non-zero the device is not
        // a plain ATA disk (e.g. ATAPI) — treat as absent for this driver.
        if !wait_clear(SR_BSY) {
            return None;
        }
        if inb(LBA_MID) != 0 || inb(LBA_HI) != 0 {
            return None;
        }
        // Wait for DRQ (data ready) or ERR.
        if !wait_drq_or_err() {
            return None;
        }
        if inb(STATUS_CMD) & SR_ERR != 0 {
            return None;
        }
        // Read the 256-word IDENTIFY block; words 60..61 hold the LBA28 sector
        // count (a u32, little-endian word order).
        let mut id = [0u16; 256];
        for w in id.iter_mut() {
            *w = inw(DATA);
        }
        let lba28 = (id[60] as u64) | ((id[61] as u64) << 16);
        let sectors = if lba28 == 0 { 0 } else { lba28 };
        if sectors == 0 {
            return None;
        }
        Some(AtaDisk {
            lock: SpinLock::new(()),
            sectors,
            select,
        })
    }

    /// Total addressable sectors (LBA28).
    #[must_use]
    pub fn sectors(&self) -> u64 {
        self.sectors
    }

    /// Common preamble: select the device and load the LBA + count registers for
    /// an LBA28 access of `count` sectors at `lba`.
    ///
    /// # Safety
    ///
    /// Caller holds the bus lock.
    unsafe fn setup(&self, lba: u64, count: u8) {
        outb(DRIVE, self.select | (((lba >> 24) & 0x0F) as u8));
        outb(FEATURES, 0);
        outb(SECCOUNT, count);
        outb(LBA_LO, (lba & 0xFF) as u8);
        outb(LBA_MID, ((lba >> 8) & 0xFF) as u8);
        outb(LBA_HI, ((lba >> 16) & 0xFF) as u8);
    }
}

impl BlockDevice for AtaDisk {
    fn block_count(&self) -> u64 {
        self.sectors
    }

    fn read_blocks(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), BlockError> {
        check_range(self.sectors, lba, count, buf.len())?;
        // LBA28 with an 8-bit sector count: 0 means 256. Do up to 255 per call
        // and loop for larger requests to keep the count in range.
        let _guard = self.lock.lock();
        let mut done: u32 = 0;
        while done < count {
            let chunk = core::cmp::min(count - done, 255) as u8;
            let cur_lba = lba + done as u64;
            // SAFETY: bus lock held; ports are the fixed primary-bus registers.
            unsafe {
                self.setup(cur_lba, chunk);
                outb(STATUS_CMD, CMD_READ_SECTORS);
                for s in 0..chunk as u32 {
                    if !wait_drq_or_err() {
                        return Err(BlockError::Timeout);
                    }
                    let st = inb(STATUS_CMD);
                    if st & (SR_ERR | SR_DF) != 0 {
                        return Err(BlockError::DeviceError);
                    }
                    let off = (done + s) as usize * BLOCK_SIZE;
                    let sector = &mut buf[off..off + BLOCK_SIZE];
                    for w in 0..(BLOCK_SIZE / 2) {
                        let word = inw(DATA);
                        sector[w * 2] = (word & 0xFF) as u8;
                        sector[w * 2 + 1] = (word >> 8) as u8;
                    }
                }
            }
            done += chunk as u32;
        }
        Ok(())
    }

    fn write_blocks(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), BlockError> {
        check_range(self.sectors, lba, count, buf.len())?;
        let _guard = self.lock.lock();
        let mut done: u32 = 0;
        while done < count {
            let chunk = core::cmp::min(count - done, 255) as u8;
            let cur_lba = lba + done as u64;
            // SAFETY: bus lock held; ports are the fixed primary-bus registers.
            unsafe {
                self.setup(cur_lba, chunk);
                outb(STATUS_CMD, CMD_WRITE_SECTORS);
                for s in 0..chunk as u32 {
                    if !wait_drq_or_err() {
                        return Err(BlockError::Timeout);
                    }
                    let st = inb(STATUS_CMD);
                    if st & (SR_ERR | SR_DF) != 0 {
                        return Err(BlockError::DeviceError);
                    }
                    let off = (done + s) as usize * BLOCK_SIZE;
                    let sector = &buf[off..off + BLOCK_SIZE];
                    for w in 0..(BLOCK_SIZE / 2) {
                        let word =
                            (sector[w * 2] as u16) | ((sector[w * 2 + 1] as u16) << 8);
                        outw(DATA, word);
                    }
                }
                // Flush the write cache so data reaches the medium.
                outb(STATUS_CMD, CMD_CACHE_FLUSH);
                if !wait_clear(SR_BSY) {
                    return Err(BlockError::Timeout);
                }
            }
            done += chunk as u32;
        }
        Ok(())
    }
}

// ---- low-level port + polling helpers (all assume the bus lock is held) ----

#[inline]
unsafe fn outb(port: u16, val: u8) {
    crate::arch::outb_port(port, val);
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    crate::arch::inb_port(port)
}

#[inline]
unsafe fn inw(port: u16) -> u16 {
    crate::arch::inw(port)
}

#[inline]
unsafe fn outw(port: u16, val: u16) {
    crate::arch::outw(port, val);
}

/// A short delay by reading the alternate status register a few times (each read
/// is ~100 ns; four reads give the spec's 400 ns settle after a select).
unsafe fn io_wait() {
    for _ in 0..4 {
        let _ = inb(CTRL);
    }
}

/// Poll until the given status bit(s) clear, or a bounded timeout. Returns
/// `true` if cleared.
unsafe fn wait_clear(bits: u8) -> bool {
    let mut n = 0u32;
    while n < POLL_LIMIT {
        if inb(STATUS_CMD) & bits == 0 {
            return true;
        }
        n += 1;
    }
    false
}

/// Poll until DRQ is set (data ready) or ERR is set, with BSY clear, or timeout.
/// Returns `true` if the device is ready to transfer (or signalled ERR — the
/// caller then inspects the status); `false` on timeout.
unsafe fn wait_drq_or_err() -> bool {
    let mut n = 0u32;
    while n < POLL_LIMIT {
        let st = inb(STATUS_CMD);
        if st & SR_BSY == 0 && (st & (SR_DRQ | SR_ERR) != 0) {
            return true;
        }
        n += 1;
    }
    false
}
