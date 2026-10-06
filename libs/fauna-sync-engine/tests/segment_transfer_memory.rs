//! A segment transfer's memory is a constant chunk, never the segment.
//!
//! `message-segment-store.md` § Segment size: a segment has no size ceiling (it
//! rolls on its month bucket, never on size), so a transfer that holds a half
//! in memory holds as much as the source chose to write — for a custodian, up
//! to its whole disk budget. The bytes go to a staging file instead, and are
//! verified from it.
//!
//! This drives the production path end to end — [`NestBootstrapSource`] over a
//! real HTTP byte plane, into [`AccountStore::bootstrap_scope_segments`] over
//! the SQLite backend — with a real finalized segment several times larger
//! than the ceiling below, and measures the **largest single allocation** the
//! process makes while it runs. A whole-body buffer anywhere on the path (a
//! `Vec` grown to the `.dat`, a `bytes()` read, an in-memory admission) is one
//! allocation at least half the body's size, so it cannot hide under the
//! ceiling. It is its own test binary because it installs a global allocator.
//!
//! The owner's backup pass is the second path it drives: the same byte plane
//! through `segment_backup::SourceBinding` into the custodian pull, which
//! stages the pair and seals it into its store a chunk at a time — measured
//! against a ceiling of one sealed chunk, the seal's own unit.
//!
//! The byte plane is a hand-rolled server that streams the file off disk in
//! small pieces, so the server side never allocates the body either and every
//! large allocation the counter sees is the client's.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_protocol::RpcRequester;
use fauna_protocol::scope::ContentScope;
use fauna_protocol::segments::{SegmentRef, SegmentsListReply};
use fauna_sync_engine::bootstrap_source::{NestBootstrapSource, ScopeBinding};
use fauna_sync_engine::nest_client::SyncClient;
use fauna_sync_engine::segment_backup::{
    SegmentListing, SegmentPair, SegmentSource, SourceBinding, StagedPair,
};

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

// ── the fixture ──────────────────────────────────────────────────────────────

const KIND: &str = "post";
const ACTOR: [u8; 32] = [0xAB; 32];

/// The largest single allocation a transfer may make, whatever the segment's
/// size. Generous against every constant buffer on the path (the transfer
/// chunk, one record — this fixture's are 32 KiB — the HTTP stack's own read
/// buffer), and an eighth of the segment below, so a whole-half buffer cannot
/// fit under it.
const CEILING: usize = 1024 * 1024;
const RECORD_LEN: usize = 32 * 1024;
const RECORDS: usize = 256; // 8 MiB of records: eight times the ceiling

/// The owner's backup pass seals what it keeps, and the seal's unit is a
/// **chunk** — up to `MAX_CHUNK` (8 MiB) by the content-addressed format, a
/// size fixed tree-wide — so its constant is one chunk, not a transfer piece:
/// the chunker's window, one chunk's plaintext, its compressed frame and its
/// ciphertext are each an allocation near 8 MiB. The ceiling is that plus
/// headroom, and the backup fixture below is several times it.
const BACKUP_CEILING: usize = 9 * 1024 * 1024;
const BACKUP_RECORDS: usize = 1536; // 48 MiB of records: over five times the ceiling

/// Write one real finalized segment of `records` records with the production
/// writer and return its two files' paths.
fn write_segment(dir: &Path, records: usize) -> (PathBuf, PathBuf) {
    use fauna_segment_store::{FramedSegment, SegmentHeader};
    let dat_path = dir.join("seg-00000000.dat");
    let mut seg = FramedSegment::create(
        &dat_path,
        SegmentHeader {
            kind: KIND.into(),
            actor_id: ACTOR,
            segment_id: 0,
            bucket: "2026-09".into(),
            created_at_secs: 1_780_000_000,
            record_count: 0,
        },
    )
    .unwrap();
    for i in 0..records {
        let mut body = vec![(i % 251) as u8; RECORD_LEN];
        body[..8].copy_from_slice(&(i as u64).to_be_bytes());
        seg.append_record(fauna_cbor::Cid::of_dag_cbor(&body), &body, b"")
            .unwrap();
    }
    seg.finalize().unwrap();
    let meta_path = seg.meta_path().to_path_buf();
    (dat_path, meta_path)
}

fn file_blake3(path: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(std::fs::File::open(path).unwrap())
        .unwrap();
    hex::encode(hasher.finalize().as_bytes())
}

/// A byte plane serving the pair: `…/meta` gets the sidecar, anything else the
/// `.dat`, each streamed off disk in 16 KiB pieces with a truthful length.
fn serve(dat: PathBuf, meta: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { return };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).is_err() || header == "\r\n" || header.is_empty() {
                    break;
                }
            }
            let path = request_line.split_whitespace().nth(1).unwrap_or("");
            let file = if path.ends_with("/meta") { &meta } else { &dat };
            let mut f = std::fs::File::open(file).unwrap();
            let len = f.metadata().unwrap().len();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\
                 Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
            );
            if conn.write_all(head.as_bytes()).is_err() {
                continue;
            }
            let mut piece = [0u8; 16 * 1024];
            loop {
                let n = f.read(&mut piece).unwrap();
                if n == 0 || conn.write_all(&piece[..n]).is_err() {
                    break;
                }
            }
        }
    });
    format!("http://{addr}")
}

