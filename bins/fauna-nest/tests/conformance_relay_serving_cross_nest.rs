//! **Relay serving across nests, the nest half** (tier_3) — both roles one nest
//! binary plays for a member whose account lives on another nest
//! (`docs/goal/behavior/file-sync.md` § Relay serving → *A member on another
//! nest*, steps (2) Announce, (3) Ask, (4) Answer and (6) Verdict; the kinds and
//! gates are `docs/goal/architecture/federation.md` § Cross-nest shared folders
//! + channel append → *Relay serving across nests*).
//!
//! Two real in-process nests over loopback: **H**, the folder's home, and
//! **M**, the member's own nest. A test client on M plays the member's seat,
//! exactly as `sync_relay_serving.rs`'s does on one nest; the reader is the
//! owner's plain hinted `GET` on H. The flow, end to end: the seat announces
//! `foreign:<channel>` + H's URL on M → M forwards
//! `fauna.federation.folder.serve.announce` → H gates on the write gate and
//! leases a foreign seat → the reader's `GET` misses H's store → H's walk asks
//! the seat with `fauna.federation.folder.chunk.wanted` → M pushes
//! `fauna.sync.chunk.wanted` on the announcing connection → the seat `POST`s
//! the bytes to H's answer route under its write token → the reader is served,
//! and H's store holds nothing.
//!
//! **The share is seeded beneath the handlers**: the nest refuses a
//! metadata-only folder with a member on another nest at both doors until the
//! cross-nest witness lands (`file-sync.md` § Relay serving, *Until that leg is
//! built, the pair is refused*), so the folder, its claimed channel, the
//! foreign roster row and the writer grant are written straight into H's
//! database — the state the share would have left.
//!
//! Every "the seat was asked" claim is anchored on the seat's own socket — the
//! push it receives. A lease's lapse is time, and is pinned beside the table
//! (`chunk_relay.rs`'s unit tests, with the reachability truth table's
//! foreign-seat rows); here a seat ends by its connection closing, which is
//! the member's nest's *no longer serving*.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use fauna_core::data::ContentHash;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::db::channels::RebindPower;
use fauna_nest::federation_channel::dial;
use fauna_nest::federation_handlers::{
    FedFolderChunkWantedReply, FedFolderChunkWantedRequest, KIND_FED_FOLDER_CHUNK_WANTED,
};
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::push_events::{KIND_SYNC_CHUNK_WANTED, SyncChunkWantedPayload};
use fauna_protocol::sync::{
    KIND_SYNC_SERVE_ANNOUNCE, SyncServeAnnounceReply, SyncServeAnnounceRequest,
    SyncServeForeignEntry,
};
use fauna_protocol::wrapped_blob::{BulkByteAccess, BulkByteMintPurpose};
use fauna_protocol::{
    Frame, PushEvent, Reply, Request, Value, decode_frame, decode_strict, encode_canonical,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

mod common;
use common::{Ws, open_authed};

const SEAT_DEVICE: [u8; 32] = [0xb6; 32];
const FOLDER: &str = "vault";
const GROUP_ID: [u8; 32] = [0x2c; 32];
const PROMPT: Duration = Duration::from_secs(10);

struct Nest {
    http_url: String,
    ws_url: String,
    state: Arc<AppState>,
    tokens: Arc<TokenStore>,
}

/// A real in-process nest on a loopback socket with its own identity, the
/// sync kinds on its bearer router, the discovery kinds the pool resolves a
/// peer's `nest_id` with, every federation kind, and a real blob store the
/// relay resolver shares (`build_app_state`'s wiring).
async fn start_nest() -> Nest {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();

    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir);
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());
    let resolver = Arc::new(fauna_nest::chunk_relay::ChunkResolver::new(
        Some(backup_svc.local_blob_store()),
        None,
        false,
    ));
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            b.build()
        }),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
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
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Nest {
        http_url: format!("http://{addr}"),
        ws_url: format!("ws://{addr}"),
        state,
        tokens,
    }
}

struct Harness {
    h: Nest,
    m: Nest,
    owner_token: String,
    member_kp: ActorKeypair,
    member_token: String,
    /// H's `folders.id` of the owner's metadata-only folder.
    folder_id: i64,
    channel_id: [u8; 32],
}

impl Harness {
    fn member(&self) -> [u8; 32] {
        self.member_kp.actor_id().0
    }

    fn foreign_ref(&self) -> String {
        format!("foreign:{}", hex::encode(self.channel_id))
    }

    fn entry(&self) -> SyncServeForeignEntry {
        SyncServeForeignEntry {
            folder: self.foreign_ref(),
            nest_url: self.h.http_url.clone(),
            extra: Default::default(),
        }
    }

