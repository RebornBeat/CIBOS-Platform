//! A Rust CIBOS application packaged as a `.capp`, built on the `cibos-app`
//! runtime. It runs in ring 3 and reaches the kernel only through syscalls.
//! It demonstrates: console output (`Log`), heap allocation (`alloc` over the
//! kernel-provided heap), and filesystem I/O (`Fs*`) — all from userspace.
//!
//! Built freestanding (`no_std` + `no_main`) for the bare app target and linked
//! at the application virtual address by kernel-image's build script, then
//! wrapped into a `.capp` the loader maps and enters at `_start`. The
//! `entry!`/`default_panic_handler!` macros from `cibos-app` provide `_start`
//! (which installs the heap from the registers the kernel passes) and a panic
//! handler.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

cibos_app::entry!(main);
cibos_app::default_panic_handler!();

fn main() -> u64 {
    use cibos_app::{console, fs};

    console::println("  [app:hello-rs] a Rust .capp speaking via syscalls");

    // Prove the heap works: build a String and a Vec on the kernel-provided heap.
    let mut parts: Vec<String> = Vec::new();
    for i in 0..4 {
        parts.push(format!("item{i}"));
    }
    let joined = parts.join(",");
    console::print("  [app:hello-rs] heap alloc ok: ");
    console::println(&joined);

    // Exercise filesystem syscalls from ring 3, using a heap-built payload.
    let path = b"/etc/hello-rs.txt";
    let payload = format!("a Rust ring-3 app wrote this; parts={joined}");
    match fs::write(path, payload.as_bytes()) {
        Ok(n) => {
            console::println(&format!("  [app:hello-rs] fs::write ok ({n} bytes)"));
            let mut buf = [0u8; 128];
            match fs::read_into(path, &mut buf) {
                Ok(r) if &buf[..r] == payload.as_bytes() => {
                    console::println("  [app:hello-rs] fs::read_into ok — round-trip verified");
                }
                Ok(_) => console::println("  [app:hello-rs] fs::read_into mismatch"),
                Err(_) => console::println("  [app:hello-rs] fs::read_into error"),
            }
        }
        Err(_) => console::println("  [app:hello-rs] fs::write error (no root fs mounted?)"),
    }

    // Probe the input syscall (non-blocking, so a headless boot does not hang).
    // This proves the ReadKey path works from ring 3; with a real keyboard it
    // would return the pending key.
    match cibos_app::input::poll_key() {
        Some((code, _)) => {
            console::println(&format!("  [app:hello-rs] input: a key was waiting ({code:?})"));
        }
        None => console::println("  [app:hello-rs] input: ReadKey ok (no key buffered)"),
    }

    9
}
