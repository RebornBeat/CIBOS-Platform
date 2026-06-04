//! # Courier — CIBOS messaging
//!
//! Person-to-person messaging over the Lattice. Each participant binds a Gate as
//! their **inbox**; sending a message opens a [`Link`] to the recipient's Gate,
//! delivers one framed message, and closes. The recipient drains its inbox
//! listener to collect delivered messages.
//!
//! A message is framed as a single Link message: a UTF-8 head line
//! `FROM <sender>` then `\n` then the body bytes — so one delivery is one
//! message, no length bookkeeping.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::{Gate, Lattice, Listener, NetError};

/// A delivered message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The sender's name.
    pub from: String,
    /// The message body.
    pub body: String,
}

/// Encode a message for delivery over a Link.
#[must_use]
pub fn encode(from: &str, body: &str) -> Vec<u8> {
    format!("FROM {from}\n{body}").into_bytes()
}

/// Decode a delivered message frame.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<Message> {
    let nl = bytes.iter().position(|&b| b == b'\n')?;
    let head = std::str::from_utf8(&bytes[..nl]).ok()?;
    let from = head.strip_prefix("FROM ")?.to_string();
    let body = String::from_utf8_lossy(&bytes[nl + 1..]).to_string();
    Some(Message { from, body })
}

/// A participant's inbox: the bound Gate plus collected messages.
pub struct Inbox {
    listener: Listener,
    messages: Vec<Message>,
}

impl Inbox {
    /// The Gate this inbox listens on.
    #[must_use]
    pub fn gate(&self) -> Gate {
        self.listener.gate()
    }

    /// Collect all delivered-but-unreceived messages from the Lattice into the
    /// inbox. Returns how many were collected.
    pub fn receive_pending(&mut self) -> usize {
        let mut got = 0;
        while let Some(link) = self.listener.accept() {
            if let Ok(Some(bytes)) = link.try_recv() {
                if let Some(msg) = decode(&bytes) {
                    self.messages.push(msg);
                    got += 1;
                }
            }
            link.close();
        }
        got
    }

    /// All messages collected so far.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The number of messages collected.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the inbox is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Bind `gate` as an inbox on `net`.
///
/// # Errors
/// Propagates [`NetError`] (e.g. the Gate is blocked or already bound).
pub fn open_inbox(net: &Lattice, gate: Gate) -> Result<Inbox, NetError> {
    let listener = net.bind(gate)?;
    Ok(Inbox {
        listener,
        messages: Vec::new(),
    })
}

/// Send a message from `from` to the participant listening on `to_gate`.
///
/// # Errors
/// [`NetError::Refused`] if no inbox is bound there, [`NetError::Blocked`] if
/// the Warden denies the Gate.
pub fn send(net: &Lattice, to_gate: Gate, from: &str, body: &str) -> Result<(), NetError> {
    let link = net.connect(to_gate)?;
    link.send(&encode(from, body))?;
    link.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let bytes = encode("alice", "hello bob");
        let msg = decode(&bytes).unwrap();
        assert_eq!(msg.from, "alice");
        assert_eq!(msg.body, "hello bob");
        // Body may contain newlines.
        let m2 = decode(&encode("bob", "line1\nline2")).unwrap();
        assert_eq!(m2.body, "line1\nline2");
    }

    #[test]
    fn two_parties_exchange_messages() {
        let net = Lattice::new();
        let mut alice = open_inbox(&net, 5000).unwrap();
        let mut bob = open_inbox(&net, 5001).unwrap();

        // Alice messages Bob twice.
        send(&net, bob.gate(), "alice", "hi bob").unwrap();
        send(&net, bob.gate(), "alice", "you there?").unwrap();
        assert_eq!(bob.receive_pending(), 2);
        assert_eq!(bob.messages()[0].body, "hi bob");
        assert_eq!(bob.messages()[1].from, "alice");

        // Bob replies.
        send(&net, alice.gate(), "bob", "hey alice").unwrap();
        assert_eq!(alice.receive_pending(), 1);
        assert_eq!(alice.messages()[0].body, "hey alice");
    }

    #[test]
    fn message_to_unbound_gate_is_refused() {
        let net = Lattice::new();
        assert_eq!(send(&net, 9999, "x", "y").err(), Some(NetError::Refused));
    }

    #[test]
    fn warden_can_block_messaging() {
        let net = Lattice::new();
        let _inbox = open_inbox(&net, 5002).unwrap();
        net.warden_deny(5002);
        // Even with an inbox bound, a blocked Gate refuses delivery.
        assert_eq!(send(&net, 5002, "x", "y").err(), Some(NetError::Blocked));
    }
}
