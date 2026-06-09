//! # Key event queue
//!
//! A fixed-capacity ring buffer of [`KeyEvent`]s, sized at construction by a
//! const generic so it needs no allocator — suitable for the kernel's keyboard
//! interrupt handler to push into and a consumer (the console/shell) to drain.
//!
//! The queue itself is not synchronized; the kernel wraps it in a spinlock so
//! the IRQ handler and the consumer do not race. When full, the oldest event is
//! dropped in favor of the newest (a keystroke backlog that no one is draining
//! is stale; the most recent input is the useful one). This keeps `push`
//! non-blocking and constant-time, which matters in an interrupt context.

use crate::KeyEvent;

/// A bounded ring buffer of key events with capacity `N`.
#[derive(Debug)]
pub struct KeyQueue<const N: usize> {
    buf: [Option<KeyEvent>; N],
    head: usize,
    len: usize,
}

impl<const N: usize> Default for KeyQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> KeyQueue<N> {
    /// A new, empty queue. `N` must be at least 1.
    #[must_use]
    pub const fn new() -> Self {
        assert!(N >= 1, "KeyQueue capacity must be >= 1");
        KeyQueue {
            buf: [None; N],
            head: 0,
            len: 0,
        }
    }

    /// Number of queued events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the queue is at capacity.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.len == N
    }

    /// Push an event. If the queue is full, the oldest event is dropped to make
    /// room (returns `true` if an event was dropped). Constant-time.
    pub fn push(&mut self, ev: KeyEvent) -> bool {
        let tail = (self.head + self.len) % N;
        if self.len == N {
            // Full: overwrite the oldest (at head) and advance head.
            self.buf[self.head] = Some(ev);
            self.head = (self.head + 1) % N;
            true
        } else {
            self.buf[tail] = Some(ev);
            self.len += 1;
            false
        }
    }

    /// Pop the oldest event, or `None` if empty.
    pub fn pop(&mut self) -> Option<KeyEvent> {
        if self.len == 0 {
            return None;
        }
        let ev = self.buf[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Key;

    #[test]
    fn fifo_order() {
        let mut q = KeyQueue::<4>::new();
        assert!(q.is_empty());
        assert!(!q.push(KeyEvent::ch('a')));
        assert!(!q.push(KeyEvent::ch('b')));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(KeyEvent::ch('a')));
        assert_eq!(q.pop(), Some(KeyEvent::ch('b')));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn wraps_around() {
        let mut q = KeyQueue::<3>::new();
        for c in ['a', 'b', 'c'] {
            q.push(KeyEvent::ch(c));
        }
        assert_eq!(q.pop(), Some(KeyEvent::ch('a')));
        assert!(!q.push(KeyEvent::ch('d'))); // reuses the freed slot, not full
        assert_eq!(q.pop(), Some(KeyEvent::ch('b')));
        assert_eq!(q.pop(), Some(KeyEvent::ch('c')));
        assert_eq!(q.pop(), Some(KeyEvent::ch('d')));
    }

    #[test]
    fn full_drops_oldest() {
        let mut q = KeyQueue::<2>::new();
        assert!(!q.push(KeyEvent::ch('a')));
        assert!(!q.push(KeyEvent::ch('b')));
        assert!(q.is_full());
        // Pushing into a full queue drops the oldest ('a').
        assert!(q.push(KeyEvent::ch('c')));
        assert_eq!(q.pop(), Some(KeyEvent::ch('b')));
        assert_eq!(q.pop(), Some(KeyEvent::ch('c')));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn preserves_named_keys_and_mods() {
        let mut q = KeyQueue::<2>::new();
        q.push(KeyEvent::new(Key::Enter));
        assert_eq!(q.pop(), Some(KeyEvent::new(Key::Enter)));
    }
}
