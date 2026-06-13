//! # Trove — the CIBOS app store
//!
//! A store front over the package manager. The catalog is a set of
//! [`Package`]s; installing one **verifies its hash** (a tampered package is
//! refused) and then writes its bytes into the filesystem under
//! `/apps/<name>/` — so "installed" state lives in the same storage that the
//! Live/Persistent volume governs (installs survive reboots in Persistent mode,
//! vanish in Live mode).
//!
//! Commands: `browse`, `search <q>`, `info <name>`, `install <name>`,
//! `installed`, `remove <name>`.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use cibos_console::{Console, ShellFs};
use package_manager::Package;

/// The result of an install attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallResult {
    /// Installed successfully.
    Installed,
    /// Already installed (no change).
    AlreadyInstalled,
    /// No such app in the catalog.
    NotInCatalog,
    /// The package failed hash verification (refused).
    VerificationFailed,
}

/// The app store, over a catalog and the working filesystem.
pub struct Store<F: ShellFs> {
    catalog: BTreeMap<String, Package>,
    fs: F,
}

fn meta_key(name: &str) -> String {
    format!("/apps/{name}/meta")
}
fn bin_key(name: &str) -> String {
    format!("/apps/{name}/bin")
}

impl<F: ShellFs> Store<F> {
    /// Create a store from a catalog of packages and the filesystem to install
    /// into.
    #[must_use]
    pub fn new(packages: Vec<Package>, fs: F) -> Self {
        let catalog = packages.into_iter().map(|p| (p.name.clone(), p)).collect();
        Store { catalog, fs }
    }

    /// Names of all apps available in the catalog.
    #[must_use]
    pub fn available(&self) -> Vec<String> {
        self.catalog.keys().cloned().collect()
    }

