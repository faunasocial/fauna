//! Integration round-trip for the session-management surface —
//! `fauna.sessions.{list,revoke,revoke_all,lockout}`. A behavior-preserving
//! transport migration of the bearer-authed session routes
//! (`session_routes::{list_sessions, revoke_session, revoke_all_sessions}`)
//! plus an authed variant of the emergency lockout. The handlers reuse the same
//! `token_store` methods + `db::set_locked_until` the twins call; these tests
//! exercise the WS-RPC layer: request decode, the reply shapes, the
//! `keep_token_id` "revoke all except current" semantics, the `User | Admin`
//! allowlist, the `not_found` / `malformed` mappings, and replay metadata.
//!
//! `revoke_all` names the session to keep by `token_id` (the WS connection drops
//! the raw bearer, so the client supplies the id it learned at mint) — not the
//! ~200-closure connection-context refactor the original hand-off proposed
//! (tracked internally, Track B2 of the WS-RPC-everywhere migration).
//!
//! Authority for the wire types: `libs/fauna-protocol/src/sessions.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` + real
//! `TokenStore` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::ActorId;
use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
    session_handlers,
};
use fauna_protocol::{
    decode_strict as decode,
    sessions::{
        LockoutReply, LockoutRequest, RevokeAllReply, RevokeAllRequest, RevokeReply, RevokeRequest,
        SessionsListReply, SessionsListRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    session_handlers::register_sessions_handlers(&mut b);
    (b.build(), state)
}

/// Mint `n` sessions for `actor`, returning their `token_id`s in mint order.
async fn mint(state: &AppState, actor: [u8; 32], n: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        let minted = state
            .auth
            .token_store
            .insert_with_metadata(ActorId(actor), 3600, None, None)
            .await;
        ids.push(minted.token_id);
    }
    ids
}

// ── list ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_returns_minted_sessions() {
    let (router, state) = router_and_state().await;
    let actor = [11u8; 32];
    let minted = mint(&state, actor, 2).await;

    let reply: SessionsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sessions.list",
            encode(&SessionsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    assert_eq!(reply.sessions.len(), 2);
    let got: std::collections::BTreeSet<String> =
        reply.sessions.iter().map(|s| s.token_id.clone()).collect();
    let want: std::collections::BTreeSet<String> = minted.into_iter().collect();
    assert_eq!(got, want);
}

// ── revoke (owned + not-owned) ───────────────────────────────────────────────

