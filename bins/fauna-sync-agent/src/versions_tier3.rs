//! Tier_3: the shell extension's **Version-history + Restore** path exercised
//! against a REAL nest, headless (`docs/goal/behavior/file-sync.md` § File
//! Versions / § Restore).
//!
//! Until now the service's `versions.rs` was covered only by the in-memory
//! [`FakeRestoreNest`], which re-encodes its replies through the *same*
//! `SyncClient` request types it is handed — so it can never catch a divergence
//! between what `SyncClient::{versions_list, versions_get, restore_version}`
//! puts on the wire and what the running nest actually decodes. And the whole
//! `RestoreFileVersion` verb (Track V-D) had **never once run end-to-end**
//! against a real `fauna.files.versions.*` / `fauna.sync.changes.record`
//! projection — the exact class of untested-because-"needs-a-real-nest" gap that
//! cost three sessions on Track Y and a units bug on Track X.
//!
//! ## What is real here, and what is faked
//!
//! **Real:** the whole nest control plane. [`NestHarness`] stands up a real
//! `fauna-nest` `AppState` over an in-memory `CacheDb` and dispatches every
//! request straight into the nest's own registered handlers — the identical
//! tier_3 arrangement `bins/fauna-nest/tests/conformance_files_versions.rs`
//! uses. So the request bytes [`SyncClient`] produces are the bytes the nest
//! decodes, the version projection is the real one over `sync_changes`, and the
//! restore record really lands as a new head. The service code under test —
//! `versions::{list_file_versions, restore_file_version}` and the local
//! `pipe_server::repoint_entry` — is the shipped code, unchanged.
//!
//! **Faked:** nothing on the control plane. The one thing *not* exercised is the
//! cfapi **byte plane** — producing the versions via the literal on-demand
//! *upload* path and re-hydrating the restored file's *bytes* from its
//! re-pointed manifest. That needs a real nest **chunk store** (a much heavier
//! harness) and is captured as the follow-on Slice 2 (tracked internally).
//! Restore is metadata-only ("re-point, never
//! re-upload" — § Restore), so the control plane *is* the mechanism: a version
//! is a `sync_changes` row, and an upload's nest-facing effect is exactly the
//! `changes.record` this harness drives.
//!
//! ## Run
//!
//! Opt-in (pulls the whole `fauna-nest` crate — off the default test loop, like
//! tier_4):
//!
//! ```text
//! cargo-win.cmd test -p fauna-sync-agent --features tier3-nest versions_tier3
//! ```

use std::sync::Arc;

use fauna_client_sync::SyncClient;
use fauna_core::data::ContentHash;
use fauna_nest::{
    db::CacheDb, files_handlers, folder_handlers, routes::AppState, rpc_router::RpcRouter,
    sync_handlers,
};
use fauna_protocol::{
    RpcError, RpcErrorClass, RpcRequester,
    folders::{FolderCreateReply, FolderCreateRequest},
    sync::{
        SyncChangeRecordReply, SyncChangeRecordRequest, SyncRegisterReply, SyncRegisterRequest,
    },
};
use fauna_sync_engine::db::{SyncDb, SyncState};

use crate::config::{SyncConfig, SyncPaths};
use crate::state::SyncServiceState;
use crate::versions::{list_file_versions, restore_file_version};
use fauna_client_sync::row_judge::ReaderSeat;

/// The dispatching actor. Non-zero so it can hold a `CallerClass` once seeded.
const ACTOR: [u8; 32] = [11u8; 32];
const FOLDER: &str = "docs";
/// The bound set's identity: the agent keys its engine and state DB by it.
const FOLDER_REF: fauna_core::folder_keys::FolderRef = fauna_core::folder_keys::FolderRef::Local(1);
/// The folder-relative, forward-slash path — the `path_hash` preimage on both
/// the record and the query side (§ Path hashing).
const REL: &str = "report.txt";
/// A display path outside any real sync root: only its *relative* form (`REL`)
/// is ever hashed, so the absolute prefix is irrelevant to the nest.
const ABS_PATH: &str = r"C:\Sync\docs\report.txt";

