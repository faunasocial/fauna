//! Integration test — bridge revocation teardown (`transport.md` § Connection
//! lifecycle → *Revocation teardown*; `mail-bridge-lifecycle.md` § Service-user
//! re-keying).
//!
//! Bridge service-user revocation is *explicitly compromise-motivated* (the
//! rotate affordance treats the old keys as leaked), yet pre-fix it only
//! flipped the enrollment row: the revoked MDA's live socket stayed in
//! `WsState.subs`, and `BridgePushRegistry::emit → notify_push` kept streaming
//! per-user mailbox-state metadata to it — a Push is not an RPC, so no
//! capability gate ever runs on that path.
//!
//! Two wire-level properties:
//!
//! 1. **The revoked bridge's socket closes 4401 and its push stream stops.**
//! 2. **A reconnect does not re-arm the stream.** A revoked bridge keeps its
//!    `users` row (approval inserted it; the mint gate never consults
//!    `bridge_service_users`), so a leaked key CAN re-mint. Two things keep
//!    that bearer from the stream, each pinned here: the WS upgrade asks the
//!    actor's standing through the one bearer validator (a revoked bridge has
//!    no caller class), so the socket is refused `401` and never lands in
//!    `WsState.subs[pk]`; and the registry purge
//!    (`BridgePushRegistry::remove_mda`) leaves no stale subscription that
//!    would deliver to any socket that did — a Push is not an RPC, so the
//!    capability gate never runs on that path.
//!
//! The production sequence under test — DB flip, then
//! `AppState::revoke_actor_authority` — is exactly what all three revocation
//! call sites run; the unit pins in `bridge_blob_handlers` prove the handlers
//! call it (a missing-caller regression is theirs to catch).
//!
//! Tier: tier_3 (real `AppState`, real axum server, real WebSocket — no mocks).

use std::sync::Arc;
use std::time::Duration;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::bridge_routing::MailboxStateEvent;
use fauna_protocol::{Frame, decode_frame};
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

struct Harness {
    url: String,
    kp: ActorKeypair,
    state: Arc<fauna_nest::routes::AppState>,
}

async fn start() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    let pk = kp.actor_id().0;

    // A real enrolled-and-approved MDA. Approval inserts the audit `users`
    // row — the reason a revoked bridge can still mint (property 2).
    db.create_pending_bridge_service_user(&pk, BridgeRole::Mda, "mda-1")
        .await
        .unwrap();
    db.approve_bridge_service_user(&pk, None).await.unwrap();

    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new(RpcRouter::builder().build()),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Harness {
        url: format!("ws://{addr}"),
        kp,
        state,
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open a bearer-authed WS as the bridge, minting a fresh token — the same
/// thing the bridge's challenge/verify re-mint yields, since the mint gate
/// reads only `users.{suspended, locked_until}` and a revoked bridge's row
/// carries neither.
#[allow(clippy::result_large_err)] // test helper: the tungstenite error is returned as-is for the caller to match on
async fn try_open_as_bridge(h: &Harness) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let token = h.state.auth.token_store.insert(h.kp.actor_id(), 3600).await;
    let url = format!("{}/api/v1/ws/{}", h.url, hex::encode(h.kp.actor_id().0));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{token}").parse().unwrap(),
    );
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

async fn open_as_bridge(h: &Harness) -> Ws {
    let ws = try_open_as_bridge(h)
        .await
        .expect("bridge upgrade should succeed");
    // Let the server-side subscription land in `WsState.subs`.
    tokio::time::sleep(Duration::from_millis(50)).await;
    ws
}

fn emit_mailbox_change(h: &Harness, served_user: &[u8; 32]) {
    h.state.bridge_push_registry.emit(
        &h.state.ws,
        served_user,
        "INBOX",
        &MailboxStateEvent::Append {
            uid: 42,
            flags: vec![],
            modseq: 1,
        },
    );
}

/// Read frames until a mailbox-state Push, a Close, or the deadline. Returns
/// (saw_push, close_code).
async fn read_for(ws: &mut Ws, window: Duration) -> (bool, Option<u16>) {
    let overall = tokio::time::Instant::now() + window;
    let mut saw_push = false;
    loop {
        let remaining = overall.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return (saw_push, None);
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Err(_) | Ok(None) | Ok(Some(Err(_))) => return (saw_push, None),
            Ok(Some(Ok(Message::Close(frame)))) => {
                return (saw_push, frame.map(|f| u16::from(f.code)));
            }
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                if let Ok(Frame::Push(p)) = decode_frame(&bytes)
                    && p.kind == "fauna.bridges.push.mailbox_state"
                {
                    saw_push = true;
                }
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Properties 1 + 2 in one flow, because 2's precondition is 1's aftermath.
#[tokio::test]
async fn revoked_bridge_socket_closes_4401_and_a_reconnect_is_refused() {
    let h = start().await;
    let pk = h.kp.actor_id().0;
    let served_user = [1u8; 32];

    // Live MDA with a mailbox-state subscription; prove the stream works so
    // a later silence means "revoked", not "pushes never worked".
    let mut ws = open_as_bridge(&h).await;
    h.state
        .bridge_push_registry
        .register(&served_user, "INBOX", pk);
    emit_mailbox_change(&h, &served_user);
    let (saw_push, close) = read_for(&mut ws, Duration::from_secs(2)).await;
    assert!(saw_push, "precondition: the approved MDA receives pushes");
    assert_eq!(close, None, "precondition: no close yet");

    // The production revocation sequence (all three call sites).
    assert!(h.state.db.revoke_bridge_service_user(&pk).await.unwrap());
    h.state.revoke_actor_authority(&pk).await;

    // Property 1 — the live socket closes 4401 (clear bearer, re-auth).
    let (_, close) = read_for(&mut ws, Duration::from_secs(8)).await;
    assert_eq!(
        close,
        Some(4401),
        "a revoked bridge's socket must close 4401, not linger"
    );

    // Property 2 — the leaked key re-mints (the mint gate does not refuse
    // it), but the upgrade asks the bridge's standing and refuses the socket…
    match try_open_as_bridge(&h).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => assert_eq!(
            resp.status().as_u16(),
            401,
            "a revoked bridge's re-minted bearer must be refused at the upgrade"
        ),
        Ok(_) => panic!(
            "a revoked bridge's re-minted bearer opened a socket — registered, \
             it is back in `WsState.subs` for every Push addressed to it"
        ),
        Err(e) => panic!("the upgrade failed below HTTP: {e}"),
    }
    // …and the purged registry holds nothing that would deliver to one.
    assert!(
        h.state
            .bridge_push_registry
            .matching(&served_user, "INBOX")
            .is_empty(),
        "a revoked bridge's mailbox-state subscriptions must be purged — stale \
         registry entries re-arm the leak with zero dispatches"
    );
}
