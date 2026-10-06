//! The **client-device custodian's local sealed store** — the "local blob-store
//! sink" the pull plane writes into, and the thing a standalone restore opens.
//!
//! Owner: `docs/goal/behavior/backup-destinations.md` § State & data shape → *Third destination
//! kind — client device as custodian*; pull-plane mechanics in
//! `docs/goal/architecture/message-segment-store.md` § Client-device custodian
//! (pull). The custody *policy* over what this store holds —
//! liveness, the grace window, cap enforcement — is
//! [`fauna_client_backup::custodian`], deliberately in a wasm-clean crate; this
//! module is the bytes it plans over.
//!
//! # Layout
//!
//! Under a root the caller supplies (`behavior/backup-destinations.md` § Third destination kind
//! makes *where the store lives* the only per-platform question — desktop under
//! the sync agent's data dir, mobile under app storage):
//!
//! ```text
//! <root>/blobs/<aa>/<hex64>       sealed chunk bodies, keyed by STORE key
//! <root>/manifests/<aa>/<hex64>   canonical dag-cbor ChunkManifest bytes
//! <root>/index.json               the held-generation index, plus the
//!                                 covered folders' display names by set
//! <root>/staging/pair-*/          a segment pair in transit, and its sealed
//!                                 chunk bodies before the put ([`StagedSeal`])
//! ```
//!
//! Blobs and manifests are content-addressed with the same store keys a nest
//! destination uses, because they are the *same bytes* — [`crate::seal`] is the
//! one write-side pipeline for both sinks. That is what buys the ratified
//! property that a custodian's artifacts are byte-identical to a nest
//! destination's, and it is why a standalone restore is just
//! [`fauna_core::file_download`]'s ordinary walk pointed at a different
//! [`BlobFetcher`] — no per-custodian read arm exists, or can drift.
//!
//! # Row paths are set-qualified
//!
//! Every [`HeldRow::path`] is `{set_name}/{path_within_set}` — the destination's
//! own `(set, path)` pair joined by `/` (`segment-backup-protocol.md` §
//! Client-device custodian (pull) → *Restore* → *The store is set-qualified*):
//! `__mail/{scope_hex}/seg-00000007.dat`, `__post/{scope_hex}/manifest.post`,
//! `__folder/<nest-hex>/<id>/<path_hash_hex>`. [`held_path`] is the one
//! formatter and [`path_in_set`] the one inverse; every mover derives and parses
//! a row path through them, and the within-set half is what rides the wire.
//! Several kinds share one store, and every kind's content family is
//! `{scope_hex}/seg-NNNNNNNN.*`, so without the set a second kind's pull would
//! overwrite the first's rows.
//!
//! The two-level `<aa>` fan-out is not decoration: a phone holding a full mail
//! corpus reaches tens of thousands of chunks, and a single flat directory that
//! size is pathological on several of the filesystems this ships onto.
//!
//! # Three rules this module makes structural
//!
//! **1. A corrupt index is an error, never an empty store.** A *missing* index is
//! ordinary first-run. A *present but unparseable* one is not, and the difference
//! is the whole ballgame: treating it as empty would orphan every blob, and the
//! very next [`CustodianStore::reclaim`] would delete the owner's entire
//! offline corpus — the one copy that survives when no nest does
//! (`behavior/backup-destinations.md` § Third destination kind → *Standalone restore*). So it
//! fails loudly and the pass stops.
//!
//! **2. The index is written last.** A crash mid-[`CustodianStore::put`] must
//! leave orphan bytes, never an index row pointing at bytes that are not there:
//! orphans are reclaimed by the next GC and re-fetched by the next pull (content
//! addressing re-converges), whereas a dangling row is a generation the policy
//! believes it holds and a restore cannot open. Same reasoning, same direction,
//! as the upload path recording custody before marking a segment synced.
//!
//! Re-converging is a **mechanism, not a hope**: bytes that went missing under
//! a row the index still calls held are found by [`CustodianStore::self_audit`]
//! alone — the pull's diff reads the index, which goes on believing them — so
//! the verdict carries its failing paths ([`AuditRecord::failed_paths`]),
//! `custodian_pull` re-enters them into the next pass and
//! [`CustodianStore::put_repair`] rewrites their bytes. Without that the
//! ratified "partial OS eviction is tolerated by construction"
//! (`../architecture/segment-backup-protocol.md` § Client-device custodian
//! (pull)) is an alarm with no remedy behind it.
//!
//! **3. One writer at a time, per store ROOT.** Every writing door is a
//! read-modify-write of one `index.json`, and [`CustodianStore::reclaim`]
//! sweeps by *reference set* — so a put still in flight is indistinguishable
//! from the interrupted put whose orphans rule 2 hands the sweep. Two
//! overlapping passes (on mobile, the scheduled one and the foreground push
//! loop) could therefore lose each other's row, or sweep each other's bytes.
//! The lock is keyed by root rather than owned per handle, because handles on
//! one root are routinely several ([`writer_lock_for`]); it covers no read, so
//! a restore never queues behind a pull pass.
//!
//! # Where the root is, and the cloud-backup exclusion it must carry
//!
//! [`custodian_store_root`] owns the *layout* half of "where the store lives":
//! the actor scoping and the directory name, so a shell supplies only its own
//! per-user base and cannot forget to scope it. Two accounts on one box hold
//! two independent corpora; a shell that dropped the scope would let one
//! owner's reclaim pass delete the other's offline copy.
//!
//! [`CustodianStore::ensure_root`] owns the *obligation* half. `behavior/backup-destinations.md`
//! § Third destination kind requires the store to be **excluded from platform
//! cloud backups** (iCloud / Google device backup) — replicating a sealed store
//! whose opening seed sits in the same vendor's cloud keychain is key-hygiene
//! smear plus double-charged storage. The *doing* is still per-platform (only
//! apple can call `URL.setResourceValue`, only android's manifest can declare
//! it), but the *declaring* is structural: the root cannot be created without
//! passing a [`CloudBackupExclusion`], every arm of which states its reason, and
//! the imperative arm actually runs and its failure aborts. A silently
//! un-excluded corpus is invisible on the device, which is exactly the class of
//! obligation that must not rest on four glue authors each remembering it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fauna_client_backup::custodian::HeldGeneration;
use fauna_client_backup::reseed::FolderLabel;
use fauna_core::chunk::ChunkManifest;
use fauna_core::data::ContentHash;
use fauna_core::file_download::{BlobFetcher, FileDownloadKeys};
use serde::{Deserialize, Serialize};

use std::sync::{Arc, LazyLock};

use crate::atomic_write::atomic_write_file;
use crate::seal::SealedBlob;

/// One generation of one path, as the store records it.
///
/// A richer twin of [`HeldGeneration`]: the policy's input carries only what the
/// policy may reason about (liveness is *derived*, so there is no field to get
/// wrong), while the store additionally needs `source_size_bytes` to drive
/// [`crate::segment_backup::diff_segments`] — the diff compares against the
/// source's plaintext size, which is not recoverable from the sealed size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldRow {
    /// The **set-qualified** plaintext path ([`held_path`]): the custody set's
    /// name, `/`, then the path within it — `"__mail/{scope_hex}/seg-00000007.dat"`,
    /// its sidecar sibling `"__mail/{scope_hex}/seg-00000007.meta"` (since
    /// 2026-08-29: the half a segment cannot be reopened without),
    /// `"__mail/{scope_hex}/manifest.mail"`, or a covered folder's
    /// `"__folder/<nest-hex>/<id>/<path_hash_hex>"`. Liveness is per path; the
    /// pull reads a segment's completeness off the live `.meta` sibling
    /// (`crate::segment_backup::parse_segment_rel_path`, over [`path_in_set`]).
    pub path: String,
    /// Hex-encoded manifest hash. Empty for a tombstone, which has no manifest.
    pub manifest_hash: String,
    /// Bytes this generation occupies locally (sealed chunks + manifest).
    pub size_bytes: u64,
    /// The source segment's plaintext size at the time it was pulled — what
    /// `diff_segments` compares the next `fauna.segments.list` reply against.
    #[serde(default)]
    pub source_size_bytes: u64,
    /// The source segment's record count at the time it was pulled.
    ///
    /// Recorded even though [`crate::segment_backup::diff_segments`] reads only
    /// the byte size today, because the custodian's local state is reconstituted
    /// into a [`crate::db::MailBackupSegmentState`] to drive that shared diff —
    /// and every field of a struct handed to shared logic should be a fact this
    /// store actually observed, not a zero standing in for one.
    #[serde(default)]
    pub source_record_count: u64,
    /// Unix seconds at which this device sealed and stored this generation.
    pub stored_at: i64,
    /// The source reported this path deleted. Holds no bytes; ends the path's
    /// liveness while letting what it covers age out on the normal clock.
    #[serde(default)]
    pub deleted: bool,
    /// Hex-encoded **sealed name** of the source path, for covered-folder mirror
    /// rows only (`message-segment-store.md` § Client-device custodian →
    /// *Restore*). A re-seed re-homes a mirror row into a live folder from
    /// `(path_hash, path_sealed, manifest_hash)`, and the nest refuses a live
    /// row minted without a sealed name; `path_hash` is one-way, so if the
    /// mirror does not carry this the name is simply gone.
    ///
    /// `None` on every segment-axis row (their paths are machine-authored
    /// routing keys, the class the seal requirement exempts) and on a
    /// folder row whose source served no seal.
    ///
    /// `#[serde(default)]`: `None` is a live reading (the two cases above).
    #[serde(default)]
    pub path_sealed: Option<String>,
}

impl HeldRow {
    /// Project onto the policy's input type.
    pub fn to_policy(&self) -> HeldGeneration {
        HeldGeneration {
            path: self.path.clone(),
            manifest_hash: self.manifest_hash.clone(),
            size_bytes: self.size_bytes,
            stored_at: self.stored_at,
            deleted: self.deleted,
        }
    }

    /// This generation's identity for reclaim matching.
    ///
    /// `(path, manifest_hash, stored_at)` rather than a position, so a plan
    /// computed against one read of the index can never delete a *different*
    /// generation if the index moved underneath it — and `stored_at` is what
    /// separates two tombstones at the same path (delete → recreate → delete),
    /// which share the empty manifest hash.
    ///
    /// **Unique within an index**, which is what makes it an identity at all:
    /// A → B → A inside one pass shares one `stored_at`, and two rows with one
    /// identity are rows `reclaim` cannot tell apart — expiring the superseded
    /// A would take the live A with it and leave B the path's latest. Every
    /// writer records through [`push_generation`].
    fn identity(&self) -> (&str, &str, i64) {
        (&self.path, &self.manifest_hash, self.stored_at)
    }
}

/// The store's row path for `path_in_set` within custody set `set` —
/// `{set}/{path_in_set}`, the destination's own `(set, path)` pair joined by
/// `/`. The one formatter every mover goes through (module doc, *Row paths are
/// set-qualified*); [`path_in_set`] is its inverse.
pub fn held_path(set: &str, path_in_set: &str) -> String {
    format!("{set}/{path_in_set}")
}

/// The path within `set` that `held` names, or `None` when `held` is a row of
/// another set. The inverse of [`held_path`]: what a mover parses with its
/// family grammar, and what the re-seed leg puts on the wire.
pub fn path_in_set<'a>(set: &str, held: &'a str) -> Option<&'a str> {
    held.strip_prefix(set)?.strip_prefix('/')
}

/// Append `row` as the path's newest generation, first dropping any row that
/// already carries its identity.
///
/// The earlier twin is identical in everything the policy and a restore read,
/// so dropping it loses nothing; keeping it is what let one reclaim doom both.
/// The new row goes LAST, so [`CustodianStore::live_at`]'s newest-push tie
/// rule makes it the live one — suppressing the append instead would leave
/// the intervening generation live.
fn push_generation(rows: &mut Vec<HeldRow>, row: HeldRow) {
    rows.retain(|r| r.identity() != row.identity());
    rows.push(row);
}

/// What the source said about a segment at the moment it was pulled.
///
/// Grouped rather than passed as two loose `u64`s because they are two halves of
/// one observation, and a call site that swapped them would compile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceFacts {
    /// The source segment's plaintext byte size.
    pub size_bytes: u64,
    /// The source segment's record count.
    pub record_count: u64,
}

impl SourceFacts {
    /// The facts a `fauna.segments.list` entry carries.
    pub fn of(seg: &fauna_protocol::segments::SegmentRef) -> Self {
        Self {
            size_bytes: seg.size_bytes,
            record_count: seg.record_count as u64,
        }
    }

    /// The facts a covered folder's head entry carries. The sealed name is
    /// *not* here — it rides [`CustodianStore::put_folder_row`]'s own parameter,
    /// so this type stays `Copy` and the segment axis keeps one shape.
    pub fn of_folder_head(entry: &crate::segment_backup::FolderHeadEntry) -> Self {
        Self {
            size_bytes: entry.size_bytes.max(0) as u64,
            record_count: 0,
        }
    }
}

/// The index format this build writes (`version-compatibility.md` § Dimension
/// 1, the two-number scheme). `2` = set-qualified row paths; an index with no
/// stamp reads as [`BASELINE_INDEX_VERSION`].
pub const CURRENT_INDEX_VERSION: u16 = 2;

/// The oldest build that may safely rewrite an index this build writes — the
/// reader floor a newer build raises when it writes a shape an older one would
/// damage by rewriting ([`CustodianStore::held`] refuses an index whose floor
/// is past this build). No shipped build predates version 2, so nothing reads
/// below it today; the floor stays at the scheme's baseline until a future
/// format change genuinely needs to raise it.
pub const MIN_READER_INDEX_VERSION: u16 = 1;

/// What an index with no version fields — every one written before the
/// scheme — reads as.
pub const BASELINE_INDEX_VERSION: u16 = 1;

const _: () = assert!(MIN_READER_INDEX_VERSION <= CURRENT_INDEX_VERSION);

fn baseline_index_version() -> u16 {
    BASELINE_INDEX_VERSION
}

/// The version pair alone, peeked out of an index this build could not decode
/// whole — the difference between "written by a newer build: intact, update
/// the app" and "corrupt" (the account index's `AccountIndexStamp`, same rule).
#[derive(Debug, Deserialize)]
struct StoreIndexStamp {
    #[serde(default = "baseline_index_version")]
    schema_version: u16,
    #[serde(default = "baseline_index_version")]
    min_reader_version: u16,
}

/// The on-disk index envelope. A struct rather than a bare `Vec` so the format
/// can grow additively (`#[serde(default)]` fields) without a migration.
///
/// Carries the two-number version scheme every user-irrecoverable client store
/// carries (`version-compatibility.md` § Dimension 1) and the account index's
/// unknown-key preservation ([`Self::extra`]): an older build rewriting an index
/// a newer one wrote keeps what it did not understand, and one the newer build
/// marked unreadable for it is refused, never rewritten.
#[derive(Debug, Serialize, Deserialize)]
struct StoreIndex {
    /// The shape this index was written in. Absent reads as
    /// [`BASELINE_INDEX_VERSION`].
    #[serde(default = "baseline_index_version")]
    schema_version: u16,
    /// The oldest build that may safely rewrite this index. Absent reads as
    /// [`BASELINE_INDEX_VERSION`].
    #[serde(default = "baseline_index_version")]
    min_reader_version: u16,
    #[serde(default)]
    generations: Vec<HeldRow>,
    /// Covered-folder set name → the folder's display name on the source, as
    /// the owner's own coverage listing last reported it
    /// (`fauna_protocol::backup::CoveredFolder::name`; owner
    /// `segment-backup-protocol.md` § Client-device custodian (pull) →
    /// *Restore* → *Where a restored folder's name comes from*). Recorded by
    /// the pull beside the mirror, read by the re-seed leg: after a box loss
    /// this is the only copy of the label anywhere, since destination custody
    /// deliberately never carries it. Per **set**, not per generation, because
    /// the name is the folder's, and per-row copies could disagree. Additive
    /// (2026-09-29): an index written before the field decodes as no names,
    /// and a re-seed then reports each folder set unnamed rather than guessing.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    folder_names: BTreeMap<String, String>,
    /// Covered-folder set name → the folder's address and sealed label
    /// (`CoveredFolder::{name_hash, name_sealed}`), hex, recorded by the pull
    /// beside [`Self::folder_names`] and reclaimed with it. The pair outlives
    /// the plaintext: once the source's row holds no plaintext name
    /// (`path-sealing.md` § the set-name plane) it is all the listing carries,
    /// and a re-seed names its target by the hash. Additive (2026-10-02).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    folder_labels: BTreeMap<String, StoredFolderLabel>,
    /// Keys a newer build wrote that this one has no field for — re-emitted
    /// verbatim on every rewrite. Empty in the steady state.
    #[serde(flatten, default)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl Default for StoreIndex {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_INDEX_VERSION,
            min_reader_version: MIN_READER_INDEX_VERSION,
            generations: Vec::new(),
            folder_names: BTreeMap::new(),
            folder_labels: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// One covered folder's address and sealed label as the index holds them
/// ([`StoreIndex::folder_labels`]): hex, like every other byte field here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredFolderLabel {
    name_hash: String,
    name_sealed: String,
    #[serde(flatten, default)]
    extra: BTreeMap<String, serde_json::Value>,
}

/// The custodian's local sealed store at a caller-resolved root.
#[derive(Debug)]
pub struct CustodianStore {
    root: PathBuf,
    /// This **root's** one writer — see [`writer_lock_for`]. Held across every
    /// door that mutates the index or sweeps the byte planes, never across a
    /// read: a restore must not queue behind a pull pass.
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

/// Every store root's writer lock, so two handles on one root serialize.
///
/// Process-global and keyed by **root** rather than a field each handle owns a
/// private copy of, because handles on one root are routinely several in one
/// process, started from places that never meet: the custodian host's pull
/// store, the "reclaim this device's copy" affordance's own
/// [`CustodianStore::at`], the sync agent's pipe server. A per-handle lock
/// would serialize nothing that actually races — and what races is concrete:
/// every writing door is a read-modify-write of one `index.json`, and
/// [`CustodianStore::reclaim`]'s sweep deletes by *reference set*, so a put in
/// flight is indistinguishable from the interrupted put whose orphans the
/// sweep exists to collect. (`libs/fauna-ffi/src/custodian_host.rs`'s
/// `ActivityGuard` is the other half of the same problem and covers only the
/// **manual** reclaim; the scheduled pass and the foreground push loop are
/// exactly the pair it does not separate.)
///
/// Keyed on the path as given rather than a canonicalized one: every root comes
/// from the single derivation [`custodian_store_root`], so two handles on one
/// store spell it identically, and canonicalizing would demand an `async`
/// constructor for a case that cannot arise. Never pruned — a process holds one
/// or two roots for its life, and an entry is one `Arc`.
static STORE_WRITERS: LazyLock<std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

/// The one writer lock for `root`, minted on first use.
///
/// Poisoning is ignored for the same reason the activity record ignores it: the
/// map is plain data, and a panic while holding the registry mutex cannot leave
/// a half-built entry behind.
fn writer_lock_for(root: &Path) -> Arc<tokio::sync::Mutex<()>> {
    STORE_WRITERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(root.to_path_buf())
        .or_default()
        .clone()
}

/// Whether a byte write may trust a file already sitting at a content address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rewrite {
    /// Ordinary store: a file at this key holds these exact bytes, so writing
    /// it again is pure cost. A *corrupt* one is caught at read time by the
    /// walk's whole-file address verify, which is where that check belongs.
    OnlyMissing,
    /// **Repair** — something has established that this path cannot be produced
    /// from local bytes, which is precisely the falsification of the assumption
    /// above. Eviction leaves a file absent and rot leaves it present-but-wrong;
    /// only an unconditional rewrite heals both, and content addressing makes
    /// it safe.
    Always,
}

