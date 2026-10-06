//! The **source nest's in-process segment-backup coordinator** — the nest arm
//! (`docs/goal/architecture/message-segment-store.md` § Cross-location backup
//! protocol; `docs/goal/behavior/backup-restore.md` § Background Tasks;
//! `docs/goal/architecture/key-material-hierarchy.md` § Path A-sibling-0).
//!
//! The 2026-07-23 redesign moved backup of the nest-originated message kinds
//! nest-side: **the source nest itself** reads its own segment files, seals them
//! under the owner's granted `NestBackupKey`, and writes them to each destination
//! the owner registered — with **no client and no agent alive**. This module is
//! that driver. Everything it needs already exists as a primitive; its whole job
//! is to join them per owner:
//!
//! - **which owners** → [`CacheDb::list_nest_backup_key_owners`] (the grant store);
//! - **where to** → [`CacheDb::list_backup_destinations`] (the source-side
//!   registry — the nest cannot read the `fauna.state.backup` plane entries, which
//!   rest as client-sealed ciphertext, so the client registers each
//!   destination explicitly);
//! - **what to send** → [`NestLocalSegmentSource`] over this nest's own disk;
//! - **custody** → [`federation_pool::originate_backup_changes_record`];
//! - **bulk bytes** → a [`SyncEngine`] byte plane pointed at the destination
//!   under a token from [`federation_pool::originate_backup_write_token_mint`].
//!
//! ## Why this is its own arm, sharing leaves rather than a coordinator
//!
//! The **client**-driven arm (`fauna_sync_engine`'s `BackupCoordinator`)
//! retired wholesale at the slice-5 flip and was deleted 2026-08-17; its
//! `owner_secret`-holding shape was correct for a client and impossible for a
//! nest. What is shared is every leaf that could
//! otherwise drift into a data-loss bug: [`diff_segments`],
//! [`hash_segments_canonical`], [`LiveManifestMirror`], the `segment_backup_state`
//! rows, and above all the seal + chunk pipeline inside [`SyncEngine::upload_bytes`]
//! (a second implementation of the seal would produce backups the owner's client
//! could not open). The pass *sequence* below mirrors the retired client arm's
//! step-for-step, minus the destination provisioning it needed and this arm does not —
//! the destination's `fauna.federation.backup.changes.record` handler creates the
//! owner's reserved custody-copy set lazily behind its grant gate.
//!
//! ## Trust
//!
//! The nest authenticates to each destination **as itself** over the ordinary
//! `fauna.federation.hello` handshake and holds no owner secret. Its authority is
//! exactly the nest-writer grant row the owner's client wrote *at the destination*
//! — so the owner revokes it there, with this nest fully hostile. The seal key is
//! the one exception to "no nest holds a backup key", and it is sound because the
//! nest already hosts the plaintext it seals (§ Path A-sibling-0).
//!
//! ## State
//!
//! Per-owner: `<data_dir>/nest-backup/<owner_hex>/segment-backup.sqlite`. One DB
//! per owner rather than one shared DB, because several rows of the shared
//! `SyncDb` schema key on `dest_id` alone (`backup_destination_seen`,
//! `max_manifest_synced_at_for_dest`) — on a multi-tenant nest two owners who
//! both name a destination `"dest-1"` would otherwise cross-contaminate each
//! other's upload state. Per-owner files make that unrepresentable and let the
//! shared leaves be reused verbatim.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_core::crypto::{NestBackupKey, OwnerSealKey};
use fauna_nest_http::{ApiError, BearerSource};
use fauna_protocol::backup::BackupDestinationStatusItem;
use fauna_protocol::segments::SegmentRef;
use fauna_sync_engine::db::{SeenBackupDestination, SyncDb};
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::nest_client::SyncClient;
use fauna_sync_engine::segment_backup::{
    RunReport, SegmentFamily, SegmentListing, SegmentSource, diff_segments,
    hash_segments_canonical, live_manifest_mirror,
};
use fauna_sync_engine::write_token_bearer::WriteTokenBearer;

use crate::db::backup_destinations::BackupDestinationRow;
use crate::federation_handlers::FedBackupChangesRecordRequest;
use crate::federation_pool;
use crate::routes::AppState;
use crate::segments::backup_source::NestLocalSegmentSource;

/// Segment kinds this arm backs up — the **shared** list, re-exported rather
/// than restated. It was a second independent `["mail"]` "in lockstep by
/// comment only" until 2026-07-30; see [`fauna_sync_engine::segment_backup::BACKED_UP_KINDS`]
/// for why a comment stopped being enough once a client began yielding the
/// whole kind to a nest holder.
pub use fauna_sync_engine::segment_backup::BACKED_UP_KINDS;

/// Directory under the nest data dir holding one per-owner state DB.
const STATE_SUBDIR: &str = "nest-backup";

/// How many consecutive **wholly-failed** sweeps an owner may have before this
/// nest stops claiming their `backup-upload` lease.
///
/// At the shared `PERIODIC_INTERVAL` (15 min) that is ~45 minutes of a nest
/// getting nowhere before it hands the kind back. Deliberately not `1`: the
/// nest never preempts a fresh foreign holder
/// ([`crate::delegation_runner::may_claim`]), so releasing on a single
/// transient blip would park the kind on whichever client picked it up until
/// *that* holder went stale. Three sustained failures is a stalled nest, not a
/// flapping network.
pub const MAX_CONSECUTIVE_FAILED_PASSES: u32 = 3;

/// "Not `1`" above is a consequence of the never-preempt rule, not a taste —
/// pinned here rather than in a test so tuning the constant down to `1` fails
/// the build with the reason attached.
const _: () = assert!(
    MAX_CONSECUTIVE_FAILED_PASSES > 1,
    "the threshold must tolerate a transient failure: once a client claims the \
     released lease this nest cannot preempt it back, so releasing on one blip \
     parks the kind on that client indefinitely",
);

/// The three things a custody relay needs to reach a destination: who it is,
/// where it is, and the id its handshake is pinned against.
///
/// It is **not** a [`BackupDestinationRow`] because the removal teardown's
/// destination has no registry row any more — that deregistration is what
/// triggers the teardown. Borrowing exactly the three fields keeps the departed
/// case from having to fake the other five.
#[derive(Debug, Clone, Copy)]
struct DestinationRef<'a> {
    destination_id: &'a str,
    nest_url: &'a str,
    nest_id: &'a [u8],
}

impl<'a> From<&'a BackupDestinationRow> for DestinationRef<'a> {
    fn from(row: &'a BackupDestinationRow) -> Self {
        Self {
            destination_id: &row.destination_id,
            nest_url: &row.nest_url,
            nest_id: &row.nest_id,
        }
    }
}

/// A stored 32-byte scope id, or a typed error naming the row that is wrong —
/// the state DB is this nest's own, so a mis-sized scope is corruption, not
/// input.
fn to_scope_id(stored: &[u8]) -> Result<[u8; 32]> {
    stored.try_into().map_err(|_| {
        anyhow::anyhow!(
            "segment-backup state holds a {}-byte scope_id (expected 32)",
            stored.len()
        )
    })
}

/// The attribution `kind` a folder-mirror custody record carries. The
/// destination never derives a set name from it (the additive `folder_id` +
/// its verified view of this nest do that — `federation_handlers::
/// resolve_backup_custody_set`); it exists so custody rows and logs say which
/// plane wrote them.
pub(crate) const FOLDER_MIRROR_KIND: &str = "folder";

/// What one [`NestBackupCoordinator::run_folder_once`] mirror pass achieved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FolderRunReport {
    /// Paths whose manifest+chunks were pushed and custody-recorded this pass.
    pub uploaded_paths: usize,
    /// Paths tombstoned (deleted source-side, or the folder itself is gone).
    pub dropped_paths: usize,
    /// Live paths **withheld** from the destination this pass because a
    /// legal takedown withholds their bytes (`moderation.md` § Legal takedown
    /// → *The blob-serve door* → *What the withhold binds on owner- and
    /// admin-scoped routes*, path 3): not pushed, and retracted if an earlier
    /// pass had pushed them. The local file is untouched and the path is
    /// re-tried every pass, so it mirrors the moment the flag lifts.
    pub withheld_paths: usize,
    /// Manifests this pass opened (`store.get` + decrypt) to test their store
    /// keys against the withheld set — the row 729 witness figure. Zero on an
    /// unchanged folder once the withheld set has not changed since the last
    /// full check (`folder_withhold_checkpoint`), even while a takedown
    /// stands elsewhere on the box; every not-yet-confirmed or newly-changed
    /// path still opens exactly as before.
    pub manifests_opened: usize,
}

/// A stored manifest blob's plaintext bytes **and** its parse — the mirror
/// needs both: the plaintext bytes are what the destination's
/// `/api/v1/manifests` route accepts (it re-frames them itself), and the parse
/// yields the chunk store keys to copy. The same decode as
/// [`crate::backup::gc::decode_manifest`].
fn manifest_plaintext(
    raw: &[u8],
    key: Option<&fauna_core::crypto::BackupKey>,
) -> Result<(Vec<u8>, fauna_core::chunk::ChunkManifest)> {
    let p = crate::backup::decode_blob(raw, key)?;
    let m = fauna_core::encoding::canonical_decode(&p)
        .map_err(|e| anyhow::anyhow!("stored manifest does not decode as a ChunkManifest: {e}"))?;
    Ok((p, m))
}

