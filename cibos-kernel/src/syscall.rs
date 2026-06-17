//! # Syscall Dispatch
//!
//! The portable kernel-side handler for the [`shared::protocols::syscall`] ABI.
//! The architecture trap entry decodes the hardware registers into a
//! [`SyscallRequest`] and calls [`dispatch`]; everything here is
//! architecture-neutral and host-tested.
//!
//! ## Memory safety across the boundary
//!
//! A user pointer argument is an address in the *calling boundary's* address
//! space, which is not the kernel's. The dispatcher never dereferences a user
//! pointer directly; it asks the caller-supplied [`UserMemory`] to translate and
//! copy bytes, so the (architecture-specific, `unsafe`) act of reading another
//! address space's memory stays in one audited place and every access is
//! bounds-checked against that boundary's mappings.

use shared::protocols::syscall::{FsRwArgs, Syscall, SyscallError, FS_RW_ARGS_LEN};
use shared::BoundaryId;

/// Maximum bytes a single `log` may emit, to bound kernel work per call.
pub const MAX_LOG_LEN: usize = 4096;

/// Maximum path length the filesystem syscalls accept, to bound kernel work.
pub const MAX_PATH_LEN: usize = 1024;

/// Maximum bytes a single `fs_read`/`fs_write` may transfer, to bound kernel
/// buffering per call.
pub const MAX_FS_IO_LEN: usize = 64 * 1024;

/// A decoded syscall request: the number and up to three argument words, plus
/// the boundary that issued it.
#[derive(Debug, Clone, Copy)]
pub struct SyscallRequest {
    /// Raw syscall number (from the number register).
    pub number: u64,
    /// Argument 0.
    pub arg0: u64,
    /// Argument 1.
    pub arg1: u64,
    /// Argument 2.
    pub arg2: u64,
    /// The boundary that issued the trap.
    pub boundary: BoundaryId,
}

/// What the dispatcher decided should happen after handling a syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallOutcome {
    /// Return this value to the caller (placed in the return register).
    Return(i64),
    /// The caller asked to exit with this code; the kernel tears down the
    /// boundary rather than returning to user code.
    Exit(u64),
    /// The caller yielded; the kernel should reschedule before returning 0.
    Yield,
}

/// Access to the calling boundary's memory and to kernel services the
/// dispatcher needs. Implemented by the arch/kernel layer; modelled in tests.
pub trait SyscallEnv {
    /// Copy `len` bytes from the user virtual address `ptr` (in `boundary`'s
    /// space) into `out`. Returns `Err(BadAddress)` if the range is not fully
    /// mapped and readable in that boundary. `out.len()` bounds the copy.
    fn copy_from_user(
        &self,
        boundary: BoundaryId,
        ptr: u64,
        len: usize,
        out: &mut [u8],
    ) -> Result<(), SyscallError>;

    /// Write already-validated UTF-8-ish console bytes to the kernel console.
    fn console_write(&self, bytes: &[u8]);

    /// Monotonic nanoseconds since boot (for `now`).
    fn now_nanos(&self) -> u64;

    /// Copy `bytes` into the user buffer at virtual address `ptr` (in
    /// `boundary`'s space). Returns `Err(BadAddress)` if the range is not fully
    /// mapped and writable in that boundary.
    fn copy_to_user(
        &self,
        boundary: BoundaryId,
        ptr: u64,
        bytes: &[u8],
    ) -> Result<(), SyscallError>;

