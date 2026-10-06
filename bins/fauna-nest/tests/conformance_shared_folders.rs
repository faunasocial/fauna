//! Slice 2 S2-P4 — the nest authz round-trip for cross-user **shared folders**.
//!
//! Proves the security-critical S2-P3 read gate end-to-end through the **real**
//! share + Welcome flow (no hand-registered roster): owner A binds an owner-only
//! set to a client-created MLS group via `fauna.folders.share`, then delivers
//! member B's Welcome via `fauna.conversations.welcome.deliver` (which registers B
//! on the derived `ChannelId::from_group_id(group_id)` roster). After that, B's
//! gated reads (`snapshot.get` / `snapshot.list` / `sync.files`) succeed, a
//! non-member is denied, and B — a *reader* — cannot write (`snapshot.delete`).
//!
//! The nest is **MLS-agnostic**: it holds only the `actor_channels` roster and
//! the opaque Welcome blob, never group/epoch state — so this round-trip needs no
//! real MLS crypto. The complementary **chunk seal/decrypt** half (a bound engine
//! seals under the group-derived `chunk_root` and only a group member recovers the
//! plaintext) is proven at the engine level by
//! `fauna-sync-engine::download_file_bytes_test::bound_engine_seals_chunks_under_group_chunk_root`.
//! Together they cover Slice-2 "member B reads + decrypts A's chunks"; the
//! removed-member rotate-on-removal re-key (Slice 3) is proven by the second test
//! in this file, `removed_member_reads_pre_removal_but_fails_closed_post_removal_real_nest`
//! (piece 7 — see its module-level block comment below).
//!
//! Authority: `docs/goal/architecture/key-material-hierarchy.md` § *Audience: an
//! MLS group at a specific epoch* + the shared-folders design (tracked
//! internally, § Q5).

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::types::ChannelId;
use fauna_nest::{
    conversations_handlers, db::CacheDb, filesync_handlers, folder_handlers, routes::AppState,
    rpc_router::RpcRouter, sync_handlers,
};
use fauna_protocol::{
    RpcError,
    conversations::{WelcomeDeliverRequest, WelcomeKind},
    decode_strict as decode, encode_canonical,
    filesync::{
        SnapshotDeleteRequest, SnapshotGetReply, SnapshotGetRequest, SnapshotListReply,
        SnapshotListRequest,
    },
    folders::{
        FolderShareReply, FolderShareRequest, LeaseAcquireReply, LeaseAcquireRequest,
        LeaseReleaseReply, LeaseReleaseRequest,
    },
    sync::{SyncFilesReply, SyncFilesRequest},
};

// ── piece 7 (Slice 3) imports — the real rotate-on-removal round-trip ─────────
use std::sync::Mutex;

use fauna_client_folders::FoldersClient;
use fauna_client_folders::orchestration::{CreatedGroup, FolderGroupCrypto, FoldersAuthor};
use fauna_client_folders::{FolderKeyReader, FolderKeyStore, MemoryFolderKeyStore};
use fauna_client_sync::SyncClient as CtlSyncClient;
use fauna_conversations::backend::{
    ConvRpcError, ConversationsRpc, ResolvedHandle, WelcomeChannelKind,
};
use fauna_conversations::backends::fauna_mls::{FaunaMlsBackend, poll_inbound_folder};
use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderContentKeys;
use fauna_core::format::{ConflictPolicy, FormatRegistry};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::engine::MlsEngine;
use fauna_nest::backup::service::BackupService;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::{RpcErrorClass, RpcRequester, folders::ContentKeyGetRequest};
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::nest_client::SyncClient as HttpSyncClient;
use fauna_sync_engine::test_support::MockNest;
use fauna_sync_engine::transfer::TransferPool;
use wiremock::MockServer;

// ── gated-removal twin imports — the REAL gated plane over the real nest ──────
use fauna_client_folders::custody;
use fauna_client_mls_sync::{
    BackendCatchUp, BackendChannelSend, FaunaCommitGate, MlsReplicaTransport, MlsStateSync,
    MlsTransportError, PATH_PROVIDER, PutOutcome, ReplicaBase, rpc_transport_get,
    rpc_transport_put,
};
use fauna_conversations::ConversationsManager;
use fauna_core::data::FolderPendingRemoval;
use fauna_core::folder_keys::ContentKeyGeneration;
use fauna_mls::types::ChannelEnvelope;

/// Register every handler cluster the round-trip exercises against an in-memory
/// `CacheDb` + test `AppState` (the conformance-harness pattern).
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    filesync_handlers::register_filesync_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    conversations_handlers::register_conversations_handlers(&mut b);
    (b.build(), state)
}

fn enc<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

#[tokio::test]
async fn shared_folder_member_reads_via_real_share_and_welcome_flow() {
    let (router, state) = router_and_state().await;
    let owner = [0xa1u8; 32];
    let member = [0xb2u8; 32];
    let outsider = [0xc3u8; 32];

    // Owner A owns "shared-docs" with one snapshot (the content to be shared).
    let fs_id = state.db.create_folder("shared-docs", &owner).await.unwrap();
    let snap_id = state.db.insert_snapshot_at(fs_id, 1).await.unwrap();

    // A's client created an MLS group with B; a 24-byte raw group id (not
    // fixed-32) also exercises the variable-length binding path.
    let group_id = vec![0x5au8; 24];
    let channel_id = ChannelId::from_group_id(&group_id).0;

    // 1) A binds the set to the group via the REAL `fauna.folders.share` handler
    //    (owner-scoped; nest derives + echoes the ChannelId, registers A).
    let share_reply: FolderShareReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.folders.share",
            enc(&fauna_protocol::folders::addressed(FolderShareRequest {
                name: "shared-docs".into(),
                group_id: hex::encode(&group_id),
                ..Default::default()
            })),
        )
        .await
        .expect("share ok"),
    )
    .unwrap();
    assert!(share_reply.ok);
    assert_eq!(
        share_reply.channel_id,
        hex::encode(channel_id),
        "the nest derives + echoes the ChannelId (client never asserts it)"
    );

    // Before the Welcome, B is on no channel → the gate denies the read.
    let get_req = enc(&SnapshotGetRequest {
        snapshot_id: snap_id,
        extra: Default::default(),
    });
    let pre = dispatch(
        &router,
        state.clone(),
        member,
        "fauna.filesync.snapshot.get",
        get_req.clone(),
    )
    .await;
    assert!(pre.is_err(), "B cannot read before joining the group");

    // 2) A delivers B's Welcome via the REAL handler → registers B on the
    //    derived ChannelId roster (the nest holds only the opaque blob + roster).
    // Mode gate (direct-messages.md § Reach policy): these arrangement
    // welcomes ride the Group kind, so open the recipient's inbox.
    state.db.set_inbox_mode(&member, "open").await.unwrap();
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.conversations.welcome.deliver",
        enc(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(member),
            channel_id: hex::encode(channel_id),
            welcome_bytes: vec![0x01, 0x02, 0x03],
            kind: WelcomeKind::Group {
                group_id: hex::encode(&group_id),
            },
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("welcome deliver ok");
    assert!(
        state
            .db
            .is_actor_in_channel(&member, &channel_id)
            .await
            .unwrap(),
        "B joined the roster via the real Welcome flow"
    );

    // 3) B now reads the shared set's snapshot — the S2-P3 admission via
    //    is_actor_in_channel on the derived ChannelId.
    let got: SnapshotGetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            member,
            "fauna.filesync.snapshot.get",
            get_req.clone(),
        )
        .await
        .expect("group member get succeeds"),
    )
    .unwrap();
    assert_eq!(got.id, snap_id);
    assert_eq!(got.folder, "shared-docs");

    // ...and discovers the set via `snapshot.list` + `sync.files` (the
    // change-log / manifest discovery surfaces, spec § Q5).
    let listed: SnapshotListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            member,
            "fauna.filesync.snapshot.list",
            enc(&SnapshotListRequest {
                message_kind: None,
                folder: Some("shared-docs".into()),
                limit: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("member list succeeds"),
    )
    .unwrap();
    assert_eq!(
        listed.rows.len(),
        1,
        "the member lists the shared set's snapshots"
    );
    let files: SyncFilesReply = decode(
        &dispatch(
            &router,
            state.clone(),
            member,
            "fauna.sync.files",
            enc(&SyncFilesRequest {
                folder: "shared-docs".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("member sync.files succeeds"),
    )
    .unwrap();
    assert!(
        files.files.is_empty(),
        "no files uploaded — the point is the gate admitted the discovery read"
    );

    // 4) A non-member outsider (never welcomed) is denied.
    let outsider_err = dispatch(
        &router,
        state.clone(),
        outsider,
        "fauna.filesync.snapshot.get",
        get_req.clone(),
    )
    .await
    .expect_err("non-member outsider must be denied");
    assert_eq!(
        outsider_err.code,
        "fauna.filesync.snapshot.permission_denied"
    );

    // 5) B is a READER — a write (`snapshot.delete`) stays owner-only; the gate
    //    fires before any state change.
    let del_err = dispatch(
        &router,
        state.clone(),
        member,
        "fauna.filesync.snapshot.delete",
        enc(&SnapshotDeleteRequest {
            snapshot_id: snap_id,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("member delete must be denied (read-only)");
    assert_eq!(del_err.code, "fauna.filesync.snapshot.permission_denied");
}

// ─────────────────────────────────────────────────────────────────────────────
// Slice 3 — PIECE 7: the rotate-on-removal **Done proof** on the real nest stack.
//
// Lifts `fauna-sync-engine::download_file_bytes_test::
// bound_engine_rotation_gen1_readable_gen2_fail_closed` (a pure-engine proof with
// hand-built content keys) onto the **real** nest: the content keys now come from
// the live owner-side `FoldersAuthor` rotate-on-removal orchestration over a real
// `MlsEngine` group, the genesis + rotated envelopes are sealed/stored/fetched
// through the real `fauna.folders.content_key.{put,get}` handlers + custody
// (the `fauna.state.folder-keys` plane entries), the version rides the real `fauna.sync.changes.
// {record,list}`, and the roster eviction + rebind rejection run through the real
// `members.evict` + first-binder claim. The chunks really seal/upload/download
// through the engine's content-key pipeline (`chunk_seal::seal_chunk_body` →
// version-stamped `content_seal_root` → `content_open_root` selection).
//
// What it proves end-to-end — exactly `mls-group-key-material.md` § M2 *Rotate-on-
// removal* + the OBS-1 "removed member can't read post-removal content … rests on
// the rotated content key, never the nest roster":
//   1. owner A binds a shared set to a real MLS group (A + member B) and uploads
//      generation-1 content; B (a member, on the roster, holding gen-1 keys it
//      opened from the real envelope) reads it — and learns the manifest + version
//      through the real `fauna.sync.changes.list` (the metadata path);
//   2. A removes B → the `FoldersAuthor` rotates to a fresh generation-2 content
//      key, re-seals the envelope under the post-removal epoch, evicts B from the
//      roster, and A uploads generation-2 content under the new key;
//   3. B (now evicted, holding only gen-1) is **denied** the metadata path
//      (`changes.list` → error) AND **fails closed** at the crypto layer on gen-2
//      content (`key_for(2)` is `None`), while still reading gen-1 content
//      (history-on-join) — the two-layer guarantee;
//   4. B cannot `share`-rebind itself back onto the roster (`already_claimed`).
//
// Architecture: the control plane (folder + sync + config + conversations WS-RPC
// kinds) is driven through an in-process `RouterRequester` loopback over the real
// handlers + `CacheDb` (the `conformance_sync_client.rs` seam). The **chunk byte
// store** is a `wiremock` content-addressed blob store (the `download_file_bytes_
// test` pattern) — the sanctioned tier_3 split (`conformance_sync_client.rs`: "the
// byte routes stay HTTP and are out of scope"). The fail-closed property is a
// *client-side* crypto fact (`content_open_root` can't select `key_for(2)`), so it
// is identical whether the opaque ciphertext bytes sit in wiremock or a real
// `DiskBlobStore`; the wiremock stands in only for the opaque store.
//
// Note — the real `/api/v1/chunks` route DOES accept a bound set's content-key
// chunks; this test just doesn't need it. FS-BIND-1/2 (piece 6, ciphertext-hash
// store keying — 2026-06-30) keys a content-key chunk by its **ciphertext** hash,
// so the engine uploads the ciphertext with `X-Content-Hash = blake3(ciphertext)`,
// which `chunk_routes::resolve_verified_chunk_hash` accepts on its raw-path branch
// (`blake3(body) == header`) with **no route change** — the F9 anti-poisoning
// defense is untouched. (The *superseded* pairing — a ciphertext body claiming the
// *plaintext* hash — is what the route rejects, and is exactly why the keying moved
// to the ciphertext hash. Do NOT resurrect the abandoned "let the nest trust
// `X-Content-Hash` for a claimed channel" route change; FS-BIND-1/2 replaced it.)
// The real-route round-trip is proven end-to-end in
// `conformance_content_key_chunk_route.rs`; this test keeps the wiremock store
// because its subject is the *client-side* rotate-on-removal fail-closed crypto,
// orthogonal to which store holds the opaque ciphertext.
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Arc as StdArc; // alias to keep the new helpers visually distinct

/// Owner A + member B identities (deterministic, so actor ids are stable across
/// the loopback actor arg, the MLS identity, and the minted HTTP bearer).
const A_SECRET: [u8; 32] = [0xA1; 32];
const B_SECRET: [u8; 32] = [0xB2; 32];
/// A's write-capable sync device (the chunk-plane device + the `changes.record`
/// device — one and the same).
const DEVICE_A: [u8; 32] = [0x0A; 32];

fn actor_of(secret: [u8; 32]) -> [u8; 32] {
    ActorKeypair::from_secret(secret).actor_id().0
}

/// `secret`'s control-plane `SyncClient`, signing every change record it sends
/// directly under the actor's own key and the fixture set nonce — the record a
/// writer engine sends (every record kind refuses an unsigned one
/// `signature_required`). A member signs under the OWNER's set nonce.
fn signing_sync(
    router: &StdArc<RpcRouter>,
    state: &StdArc<AppState>,
    secret: [u8; 32],
) -> CtlSyncClient<RouterRequester> {
    CtlSyncClient::new(requester(router, state, actor_of(secret))).with_record_signing(
        fauna_client_sync::RecordSigning {
            signer: common::direct_signer(&ActorKeypair::from_secret(secret)),
            set_nonce: fauna_client_sync::SetNonceSource::Fixed(common::SET_NONCE),
        },
    )
}

/// The owner's "shared" set options: the create carries the fixture set nonce
/// every signed record binds to.
fn shared_set_options() -> fauna_nest::db::FolderOptions {
    fauna_nest::db::FolderOptions {
        set_nonce: Some(common::SET_NONCE.to_vec()),
        ..Default::default()
    }
}

/// A loopback [`RpcRequester`] error: either a real nest rejection (carrying the
/// wire [`RpcError`], so [`RpcErrorClass`] exposes its code to the shared client
/// surfaces' retry loops + the `already_claimed` assertion) or a client-side codec fault.
#[derive(Debug)]
enum LoopbackError {
    Rejected(RpcError),
    Codec(String),
}
impl std::fmt::Display for LoopbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopbackError::Rejected(e) => write!(f, "rejected: {e:?}"),
            LoopbackError::Codec(s) => write!(f, "codec: {s}"),
        }
    }
}
impl RpcErrorClass for LoopbackError {
    fn is_rejection(&self) -> bool {
        matches!(self, LoopbackError::Rejected(_))
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            LoopbackError::Rejected(e) => Some(e),
            LoopbackError::Codec(_) => None,
        }
    }
}

/// The control-plane `RpcRequester`: the WebSocket transport replaced by a direct
/// dispatch into the registered handler for a fixed connection `actor` (the
/// `conformance_sync_client.rs` seam, with an [`RpcErrorClass`] error so the
/// shared client surfaces — `FoldersAuthor` and friends — compose over it).
struct RouterRequester {
    router: StdArc<RpcRouter>,
    state: StdArc<AppState>,
    actor: [u8; 32],
}
impl RpcRequester for RouterRequester {
    type Error = LoopbackError;
    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        common::seed_dispatch_actor(&self.state.db, &self.actor).await;
        let bytes = Bytes::from(
            encode_canonical(&payload)
                .map_err(|e| LoopbackError::Codec(e.to_string()))?
                .to_vec(),
        );
        let meta = self
            .router
            .kind_meta(kind)
            .ok_or_else(|| LoopbackError::Codec(format!("kind not registered: {kind}")))?;
        let reply = (meta.handler)(self.state.clone(), self.actor, bytes)
            .await
            .map_err(LoopbackError::Rejected)?;
        decode(&reply).map_err(|e| LoopbackError::Codec(e.to_string()))
    }
}

fn requester(
    router: &StdArc<RpcRouter>,
    state: &StdArc<AppState>,
    actor: [u8; 32],
) -> RouterRequester {
    RouterRequester {
        router: router.clone(),
        state: state.clone(),
        actor,
    }
}

/// Stand up the control plane: the WS-RPC kinds registered on a shared `RpcRouter`
/// over an in-memory `CacheDb` + test `AppState`, dispatched in-process via
/// [`RouterRequester`]. No HTTP server (chunks ride the wiremock store). A
/// `BackupService` over a tempdir backs the blob store.
fn control_plane() -> (StdArc<RpcRouter>, StdArc<AppState>) {
    let db = StdArc::new(CacheDb::open_in_memory().unwrap());
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never deleted under test
    let backup_svc =
        StdArc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());
    let state = StdArc::new(AppState {
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });
    let router = StdArc::new({
        let mut b = RpcRouter::builder();
        sync_handlers::register_sync_handlers(&mut b);
        folder_handlers::register_folders_handlers(&mut b);
        conversations_handlers::register_conversations_handlers(&mut b);
        // The sealed MLS replica plane (`fauna.mls.{get,put}`) — the gated-removal
        // twin's `MlsStateSync` CAS-puts the provider replica through these.
        fauna_nest::mls_replica_handlers::register_mls_replica_handlers(&mut b);
        // The durable inbox (`fauna.inbox.{fetch,ack}`) — where `welcome.deliver`
        // stages a recipient's knock. The decline pin peeks it for the `inbox_id`
        // and the shared decline recipe acks through it.
        fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
        b.build()
    });
    (router, state)
}

/// Build a **bound** shared-set [`SyncEngine`]: an HTTP `SyncClient` for the chunk
/// plane (pointed at the wiremock store; the bearer is a no-op there) + the M2
/// `content_keys` that drive the seal (no `backup_key` — a bound set carries none,
/// Q4). The control-plane `NestClient` is never connected (the test records changes
/// over the loopback `CtlSyncClient`), mirroring `download_file_bytes_test`.
fn build_bound_engine(
    chunk_url: &str,
    secret: [u8; 32],
    device_id: [u8; 32],
    watch: &std::path::Path,
    raw_group_id: Vec<u8>,
    content_keys: FolderContentKeys,
) -> SyncEngine {
    let http_bearer: StdArc<dyn BearerSource> =
        StdArc::new(StaticBearer("test.bearer".to_string()));
    let http_auth = StdArc::new(fauna_client::AuthClient::with_bearer_source(
        chunk_url.to_string(),
        ActorKeypair::from_secret(secret),
        http_bearer,
        reqwest::Client::new(),
    ));
    let engine_client = HttpSyncClient::new(http_auth, &device_id);
    let nest_client =
        fauna_client::NestClient::new(chunk_url.to_string(), ActorKeypair::from_secret(secret));

    let engine = SyncEngine::new(
        watch.to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some("shared".to_string()),
        device_id,
        None, // mls — under M2 the chunk root is `content_keys`, not the engine MLS
        None, // epoch_secret
        None, // backup_key — a bound shared set carries none (Q4)
        Some(raw_group_id), // mls_group_id — the bound marker (fail-closed if unkeyed)
        Some(content_keys),
        ConflictPolicy::Auto,
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(StdArc::new(AdaptiveConcurrency::fixed(4)), None),
        nest_client,
        fauna_sync_engine::config::SyncMode::Sync,
    );
    // A writer engine signs every record it sends, directly under the
    // actor's own key and the fixture set nonce (`signature_required`).
    engine.set_change_signer(
        Some(common::direct_signer(&ActorKeypair::from_secret(secret))),
        Some(common::SET_NONCE),
    );
    engine
}

