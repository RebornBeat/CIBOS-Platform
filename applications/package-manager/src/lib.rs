//! # Package Manager
//!
//! A CIBOS application demonstrating the SDK and CLI platform carrying real
//! software, and enforcing a core design rule: **packages are verifiable by
//! content hash**. Every package in the catalog carries the SHA-256 its
//! provider declared. Before a package is installed its actual bytes are hashed
//! and compared (in constant time) against that declared hash. A provider that
//! alters a package's contents after publishing the hash is detected, and the
//! install is refused. Verification uses the audited SHA-256 in `shared`.
//!
//! The application runs as a CLI app: it spawns a worker task (a lane) that
//! reads commands from the console and prints results. Supported commands:
//! `list`, `info <name>`, `verify <name>`, `install <name>`.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
#[cfg(feature = "std")]
use alloc::sync::Arc;
#[cfg(feature = "std")]
use alloc::vec;
use alloc::vec::Vec;

use cibos_console::Console;
use shared::crypto::hash::{digests_equal_ct, sha256, Digest256};

#[cfg(feature = "std")]
use platform_cli::{CliApp, CliContext};

/// A package: its identity, declared content hash, and the bytes as delivered.
#[derive(Debug, Clone)]
pub struct Package {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// The SHA-256 the provider declared for this package's contents.
    pub declared_hash: Digest256,
    /// The package contents as delivered (which an attacker might have altered).
    pub contents: Vec<u8>,
}

impl Package {
    /// Build a genuine package: the declared hash is computed from the contents,
    /// so it verifies.
    #[must_use]
    pub fn genuine(name: &str, version: &str, contents: Vec<u8>) -> Self {
        let declared_hash = sha256(&contents);
        Package {
            name: name.to_string(),
            version: version.to_string(),
            declared_hash,
            contents,
        }
    }

    /// Build a package whose declared hash does *not* match its contents,
    /// modelling tampering (the contents were altered after the hash was
    /// published).
    #[must_use]
    pub fn with_declared_hash(
        name: &str,
        version: &str,
        contents: Vec<u8>,
        declared_hash: Digest256,
    ) -> Self {
        Package {
            name: name.to_string(),
            version: version.to_string(),
            declared_hash,
            contents,
        }
    }

    /// Size of the package contents in bytes.
    #[must_use]
    pub fn size(&self) -> usize {
        self.contents.len()
    }

    /// Whether the contents match the declared hash (constant-time comparison).
    #[must_use]
    pub fn verify(&self) -> bool {
        let actual = sha256(&self.contents);
        digests_equal_ct(&actual, &self.declared_hash)
    }

    /// First eight bytes of the declared hash, hex-encoded, for display.
    #[must_use]
    pub fn short_hash(&self) -> String {
        self.declared_hash[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// The package catalog.
#[cfg(feature = "std")]
struct Catalog {
    packages: BTreeMap<String, Package>,
}

/// The package manager application.
#[cfg(feature = "std")]
pub struct PackageManager {
    catalog: Arc<Catalog>,
}

#[cfg(feature = "std")]
impl PackageManager {
    /// Build a package manager over the given packages.
    #[must_use]
    pub fn new(packages: Vec<Package>) -> Self {
        let mut map = BTreeMap::new();
        for p in packages {
            map.insert(p.name.clone(), p);
        }
        PackageManager {
            catalog: Arc::new(Catalog { packages: map }),
        }
    }

    /// A package manager seeded with a couple of sample packages.
    #[must_use]
    pub fn with_samples() -> Self {
        Self::new(vec![
            Package::genuine("text-editor", "1.2.0", b"editor binary contents".to_vec()),
            Package::genuine("file-manager", "0.9.1", b"file manager contents".to_vec()),
        ])
    }
}

/// Process a single command line against the catalog, writing results to the
/// console. Exposed for direct testing of the command logic.
pub fn process_command(catalog_packages: &BTreeMap<String, Package>, line: &str, console: &dyn Console) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next();

    match cmd {
        "list" => {
            if catalog_packages.is_empty() {
                console.write_line("(no packages)");
            }
            for pkg in catalog_packages.values() {
                console.write_line(&format!("{} {}", pkg.name, pkg.version));
            }
        }
        "info" => match arg.and_then(|n| catalog_packages.get(n)) {
            Some(pkg) => console.write_line(&format!(
                "{} {} ({} bytes) sha256={}",
                pkg.name,
                pkg.version,
                pkg.size(),
                pkg.short_hash()
            )),
            None => console.write_line(&not_found(arg)),
        },
        "verify" => match arg.and_then(|n| catalog_packages.get(n)) {
            Some(pkg) => {
                if pkg.verify() {
                    console.write_line(&format!("{}: ok", pkg.name));
                } else {
                    console.write_line(&format!("{}: INTEGRITY FAILURE", pkg.name));
                }
            }
            None => console.write_line(&not_found(arg)),
        },
        "install" => match arg.and_then(|n| catalog_packages.get(n)) {
            Some(pkg) => {
                if pkg.verify() {
                    console.write_line(&format!("installed {} {}", pkg.name, pkg.version));
                } else {
                    console.write_line(&format!(
                        "refused {}: integrity check failed",
                        pkg.name
                    ));
                }
            }
            None => console.write_line(&not_found(arg)),
        },
        other => console.write_line(&format!("unknown command: {other}")),
    }
}

fn not_found(arg: Option<&str>) -> String {
    match arg {
        Some(name) => format!("not found: {name}"),
        None => "missing package name".to_string(),
    }
}

#[cfg(feature = "std")]
impl CliApp for PackageManager {
    fn name(&self) -> &str {
        "package-manager"
    }

    fn run(&self, ctx: CliContext) {
        let catalog = self.catalog.clone();
        let console = ctx.console.clone();
        ctx.system.spawn_user(async move {
            while let Some(line) = console.read_line() {
                process_command(&catalog.packages, &line, &*console);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_cli::{CaptureConsole, CliRunner};

    #[test]
    fn genuine_package_verifies_and_installs() {
        let console = Arc::new(CaptureConsole::new(
            ["list", "verify text-editor", "install text-editor"]
                .iter()
                .map(|s| s.to_string()),
        ));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&PackageManager::with_samples());

        let out = console.output_text();
        assert!(out.contains("text-editor 1.2.0"));
        assert!(out.contains("text-editor: ok"));
        assert!(out.contains("installed text-editor 1.2.0"));
    }

    #[test]
    fn tampered_package_is_refused() {
        // A package whose declared hash does not match its (altered) contents.
        let genuine_hash = sha256(b"the original, audited contents");
        let tampered = Package::with_declared_hash(
            "evil",
            "6.6.6",
            b"contents secretly replaced by an attacker".to_vec(),
            genuine_hash,
        );
        let pm = PackageManager::new(vec![tampered]);

        let console = Arc::new(CaptureConsole::new(
            ["verify evil", "install evil"].iter().map(|s| s.to_string()),
        ));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&pm);

        let out = console.output_text();
        assert!(out.contains("evil: INTEGRITY FAILURE"), "tamper detected on verify");
        assert!(
            out.contains("refused evil: integrity check failed"),
            "install refused on integrity failure"
        );
        assert!(!out.contains("installed evil"), "must never install a tampered package");
    }

    #[test]
    fn unknown_package_and_command() {
        let console = Arc::new(CaptureConsole::new(
            ["info nonesuch", "frobnicate"].iter().map(|s| s.to_string()),
        ));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&PackageManager::with_samples());

        let out = console.output_text();
        assert!(out.contains("not found: nonesuch"));
        assert!(out.contains("unknown command: frobnicate"));
    }
}
