//! # Vane — the CIBOS content-serving daemon
//!
//! Vane is CIBOS's server (the role nginx plays). It binds a Lattice [`Gate`],
//! accepts [`Link`]s, and answers [`Hail`](web_protocol) requests by serving
//! files from the shared [`Filesystem`] under a document root:
//!
//! * `FETCH /page` → reads `<docroot>/page` → `200 OK` with the bytes, or
//!   `404 NOT-FOUND`.
//! * `PUSH /page` → writes the body to `<docroot>/page` → `201 STORED`.
//!
//! The serve step is synchronous and explicit ([`serve_pending`]): it accepts
//! every waiting connection and answers one request on each. This keeps the
//! whole path testable without a spinning task. A long-running daemon simply
//! calls [`serve_pending`] whenever its [`Listener`] becomes readable.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::{
    ChannelDirection, ChannelTerms, Filesystem, Gate, KernelChannel, LaneId, Lattice, Listener,
    NetError,
    System,
};
use web_protocol::{
    decode_request, encode_response, status, Request, Response, Verb,
};

/// Bind `gate` on `net` to serve content.
///
/// # Errors
/// Propagates [`NetError`] from the Lattice (e.g. blocked or already bound).
pub fn bind(net: &Lattice, gate: Gate) -> Result<Listener, NetError> {
    net.bind(gate)
}

/// Join a document root and a request path into a filesystem key, preventing
/// escape above the root.
fn resolve(docroot: &str, path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    // Reject parent-directory traversal outright.
    if trimmed.split('/').any(|seg| seg == "..") {
        return String::new();
    }
    let root = docroot.trim_end_matches('/');
    format!("{root}/{trimmed}")
}

/// Compute the response for a request against the filesystem. Pure and
/// directly testable.
#[must_use]
pub fn handle_request(req: &Request, fs: &Filesystem, docroot: &str) -> Response {
    let key = resolve(docroot, &req.path);
    if key.is_empty() {
        return Response::with_status(status::BAD_REQUEST, b"invalid path".to_vec());
    }
    match req.verb {
        Verb::Fetch => match fs.read(&key) {
            Some(bytes) => Response::ok(bytes),
            None => Response::with_status(status::NOT_FOUND, b"not found".to_vec()),
        },
        Verb::Push => {
            if fs.write(&key, &req.body) {
                Response::with_status(status::STORED, Vec::new())
            } else {
                Response::with_status(status::BAD_REQUEST, b"invalid path".to_vec())
            }
        }
    }
}

/// Handle one request on an already-accepted [`Link`]: read the request
/// message, route it, send the response, close the link. Returns whether a
/// request was handled (false if nothing was waiting yet).
fn serve_link(
    link: &cibos_sdk::Link,
    fs: &Filesystem,
    docroot: &str,
) -> bool {
    match link.try_recv() {
        Ok(Some(bytes)) => {
            let response = match decode_request(&bytes) {
                Ok(req) => handle_request(&req, fs, docroot),
                Err(e) => Response::with_status(
                    status::BAD_REQUEST,
                    format!("{e}").into_bytes(),
                ),
            };
            let _ = link.send(&encode_response(&response));
            link.close();
            true
        }
        _ => false,
    }
}

/// Accept all currently-pending connections on `listener` and answer one
/// request on each. Returns the number of requests served.
pub fn serve_pending(listener: &Listener, fs: &Filesystem, docroot: &str) -> usize {
    let mut served = 0;
    while let Some(link) = listener.accept() {
        if serve_link(&link, fs, docroot) {
            served += 1;
        }
    }
    served
}

fn doorbell_terms() -> ChannelTerms {
    // Small notifications; capacity bounds the connection backlog.
    ChannelTerms::new("vane-doorbell", ChannelDirection::Bidirectional, 8, 64).unwrap()
}

/// A live, long-running Vane server: a bound Gate plus a doorbell channel that
/// wakes the serve loop when a connection arrives.
pub struct Daemon {
    listener: Listener,
    doorbell: KernelChannel,
}

/// Stand up a live daemon on `gate`: bind the Gate, create a doorbell, and
/// install it so [`Lattice::connect_signaling`] wakes the serve loop.
///
/// # Errors
/// Propagates [`NetError`] from binding the Gate.
pub fn listen(net: &Lattice, system: &System, gate: Gate) -> Result<Daemon, NetError> {
    let listener = net.bind(gate)?;
    let doorbell = system.open_channel(&doorbell_terms());
    net.install_doorbell(gate, doorbell.clone());
    Ok(Daemon { listener, doorbell })
}

impl Daemon {
    /// The Gate this daemon serves.
    #[must_use]
    pub fn gate(&self) -> Gate {
        self.listener.gate()
    }

