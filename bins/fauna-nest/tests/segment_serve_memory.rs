//! **tier_3** — the side of a segment transfer that SERVES or PUSHES a segment
//! holds a constant piece in memory, never the segment.
//!
//! `message-segment-store.md` § Segment size: a segment has no size ceiling (it
//! rolls on its month bucket, never on size), so a nest that reads a half whole
//! to serve or back it up allocates whatever one scope wrote in one month —
//! once per request, or once per backup pass — in the process every other user
//! of the nest shares. The keeping side has streamed since 2026-09-29
//! (`libs/fauna-sync-engine/tests/segment_transfer_memory.rs`); this file pins
//! the source side:
//!
//! - the nest's `GET /api/v1/segments/{kind}/{actor}/{id}` route, whole and by
//!   `Range`, streams the file off disk (`fauna_nest::http_range::serve_file`),
//!   for the owner and for the custodied-fleet arm (an owner-fleet device
//!   fetching a pair the custodian nest holds for its owner);
//! - the nest-side backup pass seals each half off its open file a chunk at a
//!   time and uploads off the staged bodies (`SyncEngine::seal_staged` +
//!   `upload_staged`), so its constant is one sealed chunk.
//!
//! And the nest that keeps a segment back out of its custody — both restore
//! verbs, re-seed materialize and the lived-in recovery — reassembles each
//! half into a staging file a window of chunks at a time, verifies it by a
//! streaming hash and adopts it by a streaming copy (`backup::pair_stage`), so
//! its constant is the download window's chunks, never the segment.
//!
//! Each test measures the **largest single allocation** the process makes
//! while the path runs: a whole-half buffer anywhere on it is one allocation at
//! least the half's size, so it cannot hide under the ceiling. It is its own
//! test binary because it installs a global allocator, and its tests run one
//! at a time ([`SERIAL`]) so none of them measures another's allocations.

mod common;
use common::backup_nests::*;
use common::register_user;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fauna_core::crypto::NestBackupKey;
use fauna_core::identity::ActorId;
use fauna_nest::auth::BearerAuth;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::segment_backup::NestBackupCoordinator;
use fauna_nest::segments::segment_route::get_segment;
use fauna_protocol::backup::{CustodyMaterializeReply, CustodyMaterializeRequest};
use fauna_segment_store::SegmentManager;
use futures_util::StreamExt;

// ── the allocation counter ───────────────────────────────────────────────────

struct PeakAlloc;

static TRACKING: AtomicBool = AtomicBool::new(false);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn note(size: usize) {
    if TRACKING.load(Ordering::Relaxed) {
        PEAK.fetch_max(size, Ordering::Relaxed);
    }
}

// SAFETY: every call forwards to `System` unchanged; the counter only reads
// the requested size.
unsafe impl GlobalAlloc for PeakAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: PeakAlloc = PeakAlloc;

/// One test at a time: the counter is process-wide.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn start_tracking() {
    PEAK.store(0, Ordering::Relaxed);
    TRACKING.store(true, Ordering::Relaxed);
}

fn stop_tracking() -> usize {
    TRACKING.store(false, Ordering::Relaxed);
    PEAK.load(Ordering::Relaxed)
}

// ── the fixture ──────────────────────────────────────────────────────────────

const ACTOR: [u8; 32] = [0xAB; 32];

/// The largest single allocation a serve may make, whatever the segment's
/// size. Generous against every constant buffer on the path (the route's read
/// piece, `fauna_nest::http_range::FILE_SERVE_PIECE`), and an eighth of the
/// segment below, so a whole-half buffer cannot fit under it.
const CEILING: usize = 1024 * 1024;
const RECORD_LEN: usize = 32 * 1024;
const RECORDS: usize = 256; // 8 MiB of records: eight times the ceiling

/// The backup pass seals what it pushes, and the seal's unit is a **chunk** —
/// up to `MAX_CHUNK` (8 MiB) by the content-addressed format — so its constant
/// is one chunk plus headroom (the client custodian's ceiling,
/// `segment_transfer_memory.rs`), and the fixture is several times it.
const BACKUP_CEILING: usize = 9 * 1024 * 1024;
const BACKUP_RECORDS: usize = 1536; // 48 MiB of records: over five times the ceiling

