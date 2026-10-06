//! Integration round-trip for the pre-identity emergency-lockout kind
//! `fauna.account.lockout` — the no-token recovery channel that migrated off the
//! `POST /api/v1/account/lockout` HTTP twin (
//! Bucket-B). Like `fauna.account.register` it rides the **anonymous** WS
//! connection and authenticates from the **signed payload** (Ed25519 over
//! `actor_id ‖ timestamp_be`), so the dispatcher's actor arg is ignored. The
//! handler reuses `account_core::lockout_core` (the same `token_store` +
//! `db::set_locked_until` the retired twin called).
//!
//! Authority for the wire types: `libs/fauna-protocol/src/account.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` + real
//! `TokenStore` — no mocks).

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_nest::account_handlers::register_account_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::account::{AccountLockoutReply, AccountLockoutRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_account_handlers(&mut b);
    (b.build(), state)
}

/// Dispatch the kind through the registered handler. The anonymous connection
/// binds no actor — the handler reads the actor from the signed payload, so the
/// dispatcher's actor arg (`[0u8; 32]`) is irrelevant.
async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router
        .kind_meta("fauna.account.lockout")
        .expect("kind registered");
    (meta.handler)(state, [0u8; 32], payload).await
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// Build a signed `fauna.account.lockout` payload: the domain-tagged
/// `signature` over `ACCOUNT_LOCKOUT_V1 ‖ actor_id ‖ timestamp_be` (tagged-only —
/// item 2 of the finding).
fn lockout_payload(kp: &ActorKeypair, timestamp_secs: u64) -> Bytes {
    lockout_payload_with_stray_duration(kp, timestamp_secs, None)
}

/// [`lockout_payload`], optionally carrying a stray `duration_secs` key — the
/// shape an older client sent before the ignored field left the wire. It
/// rides `extra`; the signature never covered it.
fn lockout_payload_with_stray_duration(
    kp: &ActorKeypair,
    timestamp_secs: u64,
    stray_duration_secs: Option<u64>,
) -> Bytes {
    let tagged =
        fauna_protocol::account::account_lockout_signed_message(&kp.actor_id().0, timestamp_secs);
    let sig = kp.signing_key().sign(&tagged);
    let mut req = AccountLockoutRequest {
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: timestamp_secs,
        signature: hex::encode(sig.to_bytes()),
        ..Default::default()
    };
    if let Some(d) = stray_duration_secs {
        req.extra.insert(
            "duration_secs".to_string(),
            fauna_protocol::Value::Integer((d as i64).into()),
        );
    }
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Mint `n` sessions for `actor` so token-revocation is observable.
async fn mint(state: &AppState, actor: [u8; 32], n: usize) {
    for _ in 0..n {
        state
            .auth
            .token_store
            .insert_with_metadata(ActorId(actor), 3600, None, None)
            .await;
    }
}

#[tokio::test]
async fn lockout_revokes_all_tokens_and_sets_locked_until() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    // set_locked_until is an UPDATE — the user row must exist to persist.
    state.db.create_user(&actor, "free", "test").await.unwrap();
    mint(&state, actor, 3).await;

    let now = now_secs() as i64;
    let reply: AccountLockoutReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            lockout_payload(&kp, now_secs()),
        )
        .await
        .expect("lockout ok"),
    )
    .unwrap();
    assert!(reply.ok);
    // The hard-coded 24 h window.
    let window = fauna_nest::account_core::EMERGENCY_LOCKOUT_SECS as i64;
    assert!(
        reply.locked_until >= now + window && reply.locked_until <= now + window + 5,
        "locked_until = {}, now = {now}",
        reply.locked_until
    );

    // Every token for the actor revoked.
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

/// The escalation pin. The signature covers `actor_id ‖ timestamp_be`
/// only, so before the constant ruling an on-path capture of a short lockout
/// could be re-issued as a longer one inside the ±300 s window via the unsigned
/// `duration_secs`. That field has since left the wire; this test replays the
/// SAME signed bytes carrying a stray `duration_secs` key (it lands in `extra`)
/// and pins that the applied window never moves: whatever the stray key says —
/// 60 s, a week — the lock is `now + EMERGENCY_LOCKOUT_SECS`, so a replayed
/// request can re-lock (protective, intended) but can never EXTEND beyond the
/// constant window from its own dispatch instant.
#[tokio::test]
async fn a_replayed_request_with_a_stray_duration_cannot_change_the_window() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();
    let window = fauna_nest::account_core::EMERGENCY_LOCKOUT_SECS as i64;

    // One signature, minted once — every dispatch below carries these exact
    // signed bytes; only the stray, unsigned duration key differs.
    let signed_ts = now_secs();

    for requested in [60_u64, 3600, 7 * 86_400] {
        let now = now_secs() as i64;
        let reply: AccountLockoutReply = decode(
            &dispatch(
                &router,
                Arc::clone(&state),
                lockout_payload_with_stray_duration(&kp, signed_ts, Some(requested)),
            )
            .await
            .expect("lockout ok"),
        )
        .unwrap();
        assert!(
            reply.locked_until >= now + window && reply.locked_until <= now + window + 5,
            "requested {requested}s must not move the window: locked_until = {}, now = {now}",
            reply.locked_until
        );
    }
}

#[tokio::test]
async fn lockout_rejects_bad_signature() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let wrong = ActorKeypair::generate();

    // Sign with the wrong key but claim kp's actor_id.
    let ts = now_secs();
    let mut msg = Vec::with_capacity(40);
    msg.extend_from_slice(&kp.actor_id().0);
    msg.extend_from_slice(&ts.to_be_bytes());
    let sig = wrong.signing_key().sign(&msg);
    let req = AccountLockoutRequest {
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

    let err = dispatch(&router, state, payload).await.unwrap_err();
    assert_eq!(err.code, "fauna.account.signature_failed");
}

#[tokio::test]
async fn lockout_rejects_stale_timestamp() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();

    let stale = now_secs() - 1_000; // > ±300 s window
    let err = dispatch(&router, state, lockout_payload(&kp, stale))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.account.invalid_request");
}

#[tokio::test]
async fn lockout_rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    // Not a valid AccountLockoutRequest map → infra malformed code (before core).
    let err = dispatch(&router, state, Bytes::from_static(&[0xff, 0xff, 0xff]))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
}
