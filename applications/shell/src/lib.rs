//! # Shell
//!
//! An interactive command interpreter for the CLI platform. It reads command
//! lines from the console, dispatches built-in commands, and runs registered
//! *programs* — letting other CIBOS applications be composed under one prompt.
//!
//! Built-ins: `help`, `echo`, `time`, `limits`, `apps`, `clear`, `exit`.
//! Anything else is looked up in the program registry and invoked with its
//! arguments; an unknown name is reported.
//!
//! A program is a command handler `Fn(&[&str], &dyn Console)` — exactly the
//! shape of an app's per-line command processor (e.g. the package manager's
//! `process_command`), so existing apps drop in as shell programs without
//! modification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use platform_cli::{CliApp, CliContext, Console};
use cibos_sdk::{System, WeightClass};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A shell program: handles one invocation given its arguments and a console.
pub type Program = Arc<dyn Fn(&[&str], &dyn Console) + Send + Sync>;

/// The shell application.
#[derive(Default)]
pub struct Shell {
    programs: BTreeMap<String, Program>,
}

impl Shell {
    /// Create a shell with no registered programs (built-ins only).
    #[must_use]
    pub fn new() -> Self {
        Shell {
            programs: BTreeMap::new(),
        }
    }

    /// Register a program under `name`. Returns `self` for chaining.
    #[must_use]
    pub fn with_program(
        mut self,
        name: &str,
        program: impl Fn(&[&str], &dyn Console) + Send + Sync + 'static,
    ) -> Self {
        self.programs.insert(name.to_string(), Arc::new(program));
        self
    }
}

/// The prompt written before each command read.
const PROMPT: &str = "cibos> ";

fn dispatch(
    programs: &BTreeMap<String, Program>,
    system: &System,
    line: &str,
    console: &dyn Console,
) -> bool {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let Some(&cmd) = tokens.first() else {
        return true; // empty line, keep going
    };
    let args = &tokens[1..];

    match cmd {
        "exit" | "quit" => {
            console.write_line("bye");
            return false;
        }
        "help" => {
            console.write_line("built-ins: help echo time limits apps clear write read ls rm exit");
            if programs.is_empty() {
                console.write_line("programs: (none registered)");
            } else {
                let names: Vec<&str> = programs.keys().map(String::as_str).collect();
                console.write_line(&format!("programs: {}", names.join(" ")));
            }
        }
        "write" => {
            if let Some((path, rest)) = args.split_first() {
                if system.filesystem().write(path, rest.join(" ").as_bytes()) {
                    console.write_line("ok");
                } else {
                    console.write_line(&format!("invalid path: {path}"));
                }
            } else {
                console.write_line("usage: write <path> <text...>");
            }
        }
        "read" => match args.first() {
            Some(path) => match system.filesystem().read(path) {
                Some(bytes) => console.write_line(&String::from_utf8_lossy(&bytes)),
                None => console.write_line(&format!("not found: {path}")),
            },
            None => console.write_line("usage: read <path>"),
        },
        "ls" => {
            let prefix = args.first().copied().unwrap_or("");
            let paths = system.filesystem().list(prefix);
            if paths.is_empty() {
                console.write_line("(empty)");
            } else {
                for p in paths {
                    console.write_line(&p);
                }
            }
        }
        "rm" => match args.first() {
            Some(path) => {
                if system.filesystem().delete(path) {
                    console.write_line("deleted");
                } else {
                    console.write_line(&format!("not found: {path}"));
                }
            }
            None => console.write_line("usage: rm <path>"),
        },
        "echo" => console.write_line(&args.join(" ")),
        "time" => {
            let ms = system.now().as_nanos() / 1_000_000;
            console.write_line(&format!("uptime: {ms} ms"));
        }
        "limits" => {
            let l = system.resource_limits();
            console.write_line(&format!(
                "memory={} bytes, max_lanes={}, max_channels={}, max_message={} bytes",
                l.memory_bytes, l.max_lanes, l.max_channels, l.max_message_bytes
            ));
        }
        "apps" => {
            if programs.is_empty() {
                console.write_line("(no programs registered)");
            } else {
                for name in programs.keys() {
                    console.write_line(name);
                }
            }
        }
        "clear" => console.write_line("\x1b[2J\x1b[H"),
        other => match programs.get(other) {
            Some(program) => program(args, console),
            None => console.write_line(&format!("unknown command: {other}")),
        },
    }
    true
}

