//! `fauna.sync.devices.list`'s `online` — the device↔session binding
//! (`docs/goal/behavior/devices.md` § Listing Devices → *The binding*).
//!
//! A device is online while a connection **bound to it** is open on the nest:
//! a WS-RPC connection upgraded with a bearer that
//! `fauna.auth.device_handshake` minted under the row's own granted device key
//! (`sync_devices.auth_device_key`, the roster's `principal`). That is what
//! every app seat and every per-user sync agent holds — the account runtime's
//! principal client — and until 2026-09-22 the handler read only the legacy
//! daemon's data-plane seats, so the Devices page painted every device a user
//! actually owns offline.
//!
//! What these pins hold, each on the production chain (real handlers, real
//! `TokenStore`, real `WsState`, the real upgrade registration):
//!
//! - a grant-minted session's live connection paints its device online, and
//!   the device goes back offline when that connection is gone;
//! - a seed-minted session (a handshake or challenge/verify bearer — the app's
//!   own primary session) binds no device, however connected it is;
//! - the join is scoped to the actor: another actor's connection carrying the
//!   same key paints nothing on this actor's roster;
//! - retiring the grant reads offline at once, before the socket dies — the
//!   roster row no longer carries the key, so the verdict follows the row and
//!   never the lingering socket (whose closure is device removal's own duty);
//! - the custody handshake's overloaded `minted_by_device` (the custodian's
//!   own actor id) never becomes a binding;
//! - `last_seen_at` is touched, on the actor's own row, by the bound upgrade;
//! - and, over a real socket, the HTTP upgrade itself carries the key from
//!   the validated bearer onto the connection.
//!
//! Tier: tier_3 (real handlers + real token store + real DB; the last pin
//! drives a real axum server). Every assertion is on latency-independent state
//! (e2e convention 14): the one wait is a deadline poll on the registry, never
//! a sleep-then-assert.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;
use fauna_core::identity::ActorKeypair;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{auth_core, db::CacheDb, session_handlers, sync_handlers};
use fauna_protocol::encode_canonical;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

/// One in-process nest with the sync + session control surfaces registered,
/// plus the account whose root key signs the grants.
async fn nest() -> (RpcRouter, Arc<AppState>, ActorKeypair) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    sync_handlers::register_sync_handlers(&mut b);
    session_handlers::register_sessions_handlers(&mut b);
    let account = ActorKeypair::generate();
    common::seed_dispatch_actor(&state.db, &account.actor_id().0).await;
    (b.build(), state, account)
}

/// Dispatch one kind through the real registered handler, as the actor.
async fn dispatch<Req, Reply>(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Req,
) -> Result<Reply, fauna_protocol::RpcError>
where
    Req: serde::Serialize,
    Reply: serde::de::DeserializeOwned,
{
    let meta = router.kind_meta(kind).expect("kind registered");
    let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
    let reply = (meta.handler)(Arc::clone(state), actor, bytes).await?;
    Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
}

