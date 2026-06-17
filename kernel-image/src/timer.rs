//! System timer for the booted kernel (PIT channel 0 → IRQ0 → vector 0x20).
//!
//! Owns the monotonic tick counter the timer interrupt advances, and provides
//! the handler the IDT's vector-0x20 stub calls. A periodic tick is the
//! foundation for two things the system needs: a *wake/timeout source* (so the
//! kernel can wait for input or a deadline without busy-spinning or hanging),
//! and *preemption* (a later scheduler can switch tasks on a tick).
//!
//! The counter is a plain atomic — the handler only increments it and consumers
//! only read it, so no lock is needed. [`now_ticks`] reads it; [`wait_ticks`]
//! and [`wait_ticks_or`] block (via `hlt`, sleeping until the next interrupt)
//! until a deadline, the latter also returning early when a predicate becomes
//! true (e.g. a key arrived). Because `hlt` wakes on *any* interrupt and the
//! timer fires steadily, these always make progress and always terminate.

use core::sync::atomic::{AtomicU64, Ordering};

/// PIT tick rate. 100 Hz → a 10 ms tick: fine enough for responsive input and
/// timeouts, coarse enough to add negligible overhead.
pub const TICK_HZ: u32 = 100;

/// Monotonic tick count since the timer was started. Advanced by the IRQ0
/// handler; read by consumers.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// The Rust timer interrupt handler, called by the assembly stub at vector
/// 0x20. Advances the tick counter and acknowledges the PIC.
///
/// `#[no_mangle]` so the assembly stub can `call` it by name.
#[no_mangle]
pub extern "C" fn cibos_timer_irq() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: invoked only from the timer IRQ; EOI acknowledges vector 0x20.
    unsafe {
        crate::arch::pic_eoi(0x20);
    }
}

/// Current monotonic tick count.
#[must_use]
pub fn now_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Milliseconds elapsed since the timer started (derived from the tick count).
#[allow(dead_code)] // accessor for consumers (timeout displays, logs); not yet called
#[must_use]
pub fn now_millis() -> u64 {
    now_ticks() * 1000 / TICK_HZ as u64
}

/// Convert milliseconds to a tick count (rounding up so a small ms value still
/// waits at least one tick).
#[must_use]
pub fn millis_to_ticks(ms: u64) -> u64 {
    (ms * TICK_HZ as u64).div_ceil(1000).max(1)
}

/// Block until at least `ticks` timer ticks have elapsed, sleeping the CPU
/// between ticks with `hlt`. Requires interrupts enabled and the timer running.
///
/// # Safety
///
/// Interrupts must be enabled (IF set) and the timer IRQ unmasked/handled, or
/// this would sleep forever.
pub unsafe fn wait_ticks(ticks: u64) {
    let deadline = now_ticks() + ticks;
    while now_ticks() < deadline {
        core::arch::asm!("hlt", options(nomem, nostack));
    }
}

/// Block until `pred` returns true or `ticks` ticks elapse, whichever first.
/// Returns `true` if `pred` was satisfied, `false` on timeout. Sleeps with
/// `hlt` between interrupts, so it waits efficiently and always terminates (the
/// steady timer tick guarantees the deadline is reached even if `pred` never
/// holds).
///
/// This is the building block for blocking-but-bounded input: e.g. wait for a
/// keystroke up to a timeout. Interactive surfaces that should wait indefinitely
/// pass a very large `ticks`.
///
/// # Safety
///
/// As [`wait_ticks`].
pub unsafe fn wait_ticks_or(ticks: u64, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = now_ticks() + ticks;
    loop {
        if pred() {
            return true;
        }
        if now_ticks() >= deadline {
            return false;
        }
        core::arch::asm!("hlt", options(nomem, nostack));
    }
}

/// Block until `pred` becomes true, sleeping the CPU with `hlt` between PIT
/// ticks (no busy-spin). Unlike [`wait_ticks_or`] there is NO deadline — this is
/// for a TRULY blocking wait (e.g. an interactive `ReadKey` that must wait for a
/// real keystroke however long the user pauses). Honors the HIP "time as trigger,
/// not coordinator" stance: the CPU idles in `hlt` and only wakes on an IRQ.
///
/// # Safety
/// Interrupts must be enabled and the relevant IRQ (e.g. keyboard) live, or this
/// will sleep forever. Use only where an interrupt is guaranteed to fire.
pub unsafe fn wait_for(mut pred: impl FnMut() -> bool) {
    loop {
        if pred() {
            return;
        }
        core::arch::asm!("hlt", options(nomem, nostack));
    }
}