/// A stable digest over the withheld set's **content** — a domain-tagged
/// blake3 hash of the sorted digests. The folder mirror's already-mirrored
/// short-circuit uses this, not a bump-on-write generation counter:
/// `replace_blob_legal_withhold` rewrites the set wholesale on every
/// *complete* GC sweep, not only on an admin act, so a counter bumped on
/// every replace would defeat the memo on every sweep even when the set's
/// content did not change. Sorting first makes the digest independent of the
/// `HashSet`'s iteration order.
fn withheld_set_digest(withheld: &HashSet<[u8; 32]>) -> [u8; 32] {
    let mut sorted: Vec<&[u8; 32]> = withheld.iter().collect();
    sorted.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"fauna-folder-mirror-withhold-digest-v1");
    for digest in sorted {
        hasher.update(digest);
    }
    *hasher.finalize().as_bytes()
}

/// A stored chunk blob's wire body for a byte-identical re-upload: the store
/// frames every chunk-pipeline write (`backup::encode_blob`), so the wire body
/// is the un-framed payload — verified here against the store key (`blake3` of
/// the wire body) with a raw-layer fallback, mirroring the store's own
/// two-layer integrity rule, so a blob whose stored framing disagrees with the
/// store key can never be forwarded as corrupt bytes.
fn chunk_wire_body(
    key: &fauna_core::data::ContentHash,
    raw: &[u8],
    at_rest_key: Option<&fauna_core::crypto::BackupKey>,
) -> Result<Vec<u8>> {
    if let Ok(body) = crate::backup::decode_blob(raw, at_rest_key)
        && fauna_core::encoding::content_hash(&body) == *key
    {
        return Ok(body);
    }
    if fauna_core::encoding::content_hash(raw) == *key {
        return Ok(raw.to_vec());
    }
    anyhow::bail!(
        "stored chunk {} re-hashes to neither layer — refusing to mirror corrupt bytes",
        hex::encode(key.digest())
    )
}

/// What one [`NestBackupCoordinator::run_all_tuples`] pass achieved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PassOutcome {
    /// `(destination, kind)` tuples tried.
    pub attempted: usize,
    /// How many of them returned an error.
    pub failed: usize,
}

impl PassOutcome {
    /// Every tuple tried, and every one failed. An owner with no tuples is
    /// **not** wholly failed — there was nothing to get wrong, and sufficiency
    /// already requires a registered destination.
    pub fn wholly_failed(&self) -> bool {
        self.attempted > 0 && self.failed == self.attempted
    }
}

/// Per-owner health of this nest's own backup passes — the progress half of the
/// `backup-upload` lease's sufficiency predicate.
///
/// **Why a lease needs this at all**.
/// Sufficiency used to be purely configuration-shaped: a granted
/// `NestBackupKey` plus a registered destination. A nest whose every pass fails
/// (destination unreachable, its nest-writer grant revoked destination-side,
/// TLS, quota) keeps satisfying that predicate and heartbeats the lease
/// forever — and since the slice-5 flip, a client observing a fresh
/// `AlwaysOnNest` holder **stands down**. So the owner's segments silently stop
/// being backed up by anyone, with the nest's own row still claiming it runs
/// the kind. Before the flip an eligible desktop simply preempted the stalled
/// nest and uploaded; this restores that coverage.
///
/// The nest's own runner already **releases** on lost sufficiency (holder-checked
/// release + a `lease_changed` push, `delegation_runner::run_pass_for_kind`), so
/// handover is prompt rather than a `LEASE_STALE_MS` wait.
///
/// **The worker keeps sweeping a stalled owner.** Releasing the lease is not
/// standing down: if the worker stopped too, nothing would ever re-establish
/// success and the release would be permanent — a client and this nest both
/// uploading is the safe direction (uploads are content-addressed and
/// idempotent), a kind nobody uploads is not.
///
/// **In-memory, never persisted** — like the lease blackboard itself. A restart
/// forgets the failures, the nest re-claims, and one more failed sweep-run
/// re-establishes the truth; that is cheaper and more honest than a persisted
/// counter that can outlive the condition it describes.
#[derive(Default)]
pub struct BackupPassHealth {
    consecutive_failures: std::sync::Mutex<HashMap<[u8; 32], u32>>,
}

impl BackupPassHealth {
    /// Record one completed sweep for `owner`. Any success resets the counter —
    /// a nest reaching one of several destinations is making progress, and a
    /// takeover would not help the destination that is actually broken.
    pub fn record_pass(&self, owner: [u8; 32], outcome: PassOutcome) {
        let mut map = self.consecutive_failures.lock().unwrap();
        if outcome.wholly_failed() {
            let n = map.entry(owner).or_insert(0);
            *n = n.saturating_add(1);
            if *n == MAX_CONSECUTIVE_FAILED_PASSES {
                tracing::warn!(
                    owner = %hex::encode(owner),
                    failures = *n,
                    "nest segment backup: every pass has failed — releasing the \
                     backup-upload lease so a client can take over"
                );
            }
        } else {
            map.remove(&owner);
        }
    }

    /// Whether `owner`'s passes have failed often enough in a row that this nest
    /// should stop claiming their lease.
    pub fn is_stalled(&self, owner: &[u8; 32]) -> bool {
        self.consecutive_failures
            .lock()
            .unwrap()
            .get(owner)
            .is_some_and(|n| *n >= MAX_CONSECUTIVE_FAILED_PASSES)
    }

    /// Drop owners this nest no longer sweeps (grant revoked, last destination
    /// removed), so the map cannot outgrow the enrolled set.
    pub fn retain_owners(&self, live: &HashSet<[u8; 32]>) {
        self.consecutive_failures
            .lock()
            .unwrap()
            .retain(|owner, _| live.contains(owner));
    }
}

/// Whether the coordinator's push sweep acts on this registry row: a peer-nest
/// destination. Every enrolled nest destination is acted on — the row declares
/// no capability (`backup-destinations.md` § State & data shape →
/// *Capability*; the former `mode` filter dropped nothing and is deleted).
///
/// Written as "is nest", not "is not client-device", so an unknown *future*
/// kind is excluded too — a build that does not understand a kind must never
/// dial it. Shared with `delegation_runner::backup_upload_sufficient_owners`,
/// which must enumerate exactly this worker's sweep set — one predicate, so the
/// two cannot drift into disagreeing about which destinations count.
pub(crate) fn is_push_destination(d: &BackupDestinationRow) -> bool {
    d.kind == fauna_core::data::DESTINATION_KIND_NEST
}

/// Where this nest keeps per-owner segment-backup state: the nest data dir —
/// the same one that holds the nest identity, the segment stores and the enable
/// flags — through the one shared derivation
/// ([`crate::mail_enable::data_dir_from_db_path`]), so it needs no new config
/// surface (bucket 1: no human chooses it).
///
/// A nest with no resolvable data dir is a **hard error**, not a `.`-relative
/// fallback: writing a user's backup state into whatever the process CWD happens
/// to be would be silently unrecoverable across a restart.
fn state_root(state: &AppState) -> Result<PathBuf> {
    crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path)
        .map(|d| d.join(STATE_SUBDIR))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "segment backup needs a nest data dir, but nest.db_path ({:?}) has no parent",
                state.config.nest.db_path
            )
        })
}

/// Map a federation-originate failure into the bearer's error vocabulary.
///
/// The split is load-bearing in the same way it is for the client's cross-nest
/// bearer: a **grant refusal** is terminal (the owner revoked this nest's write
/// authority — retrying forever would spin), while anything else is a transport
/// fault the next pass should retry. Surfacing a revocation as `Transport` would
/// hide it as a network blip; parking on a real blip would strand a
/// perfectly-granted nest.
fn classify_mint_error(e: &federation_pool::PoolError) -> ApiError {
    let msg = e.to_string();
    // The writer gate's typed refusal; `forbidden` is the custody set's own
    // (the name is a live rail on the destination).
    if msg.contains("writer_not_seated") || msg.contains("forbidden") {
        ApiError::Status {
            code: 403,
            message: format!("nest-backup write_token.mint refused: {msg}"),
        }
    } else {
        ApiError::Transport(format!("nest-backup write_token.mint: {msg}"))
    }
}

