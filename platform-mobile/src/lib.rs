//! # Mobile platform
//!
//! The mobile platform reuses the shared input model and the GUI [`Surface`],
//! adding touch **gesture recognition**: it turns the low-level stream of
//! [`Pointer`] press/move/release events (what a touch panel produces) into
//! high-level [`Gesture`]s — taps and swipes — that touch apps actually want.
//!
//! A [`TouchApp`] implements `render` and `on_gesture`; the [`MobileRunner`]
//! feeds raw pointer events through a [`GestureRecognizer`] and delivers the
//! resulting gestures, rendering the surface after each, exactly like the GUI
//! runner but gesture-driven.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use platform_gui::{Flow, Surface};
use cibos_input::{Button, Pointer, PointerAction};

/// Swipe direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Toward smaller x.
    Left,
    /// Toward larger x.
    Right,
    /// Toward smaller y.
    Up,
    /// Toward larger y.
    Down,
}

/// A recognized touch gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// A press and release at roughly the same spot.
    Tap {
        /// Column.
        x: u16,
        /// Row.
        y: u16,
    },
    /// A press, drag, and release covering distance in a dominant direction.
    Swipe {
        /// Where the swipe began.
        from: (u16, u16),
        /// Where it ended.
        to: (u16, u16),
        /// Dominant direction.
        direction: Direction,
    },
}

/// Minimum cells moved for a gesture to count as a swipe rather than a tap.
pub const SWIPE_THRESHOLD: u16 = 2;

/// Turns a stream of pointer events into gestures.
#[derive(Default)]
pub struct GestureRecognizer {
    start: Option<(u16, u16)>,
    last: (u16, u16),
}

impl GestureRecognizer {
    /// A fresh recognizer.
    #[must_use]
    pub fn new() -> Self {
        GestureRecognizer::default()
    }

    /// Feed one pointer event; returns a gesture when one completes (on
    /// release). Only the primary button / touch is recognized.
    pub fn feed(&mut self, p: Pointer) -> Option<Gesture> {
        if p.button != Button::Primary {
            return None;
        }
        match p.action {
            PointerAction::Press => {
                self.start = Some((p.x, p.y));
                self.last = (p.x, p.y);
                None
            }
            PointerAction::Move => {
                if self.start.is_some() {
                    self.last = (p.x, p.y);
                }
                None
            }
            PointerAction::Release => {
                let start = self.start.take()?;
                let end = (p.x, p.y);
                let dx = i32::from(end.0) - i32::from(start.0);
                let dy = i32::from(end.1) - i32::from(start.1);
                let dist = dx.unsigned_abs().max(dy.unsigned_abs());
                if dist >= u32::from(SWIPE_THRESHOLD) {
                    let direction = if dx.unsigned_abs() >= dy.unsigned_abs() {
                        if dx > 0 {
                            Direction::Right
                        } else {
                            Direction::Left
                        }
                    } else if dy > 0 {
                        Direction::Down
                    } else {
                        Direction::Up
                    };
                    Some(Gesture::Swipe {
                        from: start,
                        to: end,
                        direction,
                    })
                } else {
                    Some(Gesture::Tap {
                        x: start.0,
                        y: start.1,
                    })
                }
            }
        }
    }
}

/// A gesture-driven touch application.
pub trait TouchApp {
    /// The app's name.
    fn name(&self) -> &str;

    /// React to a gesture; return [`Flow::Exit`] to stop.
    fn on_gesture(&mut self, gesture: Gesture) -> Flow;

    /// Paint current state. The runner clears the surface first.
    fn render(&self, surface: &mut Surface);
}

/// Drives a [`TouchApp`] over a virtual touch screen.
pub struct MobileRunner {
    surface: Surface,
    recognizer: GestureRecognizer,
}

impl MobileRunner {
    /// Create a runner with a screen of the given size.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        MobileRunner {
            surface: Surface::new(width, height),
            recognizer: GestureRecognizer::new(),
        }
    }

    fn render(&mut self, app: &dyn TouchApp) {
        self.surface.clear();
        app.render(&mut self.surface);
    }

    /// Render the initial frame, then feed raw pointer events; each completed
    /// gesture is delivered to the app and the surface re-rendered. Stops on
    /// [`Flow::Exit`] or when events run out. Returns the final screen.
    pub fn run<I>(&mut self, app: &mut dyn TouchApp, pointers: I) -> Surface
    where
        I: IntoIterator<Item = Pointer>,
    {
        self.render(app);
        for p in pointers {
            if let Some(gesture) = self.recognizer.feed(p) {
                let flow = app.on_gesture(gesture);
                self.render(app);
                if flow == Flow::Exit {
                    break;
                }
            }
        }
        self.surface.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_recognized() {
        let mut r = GestureRecognizer::new();
        assert_eq!(r.feed(Pointer::tap(5, 5)), None); // press
        let g = r.feed(Pointer {
            x: 5,
            y: 5,
            action: PointerAction::Release,
            button: Button::Primary,
        });
        assert_eq!(g, Some(Gesture::Tap { x: 5, y: 5 }));
    }

    #[test]
    fn horizontal_swipe_recognized() {
        let mut r = GestureRecognizer::new();
        r.feed(Pointer::tap(2, 5));
        r.feed(Pointer {
            x: 6,
            y: 5,
            action: PointerAction::Move,
            button: Button::Primary,
        });
        let g = r.feed(Pointer {
            x: 10,
            y: 5,
            action: PointerAction::Release,
            button: Button::Primary,
        });
        assert_eq!(
            g,
            Some(Gesture::Swipe {
                from: (2, 5),
                to: (10, 5),
                direction: Direction::Right
            })
        );
    }

    #[test]
    fn vertical_swipe_up() {
        let mut r = GestureRecognizer::new();
        r.feed(Pointer::tap(5, 10));
        let g = r.feed(Pointer {
            x: 5,
            y: 3,
            action: PointerAction::Release,
            button: Button::Primary,
        });
        assert!(matches!(
            g,
            Some(Gesture::Swipe {
                direction: Direction::Up,
                ..
            })
        ));
    }

    // A tiny touch app: a counter that increments on tap, and switches a label
    // on swipe; exits on a left swipe.
    #[derive(Default)]
    struct Demo {
        taps: u32,
        label: &'static str,
    }
    impl TouchApp for Demo {
        fn name(&self) -> &str {
            "demo"
        }
        fn on_gesture(&mut self, g: Gesture) -> Flow {
            match g {
                Gesture::Tap { .. } => self.taps += 1,
                Gesture::Swipe { direction, .. } => {
                    if direction == Direction::Left {
                        return Flow::Exit;
                    }
                    self.label = "swiped";
                }
            }
            Flow::Continue
        }
        fn render(&self, surface: &mut Surface) {
            surface.write_str(0, 0, &format!("taps: {} {}", self.taps, self.label));
        }
    }

    #[test]
    fn touch_app_over_runner() {
        let mut app = Demo::default();
        let mut runner = MobileRunner::new(20, 2);
        let release = |x, y| Pointer {
            x,
            y,
            action: PointerAction::Release,
            button: Button::Primary,
        };
        let screen = runner.run(
            &mut app,
            [
                Pointer::tap(1, 0),
                release(1, 0), // tap -> taps=1
                Pointer::tap(2, 0),
                release(2, 0), // tap -> taps=2
            ],
        );
        assert_eq!(app.taps, 2);
        assert_eq!(screen.row_text(0), "taps: 2");
    }
}
