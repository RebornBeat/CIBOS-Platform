//! CIBOS shell application (`.capp`).
//!
//! Runs the **real** `shell::dispatch` on the kernel: it builds a
//! [`shell::Shell`] registry, registers the existing `package-manager` app as
//! the `pkg` program (reusing `package_manager::process_command` verbatim — not
//! reimplemented), and drives a synchronous read-line loop against a
//! [`cibos_app::SyscallConsole`] and [`cibos_app::SyscallSystem`] (Log /
//! ReadKey / `Fs*` / Now syscalls). This is the same shell logic the host runs;
//! only the console + system backends differ.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;

use cibos_app::{Console, SyscallConsole, SyscallSystem};
use cibos_console::Console as _;
use package_manager::{process_command as pkg_command, Package};
use shell::{dispatch, Shell, PROMPT};

cibos_app::entry!(main);
cibos_app::default_panic_handler!();

fn main() -> u64 {
    let console = SyscallConsole::new();
    let system = SyscallSystem::default();

    // A small built-in package catalog, exposed under the `pkg` program — the
    // existing package manager, composed exactly as the host shell composes it.
    let mut catalog: BTreeMap<String, Package> = BTreeMap::new();
    for pkg in [
        Package::genuine("text-editor", "1.2.0", b"text editor contents".to_vec()),
        Package::genuine("file-manager", "0.9.1", b"file manager contents".to_vec()),
    ] {
        catalog.insert(pkg.name.clone(), pkg);
    }

    // Shared state for the bundled utilities (the existing kvstore + editor).
    let store = kvstore::new_store();
    let buffer = editor::new_buffer();

    let shell = Shell::new()
        .with_program("pkg", move |args, console| {
            pkg_command(&catalog, &args.join(" "), console);
        })
        .with_program("kv", move |args, console| {
            kvstore::process_command(&store, &args.join(" "), console);
        })
        .with_program("edit", move |args, console| {
            editor::process_command(&buffer, &args.join(" "), console);
        });

    console.write_line("CIBOS shell. Type 'help' for commands.");
    loop {
        console.write_line(PROMPT);
        let line: String = cibos_app::input::read_line(false);
        // The on-kernel console never reaches end-of-input; an empty line just
        // re-prompts (dispatch treats it as a no-op).
        if !dispatch(shell.programs(), &system, &line, &console) {
            break;
        }
    }
    console.write_line("shell exited");
    0
}
