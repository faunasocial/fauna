//! **tier_3** — destination-side **generation recovery**, driven through the
//! shared client seam: `fauna_client_backup::generations` over a real nest.
//!
//! Goal doc: `docs/goal/architecture/message-segment-store.md` § *Custody grace
//! window (T)* — *"Client-drivable recovery — `fauna.backup.generation.{list,
//! restore}`, USER-class, spoken by the owner's client on its **own** authed
//! connection to the destination (never through the source: the source is the
//! writer being recovered from)."*
//!
//! **What this covers that the nest-side proof does not.** The sibling
//! `segment_backup_gc_safety.rs` proves the *mechanism* — a superseded generation
//! survives GC inside `T` and the rogue-recovery journey works through the real
//! `changes.record` handler. It drives the DB and handlers directly. This one
//! proves the **client leg**: that the shared projection all 7 apps will render
//! from (`list_retained_generations` / `restore_generation`) actually reaches
//! those handlers, decodes their replies, and — the load-bearing part — that its
//! calls land at the **custody holder** and nowhere else.
//!
//! **The mutation this pins.** Recovery routed through the source nest is the
//! failure the whole grace window exists to prevent, and it fails *quietly*: a
//! source nest is the same binary, so it answers `generation.list` perfectly
//! well — with its own (empty) custody for that owner. A client that dialled the
//! wrong nest would therefore render "nothing to recover" to a user whose backup
//! was just overwritten by that very nest. So the last two tests point the same
//! calls at a nest that is not the custody holder and assert the recovery
//! demonstrably does **not** work there.
//!
//! The transport is a direct dispatch into the destination's real `RpcRouter` as
//! the authenticated owner — the same shape every conformance test in this
//! directory uses — wrapped in an `RpcRequester` so the *client's* code is the
//! driver. The seam itself is built by the production
//! `fauna_client_backup::impl_backup_nest_seam!` macro, so this also proves the
//! macro arms every app's glue expands.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use fauna_client_backup::generations::{
    GenerationsStatus, RestoreOutcome, list_retained_generations, restore_generation,
};
use fauna_client_backup::trust::{BackupDestinationConnector, DestinationConnection};
use fauna_core::data::{BackupDestination, ContentHash};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::federation_handlers::FedBackupChangesRecordRequest;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{backup_handlers, federation_handlers};
use fauna_protocol::backup::WriterGrantRegisterRequest;
use fauna_protocol::{RpcRequester, encode_canonical};

const OWNER: [u8; 32] = [0x11; 32];
const SOURCE_NEST: [u8; 32] = [0x51; 32];

/// The path a segment's custody is recorded under, and the two manifests that
/// occupy it in turn — the second supersedes the first, which is what retains a
/// generation.
const SEG_PATH: &str = "seg-0.dat";
const GOOD_MANIFEST: [u8; 32] = [0xA1; 32];
const JUNK_MANIFEST: [u8; 32] = [0xF9; 32];

// ═════════════════════════════════════════════════════════════════════════════
// Harness — a real nest, reached through a real `RpcRequester`
// ═════════════════════════════════════════════════════════════════════════════

/// A nest: real `AppState` + real `CacheDb`, with the client-facing backup
/// router and the federation router populated.
struct Nest {
    router: RpcRouter,
    fed: fauna_nest::federation_router::FederationRouter,
    state: Arc<AppState>,
}

async fn nest() -> Arc<Nest> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "fauna-test-genclient-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        backup_service: Some(Arc::new(
            BackupService::new(db.clone(), None, false, root, None).unwrap(),
        )),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    backup_handlers::register_backup_handlers(&mut b);
    let mut fb = fauna_nest::federation_router::FederationRouter::builder();
    federation_handlers::register_federation_handlers(&mut fb);
    Arc::new(Nest {
        router: b.build(),
        fed: fb.build(),
        state,
    })
}

/// An `RpcRequester` that dispatches into one nest's client-facing router as
/// `actor` — i.e. the authenticated owner's own connection to that nest.
struct OwnerConnection {
    nest: Arc<Nest>,
    actor: [u8; 32],
}

