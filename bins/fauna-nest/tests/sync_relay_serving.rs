//! Integration test — **relay serving, the nest half** (`file-sync.md`
//! § Relay serving): a process hosting resident engines announces the folders
//! it serves on its WS-RPC connection (`fauna.sync.serve.announce`), the relay
//! asks it for a chunk the store does not hold with a push on that one
//! connection (`fauna.sync.chunk.wanted`), and the bytes come back on the bulk
//! rail (`POST /api/v1/chunks/relay/{request_id}`, or `DELETE` for "I hold no
//! such chunk").
//!
//! Real server, real WS-RPC connections, real `DiskBlobStore`; the seat here is
//! a test client playing what the serving engine will be, and the reader is a
//! plain HTTP GET with the folder hint — the shape every app's download walk
//! already drives. What must hold:
//!
//! * an announced seat serves a **metadata-only** folder's chunk to the reader,
//!   and the store holds no copy afterwards;
//! * a `DELETE` refills the window at once — the reader is not made to wait out
//!   the 30 s fetch deadline;
//! * an answer from an actor the nest did not ask, and one for a request id it
//!   never issued, are refused and touch nothing — the asked seat still answers;
//! * a roster **member**'s announced connection is asked for the owner's
//!   shared folder, and its own actor's answer is taken;
//! * the announce admits only folders the actor owns or is a member of, on a
//!   device of its own account, bounded in count;
//! * a connection torn down by any of the four doors that reach a WS-RPC
//!   connection — session revoke, revoke-all-others, device removal,
//!   actor-wide — stops being a candidate, and the reachability verdict
//!   (`folder_content_reachable`) follows the announce both ways;
//! * every `RelayCache` arm holds through an announced seat: `Transient`
//!   (metadata-only, above), `Store` (a full folder) and `StoreIfAttributed`
//!   (a full folder of an owner who holds a metadata-only one — attributed by
//!   construction, so it caches).
//!
//! * a member whose account lives on **another nest** reads through the same
//!   arm under a byte-plane token minted for it (its write token, or the
//!   read-scoped twin), resolved through the cross-nest roster alone and
//!   re-checked at every request — a removal ends its reads inside the
//!   token's life; no other token purpose opens the arm, and a read token
//!   opens no write route (§ Relay serving → *A member on another nest*,
//!   step (5)).
//!
//! Every "the seat was asked" claim is anchored on the seat's own socket — the
//! push it receives — never on a settle-sleep.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fauna_core::data::ContentHash;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::db::channels::RebindPower;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::push_events::{KIND_SYNC_CHUNK_WANTED, SyncChunkWantedPayload};
use fauna_protocol::sync::{
    KIND_SYNC_SERVE_ANNOUNCE, SERVE_ANNOUNCE_MAX_FOLDERS, SyncServeAnnounceReply,
    SyncServeAnnounceRequest,
};
use fauna_protocol::wrapped_blob::{BulkByteAccess, BulkByteMintPurpose};
use fauna_protocol::{
    Frame, PushEvent, Reply, Request, Value, decode_frame, decode_strict, encode_canonical,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

mod common;
use common::{Ws, open_authed};

const OWNER_DEVICE: [u8; 32] = [0xd5; 32];
const MEMBER_DEVICE: [u8; 32] = [0xd6; 32];
const STRANGER_DEVICE: [u8; 32] = [0xd7; 32];
const FOLDER: &str = "photos";
/// The owner's folder is group-bound so the member arm of the reader's gate
/// has a roster to find the member on.
const GROUP_ID: [u8; 32] = [0x11; 32];
/// A member of the owner's folder whose account lives on another nest: a row
/// on the folder's cross-nest roster and nothing else on this nest.
const FOREIGN_MEMBER: [u8; 32] = [0xf0; 32];
const FOREIGN_HOME_NEST: [u8; 32] = [0xf1; 32];

/// Generous next to a healthy round trip (milliseconds), far short of the
/// relay's 30 s per-seat fetch deadline — so a reader that was made to wait
/// out that deadline fails the assertion rather than passing slowly.
const PROMPT: Duration = Duration::from_secs(10);

struct Harness {
    http_url: String,
    ws_url: String,
    state: Arc<AppState>,
    owner_kp: ActorKeypair,
    owner_token: String,
    member_kp: ActorKeypair,
    member_token: String,
    stranger_kp: ActorKeypair,
    stranger_token: String,
    /// `folders.id` of the owner's metadata-only, group-bound [`FOLDER`].
    folder_id: i64,
    /// `folders.id` of the stranger's own, same-named folder.
    stranger_folder_id: i64,
    store: Arc<dyn fauna_nest::blob_store::BlobStoreBackend>,
}

async fn start() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());

    let owner_kp = ActorKeypair::generate();
    let owner = owner_kp.actor_id().0;
    db.create_user(&owner, "free", "alice").await.ok();
    db.register_sync_device(&owner, &OWNER_DEVICE, "laptop", None, "write")
        .await
        .unwrap();
    let folder_id = db.create_folder(FOLDER, &owner).await.unwrap();
    let ok = db
        .update_folder_for_user(
            FOLDER,
            &owner,
            fauna_nest::db::FolderUpdate {
                residency: Some(Some("metadata_only")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(ok, "the owner's folder row must exist");
    assert!(
        db.set_folder_mls_group(FOLDER, &owner, Some(&GROUP_ID))
            .await
            .unwrap()
    );
    let owner_token = tokens.insert(owner_kp.actor_id(), 3600).await;
    // The share's own state: the owner claims the folder's channel, and the
    // Welcome relay records a member whose account lives on another nest —
    // no user row, no session and no device here.
    let channel = fauna_mls::types::ChannelId::from_group_id(&GROUP_ID).0;
    assert!(matches!(
        db.claim_folder_channel(&owner, &channel).await.unwrap(),
        fauna_nest::db::channels::ChannelClaimOutcome::Allowed
    ));
    db.register_foreign_channel_member(
        &channel,
        &FOREIGN_MEMBER,
        &FOREIGN_HOME_NEST,
        None,
        RebindPower::Standing,
    )
    .await
    .unwrap();

    let member_kp = ActorKeypair::generate();
    let member = member_kp.actor_id().0;
    db.create_user(&member, "free", "carol").await.ok();
    db.register_sync_device(&member, &MEMBER_DEVICE, "desktop", None, "write")
        .await
        .unwrap();
    db.register_actor_channel(&member, &channel).await.unwrap();
    let member_token = tokens.insert(member_kp.actor_id(), 3600).await;

    let stranger_kp = ActorKeypair::generate();
    let stranger = stranger_kp.actor_id().0;
    db.create_user(&stranger, "free", "bob").await.ok();
    db.register_sync_device(&stranger, &STRANGER_DEVICE, "nas", None, "write")
        .await
        .unwrap();
    let stranger_folder_id = db.create_folder(FOLDER, &stranger).await.unwrap();
    let stranger_token = tokens.insert(stranger_kp.actor_id(), 3600).await;

    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir);
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());
    let store = backup_svc.local_blob_store();

    // Same wiring `build_app_state` does: the resolver shares the
    // BackupService's blob store.
    let resolver = Arc::new(fauna_nest::chunk_relay::ChunkResolver::new(
        Some(store.clone()),
        None,
        false,
    ));
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        backup_service: Some(backup_svc),
        sync: fauna_nest::state::SyncState {
            chunk_resolver: resolver,
        },
        ..AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Harness {
        http_url: format!("http://{addr}"),
        ws_url: format!("ws://{addr}"),
        state,
        owner_kp,
        owner_token,
        member_kp,
        member_token,
        stranger_kp,
        stranger_token,
        folder_id,
        stranger_folder_id,
        store,
    }
}