impl CliApp for Shell {
    fn name(&self) -> &str {
        "shell"
    }

    fn run(&self, ctx: CliContext) {
        // Share the program registry into the worker task.
        let programs: Arc<BTreeMap<String, Program>> = Arc::new(self.programs.clone());
        let console = ctx.console.clone();
        let system = ctx.system.clone();

        ctx.system.spawn(WeightClass::User, async move {
            console.write_line("CIBOS shell. Type 'help' for commands.");
            loop {
                console.write_line(PROMPT);
                let Some(line) = console.read_line() else {
                    break; // end of input
                };
                if !dispatch(&programs, &system, &line, &*console) {
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_cli::{CaptureConsole, CliRunner};

    fn run_with(input: &[&str], shell: Shell) -> Vec<String> {
        let console = Arc::new(CaptureConsole::new(input.iter().map(|s| s.to_string())));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&shell);
        console.output()
    }

    #[test]
    fn builtins_respond() {
        let out = run_with(&["help", "echo hello there", "apps", "exit"], Shell::new());
        let text = out.join("\n");
        assert!(text.contains("built-ins:"));
        assert!(text.contains("hello there"));
        assert!(text.contains("(no programs registered)"));
        assert!(text.contains("bye"));
    }

    #[test]
    fn unknown_command_reported() {
        let out = run_with(&["frobnicate"], Shell::new());
        assert!(out.join("\n").contains("unknown command: frobnicate"));
    }

    #[test]
    fn limits_and_time_use_system() {
        let out = run_with(&["limits", "time"], Shell::new());
        let text = out.join("\n");
        assert!(text.contains("memory="));
        assert!(text.contains("max_lanes="));
        assert!(text.contains("uptime:"));
    }

    #[test]
    fn registered_program_runs() {
        let shell = Shell::new().with_program("greet", |args, console| {
            console.write_line(&format!("hello, {}", args.join(" ")));
        });
        let out = run_with(&["apps", "greet world"], shell);
        let text = out.join("\n");
        assert!(text.contains("greet"));
        assert!(text.contains("hello, world"));
    }

    #[test]
    fn filesystem_builtins() {
        let out = run_with(
            &["ls", "write /tmp/x hello there", "read /tmp/x", "ls /tmp/", "rm /tmp/x", "read /tmp/x"],
            Shell::new(),
        );
        let text = out.join("\n");
        assert!(text.contains("(empty)")); // initial ls
        assert!(text.contains("hello there")); // read back
        assert!(text.contains("/tmp/x")); // ls /tmp/
        assert!(text.contains("deleted"));
        assert!(text.contains("not found: /tmp/x")); // read after rm
    }

    #[test]
    fn composes_the_package_manager() {
        // The package manager's per-line command processor drops straight in as
        // a shell program — one app composed inside another, unmodified.
        use package_manager::{process_command, Package};
        use std::collections::BTreeMap as Map;

        let mut catalog: Map<String, Package> = Map::new();
        let pkg = Package::genuine("editor", "1.0.0", b"editor bytes".to_vec());
        catalog.insert(pkg.name.clone(), pkg);
        let catalog = Arc::new(catalog);

        let shell = Shell::new().with_program("pkg", move |args, console| {
            // Reconstruct the package-manager command line from the args.
            let line = args.join(" ");
            process_command(&catalog, &line, console);
        });

        let out = run_with(&["pkg list", "pkg install editor", "exit"], shell);
        let text = out.join("\n");
        assert!(text.contains("editor 1.0.0"));
        assert!(text.contains("installed editor 1.0.0"));
    }
}