/// An in-process nest error: either a real handler [`RpcError`] (a server
/// rejection — `Reply.ok = false`) or a codec/routing fault. Mirrors the
/// `RejectErr` shape `fauna-client-sync`'s own self-heal tests use, so the
/// shared `restore_version`'s `device_unregistered` retry classifies it
/// correctly. (That retry never fires here — the device is pre-registered — but
/// the `R::Error: RpcErrorClass` bound must be satisfiable.)
#[derive(Debug)]
enum InProcNestError {
    /// The request reached a handler and was refused.
    Rejected(RpcError),
    /// Encode / decode / routing fault — no clean reply was produced.
    Fault(String),
}

impl core::fmt::Display for InProcNestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rejected(e) => write!(f, "{}", e.code),
            Self::Fault(s) => write!(f, "{s}"),
        }
    }
}

impl RpcErrorClass for InProcNestError {
    fn is_rejection(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            Self::Rejected(e) => Some(e),
            Self::Fault(_) => None,
        }
    }
}

/// An [`RpcRequester`] backed by a REAL in-process nest: it encodes each request
/// exactly as the transport would, then calls the nest's own handler over a real
/// `AppState` + `CacheDb`. So the wire shapes it exercises are the running
/// nest's, not a fake's — which is the entire point of this harness.
struct InProcessNest {
    router: Arc<RpcRouter>,
    state: Arc<AppState>,
    actor: [u8; 32],
}

impl RpcRequester for InProcessNest {
    type Error = InProcNestError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        // Canonical dag-cbor → `Bytes` — the exact bytes the dispatcher sends.
        let bytes = fauna_protocol::encode_canonical(&payload)
            .map_err(|e| InProcNestError::Fault(format!("encode {kind}: {e}")))?;
        let meta = self
            .router
            .kind_meta(kind)
            .ok_or_else(|| InProcNestError::Fault(format!("kind not registered: {kind}")))?;
        let reply = (meta.handler)(Arc::clone(&self.state), self.actor, bytes)
            .await
            .map_err(InProcNestError::Rejected)?;
        fauna_protocol::decode_strict(&reply)
            .map_err(|e| InProcNestError::Fault(format!("decode {kind} reply: {e}")))
    }
}

/// A real in-process nest with a write-capable device + a `sync`-mode set —
/// every precondition the production record path needs.
struct NestHarness {
    router: Arc<RpcRouter>,
    state: Arc<AppState>,
    /// The hex device id the set was registered under; the restore record must
    /// carry it (a registered, write-capable device).
    device_id: String,
}

impl NestHarness {
    async fn new() -> Self {
        let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory CacheDb"));
        let state = Arc::new(AppState::for_test(db));
        let mut b = RpcRouter::builder();
        files_handlers::register_files_handlers(&mut b);
        // The production write path the version projection reads.
        folder_handlers::register_folders_handlers(&mut b);
        sync_handlers::register_sync_handlers(&mut b);
        let router = Arc::new(b.build());

        // Seed the dispatching actor's `users` row. Production reaches this
        // through `fauna.account.register` or the auth handshake's auto-provision
        // arm; a direct-dispatch test runs neither, and the central authority
        // gate resolves an actor with no `users` row to no `CallerClass` at all.
        if state.db.get_user(&ACTOR).await.expect("get_user").is_none() {
            state
                .db
                .create_user(&ACTOR, "free", "tier3-seeded")
                .await
                .expect("seed users row");
        }

        let h = Self {
            router,
            state,
            device_id: hex::encode([0xd1u8; 32]),
        };

        let _: SyncRegisterReply = h
            .dispatch(
                "fauna.sync.register",
                &SyncRegisterRequest {
                    device_id: h.device_id.clone(),
                    label: "tier3-test".into(),
                    capabilities: "read,write".into(),
                    ..Default::default()
                },
            )
            .await;
        let _: FolderCreateReply = h
            .dispatch(
                "fauna.folders.create",
                &FolderCreateRequest {
                    name: FOLDER.into(),
                    retention_policy: None,
                    ..Default::default()
                },
            )
            .await;

        h
    }