/// Register a device row + attach a root-signed renewal grant, exactly as the
/// production provision does. Returns `(renewal seed, device key)`.
async fn enroll(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    device_id_hex: &str,
) -> ([u8; 32], [u8; 32]) {
    let _: fauna_protocol::sync::SyncRegisterReply = dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.register",
        fauna_protocol::sync::SyncRegisterRequest {
            device_id: device_id_hex.to_string(),
            label: "the user's laptop".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("device registers");
    let (grant, seed) = common::fresh_device_grant(account);
    let reply: fauna_protocol::sync::DeviceGrantRegisterReply = dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex.to_string(),
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await
    .expect("grant registers");
    assert!(reply.registered);
    let device_key = ed25519_dalek::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes();
    (seed, device_key)
}

/// Mint a bearer over the production handshake path with the renewal seed —
/// the mint the account runtime's principal client and every sync agent run.
async fn mint_via_handshake(
    state: &Arc<AppState>,
    account: &ActorKeypair,
    renewal_seed: &[u8; 32],
    nonce: &[u8],
) -> auth_core::TokenMint {
    let signing = ed25519_dalek::SigningKey::from_bytes(renewal_seed);
    let device_key = signing.verifying_key().to_bytes();
    let now_ms = fauna_core::data::Timestamp::now_millis();
    let nest_id = state.bound_identity();
    let msg = fauna_protocol::auth::device_handshake_signed_message(
        &account.actor_id().0,
        &device_key,
        now_ms,
        &nest_id,
        nonce,
    );
    let sig = signing.sign(&msg);
    auth_core::device_auth_core(
        state,
        &account.actor_id_hex(),
        &fauna_core::hex32::encode(&device_key),
        now_ms,
        &sig.to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        nonce,
        &fauna_core::hex32::encode(&nest_id),
    )
    .await
    .expect("the enrolled grant mints")
}

/// The production upgrade's registration for a bearer, minus the HTTP layer:
/// validate the bearer as `ws_handler` does, derive the bound key as it does,
/// register the connection as `handle_ws` does. Returns the connection.
async fn upgrade_with(state: &Arc<AppState>, token: &str) -> Arc<fauna_nest::ws::RpcConnection> {
    let session = state
        .auth
        .token_store
        .validate_with_session(token)
        .await
        .expect("bearer validates");
    let actor_id = session.actor_id.0;
    let bound = fauna_nest::ws::bound_device_key_for(&actor_id, session.minted_by_device);
    let (conn, _rx) = state
        .register_upgraded_connection(actor_id, Some(session.token_id), bound)
        .await;
    conn
}

async fn roster(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
) -> Vec<fauna_protocol::sync::SyncDevice> {
    let reply: fauna_protocol::sync::SyncDevicesListReply = dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.devices.list",
        fauna_protocol::sync::SyncDevicesListRequest {
            extra: Default::default(),
        },
    )
    .await
    .expect("devices list");
    reply.devices
}

