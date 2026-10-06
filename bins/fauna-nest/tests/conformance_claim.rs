//! In-process WS-RPC handler tests for the pre-identity one-time admin-claim
//! kind `fauna.auth.claim_admin` — dispatch the registered handler directly (no
//! socket), exercising the shared `claim_core::claim_admin_core` ceremony and
//! the `ClaimError` → `RpcError` mapping. The HTTP-twin behavior is covered by
//! `storage_mode_api.rs` (which drives `POST /api/v1/claim-admin` to set up an
//! admin); socket-level coverage (the anonymous endpoint resolving the kind over
//! the wire) lives in `pre_identity_ws.rs`. Slice tracked internally.
//!
//! Unlike `conformance_register`, the claim ceremony reads a **claim-code file**
//! derived from `config.nest.db_path`'s parent dir, so the fixture builds an
//! `AppState` with `db_path` in a tempdir and writes a known code there (mirrors
//! the `storage_mode_api.rs` harness). `AppState::for_test` sets `db_path=""`,
//! which would fall back to `/data`.

mod common;
use common::config_with_db_path;

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;
use tempfile::TempDir;

use fauna_core::identity::ActorKeypair;
use fauna_nest::claim_handlers::register_claim_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::AuthState;
use fauna_protocol::claim::{ClaimAdminReply, ClaimAdminRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

const DOMAIN: &str = "test.fauna.social";
const CLAIM_CODE: &str = "ABCDEF";

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_claim_handlers(&mut b);
    b.build()
}

