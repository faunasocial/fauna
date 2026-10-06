//! The share pump's two-seat proof at tier_1: a FILE ARRIVES BETWEEN TWO
//! DIFFERENT USERS' SEATS over a real peer channel — Alice's seat serves her
//! recorded file through the bound contact-plane node (rows from retention,
//! manifest from retention, chunk bodies re-derived from plaintext ranges;
//! NO engine on the serving side), and Bob's seat pumps it across
//! (`share_pump::pull_set_from_peer`: dial → mutual M2 admission → page →
//! spool → the ingest door) into his own engine's provisional plane and his
//! local tree.
//!
//! This is `p2p.md` § Cross-user shared-set transfer's core mechanism, one
//! layer below the tui journey: real serve stack, real pull stack, real
//! wire (`fauna_transport::testing::MemTransport`), scripted only at the
//! seams the design scripts anyway (the M2 roster consult, the brake
//! verdict, the discovery target).

#![cfg(all(feature = "p2p-share", feature = "account-runtime"))]

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use fauna_client_capabilities::group_ceremony_node::{CeremonyBindVerdict, CeremonyNode};
use fauna_core::data::Timestamp;
use fauna_core::folder_keys::FolderContentKeys;
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_peer_share::SetMembership;
use fauna_peer_sync::discovery::PeerDialTarget;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::share_body::TreeBodySource;
use fauna_sync_engine::share_landing::Landing;
use fauna_sync_engine::share_pump::{
    ShareIngestDoor, ShareIngestSummary, SharedSetSpec, pull_set_from_peer, refresh_serve_sources,
};
use fauna_transport::testing::{MemTransport, OnPath, PathCell, await_listening};
use fauna_transport::{EndpointKey, NestPath, PathCandidates, PathKind, PeerTransport};

const SET: [u8; 32] = [0x5A; 32];
const CONTENT_KEY: [u8; 32] = [0x7E; 32];
/// The set's nonce — the binding every seat's reader installs from custody.
const SET_NONCE: [u8; 32] = [0x4E; 32];

/// Alice's retained own row, signed by her as the share leg serves it — what
/// her engine's signing funnel stores (`SyncEngine::sign_own_row`).
fn signed_by_alice(
    mut row: fauna_sync_engine::db::OwnChangeRow,
) -> fauna_sync_engine::db::OwnChangeRow {
    let mut wire = fauna_sync_engine::engine::wire_change(&row, row.seq.unwrap_or(0));
    fauna_protocol::sync_writer_sig::ChangeSigner::direct(&alice())
        .sign_row(&mut wire, SET_NONCE)
        .expect("sign");
    row.signature = wire.signature.map(|b| b.into_vec());
    row.signer_key = wire.signer_key.map(|b| b.into_vec());
    row
}
/// An arbitrary fixed day bucket — the gate's clock is injected, never read.
const TODAY: i64 = 20_600;

/// The ceiling on a pump call. Every pull below is a POSITIVE wait — *the
/// transfer finished* — so this is a **named generous budget** in the sense of
/// `e2e-conventions.md` convention 14, never a latency assertion: its whole job
/// is to turn a genuine hang into a bounded, named failure, and it must
/// therefore sit far above any non-pathological delay on the *slowest* box that
/// runs this test, which is not the box that writes it.
///
/// **Sized from measurement, after the previous 120 s described the hardware
/// rather than a hang.** That ceiling failed
/// `a_file_arrives_between_two_users_seats_over_the_peer_channel` on GitHub's
/// hosted runners on BOTH public CI runs (2026-08-29 and 2026-08-30) while
/// never once failing on a development machine — a 2-for-2 / 0-for-N split,
/// which is a systematic hardware difference and not the flake it was twice
/// taken for. The test was **46.5 s of work on a developer workstation and
/// 99.4% CPU-bound on a single core** (`real 46.6 s` against `user 44.7 s` +
/// `sys 1.6 s`): the transport is in-memory and the runtime is
/// `current_thread`, so there was nothing to wait on and the figure was pure
/// single-core compute. 120 s was thus only **2.58x** the observed cost —
/// inside the ordinary single-core spread between this box and a 4-vCPU
/// hosted runner, so crossing it was deterministic there and unreachable
/// here. That reading briefly widened the ceiling to 600 s (~13x) rather than
/// explain the 46.5 s itself.
///
/// **Row 451 then attributed the 46.5 s and found it was not inherent to the
/// work — it was quadratic accidental complexity, now fixed.** Of the 46.5 s,
/// ~2.7 s was fixture setup (`seal_blob` over the whole 20 MiB file, once)
/// and the pump itself (`pull_set_from_peer`) cost the remaining ~40 s, of
/// which ~38 s was `libs/fauna-peer-share/src/server.rs`'s `handle_chunks_pull`
/// re-deriving (read + compress + encrypt) the file's 8 sealed chunks: not 8
/// times, but **133** times. Two compounding causes, both in that file: (1)
/// a puller re-asks for every still-incomplete chunk on every pull round
/// once a chunk's sealed size exceeds `MAX_BODY_BYTES_PER_REPLY` (700 KiB) —
/// true of every chunk here, each 2–4 MiB — so a chunk needing N rounds to
/// finish was re-requested by every round of every chunk that had not yet
/// had ITS turn; (2) the server re-derived a want's FULL body before ever
/// checking whether the shared per-reply budget had any room left to use it,
/// so most of those re-requests paid the expensive path for bytes that were
/// then thrown away. Fixed by adding a bounded (16 MiB), content-addressed
/// cache of resealed chunk bodies (`fauna_sync_engine::peer_share_store::
/// ShareServeMemo`, keyed by store key — safe forever, since a hit is never
/// staler than a fresh re-derivation) and a cheap `ShareStore::chunk_exists`
/// check the server now consults instead of `chunk_body` once its reply
/// budget is already spent. Re-measured: this test's 8 chunks now re-derive
/// exactly 8 times (one miss each, cache hits for the rest of that chunk's
/// own rounds), and the whole test — all four in this file, not just this
/// one — now finishes in **~9.8 s** (this test alone: `user ~7.5 s`).
///
/// 120 s is ~12x the new measurement — comparable margin to what 600 s gave
/// the old one, on a much smaller number. Raise it if the measurement moves;
/// do not lower it to fit a faster box.
const PUMP_CEILING: std::time::Duration = std::time::Duration::from_secs(120);

fn tier1_policy() -> fauna_core::feature_gate::EffectivePolicy {
    // The offline posture: the artifact's own tier-1 constants bind.
    fauna_core::feature_gate::effective_policy(
        fauna_core::feature_gate::GatedFeature::P2pShare,
        &[],
        &[],
    )
}

fn alice() -> ActorKeypair {
    ActorKeypair::from_secret([21u8; 32])
}

fn bob() -> ActorKeypair {
    ActorKeypair::from_secret([31u8; 32])
}

/// The set's owner — neither seat, so Alice writes as a roster member and her
/// rows need Bob's cached roster, as a member's rows always do (the owner is a
/// writer by construction).
fn set_owner() -> ActorKeypair {
    ActorKeypair::from_secret([41u8; 32])
}

fn keys() -> FolderContentKeys {
    FolderContentKeys::genesis(CONTENT_KEY, 1_760_000_000)
}

/// Both seats' M2 consult: the set's roster is {alice, bob}. Scripted here
/// because the roster's own verification is `fauna-client-folders`' tested
/// adapter; what THIS test proves is everything the consult gates.
struct HolidayRoster;
impl SetMembership for HolidayRoster {
    fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
        channel_id == &SET && (*actor == alice().actor_id() || *actor == bob().actor_id())
    }
}

