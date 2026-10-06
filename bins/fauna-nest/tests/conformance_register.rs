//! In-process WS-RPC handler tests for the pre-identity account-registration
//! kind `fauna.account.register` — dispatch the registered handler directly
//! (no socket), exercising the shared `account_core::register_core` ceremony
//! and the `RegisterError` → `RpcError` mapping. There is no HTTP twin to
//! cover: `POST /api/v1/register` was retired (S4f) and this kind is the sole
//! register surface (`src/registration.rs` module docs); socket-level coverage
//! (the anonymous endpoint resolving the kind over the wire) lives in
//! `pre_identity_ws.rs`.
//! Slice: tracked internally.

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;

use fauna_core::identity::ActorKeypair;
use fauna_nest::account_handlers::register_account_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::AuthState;
use fauna_protocol::account::{RegisterReply, RegisterRequest};
use fauna_protocol::node_policy::RegistrationMode;
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

const DOMAIN: &str = "test.fauna.social";

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_account_handlers(&mut b);
    b.build()
}

/// `AppState` on `DOMAIN` in the given registration posture.
///
/// The posture is the client-set `registration_mode` singleton (mode + the
/// orthogonal free-tier ceiling); `state.auth.registration` now carries only the
/// two things that are *not* an admin choice — the handle domain and the
/// reserved-handle deny-list. `register_core` reads both (the HTTP-only per-IP
/// rate-limit lives in the twin, not the core).
fn state_in(
    db: Arc<CacheDb>,
    mode: RegistrationMode,
    max_free_users: Option<u64>,
) -> Arc<AppState> {
    Arc::new(AppState {
        auth: AuthState {
            registration: RegistrationConfig {
                handle_domain: Some(DOMAIN.to_string()),
                reserved_handles: vec!["admin".into(), "root".into(), "system".into()],
            },
            ..Default::default()
        },
        registration_mode: Arc::new(tokio::sync::RwLock::new((mode, max_free_users))),
        ..AppState::for_test(db)
    })
}

/// Open self-service registration on `DOMAIN`, no invite required, no free cap.
fn open_nest(db: Arc<CacheDb>) -> Arc<AppState> {
    state_in(db, RegistrationMode::Open, None)
}

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

/// Build a signed `fauna.account.register` payload over
/// `actor_id ‖ handle ‖ domain ‖ timestamp_be`.
fn register_payload(
    kp: &ActorKeypair,
    handle: &str,
    domain: &str,
    timestamp_ms: u64,
    invite_code: Option<&str>,
) -> Bytes {
    let msg = fauna_protocol::account::register_signed_message(
        &kp.actor_id().0,
        handle,
        domain,
        timestamp_ms,
    );
    let sig = kp.signing_key().sign(&msg);
    let req = RegisterRequest {
        actor_id: hex::encode(kp.actor_id().0),
        handle: handle.to_string(),
        timestamp: timestamp_ms,
        signature: hex::encode(sig.to_bytes()),
        invite_code: invite_code.map(str::to_string),
        age_claim: None,
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Dispatch the kind through the registered handler. The anonymous connection
/// binds no actor — the handler reads the registering actor from the signed
/// payload, so the dispatcher's actor arg is irrelevant here.
async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router
        .kind_meta("fauna.account.register")
        .expect("kind registered");
    (meta.handler)(state, [0u8; 32], payload).await
}

#[tokio::test]
async fn register_succeeds_and_returns_account_coordinates() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db.clone());
    let r = router();
    let kp = ActorKeypair::generate();

    let out = dispatch(
        &r,
        st,
        register_payload(&kp, "alice", DOMAIN, now_ms(), None),
    )
    .await
    .expect("register ok");
    let reply: RegisterReply = decode(&out).unwrap();
    assert_eq!(reply.actor_id, hex::encode(kp.actor_id().0));
    assert_eq!(reply.handle, "alice");
    assert_eq!(reply.domain, DOMAIN);
    assert_eq!(reply.tier, "free");
    assert_eq!(reply.node_url, format!("https://{DOMAIN}/api/v1"));
    // No DNS manager in `for_test` → subhandles off → addresses empty.
    assert!(reply.addresses.is_empty());

    // The account is real: the actor is now registered and the handle resolves.
    assert!(db.is_actor_registered(&kp.actor_id().0).await.unwrap());
    assert_eq!(
        db.resolve_handle("alice").await.unwrap(),
        Some(kp.actor_id().0)
    );
}

