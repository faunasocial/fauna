//! The e2e escrow-trust seed: `FAUNA_E2E_TRUST_NEST_IDENTITY`, read by the two
//! doors that stand in for a TOFU pin — `trust::trusted_escrow_holders` (the
//! generation plane's escrow-holder set, `account-data-plane.md` § The
//! ratified decisions) and `trust::pinned_nest_custodian_identity` (the
//! identity a NEST-form custody accept binds).
//!
//! `e2e-automation-surface-gating.md` § The e2e trust seed owns the contract.
//! The pin it stands in for is **per nest authority** (`authority_of`), so the
//! seed is too: `<nest url>=<64 hex>` entries joined by `,`. The unkeyed seed
//! (one bare identity) was trusted for EVERY nest, so an e2e app pointed at a
//! dedicated nest trusted the launch nest's key there — each generation mint
//! ran to a post-deposit refusal, and a custody accept would have bound the
//! wrong nest's identity. That is the regression this file pins.
//!
//! **Why this is its own test target, and one test function.** The seed is
//! read from the *process* environment, and `cargo test` runs a binary's tests
//! on concurrent threads — the same reasoning as `fauna-mail`'s
//! `imap_client_trust_seed.rs`. One process, one test, phases in order.
#![cfg(any(debug_assertions, feature = "test-helpers"))]

use fauna_anon_client::trust::{
    pin_identity_for_test, pinned_nest_custodian_identity, trusted_escrow_holders,
};

/// Spelled out rather than shared: the contract is with the Python harness
/// (`conftest._apply_r14_trust_env`), which knows only the string.
const SEED_ENV: &str = "FAUNA_E2E_TRUST_NEST_IDENTITY";

const LAUNCH_NEST: &str = "http://127.0.0.1:4242";
const DEDICATED_NEST: &str = "http://127.0.0.1:5353";
const LAUNCH_ID: [u8; 32] = [0xab; 32];
const DEDICATED_ID: [u8; 32] = [0xcd; 32];

fn hex(id: [u8; 32]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// # Safety
///
/// Single-threaded by construction: the only caller is the one test below, and
/// this file is its own test binary so nothing else in the process reads the
/// environment concurrently (see the module docs).
fn set_seed(value: Option<&str>) {
    unsafe {
        match value {
            Some(v) => std::env::set_var(SEED_ENV, v),
            None => std::env::remove_var(SEED_ENV),
        }
    }
}

#[test]
fn the_trust_seed_names_an_identity_for_its_own_nest_and_no_other() {
    // ── Unseeded: the production posture. No pin, no seed → nothing trusted.
    set_seed(None);
    assert!(trusted_escrow_holders(LAUNCH_NEST).is_empty());
    assert_eq!(pinned_nest_custodian_identity(LAUNCH_NEST), None);

    // ── One keyed entry: trusted for its own nest, under any spelling of the
    // URL that names the same authority (the pin store's own key).
    set_seed(Some(&format!("{LAUNCH_NEST}={}", hex(LAUNCH_ID))));
    assert_eq!(trusted_escrow_holders(LAUNCH_NEST), vec![LAUNCH_ID]);
    assert_eq!(
        trusted_escrow_holders("ws://127.0.0.1:4242/api/v1/ws"),
        vec![LAUNCH_ID],
        "the key is the nest's authority, as the TOFU pin's is"
    );
    assert_eq!(pinned_nest_custodian_identity(LAUNCH_NEST), Some(LAUNCH_ID));

    // …and trusted for NO other nest. This is the regression: the unkeyed seed
    // put the launch nest's key into a dedicated nest's trust set, so every
    // mint there ran to "signed by a holder this account does not trust".
    assert!(
        trusted_escrow_holders(DEDICATED_NEST).is_empty(),
        "a seed entry for one nest must never trust another nest's escrow receipts"
    );
    assert_eq!(
        pinned_nest_custodian_identity(DEDICATED_NEST),
        None,
        "a custody accept on another nest must never bind the seeded nest's identity"
    );

    // ── Two entries: each nest resolves to its own identity.
    set_seed(Some(&format!(
        "{LAUNCH_NEST}={},{DEDICATED_NEST}={}",
        hex(LAUNCH_ID),
        hex(DEDICATED_ID)
    )));
    assert_eq!(trusted_escrow_holders(LAUNCH_NEST), vec![LAUNCH_ID]);
    assert_eq!(trusted_escrow_holders(DEDICATED_NEST), vec![DEDICATED_ID]);
    assert_eq!(
        pinned_nest_custodian_identity(DEDICATED_NEST),
        Some(DEDICATED_ID)
    );

    // ── A later entry for the same authority supersedes an earlier one (the
    // harness writes a fresher read after a stale one, never beside it).
    set_seed(Some(&format!(
        "{LAUNCH_NEST}={},{LAUNCH_NEST}={}",
        hex(LAUNCH_ID),
        hex(DEDICATED_ID)
    )));
    assert_eq!(trusted_escrow_holders(LAUNCH_NEST), vec![DEDICATED_ID]);

    // ── The retired unkeyed grammar trusts nothing: a bare identity names no
    // nest, so it cannot be scoped, so it is not honoured. Likewise a malformed
    // identity.
    set_seed(Some(&hex(LAUNCH_ID)));
    assert!(trusted_escrow_holders(LAUNCH_NEST).is_empty());
    assert_eq!(pinned_nest_custodian_identity(LAUNCH_NEST), None);
    set_seed(Some(&format!("{LAUNCH_NEST}=not-hex")));
    assert!(trusted_escrow_holders(LAUNCH_NEST).is_empty());

    // ── A real pin still leads: the seed joins the escrow set beside it, and
    // the custodian identity is the pin's (the seed stands in only when no
    // pin exists).
    pin_identity_for_test(LAUNCH_NEST, DEDICATED_ID);
    set_seed(Some(&format!("{LAUNCH_NEST}={}", hex(LAUNCH_ID))));
    assert_eq!(
        trusted_escrow_holders(LAUNCH_NEST),
        vec![DEDICATED_ID, LAUNCH_ID]
    );
    assert_eq!(
        pinned_nest_custodian_identity(LAUNCH_NEST),
        Some(DEDICATED_ID)
    );

    set_seed(None);
}
