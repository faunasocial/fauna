//! Shared test doubles for this crate's `#[cfg(test)]` modules — and, behind
//! the `test-helpers` feature, for other crates' tests.
//!
//! # The mock nest
//!
//! Eight `mount_*` helpers across seven of this crate's test modules had
//! hand-copied the same wiremock byte-plane double (`POST /chunks/check` +
//! `POST /chunks` + `POST /manifests` and the matching GETs, sometimes a
//! `/blob` pair), and a ninth copy sits in `bins/fauna-nest`'s
//! `conformance_shared_folders.rs`. Several said so in their own doc comments
//! ("the `download_file_bytes_test.rs` pattern"). Priority #2 says that
//! belongs in one place; the reason it stayed copied is that the natural home
//! is *this* crate's library, and a separate crate can only reach it through a
//! Cargo feature.
//!
//! [`MockNest`] is that one place: a builder whose base mounts are the
//! chunk/manifest plane every copy shared, with the two axes they actually
//! differed on as opt-ins — [`MockNest::verifying`] (the real route's F9
//! anti-poisoning check) and [`MockNest::with_blob_plane`] (the `/api/v1/blob`
//! pair). Every observable each copy returned separately — the last manifest
//! hash, the last blob hash, the manifest hashes fetched back, the blob-POST
//! count — is a field of the single [`BlobStore`] the builder hands back.
//!
//! # The gate, and why it is not rule (a)'s exact spelling
//!
//! `docs/goal/architecture/e2e-automation-surface-gating.md` § convention 15
//! rule (a) spells a shared crate's seam gate
//! `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`, so that a
//! plain debug build of an in-process consumer reaches the seams. **This module
//! deliberately drops the `debug_assertions` arm** and is gated
//! `#[cfg(any(test, feature = "test-helpers"))]`.
//!
//! The reason is mechanical, and it makes the gate *stricter*, never weaker:
//! rule (a)'s `debug_assertions` arm exists for seams an app's own debug build
//! reaches at runtime, and such seams are written against the crate's normal
//! dependencies. This module is a wiremock HTTP double — `wiremock` is a
//! **dev**-dependency, and Cargo cannot make a dependency conditional on
//! `debug_assertions`, so keeping that arm would mean either a plain
//! `cargo build` of this crate failing to compile, or `wiremock` becoming an
//! unconditional dependency of a crate that also builds for wasm and mobile.
//! Instead `wiremock` is an *optional* dependency turned on by `test-helpers`
//! alone, so it enters no build that does not ask for it, and this module
//! exists in no artifact — debug or release — that does not ask for it either.
//! Rule (b) is untouched: the one cross-crate consumer names `test-helpers` on
//! a **dev**-dependency line (`bins/fauna-nest`), never a normal one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fauna_core::data::ContentHash;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// The final `/`-delimited segment of a mocked request's path — the id most
/// wiremock handlers here key their in-memory store lookups on.
pub fn last_path_segment(req: &Request) -> String {
    req.url.path().rsplit('/').next().unwrap_or("").to_string()
}

/// Pull the `bytes` part out of a `multipart/form-data` body.
///
/// `upload_blob_multipart` posts exactly two parts, `sidecar` then `bytes`; the
/// nest content-addresses the second. Enough of a parser for that fixed shape:
/// find the part header, skip to the blank line, and take everything up to the
/// CRLF that precedes the next boundary.
fn multipart_bytes_part(body: &[u8]) -> Option<Vec<u8>> {
    let needle = b"name=\"bytes\"";
    let start = body.windows(needle.len()).position(|w| w == needle)?;
    let rest = &body[start..];
    let header_end = rest.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let payload = &rest[header_end..];
    // The trailing boundary is preceded by CRLF; everything before it is content.
    let end = payload
        .windows(4)
        .rposition(|w| w == b"\r\n--")
        .unwrap_or(payload.len());
    Some(payload[..end].to_vec())
}