fn local_ref(id: i64) -> String {
    format!("local:{id}")
}

/// Send one `fauna.sync.serve.announce` and return its Reply.
async fn announce(ws: &mut Ws, corr: u64, device: [u8; 32], folders: Vec<String>) -> Reply {
    let req = SyncServeAnnounceRequest {
        device_id: hex::encode(device),
        folders,
        ..Default::default()
    };
    let payload: Value = decode_strict(&encode_canonical(&req).unwrap()).unwrap();
    let frame = Frame::Request(Request {
        ty: Request::TYPE,
        correlation_id: corr,
        kind: KIND_SYNC_SERVE_ANNOUNCE.to_string(),
        idempotency_key: [corr as u8; 16],
        payload,
        replay_forbidden: Some(false),
        deadline_ms: None,
    });
    ws.send(Message::Binary(
        fauna_protocol::encode_frame(&frame).unwrap(),
    ))
    .await
    .unwrap();
    next_reply(ws).await
}

fn admitted(reply: &Reply) -> Vec<String> {
    assert!(reply.ok, "the announce must succeed: {:?}", reply.payload);
    let r: SyncServeAnnounceReply =
        decode_strict(&encode_canonical(&reply.payload).unwrap()).unwrap();
    r.admitted
}

/// The next Reply on `ws` (pushes and control frames skipped).
async fn next_reply(ws: &mut Ws) -> Reply {
    loop {
        if let Frame::Reply(r) = next_frame(ws).await {
            return r;
        }
    }
}