    /// Serve forever: park on the doorbell until a connection arrives, then
    /// answer every pending request, and repeat. Returns only when the doorbell
    /// channel closes (server shutdown).
    ///
    /// Crucially this *parks* (Catch-and-Release) rather than polling, so a
    /// daemon with no traffic leaves the scheduler idle instead of spinning.
    pub async fn serve_forever(&self, lane: LaneId, fs: &Filesystem, docroot: &str) {
        // Parks on each `recv`; returns when the doorbell closes (shutdown).
        while self.doorbell.recv(lane).await.is_ok() {
            serve_pending(&self.listener, fs, docroot);
        }
    }
}

// Re-export for callers that want the error type without depending on the SDK.
pub use cibos_sdk::Link;

#[cfg(test)]
mod tests {
    use super::*;
    use web_protocol::decode_response;

    fn fs_with_index() -> Filesystem {
        let fs = Filesystem::new();
        fs.write("/www/index.html", b"<h1>Hello CIBOS</h1>");
        fs.write("/www/about", b"about page");
        fs
    }

    #[test]
    fn fetch_existing_and_missing() {
        let fs = fs_with_index();
        let ok = handle_request(&Request::fetch("/index.html"), &fs, "/www");
        assert_eq!(ok.status, status::OK);
        assert_eq!(ok.body, b"<h1>Hello CIBOS</h1>");

        let missing = handle_request(&Request::fetch("/nope"), &fs, "/www");
        assert_eq!(missing.status, status::NOT_FOUND);
    }

    #[test]
    fn push_then_fetch() {
        let fs = Filesystem::new();
        let stored = handle_request(&Request::push("/note", b"data".to_vec()), &fs, "/www");
        assert_eq!(stored.status, status::STORED);
        let back = handle_request(&Request::fetch("/note"), &fs, "/www");
        assert_eq!(back.body, b"data");
    }

    #[test]
    fn path_traversal_rejected() {
        let fs = fs_with_index();
        let resp = handle_request(&Request::fetch("/../secret"), &fs, "/www");
        assert_eq!(resp.status, status::BAD_REQUEST);
    }

    #[test]
    fn serve_pending_over_the_lattice() {
        let net = Lattice::new();
        let fs = fs_with_index();
        let listener = bind(&net, 80).unwrap();

        // A client connects and sends a request.
        let client = net.connect(80).unwrap();
        client
            .send(&web_protocol::encode_request(&Request::fetch("/index.html")))
            .unwrap();

        // The server answers it.
        assert_eq!(serve_pending(&listener, &fs, "/www"), 1);

        // The client reads the response off the link.
        let bytes = client.try_recv().unwrap().expect("a response");
        let resp = decode_response(&bytes).unwrap();
        assert_eq!(resp.status, status::OK);
        assert_eq!(resp.body, b"<h1>Hello CIBOS</h1>");
    }
}

#[cfg(test)]
mod daemon_tests {
    use super::*;
    use cibos_sdk::{Application, AppHost, CibosProfile, ResourceLimits, WeightClass};
    use web_protocol::{decode_response, encode_request};

    /// A live web service: a Vane daemon parked on its doorbell, plus two
    /// clients that connect (ringing the doorbell), fetch, and store results.
    struct WebService;

    impl Application for WebService {
        fn name(&self) -> &str {
            "web-service"
        }

        fn start(&self, system: System) {
            let net = system.lattice();
            let fs = system.filesystem();
            fs.write("/www/index.html", b"<h1>live daemon</h1>");
            fs.write("/www/about", b"served without polling");

            // The live daemon: parks on the doorbell, serves on wake.
            let daemon = listen(&net, &system, 80).unwrap();
            let server_fs = fs.clone();
            system.spawn_with_lane(WeightClass::System, move |lane| async move {
                daemon.serve_forever(lane, &server_fs, "/www").await;
            });

            // Two clients fetch different pages.
            for (path, out) in [("/index.html", "/r/index"), ("/about", "/r/about")] {
                let cnet = net.clone();
                let cfs = fs.clone();
                system.spawn_with_lane(WeightClass::User, move |lane| async move {
                    let link = cnet.connect_signaling(lane, 80).unwrap();
                    link.send(&encode_request(&Request::fetch(path))).unwrap();
                    // Wait for the daemon to answer (it wakes from the doorbell).
                    loop {
                        match link.try_recv() {
                            Ok(Some(bytes)) => {
                                let resp = decode_response(&bytes).unwrap();
                                cfs.write(out, &resp.body);
                                break;
                            }
                            Ok(None) => cibos_sdk::yield_now().await,
                            Err(_) => break,
                        }
                    }
                });
            }
        }
    }

    #[test]
    fn live_daemon_serves_clients_and_parks() {
        // If the daemon spun instead of parking, run() would never return and
        // this test would hang. Its completion proves the daemon parks.
        let mut host = AppHost::new(
            2,
            [9u8; 32],
            CibosProfile::Balanced,
            64,
            ResourceLimits::default_application(),
        );
        let system = host.system();
        host.launch(&WebService);

        let fs = system.filesystem();
        assert_eq!(
            fs.read("/r/index").as_deref(),
            Some(&b"<h1>live daemon</h1>"[..])
        );
        assert_eq!(
            fs.read("/r/about").as_deref(),
            Some(&b"served without polling"[..])
        );
    }
}