/// Record `i`'s body — unique per index, since identity is the content hash.
fn record(i: usize) -> Vec<u8> {
    let mut body = vec![(i % 251) as u8; RECORD_LEN];
    body[..8].copy_from_slice(&(i as u64).to_be_bytes());
    body
}

/// Append `records` records for `actor` into one bucket of `mgr` and finalize
/// the segment; returns its id.
async fn write_segment(mgr: &SegmentManager, actor: &[u8; 32], records: usize) -> u32 {
    let mut seg_id = None;
    for i in 0..records {
        let body = record(i);
        let outcome = mgr
            .append_record_with_bucket(
                actor,
                fauna_cbor::Cid::of_dag_cbor(&body),
                &body,
                b"",
                "2026-09",
            )
            .await
            .expect("append");
        seg_id = Some(outcome.segment_id);
    }
    mgr.finalize_open(actor).await.expect("finalize");
    seg_id.expect("at least one record")
}

/// A nest whose `post` store lives in `tmp`, holding one finalized segment of
/// [`RECORDS`] records for [`ACTOR`].
async fn serving_nest() -> (tempfile::TempDir, Arc<AppState>, u32, Vec<u8>) {
    let tmp = tempfile::tempdir().unwrap();
    let mut state = AppState::for_test(Arc::new(CacheDb::open_in_memory().unwrap()));
    state.post_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "post"));
    let seg_id = write_segment(&state.post_segments, &ACTOR, RECORDS).await;
    let dat = std::fs::read(state.post_segments.segment_file_path(&ACTOR, seg_id)).unwrap();
    assert!(
        dat.len() >= 8 * CEILING,
        "the fixture must dwarf the ceiling"
    );
    (tmp, Arc::new(state), seg_id, dat)
}

/// Call the route as the owner and drain its body piece by piece, as a client
/// does; returns the status, the `Content-Range` (if any) and the body's hash.
async fn fetch(
    state: Arc<AppState>,
    seg_id: u32,
    headers: HeaderMap,
) -> (StatusCode, Option<String>, usize, blake3::Hash) {
    let resp: Response = get_segment(
        State(state),
        BearerAuth(ActorId(ACTOR)),
        headers,
        Path(("post".to_string(), hex::encode(ACTOR), seg_id)),
    )
    .await
    .into_response();
    let status = resp.status();
    let range = resp
        .headers()
        .get(header::CONTENT_RANGE)
        .map(|v| v.to_str().unwrap().to_string());
    let mut body = resp.into_body().into_data_stream();
    let mut hasher = blake3::Hasher::new();
    let mut len = 0;
    while let Some(piece) = body.next().await {
        let piece = piece.expect("body piece");
        len += piece.len();
        hasher.update(&piece);
    }
    (status, range, len, hasher.finalize())
}

// ── the serving side ─────────────────────────────────────────────────────────

/// The route serves an 8 MiB segment whole without one allocation near its
/// size, and what it serves is the file.
#[tokio::test]
async fn the_segment_route_streams_a_whole_segment() {
    let _serial = SERIAL.lock().await;
    let (_tmp, state, seg_id, dat) = serving_nest().await;

    start_tracking();
    let (status, _, len, hash) = fetch(state, seg_id, HeaderMap::new()).await;
    let peak = stop_tracking();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(len, dat.len());
    assert_eq!(hash, blake3::hash(&dat), "the route serves the file");
    assert!(
        peak < CEILING,
        "serving a {}-byte segment made a {peak}-byte allocation — the half was read into \
         memory instead of streamed off disk (ceiling {CEILING})",
        dat.len()
    );
}

