//! Keyboard input for the booted kernel.
//!
//! Owns the global PS/2 scancode decoder and the key-event queue, and provides
//! the interrupt handler the IDT's vector-0x21 stub calls. The handler reads the
//! controller's data port, feeds the byte to the portable
//! [`ScancodeDecoder`](cibos_input::ScancodeDecoder), enqueues any resulting
//! [`KeyEvent`](cibos_input::KeyEvent), and acknowledges the PIC.
//!
//! Consumers (the console read path, and later a shell/login surface) drain the
//! queue with [`poll_key`]. The decoder and queue live behind a
//! [`SpinLock`](cibos_kernel::sync::SpinLock) so the IRQ handler and a consumer
//! do not race.

use cibos_input::{KeyEvent, KeyQueue, ScancodeDecoder};
use cibos_kernel::sync::SpinLock;

/// Capacity of the kernel key queue. 64 buffered keystrokes is ample for an
/// interactive console; excess is dropped oldest-first by [`KeyQueue`].
const QUEUE_CAP: usize = 64;

struct KeyboardState {
    decoder: ScancodeDecoder,
    queue: KeyQueue<QUEUE_CAP>,
    /// Total scancodes seen (diagnostic; proves the IRQ is firing).
    bytes_seen: u64,
}

static KEYBOARD: SpinLock<KeyboardState> = SpinLock::new(KeyboardState {
    decoder: ScancodeDecoder::new(),
    queue: KeyQueue::new(),
    bytes_seen: 0,
});

/// The Rust keyboard interrupt handler, called by the assembly stub at vector
/// 0x21. Reads one scancode, decodes it, enqueues any produced key event, and
/// sends EOI to the PIC.
///
/// `#[no_mangle]` so the assembly stub can `call` it by name.
#[no_mangle]
pub extern "C" fn cibos_keyboard_irq() {
    // SAFETY: invoked only from the keyboard IRQ; reading 0x60 consumes the
    // byte the controller raised the IRQ for, and `pic_eoi` acknowledges it.
    unsafe {
        let scancode = crate::arch::read_keyboard_data();
        {
            let mut kb = KEYBOARD.lock();
            kb.bytes_seen += 1;
            if let Some(ev) = kb.decoder.push(scancode) {
                kb.queue.push(ev);
            }
        }
        crate::arch::pic_eoi(0x21);
    }
}

/// Pop the next buffered key event, or `None` if the queue is empty.
#[must_use]
pub fn poll_key() -> Option<KeyEvent> {
    KEYBOARD.lock().queue.pop()
}

/// Number of scancodes the handler has processed (diagnostic).
#[must_use]
pub fn scancodes_seen() -> u64 {
    KEYBOARD.lock().bytes_seen
}
