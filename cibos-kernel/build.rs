//! Validate the kernel feature combination at compile time.
//!
//! ADR-007 requires that a profile binary contain only its profile's
//! mechanisms. The "requires" relationships are expressed as Cargo feature
//! dependencies in `Cargo.toml` (enabling a dependent pulls its prerequisite);
//! the "conflict" relationships and the single-bundle rule cannot be expressed
//! that way, so they are checked here and fail the build with a clear message.

fn on(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn main() {
    let full_fairness = on("CARGO_FEATURE_FULL_FAIRNESS");
    let per_lane = on("CARGO_FEATURE_PER_LANE_WEIGHTS");
    let rtro = on("CARGO_FEATURE_RTRO");
    let crypto_ipc = on("CARGO_FEATURE_CRYPTOGRAPHIC_IPC");
    let lightweight = on("CARGO_FEATURE_LIGHTWEIGHT_HANDSHAKE");

    if full_fairness && per_lane {
        panic!(
            "cibos-kernel feature conflict: `full-fairness` and `per-lane-weights` \
             are incompatible scheduling models (proportional execution-time \
             fairness vs application-assigned weights)."
        );
    }
    if rtro && lightweight {
        panic!(
            "cibos-kernel feature conflict: `rtro` and `lightweight-handshake` are \
             incompatible (resource obfuscation is meaningless where IPC is \
             trust-on-first-use)."
        );
    }
    if crypto_ipc && lightweight {
        panic!(
            "cibos-kernel feature conflict: `cryptographic-ipc` and \
             `lightweight-handshake` are mutually exclusive (IPC is one security \
             mode)."
        );
    }

    let active: Vec<&str> = [
        ("profile-maximum-isolation", "CARGO_FEATURE_PROFILE_MAXIMUM_ISOLATION"),
        ("profile-balanced", "CARGO_FEATURE_PROFILE_BALANCED"),
        ("profile-performance", "CARGO_FEATURE_PROFILE_PERFORMANCE"),
        ("profile-compute", "CARGO_FEATURE_PROFILE_COMPUTE"),
    ]
    .into_iter()
    .filter(|(_, env)| on(env))
    .map(|(name, _)| name)
    .collect();

    if active.len() > 1 {
        panic!(
            "cibos-kernel: at most one operational profile bundle may be enabled; \
             got {active:?}. Build with `--no-default-features` and a single \
             `profile-*` feature."
        );
    }
}