/// A distinct multi-chunk payload (>64 KiB forces FastCDC boundaries, under the
/// 64 MiB streaming threshold so the in-memory reassembly path runs) — disjoint
/// per `seed` so gen-1 and gen-2 chunks never collide content-addressed.
fn payload(seed: u32) -> Vec<u8> {
    (0..200_000u32)
        .map(|i| (i.wrapping_mul(seed) % 251) as u8)
        .collect()
}

fn write_file(watch: &std::path::Path, rel: &str, bytes: &[u8]) {
    let full = watch.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, bytes).unwrap();
}

/// The sealed bytes inside a stored content-key envelope blob: the nest holds
/// signature-plus-ciphertext (`fauna_protocol::folder_envelope_sig`), so a
/// member verifies the owner's signature before it opens what was signed.
fn signed_envelope_sealed(hex_blob: &str, channel_id: &[u8; 32]) -> Vec<u8> {
    let blob = hex::decode(hex_blob.trim()).expect("hex sealed envelope");
    fauna_protocol::folder_envelope_sig::verify(&blob, channel_id)
        .expect("the stored envelope is owner-signed")
        .sealed
}

/// B opens the live content-key envelope from the nest with its own MLS engine —
/// the real history-on-join key acquisition (`content_key.get` → hex-decode →
/// `open_content_key_envelope`). Returns the reconstructed generation history.
async fn open_envelope(
    files: &FoldersClient<RouterRequester>,
    engine: &MlsEngine,
    channel_id: [u8; 32],
) -> FolderContentKeys {
    let reply = files
        .content_key_get(ContentKeyGetRequest {
            name: "shared".into(),
            ..Default::default()
        })
        .await
        .expect("content_key.get (member)");
    let sealed = signed_envelope_sealed(&reply.sealed, &channel_id);
    engine
        .open_content_key_envelope(&ChannelId(channel_id), &sealed)
        .expect("open envelope with the group's current-epoch key")
}