/// A generation sealed **from a file**, one chunk at a time, its bodies staged
/// on disk rather than held — the form the custodian pull stores a segment
/// half in, so its memory is a constant chunk, never the segment
/// (`docs/goal/architecture/message-segment-store.md` § Segment size).
///
/// Byte-identical to [`crate::seal::seal_blob`] over the same bytes — both are
/// [`fauna_core::blob_seal::seal_reader`] — so a staged put converges on
/// exactly the artifacts a whole-blob put would, and on a nest destination's.
/// The manifest ([`ChunkManifest`], O(chunks)) is all it holds in memory.
pub struct StagedSeal {
    sealed: fauna_core::blob_seal::SealedManifest,
    /// One file per distinct store key, named by its hex.
    bodies: PathBuf,
}

impl StagedSeal {
    /// Seal the file at `plain`, staging each chunk body under `bodies` (a
    /// directory this creates; its owner removes it). `seal_root` is
    /// [`crate::seal::seal_blob`]'s.
    pub fn seal_file(
        plain: &Path,
        bodies: &Path,
        seal_root: Option<([u8; 32], Option<u64>)>,
    ) -> Result<Self> {
        std::fs::create_dir_all(bodies)
            .with_context(|| format!("create seal staging {}", bodies.display()))?;
        let file = std::fs::File::open(plain)
            .with_context(|| format!("open {} to seal", plain.display()))?;
        let sealed = fauna_core::blob_seal::seal_reader(
            std::io::BufReader::new(file),
            seal_root,
            |key, body| {
                let target = bodies.join(hex::encode(key.digest()));
                // A repeated chunk is one body: its key is its content.
                if !target.exists() {
                    std::fs::write(&target, &body)
                        .with_context(|| format!("stage sealed chunk {}", target.display()))?;
                }
                Ok(())
            },
        )?;
        Ok(Self {
            sealed,
            bodies: bodies.to_path_buf(),
        })
    }

    /// `blake3(manifest_bytes)` — as [`SealedBlob::manifest_hash`].
    pub fn manifest_hash(&self) -> &ContentHash {
        &self.sealed.manifest_hash
    }

    /// As [`SealedBlob::stored_bytes`].
    pub fn stored_bytes(&self) -> u64 {
        self.sealed.stored_bytes
    }
}

/// A sealed generation as the store's writing doors take it: whole in memory
/// ([`SealedBlob`]) or staged chunk by chunk on disk ([`StagedSeal`]). One set
/// of doors over both, so the convergence, charge and repair rules cannot
/// drift between the two shapes.
#[derive(Clone, Copy)]
pub enum Sealed<'a> {
    /// Every chunk body in memory — a mirror, a folder row, a sidecar backfill.
    Whole(&'a SealedBlob),
    /// Chunk bodies on disk — a segment half ([`StagedSeal`]).
    Staged(&'a StagedSeal),
}

impl<'a> From<&'a SealedBlob> for Sealed<'a> {
    fn from(sealed: &'a SealedBlob) -> Self {
        Self::Whole(sealed)
    }
}

impl<'a> From<&'a StagedSeal> for Sealed<'a> {
    fn from(sealed: &'a StagedSeal) -> Self {
        Self::Staged(sealed)
    }
}

impl Sealed<'_> {
    fn manifest_hex(&self) -> String {
        let hash = match self {
            Self::Whole(s) => &s.manifest_hash,
            Self::Staged(s) => &s.sealed.manifest_hash,
        };
        hex::encode(hash.digest())
    }

    fn manifest_bytes(&self) -> &[u8] {
        match self {
            Self::Whole(s) => &s.manifest_bytes,
            Self::Staged(s) => &s.sealed.manifest_bytes,
        }
    }

    fn stored_bytes(&self) -> u64 {
        match self {
            Self::Whole(s) => s.stored_bytes(),
            Self::Staged(s) => s.stored_bytes(),
        }
    }

    /// Every chunk's store key, in manifest order.
    fn store_keys(&self) -> Vec<ContentHash> {
        match self {
            Self::Whole(s) => s.chunks.iter().map(|(key, _)| *key).collect(),
            Self::Staged(s) => s.sealed.manifest.store_keys(),
        }
    }

    /// The body of the `index`-th chunk, whose store key is `key` — borrowed
    /// when whole, read back (one chunk) when staged.
    async fn body(&self, index: usize, key: &ContentHash) -> Result<std::borrow::Cow<'_, [u8]>> {
        match self {
            Self::Whole(s) => Ok(std::borrow::Cow::Borrowed(&s.chunks[index].1)),
            Self::Staged(s) => {
                let path = s.bodies.join(hex::encode(key.digest()));
                let body = tokio::fs::read(&path)
                    .await
                    .with_context(|| format!("read staged chunk {}", path.display()))?;
                Ok(std::borrow::Cow::Owned(body))
            }
        }
    }
}

/// The directory the custodian's sealed store occupies inside one actor's
/// scoped state dir. Named rather than inlined so a shell that resolves the
/// path for its own purposes (a "reclaim this device's copy" affordance, an
/// installer's uninstall sweep) agrees with the store by construction.
pub const CUSTODIAN_STORE_DIR: &str = "backup-custodian";

/// Where this device's custodian store lives: `<base>/<actor-id-hex>/backup-custodian`.
///
/// `flat_base` is the shell's own **unscoped** per-user data directory — the
/// desktop sync agent's `SyncPaths::flat_base_dir()`, a mobile app's storage
/// root, tui's data dir. That is the only per-platform question
/// `behavior/backup-destinations.md` § Third destination kind leaves open; the actor scoping and
/// the leaf name are shared here, over the same
/// [`crate::db::actor_state_dir`] derivation `mls_state.db` and the audit
/// state use, so one owner's corpus can never be read — or reclaimed — under
/// another's scope.
///
/// Errors when `actor_id_hex` is not 64 hex chars, rather than quietly rooting
/// a corpus at a path derived from garbage.
pub fn custodian_store_root(flat_base: &Path, actor_id_hex: &str) -> Result<PathBuf> {
    Ok(crate::db::actor_state_dir(flat_base, actor_id_hex)?.join(CUSTODIAN_STORE_DIR))
}

/// How this platform keeps the sealed store out of the OS's own cloud backup —
/// the ratified obligation in `behavior/backup-destinations.md` § State & data
/// shape → Third destination kind (*Durability + labeling*).
///
/// There is deliberately **no default and no "unknown" arm**: a glue author
/// wiring a new platform must say which of these three is true, and the answer
/// is reviewable at the call site. The failure this prevents is silent — a
/// device would hold a full sealed corpus that the vendor cloud replicates
/// alongside the keychain holding the seed that opens it, and nothing on the
/// device would look wrong.
///
/// **Two stores state it, not one (2026-08-26).** The custodian store was the
/// first consumer; the **account store** is the second
/// (`account_runtime::AccountRuntimeParams::store_backup_exclusion`), for the
/// mirror-image reason: its writer key sits in a `ThisDeviceOnly` keychain row
/// that a device restore does not carry, so a restored container holding the
/// store would strand the account plane on the new device
/// (`apps/common.md` § Credential storage → *The shared Rust credential slots
/// on the phones*). Owned rather than borrowed — `String` text, `Arc` closure —
/// precisely so it can ride an owned params struct across a spawned assembly;
/// the custodian's one-shot call pays nothing for that.
pub enum CloudBackupExclusion {
    /// No OS-managed cloud backup reaches this path on this platform (desktop
    /// linux / windows / macOS). `platform` states which, so the claim is
    /// auditable rather than a bare "nothing to do".
    NotApplicable { platform: String },
    /// Excluded declaratively by the app's own manifest — android's
    /// `data_extraction_rules` / `android:allowBackup`, which no runtime call
    /// can substitute for. `declaration` names the file + rule so a reviewer
    /// can check it exists.
    DeclaredInManifest { declaration: String },
    /// Excluded imperatively by the shell once the root exists — **iOS's**
    /// `URL.setResourceValue(true, forKey: .isExcludedFromBackup)`, which needs
    /// the directory to be there first.
    ///
    /// Read "iOS", not "apple": measured on macOS 15 (2026-08-05), that same
    /// call writes `com.apple.metadata:com_apple_backup_excludeItem` =
    /// `com.apple.backupd` and excludes from **Time Machine**, touching nothing
    /// in iCloud — so on macOS it answers a different question than this enum
    /// asks, and taking it there would drop the sealed corpus out of the user's
    /// own local backups for no cloud benefit. macOS states
    /// [`Self::NotApplicable`] instead; the reasoning is at
    /// `bins/fauna-sync-agent/src/custodian.rs::cloud_backup_exclusion`.
    ///
    /// [`CustodianStore::ensure_root`] calls this after creating the root and
    /// **propagates its error**: a store whose exclusion failed must not be
    /// written into.
    ///
    /// `Send + Sync` because the store is opened by the platform's *background*
    /// scheduling hook (the desktop sync agent's spawned host task, a mobile BG
    /// task) — a bare `dyn Fn` makes the whole opening future non-`Send`, so the
    /// one place this variant is for could not use it.
    ExcludedByShell(BackupShellExcluder),
}

/// The closure [`CloudBackupExclusion::ExcludedByShell`] carries — factored
/// into its own alias (rather than spelled inline in the variant) purely to
/// keep the enum readable; the shape itself is unchanged.
pub type BackupShellExcluder = Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>;

impl CloudBackupExclusion {
    /// Apply this exclusion to `root`, which must already exist — the one
    /// place the imperative arm runs, shared by both stores so neither can
    /// forget to propagate the failure. The declarative arms are no-ops here
    /// by construction (the manifest / the platform did the work).
    pub fn apply(&self, root: &Path) -> Result<()> {
        if let Self::ExcludedByShell(apply) = self {
            apply(root)?;
        }
        Ok(())
    }

    /// The posture of a **desktop's per-user platform root** — the one
    /// statement every desktop consumer shares (the sync agent's custodian and
    /// account hosts, the tui and linux apps, and the FFI seat when a desktop
    /// shell passes no sandboxed container). Lifted 2026-08-26 from
    /// `bins/fauna-sync-agent/src/custodian.rs`, where the reasoning below was
    /// first written; that binary now delegates here so the two stores cannot
    /// state the desktop differently.
    ///
    /// Two halves, both load-bearing on macOS (the platform the question is
    /// hardest on):
    ///
    /// 1. **No iCloud device backup exists on macOS**, and iCloud Drive reaches
    ///    only `~/Library/Mobile Documents`; the per-user root is under
    ///    `~/Library/Application Support/Fauna/sync` — the user-domain root,
    ///    never the app-group container — which iCloud Drive does not sync, and
    ///    no Fauna target publishes its container to iCloud either (pinned by
    ///    `no_apple_target_publishes_its_container_to_icloud`).
    /// 2. **The seed is device-bound by default anyway** (`apps/ios.md`
    ///    § Credential Storage) — an independent second reason, not the
    ///    load-bearing one.
    ///
    /// Time Machine is deliberately left alone: it is local (or a user's own
    /// network volume), it is not the vendor cloud holding the keychain, and a
    /// sealed corpus surviving a disk failure is the feature working. Measured
    /// 2026-08-05: `isExcludedFromBackup` on macOS excludes from Time Machine
    /// and touches nothing in iCloud, so taking it there would answer a
    /// different question than this enum asks.
    ///
    /// **On any other target it is a shell arm that REFUSES at apply time**
    /// rather than a guess: a phone's per-app storage *is* reached by iCloud /
    /// Google device backup, and the phone shells state their arm themselves
    /// (the manifest, or the shell excluder) beside the container they supply
    /// — a `NotApplicable` there would be exactly the silent misclaim the enum
    /// exists to make unrepresentable. Reaching the refusal means a phone
    /// shell passed no container, which is a wiring bug; it fails the
    /// assembly loudly at the one place every consumer already handles a
    /// failed exclusion, so no caller of a params *builder* has to grow a
    /// failure path for a desktop-only impossibility.
    pub fn platform_desktop() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::NotApplicable {
                platform: "linux (no OS-managed cloud backup reaches $XDG_DATA_HOME)".into(),
            }
        }
        #[cfg(windows)]
        {
            Self::NotApplicable {
                platform: "windows (%LOCALAPPDATA% is outside OneDrive Known Folder Move \
                           and the Windows Backup folder set)"
                    .into(),
            }
        }
        #[cfg(target_os = "macos")]
        {
            Self::NotApplicable {
                platform: "macos (no iCloud device backup exists on macOS, and iCloud Drive \
                           reaches only ~/Library/Mobile Documents; on macOS this store is under \
                           ~/Library/Application Support/Fauna/sync — the user-domain root, never \
                           the app-group container — which iCloud Drive does not sync; and no \
                           Fauna target publishes its container to iCloud either — pinned by \
                           no_apple_target_publishes_its_container_to_icloud)"
                    .into(),
            }
        }
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        {
            Self::ExcludedByShell(Arc::new(|_root: &Path| {
                anyhow::bail!(
                    "this platform has no established cloud-backup posture for a per-user \
                     platform root — a sandboxed shell states its own arm (the manifest, or \
                     the shell excluder) beside the container it supplies, and none did \
                     (behavior/backup-destinations.md § Third destination kind → Durability + \
                     labeling; apps/common.md § Credential storage)"
                )
            }))
        }
    }
}

impl std::fmt::Debug for CloudBackupExclusion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotApplicable { platform } => {
                write!(f, "NotApplicable({platform})")
            }
            Self::DeclaredInManifest { declaration } => {
                write!(f, "DeclaredInManifest({declaration})")
            }
            Self::ExcludedByShell(_) => write!(f, "ExcludedByShell"),
        }
    }
}

/// What this device's sealed store occupies right now
/// ([`CustodianStore::footprint`]) — the before-picture of which
/// [`ReclaimReport`] is the after, in the same three fields so a caller can
/// compare them without a conversion.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StoreFootprint {
    /// Generations the index calls held. **Index truth** — zero here with
    /// non-zero `bytes` is the interrupted-`put` case, not a contradiction.
    pub generations: usize,
    /// Blob + manifest files on disk, recorded or orphaned alike.
    pub files: usize,
    /// Bytes those files occupy.
    pub bytes: u64,
}

impl StoreFootprint {
    /// Is there anything here to reclaim? — the `store_holds_bytes` input to
    /// `fauna_core::data::custodian_store_is_orphaned`.
    ///
    /// Keyed on **bytes**, not on generations: an interrupted `put` leaves real
    /// disk space behind with no index row naming it, and that space is exactly
    /// what the affordance exists to give back.
    pub fn holds_bytes(&self) -> bool {
        self.bytes > 0
    }
}

/// What one [`CustodianStore::reclaim`] pass freed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReclaimReport {
    /// Index rows dropped.
    pub generations: usize,
    /// Blob + manifest files deleted, including orphans left by an earlier
    /// interrupted `put`.
    pub files: usize,
    /// Bytes those files occupied.
    pub bytes: u64,
}

impl CustodianStore {
    /// A store rooted at `root`. Nothing is touched until first use; a
    /// nonexistent root is an empty store, which is the ordinary first-run
    /// state after enrollment.
    pub fn at(root: PathBuf) -> Self {
        let write_lock = writer_lock_for(&root);
        Self { root, write_lock }
    }

    /// Create the store's root if absent and (re-)assert its cloud-backup
    /// exclusion — **the constructor a shell uses**, [`Self::at`] being the
    /// path-only one tests and read-only openers take.
    ///
    /// Re-asserting on every start rather than only on first creation is
    /// deliberate: the exclusion is an attribute of a directory that an OS
    /// restore, a profile migration or a user copy can drop, and re-applying it
    /// is cheap and idempotent. The same reconcile-on-boot shape the rest of the
    /// client uses for state it cannot prove stayed true
    /// (`nest/common.md` § Client-state recoverability).
    ///
    /// An [`CloudBackupExclusion::ExcludedByShell`] failure aborts: writing a
    /// corpus into a root the platform is still cloud-replicating is the exact
    /// outcome the obligation exists to prevent, and continuing would leave no
    /// trace of it on the device.
    pub async fn ensure_root(root: PathBuf, exclusion: CloudBackupExclusion) -> Result<Self> {
        tokio::fs::create_dir_all(&root)
            .await
            .with_context(|| format!("create custodian store root {}", root.display()))?;
        {
            exclusion.apply(&root).with_context(|| {
                format!(
                    "exclude custodian store {} from platform cloud backup",
                    root.display()
                )
            })?;
        }
        Ok(Self::at(root))
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    /// Where a pull stages a pair in transit (module doc, *Layout*): inside
    /// the root, so it carries the root's cloud-backup exclusion, and outside
    /// `blobs/` and `manifests/`, so neither the reclaim sweep nor
    /// [`Self::footprint`] ever sees it.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    fn audit_path(&self) -> PathBuf {
        self.root.join("audit.json")
    }

