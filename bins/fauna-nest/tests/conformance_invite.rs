//! In-process WS-RPC handler tests for the pre-identity in-band invite kinds
//! `fauna.account.invite_request.{submit,status,cancel}` +
//! `fauna.account.invite_code.verify` — dispatch the registered handlers
//! directly (no socket), exercising the shared `invite_core` ceremonies and the
//! `InviteError` → `RpcError` mapping. Socket-level coverage lives in
//! `pre_identity_ws.rs`. (Invite/storage slice; tracked internally.)
//!
//! Unlike the claim / storage-mode ceremonies, the invite flow touches no
//! filesystem — `AppState::for_test` (in-memory DB, default registration
//! config) is sufficient; the HTTP-only per-IP rate-limit is not on the WS path.

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::invite_handlers::register_invite_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::invite::{
    InviteCodeVerify, InviteCodeVerifyReply, InviteRequestCancel, InviteRequestCancelReply,
    InviteRequestStatus, InviteRequestStatusQuery, InviteRequestSubmit,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_invite_handlers(&mut b);
    b.build()
}

fn state(db: Arc<CacheDb>) -> Arc<AppState> {
    Arc::new(AppState::for_test(db))
}

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

// ── payload builders ─────────────────────────────────────────────────────────

/// Signed `invite_request.submit` payload — signature over
/// `actor_id ‖ handle ‖ message ‖ timestamp_be`. `handle` should already be
/// lowercase (the core lowercases before verifying).
fn submit_payload(kp: &ActorKeypair, handle: &str, message: &str, ts_ms: u64) -> Bytes {
    submit_signed_by(kp, kp, handle, message, ts_ms)
}

