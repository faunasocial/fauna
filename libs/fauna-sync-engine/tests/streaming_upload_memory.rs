//! A large file's first upload holds a constant number of chunks, never the
//! file.
//!
//! `media.md` § the >64 MiB upload path: the streaming path chunks the file to
//! a temporary directory a chunk at a time and "never loads the whole file".
//! The chunking alone does not make that true — the upload after it must also
//! read the missing chunks back a window at a time, or a first upload of a
//! 10 GiB video holds 10 GiB on the uploading device.
//!
//! No single allocation on such a path is near the file's size (every chunk is
//! its own `Vec`), so the largest-allocation pin the segment transfer uses
//! (`segment_transfer_memory.rs`) cannot see it. This one counts **live**
//! bytes: the most the process holds at once while `upload_file` runs over a
//! file several windows large. It is its own test binary because it installs
//! a global allocator.
//!
//! The byte plane is a hand-rolled server, not the shared `MockNest`: a
//! wiremock server lives in the same process and keeps what it is sent (the
//! mock nest's chunk store holds every body by design), which is the file's
//! size again on the server side of the counter. This one answers the chunk
//! check with every key missing, reads each chunk body off the socket a small
//! piece at a time and drops it, and keeps only the manifest — so every byte
//! the counter sees above the ceiling is the client's.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Relaxed;

use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::nest_client::SyncClient;
use fauna_sync_engine::transfer::TransferPool;

// ── the live-bytes counter (`fauna-archive`'s `member_inflation_bound.rs`) ───

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = LIVE.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: every call forwards to `System` unchanged; the counter only reads
// the sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

// ── the fixture ──────────────────────────────────────────────────────────────

const MIB: usize = 1024 * 1024;

/// The most the upload may hold at once, whatever the file's size. The worst
/// constants on the path: the chunker's rolling buffer (two `MAX_CHUNK`s, 8
/// MiB each, plus the tail it carries over), and a window of
/// `STAGED_UPLOAD_WINDOW` chunks held twice — the window's bodies and the HTTP
/// client's copy of each in flight — about 64 MiB at the largest chunks. The
/// ceiling is that with headroom (this fixture measures about 33 MiB, the
/// chunker's buffer), and the file below is twice it.
const CEILING: usize = 80 * MIB;
/// Over the 64 MiB streaming threshold, and twice the ceiling, so a
/// collect-all read-back of the missing chunks cannot fit under it.
const FILE_LEN: usize = 160 * MIB;
const REL: &str = "holiday.mov";

/// Incompressible bytes (xorshift), written a piece at a time so the fixture
/// itself never holds the file.
fn write_fixture(path: &std::path::Path) {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut piece = vec![0u8; MIB];
    for _ in 0..FILE_LEN / MIB {
        for word in piece.as_chunks_mut::<8>().0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *word = state.to_le_bytes();
        }
        out.write_all(&piece).unwrap();
    }
    out.flush().unwrap();
}

/// The nest's byte plane, as much of it as a first upload uses: `POST
/// /api/v1/chunks/check` answers every key missing, `POST /api/v1/chunks`
/// reads the body off the socket a piece at a time and keeps only its length,
/// `POST /api/v1/manifests` is counted. Plain HTTP/1.1 with keep-alive, a
/// thread per connection.
#[derive(Clone, Default)]
struct BytePlane {
    chunk_bytes: Arc<AtomicUsize>,
    manifests: Arc<AtomicUsize>,
}

impl BytePlane {
    fn serve(&self) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let plane = self.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                let plane = plane.clone();
                std::thread::spawn(move || plane.connection(conn));
            }
        });
        format!("http://{addr}")
    }

    fn connection(&self, conn: std::net::TcpStream) {
        let mut out = conn.try_clone().unwrap();
        let mut reader = BufReader::new(conn);
        loop {
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                return;
            }
            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 {
                    return;
                }
                if header == "\r\n" {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().unwrap();
                }
            }
            let path = request_line.split_whitespace().nth(1).unwrap_or("");
            let reply = if path == "/api/v1/chunks/check" {
                let mut body = vec![0u8; content_length];
                reader.read_exact(&mut body).unwrap();
                let asked: serde_json::Value = serde_json::from_slice(&body).unwrap();
                serde_json::json!({ "missing": asked["hashes"] }).to_string()
            } else {
                let mut piece = [0u8; 16 * 1024];
                let mut left = content_length;
                while left > 0 {
                    let want = left.min(piece.len());
                    let n = reader.read(&mut piece[..want]).unwrap();
                    assert!(n > 0, "the client hung up mid-body");
                    left -= n;
                }
                if path == "/api/v1/chunks" {
                    self.chunk_bytes.fetch_add(content_length, Relaxed);
                } else if path == "/api/v1/manifests" {
                    self.manifests.fetch_add(1, Relaxed);
                }
                String::new()
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
                reply.len()
            );
            if out.write_all(head.as_bytes()).is_err() || out.write_all(reply.as_bytes()).is_err() {
                return;
            }
        }
    }
}

/// An owner-keyed engine on `watch_dir`: byte plane at `server_uri`, control
/// plane never connected (the record after the upload is not what this
/// measures).
fn engine(server_uri: &str, watch_dir: std::path::PathBuf) -> SyncEngine {
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        ActorKeypair::generate(),
        bearer,
        reqwest::Client::new(),
    ));
    SyncEngine::new(
        watch_dir,
        SyncDb::open_in_memory().unwrap(),
        SyncClient::new(auth, &[0u8; 32]),
        Some("videos".to_string()),
        [0u8; 32],
        None,
        None,
        Some(BackupKey::from_bytes([0x61u8; 32]).into()),
        None,
        None,
        fauna_core::format::ConflictPolicy::default(),
        fauna_core::format::FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        fauna_client::NestClient::new(server_uri.to_string(), ActorKeypair::generate()),
        fauna_sync_engine::config::SyncMode::Sync,
    )
}

/// A first upload of a file many windows large sends every chunk and the
/// manifest while holding at most a constant number of chunks live.
#[tokio::test]
async fn a_large_files_first_upload_holds_a_window_of_chunks_not_the_file() {
    let plane = BytePlane::default();
    let uri = plane.serve();

    let dir = tempfile::tempdir().unwrap();
    write_fixture(&dir.path().join(REL));
    let engine = engine(&uri, dir.path().to_path_buf());

    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let outcome = engine.upload_file(REL).await;
    let peak = PEAK.load(Relaxed) - base;
    eprintln!("peak live bytes during the upload: {peak} (ceiling {CEILING})");

    // The record has no control plane to land on; the upload itself must
    // have run to the manifest.
    let _ = outcome;
    assert!(
        plane.manifests.load(Relaxed) > 0,
        "the upload posted its manifest"
    );
    let sent = plane.chunk_bytes.load(Relaxed);
    assert!(
        sent >= FILE_LEN,
        "every chunk reached the byte plane ({sent} of {FILE_LEN} bytes)"
    );
    assert!(
        peak < CEILING,
        "the upload held {peak} bytes live at once for a {FILE_LEN}-byte file — the missing \
         chunks were read back all at once instead of a window at a time (ceiling {CEILING})"
    );
}