/// In-memory chunk + manifest + blob store backing a [`MockNest`]'s routes,
/// plus the observables the mounted handlers record as they run.
///
/// Cloning shares the state (every field is an `Arc`), so a test can keep a
/// handle while the builder mounts its handlers.
#[derive(Clone, Default)]
pub struct BlobStore {
    /// Chunk bodies, keyed by the hex hash the `POST /chunks` handler stored
    /// them under (== the key `GET /chunks/{h}` serves them back by).
    pub chunks: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Manifest bodies, keyed by hex `ContentHash::of_raw(body)`.
    pub manifests: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// The hash of the most recently POSTed manifest. The upload pipeline does
    /// not return it to `upload_file`'s caller, so it is captured server-side.
    pub last_manifest_hash: Arc<Mutex<Option<ContentHash>>>,
    /// The **blob** store — a different store from `manifests`, which is the
    /// whole reason the post-succession re-seal needs a second arm: a Media-page
    /// upload records its blob-store primary's hash in the same column an
    /// engine-recorded file uses for its chunk manifest.
    pub blobs: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Hex hash of the most recently POSTed blob, so a test can assert the byte
    /// plane moved (and open the stored bytes under the key it expects).
    pub last_blob_hash: Arc<Mutex<Option<String>>>,
    /// Every manifest hash GET-ed back, in order — a fold's own direct
    /// observable. A folded row never reaches
    /// [`crate::SyncEngine::download_and_write_file`] at all, and a row that
    /// skips pre-fetch (duplicate, stale resolution) returns *before*
    /// `fetch_manifest`, so a hash appearing here means the engine committed to
    /// delivering that row's content.
    pub fetched_manifests: Arc<Mutex<Vec<String>>>,
    /// How many `POST /api/v1/blob` requests the blob plane has served — the
    /// thumbnail tests' observable (Seam-A posts a thumbnail or it does not).
    pub blob_posts: Arc<AtomicUsize>,
}

impl BlobStore {
    /// Seed a chunk body under `hash`, as if it had already been uploaded.
    pub fn seed_chunk(&self, hash: ContentHash, body: Vec<u8>) {
        self.chunks
            .lock()
            .unwrap()
            .insert(hex::encode(hash.digest()), body);
    }

    /// Seed a manifest body under `hash`, as if it had already been uploaded.
    pub fn seed_manifest(&self, hash: ContentHash, body: Vec<u8>) {
        self.manifests
            .lock()
            .unwrap()
            .insert(hex::encode(hash.digest()), body);
    }

    /// The hash of the most recently POSTed manifest, panicking with a
    /// test-legible message if the upload never posted one.
    #[track_caller]
    pub fn expect_last_manifest_hash(&self) -> ContentHash {
        self.last_manifest_hash
            .lock()
            .unwrap()
            .expect("the upload must have POSTed a manifest")
    }

    /// How many blobs have been POSTed so far.
    pub fn blob_posts(&self) -> usize {
        self.blob_posts.load(Ordering::SeqCst)
    }

    /// The manifest hashes fetched back, in order.
    pub fn fetched_manifests(&self) -> Vec<String> {
        self.fetched_manifests.lock().unwrap().clone()
    }

    /// Whether `hash`'s manifest was ever GET-ed back — i.e. whether the engine
    /// committed to delivering that row's content rather than folding it away.
    pub fn manifest_was_fetched(&self, hash: ContentHash) -> bool {
        self.fetched_manifests
            .lock()
            .unwrap()
            .contains(&hex::encode(hash.digest()))
    }
}

