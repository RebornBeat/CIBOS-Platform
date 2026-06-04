//! Non-interactive GUI demo: drive the Notepad with a scripted sequence of
//! keyboard and pointer events and print the resulting display surface. Run with
//! `cargo run -p notepad --bin gui-demo`.
//!
//! (Interactive use needs a hardware keyboard driver feeding real
//! `InputEvent`s; here we script the events to show the render path.)

use notepad::Notepad;
use platform_gui::{GuiRunner, Surface};
use cibos_input::{InputEvent, Key, KeyEvent, Pointer};

fn key(k: Key) -> InputEvent {
    InputEvent::Key(KeyEvent::new(k))
}
fn ch(c: char) -> InputEvent {
    InputEvent::Key(KeyEvent::ch(c))
}

fn print_surface(label: &str, s: &Surface) {
    let bar = "-".repeat(s.width() as usize);
    println!("{label}");
    println!("+{bar}+");
    for y in 0..s.height() {
        let row: String = (0..s.width())
            .map(|x| s.get(x, y).map(|c| c.ch).unwrap_or(' '))
            .collect();
        println!("|{row}|");
    }
    println!("+{bar}+\n");
}

fn main() {
    let mut app = Notepad::new();
    let mut runner = GuiRunner::new(30, 4);

    // Type "CIBOS", move to start, insert ">> ", tap to reposition, type "!".
    let events = [
        ch('C'),
        ch('I'),
        ch('B'),
        ch('O'),
        ch('S'),
        key(Key::Home),
        ch('>'),
        ch('>'),
        ch(' '),
        InputEvent::Pointer(Pointer::tap(8, 1)),
        ch('!'),
    ];

    let frames = runner.run_capturing(&mut app, events);
    print_surface(&format!("Initial frame (of {} total):", frames.len()), &frames[0]);
    print_surface("Final frame:", frames.last().unwrap());
    println!("text buffer: {:?}", app.text());
}
