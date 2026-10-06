//! tier_3 — the regression guard for the desktop `test@localhost` bug.
//!
//! Drives the REAL `OnboardingMachine` (real WS-RPC + self-signed channel
//! binding — the native desktop app's exact path, NOT a browser) against a
//! REAL, UNCLAIMED nest serving HTTPS from a self-signed floor cert on a **random
//! loopback port**, and asserts that a **bare `test@localhost`** handle (no port):
//!
//!   1. resolves to the *injected local-nest port* (`https://localhost:<port>`),
//!      not the old `http://localhost:3000` that made the desktop probe hit a dead
//!      port and report `RegisteredNoNest`, and
//!   2. reaches the unclaimed-nest **claim path** (`UnregisteredUnclaimedNest` →
//!      Continue routes to `ClaimCode`).
//!
//! Why this shape: the field bug was `resolve_handle_domain` special-casing
//! loopback to `http://…:3000` while the desktop nest serves `:443`
//! (`installers/windows.md` § Network-reachable nest). The fix made loopback
//! resolve like any host (`https://host`, port hidden) and added an injectable
//! `OnboardingMachine::local_nest_port` (default 443). That injectability is what
//! lets this test boot the nest on a random free port and run on **any** machine
//! — no privileged `:443`, no browser self-signed-cert wall, no FlaUI. (Like
//! every fauna-nest `tests/` target it is compile-covered only — no gate or CI
//! executes it; sessions run it by hand.)
//!
//! Combines the floor-TLS serving of `tls_channel_binding_roundtrip.rs` with the
//! unclaimed-nest + pre-identity onboarding handlers of
//! `onboarding_ws_nest_api_roundtrip.rs`.
//!
//! ⚠ **This binary holds exactly ONE test, and its public-domain twin lives in
//! its own binary (`onboarding_public_domain_floor_tls_e2e.rs`) for a reason
//! that will not be obvious from either file: a process-global dial override.
//! Read `floor_tls_nest::start_unclaimed_tls`'s note before adding a second
//! `#[tokio::test]` here or merging the two back together.** The pairing was red
//! on `origin/main` under parallel libtest until 2026-08-23, and green serially,
//! which is why it survived so long.

mod common;
mod floor_tls_nest;

use std::sync::Arc;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    HandleCheckOutcome, OnboardingMachine, OnboardingObserver, OnboardingStep,
};

use floor_tls_nest::start_unclaimed_tls;

#[tokio::test]
async fn bare_test_at_localhost_reaches_claim_path_on_injected_port_over_floor_tls() {
    let (port, _dir) = start_unclaimed_tls().await;

    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    m.seed_identity("01".repeat(32));
    // The same-box app / e2e injects WHERE its local nest listens (the Option-A
    // seam). Here: the random floor-TLS port. Without this, a bare loopback handle
    // resolves to the default https://localhost:443 (nothing there in the test).
    m.set_local_nest_port(port);
    m.set_current_handle("test@localhost".into());

    // The exact handle that failed on the desktop: bare `test@localhost`, NO port.
    m.start_handle_check("test@localhost".into()).await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::UnregisteredUnclaimedNest,
        "bare test@localhost must resolve to the injected local-nest port over the \
         self-signed floor TLS and reach the unclaimed-nest claim path — NOT \
         RegisteredNoNest (the desktop bug, where it probed the wrong port). \
         got {:?}, msg {:?}\n\
         \n\
         A `ProbeError {{ phase: ChallengeResponse, transient: true }}` here, with \
         this binary otherwise unchanged, most likely means the dial went somewhere \
         other than :{port} — a process-global `nest_dial_override` installed by \
         another OnboardingMachine in this process. `nest_url()` will still read \
         correctly; it records the resolved URL, never the override. See \
         `floor_tls_nest::start_unclaimed_tls`.",
        snap.outcome,
        snap.message.key,
    );
    // The seam resolved the bare loopback handle to the injected port over https
    // (NOT the old http://localhost:3000).
    assert_eq!(m.nest_url(), format!("https://localhost:{port}"));

    // Continue routes a bare test@localhost to the admin-claim screen — "→ claim".
    let next = m.submit_handle_check_continue().await;
    assert_eq!(
        next,
        OnboardingStep::ClaimCode,
        "Continue on a bare test@localhost unclaimed-nest outcome must route to the claim screen",
    );
}