/// The control plane: `fauna.segments.list` answering with the one segment,
/// advertising its real size and hashes.
struct Listing(SegmentRef);

#[derive(Debug)]
struct Never;
impl std::fmt::Display for Never {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unreachable")
    }
}

impl RpcRequester for Listing {
    type Error = Never;

    async fn request<Req, Reply>(&self, _kind: &'static str, _payload: Req) -> Result<Reply, Never>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let reply = SegmentsListReply {
            segments: vec![self.0.clone()],
            next_segment_id: self.0.segment_id + 1,
            extra: Default::default(),
        };
        let bytes = fauna_protocol::encode_canonical(&reply).unwrap();
        Ok(fauna_protocol::decode_strict(&bytes).unwrap())
    }
}

/// A fresh device's unbudgeted bootstrap — the path with no budget to bound a
/// download at all — adopts an 8 MiB segment without one allocation near its
/// size, and the adopted records read back.
#[tokio::test]
async fn a_segment_transfer_never_allocates_the_segment() {
    let src = tempfile::tempdir().unwrap();
    let (dat_path, meta_path) = write_segment(src.path(), RECORDS);
    let dat_len = std::fs::metadata(&dat_path).unwrap().len();
    assert!(
        dat_len as usize >= 8 * CEILING,
        "the fixture must dwarf the ceiling"
    );
    let listing = Listing(SegmentRef {
        segment_id: 0,
        blake3_hex: file_blake3(&dat_path),
        bucket: "2026-09".into(),
        record_count: RECORDS as u32,
        tombstone_count: 0,
        size_bytes: dat_len,
        created_at_secs: 1_780_000_000,
        is_open: false,
        meta_blake3_hex: file_blake3(&meta_path),
        extra: Default::default(),
    });
    let base = serve(dat_path, meta_path);

    let bearer: Arc<dyn fauna_nest_http::BearerSource> =
        Arc::new(fauna_nest_http::StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        base,
        fauna_core::identity::ActorKeypair::generate(),
        bearer,
        reqwest::Client::new(),
    ));
    let bytes = SyncClient::new(auth, &[0u8; 32]);
    let scope = ContentScope::new(KIND, ACTOR).unwrap().to_string();
    let source = NestBootstrapSource::new(
        &listing,
        &bytes,
        ScopeBinding {
            scope: scope.clone(),
            kinds: vec![KIND.into()],
            actor_hex: hex::encode(ACTOR),
        },
    );

    let store_dir = tempfile::tempdir().unwrap();
    let store = AccountStore::open(
        SqliteBackend::open(store_dir.path()).unwrap(),
        &hex::encode(ACTOR),
        WriterId([0x11; 32]),
    )
    .await
    .unwrap();

    PEAK.store(0, Ordering::Relaxed);
    TRACKING.store(true, Ordering::Relaxed);
    let report = store.bootstrap_scope_segments(&scope, &source).await;
    TRACKING.store(false, Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed);

    let report = report.unwrap();
    assert_eq!(report.adopted, 1, "{report:?}");
    assert_eq!(report.records_indexed, RECORDS, "{report:?}");
    assert!(
        peak < CEILING,
        "the transfer made a {peak}-byte allocation for a {dat_len}-byte segment — a half \
         was buffered in memory instead of streamed to a staging file (ceiling {CEILING})"
    );

    // And what was adopted is the segment: a record reads back whole.
    let mut first = vec![0u8; RECORD_LEN];
    first[..8].copy_from_slice(&0u64.to_be_bytes());
    let got = store
        .block(&fauna_cbor::Cid::of_dag_cbor(&first))
        .await
        .unwrap()
        .expect("the adopted segment serves its records");
    assert_eq!(got, first);
}

// ── the owner's backup pass ──────────────────────────────────────────────────

/// The owner's source nest as the backup pass reads it: its listing from the
/// fixture, every byte off the real [`SourceBinding`] byte plane.
struct OwnerNest {
    listing: SegmentListing,
    wire: SourceBinding,
}

#[async_trait::async_trait]
impl SegmentSource for OwnerNest {
    fn source_id(&self) -> &str {
        self.wire.source_id()
    }

    async fn list_segments(&self, _kind: &str, _scope_hex: &str) -> anyhow::Result<SegmentListing> {
        Ok(self.listing.clone())
    }

    async fn segment_pair(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
    ) -> anyhow::Result<SegmentPair> {
        self.wire.segment_pair(kind, scope_hex, segment_id).await
    }