async fn bind_seat(
    listeners: &fauna_transport::testing::Listeners,
    who: &ActorKeypair,
) -> CeremonyNode {
    bind_seat_on(listeners, who, None).await
}

/// [`bind_seat`] whose connections answer `path` when given — the seat on a
/// relayed path (`fauna_transport::testing::OnPath`).
async fn bind_seat_on(
    listeners: &fauna_transport::testing::Listeners,
    who: &ActorKeypair,
    path: Option<PathCell>,
) -> CeremonyNode {
    let mem: Arc<dyn PeerTransport> = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(who.actor_id().0),
        listeners: Arc::clone(listeners),
    });
    let transport: Arc<dyn PeerTransport> = match path {
        Some(path) => Arc::new(OnPath { inner: mem, path }),
        None => mem,
    };
    let node = CeremonyNode::bind_with_share_plane(
        CeremonyBindVerdict::Bind,
        transport,
        who.actor_id(),
        "seat".into(),
        Arc::new(Mutex::new(GroupShareConfig::default())),
        Arc::new(|| Timestamp(1_700_000_000)),
        Arc::new(|| {}),
        Arc::new(HolidayRoster),
    )
    .await
    .expect("bind");
    await_listening(listeners, &who.actor_id().0).await;
    node
}

/// Bob's state-writing half, wired EXACTLY as the agent wires it: the engine
/// owned by its resident loop, the door sending
/// [`fauna_sync_engine::always_resident::EngineCommand::ShareIngest`] over
/// the per-folder command channel and awaiting the oneshot.
struct ChannelDoor {
    cmd_tx: tokio::sync::mpsc::Sender<fauna_sync_engine::always_resident::EngineCommand>,
}

#[async_trait::async_trait]
impl ShareIngestDoor for ChannelDoor {
    async fn ingest(
        &self,
        _folder: &str,
        _folder_id: &str,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &std::path::Path,
    ) -> anyhow::Result<ShareIngestSummary> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(
                fauna_sync_engine::always_resident::EngineCommand::ShareIngest {
                    proven_actor_hex: proven_actor_hex.to_string(),
                    rows,
                    spool_dir: spool_dir.to_path_buf(),
                    reply,
                },
            )
            .await
            .map_err(|_| anyhow::anyhow!("engine loop gone"))?;
        let (report, cursor) = rx
            .await
            .map_err(|_| anyhow::anyhow!("loop dropped reply"))??;
        Ok(ShareIngestSummary {
            refused: report.refused as u32,
            overlaid: report.overlaid as u32,
            materialized: report.materialized as u32,
            already_current: report.already_current as u32,
            cursor,
            storage_limited: report.storage_limited,
        })
    }
}

/// A door that must never open — the stranger leg's assertion that a refused
/// admission reaches no ingest at all.
struct ClosedDoor;

#[async_trait::async_trait]
impl ShareIngestDoor for ClosedDoor {
    async fn ingest(
        &self,
        _folder: &str,
        _folder_id: &str,
        _proven_actor_hex: &str,
        _rows: Vec<Vec<u8>>,
        _spool_dir: &std::path::Path,
    ) -> anyhow::Result<ShareIngestSummary> {
        panic!("a refused admission must never reach the ingest door");
    }
}

/// The seat's engine as the resident loop now takes it: an `Arc`, so a
/// caller keeps a handle while the loop drives it (`run_watch_loop`'s own
/// parameter doc). Every call here feeds exactly that loop.
///
/// `SyncEngine` is `!Sync` (its `SyncDb` wraps a raw `rusqlite::Connection`);
/// the loop future is deliberately `!Send` and raced in-task, so this `Arc`
/// never crosses a thread boundary.
#[allow(clippy::arc_with_non_send_sync)]
fn engine_over(watch_dir: PathBuf, db: SyncDb) -> Arc<fauna_sync_engine::engine::SyncEngine> {
    Arc::new(engine_over_inner(watch_dir, db))
}

fn engine_over_inner(watch_dir: PathBuf, db: SyncDb) -> fauna_sync_engine::engine::SyncEngine {
    use fauna_nest_http::{BearerSource, StaticBearer};
    let url = "http://127.0.0.1:9";
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        url.to_string(),
        ActorKeypair::generate(),
        bearer,
        reqwest::Client::new(),
    ));
    let engine = fauna_sync_engine::engine::SyncEngine::new(
        watch_dir,
        db,
        fauna_sync_engine::nest_client::SyncClient::new(auth, &[0u8; 32]),
        Some("holiday".to_string()),
        [0u8; 32],
        None,
        None,
        None,
        Some(vec![0x11; 32]), // bound marker
        Some(keys()),
        fauna_core::format::ConflictPolicy::default(),
        fauna_core::format::FormatRegistry::new(),
        fauna_sync_engine::ignore::IgnoreMatcher::default(),
        4,
        fauna_sync_engine::transfer::TransferPool::new(
            Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(4)),
            None,
        ),
        fauna_client::NestClient::new(url.to_string(), ActorKeypair::generate()),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(set_owner().actor_id().0),
        ..Default::default()
    });
    engine
}

/// Alice's seat serving a recorded, sealed, multi-chunk file, and Bob's seat
/// — its engine on its resident loop, agent-style — ready to pull it. Bob's
/// seat dials over `bob_path` when given (`OnPath`; the ruling 4 pins), else
/// over the bare in-memory transport. The holiday pins below share it.
struct HolidaySeats {
    bytes: Vec<u8>,
    _alice_dir: tempfile::TempDir,
    _alice_node: CeremonyNode,
    bob_dir: tempfile::TempDir,
    bob_db_path: PathBuf,
    bob_node: CeremonyNode,
    door: ChannelDoor,
    membership: Arc<dyn SetMembership + Send + Sync>,
    spool_root: tempfile::TempDir,
    policy: fauna_core::feature_gate::EffectivePolicy,
    /// Bob's resident loop — deliberately !Send, so it is raced in-task
    /// against each pump ([`Self::pull`]).
    loop_fut: Pin<Box<dyn Future<Output = ()>>>,
}