    /// This store's audit history: when a self-audit last **ran**, and when one
    /// last **passed**.
    ///
    /// Two timestamps rather than one because they answer different questions
    /// and only one of them is honest about rot: the run clock drives the
    /// debounce, and the pass clock is what the check-in reports. Collapsing
    /// them would make every attempted audit look like a successful one — the
    /// exact inference [`fauna_client_backup::custodian::SelfAudit`] is shaped
    /// to prevent.
    ///
    /// A missing or unreadable record is `Default` — no audit has run. Unlike
    /// [`Self::held`], an unreadable file here is deliberately **not** an error:
    /// the worst it costs is one extra audit, whereas refusing to run would
    /// leave the store permanently unaudited on the strength of a corrupt
    /// side-file.
    pub async fn audit_record(&self) -> AuditRecord {
        match tokio::fs::read(&self.audit_path()).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => AuditRecord::default(),
        }
    }

    /// Record what a self-audit found at `now`, advancing the **pass** clock
    /// only if it passed.
    ///
    /// The verdict is the only thing that moves `last_passed_at`, and a failure
    /// leaves the previous pass's timestamp untouched — so the pair this writes
    /// is exactly the pair the check-in should carry.
    ///
    /// The repair list becomes the report's failing paths **plus** every
    /// standing flag the report did not sample, while its path is still live.
    /// A fresh audit is a fresh answer only about what it *opened*: a path it
    /// sampled and passed has no business staying on a repair list, but one it
    /// never looked at was not re-tested, and dropping it would clear the flag
    /// on non-observation. [`Self::self_audit`] re-opens every flag standing
    /// when it starts, so a report it built misses only a flag another audit
    /// recorded while it ran; the merge is what keeps that one too.
    /// Merging the two lists unconditionally is the opposite mistake: a healed
    /// path flagged for ever. A path no longer live leaves too, because the
    /// store has stopped claiming it and no pass could ever clear it on proof.
    ///
    /// # Errors
    ///
    /// If the index cannot be read (the liveness the merge asks about), or the
    /// record cannot be written.
    pub async fn record_audit(&self, now: i64, report: &SelfAuditReport) -> Result<AuditRecord> {
        let _writer = self.write_lock.lock().await;
        let rows = self.held().await?;
        let mut record = self.audit_record().await;
        record.last_run_at = Some(now);
        record.last_run_passed = Some(report.passed());
        let standing = std::mem::replace(&mut record.failed_paths, report.failed_paths.clone());
        for path in standing {
            if !report.sampled_paths.contains(&path)
                && !record.failed_paths.contains(&path)
                && Self::live_at(&rows, &path).is_some()
            {
                record.failed_paths.push(path);
            }
        }
        if report.passed() {
            record.last_passed_at = Some(now);
        }
        self.write_audit_record(&record).await?;
        Ok(record)
    }

    /// Drop from the standing repair list every path this store can produce
    /// again, and report how many still cannot be produced.
    ///
    /// Called at the end of a pull pass. Clearing on **proof** rather than on
    /// attempt is the whole point: a path the pass tried and failed to re-fetch
    /// (a source that has since dropped it, a cap that refused it, a torn
    /// connection) keeps its flag, so the next pass tries again instead of the
    /// device going quiet over bytes it still cannot produce. Bounded by the
    /// store's live paths: only an audit adds to the list, and
    /// [`Self::record_audit`] keeps a flag only while its path is live.
    ///
    /// Deliberately does **not** touch the verdict. A healed corpus reports
    /// itself verified only once the audit has actually re-sampled it — the
    /// same rule that stops a rotted store looking healthy again on the very
    /// next pull pass.
    pub async fn clear_repaired(&self, keys: &FileDownloadKeys) -> Result<usize> {
        let standing = self.audit_record().await.failed_paths;
        if standing.is_empty() {
            return Ok(0);
        }
        let mut remaining = Vec::new();
        for path in &standing {
            if self.can_produce(path, keys).await.is_err() {
                remaining.push(path.clone());
            }
        }
        if remaining.len() != standing.len() {
            let _writer = self.write_lock.lock().await;
            // Re-read under the lock: an audit may have answered afresh while
            // this pass was re-fetching, and its answer is the newer one.
            let mut record = self.audit_record().await;
            record.failed_paths.retain(|p| remaining.contains(p));
            self.write_audit_record(&record).await?;
        }
        Ok(remaining.len())
    }

    /// Record that the source served `served` against the ledger at `ledger`
    /// this store holds at generation `held` — a refused pull pass.
    ///
    /// A pass that finds the same regression again leaves the record as it
    /// was, so `observed_at` stays the first sighting; a different `held` or
    /// `served` replaces it.
    ///
    /// # Errors
    ///
    /// If the record cannot be written.
    pub async fn record_source_regression(
        &self,
        ledger: &str,
        held: u32,
        served: u32,
        now: i64,
    ) -> Result<()> {
        let _writer = self.write_lock.lock().await;
        let mut record = self.audit_record().await;
        if record
            .source_regressions
            .get(ledger)
            .is_some_and(|r| r.held == held && r.served == served)
        {
            return Ok(());
        }
        record.source_regressions.insert(
            ledger.to_string(),
            SourceRegression {
                held,
                served,
                observed_at: now,
            },
        );
        self.write_audit_record(&record).await
    }

    /// Drop the regression recorded against `ledger`, if any — the pass that
    /// found the source's counter back at or above the held generation.
    ///
    /// # Errors
    ///
    /// If the record cannot be written.
    pub async fn clear_source_regression(&self, ledger: &str) -> Result<()> {
        if !self
            .audit_record()
            .await
            .source_regressions
            .contains_key(ledger)
        {
            return Ok(());
        }
        let _writer = self.write_lock.lock().await;
        let mut record = self.audit_record().await;
        if record.source_regressions.remove(ledger).is_some() {
            self.write_audit_record(&record).await?;
        }
        Ok(())
    }

    async fn write_audit_record(&self, record: &AuditRecord) -> Result<()> {
        let bytes = serde_json::to_vec(record).context("encode custodian audit record")?;
        atomic_write_file(&self.audit_path(), &bytes)
            .await
            .context("write custodian audit record")
    }

    fn fanned(&self, dir: &str, hex: &str) -> PathBuf {
        // `hex` is always 64 chars of a blake3 digest here (callers hex-encode a
        // `[u8; 32]`), but slicing defensively keeps a short value from
        // panicking on a byte boundary.
        let prefix: String = hex.chars().take(2).collect();
        self.root.join(dir).join(prefix).join(hex)
    }

    fn blob_path(&self, key: &ContentHash) -> PathBuf {
        self.fanned("blobs", &hex::encode(key.digest()))
    }

    fn manifest_path(&self, hex: &str) -> PathBuf {
        self.fanned("manifests", hex)
    }

    /// Every generation this store holds, in index order, every path
    /// set-qualified (module doc, *Row paths are set-qualified*).
    ///
    /// A missing index is an empty store. A **corrupt** one is an error — see
    /// the module doc's rule 1 — and so is one a newer build marked unreadable
    /// for this one (`min_reader_version` above [`CURRENT_INDEX_VERSION`]):
    /// every door reads through here, so such an index is never rewritten.
    pub async fn held(&self) -> Result<Vec<HeldRow>> {
        Ok(self.read_index().await?.generations)
    }

    /// The covered folders' display names this store holds, by set name — what
    /// the pull last read off the owner's coverage listing
    /// ([`Self::put_folder_name`]). A set absent here is one this device never
    /// learned a name for, and a re-seed reports it unnamed.
    pub async fn folder_names(&self) -> Result<BTreeMap<String, String>> {
        Ok(self.read_index().await?.folder_names)
    }

    /// The covered folders' addresses and sealed labels this store holds, by
    /// set name ([`Self::put_folder_label`]). An entry the index holds in a
    /// shape this build cannot decode is left out, as a set never labelled is.
    pub async fn folder_labels(&self) -> Result<BTreeMap<String, FolderLabel>> {
        Ok(self
            .read_index()
            .await?
            .folder_labels
            .into_iter()
            .filter_map(|(set, label)| {
                Some((
                    set,
                    FolderLabel {
                        name_hash: fauna_core::hex32::decode(&label.name_hash).ok()?,
                        name_sealed: hex::decode(&label.name_sealed).ok()?,
                    },
                ))
            })
            .collect())
    }

    /// The whole index. The one read every door and both public readers
    /// go through, so the missing-is-empty / corrupt-is-error rule lives once.
    async fn read_index(&self) -> Result<StoreIndex> {
        let path = self.index_path();
        let raw = match tokio::fs::read(&path).await {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(StoreIndex::default());
            }
            Err(e) => {
                return Err(e).with_context(|| format!("read custodian index {}", path.display()));
            }
        };
        let parsed = serde_json::from_slice::<StoreIndex>(&raw);
        // A newer build's index is refused before anything else — whether or
        // not it decodes here — so it is never rewritten, and says so with its
        // own numbers rather than as "corrupt".
        let stamp = match &parsed {
            Ok(index) => Some((index.schema_version, index.min_reader_version)),
            Err(_) => serde_json::from_slice::<StoreIndexStamp>(&raw)
                .ok()
                .map(|s| (s.schema_version, s.min_reader_version)),
        };
        if let Some((schema_version, min_reader_version)) = stamp
            && min_reader_version > CURRENT_INDEX_VERSION
        {
            anyhow::bail!(
                "custodian index {} was written by a newer build (schema_version={schema_version}, \
                 min_reader_version={min_reader_version}, this build={CURRENT_INDEX_VERSION}) — \
                 the held copy is intact and left untouched; update the app",
                path.display()
            );
        }
        parsed.with_context(|| {
            format!(
                "parse custodian index {} — refusing to treat an unreadable index as an empty \
                 store: every held generation would be orphaned and the next reclaim would \
                 delete this device's whole offline copy of the owner's backup",
                path.display()
            )
        })
    }

    /// The live generation at `path`, if any — the newest one that is not a
    /// tombstone. The same derivation the policy applies, so the two agree by
    /// construction.
    pub fn live_at<'a>(rows: &'a [HeldRow], path: &str) -> Option<&'a HeldRow> {
        rows.iter()
            .filter(|r| r.path == path)
            .max_by_key(|r| r.stored_at)
            .filter(|r| !r.deleted)
    }

    /// Persist the index, stamped — never restamping down: an index a newer
    /// but still-readable build wrote keeps its numbers, since
    /// [`StoreIndex::extra`] carries its shape back out unchanged.
    async fn write_index(&self, index: &StoreIndex) -> Result<()> {
        let stamped = StoreIndex {
            schema_version: index.schema_version.max(CURRENT_INDEX_VERSION),
            min_reader_version: index.min_reader_version.max(MIN_READER_INDEX_VERSION),
            generations: index.generations.clone(),
            folder_names: index.folder_names.clone(),
            folder_labels: index.folder_labels.clone(),
            extra: index.extra.clone(),
        };
        let bytes = serde_json::to_vec(&stamped).context("encode custodian index")?;
        atomic_write_file(&self.index_path(), &bytes)
            .await
            .context("write custodian index")
    }

    /// Record the display name of the covered folder whose mirror set is
    /// `set` — what a re-seed restores the folder under
    /// ([`StoreIndex::folder_names`]). Idempotent; a rename on the source
    /// simply overwrites, so the store tracks the folder's current name.
    /// Returns `true` when the index changed.
    ///
    /// Its own door, taken by the pull before the set's rows land, because the
    /// name is known whether or not any byte of the folder fits under the cap:
    /// a folder the cap refused entirely still has a name, and the next pass
    /// that admits it must not be the first to learn it.
    pub async fn put_folder_name(&self, set: &str, name: &str) -> Result<bool> {
        let _writer = self.write_lock.lock().await;
        let mut index = self.read_index().await?;
        if index.folder_names.get(set).map(String::as_str) == Some(name) {
            return Ok(false);
        }
        index.folder_names.insert(set.to_string(), name.to_string());
        self.write_index(&index).await?;
        Ok(true)
    }

    /// Record the address and sealed label of the covered folder whose mirror
    /// set is `set` ([`StoreIndex::folder_labels`]) — [`Self::put_folder_name`]'s
    /// twin, taken at the same moment for the same reason, and overwritten the
    /// same way on a rename. Returns `true` when the index changed.
    pub async fn put_folder_label(
        &self,
        set: &str,
        name_hash: &[u8; 32],
        name_sealed: &[u8],
    ) -> Result<bool> {
        let _writer = self.write_lock.lock().await;
        let mut index = self.read_index().await?;
        let label = StoredFolderLabel {
            name_hash: hex::encode(name_hash),
            name_sealed: hex::encode(name_sealed),
            extra: BTreeMap::new(),
        };
        if index.folder_labels.get(set) == Some(&label) {
            return Ok(false);
        }
        index.folder_labels.insert(set.to_string(), label);
        self.write_index(&index).await?;
        Ok(true)
    }

    /// Store one sealed generation at `path`.
    ///
    /// Idempotent: re-storing a generation already recorded at the same
    /// `(path, manifest_hash)` rewrites nothing and adds no row, so a retried
    /// pull pass cannot double-count against the capacity cap.
    ///
    /// Adding a generation never removes the one it supersedes — retention is
    /// [`fauna_client_backup::custodian::plan_reclaim`]'s call, bounded by the
    /// grace window, and a store that quietly dropped the old copy would defeat
    /// exactly the rogue-source mitigation the window exists for.
    ///
    /// Returns `true` when a new generation was recorded, `false` when this one
    /// was already held. Because the seal is convergent, that boolean is also a
    /// **change detector**: re-sealing unchanged content yields the same manifest
    /// hash, so a caller can ask "did this actually change?" by storing it and
    /// reading the answer — no side-table of prior hashes to keep in step.
    pub async fn put(
        &self,
        path: &str,
        sealed: impl Into<Sealed<'_>>,
        source: SourceFacts,
        stored_at: i64,
    ) -> Result<bool> {
        self.put_inner(path, sealed.into(), source, None, stored_at)
            .await
    }

    /// What storing `sealed` at `path` would add to this store's held total —
    /// the charge a cap admission check must make.
    ///
    /// Zero when `path` is already live at this manifest: [`Self::put`] then
    /// converges without a row, and [`Self::put_repair`] rewrites bytes the
    /// held total already counts. Charging those in full refused a repair near
    /// the cap — the device that most needed its copy healed was the one that
    /// could not — and, once past the check, over-counted the running budget
    /// for the rest of the pass.
    pub async fn charge_for(&self, path: &str, sealed: impl Into<Sealed<'_>>) -> Result<u64> {
        let sealed = sealed.into();
        let manifest_hex = sealed.manifest_hex();
        let rows = self.held().await?;
        if Self::live_at(&rows, path).is_some_and(|live| live.manifest_hash == manifest_hex) {
            return Ok(0);
        }
        Ok(sealed.stored_bytes())
    }

    /// [`Self::put`] for a **covered-folder mirror** row, which additionally
    /// records the source path's sealed name (hex) — see [`HeldRow::path_sealed`].
    ///
    /// A separate door rather than a field on [`SourceFacts`] because the two
    /// axes genuinely differ: a segment-axis path is a machine-authored routing
    /// key that must *never* carry a seal, while a folder path must carry one or
    /// it can never be re-homed. Keeping them apart makes that a matter of which
    /// function you call rather than a `None` someone can forget to fill in —
    /// and leaves `SourceFacts` `Copy`.
    pub async fn put_folder_row(
        &self,
        path: &str,
        sealed: impl Into<Sealed<'_>>,
        source: SourceFacts,
        path_sealed: Option<String>,
        stored_at: i64,
    ) -> Result<bool> {
        self.put_inner(path, sealed.into(), source, path_sealed, stored_at)
            .await
    }

    async fn put_inner(
        &self,
        path: &str,
        sealed: Sealed<'_>,
        source: SourceFacts,
        path_sealed: Option<String>,
        stored_at: i64,
    ) -> Result<bool> {
        let _writer = self.write_lock.lock().await;
        let manifest_hex = sealed.manifest_hex();

        let mut index = self.read_index().await?;
        let rows = &mut index.generations;
        // Against the **live** generation, never "any generation ever held".
        // Liveness here is latest-non-deleted-per-path (the module's one
        // derivation, [`Self::live_at`]), so anything else answers a different
        // question — and answering the weaker one leaves a superseded
        // generation live through an A → B → A rotation inside the grace
        // window, and pins a tombstone over a path the source listed again
        // unchanged. Both are silent: every byte is intact and every hash
        // verifies, and the restore simply hands back the wrong content.
        if Self::live_at(rows, path).is_some_and(|live| live.manifest_hash == manifest_hex) {
            return Ok(false);
        }

        // Bytes first, index last (module doc, rule 2).
        self.write_sealed_bytes(sealed, &manifest_hex, Rewrite::OnlyMissing)
            .await?;

        push_generation(
            rows,
            HeldRow {
                path: path.to_string(),
                manifest_hash: manifest_hex,
                size_bytes: sealed.stored_bytes(),
                source_size_bytes: source.size_bytes,
                source_record_count: source.record_count,
                stored_at,
                deleted: false,
                path_sealed,
            },
        );
        self.write_index(&index).await?;
        Ok(true)
    }

    /// Write one sealed generation's chunk bodies and its manifest.
    ///
    /// The one byte-writing door, so the convergent skip and the repair's
    /// unconditional rewrite are the same code reading one flag rather than two
    /// loops that could drift on what "already stored" means.
    async fn write_sealed_bytes(
        &self,
        sealed: Sealed<'_>,
        manifest_hex: &str,
        rewrite: Rewrite,
    ) -> Result<()> {
        for (index, key) in sealed.store_keys().iter().enumerate() {
            let target = self.blob_path(key);
            if rewrite == Rewrite::OnlyMissing && tokio::fs::metadata(&target).await.is_ok() {
                continue;
            }
            let body = sealed.body(index, key).await?;
            atomic_write_file(&target, &body)
                .await
                .with_context(|| format!("store custodian chunk {}", target.display()))?;
        }
        let manifest_target = self.manifest_path(manifest_hex);
        if rewrite == Rewrite::Always || tokio::fs::metadata(&manifest_target).await.is_err() {
            atomic_write_file(&manifest_target, sealed.manifest_bytes())
                .await
                .with_context(|| format!("store custodian manifest {manifest_hex}"))?;
        }
        Ok(())
    }

    /// Rewrite every byte of the generation this store **already calls live**
    /// at `path` — the remedy behind the self-audit's verdict.
    ///
    /// [`Self::put`] cannot be that remedy and should not try to be: it
    /// converges on content addresses, which is right on the hot path (two
    /// generations of a path routinely share most of their chunks) and is
    /// exactly the assumption an audit failure falsifies. So the repair is its
    /// own door, taken only for a path something has established is
    /// unproducible — `custodian_pull`'s pass reads the standing
    /// [`AuditRecord::failed_paths`] and re-fetches them — and it rewrites
    /// whatever is on disk, because eviction leaves a chunk absent and rot
    /// leaves it present-but-wrong.
    ///
    /// **Moves bytes only, never the index**: the row was right all along, and
    /// recording a second generation of content the path already holds would
    /// charge the capacity cap twice for one copy.
    ///
    /// Returns `false` when `path`'s live generation is not this manifest.
    /// There is then nothing to repair: the content the caller holds is not
    /// what the path holds, so the ordinary put path owns it — and rewriting a
    /// superseded generation's chunks would resurrect bytes the next reclaim is
    /// entitled to sweep.
    pub async fn put_repair(&self, path: &str, sealed: impl Into<Sealed<'_>>) -> Result<bool> {
        let sealed = sealed.into();
        let _writer = self.write_lock.lock().await;
        let manifest_hex = sealed.manifest_hex();
        let rows = self.held().await?;
        if !Self::live_at(&rows, path).is_some_and(|live| live.manifest_hash == manifest_hex) {
            return Ok(false);
        }
        self.write_sealed_bytes(sealed, &manifest_hex, Rewrite::Always)
            .await?;
        Ok(true)
    }

    /// Record that the source reported `path` deleted.
    ///
    /// Idempotent against an already-tombstoned path, so a compacted-out segment
    /// re-observed on every later pass does not accumulate tombstone rows.
    pub async fn put_tombstone(&self, path: &str, stored_at: i64) -> Result<()> {
        let _writer = self.write_lock.lock().await;
        let mut index = self.read_index().await?;
        let rows = &mut index.generations;
        let already_gone = rows
            .iter()
            .filter(|r| r.path == path)
            .max_by_key(|r| r.stored_at)
            .is_some_and(|r| r.deleted);
        if already_gone {
            return Ok(());
        }
        push_generation(
            rows,
            HeldRow {
                path: path.to_string(),
                manifest_hash: String::new(),
                size_bytes: 0,
                source_size_bytes: 0,
                source_record_count: 0,
                stored_at,
                deleted: true,
                // A tombstone names no live content, so it carries no sealed name.
                path_sealed: None,
            },
        );
        self.write_index(&index).await
    }

    /// Drop `victims` from the index, then delete every blob and manifest no
    /// surviving generation references.
    ///
    /// `victims` come from [`fauna_client_backup::custodian::plan_reclaim`],
    /// which only ever names **retained** generations — this method does not
    /// re-derive that and must not be handed a live one.
    ///
    /// The sweep is by *reference set*, not by "delete this generation's
    /// chunks": content addressing means two generations of a path routinely
    /// share most of their chunks, and deleting one generation's chunk list
    /// would punch holes in the other. It also collects orphans from an
    /// interrupted [`Self::put`], which is what makes rule 2's crash ordering
    /// safe rather than merely lossless.
    pub async fn reclaim(&self, victims: &[HeldRow]) -> Result<ReclaimReport> {
        let _writer = self.write_lock.lock().await;
        let doomed: HashSet<(&str, &str, i64)> = victims.iter().map(|v| v.identity()).collect();

        let mut index = self.read_index().await?;
        let rows = std::mem::take(&mut index.generations);
        let survivors: Vec<HeldRow> = rows
            .iter()
            .filter(|r| !doomed.contains(&r.identity()))
            .cloned()
            .collect();
        let dropped = rows.len() - survivors.len();
        // A folder's name outlives none of its rows: once no generation of the
        // set remains (a detached folder aged out, or `reclaim_all`), the name
        // goes too, so an emptied store holds no label for a folder it no
        // longer holds a byte of. The next pull that covers the folder records
        // it again.
        let labels_before = index.folder_names.len() + index.folder_labels.len();
        let holds = |set: &str| {
            survivors
                .iter()
                .any(|r| r.path.starts_with(&format!("{set}/")))
        };
        index.folder_names.retain(|set, _| holds(set));
        index.folder_labels.retain(|set, _| holds(set));
        if dropped > 0 || index.folder_names.len() + index.folder_labels.len() != labels_before {
            index.generations = survivors.clone();
            self.write_index(&index).await?;
        }

        // Everything the survivors still need.
        let mut keep_manifests: HashSet<String> = HashSet::new();
        let mut keep_blobs: HashSet<String> = HashSet::new();
        for row in &survivors {
            if row.deleted || row.manifest_hash.is_empty() {
                continue;
            }
            keep_manifests.insert(row.manifest_hash.clone());
            let manifest = self.read_manifest(&row.manifest_hash).await?;
            for key in manifest.store_keys() {
                keep_blobs.insert(hex::encode(key.digest()));
            }
        }

        let mut report = ReclaimReport {
            generations: dropped,
            ..Default::default()
        };
        sweep_dir(&self.root.join("blobs"), &keep_blobs, &mut report).await?;
        sweep_dir(&self.root.join("manifests"), &keep_manifests, &mut report).await?;
        Ok(report)
    }

    /// The held manifest named by `manifest_hex`, decoded — also the size the
    /// re-home statement signs (`crate::reseed`'s `sign_folder_rehome`).
    pub(crate) async fn read_manifest(&self, manifest_hex: &str) -> Result<ChunkManifest> {
        let path = self.manifest_path(manifest_hex);
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("read custodian manifest {}", path.display()))?;
        fauna_core::encoding::canonical_decode(&bytes)
            .with_context(|| format!("decode custodian manifest {manifest_hex}"))
    }

    /// What this device's sealed store occupies **on disk**, plus how many
    /// generations the index calls held — the read behind
    /// `backup-orphaned-store-row` (`docs/goal/ui/backups.md` § Manage backup
    /// destinations → *Reclaim this device's copy*).
    ///
    /// The byte count is disk truth, walked rather than summed out of the
    /// index, and the two are deliberately not the same number. A store whose
    /// index is empty can still be holding real bytes — an interrupted
    /// [`Self::put`] leaves blobs written but unrecorded, and
    /// [`Self::reclaim`]'s sweep exists precisely because that is a normal
    /// crash outcome. Reporting the index sum would tell such a device it holds
    /// nothing, the orphaned-store row would never render, and those bytes
    /// would be unreclaimable from the app for the life of the install.
    ///
    /// Deliberately not cached: it is read on a page load, and a stale answer
    /// about disk space is the kind of number a user checks against their own
    /// file manager.
    pub async fn footprint(&self) -> Result<StoreFootprint> {
        let generations = self.held().await?.len();
        let mut footprint = StoreFootprint {
            generations,
            ..Default::default()
        };
        for dir in ["blobs", "manifests"] {
            measure_dir(&self.root.join(dir), &mut footprint).await?;
        }
        Ok(footprint)
    }

    /// Free **everything** this store holds — the `backup-destination-reclaim-button`
    /// action (`docs/goal/ui/backups.md` § Manage backup destinations →
    /// *Reclaim this device's copy*).
    ///
    /// Expressed as [`Self::reclaim`] over every row rather than as a directory
    /// delete, for two reasons that are the same reason twice:
    ///
    /// * **One sweep, so the two cannot drift.** Full reclaim inherits the
    ///   reference-set sweep — including its collection of orphans from an
    ///   interrupted [`Self::put`], which a `remove_dir_all` would get right by
    ///   accident and an index-driven delete would miss entirely.
    /// * **The root survives, and so does its cloud-backup exclusion.** The
    ///   exclusion is an attribute of the *directory*
    ///   ([`Self::ensure_root`] re-asserts it on every start for exactly this
    ///   reason); deleting and later recreating the root on re-enrollment is a
    ///   window in which a platform can start replicating a freshly-sealed
    ///   corpus into the vendor cloud that also holds the seed to open it.
    ///
    /// What is left behind is an **empty** store, not an absent one: the index
    /// records no generations and no blob or manifest file remains, so
    /// [`Self::footprint`] reports zero bytes and the orphaned-store row stops
    /// rendering. Re-enrolling this device pulls the corpus again from scratch.
    pub async fn reclaim_all(&self) -> Result<ReclaimReport> {
        let rows = self.held().await?;
        self.reclaim(&rows).await
    }

    /// Open the live generation at `path` — **standalone restore**, with no nest
    /// alive anywhere (`behavior/backup-destinations.md` § Third destination kind → *Standalone
    /// restore*, the strongest argument for the kind).
    ///
    /// This is [`fauna_core::file_download::download_file_bytes_by_manifest`],
    /// the same walk every app already runs, pointed at local bytes: the seal
    /// discriminator, key precedence, decompression and whole-file content-address
    /// verify are shared, not re-implemented here.
    pub async fn open(&self, path: &str, keys: &FileDownloadKeys) -> Result<Vec<u8>> {
        let rows = self.held().await?;
        let row = Self::live_at(&rows, path)
            .ok_or_else(|| anyhow::anyhow!("custodian store holds no live generation at {path}"))?;
        let digest = fauna_core::hex32::decode(&row.manifest_hash).map_err(|e| {
            anyhow::anyhow!("custodian row {path} has a malformed manifest hash: {e}")
        })?;
        fauna_core::file_download::download_file_bytes_by_manifest(
            &LocalBlobs { store: self },
            keys,
            ContentHash::from_digest_raw(digest),
            None, // a custodian's corpus is owner-sealed — no M2 generation
            path,
        )
        .await
    }

    /// Read one live path's held bytes **verbatim** — the manifest and every
    /// chunk it names, produced out of local storage and hash-verified against
    /// the addresses they are stored under, with **no open and no re-seal**.
    ///
    /// The as-is counterpart of [`Self::open`], and the one read behind both
    /// as-is consumers: [`Self::verify_presence`], which throws the bytes away
    /// and keeps only the verdict, and the re-seed leg's covered-folder arm
    /// ([`crate::reseed`]), which pushes them onward untouched.
    ///
    /// # Why an as-is read exists at all
    ///
    /// A covered folder's mirror already **is** the source's at-rest
    /// ciphertext — the pull stores it with no seal step
    /// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage),
    /// because the head arrives sealed under the folder's own audience and this
    /// device holds no key for it. So there is nothing here to open, and a
    /// delivery that re-sealed it would produce a *different* corpus than the
    /// one the source nest's own coordinator mirrors — breaking the very
    /// byte-identity that lets one materialize design serve both restore
    /// sources.
    ///
    /// # What it verifies, and what it does not claim
    ///
    /// Exactly what a local store can lose: a file deleted under it, a
    /// truncated body, a corrupted chunk, an index row pointing at bytes that
    /// are gone. It does **not** claim the plaintext is recoverable by this
    /// device — for a mirrored folder that was never true, and asserting it is
    /// how the alarm became permanent.
    ///
    /// The returned [`SealedBlob`] is the same shape the pull built when it
    /// stored the path, so the delivery leg's byte push is one implementation
    /// across both planes. `content_key_version` is `None`: this store records
    /// no generation, and neither does the mirror custody row the source nest's
    /// own coordinator writes (`bins/fauna-nest/src/segment_backup.rs`'s
    /// `record_folder_custody`).
    pub async fn read_as_is(&self, path: &str) -> Result<SealedBlob> {
        let rows = self.held().await?;
        let row = Self::live_at(&rows, path)
            .ok_or_else(|| anyhow::anyhow!("custodian store holds no live generation at {path}"))?;
        let digest = fauna_core::hex32::decode(&row.manifest_hash).map_err(|e| {
            anyhow::anyhow!("custodian row {path} has a malformed manifest hash: {e}")
        })?;
        let manifest_hash = ContentHash::from_digest_raw(digest);

        let blobs = LocalBlobs { store: self };
        let manifest_bytes =
            fauna_core::file_download::BlobFetcher::fetch_manifest(&blobs, &manifest_hash)
                .await
                .map_err(|e| anyhow::anyhow!("{path}: manifest is not producible: {e}"))?;
        if ContentHash::of_raw(&manifest_bytes) != manifest_hash {
            anyhow::bail!(
                "{path}: manifest bytes do not hash to {}",
                row.manifest_hash
            );
        }
        let manifest: fauna_core::chunk::ChunkManifest =
            fauna_core::encoding::canonical_decode(&manifest_bytes)
                .map_err(|e| anyhow::anyhow!("{path}: manifest does not decode: {e}"))?;

        let store_keys = manifest.store_keys();
        let bodies =
            fauna_core::file_download::BlobFetcher::fetch_chunks(&blobs, &store_keys, path)
                .await
                .map_err(|e| anyhow::anyhow!("{path}: a chunk is not producible: {e}"))?;
        if bodies.len() != store_keys.len() {
            anyhow::bail!(
                "{path}: produced {} of {} chunks",
                bodies.len(),
                store_keys.len()
            );
        }
        for (key, body) in store_keys.iter().zip(bodies.iter()) {
            if ContentHash::of_raw(body) != *key {
                anyhow::bail!(
                    "{path}: chunk {} does not hash to its address",
                    hex::encode(key.digest())
                );
            }
        }

        // `manifest` is the WIRE form here, not the writer's plaintext view a
        // fresh seal returns: this device holds no root for the mirror, so a
        // sealed head's hashes stay sealed. Its consumers read store keys only.
        Ok(SealedBlob {
            manifest,
            manifest_bytes,
            manifest_hash,
            chunks: store_keys.into_iter().zip(bodies).collect(),
            content_key_version: None,
        })
    }

    /// Produce one live path **without opening it** — [`Self::read_as_is`] with
    /// the bytes discarded and only the verdict kept.
    ///
    /// The audit floor for the ordinary-folder mirror plane, which this store
    /// holds as-is rather than sealed ([`Self::self_audit`] explains the
    /// routing). It is the same test the owner-side arm applies to the same
    /// plane — `fauna_client_backup::audit`'s presence check — run against
    /// local bytes instead of the destination's.
    pub async fn verify_presence(&self, path: &str) -> Result<()> {
        self.read_as_is(path).await.map(|_| ())
    }

    /// Can this store produce `path` from local bytes right now?
    ///
    /// The per-plane routing [`Self::self_audit`] samples with, named once so
    /// the audit and the repair loop that clears its findings
    /// ([`Self::clear_repaired`]) cannot drift into asking different questions
    /// of the same path — which would leave a path flagged for ever, or cleared
    /// on a weaker test than the one that flagged it.
    ///
    /// A reserved segment set is owner-sealed under the key this store holds,
    /// so it takes the full [`Self::open`]. A mirrored ordinary folder is
    /// stored **unsealed** — the folder's own content-layer ciphertext, put
    /// as-is — and this store has no content key for it, least of all for a
    /// shared folder whose key it was never granted; opening it can only fail,
    /// so its floor is hash-verified presence (`backup-destinations.md`
    /// § Ordinary-folder coverage → *Retention + audit*).
    pub async fn can_produce(&self, path: &str, keys: &FileDownloadKeys) -> Result<()> {
        if fauna_core::data::is_folder_mirror_set(path) {
            self.verify_presence(path).await
        } else {
            self.open(path, keys).await.map(|_| ())
        }
    }

    /// **Self-audit** — can this store still produce what its own index says it
    /// holds? (`docs/goal/behavior/backup-destinations.md` § Custodian contract question 4,
    /// *Audit answerability*.)
    ///
    /// The owner cannot inclusion-sample a sleeping device, so the custodian
    /// audits itself and its check-in carries the verdict. This is the local
    /// mirror of the owner-side inclusion arm
    /// (`fauna_client_backup::audit::evaluate_inclusion`), and deliberately the
    /// same test: sample `k` **live** paths, [`Self::open`] each, and accept
    /// nothing weaker than present-and-openable. `open` is the shared
    /// `download_file_bytes_by_manifest` walk, which content-address-verifies
    /// the reassembled file — so a truncated blob, a corrupted chunk and a
    /// deleted file all fail here rather than being noticed at restore time,
    /// which is the one moment nothing can be done about them.
    ///
    /// The sample rotates with `seed` (the caller passes the pass's `now`), so
    /// successive audits cover different paths and detection compounds across
    /// days instead of re-checking the same `k` forever — the property the
    /// owner-side arm's own seeding exists for.
    ///
    /// # The standing repair list is always re-sampled
    ///
    /// On top of the `k` rotating paths, every still-live path on the standing
    /// repair list ([`AuditRecord::failed_paths`]) is opened too. The store
    /// already knows those are suspect, and the verdict is this audit's alone
    /// ([`AuditRecord::verdict`] reads only the latest run), so an audit that
    /// left them out would answer "passed" about a store it knows cannot
    /// produce a path: a flag whose repair re-fetch failed (the source can no
    /// longer serve it, or the cap refused it) stays on the list, the next
    /// rotating sample misses it — above `k` live paths, the usual case — and
    /// every check-in after it reports the destination verified while a
    /// standalone restore off this device fails on that path. Re-opening the
    /// flags costs at most the list's length, which [`Self::record_audit`]
    /// bounds by the live set; a flag the pull did re-produce opens, passes and
    /// leaves the list on that proof.
    ///
    /// # What counts as a failure
    ///
    /// Any sampled path that cannot be produced. Notably a path whose row is
    /// live but whose manifest hash is malformed is a **failure, not a skip**:
    /// skipping unreadable rows would hand a rotted store a bypass of the whole
    /// arm, exactly as the owner-side arm refuses to skip a custody row reported
    /// without a path.
    ///
    /// An empty store passes. A device that has pulled nothing yet is holding
    /// everything it claims to (nothing), and failing it would alarm on every
    /// fresh enrollment.
    ///
    /// # Errors
    ///
    /// Only if the index itself cannot be read — which is not an audit verdict
    /// but a broken store, and [`Self::held`] already refuses to read an
    /// unreadable index as an empty one.
    pub async fn self_audit(
        &self,
        keys: &FileDownloadKeys,
        k: usize,
        seed: u64,
    ) -> Result<SelfAuditReport> {
        let rows = self.held().await?;
        // Live paths only: a superseded generation is retention the grace
        // window governs, and a tombstone holds no bytes to produce.
        let mut live: Vec<&str> = rows
            .iter()
            .map(|r| r.path.as_str())
            .filter(|p| Self::live_at(&rows, p).is_some())
            .collect();
        live.sort_unstable();
        live.dedup();

        let mut report = SelfAuditReport::default();
        for idx in fauna_client_backup::audit::sample_indices(live.len(), k, seed) {
            self.audit_one(live[idx], keys, &mut report).await;
        }
        for path in self.audit_record().await.failed_paths {
            if live.binary_search(&path.as_str()).is_ok() && !report.sampled_paths.contains(&path) {
                self.audit_one(&path, keys, &mut report).await;
            }
        }
        Ok(report)
    }

    /// Open one live path for [`Self::self_audit`] and record the answer.
    async fn audit_one(&self, path: &str, keys: &FileDownloadKeys, report: &mut SelfAuditReport) {
        report.sampled_paths.push(path.to_string());
        // Routed by custody plane, exactly as the owner-side inclusion arm
        // does — and through the same door the repair loop clears on, so the
        // two cannot drift (see [`Self::can_produce`]).
        if let Err(e) = self.can_produce(path, keys).await {
            report.failed_paths.push(path.to_string());
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(path),
                error = %e,
                "custodian self-audit: a path this store calls live could not be produced",
            );
        }
    }
}