fn submit_signed_by(
    signer: &ActorKeypair,
    claimant: &ActorKeypair,
    handle: &str,
    message: &str,
    ts_ms: u64,
) -> Bytes {
    let msg = fauna_protocol::invite::invite_submit_signed_message(
        &claimant.actor_id().0,
        handle,
        message,
        ts_ms,
    );
    let sig = signer.signing_key().sign(&msg);
    let req = InviteRequestSubmit {
        actor_id: hex::encode(claimant.actor_id().0),
        handle: handle.to_string(),
        message: message.to_string(),
        timestamp: ts_ms,
        signature: hex::encode(sig.to_bytes()),
        age_claim: None,
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn status_payload(kp: &ActorKeypair) -> Bytes {
    let req = InviteRequestStatusQuery {
        actor_id: hex::encode(kp.actor_id().0),
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Signed `invite_request.cancel` payload — signature over the tagged
/// `invite_cancel_signed_message`.
fn cancel_payload(kp: &ActorKeypair, ts_ms: u64) -> Bytes {
    cancel_signed_by(kp, kp, ts_ms)
}

fn cancel_signed_by(signer: &ActorKeypair, claimant: &ActorKeypair, ts_ms: u64) -> Bytes {
    let msg = fauna_protocol::invite::invite_cancel_signed_message(&claimant.actor_id().0, ts_ms);
    let sig = signer.signing_key().sign(&msg);
    let req = InviteRequestCancel {
        actor_id: hex::encode(claimant.actor_id().0),
        timestamp: ts_ms,
        signature: hex::encode(sig.to_bytes()),
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn verify_payload(code: &str) -> Bytes {
    let req = InviteCodeVerify {
        code: code.to_string(),
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

async fn dispatch(
    router: &RpcRouter,
    kind: &str,
    st: Arc<AppState>,
    p: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(st, [0u8; 32], p).await
}

// ── submit ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_creates_pending_request() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let kp = ActorKeypair::generate();

    let out = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        submit_payload(&kp, "alice", "let me in", now_ms()),
    )
    .await
    .expect("submit ok");
    let reply: InviteRequestStatus = decode(&out).unwrap();
    assert_eq!(reply.handle, "alice");
    assert_eq!(reply.status, "pending");
    assert_eq!(reply.actor_id, hex::encode(kp.actor_id().0));

    assert!(
        db.get_invite_request_by_actor(&kp.actor_id().0)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn submit_rejects_duplicate() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();

    dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st.clone(),
        submit_payload(&kp, "alice", "", now_ms()),
    )
    .await
    .expect("first submit ok");
    let err = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        submit_payload(&kp, "alice", "", now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invite_request_exists");
}

#[tokio::test]
async fn submit_rejects_registered_actor() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let kp = ActorKeypair::generate();
    db.create_user_with_handle(&kp.actor_id().0, "free", "existing", None)
        .await
        .unwrap();
    let st = state(db);
    let r = router();

    let err = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        submit_payload(&kp, "alice", "", now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.actor_exists");
}

#[tokio::test]
async fn submit_rejects_taken_handle() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let owner = ActorKeypair::generate();
    db.create_user_with_handle(&owner.actor_id().0, "free", "taken", None)
        .await
        .unwrap();
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        submit_payload(&kp, "taken", "", now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.handle_taken");
}

#[tokio::test]
async fn submit_rejects_bad_signature() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();
    let wrong = ActorKeypair::generate();

    let err = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        submit_signed_by(&wrong, &kp, "alice", "", now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.signature_failed");
}

#[tokio::test]
async fn submit_rejects_invalid_handle() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();

    // Too short (< 3) → validate_handle rejects.
    let err = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        submit_payload(&kp, "ab", "", now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invalid_request");
}

#[tokio::test]
async fn submit_rejects_stale_timestamp() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();

    let stale = now_ms() - 600_000; // 10 min ago, past the 30-s window.
    let err = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        submit_payload(&kp, "alice", "", stale),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invalid_request");
}

// ── status ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn status_returns_row_then_not_found() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();

    // No row yet.
    let err = dispatch(
        &r,
        "fauna.account.invite_request.status",
        st.clone(),
        status_payload(&kp),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invite_request_not_found");

    // After submit, status returns the row.
    dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st.clone(),
        submit_payload(&kp, "alice", "", now_ms()),
    )
    .await
    .expect("submit ok");
    let out = dispatch(
        &r,
        "fauna.account.invite_request.status",
        st,
        status_payload(&kp),
    )
    .await
    .expect("status ok");
    let reply: InviteRequestStatus = decode(&out).unwrap();
    assert_eq!(reply.status, "pending");
    assert_eq!(reply.handle, "alice");
}

// ── cancel ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_deletes_pending_request() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();

    dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st.clone(),
        submit_payload(&kp, "alice", "", now_ms()),
    )
    .await
    .expect("submit ok");
    let out = dispatch(
        &r,
        "fauna.account.invite_request.cancel",
        st.clone(),
        cancel_payload(&kp, now_ms()),
    )
    .await
    .expect("cancel ok");
    let reply: InviteRequestCancelReply = decode(&out).unwrap();
    assert!(reply.ok);

    // The row is gone — status now 404.
    let err = dispatch(
        &r,
        "fauna.account.invite_request.status",
        st,
        status_payload(&kp),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invite_request_not_found");
}

#[tokio::test]
async fn cancel_not_found_when_no_request() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(
        &r,
        "fauna.account.invite_request.cancel",
        st,
        cancel_payload(&kp, now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invite_request_not_found");
}

#[tokio::test]
async fn cancel_rejects_bad_signature() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();
    let kp = ActorKeypair::generate();
    let wrong = ActorKeypair::generate();

    let err = dispatch(
        &r,
        "fauna.account.invite_request.cancel",
        st,
        cancel_signed_by(&wrong, &kp, now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.signature_failed");
}

// ── verify ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn verify_valid_code_returns_invite_id() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_invite_code("WELCOME2026", "free", 5)
        .await
        .unwrap();
    let st = state(db);
    let r = router();

    let out = dispatch(
        &r,
        "fauna.account.invite_code.verify",
        st,
        verify_payload("WELCOME2026"),
    )
    .await
    .expect("verify ok");
    let reply: InviteCodeVerifyReply = decode(&out).unwrap();
    assert_eq!(reply.invite_id, "WELCOME2026");
}

#[tokio::test]
async fn verify_invalid_code_rejected() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();

    let err = dispatch(
        &r,
        "fauna.account.invite_code.verify",
        st,
        verify_payload("NOPE"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invite_code_invalid");
}

#[tokio::test]
async fn malformed_payload_rejected() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db);
    let r = router();

    let err = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── row 85: denied-row TTL sweep ─────────────────────────────────────────────
//
// A denied row otherwise persists forever (only `UPDATE invite_requests` site
// is deny; no expiry) and is the only thing blocking that actor's resubmit —
// `onboarding.md` § The pending-invite surface.

#[tokio::test]
async fn denied_row_survives_inside_ttl_then_resubmit_succeeds_once_pruned() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let st = state(db.clone());
    let r = router();
    let kp = ActorKeypair::generate();
    let admin = [7u8; 32];

    dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st.clone(),
        submit_payload(&kp, "alice", "let me in", now_ms()),
    )
    .await
    .expect("submit ok");
    let row = db
        .get_invite_request_by_actor(&kp.actor_id().0)
        .await
        .unwrap()
        .expect("row exists");
    db.deny_invite_request(row.id, &admin, Some("not ready"))
        .await
        .unwrap();

    // Resubmit is refused while the denied row exists — the one-row-per-actor
    // invariant the client-side cancel-then-submit sequence relies on.
    let err = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st.clone(),
        submit_payload(&kp, "alice", "", now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.account.invite_request_exists");

    // Inside the TTL, status still serves the denial + reason (the
    // pending-invite surface's `Denied{reason}` read).
    let out = dispatch(
        &r,
        "fauna.account.invite_request.status",
        st.clone(),
        status_payload(&kp),
    )
    .await
    .expect("status ok");
    let reply: InviteRequestStatus = decode(&out).unwrap();
    assert_eq!(reply.status, "denied");
    assert_eq!(reply.denial_reason.as_deref(), Some("not ready"));

    // Simulate the TTL elapsing: prune with a cutoff far past any real
    // `decided_at`. No cancel is sent — this is the mechanism, not the
    // client-side cancel-then-submit sequence.
    let pruned = db
        .prune_denied_invite_requests_older_than(9_999_999_999)
        .await
        .unwrap();
    assert_eq!(pruned, 1);

    // The actor can submit fresh with no cancel — the row's disappearance
    // clears `invite_core::submit_invite_request_core`'s AlreadyExists block.
    let out = dispatch(
        &r,
        "fauna.account.invite_request.submit",
        st,
        submit_payload(&kp, "alice", "second try", now_ms()),
    )
    .await
    .expect("resubmit after TTL prune ok");
    let reply: InviteRequestStatus = decode(&out).unwrap();
    assert_eq!(reply.status, "pending");
    assert_eq!(reply.message, "second try");
}
