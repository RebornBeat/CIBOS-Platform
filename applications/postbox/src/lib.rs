//! # Postbox — CIBOS email
//!
//! Email, distinct from Courier's instant messaging: structured mail with
//! `To`/`From`/`Subject`/`Body`, delivered to a recipient's **mailbox Gate** and
//! stored for later reading. A mailbox keeps received mail (and tracks
//! read/unread); composing delivers one mail over a [`Link`].
//!
//! Wire format — UTF-8 headers, a blank line, then the body:
//!
//! ```text
//! TO alice
//! FROM bob
//! SUBJECT lunch?
//!
//! Want to grab lunch tomorrow?
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::{Gate, Lattice, Listener, NetError};

/// A mail item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    /// Recipient name.
    pub to: String,
    /// Sender name.
    pub from: String,
    /// Subject line.
    pub subject: String,
    /// Body text.
    pub body: String,
    /// Whether it has been read.
    pub read: bool,
}

impl Mail {
    /// Compose a new (unread) mail.
    #[must_use]
    pub fn compose(to: &str, from: &str, subject: &str, body: &str) -> Self {
        Mail {
            to: to.to_string(),
            from: from.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
            read: false,
        }
    }
}

/// Encode mail for delivery.
#[must_use]
pub fn encode(mail: &Mail) -> Vec<u8> {
    format!(
        "TO {}\nFROM {}\nSUBJECT {}\n\n{}",
        mail.to, mail.from, mail.subject, mail.body
    )
    .into_bytes()
}

/// Decode a delivered mail frame.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<Mail> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (headers, body) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut to = None;
    let mut from = None;
    let mut subject = String::new();
    for line in headers.lines() {
        if let Some(v) = line.strip_prefix("TO ") {
            to = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("FROM ") {
            from = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("SUBJECT ") {
            subject = v.to_string();
        }
    }
    Some(Mail {
        to: to?,
        from: from?,
        subject,
        body: body.to_string(),
        read: false,
    })
}

/// A mailbox bound to a Gate, holding received mail.
pub struct Mailbox {
    listener: Listener,
    mail: Vec<Mail>,
}

impl Mailbox {
    /// The mailbox's Gate.
    #[must_use]
    pub fn gate(&self) -> Gate {
        self.listener.gate()
    }

    /// Collect delivered mail into the mailbox. Returns how many arrived.
    pub fn receive_pending(&mut self) -> usize {
        let mut got = 0;
        while let Some(link) = self.listener.accept() {
            if let Ok(Some(bytes)) = link.try_recv() {
                if let Some(mail) = decode(&bytes) {
                    self.mail.push(mail);
                    got += 1;
                }
            }
            link.close();
        }
        got
    }

    /// All mail, newest last.
    #[must_use]
    pub fn all(&self) -> &[Mail] {
        &self.mail
    }

    /// Number of unread messages.
    #[must_use]
    pub fn unread_count(&self) -> usize {
        self.mail.iter().filter(|m| !m.read).count()
    }

    /// Read (and mark read) the mail at `index`, returning a copy.
    pub fn read(&mut self, index: usize) -> Option<Mail> {
        let mail = self.mail.get_mut(index)?;
        mail.read = true;
        Some(mail.clone())
    }

    /// Subject lines with an unread marker, for an inbox listing.
    #[must_use]
    pub fn inbox_listing(&self) -> Vec<String> {
        self.mail
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    "{i}: {}{} — {}",
                    if m.read { " " } else { "*" },
                    m.from,
                    m.subject
                )
            })
            .collect()
    }
}

/// Open a mailbox on `gate`.
///
/// # Errors
/// Propagates [`NetError`] from binding the Gate.
pub fn open_mailbox(net: &Lattice, gate: Gate) -> Result<Mailbox, NetError> {
    Ok(Mailbox {
        listener: net.bind(gate)?,
        mail: Vec::new(),
    })
}

/// Send `mail` to the mailbox listening on `to_gate`.
///
/// # Errors
/// [`NetError::Refused`] if no mailbox is bound, [`NetError::Blocked`] if denied.
pub fn send(net: &Lattice, to_gate: Gate, mail: &Mail) -> Result<(), NetError> {
    let link = net.connect(to_gate)?;
    link.send(&encode(mail))?;
    link.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_round_trip() {
        let mail = Mail::compose("alice", "bob", "lunch?", "tomorrow at noon?\nlet me know");
        let back = decode(&encode(&mail)).unwrap();
        assert_eq!(back.to, "alice");
        assert_eq!(back.from, "bob");
        assert_eq!(back.subject, "lunch?");
        assert_eq!(back.body, "tomorrow at noon?\nlet me know");
    }

    #[test]
    fn deliver_and_read() {
        let net = Lattice::new();
        let mut alice = open_mailbox(&net, 25).unwrap();

        send(&net, alice.gate(), &Mail::compose("alice", "bob", "hi", "first")).unwrap();
        send(&net, alice.gate(), &Mail::compose("alice", "carol", "re: hi", "second")).unwrap();
        assert_eq!(alice.receive_pending(), 2);
        assert_eq!(alice.unread_count(), 2);

        let listing = alice.inbox_listing();
        assert!(listing[0].contains("bob"));
        assert!(listing[0].starts_with("0: *")); // unread marker

        let opened = alice.read(0).unwrap();
        assert_eq!(opened.body, "first");
        assert_eq!(alice.unread_count(), 1); // one now read
    }

    #[test]
    fn send_to_unbound_is_refused() {
        let net = Lattice::new();
        let mail = Mail::compose("x", "y", "s", "b");
        assert_eq!(send(&net, 25, &mail).err(), Some(NetError::Refused));
    }
}