/// When this store last audited itself, and when it last passed.
///
/// Persisted beside the index rather than inside it: the index is rewritten by
/// every `put` / `reclaim` / tombstone, and threading an unrelated field through
/// those read-modify-write paths is how it would eventually be dropped by one of
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Unix seconds of the last self-audit **attempt**, pass or fail. Drives the
    /// debounce, never the wire.
    #[serde(default)]
    pub last_run_at: Option<i64>,
    /// Unix seconds of the last **passing** self-audit. This is the one the
    /// check-in reports.
    #[serde(default)]
    pub last_passed_at: Option<i64>,
    /// Whether the **most recent** attempt passed. `None` until one has run.
    ///
    /// Separate from [`Self::last_passed_at`] because the two disagree exactly
    /// when it matters: a store that passed yesterday and failed today carries a
    /// yesterday timestamp *and* a false here, and it is this field — not the
    /// timestamp — that says the backup is rotten now.
    #[serde(default)]
    pub last_run_passed: Option<bool>,
    /// The live paths a self-audit could not produce and nothing has since
    /// proven producible — this store's standing **repair list**. A flag
    /// leaves only when a later audit re-samples and passes it, the pull
    /// re-produces it ([`CustodianStore::clear_repaired`]), or its path stops
    /// being live.
    ///
    /// The audit is the only thing that can *find* local bytes gone (the pull's
    /// diff reads the index, which goes on calling them held at their current
    /// manifest), so without somewhere to put the finding it is an alarm with
    /// no remedy behind it. `custodian_pull` re-enters these paths into the
    /// next pass's diff and repairs their bytes, then clears the ones it can
    /// prove it re-produced — which is what makes
    /// `segment-backup-protocol.md` § Client-device custodian (pull)'s "content
    /// addressing re-converges on the next pull" true rather than aspirational.
    ///
    /// Lives here rather than in the index for the reason the timestamps do:
    /// the index is rewritten by every put, tombstone and reclaim, and
    /// threading an unrelated field through those read-modify-write paths is
    /// how it would eventually be dropped by one of them.
    #[serde(default)]
    pub failed_paths: Vec<String>,
    /// The ledgers whose source served a saved counter **below** the
    /// generation this store holds — each a pull pass refused, keyed by the
    /// ledger's own held path (one per set and family). Written by
    /// [`CustodianStore::record_source_regression`], dropped by
    /// [`CustodianStore::clear_source_regression`] on the first pass that finds
    /// the counter back at or above the held generation
    /// (`segment-backup-protocol.md` § Client-device custodian (pull) → *The
    /// pull never tombstones against a source below its copy*).
    ///
    /// Here rather than in the index for the reason the timestamps are: it is
    /// no fact about any held row, and the index's read-modify-write paths
    /// would be the ones to drop it. Absent from a record written before the
    /// arm existed, which loads as none.
    #[serde(default)]
    pub source_regressions: BTreeMap<String, SourceRegression>,
}

