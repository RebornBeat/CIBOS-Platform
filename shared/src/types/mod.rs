//! # Core Shared Type Definitions
//!
//! The cross-cutting data vocabulary used throughout the CIBIOS/CIBOS/HIP
//! system. Every type here is `no_std`-compatible. Types that cross the
//! firmware→kernel handoff boundary carry an explicit, stable representation
//! (`#[repr(u32)]` enums with `TryFrom`, `#[repr(C)]` structs).

pub mod authentication;
pub mod config;
pub mod error;
pub mod hardware;
pub mod isolation;
pub mod profiles;
pub mod time;
