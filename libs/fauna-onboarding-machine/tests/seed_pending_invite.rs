use std::sync::Arc;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{InviteRequestState, OnboardingMachine, OnboardingStep};

fn make() -> Arc<OnboardingMachine> {
    OnboardingMachine::new(Arc::new(NullObserver))
}

#[test]
fn seed_pending_invite_jumps_to_invite_request_with_pending_review() {
    let m = make();
    let status = serde_json::json!({
        "PendingReview": { "request_id": "REQ1", "last_checked_ms": 0 }
    });
    m.seed_pending_invite(
        "https://a.example".into(),
        "alice@a.example".into(),
        "REQ1".into(),
        status.to_string(),
    );
    assert_eq!(m.step(), OnboardingStep::InviteRequest);
    assert_eq!(m.current_handle(), "alice@a.example");
    let snap = m.invite_request_snapshot();
    assert!(matches!(
        snap.state,
        InviteRequestState::PendingReview { .. }
    ));
    assert!(snap.recheck_visible);
    // FALSE since 2026-08-12, as `onboarding.md` § 3 requires: the continue-exit
    // is retired and this journey advances by polling, so a live Continue here
    // would be a control with nothing behind it.
    assert!(
        !snap.continue_enabled,
        "Continue must be dead during PendingReview — the journey advances by \
         polling (onboarding.md § The pending-invite surface)"
    );
}

#[test]
fn seed_pending_invite_with_corrupt_status_falls_back_to_pending_review() {
    let m = make();
    m.seed_pending_invite(
        "https://a.example".into(),
        "alice@a.example".into(),
        "REQ1".into(),
        "garbage-not-valid-json".into(),
    );
    assert_eq!(m.step(), OnboardingStep::InviteRequest);
    let snap = m.invite_request_snapshot();
    match snap.state {
        InviteRequestState::PendingReview { request_id, .. } => {
            assert_eq!(request_id, "REQ1");
        }
        _ => panic!("expected PendingReview fallback"),
    }
}

/// An **unparseable slot** — here the retired `Approved` spelling — resumes as
/// `PendingReview` and keeps polling.
///
/// This pins the live corrupt-slot degrade: `status_json` is stored text, so
/// any corrupt or unrecognized blob must resume safely.
///
/// `seed_pending_invite` degrades an unparseable
/// status to `PendingReview` (`unwrap_or`), so the resumed page polls and the
/// registered-probe re-derives the real answer from the nest — an approval
/// lands the user in the app, a vanished request ends in the ratified
/// `not_found` terminal. The old behavior it replaces was a dead end: a
/// rendered `Approved` state whose Continue button redeemed into
/// `ActorAlreadyRegistered`. So the degrade strictly improves on it.
#[test]
fn an_unparseable_approved_slot_resumes_as_pending_review_and_polls() {
    let m = make();
    let status = serde_json::json!({
        "Approved": {
            "quota": {
                "storage_bytes": 1_000_000_000_u64,
                "traffic_bytes_per_month": 5_000_000_000_u64,
            },
            "request_id": "REQ1",
        }
    });
    m.seed_pending_invite(
        "https://a.example".into(),
        "alice@a.example".into(),
        "REQ1".into(),
        status.to_string(),
    );
    let snap = m.invite_request_snapshot();
    // Falls back to the polling state, carrying the request id from the slot
    // (not from the unparseable blob) so the recheck targets the right row.
    match &snap.state {
        InviteRequestState::PendingReview { request_id, .. } => {
            assert_eq!(request_id, "REQ1");
        }
        other => panic!("an unparseable Approved slot must resume as PendingReview, got {other:?}"),
    }
    // The two controls that make the resume actionable: recheck is the manual
    // affordance, and Continue is dead because the journey advances by polling.
    assert!(snap.recheck_visible, "the resumed page must offer recheck");
    assert!(
        !snap.continue_enabled,
        "Continue is retired for this journey — a live button here would redeem \
         into ActorAlreadyRegistered, the dead end this retirement removes"
    );
}

/// **The slot carries `state.nest_url`, never `effective_nest_url()`.**
///
/// This is the reason `pending_invite_slot()` exists as ONE call instead of
/// three getters, so it owes a test. The `provider_base_urls` override retargets
/// HTTP request *targets* (it is how e2e points the wizard at a test cloud); the
/// slot's `nest_url` is persisted as the nest's **identity**. Read the wrong one
/// and a test-cloud URL lands in a production identity-store record — silently,
/// because both are well-formed URLs and every test that sets no override sees
/// them as equal.
///
/// So the override here is deliberately distinct: without one this test cannot
/// fail, which is exactly how the bug would survive.
#[test]
fn the_slot_carries_the_state_nest_url_not_the_provider_override() {
    let m = make();
    m.seed_pending_invite(
        "https://real-nest.example".into(),
        "alice@real-nest.example".into(),
        "REQ1".into(),
        serde_json::json!({ "PendingReview": { "request_id": "REQ1", "last_checked_ms": 0 } })
            .to_string(),
    );
    let mut urls = std::collections::HashMap::new();
    urls.insert("nest".to_string(), "https://test-cloud.invalid".to_string());
    m.set_provider_base_urls(urls);

    let slot = m.pending_invite_slot().expect("PendingReview has a slot");
    assert_eq!(
        slot.nest_url, "https://real-nest.example",
        "the resume slot must carry the nest's IDENTITY, not the HTTP override \
         — a test-cloud URL here is written into a production identity record"
    );
    assert_eq!(slot.handle, "alice@real-nest.example");
    assert_eq!(slot.request_id, "REQ1");
}

/// No slot outside `PendingReview` — so an app calling this at every submit
/// return writes nothing when the submit failed or was refused.
#[test]
fn no_slot_outside_pending_review() {
    let m = make();
    assert!(
        m.pending_invite_slot().is_none(),
        "a fresh machine has no request to resume"
    );
}

/// `status_json` round-trips: a slot written from `pending_invite_slot()` must
/// re-seed to the same state through `seed_pending_invite`. The app never
/// validates the JSON (`onboarding.md` § Long-term store contract), so the two
/// ends of that contract have to agree here or a resume silently loses fields.
#[test]
fn the_slot_status_json_reseeds_to_the_same_state() {
    let m = make();
    m.seed_pending_invite(
        "https://a.example".into(),
        "alice@a.example".into(),
        "REQ1".into(),
        serde_json::json!({ "PendingReview": { "request_id": "REQ1", "last_checked_ms": 7 } })
            .to_string(),
    );
    let slot = m.pending_invite_slot().expect("PendingReview has a slot");

    let m2 = make();
    m2.seed_pending_invite(
        slot.nest_url.clone(),
        slot.handle.clone(),
        slot.request_id.clone(),
        slot.status_json.clone(),
    );
    match m2.invite_request_snapshot().state {
        InviteRequestState::PendingReview {
            request_id,
            last_checked_ms,
        } => {
            assert_eq!(request_id, "REQ1");
            assert_eq!(last_checked_ms, 7, "the slot lost the state's own fields");
        }
        other => panic!("a written slot must re-seed to PendingReview, got {other:?}"),
    }
}