#[tokio::test]
async fn register_with_valid_invite_grants_the_code_tier() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_invite_code("WELCOME2026", "personal", 5)
        .await
        .unwrap();
    // InviteRequired proves the code path satisfies the requirement.
    let st = state_in(db, RegistrationMode::InviteRequired, None);
    let r = router();
    let kp = ActorKeypair::generate();

    let out = dispatch(
        &r,
        st,
        register_payload(&kp, "bob", DOMAIN, now_ms(), Some("WELCOME2026")),
    )
    .await
    .expect("register ok");
    let reply: RegisterReply = decode(&out).unwrap();
    assert_eq!(reply.tier, "personal");
}

#[tokio::test]
async fn register_rejects_closed_registration() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in(db, RegistrationMode::Closed, None);
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(
        &r,
        st,
        register_payload(&kp, "carol", DOMAIN, now_ms(), None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.registration_closed");
}

#[tokio::test]
async fn register_rejects_invalid_handle() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(
        &r,
        st,
        register_payload(&kp, "ab", DOMAIN, now_ms(), None), // too short (< 3)
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invalid_request");
}

#[tokio::test]
async fn register_rejects_reserved_handle() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(
        &r,
        st,
        register_payload(&kp, "admin", DOMAIN, now_ms(), None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invalid_request");
}

#[tokio::test]
async fn register_rejects_bad_signature() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = router();
    let kp = ActorKeypair::generate();
    let wrong = ActorKeypair::generate();

    // Sign with the wrong key but claim kp's actor_id.
    let ts = now_ms();
    let mut msg = Vec::new();
    msg.extend_from_slice(&kp.actor_id().0);
    msg.extend_from_slice(b"dave");
    msg.extend_from_slice(DOMAIN.as_bytes());
    msg.extend_from_slice(&ts.to_be_bytes());
    let sig = wrong.signing_key().sign(&msg);
    let req = RegisterRequest {
        actor_id: hex::encode(kp.actor_id().0),
        handle: "dave".into(),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        invite_code: None,
        age_claim: None,
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

    let err = dispatch(&r, st, payload).await.unwrap_err();
    assert_eq!(err.code, "fauna.account.signature_failed");
}

#[tokio::test]
async fn register_rejects_stale_timestamp() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = router();
    let kp = ActorKeypair::generate();

    let stale = now_ms() - 300_000; // 5 minutes ago, well past ±30 s.
    let err = dispatch(&r, st, register_payload(&kp, "erin", DOMAIN, stale, None))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.account.invalid_request");
}

#[tokio::test]
async fn register_rejects_already_registered_actor() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = router();
    let kp = ActorKeypair::generate();

    dispatch(
        &r,
        st.clone(),
        register_payload(&kp, "frank", DOMAIN, now_ms(), None),
    )
    .await
    .expect("first register ok");

    // Same actor, different handle → ActorAlreadyRegistered.
    let err = dispatch(
        &r,
        st,
        register_payload(&kp, "frank2", DOMAIN, now_ms(), None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.actor_exists");
}

#[tokio::test]
async fn register_rejects_taken_handle() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::generate();
    db.create_user_with_handle(&owner.actor_id().0, "free", "grace", None)
        .await
        .unwrap();
    let st = open_nest(db);
    let r = router();

    // A different actor tries to claim the assigned handle → HandleTaken.
    let kp = ActorKeypair::generate();
    let err = dispatch(
        &r,
        st,
        register_payload(&kp, "grace", DOMAIN, now_ms(), None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.handle_taken");
}

#[tokio::test]
async fn register_rejects_when_invite_required_and_absent() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in(db, RegistrationMode::InviteRequired, None);
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(
        &r,
        st,
        register_payload(&kp, "heidi", DOMAIN, now_ms(), None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invite_required");
}

#[tokio::test]
async fn register_rejects_invalid_invite_code() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(
        &r,
        st,
        register_payload(&kp, "ivan", DOMAIN, now_ms(), Some("BOGUS")),
    )
    .await
    .unwrap_err();
    // An invalid code is a 400-class validation failure.
    assert_eq!(err.code, "fauna.account.invalid_request");
}

#[tokio::test]
async fn register_rejects_when_free_limit_reached() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state_in(db, RegistrationMode::Open, Some(0));
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(
        &r,
        st,
        register_payload(&kp, "judy", DOMAIN, now_ms(), None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.free_limit_reached");
}

#[tokio::test]
async fn register_rejects_malformed_payload() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = open_nest(db);
    let r = router();

    // Not a valid RegisterRequest map → infra malformed code (before the core).
    let err = dispatch(&r, st, Bytes::from_static(&[0xff, 0xff, 0xff]))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
}