#[tokio::test]
async fn revoke_owned_session_then_not_found() {
    let (router, state) = router_and_state().await;
    let actor = [12u8; 32];
    let ids = mint(&state, actor, 2).await;

    let reply: RevokeReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sessions.revoke",
            encode(&RevokeRequest {
                token_id: ids[0].clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("revoke ok"),
    )
    .unwrap();
    assert!(reply.ok);
    assert_eq!(
        state
            .auth
            .token_store
            .list_sessions(&ActorId(actor))
            .await
            .len(),
        1
    );

    // Revoking it again (now gone) → not_found.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sessions.revoke",
        encode(&RevokeRequest {
            token_id: ids[0].clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("already-revoked → not_found");
    assert_eq!(err.code, "fauna.sessions.not_found");
}

#[tokio::test]
async fn revoke_cannot_touch_another_actors_session() {
    let (router, state) = router_and_state().await;
    let owner = [13u8; 32];
    let attacker = [14u8; 32];
    let owner_ids = mint(&state, owner, 1).await;
    let _attacker_ids = mint(&state, attacker, 1).await;

    // attacker names the owner's token_id → not_found (ownership check), and the
    // owner's session survives.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        attacker,
        "fauna.sessions.revoke",
        encode(&RevokeRequest {
            token_id: owner_ids[0].clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("cross-actor revoke → not_found");
    assert_eq!(err.code, "fauna.sessions.not_found");
    assert_eq!(
        state
            .auth
            .token_store
            .list_sessions(&ActorId(owner))
            .await
            .len(),
        1
    );
}

// ── revoke_all (keep current) ────────────────────────────────────────────────

#[tokio::test]
async fn revoke_all_keeps_named_session() {
    let (router, state) = router_and_state().await;
    let actor = [15u8; 32];
    let ids = mint(&state, actor, 3).await;

    let reply: RevokeAllReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sessions.revoke_all",
            encode(&RevokeAllRequest {
                keep_token_id: ids[0].clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("revoke_all ok"),
    )
    .unwrap();
    assert!(reply.ok);
    assert_eq!(reply.revoked, 2);

    let remaining = state.auth.token_store.list_sessions(&ActorId(actor)).await;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].token_id, ids[0]);
}

// ── lockout (authed) ─────────────────────────────────────────────────────────

#[tokio::test]
async fn lockout_revokes_all_and_sets_locked_until() {
    let (router, state) = router_and_state().await;
    let actor = [16u8; 32];
    // set_locked_until is an UPDATE — the user row must exist.
    state.db.create_user(&actor, "free", "test").await.unwrap();
    mint(&state, actor, 3).await;

    let now = fauna_core::data::Timestamp::now_secs();

    let reply: LockoutReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sessions.lockout",
            encode(&LockoutRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("lockout ok"),
    )
    .unwrap();
    assert!(reply.ok);
    // The hard-coded 24 h window — the requested 7200 is ignored, the same
    // ruling as the pre-identity twin (one shared constant on both
    // kinds, so the twins cannot diverge).
    let window = fauna_nest::account_core::EMERGENCY_LOCKOUT_SECS as i64;
    assert!(
        reply.locked_until >= now + window && reply.locked_until <= now + window + 5,
        "locked_until = {}, now = {now}",
        reply.locked_until
    );

    // All sessions revoked.
    assert_eq!(
        state
            .auth
            .token_store
            .list_sessions(&ActorId(actor))
            .await
            .len(),
        0
    );
    // locked_until persisted.
    assert_eq!(
        state.db.get_locked_until(&actor).await.unwrap(),
        Some(reply.locked_until)
    );
}

#[tokio::test]
async fn lockout_ignores_the_requested_duration() {
    let (router, state) = router_and_state().await;
    let actor = [17u8; 32];
    state.db.create_user(&actor, "free", "test").await.unwrap();

    let now = fauna_core::data::Timestamp::now_secs();

    let reply: LockoutReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.sessions.lockout",
            encode(&LockoutRequest {
                // The retired duration key, sent by a non-conforming caller — it
                // lands in `extra` and never moves the window.
                extra: [(
                    "duration_secs".to_string(),
                    fauna_protocol::Value::Integer(60_i64.into()),
                )]
                .into_iter()
                .collect(),
            }),
        )
        .await
        .expect("lockout ok"),
    )
    .unwrap();
    let window = fauna_nest::account_core::EMERGENCY_LOCKOUT_SECS as i64;
    assert!(
        reply.locked_until >= now + window && reply.locked_until <= now + window + 5,
        "a stray 60s duration must not move the window; locked_until = {}, now = {now}",
        reply.locked_until
    );
}

// ── malformed payload ────────────────────────────────────────────────────────

#[tokio::test]
async fn rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [18u8; 32],
        "fauna.sessions.revoke",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── replay metadata ──────────────────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_and_state().await;
    for kind in [
        "fauna.sessions.list",
        "fauna.sessions.revoke",
        "fauna.sessions.revoke_all",
        "fauna.sessions.lockout",
    ] {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(!m.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
}

// ── allowlist ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn allowlist_user_admin_permitted_bridges_denied() {
    for kind in [
        "fauna.sessions.list",
        "fauna.sessions.revoke",
        "fauna.sessions.revoke_all",
        "fauna.sessions.lockout",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
}