async fn holiday_seats(bob_path: Option<PathCell>) -> HolidaySeats {
    let listeners = fauna_transport::testing::listeners();

    // ── Alice's seat: a recorded, sealed, multi-chunk file + retention, and
    // the serving node. NO engine — serving is read-only against her state.
    let alice_dir = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0..20 * 1024 * 1024u64)
        .map(|i| ((i.wrapping_mul(37) ^ (i / 239)) % 251) as u8)
        .collect();
    std::fs::write(alice_dir.path().join("holiday.mp4"), &bytes).unwrap();
    let sealed = fauna_sync_engine::seal::seal_blob(&bytes, Some((CONTENT_KEY, Some(1)))).unwrap();
    assert!(
        sealed.manifest.chunk_hashes.len() >= 2,
        "the fixture must be multi-chunk (ranged pulls are what make the leg real)"
    );

    let alice_db_path = alice_dir.path().join("fs-holiday.db");
    {
        let db = SyncDb::open(&alice_db_path).unwrap();
        db.retain_own_change(&signed_by_alice(fauna_sync_engine::db::OwnChangeRow {
            seq: Some(41),
            path: "holiday.mp4".to_string(),
            path_hash: hex::encode(fauna_core::sync::path_hash("holiday.mp4")),
            path_sealed: None,
            manifest_hash: Some(hex::encode(sealed.manifest_hash.digest())),
            size_bytes: bytes.len() as i64,
            change_type: "create".to_string(),
            created_at: 1_700_000_000_000,
            content_key_version: Some(1),
            thumbnail_hash: None,
            derived_through: None,
            is_resolution: None,
            author_actor_id: alice().actor_id().to_hex(),
            device_id: "d2".repeat(32),
            ..Default::default()
        }))
        .unwrap();
        db.retain_manifest(
            &hex::encode(sealed.manifest_hash.digest()),
            &sealed.manifest_bytes,
            "holiday.mp4",
            Some(1),
        )
        .unwrap();
    }

    let alice_node = bind_seat(&listeners, &alice()).await;
    let routed = refresh_serve_sources(
        &alice_node,
        &[SharedSetSpec {
            folder: "holiday".to_string(),
            folder_id: "local:1".to_string(),
            set_id: SET,
            db_path: alice_db_path.clone(),
            body: TreeBodySource::shared(alice_dir.path()),
            landing: Landing::Resident,
            content_keys: keys(),
        }],
    );
    assert_eq!(routed, 1);

    // ── Bob's seat: an empty tree, an engine over his own state DB (with
    // Alice cached as a WRITER — the fail-closed row consult), and the pump.
    let bob_dir = tempfile::tempdir().unwrap();
    let bob_db_path = bob_dir.path().join("fs-holiday.db");
    {
        let db = SyncDb::open(&bob_db_path).unwrap();
        db.cache_share_writer_roster(&[(alice().actor_id().to_hex(), true)], &[], None)
            .unwrap();
    }
    let bob_node = bind_seat_on(&listeners, &bob(), bob_path).await;
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(2);
    let resident = fauna_sync_engine::always_resident::run_watch_loop(
        engine_over(
            bob_dir.path().to_path_buf(),
            SyncDb::open(&bob_db_path).unwrap(),
        ),
        bob_dir.path().to_path_buf(),
        "holiday".to_string(),
        std::time::Duration::from_secs(3600),
        None,
        Some(cmd_rx),
    );
    HolidaySeats {
        bytes,
        _alice_dir: alice_dir,
        _alice_node: alice_node,
        bob_dir,
        bob_db_path,
        bob_node,
        door: ChannelDoor { cmd_tx },
        membership: Arc::new(HolidayRoster),
        spool_root: tempfile::tempdir().unwrap(),
        policy: tier1_policy(),
        loop_fut: Box::pin(async move {
            let _ = resident.await;
        }),
    }
}

impl HolidaySeats {
    /// One `pull_set_from_peer` of the holiday set from Alice, raced against
    /// Bob's resident loop.
    async fn pull(
        &mut self,
        ledger: &mut fauna_sync_engine::share_pump::TransferUsageLedger,
        nest: NestPath,
    ) -> fauna_sync_engine::share_pump::SetPullOutcome {
        let spec = SharedSetSpec {
            folder: "holiday".to_string(),
            folder_id: "local:1".to_string(),
            set_id: SET,
            db_path: self.bob_db_path.clone(),
            body: TreeBodySource::shared(self.bob_dir.path()),
            landing: Landing::Resident,
            content_keys: keys(),
        };
        let target = PeerDialTarget {
            node_id: alice().actor_id().0,
            candidates: PathCandidates::default(),
            relay_url: None,
        };
        let pump = pull_set_from_peer(
            &self.bob_node,
            &self.membership,
            &self.door,
            &[SET],
            &spec,
            &target,
            self.spool_root.path(),
            &self.policy,
            ledger,
            TODAY,
            nest,
        );
        tokio::select! {
            _ = &mut self.loop_fut => panic!("the resident loop must not exit"),
            got = tokio::time::timeout(PUMP_CEILING, pump) => got
                .expect("the pull must finish well inside the ceiling")
                .expect("pull"),
        }
    }

    fn landed(&self) -> Option<Vec<u8>> {
        std::fs::read(self.bob_dir.path().join("holiday.mp4")).ok()
    }
}

#[tokio::test]
async fn a_file_arrives_between_two_users_seats_over_the_peer_channel() {
    let mut seats = holiday_seats(None).await;
    let mut ledger = fauna_sync_engine::share_pump::TransferUsageLedger::default();

    let outcome = seats.pull(&mut ledger, NestPath::Reachable).await;
    assert!(
        outcome.admitted,
        "the mutual M2 admission let the set through"
    );
    assert_eq!(outcome.rows_accepted, 1, "{outcome:?}");
    assert_eq!(outcome.materialized, 1, "{outcome:?}");
    assert_eq!(outcome.relay_deferred, 0, "a LAN path carries the bytes");
    assert_eq!(outcome.cursor, 41, "cursor advanced to Alice's row");
    assert_eq!(
        seats.landed().as_deref(),
        Some(&seats.bytes[..]),
        "the holiday video arrived byte-identical on Bob's seat"
    );
    // The transfer gate spent: one file op, the bytes that actually crossed
    // the wire, and Alice became a counted counterparty.
    //
    // ⚠ Volume is the WIRE spend, not the plaintext size the change row
    // asserts. The row's `size_bytes` is the
    // counterparty's own claim and may not be metered; what is metered is
    // what this side counted arriving, and stored chunk bodies are
    // compressed — this 20 MiB video meters as ~19 MB. So the bound to
    // assert is that a real multi-megabyte transfer was recorded against
    // Alice, in the file's own order of magnitude.
    let spent = ledger.usage_counters(TODAY);
    assert_eq!(spent.operations.day, 1);
    assert!(
        spent.volume.day > 1_000_000,
        "a 20 MiB file must meter as megabytes of wire spend, got {}",
        spent.volume.day
    );
    assert!(
        spent.volume.day < 2 * seats.bytes.len() as u64,
        "...and must stay in the file's own order of magnitude, got {}",
        spent.volume.day
    );
    assert_eq!(spent.counterparties.month, 1);

    // Second pass: idempotent and quiet — the cursor stands, nothing re-pulls.
    let again = seats.pull(&mut ledger, NestPath::Reachable).await;
    assert_eq!(again.rows_accepted, 0, "nothing new past the cursor");
    assert_eq!(
        ledger.usage_counters(TODAY).volume.day,
        spent.volume.day,
        "a cursor-quiet pass moves no bytes and must meter none"
    );
}

/// `p2p.md` § The relay, ruling 4, on the share pump: over a relayed
/// connection with the set's nest answering, the page's rows are walked and
/// handed through the door but no body crosses — the file is not landed, no
/// byte is metered, and the cursor stays below the row (deferred, never
/// failed). The path is re-read per page: the same seat on a connection the
/// substrate turned direct lands the file on the next pull.
#[tokio::test]
async fn a_relayed_seat_leaves_the_bodies_to_a_reachable_nest() {
    let path = PathCell::new(PathKind::Relay);
    let mut seats = holiday_seats(Some(path.clone())).await;
    let mut ledger = fauna_sync_engine::share_pump::TransferUsageLedger::default();

    let outcome = seats.pull(&mut ledger, NestPath::Reachable).await;
    assert!(outcome.admitted, "{outcome:?}");
    assert_eq!(outcome.rows_accepted, 1, "the row is walked: {outcome:?}");
    assert_eq!(outcome.relay_deferred, 1, "{outcome:?}");
    assert_eq!(outcome.materialized, 0, "{outcome:?}");
    assert!(
        outcome.cursor < 41,
        "a row whose body did not arrive holds the cursor below it: {outcome:?}"
    );
    assert!(seats.landed().is_none(), "no body crossed the relayed path");
    assert_eq!(
        ledger.usage_counters(TODAY).volume.day,
        0,
        "nothing moved, nothing metered"
    );

    path.set(PathKind::Lan);
    let outcome = seats.pull(&mut ledger, NestPath::Reachable).await;
    assert_eq!(outcome.relay_deferred, 0, "{outcome:?}");
    assert_eq!(outcome.materialized, 1, "{outcome:?}");
    assert_eq!(outcome.cursor, 41, "{outcome:?}");
    assert_eq!(seats.landed().as_deref(), Some(&seats.bytes[..]));
}