    async fn stage_segment_pair(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
        staging: &Path,
    ) -> anyhow::Result<StagedPair> {
        self.wire
            .stage_segment_pair(kind, scope_hex, segment_id, staging)
            .await
    }

    async fn segment_meta_bytes(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
    ) -> anyhow::Result<Vec<u8>> {
        self.wire
            .segment_meta_bytes(kind, scope_hex, segment_id)
            .await
    }
}

/// The source nest's `fauna.backup.*` surface: it accepts the check-in.
struct AcceptsCheckin;

impl RpcRequester for AcceptsCheckin {
    type Error = fauna_client::NestClientError;

    async fn request<Req, Reply>(
        &self,
        _kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, fauna_client::NestClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        serde_json::from_str(r#"{"ok":true}"#)
            .map_err(|e| fauna_client::NestClientError::Decode(e.to_string()))
    }
}

/// The owner's backup pass — [`SourceBinding`] into the custodian pull's
/// seal-and-put — keeps a 48 MiB segment without one allocation near its
/// size, and what it keeps is byte-identical to the whole-blob seal of the
/// same `.dat` (the artifacts a nest destination would hold).
#[tokio::test]
async fn the_owners_backup_pass_never_allocates_the_segment() {
    use fauna_core::crypto::{BackupKey, OwnerSealKey};
    use fauna_sync_engine::custodian_pull::CustodianPull;
    use fauna_sync_engine::custodian_store::CustodianStore;

    let src = tempfile::tempdir().unwrap();
    let (dat_path, meta_path) = write_segment(src.path(), BACKUP_RECORDS);
    let dat_len = std::fs::metadata(&dat_path).unwrap().len();
    assert!(
        dat_len as usize >= 5 * BACKUP_CEILING,
        "the fixture must dwarf the ceiling"
    );
    let listing = SegmentListing {
        segments: vec![SegmentRef {
            segment_id: 0,
            blake3_hex: file_blake3(&dat_path),
            bucket: "2026-09".into(),
            record_count: BACKUP_RECORDS as u32,
            tombstone_count: 0,
            size_bytes: dat_len,
            created_at_secs: 1_780_000_000,
            is_open: false,
            meta_blake3_hex: file_blake3(&meta_path),
            extra: Default::default(),
        }],
        next_segment_id: 1,
    };
    let base = serve(dat_path.clone(), meta_path);

    let bearer: Arc<dyn fauna_nest_http::BearerSource> =
        Arc::new(fauna_nest_http::StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        base,
        fauna_core::identity::ActorKeypair::generate(),
        bearer,
        reqwest::Client::new(),
    ));
    let source = OwnerNest {
        listing,
        wire: SourceBinding {
            source_nest_id: "owner-nest".into(),
            sync_client: Arc::new(SyncClient::new(auth.clone(), &[0u8; 32])),
            ws_client: fauna_client::NestClient::with_auth(auth),
        },
    };

    let store_dir = tempfile::tempdir().unwrap();
    let store = CustodianStore::at(store_dir.path().join("custody"));
    let backup = fauna_client_backup::BackupClient::new(AcceptsCheckin);
    let seal_key = OwnerSealKey::Client(BackupKey::from_bytes([5u8; 32]));
    let seal_root = seal_key.convergent_chunk_root();
    let pull = CustodianPull {
        source: &source,
        store: &store,
        backup: &backup,
        destination_id: "this-laptop".into(),
        scope_id: ACTOR,
        seal_key,
        cap_bytes: None,
        device_id: None,
        folder_source: None,
        not_assigned: None,
    };

    PEAK.store(0, Ordering::Relaxed);
    TRACKING.store(true, Ordering::Relaxed);
    let report = pull.run_once(KIND, 1_780_000_100).await;
    TRACKING.store(false, Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed);

    let report = report.unwrap();
    assert_eq!(report.stored_segments, vec![0], "{report:?}");
    assert!(
        peak < BACKUP_CEILING,
        "the backup pass made a {peak}-byte allocation for a {dat_len}-byte segment — a half \
         was held in memory instead of staged and sealed a chunk at a time (ceiling \
         {BACKUP_CEILING})"
    );

    // What it kept is the whole-blob seal's generation, byte for byte.
    let whole = fauna_sync_engine::seal::seal_blob(
        &std::fs::read(&dat_path).unwrap(),
        Some((seal_root, None)),
    )
    .unwrap();
    let held = store.held().await.unwrap();
    let dat_row = held
        .iter()
        .find(|row| row.path.ends_with("seg-00000000.dat"))
        .expect("the .dat is held");
    assert_eq!(
        dat_row.manifest_hash,
        hex::encode(whole.manifest_hash.digest())
    );
    assert_eq!(dat_row.size_bytes, whole.stored_bytes());
    // And nothing it staged outlived the pass.
    let staged = std::fs::read_dir(store.staging_dir()).map_or(0, |dir| dir.count());
    assert_eq!(staged, 0, "a staged pair outlived its pass");
}
