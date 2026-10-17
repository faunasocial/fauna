//! The install-scoped trust home **refuses a process with no absolute home** —
//! `None` for the caller to log and fail closed on (the in-memory pin store
//! stays installed), never a panic and never a `/tmp` pin file another local
//! account could pre-plant (`docs/goal/architecture/account-data-plane.md`
//! § The account store → Placement).
//!
//! Its own test binary because it sets `HOME` for the whole process: no other
//! test may read the environment concurrently.

#![cfg(unix)]

#[test]
fn a_relative_home_is_refused_by_the_trust_home() {
    // SAFETY: the only test in this binary, so no other thread reads or writes
    // the environment while it changes.
    unsafe {
        std::env::set_var("HOME", "relative-home");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    assert_eq!(
        fauna_anon_client::cert_binding::install_scoped_trust_home(),
        None
    );
}
