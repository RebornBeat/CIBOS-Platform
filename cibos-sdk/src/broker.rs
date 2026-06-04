//! Cross-container channel rendezvous.
//!
//! All containers on one host share a [`ChannelBroker`]. A requester enqueues a
//! [`PendingRequest`] for a target [`BoundaryId`] and rings the target's
//! doorbell; the target drains its mailbox (waiting on the doorbell when empty),
//! and accepts or rejects, signalling the requester over the request's
//! `accept_signal`. The broker is type-agnostic: the typed connection state
//! travels as an opaque `Box<dyn Any>` that the channel layer downcasts on
//! accept.
//!
//! On the single-threaded cooperative host this is race-free: a target checks
//! its mailbox and only then waits on the doorbell, with no await in between, so
//! a request enqueued concurrently is either seen immediately or wakes the
//! waiting target.

use cibos_kernel::Channel as KernelChannel;
use shared::{BoundaryId, LaneId};
use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

/// A pending cross-container channel request awaiting the target's decision.
pub(crate) struct PendingRequest {
    /// The requesting container.
    pub(crate) source: BoundaryId,
    /// Informational channel purpose.
    pub(crate) purpose: &'static str,
    /// Type-erased shared connection state (the channel layer owns the type).
    pub(crate) payload: Box<dyn Any>,
    /// The requester waits on this; accept sends `[1]`, reject sends `[0]`.
    pub(crate) accept_signal: KernelChannel,
}

/// Shared cross-container rendezvous for every container on one host.
pub(crate) struct ChannelBroker {
    inbox: RefCell<BTreeMap<u64, VecDeque<PendingRequest>>>,
    doorbells: RefCell<BTreeMap<u64, KernelChannel>>,
}

impl ChannelBroker {
    /// A fresh, empty broker.
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(ChannelBroker {
            inbox: RefCell::new(BTreeMap::new()),
            doorbells: RefCell::new(BTreeMap::new()),
        })
    }

    /// Enqueue a request for `target` and ring its doorbell if it is waiting.
    /// `ringer` is the requesting lane (for the best-effort wakeup send).
    pub(crate) fn submit(&self, target: BoundaryId, request: PendingRequest, ringer: LaneId) {
        self.inbox
            .borrow_mut()
            .entry(target.0)
            .or_default()
            .push_back(request);
        if let Some(doorbell) = self.doorbells.borrow().get(&target.0) {
            // Best-effort nudge; the mailbox is the source of truth, so a full
            // doorbell buffer (target already pending a wake) is harmless.
            let _ = doorbell.try_send(ringer, &[1u8]);
        }
    }

    /// The doorbell a container waits on for incoming requests, creating it via
    /// `make` on first use.
    pub(crate) fn doorbell(
        &self,
        target: BoundaryId,
        make: impl FnOnce() -> KernelChannel,
    ) -> KernelChannel {
        self.doorbells
            .borrow_mut()
            .entry(target.0)
            .or_insert_with(make)
            .clone()
    }

    /// Take the next pending request for `target`, if any.
    pub(crate) fn take(&self, target: BoundaryId) -> Option<PendingRequest> {
        self.inbox
            .borrow_mut()
            .get_mut(&target.0)
            .and_then(VecDeque::pop_front)
    }
}