/// The next `fauna.sync.chunk.wanted` push on `ws`.
async fn next_wanted(ws: &mut Ws) -> SyncChunkWantedPayload {
    loop {
        if let Frame::Push(p) = next_frame(ws).await
            && p.kind == KIND_SYNC_CHUNK_WANTED
        {
            match PushEvent::from_push(&p.kind, p.payload) {
                PushEvent::SyncChunkWanted(w) => return w,
                other => panic!("expected SyncChunkWanted, got {other:?}"),
            }
        }
    }
}

async fn next_frame(ws: &mut Ws) -> Frame {
    loop {
        let msg = tokio::time::timeout(PROMPT, ws.next())
            .await
            .expect("a frame arrives in time")
            .expect("the socket stays open")
            .expect("no ws error");
        if let Message::Binary(b) = msg {
            return decode_frame(&b).expect("a frame");
        }
    }
}

fn chunk_url(h: &Harness, hash: &ContentHash) -> String {
    format!(
        "{}{}?{}={FOLDER}",
        h.http_url,
        fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest())),
        fauna_nest_http::paths::chunk_store::FOLDER_HINT_PARAM
    )
}

/// The reader's hinted GET, in the background so the test can play the seat.
fn spawn_get(
    url: String,
    bearer: String,
) -> tokio::task::JoinHandle<(reqwest::StatusCode, Vec<u8>, Duration)> {
    tokio::spawn(async move {
        let started = Instant::now();
        let resp = reqwest::Client::new()
            .get(&url)
            .bearer_auth(bearer)
            .send()
            .await
            .expect("GET completes");
        let status = resp.status();
        let body = resp.bytes().await.unwrap().to_vec();
        (status, body, started.elapsed())
    })
}

async fn post_answer(h: &Harness, token: &str, request_id: u64, body: Vec<u8>) -> u16 {
    reqwest::Client::new()
        .post(format!(
            "{}{}",
            h.http_url,
            fauna_nest_http::paths::chunk_store::chunk_relay_answer(request_id)
        ))
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .expect("POST completes")
        .status()
        .as_u16()
}

async fn delete_answer(h: &Harness, token: &str, request_id: u64) -> u16 {
    reqwest::Client::new()
        .delete(format!(
            "{}{}",
            h.http_url,
            fauna_nest_http::paths::chunk_store::chunk_relay_answer(request_id)
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("DELETE completes")
        .status()
        .as_u16()
}

async fn reachable(h: &Harness) -> bool {
    let row = h
        .state
        .db
        .get_folder_by_id(h.folder_id)
        .await
        .unwrap()
        .unwrap();
    fauna_nest::chunk_relay::folder_content_reachable(
        &h.state.ws,
        h.state.sync.chunk_resolver.foreign_seats(),
        &row,
    )
}

#[tokio::test]
async fn an_announced_seat_serves_a_metadata_only_folder_and_the_store_keeps_nothing() {
    let h = start().await;
    assert!(
        !reachable(&h).await,
        "a metadata-only folder with no holder connected reads unreachable"
    );
    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    let reply = announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await;
    assert_eq!(admitted(&reply), vec![local_ref(h.folder_id)]);
    assert!(
        reachable(&h).await,
        "an announced connection is a holder of the folder"
    );

    let data = b"bytes that live on the owner's laptop alone".to_vec();
    let hash = ContentHash::of_raw(&data);
    let get = spawn_get(chunk_url(&h, &hash), h.owner_token.clone());

    let wanted = next_wanted(&mut seat).await;
    assert_eq!(wanted.folder, local_ref(h.folder_id));
    assert_eq!(wanted.store_key, hex::encode(hash.digest()));
    assert_eq!(
        post_answer(&h, &h.owner_token, wanted.request_id, data.clone()).await,
        204
    );

    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200, "the reader is served the seat's bytes");
    assert_eq!(body, data);
    assert!(
        !h.store.exists(&hash).await.unwrap(),
        "a metadata-only folder's relayed bytes never rest on the nest"
    );
    assert_eq!(
        post_answer(&h, &h.owner_token, wanted.request_id, data).await,
        404,
        "an answered request is no longer pending"
    );
}

#[tokio::test]
async fn a_decline_refills_the_window_without_waiting_out_the_deadline() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await);

    let hash = ContentHash::of_raw(b"a chunk nobody holds");
    let get = spawn_get(chunk_url(&h, &hash), h.owner_token.clone());
    let wanted = next_wanted(&mut seat).await;
    assert_eq!(
        delete_answer(&h, &h.owner_token, wanted.request_id).await,
        204
    );

    let (status, _, elapsed) = get.await.unwrap();
    assert_eq!(status, 404, "no holder had it");
    assert!(
        elapsed < PROMPT,
        "a decline must settle the ask at once, not at the fetch deadline ({elapsed:?})"
    );
}

