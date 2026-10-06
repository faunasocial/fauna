//! The two seams (`archive-import.md` § The wizard and its machine): the
//! nest-ward [`ArchiveNest`] and the platform's [`ArchiveOpener`]. Dual
//! `async_trait` arm + `MaybeSendSync`, the `MailImportNest` shape — natively
//! the seam wraps a `Send + Sync` transport so the machine stays `Send` for
//! `tokio::spawn`; on wasm it wraps an `Rc`-based one and the bound relaxes.
//!
//! The machine never depends on `fauna-client-*`: every nest call the import
//! makes is one method here, and the real WS-RPC delegation lives behind the
//! `rpc-glue` feature (slice 3, Task 10).

use std::sync::Arc;

use fauna_archive::ArchiveSource;
use fauna_core::MaybeSendSync;

use crate::state::ArchiveMarker;

/// A shared, random-access handle on an archive zip. `ArchiveSource` carries
/// no auto-trait bounds of its own, so the `Send + Sync` half is added here
/// on every target but wasm32 — where a browser `File`-backed source is
/// single-threaded and could not satisfy it.
#[cfg(not(target_arch = "wasm32"))]
pub type SharedSource = Arc<dyn ArchiveSource + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type SharedSource = Arc<dyn ArchiveSource>;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ArchiveNestError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("{code}: {detail}")]
    Rejected { code: String, detail: String },
    #[error("not supported on this platform: {0}")]
    Unsupported(String),
    #[error("not found: {0}")]
    NotFound(String),
}

/// A folder carrying an archive marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveFolderRef {
    pub folder: String,
    pub marker: ArchiveMarker,
}

/// What a gated post seals under (mirrors
/// `fauna_client_subscriptions::orchestration::TierGateMaterial`, kept
/// crate-local so the machine stays free of the subscriptions crate).
pub struct TierGate {
    pub tier: String,
    pub rank: u32,
    pub period_key: zeroize::Zeroizing<[u8; 32]>,
    pub period_version: u64,
    pub key_blob_ref: [u8; 32],
}

/// A resolved media upload — the `MediaItem` inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedMedia {
    pub blob_hash: [u8; 32],
    pub media_type: String,
    pub size_bytes: u64,
}

/// The seal a gated post's media rides under.
pub struct MediaSeal {
    pub seal_id: [u8; 32],
    pub tier: String,
    pub period_version: u64,
    pub period_key: zeroize::Zeroizing<[u8; 32]>,
}