/// `AppState` over a tempdir-backed `db_path` with a `claim-code` file present
/// (the unclaimed state). `claim_admin_core` reads `state.auth.registration.
/// handle_domain` (set to `DOMAIN`), `state.auth.token_store` (the default), and
/// `state.db`. Returns the `TempDir` so the caller keeps it alive.
fn state_with_claim_code(db: Arc<CacheDb>, code: Option<&str>) -> (Arc<AppState>, TempDir) {
    let data_dir = tempfile::tempdir().unwrap();
    if let Some(code) = code {
        std::fs::write(data_dir.path().join("claim-code"), code).unwrap();
    }
    let db_path = data_dir
        .path()
        .join("nest.db")
        .to_string_lossy()
        .into_owned();
    let state = Arc::new(AppState {
        config: config_with_db_path(db_path),
        auth: AuthState {
            registration: RegistrationConfig {
                handle_domain: Some(DOMAIN.to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..AppState::for_test(db)
    });
    (state, data_dir)
}

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

/// Build a signed `fauna.auth.claim_admin` payload. Signs exactly as the
/// production client (`WsRpcNestApi::claim_admin`) does: the domain-tagged
/// `signature` over `CLAIM_ADMIN_V1 ‖ actor_id ‖ timestamp_be` (tagged-only —
/// the finding; no untagged accept exists).
fn claim_payload(kp: &ActorKeypair, claim_code: &str, handle: &str, timestamp_ms: u64) -> Bytes {
    let tagged = fauna_protocol::claim::claim_admin_signed_message(&kp.actor_id().0, timestamp_ms);
    let sig = kp.signing_key().sign(&tagged);
    let req = ClaimAdminRequest {
        claim_code: claim_code.to_string(),
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: timestamp_ms,
        signature: hex::encode(sig.to_bytes()),
        handle: handle.to_string(),
        mail_domain: None,
        ..Default::default()
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router
        .kind_meta("fauna.auth.claim_admin")
        .expect("kind registered");
    (meta.handler)(state, [0u8; 32], payload).await
}

#[tokio::test]
async fn claim_succeeds_and_grants_admin() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db.clone(), Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    // A handle is required to claim — it is set atomically as part of the claim.
    let out = dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "admin", now_ms()))
        .await
        .expect("claim ok");
    let reply: ClaimAdminReply = decode(&out).unwrap();
    assert!(!reply.token.is_empty());
    assert_eq!(reply.domain, DOMAIN);
    assert_eq!(reply.handle, "admin", "the claim sets the required handle");
    assert!(
        reply.expires_at > now_ms() / 1000,
        "token expiry in the future"
    );

    // The claim is real: the actor is registered, admin (superadmin role), AND
    // its handle is resolvable (so it is a routable identity, not a degenerate
    // handle-less admin).
    assert!(db.is_actor_registered(&kp.actor_id().0).await.unwrap());
    assert!(db.is_admin(&kp.actor_id().0).await.unwrap());
    assert_eq!(
        db.get_admin_role(&kp.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("superadmin")
    );
    assert_eq!(
        db.resolve_handle("admin").await.unwrap(),
        Some(kp.actor_id().0)
    );
}

/// box-recovery.md § Mechanism (build-order step 2 — claim-reply seed hand-off): a
/// successful claim returns the nest's **deployment signing seed** (hex) to the
/// claiming admin's client so it can custody the identity off-box and re-install it
/// after total box loss. The returned seed must be the box's ACTUAL identity — the
/// preimage of its `nest_actor_id` — so the rebuilt box re-presents the same id.
#[tokio::test]
async fn claim_reply_hands_off_the_deployment_seed() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    std::fs::write(data_dir.path().join("claim-code"), CLAIM_CODE).unwrap();
    let db_path = data_dir
        .path()
        .join("nest.db")
        .to_string_lossy()
        .into_owned();

    // The box's deployment identity — production loads this into
    // `nest_signing_key` from the `nest_keypair` row at boot.
    let seed = [0x5au8; 32];
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);

    let state = Arc::new(AppState {
        config: config_with_db_path(db_path),
        auth: AuthState {
            registration: RegistrationConfig {
                handle_domain: Some(DOMAIN.to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        nest_signing_key: Some(signing_key),
        ..AppState::for_test(db)
    });

    let r = router();
    let kp = ActorKeypair::generate();
    let out = dispatch(&r, state, claim_payload(&kp, CLAIM_CODE, "admin", now_ms()))
        .await
        .expect("claim ok");
    let reply: ClaimAdminReply = decode(&out).unwrap();

    assert_eq!(
        reply.deployment_seed.as_deref(),
        Some(hex::encode(seed).as_str()),
        "the claim reply must hand off the box's deployment signing seed (64-char hex)"
    );
}

/// A nest with no loaded signing key (degenerate — should not happen post-boot)
/// simply omits the seed; the claim still succeeds. Guards the `Option` contract
/// so a missing key never fails the claim.
#[tokio::test]
async fn claim_reply_omits_seed_when_no_signing_key() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db.clone(), Some(CLAIM_CODE));
    // `for_test` leaves `nest_signing_key: None`.
    let r = router();
    let kp = ActorKeypair::generate();
    let out = dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "admin", now_ms()))
        .await
        .expect("claim ok");
    let reply: ClaimAdminReply = decode(&out).unwrap();
    assert!(
        reply.deployment_seed.is_none(),
        "no signing key ⇒ no seed in the reply (claim still succeeds)"
    );
}

/// The claimed admin gets a non-empty display `label`, defaulted to its handle.
/// Regression for the admin actor-picker reds (`test_admin_catch_all` /
/// `test_admin_role_address` / `test_web_authoring` `_first_actor` reading
/// `fauna.admin.users.list`'s `users[0].label`): the box claimer flows through
/// `create_user_with_handle`, which defaults `label = handle` (admin.md § Users —
/// "`UserRow.label` … defaults to the actor's handle at registration"), so the
/// `admin-dns` catch-all / role-address / web-author pickers have a selectable
/// name out of the box rather than only the raw actor-id. `claim_succeeds_*`
/// above asserts the *handle* is set; this guards the distinct *label* field on
/// the production claim path (the existing `conformance_admin` carol test covers
/// only the direct `create_user_with_handle` call, not the claim ceremony).
#[tokio::test]
async fn claim_defaults_admin_display_label_to_handle() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db.clone(), Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "admin", now_ms()))
        .await
        .expect("claim ok");

    let admin_row = db
        .get_user(&kp.actor_id().0)
        .await
        .unwrap()
        .expect("claimed admin has a user row");
    assert_eq!(
        admin_row.label, "admin",
        "claimed admin's display label should default to its handle (non-empty), \
         so the admin actor pickers can show a selectable name"
    );
}