/// One owner's live backup context: their seal key, their registered
/// destinations, and the local state DB tracking what has already been written
/// to each. Cheap to construct (opens one SQLite file); built per pass by
/// [`NestBackupWorker`] and per call by the `fauna.backup.status` handler.
pub struct NestBackupCoordinator {
    state: Arc<AppState>,
    owner: [u8; 32],
    /// The owner-granted seal. Every uploaded chunk roots here; a destination
    /// stores opaque ciphertext.
    seal: OwnerSealKey,
    /// The per-owner state DB, behind a mutex **for `Sync`, not for contention**
    /// — a `rusqlite::Connection` is `Send` but not `Sync`, and the hosting loop
    /// holds `&self` across `.await`s. Guards are per-statement by construction:
    /// holding one across an `.await` would make the loop future `!Send` and fail
    /// to compile, which is the guardrail keeping this honest.
    sync_db: std::sync::Mutex<SyncDb>,
    destinations: Vec<BackupDestinationRow>,
    /// Ordinary-folder coverage rows `(destination_id, folder_id)` for the nest
    /// destinations above — the sweep's second axis
    /// (`backup-destinations.md` § Ordinary-folder coverage). Loaded fresh with
    /// the coordinator (built per pass), so an attach/detach lands next tick.
    /// The row's display name is the listing's concern, not the sweep's:
    /// custody never carries it.
    covered_folders: Vec<(String, i64)>,
    source: Arc<NestLocalSegmentSource>,
    /// Inert scratch the destination engines are constructed against — they only
    /// ever `upload_bytes`, never watch a tree.
    scratch_dir: PathBuf,
}

