//! # Lens — the CIBOS fetch client
//!
//! Lens is the "browser": it opens a [`Link`] to a [`Vane`](vane) [`Gate`],
//! issues a [`Hail`](web_protocol) request, and reads the response. Because the
//! Lattice is synchronous in-memory, a caller drives a request by opening a
//! link, sending, letting the server serve, then reading the response — the
//! exact sequence the runnable `web-demo` binary performs.
//!
//! [`render`] turns a response into displayable text — the minimal "rendering"
//! a text browser does.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::{Gate, LaneId, Lattice, Link, NetError};
use std::fmt;
use web_protocol::{decode_response, encode_request, ProtocolError, Request, Response};

/// Errors fetching over the Lattice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    /// A network-layer error (refused, blocked, closed).
    Net(NetError),
    /// The response could not be decoded.
    Protocol(ProtocolError),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Net(e) => write!(f, "network: {e}"),
            FetchError::Protocol(e) => write!(f, "protocol: {e}"),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<NetError> for FetchError {
    fn from(e: NetError) -> Self {
        FetchError::Net(e)
    }
}

/// Open a Link to a Vane Gate.
///
/// # Errors
/// [`NetError`] if the connection is refused or blocked.
pub fn open(net: &Lattice, gate: Gate) -> Result<Link, NetError> {
    net.connect(gate)
}

/// Send a request over an open Link.
///
/// # Errors
/// [`NetError::LinkClosed`] if the peer has gone away.
pub fn request(link: &Link, req: &Request) -> Result<(), NetError> {
    link.send(&encode_request(req))
}

/// Try to read a response from a Link: `Ok(Some(_))` if one has arrived,
/// `Ok(None)` if not yet.
///
/// # Errors
/// [`FetchError`] if the link closed with no response, or the bytes could not
/// be decoded.
pub fn read_response(link: &Link) -> Result<Option<Response>, FetchError> {
    match link.try_recv() {
        Ok(Some(bytes)) => decode_response(&bytes)
            .map(Some)
            .map_err(FetchError::Protocol),
        Ok(None) => Ok(None),
        Err(e) => Err(FetchError::Net(e)),
    }
}

/// Render a response as displayable text: a status line followed by the body
/// interpreted as UTF-8 (lossily).
#[must_use]
pub fn render(resp: &Response) -> String {
    format!(
        "[{} {}]\n{}",
        resp.status,
        resp.reason,
        String::from_utf8_lossy(&resp.body)
    )
}

/// Fetch a page over a **live** Vane daemon from inside an async task: connect
/// (ringing the daemon's doorbell so it wakes), send a `Fetch`, and await the
/// response. Returns `None` if the connection is refused/blocked or the link
/// closes without a valid response.
pub async fn browse(net: &Lattice, gate: Gate, path: &str, lane: LaneId) -> Option<Response> {
    let link = net.connect_signaling(lane, gate).ok()?;
    request(&link, &Request::fetch(path)).ok()?;
    loop {
        match link.try_recv() {
            Ok(Some(bytes)) => return decode_response(&bytes).ok(),
            Ok(None) => cibos_sdk::yield_now().await,
            Err(_) => return None,
        }
    }
}

/// Parse links from a page body. A link is a line `LINK <path> <label>`; the
/// label is optional. Returns `(path, label)` pairs in document order.
#[must_use]
pub fn parse_links(body: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(body);
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("LINK ")?;
            let mut it = rest.splitn(2, char::is_whitespace);
            let path = it.next()?.to_string();
            let label = it.next().unwrap_or("").to_string();
            Some((path, label))
        })
        .collect()
}

/// Navigation history with back/forward, like a browser's.
#[derive(Debug, Default, Clone)]
pub struct History {
    entries: Vec<String>,
    /// Index of the current entry (when non-empty).
    cursor: usize,
}

impl History {
    /// Empty history.
    #[must_use]
    pub fn new() -> Self {
        History::default()
    }

    /// Visit a new path. Any forward entries are discarded (new branch).
    pub fn visit(&mut self, path: &str) {
        if !self.entries.is_empty() {
            self.entries.truncate(self.cursor + 1);
        }
        self.entries.push(path.to_string());
        self.cursor = self.entries.len() - 1;
    }