#[tokio::test]
async fn an_answer_from_another_actor_or_for_an_unissued_request_touches_nothing() {
    let h = start().await;
    assert_eq!(
        post_answer(&h, &h.owner_token, 9_999_999, b"x".to_vec()).await,
        404,
        "a request id never issued is refused"
    );
    assert_eq!(delete_answer(&h, &h.owner_token, 9_999_999).await, 404);

    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await);
    let data = b"only the asked seat may hand these over".to_vec();
    let hash = ContentHash::of_raw(&data);
    let get = spawn_get(chunk_url(&h, &hash), h.owner_token.clone());
    let wanted = next_wanted(&mut seat).await;

    assert_eq!(
        post_answer(&h, &h.stranger_token, wanted.request_id, data.clone()).await,
        404,
        "another actor's answer is refused"
    );
    assert_eq!(
        delete_answer(&h, &h.stranger_token, wanted.request_id).await,
        404,
        "another actor cannot decline on the asked seat's behalf"
    );
    assert_eq!(
        post_answer(&h, &h.owner_token, wanted.request_id, data.clone()).await,
        204,
        "the refused answers touched nothing: the asked seat still answers"
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data);
}

#[tokio::test]
async fn a_members_announced_seat_is_asked_for_the_owners_shared_folder() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.member_kp, &h.member_token).await;
    let reply = announce(&mut seat, 1, MEMBER_DEVICE, vec![local_ref(h.folder_id)]).await;
    assert_eq!(
        admitted(&reply),
        vec![local_ref(h.folder_id)],
        "a roster member may announce the shared folder"
    );

    let data = b"what the member wrote, held by the member alone".to_vec();
    let hash = ContentHash::of_raw(&data);
    let get = spawn_get(chunk_url(&h, &hash), h.owner_token.clone());
    let wanted = next_wanted(&mut seat).await;
    assert_eq!(
        post_answer(&h, &h.owner_token, wanted.request_id, data.clone()).await,
        404,
        "the owner is not the actor the nest asked"
    );
    assert_eq!(
        post_answer(&h, &h.member_token, wanted.request_id, data.clone()).await,
        204
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data);
    assert!(!h.store.exists(&hash).await.unwrap());
}

#[tokio::test]
async fn the_announce_admits_only_readable_folders_on_a_device_of_the_account() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.stranger_kp, &h.stranger_token).await;

    let reply = announce(
        &mut seat,
        1,
        STRANGER_DEVICE,
        vec![
            local_ref(h.folder_id),
            local_ref(h.stranger_folder_id),
            local_ref(h.stranger_folder_id),
            format!("foreign:{}", "ab".repeat(32)),
            "photos".to_string(),
            local_ref(987_654),
        ],
    )
    .await;
    assert_eq!(
        admitted(&reply),
        vec![local_ref(h.stranger_folder_id)],
        "only the stranger's own folder is admitted, once; another user's \
         folder, a foreign ref, a bare name and an absent row are left out"
    );
    assert!(
        !reachable(&h).await,
        "the owner's folder gained no holder from the stranger's announce"
    );

    let reply = announce(
        &mut seat,
        2,
        OWNER_DEVICE,
        vec![local_ref(h.stranger_folder_id)],
    )
    .await;
    assert!(!reply.ok, "a device of another account is refused");

    let too_many = (0..=SERVE_ANNOUNCE_MAX_FOLDERS as i64)
        .map(local_ref)
        .collect();
    let reply = announce(&mut seat, 3, STRANGER_DEVICE, too_many).await;
    assert!(
        !reply.ok,
        "an announce over the count bound is refused whole"
    );
}

