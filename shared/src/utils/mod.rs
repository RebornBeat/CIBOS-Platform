//! # Utilities
//!
//! Small, dependency-light helpers shared across crates: a bounds-checked
//! [`serialization`] byte cursor for wire encoding, and [`validation`] helpers
//! that centralize value-range rules. Both are `no_std` and allocation-free.
//!
//! Deliberately *not* here: logging and configuration *parsing*. Logging sinks
//! are platform-specific (a serial port in firmware, a system log in the
//! kernel, the standard library above the SDK line) and configuration parsing
//! pulls in `std`-side format crates, so both live with their consumers rather
//! than being forced into this `no_std` foundation.

pub mod serialization;
pub mod validation;
