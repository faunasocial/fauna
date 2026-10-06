//! Integration — the shared onboarding **`WsRpcNestApi`**
//! (`fauna_onboarding_machine::nest_api::ws_rpc_impl`) drives all eight wizard
//! nest calls over the native anonymous WS-RPC connector
//! (`fauna_anon_client::AnonymousNestClient`, `GET /api/v1/ws`, no bearer) against a
//! real in-process nest. Proves the *client-side* kind→wire-type mapping and the
//! `RpcError.code` → per-endpoint error mapping that `reqwest_impl.rs` did over
//! HTTP — written once in the shared crate, generic over the transport.
//!
//! Socket-level nest plumbing is proven by `pre_identity_ws.rs`; the connector
//! by `anonymous_client_roundtrip.rs`. This proves the onboarding mapping layer
//! that sits between them. Slice: tracked internally (S3).

use std::sync::Arc;

use ed25519_dalek::Signer;

use fauna_anon_client::AnonymousNestClient;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_onboarding_machine::nest_api::{
    ClaimAdminError, InviteCodeError, InviteRequestBody, InviteRequestError, NatModeError,
    NodeMode, RegisterBody, RegisterError, SilentChallengeOutcome, WsRpcNestApi,
};

/// Spin an in-process nest serving the anonymous endpoint with every
/// pre-identity onboarding kind registered + allowlisted. Returns the `http://`
/// base URL (the connector swaps the scheme to `ws://`).
async fn start() -> String {
    start_with_db().await.0
}

/// [`start`], also handing back the nest's db so a test can seed rows the
/// anonymous surface cannot create (e.g. an already-registered actor).
async fn start_with_db() -> (String, Arc<CacheDb>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::claim_handlers::register_claim_handlers(&mut b);
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            fauna_nest::invite_handlers::register_invite_handlers(&mut b);
            fauna_nest::nat_mode_handlers::register_nat_mode_handlers(&mut b);
            // The handle-check silent sign-in (`fauna.auth.{challenge,verify}`).
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), db)
}

async fn connect(base: &str) -> WsRpcNestApi<AnonymousNestClient> {
    let client = AnonymousNestClient::connect(base)
        .await
        .expect("open anonymous connection");
    WsRpcNestApi::new(client)
}

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

/// Deterministic Ed25519 keypair from a 32-byte seed → (secret_hex, actor_hex).
fn keypair(seed: u8) -> (ed25519_dalek::SigningKey, String) {
    let sk = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let actor_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, actor_hex)
}

#[tokio::test]
async fn probe_setup_status_round_trips() {
    let base = start().await;
    let api = connect(&base).await;
    let status = api
        .probe_setup_status()
        .await
        .expect("setup.status should round-trip");
    // `for_test` answers the heartbeat; the NAT axis is the one non-trivial
    // field projection in this method (`probe_setup_status_carries_node_mode`
    // pins its value).
    assert!(
        status.node_mode.is_some(),
        "the heartbeat carries the NAT axis"
    );
}

#[tokio::test]
async fn claim_admin_local_bad_secret_is_invalid_identity() {
    let base = start().await;
    let api = connect(&base).await;
    // Non-hex secret → local failure, never leaves the client.
    let err = api
        .claim_admin("ABCDEF", "zz", "admin", None)
        .await
        .expect_err("a corrupt secret hex must fail locally");
    assert!(
        matches!(err, ClaimAdminError::InvalidIdentity { .. }),
        "bad secret hex → InvalidIdentity, got {err:?}"
    );
}

#[tokio::test]
async fn claim_admin_server_rejection_maps_to_already_claimed() {
    let base = start().await;
    let api = connect(&base).await;
    // A valid secret → the client signs correctly, so the signature verifies
    // server-side; with no claim-code file provisioned the core returns
    // `already_claimed`, which maps to its own dedicated `AlreadyClaimed`
    // variant (not the generic `Invalid` — a different code would not help
    // here, so the client shows its own message rather than leaking the raw
    // wire code through `Invalid`'s `{reason}` substitution). This proves the
    // internal-signing path AND the server-rejection mapping in one shot.
    let secret_hex = hex::encode([3u8; 32]);
    let err = api
        .claim_admin("ABCDEF", &secret_hex, "admin", None)
        .await
        .expect_err("unclaimed-file nest rejects the claim");
    assert!(
        matches!(err, ClaimAdminError::AlreadyClaimed),
        "server claim rejection (already_claimed) → AlreadyClaimed, got {err:?}"
    );
}