#[tokio::test]
async fn a_revoked_connection_stops_being_a_candidate() {
    // Every teardown that reaches a WS-RPC connection, each through its
    // production `AppState` door — the revocation pin the relay's candidates
    // have.
    #[derive(Debug, Clone, Copy)]
    enum Teardown {
        Session,
        OtherSessions,
        Device,
        Actor,
    }
    const SEAT_DEVICE_KEY: [u8; 32] = [0x6b; 32];
    for teardown in [
        Teardown::Session,
        Teardown::OtherSessions,
        Teardown::Device,
        Teardown::Actor,
    ] {
        let h = start().await;
        let owner = h.owner_kp.actor_id();
        // The seat's own session, minted by a device key so the device door
        // reaches it; the reader is the roster member, whose session no
        // teardown here touches.
        let minted = h
            .state
            .auth
            .token_store
            .insert_with_metadata(owner, 3600, None, Some(SEAT_DEVICE_KEY))
            .await;
        let mut seat = open_authed(&h.ws_url, &h.owner_kp, &minted.token).await;
        admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await);
        assert!(reachable(&h).await, "{teardown:?}: announced");

        match teardown {
            Teardown::Session => {
                h.state
                    .revoke_session_authority(&owner.0, &minted.token_id)
                    .await
            }
            Teardown::OtherSessions => {
                let keep = h
                    .state
                    .auth
                    .token_store
                    .insert_with_metadata(owner, 3600, None, None)
                    .await;
                h.state
                    .revoke_other_sessions_authority(&owner.0, &keep.token_id)
                    .await;
            }
            Teardown::Device => {
                h.state
                    .revoke_device_authority(&owner.0, &SEAT_DEVICE_KEY)
                    .await;
            }
            Teardown::Actor => h.state.revoke_actor_authority(&owner.0).await,
        }
        assert!(
            !reachable(&h).await,
            "{teardown:?}: a revoked connection holds nothing for the verdict"
        );

        let hash = ContentHash::of_raw(b"asked of nobody");
        let (status, _, elapsed) = spawn_get(chunk_url(&h, &hash), h.member_token.clone())
            .await
            .unwrap();
        assert_eq!(status, 404, "{teardown:?}");
        assert!(
            elapsed < PROMPT,
            "{teardown:?}: a revoked connection is not asked, so nothing waits on it ({elapsed:?})"
        );
    }
}

/// The `Store` arm through an announced seat: a full folder of an owner who
/// holds no metadata-only folder rests the relayed bytes, the historical
/// re-hydration cache.
#[tokio::test]
async fn an_announced_answer_for_a_full_folder_rests_in_the_store() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.stranger_kp, &h.stranger_token).await;
    admitted(
        &announce(
            &mut seat,
            1,
            STRANGER_DEVICE,
            vec![local_ref(h.stranger_folder_id)],
        )
        .await,
    );
    let data = b"a full folder's chunk the store had lost".to_vec();
    let hash = ContentHash::of_raw(&data);
    // The stranger's own hint resolves their own same-named, full folder.
    let get = spawn_get(chunk_url(&h, &hash), h.stranger_token.clone());
    let wanted = next_wanted(&mut seat).await;
    assert_eq!(wanted.folder, local_ref(h.stranger_folder_id));
    assert_eq!(
        post_answer(&h, &h.stranger_token, wanted.request_id, data.clone()).await,
        204
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data);
    assert!(
        h.store.exists(&hash).await.unwrap(),
        "a full folder re-hydrates the store from the relayed bytes"
    );
}

