//! Integration round-trip for `fauna.spam.{get_preferences,
//! set_preferences}` — a faithful transport migration of
//! `GET|PUT /api/v1/spam/preferences`. Reaches `CacheDb` directly through
//! the handler via `state.db.{get,upsert}_spam_preferences`.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/spam.rs`.
//! Slice: tracked internally.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
    spam_handlers,
};
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    spam::{SpamGetPreferencesRequest, SpamPreferences, SpamSetPreferencesRequest},
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    spam_handlers::register_spam_handlers(&mut b);
    (b.build(), state)
}

fn get_payload() -> Bytes {
    let req = SpamGetPreferencesRequest {
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn set_payload(req: SpamSetPreferencesRequest) -> Bytes {
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn empty_set() -> SpamSetPreferencesRequest {
    SpamSetPreferencesRequest {
        spam_threshold: None,
        phishing_threshold: None,
        extra: std::collections::BTreeMap::new(),
    }
}

async fn get_prefs(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) -> SpamPreferences {
    let bytes = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.spam.get_preferences",
        get_payload(),
    )
    .await
    .expect("get ok");
    decode(&bytes).unwrap()
}

#[tokio::test]
async fn get_returns_defaults_for_fresh_actor() {
    let (router, state) = router_with_db_only().await;
    let prefs = get_prefs(&router, &state, [7u8; 32]).await;
    assert_eq!(prefs.spam_threshold, 500); // 0.5 → per-mille
    assert_eq!(prefs.phishing_threshold, 300); // 0.3 → per-mille
}

#[tokio::test]
async fn set_updates_only_present_fields_and_echoes() {
    let (router, state) = router_with_db_only().await;
    let actor = [1u8; 32];

    // Set only spam_threshold; leave phishing_threshold untouched (None).
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.spam.set_preferences",
        set_payload(SpamSetPreferencesRequest {
            spam_threshold: Some(900), // 0.9 per-mille
            ..empty_set()
        }),
    )
    .await
    .expect("set ok");
    let reply: SpamPreferences = decode(&reply_bytes).unwrap();
    assert_eq!(reply.spam_threshold, 900, "updated");
    assert_eq!(reply.phishing_threshold, 300, "untouched (default kept)");

    // A subsequent get reflects the persisted state.
    let persisted = get_prefs(&router, &state, actor).await;
    assert_eq!(persisted.spam_threshold, 900);
    assert_eq!(persisted.phishing_threshold, 300);
}

#[tokio::test]
async fn set_clamps_thresholds() {
    let (router, state) = router_with_db_only().await;
    let actor = [2u8; 32];
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.spam.set_preferences",
        // Per-mille > 1000 clamps down to 1000 (=1.0). The low end can't
        // underflow: the wire type is `u16`, so a negative is unrepresentable
        // (0 = 0.0 is already the floor) — the unsigned type is the clamp.
        set_payload(SpamSetPreferencesRequest {
            spam_threshold: Some(5000),
            phishing_threshold: Some(2000),
            ..empty_set()
        }),
    )
    .await
    .expect("set ok");
    let reply: SpamPreferences = decode(&reply_bytes).unwrap();
    assert_eq!(reply.spam_threshold, 1000, "clamped to 1000 (=1.0)");
    assert_eq!(reply.phishing_threshold, 1000, "clamped to 1000 (=1.0)");
}

#[tokio::test]
async fn set_is_idempotent_on_replay() {
    let (router, state) = router_with_db_only().await;
    let actor = [4u8; 32];
    let req = SpamSetPreferencesRequest {
        spam_threshold: Some(700),
        phishing_threshold: Some(600),
        ..empty_set()
    };
    for _ in 0..2 {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.spam.set_preferences",
            set_payload(req.clone()),
        )
        .await
        .expect("set ok (replay must not error)");
    }
    let persisted = get_prefs(&router, &state, actor).await;
    assert_eq!(persisted.spam_threshold, 700);
    assert_eq!(persisted.phishing_threshold, 600);
}

#[tokio::test]
async fn spam_kinds_are_user_only_at_allowlist_layer() {
    for kind in ["fauna.spam.get_preferences", "fauna.spam.set_preferences"] {
        assert!(
            is_permitted(CallerClass::User, kind),
            "{kind} should be permitted for User"
        );
        // Admin ⊇ User: an admin inherits every User permission (api-layers.md
        // § Caller-class authorization), so a User-min kind is permitted for Admin.
        assert!(
            is_permitted(CallerClass::Admin, kind),
            "{kind} should be permitted for Admin (Admin ⊇ User)"
        );
        // Bridge classes are not users — denied.
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }
}