/// A wiremock double of the nest's byte plane.
///
/// The base mounts — always registered by [`MockNest::mount`] — are the
/// chunk/manifest plane every hand-copied `mount_*` shared:
///
/// * `POST /api/v1/chunks/check` — report every requested hash missing, forcing
///   the upload path to actually upload.
/// * `POST /api/v1/chunks` — store the body under its `X-Content-Hash` header
///   (or, absent the header, under `blake3(body)`, the route's own
///   header-absent branch).
/// * `POST /api/v1/manifests` — store under `ContentHash::of_raw(body)`,
///   matching how the engine computes `manifest_hash` for the GET path, and
///   record it as [`BlobStore::last_manifest_hash`].
/// * `GET /api/v1/chunks/{hash}` and `GET /api/v1/manifests/{hash}` — serve
///   them back; a miss is a 404, exactly as an unmounted route would be.
///
/// The two axes the copies genuinely differed on are opt-in:
/// [`MockNest::verifying`] and [`MockNest::with_blob_plane`].
pub struct MockNest {
    store: BlobStore,
    verify_chunks: bool,
    blob_plane: bool,
}

impl Default for MockNest {
    fn default() -> Self {
        Self::new()
    }
}

impl MockNest {
    /// A mock nest with the base chunk/manifest plane and nothing else.
    pub fn new() -> Self {
        Self {
            store: BlobStore::default(),
            verify_chunks: false,
            blob_plane: false,
        }
    }

    /// Make `POST /chunks` enforce the **real** nest route's F9 anti-poisoning
    /// check (`chunk_routes::resolve_verified_chunk_hash`): store a body only if
    /// its `X-Content-Hash` equals `blake3(body)` or `blake3(decompress(body))`,
    /// else reply `400`.
    ///
    /// This is the store an AEAD-ciphertext upload must satisfy — a body sealed
    /// under a content key whose `X-Content-Hash` is the *plaintext* hash is
    /// rejected, so the engine must key the encrypted store by the *ciphertext*
    /// hash (FS-BIND, PIECE 6). Without it the handler is a deliberately naive
    /// store, fine for paths whose upload key already equals `blake3(body)`.
    pub fn verifying(mut self) -> Self {
        self.verify_chunks = true;
        self
    }

    /// Also mount the `/api/v1/blob` pair: a `POST` that content-addresses the
    /// multipart `bytes` part exactly as the real route does (recording
    /// [`BlobStore::last_blob_hash`] and bumping [`BlobStore::blob_posts`]), and
    /// a `GET /api/v1/blob/{hash}` serving the second store back.
    ///
    /// Left off by default because a test whose corpus never triggers thumbnail
    /// generation never posts a blob, and an unmounted route says so loudly.
    pub fn with_blob_plane(mut self) -> Self {
        self.blob_plane = true;
        self
    }

    /// The store these routes read and write — usable before [`Self::mount`],
    /// so a fixture-backed test can seed it and then mount.
    pub fn store(&self) -> &BlobStore {
        &self.store
    }