/// One refused pull: the source's saved counter went below the ledger this
/// store holds for one set and family — for a client-device custodian after a
/// total loss, the rebuilt box's counter at zero against the owner's only copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRegression {
    /// The generation of the ledger this store holds
    /// (`LiveManifestMirror::next_segment_id_seen`).
    pub held: u32,
    /// The counter the source served (a reply without one counts as zero).
    pub served: u32,
    /// Unix seconds of the first pass that found the source at `served`.
    pub observed_at: i64,
}

impl AuditRecord {
    /// The verdict the check-in should carry, or `None` when no audit has ever
    /// run on this store.
    ///
    /// `None` is not a failure: a device enrolled minutes ago has audited
    /// nothing, and reporting it as failing would alarm on every enrollment —
    /// the same rule `evaluate_overdue` applies to a never-audited destination.
    pub fn verdict(&self) -> Option<fauna_client_backup::custodian::SelfAudit> {
        // The repair list says what to re-fetch, never what to report. It
        // reaches the verdict only through the audit, which re-opens every
        // standing flag (`CustodianStore::self_audit`) — so a flag whose
        // repair failed keeps the verdict failing, and a pass that healed
        // everything still carries the failing verdict until the audit
        // re-samples.
        use fauna_client_backup::custodian::SelfAudit;
        match self.last_run_passed? {
            true => self.last_passed_at.map(SelfAudit::passed),
            // Carries the previous PASS, which is what makes a failure legible:
            // "last verified then, rotten since" rather than a bare failure with
            // no history or — worse — a fresh timestamp.
            false => Some(SelfAudit::failed(self.last_passed_at)),
        }
    }
}

/// What one [`CustodianStore::self_audit`] pass found.
///
/// Carries the failing paths rather than a bare boolean so the host can log
/// *which* generation rotted — the check-in wire only has room for the verdict,
/// and on a headless host the log is the only place the detail can land.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfAuditReport {
    /// The live paths this pass actually opened.
    ///
    /// Named rather than counted because the standing repair list is merged
    /// against it: a flag this pass did not sample was not re-tested, and
    /// dropping it on that basis would clear it on non-observation
    /// ([`CustodianStore::record_audit`]).
    pub sampled_paths: Vec<String>,
    /// The sampled paths that could not be produced from local bytes — always
    /// a subset of [`Self::sampled_paths`].
    pub failed_paths: Vec<String>,
}

impl SelfAuditReport {
    /// How many live paths this pass actually opened.
    pub fn sampled(&self) -> u32 {
        u32::try_from(self.sampled_paths.len()).unwrap_or(u32::MAX)
    }

    /// The verdict this report carries.
    ///
    /// A pass is *no failures among what was sampled* — including the empty
    /// sample, which is what a store holding nothing legitimately reports.
    pub fn passed(&self) -> bool {
        self.failed_paths.is_empty()
    }
}

/// Delete every file under `dir` whose name is not in `keep`, tallying into
/// `report`. Missing directory = nothing to sweep.
/// Sum one fanned-out directory into a [`StoreFootprint`]. The read-only twin
/// of [`sweep_dir`]'s walk, deliberately shaped the same way: an absent
/// directory is an empty one (a store that has never stored anything), and a
/// file whose metadata cannot be read counts as a file of unknown size rather
/// than vanishing from the total.
async fn measure_dir(dir: &Path, footprint: &mut StoreFootprint) -> Result<()> {
    let mut fanout = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("scan {}", dir.display())),
    };
    while let Some(bucket) = fanout
        .next_entry()
        .await
        .with_context(|| format!("scan {}", dir.display()))?
    {
        if !bucket.file_type().await?.is_dir() {
            continue;
        }
        let bucket_path = bucket.path();
        let mut files = tokio::fs::read_dir(&bucket_path)
            .await
            .with_context(|| format!("scan {}", bucket_path.display()))?;
        while let Some(entry) = files.next_entry().await? {
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            footprint.files += 1;
            footprint.bytes = footprint.bytes.saturating_add(size);
        }
    }
    Ok(())
}

async fn sweep_dir(dir: &Path, keep: &HashSet<String>, report: &mut ReclaimReport) -> Result<()> {
    let mut fanout = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("scan {}", dir.display())),
    };
    while let Some(bucket) = fanout
        .next_entry()
        .await
        .with_context(|| format!("scan {}", dir.display()))?
    {
        if !bucket.file_type().await?.is_dir() {
            continue;
        }
        let bucket_path = bucket.path();
        let mut files = tokio::fs::read_dir(&bucket_path)
            .await
            .with_context(|| format!("scan {}", bucket_path.display()))?;
        while let Some(entry) = files.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if keep.contains(&name) {
                continue;
            }
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            tokio::fs::remove_file(entry.path())
                .await
                .with_context(|| format!("reclaim {}", entry.path().display()))?;
            report.files += 1;
            report.bytes = report.bytes.saturating_add(size);
        }
    }
    Ok(())
}

/// The store as a [`BlobFetcher`] — what makes standalone restore the shared
/// walk rather than a second read implementation.
struct LocalBlobs<'a> {
    store: &'a CustodianStore,
}