#[tokio::test]
async fn removed_member_reads_pre_removal_but_fails_closed_post_removal_real_nest() {
    let (router, state) = control_plane();
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let chunk_url = server.uri();
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);

    // ── A + B form a real 2-member MLS group via the real adapter create_group ──
    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine = MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap();
    let b_kp_bytes = b_engine.generate_key_packages_bytes(1).unwrap();
    let created: CreatedGroup = FolderGroupCrypto::create_group(&a_engine, &b_kp_bytes).unwrap();
    let channel_id = created.channel_id;
    let b_joined = b_engine.join_from_welcome_bytes(&created.welcome).unwrap();
    assert_eq!(b_joined.0, channel_id, "B joins the same derived channel");

    // ── A owns "shared" and binds it to the group via the REAL share handler ────
    state
        .db
        .create_folder_with_options("shared", &a_actor, shared_set_options())
        .await
        .unwrap();
    let a_files = FoldersClient::new(requester(&router, &state, a_actor));
    let share_reply = a_files
        .share(FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&created.raw_group_id),
            ..Default::default()
        })
        .await
        .expect("share ok");
    assert!(share_reply.ok);
    assert_eq!(share_reply.channel_id, hex::encode(channel_id));

    // ── A's content-key orchestration: bind the genesis content key + publish ───
    let a_author = FoldersAuthor::new(
        FoldersClient::new(requester(&router, &state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    );
    a_author
        .bind_set("shared", channel_id)
        .await
        .expect("bind_set");

    // ── B joins the roster via the REAL Welcome flow → can read the shared set ──
    // Mode gate (direct-messages.md § Reach policy): these arrangement
    // welcomes ride the Group kind, so open the recipient's inbox.
    state.db.set_inbox_mode(&b_actor, "open").await.unwrap();
    dispatch(
        router.as_ref(),
        state.clone(),
        a_actor,
        "fauna.conversations.welcome.deliver",
        enc(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(b_actor),
            channel_id: hex::encode(channel_id),
            welcome_bytes: created.welcome.clone(),
            kind: WelcomeKind::Group {
                group_id: hex::encode(&created.raw_group_id),
            },
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("welcome deliver ok");
    assert!(
        state
            .db
            .is_actor_in_channel(&b_actor, &channel_id)
            .await
            .unwrap(),
        "B is on the roster"
    );

    // A + B both acquire their gen-1 content keys by opening the live envelope.
    let a_keys_gen1 = open_envelope(&a_files, &a_engine, channel_id).await;
    let b_files = FoldersClient::new(requester(&router, &state, b_actor));
    let b_keys_gen1 = open_envelope(&b_files, &b_engine, channel_id).await;
    assert_eq!(
        a_keys_gen1, b_keys_gen1,
        "owner + member open the same gen-1 bundle"
    );
    assert_eq!(b_keys_gen1.current_version(), 1, "genesis is generation 1");

    // ── A uploads generation-1 content (sealed under gen-1, version 1) ──────────
    let a_sync = signing_sync(&router, &state, A_SECRET);
    a_sync
        .register(hex::encode(DEVICE_A), "laptop", None)
        .await
        .expect("register A device");

    let bytes_gen1 = payload(13);
    let watch_a1 = tempfile::tempdir().unwrap();
    write_file(watch_a1.path(), "gen1.bin", &bytes_gen1);
    let a_engine_gen1 = build_bound_engine(
        &chunk_url,
        A_SECRET,
        DEVICE_A,
        watch_a1.path(),
        created.raw_group_id.clone(),
        a_keys_gen1.clone(),
    );
    a_engine_gen1
        .upload_file("gen1.bin")
        .await
        .expect("upload gen-1");
    let manifest_gen1 = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("gen-1 manifest posted");
    a_sync
        .changes_record(
            "shared",
            hex::encode(DEVICE_A),
            "gen1.bin",
            Some(hex::encode(manifest_gen1.digest())),
            bytes_gen1.len() as i64,
            "Created",
            Some(1),
            None,
            Some(b"e2e-synthetic-seal".to_vec()),
            None,
            None,
        )
        .await
        .expect("record gen-1 change");

    // ── B (a member) discovers gen-1 through the REAL changes.list + reads it ───
    let b_sync = CtlSyncClient::new(requester(&router, &state, b_actor));
    let listed = b_sync
        .changes_list(Some("shared".into()), None, 0)
        .await
        .expect("B lists");
    assert_eq!(listed.changes.len(), 1, "the member sees the gen-1 change");
    assert_eq!(
        listed.changes[0].manifest_hash.as_deref(),
        Some(hex::encode(manifest_gen1.digest()).as_str())
    );
    assert_eq!(
        listed.changes[0].content_key_version,
        Some(1),
        "the version round-trips through the real nest"
    );

    // Phase 0 read leg: B sources its content keys from INGESTED custody, not the
    // raw envelope. Fold the opened bundle into B's own folder-keys custody
    // (`custody::merge_received_keys` — the pure half of the ingest driver), then
    // read it back out (`custody::content_keys`). The download below therefore
    // exercises the production custody→download seam end-to-end against the real
    // nest, not the raw `open_envelope` result.
    let mut b_cfg = fauna_core::data::FoldersConfig::default();
    assert!(
        custody::merge_received_keys(&mut b_cfg, channel_id, b_keys_gen1.clone()),
        "first ingest populates member custody"
    );
    let b_custody_keys = custody::content_keys(&b_cfg, &channel_id)
        .expect("B holds gen-1 content keys after ingest");
    assert_eq!(
        b_custody_keys, b_keys_gen1,
        "ingested custody matches the opened envelope"
    );

    let watch_b = tempfile::tempdir().unwrap();
    let b_engine_gen1 = build_bound_engine(
        &chunk_url,
        B_SECRET,
        [0x0B; 32],
        watch_b.path(),
        created.raw_group_id.clone(),
        b_custody_keys,
    );
    let read_pre = b_engine_gen1
        .download_file_bytes_by_manifest(
            manifest_gen1,
            listed.changes[0].content_key_version,
            "gen1.bin",
        )
        .await
        .expect("member reads gen-1 content");
    assert_eq!(
        read_pre, bytes_gen1,
        "B decrypts gen-1 (it has the gen-1 key)"
    );

    // ── A removes B: rotate to gen-2 + re-seal envelope + evict from roster ─────
    let out = a_author
        .remove_member("shared", channel_id, ActorId(b_actor))
        .await
        .expect("remove_member");
    assert!(out.rotated, "removal rotated the content key");
    assert!(out.evicted, "removal evicted B from the roster");
    assert!(
        !state
            .db
            .is_actor_in_channel(&b_actor, &channel_id)
            .await
            .unwrap(),
        "B is off the roster (F1/OBS-1 metadata eviction)"
    );

    // A opens the rotated envelope (post-removal epoch) → gen-1 + gen-2 history.
    let a_keys_gen2 = open_envelope(&a_files, &a_engine, channel_id).await;
    assert_eq!(
        a_keys_gen2.current_version(),
        2,
        "owner rotated to generation 2"
    );
    assert_eq!(
        a_keys_gen2.key_for(1),
        b_keys_gen1.key_for(1),
        "gen-1 retained (history)"
    );
    assert_ne!(
        a_keys_gen2.current_key(),
        b_keys_gen1.current_key(),
        "fs-m2-fresh: gen-2 key independent of gen-1"
    );

    // ── A uploads generation-2 content (sealed under gen-2, version 2) ──────────
    let bytes_gen2 = payload(29);
    let watch_a2 = tempfile::tempdir().unwrap();
    write_file(watch_a2.path(), "gen2.bin", &bytes_gen2);
    let a_engine_gen2 = build_bound_engine(
        &chunk_url,
        A_SECRET,
        DEVICE_A,
        watch_a2.path(),
        created.raw_group_id.clone(),
        a_keys_gen2.clone(),
    );
    a_engine_gen2
        .upload_file("gen2.bin")
        .await
        .expect("upload gen-2");
    let manifest_gen2 = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("gen-2 manifest posted");
    let rec2 = a_sync
        .changes_record(
            "shared",
            hex::encode(DEVICE_A),
            "gen2.bin",
            Some(hex::encode(manifest_gen2.digest())),
            bytes_gen2.len() as i64,
            "Created",
            Some(2),
            None,
            Some(b"e2e-synthetic-seal".to_vec()),
            None,
            None,
        )
        .await
        .expect("record gen-2 change");
    assert!(rec2.seq > 0);

    // ── (metadata layer) B is evicted → the discovery path is now DENIED ────────
    let b_list_post = b_sync.changes_list(Some("shared".into()), None, 0).await;
    assert!(
        b_list_post.is_err(),
        "an evicted member cannot list the shared set"
    );
    let b_envelope_post = b_files
        .content_key_get(ContentKeyGetRequest {
            name: "shared".into(),
            ..Default::default()
        })
        .await;
    assert!(
        b_envelope_post.is_err(),
        "an evicted member cannot fetch the rotated envelope"
    );

    // ── (crypto layer) B still reads gen-1 (history) but FAILS CLOSED on gen-2 ──
    let read_history = b_engine_gen1
        .download_file_bytes_by_manifest(manifest_gen1, Some(1), "gen1.bin")
        .await
        .expect("a removed member still reads pre-removal content (history-on-join)");
    assert_eq!(read_history, bytes_gen1);

    // Even handed the gen-2 manifest + version (the bytes are public-by-hash, so B
    // CAN fetch the ciphertext), B's gen-1-only keys cannot select `key_for(2)` —
    // the read fails closed rather than fall through to plaintext. This is the
    // crypto-layer basis OBS-1 says the guarantee rests on, never the roster.
    let read_post = b_engine_gen1
        .download_file_bytes_by_manifest(manifest_gen2, Some(2), "gen2.bin")
        .await;
    assert!(
        read_post.is_err(),
        "a gen-1-only (removed) member MUST fail closed on gen-2 (post-removal) content"
    );

    // ── B cannot `share`-rebind itself back onto the roster ──
    state
        .db
        .create_folder_with_options(
            "b-set",
            &b_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let rebind = b_files
        .share(FolderShareRequest {
            name: "b-set".into(),
            group_id: hex::encode(&created.raw_group_id),
            ..Default::default()
        })
        .await
        .expect_err("a removed member must not be able to claim the group's channel");
    assert!(
        rebind
            .as_rpc_error()
            .is_some_and(|e| e.code.contains("already_claimed")),
        "rebind denied with already_claimed, got: {rebind}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 5d(d) — the **remaining member's epoch-advance liveness** on the real nest
// stack (`mls-group-key-material.md` § M2 *Rotate-on-removal*, the "remaining
// members advance to the new epoch" half).
//
// A three-party set: owner A, removed B, remaining C. What it proves:
//   1. A's real `FoldersAuthor::remove_member` now DISTRIBUTES the MLS Remove
//      commit: the real `fauna.conversations.channel.send` handler accepts the
//      owner's `ChannelEnvelope::Commit` on the CLAIMED folder channel (the
//      claim + roster gates pass for the claimant);
//   2. C's production folder commit poll (`poll_inbound_folder` over a
//      `ConversationsRpc` backed by the real `channel.fetch` handler) applies
//      the commit, advancing C's engine to the post-removal epoch;
//   3. C then opens the RE-PUBLISHED envelope through the real
//      `content_key.get` — full history (gen 1 + gen 2) — which fails closed
//      before the poll runs (the pre-poll assertion is the former 5d(d) gap);
//   4. C stays on the roster throughout (eviction touched only B).
// ─────────────────────────────────────────────────────────────────────────────

/// Remaining member C's identity.
const C_SECRET: [u8; 32] = [0xC3; 32];

/// The minimal loopback [`ConversationsRpc`] the member-side commit poll
/// drives — `channel_send`/`channel_fetch` dispatch into the REAL handlers via
/// [`RouterRequester`]; every other method is unreachable in this test.
struct LoopbackConvRpc(RouterRequester);

#[async_trait::async_trait]
impl ConversationsRpc for LoopbackConvRpc {
    async fn channel_send_remote(
        &self,
        _channel_id_hex: String,
        _home_nest_url: String,
        _envelope: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        // Single-nest rig — no channel is foreign-homed; reaching this is a
        // routing bug in the test, not a path to emulate. Fail loud.
        Err(ConvRpcError::Rejected {
            message: "single-nest test rig has no federation relay (unexpected send_remote)".into(),
        })
    }

    async fn channel_send(
        &self,
        channel_id_hex: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        let reply: fauna_protocol::conversations::ChannelSendReply = self
            .0
            .request(
                "fauna.conversations.channel.send",
                fauna_protocol::conversations::ChannelSendRequest {
                    channel_id: channel_id_hex,
                    envelope,
                    expect_no_commit_since,
                    attachment_refs,
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| ConvRpcError::Rejected {
                message: e.to_string(),
            })?;
        Ok(reply.seq)
    }

    async fn channel_fetch(
        &self,
        channel_id_hex: String,
        after: i64,
        limit: i64,
        _home_nest_url: Option<String>,
    ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
        let reply: fauna_protocol::conversations::ChannelFetchReply = self
            .0
            .request(
                "fauna.conversations.channel.fetch",
                fauna_protocol::conversations::ChannelFetchRequest {
                    channel_id: channel_id_hex,
                    after,
                    limit: if limit == 0 { 500 } else { limit },
                    nest_url: None,
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| ConvRpcError::Rejected {
                message: e.to_string(),
            })?;
        Ok(reply
            .messages
            .into_iter()
            .map(|m| fauna_conversations::backend::FetchedRecord {
                seq: m.seq,
                envelope: m.envelope,
                legal_takedown_ref: m.legal_takedown.map(|t| t.reference),
                labels: m.labels,
                author: m.author,
            })
            .collect())
    }

    async fn keypackage_count(&self, _a: String) -> Result<u64, ConvRpcError> {
        unreachable!("not used by the folder commit poll")
    }
    async fn actor_by_handle(&self, _h: String) -> Result<Option<ResolvedHandle>, ConvRpcError> {
        unreachable!("not used by the folder commit poll")
    }
    async fn actor_by_handle_remote(
        &self,
        _d: String,
        _l: String,
    ) -> Result<Option<ResolvedHandle>, ConvRpcError> {
        unreachable!("not used by the folder commit poll")
    }
    async fn keypackage_fetch(
        &self,
        _a: String,
        _p: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        unreachable!("not used by the folder commit poll")
    }
    async fn keypackage_upload(&self, _p: Vec<Vec<u8>>, _l: bool) -> Result<u64, ConvRpcError> {
        unreachable!("not used by the folder commit poll")
    }
    async fn welcome_deliver(
        &self,
        _r: String,
        _c: String,
        _w: Vec<u8>,
        _k: WelcomeChannelKind,
        _p: Option<String>,
    ) -> Result<(), ConvRpcError> {
        unreachable!("not used by the folder commit poll")
    }
    async fn blob_put(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
        _b: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        unreachable!("not used by the folder commit poll")
    }
    async fn blob_get(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        unreachable!("not used by the folder commit poll")
    }
}

#[tokio::test]
async fn remaining_member_advances_epoch_via_distributed_remove_commit_real_nest() {
    let (router, state) = control_plane();
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);
    let c_actor = actor_of(C_SECRET);

    // ── A + B + C form a real 3-member MLS group via the real adapter ───────────
    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine = MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap();
    let c_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(C_SECRET)).unwrap());
    let kp_bytes: Vec<Vec<u8>> = b_engine
        .generate_key_packages_bytes(1)
        .unwrap()
        .into_iter()
        .chain(c_engine.generate_key_packages_bytes(1).unwrap())
        .collect();
    let created: CreatedGroup = FolderGroupCrypto::create_group(&a_engine, &kp_bytes).unwrap();
    let channel_id = created.channel_id;
    assert_eq!(
        b_engine
            .join_from_welcome_bytes(&created.welcome)
            .unwrap()
            .0,
        channel_id
    );
    assert_eq!(
        c_engine
            .join_from_welcome_bytes(&created.welcome)
            .unwrap()
            .0,
        channel_id
    );

    // ── A owns "shared", claims the channel, binds the genesis content key ──────
    state
        .db
        .create_folder_with_options(
            "shared",
            &a_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let a_files = FoldersClient::new(requester(&router, &state, a_actor));
    a_files
        .share(FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&created.raw_group_id),
            ..Default::default()
        })
        .await
        .expect("share ok");
    let a_author = FoldersAuthor::new(
        FoldersClient::new(requester(&router, &state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    );
    a_author
        .bind_set("shared", channel_id)
        .await
        .expect("bind_set");

    // ── B + C join the roster via the REAL Welcome flow ─────────────────────────
    for member in [b_actor, c_actor] {
        // Mode gate (direct-messages.md § Reach policy): these arrangement
        // welcomes ride the Group kind, so open the recipient's inbox.
        state.db.set_inbox_mode(&member, "open").await.unwrap();
        dispatch(
            router.as_ref(),
            state.clone(),
            a_actor,
            "fauna.conversations.welcome.deliver",
            enc(&WelcomeDeliverRequest {
                recipient_actor_id: hex::encode(member),
                channel_id: hex::encode(channel_id),
                welcome_bytes: created.welcome.clone(),
                kind: WelcomeKind::Group {
                    group_id: hex::encode(&created.raw_group_id),
                },
                nest_url: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("welcome deliver ok");
    }

    // C acquires gen-1 through the real envelope fetch — the shared-epoch baseline.
    let c_files = FoldersClient::new(requester(&router, &state, c_actor));
    let c_keys_gen1 = open_envelope(&c_files, &c_engine, channel_id).await;
    assert_eq!(c_keys_gen1.current_version(), 1);

    // ── A removes B — the real orchestration now also DISTRIBUTES the commit ────
    let out = a_author
        .remove_member("shared", channel_id, ActorId(b_actor))
        .await
        .expect("remove_member (accepts the owner's commit-send on the claimed channel)");
    assert!(out.rotated && out.evicted);
    assert!(
        out.commit.is_some(),
        "a real removal produced the distributed commit"
    );
    assert!(
        state
            .db
            .is_actor_in_channel(&c_actor, &channel_id)
            .await
            .unwrap(),
        "the remaining member C stays on the roster"
    );

    // Pre-poll: C is stuck at the pre-removal epoch — the re-published envelope
    // (sealed under the post-removal epoch) fails closed. This is the exact
    // 5d(d) liveness gap the distribution + poll close.
    let c_envelope_reply = c_files
        .content_key_get(ContentKeyGetRequest {
            name: "shared".into(),
            ..Default::default()
        })
        .await
        .expect("a remaining member still fetches the envelope");
    let sealed_gen2 = signed_envelope_sealed(&c_envelope_reply.sealed, &channel_id);
    assert!(
        c_engine
            .open_content_key_envelope(&ChannelId(channel_id), &sealed_gen2)
            .is_err(),
        "before the commit poll, C cannot open the post-removal envelope"
    );

    // ── C's production folder commit poll over the REAL channel.fetch ─────────
    let c_backend = FaunaMlsBackend::new(
        c_engine.clone(),
        StdArc::new(LoopbackConvRpc(requester(&router, &state, c_actor)))
            as StdArc<dyn ConversationsRpc>,
        "carol",
        ActorId(c_actor),
    );
    assert_eq!(
        c_backend.folder_poll_channels(),
        vec![ChannelId(channel_id)],
        "the joined set is derived from the engine (restart-durable poll set)"
    );
    let mut cursor = 0i64;
    let outcome = poll_inbound_folder(&c_backend, &ChannelId(channel_id), &mut cursor, 0)
        .await
        .expect("folder commit poll");
    assert_eq!(
        outcome.applied, 1,
        "the owner's Remove commit advanced C's epoch"
    );
    assert!(!outcome.stalled, "nothing to heal on this pass");

    // ── C now opens the re-published envelope: full history, gen-2 current ──────
    let c_keys_gen2 = c_engine
        .open_content_key_envelope(&ChannelId(channel_id), &sealed_gen2)
        .expect("post-poll open");
    assert_eq!(c_keys_gen2.current_version(), 2, "rotated to generation 2");
    assert_eq!(
        c_keys_gen2.key_for(1),
        c_keys_gen1.key_for(1),
        "gen-1 retained (history-on-join)"
    );

    // Phase 0 read leg — re-ingest across the rotation. C's custody, populated at
    // join with gen-1 (`custody::merge_received_keys`), advances to gen-2 when the
    // post-rotation bundle is ingested — exactly the sequence the folder commit
    // poll's ingest driver runs after applying the Remove commit. The merge is a
    // CRDT: gen-2 becomes current, gen-1 is retained.
    let mut c_cfg = fauna_core::data::FoldersConfig::default();
    assert!(custody::merge_received_keys(
        &mut c_cfg,
        channel_id,
        c_keys_gen1.clone()
    ));
    assert_eq!(
        custody::content_keys(&c_cfg, &channel_id)
            .unwrap()
            .current_version(),
        1,
        "custody holds gen-1 after the join ingest"
    );
    assert!(
        custody::merge_received_keys(&mut c_cfg, channel_id, c_keys_gen2.clone()),
        "re-ingesting the rotated bundle advances custody"
    );
    let c_custody = custody::content_keys(&c_cfg, &channel_id).unwrap();
    assert_eq!(c_custody.current_version(), 2, "custody advanced to gen-2");
    assert_eq!(
        c_custody.key_for(1),
        c_keys_gen1.key_for(1),
        "gen-1 retained in custody across the rotation"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The GATED removal route + the resume paths over the real nest stack
// (`mls-group-key-material.md` § M2 *Rotate-on-removal* — the crash-safety
// contract). Until this section, the gated route (CommitGate adoption) and the
// resume/recovery discipline were pinned at tier_1 only: `fauna-client-folders`
// unit tests over `FakeNest`/`FakeGroup`, `fauna-client-mls-sync` gate tests
// over `FakeConvNest`/`MemReplica`. The three tests below wire the REAL gated
// plane — `FaunaMlsBackend` + `MlsStateSync` over the real `fauna.mls.{get,put}`
// handlers + `FaunaCommitGate` — against the real `channel.{send,fetch}` /
// `content_key.{put,get}` / `members.evict` handlers:
//
//   1. `gated_removal_...` — a fresh gated removal: the Remove rides
//      `send_commit_gated` (stage → CAS-put the provider replica → gate-send
//      under `expect_no_commit_since` → merge on accept); the sentinel carries
//      no bytes; the remaining member's production poll advances; the removed
//      member fails closed on the re-published envelope.
//   2. `byted_resume_...` — the ungated crash window (bytes durable on the
//      sentinel, engine merged, send never happened): the resume re-sends the
//      sentinel's EXACT bytes over the real channel log — never rebuilds —
//      even though a gate is wired (durable bytes outrank the gate).
//   3. `gated_resume_...` — a byte-less sentinel (crash before the commit was
//      built) with the gate wired: the resume routes through the gate's
//      walk-to-head over the real log and freshly removes.
// ─────────────────────────────────────────────────────────────────────────────

/// The real [`MlsReplicaTransport`] over the in-process router — the loopback
/// twin of `fauna-client-conversations::NestMlsReplicaTransport`, dispatching
/// `fauna.mls.{get,put}` into the REAL sealed-replica handlers.
struct LoopbackMlsReplica(RouterRequester);

#[async_trait::async_trait]
impl MlsReplicaTransport for LoopbackMlsReplica {
    async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
        rpc_transport_get(&self.0, path).await
    }
    async fn put(
        &self,
        path: String,
        blob: Vec<u8>,
        base: ReplicaBase,
    ) -> Result<PutOutcome, MlsTransportError> {
        rpc_transport_put(&self.0, path, blob, base).await
    }
}

/// The owner's FULL gated plane over the real nest — the production graph the
/// per-app legs assemble (`fauna-client-mls-sync::restore_and_wire` /
/// `gate_impl`'s `assemble_device_for`): backend + manager registered, the
/// channel marked as a folder membership (the `BackendCatchUp` routing state a
/// recipient's `join_folder_welcome` establishes), `MlsStateSync` loaded over
/// the real `fauna.mls.{get,put}` (lifting the launch gate), `BackendCatchUp` +
/// [`FaunaCommitGate`] assembled and injected. The returned `manager`/`sync`
/// must stay alive — `BackendCatchUp` holds them weakly.
struct GatedOwnerPlane {
    backend: StdArc<FaunaMlsBackend>,
    #[allow(dead_code)] // held for the BackendCatchUp Weak refs
    manager: StdArc<ConversationsManager>,
    #[allow(dead_code)]
    sync: StdArc<MlsStateSync>,
}

async fn assemble_gated_owner(
    router: &StdArc<RpcRouter>,
    state: &StdArc<AppState>,
    engine: StdArc<MlsEngine>,
    secret: [u8; 32],
    channel: ChannelId,
) -> GatedOwnerPlane {
    let actor = actor_of(secret);
    let conv: StdArc<dyn ConversationsRpc> =
        StdArc::new(LoopbackConvRpc(requester(router, state, actor)));
    let manager = ConversationsManager::new();
    let backend = StdArc::new(FaunaMlsBackend::new(
        engine.clone(),
        conv.clone(),
        "owner",
        engine.identity_actor_id(),
    ));
    manager.register_backend(backend.clone());
    backend.mark_folder_channel(channel);
    let sync = StdArc::new(MlsStateSync::new(
        Box::new(LoopbackMlsReplica(requester(router, state, actor))),
        &ActorKeypair::from_secret(secret),
    ));
    sync.load()
        .await
        .expect("replica load over the real fauna.mls.get (lifts the launch gate)");
    let catch_up = BackendCatchUp::new(&backend, &manager);
    let gate = StdArc::new(FaunaCommitGate::new(
        sync.clone(),
        engine,
        BackendChannelSend::new(&backend),
        catch_up,
    ));
    backend.set_commit_gate(gate);
    GatedOwnerPlane {
        backend,
        manager,
        sync,
    }
}

/// A + B + C form a real 3-member group; A owns + shares + binds "shared" and
/// delivers B's + C's Welcomes through the real roster flow — the shared setup
/// of the three gated/resume tests (the test-3 shape, factored).
struct ThreeParty {
    router: StdArc<RpcRouter>,
    state: StdArc<AppState>,
    a_engine: StdArc<MlsEngine>,
    b_engine: MlsEngine,
    c_engine: StdArc<MlsEngine>,
    channel_id: [u8; 32],
    /// The owner's folder-key custody — the store every author A builds shares,
    /// as the seat's one `AccountStoreHandle` is shared in production.
    a_custody: StdArc<MemoryFolderKeyStore>,
}

async fn three_party_bound_set() -> ThreeParty {
    let (router, state) = control_plane();
    let a_actor = actor_of(A_SECRET);

    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine = MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap();
    let c_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(C_SECRET)).unwrap());
    let kp_bytes: Vec<Vec<u8>> = b_engine
        .generate_key_packages_bytes(1)
        .unwrap()
        .into_iter()
        .chain(c_engine.generate_key_packages_bytes(1).unwrap())
        .collect();
    let created: CreatedGroup = FolderGroupCrypto::create_group(&a_engine, &kp_bytes).unwrap();
    let channel_id = created.channel_id;
    assert_eq!(
        b_engine
            .join_from_welcome_bytes(&created.welcome)
            .unwrap()
            .0,
        channel_id
    );
    assert_eq!(
        c_engine
            .join_from_welcome_bytes(&created.welcome)
            .unwrap()
            .0,
        channel_id
    );

    state
        .db
        .create_folder_with_options(
            "shared",
            &a_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let a_files = FoldersClient::new(requester(&router, &state, a_actor));
    a_files
        .share(FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&created.raw_group_id),
            ..Default::default()
        })
        .await
        .expect("share ok");
    // Genesis custody + the sealed gen-1 envelope (ungated author — bind
    // produces no MLS commit, so the gate is irrelevant here).
    let a_custody = StdArc::new(MemoryFolderKeyStore::default());
    FoldersAuthor::new(
        FoldersClient::new(requester(&router, &state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        a_custody.clone(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    )
    .bind_set("shared", channel_id)
    .await
    .expect("bind_set");

    for member in [actor_of(B_SECRET), actor_of(C_SECRET)] {
        // Mode gate (direct-messages.md § Reach policy): these arrangement
        // welcomes ride the Group kind, so open the recipient's inbox.
        state.db.set_inbox_mode(&member, "open").await.unwrap();
        dispatch(
            router.as_ref(),
            state.clone(),
            a_actor,
            "fauna.conversations.welcome.deliver",
            enc(&WelcomeDeliverRequest {
                recipient_actor_id: hex::encode(member),
                channel_id: hex::encode(channel_id),
                welcome_bytes: created.welcome.clone(),
                kind: WelcomeKind::Group {
                    group_id: hex::encode(&created.raw_group_id),
                },
                nest_url: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("welcome deliver ok");
    }

    ThreeParty {
        router,
        state,
        a_engine,
        b_engine,
        c_engine,
        channel_id,
        a_custody,
    }
}

/// The gated author: the production wiring (`libs/fauna-ffi/src/folders_author.rs`
/// / linux `client.rs`) — the session backend doubles as the `FolderCommitGate`.
fn gated_author(
    w: &ThreeParty,
    backend: &StdArc<FaunaMlsBackend>,
) -> FoldersAuthor<RouterRequester, StdArc<MlsEngine>> {
    let a_actor = actor_of(A_SECRET);
    FoldersAuthor::new(
        FoldersClient::new(requester(&w.router, &w.state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        w.a_custody.clone(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        w.a_engine.clone(),
    )
    .with_commit_gate(StdArc::new(backend.clone()))
}

/// Fabricate the crash sentinel in the owner's custody: stage a removal of
/// `removed` one generation past the genesis, carrying `commit` (the bytes
/// staged before the crash, if any), and persist it — the write
/// `remove_member`'s stage step makes before its drive.
async fn stage_crash_sentinel(w: &ThreeParty, removed: [u8; 32], key: u8, commit: Option<Vec<u8>>) {
    let mut custody = w.a_custody.load().await.expect("load custody");
    let base = custody::current_generation(&custody, &w.channel_id).expect("genesis custody");
    custody::stage_pending_removal(
        &mut custody,
        FolderPendingRemoval {
            channel_id: w.channel_id,
            name: "shared".into(),
            removed_member: ActorId(removed),
            new_generation: ContentKeyGeneration {
                version: base.version + 1,
                key: [key; 32].into(),
                rotated_at: base.rotated_at + 1,
            },
            commit,
            gated_attempted: false,
        },
    );
    w.a_custody
        .merge(custody)
        .await
        .expect("persist the sentinel");
}

/// Fetch the raw channel log via the real `channel.fetch` and decode every
/// envelope — the wire-level view of what the removal actually distributed.
async fn channel_log(w: &ThreeParty) -> Vec<ChannelEnvelope> {
    LoopbackConvRpc(requester(&w.router, &w.state, actor_of(C_SECRET)))
        .channel_fetch(hex::encode(w.channel_id), 0, 0, None)
        .await
        .expect("channel.fetch")
        .into_iter()
        .map(|r| ChannelEnvelope::from_bytes(&r.envelope).expect("decode envelope"))
        .collect()
}

/// The remaining member C applies the distributed Remove through the production
/// folder commit poll and opens the re-published envelope; the removed member
/// B fails closed on it (new epoch) AND is denied the fetch (evicted). The
/// shared verification tail of all three tests.
async fn assert_rotation_landed(w: &ThreeParty, gen1: &FolderContentKeys) {
    let c_actor = actor_of(C_SECRET);
    let b_actor = actor_of(B_SECRET);

    // C's production poll over the real channel.fetch applies the Remove.
    let c_backend = FaunaMlsBackend::new(
        w.c_engine.clone(),
        StdArc::new(LoopbackConvRpc(requester(&w.router, &w.state, c_actor)))
            as StdArc<dyn ConversationsRpc>,
        "carol",
        ActorId(c_actor),
    );
    let mut cursor = 0i64;
    let outcome = poll_inbound_folder(&c_backend, &ChannelId(w.channel_id), &mut cursor, 0)
        .await
        .expect("folder commit poll");
    assert_eq!(
        outcome.applied, 1,
        "the distributed Remove commit advanced C's epoch"
    );
    assert!(!outcome.stalled);

    // C opens the re-published envelope: rotated to gen-2, gen-1 retained.
    let c_files = FoldersClient::new(requester(&w.router, &w.state, c_actor));
    let c_keys = open_envelope(&c_files, &w.c_engine, w.channel_id).await;
    assert_eq!(c_keys.current_version(), 2, "rotated to generation 2");
    assert_eq!(
        c_keys.key_for(1),
        gen1.key_for(1),
        "gen-1 retained (history-on-join)"
    );

    // The removed member B: evicted from the roster (the envelope fetch is
    // denied) AND cryptographically excluded (the re-published envelope is
    // sealed under the post-removal epoch B never reaches).
    let b_files = FoldersClient::new(requester(&w.router, &w.state, b_actor));
    let denied = b_files
        .content_key_get(ContentKeyGetRequest {
            name: "shared".into(),
            ..Default::default()
        })
        .await;
    assert!(
        denied.is_err(),
        "an evicted member cannot fetch the rotated envelope"
    );
    let sealed_gen2 = signed_envelope_sealed(
        &c_files
            .content_key_get(ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            })
            .await
            .expect("member envelope fetch")
            .sealed,
        &w.channel_id,
    );
    assert!(
        w.b_engine
            .open_content_key_envelope(&ChannelId(w.channel_id), &sealed_gen2)
            .is_err(),
        "the removed member cannot open the post-removal envelope"
    );

    // The sentinel is cleared — the rotation committed.
    assert!(
        w.a_custody.snapshot().pending_removals.is_empty(),
        "the pending-removal sentinel is cleared once the rotation commits"
    );
}

#[tokio::test]
async fn gated_removal_rotates_and_excludes_removed_member_real_nest() {
    let w = three_party_bound_set().await;
    let b_actor = actor_of(B_SECRET);
    let c_actor = actor_of(C_SECRET);

    // Baseline: C holds gen-1 through the real envelope fetch.
    let c_files = FoldersClient::new(requester(&w.router, &w.state, c_actor));
    let gen1 = open_envelope(&c_files, &w.c_engine, w.channel_id).await;
    assert_eq!(gen1.current_version(), 1);

    // The owner's FULL gated plane + the production author wiring.
    let plane = assemble_gated_owner(
        &w.router,
        &w.state,
        w.a_engine.clone(),
        A_SECRET,
        ChannelId(w.channel_id),
    )
    .await;
    let author = gated_author(&w, &plane.backend);

    // A removes B THROUGH THE GATE.
    let out = author
        .remove_member("shared", w.channel_id, ActorId(b_actor))
        .await
        .expect("gated remove_member");
    assert!(out.rotated && out.evicted);
    assert!(
        out.commit.is_none(),
        "the gated route distributes inside the rebase loop and never \
         re-surfaces bytes (ungated would return Some) — proves the gate ran"
    );

    // The gate's step-2 durability hit the REAL nest: the provider replica was
    // CAS-put through fauna.mls.put before the send.
    let replica = rpc_transport_get(
        &requester(&w.router, &w.state, actor_of(A_SECRET)),
        PATH_PROVIDER.to_string(),
    )
    .await
    .expect("fauna.mls.get");
    assert!(
        replica.is_some(),
        "send_commit_gated CAS-put the provider replica (durable, \
         identity-stamped staged pending) through the real fauna.mls.put"
    );

    // Exactly one Commit envelope rode the real channel log.
    let log = channel_log(&w).await;
    assert_eq!(log.len(), 1, "exactly the gated Remove was distributed");
    assert!(matches!(log[0], ChannelEnvelope::Commit(_)));

    assert_rotation_landed(&w, &gen1).await;
}

#[tokio::test]
async fn byted_resume_resends_real_commit_bytes_and_completes_rotation_real_nest() {
    let w = three_party_bound_set().await;
    let b_actor = actor_of(B_SECRET);
    let c_actor = actor_of(C_SECRET);
    let channel = ChannelId(w.channel_id);

    let c_files = FoldersClient::new(requester(&w.router, &w.state, c_actor));
    let gen1 = open_envelope(&c_files, &w.c_engine, w.channel_id).await;

    // ── Fabricate the ungated crash window with REAL commit bytes: stage the
    // MLS Remove, record its bytes on the sentinel (durable), merge — crash
    // BEFORE the send (the exact `drive_removal_commit_ungated` sequence,
    // interrupted at its last step). The member is off the owner's leaf; only
    // the sentinel's bytes can reach the remaining members.
    let commit_bytes =
        FolderGroupCrypto::remove_member_staged(&w.a_engine, &w.channel_id, &ActorId(b_actor))
            .expect("stage the Remove")
            .expect("B was in the group — a commit was produced");
    stage_crash_sentinel(&w, b_actor, 0x3d, Some(commit_bytes.clone())).await;
    FolderGroupCrypto::merge_pending_commit(&w.a_engine, &w.channel_id)
        .expect("merge (the crash hits before the send)");

    // ── Resume on the next launch — with a GATE WIRED: durable sentinel bytes
    // outrank the gate (the ungated leg re-sends them verbatim; the gate would
    // misread the merged engine as "member already absent" and strand C).
    let plane =
        assemble_gated_owner(&w.router, &w.state, w.a_engine.clone(), A_SECRET, channel).await;
    let author = gated_author(&w, &plane.backend);
    assert_eq!(
        author
            .resume_pending_removals()
            .await
            .expect("resume completes the interrupted removal"),
        1
    );

    // The wire saw the sentinel's EXACT bytes — re-sent, never rebuilt.
    let log = channel_log(&w).await;
    assert_eq!(log.len(), 1, "exactly the resumed Remove was distributed");
    assert!(
        matches!(&log[0], ChannelEnvelope::Commit(b) if *b == commit_bytes),
        "the resume re-sent the durable sentinel bytes verbatim"
    );

    assert_rotation_landed(&w, &gen1).await;
}

#[tokio::test]
async fn gated_resume_completes_byteless_sentinel_via_walk_to_head_real_nest() {
    let w = three_party_bound_set().await;
    let b_actor = actor_of(B_SECRET);
    let c_actor = actor_of(C_SECRET);
    let channel = ChannelId(w.channel_id);

    let c_files = FoldersClient::new(requester(&w.router, &w.state, c_actor));
    let gen1 = open_envelope(&c_files, &w.c_engine, w.channel_id).await;

    // ── Fabricate the earlier crash window: the sentinel was staged + persisted
    // (`remove_member`'s stage step) but the drive never ran — no commit was
    // built, B is still on the leaf.
    stage_crash_sentinel(&w, b_actor, 0x3d, None).await;

    // ── Resume with the gate wired: a byte-less sentinel consults the gate,
    // whose entry protocol walks the real channel log to head (clean — nothing
    // was ever sent), then freshly removes through the rebase loop.
    let plane =
        assemble_gated_owner(&w.router, &w.state, w.a_engine.clone(), A_SECRET, channel).await;
    let author = gated_author(&w, &plane.backend);
    assert_eq!(
        author
            .resume_pending_removals()
            .await
            .expect("gated resume completes the staged removal"),
        1
    );
    assert!(
        !FolderGroupCrypto::contains_member(&w.a_engine, &w.channel_id, &ActorId(b_actor))
            .expect("membership read"),
        "B is out of the owner's group after the gated resume"
    );

    let log = channel_log(&w).await;
    assert_eq!(log.len(), 1, "exactly the gated Remove was distributed");
    assert!(matches!(log[0], ChannelEnvelope::Commit(_)));

    assert_rotation_landed(&w, &gen1).await;
}

/// The **launch recipe** — the author as the launch-time crash recovery actually
/// builds it (`libs/fauna-ffi/src/mls_sync_launch.rs::resume_folder_removals`, and
/// its linux/web twins in `conv_backend.rs` / `conversations.ts`).
///
/// Every other resume proof in this file hands the author `w.a_engine` *directly*. The
/// launch triggers can't: they hold a [`FaunaMlsBackend`], so they derive the
/// group-crypto seam from `backend.engine()` and reuse that same backend as the
/// `FolderCommitGate`. Nothing pinned that `backend.engine()` is *the engine holding
/// the set's group* — and if it ever stopped being so, the launch-time resume would
/// build its author over an empty engine, find no group, and **silently complete zero
/// removals**. That is precisely the failure mode this rail just spent a fix on: the
/// recovery machinery was fully green at tier_1 and tier_3 while being entirely dark in
/// production, because nothing exercised the path the apps actually take. So the
/// launch recipe gets its own end-to-end proof over the real gated plane.
#[tokio::test]
async fn launch_recipe_resumes_staged_removal_over_backend_engine_real_nest() {
    let w = three_party_bound_set().await;
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);
    let c_actor = actor_of(C_SECRET);
    let channel = ChannelId(w.channel_id);

    let c_files = FoldersClient::new(requester(&w.router, &w.state, c_actor));
    let gen1 = open_envelope(&c_files, &w.c_engine, w.channel_id).await;

    // ── The crash window: a removal was staged + persisted, then the process died
    // before the drive published anything. B is still on the leaf, and nothing in the
    // client will ever re-drive this except the launch-time resume.
    stage_crash_sentinel(&w, b_actor, 0x5e, None).await;

    // ── Relaunch: the restore wires the plane, which is what makes the gate live.
    let plane =
        assemble_gated_owner(&w.router, &w.state, w.a_engine.clone(), A_SECRET, channel).await;

    // The load-bearing identity: the seam the launcher passes as `FolderGroupCrypto`
    // is the very engine that holds the group. Asserted directly — a refactor handing
    // the backend its own engine would otherwise turn the resume into a no-op that
    // still reports success.
    assert!(
        StdArc::ptr_eq(&plane.backend.engine(), &w.a_engine),
        "backend.engine() must be the engine holding the set's group — the launch \
         recovery derives its group-crypto seam from it"
    );

    // ── The author exactly as `mls_sync_launch::resume_folder_removals` builds it:
    // engine AND gate both derived from the backend the launcher holds.
    let author = FoldersAuthor::new(
        FoldersClient::new(requester(&w.router, &w.state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        w.a_custody.clone(),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        plane.backend.engine(),
    )
    .with_commit_gate(StdArc::new(plane.backend.clone()));

    assert_eq!(
        author
            .resume_pending_removals()
            .await
            .expect("the launch recipe completes the staged removal"),
        1,
        "launch-time recovery drives the crash-staged removal to completion"
    );

    assert!(
        !FolderGroupCrypto::contains_member(&w.a_engine, &w.channel_id, &ActorId(b_actor))
            .expect("membership read"),
        "B is out of the group after the launch-time resume"
    );

    let log = channel_log(&w).await;
    assert_eq!(log.len(), 1, "exactly the recovered Remove was distributed");
    assert!(matches!(log[0], ChannelEnvelope::Commit(_)));

    // The rotation the crashed drive staged is now published — forward secrecy
    // completes, which is the whole point of the launch-time resume.
    assert_rotation_landed(&w, &gen1).await;
}

// ── Multi-writer Phase 1: the same-nest read-write plane over the REAL router ──
//
// `file-sync.md` § Multi-writer shared sets + `ui/folders.md` § Sharing +
// KMH § M2 version floor. Handler-level conformance of the whole write plane:
// the share-time grant, the `writable_folder` gate at `changes.record`,
// owner-pays metering + the member byte cap, the monotonic version floor with
// the owner exemption, role demotion, the members-stay-refused owner-only
// kinds, and nest-stamped attribution on the member feed.

/// Register a write-capable sync device for `actor` over the real
/// `fauna.sync.register` kind.
async fn register_device(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    device: [u8; 32],
) {
    dispatch(
        router,
        state.clone(),
        actor,
        "fauna.sync.register",
        enc(&fauna_protocol::sync::SyncRegisterRequest {
            device_id: hex::encode(device),
            label: "test-device".into(),
            capabilities: "read,write".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("device registers");
}

/// A `changes.record` request for `path`, `size` bytes, stamped with
/// `content_key_version` — signed by `recorder`'s own key under the set's
/// stored nonce ([`create_rw_docs`]), as every writer engine signs (a member
/// signs under the OWNER's set nonce with its own key).
fn record_req(
    recorder: &ActorKeypair,
    device: [u8; 32],
    path: &str,
    size: i64,
    manifest_byte: u8,
    content_key_version: Option<u64>,
) -> Bytes {
    enc(&fauna_protocol::folders::addressed(common::signed_record(
        fauna_protocol::sync::SyncChangeRecordRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            nest_url: None,
            channel_id: None,
            folder: "rw-docs".into(),
            device_id: hex::encode(device),
            path: path.into(),
            change_type: "create".into(),
            size_bytes: size,
            manifest_hash: Some(hex::encode([manifest_byte; 32])),
            content_key_version,
            thumbnail_hash: None,
            ..Default::default()
        },
        recorder,
    )))
}

/// The owner creates "rw-docs" under the fixture set nonce — what every
/// [`record_req`] binds to.
async fn create_rw_docs(state: &Arc<AppState>, owner: &[u8; 32]) {
    state
        .db
        .create_folder_with_options(
            "rw-docs",
            owner,
            fauna_nest::db::FolderOptions {
                set_nonce: Some(common::SET_NONCE.to_vec()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

/// The owner's `storage_bytes_used` (owner-pays: the only counter that moves).
async fn storage_used(state: &Arc<AppState>, actor: [u8; 32]) -> i64 {
    state
        .db
        .list_users()
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.actor_id == actor.to_vec())
        .map(|u| u.storage_bytes_used)
        .unwrap_or(0)
}

#[tokio::test]
async fn writer_member_records_reader_refused_owner_pays_cap_and_floor_real_router() {
    let (router, state) = router_and_state().await;
    // Real keys: every recorder signs its records (`signature_required`).
    let (owner_kp, writer_kp, reader_kp, outsider_kp) = (
        common::signing_actor(0xa1),
        common::signing_actor(0xb2),
        common::signing_actor(0xc3),
        common::signing_actor(0xee),
    );
    let owner = owner_kp.actor_id().0;
    let writer = writer_kp.actor_id().0;
    let reader = reader_kp.actor_id().0;
    let outsider = outsider_kp.actor_id().0;
    for (actor, label) in [(owner, "owner"), (writer, "writer"), (reader, "reader")] {
        state.db.create_user(&actor, "free", label).await.unwrap();
    }

    // Owner creates + shares "rw-docs", granting WRITER at share time (D5: the
    // grant rides the share wire, recorded with the bind).
    create_rw_docs(&state, &owner).await;
    let group_id = vec![0x6bu8; 24];
    let channel_id = ChannelId::from_group_id(&group_id).0;
    let share: FolderShareReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.folders.share",
            enc(&fauna_protocol::folders::addressed(FolderShareRequest {
                name: "rw-docs".into(),
                group_id: hex::encode(&group_id),
                member_actor_id: Some(hex::encode(writer)),
                access: Some("writer".into()),
                ..Default::default()
            })),
        )
        .await
        .expect("share ok"),
    )
    .unwrap();
    assert!(share.ok);

    // Both members join the roster via the real Welcome path; the reader gets
    // no grant (absent role row = reader, the fail-safe default).
    for member in [writer, reader] {
        // Mode gate (direct-messages.md § Reach policy): these arrangement
        // welcomes ride the Group kind, so open the recipient's inbox.
        state.db.set_inbox_mode(&member, "open").await.unwrap();
        dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.conversations.welcome.deliver",
            enc(&WelcomeDeliverRequest {
                recipient_actor_id: hex::encode(member),
                channel_id: hex::encode(channel_id),
                welcome_bytes: vec![0x01, 0x02, 0x03],
                kind: WelcomeKind::Folder {
                    group_id: hex::encode(&group_id),
                },
                nest_url: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("welcome deliver ok");
    }

    // (a) The owner's roster read projects the grants.
    let listed: fauna_protocol::folders::ActorMembersListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.folders.members.list_actors",
            enc(&fauna_protocol::folders::addressed(
                fauna_protocol::folders::ActorMembersListRequest {
                    name: "rw-docs".into(),
                    ..Default::default()
                },
            )),
        )
        .await
        .expect("list_actors ok"),
    )
    .unwrap();
    let row = |actor: [u8; 32]| {
        listed
            .members
            .iter()
            .find(|m| m.actor_id == hex::encode(actor))
            .expect("rostered")
            .clone()
    };
    assert_eq!(row(writer).access.as_deref(), Some("writer"));
    assert_eq!(row(reader).access.as_deref(), Some("reader"));
    assert_eq!(row(owner).access, None, "owner row carries no grant");

    // Devices for everyone (the record handler's device write-capability gate).
    let (dev_o, dev_w, dev_r) = ([0x0au8; 32], [0x0bu8; 32], [0x0cu8; 32]);
    register_device(&router, &state, owner, dev_o).await;
    register_device(&router, &state, writer, dev_w).await;
    register_device(&router, &state, reader, dev_r).await;
    // The outsider registers a device too — proving the refusal below is the
    // folder gate, not the device gate.
    register_device(&router, &state, outsider, [0x0du8; 32]).await;

    // Genesis envelope publish, floor = generation 1 (KMH § M2).
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.folders.content_key.put",
        enc(&fauna_protocol::folders::addressed(
            fauna_protocol::folders::ContentKeyPutRequest {
                name: "rw-docs".into(),
                epoch: 1,
                sealed: "ab".repeat(48),
                current_version: 1,
                ..Default::default()
            },
        )),
    )
    .await
    .expect("envelope publish ok");

    // (b/handler half) The WRITER records — lands, charges the OWNER.
    let before = storage_used(&state, owner).await;
    let rec: fauna_protocol::sync::SyncChangeRecordReply = decode(
        &dispatch(
            &router,
            state.clone(),
            writer,
            "fauna.sync.changes.record",
            record_req(&writer_kp, dev_w, "notes/a.txt", 600, 0xA1, Some(1)),
        )
        .await
        .expect("writer record lands"),
    )
    .unwrap();
    assert!(rec.seq > 0);
    assert_eq!(
        storage_used(&state, owner).await,
        before + 600,
        "owner-pays: the writer's bytes charge the OWNER"
    );
    assert_eq!(
        storage_used(&state, writer).await,
        0,
        "the writer's own quota is untouched"
    );

    // The READER (absent grant) and the OUTSIDER are both refused with the
    // same not_found (ST-RES-1 — the write plane leaks no set existence).
    for (who, dev, label) in [
        (&reader_kp, dev_r, "reader"),
        (&outsider_kp, [0x0du8; 32], "outsider"),
    ] {
        let err = dispatch(
            &router,
            state.clone(),
            who.actor_id().0,
            "fauna.sync.changes.record",
            record_req(who, dev, "notes/b.txt", 100, 0xB1, Some(1)),
        )
        .await
        .expect_err(label);
        assert_eq!(err.code, "fauna.sync.not_found", "{label}");
    }

    // (d) Byte cap: cap the writer at 1000 → a 600-byte record refuses typed
    // `member_cap_exceeded` and charges NOTHING.
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.folders.members.set_access",
        enc(&fauna_protocol::folders::MemberSetAccessRequest {
            name: "rw-docs".into(),
            actor_id: hex::encode(writer),
            access: "writer".into(),
            byte_cap: Some(1000),
            ..Default::default()
        }),
    )
    .await
    .expect("cap set");
    let before = storage_used(&state, owner).await;
    let err = dispatch(
        &router,
        state.clone(),
        writer,
        "fauna.sync.changes.record",
        record_req(&writer_kp, dev_w, "notes/big.bin", 600, 0xC1, Some(1)),
    )
    .await
    .expect_err("cap refusal");
    assert_eq!(err.code, "fauna.sync.member_cap_exceeded");
    assert_eq!(
        storage_used(&state, owner).await,
        before,
        "a cap refusal charges nothing"
    );

    // (f) Version floor: rotation publishes generation 3 → a writer record
    // stamped below it (or unstamped) refuses typed `stale_content_key`; a
    // fresh stamp lands; the OWNER records unstamped fine (exempt).
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.folders.content_key.put",
        enc(&fauna_protocol::folders::addressed(
            fauna_protocol::folders::ContentKeyPutRequest {
                name: "rw-docs".into(),
                epoch: 2,
                sealed: "cd".repeat(48),
                current_version: 3,
                ..Default::default()
            },
        )),
    )
    .await
    .expect("rotated envelope publish ok");
    for (stamp, label) in [(Some(1), "stale stamp"), (None, "absent stamp")] {
        let err = dispatch(
            &router,
            state.clone(),
            writer,
            "fauna.sync.changes.record",
            record_req(&writer_kp, dev_w, "notes/c.txt", 10, 0xD1, stamp),
        )
        .await
        .expect_err(label);
        assert_eq!(err.code, "fauna.sync.stale_content_key", "{label}");
    }
    dispatch(
        &router,
        state.clone(),
        writer,
        "fauna.sync.changes.record",
        record_req(&writer_kp, dev_w, "notes/c.txt", 10, 0xD1, Some(3)),
    )
    .await
    .expect("a fresh stamp lands");
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.sync.changes.record",
        record_req(&owner_kp, dev_o, "notes/owner.txt", 10, 0xE1, None),
    )
    .await
    .expect("the owner is floor-exempt (their drain self-corrects)");

    // Owner-only kinds stay owner-only for members: supersede refuses.
    let err = dispatch(
        &router,
        state.clone(),
        writer,
        "fauna.sync.changes.supersede",
        enc(&fauna_protocol::sync::SyncChangesSupersedeRequest {
            folder: "rw-docs".into(),
            device_id: hex::encode(dev_w),
            path: "notes/a.txt".into(),
            manifest_hash: hex::encode([0xA1u8; 32]),
            ..Default::default()
        }),
    )
    .await
    .expect_err("supersede stays owner-only");
    assert_eq!(err.code, "fauna.sync.not_found");

    // (c/handler half) Attribution: the member feed carries the nest-stamped
    // recorder on every row — the writer's rows say the writer, the owner's
    // say the owner; nothing is client-asserted.
    let feed: fauna_protocol::sync::SyncChangesListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            reader,
            "fauna.sync.changes.list",
            enc(&fauna_protocol::sync::SyncChangesListRequest {
                folder: Some("rw-docs".into()),
                device_id: None,
                since: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("member feed read"),
    )
    .unwrap();
    let author_of = |path: &str| {
        feed.changes
            .iter()
            .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash(path)))
            .and_then(|c| c.author_actor_id.clone())
    };
    assert_eq!(author_of("notes/a.txt"), Some(hex::encode(writer)));
    assert_eq!(author_of("notes/owner.txt"), Some(hex::encode(owner)));

    // Demotion (writer → reader, no rotation) revokes the write gate at once.
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.folders.members.set_access",
        enc(&fauna_protocol::folders::MemberSetAccessRequest {
            name: "rw-docs".into(),
            actor_id: hex::encode(writer),
            access: "reader".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("demote");
    let err = dispatch(
        &router,
        state.clone(),
        writer,
        "fauna.sync.changes.record",
        record_req(&writer_kp, dev_w, "notes/d.txt", 10, 0xF1, Some(3)),
    )
    .await
    .expect_err("demoted writer refused");
    assert_eq!(err.code, "fauna.sync.not_found");
}

/// Success (e), record-half — the **eviction** corollary (distinct from the
/// demotion case pinned above). `members.evict` DELETES the writer's
/// `folder_member_access` row (`members_evict_core` →
/// `delete_folder_member_role`), so the evictee falls to the fail-safe reader
/// default and the `writable_folder` gate refuses their next `changes.record`
/// with the same set-existence-hiding `not_found`. Where the demotion test
/// leaves a `reader` row, this leaves NO row — the two revoke paths a writer can
/// hit (owner sets access→reader; owner evicts from the roster) must both close
/// the write gate at once. Pure router (in-process dispatch); the crypto side of
/// (e) — the evictee can't decrypt post-rotation content — is pinned separately
/// by `removed_member_reads_pre_removal_but_fails_closed_post_removal_real_nest`.
#[tokio::test]
async fn evicted_writer_member_cannot_record_role_row_deleted_flips_to_reader_real_router() {
    let (router, state) = router_and_state().await;
    // Real keys: the writer signs its records (`signature_required`).
    let owner = common::signing_actor(0xa1).actor_id().0;
    let writer_kp = common::signing_actor(0xb2);
    let writer = writer_kp.actor_id().0;
    for (actor, label) in [(owner, "owner"), (writer, "writer")] {
        state.db.create_user(&actor, "free", label).await.unwrap();
    }

    // Owner creates + shares "rw-docs", granting the member WRITER at share time.
    create_rw_docs(&state, &owner).await;
    let group_id = vec![0x6bu8; 24];
    let channel_id = ChannelId::from_group_id(&group_id).0;
    let share: FolderShareReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.folders.share",
            enc(&fauna_protocol::folders::addressed(FolderShareRequest {
                name: "rw-docs".into(),
                group_id: hex::encode(&group_id),
                member_actor_id: Some(hex::encode(writer)),
                access: Some("writer".into()),
                ..Default::default()
            })),
        )
        .await
        .expect("share ok"),
    )
    .unwrap();
    assert!(share.ok);

    // The writer joins the roster via the real Welcome path.
    // Mode gate (direct-messages.md § Reach policy): these arrangement
    // welcomes ride the Group kind, so open the recipient's inbox.
    state.db.set_inbox_mode(&writer, "open").await.unwrap();
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.conversations.welcome.deliver",
        enc(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(writer),
            channel_id: hex::encode(channel_id),
            welcome_bytes: vec![0x01, 0x02, 0x03],
            kind: WelcomeKind::Folder {
                group_id: hex::encode(&group_id),
            },
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("welcome deliver ok");

    let dev_w = [0x0bu8; 32];
    register_device(&router, &state, writer, dev_w).await;
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.folders.content_key.put",
        enc(&fauna_protocol::folders::addressed(
            fauna_protocol::folders::ContentKeyPutRequest {
                name: "rw-docs".into(),
                epoch: 1,
                sealed: "ab".repeat(48),
                current_version: 1,
                ..Default::default()
            },
        )),
    )
    .await
    .expect("envelope publish ok");

    // Baseline: while a writer, the member records fine (the gate admits them).
    dispatch(
        &router,
        state.clone(),
        writer,
        "fauna.sync.changes.record",
        record_req(&writer_kp, dev_w, "notes/a.txt", 100, 0xA1, Some(1)),
    )
    .await
    .expect("writer records before eviction");

    // The owner EVICTS the writer from the roster — the role row is deleted.
    let evict: fauna_protocol::folders::MemberEvictReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.folders.members.evict",
            enc(&fauna_protocol::folders::MemberEvictRequest {
                name: "rw-docs".into(),
                member: hex::encode(writer),
                ..Default::default()
            }),
        )
        .await
        .expect("evict ok"),
    )
    .unwrap();
    assert!(evict.ok && evict.evicted, "the writer was on the roster");

    // With no role row (absent ⇒ reader, the fail-safe), the evictee's next
    // record is refused with the set-existence-hiding `not_found` — same as a
    // reader, an outsider, or a demoted writer.
    let err = dispatch(
        &router,
        state.clone(),
        writer,
        "fauna.sync.changes.record",
        record_req(&writer_kp, dev_w, "notes/b.txt", 100, 0xB1, Some(1)),
    )
    .await
    .expect_err("evicted writer refused");
    assert_eq!(err.code, "fauna.sync.not_found");
}

/// The upload lease is HOLDER-SCOPED on release. Before the
/// fix, Phase 1 widened `lease.release` to writer members but the DELETE scoped
/// only on `folder_id`, so any writer could holder-blind-drop the owner's (or
/// another member's) ACTIVE lease and grab it (acquire is holder-checked;
/// release was not). After the fix: a non-owner with NO `device_id` is refused;
/// the owner's device_id-less force-release still works; a device_id scopes the
/// release to that device only. (Refutable finding, verify owed back to
/// the security review; the literal contract fix is implemented — a residual
/// remains that a writer naming the owner's *known* device_id could target it,
/// bounded to the same auto-resolved-conflict class, documented in the
/// verify-back.)
#[tokio::test]
async fn lease_release_is_holder_scoped_writer_cannot_drop_owners_lease() {
    let (router, state) = router_and_state().await;
    let owner = [0xa1u8; 32];
    let writer = [0xb2u8; 32];
    for (actor, label) in [(owner, "owner"), (writer, "writer")] {
        state.db.create_user(&actor, "free", label).await.unwrap();
    }

    // Owner creates + shares "rw-docs", granting the writer at share time.
    state.db.create_folder("rw-docs", &owner).await.unwrap();
    let group_id = vec![0x6bu8; 24];
    let channel_id = ChannelId::from_group_id(&group_id).0;
    let share: FolderShareReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.folders.share",
            enc(&fauna_protocol::folders::addressed(FolderShareRequest {
                name: "rw-docs".into(),
                group_id: hex::encode(&group_id),
                member_actor_id: Some(hex::encode(writer)),
                access: Some("writer".into()),
                ..Default::default()
            })),
        )
        .await
        .expect("share ok"),
    )
    .unwrap();
    assert!(share.ok);
    // Mode gate (direct-messages.md § Reach policy): these arrangement
    // welcomes ride the Group kind, so open the recipient's inbox.
    state.db.set_inbox_mode(&writer, "open").await.unwrap();
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.conversations.welcome.deliver",
        enc(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(writer),
            channel_id: hex::encode(channel_id),
            welcome_bytes: vec![0x01, 0x02, 0x03],
            kind: WelcomeKind::Folder {
                group_id: hex::encode(&group_id),
            },
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("welcome deliver ok");

    let (dev_o, dev_w) = ([0x0au8; 32], [0x0bu8; 32]);
    let acquire = |actor: [u8; 32], dev: [u8; 32]| {
        let router = &router;
        let state = &state;
        async move {
            dispatch(
                router,
                state.clone(),
                actor,
                "fauna.folders.lease.acquire",
                enc(&LeaseAcquireRequest {
                    name: "rw-docs".into(),
                    device_id: hex::encode(dev),
                    ..Default::default()
                }),
            )
            .await
        }
    };

    // The owner's device holds the lease.
    let acq: LeaseAcquireReply =
        decode(&acquire(owner, dev_o).await.expect("owner acquires")).unwrap();
    assert!(acq.acquired);

    // ── The hole: a holder-blind release (no device_id) is MALFORMED for
    // everyone — the field is required since the compat-remnant sweep retired
    // the device-id-less clear (2026-09-24), so a writer cannot even ask to
    // drop the owner's active lease. ──
    #[derive(serde::Serialize)]
    struct DeviceIdLessRelease {
        name: String,
    }
    let err = dispatch(
        &router,
        state.clone(),
        writer,
        "fauna.folders.lease.release",
        enc(&DeviceIdLessRelease {
            name: "rw-docs".into(),
        }),
    )
    .await
    .expect_err("a device-id-less release is malformed");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // The owner's lease survived: the writer's own device still can't acquire.
    let contended = acquire(writer, dev_w).await.expect_err("owner still holds");
    assert_eq!(contended.code, "fauna.folders.conflict");

    // A scoped release naming a NON-holding device is a harmless no-op — the
    // owner's lease survives (the writer naming dev_w drops nothing).
    let rel: LeaseReleaseReply = decode(
        &dispatch(
            &router,
            state.clone(),
            writer,
            "fauna.folders.lease.release",
            enc(&LeaseReleaseRequest {
                name: "rw-docs".into(),
                device_id: hex::encode(dev_w),
                ..Default::default()
            }),
        )
        .await
        .expect("scoped release returns ok"),
    )
    .unwrap();
    assert!(rel.released);
    let still_held = acquire(writer, dev_w).await.expect_err("owner STILL holds");
    assert_eq!(still_held.code, "fauna.folders.conflict");

    // The owner, too, must name its holding device: the device-id-less form
    // is malformed for the owner as well — there is no force-release arm; a
    // crash-stuck lease lapses on the nest's TTL takeover.
    let err = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.folders.lease.release",
        enc(&DeviceIdLessRelease {
            name: "rw-docs".into(),
        }),
    )
    .await
    .expect_err("the owner's device-id-less release is malformed too");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // The owner releases its own device's lease, which frees it for the writer.
    let rel: LeaseReleaseReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.folders.lease.release",
            enc(&LeaseReleaseRequest {
                name: "rw-docs".into(),
                device_id: hex::encode(dev_o),
                ..Default::default()
            }),
        )
        .await
        .expect("owner releases its own lease"),
    )
    .unwrap();
    assert!(rel.released);
    let acq: LeaseAcquireReply = decode(
        &acquire(writer, dev_w)
            .await
            .expect("writer acquires after release"),
    )
    .unwrap();
    assert!(acq.acquired, "the freed lease is now available");
}

/// Task 9 capstone (Success b + c): TWO same-nest actors, each driving a REAL
/// `SyncEngine`, round-trip a shared-set edit **both ways** over the real gated
/// write plane. A owns "shared" and grants B `writer` at share time; B binds +
/// uploads a file whose bytes land in the owner's replica (the `writable_folder`
/// gate admits B, owner-pays meters A, and A opens B's chunks sealed under the
/// shared content key from B's own custody), and an owner edit round-trips back to
/// B. B's version is nest-stamped to B (author attribution). This is the
/// engine-level twin of `writer_member_records_reader_refused_…_real_router` (which
/// proves the gate/metering/floor at the router level with fake manifests) — here
/// the seal/open actually happens cross-actor over the wiremock chunk plane.
#[tokio::test]
async fn writer_member_and_owner_round_trip_edits_both_ways_two_engines() {
    let (router, state) = control_plane();
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let chunk_url = server.uri();
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);

    // ── A + B form a real 2-member MLS group ────────────────────────────────────
    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine = MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap();
    let b_kp_bytes = b_engine.generate_key_packages_bytes(1).unwrap();
    let created: CreatedGroup = FolderGroupCrypto::create_group(&a_engine, &b_kp_bytes).unwrap();
    let channel_id = created.channel_id;
    let b_joined = b_engine.join_from_welcome_bytes(&created.welcome).unwrap();
    assert_eq!(b_joined.0, channel_id, "B joins the same derived channel");

    // ── A owns "shared" and shares it, granting B WRITER at share time (D5) ──────
    state
        .db
        .create_folder_with_options("shared", &a_actor, shared_set_options())
        .await
        .unwrap();
    let a_files = FoldersClient::new(requester(&router, &state, a_actor));
    let share_reply = a_files
        .share(FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&created.raw_group_id),
            member_actor_id: Some(hex::encode(b_actor)),
            access: Some("writer".into()),
            ..Default::default()
        })
        .await
        .expect("share ok");
    assert!(share_reply.ok);

    // A binds the genesis content key + publishes the envelope.
    let a_author = FoldersAuthor::new(
        FoldersClient::new(requester(&router, &state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    );
    a_author
        .bind_set("shared", channel_id)
        .await
        .expect("bind_set");

    // B joins the roster via the real Welcome flow.
    // Mode gate (direct-messages.md § Reach policy): these arrangement
    // welcomes ride the Group kind, so open the recipient's inbox.
    state.db.set_inbox_mode(&b_actor, "open").await.unwrap();
    dispatch(
        router.as_ref(),
        state.clone(),
        a_actor,
        "fauna.conversations.welcome.deliver",
        enc(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(b_actor),
            channel_id: hex::encode(channel_id),
            welcome_bytes: created.welcome.clone(),
            kind: WelcomeKind::Group {
                group_id: hex::encode(&created.raw_group_id),
            },
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("welcome deliver ok");

    // Both open gen-1 keys; B ingests them into its OWN custody (Phase 0 read leg)
    // — the writer seals from the same custody source the owner does.
    let a_keys = open_envelope(&a_files, &a_engine, channel_id).await;
    let b_files = FoldersClient::new(requester(&router, &state, b_actor));
    let b_keys_env = open_envelope(&b_files, &b_engine, channel_id).await;
    let mut b_cfg = fauna_core::data::FoldersConfig::default();
    assert!(
        custody::merge_received_keys(&mut b_cfg, channel_id, b_keys_env.clone()),
        "first ingest populates the writer's custody"
    );
    let b_keys = custody::content_keys(&b_cfg, &channel_id).expect("B holds gen-1 content keys");

    // Devices for both (the record handler's device write-capability gate keys on
    // the connection actor, so each actor's own registered device satisfies it).
    let a_sync = signing_sync(&router, &state, A_SECRET);
    a_sync
        .register(hex::encode(DEVICE_A), "a-laptop", None)
        .await
        .expect("register A device");
    let b_sync = signing_sync(&router, &state, B_SECRET);
    let dev_b = [0x0Bu8; 32];
    b_sync
        .register(hex::encode(dev_b), "b-laptop", None)
        .await
        .expect("register B device");

    // ── WRITER → OWNER: B uploads a file; it lands and charges the OWNER ─────────
    let b_bytes = payload(41);
    let watch_b = tempfile::tempdir().unwrap();
    write_file(watch_b.path(), "from-b.bin", &b_bytes);
    let b_engine_w = build_bound_engine(
        &chunk_url,
        B_SECRET,
        dev_b,
        watch_b.path(),
        created.raw_group_id.clone(),
        b_keys.clone(),
    );
    b_engine_w
        .upload_file("from-b.bin")
        .await
        .expect("B (writer) uploads its chunks");
    let b_manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("B's manifest posted");
    let owner_before = storage_used(&state, a_actor).await;
    b_sync
        .changes_record(
            "shared",
            hex::encode(dev_b),
            "from-b.bin",
            Some(hex::encode(b_manifest.digest())),
            b_bytes.len() as i64,
            "Created",
            Some(1),
            None,
            Some(b"e2e-synthetic-seal".to_vec()),
            None,
            None,
        )
        .await
        .expect("WRITER records — the writable_folder gate admits B");
    assert_eq!(
        storage_used(&state, a_actor).await,
        owner_before + b_bytes.len() as i64,
        "owner-pays: a writer's bytes charge the OWNER's quota, not the writer's"
    );

    // A reads B's file back — sealed by B, opened by A under the shared content key.
    let a_listed = a_sync
        .changes_list(Some("shared".into()), None, 0)
        .await
        .expect("A lists");
    let b_change = a_listed
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash("from-b.bin")))
        .expect("A sees B's change");
    assert_eq!(
        b_change.author_actor_id.as_deref(),
        Some(hex::encode(b_actor).as_str()),
        "the version is nest-stamped to B (author attribution, Success c)"
    );
    let watch_a = tempfile::tempdir().unwrap();
    let a_engine_r = build_bound_engine(
        &chunk_url,
        A_SECRET,
        DEVICE_A,
        watch_a.path(),
        created.raw_group_id.clone(),
        a_keys.clone(),
    );
    let a_read = a_engine_r
        .download_file_bytes_by_manifest(b_manifest, b_change.content_key_version, "from-b.bin")
        .await
        .expect("owner reads the writer's content");
    assert_eq!(
        a_read, b_bytes,
        "the writer's edit round-trips into the OWNER's replica (Success b →)"
    );

    // ── OWNER → WRITER: A uploads; B reads it back (the reverse direction) ───────
    let a_bytes = payload(53);
    write_file(watch_a.path(), "from-a.bin", &a_bytes);
    a_engine_r
        .upload_file("from-a.bin")
        .await
        .expect("A uploads its chunks");
    let a_manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("A's manifest posted");
    a_sync
        .changes_record(
            "shared",
            hex::encode(DEVICE_A),
            "from-a.bin",
            Some(hex::encode(a_manifest.digest())),
            a_bytes.len() as i64,
            "Created",
            Some(1),
            None,
            Some(b"e2e-synthetic-seal".to_vec()),
            None,
            None,
        )
        .await
        .expect("owner records");
    let b_listed = b_sync
        .changes_list(Some("shared".into()), None, 0)
        .await
        .expect("B lists");
    let a_change = b_listed
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash("from-a.bin")))
        .expect("B sees A's change");
    let b_read = b_engine_w
        .download_file_bytes_by_manifest(a_manifest, a_change.content_key_version, "from-a.bin")
        .await
        .expect("writer reads the owner's content");
    assert_eq!(
        b_read, a_bytes,
        "the owner's edit round-trips back to the WRITER (Success b ←)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// D3 / D4 writer-engine LIVE pins. Unlike the tests
// above (which drive the control plane in-process via `RouterRequester`, so a
// writer engine's OWN `changes.record`/`changes.supersede` never reach the
// router), these serve the same control plane over a **real WS listener** and
// give the writer engine a *connected* `NestClient`. Its re-seal path then
// drives `fauna.sync.changes.record` (lands — writable_folder gate) and
// `fauna.sync.changes.supersede` (refused — owner-only gate) against the genuine
// nest, so the D3 best-effort-supersede tolerance (`engine.rs` re-seal `Err`
// arm) and the D4 floor heal are pinned LIVE, not structurally. The byte plane
// stays on the opaque wiremock chunk store (chunks are ciphertext).
// ─────────────────────────────────────────────────────────────────────────────

/// [`control_plane`] + a real WS listener, so a `NestClient::connect()` can drive
/// the sync control plane end-to-end. The returned `router` is the SAME `Arc` the
/// served app dispatches through (`state.rpc_router`), so the in-process
/// `RouterRequester` setup path and the writer engine's WS control plane share
/// one handler set + one `AppState`. Returns `(router, state, http_base)`.
async fn served_control_plane() -> (StdArc<RpcRouter>, StdArc<AppState>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());

    let db = StdArc::new(CacheDb::open_in_memory().unwrap());
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never deleted under test
    let backup_svc =
        StdArc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());

    // One router for both the in-process setup path and the WS-served path
    // (`routes.rs` dispatches WS RPC through `state.rpc_router`).
    let router = StdArc::new({
        let mut b = RpcRouter::builder();
        fauna_nest::auth_handlers::register_auth_handlers(&mut b); // WS connect/auth
        fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
        sync_handlers::register_sync_handlers(&mut b);
        folder_handlers::register_folders_handlers(&mut b);
        conversations_handlers::register_conversations_handlers(&mut b);
        fauna_nest::mls_replica_handlers::register_mls_replica_handlers(&mut b);
        b.build()
    });

    let state = StdArc::new(AppState {
        nest_identity: StdArc::new(fauna_nest::nest_identity::NestIdentity::generate()),
        rpc_router: router.clone(),
        auth: fauna_nest::state::AuthState {
            token_store: StdArc::new(fauna_nest::token_store::TokenStore::new()),
            registration: fauna_nest::routes::RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        // Records must LAND (the writer's re-record is the point); the tier
        // ceiling is not what these tests exercise, so don't enforce it.
        enforce_tier_quotas: StdArc::new(tokio::sync::RwLock::new(false)),
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (router, state, format!("http://{authority}"))
}

/// [`build_bound_engine`] but with a caller-supplied **connected** control-plane
/// `NestClient` (so the engine's own `changes.record`/`changes.supersede` reach
/// the served router) while the byte plane stays on the wiremock `chunk_url`.
#[allow(clippy::too_many_arguments)]
fn build_writer_engine_connected(
    chunk_url: &str,
    nest_client: StdArc<fauna_client::NestClient>,
    secret: [u8; 32],
    device_id: [u8; 32],
    watch: &std::path::Path,
    raw_group_id: Vec<u8>,
    content_keys: FolderContentKeys,
) -> SyncEngine {
    let http_bearer: StdArc<dyn BearerSource> =
        StdArc::new(StaticBearer("test.bearer".to_string()));
    let http_auth = StdArc::new(fauna_client::AuthClient::with_bearer_source(
        chunk_url.to_string(),
        ActorKeypair::from_secret(secret),
        http_bearer,
        reqwest::Client::new(),
    ));
    let engine_client = HttpSyncClient::new(http_auth, &device_id);
    let engine = SyncEngine::new(
        watch.to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some("shared".to_string()),
        device_id,
        None, // mls — the chunk root is `content_keys`, not the engine MLS
        None, // epoch_secret
        None, // backup_key — a bound shared set carries none (Q4)
        Some(raw_group_id),
        Some(content_keys),
        ConflictPolicy::Auto,
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(StdArc::new(AdaptiveConcurrency::fixed(4)), None),
        nest_client,
        fauna_sync_engine::config::SyncMode::Sync,
    );
    // A writer engine signs every record it sends, directly under the
    // actor's own key and the fixture set nonce (`signature_required`).
    engine.set_change_signer(
        Some(common::direct_signer(&ActorKeypair::from_secret(secret))),
        Some(common::SET_NONCE),
    );
    engine
}

/// The shared D3/D4 setup: A + B form a real MLS group; A owns "shared", shares
/// it granting B **writer** at share time, binds the genesis content key, and
/// delivers B's Welcome; B ingests the gen-1 keys into its own custody (the
/// writer seals from the same source the owner does); both register a device.
/// Returns everything the pins need. The `MockServer` + served nest outlive the
/// test (spawned / `mem::forget`ed inside their helpers).
struct WriterPlane {
    _server: MockServer,
    store: fauna_sync_engine::test_support::BlobStore,
    chunk_url: String,
    router: StdArc<RpcRouter>,
    state: StdArc<AppState>,
    base: String,
    raw_group_id: Vec<u8>,
    a_actor: [u8; 32],
    b_actor: [u8; 32],
    dev_b: [u8; 32],
    b_keys: FolderContentKeys,
}

async fn served_writer_plane() -> WriterPlane {
    let (router, state, base) = served_control_plane().await;
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let chunk_url = server.uri();
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);

    // A + B form a real 2-member MLS group.
    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine = MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap();
    let b_kp_bytes = b_engine.generate_key_packages_bytes(1).unwrap();
    let created: CreatedGroup = FolderGroupCrypto::create_group(&a_engine, &b_kp_bytes).unwrap();
    let channel_id = created.channel_id;
    assert_eq!(
        b_engine
            .join_from_welcome_bytes(&created.welcome)
            .unwrap()
            .0,
        channel_id
    );

    state
        .db
        .create_folder_with_options("shared", &a_actor, shared_set_options())
        .await
        .unwrap();
    let a_files = FoldersClient::new(requester(&router, &state, a_actor));
    a_files
        .share(FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&created.raw_group_id),
            member_actor_id: Some(hex::encode(b_actor)),
            access: Some("writer".into()),
            ..Default::default()
        })
        .await
        .expect("share (writer grant) ok");
    FoldersAuthor::new(
        FoldersClient::new(requester(&router, &state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    )
    .bind_set("shared", channel_id)
    .await
    .expect("bind_set");

    // Mode gate (direct-messages.md § Reach policy): these arrangement
    // welcomes ride the Group kind, so open the recipient's inbox.
    state.db.set_inbox_mode(&b_actor, "open").await.unwrap();
    dispatch(
        router.as_ref(),
        state.clone(),
        a_actor,
        "fauna.conversations.welcome.deliver",
        enc(&WelcomeDeliverRequest {
            recipient_actor_id: hex::encode(b_actor),
            channel_id: hex::encode(channel_id),
            welcome_bytes: created.welcome.clone(),
            kind: WelcomeKind::Group {
                group_id: hex::encode(&created.raw_group_id),
            },
            nest_url: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("welcome deliver ok");

    // B ingests the gen-1 content keys into its own custody.
    let b_files = FoldersClient::new(requester(&router, &state, b_actor));
    let b_keys_env = open_envelope(&b_files, &b_engine, channel_id).await;
    let mut b_cfg = fauna_core::data::FoldersConfig::default();
    assert!(custody::merge_received_keys(
        &mut b_cfg, channel_id, b_keys_env
    ));
    let b_keys = custody::content_keys(&b_cfg, &channel_id).expect("B holds gen-1 keys");

    // Devices (the record handler's write-capability gate keys on the connection
    // actor; each actor registers its own).
    let a_sync = CtlSyncClient::new(requester(&router, &state, a_actor));
    a_sync
        .register(hex::encode(DEVICE_A), "a-laptop", None)
        .await
        .expect("register A device");
    let dev_b = [0x0Bu8; 32];
    CtlSyncClient::new(requester(&router, &state, b_actor))
        .register(hex::encode(dev_b), "b-laptop", None)
        .await
        .expect("register B device");

    WriterPlane {
        _server: server,
        store,
        chunk_url,
        router,
        state,
        base,
        raw_group_id: created.raw_group_id,
        a_actor,
        b_actor,
        dev_b,
        b_keys,
    }
}

/// Stage a genuinely *pending* upload of `rel` at the writer's current
/// generation: an UNCONNECTED-control-plane engine uploads the chunks to the
/// wiremock store and POSTs the manifest (its record silently no-ops — a pure
/// pending upload), and we learn the ciphertext store keys from the manifest so
/// the caller can re-enqueue them on a rotated engine. Returns the store keys.
async fn stage_pending_upload(
    w: &WriterPlane,
    watch: &std::path::Path,
    rel: &str,
    bytes: &[u8],
    keys: FolderContentKeys,
) -> Vec<ContentHash> {
    write_file(watch, rel, bytes);
    let pending = build_bound_engine(
        &w.chunk_url,
        B_SECRET,
        w.dev_b,
        watch,
        w.raw_group_id.clone(),
        keys,
    );
    pending
        .upload_file(rel)
        .await
        .expect("stage: chunks + manifest posted");
    let manifest_hash = w
        .store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("manifest posted");
    let manifest_bytes = w
        .store
        .manifests
        .lock()
        .unwrap()
        .get(&hex::encode(manifest_hash.digest()))
        .cloned()
        .expect("manifest present in store");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).expect("decode manifest");
    manifest.store_keys()
}

/// D3: a writer crosses a content-key rotation with a **pending
/// upload** and the drain must NOT wedge on the owner-only `changes.supersede`
/// refusal. A gen-1 pending upload is staged; the rotation lands (gen-2 current,
/// gen-1 retained); the writer's *connected* engine drains → the path is
/// requeued under gen-2 and re-recorded via `changes.record` (lands — the
/// writable_folder gate admits B), then the re-seal best-effort-supersedes,
/// which the owner-only gate refuses with a genuine `not_found`. The drain
/// returning `Ok` IS the tolerance (the re-seal `Err` arm swallowed the refusal
/// rather than propagating it); the landed gen-2 record proves the record leg
/// completed (so the supersede was genuinely reached and refused, not skipped).
#[tokio::test]
async fn writer_drain_across_rotation_tolerates_owner_only_supersede_refusal_two_plane() {
    let w = served_writer_plane().await;
    let watch = tempfile::tempdir().unwrap();

    // Stage a pending gen-1 upload (its record no-ops — pure pending upload).
    let store_keys =
        stage_pending_upload(&w, watch.path(), "wip.bin", &payload(41), w.b_keys.clone()).await;
    assert!(
        !store_keys.is_empty(),
        "the staged upload enqueued ≥1 chunk"
    );

    // The rotation lands: gen-2 is current, gen-1 retained (history-on-join). The
    // removed member holds gen-1 irrevocably, so the pending gen-1 chunks must
    // NOT be republished — the drain requeues the path under gen-2 instead.
    w.store.chunks.lock().unwrap().clear();
    let mut gen2_keys = w.b_keys.clone();
    gen2_keys.rotate([0x9u8; 32], 2_000);
    assert_eq!(gen2_keys.current_version(), 2);

    // The writer's engine with a CONNECTED control plane + the rotated custody.
    let b_nest = common::connected_client(&w.base, ActorKeypair::from_secret(B_SECRET)).await;
    let rotated = build_writer_engine_connected(
        &w.chunk_url,
        b_nest,
        B_SECRET,
        w.dev_b,
        watch.path(),
        w.raw_group_id.clone(),
        gen2_keys,
    );
    for sk in &store_keys {
        rotated
            .db()
            .enqueue_transfer("wip.bin", "upload", *sk, 0)
            .expect("enqueue stale gen-1 entry");
    }

    // THE PIN: the drain drives a real `changes.record` (lands) then a real
    // owner-only `changes.supersede` (refused `not_found`) — and returns Ok. If
    // the re-seal propagated the supersede refusal, this would be `Err`.
    rotated
        .drain_pending_uploads()
        .await
        .expect("the writer engine tolerates the owner-only supersede refusal (does not wedge)");

    // The requeue re-recorded the path under the CURRENT (gen-2) generation, and
    // it LANDED (proving the record leg ran → the supersede was reached, then
    // refused-and-tolerated, not skipped). Owner-authored read confirms it.
    let a_sync = CtlSyncClient::new(requester(&w.router, &w.state, w.a_actor));
    let listed = a_sync
        .changes_list(Some("shared".into()), None, 0)
        .await
        .expect("owner lists");
    let rec = listed
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash("wip.bin")))
        .expect("the requeued record landed on the nest");
    assert_eq!(
        rec.content_key_version,
        Some(2),
        "re-recorded under the current (gen-2) generation, not the stale gen-1"
    );
    assert_eq!(
        rec.author_actor_id.as_deref(),
        Some(hex::encode(w.b_actor).as_str()),
        "nest-stamped to the writer (author attribution)"
    );

    // The stale gen-1 queue entries were completed only after the requeue landed.
    assert!(
        rotated
            .db()
            .eligible_transfers("upload")
            .unwrap()
            .is_empty(),
        "stale gen-1 entries are completed once the requeue lands"
    );
}

/// D4 (heal-half): a writer record stamped BELOW the
/// nest's content-key floor is refused typed `stale_content_key`, and after a
/// re-ingest advances the writer's `current` to meet the floor, the ordinary
/// drain re-seals + re-records under it and the record LANDS. The refusal alone
/// is pinned router-level in `writer_member_records_reader_refused_…`; this pins
/// the HEAL live over a connected writer engine. The owner publishes generation
/// 2 (floor := 2); a direct gen-1 record is refused; then the writer re-ingests
/// gen-2 and a pending gen-1 upload is requeued under gen-2 and recorded past the
/// floor (`2 >= floor(2)`) — the record that was stale is now durable.
#[tokio::test]
async fn writer_below_floor_record_refused_then_heals_after_reingest_two_plane() {
    let w = served_writer_plane().await;
    let watch = tempfile::tempdir().unwrap();

    // The owner rotates + publishes generation 2 → the nest floor is now 2.
    dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.a_actor,
        "fauna.folders.content_key.put",
        enc(&fauna_protocol::folders::addressed(
            fauna_protocol::folders::ContentKeyPutRequest {
                name: "shared".into(),
                epoch: 2,
                sealed: "cd".repeat(48),
                current_version: 2,
                ..Default::default()
            },
        )),
    )
    .await
    .expect("gen-2 envelope publish (floor := 2)");

    // BEFORE: the writer, still holding only gen-1, records below the floor → the
    // nest refuses it typed `stale_content_key` (a member is not floor-exempt).
    let err = dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.b_actor,
        "fauna.sync.changes.record",
        enc(&fauna_protocol::folders::addressed(common::signed_record(
            fauna_protocol::sync::SyncChangeRecordRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                nest_url: None,
                channel_id: None,
                folder: "shared".into(),
                device_id: hex::encode(w.dev_b),
                path: "heal.bin".into(),
                change_type: "create".into(),
                size_bytes: 100,
                manifest_hash: Some(hex::encode([0xD1u8; 32])),
                content_key_version: Some(1),
                thumbnail_hash: None,
                ..Default::default()
            },
            &ActorKeypair::from_secret(B_SECRET),
        ))),
    )
    .await
    .expect_err("below-floor writer record refused");
    assert_eq!(err.code, "fauna.sync.stale_content_key");

    // Stage a pending gen-1 upload of the same path (its record no-ops).
    let store_keys =
        stage_pending_upload(&w, watch.path(), "heal.bin", &payload(53), w.b_keys.clone()).await;
    w.store.chunks.lock().unwrap().clear();

    // HEAL: the writer re-ingests generation 2 (models the rotation-commit poll
    // advancing `current`); its connected engine drains → requeues heal.bin under
    // gen-2 and re-records it — now `2 >= floor(2)`, so the record LANDS.
    let mut gen2_keys = w.b_keys.clone();
    gen2_keys.rotate([0x9u8; 32], 2_000);
    let b_nest = common::connected_client(&w.base, ActorKeypair::from_secret(B_SECRET)).await;
    let healed = build_writer_engine_connected(
        &w.chunk_url,
        b_nest,
        B_SECRET,
        w.dev_b,
        watch.path(),
        w.raw_group_id.clone(),
        gen2_keys,
    );
    for sk in &store_keys {
        healed
            .db()
            .enqueue_transfer("heal.bin", "upload", *sk, 0)
            .expect("enqueue stale gen-1 entry");
    }
    healed
        .drain_pending_uploads()
        .await
        .expect("drain re-records under the advanced current");

    // The healed record landed at the advanced current, meeting the floor.
    let a_sync = CtlSyncClient::new(requester(&w.router, &w.state, w.a_actor));
    let listed = a_sync
        .changes_list(Some("shared".into()), None, 0)
        .await
        .expect("owner lists");
    let rec = listed
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash("heal.bin")))
        .expect("the healed record landed past the floor");
    assert_eq!(
        rec.content_key_version,
        Some(2),
        "recorded at the advanced current (gen-2), meeting the floor"
    );
    assert_eq!(
        rec.author_actor_id.as_deref(),
        Some(hex::encode(w.b_actor).as_str()),
        "nest-stamped to the writer (author attribution)"
    );
}

/// Row 29 (found 2026-08-21 by the member-seat e2e): the content-key floor has
/// **no public-audience arm**, so a writer MEMBER can never declassify.
///
/// The floor gate (`sync_handlers::record_change_core`) asks
/// `content_key_version.is_some_and(|v| v as i64 >= floor)` and lives inside the
/// `member_channel` arm — member writers only, never the owner. A DECLASSIFIED
/// record is plaintext and carries `content_key_version: None`, which can never
/// satisfy `>= floor`. Consequence before the fix: on a bound folder the owner
/// published, every MEMBER-authored file stays sealed at rest forever — the
/// owner's own files declassify fine, so the site serves in part and silently
/// not in part.
///
/// The gate predates the class: it landed 2026-07-19, a month
/// before phase 4 gave the seal sites their ratified plaintext arm and gave the
/// S9 path-seal exemption its `audience='public'` arm at all six record sites.
/// This is the seventh site.
///
/// Why the exemption is not a hole, and what this test pins around it:
/// - it is **unstamped-only** — a member record STAMPED BELOW the floor is
///   still refused on a public folder (the rotation race KMH § M2 describes is
///   about a record sealed under a superseded key; that record still exists);
/// - it is **public-only** — on a `shared`-audience folder an unstamped record
///   still fails closed, the non-conforming-caller arm the gate was written for;
/// - removal is enforced at the ROLE gate, not here
///   (`resolve_writable_folder` requires roster membership AND an explicit
///   `writer` role row; evict deletes it), so no removed member gains a write.
///
/// Contract: `docs/goal/behavior/folders.md` § Target re-model — "audience rides
/// **both** projection arms … a member's engine takes the plaintext arm off it",
/// and the flip-back "converges on **every seat, the members' included**".
#[tokio::test]
async fn public_audience_exempts_a_member_unstamped_record_from_the_content_key_floor() {
    let w = served_writer_plane().await;

    // The owner rotates + publishes generation 2 → the nest floor is now 2.
    dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.a_actor,
        "fauna.folders.content_key.put",
        enc(&fauna_protocol::folders::addressed(
            fauna_protocol::folders::ContentKeyPutRequest {
                name: "shared".into(),
                epoch: 2,
                sealed: "cd".repeat(48),
                current_version: 2,
                ..Default::default()
            },
        )),
    )
    .await
    .expect("gen-2 envelope publish (floor := 2)");

    // BEFORE the flip — the folder is `shared`, so an UNSTAMPED member record
    // fails closed exactly as the gate was written to: a non-conforming
    // client cannot prove freshness. This arm must survive the fix.
    let err = dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.b_actor,
        "fauna.sync.changes.record",
        enc(&fauna_protocol::folders::addressed(common::signed_record(
            fauna_protocol::sync::SyncChangeRecordRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "shared".into(),
                device_id: hex::encode(w.dev_b),
                path: "sealed-era.bin".into(),
                change_type: "create".into(),
                size_bytes: 100,
                manifest_hash: Some(hex::encode([0xD2u8; 32])),
                content_key_version: None,
                ..Default::default()
            },
            &ActorKeypair::from_secret(B_SECRET),
        ))),
    )
    .await
    .expect_err("unstamped member record on a SHARED folder still fails closed");
    assert_eq!(
        err.code, "fauna.sync.stale_content_key",
        "the non-conforming-caller fail-closed arm is untouched by the public exemption"
    );

    // The owner DECLASSIFIES the bound folder (phase 4: →`public` from any
    // audience; the nest records the flip, the corpus moves client-side).
    dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.a_actor,
        "fauna.folders.update",
        enc(&fauna_protocol::folders::addressed(
            fauna_protocol::folders::FolderUpdateRequest {
                name: "shared".into(),
                audience: Some("public".into()),
                ..Default::default()
            },
        )),
    )
    .await
    .expect("owner declassifies the bound folder");

    // THE SUBJECT: a member's declassified record is plaintext — no content key
    // to stamp, and (S9's public arm) no sealed path either. It must LAND.
    dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.b_actor,
        "fauna.sync.changes.record",
        enc(&fauna_protocol::folders::addressed(common::signed_record(
            fauna_protocol::sync::SyncChangeRecordRequest {
                path_sealed: None,
                folder: "shared".into(),
                device_id: hex::encode(w.dev_b),
                path: "member-note.html".into(),
                change_type: "create".into(),
                size_bytes: 30,
                manifest_hash: Some(hex::encode([0xD3u8; 32])),
                content_key_version: None,
                ..Default::default()
            },
            &ActorKeypair::from_secret(B_SECRET),
        ))),
    )
    .await
    .expect("a member's PLAINTEXT record on a public folder passes the floor");

    // It rests as the public shape: plaintext path, no content-key stamp.
    let a_sync = CtlSyncClient::new(requester(&w.router, &w.state, w.a_actor));
    let listed = a_sync
        .changes_list(Some("shared".into()), None, 0)
        .await
        .expect("owner lists");
    let rec = listed
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash("member-note.html")))
        .expect("the member's declassified record landed");
    assert_eq!(
        rec.content_key_version, None,
        "a declassified record carries no content-key generation"
    );
    assert_eq!(
        rec.author_actor_id.as_deref(),
        Some(hex::encode(w.b_actor).as_str()),
        "nest-stamped to the writer member (author attribution)"
    );

    // GUARD: the exemption is for UNSTAMPED records only. A member record
    // STAMPED BELOW the floor is still refused on the very same public folder —
    // "no key at all" is the ratified plaintext shape, "a superseded key" is
    // the race the floor exists to stop, and public audience does not merge them.
    let err = dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.b_actor,
        "fauna.sync.changes.record",
        enc(&fauna_protocol::folders::addressed(common::signed_record(
            fauna_protocol::sync::SyncChangeRecordRequest {
                path_sealed: None,
                folder: "shared".into(),
                device_id: hex::encode(w.dev_b),
                path: "stale-sealed.bin".into(),
                change_type: "create".into(),
                size_bytes: 100,
                manifest_hash: Some(hex::encode([0xD4u8; 32])),
                content_key_version: Some(1),
                ..Default::default()
            },
            &ActorKeypair::from_secret(B_SECRET),
        ))),
    )
    .await
    .expect_err("a below-floor STAMPED member record is refused on a public folder too");
    assert_eq!(err.code, "fauna.sync.stale_content_key");
}