/// The `StoreIfAttributed` arm through an announced seat: the owner holds a
/// metadata-only folder, so a full folder's hint caches only on the
/// answering seat's attribution — and an announced seat's answer is
/// attributed by construction, because the ask named the folder.
#[tokio::test]
async fn an_announced_answer_is_attributed_so_a_full_folder_still_caches() {
    let h = start().await;
    let owner = h.owner_kp.actor_id().0;
    let docs_id = h.state.db.create_folder("docs", &owner).await.unwrap();
    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(docs_id)]).await);

    let data = b"a full folder's chunk, owner also holds a metadata-only one".to_vec();
    let hash = ContentHash::of_raw(&data);
    let url = format!(
        "{}{}?{}=docs",
        h.http_url,
        fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest())),
        fauna_nest_http::paths::chunk_store::FOLDER_HINT_PARAM
    );
    let get = spawn_get(url, h.owner_token.clone());
    let wanted = next_wanted(&mut seat).await;
    assert_eq!(wanted.folder, local_ref(docs_id));
    assert_eq!(
        post_answer(&h, &h.owner_token, wanted.request_id, data.clone()).await,
        204
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data);
    assert!(
        h.store.exists(&hash).await.unwrap(),
        "the announced answer is attributed to the hinted full folder, so it rests"
    );
}

/// The hint's address form (`?folder_hash=<hex>`, no plaintext): a sealed
/// set's name rests blank on the nest, so the hash alone must find the row and
/// reach its announced seat; a malformed hash names no folder — `404`, asking
/// nobody, never a fall back to some other row.
#[tokio::test]
async fn a_hash_only_hint_relays_from_the_hinted_sets_seat() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await);

    let data = b"bytes of a set the nest knows only by hash".to_vec();
    let hash = ContentHash::of_raw(&data);
    let by_hash = |folder_hash: String| {
        format!(
            "{}{}?{}={folder_hash}",
            h.http_url,
            fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest())),
            fauna_nest_http::paths::chunk_store::FOLDER_HASH_HINT_PARAM
        )
    };

    let (status, _, _) = spawn_get(by_hash("not-hex".into()), h.owner_token.clone())
        .await
        .unwrap();
    assert_eq!(status, 404, "a malformed hash hint names no folder");

    let get = spawn_get(
        by_hash(hex::encode(fauna_core::path_crypto::set_name_hash(FOLDER))),
        h.owner_token.clone(),
    );
    let wanted = next_wanted(&mut seat).await;
    // The causal barrier: the first ask the seat received, so the malformed
    // hint above asked nobody.
    assert_eq!(wanted.folder, local_ref(h.folder_id));
    assert_eq!(
        post_answer(&h, &h.owner_token, wanted.request_id, data.clone()).await,
        204
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200, "the hash alone finds the set and its seat");
    assert_eq!(body, data);
}

/// The chunk route's own arms around the relay (`chunk_routes::download_chunk`):
/// an unhinted miss stays a plain `404` and asks nobody; a hint without a
/// bearer is `401`; a stranger's hint resolves the stranger's own same-named
/// folder and never reaches the owner's seat; and a relayed chunk a full
/// folder rested serves the unhinted public read afterwards — the store is
/// uniform (encode on write, decode on read). The first ask the seat ever
/// receives is the causal barrier for every "asked nobody" claim.
#[tokio::test]
async fn the_route_relays_only_a_hinted_miss_of_a_readable_folder() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await);

    let data = b"bytes the owner's seat holds".to_vec();
    let hash = ContentHash::of_raw(&data);
    let unhinted = format!(
        "{}{}",
        h.http_url,
        fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest()))
    );
    let plain = |url: String| async move {
        let resp = reqwest::Client::new().get(&url).send().await.unwrap();
        let status = resp.status();
        (status, resp.bytes().await.unwrap().to_vec())
    };

    let (status, _) = plain(unhinted.clone()).await;
    assert_eq!(status, 404, "an unhinted miss stays a plain 404");
    let (status, _) = plain(chunk_url(&h, &hash)).await;
    assert_eq!(status, 401, "the hinted arm needs the session bearer");
    let (status, _, _) = spawn_get(chunk_url(&h, &hash), h.stranger_token.clone())
        .await
        .unwrap();
    assert_eq!(
        status, 404,
        "a same-named folder of another user never reaches the owner's seat"
    );

    // The owner's folder flipped to full, so the relayed bytes rest.
    assert!(
        h.state
            .db
            .update_folder_for_user(
                FOLDER,
                &h.owner_kp.actor_id().0,
                fauna_nest::db::FolderUpdate {
                    residency: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    );
    let get = spawn_get(chunk_url(&h, &hash), h.owner_token.clone());
    let wanted = next_wanted(&mut seat).await;
    // The causal barrier: this is the FIRST ask the seat ever received, so
    // none of the three reads above asked it anything.
    assert_eq!(wanted.store_key, hex::encode(hash.digest()));
    assert_eq!(
        post_answer(&h, &h.owner_token, wanted.request_id, data.clone()).await,
        204
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data);

    let (status, body) = plain(unhinted).await;
    assert_eq!(status, 200, "the rested copy serves the unhinted read");
    assert_eq!(body, data);
}

#[tokio::test]
async fn a_new_announce_replaces_the_old_and_an_empty_one_withdraws() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await);
    assert!(reachable(&h).await);
    assert!(admitted(&announce(&mut seat, 2, OWNER_DEVICE, vec![]).await).is_empty());
    assert!(
        !reachable(&h).await,
        "the empty announce replaced the earlier one whole"
    );
}