    /// The current path, if any.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.cursor).map(String::as_str)
    }

    /// Go back; returns the new current path.
    pub fn back(&mut self) -> Option<&str> {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.current()
    }

    /// Go forward; returns the new current path.
    pub fn forward(&mut self) -> Option<&str> {
        if self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
        }
        self.current()
    }

    /// Whether back is possible.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    /// Whether forward is possible.
    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// All visited paths in order.
    #[must_use]
    pub fn entries(&self) -> &[String] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cibos_sdk::Filesystem;
    use web_protocol::status;

    #[test]
    fn end_to_end_fetch_through_vane() {
        let net = Lattice::new();
        let fs = Filesystem::new();
        fs.write("/www/index.html", b"<h1>Hi</h1>");
        let listener = vane::bind(&net, 80).unwrap();

        // Open, request, let the server serve, read the response.
        let link = open(&net, 80).unwrap();
        request(&link, &Request::fetch("/index.html")).unwrap();
        vane::serve_pending(&listener, &fs, "/www");
        let resp = read_response(&link).unwrap().expect("a response");

        assert_eq!(resp.status, status::OK);
        assert_eq!(render(&resp), "[200 OK]\n<h1>Hi</h1>");
    }

    #[test]
    fn push_then_fetch_through_vane() {
        let net = Lattice::new();
        let fs = Filesystem::new();
        let listener = vane::bind(&net, 80).unwrap();

        let link = open(&net, 80).unwrap();
        request(&link, &Request::push("/saved", b"payload".to_vec())).unwrap();
        vane::serve_pending(&listener, &fs, "/www");
        let stored = read_response(&link).unwrap().unwrap();
        assert_eq!(stored.status, status::STORED);

        let link = open(&net, 80).unwrap();
        request(&link, &Request::fetch("/saved")).unwrap();
        vane::serve_pending(&listener, &fs, "/www");
        let got = read_response(&link).unwrap().unwrap();
        assert_eq!(got.body, b"payload");
    }

    #[test]
    fn fetch_blocked_gate_is_refused() {
        let net = Lattice::new();
        net.warden_deny(80);
        assert_eq!(open(&net, 80).err(), Some(NetError::Blocked));
    }

    #[test]
    fn parses_links_from_page() {
        let body = b"Welcome\nLINK /about About Us\nLINK /contact\nnot a link";
        let links = parse_links(body);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0], ("/about".to_string(), "About Us".to_string()));
        assert_eq!(links[1], ("/contact".to_string(), String::new()));
    }

    #[test]
    fn history_back_forward_and_branch() {
        let mut h = History::new();
        h.visit("/home");
        h.visit("/about");
        h.visit("/contact");
        assert_eq!(h.current(), Some("/contact"));
        assert_eq!(h.back(), Some("/about"));
        assert_eq!(h.back(), Some("/home"));
        assert!(!h.can_go_back());
        assert_eq!(h.forward(), Some("/about"));
        // Visiting from a back position branches: forward history is dropped.
        h.visit("/news");
        assert_eq!(h.current(), Some("/news"));
        assert!(!h.can_go_forward());
        assert_eq!(h.entries(), &["/home", "/about", "/news"]);
    }
}

#[cfg(test)]
mod browser_tests {
    use super::*;
    use cibos_sdk::{
        AppHost, Application, CibosProfile, Filesystem, ResourceLimits, System, WeightClass,
    };

    /// A linked site served by a live Vane daemon, plus a browser task that
    /// fetches the home page, follows a link, and navigates back.
    struct Site;

    impl Application for Site {
        fn name(&self) -> &str {
            "site"
        }
        fn start(&self, system: System) {
            let net = system.lattice();
            let fs = system.filesystem();
            fs.write("/site/home", b"Home\nLINK /about About\nLINK /contact Contact");
            fs.write("/site/about", b"About\nLINK /home Home");
            fs.write("/site/contact", b"Contact info");

            // Live daemon.
            let daemon = vane::listen(&net, &system, 80).unwrap();
            let server_fs = fs.clone();
            system.spawn_with_lane(WeightClass::System, move |lane| async move {
                daemon.serve_forever(lane, &server_fs, "/site").await;
            });

            // Browser session.
            let cnet = net.clone();
            let out = fs.clone();
            system.spawn_with_lane(WeightClass::User, move |lane| async move {
                let mut history = History::new();

                let home = browse(&cnet, 80, "/home", lane).await.unwrap();
                history.visit("/home");
                let links = parse_links(&home.body);
                out.write("/out/home", &home.body);
                out.write(
                    "/out/links",
                    links
                        .iter()
                        .map(|(p, _)| p.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                        .as_bytes(),
                );

                // Follow the first link, then go back in history.
                let about = browse(&cnet, 80, &links[0].0, lane).await.unwrap();
                history.visit(&links[0].0);
                out.write("/out/about", &about.body);

                let prev = history.back().unwrap().to_string();
                out.write("/out/back", prev.as_bytes());
            });
        }
    }

    #[test]
    fn browse_linked_pages_over_live_daemon() {
        let mut host = AppHost::new(
            2,
            [3u8; 32],
            CibosProfile::Balanced,
            64,
            ResourceLimits::default_application(),
        );
        let system = host.system();
        host.launch(&Site);

        let fs: Filesystem = system.filesystem();
        assert_eq!(
            fs.read("/out/home").as_deref(),
            Some(&b"Home\nLINK /about About\nLINK /contact Contact"[..])
        );
        assert_eq!(fs.read("/out/links").as_deref(), Some(&b"/about,/contact"[..]));
        assert_eq!(
            fs.read("/out/about").as_deref(),
            Some(&b"About\nLINK /home Home"[..])
        );
        assert_eq!(fs.read("/out/back").as_deref(), Some(&b"/home"[..]));
    }
}