    /// Dispatch a request straight into a real nest handler (the setup path).
    async fn dispatch<Req, Reply>(&self, kind: &str, req: &Req) -> Reply
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let payload = fauna_protocol::encode_canonical(req).expect("encode request");
        let meta = self.router.kind_meta(kind).expect("kind registered");
        let reply = (meta.handler)(Arc::clone(&self.state), ACTOR, payload)
            .await
            .unwrap_or_else(|e| panic!("handler {kind} rejected: {}", e.code));
        fauna_protocol::decode_strict(&reply).expect("decode reply")
    }

    /// Record a change over the real `fauna.sync.changes.record` handler — an
    /// upload's nest-facing effect — and return its `seq` (= the version_num the
    /// projection exposes).
    async fn record(
        &self,
        manifest: [u8; 32],
        size: i64,
        change_type: &str,
        ckv: Option<u64>,
    ) -> i64 {
        let reply: SyncChangeRecordReply = self
            .dispatch(
                "fauna.sync.changes.record",
                &SyncChangeRecordRequest {
                    nest_url: None,
                    channel_id: None,
                    folder: FOLDER.into(),
                    device_id: self.device_id.clone(),
                    path: REL.into(),
                    manifest_hash: Some(hex::encode(manifest)),
                    size_bytes: size,
                    change_type: change_type.into(),
                    content_key_version: ckv,
                    thumbnail_hash: None,
                    ..Default::default()
                },
            )
            .await;
        reply.seq
    }

    /// A `SyncClient` over this nest — the exact generic surface the shell
    /// extension's Version-history verbs drive in production.
    fn sync(&self) -> SyncClient<InProcessNest> {
        SyncClient::new(InProcessNest {
            router: Arc::clone(&self.router),
            state: Arc::clone(&self.state),
            actor: ACTOR,
        })
    }
}

/// The read path: the service's own `list_file_versions` returns the nest's
/// `sync_changes` projection — oldest→newest, `version_num` = the recording
/// `seq`, never renumbered. Proves `SyncClient::versions_list`'s request +
/// reply shapes against the real handler (what `FakeRestoreNest` cannot).
#[tokio::test]
async fn list_file_versions_against_a_real_nest_returns_recorded_versions() {
    let h = NestHarness::new().await;
    let seq1 = h.record([0xa1; 32], 100, "create", None).await;
    let seq2 = h.record([0xa2; 32], 200, "modify", None).await;

    let sync = h.sync();
    let info = list_file_versions(&sync, ABS_PATH, FOLDER, REL, &ReaderSeat::default())
        .await
        .expect("list versions against a real nest");

    assert_eq!(info.path, ABS_PATH, "the display path is echoed verbatim");
    assert_eq!(info.folder, FOLDER);
    assert_eq!(
        info.versions.len(),
        2,
        "every recorded change is a listable version"
    );
    assert_eq!(info.versions[0].version_num, seq1);
    assert_eq!(info.versions[1].version_num, seq2);
    assert_eq!(info.versions[0].size_bytes, 100);
    assert_eq!(info.versions[1].size_bytes, 200);
}