impl NestBackupCoordinator {
    /// Build the coordinator for `owner`, or `Ok(None)` when this owner has not
    /// granted a `NestBackupKey` — the enrollment gate. Returning `None` rather
    /// than erroring keeps "not enrolled" distinct from "enrolled and broken" at
    /// every call site.
    ///
    /// Opens (creating on first use) the owner's state DB. A registered
    /// destination that is not a push destination ([`is_push_destination`]) is
    /// dropped here, so no later step has to re-check it.
    pub async fn open_for_owner(state: Arc<AppState>, owner: [u8; 32]) -> Result<Option<Self>> {
        let Some(key_bytes) = state
            .db
            .get_nest_backup_key(&owner)
            .await
            .context("read granted NestBackupKey")?
        else {
            return Ok(None);
        };
        let key: [u8; 32] = key_bytes.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "granted NestBackupKey for {} is {} bytes, not 32",
                hex::encode(owner),
                key_bytes.len()
            )
        })?;

        // Peer-nest rows only. The kind filter is what keeps this **push**
        // coordinator away from a destination that has no address: a
        // `client-device` row carries an empty `nest_url` and an empty
        // `nest_id`, and `run_pass` would otherwise hand it to `run_once` and
        // federation-dial it every tick (`behavior/backup-destinations.md`
        // § Custodian contract, question 2: nest and S3 are nest-driven push; a
        // client device is destination-driven pull).
        let destinations: Vec<BackupDestinationRow> = state
            .db
            .list_backup_destinations(&owner)
            .await
            .context("read registered backup destinations")?
            .into_iter()
            .filter(is_push_destination)
            .collect();

        // Ordinary-folder coverage, restricted to the same push set: a
        // client-device destination's coverage is that device's own pull to
        // serve, never this coordinator's dial.
        let push_dests: HashSet<&str> = destinations
            .iter()
            .map(|d| d.destination_id.as_str())
            .collect();
        let covered_folders: Vec<(String, i64)> = state
            .db
            .list_backup_destination_folders(&owner)
            .await
            .context("read folder coverage rows")?
            .into_iter()
            .filter(|c| push_dests.contains(c.destination_id.as_str()))
            .map(|c| (c.destination_id, c.folder_id))
            .collect();

        let owner_dir = state_root(&state)?.join(hex::encode(owner));
        std::fs::create_dir_all(&owner_dir)
            .with_context(|| format!("create backup state dir {}", owner_dir.display()))?;
        let sync_db = SyncDb::open(owner_dir.join("segment-backup.sqlite"))
            .context("open per-owner segment-backup state db")?;

        let source = Arc::new(NestLocalSegmentSource::new(
            Arc::clone(&state),
            hex::encode(state.nest_identity.public_key_bytes()),
        ));

        Ok(Some(Self {
            state,
            owner,
            seal: OwnerSealKey::SourceNest(NestBackupKey::from_bytes(key)),
            sync_db: std::sync::Mutex::new(sync_db),
            destinations,
            covered_folders,
            source,
            scratch_dir: owner_dir,
        }))
    }

    /// The owner's registered push destinations ([`is_push_destination`]).
    pub fn destinations(&self) -> &[BackupDestinationRow] {
        &self.destinations
    }

    /// Borrow the state DB for **one statement**. Never bind the guard across an
    /// `.await` (see the field doc) — bind the *result* instead.
    fn db(&self) -> std::sync::MutexGuard<'_, SyncDb> {
        self.sync_db.lock().expect("segment-backup state db mutex")
    }

    /// Build this owner's byte plane to one destination: a [`SyncEngine`] whose
    /// HTTP `SyncClient` presents a federation-minted, short-TTL, write-only bulk
    /// token, sealing every chunk under the owner's `NestBackupKey`.
    ///
    /// Three deliberate choices, each the honest one for a nest that holds no
    /// owner secret:
    ///
    /// * [`fauna_client::AuthClient::bearer_only`] — the constructor for a party
    ///   that has the owner's **public** actor id and a bearer, and no keypair.
    ///   The alternative constructor would demand an owner `ActorKeypair` this
    ///   nest must never have.
    /// * the device id is **this nest's own id**. The destination treats a source
    ///   nest's `device_id` as attribution only (it registers no device there),
    ///   so naming itself is both truthful and greppable in the custody rows.
    /// * the engine's control-plane `NestClient` is the nest's own identity. It
    ///   is unreachable on this path — `upload_bytes` never touches it (only the
    ///   conflict path does, which segment backup has no notion of) — and a
    ///   fabricated keypair there would read as if the nest impersonated someone.
    ///
    /// The raw HTTP byte plane to one destination — the bearer-minting
    /// [`SyncClient`] that [`Self::destination_engine`] wraps.
    ///
    /// The **folder-mirror pass drives this directly** rather than through the
    /// engine: a covered folder's bytes are already at-rest ciphertext and are
    /// copied as-is, so the engine's `upload_bytes` seal pipeline must never
    /// touch them (`backup-destinations.md` § Ordinary-folder coverage — no
    /// re-seal, no `NestBackupKey`).
    fn destination_sync_client(&self, dest: &BackupDestinationRow) -> SyncClient {
        let pool = Arc::clone(&self.state.federation_pool);
        let state = Arc::clone(&self.state);
        let peer_url = dest.nest_url.clone();
        let pinned_nest_id = dest.nest_id.clone();
        let owner_hex = hex::encode(self.owner);
        let bearer: Arc<dyn BearerSource> = Arc::new(WriteTokenBearer::from_minter(move || {
            let pool = Arc::clone(&pool);
            let state = Arc::clone(&state);
            let peer_url = peer_url.clone();
            let pinned_nest_id = pinned_nest_id.clone();
            let owner_hex = owner_hex.clone();
            async move {
                let reply = federation_pool::originate_backup_write_token_mint(
                    &pool,
                    &state,
                    &peer_url,
                    &pinned_nest_id,
                    &owner_hex,
                )
                .await
                .map_err(|e| classify_mint_error(&e))?;
                Ok((reply.token, reply.expires_at))
            }
        }));

        let nest_id = self.state.nest_identity.public_key_bytes();
        let byte_auth = Arc::new(fauna_client::AuthClient::bearer_only(
            dest.nest_url.clone(),
            self.owner,
            bearer,
            self.state.http_client.clone(),
        ));
        SyncClient::new(byte_auth, &nest_id)
    }

    fn destination_engine(&self, dest: &BackupDestinationRow, folder: &str) -> SyncEngine {
        use fauna_core::identity::ActorKeypair;

        let nest_id = self.state.nest_identity.public_key_bytes();
        SyncEngine::new(
            self.scratch_dir.clone(),
            // The engine's own DB backs `upload_file`'s resume queue, which
            // `upload_bytes` deliberately skips — the coordinator re-drives a
            // failed pass instead. Nothing here needs to outlive the pass.
            SyncDb::open_in_memory().expect("in-memory sync db"),
            self.destination_sync_client(dest),
            Some(folder.to_string()),
            nest_id,
            None, // mls — segment backup never touches MLS
            None, // epoch_secret
            Some(self.seal.clone()),
            None, // mls_group_id: owner-scoped backup, never a shared set
            None, // content_keys: owner-scoped backup has no M2 generations
            fauna_core::format::ConflictPolicy::default(),
            fauna_core::format::FormatRegistry::new(),
            fauna_sync_engine::ignore::IgnoreMatcher::default(),
            4,
            fauna_sync_engine::transfer::TransferPool::new(
                Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(8)),
                None,
            ),
            fauna_client::NestClient::new(
                dest.nest_url.clone(),
                ActorKeypair::from_secret(self.state.nest_identity.signing_key.to_bytes()),
            ),
            // Backup: this engine only ever pushes the nest's own segments to a
            // backup destination (`upload_bytes`), so it never pulls and never
            // reaches the delete arm. Naming the mode it actually serves keeps
            // the value honest if a pull path is ever added here.
            fauna_sync_engine::config::SyncMode::Backup,
        )
    }

    /// Relay one custody record to the destination over the federation channel.
    /// Exactly-once by content on the serving side, so a retried pass converges
    /// rather than duplicating.
    async fn record_custody(
        &self,
        dest: DestinationRef<'_>,
        kind: &str,
        scope_id: &[u8; 32],
        path: &str,
        manifest_hash: Option<String>,
        size_bytes: i64,
        change_type: &str,
    ) -> Result<()> {
        let req = FedBackupChangesRecordRequest {
            owner_actor_id: hex::encode(self.owner),
            kind: kind.to_string(),
            scope_id: hex::encode(scope_id),
            device_id: hex::encode(self.state.nest_identity.public_key_bytes()),
            path: path.to_string(),
            manifest_hash,
            size_bytes,
            change_type: change_type.to_string(),
            folder_id: None,
            path_sealed: None,
        };
        federation_pool::originate_backup_changes_record(
            &self.state.federation_pool,
            &self.state,
            dest.nest_url,
            dest.nest_id,
            &req,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| {
            format!(
                "record backup custody destination={} kind={kind} path={path}",
                dest.destination_id
            )
        })?;
        Ok(())
    }

    /// Relay one **ordinary-folder mirror** custody record. `folder_id` present
    /// makes the destination derive the `__folder/<hex>/<id>` set from its own
    /// verified view of this nest (`federation_handlers::resolve_backup_custody_set`);
    /// the kind is attribution only on this axis. Same exactly-once-by-content
    /// contract as [`Self::record_custody`].
    #[allow(clippy::too_many_arguments)]
    async fn record_folder_custody(
        &self,
        dest: DestinationRef<'_>,
        folder_id: i64,
        path: &str,
        manifest_hash: Option<String>,
        size_bytes: i64,
        change_type: &str,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<()> {
        let req = FedBackupChangesRecordRequest {
            owner_actor_id: hex::encode(self.owner),
            kind: FOLDER_MIRROR_KIND.to_string(),
            scope_id: hex::encode(self.owner),
            device_id: hex::encode(self.state.nest_identity.public_key_bytes()),
            path: path.to_string(),
            manifest_hash,
            size_bytes,
            change_type: change_type.to_string(),
            folder_id: Some(folder_id),
            path_sealed,
        };
        federation_pool::originate_backup_changes_record(
            &self.state.federation_pool,
            &self.state,
            dest.nest_url,
            dest.nest_id,
            &req,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| {
            format!(
                "record folder-mirror custody destination={} folder={folder_id} path={path}",
                dest.destination_id
            )
        })?;
        Ok(())
    }

    /// One `(destination, kind)` pass over the kind's **content** family, with
    /// the source segment list supplied. The journal family is
    /// [`Self::run_family_with_src_refs`]'s other caller; [`Self::run_once`]
    /// walks both.
    pub async fn run_once_with_src_refs(
        &self,
        dest: &BackupDestinationRow,
        kind: &str,
        src_listing: SegmentListing,
    ) -> Result<RunReport> {
        self.run_family_with_src_refs(dest, kind, SegmentFamily::Content, src_listing)
            .await
    }

    /// One `(destination, kind, family)` pass, with the source segment list
    /// supplied.
    ///
    /// Step-for-step the retired client arm's `run_once_with_src_refs`, and the ordering
    /// carries the same **data-loss** guarantee: each path's custody is recorded
    /// on the destination *before* the local `segment_backup_state` advances, so
    /// a record failure re-uploads (cheaply, via chunk dedup) and re-records next
    /// pass rather than marking a segment done that the destination's GC never
    /// learned to keep.
    ///
    /// **One body for both families of a backed-up kind**
    /// (`segment-backup-protocol.md` § Client-device custodian (pull) →
    /// *Restore* → *The placement journal rides the set*). Three names carry
    /// the whole difference, and keeping them apart is the point:
    ///
    /// - `kind` — the backed-up kind. It names the **set** the custody rests
    ///   in, so every custody record carries it, journal paths included: the
    ///   journal rides the kind's own set.
    /// - `tag` — the family's serve tag. It is what the source is read under
    ///   and what this family's upload state is filed under, so the two
    ///   families' state rows can never be diffed against each other.
    /// - `family` — the path family inside the set.
    pub async fn run_family_with_src_refs(
        &self,
        dest: &BackupDestinationRow,
        kind: &str,
        family: SegmentFamily,
        src_listing: SegmentListing,
    ) -> Result<RunReport> {
        let src_refs = &src_listing.segments;
        let tag = family
            .serve_kind(kind)
            .ok_or_else(|| anyhow::anyhow!("kind {kind} has no {family:?} family"))?;
        // The backup scope for mail is the owner's own actor; per-kind rollouts
        // bring their own scope (conv scopes on channel_id).
        let scope_id = self.owner;
        let scope_hex = hex::encode(scope_id);
        let folder = crate::db::sync_storage::reserved_backup_set_name(kind, &scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no backup surface: {kind}"))?;
        let engine = self.destination_engine(dest, &folder);

        // Remember we have written to this destination, so a later pass can tell
        // a departed destination's stale local rows from a live one's — and can
        // still reach it: the pinned `nest_id` is what lets the removal teardown
        // dial a destination the registry no longer carries.
        self.db().record_backup_destination_seen(
            &dest.destination_id,
            &dest.nest_url,
            &folder,
            &dest.nest_id,
        )?;

        let local = self
            .db()
            .list_segment_backup_state(&dest.destination_id, &scope_id, tag)?;
        // `diff.to_backfill_meta` names rows recorded without their sidecar;
        // this arm records every segment with its `.meta`, so it has none (the
        // client custodian's pull is the arm that backfills).
        let diff = diff_segments(src_refs, &local);
        let mut report = RunReport::default();

        for src_ref in &diff.to_upload {
            let pair = self
                .source
                .segment_pair(tag, &scope_hex, src_ref.segment_id)
                .await?;
            // The pair was read under the scope lock, but the listing this
            // pass diffed against may predate it (a rotation in between):
            // custody must name exactly the bytes the mirror will describe.
            pair.verify_against(src_ref)?;
            let dat_size = pair.dat.len() as i64;
            let meta_size = pair.meta.len();
            let rel_path = family.dat_path(&scope_hex, src_ref.segment_id);
            let meta_rel_path = family.meta_path(&scope_hex, src_ref.segment_id);

            let manifest_hash = engine
                .upload_bytes(pair.dat, &rel_path, &folder)
                .await
                .with_context(|| {
                    format!(
                        "upload segment destination={} kind={kind} seg={}",
                        dest.destination_id, src_ref.segment_id
                    )
                })?;
            self.record_custody(
                dest.into(),
                kind,
                &scope_id,
                &rel_path,
                Some(hex::encode(manifest_hash.digest())),
                dat_size,
                "create",
            )
            .await?;

            // The sidecar: same set, sibling path, same seal, its own custody
            // row — the half that makes the segment reopenable. Its record
            // lands before the state row advances, like the `.dat`'s, so a
            // tear between the two re-pushes the pair (dedup makes that free)
            // rather than marking a half-corpus done.
            let meta_manifest_hash = engine
                .upload_bytes(pair.meta, &meta_rel_path, &folder)
                .await
                .with_context(|| {
                    format!(
                        "upload segment sidecar destination={} kind={kind} seg={}",
                        dest.destination_id, src_ref.segment_id
                    )
                })?;
            self.record_custody(
                dest.into(),
                kind,
                &scope_id,
                &meta_rel_path,
                Some(hex::encode(meta_manifest_hash.digest())),
                meta_size as i64,
                "create",
            )
            .await?;

            self.db().put_segment_backup_state(
                &dest.destination_id,
                &scope_id,
                tag,
                src_ref.segment_id,
                src_ref.record_count as u64,
                src_ref.size_bytes,
                Some(meta_size as u64),
            )?;
            report.uploaded_segments.push(src_ref.segment_id);
        }

        // Compacted-out segments: tombstone the paths first so the destination's
        // GC reclaims, then forget them locally (a failed record is retried
        // because the local row survives). The sidecar's path tombstones only
        // if this coordinator ever placed one there.
        for seg_id in &diff.to_drop {
            let rel_path = family.dat_path(&scope_hex, *seg_id);
            self.record_custody(dest.into(), kind, &scope_id, &rel_path, None, 0, "delete")
                .await?;
            if local
                .get(seg_id)
                .is_some_and(|row| row.last_meta_size.is_some())
            {
                let meta_rel_path = family.meta_path(&scope_hex, *seg_id);
                self.record_custody(
                    dest.into(),
                    kind,
                    &scope_id,
                    &meta_rel_path,
                    None,
                    0,
                    "delete",
                )
                .await?;
            }
            self.db()
                .delete_segment_backup_state(&dest.destination_id, &scope_id, tag, *seg_id)?;
            report.dropped_segments.push(*seg_id);
        }

        // The `manifest.<kind>` mirror, only when something moved.
        let current_hash = hash_segments_canonical(src_refs);
        let prior_hash =
            self.db()
                .get_segment_backup_manifest_state(&dest.destination_id, &scope_id, tag)?;
        let manifest_changed = prior_hash.as_ref() != Some(&current_hash);
        // Whether THIS pass actually moved real content — distinct from
        // `manifest_changed`, which also fires on an owner's first pass with
        // zero segments (`None != Some(empty_hash)`). Only this drives the
        // UI-facing "Last synced" timestamp (`docs/goal/behavior/backup-destinations.md` §
        // Per-destination status read).
        // A pass that places a `.meta` counts here, which is what keeps this
        // arm's corpus ANCHORED
        // (`../../../docs/goal/architecture/message-segment-store.md`
        // § Cross-location backup protocol → *A sidecar in the corpus is
        // anchored, or it is not restored*): it also re-uploads the mirror that
        // names it, so the corpus never holds a sidecar restore has nothing to
        // verify against. This arm needs no
        // ordering machinery for that — unlike the custodian's, which is capped
        // and had to store the mirror FIRST. It has no cap, its `src_listing`
        // comes from `list_handler::segment_listing`, which always advertises
        // `meta_blake3_hex` and the manifest's saved counter,
        // and a crash between the two heals on the very next pass: the upload
        // below is what advances `prior_hash`, so a crash before it leaves
        // `manifest_changed` true and the mirror is re-uploaded.
        let content_moved =
            !report.uploaded_segments.is_empty() || !report.dropped_segments.is_empty();

        if content_moved || manifest_changed {
            // The ledger's generation is the source's saved counter
            // (`live_manifest_mirror`) — the same choice the custodian pull
            // makes, so a device pins one notion of generation.
            let mirror = live_manifest_mirror(&src_listing);
            let mirror_bytes = mirror.to_bytes()?;
            let mirror_size = mirror_bytes.len() as i64;
            let rel_path = family.mirror_path(&scope_hex, kind);
            let mirror_manifest_hash = engine
                .upload_bytes(mirror_bytes, &rel_path, &folder)
                .await
                .with_context(|| {
                    format!(
                        "upload manifest mirror destination={} kind={kind}",
                        dest.destination_id
                    )
                })?;
            self.record_custody(
                dest.into(),
                kind,
                &scope_id,
                &rel_path,
                Some(hex::encode(mirror_manifest_hash.digest())),
                mirror_size,
                "create",
            )
            .await?;
            self.db().put_segment_backup_manifest_state(
                &dest.destination_id,
                &scope_id,
                tag,
                &current_hash,
                content_moved,
            )?;
            report.manifest_uploaded = true;
        }

        tracing::info!(
            owner = %hex::encode(self.owner),
            destination_id = %dest.destination_id,
            kind = kind,
            family = ?family,
            uploaded = report.uploaded_segments.len(),
            dropped = report.dropped_segments.len(),
            manifest_uploaded = report.manifest_uploaded,
            "nest segment backup: pass complete",
        );
        Ok(report)
    }

    /// One `(destination, kind)` pass, listing the source segments first:
    /// the content family, then the kind's placement journal
    /// ([`SegmentFamily::PASS_ORDER`]).
    ///
    /// The report's own fields are the content family's; the journal's ride in
    /// [`RunReport::placement`]. A journal failure fails the tuple, so it is
    /// retried next tick — but only after the content family's progress has
    /// been recorded, which is why content goes first.
    pub async fn run_once(&self, dest: &BackupDestinationRow, kind: &str) -> Result<RunReport> {
        let scope_hex = hex::encode(self.owner);
        let src_listing = self.source.list_segments(kind, &scope_hex).await?;
        let mut report = self.run_once_with_src_refs(dest, kind, src_listing).await?;
        if let Some(journal_listing) = self
            .source
            .list_family(kind, SegmentFamily::Placement, &scope_hex)
            .await?
        {
            let journal = self
                .run_family_with_src_refs(dest, kind, SegmentFamily::Placement, journal_listing)
                .await?;
            report.placement = Some(Box::new(journal));
        }
        Ok(report)
    }

    /// One `(destination, covered folder)` mirror pass — the sweep's
    /// ordinary-folder axis (`backup-destinations.md` § Ordinary-folder
    /// coverage). The folder's live head (latest-per-path) is copied **as-is**:
    /// the manifests + chunks are already this nest's at-rest bytes, sealed per
    /// the folder's audience, so nothing here re-seals and no `NestBackupKey`
    /// is involved — the byte plane is driven directly, never through the
    /// engine's seal pipeline.
    ///
    /// Same record-before-state-advance ordering as the segment pass: a path's
    /// custody lands on the destination before the local cursor moves, so a
    /// failure re-uploads (cheaply, via chunk dedup) rather than marking a path
    /// done the destination's GC never learned to keep.
    ///
    /// A covered folder that no longer exists — or changed hands — mirrors as
    /// an **empty** head: every previously pushed path tombstones (retained
    /// under the grace window T, like any writer delete), and the now-pointless
    /// coverage row is dropped so the sweep stops revisiting it.
    pub async fn run_folder_once(
        &self,
        dest: &BackupDestinationRow,
        folder_id: i64,
    ) -> Result<FolderRunReport> {
        let svc = self
            .state
            .backup_service
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("folder mirror needs a configured blob store"))?;
        let store = svc.local_blob_store();
        let at_rest_key = svc.encryption_key();

        let folder_gone = !matches!(
            self.state.db.get_folder_by_id(folder_id).await?,
            Some(f) if f.actor_id == self.owner
        );
        let live: Vec<crate::db::SyncFileInfo> = if folder_gone {
            Vec::new()
        } else {
            self.state.db.get_files_for_folder(folder_id).await?
        };

        let client = self.destination_sync_client(dest);
        let local = self
            .db()
            .list_folder_backup_state(&dest.destination_id, folder_id)?;
        let pushed: HashMap<Vec<u8>, Vec<u8>> = local.into_iter().collect();

        // The legal-takedown withhold binds this route too (`moderation.md`
        // § Legal takedown → *The blob-serve door* → *What the withhold binds
        // on owner- and admin-scoped routes*, path 3): a mirrored chunk lands
        // in the destination's own content-addressed store, where its
        // unauthenticated `/api/v1/chunks/{hash}` door — holding no flag —
        // would serve the compelled bytes to anyone with the hex the source's
        // doors already answer 451 for. So a live path naming a withheld
        // digest is neither pushed nor left standing: it is skipped, declared
        // in the report, and retracted at the destination if an earlier pass
        // mirrored it. Gate before the store read where the digest is already
        // known (the manifest hash), and before any chunk read otherwise.
        let withheld = self.state.db.list_blob_legal_withhold().await?;
        let path_withheld =
            |manifest_key: &fauna_core::data::ContentHash,
             manifest: &fauna_core::chunk::ChunkManifest| {
                withheld.contains(&manifest_key.digest())
                    || manifest
                        .store_keys()
                        .iter()
                        .any(|k| withheld.contains(&k.digest()))
            };

        // The set is empty on nearly every box, and then the loop costs what
        // it always did (`already_mirrored` alone short-circuits). While a
        // withhold DOES stand, `already_mirrored` alone is not enough — but a
        // path this exact destination+folder pair already confirmed clean
        // against this exact set's *content* needs no re-check either (row
        // 729): re-opening and re-decrypting every already-mirrored manifest,
        // every pass, for the life of a takedown that never touched them, is
        // the cost this memo removes. `withhold_unchanged` is that "already
        // confirmed against this content" test.
        let withheld_digest = withheld_set_digest(&withheld);
        let checkpoint = self
            .db()
            .get_folder_withhold_checkpoint(&dest.destination_id, folder_id)?;
        let withhold_unchanged = checkpoint.as_deref() == Some(&withheld_digest[..]);
        if !withhold_unchanged {
            // A stamp describing a different withheld set can't be trusted
            // for this pass's already-mirrored paths -- and if THIS pass
            // aborts partway, it must never survive to be trusted by a LATER
            // pass that happens to land back on the same digest: an overturn, a pass that pushes some paths then
            // aborts, the same takedown reinstated -- the stale stamp from
            // before the overturn matches again and a now-withheld path stays
            // mirrored. Delete it now, before any path is examined, so an
            // abort leaves nothing to trust; the loop below re-stamps at the
            // end if this pass runs clean.
            self.db()
                .delete_folder_withhold_checkpoint(&dest.destination_id, folder_id)?;
        }

        let mut report = FolderRunReport::default();
        for file in &live {
            let already_mirrored = pushed.get(&file.path_hash).map(Vec::as_slice)
                == Some(file.manifest_hash.as_slice());
            if already_mirrored && (withheld.is_empty() || withhold_unchanged) {
                continue;
            }
            let manifest_key = to_scope_id(&file.manifest_hash)
                .map(fauna_core::data::ContentHash::from_digest_raw)
                .context("folder head row carries a non-32-byte manifest hash")?;
            let mut is_withheld = withheld.contains(&manifest_key.digest());
            let mut opened: Option<(Vec<u8>, fauna_core::chunk::ChunkManifest)> = None;
            if !is_withheld {
                // GC pins every live head's manifest
                // (`db/sync_storage.rs::sync_change_manifest_refs`'s
                // `superseded_at IS NULL` arm, walked by `backup/gc.rs`), so a
                // live already-covered path missing its manifest here should
                // be unreachable; abort loud rather than silently skip a row
                // the GC contract says cannot happen.
                let raw = store.get(&manifest_key).await?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "source blob store is missing manifest {} for folder {folder_id}",
                        hex::encode(&file.manifest_hash)
                    )
                })?;
                report.manifests_opened += 1;
                let (manifest_bytes, manifest) = manifest_plaintext(&raw, at_rest_key)?;
                is_withheld = path_withheld(&manifest_key, &manifest);
                opened = Some((manifest_bytes, manifest));
            }
            if is_withheld {
                // Retract what an earlier pass mirrored — the takedown's
                // retract-where-expressible posture (the ATProto `deleteRecord`
                // and the Nostr kind-5 legs), here as the ordinary path delete
                // the destination already retains under its grace window. The
                // state row goes with it, so the next pass after an overturn
                // sees the path as never pushed and mirrors it again.
                if pushed.contains_key(&file.path_hash) {
                    let rest_path = hex::encode(&file.path_hash);
                    self.record_folder_custody(
                        dest.into(),
                        folder_id,
                        &rest_path,
                        None,
                        0,
                        "delete",
                        None,
                    )
                    .await?;
                    self.db().delete_folder_backup_state(
                        &dest.destination_id,
                        folder_id,
                        &file.path_hash,
                    )?;
                }
                report.withheld_paths += 1;
                tracing::warn!(
                    owner = %hex::encode(self.owner),
                    destination_id = %dest.destination_id,
                    folder_id = folder_id,
                    path_hash = %hex::encode(&file.path_hash),
                    "nest folder mirror: path withheld from the destination under a legal takedown; retried next pass",
                );
                continue;
            }
            if already_mirrored {
                continue;
            }
            let (manifest_bytes, manifest) =
                opened.expect("a path that is not withheld had its manifest opened above");

            // Chunks first, then the manifest, then custody: the destination's
            // charge derivation refuses a record naming bytes it does not hold.
            let store_keys = manifest.store_keys();
            let missing = client.check_chunks(&store_keys).await?;
            for key in &missing {
                let chunk_raw = store.get(key).await?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "source blob store is missing chunk {} for folder {folder_id}",
                        hex::encode(key.digest())
                    )
                })?;
                let body = chunk_wire_body(key, &chunk_raw, at_rest_key)?;
                client.upload_chunk(key, &body).await?;
            }
            client.upload_manifest(&manifest_bytes).await?;

            // The destination custody path is the SOURCE row's `path_hash`,
            // hex-spelled — a plaintext machine path, uniform across sealed-
            // and plaintext-path rows so supersede keying can never fork when
            // a folder's audience flips. The sealed name blob (already
            // ciphertext, blind to the destination) rides along so a restore
            // can recover names under the owner's keys.
            let rest_path = hex::encode(&file.path_hash);
            self.record_folder_custody(
                dest.into(),
                folder_id,
                &rest_path,
                Some(hex::encode(&file.manifest_hash)),
                file.size_bytes,
                "create",
                file.path_sealed.clone(),
            )
            .await?;
            self.db().put_folder_backup_state(
                &dest.destination_id,
                folder_id,
                &file.path_hash,
                &file.manifest_hash,
            )?;
            report.uploaded_paths += 1;
        }

        // Paths mirrored earlier that the live head no longer carries
        // (deleted, or the whole folder is gone): tombstone, then forget.
        let live_hashes: HashSet<&[u8]> = live.iter().map(|f| f.path_hash.as_slice()).collect();
        let dropped: Vec<Vec<u8>> = pushed
            .keys()
            .filter(|ph| !live_hashes.contains(ph.as_slice()))
            .cloned()
            .collect();
        for path_hash in dropped {
            let rest_path = hex::encode(&path_hash);
            self.record_folder_custody(dest.into(), folder_id, &rest_path, None, 0, "delete", None)
                .await?;
            self.db()
                .delete_folder_backup_state(&dest.destination_id, folder_id, &path_hash)?;
            report.dropped_paths += 1;
        }

        // Every live path reached the main loop above without erroring, and
        // every dropped path's teardown just above completed too, so the
        // digest is now trustworthy for the state this pair is actually in.
        // Stamp it here -- after BOTH loops, not before either -- so a pass
        // that aborts partway (a missing manifest, a teardown failure) never
        // claims paths it never actually finished checking this time.
        self.db().put_folder_withhold_checkpoint(
            &dest.destination_id,
            folder_id,
            &withheld_digest,
        )?;

        if folder_gone {
            // Only after the teardown above landed: dropping the coverage row
            // first would orphan the mirrored custody forever.
            let _ = self
                .state
                .db
                .detach_backup_destination_folder(&self.owner, &dest.destination_id, folder_id)
                .await;
            // The memo just stamped above describes paths that no longer
            // exist for this pair — drop it so a folder id reused later (or
            // re-attached) starts from a clean checkpoint rather than one
            // that could spuriously match.
            self.db()
                .delete_folder_withhold_checkpoint(&dest.destination_id, folder_id)?;
        }

        tracing::info!(
            owner = %hex::encode(self.owner),
            destination_id = %dest.destination_id,
            folder_id = folder_id,
            uploaded = report.uploaded_paths,
            dropped = report.dropped_paths,
            withheld = report.withheld_paths,
            "nest folder mirror: pass complete",
        );
        Ok(report)
    }

    /// Tear down the mirror of every folder this coordinator has pushed to a
    /// live destination but whose coverage row is gone (a detach): per-path
    /// tombstones over the same writer grant, then forget the local cursor —
    /// the folder-axis twin of [`Self::reconcile_removed_destinations`].
    /// All-or-nothing per folder: a failure keeps the rows and retries next
    /// pass.
    pub async fn reconcile_detached_folders(&self) -> Result<usize> {
        let covered: HashSet<(&str, i64)> = self
            .covered_folders
            .iter()
            .map(|(d, f)| (d.as_str(), *f))
            .collect();
        let mut torn_down = 0;
        for dest in &self.destinations {
            let mirrored = self
                .db()
                .list_folder_backup_folder_ids(&dest.destination_id)?;
            for folder_id in mirrored {
                if covered.contains(&(dest.destination_id.as_str(), folder_id)) {
                    continue;
                }
                let paths = self
                    .db()
                    .list_folder_backup_state(&dest.destination_id, folder_id)?;
                let mut all_down = true;
                for (path_hash, _) in paths {
                    let rest_path = hex::encode(&path_hash);
                    if let Err(e) = self
                        .record_folder_custody(
                            dest.into(),
                            folder_id,
                            &rest_path,
                            None,
                            0,
                            "delete",
                            None,
                        )
                        .await
                    {
                        tracing::warn!(
                            owner = %hex::encode(self.owner),
                            destination_id = %dest.destination_id,
                            folder_id = folder_id,
                            error = %e,
                            "nest folder mirror: detach teardown deferred; will retry"
                        );
                        all_down = false;
                        break;
                    }
                    self.db().delete_folder_backup_state(
                        &dest.destination_id,
                        folder_id,
                        &path_hash,
                    )?;
                }
                if all_down {
                    self.db()
                        .delete_folder_withhold_checkpoint(&dest.destination_id, folder_id)?;
                    torn_down += 1;
                }
            }
        }
        Ok(torn_down)
    }

    /// Tear the owner's custody down at every destination they have removed from
    /// the registry, then drop the local upload state for it.
    ///
    /// **Both halves matter, and only the second one used to exist.** Forgetting
    /// the local rows makes a destination re-registered under the same id
    /// re-upload from scratch instead of trusting state that describes custody
    /// someone has since deleted. But a departed destination that is never told
    /// keeps every chunk it holds GC-pinned *forever*: its custody record is the
    /// only thing its GC walks for a custody-copy set, and nothing else will
    /// ever mention those paths again. Before the slice-5 flip the client
    /// coordinator drove `fauna.folders.delete` here; the flip retired that arm
    /// and left the teardown with no owner at all.
    ///
    /// **Why a per-path `delete` and not a set delete.** The set delete
    /// authenticates as the *owner*, which this nest cannot do and must never be
    /// able to. The per-path tombstone rides the same nest-writer grant the
    /// uploads rode — the authority the owner already granted for exactly these
    /// paths — and is the second shape
    /// `docs/goal/behavior/backup-destinations.md` § Create / edit / remove
    /// protocol → *Remove* ratifies ("or records a `delete` for each backed-up
    /// path"). It is also the shape that keeps the custody **grace window**
    /// honest: a tombstone from a writer retains the generation for `T` before
    /// reclaiming, which is precisely the bound on a rogue source nest's delete
    /// power (`message-segment-store.md` § Custody grace window). So the chunks
    /// reclaim one grace window after removal rather than instantly — the
    /// deliberate price of the nest never holding owner authority.
    ///
    /// Idempotent and retried every pass: a destination that is unreachable, or
    /// whose grant the owner already revoked, keeps its rows and is retried,
    /// exactly the loop-reconcile crash-safety the retired client arm had
    /// (`nest/common.md` § Client-state recoverability).
    pub async fn reconcile_removed_destinations(&self) -> Result<usize> {
        let live: std::collections::HashSet<&str> = self
            .destinations
            .iter()
            .map(|d| d.destination_id.as_str())
            .collect();
        let mut forgotten = 0;
        // Bind the list before the loop: a guard taken in the `for` iterator
        // expression would still be alive in the body and deadlock on `db()`.
        let seen = self.db().list_backup_destinations_seen()?;
        for dest in seen {
            if live.contains(dest.dest_id.as_str()) {
                continue;
            }
            if let Err(e) = self.drop_remote_custody(&dest).await {
                // Keep the rows so the next pass retries. Forgetting them here
                // would strand the destination's custody permanently — the very
                // leak this function exists to close.
                tracing::warn!(
                    owner = %hex::encode(self.owner),
                    destination_id = %dest.dest_id,
                    error = %e,
                    "nest segment backup: removal teardown deferred (destination \
                     unreachable or grant revoked); will retry"
                );
                continue;
            }
            self.db().forget_backup_destination(&dest.dest_id)?;
            forgotten += 1;
            tracing::info!(
                owner = %hex::encode(self.owner),
                destination_id = %dest.dest_id,
                "nest segment backup: removed destination reconciled (custody torn down)"
            );
        }
        Ok(forgotten)
    }

    /// Record a `delete` for every path this coordinator placed at a departed
    /// destination. All-or-nothing per destination: the first failure aborts, so
    /// the caller keeps the local rows and retries the whole set next pass.
    async fn drop_remote_custody(&self, dest: &SeenBackupDestination) -> Result<()> {
        let paths = self.db().list_backup_paths_for_destination(&dest.dest_id)?;
        let target = DestinationRef {
            destination_id: &dest.dest_id,
            nest_url: &dest.dest_url,
            nest_id: &dest.nest_id,
        };

        // A state row's `kind` is the serve TAG it was filed under, which names
        // a family of a backed-up kind. The path comes from the family and the
        // custody record from the kind, because the journal's custody rests in
        // the kind's own set.
        for placed in &paths.segments {
            let scope = to_scope_id(&placed.scope_id)?;
            let scope_hex = hex::encode(scope);
            let (family, kind) = placed_family(&placed.kind)?;
            let rel_path = family.dat_path(&scope_hex, placed.segment_id);
            self.record_custody(target, kind, &scope, &rel_path, None, 0, "delete")
                .await?;
            // The sidecar's path, only where this coordinator actually put
            // one (a row from before the widening never did).
            if placed.meta_pushed {
                let meta_rel_path = family.meta_path(&scope_hex, placed.segment_id);
                self.record_custody(target, kind, &scope, &meta_rel_path, None, 0, "delete")
                    .await?;
            }
        }
        for (tag, scope_id) in &paths.manifests {
            let scope = to_scope_id(scope_id)?;
            let (family, kind) = placed_family(tag)?;
            let rel_path = family.mirror_path(&hex::encode(scope), kind);
            self.record_custody(target, kind, &scope, &rel_path, None, 0, "delete")
                .await?;
        }
        // The folder-mirror axis: every covered-folder path this coordinator
        // placed there tombstones too (the coverage rows themselves are already
        // gone — destination-remove drops them with the registry row).
        let folder_ids = self.db().list_folder_backup_folder_ids(&dest.dest_id)?;
        for folder_id in folder_ids {
            let paths = self
                .db()
                .list_folder_backup_state(&dest.dest_id, folder_id)?;
            for (path_hash, _) in paths {
                let rest_path = hex::encode(&path_hash);
                self.record_folder_custody(target, folder_id, &rest_path, None, 0, "delete", None)
                    .await?;
            }
        }
        Ok(())
    }

    /// Every `(destination, kind)` tuple this owner has. Per-tuple errors are
    /// logged and do not abort the rest — one unreachable destination must not
    /// stop the others from being backed up.
    ///
    /// Returns the pass's [`PassOutcome`], which is what makes the
    /// `backup-upload` lease progress-shaped rather than configuration-shaped
    /// (see [`BackupPassHealth`]).
    pub async fn run_all_tuples(&self) -> Result<PassOutcome> {
        if let Err(e) = self.reconcile_removed_destinations().await {
            tracing::warn!(error = %e, "nest segment backup: removal reconcile failed");
        }
        if let Err(e) = self.reconcile_detached_folders().await {
            tracing::warn!(error = %e, "nest folder mirror: detach reconcile failed");
        }
        let mut outcome = PassOutcome::default();
        for dest in &self.destinations {
            for &kind in BACKED_UP_KINDS {
                outcome.attempted += 1;
                if let Err(e) = self.run_once(dest, kind).await {
                    outcome.failed += 1;
                    tracing::warn!(
                        owner = %hex::encode(self.owner),
                        destination_id = %dest.destination_id,
                        kind = kind,
                        error = %format!("{e:#}"),
                        "nest segment backup: pass failed; retrying next tick"
                    );
                }
            }
            // The ordinary-folder mirror axis — `destination × covered folders`
            // (`backup-destinations.md` § Ordinary-folder coverage).
            for (dest_id, folder_id) in &self.covered_folders {
                if dest_id != &dest.destination_id {
                    continue;
                }
                outcome.attempted += 1;
                if let Err(e) = self.run_folder_once(dest, *folder_id).await {
                    outcome.failed += 1;
                    tracing::warn!(
                        owner = %hex::encode(self.owner),
                        destination_id = %dest.destination_id,
                        folder_id = folder_id,
                        error = %format!("{e:#}"),
                        "nest folder mirror: pass failed; retrying next tick"
                    );
                }
            }
        }
        Ok(outcome)
    }

    /// Per-destination status rows for the Backups page
    /// (`docs/goal/behavior/backup-destinations.md` § State & data shape → status read), computed
    /// from the same local state the pass advances: `last_upload_time` from the
    /// manifest-mirror rows, `backlog_count` from re-diffing the live source
    /// list. Correct before the first pass too — a freshly enrolled destination
    /// reports `None` and its full backlog, which is exactly true.
    pub fn status_with_src_lists(
        &self,
        per_kind_refs: &HashMap<&'static str, Vec<SegmentRef>>,
    ) -> Result<Vec<BackupDestinationStatusItem>> {
        let mut out = Vec::with_capacity(self.destinations.len());
        for dest in &self.destinations {
            let last_upload_time = self
                .db()
                .max_manifest_synced_at_for_dest(&dest.destination_id)?;
            let mut backlog_count: u32 = 0;
            for &kind in BACKED_UP_KINDS {
                let local =
                    self.db()
                        .list_segment_backup_state(&dest.destination_id, &self.owner, kind)?;
                if let Some(src_refs) = per_kind_refs.get(kind) {
                    backlog_count = backlog_count
                        .saturating_add(diff_segments(src_refs, &local).to_upload.len() as u32);
                }
            }
            out.push(BackupDestinationStatusItem {
                destination_id: dest.destination_id.clone(),
                last_upload_time,
                backlog_count,
                // Nest destinations have no capacity cap and no held-bytes
                // figure; leaving both `None` is what lets a render tell "not
                // applicable" from "zero". The custodian arm that fills them
                // is tracked.
                ..Default::default()
            });
        }
        Ok(out)
    }

    /// [`Self::status_with_src_lists`] with the source lists read first.
    pub async fn status(&self) -> Result<Vec<BackupDestinationStatusItem>> {
        let scope_hex = hex::encode(self.owner);
        let mut per_kind: HashMap<&'static str, Vec<SegmentRef>> = HashMap::new();
        for &kind in BACKED_UP_KINDS {
            per_kind.insert(
                kind,
                self.source.list_segments(kind, &scope_hex).await?.segments,
            );
        }
        self.status_with_src_lists(&per_kind)
    }
}

