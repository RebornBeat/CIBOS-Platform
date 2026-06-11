//! Freestanding runtime entry for a CIBOS application.
//!
//! The kernel enters a `.capp` at `_start` in ring 3 with the application heap
//! described in `rdi` (base) and `rsi` (size). An application uses the
//! [`entry!`](crate::entry) macro to supply its `main`; the macro emits a
//! `_start` that captures those registers, installs the heap so `alloc` works,
//! runs `main`, and exits with its return code. It also provides a default
//! `#[panic_handler]` that reports the panic location to the console and exits
//! with a non-zero code.
//!
//! This module exists only on the bare application target (`target_os = "none"`)
//! because it defines a global `_start` and panic handler.

/// Install the heap from the entry-register values. Called by the generated
/// `_start`; not intended for direct use.
///
/// # Safety
///
/// Must be called exactly once, at startup, with the kernel-provided heap base
/// and size.
pub unsafe fn init_heap_from_regs(heap_base: u64, heap_size: u64) {
    if heap_base != 0 && heap_size != 0 {
        // SAFETY: the kernel mapped [heap_base, heap_base+heap_size) writable for
        // this application; called once before any allocation.
        unsafe { crate::heap::init(heap_base as usize, heap_size as usize) };
    }
}

/// Define the application entry point.
///
/// ```ignore
/// cibos_app::entry!(main);
/// fn main() -> u64 { /* ... */ 0 }
/// ```
///
/// The macro emits a `#[no_mangle] _start` that captures the heap registers,
/// initializes the allocator, calls `main`, and exits with its return value.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        pub unsafe extern "C" fn _start(heap_base: u64, heap_size: u64) -> ! {
            // SAFETY: startup, called once by the kernel loader.
            unsafe { $crate::rt::init_heap_from_regs(heap_base, heap_size) };
            let code: u64 = $main();
            $crate::console::exit(code)
        }
    };
}

/// A default panic handler applications can opt into with
/// [`default_panic_handler!`](crate::default_panic_handler). Reports the panic
/// and exits with code 101 (matching the conventional Rust panic exit code).
pub fn report_panic(info: &core::panic::PanicInfo) -> ! {
    crate::console::print("  [panic] ");
    if let Some(loc) = info.location() {
        crate::console::print(loc.file());
    }
    crate::console::println("");
    crate::console::exit(101)
}

/// Emit the default `#[panic_handler]` (delegates to [`rt::report_panic`]).
#[macro_export]
macro_rules! default_panic_handler {
    () => {
        #[panic_handler]
        fn __cibos_panic(info: &core::panic::PanicInfo) -> ! {
            $crate::rt::report_panic(info)
        }
    };
}
