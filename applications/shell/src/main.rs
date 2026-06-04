//! Runnable CIBOS system shell: the shell library with bundled utilities,
//! driven by the host's standard console. Run with `cargo run -p shell --bin
//! cibos-shell` and type `help`.

use package_manager::{process_command as pkg_command, Package};
use platform_cli::{CliRunner, StdConsole};
use shell::Shell;
use std::collections::BTreeMap;
use std::sync::Arc;

fn main() {
    // A small package catalog, exposed under the `pkg` program.
    let mut catalog: BTreeMap<String, Package> = BTreeMap::new();
    for pkg in [
        Package::genuine("text-editor", "1.2.0", b"text editor contents".to_vec()),
        Package::genuine("file-manager", "0.9.1", b"file manager contents".to_vec()),
    ] {
        catalog.insert(pkg.name.clone(), pkg);
    }
    let catalog = Arc::new(catalog);

    // Shared state for the bundled utilities.
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

    let mut runner = CliRunner::new(Arc::new(StdConsole));
    runner.run(&shell);
}
