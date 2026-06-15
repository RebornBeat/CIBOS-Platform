//! Local channel communication and lane spawning for a CIBOS application, over
//! the `OpenChannel`/`ChannelSend`/`ChannelRecv`/`Spawn` syscalls.
//!
//! These map the canonical HIP IPC model onto the ring-3 ABI:
//!
//! * A channel is a point-to-point, bounded, back-pressured message buffer
//!   (the `Channel::new_local` contract from the API reference). Sending on a
//!   full buffer or receiving from an empty open buffer reports
//!   [`SyscallError::WouldBlock`] — the Catch-and-Release signal that the lane
//!   should park and retry. The wrappers surface that distinctly so the caller
//!   can choose to retry or do other work, exactly like `try_send`/`try_recv`.
//! * [`spawn`] requests a new cooperative lane. The single-selector kernel
//!   executor owns dispatch; the application does not block waiting for it.

use crate::syscall::{decode, syscall3};
use shared::protocols::syscall::{Syscall, SyscallError};

/// A handle to a local channel, valid within the calling boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelHandle(u64);

impl ChannelHandle {
    /// The raw kernel handle value.
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Open a local bounded channel buffering up to `capacity` messages of at most
/// `max_message_bytes` each. Returns a [`ChannelHandle`] valid within this
/// boundary.
///
/// # Errors
///
/// [`SyscallError::InvalidArgument`] for a zero capacity/size or an oversized
/// message bound; [`SyscallError::NotPermitted`] if the kernel exposes no IPC
/// surface.
pub fn open(capacity: usize, max_message_bytes: usize) -> Result<ChannelHandle, SyscallError> {
    // SAFETY: OpenChannel takes two scalar arguments and no pointers.
    let ret = unsafe {
        syscall3(
            Syscall::OpenChannel,
            capacity as u64,
            max_message_bytes as u64,
            0,
        )
    };
    decode(ret).map(|h| ChannelHandle(h as u64))
}

/// Send `data` on `handle`. On a full buffer this returns
/// [`SyscallError::WouldBlock`] (back-pressure); the caller may retry later or
/// do other work in the meantime.
///
/// # Errors
///
/// [`SyscallError::WouldBlock`] when the buffer is full;
/// [`SyscallError::NotFound`] if the handle is unknown or closed;
/// [`SyscallError::InvalidArgument`] if `data` exceeds the channel's message
/// bound.
pub fn send(handle: ChannelHandle, data: &[u8]) -> Result<(), SyscallError> {
    // SAFETY: data is a valid readable slice; the kernel validates it against
    // the calling boundary and copies at most data.len() bytes.
    let ret = unsafe {
        syscall3(
            Syscall::ChannelSend,
            handle.0,
            data.as_ptr() as u64,
            data.len() as u64,
        )
    };
    decode(ret).map(|_| ())
}

/// Receive one message from `handle` into `buf`, returning the number of bytes
/// written (truncated to `buf.len()`). On an empty open buffer this returns
/// [`SyscallError::WouldBlock`]; the caller may retry later.
///
/// # Errors
///
/// [`SyscallError::WouldBlock`] when the buffer is empty but open;
/// [`SyscallError::NotFound`] if the handle is unknown or the channel is closed
/// and drained.
pub fn recv(handle: ChannelHandle, buf: &mut [u8]) -> Result<usize, SyscallError> {
    // SAFETY: buf is a valid writable slice; the kernel validates it against the
    // calling boundary and writes at most buf.len() bytes.
    let ret = unsafe {
        syscall3(
            Syscall::ChannelRecv,
            handle.0,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    decode(ret).map(|n| n as usize)
}

/// A spawned cooperative lane within the calling boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneHandle(u64);

impl LaneHandle {
    /// The raw kernel lane id.
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Request a new cooperative lane beginning at `entry` with argument `arg`. The
/// single-selector kernel executor schedules it; this call does not block.
///
/// # Errors
///
/// [`SyscallError::NotPermitted`] until the live multi-context loader is wired
/// (today a `.capp` runs one-at-a-time, so there is no live ring-3 lane surface
/// to spawn onto yet — the ABI is in place ahead of that wiring).
pub fn spawn(entry: u64, arg: u64) -> Result<LaneHandle, SyscallError> {
    // SAFETY: Spawn takes two scalar arguments and no pointers.
    let ret = unsafe { syscall3(Syscall::Spawn, entry, arg, 0) };
    decode(ret).map(|l| LaneHandle(l as u64))
}
