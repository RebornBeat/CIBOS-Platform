//! CIBOS login application (`.capp`).
//!
//! Runs the **real** login gate on the kernel: it drives
//! [`login::run_login`] — the same authentication flow the host uses — against a
//! [`cibos_app::SyscallConsole`] (Log + ReadKey syscalls) and an
//! [`accounts::Accounts`] registry loaded from the kernel filesystem (CIBOSFS).
//! There is no reimplemented login logic here; this app is the glue that loads
//! credentials from disk, runs the shared gate, and persists new profiles.
//!
//! Persistence: each profile's password verifier is stored as a
//! [`shared::crypto::credential::CredentialRecord`] under `/etc/passwd.d/<name>`.
//! Records are loaded into the registry via [`accounts::Accounts::enroll_password_record`]
//! and written back via [`accounts::Accounts::password_record_for`], so the
//! on-disk format and the in-memory registry are the single shared format.
//!
//! Flow: prompt for a profile. If `/etc/passwd.d/<name>` does not exist, run
//! **create-user** (new password twice, CSPRNG salt, enroll, persist).
//! Otherwise load the record and run the shared [`login::run_login`] gate.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use cibos_app::{console, fs, input, rand, SyscallConsole};
use shared::crypto::credential::{CredentialRecord, CREDENTIAL_RECORD_LEN};
use shared::BoundaryId;

cibos_app::entry!(main);
cibos_app::default_panic_handler!();

/// Maximum login attempts before the gate gives up.
const MAX_ATTEMPTS: u32 = 3;

/// The boundary a successfully-authenticated profile activates. A real system
/// allocates these per profile; this app uses a fixed boundary for its single
/// profile slot (the registry keys profiles by boundary).
const PROFILE_BOUNDARY: BoundaryId = BoundaryId::new(1);

fn cred_path(name: &str) -> String {
    format!("/etc/passwd.d/{name}")
}

/// Ensure the credential directory exists (ignore "already exists").
fn ensure_dirs() {
    let _ = fs::mkdir(b"/etc");
    let _ = fs::mkdir(b"/etc/passwd.d");
}

/// Load a profile's persisted credential record from CIBOSFS, if present.
fn load_record(name: &str) -> Option<CredentialRecord> {
    let mut buf = [0u8; CREDENTIAL_RECORD_LEN];
    let n = fs::read_into(cred_path(name).as_bytes(), &mut buf).ok()?;
    CredentialRecord::from_bytes(&buf[..n])
}

/// Create a new profile: prompt for a password (twice), generate a salt, enroll
/// it in the registry, and persist the exported record. Returns true on success.
fn create_user(name: &str) -> bool {
    console::println(&format!("creating new profile '{name}'"));
    console::print("new password: ");
    let pw1 = input::read_line(true);
    console::print("confirm password: ");
    let pw2 = input::read_line(true);
    if pw1 != pw2 {
        console::println("passwords do not match");
        return false;
    }
    if pw1.is_empty() {
        console::println("password must not be empty");
        return false;
    }
    let Ok(salt) = rand::salt32() else {
        console::println("no entropy source available");
        return false;
    };

    // Enroll in the shared registry, then export the canonical record to persist.
    let mut acc = accounts::Accounts::new();
    acc.enroll_password(name, PROFILE_BOUNDARY, salt, pw1.as_bytes());
    let Some(rec) = acc.password_record_for(PROFILE_BOUNDARY) else {
        console::println("internal error: no record for new profile");
        return false;
    };
    match fs::write(cred_path(name).as_bytes(), &rec.to_bytes()) {
        Ok(_) => {
            console::println(&format!("profile '{name}' created"));
            true
        }
        Err(_) => {
            console::println("could not write credentials");
            false
        }
    }
}

fn main() -> u64 {
    ensure_dirs();

    console::print("profile: ");
    let name_raw = input::read_line(false);
    let name = name_raw.trim();
    if name.is_empty() {
        console::println("no profile given");
        return 1;
    }

    // First run for a profile: create it.
    let Some(record) = load_record(name) else {
        return if create_user(name) { 0 } else { 1 };
    };

    // Existing profile: load it into a registry and run the SHARED login gate
    // for this known profile (we already prompted for and resolved the name, so
    // use the per-profile entry point rather than prompting for it again).
    let mut acc = accounts::Accounts::new();
    acc.enroll_password_record(name, PROFILE_BOUNDARY, &record);

    let console = SyscallConsole::new();
    match login::run_login_for(&console, &acc, PROFILE_BOUNDARY, MAX_ATTEMPTS) {
        login::LoginResult::Granted(_session) => 0,
        login::LoginResult::Denied => 1,
        login::LoginResult::Aborted => 2,
    }
}