/// The SUCCESS arm, and the end-to-end pin for both of its defects: a
/// writer MEMBER's seat declassifies a bound folder the owner published, its
/// change records LAND, and only then does the corpus stamp itself converged.
///
/// This is the test the sync-engine crate structurally cannot host — its engines
/// run an unconnected `NestClient`, so no change record can land there and every
/// re-seal/declassify test in that module pins the refusal arm
/// (`download_file_bytes_test::expect_unrecorded`). Here the engine is connected
/// to the real router, so the record is genuinely gated by
/// `record_change_core`'s floor.
///
/// Non-vacuity, deliberately arranged: the owner publishes generation 2 **after**
/// the member's sealed upload, so the nest's content-key floor is 2 while the
/// member holds gen-1. A declassified record carries no generation at all, so
/// before the public-audience arm it was refused `stale_content_key` — and
/// before the pass learned to report that, `converge_corpus_to_audience` stamped
/// `corpus_audience` anyway and the `current == target` short-circuit made the
/// miss permanent. Revert either half and this test reds:
/// - without the nest's public arm, the declassify pass returns the "1 of 1
///   path(s) uploaded but their change records did not land" error;
/// - without the client's shortfall report, the pass returns `Ok(1)` while the
///   nest head still names the SEALED manifest — which the keyless read below
///   catches.
#[tokio::test]
async fn public_folder_member_declassify_converges_and_then_steadies() {
    let w = served_writer_plane().await;
    let watch = tempfile::tempdir().unwrap();

    // The member's own seat, connected to the real router and sealed-armed —
    // the ordinary posture of a writer on a `shared` folder.
    let b_nest = common::connected_client(&w.base, ActorKeypair::from_secret(B_SECRET)).await;
    let engine = build_writer_engine_connected(
        &w.chunk_url,
        b_nest,
        B_SECRET,
        w.dev_b,
        watch.path(),
        w.raw_group_id.clone(),
        w.b_keys.clone(),
    );

    // A member-authored file, uploaded + recorded SEALED under the member's own
    // custody copy of generation 1 (the writer seals from the same source the
    // owner does).
    let rel = "member-note.html";
    let original: Vec<u8> = (0..40_000u32)
        .map(|i| (i.wrapping_mul(37) % 251) as u8)
        .collect();
    write_file(watch.path(), rel, &original);
    engine
        .upload_file(rel)
        .await
        .expect("the member's sealed upload records under gen-1");
    let sealed_manifest_hash = w
        .store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("sealed manifest posted");

    // The owner rotates + publishes generation 2 → the nest floor is now 2,
    // ABOVE anything the member holds. This is what makes the declassify below
    // a real test of the floor's public arm rather than a walk past a gate that
    // was never armed.
    dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.a_actor,
        "fauna.folders.content_key.put",
        enc(&fauna_protocol::folders::addressed(
            fauna_protocol::folders::ContentKeyPutRequest {
                name: "shared".into(),
                epoch: 2,
                sealed: "ef".repeat(48),
                current_version: 2,
                ..Default::default()
            },
        )),
    )
    .await
    .expect("gen-2 envelope publish (floor := 2)");

    // The owner DECLASSIFIES the bound folder. The nest records the state flip
    // only; the corpus moves client-side, on every seat — "audience rides both
    // projection arms … a member's engine takes the plaintext arm off it".
    dispatch(
        w.router.as_ref(),
        w.state.clone(),
        w.a_actor,
        "fauna.folders.update",
        enc(&fauna_protocol::folders::addressed(
            fauna_protocol::folders::FolderUpdateRequest {
                name: "shared".into(),
                audience: Some("public".into()),
                ..Default::default()
            },
        )),
    )
    .await
    .expect("owner declassifies the bound folder");

    // The member's seat picks the audience up off the projection and converges.
    let engine = engine.with_public_audience(true);
    assert_eq!(
        engine
            .converge_corpus_to_audience()
            .await
            .expect("the member's declassify records past the floor and converges"),
        1,
        "the member's one file declassifies"
    );
    assert_eq!(
        engine.db().corpus_audience().unwrap().as_deref(),
        Some("plaintext"),
        "a genuinely clean pass DOES stamp the corpus converged"
    );

    // The nest head really moved: it names a new, UNSTAMPED manifest, authored
    // by the member — the record the floor used to refuse.
    let a_sync = CtlSyncClient::new(requester(&w.router, &w.state, w.a_actor));
    let listed = a_sync
        .changes_list(Some("shared".into()), None, 0)
        .await
        .expect("owner lists");
    let rec = listed
        .changes
        .iter()
        .filter(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash(rel)))
        .max_by_key(|c| c.seq)
        .expect("the member's declassified record is the head");
    assert_eq!(
        rec.content_key_version, None,
        "the head carries no content-key generation — it is plaintext at rest"
    );
    assert_ne!(
        rec.manifest_hash.as_deref(),
        Some(hex::encode(sealed_manifest_hash.digest()).as_str()),
        "the head is the NEW plaintext manifest, not the sealed one it replaced"
    );
    assert_eq!(
        rec.author_actor_id.as_deref(),
        Some(hex::encode(w.b_actor).as_str()),
        "nest-stamped to the writer member"
    );

    // …and the whole point of publishing: a stranger holding none of this
    // folder's key material opens the member's file. Before the fix this file
    // alone stayed sealed while the owner's own files served, so the site
    // published in part, silently.
    let declassified_hash = w
        .store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the declassify posted a manifest");
    let reader_watch = tempfile::tempdir().unwrap();
    let stranger = build_bound_engine(
        &w.chunk_url,
        B_SECRET,
        w.dev_b,
        reader_watch.path(),
        w.raw_group_id.clone(),
        // A generation this file was never sealed under: it opens the plaintext
        // manifest anyway, because a declassified manifest carries no
        // `stored_hashes` and there is nothing to decrypt.
        FolderContentKeys::genesis([0xEEu8; 32], 1),
    );
    let bytes = stranger
        .download_file_bytes_by_manifest(declassified_hash, None, rel)
        .await
        .expect("a reader with none of the folder's keys opens the declassified file");
    assert_eq!(bytes, original);

    // Steady state: the next pass short-circuits off the meta row alone — the
    // stamp is now TRUE, so the short-circuit is correct rather than a wedge.
    assert_eq!(
        engine.converge_corpus_to_audience().await.unwrap(),
        0,
        "a converged corpus costs one meta-row read"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// MULTI-MEMBER SHARE — the goal-doc contract that one shared set is bound to ONE
// MLS group whose members are N other users.
//
// `docs/goal/ui/folders.md` § Sharing states it four ways: a set is "bound to
// **one** MLS group whose members are *other users*"; `folder-member-item` is
// **indexed** ("one person the set is shared with"); `folder-shared-badge`
// renders "Shared · **N**"; and "**each** member has an `access` of reader or
// writer". Removing *a* member rotates the content key so *that* member cannot
// decrypt post-removal content — which only means anything if the other members
// keep reading.
//
// So the second share of an already-shared set must ADD the new person to the
// set's existing group, never re-bind the set to a fresh one: the roster
// (`members.list_actors`) and the read gate (`folder_authz`) are both
// projected over the set's *current* derived `ChannelId`, so a re-bind silently
// drops every earlier member — the exact "user always controls their data"
// breach a rotation is designed to make explicit and auditable.
// ─────────────────────────────────────────────────────────────────────────────

/// A [`RpcRequester`] that captures every `fauna.conversations.welcome.deliver`
/// payload the owner sends and then forwards it unchanged — so the test can join
/// the recipient engines from the **real** Welcome bytes `share_set` produced (its
/// `ShareOutcome` returns only the channel + inbox id). Everything else passes
/// straight through to the wrapped [`RouterRequester`].
struct CapturingRequester {
    inner: RouterRequester,
    welcomes: StdArc<std::sync::Mutex<Vec<WelcomeDeliverRequest>>>,
}
impl CapturingRequester {
    fn new(
        inner: RouterRequester,
        welcomes: StdArc<std::sync::Mutex<Vec<WelcomeDeliverRequest>>>,
    ) -> Self {
        Self { inner, welcomes }
    }
}
impl RpcRequester for CapturingRequester {
    type Error = LoopbackError;
    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        if kind == "fauna.conversations.welcome.deliver"
            && let Ok(bytes) = encode_canonical(&payload)
            && let Ok(req) = decode::<WelcomeDeliverRequest>(&bytes)
        {
            self.welcomes.lock().unwrap().push(req);
        }
        self.inner.request(kind, payload).await
    }
}