/// A resumed fetch — a `Range` from a quarter of the way in — is served off the
/// file at its offset, not sliced out of a whole-file read.
#[tokio::test]
async fn the_segment_route_streams_a_range() {
    let _serial = SERIAL.lock().await;
    let (_tmp, state, seg_id, dat) = serving_nest().await;
    let start = dat.len() / 4;
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, format!("bytes={start}-").parse().unwrap());

    start_tracking();
    let (status, range, len, hash) = fetch(state, seg_id, headers).await;
    let peak = stop_tracking();

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        range.as_deref(),
        Some(format!("bytes {start}-{}/{}", dat.len() - 1, dat.len()).as_str())
    );
    assert_eq!(len, dat.len() - start);
    assert_eq!(
        hash,
        blake3::hash(&dat[start..]),
        "the range is the file's tail"
    );
    assert!(
        peak < CEILING,
        "serving a range of a {}-byte segment made a {peak}-byte allocation (ceiling \
         {CEILING})",
        dat.len()
    );
}

// ── the serving side: the custodied-fleet arm ────────────────────────────────

const FLEET_OWNER_SEED: [u8; 32] = [0x7A; 32];
const FLEET_HOST_SEED: [u8; 32] = [0x7C; 32];
const FLEET_DEVICE_SEED: [u8; 32] = [0x5D; 32];
/// The custodian NEST's deployment seed — its public key is the witness's
/// `custodian_key`, so the pump pulls as the nest's own identity.
const CUSTODIAN_NEST_SEED: [u8; 32] = [0xCE; 32];
const FLEET_GRANT_ID: [u8; fauna_core::custody_grant::CUSTODY_GRANT_ID_LEN] =
    [0x3F; fauna_core::custody_grant::CUSTODY_GRANT_ID_LEN];

fn fleet_owner() -> fauna_core::identity::ActorKeypair {
    fauna_core::identity::ActorKeypair::from_secret(FLEET_OWNER_SEED)
}

fn custodian_nest_pub() -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(&CUSTODIAN_NEST_SEED)
        .verifying_key()
        .to_bytes()
}

/// A nest serving over a real listener, the doors the custody rig drives
/// (the shape of `conformance_custody_hosting_pump_client.rs`'s two nests).
/// `identity_seed` is the deployment seed (`None`: a random one);
/// `hosting_root` makes it a custodian nest.
async fn custody_rig_nest(
    identity_seed: Option<[u8; 32]>,
    hosting_root: Option<std::path::PathBuf>,
) -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(
            db.clone(),
            None,
            false,
            tmp.path().to_path_buf(),
            None,
        )
        .unwrap(),
    );
    let seed = identity_seed.unwrap_or_else(|| {
        let mut s = [0u8; 32];
        getrandom::fill(&mut s).unwrap();
        s
    });
    let owner_side = hosting_root.is_none();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(fauna_nest::nest_identity::NestIdentity::from_seed(&seed)),
        custody_hosting_root: hosting_root,
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            if owner_side {
                fauna_nest::sync_handlers::register_sync_handlers(&mut b);
                fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
                fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
                fauna_nest::segments::register_segments_handlers(&mut b);
                fauna_nest::custody_receipt_handlers::register_custody_receipt_handlers(&mut b);
            } else {
                fauna_nest::custody_hosting_handlers::register_custody_hosting_handlers(&mut b);
            }
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store: Arc::new(fauna_nest::token_store::TokenStore::new()),
            registration: fauna_nest::routes::RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, tmp)
}

/// Mint the custody capability row on the OWNER's nest, custodian = the
/// custodian nest's identity (the pump test's record-then-deposit recipe).
async fn mint_custody_row(nest: &Arc<fauna_client::NestClient>) {
    use fauna_client_capabilities::custody_grants::{custody_event_scopes, custody_mint_blob};
    use fauna_client_capabilities::grant_log::{self, PublishedGrants};
    use fauna_core::custody_grant::CustodyScopeSet;
    use fauna_mls::wrapped_blob::GrantWindow;

    let now = fauna_core::data::Timestamp::now_secs().max(0) as u64;
    let set = CustodyScopeSet::Account;
    let undeposited = custody_mint_blob(
        &fleet_owner().actor_id().0,
        &FLEET_GRANT_ID,
        &custodian_nest_pub(),
        GrantWindow(now, now + 90 * 24 * 3600),
        &set,
    )
    .expect("keyless custody blob");
    let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(fleet_owner().actor_id());
    grant_log::record_mint(
        &mut log,
        fleet_owner().signing_key(),
        FLEET_GRANT_ID,
        custodian_nest_pub(),
        custody_event_scopes(&set),
        now,
        now + 90 * 24 * 3600,
        now,
    )
    .expect("record the Mint event");
    let blob_bytes = undeposited
        .release(&PublishedGrants::from_published(
            &fauna_client_config::PublishedLedger::acknowledged_for_test(log),
        ))
        .expect("released against the published log");
    let reply = fauna_client_capabilities::rpc::CapabilitiesClient::new(Arc::clone(nest))
        .mint(blob_bytes)
        .await
        .expect("mint RPC");
    assert!(reply.ok, "the custody row deposited");
}

