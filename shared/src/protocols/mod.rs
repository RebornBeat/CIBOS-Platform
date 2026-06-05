//! # Protocols
//!
//! Wire and boundary protocols shared across the system: the bootloader→CIBIOS
//! [`boot`] contract, the firmware→kernel [`handoff`] contract, the [`ipc`]
//! kernel/runtime interface and channel vocabulary, and the [`authentication`]
//! exchange envelopes.

pub mod authentication;
pub mod boot;
pub mod handoff;
pub mod ipc;
