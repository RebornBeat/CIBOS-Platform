//! # Login gate
//!
//! A console login gate: it prompts for a profile name and password,
//! authenticates against the [`Accounts`] registry, and on success yields a
//! [`Session`] — the activated isolation boundary. A caller runs this before
//! launching the shell, so the console is gated behind authentication.
//!
//! Only the password factor is handled here, since a console has only a
//! keyboard; profiles requiring a key device are reported as needing one (the
//! device interaction belongs to a platform with the wired port available).

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::format;
use alloc::string::ToString;

use accounts::{Accounts, Credential, Session};
use cibos_console::Console;
use shared::AuthenticationMethod;

/// Outcome of a login attempt sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginResult {
    /// Authentication succeeded.
    Granted(Session),
    /// All attempts were exhausted without success.
    Denied,
    /// The console was closed (no more input).
    Aborted,
}

/// Run the login gate over `console`, allowing up to `max_attempts` tries.
///
/// Each attempt prompts for a profile name and password. Returns as soon as a
/// profile authenticates, or after the attempts are exhausted.
#[must_use]
pub fn run_login(console: &dyn Console, accounts: &Accounts, max_attempts: u32) -> LoginResult {
    console.write_line("== CIBOS login ==");
    for remaining in (1..=max_attempts).rev() {
        console.write_line("profile:");
        let Some(name) = console.read_line() else {
            return LoginResult::Aborted;
        };
        let name = name.trim().to_string();

        let Some(boundary) = accounts.find_by_name(&name) else {
            console.write_line("unknown profile");
            continue;
        };

        // A key-device profile cannot complete on a console-only gate.
        if accounts.method_for(boundary) == Some(AuthenticationMethod::PhysicalKeyDevice) {
            console.write_line("this profile requires a physical key device");
            continue;
        }

        // Resolved the profile; run the password gate for it, giving the user
        // the remaining attempts. A bad password here ends the sequence (the
        // profile was correctly identified), matching a single-prompt gate.
        return run_login_for(console, accounts, boundary, remaining);
    }
    LoginResult::Denied
}

/// Run the password gate for an already-resolved profile `boundary`, allowing up
/// to `max_attempts` password tries. This is the per-profile core of
/// [`run_login`]; callers that already know which profile is being authenticated
/// (e.g. the on-kernel login app, which prompted for the name to load the
/// profile's credential from disk) use this directly to avoid prompting for the
/// name twice. Returns as soon as the password verifies, or after the attempts
/// are exhausted.
#[must_use]
pub fn run_login_for(
    console: &dyn Console,
    accounts: &Accounts,
    boundary: shared::BoundaryId,
    max_attempts: u32,
) -> LoginResult {
    if accounts.method_for(boundary) == Some(AuthenticationMethod::PhysicalKeyDevice) {
        console.write_line("this profile requires a physical key device");
        return LoginResult::Denied;
    }
    for remaining in (1..=max_attempts).rev() {
        console.write_line("password:");
        let Some(password) = console.read_secret() else {
            return LoginResult::Aborted;
        };

        match accounts.open_session(boundary, Credential::Password(password.trim_end().as_bytes())) {
            Some(session) => {
                console.write_line(&format!("welcome, {}", session.profile));
                return LoginResult::Granted(session);
            }
            None => {
                if remaining > 1 {
                    console.write_line(&format!(
                        "access denied ({} attempt(s) left)",
                        remaining - 1
                    ));
                } else {
                    console.write_line("access denied");
                }
            }
        }
    }
    LoginResult::Denied
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_cli::CaptureConsole;
    use shared::BoundaryId;
    use std::sync::Arc;

    const SALT: [u8; 32] = [0x22; 32];

    fn accounts_with_operator() -> Accounts {
        let mut acc = Accounts::new();
        acc.enroll_password("operator", BoundaryId::new(1), SALT, b"open sesame");
        acc
    }

    fn console(lines: &[&str]) -> Arc<CaptureConsole> {
        Arc::new(CaptureConsole::new(lines.iter().map(|s| s.to_string())))
    }

    #[test]
    fn successful_login() {
        let acc = accounts_with_operator();
        let c = console(&["operator", "open sesame"]);
        let result = run_login(&*c, &acc, 3);
        match result {
            LoginResult::Granted(s) => {
                assert_eq!(s.profile, "operator");
                assert_eq!(s.boundary, BoundaryId::new(1));
            }
            other => panic!("expected Granted, got {other:?}"),
        }
        assert!(c.output_text().contains("welcome, operator"));
    }

    #[test]
    fn retry_then_succeed() {
        let acc = accounts_with_operator();
        let c = console(&["operator", "wrong", "operator", "open sesame"]);
        assert!(matches!(run_login(&*c, &acc, 3), LoginResult::Granted(_)));
        assert!(c.output_text().contains("access denied"));
    }

    #[test]
    fn denied_after_attempts() {
        let acc = accounts_with_operator();
        let c = console(&["operator", "no", "operator", "nope"]);
        assert_eq!(run_login(&*c, &acc, 2), LoginResult::Denied);
    }

    #[test]
    fn unknown_profile_reported() {
        let acc = accounts_with_operator();
        let c = console(&["ghost", "x", "operator", "open sesame"]);
        assert!(matches!(run_login(&*c, &acc, 3), LoginResult::Granted(_)));
        assert!(c.output_text().contains("unknown profile"));
    }
}