#[tokio::test]
async fn claim_rejects_empty_handle() {
    // A nest cannot be claimed without a handle: the admin's handle is its mail
    // address + identity, and the canonical recipient alias is materialized from
    // it (mail-aliases.md § Kind 1). A handle-less admin is a degenerate,
    // unusable state. The wire type now makes an *absent* handle unrepresentable
    // (the request field is a required `String` — see
    // `claim::claim_admin_request_missing_handle_key_fails_to_decode`), so the
    // only representable handle-less form is an empty string, which `validate_handle`
    // rejects. The handle is checked AFTER the claim-code gate, so a
    // valid-code-but-empty-handle attempt surfaces the request-validation error
    // (not invalid_claim_code).
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db.clone(), Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "", now_ms()))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.auth.invalid_request");

    // The claim was refused outright: no admin was created, so the box stays
    // unclaimed and a retry WITH a handle still works (the single-use code was
    // not consumed by the rejected attempt).
    assert!(!db.is_actor_registered(&kp.actor_id().0).await.unwrap());
    assert!(!db.is_admin(&kp.actor_id().0).await.unwrap());
}

#[tokio::test]
async fn claim_with_handle_sets_it() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db.clone(), Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    let out = dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "Admin", now_ms()))
        .await
        .expect("claim ok");
    let reply: ClaimAdminReply = decode(&out).unwrap();
    // Handle is lowercased by the core (matching the HTTP twin).
    assert_eq!(reply.handle, "admin");
    assert_eq!(
        db.resolve_handle("admin").await.unwrap(),
        Some(kp.actor_id().0)
    );
}

#[tokio::test]
async fn claim_with_mail_domain_auto_registers_it() {
    // The wizard handle carries the mail domain (`alice@fauna.test`); the client
    // sends the bare local-part as the handle and the `@domain` suffix as
    // `mail_domain`. The nest auto-registers that domain as the (primary) mail
    // domain at claim, so the handle is a routable email out of the box — no
    // manual admin-dns add-domain step (`claim_admin_core::ensure_mail_domain_registered`).
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db.clone(), Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    let ts = now_ms();
    let tagged = fauna_protocol::claim::claim_admin_signed_message(&kp.actor_id().0, ts);
    let sig = kp.signing_key().sign(&tagged);
    let req = ClaimAdminRequest {
        claim_code: CLAIM_CODE.to_string(),
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        handle: "alice".into(),
        mail_domain: Some("Fauna.Test".into()), // mixed case → normalized lower
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

    dispatch(&r, st, payload).await.expect("claim ok");

    let dom = db
        .lookup_active_mail_domain("fauna.test")
        .await
        .unwrap()
        .expect("the handle's mail domain is auto-registered at claim");
    assert_eq!(dom.domain_name, "fauna.test");
    assert!(dom.is_primary, "first active domain is the primary");
}

#[tokio::test]
async fn claim_refuses_a_mail_domain_carrying_userinfo() {
    // `resolve_handle_domain(d).is_public_dns_name` alone is a negative
    // test — it also reads a domain like this as "public"
    // — so the door must refuse the syntax outright rather than let a
    // userinfo-carrying string reach the identity cache. The claim itself
    // still succeeds (mail-domain registration is best-effort); only the bad
    // string is refused.
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db.clone(), Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    let ts = now_ms();
    let tagged = fauna_protocol::claim::claim_admin_signed_message(&kp.actor_id().0, ts);
    let sig = kp.signing_key().sign(&tagged);
    let req = ClaimAdminRequest {
        claim_code: CLAIM_CODE.to_string(),
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        handle: "alice".into(),
        mail_domain: Some("nest.example.com@attacker.example".into()),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

    dispatch(&r, st, payload).await.expect("claim ok");

    assert!(
        db.list_active_mail_domains().await.unwrap().is_empty(),
        "a userinfo-carrying mail_domain must never reach the mail_domains table"
    );
}

#[tokio::test]
async fn claim_rejects_wrong_code() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db, Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(&r, st, claim_payload(&kp, "WRONG1", "admin", now_ms()))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.auth.invalid_claim_code");
}

#[tokio::test]
async fn claim_rejects_already_claimed() {
    // No claim-code file present → the nest is already claimed.
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db, None);
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "admin", now_ms()))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.auth.already_claimed");
}