/// Which family of which backed-up kind an upload-state row was filed under.
///
/// A row whose tag nothing derives is a row this binary did not write and
/// cannot address: refusing is what keeps the teardown all-or-nothing, rather
/// than tombstoning a path guessed from a tag it does not understand.
fn placed_family(tag: &str) -> Result<(SegmentFamily, &'static str)> {
    SegmentFamily::from_serve_kind(tag)
        .ok_or_else(|| anyhow::anyhow!("upload state filed under an unknown tag {tag:?}"))
}

/// This nest's own segment **head** for `owner`: the greatest segment id it
/// holds, plus one, across every backed-up kind.
///
/// This is the figure a client-device custodian's `high_water` is measured
/// against — `backlog_count = head − high_water` (`behavior/backup-destinations.md` § Third
/// destination kind → *Status projection inverts*). It is deliberately computed
/// the same way the custodian computes its own side
/// (`fauna_sync_engine::custodian_pull::CustodianPull::high_water` — greatest id
/// held, plus one), because two differently-derived numbers subtracted from one
/// another is how an off-by-one becomes a permanently-lagging status row.
///
/// A free function rather than a coordinator method on purpose: a custodian-only
/// owner has **no** coordinator (they grant no `NestBackupKey`), and their status
/// row still needs this.
pub async fn owner_segment_head(state: &Arc<AppState>, owner: [u8; 32]) -> Result<u64> {
    let source = NestLocalSegmentSource::new(
        Arc::clone(state),
        hex::encode(state.nest_identity.public_key_bytes()),
    );
    let scope_hex = hex::encode(owner);
    let mut head = 0u64;
    for &kind in BACKED_UP_KINDS {
        let refs = source.list_segments(kind, &scope_hex).await?.segments;
        if let Some(max_id) = refs.iter().map(|r| r.segment_id).max() {
            head = head.max(max_id as u64 + 1);
        }
    }
    Ok(head)
}