/// A custodian nest that holds one adopted ~8 MiB mail segment for an owner —
/// pumped from the owner's nest as the nest's own identity — and an owner-fleet
/// device that has handshaken there, so the nest classes its bearer
/// `CallerClass::CustodiedFleet`. Returns the custodian nest, the device's
/// public key, the segment id, the owner's `.dat`, and what keeps the rig up.
async fn custodied_fleet_nest() -> (
    Arc<AppState>,
    [u8; 32],
    u32,
    Vec<u8>,
    Vec<tempfile::TempDir>,
    Arc<AppState>,
) {
    use fauna_client_capabilities::custody_hosting::{CustodyHostingClient, HostingDeposit};
    use fauna_core::custody_grant::{CustodyGrant, CustodyScopeSet, sign_custody_grant};
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, sign_envelope};

    let (owner_base, owner_state, owner_tmp) = custody_rig_nest(None, None).await;
    let host_seed_tmp = tempfile::tempdir().unwrap();
    let (host_base, host_state, host_tmp) = custody_rig_nest(
        Some(CUSTODIAN_NEST_SEED),
        Some(host_seed_tmp.path().join("custody-hosting")),
    )
    .await;
    let host = fauna_core::identity::ActorKeypair::from_secret(FLEET_HOST_SEED);

    // The owner's nest holds one finalized mail segment, eight times the ceiling.
    owner_state
        .db
        .create_user_with_handle(&fleet_owner().actor_id().0, "free", "owner", None)
        .await
        .unwrap();
    let owner_nest = common::connected_client(&owner_base, fleet_owner()).await;
    for i in 0..RECORDS {
        fauna_nest::segments::mail::append_record(
            &owner_state.mail_segments,
            &owner_state.db,
            &fleet_owner().actor_id().0,
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(record(i)),
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"sealed-index-hint".to_vec(),
            ),
            fauna_mail::segments::MailFloorMetadata {
                format_version: fauna_mail::segments::MAIL_FLOOR_FORMAT_VERSION,
                received_at: 1_715_000_000_000,
                timestamp: 1_715_000_000,
                sender_domain: "example.com".to_string(),
                spam_disposition: "accept".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("append one sealed mail record on the owner's scope");
    }
    let seg_id = owner_state
        .mail_segments
        .finalize_open(&fleet_owner().actor_id().0)
        .await
        .expect("finalize the open mail segment")
        .expect("one open segment to finalize");
    mint_custody_row(&owner_nest).await;

    // The host deposits a hosting row on the custodian nest; its pump pulls.
    host_state
        .db
        .create_user_with_handle(&host.actor_id().0, "free", "host", None)
        .await
        .unwrap();
    let host_client = common::connected_client(&host_base, host).await;
    let witness = sign_custody_grant(
        &fleet_owner(),
        &CustodyGrant {
            grant_id: FLEET_GRANT_ID.to_vec(),
            owner: fleet_owner().actor_id(),
            custodian_key: custodian_nest_pub(),
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(9_000_000_000_000_000),
            removed_devices: Vec::new(),
        },
    )
    .expect("sign witness");
    let reply = CustodyHostingClient::new(Arc::clone(&host_client))
        .register(&HostingDeposit {
            grant_id: FLEET_GRANT_ID.to_vec(),
            owner: fleet_owner().actor_id().0,
            witness: fauna_core::encoding::canonical_encode(&witness)
                .unwrap()
                .to_vec(),
            owner_nest_url: owner_base.clone(),
            owner_devices: Vec::new(),
            retained_bytes_cap: 8 * 1024 * 1024 * 1024,
            stopped: false,
        })
        .await
        .expect("hosting register RPC");
    assert!(reply.ok, "the hosting row deposited");
    host_client.disconnect().await;
    let report = fauna_nest::custody_hosting_worker::CustodyHostingWorker::new(
        Arc::clone(&host_state),
        std::time::Duration::MAX,
    )
    .run_once()
    .await
    .expect("pump pass");
    assert_eq!(
        report.adopted_segments, 1,
        "the pair was adopted: {report:?}"
    );

    // An owner-fleet device handshakes at the custodian nest: a root-signed
    // `DeviceAuthorization`, no account there.
    let device = ed25519_dalek::SigningKey::from_bytes(&FLEET_DEVICE_SEED);
    let device_pub = device.verifying_key().to_bytes();
    let (bytes, env) = sign_envelope(
        &fleet_owner(),
        &DeviceAuthorization {
            actor_id: fleet_owner().actor_id(),
            device_key: device_pub,
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        },
    )
    .unwrap();
    fauna_anon_client::mint_bearer_over_custodied_fleet_handshake(
        &host_base,
        fleet_owner().actor_id().0,
        &device,
        &EmbedAsBytes::from_signed(bytes, env),
    )
    .await
    .expect("the fleet device handshakes");

    let dat = std::fs::read(
        owner_state
            .mail_segments
            .segment_file_path(&fleet_owner().actor_id().0, seg_id),
    )
    .unwrap();
    assert!(
        dat.len() >= 8 * CEILING,
        "the fixture must dwarf the ceiling"
    );
    (
        host_state,
        device_pub,
        seg_id,
        dat,
        vec![owner_tmp, host_seed_tmp, host_tmp],
        owner_state,
    )
}