#[async_trait::async_trait]
impl BlobFetcher for LocalBlobs<'_> {
    async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let hex = hex::encode(hash.digest());
        let path = self.store.manifest_path(&hex);
        tokio::fs::read(&path)
            .await
            .with_context(|| format!("custodian store is missing manifest {hex}"))
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            let path = self.store.blob_path(key);
            let body = tokio::fs::read(&path).await.with_context(|| {
                format!(
                    "custodian store is missing chunk {} for {} — the OS may have \
                     evicted it; the self-audit flags the path and the next pull pass \
                     re-fetches and rewrites it",
                    hex::encode(key.digest()),
                    fauna_core::log_redact::log_path(relative_path)
                )
            })?;
            out.push(body);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_backup::custodian::{CUSTODIAN_GRACE_SECS, plan_reclaim};
    use fauna_core::crypto::BackupKey;

    const DAY: i64 = 24 * 60 * 60;

    fn seed_key() -> BackupKey {
        BackupKey::from_bytes([3u8; 32])
    }

    fn seal(body: &[u8]) -> SealedBlob {
        crate::seal::seal_blob(body, Some((seed_key().convergent_chunk_root(), None))).unwrap()
    }

    fn body(tag: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| tag ^ (i % 251) as u8).collect()
    }

    fn store(dir: &tempfile::TempDir) -> CustodianStore {
        CustodianStore::at(dir.path().join("custody"))
    }

    /// A self-audit report with the given verdict — a failing one naming one
    /// path, because a failure with no failing path is not a shape the audit
    /// can produce.
    fn audited(passed: bool) -> SelfAuditReport {
        SelfAuditReport {
            sampled_paths: vec!["__mail/aa/seg-00000001.dat".into()],
            failed_paths: if passed {
                Vec::new()
            } else {
                vec!["__mail/aa/seg-00000001.dat".into()]
            },
        }
    }

    /// The kind's strongest property: bytes sealed locally come back out with no
    /// nest in the picture at all.
    #[tokio::test]
    async fn a_stored_generation_opens_again_with_no_nest_alive() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let plaintext = body(0xA5, 40_000);

        store
            .put(
                "__mail/aa/seg-00000001.dat",
                &seal(&plaintext),
                SourceFacts {
                    size_bytes: 40_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        let keys = FileDownloadKeys::owner(seed_key());
        let read_back = store
            .open("__mail/aa/seg-00000001.dat", &keys)
            .await
            .unwrap();
        assert_eq!(read_back, plaintext);
    }

    /// The folder door records the sealed name; the segment door never does.
    /// That asymmetry is the point of having two doors — a segment path is a
    /// machine-authored routing key, the class the S9 seal requirement exempts.
    #[tokio::test]
    async fn only_the_folder_door_records_a_sealed_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let facts = SourceFacts {
            size_bytes: 10,
            record_count: 0,
        };

        store
            .put("__mail/aa/seg-00000001.dat", &seal(&body(1, 900)), facts, 0)
            .await
            .unwrap();
        store
            .put_folder_row(
                "__folder/aa/1/abcd",
                &seal(&body(2, 900)),
                facts,
                Some("5e41ed".into()),
                0,
            )
            .await
            .unwrap();

        let rows = store.held().await.unwrap();
        let seg = CustodianStore::live_at(&rows, "__mail/aa/seg-00000001.dat").unwrap();
        let folder = CustodianStore::live_at(&rows, "__folder/aa/1/abcd").unwrap();
        assert_eq!(seg.path_sealed, None, "a segment path carries no seal");
        assert_eq!(folder.path_sealed.as_deref(), Some("5e41ed"));
    }

    /// The additive half: an index row carrying no `path_sealed` key (the
    /// `serde(default)` reading) decodes unchanged, with `path_sealed` absent
    /// rather than the read failing. *No user-data loss* binds the local store
    /// too — a custodian's corpus must survive a decode untouched.
    #[tokio::test]
    async fn an_index_row_without_path_sealed_still_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let keyless = r#"{"generations":[{"path":"__folder/aa/1/abcd","manifest_hash":"ab","size_bytes":7,"stored_at":5}]}"#;
        tokio::fs::create_dir_all(dir.path().join("custody"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("custody").join("index.json"), keyless)
            .await
            .unwrap();

        let rows = store
            .held()
            .await
            .expect("an index row without path_sealed still decodes");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path_sealed, None);
        assert_eq!(rows[0].manifest_hash, "ab");
        assert!(
            store.folder_names().await.unwrap().is_empty(),
            "an index written before folder names were kept holds none"
        );
    }

    // ── set-qualified row paths + the index version pair (2026-09-29) ───────

    fn index_file(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("custody").join("index.json")
    }

    fn raw_index(dir: &tempfile::TempDir) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(index_file(dir)).unwrap()).unwrap()
    }

    /// **An index a newer build marked unreadable for this one is refused and
    /// left byte-intact** — by every door, reads included, whether or not the
    /// rest of it decodes here — and says so with its own numbers.
    #[tokio::test]
    async fn an_index_whose_reader_floor_is_past_this_build_is_refused_and_left_intact() {
        for newer in [
            r#"{"schema_version":3,"min_reader_version":3,"generations":[{"path":"__mail/ab/seg-00000001.dat","manifest_hash":"ab","size_bytes":7,"stored_at":5}]}"#,
            r#"{"schema_version":4,"min_reader_version":3,"generations":"retyped by a newer build"}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = store(&dir);
            tokio::fs::create_dir_all(dir.path().join("custody"))
                .await
                .unwrap();
            std::fs::write(index_file(&dir), newer).unwrap();

            let err = store.held().await.unwrap_err().to_string();
            assert!(
                err.contains("newer build") && err.contains("min_reader_version=3"),
                "{err}"
            );
            assert!(
                store
                    .put(
                        "__mail/ab/seg-00000002.dat",
                        &seal(&body(2, 90)),
                        SourceFacts::default(),
                        9
                    )
                    .await
                    .is_err()
            );
            assert!(store.put_folder_name("__folder/aa/7", "x").await.is_err());
            assert!(store.reclaim_all().await.is_err());
            assert_eq!(
                std::fs::read(index_file(&dir)).unwrap(),
                newer.as_bytes(),
                "the newer build's index is never rewritten"
            );
        }
    }

    /// A newer build whose change was additive keeps the floor at 1: this build
    /// reads it, rewrites it, and hands back what it did not understand, stamped
    /// with the newer numbers rather than restamped down.
    #[tokio::test]
    async fn a_newer_but_additive_index_is_rewritten_with_its_unknown_keys_and_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        tokio::fs::create_dir_all(dir.path().join("custody"))
            .await
            .unwrap();
        std::fs::write(
            index_file(&dir),
            r#"{"schema_version":5,"min_reader_version":1,"generations":[],"future_root":{"k":1}}"#,
        )
        .unwrap();

        store
            .put_folder_name("__folder/aa/7", "Photos")
            .await
            .unwrap();

        let on_disk = raw_index(&dir);
        assert_eq!(on_disk["future_root"], serde_json::json!({"k": 1}));
        assert_eq!(on_disk["schema_version"], 5);
        assert_eq!(on_disk["min_reader_version"], 1);
    }

    // ── the covered folders' display names (2026-09-29) ─────────────────────

    /// The name rides the index beside the rows, survives every other door's
    /// read-modify-write, overwrites on a rename, and is idempotent.
    #[tokio::test]
    async fn a_folder_name_is_kept_beside_the_rows_and_survives_other_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let set = format!("__folder/{}/7", "ab".repeat(32));

        assert!(store.put_folder_name(&set, "Photos").await.unwrap());
        assert!(
            !store.put_folder_name(&set, "Photos").await.unwrap(),
            "re-recording the same name changes nothing"
        );

        // Every other writing door round-trips the names it did not touch.
        store
            .put_folder_row(
                &format!("{set}/abcd"),
                &seal(&body(1, 900)),
                SourceFacts::default(),
                Some("5e41ed".into()),
                5,
            )
            .await
            .unwrap();
        store.put_tombstone("__folder/aa/1/other", 6).await.unwrap();
        assert_eq!(
            store.folder_names().await.unwrap(),
            BTreeMap::from([(set.clone(), "Photos".to_string())])
        );

        // A rename on the source overwrites: the store tracks the current name.
        assert!(store.put_folder_name(&set, "Pictures").await.unwrap());
        assert_eq!(
            store
                .folder_names()
                .await
                .unwrap()
                .get(&set)
                .map(String::as_str),
            Some("Pictures")
        );
        assert_eq!(
            store.held().await.unwrap().len(),
            2,
            "the rows are untouched by the name door"
        );
    }

    /// A name outlives none of its set's rows: reclaiming the set's last
    /// generation drops the name, and a full reclaim leaves no label behind —
    /// while a set that still holds a row keeps its name through the sweep.
    #[tokio::test]
    async fn a_folder_name_is_dropped_with_its_sets_last_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let gone = format!("__folder/{}/7", "ab".repeat(32));
        let kept = format!("__folder/{}/8", "ab".repeat(32));
        for (set, name) in [(&gone, "Photos"), (&kept, "Docs")] {
            store.put_folder_name(set, name).await.unwrap();
            store
                .put_folder_row(
                    &format!("{set}/abcd"),
                    &seal(&body(set.len() as u8, 900)),
                    SourceFacts::default(),
                    Some("5e".into()),
                    5,
                )
                .await
                .unwrap();
        }

        let rows = store.held().await.unwrap();
        let victims: Vec<HeldRow> = rows
            .iter()
            .filter(|r| r.path.starts_with(&format!("{gone}/")))
            .cloned()
            .collect();
        assert_eq!(victims.len(), 1);
        store.reclaim(&victims).await.unwrap();
        assert_eq!(
            store.folder_names().await.unwrap(),
            BTreeMap::from([(kept.clone(), "Docs".to_string())]),
            "the reclaimed set's name went with its last row; the other stayed"
        );

        store.reclaim_all().await.unwrap();
        assert!(
            store.folder_names().await.unwrap().is_empty(),
            "an emptied store holds no label for a folder it holds no byte of"
        );
    }

    /// A fresh store is empty, not an error — the state right after enrollment.
    #[tokio::test]
    async fn a_store_that_was_never_written_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(store(&dir).held().await.unwrap().is_empty());
    }

    /// Rule 1. A corrupt index must NOT read as empty: everything held would be
    /// orphaned and the next reclaim would delete the owner's offline corpus.
    #[tokio::test]
    async fn a_corrupt_index_is_an_error_not_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .put(
                "__mail/aa/seg-00000001.dat",
                &seal(&body(1, 5_000)),
                SourceFacts {
                    size_bytes: 5_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        tokio::fs::write(store.index_path(), b"{ this is not json")
            .await
            .unwrap();

        let err = store.held().await.expect_err("a corrupt index must fail");
        assert!(
            format!("{err:#}").contains("refusing to treat an unreadable index as an empty store"),
            "the error must say why it refuses, got: {err:#}"
        );
    }

    /// Storing a newer generation must not disturb the one it supersedes — the
    /// grace window is the policy's call, not the store's.
    #[tokio::test]
    async fn a_new_generation_does_not_evict_the_one_it_supersedes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/seg-00000001.dat";
        let old = body(1, 6_000);

        store
            .put(
                path,
                &seal(&old),
                SourceFacts {
                    size_bytes: 6_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();
        store
            .put(
                path,
                &seal(&body(2, 6_000)),
                SourceFacts {
                    size_bytes: 6_000,
                    record_count: 1,
                },
                DAY,
            )
            .await
            .unwrap();

        let rows = store.held().await.unwrap();
        assert_eq!(rows.len(), 2, "both generations are held");
        assert_eq!(CustodianStore::live_at(&rows, path).unwrap().stored_at, DAY);
    }

    /// A retried pull pass must not double-count against the capacity cap.
    #[tokio::test]
    async fn re_storing_the_same_generation_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let sealed = seal(&body(7, 9_000));

        store
            .put(
                "__mail/aa/x.dat",
                &sealed,
                SourceFacts {
                    size_bytes: 9_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();
        store
            .put(
                "__mail/aa/x.dat",
                &sealed,
                SourceFacts {
                    size_bytes: 9_000,
                    record_count: 1,
                },
                5,
            )
            .await
            .unwrap();

        assert_eq!(store.held().await.unwrap().len(), 1);
    }

    /// The load-bearing GC rule: reclaiming one generation must not punch holes
    /// in another that shares its chunks. Content addressing makes overlap the
    /// normal case, not the exotic one.
    #[tokio::test]
    async fn reclaiming_one_generation_leaves_a_sharer_openable() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let shared = body(0x11, 30_000);

        // Two paths, identical content — so they seal to the very same chunks.
        store
            .put(
                "__mail/aa/one.dat",
                &seal(&shared),
                SourceFacts {
                    size_bytes: 30_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();
        store
            .put(
                "__mail/aa/two.dat",
                &seal(&shared),
                SourceFacts {
                    size_bytes: 30_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        let rows = store.held().await.unwrap();
        let victim = rows
            .iter()
            .find(|r| r.path == "__mail/aa/one.dat")
            .unwrap()
            .clone();
        store.reclaim(&[victim]).await.unwrap();

        let keys = FileDownloadKeys::owner(seed_key());
        assert_eq!(
            store.open("__mail/aa/two.dat", &keys).await.unwrap(),
            shared
        );
        assert!(
            store.open("__mail/aa/one.dat", &keys).await.is_err(),
            "the reclaimed generation is gone from the index"
        );
    }

    /// Reclaim frees the bytes only the victim held.
    #[tokio::test]
    async fn reclaim_deletes_the_victims_own_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .put(
                "__mail/aa/keep.dat",
                &seal(&body(1, 20_000)),
                SourceFacts {
                    size_bytes: 20_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();
        store
            .put(
                "__mail/aa/drop.dat",
                &seal(&body(2, 20_000)),
                SourceFacts {
                    size_bytes: 20_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        let rows = store.held().await.unwrap();
        let victim = rows
            .iter()
            .find(|r| r.path == "__mail/aa/drop.dat")
            .unwrap()
            .clone();
        let report = store.reclaim(&[victim]).await.unwrap();

        assert_eq!(report.generations, 1);
        assert!(report.files >= 2, "at least one chunk plus its manifest");
        assert!(report.bytes > 0);

        let keys = FileDownloadKeys::owner(seed_key());
        assert!(store.open("__mail/aa/keep.dat", &keys).await.is_ok());
    }

    /// Rule 2's recovery half: bytes written by a `put` that never reached the
    /// index are orphans, and the next reclaim collects them.
    #[tokio::test]
    async fn an_orphan_blob_from_an_interrupted_put_is_collected() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .put(
                "__mail/aa/live.dat",
                &seal(&body(1, 12_000)),
                SourceFacts {
                    size_bytes: 12_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        // Exactly what a crash between the blob write and the index write leaves.
        let orphan = ContentHash::of_raw(b"never indexed");
        atomic_write_file(&store.blob_path(&orphan), b"never indexed")
            .await
            .unwrap();

        let report = store.reclaim(&[]).await.unwrap();
        assert_eq!(report.generations, 0, "no index row was dropped");
        assert_eq!(report.files, 1, "the orphan was collected");
        assert!(!store.blob_path(&orphan).exists());

        let keys = FileDownloadKeys::owner(seed_key());
        assert!(store.open("__mail/aa/live.dat", &keys).await.is_ok());
    }

    // ── Footprint + full reclaim (`ui/backups.md` § Manage backup destinations
    // → *Reclaim this device's copy*) ───────────────────────────────────────

    /// The read behind `backup-orphaned-store-row` reports **disk** bytes and
    /// **index** generations, and the two are different questions.
    #[tokio::test]
    async fn a_footprint_reports_disk_bytes_and_index_generations() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        assert_eq!(
            store.footprint().await.unwrap(),
            StoreFootprint::default(),
            "a store that was never written holds nothing"
        );
        assert!(!store.footprint().await.unwrap().holds_bytes());

        store
            .put(
                "__mail/aa/one.dat",
                &seal(&body(1, 9_000)),
                SourceFacts {
                    size_bytes: 9_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        let footprint = store.footprint().await.unwrap();
        assert_eq!(footprint.generations, 1);
        assert!(footprint.files >= 2, "at least a manifest and one blob");
        assert!(footprint.holds_bytes());
    }

    /// ⚠ The reason the footprint walks the disk instead of summing the index:
    /// a store whose index is empty can still be holding real space, and those
    /// bytes are exactly what the reclaim affordance exists to give back. An
    /// index-derived answer would report "nothing held", the orphaned-store row
    /// would never render, and the space would be unreclaimable from the app.
    #[tokio::test]
    async fn bytes_left_by_an_interrupted_put_still_count_as_held() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let orphan = ContentHash::of_raw(b"never indexed");
        atomic_write_file(&store.blob_path(&orphan), &body(7, 4_096))
            .await
            .unwrap();

        let footprint = store.footprint().await.unwrap();
        assert_eq!(footprint.generations, 0, "the index names nothing");
        assert_eq!(footprint.files, 1);
        assert_eq!(footprint.bytes, 4_096);
        assert!(
            footprint.holds_bytes(),
            "unindexed bytes are still this device's disk space"
        );
    }

    /// Reclaim-everything frees every generation AND every orphan, and leaves
    /// an **empty** store rather than an absent one — the root (and with it the
    /// platform cloud-backup exclusion asserted on that directory) survives.
    #[tokio::test]
    async fn reclaim_all_frees_the_whole_store_and_keeps_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        for (i, path) in ["__mail/aa/one.dat", "__mail/bb/two.dat"]
            .iter()
            .enumerate()
        {
            store
                .put(
                    path,
                    &seal(&body(i as u8 + 1, 8_000)),
                    SourceFacts {
                        size_bytes: 8_000,
                        record_count: 1,
                    },
                    i as i64,
                )
                .await
                .unwrap();
        }
        let orphan = ContentHash::of_raw(b"interrupted");
        atomic_write_file(&store.blob_path(&orphan), b"interrupted")
            .await
            .unwrap();

        let before = store.footprint().await.unwrap();
        assert_eq!(before.generations, 2);

        let report = store.reclaim_all().await.unwrap();
        assert_eq!(report.generations, 2, "both index rows dropped");
        assert!(
            report.files >= before.files,
            "every file swept, the orphan included: {report:?} vs {before:?}"
        );

        let after = store.footprint().await.unwrap();
        assert_eq!(after, StoreFootprint::default(), "nothing left to reclaim");
        assert!(!after.holds_bytes());
        assert!(
            store.root.exists(),
            "the root survives — its cloud-backup exclusion is an attribute of \
             the directory, and re-creating it later is a window in which the \
             platform can start replicating the sealed corpus"
        );
        assert!(store.held().await.unwrap().is_empty());

        // Nothing is openable any more: this is the destructive gesture the
        // confirm modal exists for, not a prune.
        let keys = FileDownloadKeys::owner(seed_key());
        assert!(store.open("__mail/aa/one.dat", &keys).await.is_err());
    }

    /// Reclaiming twice is a no-op, not an error — the gesture is idempotent,
    /// so a repaint that re-fires it cannot fail the page.
    #[tokio::test]
    async fn reclaim_all_on_an_empty_store_is_a_clean_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let report = store.reclaim_all().await.unwrap();
        assert_eq!(report, ReclaimReport::default());
        assert!(store.reclaim_all().await.is_ok());
    }

    /// A tombstone ends liveness without holding bytes.
    #[tokio::test]
    async fn a_tombstone_ends_liveness_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/gone.dat";
        store
            .put(
                path,
                &seal(&body(1, 5_000)),
                SourceFacts {
                    size_bytes: 5_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        store.put_tombstone(path, DAY).await.unwrap();
        store.put_tombstone(path, 2 * DAY).await.unwrap();

        let rows = store.held().await.unwrap();
        assert_eq!(
            rows.len(),
            2,
            "one body, one tombstone — not two tombstones"
        );
        assert!(CustodianStore::live_at(&rows, path).is_none());
    }

    /// The store and the policy compose: what `plan_reclaim` names is exactly
    /// what `reclaim` can be handed, and the survivors still open.
    #[tokio::test]
    async fn the_policys_plan_drives_the_store_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/seg-00000001.dat";
        let newest = body(9, 15_000);

        store
            .put(
                path,
                &seal(&body(8, 15_000)),
                SourceFacts {
                    size_bytes: 15_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();
        store
            .put(
                path,
                &seal(&newest),
                SourceFacts {
                    size_bytes: 15_000,
                    record_count: 1,
                },
                DAY,
            )
            .await
            .unwrap();

        let rows = store.held().await.unwrap();
        let policy: Vec<HeldGeneration> = rows.iter().map(HeldRow::to_policy).collect();
        // 31 days after the supersede, so the older generation is past its window.
        let plan = plan_reclaim(&policy, None, 32 * DAY);
        assert_eq!(plan.reclaim.len(), 1, "exactly the superseded generation");

        let victims: Vec<HeldRow> = plan.reclaim.iter().map(|&i| rows[i].clone()).collect();
        store.reclaim(&victims).await.unwrap();

        let keys = FileDownloadKeys::owner(seed_key());
        assert_eq!(store.open(path, &keys).await.unwrap(), newest);
        assert_eq!(store.held().await.unwrap().len(), 1);
    }

    /// A victim identity that no longer matches any row must delete nothing —
    /// the guard that a plan computed against a stale read cannot take out the
    /// wrong generation.
    #[tokio::test]
    async fn a_stale_victim_matches_nothing_and_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .put(
                "__mail/aa/live.dat",
                &seal(&body(1, 8_000)),
                SourceFacts {
                    size_bytes: 8_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        let mut stale = store.held().await.unwrap()[0].clone();
        stale.stored_at += 1; // a different generation of the same path

        let report = store.reclaim(&[stale]).await.unwrap();
        assert_eq!(report.generations, 0);
        assert_eq!(report.files, 0);
        assert_eq!(store.held().await.unwrap().len(), 1);
    }

    /// Partial OS eviction is tolerated by construction: the index still lists
    /// the generation, the open fails loudly, and nothing is corrupted — the
    /// next pull re-fetches. (`behavior/backup-destinations.md`: iOS/Android app storage carries
    /// no durability guarantee under pressure.)
    #[tokio::test]
    async fn an_os_evicted_chunk_fails_the_open_loudly_without_corrupting_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let sealed = seal(&body(4, 25_000));
        store
            .put(
                "__mail/aa/evicted.dat",
                &sealed,
                SourceFacts {
                    size_bytes: 25_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        let (key, _) = &sealed.chunks[0];
        tokio::fs::remove_file(store.blob_path(key)).await.unwrap();

        let keys = FileDownloadKeys::owner(seed_key());
        let err = store
            .open("__mail/aa/evicted.dat", &keys)
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("missing chunk"),
            "the failure must name the eviction, got: {err:#}"
        );
        assert_eq!(store.held().await.unwrap().len(), 1, "the index is intact");
    }

    // ---- the root + its cloud-backup exclusion (slice 3c glue seam) ----

    const ACTOR_A: &str = "11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff";
    const ACTOR_B: &str = "ffeeddccbbaa00998877665544332211ffeeddccbbaa00998877665544332211";

    /// Two accounts on one device must hold two corpora. A shell that dropped
    /// the actor scope would put both owners' generations in one index, where
    /// one owner's reclaim pass computes over the other's rows.
    #[test]
    fn two_actors_root_at_different_stores() {
        let base = Path::new("/var/lib/fauna");
        let a = custodian_store_root(base, ACTOR_A).unwrap();
        let b = custodian_store_root(base, ACTOR_B).unwrap();
        assert_ne!(a, b, "per-actor scoping collapsed onto one corpus");
        assert_eq!(a.file_name(), b.file_name());
        assert!(a.ends_with(Path::new(ACTOR_A).join(CUSTODIAN_STORE_DIR)));
    }

    /// A garbled actor id is a refusal, not a corpus rooted at a path derived
    /// from garbage — the same rule `actor_state_dir` enforces for sync state.
    #[test]
    fn a_malformed_actor_id_refuses_a_root() {
        assert!(custodian_store_root(Path::new("/var/lib/fauna"), "not-hex").is_err());
    }

    #[tokio::test]
    async fn ensure_root_creates_the_directory_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = custodian_store_root(dir.path(), ACTOR_A).unwrap();

        let store = CustodianStore::ensure_root(
            root.clone(),
            CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        )
        .await
        .unwrap();
        assert!(root.is_dir(), "the root was not created");
        assert!(store.held().await.unwrap().is_empty());

        // Second start of the same device — an existing root is the ordinary
        // case, not a conflict.
        CustodianStore::ensure_root(
            root.clone(),
            CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        )
        .await
        .expect("re-asserting an existing root must be a no-op");
    }

    /// The imperative arm runs, and runs *after* the root exists — apple's
    /// `setResourceValue` has nothing to mark otherwise.
    #[tokio::test]
    async fn the_shell_exclusion_runs_against_the_created_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = custodian_store_root(dir.path(), ACTOR_A).unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));

        let sink = Arc::clone(&seen);
        CustodianStore::ensure_root(
            root.clone(),
            CloudBackupExclusion::ExcludedByShell(Arc::new(move |p: &Path| {
                assert!(
                    p.is_dir(),
                    "the shell was handed a root that does not exist"
                );
                sink.lock().unwrap().push(p.to_path_buf());
                Ok(())
            })),
        )
        .await
        .unwrap();

        assert_eq!(&*seen.lock().unwrap(), &[root]);
    }

    /// **Mutation-verified rule.** A failed exclusion aborts. Swallowing it
    /// (logging and carrying on) would leave a device holding a full sealed
    /// corpus that iCloud replicates alongside the keychain holding the seed
    /// that opens it — with nothing on the device looking wrong.
    #[tokio::test]
    async fn a_failed_exclusion_refuses_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = custodian_store_root(dir.path(), ACTOR_A).unwrap();

        let err = CustodianStore::ensure_root(
            root,
            CloudBackupExclusion::ExcludedByShell(Arc::new(|_p: &Path| {
                anyhow::bail!("setResourceValue refused")
            })),
        )
        .await
        .expect_err("a store must not be handed out with its exclusion unproven");

        let text = format!("{err:#}");
        assert!(
            text.contains("cloud backup") && text.contains("setResourceValue refused"),
            "the failure must name the obligation and the cause, got: {text}"
        );
    }

    // ── The as-is read (behavior/backup-destinations.md § Ordinary-folder coverage) ──────────────

    /// **`read_as_is` produces the held bytes verbatim** — the property the
    /// re-seed leg's covered-folder arm rests on.
    ///
    /// A mirrored folder rests under the *source folder's* audience seal, which
    /// this device holds no key for, so its delivery cannot open and re-seal:
    /// it must move byte-for-byte what the source nest itself mirrored. The
    /// assertion is therefore against the caller's own blob — same manifest
    /// bytes, same manifest hash, same `(store key, body)` pairs in the same
    /// order — and NOT against a second call into the store, which would pass
    /// even if the read quietly re-encoded everything it touched.
    ///
    /// `content_key_version` is `None` on the way out because the store records
    /// none: an owner-scoped corpus has no M2 generations, and neither does the
    /// mirror custody row the source nest's own coordinator writes.
    #[tokio::test]
    async fn read_as_is_produces_the_held_bytes_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__folder/aa/7/bb";
        // One chunk, by construction rather than by accident: the chunker
        // stores anything below `SINGLE_CHUNK_THRESHOLD` (8 MB) as a single
        // blob (`fauna_core::chunker`), so no unit-sized fixture can exercise
        // multi-chunk ordering. What this test owns is therefore *verbatim
        // bytes*; the ordering claim is the delivery leg's, where several paths
        // cross in sequence (`crate::reseed`'s covered-folder tests).
        let held = seal(&body(9, 40_000));

        store
            .put_folder_row(
                path,
                &held,
                SourceFacts {
                    size_bytes: 40_000,
                    record_count: 1,
                },
                Some("5e".to_string()),
                0,
            )
            .await
            .unwrap();

        let out = store.read_as_is(path).await.unwrap();

        assert_eq!(out.manifest_bytes, held.manifest_bytes);
        assert_eq!(out.manifest_hash, held.manifest_hash);
        assert_eq!(
            out.chunks, held.chunks,
            "every (store key, body) pair must come back untouched"
        );
        assert_eq!(out.content_key_version, None);
    }

    /// The as-is read refuses a path the store does not hold live, rather than
    /// producing an empty blob — the same refusal `open` makes, and for the
    /// same reason: a delivery that pushed an empty manifest would mint custody
    /// the target believes in and no restore could ever satisfy.
    #[tokio::test]
    async fn read_as_is_refuses_a_path_with_no_live_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__folder/aa/7/bb";
        store
            .put_folder_row(
                path,
                &seal(&body(9, 4_000)),
                SourceFacts {
                    size_bytes: 4_000,
                    record_count: 1,
                },
                None,
                0,
            )
            .await
            .unwrap();
        store.put_tombstone(path, 10).await.unwrap();

        // `.err()` rather than `unwrap_err()`/`expect_err()`: the Ok type is a
        // `SealedBlob`, which deliberately derives no `Debug` — it carries chunk
        // bodies, and a `Debug` on it is an invitation to log ciphertext.
        #[allow(clippy::err_expect)]
        let err = store
            .read_as_is(path)
            .await
            .err()
            .expect("a tombstoned path must not read as-is");
        assert!(
            err.to_string().contains("no live generation"),
            "unexpected error: {err}"
        );
    }

    // ── Self-audit (behavior/backup-destinations.md § Custodian contract q4) ────────────────────
    /// **A store whose bytes are intact but which cannot DECRYPT them fails its
    /// own audit** — the sealed plane's arm is the full `open`, and this is what
    /// says so.
    ///
    /// The rot tests above cannot see this: they damage the bytes, which fails
    /// *either* arm identically, because `verify_presence` hash-verifies too.
    /// (Measured 2026-08-21 by routing `self_audit`'s sealed branch to
    /// `verify_presence` — every existing test stayed green.) The only thing
    /// that separates the two arms is decryption, so the only mutation that can
    /// grade the routing is one where the bytes are perfect and the KEY is
    /// wrong.
    ///
    /// Why the case is real rather than a contrived one: the store outlives the
    /// process that wrote it. A device that adopts a store written under another
    /// account's scope, or that rotates the owner's backup key, holds a corpus
    /// that is present, hash-correct, and unopenable — and it must say so. A
    /// custodian is the one destination the owner-side audit loop can never
    /// sample (it has no address), so this verdict is the ONLY failure signal
    /// that exists for it; downgrading the arm would leave the row reporting
    /// healthy until someone tried to restore, the one moment nothing can be
    /// done about it.
    #[tokio::test]
    async fn a_store_it_cannot_decrypt_fails_its_own_audit_even_though_every_byte_is_intact() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/seg-00000001.dat";

        store
            .put(
                path,
                &seal(&body(7, 40_000)),
                SourceFacts {
                    size_bytes: 40_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        // Nothing is touched on disk. Every blob is present and hashes to the
        // address it is stored under, so a presence check cannot fault it.
        let intact = store
            .self_audit(&FileDownloadKeys::owner(seed_key()), 16, 0)
            .await
            .unwrap();
        assert!(
            intact.passed(),
            "the store is byte-perfect under its own key; this is the baseline the              assertion below contrasts against"
        );

        // Same bytes, different owner key — the state a rotated or misattributed
        // store is actually in.
        let stranger = FileDownloadKeys::owner(BackupKey::from_bytes([9u8; 32]));
        let report = store.self_audit(&stranger, 16, 0).await.unwrap();
        assert!(
            !report.passed(),
            "a store that cannot produce its own plaintext must FAIL its self-audit.              Passing here means the sealed plane is being sampled with a presence              check, which is satisfied by bytes nobody can ever open — and the              owner's row would keep reporting healthy over an unrestorable backup"
        );
        assert_eq!(
            report.sampled(),
            1,
            "the row must be sampled, not skipped — a skipped sample is how a              rotted store bypasses the arm entirely"
        );
        assert_eq!(
            report.failed_paths,
            vec![path.to_string()],
            "the failing path must be named, so the log says which generation is              unopenable"
        );
    }

    /// The arm the whole finding is about: a store whose bytes rotted under it
    /// must FAIL its own audit.
    ///
    /// Everything else about such a store still looks healthy — it keeps
    /// pulling, its index still lists the generation, `held_bytes` and
    /// `cap_state` are unchanged, and the 30-day intermittency alarm watches for
    /// a *silent* device while this one is talking. Without this, silent backup
    /// rot on the kind built for nest loss.
    ///
    /// Mutation check: make `self_audit` skip a path it cannot open (a `continue`
    /// instead of recording the failure) and this goes green while the store is
    /// visibly unrestorable.
    #[tokio::test]
    async fn a_store_whose_blobs_rotted_fails_its_own_audit() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let keys = FileDownloadKeys::owner(seed_key());
        let path = "__mail/aa/seg-00000001.dat";

        store
            .put(
                path,
                &seal(&body(1, 40_000)),
                SourceFacts {
                    size_bytes: 40_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();

        let healthy = store.self_audit(&keys, 16, 0).await.unwrap();
        assert!(
            healthy.passed(),
            "a store that just sealed its own bytes passes"
        );
        assert_eq!(healthy.sampled(), 1);

        // Rot it exactly as a filesystem would: the index still lists the
        // generation, but a chunk is gone.
        let mut blobs = tokio::fs::read_dir(store.root.join("blobs")).await.unwrap();
        let bucket = blobs
            .next_entry()
            .await
            .unwrap()
            .expect("a blob bucket")
            .path();
        let mut files = tokio::fs::read_dir(&bucket).await.unwrap();
        let victim = files.next_entry().await.unwrap().expect("a blob").path();
        tokio::fs::remove_file(&victim).await.unwrap();

        let rotted = store.self_audit(&keys, 16, 0).await.unwrap();
        assert!(
            !rotted.passed(),
            "a live path that cannot be produced is a failed audit, not a skipped sample"
        );
        assert_eq!(rotted.failed_paths, vec![path.to_string()]);
        // The index is untouched — which is exactly why nothing else notices.
        assert_eq!(store.held().await.unwrap().len(), 1);
    }

    /// One mirrored ordinary-folder file, stored the way
    /// `custodian_pull::run_covered_folders` stores it: **as-is**, the folder's
    /// own content-layer ciphertext with no re-seal, so the store keys address
    /// the bytes themselves and this device holds no key that opens them.
    fn mirrored(ciphertext: &[u8]) -> SealedBlob {
        let key = ContentHash::of_raw(ciphertext);
        let manifest = fauna_core::chunk::ChunkManifest {
            // The plaintext hash of bytes this device cannot produce.
            file_hash: ContentHash::of_raw(b"plaintext this custodian never sees"),
            total_size: ciphertext.len() as u64,
            chunk_hashes: vec![ContentHash::of_raw(b"plaintext chunk")],
            chunk_sizes: vec![ciphertext.len() as u64],
            stored_hashes: Some(vec![key]),
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
        SealedBlob {
            manifest_hash: ContentHash::of_raw(&manifest_bytes),
            manifest,
            manifest_bytes,
            chunks: vec![(key, ciphertext.to_vec())],
            content_key_version: None,
        }
    }

    fn mirror_path() -> String {
        format!("__folder/{}/5/{}", "ab".repeat(32), "cd".repeat(32))
    }

    /// A store holding BOTH populations — a reserved segment set
    /// it sealed itself, and a mirrored ordinary folder it copied as-is —
    /// passes its own audit. Before the plane routing, the mirror row failed an
    /// open no key could ever satisfy, and since a failure never advances
    /// `last_passed_at`, one covered folder froze this device's audit answer
    /// forever: the loudest surface a headless host has, permanently on, hiding
    /// any real rot behind it.
    #[tokio::test]
    async fn a_store_holding_a_mirrored_folder_and_a_sealed_segment_passes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let keys = FileDownloadKeys::owner(seed_key());

        store
            .put(
                "__mail/aa/seg-00000001.dat",
                &seal(&body(1, 20_000)),
                SourceFacts {
                    size_bytes: 20_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();
        store
            .put(
                &mirror_path(),
                &mirrored(&body(2, 20_000)),
                SourceFacts {
                    size_bytes: 20_000,
                    record_count: 0,
                },
                0,
            )
            .await
            .unwrap();

        let report = store.self_audit(&keys, 16, 0).await.unwrap();
        assert_eq!(report.sampled(), 2, "both populations are audited");
        assert!(
            report.passed(),
            "a mirrored folder is verified by presence, not by an open this device cannot do: {:?}",
            report.failed_paths
        );
    }

    /// The load-bearing half: presence is a real test, not a skip. Rot the
    /// mirrored blob exactly as a filesystem would — index intact, bytes gone —
    /// and the audit must fail. A remedy that simply skipped `__folder/…` rows
    /// passes the test above and this one goes green while the device holds
    /// nothing.
    #[tokio::test]
    async fn a_rotted_mirror_blob_still_fails_the_self_audit() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let keys = FileDownloadKeys::owner(seed_key());
        let path = mirror_path();

        store
            .put(
                &path,
                &mirrored(&body(3, 20_000)),
                SourceFacts {
                    size_bytes: 20_000,
                    record_count: 0,
                },
                0,
            )
            .await
            .unwrap();
        assert!(store.self_audit(&keys, 16, 0).await.unwrap().passed());

        let mut blobs = tokio::fs::read_dir(store.root.join("blobs")).await.unwrap();
        let bucket = blobs
            .next_entry()
            .await
            .unwrap()
            .expect("a blob bucket")
            .path();
        let mut files = tokio::fs::read_dir(&bucket).await.unwrap();
        let victim = files.next_entry().await.unwrap().expect("a blob").path();
        tokio::fs::remove_file(&victim).await.unwrap();

        let rotted = store.self_audit(&keys, 16, 0).await.unwrap();
        assert!(
            !rotted.passed(),
            "a mirror path whose bytes are gone is a failure, never a skipped sample"
        );
        assert_eq!(rotted.failed_paths, vec![path]);
    }

    /// A device that has pulled nothing yet is holding everything it claims to.
    /// Failing it would alarm on every fresh enrollment.
    #[tokio::test]
    async fn an_empty_store_passes_its_audit() {
        let dir = tempfile::tempdir().unwrap();
        let report = store(&dir)
            .self_audit(&FileDownloadKeys::owner(seed_key()), 16, 0)
            .await
            .unwrap();
        assert!(report.passed());
        assert_eq!(
            report.sampled(),
            0,
            "nothing to sample, and that is not a failure"
        );
    }

    /// A tombstoned path holds no bytes to produce, so it is not sampled — the
    /// audit is about what the store claims to *hold*.
    #[tokio::test]
    async fn a_tombstoned_path_is_not_audited() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/seg-00000001.dat";

        store
            .put(
                path,
                &seal(&body(1, 5_000)),
                SourceFacts {
                    size_bytes: 5_000,
                    record_count: 1,
                },
                0,
            )
            .await
            .unwrap();
        store.put_tombstone(path, DAY).await.unwrap();

        let report = store
            .self_audit(&FileDownloadKeys::owner(seed_key()), 16, 0)
            .await
            .unwrap();
        assert_eq!(report.sampled(), 0);
        assert!(report.passed());
    }

    /// **A failure must never advance the pass clock.**
    ///
    /// The twin of `cap_state`'s own rule: read the verdict, never infer it. A
    /// store that failed an instant ago would otherwise carry a timestamp saying
    /// it was just verified — the rotting store looking healthiest exactly when
    /// it is worst.
    ///
    /// Mutation check: set `last_passed_at = Some(now)` unconditionally in
    /// `record_audit` and the second assertion flips to `DAY`.
    #[tokio::test]
    async fn a_failed_audit_keeps_the_previous_pass_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);

        assert_eq!(
            store.audit_record().await.verdict(),
            None,
            "a store that has never audited reports NOTHING, not a failure — else \
             every fresh enrollment alarms"
        );

        store.record_audit(0, &audited(true)).await.unwrap();
        let after_pass = store.audit_record().await;
        assert_eq!(after_pass.last_passed_at, Some(0));
        assert_eq!(
            after_pass.verdict(),
            Some(fauna_client_backup::custodian::SelfAudit::passed(0)),
        );

        store.record_audit(DAY, &audited(false)).await.unwrap();
        let after_fail = store.audit_record().await;
        assert_eq!(
            after_fail.last_passed_at,
            Some(0),
            "the failure at DAY must not move the last-PASSED clock"
        );
        assert_eq!(after_fail.last_run_at, Some(DAY));
        assert_eq!(
            after_fail.verdict(),
            Some(fauna_client_backup::custodian::SelfAudit::failed(Some(0))),
            "the verdict is Failed, carrying the previous pass — 'verified then, \
             rotten since'"
        );
    }

    /// An unreadable audit side-file costs one extra audit, never a permanently
    /// unaudited store — deliberately unlike the index, where refusing is the
    /// safe direction because reading it as empty would orphan every blob.
    #[tokio::test]
    async fn a_corrupt_audit_record_reads_as_never_audited() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.record_audit(DAY, &audited(true)).await.unwrap();
        tokio::fs::write(store.audit_path(), b"{ not json")
            .await
            .unwrap();

        let record = store.audit_record().await;
        assert_eq!(record, AuditRecord::default());
        assert_eq!(record.verdict(), None);
    }

    /// A store of 20 live generations — past `AUDIT_SAMPLE_K`, so an audit
    /// genuinely samples a subset — with the last one's bytes evicted. Returns
    /// the evicted path and its sealed blob, for a repair to put back.
    async fn store_past_the_sample_with_one_evicted(
        store: &CustodianStore,
    ) -> (String, SealedBlob) {
        let facts = SourceFacts {
            size_bytes: 900,
            record_count: 1,
        };
        for id in 1..20u8 {
            let path = format!("__mail/aa/seg-{id:08}.dat");
            store
                .put(&path, &seal(&body(id, 900)), facts, 0)
                .await
                .unwrap();
        }
        let before = blob_files(store);
        let evicted = "__mail/aa/seg-00000020.dat".to_string();
        let sealed = seal(&body(20, 900));
        store.put(&evicted, &sealed, facts, 0).await.unwrap();
        let own: Vec<PathBuf> = blob_files(store)
            .into_iter()
            .filter(|f| !before.contains(f))
            .collect();
        assert!(!own.is_empty(), "nothing stored to evict");
        for f in &own {
            std::fs::remove_file(f).unwrap();
        }
        (evicted, sealed)
    }

    /// The first seed from `from` whose `k`-sample does (or does not) cover
    /// `path` — found by asking the audit itself, so the test cannot drift
    /// from the sampler it depends on.
    async fn seed_sampling(
        store: &CustodianStore,
        path: &str,
        covered: bool,
        from: u64,
    ) -> (u64, SelfAuditReport) {
        let keys = FileDownloadKeys::owner(seed_key());
        for seed in from..from + 1_000 {
            let report = store.self_audit(&keys, 16, seed).await.unwrap();
            if report.sampled_paths.iter().any(|p| p == path) == covered {
                return (seed, report);
            }
        }
        panic!(
            "no seed in 1000 {} {path}",
            if covered { "samples" } else { "skips" }
        );
    }

    /// **A flag leaves the repair list only on proof — never because the next
    /// audit simply did not look.**
    ///
    /// Above `AUDIT_SAMPLE_K` live paths an audit re-tests a subset, so the
    /// audit after the one that flagged a path usually does not re-open it.
    /// Taking that audit's failures as the whole list dropped the flag on
    /// non-observation: the path stayed live and unproducible, stopped being
    /// re-fetched, and the check-in reported the destination passed — the
    /// "content addressing re-converges on the next pull" of
    /// `segment-backup-protocol.md` § Client-device custodian (pull) quietly
    /// false for as long as the seeded stride took to land on it again.
    ///
    /// Mutation check: restore the wholesale `clone_from` in `record_audit`
    /// and the retained flag is gone.
    #[tokio::test]
    async fn a_flag_the_next_audit_did_not_sample_stays_on_the_repair_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let (evicted, _) = store_past_the_sample_with_one_evicted(&store).await;

        // Taken before the flag exists: once it does, every audit re-opens it
        // (the test below), so the only report that can miss a standing flag
        // is one that started before another audit recorded it.
        let (_, blind) = seed_sampling(&store, &evicted, false, 0).await;
        assert!(blind.passed(), "every path this audit opened is healthy");

        let (_, flagging) = seed_sampling(&store, &evicted, true, 0).await;
        assert_eq!(flagging.failed_paths, vec![evicted.clone()]);
        store.record_audit(DAY, &flagging).await.unwrap();

        store.record_audit(2 * DAY, &blind).await.unwrap();

        assert_eq!(
            store.audit_record().await.failed_paths,
            vec![evicted],
            "an audit that never opened the flagged path proves nothing about it; \
             dropping the flag here stops the re-fetch while the bytes are still gone"
        );
    }

    /// **A standing flag holds the verdict failing — the audit re-opens it
    /// whether or not the rotating sample lands on it.**
    ///
    /// The verdict is the latest audit's alone, so an audit that sampled only
    /// its `k` rotating paths answered "passed" about a store whose own repair
    /// list named a path it could not produce: a flag whose re-fetch failed
    /// stayed on the list, and every check-in after it reported the
    /// destination verified while a standalone restore failed on that path.
    ///
    /// Mutation check: drop the standing-flag loop from `self_audit` and the
    /// audit passes.
    #[tokio::test]
    async fn a_standing_flag_is_re_opened_by_an_audit_whose_sample_misses_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let (evicted, _) = store_past_the_sample_with_one_evicted(&store).await;

        // A seed whose rotating sample skips the path, found while no flag
        // exists — after that the audit opens it on every seed.
        let (miss, _) = seed_sampling(&store, &evicted, false, 0).await;
        let (_, flagging) = seed_sampling(&store, &evicted, true, 0).await;
        store.record_audit(DAY, &flagging).await.unwrap();

        let report = store
            .self_audit(&FileDownloadKeys::owner(seed_key()), 16, miss)
            .await
            .unwrap();
        assert_eq!(
            report.failed_paths,
            vec![evicted.clone()],
            "the store knows this path is unproducible; an audit that does not \
             re-open it answers 'passed' about bytes it knows are gone"
        );
        let record = store.record_audit(2 * DAY, &report).await.unwrap();
        assert_eq!(record.failed_paths, vec![evicted]);
        assert_eq!(
            record.verdict(),
            Some(fauna_client_backup::custodian::SelfAudit::failed(None)),
            "a store holding an unrepaired flag must not report itself verified"
        );
    }

    /// The healed half: a flag the pull re-produced is re-opened by the same
    /// rule, passes, and leaves — and the verdict goes back to passed.
    #[tokio::test]
    async fn a_re_produced_flag_re_opened_by_the_audit_passes_and_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let (evicted, sealed) = store_past_the_sample_with_one_evicted(&store).await;

        let (miss, _) = seed_sampling(&store, &evicted, false, 0).await;
        let (_, flagging) = seed_sampling(&store, &evicted, true, 0).await;
        store.record_audit(DAY, &flagging).await.unwrap();
        assert!(store.put_repair(&evicted, &sealed).await.unwrap());

        let report = store
            .self_audit(&FileDownloadKeys::owner(seed_key()), 16, miss)
            .await
            .unwrap();
        assert!(report.sampled_paths.contains(&evicted));
        assert!(report.passed());
        let record = store.record_audit(2 * DAY, &report).await.unwrap();
        assert!(record.failed_paths.is_empty());
        assert_eq!(
            record.verdict(),
            Some(fauna_client_backup::custodian::SelfAudit::passed(2 * DAY))
        );
    }

    /// The other half, which must not regress: a flagged path the next audit
    /// **sampled and passed** comes off — a fresh answer about a path it
    /// re-tested is the proof the list is waiting for.
    ///
    /// Mutation check: union the standing list into the new one
    /// unconditionally and the healed path stays flagged for ever.
    #[tokio::test]
    async fn a_flag_the_next_audit_sampled_and_passed_comes_off_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let (evicted, sealed) = store_past_the_sample_with_one_evicted(&store).await;

        let (seed, flagging) = seed_sampling(&store, &evicted, true, 0).await;
        store.record_audit(DAY, &flagging).await.unwrap();

        assert!(store.put_repair(&evicted, &sealed).await.unwrap());
        let (_, healed) = seed_sampling(&store, &evicted, true, seed + 1).await;
        assert!(healed.passed());
        store.record_audit(2 * DAY, &healed).await.unwrap();

        assert!(
            store.audit_record().await.failed_paths.is_empty(),
            "a path the audit re-opened and produced has no business on a repair list"
        );
    }

    /// A flag on a path the store no longer calls live leaves with the next
    /// audit, sampled or not: the store has stopped claiming it, so there is
    /// nothing left to produce — and no pass could ever clear it on proof,
    /// because [`CustodianStore::can_produce`] rightly refuses a tombstone.
    ///
    /// Mutation check: drop the liveness filter in `record_audit` and the
    /// tombstoned path is flagged for ever.
    #[tokio::test]
    async fn a_flag_on_a_path_no_longer_live_leaves_with_the_next_audit() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let (evicted, _) = store_past_the_sample_with_one_evicted(&store).await;

        let (_, flagging) = seed_sampling(&store, &evicted, true, 0).await;
        store.record_audit(DAY, &flagging).await.unwrap();
        store.put_tombstone(&evicted, DAY + 1).await.unwrap();

        let report = store
            .self_audit(&FileDownloadKeys::owner(seed_key()), 16, 0)
            .await
            .unwrap();
        store.record_audit(2 * DAY, &report).await.unwrap();
        assert!(store.audit_record().await.failed_paths.is_empty());
    }

    // ── The copy that never heals ─────────────────────

    /// Every sealed blob this store holds, as file paths — the disk truth an
    /// eviction or a rot acts on.
    fn blob_files(store: &CustodianStore) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![store.root.join("blobs")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    /// **A → B → A must leave A live.**
    ///
    /// Liveness is the *latest* non-deleted generation per path
    /// (`docs/goal/architecture/segment-backup-protocol.md` § Client-device
    /// custodian (pull) → *Custody = the local store*), so the question a put
    /// asks is "is this what the path holds **now**", not "have I ever held
    /// this". The weaker question leaves the superseded B live for the whole
    /// grace window, and a standalone restore off this device then hands back
    /// content the source replaced — silently, because every byte is intact
    /// and every hash verifies.
    ///
    /// Mutation check: widen the duplicate test back to *any* non-deleted row
    /// and the reopened bytes come back as B.
    #[tokio::test]
    async fn re_storing_superseded_content_makes_it_live_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/seg-00000001.dat";
        let facts = SourceFacts {
            size_bytes: 900,
            record_count: 1,
        };
        let a = body(0xA, 900);
        let b = body(0xB, 900);

        assert!(store.put(path, &seal(&a), facts, 0).await.unwrap());
        assert!(store.put(path, &seal(&b), facts, 10).await.unwrap());
        assert!(
            store.put(path, &seal(&a), facts, 20).await.unwrap(),
            "A is not what the path holds NOW, so re-storing it records a new \
             generation rather than being waved through as already held"
        );

        let keys = FileDownloadKeys::owner(seed_key());
        assert_eq!(
            store.open(path, &keys).await.unwrap(),
            a,
            "the live generation must be the one the source last named; returning B \
             here is a restore that silently hands back replaced content"
        );
    }

    /// A path the source stopped listing and then listed again — with the very
    /// same content — must come back live.
    ///
    /// The tombstone is the newest row, so liveness is "deleted". A duplicate
    /// test that looks at *any* non-deleted generation finds the pre-tombstone
    /// row, calls the content already held and records nothing, so the path
    /// stays tombstoned forever — and every later pass re-downloads it,
    /// because the diff reads liveness, finds none, and asks again.
    #[tokio::test]
    async fn a_relisted_path_comes_back_live_after_its_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/seg-00000001.dat";
        let facts = SourceFacts {
            size_bytes: 900,
            record_count: 1,
        };
        let content = body(0xC, 900);

        store.put(path, &seal(&content), facts, 0).await.unwrap();
        store.put_tombstone(path, 10).await.unwrap();
        assert!(
            CustodianStore::live_at(&store.held().await.unwrap(), path).is_none(),
            "the tombstone is the newest row, so the path is not live"
        );

        assert!(
            store.put(path, &seal(&content), facts, 20).await.unwrap(),
            "the source listed the path again — unchanged content on a tombstoned \
             path is still a new generation"
        );
        let keys = FileDownloadKeys::owner(seed_key());
        assert_eq!(store.open(path, &keys).await.unwrap(), content);
    }

    /// **Both rotations above must survive landing inside ONE second.**
    ///
    /// The two tests above space their generations ten seconds apart, so what
    /// they actually pin is the derivation, not its tie rule — and the tie rule
    /// is doing real work. [`CustodianStore::live_at`] resolves equal
    /// `stored_at` through `max_by_key`, which yields the **last** maximum, and
    /// rows are appended chronologically, so the newest push wins a tie. Flip
    /// that (a `rev()` anywhere in the derivation) and A → B → A inside one
    /// second leaves B live while only an unrelated covered-folder test
    /// reddens — which
    /// is the silent failure this whole door exists to stop: every byte intact,
    /// every hash verifying, and the restore handing back replaced content.
    #[test]
    fn a_tie_on_stored_at_resolves_to_the_newest_push() {
        let path = "__mail/aa/seg-00000001.dat";
        let held = |manifest: &str, stored_at: i64, deleted: bool| HeldRow {
            path: path.to_string(),
            manifest_hash: manifest.to_string(),
            size_bytes: 10,
            source_size_bytes: 10,
            source_record_count: 1,
            stored_at,
            deleted,
            path_sealed: None,
        };

        let rotated = [
            held("aa", 100, false),
            held("bb", 100, false),
            held("aa", 100, false),
        ];
        assert_eq!(
            CustodianStore::live_at(&rotated, path)
                .unwrap()
                .manifest_hash,
            "aa",
            "A → B → A inside one second must still leave A live"
        );

        let relisted = [
            held("cc", 100, false),
            held("", 200, true),
            held("cc", 200, false),
        ];
        assert_eq!(
            CustodianStore::live_at(&relisted, path)
                .unwrap()
                .manifest_hash,
            "cc",
            "a path relisted inside its own tombstone's second must come back live"
        );
    }

    /// Run the reclaim the policy plans for `now` — the pull's own step 2,
    /// victims mapped back from the plan's indices exactly as it does.
    async fn reclaim_as_planned(store: &CustodianStore, now: i64) {
        let rows = store.held().await.unwrap();
        let policy: Vec<_> = rows.iter().map(HeldRow::to_policy).collect();
        let plan = plan_reclaim(&policy, None, now);
        let victims: Vec<HeldRow> = plan.reclaim.iter().map(|&i| rows[i].clone()).collect();
        store.reclaim(&victims).await.unwrap();
    }

    /// **Reclaiming a superseded generation must never take the live one with
    /// it** — the reclaim side of the same-second tie the test above pins for
    /// liveness.
    ///
    /// A → B → A inside one pass shares one `stored_at`, so the first and
    /// third generations were one identity. `reclaim` dooms by identity, so
    /// expiring the superseded first A also dropped the live third A, leaving
    /// B the path's latest: a restore then handed back content the source had
    /// replaced — silently, every byte intact and every hash verifying.
    ///
    /// Mutation check: drop the earlier duplicate's removal in
    /// `push_generation` and the index holds three rows (the second
    /// assertion) and a restore returns B.
    #[tokio::test]
    async fn reclaiming_a_superseded_twin_keeps_the_live_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/seg-00000001.dat";
        let facts = SourceFacts {
            size_bytes: 900,
            record_count: 1,
        };
        let a = body(0xA, 900);
        let b = body(0xB, 900);
        const T: i64 = 1_000;

        store.put(path, &seal(&a), facts, T).await.unwrap();
        store.put(path, &seal(&b), facts, T).await.unwrap();
        store.put(path, &seal(&a), facts, T).await.unwrap();

        let on_disk: StoreIndex =
            serde_json::from_slice(&std::fs::read(store.index_path()).unwrap()).unwrap();
        assert_eq!(
            on_disk.generations.len(),
            2,
            "re-recording an identity already held replaces it — two rows with one \
             identity are two rows one reclaim cannot tell apart"
        );

        reclaim_as_planned(&store, T + CUSTODIAN_GRACE_SECS).await;
        let keys = FileDownloadKeys::owner(seed_key());
        assert_eq!(
            store.open(path, &keys).await.unwrap(),
            a,
            "the source's latest listing is A; handing back B is the silent \
             wrong-content restore"
        );
    }

    /// **An evicted or rotted generation heals — through the repair door, and
    /// only through it.**
    ///
    /// The two shapes local bytes fail in are *absent* (the OS evicted the
    /// file) and *present but wrong* (rot). [`CustodianStore::put`] heals
    /// neither: it converges on content addresses, which is right on the hot
    /// path and is exactly the assumption the self-audit's verdict falsifies.
    /// So the repair is its own door that rewrites unconditionally — and it
    /// moves bytes only, never the index, because the row was right all along.
    ///
    /// Without this the goal's ratified property is false:
    /// `docs/goal/behavior/backup-destinations.md` § Third destination kind →
    /// *Durability + labeling* promises "the store must tolerate partial
    /// eviction — content addressing re-converges automatically on the next
    /// pull", and nothing was re-converging anything.
    #[tokio::test]
    async fn a_repair_rewrites_bytes_the_convergent_put_skips() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let facts = SourceFacts {
            size_bytes: 40_000,
            record_count: 1,
        };
        let keys = FileDownloadKeys::owner(seed_key());

        // **One path per shape.** Local bytes fail in two ways and a repair
        // that handled only one would look healed on a test that broke only
        // one — the vacuous half `rot_every_blob`'s own doc comment warns
        // about. `evicted`'s chunks are deleted (the OS reclaimed them);
        // `rotted`'s chunks AND its manifest are overwritten in place, which no
        // `metadata` check can tell from healthy bytes.
        let evicted = "__mail/aa/seg-00000001.dat";
        let rotted = "__mail/aa/seg-00000002.dat";
        let evicted_bytes = body(0x5A, 40_000);
        let rotted_bytes = body(0x3C, 40_000);
        let sealed_evicted = seal(&evicted_bytes);
        let sealed_rotted = seal(&rotted_bytes);

        store.put(evicted, &sealed_evicted, facts, 0).await.unwrap();
        let evicted_files = blob_files(&store);
        store.put(rotted, &sealed_rotted, facts, 0).await.unwrap();
        let rotted_files: Vec<PathBuf> = blob_files(&store)
            .into_iter()
            .filter(|f| !evicted_files.contains(f))
            .collect();
        assert!(!evicted_files.is_empty(), "nothing stored to evict");
        assert!(!rotted_files.is_empty(), "nothing stored to rot");

        for f in &evicted_files {
            std::fs::remove_file(f).unwrap();
        }
        for f in &rotted_files {
            std::fs::write(f, b"rot").unwrap();
        }
        std::fs::write(
            store.manifest_path(&hex::encode(sealed_rotted.manifest_hash.digest())),
            b"rot",
        )
        .unwrap();

        for (path, sealed) in [(evicted, &sealed_evicted), (rotted, &sealed_rotted)] {
            assert!(
                store.open(path, &keys).await.is_err(),
                "{path}: the store cannot produce what its index calls live"
            );
            assert!(
                !store.put(path, sealed, facts, 1).await.unwrap(),
                "{path}: the ordinary put still converges — this IS the live generation"
            );
            assert!(
                store.open(path, &keys).await.is_err(),
                "{path}: and converging heals nothing; a present file is taken at \
                 its address, which is the whole assumption an audit failure \
                 falsifies"
            );
            assert!(
                store.put_repair(path, sealed).await.unwrap(),
                "{path}: the live generation is this manifest, so there is \
                 something to repair"
            );
        }

        assert_eq!(store.open(evicted, &keys).await.unwrap(), evicted_bytes);
        assert_eq!(
            store.open(rotted, &keys).await.unwrap(),
            rotted_bytes,
            "a rotted chunk is present-but-wrong: only an unconditional rewrite \
             heals it"
        );
        assert_eq!(
            store.held().await.unwrap().len(),
            2,
            "a repair moves bytes, never the index — the rows were never wrong"
        );
    }

    /// A repair of content the path no longer holds is a no-op: the ordinary
    /// put path is about to store the new generation, and rewriting the old
    /// one's bytes would resurrect chunks the reclaim is entitled to sweep.
    #[tokio::test]
    async fn a_repair_of_superseded_content_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let path = "__mail/aa/seg-00000001.dat";
        let facts = SourceFacts {
            size_bytes: 900,
            record_count: 1,
        };
        let old = seal(&body(1, 900));
        store.put(path, &old, facts, 0).await.unwrap();
        store
            .put(path, &seal(&body(2, 900)), facts, 10)
            .await
            .unwrap();

        assert!(
            !store.put_repair(path, &old).await.unwrap(),
            "this is not what the path holds now"
        );
    }

    /// **One writer at a time per store ROOT, not per handle.**
    ///
    /// Handles on one root are routinely several in one process — the host's
    /// pull store, the reclaim affordance's read-only `at`, the agent's pipe
    /// server — so a lock each handle owned a private copy of would serialize
    /// nothing that actually races.
    #[tokio::test]
    async fn handles_on_one_root_share_one_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("custody");
        let a = CustodianStore::at(root.clone());
        let b = CustodianStore::at(root);
        let elsewhere = CustodianStore::at(dir.path().join("other"));

        assert!(
            Arc::ptr_eq(&a.write_lock, &b.write_lock),
            "two handles on one root are one store and must share one writer"
        );
        assert!(
            !Arc::ptr_eq(&a.write_lock, &elsewhere.write_lock),
            "a different root is a different store — two accounts on one box must \
             not serialize against each other"
        );
    }

    /// **Concurrent writers never lose a row.**
    ///
    /// Every writing door is a read-modify-write of one `index.json`. On mobile
    /// the scheduled pass and the foreground push loop genuinely overlap
    /// (`fauna_ffi::custodian_host`'s `ActivityGuard` counts them and serializes
    /// only the *manual* reclaim), so two interleaved puts would have one
    /// snapshot the index before the other's row and write it back without it —
    /// a generation whose bytes are on disk and whose row is gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_on_one_root_never_lose_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("custody");
        let facts = SourceFacts {
            size_bytes: 4_000,
            record_count: 1,
        };

        let mut tasks = Vec::new();
        for i in 0..8u8 {
            let root = root.clone();
            tasks.push(tokio::spawn(async move {
                CustodianStore::at(root)
                    .put(
                        &format!("__mail/aa/seg-{:08}.dat", i + 1),
                        &seal(&body(i, 4_000)),
                        facts,
                        0,
                    )
                    .await
                    .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        let rows = CustodianStore::at(root).held().await.unwrap();
        assert_eq!(
            rows.len(),
            8,
            "every concurrent put must be in the index; a short count is a \
             generation this device believes it does not hold, held rows: {rows:?}"
        );
    }

    /// **A reclaim never sweeps a concurrent writer's bytes.**
    ///
    /// The sweep is by *reference set*: every file no surviving row names dies,
    /// which is what collects orphans from an interrupted put — and which makes
    /// a put still in flight indistinguishable from one. The pass that wrote
    /// those bytes then indexes a generation whose chunks are already gone, and
    /// the index says the device holds what it cannot produce.
    ///
    /// Repeated rather than run once: the collision needs the sweep to land
    /// inside the other writer's bytes-then-index window, and a single round
    /// that happened to miss it would report a lock that is not there.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_reclaim_never_sweeps_a_concurrent_puts_bytes() {
        let keys = FileDownloadKeys::owner(seed_key());
        let facts = SourceFacts {
            size_bytes: 40_000,
            record_count: 1,
        };

        for round in 0..8u8 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("custody");
            let staged = CustodianStore::at(root.clone());

            // A superseded generation for the reclaim to actually drop, so its
            // sweep has real work rather than short-circuiting.
            let victim_path = "__mail/aa/seg-00000001.dat";
            let survivor = body(0xF1, 40_000);
            staged
                .put(victim_path, &seal(&body(0xF0, 40_000)), facts, 0)
                .await
                .unwrap();
            staged
                .put(victim_path, &seal(&survivor), facts, 10)
                .await
                .unwrap();
            let victims: Vec<HeldRow> = staged
                .held()
                .await
                .unwrap()
                .into_iter()
                .filter(|r| r.stored_at == 0)
                .collect();
            assert_eq!(victims.len(), 1);

            let fresh_path = "__mail/aa/seg-00000002.dat";
            let fresh = body(round, 40_000);
            let sealed = seal(&fresh);

            let reclaimer = {
                let root = root.clone();
                tokio::spawn(async move { CustodianStore::at(root).reclaim(&victims).await })
            };
            let writer = {
                let root = root.clone();
                tokio::spawn(async move {
                    CustodianStore::at(root)
                        .put(fresh_path, &sealed, facts, 20)
                        .await
                })
            };
            reclaimer.await.unwrap().unwrap();
            writer.await.unwrap().unwrap();

            let store = CustodianStore::at(root);
            assert_eq!(
                store.open(fresh_path, &keys).await.unwrap(),
                fresh,
                "round {round}: the reclaim swept bytes a concurrent put had \
                 written but not yet indexed"
            );
            assert_eq!(
                store.open(victim_path, &keys).await.unwrap(),
                survivor,
                "round {round}: the surviving generation must be untouched"
            );
        }
    }
}
