# CIBOS platforms

A *platform* adapts a CIBOS application to a class of device: it provides the
input/output surfaces and a runner that drives the app against the kernel. All
platforms share the same SDK (channels, spawning, timers, filesystem, Lattice)
and the same input model; they differ only in how a human (or none) interacts.

| Platform | Crate | Surface | Input | Runner |
|---|---|---|---|---|
| **CLI** (terminal/desktop) | `platform-cli` | line console | typed lines | `CliRunner` |
| **GUI** (desktop) | `platform-gui` | cell-grid `Surface` | keyboard + pointer | `GuiRunner` |
| **Mobile** (touch) | `platform-mobile` | cell-grid `Surface` | touch gestures (tap/swipe) | `MobileRunner` |
| **Server** (headless daemon) | `platform-server` | none | none | `ServerRunner` |

* **CLI** and **GUI** target a desktop with a keyboard; GUI adds a 2-D display
  and pointer.
* **Mobile** reuses the GUI `Surface` and the shared input model, recognizing
  touch **gestures** (`Tap`, `Swipe`) from raw pointer events.
* **Server** has no UI at all — it hosts long-running services (e.g. Vane) that
  use channels, the filesystem, and the Lattice, and runs their tasks to
  completion under the same scheduler.

The input model (`cibos-input`) is shared: `Key`/`Modifiers`/`KeyEvent` for
keyboards, `Pointer`/`Button`/`PointerAction` for mice and touch (a tap is a
primary-button pointer). Hardware drivers translate raw device reports into
these events; apps only ever see them, so the same app logic runs on a virtual
backend (tests) or real hardware.