/// [`fetch`] as the fleet device, against the custodian nest, for the owner's
/// `mail` segment — the route's `CustodiedFleet` arm.
async fn fetch_as_fleet_device(
    state: Arc<AppState>,
    device: [u8; 32],
    seg_id: u32,
    headers: HeaderMap,
) -> (StatusCode, Option<String>, usize, blake3::Hash) {
    let resp: Response = get_segment(
        State(state),
        BearerAuth(ActorId(device)),
        headers,
        Path((
            "mail".to_string(),
            hex::encode(fleet_owner().actor_id().0),
            seg_id,
        )),
    )
    .await
    .into_response();
    let status = resp.status();
    let range = resp
        .headers()
        .get(header::CONTENT_RANGE)
        .map(|v| v.to_str().unwrap().to_string());
    let mut body = resp.into_body().into_data_stream();
    let mut hasher = blake3::Hasher::new();
    let mut len = 0;
    while let Some(piece) = body.next().await {
        let piece = piece.expect("body piece");
        len += piece.len();
        hasher.update(&piece);
    }
    (status, range, len, hasher.finalize())
}

/// The custodied-fleet arm serves an adopted 8 MiB `.dat` — whole, and by a
/// `Range` from a quarter of the way in — off the held file, without one
/// allocation near its size. The owner-arm pins above never reach this arm.
#[tokio::test]
async fn the_custodied_fleet_arm_streams_an_adopted_segment() {
    let _serial = SERIAL.lock().await;
    let (state, device, seg_id, dat, _keep, _owner) = custodied_fleet_nest().await;

    start_tracking();
    let (status, _, len, hash) =
        fetch_as_fleet_device(Arc::clone(&state), device, seg_id, HeaderMap::new()).await;
    let peak = stop_tracking();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(len, dat.len());
    assert_eq!(
        hash,
        blake3::hash(&dat),
        "the fleet arm serves the held file"
    );
    assert!(
        peak < CEILING,
        "serving a {}-byte held segment to a fleet device made a {peak}-byte allocation — the \
         held `.dat` was read into memory instead of streamed off disk (ceiling {CEILING})",
        dat.len()
    );

    let start = dat.len() / 4;
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, format!("bytes={start}-").parse().unwrap());
    start_tracking();
    let (status, range, len, hash) = fetch_as_fleet_device(state, device, seg_id, headers).await;
    let peak = stop_tracking();
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        range.as_deref(),
        Some(format!("bytes {start}-{}/{}", dat.len() - 1, dat.len()).as_str())
    );
    assert_eq!(len, dat.len() - start);
    assert_eq!(
        hash,
        blake3::hash(&dat[start..]),
        "the range is the file's tail"
    );
    assert!(
        peak < CEILING,
        "serving a range of a {}-byte held segment made a {peak}-byte allocation (ceiling \
         {CEILING})",
        dat.len()
    );
}