    /// Read the whole file at `path` into a kernel buffer. `Ok(None)` if the
    /// path does not exist; `Err` for other failures. The default rejects all
    /// access (a kernel without a mounted filesystem); the real environment
    /// overrides these four.
    fn fs_read(&self, _path: &[u8]) -> Result<Option<alloc::vec::Vec<u8>>, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Create/overwrite the file at `path` with `data`. `Ok(())` on success.
    fn fs_write(&self, _path: &[u8], _data: &[u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Create a directory at `path`.
    fn fs_mkdir(&self, _path: &[u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Whether `path` exists.
    fn fs_exists(&self, _path: &[u8]) -> Result<bool, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// List the directory at `path`, returning the entry names. `Ok(None)` if the
    /// path does not exist or is not a directory.
    fn fs_list(&self, _path: &[u8]) -> Result<Option<alloc::vec::Vec<alloc::string::String>>, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Delete the file at `path`. `Ok(())` on success;
    /// [`SyscallError::NotFound`] if absent.
    fn fs_delete(&self, _path: &[u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Cooperatively sleep for at least `nanos` nanoseconds, then resume. The
    /// default is a no-op (returns immediately); a kernel environment with a
    /// timer overrides it to actually wait.
    fn sleep_nanos(&self, _nanos: u64) -> Result<(), SyscallError> {
        Ok(())
    }

    /// Read the next keyboard event. `blocking` selects waiting vs immediate.
    /// Returns the packed key value (>= 0, via `encode_key`) or a negative
    /// [`SyscallError`] (`NotFound` when non-blocking and the queue is empty).
    /// The default reports no input device.
    fn read_key(&self, _blocking: bool) -> i64 {
        SyscallError::NotFound.as_return()
    }

    /// Fill `out` with cryptographically-random bytes from the kernel CSPRNG.
    /// The default reports no entropy source.
    fn fill_random(&self, _out: &mut [u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Open a local (intra-boundary) bounded channel with `capacity` buffered
    /// messages of up to `max_message_bytes` each. Returns an opaque channel
    /// handle (>= 0) valid within `boundary`. The default reports no IPC
    /// surface; a kernel environment with a channel registry overrides it.
    fn open_channel(
        &self,
        _boundary: BoundaryId,
        _capacity: usize,
        _max_message_bytes: usize,
    ) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Send `data` on the channel `handle` owned by `boundary`. Cooperative
    /// back-pressure is handled by the kernel (Catch and Release): on a full
    /// buffer the call reports [`SyscallError::WouldBlock`] so the caller's lane
    /// can be parked and retried when space is signalled. The default reports no
    /// IPC surface.
    fn channel_send(
        &self,
        _boundary: BoundaryId,
        _handle: u64,
        _data: &[u8],
    ) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Receive one message from channel `handle` owned by `boundary`. Returns
    /// the message bytes, or [`SyscallError::WouldBlock`] when the buffer is
    /// empty and the channel is open (the lane parks until data is signalled),
    /// or [`SyscallError::NotFound`] when the channel is closed and drained. The
    /// default reports no IPC surface.
    fn channel_recv(
        &self,
        _boundary: BoundaryId,
        _handle: u64,
    ) -> Result<alloc::vec::Vec<u8>, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Spawn a cooperative lane in `boundary` beginning at the user entry point
    /// `entry` with argument `arg`. Returns a lane id (>= 0). The default
    /// reports no scheduling surface; a kernel environment overrides it to spawn
    /// onto the single-selector executor.
    fn spawn(
        &self,
        _boundary: BoundaryId,
        _entry: u64,
        _arg: u64,
    ) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Propose a cross-boundary channel from `requester` to `target` with the
    /// proposed `terms`. Returns a request id (>= 0) the requester later polls.
    /// The default reports no IPC surface.
    fn request_channel(
        &self,
        _requester: BoundaryId,
        _target: BoundaryId,
        _terms: &shared::protocols::ipc::ChannelTerms,
    ) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// The next pending request aimed at `target` (the caller). Returns the
    /// request id and fills `out` with the encoded [`ChannelRequestWire`].
    /// `NotFound` if none. The default reports no IPC surface.
    fn poll_channel_request(
        &self,
        _target: BoundaryId,
        _out: &mut [u8],
    ) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Accept the pending request `request_id` (the caller `target` must be its
    /// target). Returns a channel handle (>= 0) for the accepting boundary. The
    /// default reports no IPC surface.
    fn accept_channel(
        &self,
        _target: BoundaryId,
        _request_id: u64,
    ) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Reject the pending request `request_id` (the caller must be its target).
    /// The default reports no IPC surface.
    fn reject_channel(
        &self,
        _target: BoundaryId,
        _request_id: u64,
    ) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// The requester polls the outcome of its `request_id`: a channel handle
    /// (>= 0) once accepted, `WouldBlock` while pending, `NotFound` if rejected.
    /// The default reports no IPC surface.
    fn poll_channel_outcome(
        &self,
        _requester: BoundaryId,
        _request_id: u64,
    ) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Bind the caller's boundary as the listener on Lattice `gate`. Returns a
    /// listener handle. The default reports no networking surface.
    fn gate_bind(&self, _owner: BoundaryId, _gate: u16) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Open a Link from the caller's boundary to whatever is bound on `gate`.
    /// Returns the connector's Link handle. The default reports no surface.
    fn gate_connect(&self, _from: BoundaryId, _gate: u16) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// The listener on `gate` accepts the next pending connect, returning its Link
    /// handle. The default reports no surface.
    fn gate_accept(&self, _owner: BoundaryId, _gate: u16) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Send `data` on the Link `handle`. The default reports no surface.
    fn link_send(&self, _boundary: BoundaryId, _handle: u64, _data: &[u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Receive a message from the Link `handle`. The default reports no surface.
    fn link_recv(&self, _boundary: BoundaryId, _handle: u64) -> Result<alloc::vec::Vec<u8>, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Close the Link `handle`. The default reports no surface.
    fn link_close(&self, _boundary: BoundaryId, _handle: u64) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Set the Warden policy for `gate` (`allow` false = total denial). The
    /// default reports no surface.
    fn warden_set(&self, _boundary: BoundaryId, _gate: u16, _allow: bool) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Probe `gate`: 0 Closed, 1 Open, 2 Blocked. The default reports no surface.
    fn gate_probe(&self, _boundary: BoundaryId, _gate: u16) -> Result<u64, SyscallError> {
        Err(SyscallError::NotPermitted)
    }
}

/// Handle one syscall request against `env`, returning the outcome.
///
/// Pure and synchronous: it validates arguments, performs the primitive, and
/// reports what the trap glue should do. It never panics on bad user input —
/// invalid arguments become [`SyscallError`] return values.
pub fn dispatch<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let Some(call) = Syscall::from_number(req.number) else {
        return SyscallOutcome::Return(SyscallError::NoSuchCall.as_return());
    };

    match call {
        Syscall::Log => {
            let ptr = req.arg0;
            let len = req.arg1 as usize;
            if len > MAX_LOG_LEN {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            }
            // Copy into a fixed kernel buffer (bounded by MAX_LOG_LEN), then
            // write. No allocation, no trust in the user length beyond the cap.
            let mut buf = [0u8; MAX_LOG_LEN];
            match env.copy_from_user(req.boundary, ptr, len, &mut buf[..len]) {
                Ok(()) => {
                    env.console_write(&buf[..len]);
                    SyscallOutcome::Return(0)
                }
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::Exit => SyscallOutcome::Exit(req.arg0),
        Syscall::Yield => SyscallOutcome::Yield,
        Syscall::Now => {
            // Monotonic nanoseconds fit in i64 for ~292 years; clamp defensively.
            let ns = env.now_nanos();
            SyscallOutcome::Return((ns & (i64::MAX as u64)) as i64)
        }
        Syscall::FsRead => fs_read(req, env),
        Syscall::FsWrite => fs_write(req, env),
        Syscall::FsMkdir => {
            match read_path(req.boundary, req.arg0, req.arg1 as usize, env) {
                Ok(path) => match env.fs_mkdir(&path) {
                    Ok(()) => SyscallOutcome::Return(0),
                    Err(e) => SyscallOutcome::Return(e.as_return()),
                },
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::FsExists => {
            match read_path(req.boundary, req.arg0, req.arg1 as usize, env) {
                Ok(path) => match env.fs_exists(&path) {
                    Ok(true) => SyscallOutcome::Return(1),
                    Ok(false) => SyscallOutcome::Return(0),
                    Err(e) => SyscallOutcome::Return(e.as_return()),
                },
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::ReadKey => SyscallOutcome::Return(env.read_key(req.arg0 != 0)),
        Syscall::GetRandom => get_random(req, env),
        Syscall::FsList => fs_list(req, env),
        Syscall::FsDelete => {
            match read_path(req.boundary, req.arg0, req.arg1 as usize, env) {
                Ok(path) => match env.fs_delete(&path) {
                    Ok(()) => SyscallOutcome::Return(0),
                    Err(e) => SyscallOutcome::Return(e.as_return()),
                },
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::Sleep => {
            // Duration is a u64 carried in arg0 (low) and arg1 (high).
            let nanos = req.arg0 | (req.arg1 << 32);
            match env.sleep_nanos(nanos) {
                Ok(()) => SyscallOutcome::Return(0),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::OpenChannel => {
            // arg0 = buffer capacity (messages), arg1 = max message bytes.
            let capacity = req.arg0 as usize;
            let max_message_bytes = req.arg1 as usize;
            if capacity == 0 || max_message_bytes == 0 || max_message_bytes > MAX_FS_IO_LEN {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            }
            match env.open_channel(req.boundary, capacity, max_message_bytes) {
                // Handle is non-negative; clamp into the i64 success range.
                Ok(handle) => SyscallOutcome::Return((handle & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::ChannelSend => channel_send(req, env),
        Syscall::ChannelRecv => channel_recv(req, env),
        Syscall::Spawn => {
            // arg0 = user entry pointer, arg1 = argument word.
            match env.spawn(req.boundary, req.arg0, req.arg1) {
                Ok(lane) => SyscallOutcome::Return((lane & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::RequestChannel => {
            // arg0 = target boundary, arg1 = terms_ptr, arg2 = terms_len.
            use shared::protocols::ipc::{ChannelTermsWire, CHANNEL_TERMS_WIRE_LEN};
            let target = BoundaryId(req.arg0);
            if (req.arg2 as usize) < CHANNEL_TERMS_WIRE_LEN {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            }
            let mut buf = [0u8; CHANNEL_TERMS_WIRE_LEN];
            if let Err(e) =
                env.copy_from_user(req.boundary, req.arg1, CHANNEL_TERMS_WIRE_LEN, &mut buf)
            {
                return SyscallOutcome::Return(e.as_return());
            }
            let Some(terms) = ChannelTermsWire::from_bytes(&buf).to_terms() else {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            };
            // A boundary cannot request a channel TO ITSELF via the cross-boundary
            // handshake (that is what OpenChannel is for); keep them distinct.
            if target == req.boundary {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            }
            match env.request_channel(req.boundary, target, &terms) {
                Ok(id) => SyscallOutcome::Return((id & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::PollChannelRequest => {
            // arg0 = out_ptr, arg1 = out_len. The CALLER's boundary is the target.
            use shared::protocols::ipc::CHANNEL_REQUEST_WIRE_LEN;
            if (req.arg1 as usize) < CHANNEL_REQUEST_WIRE_LEN {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            }
            let mut out = [0u8; CHANNEL_REQUEST_WIRE_LEN];
            match env.poll_channel_request(req.boundary, &mut out) {
                Ok(id) => match env.copy_to_user(req.boundary, req.arg0, &out) {
                    Ok(()) => SyscallOutcome::Return((id & (i64::MAX as u64)) as i64),
                    Err(e) => SyscallOutcome::Return(e.as_return()),
                },
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::AcceptChannel => {
            // arg0 = request_id. The CALLER's boundary must be the target.
            match env.accept_channel(req.boundary, req.arg0) {
                Ok(handle) => SyscallOutcome::Return((handle & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::RejectChannel => {
            // arg0 = request_id. The CALLER's boundary must be the target.
            match env.reject_channel(req.boundary, req.arg0) {
                Ok(()) => SyscallOutcome::Return(0),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::PollChannelOutcome => {
            // arg0 = request_id. The CALLER is the requester.
            match env.poll_channel_outcome(req.boundary, req.arg0) {
                Ok(handle) => SyscallOutcome::Return((handle & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::GateBind => {
            // arg0 = gate (u16). Caller boundary (trap) becomes the owner.
            let Ok(gate) = u16::try_from(req.arg0) else {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            };
            match env.gate_bind(req.boundary, gate) {
                Ok(h) => SyscallOutcome::Return((h & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::GateConnect => {
            // arg0 = gate (u16). Caller boundary (trap) is the connector.
            let Ok(gate) = u16::try_from(req.arg0) else {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            };
            match env.gate_connect(req.boundary, gate) {
                Ok(h) => SyscallOutcome::Return((h & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::GateAccept => {
            // arg0 = gate (u16). Caller must own the Gate.
            let Ok(gate) = u16::try_from(req.arg0) else {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            };
            match env.gate_accept(req.boundary, gate) {
                Ok(h) => SyscallOutcome::Return((h & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::LinkSend => {
            // arg0 = handle, arg1 = ptr, arg2 = len.
            let len = req.arg2 as usize;
            let mut buf = alloc::vec![0u8; len];
            if let Err(e) = env.copy_from_user(req.boundary, req.arg1, len, &mut buf) {
                return SyscallOutcome::Return(e.as_return());
            }
            match env.link_send(req.boundary, req.arg0, &buf) {
                Ok(()) => SyscallOutcome::Return(0),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::LinkRecv => {
            // arg0 = handle, arg1 = ptr, arg2 = cap.
            match env.link_recv(req.boundary, req.arg0) {
                Ok(bytes) => {
                    let cap = req.arg2 as usize;
                    if bytes.len() > cap {
                        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
                    }
                    match env.copy_to_user(req.boundary, req.arg1, &bytes) {
                        Ok(()) => SyscallOutcome::Return((bytes.len() & (i64::MAX as usize)) as i64),
                        Err(e) => SyscallOutcome::Return(e.as_return()),
                    }
                }
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::LinkClose => {
            // arg0 = handle.
            match env.link_close(req.boundary, req.arg0) {
                Ok(()) => SyscallOutcome::Return(0),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::WardenSet => {
            // arg0 = gate (u16), arg1 = allow (0 = deny).
            let Ok(gate) = u16::try_from(req.arg0) else {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            };
            match env.warden_set(req.boundary, gate, req.arg1 != 0) {
                Ok(()) => SyscallOutcome::Return(0),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::GateProbe => {
            // arg0 = gate (u16). 0 Closed, 1 Open, 2 Blocked.
            let Ok(gate) = u16::try_from(req.arg0) else {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            };
            match env.gate_probe(req.boundary, gate) {
                Ok(state) => SyscallOutcome::Return((state & (i64::MAX as u64)) as i64),
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
    }
}

/// `channel_send`: copy the user buffer (`arg1` ptr, `arg2` len) and hand it to
/// the channel `arg0`. A full buffer surfaces as [`SyscallError::WouldBlock`].
fn channel_send<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let handle = req.arg0;
    let len = req.arg2 as usize;
    if len > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    let mut buf = alloc::vec![0u8; len];
    if let Err(e) = env.copy_from_user(req.boundary, req.arg1, len, &mut buf) {
        return SyscallOutcome::Return(e.as_return());
    }
    match env.channel_send(req.boundary, handle, &buf) {
        Ok(()) => SyscallOutcome::Return(0),
        Err(e) => SyscallOutcome::Return(e.as_return()),
    }
}

/// `channel_recv`: receive one message from channel `arg0` and copy it to the
/// user buffer (`arg1` ptr, `arg2` len), returning the byte count written
/// (truncated to the buffer). An empty open channel surfaces as
/// [`SyscallError::WouldBlock`]; a closed, drained channel as `NotFound`.
fn channel_recv<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let handle = req.arg0;
    let cap = req.arg2 as usize;
    if cap > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    match env.channel_recv(req.boundary, handle) {
        Ok(msg) => {
            let n = core::cmp::min(msg.len(), cap);
            if let Err(e) = env.copy_to_user(req.boundary, req.arg1, &msg[..n]) {
                return SyscallOutcome::Return(e.as_return());
            }
            SyscallOutcome::Return(n as i64)
        }
        Err(e) => SyscallOutcome::Return(e.as_return()),
    }
}

/// `get_random`: fill the user buffer at `arg0` (len `arg1`) with CSPRNG bytes.
fn get_random<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let len = req.arg1 as usize;
    if len == 0 || len > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    let mut buf = alloc::vec![0u8; len];
    if let Err(e) = env.fill_random(&mut buf) {
        return SyscallOutcome::Return(e.as_return());
    }
    if let Err(e) = env.copy_to_user(req.boundary, req.arg0, &buf) {
        return SyscallOutcome::Return(e.as_return());
    }
    SyscallOutcome::Return(len as i64)
}

/// Copy a bounded path from user memory into a kernel `Vec`.
fn read_path<E: SyscallEnv>(
    boundary: BoundaryId,
    ptr: u64,
    len: usize,
    env: &E,
) -> Result<alloc::vec::Vec<u8>, SyscallError> {
    if len == 0 || len > MAX_PATH_LEN {
        return Err(SyscallError::InvalidArgument);
    }
    let mut buf = alloc::vec![0u8; len];
    env.copy_from_user(boundary, ptr, len, &mut buf)?;
    Ok(buf)
}

/// Decode the `FsRwArgs` block from user memory at `ptr`.
fn read_rw_args<E: SyscallEnv>(
    boundary: BoundaryId,
    ptr: u64,
    env: &E,
) -> Result<FsRwArgs, SyscallError> {
    let mut raw = [0u8; FS_RW_ARGS_LEN];
    env.copy_from_user(boundary, ptr, FS_RW_ARGS_LEN, &mut raw)?;
    Ok(FsRwArgs::from_bytes(&raw))
}

/// `fs_read`: read the file named by the path in the arg block into the user
/// buffer in the arg block; return bytes read or a negative error.
fn fs_read<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let args = match read_rw_args(req.boundary, req.arg0, env) {
        Ok(a) => a,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    if args.buf_len as usize > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    let path = match read_path(req.boundary, args.path_ptr, args.path_len as usize, env) {
        Ok(p) => p,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    let data = match env.fs_read(&path) {
        Ok(Some(d)) => d,
        Ok(None) => return SyscallOutcome::Return(SyscallError::NotFound.as_return()),
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    // Copy up to buf_len bytes back to the user; return the count.
    let n = core::cmp::min(data.len(), args.buf_len as usize);
    if let Err(e) = env.copy_to_user(req.boundary, args.buf_ptr, &data[..n]) {
        return SyscallOutcome::Return(e.as_return());
    }
    SyscallOutcome::Return(n as i64)
}

/// `fs_list`: list the directory named by the path in the arg block, writing the
/// entry names joined by `\n` (no trailing newline) into the user buffer in the
/// arg block; return bytes written (truncated to `buf_len`) or a negative error.
fn fs_list<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let args = match read_rw_args(req.boundary, req.arg0, env) {
        Ok(a) => a,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    if args.buf_len as usize > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    let path = match read_path(req.boundary, args.path_ptr, args.path_len as usize, env) {
        Ok(p) => p,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    let names = match env.fs_list(&path) {
        Ok(Some(n)) => n,
        Ok(None) => return SyscallOutcome::Return(SyscallError::NotFound.as_return()),
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    // Join the entry names with '\n' into a single byte buffer.
    let mut out = alloc::vec::Vec::new();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        out.extend_from_slice(name.as_bytes());
    }
    let n = core::cmp::min(out.len(), args.buf_len as usize);
    if let Err(e) = env.copy_to_user(req.boundary, args.buf_ptr, &out[..n]) {
        return SyscallOutcome::Return(e.as_return());
    }
    SyscallOutcome::Return(n as i64)
}

/// `fs_write`: create/overwrite the file named by the path in the arg block with
/// the data buffer in the arg block; return bytes written or a negative error.
fn fs_write<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let args = match read_rw_args(req.boundary, req.arg0, env) {
        Ok(a) => a,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    let dlen = args.buf_len as usize;
    if dlen > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    let path = match read_path(req.boundary, args.path_ptr, args.path_len as usize, env) {
        Ok(p) => p,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    let mut data = alloc::vec![0u8; dlen];
    if dlen > 0 {
        if let Err(e) = env.copy_from_user(req.boundary, args.buf_ptr, dlen, &mut data) {
            return SyscallOutcome::Return(e.as_return());
        }
    }
    match env.fs_write(&path, &data) {
        Ok(()) => SyscallOutcome::Return(dlen as i64),
        Err(e) => SyscallOutcome::Return(e.as_return()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    struct MockEnv {
        // A single mapped region [base, base+data.len()) in the test boundary.
        base: u64,
        data: RefCell<alloc::vec::Vec<u8>>,
        written: RefCell<alloc::vec::Vec<u8>>,
        clock: u64,
        // A tiny in-memory filesystem: path -> contents; dirs tracked as a set.
        files: RefCell<alloc::collections::BTreeMap<alloc::vec::Vec<u8>, alloc::vec::Vec<u8>>>,
        dirs: RefCell<alloc::collections::BTreeSet<alloc::vec::Vec<u8>>>,
        // A tiny in-memory channel registry exercising the canonical local-channel
        // contract: handle -> (capacity, FIFO of buffered messages). `WouldBlock`
        // models Catch-and-Release back-pressure (full on send, empty on recv).
        channels: RefCell<alloc::collections::BTreeMap<u64, (usize, alloc::collections::VecDeque<alloc::vec::Vec<u8>>)>>,
        next_handle: RefCell<u64>,
        // Each spawn records (entry, arg); returns a fresh lane id.
        spawned: RefCell<alloc::vec::Vec<(u64, u64)>>,
    }

    impl SyscallEnv for MockEnv {
        fn copy_from_user(
            &self,
            _boundary: BoundaryId,
            ptr: u64,
            len: usize,
            out: &mut [u8],
        ) -> Result<(), SyscallError> {
            let data = self.data.borrow();
            let start = ptr
                .checked_sub(self.base)
                .ok_or(SyscallError::BadAddress)? as usize;
            let end = start.checked_add(len).ok_or(SyscallError::InvalidArgument)?;
            if end > data.len() {
                return Err(SyscallError::BadAddress);
            }
            out[..len].copy_from_slice(&data[start..end]);
            Ok(())
        }
        fn console_write(&self, bytes: &[u8]) {
            self.written.borrow_mut().extend_from_slice(bytes);
        }
        fn now_nanos(&self) -> u64 {
            self.clock
        }
        fn copy_to_user(
            &self,
            _boundary: BoundaryId,
            ptr: u64,
            bytes: &[u8],
        ) -> Result<(), SyscallError> {
            let mut data = self.data.borrow_mut();
            let start = ptr
                .checked_sub(self.base)
                .ok_or(SyscallError::BadAddress)? as usize;
            let end = start.checked_add(bytes.len()).ok_or(SyscallError::InvalidArgument)?;
            if end > data.len() {
                return Err(SyscallError::BadAddress);
            }
            data[start..end].copy_from_slice(bytes);
            Ok(())
        }
        fn fs_read(&self, path: &[u8]) -> Result<Option<alloc::vec::Vec<u8>>, SyscallError> {
            Ok(self.files.borrow().get(path).cloned())
        }
        fn fs_write(&self, path: &[u8], data: &[u8]) -> Result<(), SyscallError> {
            self.files.borrow_mut().insert(path.to_vec(), data.to_vec());
            Ok(())
        }
        fn fs_mkdir(&self, path: &[u8]) -> Result<(), SyscallError> {
            self.dirs.borrow_mut().insert(path.to_vec());
            Ok(())
        }
        fn fs_list(
            &self,
            path: &[u8],
        ) -> Result<Option<alloc::vec::Vec<alloc::string::String>>, SyscallError> {
            use alloc::string::String;
            if !self.dirs.borrow().contains(path) {
                return Ok(None);
            }
            let mut prefix = path.to_vec();
            if prefix.last() != Some(&b'/') {
                prefix.push(b'/');
            }
            // Snapshot the keys first so we don't hold the files borrow.
            let keys: alloc::vec::Vec<alloc::vec::Vec<u8>> =
                self.files.borrow().keys().cloned().collect();
            let mut names = alloc::vec::Vec::new();
            for key in keys {
                if key.len() > prefix.len() && key.starts_with(&prefix) {
                    let rest = &key[prefix.len()..];
                    if !rest.contains(&b'/') {
                        names.push(String::from_utf8_lossy(rest).into_owned());
                    }
                }
            }
            Ok(Some(names))
        }
        fn fs_delete(&self, path: &[u8]) -> Result<(), SyscallError> {
            if self.files.borrow_mut().remove(path).is_some() {
                Ok(())
            } else {
                Err(SyscallError::NotFound)
            }
        }
        fn fs_exists(&self, path: &[u8]) -> Result<bool, SyscallError> {
            Ok(self.files.borrow().contains_key(path) || self.dirs.borrow().contains(path))
        }
        fn read_key(&self, _blocking: bool) -> i64 {
            use shared::protocols::syscall::{encode_key, KeyCode, KeyMods};
            // The mock yields a single 'k' keypress.
            encode_key(KeyCode::Char('k'), KeyMods::default())
        }
        fn fill_random(&self, out: &mut [u8]) -> Result<(), SyscallError> {
            // Deterministic non-zero fill for the test.
            for (i, b) in out.iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(31).wrapping_add(7);
            }
            Ok(())
        }
        fn open_channel(
            &self,
            _boundary: BoundaryId,
            capacity: usize,
            _max_message_bytes: usize,
        ) -> Result<u64, SyscallError> {
            let mut next = self.next_handle.borrow_mut();
            let handle = *next;
            *next += 1;
            self.channels
                .borrow_mut()
                .insert(handle, (capacity, alloc::collections::VecDeque::new()));
            Ok(handle)
        }
        fn channel_send(
            &self,
            _boundary: BoundaryId,
            handle: u64,
            data: &[u8],
        ) -> Result<(), SyscallError> {
            let mut chans = self.channels.borrow_mut();
            let (cap, queue) = chans.get_mut(&handle).ok_or(SyscallError::NotFound)?;
            // Full buffer => Catch-and-Release back-pressure, surfaced as WouldBlock.
            if queue.len() >= *cap {
                return Err(SyscallError::WouldBlock);
            }
            queue.push_back(data.to_vec());
            Ok(())
        }
        fn channel_recv(
            &self,
            _boundary: BoundaryId,
            handle: u64,
        ) -> Result<alloc::vec::Vec<u8>, SyscallError> {
            let mut chans = self.channels.borrow_mut();
            let (_cap, queue) = chans.get_mut(&handle).ok_or(SyscallError::NotFound)?;
            // Empty but open => park (WouldBlock); the kernel env distinguishes a
            // closed-drained channel as NotFound. The mock keeps channels open.
            queue.pop_front().ok_or(SyscallError::WouldBlock)
        }
        fn spawn(
            &self,
            _boundary: BoundaryId,
            entry: u64,
            arg: u64,
        ) -> Result<u64, SyscallError> {
            let mut spawned = self.spawned.borrow_mut();
            let lane = spawned.len() as u64;
            spawned.push((entry, arg));
            Ok(lane)
        }
    }

    fn env_with(text: &str) -> MockEnv {
        MockEnv {
            base: 0x4000_0000,
            data: RefCell::new(text.as_bytes().to_vec()),
            written: RefCell::new(alloc::vec::Vec::new()),
            clock: 123_456_789,
            files: RefCell::new(alloc::collections::BTreeMap::new()),
            dirs: RefCell::new(alloc::collections::BTreeSet::new()),
            channels: RefCell::new(alloc::collections::BTreeMap::new()),
            next_handle: RefCell::new(0),
            spawned: RefCell::new(alloc::vec::Vec::new()),
        }
    }

    /// Build an env whose user memory is exactly `bytes` at `base`.
    fn env_bytes(bytes: &[u8]) -> MockEnv {
        let e = env_with("");
        *e.data.borrow_mut() = bytes.to_vec();
        e
    }

    fn req(number: u64, a0: u64, a1: u64) -> SyscallRequest {
        SyscallRequest {
            number,
            arg0: a0,
            arg1: a1,
            arg2: 0,
            boundary: BoundaryId::new(1),
        }
    }

    #[test]
    fn log_writes_user_string() {
        let env = env_with("hello kernel");
        let r = req(Syscall::Log.number(), env.base, 12);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(0));
        assert_eq!(&*env.written.borrow(), b"hello kernel");
    }

    #[test]
    fn log_rejects_out_of_range_pointer() {
        let env = env_with("hi");
        // Pointer past the mapped region.
        let r = req(Syscall::Log.number(), env.base + 100, 2);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::BadAddress.as_return())
        );
        assert!(env.written.borrow().is_empty());
    }

    #[test]
    fn log_rejects_oversize_len() {
        let env = env_with("x");
        let r = req(Syscall::Log.number(), env.base, (MAX_LOG_LEN + 1) as u64);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
    }

    #[test]
    fn unknown_syscall_is_rejected() {
        let env = env_with("");
        let r = req(9999, 0, 0);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::NoSuchCall.as_return())
        );
    }

    #[test]
    fn exit_reports_code() {
        let env = env_with("");
        let r = req(Syscall::Exit.number(), 7, 0);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Exit(7));
    }

    #[test]
    fn yield_reports_yield() {
        let env = env_with("");
        let r = req(Syscall::Yield.number(), 0, 0);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Yield);
    }

    #[test]
    fn now_returns_clock() {
        let env = env_with("");
        let r = req(Syscall::Now.number(), 0, 0);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(123_456_789));
    }

    #[test]
    fn sleep_returns_ok() {
        // The default SyscallEnv::sleep_nanos is a no-op returning Ok, so the
        // dispatcher returns 0. (The kernel env overrides it to actually wait.)
        let env = env_with("");
        let r = req(Syscall::Sleep.number(), 5_000_000, 0);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(0));
    }

    #[test]
    fn read_key_returns_encoded_event() {
        use shared::protocols::syscall::{decode_key, KeyCode, KeyMods};
        let env = env_with("");
        let r = req(Syscall::ReadKey.number(), 1, 0);
        let out = dispatch(&r, &env);
        let SyscallOutcome::Return(v) = out else {
            panic!("expected Return");
        };
        assert_eq!(decode_key(v), Some((KeyCode::Char('k'), KeyMods::default())));
    }

    #[test]
    fn get_random_fills_user_buffer() {
        // User memory is a 16-byte zeroed region at base; GetRandom must fill it.
        let env = env_bytes(&[0u8; 16]);
        let r = SyscallRequest {
            number: Syscall::GetRandom.number(),
            arg0: 0x4000_0000,
            arg1: 16,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(16));
        // The buffer is no longer all-zero (the mock filled it deterministically).
        assert!(env.data.borrow().iter().any(|&b| b != 0));
        // Zero length is rejected.
        let r0 = req(Syscall::GetRandom.number(), 0x4000_0000, 0);
        assert_eq!(
            dispatch(&r0, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
    }

    #[test]
    fn fs_write_then_read_through_syscalls() {
        // User memory layout: [path bytes][data bytes][FsRwArgs block][read buf].
        let path = b"/etc/passwd";
        let data = b"alice:hash";
        let base = 0x4000_0000u64;
        let path_off = 0usize;
        let data_off = path.len();
        let args_off = data_off + data.len();
        let readbuf_off = args_off + FS_RW_ARGS_LEN;
        let readbuf_len = 32usize;

        let write_args = FsRwArgs {
            path_ptr: base + path_off as u64,
            path_len: path.len() as u64,
            buf_ptr: base + data_off as u64,
            buf_len: data.len() as u64,
        };

        let mut mem = alloc::vec::Vec::new();
        mem.extend_from_slice(path);
        mem.extend_from_slice(data);
        mem.extend_from_slice(&write_args.to_bytes());
        mem.extend_from_slice(&[0u8; 32]); // read buffer space
        let env = env_bytes(&mem);

        // fs_write(args_ptr)
        let wr = SyscallRequest {
            number: Syscall::FsWrite.number(),
            arg0: base + args_off as u64,
            arg1: 0,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        assert_eq!(dispatch(&wr, &env), SyscallOutcome::Return(data.len() as i64));
        assert_eq!(env.files.borrow().get(&path.to_vec()).unwrap(), data);

        // fs_read(args_ptr) into the read buffer; reuse an args block pointing at
        // the read buffer.
        let read_args = FsRwArgs {
            path_ptr: base + path_off as u64,
            path_len: path.len() as u64,
            buf_ptr: base + readbuf_off as u64,
            buf_len: readbuf_len as u64,
        };
        // Overwrite the args block in user memory with the read args.
        env.data.borrow_mut()[args_off..args_off + FS_RW_ARGS_LEN]
            .copy_from_slice(&read_args.to_bytes());
        let rr = SyscallRequest {
            number: Syscall::FsRead.number(),
            arg0: base + args_off as u64,
            arg1: 0,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        assert_eq!(dispatch(&rr, &env), SyscallOutcome::Return(data.len() as i64));
        // The data landed in the read buffer.
        let got = &env.data.borrow()[readbuf_off..readbuf_off + data.len()];
        assert_eq!(got, data);
    }

    #[test]
    fn fs_read_missing_is_not_found() {
        // Layout: [path at 0][args block at 64][read buf at 96].
        let path = b"/nope";
        let base = 0x4000_0000u64;
        let args = FsRwArgs {
            path_ptr: base,
            path_len: path.len() as u64,
            buf_ptr: base + 96,
            buf_len: 16,
        };
        let mut mem = alloc::vec::Vec::new();
        mem.extend_from_slice(path);
        mem.resize(64, 0);
        mem.extend_from_slice(&args.to_bytes()); // 64..96
        mem.resize(96 + 16, 0); // read buffer
        let env = env_bytes(&mem);
        let r = SyscallRequest {
            number: Syscall::FsRead.number(),
            arg0: base + 64,
            arg1: 0,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::NotFound.as_return())
        );
    }

    #[test]
    fn fs_list_and_delete_through_syscalls() {
        // Set up a dir /d with two files via the env directly, then exercise the
        // FsList and FsDelete syscalls through dispatch.
        let base = 0x4000_0000u64;
        let env = env_with("");
        env.dirs.borrow_mut().insert(b"/d".to_vec());
        env.files
            .borrow_mut()
            .insert(b"/d/a".to_vec(), b"x".to_vec());
        env.files
            .borrow_mut()
            .insert(b"/d/b".to_vec(), b"y".to_vec());

        // User memory: [path "/d"][FsRwArgs][listing buffer].
        let path = b"/d";
        let args_off = path.len();
        let buf_off = args_off + FS_RW_ARGS_LEN;
        let buf_len = 32usize;
        let list_args = FsRwArgs {
            path_ptr: base,
            path_len: path.len() as u64,
            buf_ptr: base + buf_off as u64,
            buf_len: buf_len as u64,
        };
        let mut mem = alloc::vec::Vec::new();
        mem.extend_from_slice(path);
        mem.extend_from_slice(&list_args.to_bytes());
        mem.extend_from_slice(&alloc::vec![0u8; buf_len]);
        *env.data.borrow_mut() = mem;

        // FsList -> writes "a\nb" (3 bytes) into the buffer.
        let lr = SyscallRequest {
            number: Syscall::FsList.number(),
            arg0: base + args_off as u64,
            arg1: 0,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        let out = dispatch(&lr, &env);
        // The listing is "a" and "b" joined by '\n' = 3 bytes (order from BTreeMap).
        assert_eq!(out, SyscallOutcome::Return(3));
        let written: alloc::vec::Vec<u8> = env.data.borrow()[buf_off..buf_off + 3].to_vec();
        assert_eq!(&written, b"a\nb");

        // FsDelete /d/a -> 0, then listing has only "b".
        let delpath = b"/d/a";
        // Reuse arg0/arg1 path-style for FsDelete: put the path at base again.
        *env.data.borrow_mut() = delpath.to_vec();
        let dr = req(Syscall::FsDelete.number(), base, delpath.len() as u64);
        assert_eq!(dispatch(&dr, &env), SyscallOutcome::Return(0));
        assert!(!env.files.borrow().contains_key(&delpath.to_vec()));
        // Deleting a missing file -> NotFound.
        assert_eq!(
            dispatch(&dr, &env),
            SyscallOutcome::Return(SyscallError::NotFound.as_return())
        );
    }

    #[test]
    fn fs_mkdir_and_exists() {
        let path = b"/home";
        let base = 0x4000_0000u64;
        let env = env_bytes(path);
        let mk = req(Syscall::FsMkdir.number(), base, path.len() as u64);
        assert_eq!(dispatch(&mk, &env), SyscallOutcome::Return(0));
        let ex = req(Syscall::FsExists.number(), base, path.len() as u64);
        assert_eq!(dispatch(&ex, &env), SyscallOutcome::Return(1));
        // A different path does not exist.
        let env2 = env_bytes(b"/other");
        let ex2 = req(Syscall::FsExists.number(), base, 6);
        assert_eq!(dispatch(&ex2, &env2), SyscallOutcome::Return(0));
    }

    #[test]
    fn fs_default_env_denies() {
        // The default trait methods reject access (kernel with no filesystem).
        struct Bare {
            base: u64,
            data: alloc::vec::Vec<u8>,
        }
        impl SyscallEnv for Bare {
            fn copy_from_user(&self, _b: BoundaryId, ptr: u64, len: usize, out: &mut [u8]) -> Result<(), SyscallError> {
                let s = ptr.checked_sub(self.base).ok_or(SyscallError::BadAddress)? as usize;
                let e = s.checked_add(len).ok_or(SyscallError::InvalidArgument)?;
                if e > self.data.len() { return Err(SyscallError::BadAddress); }
                out[..len].copy_from_slice(&self.data[s..e]);
                Ok(())
            }
            fn console_write(&self, _b: &[u8]) {}
            fn now_nanos(&self) -> u64 { 0 }
            fn copy_to_user(&self, _b: BoundaryId, _p: u64, _by: &[u8]) -> Result<(), SyscallError> {
                Ok(())
            }
        }
        let env = Bare { base: 0x4000_0000, data: b"/x".to_vec() };
        let r = req(Syscall::FsExists.number(), 0x4000_0000, 2);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::NotPermitted.as_return())
        );
    }

    // ---- Track 2: channels + spawn ----

    /// Build a request with all three argument registers.
    fn req3(number: u64, a0: u64, a1: u64, a2: u64) -> SyscallRequest {
        SyscallRequest {
            number,
            arg0: a0,
            arg1: a1,
            arg2: a2,
            boundary: BoundaryId::new(1),
        }
    }

    #[test]
    fn open_channel_returns_handle() {
        let env = env_with("");
        // capacity=4 messages, max 64 bytes each.
        let r = req(Syscall::OpenChannel.number(), 4, 64);
        // First handle is 0 (the mock's counter starts at 0).
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(0));
        // A second open yields a distinct handle.
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(1));
        assert_eq!(env.channels.borrow().len(), 2);
    }

    #[test]
    fn open_channel_rejects_zero_capacity_or_size() {
        let env = env_with("");
        let bad_cap = req(Syscall::OpenChannel.number(), 0, 64);
        assert_eq!(
            dispatch(&bad_cap, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
        let bad_size = req(Syscall::OpenChannel.number(), 4, 0);
        assert_eq!(
            dispatch(&bad_size, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
        // Oversized message bound is rejected too.
        let too_big = req(Syscall::OpenChannel.number(), 4, (MAX_FS_IO_LEN + 1) as u64);
        assert_eq!(
            dispatch(&too_big, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
    }

    #[test]
    fn channel_send_then_recv_round_trips() {
        let env = env_with("");
        // Open a channel (handle 0).
        let open = req(Syscall::OpenChannel.number(), 4, 64);
        assert_eq!(dispatch(&open, &env), SyscallOutcome::Return(0));

        // Lay out user memory: 5 payload bytes at base, then a 64-byte recv buffer.
        let payload = b"hello";
        let recv_off = payload.len();
        let recv_cap = 64usize;
        let mut mem = alloc::vec::Vec::new();
        mem.extend_from_slice(payload);
        mem.extend_from_slice(&alloc::vec![0u8; recv_cap]);
        *env.data.borrow_mut() = mem;
        let base = env.base;

        // Send: arg0=handle 0, arg1=ptr to payload, arg2=len.
        let send = req3(Syscall::ChannelSend.number(), 0, base, payload.len() as u64);
        assert_eq!(dispatch(&send, &env), SyscallOutcome::Return(0));

        // Recv: arg0=handle 0, arg1=ptr to recv buffer, arg2=capacity.
        // Returns the byte count written (5).
        let recv = req3(
            Syscall::ChannelRecv.number(),
            0,
            base + recv_off as u64,
            recv_cap as u64,
        );
        assert_eq!(dispatch(&recv, &env), SyscallOutcome::Return(payload.len() as i64));
        let got = env.data.borrow()[recv_off..recv_off + payload.len()].to_vec();
        assert_eq!(&got, payload);
    }

    #[test]
    fn channel_send_full_buffer_would_block() {
        let env = env_with("");
        // Capacity 1.
        let open = req(Syscall::OpenChannel.number(), 1, 64);
        assert_eq!(dispatch(&open, &env), SyscallOutcome::Return(0));
        let payload = b"x";
        *env.data.borrow_mut() = payload.to_vec();
        let base = env.base;
        let send = req3(Syscall::ChannelSend.number(), 0, base, 1);
        // First send fits.
        assert_eq!(dispatch(&send, &env), SyscallOutcome::Return(0));
        // Second send hits back-pressure -> WouldBlock (Catch-and-Release parks the lane).
        assert_eq!(
            dispatch(&send, &env),
            SyscallOutcome::Return(SyscallError::WouldBlock.as_return())
        );
    }

    #[test]
    fn channel_recv_empty_would_block() {
        let env = env_with("");
        let open = req(Syscall::OpenChannel.number(), 4, 64);
        assert_eq!(dispatch(&open, &env), SyscallOutcome::Return(0));
        // Recv on an empty (but open) channel -> WouldBlock.
        *env.data.borrow_mut() = alloc::vec![0u8; 64];
        let base = env.base;
        let recv = req3(Syscall::ChannelRecv.number(), 0, base, 64);
        assert_eq!(
            dispatch(&recv, &env),
            SyscallOutcome::Return(SyscallError::WouldBlock.as_return())
        );
    }

    #[test]
    fn channel_ops_on_unknown_handle_not_found() {
        let env = env_with("");
        *env.data.borrow_mut() = alloc::vec![0u8; 64];
        let base = env.base;
        // No channel opened; handle 7 does not exist.
        let send = req3(Syscall::ChannelSend.number(), 7, base, 1);
        assert_eq!(
            dispatch(&send, &env),
            SyscallOutcome::Return(SyscallError::NotFound.as_return())
        );
        let recv = req3(Syscall::ChannelRecv.number(), 7, base, 64);
        assert_eq!(
            dispatch(&recv, &env),
            SyscallOutcome::Return(SyscallError::NotFound.as_return())
        );
    }

    #[test]
    fn spawn_records_entry_and_arg() {
        let env = env_with("");
        // arg0 = entry pointer, arg1 = argument word.
        let r = req(Syscall::Spawn.number(), 0xdead_beef, 42);
        // First lane id is 0.
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(0));
        // Second spawn -> lane id 1.
        let r2 = req(Syscall::Spawn.number(), 0xfeed_face, 7);
        assert_eq!(dispatch(&r2, &env), SyscallOutcome::Return(1));
        let spawned = env.spawned.borrow();
        assert_eq!(spawned.len(), 2);
        assert_eq!(spawned[0], (0xdead_beef, 42));
        assert_eq!(spawned[1], (0xfeed_face, 7));
    }

    #[test]
    fn track2_default_env_denies() {
        // The default trait methods reject channel/spawn (a kernel env with no
        // IPC/scheduling surface). Real success requires an env override.
        struct Bare;
        impl SyscallEnv for Bare {
            fn copy_from_user(&self, _b: BoundaryId, _p: u64, _l: usize, _o: &mut [u8]) -> Result<(), SyscallError> {
                Ok(())
            }
            fn console_write(&self, _b: &[u8]) {}
            fn now_nanos(&self) -> u64 { 0 }
            fn copy_to_user(&self, _b: BoundaryId, _p: u64, _by: &[u8]) -> Result<(), SyscallError> {
                Ok(())
            }
        }
        let env = Bare;
        let denied = SyscallError::NotPermitted.as_return();
        assert_eq!(
            dispatch(&req(Syscall::OpenChannel.number(), 4, 64), &env),
            SyscallOutcome::Return(denied)
        );
        assert_eq!(
            dispatch(&req3(Syscall::ChannelSend.number(), 0, 0, 0), &env),
            SyscallOutcome::Return(denied)
        );
        assert_eq!(
            dispatch(&req3(Syscall::ChannelRecv.number(), 0, 0, 0), &env),
            SyscallOutcome::Return(denied)
        );
        assert_eq!(
            dispatch(&req(Syscall::Spawn.number(), 0, 0), &env),
            SyscallOutcome::Return(denied)
        );
    }

    // ---- Handshake syscall dispatch (decode/validate + wire round-trip) -------

    #[test]
    fn channel_terms_wire_round_trips() {
        use shared::protocols::ipc::{ChannelDirection, ChannelTerms, ChannelTermsWire};
        let terms = ChannelTerms::new("telemetry", ChannelDirection::Bidirectional, 256, 8).unwrap();
        let wire = ChannelTermsWire::from_terms(&terms);
        let bytes = wire.to_bytes();
        let back = ChannelTermsWire::from_bytes(&bytes).to_terms().unwrap();
        assert_eq!(back, terms, "terms survive wire encode/decode unchanged");
    }

    #[test]
    fn channel_request_wire_round_trips() {
        use shared::protocols::ipc::{
            ChannelDirection, ChannelRequestWire, ChannelTerms, ChannelTermsWire,
        };
        let terms = ChannelTerms::new("rpc", ChannelDirection::RequesterToReceiver, 64, 2).unwrap();
        let wire = ChannelRequestWire {
            requester: 0x100,
            terms: ChannelTermsWire::from_terms(&terms),
        };
        let bytes = wire.to_bytes();
        let back = ChannelRequestWire::from_bytes(&bytes);
        assert_eq!(back.requester, 0x100);
        assert_eq!(back.terms.to_terms().unwrap(), terms);
    }

    #[test]
    fn request_channel_to_self_is_rejected() {
        use shared::protocols::ipc::{ChannelDirection, ChannelTerms, ChannelTermsWire};
        // Build a valid terms buffer in the mock's mapped region, then target the
        // CALLER's own boundary (1) — dispatch must reject this as InvalidArgument
        // (cross-boundary handshake is between DISTINCT boundaries).
        let terms = ChannelTerms::new("x", ChannelDirection::Bidirectional, 32, 1).unwrap();
        let bytes = ChannelTermsWire::from_terms(&terms).to_bytes();
        let mut env = env_with("");
        env.base = 0x1000;
        *env.data.borrow_mut() = bytes.to_vec();
        let r = req3(
            Syscall::RequestChannel.number(),
            1, /* target == caller boundary (1) */
            0x1000,
            bytes.len() as u64,
        );
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
    }

    #[test]
    fn request_channel_short_terms_buffer_is_rejected() {
        // terms_len below the wire length must be rejected before any copy.
        let env = env_with("");
        let r = req3(Syscall::RequestChannel.number(), 2, 0x1000, 4);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
    }

    #[test]
    fn handshake_calls_default_to_not_permitted_without_ipc() {
        // The mock env uses trait defaults for the handshake methods, so a valid
        // cross-boundary request (distinct target, full buffer) reaches the env
        // and reports NotPermitted — confirming dispatch wiring reaches the env.
        use shared::protocols::ipc::{ChannelDirection, ChannelTerms, ChannelTermsWire};
        let terms = ChannelTerms::new("y", ChannelDirection::Bidirectional, 32, 1).unwrap();
        let bytes = ChannelTermsWire::from_terms(&terms).to_bytes();
        let mut env = env_with("");
        env.base = 0x1000;
        *env.data.borrow_mut() = bytes.to_vec();
        let r = req3(Syscall::RequestChannel.number(), 2, 0x1000, bytes.len() as u64);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::NotPermitted.as_return())
        );
        // Accept/Reject/Outcome with default env also report NotPermitted.
        assert_eq!(
            dispatch(&req(Syscall::AcceptChannel.number(), 0, 0), &env),
            SyscallOutcome::Return(SyscallError::NotPermitted.as_return())
        );
        assert_eq!(
            dispatch(&req(Syscall::PollChannelOutcome.number(), 0, 0), &env),
            SyscallOutcome::Return(SyscallError::NotPermitted.as_return())
        );
    }

    // ---- Lattice net syscall dispatch (decode/validate + reaches env) ---------

    #[test]
    fn net_syscalls_reach_env_default_not_permitted() {
        // MockEnv uses the trait defaults for the net methods, so a valid call
        // (gate in range) reaches the env and reports NotPermitted — confirming
        // dispatch wiring for each net syscall.
        let env = env_with("");
        for call in [
            Syscall::GateBind,
            Syscall::GateConnect,
            Syscall::GateAccept,
            Syscall::WardenSet,
            Syscall::GateProbe,
        ] {
            assert_eq!(
                dispatch(&req(call.number(), 80, 1), &env),
                SyscallOutcome::Return(SyscallError::NotPermitted.as_return()),
                "{call:?} should reach env and report NotPermitted",
            );
        }
    }

    #[test]
    fn gate_out_of_range_is_invalid_argument() {
        // A gate number above u16::MAX is rejected by dispatch BEFORE the env.
        let env = env_with("");
        let big = (u16::MAX as u64) + 1;
        for call in [Syscall::GateBind, Syscall::GateConnect, Syscall::GateProbe] {
            assert_eq!(
                dispatch(&req(call.number(), big, 0), &env),
                SyscallOutcome::Return(SyscallError::InvalidArgument.as_return()),
                "{call:?} with out-of-range gate should be InvalidArgument",
            );
        }
    }

    #[test]
    fn link_close_reaches_env() {
        // LinkClose takes a scalar handle; default env reports NotPermitted.
        let env = env_with("");
        assert_eq!(
            dispatch(&req(Syscall::LinkClose.number(), 0, 0), &env),
            SyscallOutcome::Return(SyscallError::NotPermitted.as_return())
        );
    }
}