/// The single-use guarantee must NOT rest solely on the post-claim file delete
/// succeeding. If that delete ever fails (a read-only cloud-init mount, an
/// `EBUSY`/transient FS error) the still-readable code must not let a *different*
/// actor become a SECOND admin: an admin already existing (`admin_count > 0`) is
/// the authoritative claimed signal, so a second claim is rejected even with the
/// code file present. Closes the latent second-admin hole + makes the
/// claimed-state robust to a lingering code (§ Client-state recoverability).
#[tokio::test]
async fn claim_rejects_second_admin_when_admin_exists_and_code_present() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, dir) = state_with_claim_code(db.clone(), Some(CLAIM_CODE));
    let r = router();

    // First actor claims and becomes admin.
    let first = ActorKeypair::generate();
    dispatch(
        &r,
        st.clone(),
        claim_payload(&first, CLAIM_CODE, "admin", now_ms()),
    )
    .await
    .expect("first claim ok");
    assert!(db.is_admin(&first.actor_id().0).await.unwrap());
    assert_eq!(db.admin_count().await.unwrap(), 1);

    // Simulate a lingering, still-readable claim code (the post-claim delete
    // failed — e.g. an un-unlinkable read-only mountpoint).
    std::fs::write(dir.path().join("claim-code"), CLAIM_CODE).unwrap();

    // A DIFFERENT actor presents the SAME valid code → rejected before any user
    // or admin is created (the gate runs ahead of the code/handle checks).
    let attacker = ActorKeypair::generate();
    let err = dispatch(
        &r,
        st.clone(),
        claim_payload(&attacker, CLAIM_CODE, "intruder", now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.auth.already_claimed");
    assert!(!db.is_admin(&attacker.actor_id().0).await.unwrap());
    assert!(
        !db.is_actor_registered(&attacker.actor_id().0)
            .await
            .unwrap()
    );
    assert_eq!(
        db.admin_count().await.unwrap(),
        1,
        "no second admin may be created from a lingering claim code"
    );
}

#[tokio::test]
async fn claim_rejects_bad_signature() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db, Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();
    let wrong = ActorKeypair::generate();

    // Sign with the wrong key but claim kp's actor_id.
    let ts = now_ms();
    let mut msg = Vec::with_capacity(40);
    msg.extend_from_slice(&kp.actor_id().0);
    msg.extend_from_slice(&ts.to_be_bytes());
    let sig = wrong.signing_key().sign(&msg);
    let req = ClaimAdminRequest {
        claim_code: CLAIM_CODE.into(),
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        handle: "admin".into(),
        mail_domain: None,
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

    let err = dispatch(&r, st, payload).await.unwrap_err();
    assert_eq!(err.code, "fauna.auth.signature_failed");
}

/// Item 1 of the finding — the escalation, closed at the wire: neither a
/// legacy bare login-shaped signature (over untagged `actor_id ‖ timestamp_be`)
/// nor a CURRENT login signature (tagged `AUTH_HANDSHAKE_V2`) is accepted as a
/// claim — the verifier is tagged-only and holds `signature` to the claim-admin
/// domain tag.
#[tokio::test]
async fn claim_rejects_login_signatures_bare_and_tagged() {
    let r = router();
    let kp = ActorKeypair::generate();

    let ts = now_ms();
    // (a) The pre-sweep bare shape (what `build_auth_request` used to sign).
    let mut bare_msg = Vec::with_capacity(40);
    bare_msg.extend_from_slice(&kp.actor_id().0);
    bare_msg.extend_from_slice(&ts.to_be_bytes());
    let bare_sig = kp.signing_key().sign(&bare_msg);
    // (b) A CURRENT login signature (the tagged, nest-bound handshake form).
    let login_msg = fauna_protocol::auth::handshake_signed_message(
        &kp.actor_id().0,
        ts,
        &[0x5e; 32],
        &[0x42; 32],
    );
    let login_sig = kp.signing_key().sign(&login_msg);

    for sig in [bare_sig, login_sig] {
        let (st, _dir) = state_with_claim_code(
            Arc::new(CacheDb::open_in_memory().unwrap()),
            Some(CLAIM_CODE),
        );
        let req = ClaimAdminRequest {
            claim_code: CLAIM_CODE.into(),
            actor_id: hex::encode(kp.actor_id().0),
            timestamp: ts,
            signature: hex::encode(sig.to_bytes()),
            handle: "admin".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = dispatch(&r, st, payload).await.unwrap_err();
        assert_eq!(err.code, "fauna.auth.signature_failed");
    }
}

#[tokio::test]
async fn claim_rejects_stale_timestamp() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db, Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    let stale = now_ms() - 600_000; // 10 minutes ago, past the 5-min window.
    let err = dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "admin", stale))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.auth.invalid_request");
}