// ── the pushing side ─────────────────────────────────────────────────────────

/// One nest-side backup pass pushes a 48 MiB segment to its destination without
/// one allocation near its size, and the destination's custody names exactly
/// the whole-blob seal of the `.dat` — the generation a whole-half upload would
/// have produced.
#[tokio::test]
async fn the_nest_side_backup_pass_never_allocates_the_segment() {
    let _serial = SERIAL.lock().await;
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    let seg_id = write_segment(&source.mail_segments, &OWNER, BACKUP_RECORDS).await;
    let dat_path = source.mail_segments.segment_file_path(&OWNER, seg_id);
    let dat_len = std::fs::metadata(&dat_path).unwrap().len();
    assert!(
        dat_len as usize >= 5 * BACKUP_CEILING,
        "the fixture must dwarf the ceiling"
    );
    enroll(&source, &dest, &d_url).await;

    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();

    start_tracking();
    let report = coordinator.run_once(&dest_row, "mail").await;
    let peak = stop_tracking();

    let report = report.unwrap();
    assert_eq!(report.uploaded_segments, vec![seg_id], "{report:?}");
    assert!(
        peak < BACKUP_CEILING,
        "the backup pass made a {peak}-byte allocation for a {dat_len}-byte segment — a half \
         was held in memory instead of sealed and uploaded a chunk at a time (ceiling \
         {BACKUP_CEILING})"
    );

    // What custody names is the whole-blob seal of the `.dat`, byte for byte.
    let root = NestBackupKey::from_bytes(GRANTED_KEY).convergent_chunk_root();
    let whole =
        fauna_sync_engine::seal::seal_blob(&std::fs::read(&dat_path).unwrap(), Some((root, None)))
            .unwrap();
    let custody = dest.db.backup_custody_manifest_hashes().await.unwrap();
    assert!(
        custody
            .iter()
            .any(|h| h.as_slice() == whole.manifest_hash.digest().as_slice()),
        "the destination's custody names the whole-blob seal of the segment"
    );
    // And nothing the pass staged outlived it.
    let staging = coordinator_staging(&source);
    let left = std::fs::read_dir(&staging).map_or(0, |dir| dir.count());
    assert_eq!(left, 0, "a staged seal outlived its pass");
}

// ── the keeping side: a restore out of custody ───────────────────────────────

/// The restore's ceiling: its unit is the download window
/// (`fauna_core::file_download::ManifestWalk::WINDOW` chunks), and opening one
/// chunk decodes it into a buffer of the decompression bomb guard's bound
/// (`fauna_core::compress::MAX_DECOMPRESSED_CHUNK`, allocated whole whatever
/// the chunk's size) — a constant per chunk, never the segment. So that bound
/// plus headroom caps any single allocation it makes, and the fixture is
/// several times it.
const RESTORE_CEILING: usize = fauna_core::compress::MAX_DECOMPRESSED_CHUNK + 1024 * 1024;