impl RpcRequester for OwnerConnection {
    type Error = String;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let meta = self
            .nest
            .router
            .kind_meta(kind)
            .ok_or_else(|| format!("kind not registered: {kind}"))?;
        let bytes = Bytes::from(
            encode_canonical(&payload)
                .map_err(|e| e.to_string())?
                .to_vec(),
        );
        let out = (meta.handler)(self.nest.state.clone(), self.actor, bytes)
            .await
            .map_err(|e| format!("{}: {:?}", e.code, e.message))?;
        // The real transport enforces the 2 MiB message cap symmetrically
        // (`routes.rs::MAX_WS_MESSAGE_SIZE` / `ws_adapter.rs::max_message_size`,
        // both from `fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`) — an
        // in-process dispatch that skipped it would green-light replies the
        // wire can never deliver, which is exactly the defect class the storm
        // tests below pin (`transport.md` § Max frame).
        if out.len() > fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE {
            return Err(format!(
                "reply for {kind} is {} bytes — over the {} byte WS frame cap",
                out.len(),
                fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE
            ));
        }
        fauna_protocol::decode_strict(&out).map_err(|e| e.to_string())
    }
}

// The production macro — the same expansion every app's glue carries.
fauna_client_backup::impl_backup_nest_seam!(struct RpcBackupNest<OwnerConnection>);

/// Maps destination URL → the nest that URL actually reaches. Standing in for
/// each app's native/wasm connector, whose only job is the same mapping — plus
/// the identity the connection proved, which here is always the one every
/// [`dest`] row enrolled ([`ENROLLED_ID`]): the refusal of a box proving
/// another is pinned at the shared door (`fauna_client_backup::trust::
/// connect_destination`'s own tests), not re-proven against a real nest.
#[derive(Default)]
struct TestConnector {
    by_url: HashMap<String, Arc<Nest>>,
}

impl TestConnector {
    fn with(mut self, url: &str, n: &Arc<Nest>) -> Self {
        self.by_url.insert(url.to_string(), n.clone());
        self
    }
}

#[async_trait::async_trait]
impl BackupDestinationConnector for TestConnector {
    async fn connect(&self, url: &str) -> Result<DestinationConnection, String> {
        let n = self
            .by_url
            .get(url)
            .ok_or_else(|| format!("unreachable: {url}"))?;
        Ok(DestinationConnection {
            seam: Arc::new(RpcBackupNest {
                client: fauna_client_backup::BackupClient::new(OwnerConnection {
                    nest: n.clone(),
                    actor: OWNER,
                }),
            }),
            bound_nest_id: ENROLLED_ID,
        })
    }
}

/// The destination identity every [`dest`] row enrolled.
const ENROLLED_ID: [u8; 32] = [7u8; 32];