/// Share the same set with B, then with C: the set stays on ONE MLS group, both
/// members are rostered, and **each opens the content-key envelope through their
/// own engine at their own epoch** — B keeps reading after C is later evicted.
/// The GREEN proof of `mls-group-key-material.md` § M2 *Admitting a member* +
/// `ui/folders.md` § Sharing → *Adding the 2nd..Nth member*.
///
/// Before the fix this was RED (pinned `#[ignore]`d): `share_set` unconditionally
/// called `create_group`, so share #2 minted a fresh group and re-bound the set,
/// silently dropping B from the roster and the read gate — the
/// `docs/goal/principles.md` § *User always controls their data* breach the fix
/// closes (grants are revocable **and audited**; a silent revocation is exactly
/// what that forbids).
#[tokio::test]
async fn sharing_with_a_second_member_keeps_the_first_on_the_roster_real_nest() {
    let (router, state) = control_plane();
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);
    let c_actor = actor_of(C_SECRET);

    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap());
    let c_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(C_SECRET)).unwrap());

    // Both recipients publish a real KeyPackage — what the owner's share fetches
    // and admits; a fake would be consumed but fail to parse.
    for (label, actor, engine) in [("b", b_actor, &b_engine), ("c", c_actor, &c_engine)] {
        let kp = engine.generate_key_packages_bytes(1).unwrap();
        state
            .db
            .put_key_package(&format!("{label}-kp-0"), &actor, &kp[0], 0, u64::MAX / 2)
            .await
            .unwrap();
    }

    state
        .db
        .create_folder_with_options(
            "shared",
            &a_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Capture the Welcome bytes the owner delivers so B + C can join their own
    // engines from the real share flow (all three clients share one buffer).
    let welcomes = StdArc::new(std::sync::Mutex::new(Vec::new()));
    let a_author = FoldersAuthor::new(
        FoldersClient::new(CapturingRequester::new(
            requester(&router, &state, a_actor),
            welcomes.clone(),
        )),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    );
    let convs = fauna_client_conversations::ConversationsClient::new(CapturingRequester::new(
        requester(&router, &state, a_actor),
        welcomes.clone(),
    ));

    let welcome_for = |actor: [u8; 32]| -> Vec<u8> {
        welcomes
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.recipient_actor_id == hex::encode(actor))
            .unwrap_or_else(|| panic!("no Welcome captured for {}", hex::encode(actor)))
            .welcome_bytes
            .clone()
    };

    // ── Share #1 → B (first-binder) → B joins the fresh group ────────────────
    let first = a_author
        .share_set(&convs, "shared", ActorId(b_actor), None, None)
        .await
        .expect("share to B");
    let channel_id = first.channel_id;
    assert_eq!(
        b_engine
            .join_from_welcome_bytes(&welcome_for(b_actor))
            .unwrap()
            .0,
        channel_id,
        "B joins the set's group"
    );

    // ── Share #2 → C, on the SAME already-shared set → the ADD path ──────────
    let second = a_author
        .share_set(&convs, "shared", ActorId(c_actor), None, None)
        .await
        .expect("share to C");
    assert_eq!(
        c_engine
            .join_from_welcome_bytes(&welcome_for(c_actor))
            .unwrap()
            .0,
        second.channel_id,
        "C joins the SAME group (the Add Welcome rides the existing raw group id)"
    );

    // ── The three core observables the RED pin measured ──────────────────────
    let listed: fauna_protocol::folders::ActorMembersListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            a_actor,
            "fauna.folders.members.list_actors",
            enc(&fauna_protocol::folders::addressed(
                fauna_protocol::folders::ActorMembersListRequest {
                    name: "shared".into(),
                    ..Default::default()
                },
            )),
        )
        .await
        .expect("list_actors ok"),
    )
    .unwrap();
    let rostered: Vec<String> = listed.members.iter().map(|m| m.actor_id.clone()).collect();

    let b_files = FoldersClient::new(requester(&router, &state, b_actor));
    let b_reads = b_files
        .content_key_get(ContentKeyGetRequest {
            name: "shared".into(),
            ..Default::default()
        })
        .await;

    let one_group = second.channel_id == channel_id;
    let b_rostered = rostered.contains(&hex::encode(b_actor));
    let c_rostered = rostered.contains(&hex::encode(c_actor));
    assert!(
        one_group && b_rostered && c_rostered && b_reads.is_ok(),
        "a set is bound to ONE MLS group (folders.md § Sharing) — sharing with a \
         second person must ADD them to the set's existing group, not re-bind the \
         set to a fresh group whose roster excludes everyone shared with earlier.\n\
         \x20 set stayed on one group: {one_group} (first={}, second={})\n\
         \x20 B still rostered: {b_rostered}; C rostered: {c_rostered}; roster={rostered:?}\n\
         \x20 B still passes the content-key read gate: {} ({:?})",
        hex::encode(channel_id),
        hex::encode(second.channel_id),
        b_reads.is_ok(),
        b_reads.err(),
    );

    // ── B advances across the distributed Add commit; then B + C both open ───
    // B joined at the genesis epoch; the ungated Rule-1 Add distributed a commit
    // on the channel, which B's production folder commit poll applies to reach
    // the post-Add epoch. C joined the Add Welcome directly at that epoch.
    let b_backend = FaunaMlsBackend::new(
        b_engine.clone(),
        StdArc::new(LoopbackConvRpc(requester(&router, &state, b_actor)))
            as StdArc<dyn ConversationsRpc>,
        "bob",
        ActorId(b_actor),
    );
    let mut b_cursor = 0i64;
    let add_poll = poll_inbound_folder(&b_backend, &ChannelId(channel_id), &mut b_cursor, 0)
        .await
        .expect("B's Add-commit poll");
    assert_eq!(
        add_poll.applied, 1,
        "B applied the owner's Add commit (advancing B's epoch)"
    );
    assert!(!add_poll.stalled, "nothing to heal on this pass");

    let c_files = FoldersClient::new(requester(&router, &state, c_actor));
    let b_keys = open_envelope(&b_files, &b_engine, channel_id).await;
    let c_keys = open_envelope(&c_files, &c_engine, channel_id).await;
    assert_eq!(
        b_keys.current_version(),
        1,
        "no rotation on admit (history-on-join)"
    );
    assert_eq!(
        c_keys, b_keys,
        "both members reconstruct the identical key bundle at their shared post-Add epoch"
    );

    // ── A evicts C → rotate to gen 2; B stays and keeps reading; C is gated ──
    let evict = a_author
        .remove_member("shared", channel_id, ActorId(c_actor))
        .await
        .expect("evict C");
    assert!(
        evict.rotated && evict.evicted,
        "evicting C rotated the key + removed the roster row"
    );

    let remove_poll = poll_inbound_folder(&b_backend, &ChannelId(channel_id), &mut b_cursor, 0)
        .await
        .expect("B's Remove-commit poll");
    assert_eq!(
        remove_poll.applied, 1,
        "B applied the Remove commit (advancing past C's eviction)"
    );
    let b_keys_gen2 = open_envelope(&b_files, &b_engine, channel_id).await;
    assert_eq!(
        b_keys_gen2.current_version(),
        2,
        "B reads the post-eviction generation"
    );
    assert_eq!(
        b_keys_gen2.key_for(1),
        b_keys.key_for(1),
        "gen-1 retained for B (history-on-join)"
    );

    let c_reads_after = c_files
        .content_key_get(ContentKeyGetRequest {
            name: "shared".into(),
            ..Default::default()
        })
        .await;
    assert!(
        c_reads_after.is_err(),
        "the evicted member C is gated out of the read"
    );
}