    async fn set_member_access(&self, access: &str) {
        self.h
            .state
            .db
            .set_folder_member_access(&self.channel_id, &self.member(), access, Some(1_000_000))
            .await
            .unwrap();
    }

    fn live_seats(&self) -> usize {
        self.h
            .state
            .sync
            .chunk_resolver
            .foreign_seats()
            .live_for_folder(self.folder_id)
            .len()
    }

    async fn reachable(&self) -> bool {
        let row = self
            .h
            .state
            .db
            .get_folder_by_id(self.folder_id)
            .await
            .unwrap()
            .unwrap();
        fauna_nest::chunk_relay::folder_content_reachable(
            &self.h.state.ws,
            self.h.state.sync.chunk_resolver.foreign_seats(),
            &row,
        )
    }

    /// The member's write token on H — what its uploads already carry
    /// (`fauna.federation.folder.write_token.mint`'s mint).
    async fn member_write_token(&self) -> String {
        self.h
            .state
            .auth
            .bulk_byte_tokens
            .mint(
                self.member_kp.actor_id(),
                fauna_nest::bulk_byte_token::folder_attribution(self.folder_id),
                BulkByteAccess::Write,
                BulkByteMintPurpose::ForeignFolderWrite,
                600,
            )
            .await
            .0
    }

    fn chunk_url(&self, hash: &ContentHash) -> String {
        format!(
            "{}{}?{}={FOLDER}",
            self.h.http_url,
            fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest())),
            fauna_nest_http::paths::chunk_store::FOLDER_HINT_PARAM
        )
    }

    async fn answer(&self, token: &str, request_id: u64, body: Option<Vec<u8>>) -> u16 {
        let url = format!(
            "{}{}",
            self.h.http_url,
            fauna_nest_http::paths::chunk_store::chunk_relay_answer(request_id)
        );
        let client = reqwest::Client::new();
        let req = match body {
            Some(b) => client.post(url).body(b),
            None => client.delete(url),
        };
        req.bearer_auth(token)
            .send()
            .await
            .expect("the answer completes")
            .status()
            .as_u16()
    }
}

