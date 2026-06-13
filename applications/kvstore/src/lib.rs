//! # Key-Value Store
//!
//! A small in-memory key-value store application. Commands:
//! `set <key> <value...>`, `get <key>`, `del <key>`, `list`, `clear`.
//!
//! Like the other CIBOS apps it exposes a per-line [`process_command`] handler
//! (so it composes into the shell as a program) and a [`CliApp`] that spawns a
//! worker lane reading commands from the console.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use cibos_console::Console;

// The shared store lives behind a lock so `process_command` is identical on the
// host (state shared into an async worker) and in a ring-3 `.capp`. On the host
// that lock is `std::sync::Mutex`; in `no_std` it is the spin `Mutex` from
// `cibos-sync` — both expose `lock() -> Result<Guard, _>`, so the body is the
// same either way.
#[cfg(feature = "std")]
use std::sync::Mutex;
#[cfg(not(feature = "std"))]
use cibos_sync::Mutex;

#[cfg(feature = "std")]
use cibos_sdk::WeightClass;
#[cfg(feature = "std")]
use platform_cli::{CliApp, CliContext};

/// Shared store state.
pub type Store = Arc<Mutex<BTreeMap<String, String>>>;

/// Create an empty store.
#[must_use]
pub fn new_store() -> Store {
    Arc::new(Mutex::new(BTreeMap::new()))
}

/// Process one command line against `store`, writing results to `console`.
pub fn process_command(store: &Mutex<BTreeMap<String, String>>, line: &str, console: &dyn Console) {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else {
        return;
    };
    match cmd {
        "set" => {
            let Some(key) = parts.next() else {
                console.write_line("usage: set <key> <value...>");
                return;
            };
            let value: Vec<&str> = parts.collect();
            if value.is_empty() {
                console.write_line("usage: set <key> <value...>");
                return;
            }
            store
                .lock()
                .unwrap()
                .insert(key.to_string(), value.join(" "));
            console.write_line("ok");
        }
        "get" => match parts.next() {
            Some(key) => match store.lock().unwrap().get(key) {
                Some(v) => console.write_line(v),
                None => console.write_line("(not set)"),
            },
            None => console.write_line("usage: get <key>"),
        },
        "del" => match parts.next() {
            Some(key) => {
                if store.lock().unwrap().remove(key).is_some() {
                    console.write_line("deleted");
                } else {
                    console.write_line("(not set)");
                }
            }
            None => console.write_line("usage: del <key>"),
        },
        "list" => {
            let store = store.lock().unwrap();
            if store.is_empty() {
                console.write_line("(empty)");
            } else {
                for (k, v) in store.iter() {
                    console.write_line(&format!("{k} = {v}"));
                }
            }
        }
        "clear" => {
            store.lock().unwrap().clear();
            console.write_line("cleared");
        }
        other => console.write_line(&format!("unknown kv command: {other}")),
    }
}

/// The key-value store application.
pub struct KvStore {
    store: Store,
}

impl KvStore {
    /// Create a key-value store app over a fresh store.
    #[must_use]
    pub fn new() -> Self {
        KvStore { store: new_store() }
    }

    /// The shared store handle (for composition or inspection).
    #[must_use]
    pub fn store(&self) -> Store {
        self.store.clone()
    }
}

impl Default for KvStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std")]
impl CliApp for KvStore {
    fn name(&self) -> &str {
        "kvstore"
    }

    fn run(&self, ctx: CliContext) {
        let store = self.store.clone();
        let console = ctx.console.clone();
        ctx.system.spawn(WeightClass::User, async move {
            while let Some(line) = console.read_line() {
                process_command(&store, &line, &*console);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_cli::{CaptureConsole, CliRunner};

    #[test]
    fn set_get_del_list() {
        let console = Arc::new(CaptureConsole::new(
            [
                "list",
                "set greeting hello world",
                "get greeting",
                "set color teal",
                "list",
                "del greeting",
                "get greeting",
            ]
            .iter()
            .map(|s| s.to_string()),
        ));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&KvStore::new());

        let out = console.output();
        assert_eq!(out[0], "(empty)");
        assert_eq!(out[1], "ok");
        assert_eq!(out[2], "hello world");
        assert_eq!(out[3], "ok");
        // list now shows both keys, sorted
        assert!(out.contains(&"color = teal".to_string()));
        assert!(out.contains(&"greeting = hello world".to_string()));
        assert!(out.contains(&"deleted".to_string()));
        assert_eq!(out.last().unwrap(), "(not set)");
    }

    #[test]
    fn unknown_command() {
        let console = Arc::new(CaptureConsole::new(["frob x".to_string()]));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&KvStore::new());
        assert!(console.output_text().contains("unknown kv command: frob"));
    }
}
