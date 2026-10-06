//! Integration — the production [`WsNestApi`] wrapper
//! (`fauna_onboarding_machine::nest_api`) behind `Arc<dyn NestApi>`, the exact
//! shape `OnboardingMachine` holds, driven against a real in-process nest over
//! the anonymous WS-RPC connection (`GET /api/v1/ws`, no bearer).
//!
//! Where `onboarding_ws_rpc_roundtrip.rs` proves the transport-generic
//! `WsRpcNestApi<R>` *mapping* (a connection constructed by hand), this proves
//! what `WsNestApi` adds on top: the dyn-compatible `NestApi` boxing, the
//! fresh-connection-per-call lifecycle, and the `provider_base_urls["nest"]`
//! override (the E2E handle-domain-probe redirect). Slice: tracked
//! internally (S3b).

mod common;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ed25519_dalek::{Signer, SigningKey};

use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_onboarding_machine::NestApi;
use fauna_onboarding_machine::nest_api::{InviteRequestBody, WsNestApi};

/// Spin an in-process nest serving the anonymous endpoint with every
/// pre-identity onboarding kind registered + allowlisted. Returns the `http://`
/// base URL (the connector swaps the scheme to `ws://`).
async fn start() -> String {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::claim_handlers::register_claim_handlers(&mut b);
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            fauna_nest::invite_handlers::register_invite_handlers(&mut b);
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
    format!("http://{addr}")
}

/// The happy path through the boxed trait object: a probe round-trips, and a
/// second sequential call opens its own fresh connection and round-trips too.
/// Proves connect + boxing + that back-to-back calls are independent.
#[tokio::test]
async fn probe_round_trips_through_dyn_nest_api() {
    let base = start().await;
    let api: Arc<dyn NestApi> = Arc::new(WsNestApi::new(
        None,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
    ));

    // Every nest reports `Encrypted`: user content rests **sealed** on every nest
    // and the storage-mode axis is retired (no-modes, ratified 2026-07-12 —
    // `nest/storage-modes.md`). Asserting the parsed mode proves the reply
    // actually decoded, not just that a frame came back.
    let first = api
        .probe_setup_status(&base)
        .await
        .expect("first probe opens an anonymous connection and round-trips");
    assert!(
        first.node_mode.is_some(),
        "the heartbeat carries the NAT axis"
    );

    // A second call opens its own fresh connection and round-trips independently.
    let second = api
        .probe_setup_status(&base)
        .await
        .expect("second probe round-trips on its own fresh connection");
    assert_eq!(second.node_mode, first.node_mode);
}

/// The invite-request half of the "join an existing nest" flow through the boxed
/// trait object: a real `submit_invite_request` creates a pending row, and a
/// `recheck_invite_request` finds the same row — both happy paths over the
/// anonymous WS (each on its own fresh connection). The `test_invite_request_*`
/// Python e2e injects snapshots and never drives these for real.
#[tokio::test]
async fn invite_submit_then_recheck_through_dyn_nest_api() {
    let base = start().await;
    let api: Arc<dyn NestApi> = Arc::new(WsNestApi::new(
        None,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
    ));

    let sk = SigningKey::from_bytes(&[11u8; 32]);
    let actor_hex = hex::encode(sk.verifying_key().to_bytes());
    let handle = "bob";
    let message = "";
    let timestamp = fauna_core::data::Timestamp::now_millis();

    // Signed message: the tagged invite-submit form the nest's invite handler
    // verifies (mirrors `OnboardingMachine`).
    let signed = fauna_protocol::invite::invite_submit_signed_message(
        &sk.verifying_key().to_bytes(),
        handle,
        message,
        timestamp,
    );
    let body = InviteRequestBody {
        actor_id: actor_hex.clone(),
        handle: handle.into(),
        message: message.into(),
        timestamp,
        signature: hex::encode(sk.sign(&signed).to_bytes()),
        age_claim: None,
    };

    let submitted = api
        .submit_invite_request(&base, body)
        .await
        .expect("submit creates a pending invite request over the anonymous WS");
    assert_eq!(submitted.status, "pending");

    let rechecked = api
        .recheck_invite_request(&base, &actor_hex)
        .await
        .expect("recheck finds the row just submitted");
    assert_eq!(rechecked.id, submitted.id);
    assert_eq!(rechecked.status, "pending");
}

/// A server rejection maps through the wrapper to the per-endpoint error: an
/// unknown actor's invite-request recheck → `NotFound` (the wizard reads this as
/// "admin removed the request; reset").
#[tokio::test]
async fn recheck_unknown_actor_maps_to_not_found_through_dyn_nest_api() {
    let base = start().await;
    let api: Arc<dyn NestApi> = Arc::new(WsNestApi::new(
        None,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
    ));

    let err = api
        .recheck_invite_request(&base, &"ab".repeat(32))
        .await
        .expect_err("no pending request for this actor");
    assert!(
        matches!(
            err,
            fauna_onboarding_machine::nest_api::InviteRequestError::NotFound
        ),
        "unknown actor → NotFound, got {err:?}"
    );
}

/// The `provider_base_urls["nest"]` override redirects every call to the
/// configured nest, ignoring the per-call `base_url`. This is the E2E
/// handle-domain-probe path: onboarding probes `https://{handle-domain}` while
/// the fixture redirects "nest" to the local test server.
#[tokio::test]
async fn nest_override_redirects_to_local_nest() {
    let base = start().await;
    let mut urls = HashMap::new();
    urls.insert("nest".to_string(), base);
    let api: Arc<dyn NestApi> = Arc::new(WsNestApi::new(
        Some(urls),
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
    ));

    // The caller URL is unreachable; only the override makes this round-trip.
    let status = api
        .probe_setup_status("https://handle-domain.invalid")
        .await
        .expect("override redirects the probe to the local nest");
    assert!(
        status.node_mode.is_some(),
        "the local nest answered the probe"
    );
}

/// `Arc<dyn NestApi>` must stay `Send` on native (the supertrait relaxed to
/// `MaybeSendSync`, which is `Send + Sync` here) — the wizard drives nest calls
/// from a `tokio::spawn`ed background task. Spawning a call proves it compiles
/// and runs as a `Send` future.
#[tokio::test]
async fn dyn_nest_api_is_send_for_tokio_spawn() {
    let base = start().await;
    let api: Arc<dyn NestApi> = Arc::new(WsNestApi::new(
        None,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
    ));
    let mode = tokio::spawn(async move { api.probe_setup_status(&base).await })
        .await
        .expect("spawned task joins")
        .expect("probe round-trips from a spawned task")
        .node_mode;
    assert!(mode.is_some(), "the heartbeat carries the NAT axis");
}
