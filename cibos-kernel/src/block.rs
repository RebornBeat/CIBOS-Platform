//! # Block devices
//!
//! The portable interface between storage hardware drivers and the layers above
//! them (a filesystem, the persistence volume). A block device is a fixed-size
//! array of 512-byte logical blocks addressed by LBA. Concrete drivers — the
//! x86 ATA-PIO driver today, AHCI/NVMe later — implement [`BlockDevice`]; the
//! filesystem and persistence code depend only on this trait, so they are
//! driver- and architecture-independent and unit-testable against an in-memory
//! device.
//!
//! Errors are deliberately coarse ([`BlockError`]); a driver maps its hardware
//! status into these. Reads and writes operate on whole blocks; partial-block
//! access is the caller's concern (the filesystem buffers).

/// Logical block size in bytes. 512 is the universal sector size for ATA, AHCI,
/// and the partitioning the bootloader uses; 4Kn devices are presented to this
/// layer as 512-byte logical blocks by their drivers.
pub const BLOCK_SIZE: usize = 512;

/// A block I/O error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// The LBA (or LBA + count) is outside the device.
    OutOfRange,
    /// The caller's buffer length is not a whole number of blocks, or does not
    /// match the requested block count.
    BadBuffer,
    /// The device reported an error (driver-specific hardware failure).
    DeviceError,
    /// The operation timed out waiting on the device.
    Timeout,
    /// The device is read-only and a write was attempted.
    ReadOnly,
}

/// A fixed-size array of 512-byte logical blocks.
///
/// Implementations must be safe to call from the contexts the kernel uses them
/// in; the ATA-PIO driver, for example, serializes access through a lock since
/// it pokes shared controller registers.
pub trait BlockDevice {
    /// Total number of addressable blocks.
    fn block_count(&self) -> u64;

    /// Whether the device rejects writes.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Read `count` blocks starting at `lba` into `buf`, which must be exactly
    /// `count * BLOCK_SIZE` bytes.
    ///
    /// # Errors
    ///
    /// [`BlockError::OutOfRange`] if the range exceeds the device,
    /// [`BlockError::BadBuffer`] if `buf` is mis-sized, or a device/timeout
    /// error from the driver.
    fn read_blocks(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), BlockError>;

    /// Write `count` blocks starting at `lba` from `buf`, which must be exactly
    /// `count * BLOCK_SIZE` bytes.
    ///
    /// # Errors
    ///
    /// As [`read_blocks`](BlockDevice::read_blocks), plus [`BlockError::ReadOnly`]
    /// if the device does not accept writes.
    fn write_blocks(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), BlockError>;
}

/// Validate an LBA range and buffer size against a device of `total` blocks.
/// Drivers call this at the top of read/write to centralize the bounds checks.
///
/// # Errors
///
/// [`BlockError::BadBuffer`] or [`BlockError::OutOfRange`].
pub fn check_range(
    total: u64,
    lba: u64,
    count: u32,
    buf_len: usize,
) -> Result<(), BlockError> {
    if count == 0 {
        return Err(BlockError::BadBuffer);
    }
    if buf_len != count as usize * BLOCK_SIZE {
        return Err(BlockError::BadBuffer);
    }
    let end = lba
        .checked_add(count as u64)
        .ok_or(BlockError::OutOfRange)?;
    if end > total {
        return Err(BlockError::OutOfRange);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    /// A simple in-memory block device for testing the trait and its consumers.
    struct RamDisk {
        blocks: RefCell<Vec<u8>>,
        count: u64,
        ro: bool,
    }

    impl RamDisk {
        fn new(count: u64) -> Self {
            RamDisk {
                blocks: RefCell::new(vec![0u8; count as usize * BLOCK_SIZE]),
                count,
                ro: false,
            }
        }
    }

    impl BlockDevice for RamDisk {
        fn block_count(&self) -> u64 {
            self.count
        }
        fn is_read_only(&self) -> bool {
            self.ro
        }
        fn read_blocks(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), BlockError> {
            check_range(self.count, lba, count, buf.len())?;
            let start = lba as usize * BLOCK_SIZE;
            let len = count as usize * BLOCK_SIZE;
            buf.copy_from_slice(&self.blocks.borrow()[start..start + len]);
            Ok(())
        }
        fn write_blocks(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), BlockError> {
            if self.ro {
                return Err(BlockError::ReadOnly);
            }
            check_range(self.count, lba, count, buf.len())?;
            let start = lba as usize * BLOCK_SIZE;
            let len = count as usize * BLOCK_SIZE;
            self.blocks.borrow_mut()[start..start + len].copy_from_slice(buf);
            Ok(())
        }
    }

    #[test]
    fn write_then_read_roundtrip() {
        let dev = RamDisk::new(8);
        let mut out = vec![0xABu8; BLOCK_SIZE];
        dev.write_blocks(3, 1, &out).unwrap();
        let mut back = vec![0u8; BLOCK_SIZE];
        dev.read_blocks(3, 1, &mut back).unwrap();
        assert_eq!(out, back);
        // A different block is still zero.
        out.fill(0);
        dev.read_blocks(4, 1, &mut back).unwrap();
        assert_eq!(out, back);
    }

    #[test]
    fn multi_block_roundtrip() {
        let dev = RamDisk::new(8);
        let mut pat = vec![0u8; BLOCK_SIZE * 3];
        for (i, b) in pat.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        dev.write_blocks(2, 3, &pat).unwrap();
        let mut back = vec![0u8; BLOCK_SIZE * 3];
        dev.read_blocks(2, 3, &mut back).unwrap();
        assert_eq!(pat, back);
    }

    #[test]
    fn range_and_buffer_checks() {
        let dev = RamDisk::new(4);
        let mut buf = vec![0u8; BLOCK_SIZE];
        // Past the end.
        assert_eq!(dev.read_blocks(4, 1, &mut buf), Err(BlockError::OutOfRange));
        assert_eq!(dev.read_blocks(3, 2, &mut vec![0u8; 2 * BLOCK_SIZE]), Err(BlockError::OutOfRange));
        // Mis-sized buffer.
        assert_eq!(dev.read_blocks(0, 1, &mut vec![0u8; 10]), Err(BlockError::BadBuffer));
        // Zero count.
        assert_eq!(dev.read_blocks(0, 0, &mut []), Err(BlockError::BadBuffer));
    }

    #[test]
    fn read_only_rejects_writes() {
        let mut dev = RamDisk::new(2);
        dev.ro = true;
        assert_eq!(dev.write_blocks(0, 1, &vec![0u8; BLOCK_SIZE]), Err(BlockError::ReadOnly));
    }

    #[test]
    fn check_range_direct() {
        assert_eq!(check_range(10, 0, 1, BLOCK_SIZE), Ok(()));
        assert_eq!(check_range(10, 9, 1, BLOCK_SIZE), Ok(()));
        assert_eq!(check_range(10, 9, 2, 2 * BLOCK_SIZE), Err(BlockError::OutOfRange));
        assert_eq!(check_range(10, u64::MAX, 1, BLOCK_SIZE), Err(BlockError::OutOfRange));
        assert_eq!(check_range(10, 0, 1, 7), Err(BlockError::BadBuffer));
    }
}
