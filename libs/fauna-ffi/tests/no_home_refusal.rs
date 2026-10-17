//! The UniFFI exports **refuse a process with no absolute home** — they throw
//! (or, without an error channel, report what was not reached), never panic
//! across the FFI boundary and never resolve a shared `/tmp` base
//! (`docs/goal/architecture/account-data-plane.md` § The account store →
//! Placement).
//!
//! Its own test binary because it sets `HOME` for the whole process: no other
//! test may read the environment concurrently.

#![cfg(unix)]

use fauna_core::platform_ids::NO_HOME_REFUSAL;

#[test]
fn a_relative_home_is_refused_by_every_export_that_resolves_the_platform_root() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary, so no other thread reads or writes
    // the environment while it changes.
    unsafe {
        std::env::set_var("HOME", "relative-home");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    let refused = |e: fauna_ffi::FfiError| e.to_string().contains(NO_HOME_REFUSAL);
    assert!(refused(fauna_ffi::platform_state_base().unwrap_err()));
    assert!(refused(fauna_ffi::install_scoped_trust_home().unwrap_err()));
    assert!(refused(
        fauna_ffi::backup_audit_local_agent_state_dir("ab".repeat(32)).unwrap_err()
    ));
    assert!(refused(
        fauna_ffi::account_state_erase_scope(
            tmp.path().display().to_string(),
            "ab".repeat(32),
            None,
        )
        .unwrap_err()
    ));

    // No error channel: the unreached store root is a SURVIVOR, so the sign-out
    // can never paint a clean device over it.
    let sweep = fauna_ffi::account_state_erase_all_scopes(tmp.path().display().to_string(), None);
    assert_eq!(sweep.survivors.len(), 1, "{:?}", sweep.survivors);
    assert!(sweep.survivors[0].contains(NO_HOME_REFUSAL));

    // A sandboxed shell's container needs no home: it still resolves.
    fauna_ffi::account_state_erase_scope(
        tmp.path().display().to_string(),
        "ab".repeat(32),
        Some(tmp.path().join("container").display().to_string()),
    )
    .expect("a container root resolves without a home");
}