    /// Catalog names containing `query` (case-insensitive).
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<String> {
        let q = query.to_lowercase();
        self.catalog
            .keys()
            .filter(|n| n.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    /// Whether `name` is installed.
    #[must_use]
    pub fn is_installed(&self, name: &str) -> bool {
        self.fs.exists(&meta_key(name))
    }

    /// Names of installed apps. Works across filesystem backends: a flat
    /// key-value FS lists full paths (e.g. `/apps/foo/meta`), while a
    /// hierarchical FS lists immediate child names (e.g. `foo`). We normalize
    /// each entry to the bare app name and de-duplicate.
    #[must_use]
    pub fn installed(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .fs
            .list("/apps/")
            .into_iter()
            .filter_map(|entry| {
                // Flat backend: ".../apps/<name>/meta" or ".../apps/<name>/bin".
                if let Some(rest) = entry.strip_prefix("/apps/") {
                    let app = rest.split('/').next().unwrap_or("");
                    if !app.is_empty() {
                        return Some(app.to_string());
                    }
                }
                // Hierarchical backend: a bare child name ("<name>"), the per-app
                // directory. Skip stray files.
                if !entry.is_empty() && !entry.contains('/') {
                    return Some(entry);
                }
                None
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Install `name` from the catalog, verifying its hash first.
    pub fn install(&self, name: &str) -> InstallResult {
        let Some(pkg) = self.catalog.get(name) else {
            return InstallResult::NotInCatalog;
        };
        if self.is_installed(name) {
            return InstallResult::AlreadyInstalled;
        }
        if !pkg.verify() {
            return InstallResult::VerificationFailed;
        }
        // Ensure the per-app directory exists before writing files beneath it
        // (a hierarchical filesystem requires the parent; flat backends no-op).
        self.fs.mkdir("/apps");
        self.fs.mkdir(&format!("/apps/{name}"));
        self.fs.write(&bin_key(name), &pkg.contents);
        self.fs
            .write(&meta_key(name), pkg.version.as_bytes());
        InstallResult::Installed
    }

    /// Remove an installed app. Returns whether it was installed.
    pub fn remove(&self, name: &str) -> bool {
        let had = self.is_installed(name);
        self.fs.delete(&bin_key(name));
        self.fs.delete(&meta_key(name));
        had
    }

    /// Catalog entry for `name`.
    #[must_use]
    pub fn package(&self, name: &str) -> Option<&Package> {
        self.catalog.get(name)
    }
}

/// Process one store command, writing output to `console`.
pub fn process_command<F: ShellFs>(store: &Store<F>, line: &str, console: &dyn Console) {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else {
        return;
    };
    match cmd {
        "browse" | "list" => {
            let available = store.available();
            if available.is_empty() {
                console.write_line("(catalog empty)");
            } else {
                for name in available {
                    let mark = if store.is_installed(&name) {
                        " [installed]"
                    } else {
                        ""
                    };
                    let version = store
                        .package(&name)
                        .map(|p| p.version.as_str())
                        .unwrap_or("?");
                    console.write_line(&format!("{name} {version}{mark}"));
                }
            }
        }
        "search" => {
            let q = parts.collect::<Vec<_>>().join(" ");
            if q.is_empty() {
                console.write_line("usage: search <query>");
                return;
            }
            let hits = store.search(&q);
            if hits.is_empty() {
                console.write_line("(no matches)");
            } else {
                for n in hits {
                    console.write_line(&n);
                }
            }
        }
        "info" => match parts.next() {
            Some(name) => match store.package(name) {
                Some(pkg) => {
                    console.write_line(&format!(
                        "{} {} — {} bytes, sha {}{}",
                        pkg.name,
                        pkg.version,
                        pkg.size(),
                        pkg.short_hash(),
                        if store.is_installed(name) {
                            " [installed]"
                        } else {
                            ""
                        }
                    ));
                }
                None => console.write_line(&format!("not in catalog: {name}")),
            },
            None => console.write_line("usage: info <name>"),
        },
        "install" => match parts.next() {
            Some(name) => {
                let msg = match store.install(name) {
                    InstallResult::Installed => format!("installed {name}"),
                    InstallResult::AlreadyInstalled => format!("{name} already installed"),
                    InstallResult::NotInCatalog => format!("not in catalog: {name}"),
                    InstallResult::VerificationFailed => {
                        format!("REFUSED: {name} failed verification")
                    }
                };
                console.write_line(&msg);
            }
            None => console.write_line("usage: install <name>"),
        },
        "installed" => {
            let apps = store.installed();
            if apps.is_empty() {
                console.write_line("(nothing installed)");
            } else {
                for a in apps {
                    console.write_line(&a);
                }
            }
        }
        "remove" => match parts.next() {
            Some(name) => {
                if store.remove(name) {
                    console.write_line(&format!("removed {name}"));
                } else {
                    console.write_line(&format!("not installed: {name}"));
                }
            }
            None => console.write_line("usage: remove <name>"),
        },
        other => console.write_line(&format!("unknown store command: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::crypto::hash::sha256;

    fn catalog() -> Vec<Package> {
        vec![
            Package::genuine("courier", "1.0.0", b"courier app bytes".to_vec()),
            Package::genuine("notepad", "2.1.0", b"notepad app bytes".to_vec()),
            // A tampered package: contents altered after the hash was declared.
            Package::with_declared_hash(
                "trojan",
                "0.0.1",
                b"malicious bytes".to_vec(),
                sha256(b"the original benign bytes"),
            ),
        ]
    }

    fn store() -> Store<cibos_sdk::Filesystem> {
        Store::new(catalog(), cibos_sdk::Filesystem::new())
    }

    #[test]
    fn install_verify_and_persist_to_fs() {
        let s = store();
        assert_eq!(s.install("courier"), InstallResult::Installed);
        assert!(s.is_installed("courier"));
        // The app bytes are now in the filesystem (survive reboot in Persistent
        // mode).
        let fs = cibos_sdk::Filesystem::new();
        let _ = fs; // documentation: installs live in the volume's fs
        assert_eq!(s.installed(), vec!["courier".to_string()]);
        // Re-install is a no-op.
        assert_eq!(s.install("courier"), InstallResult::AlreadyInstalled);
    }

    #[test]
    fn tampered_package_refused() {
        let s = store();
        assert_eq!(s.install("trojan"), InstallResult::VerificationFailed);
        assert!(!s.is_installed("trojan"));
    }

    #[test]
    fn unknown_app() {
        let s = store();
        assert_eq!(s.install("ghost"), InstallResult::NotInCatalog);
    }

    #[test]
    fn search_and_remove() {
        let s = store();
        assert_eq!(s.search("note"), vec!["notepad".to_string()]);
        s.install("notepad");
        assert!(s.is_installed("notepad"));
        assert!(s.remove("notepad"));
        assert!(!s.is_installed("notepad"));
        assert!(!s.remove("notepad")); // already gone
    }

    #[test]
    fn command_interface() {
        use platform_cli::CaptureConsole;
        use std::sync::Arc;
        let s = store();
        let console = Arc::new(CaptureConsole::new(std::iter::empty()));
        process_command(&s, "browse", &*console);
        process_command(&s, "install courier", &*console);
        process_command(&s, "installed", &*console);
        process_command(&s, "install trojan", &*console);
        let out = console.output_text();
        assert!(out.contains("courier 1.0.0"));
        assert!(out.contains("installed courier"));
        assert!(out.contains("REFUSED: trojan failed verification"));
    }
}
