//! The platform root **refuses a process with no absolute home** — an error the
//! caller fails closed on, never a panic and never a shared `/tmp` base
//! (`docs/goal/architecture/account-data-plane.md` § The account store →
//! Placement).
//!
//! Its own test binary because it sets `HOME` for the whole process: no other
//! test may read the environment concurrently. A relative `HOME` is the form
//! that reaches the refusal on every unix (macOS resolves an unset one through
//! the passwd database).

#![cfg(all(unix, not(target_arch = "wasm32")))]

use fauna_account_store::root::{NO_HOME_REFUSAL, StoreRoot, platform_state_base};

#[test]
fn a_relative_home_is_refused_by_the_state_base_and_the_store_root() {
    // SAFETY: the only test in this binary, so no other thread reads or writes
    // the environment while it changes.
    unsafe {
        std::env::set_var("HOME", "relative-home");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    let refusal = platform_state_base().expect_err("a relative HOME must be refused");
    assert_eq!(format!("{refusal:#}"), NO_HOME_REFUSAL);
    let refusal = StoreRoot::platform().expect_err("the store root refuses with it");
    assert_eq!(format!("{refusal:#}"), NO_HOME_REFUSAL);
}