async fn online(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    device_id_hex: &str,
) -> bool {
    roster(router, state, account)
        .await
        .into_iter()
        .find(|d| d.device_id == device_id_hex)
        .expect("the device is listed")
        .online
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_minted_sessions_live_connection_paints_its_device_online() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x51; 32]);
    let (seed, _key) = enroll(&router, &state, &account, &device_id_hex).await;
    assert!(
        !online(&router, &state, &account, &device_id_hex).await,
        "an enrolled device with no connection is offline"
    );

    let minted = mint_via_handshake(&state, &account, &seed, b"nonce-online").await;
    let conn = upgrade_with(&state, &minted.token).await;
    assert!(
        online(&router, &state, &account, &device_id_hex).await,
        "the grant-minted session's live connection binds the device"
    );

    state.ws.remove(&account.actor_id().0, conn.conn_id);
    assert!(
        !online(&router, &state, &account, &device_id_hex).await,
        "the device is offline again once its bound connection is gone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seed_minted_session_binds_no_device() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x52; 32]);
    let _ = enroll(&router, &state, &account, &device_id_hex).await;

    // The app's own primary bearer: minted by the identity seed (handshake or
    // challenge/verify), tagged with no device.
    let app_session = state
        .auth
        .token_store
        .insert(account.actor_id(), auth_core::TOKEN_TTL_SECS)
        .await;
    let conn = upgrade_with(&state, &app_session).await;
    assert!(
        state.ws.has_connections(&account.actor_id().0),
        "the seed session is connected"
    );
    assert!(conn.bound_device_key.is_none());
    assert!(
        !online(&router, &state, &account, &device_id_hex).await,
        "a seed-minted session names an identity, not a device — it binds nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn another_actors_connection_with_the_same_key_paints_nothing_here() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x53; 32]);
    let (_seed, key) = enroll(&router, &state, &account, &device_id_hex).await;

    // A connection of a DIFFERENT actor that somehow carries this actor's
    // device key: registered straight into the registry, since no mint path
    // could produce it — the join must still be scoped to the actor's own
    // subscription entry.
    let stranger = ActorKeypair::generate();
    let (_conn, _rx) = state.ws.subscribe_with_session(
        stranger.actor_id().0,
        Some("feedfeedfeedfeed".into()),
        Some(key),
    );
    assert!(
        !online(&router, &state, &account, &device_id_hex).await,
        "the roster join runs inside the actor's own entry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retiring_the_grant_reads_offline_before_the_socket_dies() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x54; 32]);
    let (seed, key) = enroll(&router, &state, &account, &device_id_hex).await;
    let minted = mint_via_handshake(&state, &account, &seed, b"nonce-retire").await;
    let _conn = upgrade_with(&state, &minted.token).await;
    assert!(online(&router, &state, &account, &device_id_hex).await);

    // `fauna.sync.device_grant.revoke` over the app/user arm: the named row
    // keeps its label and memberships and loses its grant columns. The socket
    // the retired key opened is deliberately left in the registry here — its
    // closure is device removal's own duty (transport-connection.md
    // § Revocation teardown), not this verdict's.
    let reply: fauna_protocol::sync::DeviceGrantRevokeReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.revoke",
        fauna_protocol::sync::DeviceGrantRevokeRequest {
            device_key: fauna_core::hex32::encode(&key),
            timestamp_ms: None,
            nonce: None,
            signature: None,
            extra: Default::default(),
        },
    )
    .await
    .expect("grant retires");
    assert!(reply.revoked);
    assert!(
        state
            .ws
            .has_connection_bound_to(&account.actor_id().0, &key),
        "the socket the retired key opened is still registered (the fixture)"
    );
    assert!(
        !online(&router, &state, &account, &device_id_hex).await,
        "the verdict follows the row: no principal on the row, nothing to bind"
    );
}

#[test]
fn the_custody_handshakes_overloaded_tag_never_binds() {
    // `fauna.auth.custody_handshake` mints a session whose actor IS the
    // custodian and tags it `minted_by_device = Some(custodian actor id)` so
    // the sessions list can label it — the one mint whose tag is not a device
    // key. Its signature is the tag equalling the connection's own actor.
    let actor = [0xc1; 32];
    let device_key = [0xd1; 32];
    assert_eq!(
        fauna_nest::ws::bound_device_key_for(&actor, Some(device_key)),
        Some(device_key),
        "a device-handshake mint binds its key"
    );
    assert_eq!(
        fauna_nest::ws::bound_device_key_for(&actor, Some(actor)),
        None,
        "a custody mint's tag is the custodian's own identity, never a device"
    );
    assert_eq!(
        fauna_nest::ws::bound_device_key_for(&actor, None),
        None,
        "a seed mint carries no tag"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bound_upgrade_touches_last_seen_on_the_actors_own_row() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x55; 32]);
    let (_seed, key) = enroll(&router, &state, &account, &device_id_hex).await;

    // The exact write the bound registration performs, on its own so the row
    // it targets is the assertion — the actor's row carrying the key, one row,
    // and never another actor's.
    let touched = state
        .db
        .touch_device_last_seen_by_principal(&account.actor_id().0, &key)
        .await
        .expect("touch");
    assert_eq!(touched, 1, "exactly the row carrying the principal");
    let stranger = ActorKeypair::generate();
    let touched = state
        .db
        .touch_device_last_seen_by_principal(&stranger.actor_id().0, &key)
        .await
        .expect("touch");
    assert_eq!(
        touched, 0,
        "another actor's touch reaches no row of this actor"
    );

    let row = roster(&router, &state, &account)
        .await
        .into_iter()
        .find(|d| d.device_id == device_id_hex)
        .unwrap();
    assert!(
        row.last_seen_at >= row.registered_at,
        "last_seen never reads before the row's own registration"
    );
}

/// The HTTP upgrade itself: a real socket opened with a device-minted bearer
/// registers a connection that carries the key — `ws_handler`'s arm, which the
/// in-process pins above bypass.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_upgrade_with_a_device_minted_bearer_binds_the_connection() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x56; 32]);
    let (seed, key) = enroll(&router, &state, &account, &device_id_hex).await;
    let minted = mint_via_handshake(&state, &account, &seed, b"nonce-wire").await;

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let actor = account.actor_id().0;
    let url = format!("ws://{addr}/api/v1/ws/{}", hex::encode(actor));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{}", minted.token)
            .parse()
            .unwrap(),
    );
    let (_ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // The 101 is out before the server side registers the connection (the
    // upgrade window); poll the registry for the state the upgrade produces
    // rather than sleeping past it.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
    while !state.ws.has_connection_bound_to(&actor, &key) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the upgraded connection never registered bound to the device key"
        );
        tick.tick().await;
    }
    assert!(
        online(&router, &state, &account, &device_id_hex).await,
        "the device the real socket's bearer was minted under is online"
    );
}
