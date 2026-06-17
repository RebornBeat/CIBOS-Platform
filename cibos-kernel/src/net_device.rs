//! # Network devices
//!
//! The portable interface between network-interface hardware drivers and the
//! layers above them (a TCP/IP stack, and beneath it the Lattice's NIC-backed
//! transport). This is the networking analogue of [`block`](crate::block): a
//! concrete driver — the virtio-net driver today, e1000 later — implements
//! [`NetDevice`]; the layers above depend only on this trait, so they are
//! driver- and architecture-independent and unit-testable against an in-memory
//! device.
//!
//! Unlike a block device (a fixed array of addressable 512-byte blocks), a NIC
//! is **frame-oriented**: it sends and receives whole link-layer frames
//! (Ethernet frames here), it has a hardware MAC address, an MTU, and a link
//! that may be up or down. Errors are deliberately coarse ([`NetDeviceError`]);
//! a driver maps its hardware status into these.
//!
//! IMPORTANT (alignment): adding a NIC transport does NOT change the Lattice
//! Gate/Link/Warden surface. This trait is the *backing transport* the Lattice
//! can sit on; applications and the net syscalls are unchanged when the fabric
//! moves from loopback to a real NIC (see `NETWORKING.md`).

/// A 6-byte Ethernet MAC address.
pub type MacAddress = [u8; 6];

/// The maximum standard Ethernet payload (frame MTU), in bytes. Jumbo frames are
/// a driver-specific extension presented above this default when supported.
pub const DEFAULT_MTU: usize = 1500;

/// A network device I/O error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetDeviceError {
    /// The link is down; no frames can be sent or received.
    LinkDown,
    /// The frame exceeds the device MTU (on send) or the caller's buffer was too
    /// small to hold a received frame.
    TooLarge,
    /// The transmit path is momentarily full (rings exhausted); retry later.
    Busy,
    /// The device reported an error (driver-specific hardware failure).
    DeviceError,
}

/// A frame-oriented network interface.
///
/// Implementations must be safe to call from the contexts the kernel uses them
/// in; the virtio-net driver, for example, serializes access through a lock
/// since it pokes shared virtqueue and device registers.
pub trait NetDevice {
    /// The device's hardware MAC address.
    fn mac(&self) -> MacAddress;

    /// Whether the link is currently up (carrier present).
    fn link_up(&self) -> bool;

    /// The device MTU (maximum frame payload) in bytes.
    fn mtu(&self) -> usize {
        DEFAULT_MTU
    }

    /// Transmit one link-layer frame. `frame` must be a complete frame no larger
    /// than the MTU (plus headers, as the driver defines).
    ///
    /// # Errors
    ///
    /// [`NetDeviceError::LinkDown`] if the carrier is down,
    /// [`NetDeviceError::TooLarge`] if the frame exceeds the MTU,
    /// [`NetDeviceError::Busy`] if the transmit ring is full, or
    /// [`NetDeviceError::DeviceError`] on a hardware failure.
    fn send_frame(&self, frame: &[u8]) -> Result<(), NetDeviceError>;

    /// Receive one waiting frame into `buf`, returning the number of bytes
    /// written, or `None` if no frame is currently waiting (non-blocking).
    ///
    /// # Errors
    ///
    /// [`NetDeviceError::LinkDown`] if the carrier is down,
    /// [`NetDeviceError::TooLarge`] if `buf` is smaller than the waiting frame,
    /// or [`NetDeviceError::DeviceError`] on a hardware failure.
    fn recv_frame(&self, buf: &mut [u8]) -> Result<Option<usize>, NetDeviceError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::collections::VecDeque;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    /// A simple in-memory loopback NIC for testing the trait and its consumers:
    /// frames sent are queued and become available to receive on the same device.
    struct LoopbackNic {
        mac: MacAddress,
        up: RefCell<bool>,
        queue: RefCell<VecDeque<Vec<u8>>>,
    }

    impl LoopbackNic {
        fn new() -> Self {
            Self {
                mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
                up: RefCell::new(true),
                queue: RefCell::new(VecDeque::new()),
            }
        }
    }

    impl NetDevice for LoopbackNic {
        fn mac(&self) -> MacAddress {
            self.mac
        }
        fn link_up(&self) -> bool {
            *self.up.borrow()
        }
        fn send_frame(&self, frame: &[u8]) -> Result<(), NetDeviceError> {
            if !*self.up.borrow() {
                return Err(NetDeviceError::LinkDown);
            }
            if frame.len() > self.mtu() {
                return Err(NetDeviceError::TooLarge);
            }
            self.queue.borrow_mut().push_back(frame.to_vec());
            Ok(())
        }
        fn recv_frame(&self, buf: &mut [u8]) -> Result<Option<usize>, NetDeviceError> {
            if !*self.up.borrow() {
                return Err(NetDeviceError::LinkDown);
            }
            let mut q = self.queue.borrow_mut();
            let Some(frame) = q.front() else {
                return Ok(None);
            };
            if buf.len() < frame.len() {
                return Err(NetDeviceError::TooLarge);
            }
            let frame = q.pop_front().unwrap();
            buf[..frame.len()].copy_from_slice(&frame);
            Ok(Some(frame.len()))
        }
    }

    #[test]
    fn mac_and_default_mtu() {
        let nic = LoopbackNic::new();
        assert_eq!(nic.mac(), [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        assert_eq!(nic.mtu(), DEFAULT_MTU);
        assert!(nic.link_up());
    }

    #[test]
    fn send_then_recv_round_trips_a_frame() {
        let nic = LoopbackNic::new();
        nic.send_frame(b"hello-frame").unwrap();
        let mut buf = vec![0u8; 64];
        let n = nic.recv_frame(&mut buf).unwrap().expect("a frame is waiting");
        assert_eq!(&buf[..n], b"hello-frame");
    }

    #[test]
    fn recv_with_no_frame_returns_none() {
        let nic = LoopbackNic::new();
        let mut buf = vec![0u8; 64];
        assert_eq!(nic.recv_frame(&mut buf).unwrap(), None);
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let nic = LoopbackNic::new();
        let big = vec![0u8; DEFAULT_MTU + 1];
        assert_eq!(nic.send_frame(&big), Err(NetDeviceError::TooLarge));
    }

    #[test]
    fn recv_into_small_buffer_is_too_large() {
        let nic = LoopbackNic::new();
        nic.send_frame(b"0123456789").unwrap();
        let mut small = vec![0u8; 4];
        assert_eq!(nic.recv_frame(&mut small), Err(NetDeviceError::TooLarge));
    }

    #[test]
    fn link_down_blocks_send_and_recv() {
        let nic = LoopbackNic::new();
        *nic.up.borrow_mut() = false;
        assert_eq!(nic.send_frame(b"x"), Err(NetDeviceError::LinkDown));
        let mut buf = vec![0u8; 16];
        assert_eq!(nic.recv_frame(&mut buf), Err(NetDeviceError::LinkDown));
    }
}