fn dest(id: &str, url: &str) -> BackupDestination {
    BackupDestination {
        destination_id: id.into(),
        destination_nest_url: url.into(),
        destination_actor_pubkey: ENROLLED_ID,
        folder_name: "__mail".into(),
        added_at: 0,
        display_name: Some("Aunt's nest".into()),
        ..Default::default()
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Arrangement — a real supersede, through the real production write path
// ═════════════════════════════════════════════════════════════════════════════

/// Register the owner, authorize the source nest to write custody here, then
/// record `SEG_PATH` twice — first the good manifest, then junk. The second
/// write supersedes the first, and on a reserved destination set a supersede
/// *retains* the displaced generation rather than forgetting it. That retained
/// row is exactly what a rogue-source recovery rolls back to.
async fn arrange_superseded_generation(n: &Arc<Nest>) {
    arrange_owner_and_grant(n).await;
    record(n, &GOOD_MANIFEST).await;
    record(n, &JUNK_MANIFEST).await;
}

/// The shared arrangement floor: the owner exists and has authorized the source
/// nest to write custody here.
async fn arrange_owner_and_grant(n: &Arc<Nest>) {
    n.state
        .db
        .create_user_with_handle(&OWNER, "free", "alice", None)
        .await
        .unwrap();

    let meta = n
        .router
        .kind_meta("fauna.backup.writer_grant.register")
        .unwrap();
    (meta.handler)(
        n.state.clone(),
        OWNER,
        Bytes::from(
            encode_canonical(&WriterGrantRegisterRequest {
                writer_nest_id: hex::encode(SOURCE_NEST),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("owner authorizes the source nest to write custody here");
}

/// Deterministic single-chunk manifest bytes for a 32-byte id (the chunk body
/// IS the id) — real held content the tests can name the way the old phantom
/// constants were named, now that the destination-derived charge accepts only
/// manifests it actually holds.
fn staged_manifest_bytes(id: &[u8; 32]) -> (Vec<u8>, [u8; 32]) {
    let chunk = ContentHash::of_raw(id);
    let m = fauna_core::chunk::ChunkManifest {
        file_hash: chunk,
        total_size: 32,
        chunk_hashes: vec![chunk],
        chunk_sizes: vec![32],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
    };
    (
        fauna_core::encoding::canonical_encode(&m).unwrap(),
        chunk.digest(),
    )
}

/// The real manifest hash [`record_at`] records for `id` — a pure function, so
/// assertions can name the expected live/retained manifest without threading
/// fixture state around.
fn staged_manifest_hash(id: &[u8; 32]) -> [u8; 32] {
    ContentHash::of_raw(&staged_manifest_bytes(id).0).digest()
}

/// Stage `id`'s chunk + manifest into the nest's store + `blob_metadata`
/// (idempotent — re-puts skip, metadata is INSERT OR IGNORE), returning the
/// real manifest hash to record.
async fn stage_manifest(n: &Arc<Nest>, id: &[u8; 32]) -> [u8; 32] {
    let store = n
        .state
        .backup_service
        .as_ref()
        .expect("test nest has a blob store")
        .local_blob_store();
    let (manifest, chunk_digest) = staged_manifest_bytes(id);
    store
        .put(&ContentHash::from_digest_raw(chunk_digest), id)
        .await
        .unwrap();
    n.state
        .db
        .put_blob_metadata(&chunk_digest, 32, "chunk", None, None)
        .await
        .unwrap();
    let mh = ContentHash::of_raw(&manifest);
    store.put(&mh, &manifest).await.unwrap();
    n.state
        .db
        .put_blob_metadata(&mh.digest(), manifest.len() as i64, "manifest", None, None)
        .await
        .unwrap();
    mh.digest()
}

/// One federated custody record from the source nest — the real production
/// write path a source nest's coordinator drives.
async fn record(n: &Arc<Nest>, manifest_id: &[u8; 32]) {
    record_at(n, SEG_PATH, manifest_id).await;
}

/// [`record`] at an arbitrary path — the storm tests write many. `manifest_id`
/// names the content; the staged manifest's real hash is what gets recorded
/// (and what [`staged_manifest_hash`] answers for assertions).
async fn record_at(n: &Arc<Nest>, path: &str, manifest_id: &[u8; 32]) {
    let real = stage_manifest(n, manifest_id).await;
    let meta = n
        .fed
        .kind_meta("fauna.federation.backup.changes.record")
        .unwrap();
    (meta.handler)(
        n.state.clone(),
        SOURCE_NEST,
        Bytes::from(
            encode_canonical(&FedBackupChangesRecordRequest {
                owner_actor_id: hex::encode(OWNER),
                kind: "mail".to_string(),
                scope_id: hex::encode(OWNER),
                device_id: hex::encode([0xD1u8; 32]),
                path: path.to_string(),
                manifest_hash: Some(hex::encode(real)),
                size_bytes: 4096,
                change_type: "create".to_string(),
                folder_id: None,
                path_sealed: None,
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("granted source nest records custody");
}

/// The manifest currently *live* for `SEG_PATH` at this nest, read straight from
/// the custody projection — the observable a restore has to move.
async fn live_manifest(n: &Arc<Nest>) -> Option<String> {
    n.state
        .db
        .list_backup_custody(&OWNER, None, 0)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.path.as_deref() == Some(SEG_PATH))
        .map(|c| hex::encode(c.manifest_hash))
}

// ═════════════════════════════════════════════════════════════════════════════
// The client leg
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn the_client_lists_a_superseded_generation_at_the_destination() {
    let d = nest().await;
    arrange_superseded_generation(&d).await;
    let conn = TestConnector::default().with("https://aunt.example", &d);

    let groups = list_retained_generations(&[dest("d1", "https://aunt.example")], &conn).await;

    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(g.status, GenerationsStatus::Listed);
    assert_eq!(g.destination_id, "d1");
    assert!(
        g.has_recoverable(),
        "the superseded good manifest is listed"
    );
    assert_eq!(g.generations.len(), 1);

    let row = &g.generations[0];
    assert_eq!(
        row.manifest_hash,
        hex::encode(staged_manifest_hash(&GOOD_MANIFEST)),
        "the retained generation is the one the junk write displaced"
    );
    assert_eq!(row.path.as_deref(), Some(SEG_PATH));

    // `grace_secs` is the nest's own constant, and the deadline is derived from
    // it — never from a client-side copy of T.
    assert_eq!(
        g.grace_secs,
        fauna_nest::backup::gc::BACKUP_CUSTODY_GRACE_SECS,
        "the window comes off the wire, from the destination"
    );
    assert_eq!(row.expires_at, row.superseded_at + g.grace_secs);

    // …and the junk is what is live right now, which is the state the user is
    // about to roll back.
    assert_eq!(
        live_manifest(&d).await,
        Some(hex::encode(staged_manifest_hash(&JUNK_MANIFEST)))
    );
}

#[tokio::test]
async fn the_client_restores_the_generation_and_the_junk_becomes_the_recoverable_one() {
    let d = nest().await;
    arrange_superseded_generation(&d).await;
    let conn = TestConnector::default().with("https://aunt.example", &d);

    let groups = list_retained_generations(&[dest("d1", "https://aunt.example")], &conn).await;
    let outcome = restore_generation(
        &conn,
        &dest("d1", "https://aunt.example"),
        &groups[0].generations[0],
    )
    .await
    .expect("the restore reaches the destination");
    assert_eq!(outcome, RestoreOutcome::Restored);

    // The rollback actually moved the live custody row.
    assert_eq!(
        live_manifest(&d).await,
        Some(hex::encode(staged_manifest_hash(&GOOD_MANIFEST))),
        "the good manifest is live again"
    );

    // And restoring was not itself destructive: the junk it displaced is now the
    // retained generation, so a mistaken restore is undoable within T.
    let after = list_retained_generations(&[dest("d1", "https://aunt.example")], &conn).await;
    assert_eq!(after[0].generations.len(), 1);
    assert_eq!(
        after[0].generations[0].manifest_hash,
        hex::encode(staged_manifest_hash(&JUNK_MANIFEST)),
        "the displaced generation is retained by the same machinery"
    );
}

#[tokio::test]
async fn restoring_a_generation_the_destination_does_not_hold_is_no_such_generation() {
    // Past-T or unknown: a product state a shell must word as "too late", not as
    // a failed restore. Proven by asking for a manifest that was never written.
    let d = nest().await;
    arrange_superseded_generation(&d).await;
    let conn = TestConnector::default().with("https://aunt.example", &d);

    let groups = list_retained_generations(&[dest("d1", "https://aunt.example")], &conn).await;
    let mut phantom = groups[0].generations[0].clone();
    phantom.manifest_hash = hex::encode([0xEE; 32]);

    let outcome = restore_generation(&conn, &dest("d1", "https://aunt.example"), &phantom)
        .await
        .expect("an unknown generation is not a transport failure");
    assert_eq!(outcome, RestoreOutcome::NoSuchGeneration);
    assert_eq!(
        live_manifest(&d).await,
        Some(hex::encode(staged_manifest_hash(&JUNK_MANIFEST))),
        "a no-op restore changes nothing"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The mutation — recovery must not work through a nest that holds no custody
// ═════════════════════════════════════════════════════════════════════════════

/// Point the *same* list at the source nest instead of the destination. The
/// source is the same binary and answers happily — with nothing — which is
/// exactly why this has to be pinned: a client that dialled the wrong nest would
/// tell a user whose backup was just overwritten that there is nothing to
/// recover.
#[tokio::test]
async fn listing_at_the_source_nest_finds_nothing_to_recover() {
    let destination = nest().await;
    let source = nest().await;
    arrange_superseded_generation(&destination).await;
    // The source knows the owner but holds none of their custody.
    source
        .state
        .db
        .create_user_with_handle(&OWNER, "free", "alice", None)
        .await
        .unwrap();

    let conn = TestConnector::default()
        .with("https://aunt.example", &destination)
        .with("https://source.example", &source);

    let at_destination =
        list_retained_generations(&[dest("d1", "https://aunt.example")], &conn).await;
    assert!(at_destination[0].has_recoverable());

    let at_source = list_retained_generations(&[dest("d1", "https://source.example")], &conn).await;
    assert_eq!(
        at_source[0].status,
        GenerationsStatus::Listed,
        "the source answers — it is the same binary; that is the trap"
    );
    assert!(
        !at_source[0].has_recoverable(),
        "recovery routed through the source finds nothing — only the custody \
         holder can answer, which is why the URL comes from the client's own \
         pinned config and never from the source"
    );
}

#[tokio::test]
async fn restoring_at_the_source_nest_does_not_move_the_destinations_custody() {
    let destination = nest().await;
    let source = nest().await;
    arrange_superseded_generation(&destination).await;
    source
        .state
        .db
        .create_user_with_handle(&OWNER, "free", "alice", None)
        .await
        .unwrap();

    let conn = TestConnector::default()
        .with("https://aunt.example", &destination)
        .with("https://source.example", &source);
    let groups = list_retained_generations(&[dest("d1", "https://aunt.example")], &conn).await;

    // Same generation, wrong nest.
    let outcome = restore_generation(
        &conn,
        &dest("s", "https://source.example"),
        &groups[0].generations[0],
    )
    .await
    .expect("the source answers the kind");
    assert_eq!(
        outcome,
        RestoreOutcome::NoSuchGeneration,
        "the source holds no such generation — it cannot roll back what it does not hold"
    );
    assert_eq!(
        live_manifest(&destination).await,
        Some(hex::encode(staged_manifest_hash(&JUNK_MANIFEST))),
        "the destination's custody is untouched by a restore aimed elsewhere"
    );
}

/// One destination being unreachable must never read as "nothing to recover" —
/// the false reassurance a dropped connection would otherwise buy.
#[tokio::test]
async fn an_unreachable_destination_is_not_reported_as_clean() {
    let d = nest().await;
    arrange_superseded_generation(&d).await;
    let conn = TestConnector::default().with("https://aunt.example", &d);

    let groups = list_retained_generations(
        &[
            dest("gone", "https://gone.example"),
            dest("d1", "https://aunt.example"),
        ],
        &conn,
    )
    .await;

    assert_eq!(groups[0].status, GenerationsStatus::Unreachable);
    assert!(!groups[0].has_recoverable());
    assert_eq!(
        groups[1].status,
        GenerationsStatus::Listed,
        "a dead sibling must not blank a live destination"
    );
    assert!(groups[1].has_recoverable());
}

/// Owner-scoping is the destination's, not the client's: a different actor's
/// connection sees none of Alice's retained generations.
#[tokio::test]
async fn the_generation_list_is_owner_scoped_at_the_destination() {
    let d = nest().await;
    arrange_superseded_generation(&d).await;
    let stranger: [u8; 32] = [0x77; 32];
    d.state
        .db
        .create_user_with_handle(&stranger, "free", "mallory", None)
        .await
        .unwrap();

    let reply = fauna_client_backup::BackupClient::new(OwnerConnection {
        nest: d.clone(),
        actor: stranger,
    })
    .generation_list(None)
    .await
    .expect("the stranger may call the kind");

    assert!(
        reply.generations.is_empty(),
        "a caller only ever sees custody held for itself"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The storm — a row set past the WS frame must not make recovery unreachable
// ═════════════════════════════════════════════════════════════════════════════
//
// The defect chain these tests pin: an unpaginated full-table serve breaks
// the 2 MiB frame at a few thousand rows, the projection degrades the
// destination to `Unreachable`, the shell renders no restore affordance, and
// the retained generations reclaim at T while the read that would surface them
// cannot complete. The row count is attacker-cheap (the rogue source declares
// `size_bytes`, so a storm can cost zero quota) and the honest open-tail
// re-upload path reaches it too. These tests build a storm whose encoded reply
// exceeds the frame budget and assert the walk still returns every row — a
// token multi-row list would pass against the unfixed code and prove nothing.

/// Rogue-source storm: one path superseded over and over. Long paths are the
/// adversary's prerogative (the source controls `path`), and `size_bytes: 0`
/// is the review's own observation that a storm can cost no quota at all.
const STORM_ROWS: usize = 2_000;

fn storm_path(i: usize) -> String {
    format!("{}/seg-{i:08}.dat", "s".repeat(1_000))
}

fn storm_manifest(i: usize) -> [u8; 32] {
    let mut m = [0xB0u8; 32];
    m[..8].copy_from_slice(&(i as u64).to_be_bytes());
    m
}

/// `STORM_ROWS` retained generations on one path: write `STORM_ROWS + 1`
/// manifests in turn; every write after the first supersedes-and-retains.
async fn arrange_generation_storm(n: &Arc<Nest>) {
    let path = storm_path(0);
    for i in 0..=STORM_ROWS {
        record_at(n, &path, &storm_manifest(i)).await;
    }
}

/// `STORM_ROWS` *live* custody rows: one write per distinct path.
async fn arrange_custody_storm(n: &Arc<Nest>) {
    arrange_owner_and_grant(n).await;
    for i in 0..STORM_ROWS {
        record_at(n, &storm_path(i), &storm_manifest(i)).await;
    }
}

#[tokio::test]
async fn a_generation_storm_past_the_ws_frame_is_still_fully_listable() {
    let d = nest().await;
    arrange_superseded_generation(&d).await;
    arrange_generation_storm(&d).await;
    let conn = TestConnector::default().with("https://aunt.example", &d);

    let groups = list_retained_generations(&[dest("d1", "https://aunt.example")], &conn).await;

    assert_eq!(
        groups[0].status,
        GenerationsStatus::Listed,
        "a storm must not degrade the recovery read to Unreachable"
    );
    // Every retained row comes back — the storm's rows are exactly the ones a
    // user recovering from a rogue source needs to see.
    assert_eq!(
        groups[0].generations.len(),
        STORM_ROWS + 1,
        "the walk returns the whole retained set, not one frame's worth"
    );
}

/// The audit loop's read is the same shape, so the same storm must not blind
/// the mechanism that warns. Freshness is arranged to fail (it is evaluated
/// from the custody reply and short-circuits before inclusion), so a real
/// verdict here proves the read completed; the unfixed code yields
/// `Unreachable` instead.
struct NoInclusion;

impl fauna_client_backup::audit::BackupInclusionSource for NoInclusion {
    fn fetcher(&self, _url: &str) -> Arc<dyn fauna_core::file_download::BlobFetcher> {
        unreachable!("freshness fails before the inclusion arm runs")
    }
    fn keys(&self) -> fauna_core::file_download::FileDownloadKeys {
        unreachable!("freshness fails before the inclusion arm runs")
    }
    fn folder_index(&self, _: &str) -> Option<fauna_client_backup::audit::FolderIndex> {
        unreachable!("freshness fails before the inclusion arm runs")
    }
}

#[tokio::test]
async fn a_custody_storm_past_the_ws_frame_still_yields_a_real_audit_verdict() {
    use fauna_client_backup::audit::{
        AuditInputs, AuditVerdict, DestinationAuditState, FRESHNESS_SLACK_SECS, audit_destination,
    };

    let d = nest().await;
    arrange_custody_storm(&d).await;
    let conn = TestConnector::default().with("https://aunt.example", &d);
    let seam = conn.connect("https://aunt.example").await.unwrap().seam;

    let now = fauna_core::data::Timestamp::now_secs();
    let inputs = AuditInputs {
        // Far ahead of anything the destination stamped, so freshness fails —
        // a REAL verdict, reachable only if the custody read completes.
        local_high_water: Some(now + FRESHNESS_SLACK_SECS + 1_000),
        added_at: 0,
    };

    let (verdict, _state) = audit_destination(
        seam.as_ref(),
        &NoInclusion,
        "https://aunt.example",
        &inputs,
        &DestinationAuditState::default(),
        // No covered folders attached — this test is about the custody-list
        // walk under a storm, and its verdict is freshness, which the plane
        // routing never reaches.
        &Default::default(),
        // No source to vouch either: freshness fails before any ledger opens.
        &fauna_client_backup::audit::NoSourceVouch,
        now,
    )
    .await;

    assert!(
        matches!(verdict, AuditVerdict::FreshnessFailure { .. }),
        "the storm must not blind the audit read: expected a real freshness \
         verdict over the full custody set, got {verdict:?}"
    );

    // And completeness, not just reachability: the walk returns every live
    // row through the frame-capped transport.
    let items = fauna_client_backup::audit::read_full_custody(seam.as_ref())
        .await
        .expect("the paged walk completes over the storm");
    assert_eq!(
        items.len(),
        STORM_ROWS,
        "the walk returns the whole custody set, not one frame's worth"
    );
}
