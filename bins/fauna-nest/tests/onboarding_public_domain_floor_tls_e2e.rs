//! tier_3 — the **public-domain** twin of the desktop `test@localhost` bug:
//! claiming a fresh internet nest from a **native** client.
//!
//! Split out of `onboarding_bare_localhost_tls_e2e.rs` on 2026-08-23, where it
//! shared a binary with the bare-loopback test and the two were **red on
//! `origin/main` whenever libtest ran them in parallel**. This test installs a
//! `"nest"` provider override, which lands in a *process-global* dial mirror and
//! captured the other test's dial. Neither test is at fault; one process holding
//! two `OnboardingMachine`s is. Full mechanism, and why a serial run hides it, in
//! `floor_tls_nest::start_unclaimed_tls`.
//!
//! ⚠ **ONE test per binary here. Do not add a second `#[tokio::test]`, and do
//! not merge this back with its sibling** — that is precisely the shape that was
//! broken, and it fails in a way that points at the wrong crate entirely
//! (`ChallengeResponse` / "challenge error", with `nest_url()` reading correct).
//!
//! (Like every fauna-nest `tests/` target it is compile-covered only — no gate
//! or CI executes it; sessions run it by hand.)

mod common;
mod floor_tls_nest;

use std::sync::Arc;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    HandleCheckOutcome, OnboardingMachine, OnboardingObserver, OnboardingStep,
};

use floor_tls_nest::start_unclaimed_tls;

/// A nest deployed per `docs/guides/nest-internet-setup.md` boots **domainless**
/// (its compose carries no domain — the nest learns its name at claim), so at the
/// moment the admin claims it, it is necessarily serving its **self-signed floor**
/// — no ACME cert can exist yet for a name the nest does not yet know
/// (`nest/domains-and-tls-bootstrap.md` § Boot / § Claim sets identity).
///
/// The handle-check nest-health probe therefore MUST tolerate a non-WebPKI cert
/// for a registerable domain too. It used to pick the strict-WebPKI client
/// whenever `is_public_dns_name` held, on the assumption that "a public nest has a
/// real ACME cert" — false for exactly this window, which made the probe's TLS
/// handshake fail, collapse to `ConnectionRefused`, and report `RegisteredNoNest`.
/// That is a **dead end**: `RegisteredNoNest` offers only the "I control this
/// domain, set a nest up" checkbox, so a native app could never claim a fresh
/// internet nest. The web app escaped it only because the user clicks through
/// the browser's cert interstitial, which grants that origin an exception.
///
/// The probe is a **reachability** check, never the auth path — authentication is
/// the downstream channel-binding ceremony (`security.md` § Transport trust,
/// Axis 1), which the anonymous WS leg already runs against a provisionally
/// accepted cert. Reported from a real VPS walkthrough, 2026-07-24.
#[tokio::test]
async fn public_domain_handle_reaches_claim_path_over_floor_tls() {
    let (port, _dir) = start_unclaimed_tls().await;

    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    // The `nest` override points the probe at the floor-TLS nest while the handle
    // domain stays a *registerable* one — which is what selects the probe's TLS
    // client (`is_public_dns_name`), so this exercises the real branch. It also
    // makes the test hermetic: an override skips the live DoH lookup.
    //
    // ⚠ This constructor also installs the override **process-globally**
    // (`fauna_launch_machine::set_nest_dial_override`), which is why this test
    // owns its binary — see the module doc.
    let m = OnboardingMachine::new_with_provider_base_urls(
        observer,
        Some(std::collections::HashMap::from([(
            "nest".to_string(),
            format!("https://localhost:{port}"),
        )])),
    );
    m.seed_identity("01".repeat(32));
    m.set_current_handle("you@example.com".into());

    m.start_handle_check("you@example.com".into()).await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::UnregisteredUnclaimedNest,
        "a registerable handle domain must reach the unclaimed-nest claim path against a nest \
         still on its self-signed floor (every internet nest, at claim time) — NOT \
         RegisteredNoNest, whose control-checkbox UX is a dead end for claiming. \
         got {:?}, msg {:?}",
        snap.outcome,
        snap.message.key,
    );

    let next = m.submit_handle_check_continue().await;
    assert_eq!(
        next,
        OnboardingStep::ClaimCode,
        "Continue on a public-domain unclaimed-nest outcome must route to the claim screen",
    );
}