/// **The data question the ⚠ block in `ui/folders.md` § Sharing left open**: does content uploaded *before* a second share stay readable
/// after it?
///
/// Under the old re-binding behavior the answer was **no, for the owner too**:
/// share #2 minted a fresh group, so the set's derived `ChannelId` changed and
/// `bind_set` recorded a *fresh* genesis key under the new channel. Content
/// sealed under the old channel's generation 1 was then read back against the new
/// channel's generation 1 — the same version *number*, a different key — so it
/// failed to decrypt for everyone, owner included.
///
/// The fix makes the question **moot by construction**: admitting a member never
/// changes the set's group, so the channel is stable and generation 1 keeps the
/// key that sealed the content (no rotation on admit — history-on-join). This
/// test pins exactly that, end-to-end through the real chunk route: bytes
/// uploaded before share #2 are still decryptable afterwards by the owner *and*
/// by the newly-admitted member, using the key resolved from the freshly-fetched
/// envelope (not a stashed copy).
#[tokio::test]
async fn content_uploaded_before_a_second_share_stays_readable_after_it_real_nest() {
    let (router, state) = control_plane();
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let chunk_url = server.uri();
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);
    let c_actor = actor_of(C_SECRET);

    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap());
    let c_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(C_SECRET)).unwrap());
    for (label, actor, engine) in [("b", b_actor, &b_engine), ("c", c_actor, &c_engine)] {
        let kp = engine.generate_key_packages_bytes(1).unwrap();
        state
            .db
            .put_key_package(&format!("{label}-kp-0"), &actor, &kp[0], 0, u64::MAX / 2)
            .await
            .unwrap();
    }
    state
        .db
        .create_folder_with_options("shared", &a_actor, shared_set_options())
        .await
        .unwrap();

    let welcomes = StdArc::new(std::sync::Mutex::new(Vec::new()));
    let a_author = FoldersAuthor::new(
        FoldersClient::new(CapturingRequester::new(
            requester(&router, &state, a_actor),
            welcomes.clone(),
        )),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    );
    let convs = fauna_client_conversations::ConversationsClient::new(CapturingRequester::new(
        requester(&router, &state, a_actor),
        welcomes.clone(),
    ));
    let welcome_for = |actor: [u8; 32]| -> Vec<u8> {
        welcomes
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.recipient_actor_id == hex::encode(actor))
            .unwrap_or_else(|| panic!("no Welcome captured for {}", hex::encode(actor)))
            .welcome_bytes
            .clone()
    };

    // ── Share #1 → B; the set binds and gets its genesis (gen-1) content key ──
    let first = a_author
        .share_set(&convs, "shared", ActorId(b_actor), None, None)
        .await
        .expect("share to B");
    let channel_id = first.channel_id;
    b_engine
        .join_from_welcome_bytes(&welcome_for(b_actor))
        .unwrap();
    let raw_group_id = a_engine
        .group_id_bytes(&ChannelId(channel_id))
        .expect("the owner holds the bound group");

    let a_files = FoldersClient::new(requester(&router, &state, a_actor));
    let keys_before = open_envelope(&a_files, &a_engine, channel_id).await;
    assert_eq!(keys_before.current_version(), 1, "genesis is generation 1");

    // ── A uploads content sealed under gen-1, BEFORE the second share ─────────
    let a_sync = signing_sync(&router, &state, A_SECRET);
    a_sync
        .register(hex::encode(DEVICE_A), "laptop", None)
        .await
        .expect("register A device");
    let pre_bytes = payload(29);
    let watch_a = tempfile::tempdir().unwrap();
    write_file(watch_a.path(), "pre.bin", &pre_bytes);
    let a_upload_engine = build_bound_engine(
        &chunk_url,
        A_SECRET,
        DEVICE_A,
        watch_a.path(),
        raw_group_id.clone(),
        keys_before.clone(),
    );
    a_upload_engine
        .upload_file("pre.bin")
        .await
        .expect("upload pre-share-#2 content");
    let pre_manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("manifest posted");
    a_sync
        .changes_record(
            "shared",
            hex::encode(DEVICE_A),
            "pre.bin",
            Some(hex::encode(pre_manifest.digest())),
            pre_bytes.len() as i64,
            "Created",
            Some(1),
            None,
            Some(b"e2e-synthetic-seal".to_vec()),
            None,
            None,
        )
        .await
        .expect("record the pre-share change");

    // ── Share #2 → C: the operation that used to re-bind the set ──────────────
    let second = a_author
        .share_set(&convs, "shared", ActorId(c_actor), None, None)
        .await
        .expect("share to C");
    c_engine
        .join_from_welcome_bytes(&welcome_for(c_actor))
        .unwrap();

    assert_eq!(
        second.channel_id, channel_id,
        "the set's channel is unchanged — the key-resolution address never moved"
    );

    // The owner re-fetches the envelope AFTER share #2 (not a stashed copy) and
    // still resolves the very key that sealed `pre.bin`.
    let keys_after = open_envelope(&a_files, &a_engine, channel_id).await;
    assert_eq!(
        keys_after.current_version(),
        1,
        "no rotation on admit — generation 1 is still current"
    );
    assert_eq!(
        keys_after.key_for(1),
        keys_before.key_for(1),
        "generation 1 still carries the key that sealed the pre-share content"
    );

    // ── The owner reads `pre.bin` back through the real chunk route ───────────
    let watch_a2 = tempfile::tempdir().unwrap();
    let a_read_engine = build_bound_engine(
        &chunk_url,
        A_SECRET,
        DEVICE_A,
        watch_a2.path(),
        raw_group_id.clone(),
        keys_after.clone(),
    );
    let owner_read = a_read_engine
        .download_file_bytes_by_manifest(pre_manifest, Some(1), "pre.bin")
        .await
        .expect("the OWNER still decrypts content uploaded before the second share");
    assert_eq!(
        owner_read, pre_bytes,
        "owner-side readability survives the second share (no data loss)"
    );

    // ── And the newly-admitted member reads it too (history-on-join) ──────────
    let c_files = FoldersClient::new(requester(&router, &state, c_actor));
    let c_keys = open_envelope(&c_files, &c_engine, channel_id).await;
    assert_eq!(
        c_keys.key_for(1),
        keys_before.key_for(1),
        "the newcomer's bundle carries generation 1 (history-on-join)"
    );
    let watch_c = tempfile::tempdir().unwrap();
    let c_read_engine = build_bound_engine(
        &chunk_url,
        C_SECRET,
        [0x0C; 32],
        watch_c.path(),
        raw_group_id,
        c_keys,
    );
    let member_read = c_read_engine
        .download_file_bytes_by_manifest(pre_manifest, Some(1), "pre.bin")
        .await
        .expect("the newly-admitted member decrypts pre-existing content");
    assert_eq!(
        member_read, pre_bytes,
        "a member admitted AFTER the upload reads it (history-on-join)"
    );
}