/// H holds the owner's metadata-only, group-bound folder with its channel
/// claimed, and a roster row binding the member to M at M's URL — the state
/// the cross-nest share leaves, written beneath the refusing doors. The member
/// has an account and a sync device on M, none on H. The writer grant is the
/// test's to give.
async fn start() -> Harness {
    let h = start_nest().await;
    let m = start_nest().await;

    let owner_kp = ActorKeypair::generate();
    let owner = owner_kp.actor_id().0;
    h.state
        .db
        .create_user(&owner, "free", "alice")
        .await
        .unwrap();
    let folder_id = h.state.db.create_folder(FOLDER, &owner).await.unwrap();
    assert!(
        h.state
            .db
            .update_folder_for_user(
                FOLDER,
                &owner,
                fauna_nest::db::FolderUpdate {
                    residency: Some(Some("metadata_only")),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    );
    assert!(
        h.state
            .db
            .set_folder_mls_group(FOLDER, &owner, Some(&GROUP_ID))
            .await
            .unwrap()
    );
    let channel_id = fauna_mls::types::ChannelId::from_group_id(&GROUP_ID).0;
    assert!(matches!(
        h.state
            .db
            .claim_folder_channel(&owner, &channel_id)
            .await
            .unwrap(),
        fauna_nest::db::channels::ChannelClaimOutcome::Allowed
    ));
    let owner_token = h.tokens.insert(owner_kp.actor_id(), 3600).await;

    let member_kp = ActorKeypair::generate();
    let member = member_kp.actor_id().0;
    m.state
        .db
        .create_user(&member, "free", "bob")
        .await
        .unwrap();
    m.state
        .db
        .register_sync_device(&member, &SEAT_DEVICE, "desktop", None, "write")
        .await
        .unwrap();
    let member_token = m.tokens.insert(member_kp.actor_id(), 3600).await;
    h.state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &member,
            &m.state.nest_identity.public_key_bytes(),
            Some(&m.http_url),
            RebindPower::Standing,
        )
        .await
        .unwrap();

    Harness {
        h,
        m,
        owner_token,
        member_kp,
        member_token,
        folder_id,
        channel_id,
    }
}

/// Send one `fauna.sync.serve.announce` on `ws` and return what it admitted.
async fn announce(ws: &mut Ws, corr: u64, foreign: Vec<SyncServeForeignEntry>) -> Vec<String> {
    let req = SyncServeAnnounceRequest {
        device_id: hex::encode(SEAT_DEVICE),
        foreign,
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
    let reply = next_reply(ws).await;
    assert!(reply.ok, "the announce must succeed: {:?}", reply.payload);
    let r: SyncServeAnnounceReply =
        decode_strict(&encode_canonical(&reply.payload).unwrap()).unwrap();
    r.admitted
}

async fn next_reply(ws: &mut Ws) -> Reply {
    loop {
        if let Frame::Reply(r) = next_frame(ws).await {
            return r;
        }
    }
}

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

/// Wait, bounded, until `cond` holds — for the one fact here that settles in
/// the background: the member's nest's *no longer serving* reaching H.
async fn eventually(what: &str, mut cond: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + PROMPT;
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The flow sentence, end to end, on a metadata-only folder: a writer's seat
/// announces through its own nest, the home nest leases it and counts it
/// reachable, asks it through the member's nest, takes its answer under its
/// write token, serves the reader — and rests nothing.
#[tokio::test]
async fn a_cross_nest_writers_seat_serves_a_metadata_only_folder_through_its_own_nest() {
    let h = start().await;
    h.set_member_access("writer").await;
    assert!(!h.reachable().await, "no seat yet: no holder");

    let mut seat = open_authed(&h.m.ws_url, &h.member_kp, &h.member_token).await;
    assert_eq!(
        announce(&mut seat, 1, vec![h.entry()]).await,
        vec![h.foreign_ref()],
        "the home nest admitted the forwarded announce"
    );
    assert_eq!(h.live_seats(), 1, "the home nest leased one foreign seat");
    assert!(h.reachable().await, "a live foreign seat holds the folder");

    let data = b"bytes only the member's desktop holds".to_vec();
    let hash = ContentHash::of_raw(&data);
    let get = spawn_get(h.chunk_url(&hash), h.owner_token.clone());

    let wanted = next_wanted(&mut seat).await;
    assert_eq!(
        wanted.folder,
        h.foreign_ref(),
        "the ask names the folder by its foreign ref"
    );
    assert_eq!(wanted.store_key, hex::encode(hash.digest()));
    let token = h.member_write_token().await;
    assert_eq!(
        h.answer(&token, wanted.request_id, Some(data.clone()))
            .await,
        204,
        "the seat's answer on the home nest's byte plane is taken"
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data);
    let store =
        h.h.state
            .backup_service
            .as_ref()
            .unwrap()
            .local_blob_store();
    assert!(
        !store.exists(&hash).await.unwrap(),
        "a metadata-only folder's relayed bytes never rest on the home nest"
    );
    assert!(
        !h.m.state
            .backup_service
            .as_ref()
            .unwrap()
            .local_blob_store()
            .exists(&hash)
            .await
            .unwrap(),
        "the member's nest carries a store key and never a byte"
    );

    // Its connection closing is *no longer serving*: the lease ends at once,
    // and the folder reads unreachable.
    drop(seat);
    eventually("the closed seat's lease is withdrawn", async || {
        h.live_seats() == 0
    })
    .await;
    assert!(!h.reachable().await);
}

/// The home nest gates: a member without the `writer` grant is no seat — the
/// forward is refused and the entry is left out of `admitted`, never an
/// error — and a seat whose writer grant is withdrawn is dropped at the next
/// ask, asked nothing.
#[tokio::test]
async fn only_a_writer_is_leased_and_a_demoted_seat_is_dropped_at_the_next_ask() {
    let h = start().await;
    h.set_member_access("reader").await;
    let mut seat = open_authed(&h.m.ws_url, &h.member_kp, &h.member_token).await;
    assert!(
        announce(&mut seat, 1, vec![h.entry()]).await.is_empty(),
        "a reader's seat is refused by omission"
    );
    assert_eq!(h.live_seats(), 0);

    h.set_member_access("writer").await;
    assert_eq!(
        announce(&mut seat, 2, vec![h.entry()]).await,
        vec![h.foreign_ref()]
    );
    assert_eq!(h.live_seats(), 1);

    h.set_member_access("reader").await;
    let data = b"a demoted member's bytes".to_vec();
    let hash = ContentHash::of_raw(&data);
    let (status, _, elapsed) = spawn_get(h.chunk_url(&hash), h.owner_token.clone())
        .await
        .unwrap();
    assert_eq!(status, 404, "a demoted seat is no candidate");
    assert!(elapsed < PROMPT, "and nothing waited on it");
    assert_eq!(h.live_seats(), 0, "the walk dropped the demoted seat");
}

/// A foreign seat whose member's nest has no connection to push to answers
/// `pushed: false`, and the walk passes it over at once — the reader is not
/// made to wait out the 30 s fetch deadline.
#[tokio::test]
async fn a_seat_with_no_connection_refills_the_window_at_once() {
    let h = start().await;
    h.set_member_access("writer").await;
    // Seeded beneath the announce: a lease for a device of the member that no
    // connection on M announced.
    h.h.state
        .sync
        .chunk_resolver
        .foreign_seats()
        .lease(fauna_nest::chunk_relay::ForeignSeat {
            folder_id: h.folder_id,
            channel_id: h.channel_id,
            member: h.member(),
            device: [0xee; 32],
            origin_nest_id: h.m.state.nest_identity.public_key_bytes(),
            nest_url: h.m.http_url.clone(),
        })
        .unwrap();
    let hash = ContentHash::of_raw(b"held by no connected seat");
    let (status, _, elapsed) = spawn_get(h.chunk_url(&hash), h.owner_token.clone())
        .await
        .unwrap();
    assert_eq!(status, 404);
    assert!(
        elapsed < Duration::from_secs(10),
        "an unpushed ask settles at once, not at the deadline ({elapsed:?})"
    );
}

/// The member's nest pushes an ask only for the nest it forwarded that
/// device's announce to: a third nest asking about the same seat pushes
/// nothing. And the home nest takes an answer only from the actor it asked:
/// the owner's own session answering the member's ask is refused and touches
/// nothing, and the member's answer still lands. The first push the seat
/// receives is the home nest's — the barrier for the third nest's refusal.
#[tokio::test]
async fn an_ask_from_another_nest_pushes_nothing_and_only_the_asked_actor_answers() {
    let h = start().await;
    h.set_member_access("writer").await;
    let mut seat = open_authed(&h.m.ws_url, &h.member_kp, &h.member_token).await;
    assert_eq!(
        announce(&mut seat, 1, vec![h.entry()]).await,
        vec![h.foreign_ref()]
    );

    let x = start_nest().await;
    let conn = dial(
        &x.state,
        &h.m.http_url,
        &hex::encode(h.m.state.nest_identity.public_key_bytes()),
    )
    .await
    .unwrap();
    let stolen = ContentHash::of_raw(b"what a third nest wants");
    let req = FedFolderChunkWantedRequest {
        requesting_actor_id: hex::encode(h.member()),
        channel_id: hex::encode(h.channel_id),
        device_id: hex::encode(SEAT_DEVICE),
        request_id: 4242,
        store_key: hex::encode(stolen.digest()),
    };
    let reply = conn
        .dispatcher
        .request_raw(
            KIND_FED_FOLDER_CHUNK_WANTED,
            [0x41; 16],
            decode_strict::<Value>(&encode_canonical(&req).unwrap()).unwrap(),
            None,
        )
        .await
        .unwrap()
        .await_reply()
        .await
        .expect("the member's nest answers the kind");
    let reply: FedFolderChunkWantedReply =
        decode_strict(&encode_canonical(&reply).unwrap()).unwrap();
    assert!(
        !reply.pushed,
        "a nest that is not the seat's home pushes nothing"
    );

    let data = b"the member's bytes".to_vec();
    let hash = ContentHash::of_raw(&data);
    let get = spawn_get(h.chunk_url(&hash), h.owner_token.clone());
    let wanted = next_wanted(&mut seat).await;
    assert_eq!(
        wanted.store_key,
        hex::encode(hash.digest()),
        "the first ask the seat receives is the home nest's"
    );
    assert_eq!(
        h.answer(&h.owner_token, wanted.request_id, Some(b"forged".to_vec()))
            .await,
        404,
        "an answer under another actor's token is refused"
    );
    let token = h.member_write_token().await;
    assert_eq!(
        h.answer(&token, wanted.request_id, Some(data.clone()))
            .await,
        204,
        "the asked actor's answer still lands"
    );
    let (status, body, _) = get.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data);
}

/// An announce that drops a foreign folder is that folder's *no longer
/// serving*: the home nest's lease ends without waiting for it to lapse.
#[tokio::test]
async fn a_replacing_announce_withdraws_the_dropped_foreign_folder() {
    let h = start().await;
    h.set_member_access("writer").await;
    let mut seat = open_authed(&h.m.ws_url, &h.member_kp, &h.member_token).await;
    assert_eq!(
        announce(&mut seat, 1, vec![h.entry()]).await,
        vec![h.foreign_ref()]
    );
    assert_eq!(h.live_seats(), 1);
    assert!(announce(&mut seat, 2, vec![]).await.is_empty());
    eventually("the dropped folder's lease is withdrawn", async || {
        h.live_seats() == 0
    })
    .await;
    assert!(!h.reachable().await);
}