/// **The flagship: Restore, end to end, against a real nest.**
///
/// Two versions are recorded (v1 sealed under content-key generation 5 — the
/// sealed-set edge). The service restores v1 through its real RPC path; the
/// returned [`RestoredVersion`](crate::versions::RestoredVersion) must carry v1's
/// `manifest_hash` / `size_bytes` / `content_key_version` **verbatim** (§ Restore
/// — re-point, never re-upload). Reading the new head back over
/// `versions.get` proves the restore really landed on the nest as a new,
/// listable version (append-only history — restore is reversible). Finally the
/// recording device re-points its **own local row** through the shipped
/// `pipe_server::repoint_entry`, so the next open serves the restored bytes
/// (§ Restore, *the recording device must re-point its own local copy*).
#[tokio::test]
async fn restore_end_to_end_against_a_real_nest() {
    // ── Arrange: two versions on a real nest. ──
    let h = NestHarness::new().await;
    let seq1 = h.record([0xa1; 32], 100, "create", Some(5)).await;
    let _seq2 = h.record([0xa2; 32], 200, "modify", Some(6)).await;

    let sync = h.sync();
    assert_eq!(
        list_file_versions(&sync, ABS_PATH, FOLDER, REL, &ReaderSeat::default())
            .await
            .unwrap()
            .versions
            .len(),
        2,
        "the file has ≥2 versions before restore — the precondition TRACK T made reachable"
    );

    // ── Act: the service restores v1 through its real RPC path. ──
    // `path_sealed: None` — this harness exercises the RPC path, not the
    // capability-derived mint (`pipe_server::handle_restore_file_version`),
    // which is where production mints the seal (S8 D2).
    let restored = restore_file_version(
        &sync,
        FOLDER,
        &h.device_id,
        REL,
        seq1,
        None,
        &ReaderSeat::default(),
        &crate::versions::NeverReseals,
    )
    .await
    .expect("restore version 1 against a real nest");

    // The historical metadata is carried VERBATIM (§ Restore).
    assert_eq!(
        restored.manifest_hash, [0xa1; 32],
        "restore re-points at v1's manifest, not a guess"
    );
    assert_eq!(restored.size_bytes, 100);
    assert_eq!(
        restored.content_key_version,
        Some(5),
        "the sealed-set generation is carried verbatim so the reader selects key_for(version)"
    );

    // ── Assert: the restore is a NEW head AND a new listable version. ──
    let after = list_file_versions(&sync, ABS_PATH, FOLDER, REL, &ReaderSeat::default())
        .await
        .unwrap();
    assert_eq!(
        after.versions.len(),
        3,
        "restore appends a version — history is append-only, so restore is itself reversible"
    );
    let head = after.versions.last().unwrap();
    assert_eq!(
        head.version_num, restored.recorded_seq,
        "the restore's own recording seq is the new head"
    );
    assert_eq!(head.size_bytes, 100, "the head now carries v1's size");

    // Reading the head back over `versions.get` proves it landed on the real
    // nest (not just in our RestoredVersion): its content IS v1's.
    let head_info = sync
        .versions_get(fauna_core::sync::path_hash(REL), restored.recorded_seq)
        .await
        .expect("versions.get the restored head");
    assert_eq!(
        head_info.manifest_hash.as_ref(),
        &[0xa1u8; 32],
        "the recorded head's manifest is v1's — restore = re-point"
    );
    assert_eq!(head_info.content_key_version, Some(5));

    // ── Assert: the recording device re-points its OWN local copy. ──
    // Catch-up skips a device's own changes, so recording alone would leave THIS
    // machine showing the pre-restore content (§ Restore).
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
    let (shutdown_tx, _srx) = tokio::sync::watch::channel(false);
    let (event_tx, _erx) = tokio::sync::broadcast::channel(64);
    let state = SyncServiceState::new(SyncConfig::default(), shutdown_tx, event_tx, paths.clone());

    // Seed this device's local row as the PRE-restore head (v2, hydrated).
    let db_path = paths.sync_db_path_for_ref(FOLDER_REF);
    std::fs::create_dir_all(db_path.parent().unwrap()).expect("create db dir");
    {
        let db = SyncDb::open(&db_path).expect("open per-folder db");
        db.upsert_entry(
            REL,
            None,
            None,
            Some(ContentHash::from_digest_raw([0xa2; 32])),
            SyncState::Synced,
            0,
            1_700_000_000,
            200,
            1,
            Some(6),
        )
        .expect("seed the pre-restore head row");
    }

    // Ref-less (pre-identity) binding: the name-keyed DB path this harness seeded
    // is exactly what `sync_db_path_for_ref(FOLDER_REF)` resolves to. The restore
    // recorded as the dispatching actor, so the re-point stamps the head as signed
    // by it — production passes the capability's actor id the same way.
    let repointed = crate::pipe_server::repoint_entry(
        &state,
        FOLDER_REF,
        REL,
        &restored,
        Some(ACTOR),
        fauna_core::data::Timestamp::now_secs_or_zero(),
    )
    .await
    .expect("re-point the local row");
    assert!(repointed, "there was a local copy to re-point");

    let entry = SyncDb::open(&db_path)
        .expect("reopen db")
        .get_entry(REL)
        .expect("get entry")
        .expect("the row survives the re-point");
    assert_eq!(
        entry.manifest_hash,
        Some(ContentHash::from_digest_raw(restored.manifest_hash)),
        "the local row now anchors on the restored version's manifest, so the next FETCH_DATA \
         serves v1's bytes"
    );
    assert_eq!(entry.size_bytes, restored.size_bytes);
    assert_eq!(entry.content_key_version, restored.content_key_version);
    assert_eq!(
        entry.state,
        SyncState::Placeholder,
        "the stale bytes are to be freed — the row is a placeholder at the new head"
    );
    assert_eq!(
        entry.version_num, 1,
        "the legacy local counter is left undisturbed (the real, retroactive history is nest-side)"
    );
}