/// Ruling 4's one exception on the share pump: the nest path unavailable to
/// this device, the relayed connection carries the page's bodies too.
#[tokio::test]
async fn a_relayed_seat_carries_the_bodies_when_the_nest_path_is_unavailable() {
    let mut seats = holiday_seats(Some(PathCell::new(PathKind::Relay))).await;
    let mut ledger = fauna_sync_engine::share_pump::TransferUsageLedger::default();

    let outcome = seats.pull(&mut ledger, NestPath::Unavailable).await;
    assert_eq!(outcome.relay_deferred, 0, "{outcome:?}");
    assert_eq!(outcome.materialized, 1, "{outcome:?}");
    assert_eq!(outcome.cursor, 41, "{outcome:?}");
    assert_eq!(seats.landed().as_deref(), Some(&seats.bytes[..]));
}

/// A stranger's pull gets nothing: the admission consult refuses an actor
/// outside the set's roster, so no rows and no bytes ever leave Alice's seat
/// (wormability rule 1 — admission before serving).
#[tokio::test]
async fn a_stranger_is_refused_at_admission_and_pulls_nothing() {
    let listeners = fauna_transport::testing::listeners();

    let alice_dir = tempfile::tempdir().unwrap();
    std::fs::write(alice_dir.path().join("holiday.mp4"), b"secret").unwrap();
    let alice_db_path = alice_dir.path().join("fs-holiday.db");
    drop(SyncDb::open(&alice_db_path).unwrap());
    let alice_node = bind_seat(&listeners, &alice()).await;
    refresh_serve_sources(
        &alice_node,
        &[SharedSetSpec {
            folder: "holiday".to_string(),
            folder_id: "local:1".to_string(),
            set_id: SET,
            db_path: alice_db_path,
            body: TreeBodySource::shared(alice_dir.path()),
            landing: Landing::Resident,
            content_keys: keys(),
        }],
    );

    let mallory = ActorKeypair::from_secret([41u8; 32]);
    let mallory_dir = tempfile::tempdir().unwrap();
    let mallory_db = mallory_dir.path().join("fs-holiday.db");
    drop(SyncDb::open(&mallory_db).unwrap());
    let mallory_node = bind_seat(&listeners, &mallory).await;
    let door = ClosedDoor;
    // Mallory's own consult happily admits everyone — HER verdict is not the
    // one that protects Alice; Alice's is.
    struct AdmitAll;
    impl SetMembership for AdmitAll {
        fn is_member(&self, _c: &[u8; 32], _a: &ActorId) -> bool {
            true
        }
    }
    let membership: Arc<dyn SetMembership + Send + Sync> = Arc::new(AdmitAll);
    let spool_root = tempfile::tempdir().unwrap();
    let mut ledger = fauna_sync_engine::share_pump::TransferUsageLedger::default();
    let policy = tier1_policy();

    let refused = pull_set_from_peer(
        &mallory_node,
        &membership,
        &door,
        &[SET],
        &SharedSetSpec {
            folder: "holiday".to_string(),
            folder_id: "local:1".to_string(),
            set_id: SET,
            db_path: mallory_db,
            body: TreeBodySource::shared(mallory_dir.path()),
            landing: Landing::Resident,
            content_keys: keys(),
        },
        &PeerDialTarget {
            node_id: alice().actor_id().0,
            candidates: PathCandidates::default(),
            relay_url: None,
        },
        spool_root.path(),
        &policy,
        &mut ledger,
        TODAY,
        NestPath::Reachable,
    )
    .await;

    // Alice's serve side refuses the admit exchange itself for an actor her
    // roster does not vouch for — the pull errors at admission (the pump's
    // pull_pass logs-and-skips exactly this), the door never opened
    // (ClosedDoor panics if it did), and no bytes left her seat.
    assert!(refused.is_err(), "a stranger's admission must not succeed");
    assert!(
        !mallory_dir.path().join("holiday.mp4").exists(),
        "no bytes left Alice's seat"
    );
}

/// The e2e probe (`share_probe::probe_set`) reads what the serve door hands
/// out without the pump's politeness: it keeps asking after a refused
/// admission. From a member it must come back admitted with the row and the
/// manifest — the control — and from a stranger holding the very manifest hash
/// the member saw, with nothing, while Alice's serve tally does not move.
#[tokio::test]
async fn the_share_probe_reads_a_members_set_and_a_stranger_gets_nothing() {
    use fauna_client_capabilities::group_ceremony_view::PeerCode;
    use fauna_sync_engine::share_probe::probe_set;
    use fauna_sync_engine::share_serve_tally;

    let listeners = fauna_transport::testing::listeners();

    // A path no other test in this binary serves: the tally is process-wide.
    const PATH: &str = "probe-notes.txt";
    let bytes = b"the probe's payload".to_vec();
    let alice_dir = tempfile::tempdir().unwrap();
    std::fs::write(alice_dir.path().join(PATH), &bytes).unwrap();
    let sealed = fauna_sync_engine::seal::seal_blob(&bytes, Some((CONTENT_KEY, Some(1)))).unwrap();
    let manifest_hex = hex::encode(sealed.manifest_hash.digest());
    let alice_db_path = alice_dir.path().join("fs-holiday.db");
    {
        let db = SyncDb::open(&alice_db_path).unwrap();
        db.retain_own_change(&signed_by_alice(fauna_sync_engine::db::OwnChangeRow {
            seq: Some(7),
            path: PATH.to_string(),
            path_hash: hex::encode(fauna_core::sync::path_hash(PATH)),
            path_sealed: None,
            manifest_hash: Some(manifest_hex.clone()),
            size_bytes: bytes.len() as i64,
            change_type: "create".to_string(),
            created_at: 1_700_000_000_000,
            content_key_version: Some(1),
            thumbnail_hash: None,
            derived_through: None,
            is_resolution: None,
            author_actor_id: alice().actor_id().to_hex(),
            device_id: "d2".repeat(32),
            ..Default::default()
        }))
        .unwrap();
        db.retain_manifest(&manifest_hex, &sealed.manifest_bytes, PATH, Some(1))
            .unwrap();
    }
    let alice_node = bind_seat(&listeners, &alice()).await;
    refresh_serve_sources(
        &alice_node,
        &[SharedSetSpec {
            folder: "holiday".to_string(),
            folder_id: "local:1".to_string(),
            set_id: SET,
            db_path: alice_db_path,
            body: TreeBodySource::shared(alice_dir.path()),
            landing: Landing::Resident,
            content_keys: keys(),
        }],
    );
    let alice_code = PeerCode {
        actor: alice().actor_id(),
        lan_endpoints: Vec::new(),
    };
    let served = |path: &str| {
        let tally = share_serve_tally::snapshot();
        (
            tally.manifests.get(path).copied().unwrap_or(0),
            tally.chunks.get(path).copied().unwrap_or(0),
        )
    };

    // The control: a member's probe reaches the row and the manifest.
    let bob_node = bind_seat(&listeners, &bob()).await;
    let member = probe_set(&bob_node, &alice_code, SET, &[]).await;
    assert!(member.dialed && member.admitted, "member probe: {member:?}");
    assert_eq!(member.rows, 1, "member probe: {member:?}");
    assert_eq!(
        member.paths,
        vec![PATH.to_string()],
        "member probe: {member:?}"
    );
    assert_eq!(member.manifests, 1, "member probe: {member:?}");
    assert_eq!(member.manifest_hashes, vec![manifest_hex.clone()]);
    let after_member = served(PATH);
    assert_eq!(after_member.0, 1, "the member's manifest answer is counted");

    // The stranger, handed the exact manifest hash the member saw.
    let mallory = ActorKeypair::from_secret([41u8; 32]);
    let mallory_node = bind_seat(&listeners, &mallory).await;
    let stranger = probe_set(&mallory_node, &alice_code, SET, &[manifest_hex]).await;
    assert!(
        stranger.dialed,
        "the stranger must reach the door: {stranger:?}"
    );
    assert!(!stranger.admitted, "{stranger:?}");
    assert!(stranger.admit_error.is_some(), "{stranger:?}");
    assert_eq!(stranger.rows, 0, "{stranger:?}");
    assert!(stranger.paths.is_empty(), "{stranger:?}");
    assert!(stranger.rows_error.is_some(), "{stranger:?}");
    assert_eq!(stranger.manifests, 0, "{stranger:?}");
    assert_eq!(stranger.manifest_errors.len(), 1, "{stranger:?}");
    assert_eq!(
        served(PATH),
        after_member,
        "nothing was served to the stranger"
    );
}

