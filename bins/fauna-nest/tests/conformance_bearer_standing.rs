//! Every bearer door asks the actor's standing at USE time, not only at mint.
//!
//! A bearer outlives the moment it was minted: the teardown that suspension
//! and lockout run revokes the tokens it can see, but a bearer validated
//! before the standing changed — or one the revocation never reached — is
//! still a valid string in the token store. The WS-RPC dispatch gate
//! (`caller_class_for_actor`, re-read per RPC) refused such a bearer's
//! *calls*, and until this file's fix it was the only door that asked: the
//! HTTP extractors served its byte uploads and downloads, and the WS upgrade
//! registered its socket, so it **received Push frames** — the exact harm the
//! suspend teardown exists to end (`admin.md` § 2 Users → *Cutting a user
//! off*; `transport-connection.md` § Connection lifecycle → *Revocation
//! teardown*).
//!
//! The fix is one validator, `auth::validate_bearer{,_session}`, that every
//! door calls: `token_store` validity **plus** the standing question the
//! dispatch gate asks, answered by that same function. These pins insert a
//! bearer straight into the token store for an actor whose standing is then
//! revoked (the shape the teardown harness uses), so what is observed is the
//! use-time check alone, never the teardown's token revocation.
//!
//! Each refusal arm is paired with a healthy-actor control on the same door,
//! so a door that refuses everyone cannot pass for one that asks.
//!
//! Tier: tier_3 (real `AppState`, real axum server, real HTTP and a real
//! WebSocket over tungstenite, real `CacheDb` — no mocks).

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::token_store::TokenStore;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

struct Nest {
    addr: std::net::SocketAddr,
    state: Arc<AppState>,
    /// Keeps the backup service's directory alive for the nest's lifetime.
    _dir: tempfile::TempDir,
}

async fn start() -> Nest {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        auth: fauna_nest::state::AuthState {
            token_store: Arc::new(TokenStore::new()),
            ..Default::default()
        },
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Nest {
        addr,
        state,
        _dir: dir,
    }
}

#[derive(Clone, Copy, Debug)]
enum Standing {
    Healthy,
    Suspended,
    LockedOut,
}

/// A registered actor in `standing`, holding a bearer that was inserted into
/// the token store directly — so no teardown ever had the chance to revoke it.
async fn actor_with_bearer(
    nest: &Nest,
    handle: &str,
    standing: Standing,
) -> (ActorKeypair, String) {
    let kp = ActorKeypair::generate();
    let id = kp.actor_id().0;
    nest.state
        .db
        .create_user_with_handle(&id, "personal", handle, None)
        .await
        .unwrap();
    let token = nest
        .state
        .auth
        .token_store
        .insert(kp.actor_id(), 3600)
        .await;
    match standing {
        Standing::Healthy => {}
        Standing::Suspended => {
            assert!(
                nest.state
                    .db
                    .suspend_user_now(&id, "test", "other")
                    .await
                    .unwrap(),
                "precondition: the suspension must land"
            );
        }
        Standing::LockedOut => lock(nest, &id).await,
    }
    (kp, token)
}

async fn lock(nest: &Nest, id: &[u8; 32]) {
    let until = fauna_core::data::Timestamp::now_secs() + 3600;
    nest.state
        .db
        .set_locked_until(&id[..], Some(until))
        .await
        .unwrap();
}