#[tokio::test]
async fn claim_rejects_invalid_handle() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db, Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    // Too short (< 3) → validate_handle rejects before the claim ceremony.
    let err = dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "ab", now_ms()))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.auth.invalid_request");
}

#[tokio::test]
async fn claim_admits_a_reserved_handle_as_the_deliberate_carve_out() {
    // Pins the deliberate exemption documented in `claim_admin_core`: unlike
    // register/discovery/invite/handle-change, the claim ceremony does NOT
    // refuse a `RESERVED_HANDLES` name. Self-inflicted only (the box owner is
    // the sole affected party) — this guards against a future session
    // "fixing" the inconsistency without re-litigating the tradeoff (it
    // would break every test in this file that claims as "admin").
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db, Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    let out = dispatch(
        &r,
        st,
        claim_payload(&kp, CLAIM_CODE, "postmaster", now_ms()),
    )
    .await
    .expect("reserved handle is admitted at claim time");
    let reply: ClaimAdminReply = decode(&out).unwrap();
    assert_eq!(reply.handle, "postmaster");
}

#[tokio::test]
async fn claim_rejects_taken_handle() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    // A different actor already owns "taken".
    let owner = ActorKeypair::generate();
    db.create_user_with_handle(&owner.actor_id().0, "free", "taken", None)
        .await
        .unwrap();
    let (st, _dir) = state_with_claim_code(db, Some(CLAIM_CODE));
    let r = router();
    let kp = ActorKeypair::generate();

    let err = dispatch(&r, st, claim_payload(&kp, CLAIM_CODE, "taken", now_ms()))
        .await
        .unwrap_err();
    // Reuses the A3 account code — same "handle already taken" concept.
    assert_eq!(err.code, "fauna.account.handle_taken");
}

#[tokio::test]
async fn claim_rejects_malformed_payload() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (st, _dir) = state_with_claim_code(db, Some(CLAIM_CODE));
    let r = router();

    // Not a valid ClaimAdminRequest map → infra malformed code (before the core).
    let err = dispatch(&r, st, Bytes::from_static(&[0xff, 0xff, 0xff]))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
}