/// **The offline half — B2.5.** A file the author minted while the nest was
/// unreachable exists ONLY as a pending row (`seq IS NULL`) plus plaintext on
/// the author's own disk: no nest sequence number, so its only writer proof is
/// its own signature (when its host signs) or the channel's. It must still cross to an
/// admitted member, because crossing without the nest is the entire reason
/// the plane exists (`p2p.md` § Cross-user shared-set transfer).
///
/// The sibling above proves the SEQUENCED path; every assertion it makes
/// holds for a row that already carries a nest coordinate. This one walks
/// the path the two-actor journey's step 6 walks, and was — until it was
/// written — the only step of that journey with no witness below a
/// ~35-minute two-GUI e2e. That gap is why three real runs of the journey
/// could report "NEVER ARRIVED" without naming a link.
///
/// Two things separate it from its sibling, and both are precisely what the
/// offline wire form changes:
///
/// - the row serves on the TAIL page only (it has no cursor position), so it
///   is reached through `changes_since`'s pending leg, not its sequenced one;
/// - it carries no nest stamp, so the receiver's `judge_peer_row` admits it
///   on its writer signature alone — an unsigned one is refused.
#[tokio::test]
async fn an_offline_authored_pending_file_arrives_at_an_admitted_member() {
    let listeners = fauna_transport::testing::listeners();

    // ── Alice's seat: the cabin write. Small on purpose — the multi-chunk
    // ranged pull is the sibling's contract; this test is about the row's
    // PROVENANCE shape, and a small body keeps the witness quick enough to
    // sit in a gate.
    let alice_dir = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = b"authored in the cabin, with the nest unreachable\n".to_vec();
    std::fs::write(alice_dir.path().join("cabin-draft.txt"), &bytes).unwrap();
    let sealed = fauna_sync_engine::seal::seal_blob(&bytes, Some((CONTENT_KEY, Some(1)))).unwrap();

    let alice_db_path = alice_dir.path().join("fs-holiday.db");
    {
        let db = SyncDb::open(&alice_db_path).unwrap();
        // The offline mint: NO seq. `mint_pending_own_change` is the only
        // door to this shape — `retain_own_change` rejects a seq-less row.
        db.mint_pending_own_change(&signed_by_alice(fauna_sync_engine::db::OwnChangeRow {
            seq: None,
            path: "cabin-draft.txt".to_string(),
            path_hash: hex::encode(fauna_core::sync::path_hash("cabin-draft.txt")),
            path_sealed: None,
            manifest_hash: Some(hex::encode(sealed.manifest_hash.digest())),
            size_bytes: bytes.len() as i64,
            change_type: "create".to_string(),
            created_at: 1_700_000_500_000,
            content_key_version: Some(1),
            thumbnail_hash: None,
            derived_through: None,
            is_resolution: None,
            author_actor_id: alice().actor_id().to_hex(),
            device_id: "d2".repeat(32),
            ..Default::default()
        }))
        .unwrap();
        db.retain_manifest(
            &hex::encode(sealed.manifest_hash.digest()),
            &sealed.manifest_bytes,
            "cabin-draft.txt",
            Some(1),
        )
        .unwrap();
    }

    let alice_node = bind_seat(&listeners, &alice()).await;
    let routed = refresh_serve_sources(
        &alice_node,
        &[SharedSetSpec {
            folder: "holiday".to_string(),
            folder_id: "local:1".to_string(),
            set_id: SET,
            db_path: alice_db_path.clone(),
            body: TreeBodySource::shared(alice_dir.path()),
            landing: Landing::Resident,
            content_keys: keys(),
        }],
    );
    assert_eq!(routed, 1);

    // ── Bob's seat, exactly as the sibling builds it: Alice cached as a
    // WRITER, which for a STAMPLESS row is not a formality but the whole
    // attribution — refuse it and the row is unattributable.
    let bob_dir = tempfile::tempdir().unwrap();
    let bob_db_path = bob_dir.path().join("fs-holiday.db");
    {
        let db = SyncDb::open(&bob_db_path).unwrap();
        db.cache_share_writer_roster(&[(alice().actor_id().to_hex(), true)], &[], None)
            .unwrap();
    }
    let bob_node = bind_seat(&listeners, &bob()).await;
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(2);
    let loop_fut = fauna_sync_engine::always_resident::run_watch_loop(
        engine_over(
            bob_dir.path().to_path_buf(),
            SyncDb::open(&bob_db_path).unwrap(),
        ),
        bob_dir.path().to_path_buf(),
        "holiday".to_string(),
        std::time::Duration::from_secs(3600),
        None,
        Some(cmd_rx),
    );
    tokio::pin!(loop_fut);
    let door = ChannelDoor { cmd_tx };
    let membership: Arc<dyn SetMembership + Send + Sync> = Arc::new(HolidayRoster);
    let spool_root = tempfile::tempdir().unwrap();
    let mut ledger = fauna_sync_engine::share_pump::TransferUsageLedger::default();
    let policy = tier1_policy();

    let bob_spec = SharedSetSpec {
        folder: "holiday".to_string(),
        folder_id: "local:1".to_string(),
        set_id: SET,
        db_path: bob_db_path.clone(),
        body: TreeBodySource::shared(bob_dir.path()),
        landing: Landing::Resident,
        content_keys: keys(),
    };
    let alice_target = PeerDialTarget {
        node_id: alice().actor_id().0,
        candidates: PathCandidates::default(),
        relay_url: None,
    };
    let pump = pull_set_from_peer(
        &bob_node,
        &membership,
        &door,
        &[SET],
        &bob_spec,
        &alice_target,
        spool_root.path(),
        &policy,
        &mut ledger,
        TODAY,
        NestPath::Reachable,
    );
    let outcome = tokio::select! {
        _ = &mut loop_fut => panic!("the resident loop must not exit"),
        got = tokio::time::timeout(PUMP_CEILING, pump) => got
            .expect("the pull must finish well inside the ceiling")
            .expect("pull"),
    };

    assert!(
        outcome.admitted,
        "the mutual M2 admission let the set through"
    );
    assert_eq!(
        outcome.rows_accepted, 1,
        "the stampless pending row is attributable to the channel-proven \
         peer and must be ACCEPTED, not refused: {outcome:?}"
    );
    assert_eq!(
        outcome.materialized, 1,
        "an accepted offline row must reach the tree, not stop at the \
         provisional plane: {outcome:?}"
    );
    assert_eq!(
        std::fs::read(bob_dir.path().join("cabin-draft.txt")).unwrap(),
        bytes,
        "the cabin draft arrived byte-identical, with no nest in the story"
    );
    // A pending row has no nest coordinate, so it can move no cursor — the
    // tail it serves on is reached by every pass, which is exactly how a
    // truncated pending set drains later (`changes_since`'s own contract).
    assert_eq!(
        outcome.cursor, 0,
        "an own-pending row carries no sequence and must not advance the \
         puller's cursor: {outcome:?}"
    );
}