/// A declined share stops being a share: the recipient's roster row goes with the
/// decline, so the owner's "Shared with" list stops over-reporting **and** a later
/// re-share is a genuine re-invite (a fresh Welcome the recipient can actually
/// join), not the add path's no-op access-refresh arm.
///
/// The GREEN proof of `ui/folders.md` § Sharing → *Adding the 2nd..Nth member*,
/// the "**Declining an invitation removes you from the roster**" rule, and of the § *Recipient side* knock bullet's
/// matching sentence. Before this, a decline was a bare `ack`: the row lingered,
/// the owner's list lied, and — because the add path discriminates on that very
/// roster — the decliner was permanently un-re-invitable.
///
/// Drives the **shared** recipe every app runs
/// (`fauna_client_folders::decline_folder_share`), against the real router, so
/// the ordering guarantee and the roster effect are pinned together. The re-share
/// then exercises the ratified **ghost heal** (Q1(c)): B is still an MLS leaf in
/// A's group (declining never joins, but the owner's Add already placed the leaf),
/// so a re-share evicts the ghost and admits B fresh.
#[tokio::test]
async fn declining_a_share_drops_the_roster_row_and_a_re_share_re_invites_real_nest() {
    let (router, state) = control_plane();
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);

    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap());

    // TWO real KeyPackages: share #1 consumes one, and the post-decline re-share is
    // a genuine re-invite that mints a fresh Welcome from the second.
    let kps = b_engine.generate_key_packages_bytes(2).unwrap();
    for (i, kp) in kps.iter().enumerate() {
        state
            .db
            .put_key_package(&format!("b-kp-{i}"), &b_actor, kp, 0, u64::MAX / 2)
            .await
            .unwrap();
    }

    state
        .db
        .create_folder_with_options(
            "shared",
            &a_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let welcomes = StdArc::new(std::sync::Mutex::new(Vec::new()));
    let a_author = FoldersAuthor::new(
        FoldersClient::new(CapturingRequester::new(
            requester(&router, &state, a_actor),
            welcomes.clone(),
        )),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    );
    let convs = fauna_client_conversations::ConversationsClient::new(CapturingRequester::new(
        requester(&router, &state, a_actor),
        welcomes.clone(),
    ));

    let rostered = |actor: [u8; 32]| {
        let router = router.clone();
        let state = state.clone();
        async move {
            let listed: fauna_protocol::folders::ActorMembersListReply = decode(
                &dispatch(
                    &router,
                    state,
                    a_actor,
                    "fauna.folders.members.list_actors",
                    enc(&fauna_protocol::folders::addressed(
                        fauna_protocol::folders::ActorMembersListRequest {
                            name: "shared".into(),
                            ..Default::default()
                        },
                    )),
                )
                .await
                .expect("list_actors ok"),
            )
            .unwrap();
            listed
                .members
                .iter()
                .any(|m| m.actor_id == hex::encode(actor))
        }
    };

    // ── Share #1 → B. B is rostered at Welcome delivery, BEFORE they ever act ──
    let first = a_author
        .share_set(&convs, "shared", ActorId(b_actor), None, None)
        .await
        .expect("share to B");
    assert!(
        rostered(b_actor).await,
        "the share rosters B at Welcome delivery — the row a decline must remove"
    );
    let b_files = FoldersClient::new(requester(&router, &state, b_actor));
    assert!(
        b_files
            .content_key_get(ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            })
            .await
            .is_ok(),
        "rostered ⇒ the content-key read gate is open"
    );

    // ── B DECLINES through the shared client recipe ───────────────────────────
    let b_inbox = fauna_client_inbox::InboxClient::new(requester(&router, &state, b_actor));
    let pending = fauna_client_inbox::list_folder_pending_shares(
        &b_inbox,
        fauna_client_inbox::PENDING_SHARE_PEEK_LIMIT,
    )
    .await
    .expect("peek B's staged shares");
    assert_eq!(pending.len(), 1, "the share staged one knock in B's inbox");
    fauna_client_folders::decline_folder_share(&b_inbox, &b_files, pending[0].inbox_id)
        .await
        .expect("B declines");

    assert!(
        !rostered(b_actor).await,
        "declining removes you from the roster (folders.md § Sharing → Adding the \
         2nd..Nth member) — the owner's \"Shared with\" list must stop over-reporting"
    );
    assert!(
        b_files
            .content_key_get(ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            })
            .await
            .is_err(),
        "off the roster ⇒ the read gate closes, so a decliner stops receiving rotations"
    );
    assert!(
        fauna_client_inbox::list_folder_pending_shares(
            &b_inbox,
            fauna_client_inbox::PENDING_SHARE_PEEK_LIMIT,
        )
        .await
        .expect("re-peek")
        .is_empty(),
        "the decline also acked the staged Welcome — it is never joined"
    );

    // ── A re-shares → a GENUINE re-invite (the ghost heal, not a no-op refresh) ─
    let second = a_author
        .share_set(&convs, "shared", ActorId(b_actor), None, None)
        .await
        .expect("re-share to B after the decline");
    assert!(
        rostered(b_actor).await,
        "the re-share re-invites the decliner — the whole point of dropping the row"
    );

    let b_welcomes: Vec<Vec<u8>> = welcomes
        .lock()
        .unwrap()
        .iter()
        .filter(|w| w.recipient_actor_id == hex::encode(b_actor))
        .map(|w| w.welcome_bytes.clone())
        .collect();
    assert_eq!(
        b_welcomes.len(),
        2,
        "a re-share after a decline mints a FRESH Welcome (not the no-op \
         access-refresh arm, which delivers none)"
    );
    assert_ne!(
        b_welcomes[0], b_welcomes[1],
        "the second Welcome is genuinely new — B was evicted as a ghost leaf and \
         re-admitted from a fresh KeyPackage"
    );
    assert_eq!(
        b_engine.join_from_welcome_bytes(&b_welcomes[1]).unwrap().0,
        second.channel_id,
        "B joins the set's group from the re-invite Welcome"
    );
    assert!(
        b_files
            .content_key_get(ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            })
            .await
            .is_ok(),
        "re-admitted ⇒ the read gate is open again"
    );
    assert_eq!(
        second.channel_id, first.channel_id,
        "the set never left its own MLS group — a re-invite is an Add, not a re-bind"
    );
}