/// The always-on hosting loop: every tick, back up every enrolled owner to every
/// destination they registered.
///
/// This is what makes "backup keeps running with every app asleep" literally
/// true (`backup-restore.md` § Background Tasks). It is a plain interval loop, in
/// the shape of [`crate::segments::CompactionWorker`] and the membership-lapse
/// sweeper: no lease, no leader election — a source nest is the single writer for
/// its own owners by construction.
pub struct NestBackupWorker {
    state: Arc<AppState>,
    interval: std::time::Duration,
}

impl NestBackupWorker {
    /// `interval` is the shared
    /// [`fauna_sync_engine::segment_backup::PERIODIC_INTERVAL`] in production —
    /// taken as a parameter so a test can drive the loop without waiting.
    pub fn new(state: Arc<AppState>, interval: std::time::Duration) -> Self {
        Self { state, interval }
    }

    /// One sweep over every enrolled owner. Returns how many owners were run.
    /// A single owner's failure is logged and skipped — never fatal to the sweep.
    pub async fn run_once(&self) -> Result<usize> {
        let owners = self
            .state
            .db
            .list_nest_backup_key_owners()
            .await
            .context("list owners with a granted NestBackupKey")?;
        let mut ran = 0;
        // Owners this sweep actually swept — what the health map is pruned to,
        // so a revoked or destination-less owner cannot leave a stale counter
        // behind (and, on re-enrolment, start life already stalled).
        let mut swept: HashSet<[u8; 32]> = HashSet::new();
        for owner_bytes in owners {
            let Ok(owner) = <[u8; 32]>::try_from(owner_bytes.as_slice()) else {
                tracing::warn!(
                    owner = %hex::encode(&owner_bytes),
                    "nest segment backup: skipping malformed owner id in the grant store"
                );
                continue;
            };
            match NestBackupCoordinator::open_for_owner(Arc::clone(&self.state), owner).await {
                Ok(Some(coordinator)) => {
                    if coordinator.destinations().is_empty() {
                        continue;
                    }
                    swept.insert(owner);
                    // The lease's progress half: a wholly-failed pass counts
                    // toward the stall threshold, any success clears it
                    // ([`BackupPassHealth`]). Recorded here rather than inside
                    // the coordinator: pass health feeds the nest lease, which
                    // is this worker's concern, not the coordinator's.
                    if let Ok(outcome) = coordinator.run_all_tuples().await {
                        self.state.backup_pass_health.record_pass(owner, outcome);
                    }
                    ran += 1;
                }
                // Raced with a revoke between the list and the read.
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    owner = %hex::encode(owner),
                    error = %format!("{e:#}"),
                    "nest segment backup: could not open owner state; retrying next tick"
                ),
            }
        }
        self.state.backup_pass_health.retain_owners(&swept);
        Ok(ran)
    }

    /// Spawn the loop. The first tick fires immediately, so a nest that restarted
    /// mid-backlog resumes at boot rather than after a full interval.
    ///
    /// Each sweep runs on the **blocking pool**, driven by this runtime's handle.
    /// Two independent reasons, either sufficient:
    ///
    /// * A pass is genuinely blocking-shaped — SQLite state reads, segment file
    ///   reads, and the CDC-chunk → BLAKE3 → ChaCha20 seal of every uploaded
    ///   segment. That is CPU and disk work, which does not belong on an async
    ///   worker thread serving live client connections.
    /// * A pass is `!Send` by construction: [`SyncEngine`] and [`SyncDb`] hold a
    ///   `rusqlite::Connection`, which is `Send` but not `Sync`, so a future
    ///   borrowing one cannot cross a `tokio::spawn` boundary. (The retired
    ///   client arm's desktop drivers met the same constraint with
    ///   `spawn_local`.)
    ///
    /// `Handle::block_on` — rather than a private current-thread runtime on a
    /// dedicated thread — is deliberate: the federation channels the pass
    /// originates on are pooled and shared with the rest of the nest, and a
    /// socket registered with one runtime's IO driver cannot be polled from
    /// another's.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        // spawn-ok(returns-handle-for-scope): the caller adopts this handle via `AppState::scope_handle`
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.interval);
            loop {
                ticker.tick().await;
                let state = Arc::clone(&self.state);
                let sweep = tokio::task::spawn_blocking(move || {
                    let worker = NestBackupWorker::new(state, std::time::Duration::MAX);
                    tokio::runtime::Handle::current().block_on(worker.run_once())
                })
                .await;
                match sweep {
                    Ok(Ok(n)) if n > 0 => {
                        tracing::debug!("nest segment backup: swept {n} enrolled owner(s)")
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(error = %format!("{e:#}"), "nest segment backup sweep failed")
                    }
                    Err(e) => tracing::error!(error = %e, "nest segment backup sweep panicked"),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Residue: this arm re-exports `fauna_sync_engine`'s
    /// `BACKED_UP_KINDS` rather than declaring its own `["mail"]`, so a second
    /// kind list is unrepresentable by construction (the client coordinator
    /// this once had to stay in lockstep with is deleted; the custodian pull
    /// reads the same constant).
    ///
    /// The one site that cannot share the list is `NestLocalSegmentSource`
    /// (its scope resolution is actor-shaped), so it carries a compile-time
    /// pin instead. This records what that pin protects; the pin itself is
    /// proven by the build failing when a kind it cannot resolve is added.
    #[test]
    fn the_kind_list_is_still_the_one_the_nest_local_source_serves() {
        assert_eq!(
            BACKED_UP_KINDS,
            &["mail", "post", "calendar", "card"],
            "adding a kind needs a per-kind arm in segments/backup_source.rs first — \
             its `const _: () = assert!(…)` fails the build until it has one"
        );
    }

    // ── The pass-health half of backup-upload sufficiency ──

    #[test]
    fn a_pass_with_nothing_to_do_is_not_a_failure() {
        assert!(!PassOutcome::default().wholly_failed());
        assert!(
            !PassOutcome {
                attempted: 0,
                failed: 0,
            }
            .wholly_failed(),
            "no tuples means nothing got it wrong — sufficiency already requires a destination"
        );
    }

    #[test]
    fn wholly_failed_means_every_tuple_errored() {
        assert!(
            PassOutcome {
                attempted: 3,
                failed: 3
            }
            .wholly_failed()
        );
        assert!(
            !PassOutcome {
                attempted: 3,
                failed: 2
            }
            .wholly_failed(),
            "one destination working is progress; a client takeover would not fix the other"
        );
    }

    #[test]
    fn a_single_failed_pass_never_hands_the_kind_away() {
        // The property itself is a compile-time pin beside the constant (tuning
        // it to 1 fails the build). What a runtime test can add is the
        // *observable*: below the threshold the owner is still swept-and-held.
        let health = BackupPassHealth::default();
        let owner = [11u8; 32];
        health.record_pass(
            owner,
            PassOutcome {
                attempted: 1,
                failed: 1,
            },
        );
        assert!(
            !health.is_stalled(&owner),
            "one transient failure must not hand the kind away — this nest cannot \
             preempt it back once a client claims the released lease"
        );
    }

    #[test]
    fn the_stall_needs_a_sustained_run_of_failures() {
        let health = BackupPassHealth::default();
        let owner = [7u8; 32];
        let bad = PassOutcome {
            attempted: 1,
            failed: 1,
        };

        for _ in 0..MAX_CONSECUTIVE_FAILED_PASSES - 1 {
            health.record_pass(owner, bad);
            assert!(
                !health.is_stalled(&owner),
                "below the threshold the nest keeps the kind — it cannot preempt itself back \
                 once a client takes over, so a single blip must not hand it away"
            );
        }
        health.record_pass(owner, bad);
        assert!(health.is_stalled(&owner));
    }

    #[test]
    fn any_success_clears_the_stall() {
        let health = BackupPassHealth::default();
        let owner = [8u8; 32];
        for _ in 0..MAX_CONSECUTIVE_FAILED_PASSES + 2 {
            health.record_pass(
                owner,
                PassOutcome {
                    attempted: 2,
                    failed: 2,
                },
            );
        }
        assert!(health.is_stalled(&owner));

        health.record_pass(
            owner,
            PassOutcome {
                attempted: 2,
                failed: 1,
            },
        );
        assert!(
            !health.is_stalled(&owner),
            "the counter resets rather than decays — a recovered nest re-claims on the next pass"
        );
    }

    #[test]
    fn an_owner_the_sweep_dropped_does_not_keep_a_stale_counter() {
        // Otherwise a re-enrolled owner would start life already stalled, with
        // a counter describing a grant that no longer exists.
        let health = BackupPassHealth::default();
        let gone = [9u8; 32];
        let live = [10u8; 32];
        let bad = PassOutcome {
            attempted: 1,
            failed: 1,
        };
        for _ in 0..MAX_CONSECUTIVE_FAILED_PASSES {
            health.record_pass(gone, bad);
            health.record_pass(live, bad);
        }

        health.retain_owners(&HashSet::from([live]));

        assert!(
            !health.is_stalled(&gone),
            "a dropped owner's counter is forgotten"
        );
        assert!(health.is_stalled(&live), "a swept owner's counter survives");
    }
}