/// **The fail-closed arm's SIGNATURE** — and why a caller must read
/// `rows_refused`, not the outcome's mere existence.
///
/// `judge_peer_row` consults the cached writer roster before it weighs a row
/// at all, so a receiver whose roster cache is empty for the serving peer
/// refuses EVERY row that peer serves — sequenced and own-pending alike. That
/// is the designed, correct posture (no cached role ⇒ refuse; the next online
/// roster read heals it). What is NOT designed is how closely the resulting
/// outcome resembles a healthy, quiet pass: same `admitted: true`, same zero
/// `rows_accepted`, same zero `materialized`, same absent file. The ONE field
/// that separates "I refused everything you served" from "you had nothing new
/// for me" is `rows_refused`.
///
/// This matters beyond the pump. The two-actor journey's step-3 peer barrier
/// asserts a `share-transfer-item` row exists, and its comment claims such a
/// row appears "only after a completed peer pull … page succeeded". This test
/// is the counter-example: an admitted pull that transfers NOTHING still
/// yields an outcome, and an outcome is exactly what mints that row. So the
/// barrier passes on dial + admission alone, and cannot do the job it was
/// added for — guaranteeing that stopping the nest will not strand the test.
#[tokio::test]
async fn an_uncached_writer_is_refused_wholesale_and_looks_like_a_quiet_pass() {
    let listeners = fauna_transport::testing::listeners();

    let alice_dir = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = b"served, but never attributable\n".to_vec();
    std::fs::write(alice_dir.path().join("cabin-draft.txt"), &bytes).unwrap();
    let sealed = fauna_sync_engine::seal::seal_blob(&bytes, Some((CONTENT_KEY, Some(1)))).unwrap();

    let alice_db_path = alice_dir.path().join("fs-holiday.db");
    {
        let db = SyncDb::open(&alice_db_path).unwrap();
        db.mint_pending_own_change(&signed_by_alice(fauna_sync_engine::db::OwnChangeRow {
            seq: None,
            path: "cabin-draft.txt".to_string(),
            path_hash: hex::encode(fauna_core::sync::path_hash("cabin-draft.txt")),
            path_sealed: None,
            manifest_hash: Some(hex::encode(sealed.manifest_hash.digest())),
            size_bytes: bytes.len() as i64,
            change_type: "create".to_string(),
            created_at: 1_700_000_500_000,
            content_key_version: Some(1),
            thumbnail_hash: None,
            derived_through: None,
            is_resolution: None,
            author_actor_id: alice().actor_id().to_hex(),
            device_id: "d2".repeat(32),
            ..Default::default()
        }))
        .unwrap();
        db.retain_manifest(
            &hex::encode(sealed.manifest_hash.digest()),
            &sealed.manifest_bytes,
            "cabin-draft.txt",
            Some(1),
        )
        .unwrap();
    }

    let alice_node = bind_seat(&listeners, &alice()).await;
    assert_eq!(
        refresh_serve_sources(
            &alice_node,
            &[SharedSetSpec {
                folder: "holiday".to_string(),
                folder_id: "local:1".to_string(),
                set_id: SET,
                db_path: alice_db_path.clone(),
                body: TreeBodySource::shared(alice_dir.path()),
                landing: Landing::Resident,
                content_keys: keys(),
            }],
        ),
        1
    );

    // Bob's seat — identical to the test above in EVERY respect but one: his
    // roster cache was never filled for Alice (the offline-from-cold case,
    // and the case a roster read that never succeeded leaves behind).
    let bob_dir = tempfile::tempdir().unwrap();
    let bob_db_path = bob_dir.path().join("fs-holiday.db");
    let bob_node = bind_seat(&listeners, &bob()).await;
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(2);
    let loop_fut = fauna_sync_engine::always_resident::run_watch_loop(
        engine_over(
            bob_dir.path().to_path_buf(),
            SyncDb::open(&bob_db_path).unwrap(),
        ),
        bob_dir.path().to_path_buf(),
        "holiday".to_string(),
        std::time::Duration::from_secs(3600),
        None,
        Some(cmd_rx),
    );
    tokio::pin!(loop_fut);
    let door = ChannelDoor { cmd_tx };
    let membership: Arc<dyn SetMembership + Send + Sync> = Arc::new(HolidayRoster);
    let spool_root = tempfile::tempdir().unwrap();
    let mut ledger = fauna_sync_engine::share_pump::TransferUsageLedger::default();
    let policy = tier1_policy();

    let bob_spec = SharedSetSpec {
        folder: "holiday".to_string(),
        folder_id: "local:1".to_string(),
        set_id: SET,
        db_path: bob_db_path.clone(),
        body: TreeBodySource::shared(bob_dir.path()),
        landing: Landing::Resident,
        content_keys: keys(),
    };
    let alice_target = PeerDialTarget {
        node_id: alice().actor_id().0,
        candidates: PathCandidates::default(),
        relay_url: None,
    };
    let pump = pull_set_from_peer(
        &bob_node,
        &membership,
        &door,
        &[SET],
        &bob_spec,
        &alice_target,
        spool_root.path(),
        &policy,
        &mut ledger,
        TODAY,
        NestPath::Reachable,
    );
    let outcome = tokio::select! {
        _ = &mut loop_fut => panic!("the resident loop must not exit"),
        got = tokio::time::timeout(PUMP_CEILING, pump) => got
            .expect("the pull must finish well inside the ceiling")
            .expect("pull"),
    };

    // Admission is a SET-level verdict and is unaffected — which is the whole
    // trap: the peer let us in, then we refused everything they handed us.
    assert!(
        outcome.admitted,
        "the roster consult refuses ROWS, never the admission: {outcome:?}"
    );
    // The row is signed, so it passes the PAGE screen (whose cached-writer
    // consult is the unsigned row's channel proof) and is refused at the
    // engine's door, where the SIGNED actor must be a cached writer.
    assert_eq!(outcome.rows_accepted, 1, "{outcome:?}");
    assert_eq!(outcome.materialized, 0, "{outcome:?}");
    assert!(
        !bob_dir.path().join("cabin-draft.txt").exists(),
        "an unattributable row must not reach the tree"
    );
    // The one witness that this was a REFUSAL and not an empty peer. A
    // consumer reading only `admitted`, `materialized`, or the row's
    // existence cannot tell these two states apart.
    assert_eq!(
        outcome.rows_refused, 1,
        "the refusal must be COUNTED — it is the only field distinguishing \
         a wholesale refusal from a peer with nothing to serve: {outcome:?}"
    );
}

// ── The on-demand seat (`p2p-shared-set-build.md` § *Phone peers — design*,
// decision 5): one seat over a bound tree, one over an on-demand replica ────

fn carol() -> ActorKeypair {
    ActorKeypair::from_secret([51u8; 32])
}

/// The cabin's roster: Alice wrote, Bob's phone carries, Carol arrives later.
struct CabinRoster;
impl SetMembership for CabinRoster {
    fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
        channel_id == &SET
            && [alice(), bob(), carol()]
                .iter()
                .any(|member| member.actor_id() == *actor)
    }
}

