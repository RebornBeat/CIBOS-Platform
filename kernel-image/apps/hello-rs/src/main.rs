//! A Rust CIBOS application packaged as a `.capp`, built on the `cibos-app`
//! runtime. It runs in ring 3 and reaches the kernel only through syscalls:
//! it logs a line via the console (`Log`), then exits (`Exit`) with a
//! distinctive code. This proves a real Rust application — not hand-written
//! assembly — runs unprivileged on the kernel through the syscall ABI.
//!
//! Built freestanding (`no_std` + `no_main`) for the bare app target and linked
//! at the application virtual address by kernel-image's build script, then
//! wrapped into a `.capp` the loader maps and enters at `_start`.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

/// Application entry point. The loader enters here in ring 3.
///
/// # Safety
///
/// Called once by the kernel loader as the program entry; not callable from
/// Rust otherwise.
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    cibos_app::console::println("  [app:hello-rs] a Rust .capp speaking via syscalls");
    // Exit code 9 is distinctive (vs the asm hello app's 7), so the kernel log
    // confirms which application returned.
    cibos_app::console::exit(9)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // Nothing to unwind to; report nothing and exit non-zero.
    cibos_app::console::exit(101)
}