/// A byte-plane token as the home nest's mint hands one out, bound to `actor`.
async fn bulk_token(
    h: &Harness,
    actor: [u8; 32],
    access: BulkByteAccess,
    purpose: BulkByteMintPurpose,
) -> String {
    h.state
        .auth
        .bulk_byte_tokens
        .mint(
            fauna_core::identity::ActorId(actor),
            fauna_nest::bulk_byte_token::folder_attribution(h.folder_id),
            access,
            purpose,
            600,
        )
        .await
        .0
}

fn chunk_url_by_hash(h: &Harness, hash: &ContentHash, folder_hash: &str) -> String {
    format!(
        "{}{}?{}={folder_hash}",
        h.http_url,
        fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest())),
        fauna_nest_http::paths::chunk_store::FOLDER_HASH_HINT_PARAM
    )
}

/// `file-sync.md` § Relay serving → *A member on another nest*, step (5): the
/// store-miss arm admits a cross-nest member under the write token its
/// uploads carry, and under the read-scoped twin a reader is minted — by the
/// name hint and by the hash hint alike — and a metadata-only folder's bytes
/// still never rest.
#[tokio::test]
async fn a_cross_nest_member_reads_by_relay_under_its_write_or_read_token() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await);

    let folder_hash = hex::encode(fauna_core::path_crypto::set_name_hash(FOLDER));
    for (i, (access, purpose, by_hash)) in [
        (
            BulkByteAccess::Write,
            BulkByteMintPurpose::ForeignFolderWrite,
            false,
        ),
        (
            BulkByteAccess::Read,
            BulkByteMintPurpose::ForeignFolderRead,
            false,
        ),
        (
            BulkByteAccess::Read,
            BulkByteMintPurpose::ForeignFolderRead,
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let token = bulk_token(&h, FOREIGN_MEMBER, access, purpose).await;
        let data = format!("bytes only the owner's laptop holds, read {i}").into_bytes();
        let hash = ContentHash::of_raw(&data);
        let url = if by_hash {
            chunk_url_by_hash(&h, &hash, &folder_hash)
        } else {
            chunk_url(&h, &hash)
        };
        let get = spawn_get(url, token);

        let wanted = next_wanted(&mut seat).await;
        assert_eq!(wanted.folder, local_ref(h.folder_id));
        assert_eq!(wanted.store_key, hex::encode(hash.digest()));
        assert_eq!(
            post_answer(&h, &h.owner_token, wanted.request_id, data.clone()).await,
            204
        );
        let (status, body, _) = get.await.unwrap();
        assert_eq!(
            status, 200,
            "a cross-nest member is served under {purpose:?} (by_hash: {by_hash})"
        );
        assert_eq!(body, data);
        assert!(
            !h.store.exists(&hash).await.unwrap(),
            "a metadata-only folder's relayed bytes never rest on the nest"
        );
    }
}

