//! # Line Editor
//!
//! A minimal in-memory line-buffer editor. Commands:
//! `append <text>`, `insert <n> <text>`, `delete <n>`, `show`, `count`, `clear`.
//! Line numbers are 1-based.
//!
//! Exposes a per-line [`process_command`] handler (so it composes into the
//! shell) and a [`CliApp`] that spawns a worker lane.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::WeightClass;
use platform_cli::{CliApp, CliContext, Console};
use std::sync::{Arc, Mutex};

/// Shared editor buffer (the lines of text).
pub type Buffer = Arc<Mutex<Vec<String>>>;

/// Create an empty buffer.
#[must_use]
pub fn new_buffer() -> Buffer {
    Arc::new(Mutex::new(Vec::new()))
}

/// Process one command line against `buffer`, writing results to `console`.
pub fn process_command(buffer: &Mutex<Vec<String>>, line: &str, console: &dyn Console) {
    let trimmed = line.trim_start();
    let (cmd, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim_start()),
        None => (trimmed, ""),
    };

    match cmd {
        "append" => {
            buffer.lock().unwrap().push(rest.to_string());
            console.write_line("ok");
        }
        "insert" => {
            // insert <n> <text>: place text so it becomes line n (1-based).
            let mut it = rest.splitn(2, char::is_whitespace);
            let (Some(n_str), text) = (it.next(), it.next().unwrap_or("")) else {
                console.write_line("usage: insert <n> <text>");
                return;
            };
            let Ok(n) = n_str.parse::<usize>() else {
                console.write_line("usage: insert <n> <text>");
                return;
            };
            let mut buf = buffer.lock().unwrap();
            if n == 0 || n > buf.len() + 1 {
                console.write_line(&format!("out of range: {n}"));
                return;
            }
            buf.insert(n - 1, text.to_string());
            console.write_line("ok");
        }
        "delete" => {
            let Ok(n) = rest.trim().parse::<usize>() else {
                console.write_line("usage: delete <n>");
                return;
            };
            let mut buf = buffer.lock().unwrap();
            if n == 0 || n > buf.len() {
                console.write_line(&format!("out of range: {n}"));
                return;
            }
            buf.remove(n - 1);
            console.write_line("ok");
        }
        "show" => {
            let buf = buffer.lock().unwrap();
            if buf.is_empty() {
                console.write_line("(empty)");
            } else {
                for (i, line) in buf.iter().enumerate() {
                    console.write_line(&format!("{:>3}  {}", i + 1, line));
                }
            }
        }
        "count" => {
            let n = buffer.lock().unwrap().len();
            console.write_line(&format!("{n} line(s)"));
        }
        "clear" => {
            buffer.lock().unwrap().clear();
            console.write_line("ok");
        }
        other => console.write_line(&format!("unknown editor command: {other}")),
    }
}

/// The line editor application.
pub struct Editor {
    buffer: Buffer,
}

impl Editor {
    /// Create an editor over a fresh buffer.
    #[must_use]
    pub fn new() -> Self {
        Editor {
            buffer: new_buffer(),
        }
    }

    /// The shared buffer handle.
    #[must_use]
    pub fn buffer(&self) -> Buffer {
        self.buffer.clone()
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl CliApp for Editor {
    fn name(&self) -> &str {
        "editor"
    }

    fn run(&self, ctx: CliContext) {
        let buffer = self.buffer.clone();
        let console = ctx.console.clone();
        let fs = ctx.system.filesystem();
        ctx.system.spawn(WeightClass::User, async move {
            while let Some(line) = console.read_line() {
                // `save`/`load` use the shared filesystem; everything else is a
                // pure buffer operation.
                if !handle_storage(&buffer, &fs, &line, &*console) {
                    process_command(&buffer, &line, &*console);
                }
            }
        });
    }
}

/// Handle `save <path>` / `load <path>` against the filesystem. Returns whether
/// the line was a storage command (and thus already handled).
fn handle_storage(
    buffer: &Mutex<Vec<String>>,
    fs: &cibos_sdk::Filesystem,
    line: &str,
    console: &dyn Console,
) -> bool {
    let trimmed = line.trim_start();
    let (cmd, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (trimmed, ""),
    };
    match cmd {
        "save" => {
            if rest.is_empty() {
                console.write_line("usage: save <path>");
                return true;
            }
            let text = buffer.lock().unwrap().join("\n");
            if fs.write(rest, text.as_bytes()) {
                let n = buffer.lock().unwrap().len();
                console.write_line(&format!("saved {n} line(s) to {rest}"));
            } else {
                console.write_line(&format!("invalid path: {rest}"));
            }
            true
        }
        "load" => {
            if rest.is_empty() {
                console.write_line("usage: load <path>");
                return true;
            }
            match fs.read(rest) {
                Some(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let lines: Vec<String> = if text.is_empty() {
                        Vec::new()
                    } else {
                        text.split('\n').map(str::to_string).collect()
                    };
                    let n = lines.len();
                    *buffer.lock().unwrap() = lines;
                    console.write_line(&format!("loaded {n} line(s) from {rest}"));
                }
                None => console.write_line(&format!("not found: {rest}")),
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_cli::{CaptureConsole, CliRunner};

    fn run(input: &[&str]) -> Vec<String> {
        let console = Arc::new(CaptureConsole::new(input.iter().map(|s| s.to_string())));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&Editor::new());
        console.output()
    }

    #[test]
    fn append_and_show() {
        let out = run(&["append first line", "append second line", "count", "show"]);
        assert_eq!(out[0], "ok");
        assert_eq!(out[2], "2 line(s)");
        assert!(out.contains(&"  1  first line".to_string()));
        assert!(out.contains(&"  2  second line".to_string()));
    }

    #[test]
    fn insert_and_delete() {
        let out = run(&[
            "append a",
            "append c",
            "insert 2 b", // becomes line 2, pushing c to 3
            "show",
            "delete 1", // remove a
            "show",
        ]);
        // After insert: a, b, c
        assert!(out.contains(&"  2  b".to_string()));
        assert!(out.contains(&"  3  c".to_string()));
        // After delete 1: b, c
        let tail = out.join("\n");
        assert!(tail.contains("  1  b"));
        assert!(tail.contains("  2  c"));
    }

    #[test]
    fn out_of_range_and_unknown() {
        let out = run(&["delete 5", "insert 9 x", "frobnicate"]).join("\n");
        assert!(out.contains("out of range: 5"));
        assert!(out.contains("out of range: 9"));
        assert!(out.contains("unknown editor command: frobnicate"));
    }

    #[test]
    fn save_then_load_round_trips_through_filesystem() {
        // Build a buffer, save it to the shared filesystem, clear, then load it
        // back — proving the editor persists through the SDK filesystem service.
        let out = run(&[
            "append alpha",
            "append beta",
            "save /docs/notes.txt",
            "clear",
            "count",        // expect 0 line(s)
            "load /docs/notes.txt",
            "count",        // expect 2 line(s)
            "show",
        ]);
        let text = out.join("\n");
        assert!(text.contains("saved 2 line(s) to /docs/notes.txt"));
        assert!(text.contains("0 line(s)"));
        assert!(text.contains("loaded 2 line(s) from /docs/notes.txt"));
        assert!(text.contains("2 line(s)"));
        assert!(text.contains("  1  alpha"));
        assert!(text.contains("  2  beta"));
    }

    #[test]
    fn load_missing_file_reports_not_found() {
        let out = run(&["load /nope"]).join("\n");
        assert!(out.contains("not found: /nope"));
    }
}