async fn bind_cabin_seat(
    listeners: &fauna_transport::testing::Listeners,
    who: &ActorKeypair,
) -> CeremonyNode {
    let transport = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(who.actor_id().0),
        listeners: Arc::clone(listeners),
    });
    let node = CeremonyNode::bind_with_share_plane(
        CeremonyBindVerdict::Bind,
        transport,
        who.actor_id(),
        "seat".into(),
        Arc::new(Mutex::new(GroupShareConfig::default())),
        Arc::new(|| Timestamp(1_700_000_000)),
        Arc::new(|| {}),
        Arc::new(CabinRoster),
    )
    .await
    .expect("bind");
    await_listening(listeners, &who.actor_id().0).await;
    node
}

type OnDemandIngest = (
    String,
    Vec<Vec<u8>>,
    PathBuf,
    tokio::sync::oneshot::Sender<anyhow::Result<ShareIngestSummary>>,
);

/// The on-demand host's worker, as the FFI host runs it: the replica's one
/// writer owns the engine and the tree, and answers the door's requests one
/// at a time through [`OwnedTree::share_ingest`].
///
/// [`OwnedTree::share_ingest`]: fauna_sync_engine::provider_face::owned_tree::OwnedTree::share_ingest
async fn on_demand_worker(
    tree: fauna_sync_engine::provider_face::owned_tree::OwnedTree,
    engine: fauna_sync_engine::engine::SyncEngine,
    mut requests: tokio::sync::mpsc::Receiver<OnDemandIngest>,
) {
    while let Some((proven, rows, spool, reply)) = requests.recv().await {
        let out =
            tree.share_ingest(&engine, &proven, &rows, spool)
                .await
                .map(|(report, cursor)| ShareIngestSummary {
                    refused: report.refused as u32,
                    overlaid: report.overlaid as u32,
                    materialized: report.materialized as u32,
                    already_current: report.already_current as u32,
                    cursor,
                    storage_limited: report.storage_limited,
                });
        let _ = reply.send(out);
    }
}

