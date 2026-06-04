//! Runnable end-to-end web demo: a Vane server serving files from the
//! filesystem, and a Lens client fetching them over the Lattice. Run with
//! `cargo run -p lens --bin web-demo`.

use cibos_sdk::{Filesystem, Lattice};
use web_protocol::Request;

const GATE: u16 = 80;

fn main() {
    // Set up the fabric, a document root in the filesystem, and the server.
    let net = Lattice::new();
    let fs = Filesystem::new();
    fs.write(
        "/www/index.html",
        b"<h1>Welcome to CIBOS</h1>\nServed by Vane over the Lattice.",
    );
    fs.write("/www/about", b"CIBOS: an isolation-first operating system.");
    let listener = vane::bind(&net, GATE).expect("bind gate 80");

    println!("== Vane serving on Gate {GATE} ==\n");

    // Fetch a few pages, including a missing one.
    for path in ["/index.html", "/about", "/missing"] {
        let link = lens::open(&net, GATE).expect("connect");
        lens::request(&link, &Request::fetch(path)).expect("send");
        vane::serve_pending(&listener, &fs, "/www");
        match lens::read_response(&link) {
            Ok(Some(resp)) => println!("FETCH {path}\n{}\n", lens::render(&resp)),
            Ok(None) => println!("FETCH {path}\n(no response)\n"),
            Err(e) => println!("FETCH {path}\nerror: {e}\n"),
        }
    }

    // Push a new page, then fetch it back.
    let link = lens::open(&net, GATE).expect("connect");
    lens::request(&link, &Request::push("/note", b"stored via Lens".to_vec())).expect("send");
    vane::serve_pending(&listener, &fs, "/www");
    if let Ok(Some(resp)) = lens::read_response(&link) {
        println!("PUSH /note\n{}\n", lens::render(&resp));
    }

    let link = lens::open(&net, GATE).expect("connect");
    lens::request(&link, &Request::fetch("/note")).expect("send");
    vane::serve_pending(&listener, &fs, "/www");
    if let Ok(Some(resp)) = lens::read_response(&link) {
        println!("FETCH /note\n{}", lens::render(&resp));
    }
}