    /// Mount the configured routes on `server`, returning the store handle.
    pub async fn mount(self, server: &MockServer) -> BlobStore {
        let store = self.store.clone();

        // POST /chunks/check → report every requested hash as missing.
        Mock::given(method("POST"))
            .and(path("/api/v1/chunks/check"))
            .respond_with(|req: &Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
                let missing: Vec<String> = body
                    .get("hashes")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|h| h.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "missing": missing }))
            })
            .mount(server)
            .await;

        // POST /chunks → store body keyed by X-Content-Hash (== the GET path).
        {
            let chunks = Arc::clone(&store.chunks);
            let verify_chunks = self.verify_chunks;
            Mock::given(method("POST"))
                .and(path("/api/v1/chunks"))
                .respond_with(move |req: &Request| {
                    let Some(h) = req
                        .headers
                        .get("X-Content-Hash")
                        .and_then(|v| v.to_str().ok())
                    else {
                        // No header ⇒ keyed by blake3(body), content-addressed by
                        // construction (the route's header-absent branch).
                        let key = hex::encode(blake3::hash(&req.body).as_bytes());
                        chunks.lock().unwrap().insert(key, req.body.clone());
                        return ResponseTemplate::new(200);
                    };
                    if verify_chunks {
                        let Ok(claimed) = fauna_core::hex32::decode(h) else {
                            return ResponseTemplate::new(400);
                        };
                        let raw_ok = *blake3::hash(&req.body).as_bytes() == claimed;
                        let decompressed_ok = fauna_core::compress::decompress_chunk_bounded(
                            &req.body,
                            fauna_core::compress::MAX_DECOMPRESSED_CHUNK,
                        )
                        .map(|p| *blake3::hash(&p).as_bytes() == claimed)
                        .unwrap_or(false);
                        if !(raw_ok || decompressed_ok) {
                            return ResponseTemplate::new(400);
                        }
                    }
                    chunks
                        .lock()
                        .unwrap()
                        .insert(h.to_string(), req.body.clone());
                    ResponseTemplate::new(200)
                })
                .mount(server)
                .await;
        }

        // POST /manifests → key by ContentHash::of_raw(body), matching how the
        // engine computes manifest_hash for the GET path.
        {
            let manifests = Arc::clone(&store.manifests);
            let last = Arc::clone(&store.last_manifest_hash);
            Mock::given(method("POST"))
                .and(path("/api/v1/manifests"))
                .respond_with(move |req: &Request| {
                    let hash = ContentHash::of_raw(&req.body);
                    let hex = hex::encode(hash.digest());
                    manifests.lock().unwrap().insert(hex, req.body.clone());
                    *last.lock().unwrap() = Some(hash);
                    ResponseTemplate::new(200)
                })
                .mount(server)
                .await;
        }

        // GET /chunks/{hash}
        {
            let chunks = Arc::clone(&store.chunks);
            Mock::given(method("GET"))
                .and(path_regex(r"^/api/v1/chunks/[0-9a-f]+$"))
                .respond_with(move |req: &Request| {
                    match chunks.lock().unwrap().get(&last_path_segment(req)) {
                        Some(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes.clone()),
                        None => ResponseTemplate::new(404),
                    }
                })
                .mount(server)
                .await;
        }

        // GET /manifests/{hash} — recording each hash served, in order.
        {
            let manifests = Arc::clone(&store.manifests);
            let fetched = Arc::clone(&store.fetched_manifests);
            Mock::given(method("GET"))
                .and(path_regex(r"^/api/v1/manifests/[0-9a-f]+$"))
                .respond_with(move |req: &Request| {
                    let key = last_path_segment(req);
                    fetched.lock().unwrap().push(key.clone());
                    match manifests.lock().unwrap().get(&key) {
                        Some(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes.clone()),
                        None => ResponseTemplate::new(404),
                    }
                })
                .mount(server)
                .await;
        }

        if self.blob_plane {
            // POST /blob (multipart) → content-address the `bytes` part, exactly
            // as the real route does, and report the hash back.
            {
                let blobs = Arc::clone(&store.blobs);
                let last = Arc::clone(&store.last_blob_hash);
                let posts = Arc::clone(&store.blob_posts);
                Mock::given(method("POST"))
                    .and(path("/api/v1/blob"))
                    .respond_with(move |req: &Request| {
                        posts.fetch_add(1, Ordering::SeqCst);
                        let Some(bytes) = multipart_bytes_part(&req.body) else {
                            return ResponseTemplate::new(400);
                        };
                        let hex = blake3::hash(&bytes).to_hex().to_string();
                        blobs.lock().unwrap().insert(hex.clone(), bytes);
                        *last.lock().unwrap() = Some(hex.clone());
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({ "hash": hex }))
                    })
                    .mount(server)
                    .await;
            }

            // GET /blob/{hash} — the second store. A miss is a typed 404, which
            // is what lets the re-seal tell "this hash names a manifest" from
            // "the transport broke".
            {
                let blobs = Arc::clone(&store.blobs);
                Mock::given(method("GET"))
                    .and(path_regex(r"^/api/v1/blob/[0-9a-f]+$"))
                    .respond_with(move |req: &Request| {
                        match blobs.lock().unwrap().get(&last_path_segment(req)) {
                            Some(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes.clone()),
                            None => ResponseTemplate::new(404),
                        }
                    })
                    .mount(server)
                    .await;
            }
        }

        store
    }
}