#[tokio::test]
async fn probe_setup_status_carries_node_mode() {
    // The resolved NAT axis rides the same heartbeat: the nest's lowercase
    // wire string (`"public"` — the `for_test` default) parses into the
    // client's `NodeMode`, the seed for the `nat_mode_choice` pre-selection.
    let base = start().await;
    let api = connect(&base).await;
    let status = api
        .probe_setup_status()
        .await
        .expect("setup.status should round-trip");
    assert_eq!(
        status.node_mode,
        Some(NodeMode::Public),
        "for_test resolves the public default"
    );
}

#[tokio::test]
async fn submit_nat_mode_unclaimed_nest_maps_to_invalid() {
    // The full V2 client path over the real wire: `submit_nat_mode` reads the
    // nest's possession-proven identity + advert off this connection, signs
    // the nest-bound V2 body, and submits. On an unclaimed nest the core
    // rejects with `not_claimed` (4xx-class → `Invalid`) — and the reason
    // MENTIONING the claim is the end-to-end discriminator: `not_claimed`
    // fires only AFTER the signature verified, so reaching it proves the
    // bound nest_id survived client→wire→handler→core and the V2 bytes
    // verified (a dropped or wrong binding would fail as `signature_failed` /
    // `invalid_request` first).
    let base = start().await;
    let api = connect(&base).await;
    let (sk, _actor_hex) = keypair(8);
    let secret_hex = hex::encode(sk.to_bytes());
    let err = api
        .submit_nat_mode(&secret_hex, NodeMode::Private)
        .await
        .expect_err("an unclaimed nest must reject the commit");
    match &err {
        NatModeError::Invalid { reason } => assert!(
            reason.contains("claim"),
            "the V2 signature must verify (reaching the claim gate), got reason {reason:?}"
        ),
        other => panic!("nat-mode commit on an unclaimed nest → Invalid, got {other:?}"),
    }
}

#[tokio::test]
async fn nest_handshake_read_learns_the_identity_the_commit_binds() {
    // The identity source the nest-bound commit signs with, over the real
    // wire: `read_login_binding` on a plaintext connection (possession-only —
    // no captured SPKI) learns the in-process nest's identity from the real
    // `fauna.auth.nest_handshake` handler.
    let base = start().await;
    let client = AnonymousNestClient::connect(&base)
        .await
        .expect("open anonymous connection");
    let read = fauna_client_core::nest_trust::read_login_binding(&client, None)
        .await
        .expect("a current nest proves an identity");
    assert_eq!(read.len(), 32, "a 32-byte nest identity");
}

#[tokio::test]
async fn register_on_closed_nest_maps_to_failed() {
    let base = start().await;
    let api = connect(&base).await;
    let (_, actor_hex) = keypair(5);
    // `for_test` disables open registration → `registration_closed`. The wizard
    // treats register as binary, so any failure is `Failed`.
    let body = RegisterBody {
        actor_id: actor_hex,
        handle: "bob".into(),
        timestamp: now_ms(),
        signature: hex::encode([0u8; 64]),
        invite_code: None,
        age_claim: None,
    };
    let err = api
        .register(body)
        .await
        .expect_err("registration is closed on a for_test nest");
    assert!(
        matches!(err, RegisterError::Failed { .. }),
        "register failure → Failed, got {err:?}"
    );
}

#[tokio::test]
async fn invite_request_submit_then_recheck_round_trips() {
    let base = start().await;
    let api = connect(&base).await;
    let (sk, actor_hex) = keypair(6);
    let handle = "alice";
    let message = "";
    let ts = now_ms();

    // Signed message: the tagged invite-submit form.
    let signed = fauna_protocol::invite::invite_submit_signed_message(
        &sk.verifying_key().to_bytes(),
        handle,
        message,
        ts,
    );
    let signature = hex::encode(sk.sign(&signed).to_bytes());

    let body = InviteRequestBody {
        actor_id: actor_hex.clone(),
        handle: handle.into(),
        message: message.into(),
        timestamp: ts,
        signature,
        age_claim: None,
    };
    let submitted = api
        .submit_invite_request(body)
        .await
        .expect("submit should create a pending invite request");
    assert_eq!(submitted.status, "pending");
    // The WS reply carries no quota cell → the projection yields None.
    assert!(
        submitted.quota.is_none(),
        "WS invite status carries no quota"
    );

    // Re-reading the same actor's request returns the same pending row.
    let rechecked = api
        .recheck_invite_request(&actor_hex)
        .await
        .expect("recheck should find the row just submitted");
    assert_eq!(rechecked.id, submitted.id);
    assert_eq!(rechecked.status, "pending");
}