/// The purpose selects the roster, and the roster is read at every request.
/// A federated token names its caller through the cross-nest roster ALONE —
/// never the owner arm, never the same-nest roster — so a token bound to an
/// actor that roster does not hold reads nothing (`404`, the plain miss), a
/// malformed hash hint names no folder, and a member removed after the mint
/// reads nothing for the rest of the token's life. A token of any other
/// purpose does not open the arm at all (`401`, as with no bearer). The first
/// ask the seat ever receives is the causal barrier for every refusal above
/// it.
#[tokio::test]
async fn a_federated_token_reads_only_through_the_cross_nest_roster() {
    let h = start().await;
    let mut seat = open_authed(&h.ws_url, &h.owner_kp, &h.owner_token).await;
    admitted(&announce(&mut seat, 1, OWNER_DEVICE, vec![local_ref(h.folder_id)]).await);
    let channel = fauna_mls::types::ChannelId::from_group_id(&GROUP_ID).0;

    let data = b"bytes a removed member must not be handed".to_vec();
    let hash = ContentHash::of_raw(&data);
    let read = BulkByteAccess::Read;
    let read_purpose = BulkByteMintPurpose::ForeignFolderRead;

    for (actor, who) in [
        ([0xe0u8; 32], "an actor on no roster"),
        (
            h.member_kp.actor_id().0,
            "a same-nest member (that roster is not read for a federated token)",
        ),
        (
            h.owner_kp.actor_id().0,
            "the owner (the owner arm is not read for a federated token)",
        ),
    ] {
        let token = bulk_token(&h, actor, read, read_purpose).await;
        let (status, _, _) = spawn_get(chunk_url(&h, &hash), token).await.unwrap();
        assert_eq!(status, 404, "{who} reads nothing");
    }

    let member_token = bulk_token(&h, FOREIGN_MEMBER, read, read_purpose).await;
    let (status, _, _) = spawn_get(
        chunk_url_by_hash(&h, &hash, "not-hex"),
        member_token.clone(),
    )
    .await
    .unwrap();
    assert_eq!(status, 404, "a malformed hash hint names no folder");

    for purpose in [
        BulkByteMintPurpose::Folder,
        BulkByteMintPurpose::MailBody,
        BulkByteMintPurpose::ForeignConversationWrite,
        BulkByteMintPurpose::NestBackupWrite,
    ] {
        let token = bulk_token(&h, FOREIGN_MEMBER, BulkByteAccess::Write, purpose).await;
        let (status, _, _) = spawn_get(chunk_url(&h, &hash), token).await.unwrap();
        assert_eq!(
            status, 401,
            "a {purpose:?} token does not open the relay arm"
        );
    }

    // Removed after the mint: the token is still live, the row is gone.
    assert!(
        h.state
            .db
            .remove_foreign_channel_member(&channel, &FOREIGN_MEMBER)
            .await
            .unwrap()
    );
    let (status, _, _) = spawn_get(chunk_url(&h, &hash), member_token.clone())
        .await
        .unwrap();
    assert_eq!(
        status, 404,
        "a removal ends the member's reads inside the token's life"
    );

    // Back on the roster, the same token reads — and this is the FIRST ask the
    // seat ever received, so none of the reads above asked it anything.
    h.state
        .db
        .register_foreign_channel_member(
            &channel,
            &FOREIGN_MEMBER,
            &FOREIGN_HOME_NEST,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    let get = spawn_get(chunk_url(&h, &hash), member_token);
    let wanted = next_wanted(&mut seat).await;
    assert_eq!(wanted.store_key, hex::encode(hash.digest()));
    assert_eq!(
        post_answer(&h, &h.owner_token, wanted.request_id, data.clone()).await,
        204
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data);
}

/// A read token is a read token: the bulk write routes refuse it `403`, and
/// the relay's answer route — a bulk write route — with them.
#[tokio::test]
async fn a_read_token_opens_no_write_route() {
    let h = start().await;
    let token = bulk_token(
        &h,
        FOREIGN_MEMBER,
        BulkByteAccess::Read,
        BulkByteMintPurpose::ForeignFolderRead,
    )
    .await;
    let data = b"bytes a reader may not rest".to_vec();
    let hash = ContentHash::of_raw(&data);
    let status = reqwest::Client::new()
        .post(format!(
            "{}{}",
            h.http_url,
            fauna_nest_http::paths::chunk_store::CHUNKS_UPLOAD
        ))
        .bearer_auth(&token)
        .header("X-Content-Hash", hex::encode(hash.digest()))
        .body(data.clone())
        .send()
        .await
        .expect("POST completes")
        .status();
    assert_eq!(status, 403, "a read token on POST /api/v1/chunks");
    assert!(!h.store.exists(&hash).await.unwrap());
    assert_eq!(
        post_answer(&h, &token, 1, data).await,
        403,
        "a read token on the relay's answer route"
    );
}