/// The door to that worker. `cut` drops the next hand-off on the floor — a
/// transfer interrupted after its bytes crossed and before anything landed.
struct OnDemandDoor {
    requests: tokio::sync::mpsc::Sender<OnDemandIngest>,
    cut: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl ShareIngestDoor for OnDemandDoor {
    async fn ingest(
        &self,
        _folder: &str,
        _folder_id: &str,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &std::path::Path,
    ) -> anyhow::Result<ShareIngestSummary> {
        if self.cut.swap(false, std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("the app left the foreground mid-transfer");
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.requests
            .send((
                proven_actor_hex.to_string(),
                rows,
                spool_dir.to_path_buf(),
                reply,
            ))
            .await
            .map_err(|_| anyhow::anyhow!("host worker gone"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("worker dropped reply"))?
    }
}

fn own_row(
    seq: Option<i64>,
    path: &str,
    sealed: &fauna_sync_engine::seal::SealedBlob,
    size: usize,
) -> fauna_sync_engine::db::OwnChangeRow {
    signed_by_alice(fauna_sync_engine::db::OwnChangeRow {
        seq,
        path: path.to_string(),
        path_hash: hex::encode(fauna_core::sync::path_hash(path)),
        path_sealed: None,
        manifest_hash: Some(hex::encode(sealed.manifest_hash.digest())),
        size_bytes: size as i64,
        change_type: "create".to_string(),
        created_at: 1_700_000_500_000,
        content_key_version: Some(1),
        thumbnail_hash: None,
        derived_through: None,
        is_resolution: None,
        author_actor_id: alice().actor_id().to_hex(),
        device_id: "d2".repeat(32),
        ..Default::default()
    })
}

/// **A phone as a partial peer.** Alice's desktop (a bound tree) holds one
/// file the nest recorded and one she wrote with the nest unreachable. Bob's
/// phone (an on-demand replica) pulls from her, and Carol's desktop then pulls
/// from Bob's phone:
///
/// - the un-sequenced body lands in Bob's **kept root** — peers are its only
///   source — and the sequenced row lands **no body**, only a placeholder;
/// - a pull cut before anything landed leaves the cursor at rest, the next
///   one lands the page, and the one after re-fetches nothing that landed;
/// - Bob's phone **serves the kept-root body back** to a third puller, and
///   answers **none** for the placeholder's bytes without failing her pull —
///   the honest partial seed.
#[tokio::test]
async fn an_on_demand_seat_lands_the_unsequenced_body_and_serves_it_to_a_third_pull() {
    use fauna_sync_engine::provider_face::owned_tree::OwnedTree;
    use fauna_sync_engine::share_body::OnDemandBodySource;

    let listeners = fauna_transport::testing::listeners();
    let membership: Arc<dyn SetMembership + Send + Sync> = Arc::new(CabinRoster);
    let policy = tier1_policy();

    // ── Alice's desktop: a bound tree, one recorded file, one cabin draft.
    let alice_dir = tempfile::tempdir().unwrap();
    let recorded: Vec<u8> = b"recorded while the nest was reachable\n".to_vec();
    let draft: Vec<u8> = b"authored in the cabin, with the nest unreachable\n".to_vec();
    std::fs::write(alice_dir.path().join("recorded.txt"), &recorded).unwrap();
    std::fs::write(alice_dir.path().join("cabin-draft.txt"), &draft).unwrap();
    let seal = |bytes: &[u8]| {
        fauna_sync_engine::seal::seal_blob(bytes, Some((CONTENT_KEY, Some(1)))).unwrap()
    };
    let (recorded_sealed, draft_sealed) = (seal(&recorded), seal(&draft));
    let alice_db_path = alice_dir.path().join("fs-holiday.db");
    {
        let db = SyncDb::open(&alice_db_path).unwrap();
        db.retain_own_change(&own_row(
            Some(4),
            "recorded.txt",
            &recorded_sealed,
            recorded.len(),
        ))
        .unwrap();
        db.mint_pending_own_change(&own_row(
            None,
            "cabin-draft.txt",
            &draft_sealed,
            draft.len(),
        ))
        .unwrap();
        for (path, sealed) in [
            ("recorded.txt", &recorded_sealed),
            ("cabin-draft.txt", &draft_sealed),
        ] {
            db.retain_manifest(
                &hex::encode(sealed.manifest_hash.digest()),
                &sealed.manifest_bytes,
                path,
                Some(1),
            )
            .unwrap();
        }
    }
    let alice_node = bind_cabin_seat(&listeners, &alice()).await;
    assert_eq!(
        refresh_serve_sources(
            &alice_node,
            &[SharedSetSpec {
                folder: "holiday".to_string(),
                folder_id: "local:1".to_string(),
                set_id: SET,
                db_path: alice_db_path.clone(),
                body: TreeBodySource::shared(alice_dir.path()),
                landing: Landing::Resident,
                content_keys: keys(),
            }],
        ),
        1
    );

    // ── Bob's phone: an on-demand replica — two roots, the engine over the
    // kept root, and the host's worker as the set's one writer.
    let bob_dir = tempfile::tempdir().unwrap();
    let (kept, cache) = (bob_dir.path().join("kept"), bob_dir.path().join("cache"));
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let bob_db_path = bob_dir.path().join("fsid-local-1.db");
    {
        let db = SyncDb::open(&bob_db_path).unwrap();
        db.cache_share_writer_roster(&[(alice().actor_id().to_hex(), true)], &[], None)
            .unwrap();
    }
    let bob_node = bind_cabin_seat(&listeners, &bob()).await;
    let (requests, rx) = tokio::sync::mpsc::channel(2);
    let worker = on_demand_worker(
        OwnedTree::new(kept.clone(), cache.clone()),
        engine_over_inner(kept.clone(), SyncDb::open(&bob_db_path).unwrap()),
        rx,
    );
    tokio::pin!(worker);
    let door = OnDemandDoor {
        requests,
        cut: std::sync::atomic::AtomicBool::new(true),
    };
    let bob_spec = || SharedSetSpec {
        folder: "holiday".to_string(),
        folder_id: "local:1".to_string(),
        set_id: SET,
        db_path: bob_db_path.clone(),
        body: Arc::new(OnDemandBodySource::new(
            kept.clone(),
            cache.clone(),
            SyncDb::open(&bob_db_path).unwrap(),
        )),
        landing: Landing::OnDemand {
            kept_root: kept.clone(),
        },
        content_keys: keys(),
    };
    let alice_target = PeerDialTarget {
        node_id: alice().actor_id().0,
        candidates: PathCandidates::default(),
        relay_url: None,
    };
    let bob_spool = tempfile::tempdir().unwrap();
    let mut ledger = fauna_sync_engine::share_pump::TransferUsageLedger::default();
    let bob_cursor = || {
        SyncDb::open(&bob_db_path)
            .unwrap()
            .share_pull_cursor(&alice().actor_id().to_hex())
            .unwrap()
    };
    let bob_state = |rel: &str| {
        SyncDb::open(&bob_db_path)
            .unwrap()
            .get_entry(rel)
            .unwrap()
            .map(|e| e.state)
    };

    // Pull 1 — cut: the bytes crossed, nothing landed, the cursor is at rest.
    {
        let spec = bob_spec();
        let pump = pull_set_from_peer(
            &bob_node,
            &membership,
            &door,
            &[SET],
            &spec,
            &alice_target,
            bob_spool.path(),
            &policy,
            &mut ledger,
            TODAY,
            NestPath::Reachable,
        );
        let cut = tokio::select! {
            _ = &mut worker => panic!("the host worker must not exit"),
            got = tokio::time::timeout(PUMP_CEILING, pump) => got
                .expect("the pull must finish well inside the ceiling"),
        };
        assert!(cut.is_err(), "the cut pull reports its failure");
    }
    assert_eq!(bob_cursor(), 0, "a cut pull leaves the cursor at rest");
    assert!(!kept.join("cabin-draft.txt").exists());

    // Pull 2 — the app is back in front: the page lands.
    let outcome = {
        let spec = bob_spec();
        let pump = pull_set_from_peer(
            &bob_node,
            &membership,
            &door,
            &[SET],
            &spec,
            &alice_target,
            bob_spool.path(),
            &policy,
            &mut ledger,
            TODAY,
            NestPath::Reachable,
        );
        tokio::select! {
            _ = &mut worker => panic!("the host worker must not exit"),
            got = tokio::time::timeout(PUMP_CEILING, pump) => got
                .expect("the pull must finish well inside the ceiling")
                .expect("pull"),
        }
    };
    assert!(outcome.admitted);
    assert_eq!(
        outcome.rows_accepted, 2,
        "both rows are recorded: {outcome:?}"
    );
    assert_eq!(
        outcome.materialized, 1,
        "only the un-sequenced body lands: {outcome:?}"
    );
    assert_eq!(
        std::fs::read(kept.join("cabin-draft.txt")).unwrap(),
        draft,
        "the cabin draft landed in the KEPT root, byte-identical"
    );
    assert!(!cache.join("cabin-draft.txt").exists());
    assert_eq!(
        bob_state("cabin-draft.txt"),
        Some(fauna_sync_engine::db::SyncState::Synced)
    );
    assert!(
        !kept.join("recorded.txt").exists() && !cache.join("recorded.txt").exists(),
        "a sequenced row lands no body on an on-demand replica"
    );
    assert_eq!(
        bob_state("recorded.txt"),
        Some(fauna_sync_engine::db::SyncState::Placeholder),
        "it lists as a placeholder"
    );
    assert_eq!(outcome.cursor, 4);
    assert_eq!(bob_cursor(), 4, "the state writer owns the cursor");

    // Pull 3 — nothing new: nothing that landed is fetched again.
    let spent_before = fauna_core::encoding::canonical_encode(&ledger).unwrap();
    let again = {
        let spec = bob_spec();
        let pump = pull_set_from_peer(
            &bob_node,
            &membership,
            &door,
            &[SET],
            &spec,
            &alice_target,
            bob_spool.path(),
            &policy,
            &mut ledger,
            TODAY,
            NestPath::Reachable,
        );
        tokio::select! {
            _ = &mut worker => panic!("the host worker must not exit"),
            got = tokio::time::timeout(PUMP_CEILING, pump) => got
                .expect("the pull must finish well inside the ceiling")
                .expect("pull"),
        }
    };
    assert_eq!(again.materialized, 0, "{again:?}");
    assert_eq!(again.cursor, 4);
    assert_eq!(
        fauna_core::encoding::canonical_encode(&ledger).unwrap(),
        spent_before,
        "a resumed pull moves no byte that already landed"
    );

    // ── Bob's phone serves: the kept-root body, and none for the placeholder.
    assert_eq!(refresh_serve_sources(&bob_node, &[bob_spec()]), 1);

    // ── Carol's desktop pulls from Bob's phone — Alice is away.
    let carol_dir = tempfile::tempdir().unwrap();
    let carol_db_path = carol_dir.path().join("fs-holiday.db");
    {
        let db = SyncDb::open(&carol_db_path).unwrap();
        db.cache_share_writer_roster(
            &[
                (alice().actor_id().to_hex(), true),
                (bob().actor_id().to_hex(), true),
            ],
            &[],
            None,
        )
        .unwrap();
    }
    let carol_node = bind_cabin_seat(&listeners, &carol()).await;
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(2);
    let carol_loop = fauna_sync_engine::always_resident::run_watch_loop(
        engine_over(
            carol_dir.path().to_path_buf(),
            SyncDb::open(&carol_db_path).unwrap(),
        ),
        carol_dir.path().to_path_buf(),
        "holiday".to_string(),
        std::time::Duration::from_secs(3600),
        None,
        Some(cmd_rx),
    );
    tokio::pin!(carol_loop);
    let carol_door = ChannelDoor { cmd_tx };
    let carol_spec = SharedSetSpec {
        folder: "holiday".to_string(),
        folder_id: "local:1".to_string(),
        set_id: SET,
        db_path: carol_db_path.clone(),
        body: TreeBodySource::shared(carol_dir.path()),
        landing: Landing::Resident,
        content_keys: keys(),
    };
    let bob_target = PeerDialTarget {
        node_id: bob().actor_id().0,
        candidates: PathCandidates::default(),
        relay_url: None,
    };
    let carol_spool = tempfile::tempdir().unwrap();
    let mut carol_ledger = fauna_sync_engine::share_pump::TransferUsageLedger::default();
    let pump = pull_set_from_peer(
        &carol_node,
        &membership,
        &carol_door,
        &[SET],
        &carol_spec,
        &bob_target,
        carol_spool.path(),
        &policy,
        &mut carol_ledger,
        TODAY,
        NestPath::Reachable,
    );
    let served = tokio::select! {
        _ = &mut carol_loop => panic!("the resident loop must not exit"),
        got = tokio::time::timeout(PUMP_CEILING, pump) => got
            .expect("the pull must finish well inside the ceiling")
            .expect("a placeholder the phone cannot serve must not fail the pull"),
    };
    assert!(served.admitted);
    assert_eq!(served.rows_accepted, 2, "both rows relay: {served:?}");
    assert_eq!(
        served.materialized, 1,
        "the phone serves the body it holds, and only that one: {served:?}"
    );
    assert_eq!(
        std::fs::read(carol_dir.path().join("cabin-draft.txt")).unwrap(),
        draft,
        "the cabin draft reached a third device from the phone's kept root"
    );
    assert!(
        !carol_dir.path().join("recorded.txt").exists(),
        "the placeholder answered none — the phone never hydrates on a peer's behalf"
    );
}