/// **The server half of the multi-member-share breach**.
///
/// The 2026-07-23 fix that made share #2..N an *Add* rather than a re-bind was
/// entirely client-library-side. The nest kept accepting the old gesture: an
/// owner-scoped `fauna.folders.share` naming a **fresh** group id for an
/// already-bound set claimed the brand-new channel and unconditionally
/// re-pointed `folders.mls_group_id` at it — and because the roster and the
/// read gate both project over the set's *current* derived `ChannelId`, every
/// earlier member silently dropped off a set nobody revoked.
///
/// That is reachable without any attacker: a non-conforming client — whose
/// `share_set` unconditionally mints a fresh group — can make this exact call
/// against the nest.
///
/// So the nest, which is the authority for the name→group binding, now enforces
/// the ratified target itself (`mls-group-key-material.md` § M2 *Admitting a
/// member*; `ui/folders.md` § Sharing → *Adding the 2nd..Nth member*: "never a
/// fresh group, never a re-bind"): a `share` whose `group_id` differs from the
/// set's stored binding is refused with a typed `already_bound`. A visible error
/// to a non-conforming caller beats a silent revocation — and the conforming
/// client never trips it, because its add path re-sends the *stored* id.
#[tokio::test]
async fn share_refuses_to_rebind_a_bound_set_to_a_different_group_real_nest() {
    let (router, state) = control_plane();
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);

    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap());

    // B publishes a real KeyPackage — what the owner's first share admits.
    let kp = b_engine.generate_key_packages_bytes(1).unwrap();
    state
        .db
        .put_key_package("b-kp-0", &b_actor, &kp[0], 0, u64::MAX / 2)
        .await
        .unwrap();

    state
        .db
        .create_folder_with_options(
            "shared",
            &a_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // ── Share #1 through the real author: the set binds, B lands on the roster ─
    let a_author = FoldersAuthor::new(
        FoldersClient::new(requester(&router, &state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    );
    let convs =
        fauna_client_conversations::ConversationsClient::new(requester(&router, &state, a_actor));
    let first = a_author
        .share_set(&convs, "shared", ActorId(b_actor), None, None)
        .await
        .expect("share to B");

    let bound_group = state
        .db
        .get_folder_for_actor_by_name_hash(
            &fauna_core::path_crypto::set_name_hash("shared"),
            &a_actor,
        )
        .await
        .unwrap()
        .unwrap()
        .mls_group_id
        .expect("the first share bound the set");

    // ── A non-conforming client's gesture: share again, naming a FRESH group ──
    // Byte-for-byte what `share_set_first_binder` sends — a client reaching
    // its unconditional `create_group` arm for the second time.
    let fresh_group = vec![0x5au8; 20];
    assert_ne!(fresh_group, bound_group, "the test's premise");
    let fresh_channel = ChannelId::from_group_id(&fresh_group).0;

    let err = dispatch(
        &router,
        state.clone(),
        a_actor,
        "fauna.folders.share",
        enc(&fauna_protocol::folders::addressed(FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&fresh_group),
            ..Default::default()
        })),
    )
    .await
    .expect_err(
        "a share naming a group other than the set's own must be refused — accepting \
         it re-points the set's ChannelId and silently drops every earlier member",
    );
    assert_eq!(
        err.code, "fauna.folders.already_bound",
        "the refusal is typed, so any caller can render it (got {err:?})"
    );

    // ── Nothing moved: the binding, the roster, and B's read all survive ──────
    assert_eq!(
        state
            .db
            .get_folder_for_actor_by_name_hash(
                &fauna_core::path_crypto::set_name_hash("shared"),
                &a_actor,
            )
            .await
            .unwrap()
            .unwrap()
            .mls_group_id,
        Some(bound_group.clone()),
        "the refused share left the set on its own group"
    );

    let listed: fauna_protocol::folders::ActorMembersListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            a_actor,
            "fauna.folders.members.list_actors",
            enc(&fauna_protocol::folders::addressed(
                fauna_protocol::folders::ActorMembersListRequest {
                    name: "shared".into(),
                    ..Default::default()
                },
            )),
        )
        .await
        .expect("list_actors ok"),
    )
    .unwrap();
    assert!(
        listed
            .members
            .iter()
            .any(|m| m.actor_id == hex::encode(b_actor)),
        "the first member is still rostered, roster={:?}",
        listed
            .members
            .iter()
            .map(|m| m.actor_id.clone())
            .collect::<Vec<_>>()
    );

    let b_files = FoldersClient::new(requester(&router, &state, b_actor));
    assert!(
        b_files
            .content_key_get(ContentKeyGetRequest {
                name: "shared".into(),
                ..Default::default()
            })
            .await
            .is_ok(),
        "the first member still passes the content-key read gate"
    );

    // The refusal lands BEFORE the namespace claim, so the rejected gesture
    // leaves no stray claim + no stray owner roster row on the fresh channel —
    // the channel stays claimable by whoever legitimately binds it later.
    assert!(
        !state
            .db
            .is_actor_in_channel(&a_actor, &fresh_channel)
            .await
            .unwrap(),
        "a refused share must not have registered the owner on the fresh channel"
    );

    // ── The built add path is untouched: re-sending the STORED id still works ─
    // This is the shape `share_set_add` sends (`ui/folders.md` § Sharing: "the
    // add path reuses `fauna.folders.share` unchanged — with the existing
    // group id"), so the guard must not break the idempotent claimant re-bind.
    let reply: FolderShareReply = decode(
        &dispatch(
            &router,
            state.clone(),
            a_actor,
            "fauna.folders.share",
            enc(&fauna_protocol::folders::addressed(FolderShareRequest {
                name: "shared".into(),
                group_id: hex::encode(&bound_group),
                ..Default::default()
            })),
        )
        .await
        .expect("re-sending the set's OWN group id is the add path's idempotent re-bind"),
    )
    .unwrap();
    assert!(reply.ok);
    assert_eq!(
        reply.channel_id,
        hex::encode(first.channel_id),
        "the idempotent re-bind echoes the set's unchanged channel"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The member-side **custody-changed observer** edge (residual R3 (account-data-plane.md § The ratified decisions)).
//
// `remaining_member_advances_epoch_via_distributed_remove_commit_real_nest`
// above proves the member's *custody* catches up across an owner rotation. That
// is only half of a member staying able to read the set: a bound set's content
// keys are final at engine build time (the sync agent's `engine_stamp` keys on
// `(mls_group_id, current generation)`), so something has to tell the app to
// re-push the resolved content-key blob or the running engine keeps the
// generation it was built with.
//
// The OWNER's own rotation has always had that trigger — the removal raises
// `DataMessage::FolderContentKeyRotated`, which the shell routes to
// `sync_agent::restart_engine_for_set` → `refresh_content_keys`. A MEMBER never
// sees that message: they receive the rotation on the conversations poll path
// (`poll_inbound_folder` → `maybe_ingest_folder_custody`), which persisted
// the new generation into the folder-keys plane entry and told nobody. The member's engine
// therefore kept its build-time generation and the owner's next upload — stamped
// N+1 — failed closed in `content_open_roots` until an app restart or a fresh
// bind: fail-closed and logged, not data loss, but the member silently stops
// receiving new content.
//
// This pins the seam: `NestFolderCustodySink::with_observer` fires exactly on
// an ingest that genuinely ADVANCED custody. (The desktop sync agent no longer
// needs an app to relay it: the ingest's plane write is the nudge the agent
// re-resolves on, and `engine_stamp_distinguishes_every_transition`
// (`bins/fauna-sync-agent/src/engine_driver.rs`) pins that a `Some(N)` →
// `Some(N+1)` generation move restarts the engine.)
//
// Authority: `mls-group-key-material.md` § M2 *Rotate-on-removal*.
// ─────────────────────────────────────────────────────────────────────────────

/// Records every `custody_changed` edge in order, so the pin can assert both
/// that the rotation fired one **and** that a quiet re-poll fires none.
#[derive(Default)]
struct RecordingCustodyObserver {
    fired: Mutex<Vec<[u8; 32]>>,
}

impl RecordingCustodyObserver {
    fn fired(&self) -> Vec<[u8; 32]> {
        self.fired.lock().unwrap().clone()
    }
}

impl fauna_client_folders::FolderCustodyObserver for RecordingCustodyObserver {
    fn custody_changed(&self, channel_id: &[u8; 32]) {
        self.fired.lock().unwrap().push(*channel_id);
    }
}

#[tokio::test]
async fn member_custody_ingest_notifies_the_observer_only_when_custody_actually_advances_real_nest()
{
    let (router, state, base) = served_control_plane().await;
    let a_actor = actor_of(A_SECRET);
    let b_actor = actor_of(B_SECRET);
    let c_actor = actor_of(C_SECRET);

    // ── A + B + C form a real 3-member MLS group ────────────────────────────────
    let a_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(A_SECRET)).unwrap());
    let b_engine = MlsEngine::new_in_memory(ActorKeypair::from_secret(B_SECRET)).unwrap();
    let c_engine =
        StdArc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(C_SECRET)).unwrap());
    let kp_bytes: Vec<Vec<u8>> = b_engine
        .generate_key_packages_bytes(1)
        .unwrap()
        .into_iter()
        .chain(c_engine.generate_key_packages_bytes(1).unwrap())
        .collect();
    let created: CreatedGroup = FolderGroupCrypto::create_group(&a_engine, &kp_bytes).unwrap();
    let channel_id = created.channel_id;
    assert_eq!(
        b_engine
            .join_from_welcome_bytes(&created.welcome)
            .unwrap()
            .0,
        channel_id
    );
    assert_eq!(
        c_engine
            .join_from_welcome_bytes(&created.welcome)
            .unwrap()
            .0,
        channel_id
    );

    // The production folder-Welcome join stamps the MLS-authenticated sender as
    // the channel's folder owner; the raw join above does not, so stamp it here —
    // the ingest lets only that recorded owner's signed envelope move custody.
    c_engine.mark_folder_channel_owner(&ChannelId(channel_id), &ActorId(a_actor));

    // ── A owns "shared", claims the channel, binds the genesis content key ──────
    state
        .db
        .create_folder_with_options(
            "shared",
            &a_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let a_files = FoldersClient::new(requester(&router, &state, a_actor));
    a_files
        .share(FolderShareRequest {
            name: "shared".into(),
            group_id: hex::encode(&created.raw_group_id),
            ..Default::default()
        })
        .await
        .expect("share ok");
    let a_author = FoldersAuthor::new(
        FoldersClient::new(requester(&router, &state, a_actor)),
        ActorKeypair::from_secret(A_SECRET),
        StdArc::new(MemoryFolderKeyStore::default()),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        a_engine.clone(),
    );
    a_author
        .bind_set("shared", channel_id)
        .await
        .expect("bind_set");

    // ── B + C join the roster via the REAL Welcome flow ─────────────────────────
    for member in [b_actor, c_actor] {
        // Mode gate (direct-messages.md § Reach policy): these arrangement
        // welcomes ride the Group kind, so open the recipient's inbox.
        state.db.set_inbox_mode(&member, "open").await.unwrap();
        dispatch(
            router.as_ref(),
            state.clone(),
            a_actor,
            "fauna.conversations.welcome.deliver",
            enc(&WelcomeDeliverRequest {
                recipient_actor_id: hex::encode(member),
                channel_id: hex::encode(channel_id),
                welcome_bytes: created.welcome.clone(),
                kind: WelcomeKind::Group {
                    group_id: hex::encode(&created.raw_group_id),
                },
                nest_url: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("welcome deliver ok");
    }

    // ── C's PRODUCTION member-side ingest seam over a real connected client ─────
    // `NestFolderCustodySink` is the one impl every native app injects, so
    // wiring the observer here is what gives linux/tui/apple/windows/android the
    // edge with no per-app crypto or bookkeeping.
    let observer = StdArc::new(RecordingCustodyObserver::default());
    // C's WS auth is a real `fauna.auth.*` handshake, so C must be a registered
    // user first — the in-process `RouterRequester` seeds that implicitly on its
    // first dispatch, but C's first traffic here is over the socket.
    common::seed_dispatch_actor(&state.db, &c_actor).await;
    let c_nest = common::connected_client(&base, ActorKeypair::from_secret(C_SECRET)).await;
    let c_custody = StdArc::new(MemoryFolderKeyStore::default());
    let c_sink = fauna_client_folders::NestFolderCustodySink::new(c_nest, c_custody.clone())
        .with_observer(observer.clone() as StdArc<dyn fauna_client_folders::FolderCustodyObserver>);
    let c_backend = StdArc::new(FaunaMlsBackend::new(
        c_engine.clone(),
        StdArc::new(LoopbackConvRpc(requester(&router, &state, c_actor)))
            as StdArc<dyn ConversationsRpc>,
        "carol",
        ActorId(c_actor),
    ));
    c_backend.set_folder_custody_sink(StdArc::new(c_sink));

    /// The generation the member's own custody store currently holds — read back
    /// through the store the sink writes, so the assertions describe persisted
    /// custody rather than the sink's in-memory bookkeeping.
    async fn held_generation(store: &MemoryFolderKeyStore, channel: &[u8; 32]) -> Option<u64> {
        let loaded = store.load().await.expect("C's custody loads");
        custody::content_keys(&loaded, channel).map(|k| k.current_version())
    }

    // ── Pass 1: the first ingest. Custody goes absent → gen-1: an ADVANCE. ──────
    let mut cursor = 0i64;
    let outcome = poll_inbound_folder(&c_backend, &ChannelId(channel_id), &mut cursor, 0)
        .await
        .expect("folder commit poll");
    assert_eq!(outcome.applied, 0, "no commit distributed yet");
    assert_eq!(
        held_generation(&c_custody, &channel_id).await,
        Some(1),
        "the first ingest persisted gen-1 into C's own custody"
    );
    assert_eq!(
        observer.fired(),
        vec![channel_id],
        "a first ingest is a custody ADVANCE — the observer must fire so the \
         member's engine is built keyed rather than unbound"
    );

    // ── Pass 2: a quiet re-poll. Nothing moved, so nothing may fire. ────────────
    // The negative half matters as much as the positive one: this edge drives a
    // full agent re-provision + engine restart, so a per-poll fire would restart
    // a member's engine on every cadence tick forever.
    let outcome = poll_inbound_folder(&c_backend, &ChannelId(channel_id), &mut cursor, 0)
        .await
        .expect("quiet re-poll");
    assert_eq!(outcome.applied, 0);
    assert_eq!(
        observer.fired(),
        vec![channel_id],
        "a quiet re-poll leaves custody untouched and must NOT fire the observer"
    );

    // ── A removes B: rotate-on-removal + the distributed Remove commit ──────────
    let out = a_author
        .remove_member("shared", channel_id, ActorId(b_actor))
        .await
        .expect("remove_member");
    assert!(out.rotated && out.evicted);

    // ── Pass 3: C applies the commit, re-ingests gen-2 — the R3 edge. ───────────
    let outcome = poll_inbound_folder(&c_backend, &ChannelId(channel_id), &mut cursor, 0)
        .await
        .expect("post-rotation commit poll");
    assert_eq!(
        outcome.applied, 1,
        "the owner's Remove commit advanced C's epoch"
    );
    assert_eq!(
        held_generation(&c_custody, &channel_id).await,
        Some(2),
        "C's custody advanced to the rotated generation"
    );
    assert_eq!(
        observer.fired(),
        vec![channel_id, channel_id],
        "THE R3 EDGE: an owner rotation ingested by a MEMBER must notify the app, \
         or the member's engine keeps its build-time generation and every later \
         owner upload fails closed until an app restart"
    );
}