async fn post_chunk(nest: &Nest, token: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{}/api/v1/chunks", nest.addr))
        .header("authorization", format!("Bearer {token}"))
        .body(b"a chunk a revoked actor must not be able to store".to_vec())
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// A mailbox-export session that does not exist: a bearer the door accepts is
/// answered 404 by the handler's row lookup, a bearer it refuses 401 — so the
/// standing check is observable without driving a whole export.
async fn get_export_blob(nest: &Nest, token: &str) -> u16 {
    reqwest::Client::new()
        .get(format!(
            "http://{}/api/v1/export/no-such-session",
            nest.addr
        ))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// Attempt the authenticated WS upgrade; `Ok(())` when the nest answered 101.
async fn try_upgrade(nest: &Nest, kp: &ActorKeypair, token: &str) -> Result<(), u16> {
    let url = format!(
        "ws://{}/api/v1/ws/{}",
        nest.addr,
        hex::encode(kp.actor_id().0)
    );
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{token}").parse().unwrap(),
    );
    match tokio_tungstenite::connect_async(req).await {
        Ok(_) => Ok(()),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
        Err(e) => panic!("the upgrade failed below HTTP: {e}"),
    }
}

// --- POST /api/v1/chunks (BulkWriteAuth's session arm) ---------------------

#[tokio::test]
async fn a_healthy_bearer_stores_a_chunk() {
    let nest = start().await;
    let (_, token) = actor_with_bearer(&nest, "alice", Standing::Healthy).await;
    assert_eq!(post_chunk(&nest, &token).await, 201);
}

#[tokio::test]
async fn a_suspended_actors_bearer_stores_no_chunk() {
    let nest = start().await;
    let (_, token) = actor_with_bearer(&nest, "alice", Standing::Suspended).await;
    assert_eq!(
        post_chunk(&nest, &token).await,
        401,
        "a suspended actor's surviving bearer wrote to the byte store — the \
         chunk door trusts the mint and never asks the actor's standing"
    );
}

#[tokio::test]
async fn a_locked_out_actors_bearer_stores_no_chunk() {
    let nest = start().await;
    let (_, token) = actor_with_bearer(&nest, "alice", Standing::LockedOut).await;
    assert_eq!(
        post_chunk(&nest, &token).await,
        401,
        "a locked-out actor's surviving bearer wrote to the byte store — the \
         emergency lockout does not reach the chunk door"
    );
}

/// The door asks the dispatch gate's question in the dispatch gate's order, so
/// an admin resolves before the lockout read and a locked-out (sole) admin is
/// not bricked off the byte plane — the same ruling `caller_class_for_actor`
/// makes for dispatch.
#[tokio::test]
async fn a_locked_out_admin_keeps_the_chunk_door() {
    let nest = start().await;
    let (kp, token) = actor_with_bearer(&nest, "admin", Standing::Healthy).await;
    nest.state
        .db
        .add_admin_actor(&kp.actor_id().0[..])
        .await
        .unwrap();
    lock(&nest, &kp.actor_id().0).await;
    assert_eq!(post_chunk(&nest, &token).await, 201);
}

// --- GET /api/v1/export/{session_id} (the mailbox-export blob door) ---------
//
// A whole sealed mailbox leaves through this door, so it answers the account
// export's standing question too — with the byte
// plane's `401`, not the account route's `403` (`mail-export.md` § Download
// flow → *Standing and the owner's notice*).

#[tokio::test]
async fn a_healthy_bearer_passes_the_mailbox_export_door() {
    let nest = start().await;
    let (_, token) = actor_with_bearer(&nest, "alice", Standing::Healthy).await;
    assert_eq!(get_export_blob(&nest, &token).await, 404);
}

#[tokio::test]
async fn a_suspended_actors_bearer_is_refused_at_the_mailbox_export_door() {
    let nest = start().await;
    let (_, token) = actor_with_bearer(&nest, "alice", Standing::Suspended).await;
    assert_eq!(
        get_export_blob(&nest, &token).await,
        401,
        "a suspended actor's surviving bearer passed the mailbox-export door \
         (404 is the handler answering, 401 the door refusing)"
    );
}

#[tokio::test]
async fn a_locked_out_actors_bearer_is_refused_at_the_mailbox_export_door() {
    let nest = start().await;
    let (_, token) = actor_with_bearer(&nest, "alice", Standing::LockedOut).await;
    assert_eq!(
        get_export_blob(&nest, &token).await,
        401,
        "a locked-out actor's surviving bearer passed the mailbox-export door — \
         the emergency lockout does not stop a whole mailbox leaving"
    );
}

// --- GET /api/v1/ws/{actor} (the authenticated WS upgrade) ------------------

#[tokio::test]
async fn a_healthy_bearer_upgrades() {
    let nest = start().await;
    let (kp, token) = actor_with_bearer(&nest, "alice", Standing::Healthy).await;
    assert_eq!(try_upgrade(&nest, &kp, &token).await, Ok(()));
}

/// Refused at the upgrade, before `register_upgraded_connection` — so the
/// socket never enters `WsState.subs` and no Push frame can reach it.
#[tokio::test]
async fn a_suspended_actors_bearer_opens_no_socket_and_receives_no_push() {
    let nest = start().await;
    let (kp, token) = actor_with_bearer(&nest, "alice", Standing::Suspended).await;
    assert_eq!(
        try_upgrade(&nest, &kp, &token).await,
        Err(401),
        "a suspended actor's surviving bearer opened a WebSocket — registered, \
         it receives the Push stream the suspend teardown exists to end"
    );
    assert!(
        nest.state.ws.connections_for(&kp.actor_id().0).is_empty(),
        "the refused upgrade must leave no connection to push to"
    );
}

#[tokio::test]
async fn a_locked_out_actors_bearer_opens_no_socket_and_receives_no_push() {
    let nest = start().await;
    let (kp, token) = actor_with_bearer(&nest, "alice", Standing::LockedOut).await;
    assert_eq!(
        try_upgrade(&nest, &kp, &token).await,
        Err(401),
        "a locked-out actor's surviving bearer opened a WebSocket — registered, \
         it receives the Push stream the emergency lockout exists to end"
    );
    assert!(
        nest.state.ws.connections_for(&kp.actor_id().0).is_empty(),
        "the refused upgrade must leave no connection to push to"
    );
}