/// A destination holding one delivered [`BACKUP_RECORDS`]-record `post`
/// segment of [`OWNER`]'s, granted the owner's key — the state both restore
/// verbs start from. Returns the destination, the segment id and the source's
/// `.dat`.
async fn delivered_post_segment() -> (Arc<AppState>, Arc<AppState>, u32, Vec<u8>) {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    // Through the post door, so each record carries the floor a restore
    // decodes; one month bucket, so one segment.
    let mut seg_id = None;
    for i in 0..BACKUP_RECORDS {
        let outcome = fauna_nest::segments::post::append_body(
            &source.post_segments,
            &source.db,
            &OWNER,
            &record(i),
            1_715_000_000_000 + i as i64,
        )
        .await
        .expect("append a post body");
        seg_id = Some(outcome.seg_id);
    }
    source
        .post_segments
        .finalize_open(&OWNER)
        .await
        .expect("finalize");
    let seg_id = seg_id.expect("at least one record");
    let dat = std::fs::read(source.post_segments.segment_file_path(&OWNER, seg_id)).unwrap();
    assert!(
        dat.len() >= 2 * RESTORE_CEILING,
        "the fixture must dwarf the ceiling"
    );
    enroll(&source, &dest, &d_url).await;
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    let report = coordinator.run_once(&dest_row, "post").await.unwrap();
    assert_eq!(report.uploaded_segments, vec![seg_id], "{report:?}");
    grant_key_to(&dest).await;
    (source, dest, seg_id, dat)
}

/// Nothing a restore staged outlives it.
fn assert_stage_empty(nest: &AppState) {
    let stage = nest.post_segments.data_dir().join(".pair-stage");
    let left = std::fs::read_dir(&stage).map_or(0, |dir| dir.count());
    assert_eq!(left, 0, "a staged pair outlived its restore");
}

/// One re-seed materialize of a 48 MiB segment restores it without one
/// allocation near its size, byte-identical to the source's.
#[tokio::test]
async fn re_seed_materialize_never_allocates_the_segment() {
    let _serial = SERIAL.lock().await;
    let (_source, dest, seg_id, dat) = delivered_post_segment().await;

    let landing = land(&dest, "__post").await;
    start_tracking();
    let reply: Result<CustodyMaterializeReply, _> = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.materialize",
        &CustodyMaterializeRequest {
            set_name: landing,
            folder_display_name: None,
            ..Default::default()
        },
    )
    .await;
    let peak = stop_tracking();

    let reply = reply.expect("the owner materializes their posts");
    assert_eq!(reply.records, BACKUP_RECORDS as u64);
    let restored = std::fs::read(dest.post_segments.segment_file_path(&OWNER, seg_id)).unwrap();
    assert!(
        restored == dat,
        "the restored `.dat` is the source's, byte for byte"
    );
    assert!(
        peak < RESTORE_CEILING,
        "materialize made a {peak}-byte allocation for a {}-byte segment — a half was held in \
         memory instead of staged to disk (ceiling {RESTORE_CEILING})",
        dat.len()
    );
    assert_stage_empty(&dest);
}

/// One recovery of the same segment into a nest that lost it lands every
/// record without one allocation near the segment's size.
#[tokio::test]
async fn recovery_never_allocates_the_segment() {
    let _serial = SERIAL.lock().await;
    let (_source, dest, _seg_id, dat) = delivered_post_segment().await;
    let landing = land(&dest, "__post").await;

    start_tracking();
    let outcome =
        fauna_nest::backup::recover::recover_segment_set_for_test(&dest, &OWNER, &landing, None)
            .await;
    let peak = stop_tracking();

    let outcome = outcome.expect("the owner recovers their posts");
    assert_eq!(outcome.recovered, BACKUP_RECORDS as u64, "{outcome:?}");
    assert!(
        peak < RESTORE_CEILING,
        "recovery made a {peak}-byte allocation for a {}-byte segment — a half was held in \
         memory instead of staged to disk (ceiling {RESTORE_CEILING})",
        dat.len()
    );
    assert_stage_empty(&dest);
}

/// Where the coordinator stages its seals: `staging/` in the owner's backup
/// state directory, beside the nest database.
fn coordinator_staging(source: &AppState) -> std::path::PathBuf {
    std::path::Path::new(&source.config.nest.db_path)
        .parent()
        .unwrap()
        .join("nest-backup")
        .join(hex::encode(OWNER))
        .join("staging")
}
