//! Ring-3 Lattice networking: thin wrappers over the Gate/Link/Warden syscalls
//! (`GateBind`/`GateConnect`/`GateAccept`/`LinkSend`/`LinkRecv`/`LinkClose`/
//! `WardenSet`/`GateProbe`, numbers 23-30).
//!
//! The Lattice vocabulary matches the SDK (`cibos-sdk::net`) so apps written
//! against either see the same surface: a **Gate** is a `u16` port; **bind**ing a
//! Gate makes the caller its listener; **connect**ing opens a **Link** (a
//! bidirectional byte stream) to whoever is bound; the listener **accept**s
//! pending connects. The **Warden** is the firewall (deny is total — blocks bind
//! AND connect). A Link is backed by the canonical kernel `Channel`, so bytes
//! cross boundaries THROUGH the kernel.
//!
//! This is loopback today; a NIC transport will back the SAME surface later
//! (apps unchanged) — see NETWORKING.md.

use crate::syscall::{decode, syscall3};
use shared::protocols::syscall::{Syscall, SyscallError};

/// A Lattice Gate (port).
pub type Gate = u16;

/// The result of probing a Gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GateState {
    /// Allowed by the Warden, but no listener is bound.
    Closed,
    /// A listener is bound and accepting connects.
    Open,
    /// The Warden denies this Gate.
    Blocked,
}

/// A bound listener: the caller owns `gate` and can `accept` connects on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Listener(Gate);

/// A connected Link: a bidirectional byte stream to the peer boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Link(u64);

impl Listener {
    /// The Gate this listener is bound to.
    #[must_use]
    pub fn gate(self) -> Gate {
        self.0
    }

    /// Accept the next pending connect on this Gate, returning a [`Link`].
    /// `Ok(None)` if no connect is pending (would block).
    pub fn accept(self) -> Result<Option<Link>, SyscallError> {
        // SAFETY: GateAccept takes a scalar gate and no pointers.
        let ret = unsafe { syscall3(Syscall::GateAccept, u64::from(self.0), 0, 0) };
        match decode(ret) {
            Ok(h) => Ok(Some(Link(h as u64))),
            Err(SyscallError::WouldBlock) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Link {
    /// Send `data` on the Link. `Ok(false)` if the buffer is full (would block).
    pub fn send(self, data: &[u8]) -> Result<bool, SyscallError> {
        // SAFETY: `data` is a valid readable buffer of `data.len()`; the kernel
        // validates the pointer against the calling boundary and copies it.
        let ret = unsafe {
            syscall3(
                Syscall::LinkSend,
                self.0,
                data.as_ptr() as u64,
                data.len() as u64,
            )
        };
        match decode(ret) {
            Ok(_) => Ok(true),
            Err(SyscallError::WouldBlock) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Receive a message into `buf`. Returns `Ok(Some(n))` with the byte count,
    /// or `Ok(None)` if no message is ready (would block).
    pub fn recv(self, buf: &mut [u8]) -> Result<Option<usize>, SyscallError> {
        // SAFETY: `buf` is a valid writable buffer of `buf.len()`; the kernel
        // copies at most that many bytes into it.
        let ret = unsafe {
            syscall3(
                Syscall::LinkRecv,
                self.0,
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
            )
        };
        match decode(ret) {
            Ok(n) => Ok(Some(n as usize)),
            Err(SyscallError::WouldBlock) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Close the Link.
    pub fn close(self) -> Result<(), SyscallError> {
        // SAFETY: LinkClose takes a scalar handle and no pointers.
        let ret = unsafe { syscall3(Syscall::LinkClose, self.0, 0, 0) };
        decode(ret).map(|_| ())
    }
}

/// Bind `gate` as a listener (the caller becomes its owner).
pub fn bind(gate: Gate) -> Result<Listener, SyscallError> {
    // SAFETY: GateBind takes a scalar gate and no pointers.
    let ret = unsafe { syscall3(Syscall::GateBind, u64::from(gate), 0, 0) };
    decode(ret).map(|_| Listener(gate))
}

/// Open a [`Link`] to whatever boundary is bound on `gate`.
pub fn connect(gate: Gate) -> Result<Link, SyscallError> {
    // SAFETY: GateConnect takes a scalar gate and no pointers.
    let ret = unsafe { syscall3(Syscall::GateConnect, u64::from(gate), 0, 0) };
    decode(ret).map(|h| Link(h as u64))
}

/// Set the Warden policy for `gate`: `allow == false` denies it totally (blocks
/// both bind and connect).
pub fn warden_set(gate: Gate, allow: bool) -> Result<(), SyscallError> {
    // SAFETY: WardenSet takes scalar args and no pointers.
    let ret = unsafe { syscall3(Syscall::WardenSet, u64::from(gate), u64::from(allow), 0) };
    decode(ret).map(|_| ())
}

/// Probe `gate`: Open (bound), Closed (allowed, unbound), or Blocked (Warden).
pub fn probe(gate: Gate) -> Result<GateState, SyscallError> {
    // SAFETY: GateProbe takes a scalar gate and no pointers.
    let ret = unsafe { syscall3(Syscall::GateProbe, u64::from(gate), 0, 0) };
    decode(ret).map(|s| match s {
        1 => GateState::Open,
        2 => GateState::Blocked,
        _ => GateState::Closed,
    })
}