/// One imported calendar event (owner-only; `archive-import.md` § What each
/// category becomes, the events row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedEvent {
    /// The iCalendar UID — `archive:<platform>:<external id>`.
    pub uid: String,
    pub summary: String,
    pub description: Option<String>,
    pub start: fauna_core::data::Timestamp,
    pub end: Option<fauna_core::data::Timestamp>,
    pub location: Option<String>,
    pub url: Option<String>,
    /// `going` / `interested` / `declined` / `invited` — `fauna_status` vocabulary.
    pub rsvp: String,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ArchiveNest: MaybeSendSync {
    /// `fauna.nest.info` advertises `hidden-tiers`.
    async fn supports_hidden_tiers(&self) -> Result<bool, ArchiveNestError>;
    /// Every owned folder whose `fauna-archive.cbor` decodes.
    async fn list_archive_folders(&self) -> Result<Vec<ArchiveFolderRef>, ArchiveNestError>;
    /// `fauna.folders.create` — a private sync-type folder.
    /// `Rejected { code: "fauna.folders.conflict" }` on a taken name.
    async fn create_folder(&self, name: &str) -> Result<(), ArchiveNestError>;
    /// Write `bytes` at `path` in `folder` (chunked, owner-sealed, recorded).
    async fn write_file(
        &self,
        folder: &str,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), ArchiveNestError>;
    /// The whole file, or `None` when no live record names `path`.
    async fn read_file(
        &self,
        folder: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, ArchiveNestError>;
    /// An `ArchiveSource` over the folder-resident raw zip (range reads), or
    /// `None` when the platform cannot bridge sync reads onto the async seam.
    async fn open_folder_archive(
        &self,
        folder: &str,
        path: &str,
    ) -> Result<Option<SharedSource>, ArchiveNestError>;
    async fn provision_owner_only_tier(&self) -> Result<TierGate, ArchiveNestError>;
    async fn provision_followers_tier(&self) -> Result<TierGate, ArchiveNestError>;
    /// `fauna.posts.create` → the post id (hex).
    async fn create_post(&self, signed_bytes: Vec<u8>) -> Result<String, ArchiveNestError>;
    async fn upload_public_media(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<UploadedMedia, ArchiveNestError>;
    async fn upload_sealed_media(
        &self,
        name: &str,
        bytes: Vec<u8>,
        seal: &MediaSeal,
    ) -> Result<UploadedMedia, ArchiveNestError>;
    /// Upload a gated post's sealed body blob → its hex hash.
    async fn upload_gated_body(&self, sealed: Vec<u8>) -> Result<String, ArchiveNestError>;
    /// Whether the actor's calendar can take events (CalDAV enabled, a calendar exists).
    async fn calendar_ready(&self) -> Result<bool, ArchiveNestError>;
    async fn put_event(&self, event: &ImportedEvent) -> Result<(), ArchiveNestError>;
}

/// Turns the Archive step's path into a readable source — a file on native,
/// a browser `File` on web.
pub trait ArchiveOpener: MaybeSendSync {
    fn open(&self, path: &str) -> Result<SharedSource, ArchiveNestError>;
    /// The archive's own file name (for `raw/<name>`).
    fn file_name(&self, path: &str) -> String;
}

#[cfg(any(test, feature = "test-helpers"))]
pub mod fakes {
    //! In-memory doubles for both seams — the `MailImportMachine` `FakeNest`
    //! precedent (`archive-import.md` § Testing, tier_1). Every knob the
    //! machine's own tests and the downstream integration tests drive lives
    //! here, so no consumer hand-rolls a second fake.

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use fauna_archive::VecSource;
    use fauna_core::subscription::{FOLLOWERS_TIER, OWNER_ONLY_TIER, OWNER_ONLY_TIER_RANK};

    use super::{
        ArchiveFolderRef, ArchiveNest, ArchiveNestError, ArchiveOpener, ImportedEvent, MediaSeal,
        SharedSource, TierGate, UploadedMedia,
    };
    use crate::state::{ArchiveMarker, ImportState, MARKER_PATH, STATE_PATH};

    /// One folder's path → bytes map; a fake nest is a map of those.
    type FolderFiles = BTreeMap<String, Vec<u8>>;

    /// An in-memory `ArchiveNest`: folders and their sealed files, the posts,
    /// media and events the import produced, and the knobs a test turns.
    pub struct FakeNest {
        folders: Mutex<BTreeMap<String, FolderFiles>>,
        /// Signed post bytes, in `create_post` order.
        posts: Mutex<Vec<Vec<u8>>>,
        events: Mutex<Vec<ImportedEvent>>,
        /// `(name, len, sealed)` per media upload, in call order.
        media: Mutex<Vec<(String, usize, bool)>>,
        supports_hidden_tiers: Mutex<bool>,
        calendar_ready: Mutex<bool>,
        /// `Some(n)`: the (n+1)-th `create_post` and every later one fails
        /// until [`FakeNest::clear_fault`] — the restart-resume test's fault.
        fail_post_after: Mutex<Option<usize>>,
        owner_only_calls: Mutex<usize>,
        followers_calls: Mutex<usize>,
        /// A permanent refusal every `provision_followers_tier` answers —
        /// the lost-key / not-honored arm the real seam maps to `Rejected`.
        refuse_followers: Mutex<Option<ArchiveNestError>>,
        #[cfg(not(target_arch = "wasm32"))]
        post_created: Arc<tokio::sync::Notify>,
    }

    impl Default for FakeNest {
        fn default() -> Self {
            Self::new()
        }
    }

    impl FakeNest {
        pub fn new() -> Self {
            Self {
                folders: Mutex::new(BTreeMap::new()),
                posts: Mutex::new(Vec::new()),
                events: Mutex::new(Vec::new()),
                media: Mutex::new(Vec::new()),
                supports_hidden_tiers: Mutex::new(true),
                calendar_ready: Mutex::new(true),
                fail_post_after: Mutex::new(None),
                owner_only_calls: Mutex::new(0),
                followers_calls: Mutex::new(0),
                refuse_followers: Mutex::new(None),
                #[cfg(not(target_arch = "wasm32"))]
                post_created: Arc::new(tokio::sync::Notify::new()),
            }
        }

        /// Every `provision_followers_tier` from now on answers `error` — a
        /// permanent refusal when it is `Rejected`, a fault otherwise.
        pub fn refuse_followers_provision(&self, error: ArchiveNestError) {
            *self.refuse_followers.lock().expect("fake nest") = Some(error);
        }

        /// What `fauna.nest.info` will answer (`hidden-tiers`).
        pub fn set_supports_hidden_tiers(&self, value: bool) {
            *self.supports_hidden_tiers.lock().expect("fake nest") = value;
        }

        pub fn set_calendar_ready(&self, value: bool) {
            *self.calendar_ready.lock().expect("fake nest") = value;
        }

        /// Plants an archive folder: the marker at `MARKER_PATH` and the
        /// run state at `STATE_PATH`, exactly as a real import wrote them.
        pub fn seed_folder(&self, name: &str, marker: &ArchiveMarker, state: &ImportState) {
            let mut files = FolderFiles::new();
            files.insert(
                MARKER_PATH.to_string(),
                fauna_cbor::encode_canonical(marker).expect("encode marker"),
            );
            files.insert(
                STATE_PATH.to_string(),
                fauna_cbor::encode_canonical(state).expect("encode state"),
            );
            self.folders
                .lock()
                .expect("fake nest")
                .insert(name.to_string(), files);
        }

        /// Forgets every folder — the archive folder deleted from Folders, or
        /// simply never there. A `hydrate` afterwards must clear the resume
        /// fields it set from a folder that is now gone.
        pub fn clear_folders(&self) {
            self.folders.lock().expect("fake nest").clear();
        }

        /// Every path written into `folder`, sorted.
        pub fn files_in(&self, folder: &str) -> Vec<String> {
            self.folders
                .lock()
                .expect("fake nest")
                .get(folder)
                .map(|f| f.keys().cloned().collect())
                .unwrap_or_default()
        }

        /// The posts the import authored, decoded, in creation order.
        pub fn posts(&self) -> Vec<fauna_core::data::Post> {
            self.posts
                .lock()
                .expect("fake nest")
                .iter()
                .map(|bytes| {
                    fauna_client_core::post::decode_post(bytes)
                        .expect("a stored post decodes")
                        .0
                })
                .collect()
        }

        /// `(name, byte length, sealed)` per media upload, in call order.
        pub fn media_uploads(&self) -> Vec<(String, usize, bool)> {
            self.media.lock().expect("fake nest").clone()
        }

        pub fn events(&self) -> Vec<ImportedEvent> {
            self.events.lock().expect("fake nest").clone()
        }

        /// The folder's `state/import.cbor`, decoded. Panics when absent —
        /// a test asking for it has already asserted the run got that far.
        pub fn read_state(&self, folder: &str) -> ImportState {
            let bytes = self
                .folders
                .lock()
                .expect("fake nest")
                .get(folder)
                .and_then(|f| f.get(STATE_PATH))
                .cloned()
                .unwrap_or_else(|| panic!("no {STATE_PATH} in folder {folder}"));
            fauna_cbor::decode_strict(&bytes).expect("decode import state")
        }

        pub fn owner_only_calls(&self) -> usize {
            *self.owner_only_calls.lock().expect("fake nest")
        }

        pub fn followers_calls(&self) -> usize {
            *self.followers_calls.lock().expect("fake nest")
        }

        /// After `n` successful `create_post` calls, fail every further one
        /// with `Transport("simulated")` until [`Self::clear_fault`].
        pub fn fail_post_after(&self, n: usize) {
            *self.fail_post_after.lock().expect("fake nest") = Some(n);
        }

        pub fn clear_fault(&self) {
            *self.fail_post_after.lock().expect("fake nest") = None;
        }

        /// Notified after every successful `create_post` — the latency-
        /// independent barrier a run test waits on (e2e convention 14).
        #[cfg(not(target_arch = "wasm32"))]
        pub fn on_post_created(&self) -> Arc<tokio::sync::Notify> {
            Arc::clone(&self.post_created)
        }

        fn media_type_for(name: &str) -> String {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                "image/jpeg".to_string()
            } else {
                "application/octet-stream".to_string()
            }
        }

        fn record_media(&self, name: &str, bytes: &[u8], sealed: bool) -> UploadedMedia {
            self.media
                .lock()
                .expect("fake nest")
                .push((name.to_string(), bytes.len(), sealed));
            UploadedMedia {
                blob_hash: *blake3::hash(bytes).as_bytes(),
                media_type: Self::media_type_for(name),
                size_bytes: bytes.len() as u64,
            }
        }

        fn gate(tier: &str, rank: u32) -> TierGate {
            TierGate {
                tier: tier.to_string(),
                rank,
                period_key: zeroize::Zeroizing::new([1u8; 32]),
                period_version: 1,
                key_blob_ref: [2u8; 32],
            }
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl ArchiveNest for FakeNest {
        async fn supports_hidden_tiers(&self) -> Result<bool, ArchiveNestError> {
            Ok(*self.supports_hidden_tiers.lock().expect("fake nest"))
        }

        async fn list_archive_folders(&self) -> Result<Vec<ArchiveFolderRef>, ArchiveNestError> {
            Ok(self
                .folders
                .lock()
                .expect("fake nest")
                .iter()
                .filter_map(|(folder, files)| {
                    let marker: ArchiveMarker =
                        fauna_cbor::decode_strict(files.get(MARKER_PATH)?).ok()?;
                    Some(ArchiveFolderRef {
                        folder: folder.clone(),
                        marker,
                    })
                })
                .collect())
        }

        async fn create_folder(&self, name: &str) -> Result<(), ArchiveNestError> {
            let mut folders = self.folders.lock().expect("fake nest");
            if folders.contains_key(name) {
                return Err(ArchiveNestError::Rejected {
                    code: "fauna.folders.conflict".to_string(),
                    detail: format!("a folder named {name} already exists"),
                });
            }
            folders.insert(name.to_string(), FolderFiles::new());
            Ok(())
        }

        async fn write_file(
            &self,
            folder: &str,
            path: &str,
            bytes: Vec<u8>,
        ) -> Result<(), ArchiveNestError> {
            let mut folders = self.folders.lock().expect("fake nest");
            let files = folders
                .get_mut(folder)
                .ok_or_else(|| ArchiveNestError::NotFound(folder.to_string()))?;
            files.insert(path.to_string(), bytes);
            Ok(())
        }

        async fn read_file(
            &self,
            folder: &str,
            path: &str,
        ) -> Result<Option<Vec<u8>>, ArchiveNestError> {
            Ok(self
                .folders
                .lock()
                .expect("fake nest")
                .get(folder)
                .and_then(|f| f.get(path))
                .cloned())
        }

        /// The fake keeps whole files, not chunk manifests, so it cannot
        /// bridge range reads — the platform-can't-do-it arm every caller
        /// must already handle.
        async fn open_folder_archive(
            &self,
            _folder: &str,
            _path: &str,
        ) -> Result<Option<SharedSource>, ArchiveNestError> {
            Ok(None)
        }

        async fn provision_owner_only_tier(&self) -> Result<TierGate, ArchiveNestError> {
            *self.owner_only_calls.lock().expect("fake nest") += 1;
            Ok(Self::gate(OWNER_ONLY_TIER, OWNER_ONLY_TIER_RANK))
        }

        async fn provision_followers_tier(&self) -> Result<TierGate, ArchiveNestError> {
            *self.followers_calls.lock().expect("fake nest") += 1;
            if let Some(error) = self.refuse_followers.lock().expect("fake nest").clone() {
                return Err(error);
            }
            Ok(Self::gate(FOLLOWERS_TIER, 0))
        }

        async fn create_post(&self, signed_bytes: Vec<u8>) -> Result<String, ArchiveNestError> {
            // The guard lives in a block of its own, not merely `drop`ped: an
            // `async fn` captures every binding whose scope spans an `await`,
            // so a `MutexGuard` still in scope below would make this future
            // `!Send` and the whole seam un-`tokio::spawn`-able.
            let id = {
                let mut posts = self.posts.lock().expect("fake nest");
                if let Some(n) = *self.fail_post_after.lock().expect("fake nest")
                    && posts.len() >= n
                {
                    return Err(ArchiveNestError::Transport("simulated".into()));
                }
                // Decoding here is what makes a malformed post a nest refusal
                // rather than a panic in `posts()` later.
                fauna_client_core::post::decode_post(&signed_bytes).map_err(|e| {
                    ArchiveNestError::Rejected {
                        code: "fauna.posts.invalid".to_string(),
                        detail: e.to_string(),
                    }
                })?;
                let id = blake3::hash(&signed_bytes).to_hex().to_string();
                // Content-addressed like the real nest (`store_post` upserts
                // by `blake3(body)`): the same bytes twice are ONE post, which
                // is what makes a replayed write-ahead and a byte-identical
                // re-authoring safe — and what a fake that appended blindly
                // would have overstated as duplicates.
                if !posts.contains(&signed_bytes) {
                    posts.push(signed_bytes);
                }
                id
            };
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.post_created.notify_waiters();
                // Hand the scheduler back before the run's next record. A real
                // `fauna.posts.create` always yields at its socket; the fake
                // never pends, so without this the whole run would finish
                // inside a single poll on a current-thread runtime and a
                // `Pause` dispatched *on* the barrier above could not land
                // mid-run. This is what makes the pause/cancel tests a causal
                // barrier rather than a settle sleep (e2e convention 14).
                tokio::task::yield_now().await;
            }
            Ok(id)
        }

        async fn upload_public_media(
            &self,
            name: &str,
            bytes: Vec<u8>,
        ) -> Result<UploadedMedia, ArchiveNestError> {
            Ok(self.record_media(name, &bytes, false))
        }

        async fn upload_sealed_media(
            &self,
            name: &str,
            bytes: Vec<u8>,
            _seal: &MediaSeal,
        ) -> Result<UploadedMedia, ArchiveNestError> {
            Ok(self.record_media(name, &bytes, true))
        }

        /// Echoes the hash of what it was handed, so the machine's own
        /// blob-reference check passes against a real hash.
        async fn upload_gated_body(&self, sealed: Vec<u8>) -> Result<String, ArchiveNestError> {
            Ok(blake3::hash(&sealed).to_hex().to_string())
        }

        async fn calendar_ready(&self) -> Result<bool, ArchiveNestError> {
            Ok(*self.calendar_ready.lock().expect("fake nest"))
        }

        async fn put_event(&self, event: &ImportedEvent) -> Result<(), ArchiveNestError> {
            self.events.lock().expect("fake nest").push(event.clone());
            Ok(())
        }
    }

    /// An [`ArchiveOpener`] over a path → zip-bytes map.
    #[derive(Default)]
    pub struct VecOpener {
        files: BTreeMap<String, Vec<u8>>,
    }

    impl VecOpener {
        pub fn insert(&mut self, path: &str, zip_bytes: Vec<u8>) {
            self.files.insert(path.to_string(), zip_bytes);
        }
    }

    impl ArchiveOpener for VecOpener {
        fn open(&self, path: &str) -> Result<SharedSource, ArchiveNestError> {
            let bytes = self
                .files
                .get(path)
                .ok_or_else(|| ArchiveNestError::NotFound(path.to_string()))?;
            Ok(Arc::new(VecSource(bytes.clone())))
        }

        fn file_name(&self, path: &str) -> String {
            path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
        }
    }
}