/// A submit from an actor the nest already holds — a suspended one included,
/// by the registration doors' ruling (`login.md` § Errors) — answers
/// `fauna.account.actor_exists`. That refusal is permanent until the admin
/// restores, so it must map to the typed terminal `AlreadyRegistered`, never
/// the catch-all `Transient` whose "try again" would be a lie.
#[tokio::test]
async fn invite_request_submit_by_registered_actor_maps_to_already_registered() {
    let (base, db) = start_with_db().await;
    let api = connect(&base).await;
    let (sk, actor_hex) = keypair(7);
    db.create_user(&sk.verifying_key().to_bytes(), "free", "bob")
        .await
        .expect("seed a registered actor");
    let handle = "bob";
    let ts = now_ms();
    let signed = fauna_protocol::invite::invite_submit_signed_message(
        &sk.verifying_key().to_bytes(),
        handle,
        "",
        ts,
    );
    let body = InviteRequestBody {
        actor_id: actor_hex,
        handle: handle.into(),
        message: String::new(),
        timestamp: ts,
        signature: hex::encode(sk.sign(&signed).to_bytes()),
        age_claim: None,
    };
    let err = api
        .submit_invite_request(body)
        .await
        .expect_err("a registered actor must be refused");
    assert!(
        matches!(err, InviteRequestError::AlreadyRegistered),
        "fauna.account.actor_exists → AlreadyRegistered, got {err:?}"
    );
}

#[tokio::test]
async fn recheck_unknown_actor_maps_to_not_found() {
    let base = start().await;
    let api = connect(&base).await;
    let (_, unknown_actor) = keypair(9); // never submitted a request
    let err = api
        .recheck_invite_request(&unknown_actor)
        .await
        .expect_err("an actor with no request must be NotFound");
    assert!(
        matches!(err, InviteRequestError::NotFound),
        "unknown actor → NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn verify_unknown_invite_code_maps_to_invalid() {
    let base = start().await;
    let api = connect(&base).await;
    let err = api
        .verify_invite_code("does-not-exist")
        .await
        .expect_err("an unknown code must be rejected");
    assert!(
        matches!(err, InviteCodeError::Invalid { .. }),
        "unknown invite code → Invalid, got {err:?}"
    );
}

#[tokio::test]
async fn silent_challenge_unregistered_round_trips() {
    // The handle-check silent sign-in over WS-RPC: the onboarding
    // `WsRpcNestApi::silent_challenge` decodes the secret hex, then drives the
    // shared `fauna_protocol::auth::run_silent_challenge` ceremony (challenge →
    // sign the tagged `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id` → verify) over the
    // anonymous connector. A fresh
    // nest has no registered actors, so verify returns `fauna.auth.not_registered`
    // → `NotRegistered`. This proves the onboarding mapping layer end-to-end (the
    // ceremony itself is also covered by `launch_machine_auth_roundtrip.rs`).
    let base = start().await;
    let api = connect(&base).await;
    let secret_hex = hex::encode([7u8; 32]);
    let outcome = api.silent_challenge(&secret_hex).await;
    assert!(
        matches!(outcome, SilentChallengeOutcome::NotRegistered),
        "unregistered actor → NotRegistered, got {outcome:?}"
    );
}

#[tokio::test]
async fn silent_challenge_bad_secret_hex_is_secret_invalid() {
    // A non-hex secret is a *local* identity failure — it never opens a round
    // trip. Terminal (`SecretInvalid`), distinct from the retryable transient
    // bucket. Mirrors `claim_admin`'s local-bad-secret guard.
    let base = start().await;
    let api = connect(&base).await;
    let outcome = api.silent_challenge("zz-not-hex").await;
    assert!(
        matches!(outcome, SilentChallengeOutcome::SecretInvalid { .. }),
        "bad secret hex → SecretInvalid, got {outcome:?}"
    );
}
