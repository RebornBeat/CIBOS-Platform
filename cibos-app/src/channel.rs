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

// ---- Cross-boundary channel handshake ---------------------------------------
//
// These map the canonical request/accept-or-reject model onto ring-3:
//   * `request_channel` proposes terms to a TARGET boundary; returns a request
//     id. No channel exists yet.
//   * the target `poll_channel_request`s to see pending proposals aimed at it,
//     then `accept_channel` (accept-ALL) or `reject_channel`.
//   * the requester `poll_channel_outcome`s its request id to learn its channel
//     handle once accepted (WouldBlock while pending, NotFound if rejected).
// On accept BOTH ends hold a handle to the SAME kernel channel; bytes flow
// through the kernel via the normal `send`/`recv`.

use shared::protocols::ipc::{
    ChannelDirection, ChannelRequestWire, ChannelTerms, ChannelTermsWire,
    CHANNEL_REQUEST_WIRE_LEN, CHANNEL_TERMS_WIRE_LEN,
};

/// A pending cross-boundary request identifier (returned by [`request_channel`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RequestId(u64);

impl RequestId {
    /// The raw request id.
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// What a receiver learns about a pending request: who asked, and the proposed
/// terms (which it accepts wholesale or rejects).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IncomingRequest {
    /// The request id to accept or reject.
    pub id: RequestId,
    /// The boundary that issued the request.
    pub requester: u64,
    /// The maximum message size proposed.
    pub max_message_bytes: u32,
    /// The buffer capacity proposed.
    pub buffer_capacity: u32,
}

/// Propose a cross-boundary channel to `target_boundary` with the given terms.
/// Returns a [`RequestId`]; poll its outcome with [`poll_channel_outcome`].
pub fn request_channel(
    target_boundary: u64,
    purpose: &str,
    direction: ChannelDirection,
    max_message_bytes: u32,
    buffer_capacity: u32,
) -> Result<RequestId, SyscallError> {
    let terms = ChannelTerms::new(purpose, direction, max_message_bytes, buffer_capacity)
        .map_err(|_| SyscallError::InvalidArgument)?;
    let bytes = ChannelTermsWire::from_terms(&terms).to_bytes();
    // SAFETY: bytes is a valid readable buffer of exactly CHANNEL_TERMS_WIRE_LEN;
    // the kernel validates the pointer against the calling boundary and copies it.
    let ret = unsafe {
        syscall3(
            Syscall::RequestChannel,
            target_boundary,
            bytes.as_ptr() as u64,
            CHANNEL_TERMS_WIRE_LEN as u64,
        )
    };
    decode(ret).map(|id| RequestId(id as u64))
}

/// Poll for the next pending request aimed at THIS boundary. Returns
/// `Ok(Some(..))` with the request to decide, `Ok(None)` if none is pending.
pub fn poll_channel_request() -> Result<Option<IncomingRequest>, SyscallError> {
    let mut buf = [0u8; CHANNEL_REQUEST_WIRE_LEN];
    // SAFETY: buf is a valid writable buffer of exactly CHANNEL_REQUEST_WIRE_LEN;
    // the kernel writes the encoded request into it.
    let ret = unsafe {
        syscall3(
            Syscall::PollChannelRequest,
            buf.as_mut_ptr() as u64,
            CHANNEL_REQUEST_WIRE_LEN as u64,
            0,
        )
    };
    match decode(ret) {
        Ok(id) => {
            let wire = ChannelRequestWire::from_bytes(&buf);
            Ok(Some(IncomingRequest {
                id: RequestId(id as u64),
                requester: wire.requester,
                max_message_bytes: wire.terms.max_message_bytes,
                buffer_capacity: wire.terms.buffer_capacity,
            }))
        }
        Err(SyscallError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Accept a pending request WHOLESALE, returning this end's channel handle.
pub fn accept_channel(id: RequestId) -> Result<ChannelHandle, SyscallError> {
    // SAFETY: AcceptChannel takes a scalar request id and no pointers.
    let ret = unsafe { syscall3(Syscall::AcceptChannel, id.0, 0, 0) };
    decode(ret).map(|h| ChannelHandle(h as u64))
}

/// Reject a pending request.
pub fn reject_channel(id: RequestId) -> Result<(), SyscallError> {
    // SAFETY: RejectChannel takes a scalar request id and no pointers.
    let ret = unsafe { syscall3(Syscall::RejectChannel, id.0, 0, 0) };
    decode(ret).map(|_| ())
}

/// Poll the outcome of a request this boundary made. `Ok(Some(handle))` once the
/// target accepted; `Ok(None)` while still pending (WouldBlock); `Err(NotFound)`
/// if the request was rejected or is unknown.
pub fn poll_channel_outcome(id: RequestId) -> Result<Option<ChannelHandle>, SyscallError> {
    // SAFETY: PollChannelOutcome takes a scalar request id and no pointers.
    let ret = unsafe { syscall3(Syscall::PollChannelOutcome, id.0, 0, 0) };
    match decode(ret) {
        Ok(h) => Ok(Some(ChannelHandle(h as u64))),
        Err(SyscallError::WouldBlock) => Ok(None),
        Err(e) => Err(e),
    }
}
