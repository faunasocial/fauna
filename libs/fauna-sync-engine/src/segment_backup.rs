//! Shared leaves of the cross-location segment-backup arm, plus the
//! enroll-time destination resolution
//! (`docs/goal/architecture/message-segment-store.md` § Cross-location
//! backup protocol).
//!
//! This module used to also hold the **client-driven** upload coordinator
//! (`BackupCoordinator`: list → diff → upload → record custody → mirror,
//! per `(destination, kind)` tuple, plus the removal-reconcile teardown).
//! **That driver is deleted (2026-08-17)** — the *Coordinator seam
//! corollary* ratified that the client arm dies with the slice-5 flip,
//! which completed across all 7 apps 2026-08-16: backup upload for a
//! `nest`-kind destination is run by the source nest's own in-process
//! `NestBackupCoordinator` (`bins/fauna-nest/src/segment_backup.rs`),
//! which reuses this module's leaves verbatim.
//!
//! What lives here now, and who consumes it:
//! * the shared leaves — [`diff_segments`], [`hash_segments_canonical`],
//!   [`LiveManifestMirror`], the rel-path helpers, [`BACKED_UP_KINDS`],
//!   the cadence constants — consumed by the nest arm and by the
//!   client-device custodian pull (`crate::custodian_pull`);
//! * the [`SegmentSource`] seam + [`SourceBinding`] — the custodian
//!   pull's source arm (via `crate::custodian_host`) and the trait the
//!   nest's local source implements;
//! * the shared push pumps ([`spawn_push_pumps`] / `fauna_core::debounce::absorb_burst`)
//!   the custodian pull's driver loops ride;
//! * [`resolve_destination`] / [`resolve_destination_connected`] — the
//!   enroll-time identity resolution the destination add/edit dialogs
//!   drive on every native app.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fauna_core::data::{ContentHash, parse_folder_backup_set_name};
use fauna_protocol::folders::{FoldersListReply, FoldersListRequest, KIND_FOLDERS_LIST};
use fauna_protocol::push_events::SegmentsChangedPayload;
use fauna_protocol::segments::{SegmentRef, SegmentsListReply, SegmentsListRequest};
use fauna_protocol::sync::{SyncFilesReply, SyncFilesRequest};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::db::MailBackupSegmentState;
use crate::nest_client::SyncClient;

// ────────────────────────────────────────────────────────────────────
// Tunables
// ────────────────────────────────────────────────────────────────────

/// Debounce window for push-triggered runs (`fauna.segments.changed` +
/// `fauna.mail.received`). Ridden by the custodian pull's driver loops
/// (`crate::custodian_pull`). Per Plan 5 spec § D2 / Open items.
pub const PUSH_DEBOUNCE: Duration = Duration::from_secs(5);

/// The one global backup cadence — the nest's own `NestBackupWorker` sweep
/// and the custodian pull's `run_forever` both tick on it
/// (`docs/goal/behavior/backup-destinations.md` § Scheduling: one coordinator
/// cadence for every tuple, never per-destination).
pub const PERIODIC_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// `WS-RPC` kind string a segment source sends to the source nest
/// (`fauna_protocol::segments::KIND_SEGMENTS_LIST`, spelled once there so the
/// audit's source read and this arm cannot drift).
pub const SEGMENTS_LIST_KIND: &str = fauna_protocol::segments::KIND_SEGMENTS_LIST;

/// `WS-RPC` push kind: per-record arrival for the mail kind. Subscribed
/// by [`spawn_push_pumps`] to debounce-trigger a pass.
pub const MAIL_RECEIVED_KIND: &str = "fauna.mail.received";

/// `WS-RPC` push kind: segment-store-level event (finalize, compaction
/// in/out, tombstone). Also subscribed by [`spawn_push_pumps`].
pub const SEGMENTS_CHANGED_KIND: &str = "fauna.segments.changed";

/// The segment kinds the backup arm covers, for **every** runner — the
/// nest's own worker (`bins/fauna-nest/src/segment_backup.rs`, which
/// re-exports this) and the client-device custodian pull.
///
/// **One constant, deliberately.** The nest arm used to declare its own
/// `["mail"]` "in lockstep by comment only", which was harmless while both
/// sides independently swept their own list. It stopped being harmless when a
/// client observing a fresh nest holder began yielding the **whole kind**: the
/// nest's `backup-upload` lease is kind-agnostic, so any kind a client lists
/// and the nest does not sweep would stop being backed up **silently**
/// (the same treatment the destination mode
/// filter got before it retired). Divergence is now unrepresentable rather than warned about.
///
/// Every actor-scoped kind (2026-09-29): each has every nest-side arm (the
/// nest-local source serves it and its journal, the materialize verb
/// reconstitutes it), and a client-device custodian's store keys every row by
/// its set (`crate::custodian_store::held_path`), so kinds whose content
/// families share one within-set grammar (`{scope_hex}/seg-NNNNNNNN.*`) rest
/// side by side. `conv` is absent: it scopes on a channel, not the owner.
/// Adding a kind here is not sufficient on its own — a runner whose body is
/// kind-specific must grow an arm first, and `NestLocalSegmentSource` holds a
/// **compile-time** pin that fails the build until it does.
pub const BACKED_UP_KINDS: &[&str] = &["mail", "post", "calendar", "card"];

// ────────────────────────────────────────────────────────────────────
// Bindings + report
// ────────────────────────────────────────────────────────────────────

/// Where a backup pass reads the owner's source segments from — the **source
/// arm** seam (`docs/goal/architecture/message-segment-store.md`
/// § Cross-location backup protocol).
///
/// Two arms exist by design, because the 2026-07-23 redesign moved the writer:
///
/// - [`SourceBinding`] — the **client-side arm**: lists over WS-RPC and
///   fetches bytes over `GET /api/v1/segments/...`. Its shipped consumer is
///   the client-device **custodian pull** (`crate::custodian_host` builds one
///   per hosted account); the deleted client *upload* driver used to be the
///   other.
/// - a **local arm**, supplied by the source nest itself, which reads its own
///   segment files straight off disk — no HTTP self-fetch. It lives nest-side
///   because the on-disk layout (`<data_dir>/__<kind>/<scope_hex>/`) and the
///   manifest→[`SegmentRef`] mapping are the nest's own knowledge, already
///   implemented by its `fauna.segments.list` handler; this trait is the seam it
///   plugs into (`bins/fauna-nest/src/segments/backup_source.rs`).
///
/// Splitting it here is what lets a nest-hosted coordinator exist at all: every
/// other step of a pass (diff, upload, custody record, mirror) is identical for
/// both arms and stays in one place.
///
/// **Not** `fauna_account_store::store::BootstrapSource`, which is the account
/// plane's *adoption* arm — where a fresh replica gets segments in order to
/// become a replica (`account-data-plane.md` § the bootstrap contract). This
/// trait is the backup arm: where a pass reads the owner's segments to ship
/// them elsewhere. Both hand back the `.dat`/`.meta` **pair** — adoption reads
/// `record_order` out of the sidecar; backup ships it opaque so a later
/// materialization can (`message-segment-store.md` § Client-device custodian
/// (pull) → *Restore*, the 2026-08-29 corpus widening) — but that one is keyed
/// on the plane's `scope` and this one on `(kind, scope_hex)`. An adoption
/// source over a real nest is expected to **adapt** these mechanics, which is
/// why the two stay cross-referenced rather than quietly parallel.
///
/// ⚠ There is deliberately **no** "`.dat` alone" method: a backup arm that
/// stored the `.dat` alone would leave a corpus lacking the sidecar the footers
/// live in, so nothing it held could be reopened. The pair is the unit;
/// [`Self::segment_meta_bytes`] exists only for the sidecar backfill — crash
/// recovery for a torn pair (the `.dat` put landed, the `.meta` put did not).
/// One family's live segments as the source lists them, together with the
/// source's saved segment counter — the two read in **one** observation, so
/// the ledger a pass writes carries the counter the listed segments were live
/// under ([`live_manifest_mirror`]).
///
/// `next_segment_id` is [`SegmentsListReply::next_segment_id`]. `Default`
/// exists for fixtures (`..Default::default()`); its `0` counter leaves the
/// mirror on the derived value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SegmentListing {
    pub segments: Vec<SegmentRef>,
    pub next_segment_id: u32,
}

impl From<SegmentsListReply> for SegmentListing {
    fn from(reply: SegmentsListReply) -> Self {
        Self {
            segments: reply.segments,
            next_segment_id: reply.next_segment_id,
        }
    }
}

/// The `manifest.<kind>` ledger a pass stores for `listing`: its live
/// segments under the source's saved counter — the generation the client
/// audit pins (`fauna_protocol::segments::LiveManifestMirror::next_segment_id_seen`).
///
/// **The one place the counter is chosen**, for both writers (the nest-hosted
/// coordinator and the client custodian pull), so the two ledgers a device
/// may pin agree on what a generation is. The saved counter, not the greatest
/// live id plus one, because an empty-output compaction of the top segment
/// can lower the derived value.
pub fn live_manifest_mirror(listing: &SegmentListing) -> LiveManifestMirror {
    let derived = listing
        .segments
        .iter()
        .map(|r| r.segment_id)
        .max()
        .map(|m| m.saturating_add(1))
        .unwrap_or(0);
    LiveManifestMirror {
        // The saved counter is never below the derived one on an honest
        // source (every id ever issued is below it); the `max` only guards a
        // source whose listing and counter were somehow read across a write.
        next_segment_id_seen: listing.next_segment_id.max(derived),
        live: listing.segments.clone(),
        extra: std::collections::BTreeMap::new(),
    }
}

#[async_trait::async_trait]
pub trait SegmentSource: Send + Sync {
    /// Stable key for this source in `sync_db`'s segment-state rows, and the
    /// identifier used in log/error context.
    fn source_id(&self) -> &str;

    /// The segments currently live for `(kind, scope_hex)`, with the source's
    /// saved segment counter when it reports one ([`SegmentListing`]).
    async fn list_segments(&self, kind: &str, scope_hex: &str) -> Result<SegmentListing>;

    /// The live segments of one **family** of a backed-up kind, or `None` when
    /// the kind has no such family ([`SegmentFamily::serve_kind`] — e.g. a
    /// kind with no placement layer). A pass then moves the content and skips
    /// the journal.
    ///
    /// Every nest serves each backed-up kind's journal under its serve tag, so
    /// a source refusing that tag is an ordinary listing error, never "no
    /// journal here".
    async fn list_family(
        &self,
        kind: &str,
        family: SegmentFamily,
        scope_hex: &str,
    ) -> Result<Option<SegmentListing>> {
        match family.serve_kind(kind) {
            Some(tag) => self.list_segments(tag, scope_hex).await.map(Some),
            None => Ok(None),
        }
    }

    /// The raw bytes of one segment's `.dat` **and** its `.meta` sidecar.
    /// Treated as opaque by every caller — the coordinator never opens a
    /// record payload. A source finalizes on read so both halves exist and
    /// describe the same records.
    async fn segment_pair(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
    ) -> Result<SegmentPair>;

    /// [`Self::segment_pair`] staged to two files in a fresh directory under
    /// `staging` — the form the client custodian pull keeps a pair through, so
    /// no half is ever whole in its memory
    /// (`docs/goal/architecture/message-segment-store.md` § Segment size).
    ///
    /// The default stages what [`Self::segment_pair`] returns: an in-process
    /// source already holds the pair, so writing it out adds nothing to hold.
    /// A wire source overrides it to stream each half to disk as it arrives
    /// ([`SourceBinding`]).
    async fn stage_segment_pair(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
        staging: &std::path::Path,
    ) -> Result<StagedPair> {
        let pair = self.segment_pair(kind, scope_hex, segment_id).await?;
        StagedPair::from_pair(&pair, staging).await
    }

    /// The `.meta` sidecar alone — for backfilling a segment whose `.dat` the
    /// destination already holds without its sidecar (a torn pair)
    /// ([`SegmentDiff::to_backfill_meta`]). Never the primary read.
    ///
    /// The default keeps the sidecar half of [`Self::segment_pair`]: an
    /// in-process source already holds the pair, so there is nothing to save
    /// by a second door. A wire source overrides it to fetch the sidecar
    /// alone ([`SourceBinding`]).
    async fn segment_meta_bytes(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
    ) -> Result<Vec<u8>> {
        Ok(self.segment_pair(kind, scope_hex, segment_id).await?.meta)
    }

    /// The WS-RPC client supplying push events, when this source has one.
    ///
    /// `None` for an in-process nest-hosted source: a nest observes its own
    /// segment writes directly and drives its own scheduling, so it neither has
    /// nor needs a push subscription to itself. [`spawn_push_pumps`] then
    /// spawns nothing and the driver runs on its periodic timer alone.
    fn push_client(&self) -> Option<&Arc<fauna_client::NestClient>> {
        None
    }
}

/// The **client-side** source arm — today the custodian pull's
/// (`crate::custodian_host` builds one per hosted account).
///
/// `source_nest_id` is the stable key used in `sync_db`'s segment-state
/// rows (typically the source nest's actor pubkey hex; the caller picks
/// the scheme). `ws_client` runs the WS-RPC `fauna.segments.list` call
/// and supplies the push-event stream. `sync_client` runs the HTTP
/// `GET /api/v1/segments/...` byte fetches — which is why that
/// transitional route cannot retire while this arm ships.
pub struct SourceBinding {
    pub source_nest_id: String,
    pub sync_client: Arc<SyncClient>,
    pub ws_client: Arc<fauna_client::NestClient>,
}

#[async_trait::async_trait]
impl SegmentSource for SourceBinding {
    fn source_id(&self) -> &str {
        &self.source_nest_id
    }

    async fn list_segments(&self, kind: &str, scope_hex: &str) -> Result<SegmentListing> {
        let req = SegmentsListRequest {
            kind: kind.to_string(),
            actor_id: scope_hex.to_string(),
            extra: Default::default(),
        };
        let reply: SegmentsListReply = self
            .ws_client
            .request(SEGMENTS_LIST_KIND, req)
            .await
            .with_context(|| {
                format!(
                    "{SEGMENTS_LIST_KIND} (source_nest={}, kind={kind})",
                    self.source_nest_id
                )
            })?;
        Ok(reply.into())
    }

    async fn segment_pair(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
    ) -> Result<SegmentPair> {
        // `.dat` first: the route finalizes the open segment on read, and the
        // sidecar only exists once a segment is finalized (the same order the
        // adoption arm, `crate::bootstrap_source`, fetches in). A consumer
        // verifies each half against the listing's hashes, so a pair torn by
        // a concurrent rotation fails loudly and the next pass re-fetches.
        let dat = self
            .sync_client
            // Unbounded: the backup pass reads the owner's OWN segments for
            // the owner's own fleet — the attacker-sized-body concern bounds
            // the custody adoption arm (`crate::bootstrap_source`), where the
            // responder is another principal's nest.
            .get_segment_bytes(kind, scope_hex, segment_id, 0, u64::MAX)
            .await
            .with_context(|| {
                format!("fetch source segment kind={kind} scope={scope_hex} seg={segment_id}")
            })?;
        let meta = self.segment_meta_bytes(kind, scope_hex, segment_id).await?;
        Ok(SegmentPair { dat, meta })
    }

    /// Each half streamed off the byte plane into its staged file a transport
    /// chunk at a time and hashed on the way — the shape the adoption arm
    /// fetches in (`crate::bootstrap_source`), `.dat` first for the same
    /// reason [`Self::segment_pair`] gives. Unbounded, as that fetch is.
    async fn stage_segment_pair(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
        staging: &std::path::Path,
    ) -> Result<StagedPair> {
        use fauna_account_store::segments::SegmentHalf;

        let mut writer = StagedPairWriter::create(staging).await?;
        let dat = self
            .sync_client
            .segment_body(kind, scope_hex, segment_id, 0, u64::MAX)
            .await;
        let dat_blake3_hex =
            crate::bootstrap_source::stream_half(dat, SegmentHalf::Dat, &mut writer)
                .await
                .with_context(|| {
                    format!("fetch source segment kind={kind} scope={scope_hex} seg={segment_id}")
                })?;
        let meta = self
            .sync_client
            .segment_meta_body(kind, scope_hex, segment_id, u64::MAX)
            .await;
        let meta_blake3_hex = crate::bootstrap_source::stream_half(
            meta,
            SegmentHalf::Meta,
            &mut writer,
        )
        .await
        .with_context(|| {
            format!("fetch source segment sidecar kind={kind} scope={scope_hex} seg={segment_id}")
        })?;
        writer.finish(dat_blake3_hex, meta_blake3_hex).await
    }

    async fn segment_meta_bytes(
        &self,
        kind: &str,
        scope_hex: &str,
        segment_id: u32,
    ) -> Result<Vec<u8>> {
        self.sync_client
            .get_segment_meta_bytes(kind, scope_hex, segment_id, u64::MAX)
            .await
            .with_context(|| {
                format!(
                    "fetch source segment sidecar kind={kind} scope={scope_hex} seg={segment_id}"
                )
            })
    }

    fn push_client(&self) -> Option<&Arc<fauna_client::NestClient>> {
        Some(&self.ws_client)
    }
}

/// Both files of one segment, as a [`SegmentSource`] hands them back.
///
/// Opaque to every backup arm: neither half is decoded on this path. They are
/// carried together because a `.dat` without its `.meta` is not a segment —
/// `FramedSegment::open` refuses it, since `record_order` and the per-record
/// floor metadata exist only in the sidecar (`message-segment-store.md`
/// § Segment file format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentPair {
    /// The `.dat` — standard CARv2.
    pub dat: Vec<u8>,
    /// The `.meta` sidecar — canonical dag-cbor.
    pub meta: Vec<u8>,
}

impl SegmentPair {
    /// Check both halves against what the source's own listing advertised —
    /// the in-transit verification the adoption arm runs
    /// (`crate::bootstrap_source`), shared here so every backup arm refuses a
    /// truncated, substituted or torn pair at the transport, where the fix is
    /// a re-fetch, instead of storing bytes nothing can reopen.
    pub fn verify_against(&self, advertised: &SegmentRef) -> Result<()> {
        verify_dat_hash(&hex::encode(blake3::hash(&self.dat).as_bytes()), advertised)?;
        verify_meta_against(&self.meta, advertised)
    }
}

/// The sidecar half of [`SegmentPair::verify_against`] on its own — for the
/// backfill door, which fetches the `.meta` alone.
pub fn verify_meta_against(meta: &[u8], advertised: &SegmentRef) -> Result<()> {
    verify_meta_hash(&hex::encode(blake3::hash(meta).as_bytes()), advertised)
}

/// The `.dat` check behind every verify above, over a hash already taken.
fn verify_dat_hash(dat_got: &str, advertised: &SegmentRef) -> Result<()> {
    if dat_got != advertised.blake3_hex {
        anyhow::bail!(
            "segment {}: the .dat served hashes to {dat_got}, but the source's own listing \
             advertised {}",
            advertised.segment_id,
            advertised.blake3_hex
        );
    }
    Ok(())
}

/// The `.meta` check behind every verify above, over a hash already taken.
fn verify_meta_hash(meta_got: &str, advertised: &SegmentRef) -> Result<()> {
    if meta_got != advertised.meta_blake3_hex {
        anyhow::bail!(
            "segment {}: the .meta served hashes to {meta_got}, but the source's own \
             listing advertised {}",
            advertised.segment_id,
            advertised.meta_blake3_hex
        );
    }
    Ok(())
}

/// Both files of one segment, **staged to disk** — [`SegmentPair`]'s form for
/// a caller that keeps the pair, which [`SegmentSource::stage_segment_pair`]
/// hands back.
///
/// Each half's BLAKE3 was taken as its bytes were written, so
/// [`Self::verify_against`] is the same in-transit check [`SegmentPair`] runs
/// without reading either file back. The directory is removed when this is
/// dropped; a crash leaves it for [`sweep_stale_staging`].
#[derive(Debug)]
pub struct StagedPair {
    dir: tempfile::TempDir,
    dat_blake3_hex: String,
    meta_blake3_hex: String,
    meta_len: u64,
}

impl StagedPair {
    const DAT: &'static str = "seg.dat";
    const META: &'static str = "seg.meta";

    /// Stage a pair already in memory — the default
    /// [`SegmentSource::stage_segment_pair`].
    pub async fn from_pair(pair: &SegmentPair, staging: &std::path::Path) -> Result<Self> {
        use fauna_account_store::segments::{SegmentHalf, SegmentSink};

        let mut writer = StagedPairWriter::create(staging).await?;
        writer.write(SegmentHalf::Dat, &pair.dat).await?;
        writer.write(SegmentHalf::Meta, &pair.meta).await?;
        writer
            .finish(
                hex::encode(blake3::hash(&pair.dat).as_bytes()),
                hex::encode(blake3::hash(&pair.meta).as_bytes()),
            )
            .await
    }

    /// This pair's own directory — a caller may stage what it derives from
    /// the pair (a sealed copy) beside it, and it goes when the pair does.
    pub fn dir(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// The staged `.dat`.
    pub fn dat_path(&self) -> std::path::PathBuf {
        self.dir.path().join(Self::DAT)
    }

    /// The staged `.meta` sidecar.
    pub fn meta_path(&self) -> std::path::PathBuf {
        self.dir.path().join(Self::META)
    }

    /// The sidecar's length in bytes.
    pub fn meta_len(&self) -> u64 {
        self.meta_len
    }

    /// [`SegmentPair::verify_against`] for the staged pair.
    pub fn verify_against(&self, advertised: &SegmentRef) -> Result<()> {
        verify_dat_hash(&self.dat_blake3_hex, advertised)?;
        verify_meta_hash(&self.meta_blake3_hex, advertised)
    }
}

/// The two open files a [`StagedPair`] is written through, a chunk at a time.
struct StagedPairWriter {
    dir: tempfile::TempDir,
    dat: tokio::fs::File,
    meta: tokio::fs::File,
    meta_len: u64,
}

impl StagedPairWriter {
    async fn create(staging: &std::path::Path) -> Result<Self> {
        tokio::fs::create_dir_all(staging)
            .await
            .with_context(|| format!("create segment staging {}", staging.display()))?;
        let dir = tempfile::Builder::new()
            .prefix("pair-")
            .tempdir_in(staging)
            .with_context(|| format!("stage a segment pair under {}", staging.display()))?;
        let dat = tokio::fs::File::create(dir.path().join(StagedPair::DAT)).await?;
        let meta = tokio::fs::File::create(dir.path().join(StagedPair::META)).await?;
        Ok(Self {
            dir,
            dat,
            meta,
            meta_len: 0,
        })
    }

    async fn finish(
        mut self,
        dat_blake3_hex: String,
        meta_blake3_hex: String,
    ) -> Result<StagedPair> {
        use tokio::io::AsyncWriteExt;
        self.dat.flush().await?;
        self.meta.flush().await?;
        Ok(StagedPair {
            dir: self.dir,
            dat_blake3_hex,
            meta_blake3_hex,
            meta_len: self.meta_len,
        })
    }
}

impl fauna_account_store::segments::SegmentSink for StagedPairWriter {
    async fn write(
        &mut self,
        half: fauna_account_store::segments::SegmentHalf,
        chunk: &[u8],
    ) -> Result<()> {
        use fauna_account_store::segments::SegmentHalf;
        use tokio::io::AsyncWriteExt;
        match half {
            SegmentHalf::Dat => self.dat.write_all(chunk).await?,
            SegmentHalf::Meta => {
                self.meta.write_all(chunk).await?;
                self.meta_len += chunk.len() as u64;
            }
        }
        Ok(())
    }
}

/// How old a leftover staging directory must be before a pass removes it.
/// Far longer than any one pair's transfer, so a pass never sweeps a pair a
/// concurrent pass is still writing; a Rust constant, never a knob.
pub const STALE_STAGING_SECS: u64 = 24 * 60 * 60;

/// Remove what a crashed pass left under `staging`: each entry whose
/// modification time is over [`STALE_STAGING_SECS`] old. A [`StagedPair`]
/// removes its own directory when dropped, so only an interrupted process
/// leaves one. Best effort — a leftover costs disk, never correctness.
pub async fn sweep_stale_staging(staging: &std::path::Path) {
    let Ok(mut entries) = tokio::fs::read_dir(staging).await else {
        return;
    };
    let cutoff = std::time::Duration::from_secs(STALE_STAGING_SECS);
    while let Ok(Some(entry)) = entries.next_entry().await {
        let stale = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > cutoff);
        if stale {
            let _ = tokio::fs::remove_dir_all(entry.path()).await;
        }
    }
}

/// One live path of a covered ordinary folder's head, as the mirror pull reads
/// it (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
/// The `path_hash` is the mirror's path key — uniform across sealed- and
/// plaintext-path source rows, so the local layout matches the destination
/// set's and supersede keying can never fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderHeadEntry {
    /// The source row's 32-byte `path_hash`.
    pub path_hash: Vec<u8>,
    /// The path's live `ChunkManifest` hash.
    pub manifest_hash: ContentHash,
    /// The source's declared size — [`crate::custodian_store::SourceFacts`]
    /// bookkeeping, never a charge.
    pub size_bytes: i64,
    /// The path's **sealed name**, carried through so a custodian-sourced
    /// re-seed can re-home the row into a live folder later.
    ///
    /// Why it must ride the mirror (`message-segment-store.md` § Client-device
    /// custodian → *Restore*): a covered-folder materialize is pure row
    /// re-homing from `(path_hash, path_sealed, manifest_hash)`, and live rows
    /// minted without a sealed name are refused outright — `record_change_core`
    /// answers `path_seal_required` for any set outside the three ratified
    /// plaintext-path classes, which a live folder set is not. `path_hash` alone
    /// is one-way, so a mirror that drops this can never reconstruct the name;
    /// the source nest's own coordinator has always passed it on this axis
    /// (`record_folder_custody`), and this is the custodian arm catching up.
    ///
    /// `None` for a plaintext-path source row (the public projection
    /// strips the seal). Not fatal: the mirror still holds the bytes,
    /// and only the *materialize* arm needs the name.
    pub path_sealed: Option<Vec<u8>>,
}

/// Byte-plane access for the covered-folder mirror pull: enumerate a folder's
/// live head, then fetch its manifests + chunks by hash **as-is** — the bytes
/// are the source's at-rest ciphertext and are never opened or re-sealed here
/// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage —
/// the client-device kind's destination-driven pull). Implemented by
/// [`SourceBinding`] over the wire, and by fakes in the pull's tests.
#[async_trait::async_trait]
pub trait FolderCorpusSource: Send + Sync {
    /// The live head (latest-per-path) of one covered folder on the source.
    async fn folder_head(&self, folder_id: i64) -> Result<Vec<FolderHeadEntry>>;
    /// A stored manifest's plaintext bytes, by hash.
    async fn manifest_bytes(&self, manifest_hash: &ContentHash) -> Result<Vec<u8>>;
    /// A stored chunk's body (already-sealed ciphertext), by store key.
    async fn chunk_bytes(&self, store_key: &ContentHash) -> Result<Vec<u8>>;
}

/// `WS-RPC` kind string for a folder's live-head file listing.
pub const SYNC_FILES_KIND: &str = "fauna.sync.files";

#[async_trait::async_trait]
impl FolderCorpusSource for SourceBinding {
    async fn folder_head(&self, folder_id: i64) -> Result<Vec<FolderHeadEntry>> {
        // Resolve the coverage row's stable `folder_id` to the folder's
        // addressing (name + name_hash) — `fauna.sync.files` is name-keyed.
        let listed: FoldersListReply = self
            .ws_client
            .request(
                KIND_FOLDERS_LIST,
                FoldersListRequest {
                    include_shared_with_me: None,
                    extra: Default::default(),
                },
            )
            .await
            .with_context(|| format!("{KIND_FOLDERS_LIST} (covered folder {folder_id})"))?;
        let summary = listed
            .folders
            .into_iter()
            .find(|f| f.id == folder_id)
            .ok_or_else(|| {
                anyhow::anyhow!("covered folder {folder_id} is not in the owner's folder list")
            })?;

        let files: SyncFilesReply = self
            .ws_client
            .request(
                SYNC_FILES_KIND,
                SyncFilesRequest {
                    folder: summary.name.clone(),
                    name_hash: summary.name_hash.clone(),
                    extra: Default::default(),
                },
            )
            .await
            .with_context(|| format!("{SYNC_FILES_KIND} (covered folder {folder_id})"))?;

        let mut out = Vec::with_capacity(files.files.len());
        for f in files.files {
            // The nest's `fauna.sync.files` stamps `path_hash` on every row.
            // A row without it is one the mirror cannot address — refuse
            // loudly (the pass retries) rather than silently thin the mirror.
            let Some(path_hash) = f.path_hash.as_ref().map(|h| h.to_vec()) else {
                anyhow::bail!("covered folder {folder_id} served a head row with no path_hash");
            };
            let digest: [u8; 32] = hex::decode(&f.manifest_hash)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("covered folder {folder_id}: malformed manifest hash")
                })?;
            out.push(FolderHeadEntry {
                path_hash,
                manifest_hash: ContentHash::from_digest_raw(digest),
                size_bytes: f.size_bytes,
                // Already on the listing — `SyncFile` ships `path_sealed`
                // beside `path_hash` precisely so a sealed-first consumer never
                // needs the plaintext column. No wire widening was required
                // here; the mirror simply was not reading it.
                path_sealed: f.path_sealed.as_ref().map(|s| s.to_vec()),
            });
        }
        Ok(out)
    }

    async fn manifest_bytes(&self, manifest_hash: &ContentHash) -> Result<Vec<u8>> {
        self.sync_client
            .download_manifest(manifest_hash)
            .await
            .with_context(|| format!("fetch manifest {}", hex::encode(manifest_hash.digest())))
    }

    async fn chunk_bytes(&self, store_key: &ContentHash) -> Result<Vec<u8>> {
        self.sync_client
            .download_chunk(store_key)
            .await
            .with_context(|| format!("fetch chunk {}", hex::encode(store_key.digest())))
    }
}

/// What one backup pass over a `(destination, kind)` tuple did — filled by
/// the nest arm's `NestBackupCoordinator::run_once_with_src_refs`
/// (`bins/fauna-nest/src/segment_backup.rs`).
#[derive(Debug, Default, Clone)]
pub struct RunReport {
    /// Segment IDs successfully uploaded in this pass — both halves of each.
    pub uploaded_segments: Vec<u32>,
    /// Segment IDs whose `segment_backup_state` row was dropped
    /// because the source no longer lists them (compacted-out path).
    pub dropped_segments: Vec<u32>,
    /// `true` iff the `manifest.mail` mirror was re-uploaded.
    pub manifest_uploaded: bool,
    /// What the same pass did for the kind's **placement journal**
    /// ([`SegmentFamily::Placement`]), in the same terms. `None` when the kind
    /// has no journal ([`SegmentFamily::serve_kind`]). The fields above are the
    /// content family's alone, so every figure derived from them (the status
    /// row's backlog, "last synced") stays a statement about content.
    pub placement: Option<Box<RunReport>>,
}

/// Identity a destination nest reports at enroll time, resolved by
/// [`resolve_destination`] before a `BackupDestination`
/// row is recorded (`docs/goal/behavior/backup-destinations.md` § State & data shape → Create).
///
/// `actor_pubkey` is the destination nest's stable 32-byte Ed25519 id (from
/// `fauna.nest.info`); it is stored as `BackupDestination::destination_actor_pubkey`
/// so a later edit can tell a URL change (same nest moved) from a different
/// nest. `domain` is the destination's handle domain — the default
/// `display_name` when the user leaves the name field blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDestination {
    pub actor_pubkey: [u8; 32],
    pub domain: String,
}

// ────────────────────────────────────────────────────────────────────
// Diff
// ────────────────────────────────────────────────────────────────────

/// Pure-function classification of source segments vs. local state.
/// Plan 5 spec § D2 step 3.
#[derive(Debug, Default, Clone)]
pub struct SegmentDiff {
    /// Source ref → reason to (re)upload the **pair**. Caller orders these
    /// (new + grown_closed) before grown_open.
    pub to_upload: Vec<SegmentRef>,
    /// Source refs whose `.dat` is already held unchanged but whose `.meta`
    /// sidecar was never pushed (`last_meta_size` is `None` — a row written
    /// before the corpus carried sidecars, 2026-08-29). The caller fetches and
    /// pushes the sidecar **alone**: the `.dat` bytes are right where they
    /// are, and re-moving them would make the one-time catch-up cost a full
    /// corpus re-upload on every destination and custodian in the fleet.
    ///
    /// Why this is a diff class and not a schema default: content addressing
    /// re-converges *bytes*; it cannot re-converge a path that was never
    /// pushed, and an "unchanged" row would hide the missing half forever —
    /// the same trap corpus gap #1 hit with `path_sealed`.
    pub to_backfill_meta: Vec<SegmentRef>,
    /// Local segment_ids the source no longer lists — drop the row.
    pub to_drop: Vec<u32>,
}

/// Diff a `fauna.segments.list` reply against the local sync-state
/// rows for one (destination, actor, kind) tuple.
///
/// Classifications per Plan 5 spec § D2 (updated in Plan 6 T5 to use
/// byte-size rather than blake3 for both open and closed segment change
/// detection, as the new `segment_backup_state` table no longer stores the
/// segment blake3):
/// * new           — segment_id not in local
/// * grown_open    — `is_open && size_bytes > local.last_byte_size`
/// * grown_closed  — `!is_open && size_bytes != local.last_byte_size`
/// * unchanged     — skip, unless the sidecar was never pushed → `to_backfill_meta`
/// * compacted_out — segment_id in local but not in src → `to_drop`
///
/// The sidecar needs no change detector of its own: a finalized
/// `.dat`/`.meta` pair is immutable (compaction mints a **new** segment id
/// and tombstones the inputs; in-segment tombstone counts live in the SQLite
/// mirror, not the sidecar), so every sidecar change comes with a `.dat`
/// change, and the pair rides one decision.
pub fn diff_segments(
    src: &[SegmentRef],
    local: &HashMap<u32, MailBackupSegmentState>,
) -> SegmentDiff {
    let mut new_or_closed: Vec<SegmentRef> = Vec::new();
    let mut grown_open: Vec<SegmentRef> = Vec::new();
    let mut to_backfill_meta: Vec<SegmentRef> = Vec::new();

    for src_ref in src {
        let local_row = local.get(&src_ref.segment_id);
        match local_row {
            None => {
                // New segment — never uploaded.
                new_or_closed.push(src_ref.clone());
            }
            Some(local_state) => {
                let changed = if src_ref.is_open {
                    if src_ref.size_bytes > local_state.last_byte_size {
                        grown_open.push(src_ref.clone());
                        true
                    } else {
                        false
                    }
                } else if src_ref.size_bytes != local_state.last_byte_size {
                    new_or_closed.push(src_ref.clone());
                    true
                } else {
                    false
                };
                // Unchanged `.dat`, but a `.dat`-only row: the sidecar is owed.
                // An open segment has no sidecar on disk until it finalizes,
                // and every source finalizes on read, so the ref's
                // `is_open == false` is what makes the sidecar fetchable.
                if !changed && !src_ref.is_open && local_state.last_meta_size.is_none() {
                    to_backfill_meta.push(src_ref.clone());
                }
            }
        }
    }

    // Order: (new + grown_closed) before grown_open. The spec asks for
    // this so the destination is consistent at every intermediate point
    // — a partially-uploaded open segment is fine on its own, but a
    // missing-from-destination closed segment behind an uploaded open
    // segment leaves a temporary gap visible to readers.
    new_or_closed.extend(grown_open);

    let src_ids: std::collections::HashSet<u32> = src.iter().map(|r| r.segment_id).collect();
    let to_drop: Vec<u32> = local
        .keys()
        .filter(|id| !src_ids.contains(id))
        .copied()
        .collect();

    SegmentDiff {
        to_upload: new_or_closed,
        to_backfill_meta,
        to_drop,
    }
}

// ────────────────────────────────────────────────────────────────────
// Destination path layout + the manifest mirror — MOVED to fauna-protocol
// ────────────────────────────────────────────────────────────────────

/// The destination path layout ([`segment_rel_path`] / [`segment_meta_rel_path`]
/// / [`manifest_rel_path`] / [`parse_segment_rel_path`], [`SegmentFamily`],
/// [`SegmentHalf`]), the reserved-set kind list ([`BACKUP_SET_KINDS`]) and the
/// at-rest [`LiveManifestMirror`] live in `fauna_protocol::segments` since
/// 2026-09-28 and are re-exported here so every arm keeps its import path.
///
/// They moved because the audit loop — the fifth party to the layout
/// contract, and since then the mirror's *reader* (`fauna_client_backup::audit`
/// anchors its inclusion population in the mirror) — is
/// wasm-clean shared Rust that this crate cannot be a dependency of: this crate
/// is native-only and itself depends on `fauna-client-backup`. `SegmentRef`,
/// which the mirror is a list of, already lived there.
pub use fauna_protocol::segments::{
    BACKUP_SET_KINDS, LiveManifestMirror, SegmentFamily, SegmentHalf, manifest_rel_path,
    parse_segment_rel_path, segment_meta_rel_path, segment_rel_path,
};

/// Is `tag` a placement journal's serve tag (`mail-placement`, …)?
///
/// The serve plane needs this one question answered in one place, because a
/// journal is the one thing on that plane that is **not** a sealed content
/// kind: it rests as floor plaintext (mailbox names, flags), so it is served
/// to the owner's own session and refused to every granted holder
/// (`segment-backup-protocol.md` § *Which kinds the two planes serve*). A
/// custody grant over the whole account admits any well-formed content scope
/// string, so that refusal cannot be left to the grant's own vocabulary.
pub fn is_placement_serve_kind(tag: &str) -> bool {
    BACKUP_SET_KINDS
        .iter()
        .any(|kind| SegmentFamily::Placement.serve_kind(kind) == Some(tag))
}

/// The reserved folder name a segment `kind` backs up into, or `None` for a
/// kind that has no cross-location backup surface.
///
/// **One derivation, now four consumers, so they cannot drift**: the nest's
/// destination-capability predicate (`CacheDb::is_pure_backup_destination`), the
/// federated backup-custody relay
/// (`federation_handlers::resolve_backup_custody_set`, which get-or-creates this
/// exact set for an authorized source nest), the nest coordinator's own pass,
/// and — since the re-seed ceremony — the **client device** naming the set it
/// records delivered custody into ([`crate::reseed`]). A mismatch between any two
/// would let custody land in a set the capability gates do not recognise as a
/// pure-backup destination, i.e. a destination that both serves and backs up.
///
/// It lives here, beside [`segment_rel_path`] and [`manifest_rel_path`], because
/// this module is already the shared leaf set every backup arm reuses verbatim —
/// and because a device arm cannot reach a nest binary's private module at all.
///
/// Mail, post, calendar and card use a fixed name scoped by `actor_id`; conv
/// encodes the channel in the **name** (`__conv/<channel_hex>`) because one nest
/// holds many channels. Note the scoping actor differs by consumer: on a
/// *source* nest a conv set is scoped by `channel_id`, while a *destination*
/// holds it under the enrolled member's own actor (that member's quota pays for
/// it — `message-segment-store.md` § Cross-location backup protocol, conv v1).
/// The name is the same on both, which is all this function owns.
pub fn reserved_backup_set_name(kind: &str, scope_id: &[u8; 32]) -> Option<String> {
    match kind {
        "mail" => Some("__mail".to_string()),
        "post" => Some("__post".to_string()),
        "calendar" => Some("__calendar".to_string()),
        "card" => Some("__card".to_string()),
        "conv" => Some(format!("__conv/{}", hex::encode(scope_id))),
        _ => None,
    }
}

/// Read a segment-axis reserved backup set name back into the `(kind, scope)` it
/// was derived from, or `None` if [`reserved_backup_set_name`] could not have
/// produced it.
///
/// The **folder** axis ([`folder_backup_set_name`]'s `__folder/<nest>/<id>`) is
/// deliberately not parsed here: that name embeds a *source nest id*, which a
/// writer must never be trusted to name — the federated relay re-derives it from
/// the handshake's verified `origin_nest_id`, and no owner-authed caller has an
/// equivalent to be checked against.
///
/// Written as parse-then-re-derive rather than as a second list of prefixes, so
/// the two can never disagree: whatever this returns, feeding it back through
/// [`reserved_backup_set_name`] reproduces the input name exactly, and a kind
/// added there without being added to [`BACKUP_SET_KINDS`] is simply not
/// recognised here rather than silently mis-parsed.
pub fn parse_reserved_backup_set_name(name: &str) -> Option<(&'static str, [u8; 32])> {
    let (kind, scope) = match name.strip_prefix("__conv/") {
        Some(scope_hex) => {
            let mut scope = [0u8; 32];
            hex::decode_to_slice(scope_hex, &mut scope).ok()?;
            ("conv", scope)
        }
        None => {
            let kind = BACKUP_SET_KINDS
                .iter()
                .copied()
                .find(|k| *k != "conv" && name == format!("__{k}"))?;
            (kind, [0u8; 32])
        }
    };
    (reserved_backup_set_name(kind, &scope)?.as_str() == name).then_some((kind, scope))
}

/// This device's synced replica of a covered folder, read as the backup
/// audit's covered-folder mirror plane population
/// (`fauna_client_backup::audit::FolderIndexSource`).
///
/// **What it reads.** The set's own per-folder state DB — the `fsid-<ref>.db`
/// every host of a folder binding writes (`FolderRef::state_db_path`; the
/// agent's resident engines, the in-process FFI host, the apple File Provider
/// host all key by it) — and from it every live entry's recorded head
/// (`sync_entries.manifest_hash`) under `hex(path_hash(path))`, which is
/// exactly the `(path, manifest_hash)` the source's coordinator writes each
/// mirror row under. The `remote_mtime` — the head change's `created_at` on
/// the source — dates the head, and the DB's `last_sync` stamp (a completed
/// transfer or a clean pull pass) dates the replica's last known consistency
/// with the source. Both are the audit's slack windows.
///
/// **What it refuses, and why `None` is the honest answer each time.** A set
/// name for another source nest (an owner's linked nest can hold the same
/// numeric `folders.id`, and `FolderRef::Local` is a row on *this* device's
/// bound nest); a folder this device holds no seat on (no DB under the state
/// dir — and a pure read: `SyncDb::open` creates on open, so the existence
/// check comes first, exactly as the FFI host's badge read does); a replica
/// never known consistent. Each leaves that set on the destination's own list
/// (`backup-destinations.md` § Ordinary-folder coverage → *Retention + audit*).
///
/// A second read-only connection while an engine holds the DB is what
/// `SyncDb::open`'s 5 s `busy_timeout` exists for.
#[derive(Debug, Clone)]
pub struct ReplicaFolderIndex {
    state_dir: std::path::PathBuf,
    source_nest_id: [u8; 32],
}

impl ReplicaFolderIndex {
    /// Over `state_dir` — the actor-scoped directory this device's folder
    /// bindings keep their `fsid-<ref>.db` files in — for replicas of folders
    /// on `source_nest_id`, the nest this device is bound to.
    pub fn new(state_dir: impl Into<std::path::PathBuf>, source_nest_id: [u8; 32]) -> Self {
        Self {
            state_dir: state_dir.into(),
            source_nest_id,
        }
    }
}

impl fauna_client_backup::audit::FolderIndexSource for ReplicaFolderIndex {
    fn folder_index(&self, folder_set: &str) -> Option<fauna_client_backup::audit::FolderIndex> {
        use fauna_client_backup::audit::{FolderIndex, FolderIndexEntry};

        let (nest, folder_id) = parse_folder_backup_set_name(folder_set)?;
        if nest != self.source_nest_id {
            return None;
        }
        let path =
            fauna_core::folder_keys::FolderRef::Local(folder_id).state_db_path(&self.state_dir);
        if !path.exists() {
            return None;
        }
        let db = match crate::db::SyncDb::open(&path) {
            Ok(db) => db,
            Err(e) => {
                tracing::warn!("backup audit: open replica of {folder_set}: {e}");
                return None;
            }
        };
        let consistent_at = db.transfer_backlog().ok()?.last_sync_at()?;
        let entries = db
            .list_all()
            .ok()?
            .into_iter()
            .filter(|e| !matches!(e.state, crate::db::SyncState::Deleted))
            .filter_map(|e| {
                let manifest = e.manifest_hash?;
                let rel = fauna_core::sync::normalize_rel_path(std::path::Path::new(&e.path));
                Some(FolderIndexEntry {
                    path_hash_hex: hex::encode(fauna_core::sync::path_hash(&rel)),
                    manifest_hash_hex: hex::encode(manifest.digest()),
                    recorded_at: e.remote_mtime,
                })
            })
            .collect();
        Some(FolderIndex {
            consistent_at,
            entries,
        })
    }
}

/// The folder index a native shell hands the backup audit
/// (`fauna_client_pair::native_backup_inclusion_source`): a
/// [`ReplicaFolderIndex`] when the shell knows both where this device's
/// folder replicas rest and which nest it is bound to, else the declared
/// absence `NoFolderIndex` — under which the covered-folder mirror plane keeps
/// hash-verified presence over the destination's own list
/// (`backup-destinations.md` § Ordinary-folder coverage → *Retention + audit*).
///
/// The pure half of [`bound_replica_folder_index`], split out so the rule
/// "either half missing is the declared absence, never a guess" is pinned
/// without a live connection.
pub fn replica_folder_index_or_absent(
    sync_state_dir: Option<std::path::PathBuf>,
    bound_source_nest: Option<[u8; 32]>,
) -> std::sync::Arc<dyn fauna_client_backup::audit::FolderIndexSource> {
    match (sync_state_dir, bound_source_nest) {
        (Some(dir), Some(nest)) => std::sync::Arc::new(ReplicaFolderIndex::new(dir, nest)),
        _ => std::sync::Arc::new(fauna_client_backup::audit::NoFolderIndex),
    }
}

/// [`replica_folder_index_or_absent`] over the nest `nest` is **bound** to —
/// read once per audit pass, never per set, through the one native door
/// (`fauna_client::trust::connection_bound_identity`: the login's pin, else a
/// possession proof over this connection; never the nest's own
/// `fauna.nest.info` claim, which a box answering with a sibling's id could
/// use to make this device's replica of the *sibling's* folder N witness for
/// its own). A failed read is logged and degrades to the declared absence:
/// the audit then keeps presence over the list, which is weaker but never a
/// false alarm.
///
/// Every native shell calls this — linux and tui over
/// [`local_agent_state_dir`], the UniFFI face over its engine host's state
/// dir (priority #2: one composition, not one per shell).
pub async fn bound_replica_folder_index(
    nest: &fauna_client::NestClient,
    sync_state_dir: Option<std::path::PathBuf>,
) -> std::sync::Arc<dyn fauna_client_backup::audit::FolderIndexSource> {
    let bound = match sync_state_dir {
        None => None,
        Some(_) => {
            let url = nest.nest_url();
            match fauna_client::trust::connection_bound_identity(nest, &url).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!("backup audit: bound identity of {url}: {e}");
                    None
                }
            }
        }
    };
    replica_folder_index_or_absent(sync_state_dir, bound)
}

/// The directory this machine's local sync agent keeps `actor_id_hex`'s
/// per-set state DBs (`fsid-<ref>.db`) in — the agent's `SyncPaths::base_dir`
/// under that actor's scope, resolved from the app side so a desktop shell
/// that runs no engine of its own (linux, tui) can read its replicas.
///
/// The base is the agent's own: the shared per-OS
/// [`crate::root::platform_state_base`] (on linux `$XDG_CONFIG_HOME/fauna/sync`,
/// the same dir linux's `sync::flat_sync_dir` names), or on windows the
/// harness's `--data-dir` override the spawner forwards
/// (`fauna_client_sync::agent_spawner::AGENT_DATA_DIR_ENV`, compiled out of a
/// production build exactly as the spawner's forward is — convention 15) — on
/// unix the agent inherits the launch's isolated `HOME`/`XDG_CONFIG_HOME`, so
/// the production derivation already agrees with it.
pub fn local_agent_state_dir(actor_id_hex: &str) -> std::path::PathBuf {
    #[cfg(all(windows, any(test, debug_assertions, feature = "e2e-agent")))]
    let base = std::env::var_os(fauna_client_sync::agent_spawner::AGENT_DATA_DIR_ENV)
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::root::platform_state_base);
    #[cfg(not(all(windows, any(test, debug_assertions, feature = "e2e-agent"))))]
    let base = crate::root::platform_state_base();
    crate::db::actor_state_dir_or_unresolved(&base, actor_id_hex)
}

/// Canonical hash of a source's `segments` list, used to short-circuit
/// the manifest-mirror upload when nothing changed. The hash drives a
/// boolean equality check only — it is not stored on the destination.
///
/// `pub` because every backup arm compares against the *same* stored value
/// in `segment_backup_manifest_state` — the source nest's in-process arm in
/// `bins/fauna-nest` today (`docs/goal/architecture/message-segment-store.md`
/// § Cross-location backup protocol). Two implementations of this hash would
/// mean one arm re-mirroring every pass against rows another wrote.
pub fn hash_segments_canonical(refs: &[SegmentRef]) -> [u8; 32] {
    // canonical dag-cbor gives a stable byte representation for the same Vec.
    // `canonical_encode` is `Sized`-bounded, so the `&[SegmentRef]` slice is
    // passed by reference (`&refs`); serde encodes `&&[T]` identically to `[T]`.
    match fauna_core::encoding::canonical_encode(&refs) {
        Ok(bytes) => *blake3::hash(&bytes).as_bytes(),
        // Encoding a Vec<SegmentRef> is infallible in practice; if it ever
        // does fail, log + fall back to a value that will force a re-upload on
        // the next pass (vs. silently matching).
        Err(e) => {
            tracing::warn!(error = %e, "hash_segments_canonical: encoding failed; using fallback hash");
            [0u8; 32]
        }
    }
}
// ────────────────────────────────────────────────────────────────────
// Enroll-time destination resolution
// ────────────────────────────────────────────────────────────────────

/// Enroll-time identity resolution for a candidate backup destination
/// (`docs/goal/behavior/backup-destinations.md` § State & data shape → Create, steps 1–2).
///
/// Opens an authenticated WS-RPC connection to `destination_url` as the
/// owner's **stable cross-nest actor** (`owner_secret` → the same Ed25519
/// identity the owner uses everywhere). `NestClient::connect` performs the
/// `fauna.auth.handshake`, so a successful connect *is* the reachability +
/// authorization proof the spec calls for: for any nest the owner
/// administers the handshake succeeds; against a nest the owner cannot
/// authenticate to it fails and no destination is recorded. The destination's
/// stable 32-byte id is the one this connection is **bound** to — the host's
/// pin, else a possession proof over it
/// (`fauna_client::trust::connection_bound_identity`) — and `fauna.nest.info`
/// (an anonymous-discovery kind that also answers on an authed connection —
/// `bins/fauna-nest/src/routes.rs`) supplies the handle domain plus a claim
/// that must agree with the proof
/// (`fauna_client_backup::trust::proven_destination_id`), so every later
/// connection to the destination is held to a proven id, never a claim.
///
/// Returns the resolved identity; the caller builds the `BackupDestination`
/// row and persists it through the `fauna.state.backup` door (the single atomic
/// decision point — no destination-side state is mutated here, so a crash
/// before that write leaves nothing to clean up).
///
/// A free function: enroll needs nothing but the owner's identity and the
/// candidate URL (it outlived the deleted client upload coordinator, whose
/// associated function it used to be).
///
/// Verify-only wrapper over [`resolve_destination_connected`] — drops
/// the authenticated connection immediately. Used by edit's URL-change
/// re-verification, which must not have the grant-registration side effect
/// (below).
pub async fn resolve_destination(
    owner_secret: [u8; 32],
    destination_url: &str,
) -> Result<ResolvedDestination> {
    resolve_destination_connected(owner_secret, destination_url)
        .await
        .map(|(resolved, _client)| resolved)
}

/// [`resolve_destination`], but returns the still-open authenticated
/// connection alongside the resolved identity — for the **add** flow, which
/// reuses it to register the revocable nest-writer grant at the destination
/// (`fauna.backup.writer_grant.register`, a USER-class kind that needs an
/// authed session; `NestClient::connect` already minted one to prove
/// reachability/authorization, so this avoids a second connect+handshake).
/// The grant registration itself lives in `fauna_client_config::
/// enroll_backup_destination`, which takes this connection as its
/// `destination: D` parameter — this fn only resolves + hands back the
/// connection; it registers nothing.
pub async fn resolve_destination_connected(
    owner_secret: [u8; 32],
    destination_url: &str,
) -> Result<(ResolvedDestination, Arc<fauna_client::NestClient>)> {
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};

    let keypair = ActorKeypair::from_secret(owner_secret);
    let client = fauna_client::NestClient::new(destination_url.to_string(), keypair);
    client.connect().await.with_context(|| {
        format!("connect to backup destination {destination_url} (handshake / authorization)")
    })?;

    let bound = fauna_client::trust::connection_bound_identity(&*client, destination_url)
        .await
        .map_err(|e| anyhow::anyhow!("backup destination {destination_url} identity: {e}"))?;

    let reply: NestInfoReply = client
        .request("fauna.nest.info", NestInfoRequest::default())
        .await
        .with_context(|| format!("fauna.nest.info on backup destination {destination_url}"))?;

    let actor_pubkey =
        fauna_client_backup::trust::proven_destination_id(destination_url, bound, &reply.nest_id)
            .map_err(anyhow::Error::msg)?;

    Ok((
        ResolvedDestination {
            actor_pubkey,
            domain: reply.domain,
        },
        client,
    ))
}

/// [`resolve_destination_connected`] then
/// [`fauna_client_config::enroll_backup_destination`] in one call — the
/// sequence every native app's own "add destination" command runs
/// (`docs/goal/behavior/backup-destinations.md` § State & data shape →
/// Create step 1: blank `requested_name` falls back to the resolved domain
/// inside the enroll call). Edit's URL-change re-verification stays on
/// [`resolve_destination`] alone — it must not repeat the grant/register
/// side effects. `source_nest_id` is `nest`'s identity as its connection
/// proved it (`fauna_client_pair::resolve_this_nest_id`) — the writer the
/// destination authorizes, never the nest's own claim, and the box whose
/// `fauna.state.backup` list the row lands in (`store`).
///
/// Gated on `engine-lifecycle`/`preference-store`, whichever pulls in the
/// optional `fauna-client-config` dep this needs — both already reach every
/// caller (linux + tui enable `engine-lifecycle`).
#[cfg(any(feature = "engine-lifecycle", feature = "preference-store"))]
pub async fn resolve_and_enroll_destination<R: fauna_protocol::RpcRequester>(
    nest: R,
    store: &dyn fauna_client_config::BackupStateStore,
    owner_secret: [u8; 32],
    destination_url: String,
    requested_name: String,
    source_nest_id: [u8; 32],
) -> Result<Vec<fauna_core::data::BackupDestination>, String>
where
    R::Error: fauna_protocol::RpcErrorClass,
{
    let (resolved, destination) = resolve_destination_connected(owner_secret, &destination_url)
        .await
        .map_err(|e| e.to_string())?;
    fauna_client_config::enroll_backup_destination(
        nest,
        destination,
        store,
        owner_secret,
        fauna_client_config::ResolvedDestination {
            destination_id: uuid::Uuid::new_v4().to_string(),
            destination_nest_url: destination_url,
            destination_actor_pubkey: resolved.actor_pubkey,
            domain: resolved.domain,
            requested_name,
        },
        source_nest_id,
    )
    .await
    .map_err(|e| e.to_string())
}

/// The destination-edit ceremony every native app's edit dialog drives: read
/// the bound box's list, re-verify identity only when the url points at a
/// **different** nest (`docs/goal/behavior/backup-destinations.md` § Edit — a
/// same-nest rename must still work with the destination offline), then write
/// through [`fauna_client_config::mutate_backup`] with
/// [`fauna_client_config::edit_backup_destination`]. tui and linux
/// each independently assembled this exact sequence — linux's own doc
/// comment already pointed at "tui's twin" as the reason the same-nest
/// check's staleness is safe, but the shared home was never built, mirroring
/// [`resolve_and_enroll_destination`]'s own gap before it was closed.
///
/// The same-nest check is deliberately validated against the initial read,
/// not inside the write below: it can only be wrong if a concurrent device
/// re-pointed the very row being edited, and that write is itself gated by
/// this same check.
///
/// Returns the box's post-edit [`fauna_core::backup_state::BackupState`] — the
/// caller renders `state.backup.destinations`.
///
/// Gated the same as [`resolve_and_enroll_destination`] — see its own doc
/// comment for why `engine-lifecycle`/`preference-store` already cover
/// every caller.
#[cfg(any(feature = "engine-lifecycle", feature = "preference-store"))]
pub async fn edit_destination(
    store: &dyn fauna_client_config::BackupStateStore,
    source_nest: [u8; 32],
    owner_secret: [u8; 32],
    destination_id: String,
    destination_nest_url: String,
    display_name: String,
) -> Result<fauna_core::backup_state::BackupState, String> {
    let state = store
        .backup_state(source_nest)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(stored) = state
        .backup
        .destinations
        .iter()
        .find(|d| d.destination_id == destination_id)
        .cloned()
        && stored.destination_nest_url != destination_nest_url
    {
        let resolved = resolve_destination(owner_secret, &destination_nest_url)
            .await
            .map_err(|e| e.to_string())?;
        if resolved.actor_pubkey != stored.destination_actor_pubkey {
            return Err(
                fauna_i18n::strings::backups::BACKUP_DESTINATION_EDIT_DIFFERENT_NEST.to_string(),
            );
        }
    }

    let display_name = if display_name.is_empty() {
        None
    } else {
        Some(display_name)
    };
    fauna_client_config::mutate_backup(store, source_nest, |st| {
        fauna_client_config::edit_backup_destination(
            st,
            &destination_id,
            display_name,
            destination_nest_url,
        )
    })
    .await
    .map(|(state, _)| state)
    .map_err(|e| e.to_string())
}

/// Subscribe to the two pushes that mean "this owner's segment set moved" and
/// pulse `push_pulse` on each relevant one; returns the pumps' join handles.
///
/// Written once for every driver over the same segment plane — today
/// [`crate::custodian_pull::CustodianPull`]'s pull loop (the deleted client
/// upload loop was the other) — so "the client is woken by a push within
/// [`PUSH_DEBOUNCE`]" (`backup-restore.md` § Background Tasks) has exactly one
/// implementation. Two copies of a subscribe-and-filter loop is where the
/// *filter* drifts, and a custodian that woke on another actor's push would
/// pull on every message the nest received for anyone.
///
/// `(None, None)` when the source has no push client (the in-process nest arm,
/// which observes its own writes) — that caller runs on its periodic timer
/// alone. `label` prefixes the lag warnings so two drivers in one process are
/// distinguishable in a log.
///
/// The pumps capture only the scope id + the notifier: no `SyncDb` handle and no
/// store handle escapes the driver task, which is what keeps a `!Sync`
/// `rusqlite::Connection` on one thread.
pub(crate) fn spawn_push_pumps(
    source: &dyn SegmentSource,
    scope_id: [u8; 32],
    kinds: &'static [&'static str],
    push_pulse: Arc<Notify>,
    label: &'static str,
) -> (Option<JoinHandle<()>>, Option<JoinHandle<()>>) {
    let Some(ws) = source.push_client() else {
        return (None, None);
    };
    let mut seg_changed = ws.subscribe_kind(SEGMENTS_CHANGED_KIND);
    let mut mail_received = ws.subscribe_kind(MAIL_RECEIVED_KIND);

    let pump_seg = {
        let push_pulse = Arc::clone(&push_pulse);
        tokio::spawn(async move {
            loop {
                match seg_changed.recv().await {
                    Ok(fauna_protocol::PushEvent::SegmentsChanged(payload)) => {
                        if push_matches_actor(&payload, &scope_id, kinds) {
                            push_pulse.notify_one();
                        }
                    }
                    Ok(_) => {} // wrong kind filtered out by subscribe_kind
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("{label}: segments.changed subscriber lagged by {n}");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };

    let pump_mail = tokio::spawn(async move {
        loop {
            match mail_received.recv().await {
                Ok(_event) => {
                    // The `fauna.mail.received` payload isn't typed in
                    // fauna-protocol yet (it lives in fauna-mail). Any push of
                    // this kind for this actor is enough to kick a run — the
                    // driver ignores the body and re-runs its diff. The actor
                    // scope is enforced at the source nest's subscription layer
                    // (we only see pushes targeted at this actor).
                    push_pulse.notify_one();
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("{label}: mail.received subscriber lagged by {n}");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    (Some(pump_seg), Some(pump_mail))
}

fn push_matches_actor(
    payload: &SegmentsChangedPayload,
    scope_id: &[u8; 32],
    kinds: &[&str],
) -> bool {
    let payload_actor = match hex::decode(&payload.actor_id) {
        Ok(b) => b,
        Err(_) => return false,
    };
    if payload_actor.as_slice() != scope_id.as_slice() {
        return false;
    }
    kinds.iter().any(|k| *k == payload.kind)
}

// ────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::folder_backup_set_name;
    use std::collections::BTreeMap;

    /// The folder-axis parser is the inverse of the derivation over the ids the
    /// derivation is ever *called* with. Parse-then-re-derive is what makes that
    /// true rather than hoped-for: any spelling that would re-derive to a
    /// *different* string is refused, so a name this accepts is always one the
    /// nest itself would mint.
    #[test]
    fn the_folder_set_name_parser_is_the_derivations_inverse() {
        let nest = [0x5Au8; 32];
        for folder_id in [1i64, 7, 4_294_967_296, i64::MAX] {
            let name = folder_backup_set_name(&nest, folder_id);
            assert_eq!(
                parse_folder_backup_set_name(&name),
                Some((nest, folder_id)),
                "{name} must round-trip"
            );
        }
    }

    /// The replica reader projects a folder's state DB into the audit's index
    /// shape — every live head under `hex(path_hash)`, dated by the source's
    /// `created_at`, the replica dated by its clean pass — and answers `None`
    /// for every case it cannot vouch for: a folder this device holds no seat
    /// on (without minting a DB for it), another source nest's set, and a
    /// replica never known consistent.
    #[test]
    fn the_replica_reader_projects_live_heads_and_declares_what_it_cannot_vouch_for() {
        use fauna_client_backup::audit::FolderIndexSource;
        use fauna_core::folder_keys::FolderRef;

        let dir = tempfile::tempdir().unwrap();
        let nest = [0x5Au8; 32];
        let manifest = ContentHash::of_raw(b"a head's manifest");
        let db = crate::db::SyncDb::open(FolderRef::Local(7).state_db_path(dir.path())).unwrap();
        let put = |path: &str, manifest: Option<ContentHash>, remote_mtime: i64| {
            db.upsert_entry(
                path,
                None,
                None,
                manifest,
                crate::db::SyncState::Synced,
                0,
                remote_mtime,
                1,
                1,
                None,
            )
            .unwrap();
        };
        put("docs/a.txt", Some(manifest), 1_700_000_000);
        put("gone.txt", Some(manifest), 1_700_000_001);
        db.delete_entry("gone.txt").unwrap();
        put("unrecorded.txt", None, 1_700_000_002);

        let reader = ReplicaFolderIndex::new(dir.path(), nest);
        let set = folder_backup_set_name(&nest, 7);
        assert_eq!(
            reader.folder_index(&set),
            None,
            "a replica never known consistent with the source cannot witness"
        );

        db.mark_clean_pass_if_drained().unwrap();
        let index = reader
            .folder_index(&set)
            .expect("a clean-passed replica witnesses");
        assert!(index.consistent_at > 0);
        assert_eq!(
            index.entries,
            vec![fauna_client_backup::audit::FolderIndexEntry {
                path_hash_hex: hex::encode(fauna_core::sync::path_hash("docs/a.txt")),
                manifest_hash_hex: hex::encode(manifest.digest()),
                recorded_at: 1_700_000_000,
            }],
            "one live recorded head; the tombstone and the head-less row are not custody"
        );

        assert_eq!(
            reader.folder_index(&folder_backup_set_name(&[0x77u8; 32], 7)),
            None,
            "another source nest's folder 7 is not this replica"
        );
        assert_eq!(reader.folder_index(&folder_backup_set_name(&nest, 8)), None);
        assert!(
            !FolderRef::Local(8).state_db_path(dir.path()).exists(),
            "a pure read: asking about an unseated folder must not mint its DB"
        );
    }

    /// The shells' composition: a replica reader only when BOTH the state dir
    /// and the bound nest are known; either half missing is the declared
    /// absence — even over a clean-passed replica that would otherwise witness.
    #[test]
    fn a_shell_hands_in_the_replica_only_when_it_knows_both_dir_and_bound_nest() {
        use fauna_core::folder_keys::FolderRef;

        let dir = tempfile::tempdir().unwrap();
        let nest = [0x5Au8; 32];
        let db = crate::db::SyncDb::open(FolderRef::Local(3).state_db_path(dir.path())).unwrap();
        db.upsert_entry(
            "a.txt",
            None,
            None,
            Some(ContentHash::of_raw(b"head")),
            crate::db::SyncState::Synced,
            0,
            1_700_000_000,
            1,
            1,
            None,
        )
        .unwrap();
        db.mark_clean_pass_if_drained().unwrap();
        let set = folder_backup_set_name(&nest, 3);

        let both = replica_folder_index_or_absent(Some(dir.path().to_path_buf()), Some(nest));
        assert_eq!(
            both.folder_index(&set).map(|i| i.entries.len()),
            Some(1),
            "dir + bound nest → the replica witnesses"
        );
        for (d, n) in [
            (None, Some(nest)),
            (Some(dir.path().to_path_buf()), None),
            (None, None),
        ] {
            assert_eq!(
                replica_folder_index_or_absent(d.clone(), n).folder_index(&set),
                None,
                "a missing half ({d:?}, {n:?}) is the declared absence"
            );
        }
    }

    /// The agent-side state dir resolves under the shared per-OS base and the
    /// actor's own scope — the same dir the agent's `SyncPaths::base_dir`
    /// names once scoped.
    #[test]
    fn the_local_agent_state_dir_is_the_actor_scope_under_the_platform_base() {
        let actor = "ab".repeat(32);
        assert_eq!(
            local_agent_state_dir(&actor),
            crate::root::platform_state_base().join(&actor)
        );
    }

    /// Everything that is not a folder-mirror name — including the *segment*
    /// axis, whose own parser owns it, and near-misses whose laxer spelling
    /// would re-derive differently.
    #[test]
    fn the_folder_set_name_parser_refuses_everything_else() {
        let hex32 = "5a".repeat(32);
        for name in [
            "__mail",
            "__conv/00",
            "__index",
            "__config",
            "inbox",
            "__folder/",
            "__folder/tooshort/1",
            // A sign-prefixed or zero-padded id re-derives to a different
            // string than it was written as; a non-numeric one never parses.
            &format!("__folder/{hex32}/notanumber"),
            &format!("__folder/{hex32}/+1"),
            &format!("__folder/{hex32}/007"),
            // ⚠ These two DO re-derive perfectly — the derivation is total and
            // renders them happily — so only the rowid floor rejects them. A
            // `folders.id` is a SQLite rowid and starts at 1, so neither names a
            // folder that could exist, and admitting them would let an
            // owner-authed caller mint reserved-shaped junk sets.
            &format!("__folder/{hex32}/-1"),
            &format!("__folder/{hex32}/0"),
            // A third path segment is not part of the derivation.
            &format!("__folder/{hex32}/1/extra"),
        ] {
            assert_eq!(
                parse_folder_backup_set_name(name),
                None,
                "{name} must not parse as a folder mirror set"
            );
        }
    }

    /// The two axes stay disjoint: neither parser answers for the other's
    /// names. That disjointness is what lets the owner-authed door admit both
    /// without widening either one's meaning.
    #[test]
    fn the_two_reserved_axes_do_not_overlap() {
        let folder = folder_backup_set_name(&[0x11u8; 32], 3);
        assert!(parse_reserved_backup_set_name(&folder).is_none());
        assert!(parse_folder_backup_set_name(&folder).is_some());

        for segment in ["__mail", "__post", "__calendar", "__card"] {
            assert!(parse_reserved_backup_set_name(segment).is_some());
            assert!(parse_folder_backup_set_name(segment).is_none());
        }
    }

    fn make_ref(seg_id: u32, blake_byte: u8, size: u64, is_open: bool) -> SegmentRef {
        SegmentRef {
            segment_id: seg_id,
            blake3_hex: hex::encode([blake_byte; 32]),
            bucket: "2026-05".to_string(),
            record_count: 1,
            tombstone_count: 0,
            size_bytes: size,
            created_at_secs: 1700000000,
            is_open,
            ..Default::default()
        }
    }

    /// A local row as every pass since 2026-08-29 writes it: the pair pushed.
    fn make_local(chunk_count: u64, byte_size: u64) -> MailBackupSegmentState {
        MailBackupSegmentState {
            last_chunk_count: chunk_count,
            last_byte_size: byte_size,
            last_synced_at: 1,
            last_meta_size: Some(64),
        }
    }

    /// A local row as a torn pair leaves it: `.dat` only.
    fn make_local_dat_only(chunk_count: u64, byte_size: u64) -> MailBackupSegmentState {
        MailBackupSegmentState {
            last_meta_size: None,
            ..make_local(chunk_count, byte_size)
        }
    }

    // ── the sidecar path + its shared parser (2026-08-29) ────────────────────

    /// The parser is the inverse of both formatters over every id the
    /// formatters are called with — and refuses every spelling they never emit.
    #[test]
    fn the_segment_path_parser_is_the_two_formatters_inverse() {
        let scope = "ab".repeat(32);
        for id in [0u32, 1, 7, 99_999_999, u32::MAX] {
            assert_eq!(
                parse_segment_rel_path(&scope, &segment_rel_path(&scope, id)),
                Some((id, SegmentHalf::Dat))
            );
            assert_eq!(
                parse_segment_rel_path(&scope, &segment_meta_rel_path(&scope, id)),
                Some((id, SegmentHalf::Meta))
            );
        }
        let other_scope = "cd".repeat(32);
        for path in [
            manifest_rel_path(&scope, "mail"),
            segment_rel_path(&other_scope, 1),
            segment_meta_rel_path(&other_scope, 1),
            format!("{scope}/seg-7.dat"),
            format!("{scope}/seg-00000007.DAT"),
            format!("{scope}/seg-00000007.meta.bak"),
            format!("{scope}/seg-00000007"),
            format!("__folder/{scope}/1/deadbeef"),
            String::new(),
        ] {
            assert_eq!(
                parse_segment_rel_path(&scope, &path),
                None,
                "{path:?} must not parse as one of this scope's segment files"
            );
        }
    }

    /// The sidecar path is the `.dat` path's sibling in the same set — one
    /// more path per segment, nothing else about the layout moves.
    #[test]
    fn the_sidecar_path_is_the_dats_sibling() {
        let scope = "11".repeat(32);
        assert_eq!(
            segment_rel_path(&scope, 7),
            format!("{scope}/seg-00000007.dat")
        );
        assert_eq!(
            segment_meta_rel_path(&scope, 7),
            format!("{scope}/seg-00000007.meta")
        );
    }

    // ── the two path families of one backed-up kind (2026-09-26) ─────────────

    /// The content family IS the layout every held corpus already uses: the
    /// family type must reproduce it byte for byte, or a store filled before
    /// the journal joined the corpus would stop being addressable.
    #[test]
    fn the_content_family_is_the_layout_every_held_corpus_already_uses() {
        let scope = "11".repeat(32);
        let f = SegmentFamily::Content;
        assert_eq!(f.dat_path(&scope, 7), segment_rel_path(&scope, 7));
        assert_eq!(f.meta_path(&scope, 7), segment_meta_rel_path(&scope, 7));
        assert_eq!(
            f.mirror_path(&scope, "mail"),
            manifest_rel_path(&scope, "mail")
        );
        assert_eq!(
            f.parse(&scope, &segment_rel_path(&scope, 7)),
            Some((7, SegmentHalf::Dat))
        );
    }

    /// The journal's paths sit under their own infix in the SAME set, spelled
    /// exactly as the goal doc states them.
    #[test]
    fn the_placement_family_rests_under_its_own_infix() {
        let scope = "11".repeat(32);
        let f = SegmentFamily::Placement;
        assert_eq!(
            f.dat_path(&scope, 7),
            format!("{scope}/placement/seg-00000007.dat")
        );
        assert_eq!(
            f.meta_path(&scope, 7),
            format!("{scope}/placement/seg-00000007.meta")
        );
        assert_eq!(
            f.mirror_path(&scope, "mail"),
            format!("{scope}/placement/manifest.mail")
        );
    }

    /// **The property the infix exists for.** A client-device custodian's store
    /// is one flat path namespace and both families number their segments from
    /// 1, so no path of one family may ever parse as the other's — in either
    /// direction, for either half, or for either mirror.
    #[test]
    fn neither_family_ever_parses_the_others_paths() {
        let scope = "ab".repeat(32);
        let (content, placement) = (SegmentFamily::Content, SegmentFamily::Placement);
        for id in [0u32, 1, 7, 99_999_999, u32::MAX] {
            for (family, other) in [(content, placement), (placement, content)] {
                assert_eq!(
                    family.parse(&scope, &family.dat_path(&scope, id)),
                    Some((id, SegmentHalf::Dat))
                );
                assert_eq!(
                    family.parse(&scope, &family.meta_path(&scope, id)),
                    Some((id, SegmentHalf::Meta))
                );
                for path in [
                    other.dat_path(&scope, id),
                    other.meta_path(&scope, id),
                    other.mirror_path(&scope, "mail"),
                    family.mirror_path(&scope, "mail"),
                ] {
                    assert_eq!(
                        family.parse(&scope, &path),
                        None,
                        "{path:?} must not parse as a {family:?} segment"
                    );
                }
            }
        }
        // The two families never share a path, so a flat store never collides.
        assert_ne!(content.dat_path(&scope, 1), placement.dat_path(&scope, 1));
        assert_ne!(
            content.mirror_path(&scope, "mail"),
            placement.mirror_path(&scope, "mail")
        );
        // And the placement parser keeps the content parser's strictness.
        for path in [
            format!("{scope}/placement/seg-7.dat"),
            format!("{scope}/placement/seg-00000007.DAT"),
            format!("{scope}/placement//seg-00000007.dat"),
            format!("{scope}/placements/seg-00000007.dat"),
            format!("{}/placement/seg-00000007.dat", "cd".repeat(32)),
        ] {
            assert_eq!(placement.parse(&scope, &path), None, "{path:?}");
        }
    }

    /// The tag a family is listed and fetched under. Content is the kind
    /// itself; the journal's is the journal's own on-disk scope kind, which for
    /// calendar is NOT derivable by suffixing the manifest label.
    #[test]
    fn each_family_names_the_tag_the_serve_plane_answers_it_under() {
        let (content, placement) = (SegmentFamily::Content, SegmentFamily::Placement);
        for kind in BACKUP_SET_KINDS {
            assert_eq!(content.serve_kind(kind), Some(*kind));
        }
        assert_eq!(placement.serve_kind("mail"), Some("mail-placement"));
        assert_eq!(placement.serve_kind("calendar"), Some("calendar-placement"));
        assert_eq!(placement.serve_kind("card"), Some("card-placement"));
        // Kinds with no placement layer have no journal to move.
        assert_eq!(placement.serve_kind("conv"), None);
        assert_eq!(placement.serve_kind("post"), None);
        // A tag nothing derives is refused by both, never echoed back.
        assert_eq!(content.serve_kind("mail-placement"), None);
        assert_eq!(placement.serve_kind("not-a-kind"), None);
    }

    /// A serve tag reads back into exactly the (family, kind) that derived it,
    /// for every tag either family emits — and nothing else reads back at all.
    #[test]
    fn a_serve_tag_reads_back_into_the_family_and_kind_that_derived_it() {
        for kind in BACKUP_SET_KINDS {
            for family in SegmentFamily::PASS_ORDER {
                if let Some(tag) = family.serve_kind(kind) {
                    assert_eq!(
                        SegmentFamily::from_serve_kind(tag),
                        Some((family, *kind)),
                        "{tag}"
                    );
                }
            }
        }
        for tag in ["", "placement", "conv-placement", "mail-placement-x"] {
            assert_eq!(SegmentFamily::from_serve_kind(tag), None, "{tag:?}");
        }
    }

    /// The one question the serve plane asks before admitting a granted
    /// holder: every journal tag answers yes, every content kind answers no.
    #[test]
    fn a_journal_tag_is_recognised_and_a_content_kind_is_not() {
        for tag in ["mail-placement", "calendar-placement", "card-placement"] {
            assert!(is_placement_serve_kind(tag), "{tag}");
        }
        for kind in BACKUP_SET_KINDS {
            assert!(!is_placement_serve_kind(kind), "{kind}");
        }
        for tag in [
            "",
            "placement",
            "conv-placement",
            "post-placement",
            "mail-placement ",
        ] {
            assert!(!is_placement_serve_kind(tag), "{tag:?}");
        }
    }

    /// Content first, journal second — the pass order every mover walks.
    #[test]
    fn a_pass_walks_the_content_family_before_the_journal() {
        assert_eq!(
            SegmentFamily::PASS_ORDER,
            [SegmentFamily::Content, SegmentFamily::Placement]
        );
    }

    // ── the pair's in-transit check ──────────────────────────────────────────

    fn advertised(dat: &[u8], meta: &[u8]) -> SegmentRef {
        SegmentRef {
            segment_id: 3,
            blake3_hex: hex::encode(blake3::hash(dat).as_bytes()),
            meta_blake3_hex: hex::encode(blake3::hash(meta).as_bytes()),
            ..Default::default()
        }
    }

    #[test]
    fn a_pair_matching_the_listing_verifies() {
        let pair = SegmentPair {
            dat: b"dat".to_vec(),
            meta: b"meta".to_vec(),
        };
        pair.verify_against(&advertised(b"dat", b"meta")).unwrap();
    }

    #[test]
    fn a_pair_contradicting_the_listing_is_refused_half_by_half() {
        let pair = SegmentPair {
            dat: b"dat".to_vec(),
            meta: b"meta".to_vec(),
        };
        let dat_err = pair
            .verify_against(&advertised(b"other dat", b"meta"))
            .unwrap_err()
            .to_string();
        assert!(dat_err.contains(".dat"), "{dat_err}");
        let meta_err = pair
            .verify_against(&advertised(b"dat", b"other meta"))
            .unwrap_err()
            .to_string();
        assert!(meta_err.contains(".meta"), "{meta_err}");
    }

    // ── the sidecar backfill class ───────────────────────────────────────────

    /// An unchanged `.dat` whose row records no sidecar is a **backfill**, not
    /// an upload and not "unchanged": the bytes are already there, the
    /// sidecar is not, and only this class says so. A row written by a
    /// post-widening pass is genuinely unchanged.
    #[test]
    fn diff_classifies_a_dat_only_row_as_a_sidecar_backfill() {
        let src = vec![
            make_ref(1, 0xAA, 500, false), // held, pair complete → unchanged
            make_ref(2, 0xBB, 500, false), // held, `.dat` only → backfill
            make_ref(3, 0xCC, 900, false), // held `.dat` only but GROWN → upload the pair
            make_ref(4, 0xDD, 100, false), // new → upload the pair
        ];
        let mut local = HashMap::new();
        local.insert(1, make_local(5, 500));
        local.insert(2, make_local_dat_only(5, 500));
        local.insert(3, make_local_dat_only(5, 500));

        let diff = diff_segments(&src, &local);

        let backfill: Vec<u32> = diff.to_backfill_meta.iter().map(|r| r.segment_id).collect();
        assert_eq!(backfill, vec![2]);
        let upload: Vec<u32> = diff.to_upload.iter().map(|r| r.segment_id).collect();
        assert_eq!(
            upload,
            vec![3, 4],
            "a grown segment re-uploads the whole pair; it is not ALSO a backfill"
        );
        assert!(diff.to_drop.is_empty());
    }

    /// An open segment has no sidecar on disk yet, so it can never be owed
    /// one — the class fires only for finalized refs.
    #[test]
    fn an_open_unchanged_segment_is_not_a_sidecar_backfill() {
        let src = vec![make_ref(1, 0xAA, 500, true)];
        let mut local = HashMap::new();
        local.insert(1, make_local_dat_only(5, 500));
        let diff = diff_segments(&src, &local);
        assert!(diff.to_upload.is_empty());
        assert!(diff.to_backfill_meta.is_empty());
    }

    #[test]
    fn diff_classifies_new_grown_open_grown_closed_unchanged_compacted_out() {
        // Plan 6 T5: diff now uses byte-size for both open and closed segments
        // (blake3 no longer stored). Classification:
        // * seg 1: closed, same size (500) → unchanged
        // * seg 2: closed, different size (src=900 vs local=800) → grown_closed
        // * seg 3: open, src size (400) > local (300) → grown_open
        // * seg 4: open, src size (700) == local (700) → unchanged
        // * seg 5: not in local → new
        // * seg 99: in local, not in src → compacted_out
        let src = vec![
            make_ref(1, 0xAA, 500, false), // unchanged (same size as local)
            make_ref(2, 0xCC, 900, false), // grown_closed (size differs from local 800)
            make_ref(3, 0xDD, 400, true),  // grown_open (size grew from 300)
            make_ref(4, 0xEE, 700, true),  // unchanged (open, same size)
            make_ref(5, 0xFF, 100, false), // new (not in local)
        ];

        let mut local = HashMap::new();
        local.insert(1, make_local(5, 500)); // unchanged (same size)
        local.insert(2, make_local(8, 800)); // grown_closed (size 800 ≠ src 900)
        local.insert(3, make_local(3, 300)); // grown_open (size grew to 400)
        local.insert(4, make_local(7, 700)); // unchanged (open, same size)
        local.insert(99, make_local(1, 9)); // compacted_out

        let diff = diff_segments(&src, &local);

        let uploaded_ids: Vec<u32> = diff.to_upload.iter().map(|r| r.segment_id).collect();
        // Order: new (5) + grown_closed (2) come before grown_open (3).
        // Within new+grown_closed, order is preserved from `src`.
        // src order: 2 (grown_closed), 5 (new). Then grown_open 3.
        assert_eq!(uploaded_ids, vec![2, 5, 3]);

        assert_eq!(diff.to_drop, vec![99]);
    }

    #[test]
    fn diff_empty_local_treats_all_as_new() {
        let src = vec![make_ref(1, 0xAA, 100, false), make_ref(2, 0xBB, 200, true)];
        let local = HashMap::new();
        let diff = diff_segments(&src, &local);
        let uploaded_ids: Vec<u32> = diff.to_upload.iter().map(|r| r.segment_id).collect();
        assert_eq!(uploaded_ids, vec![1, 2]);
        assert!(diff.to_drop.is_empty());
    }

    #[test]
    fn diff_empty_src_drops_all_local() {
        let src: Vec<SegmentRef> = vec![];
        let mut local = HashMap::new();
        local.insert(1, make_local(5, 100));
        local.insert(7, make_local(3, 200));
        let diff = diff_segments(&src, &local);
        assert!(diff.to_upload.is_empty());
        let mut dropped = diff.to_drop;
        dropped.sort();
        assert_eq!(dropped, vec![1, 7]);
    }

    #[test]
    fn diff_open_segment_grown_in_size_classified_as_grown_open() {
        let src = vec![make_ref(5, 0xAA, 1000, true)];
        let mut local = HashMap::new();
        // Open segment; local has smaller size → grown.
        local.insert(5, make_local(4, 500));
        let diff = diff_segments(&src, &local);
        let uploaded_ids: Vec<u32> = diff.to_upload.iter().map(|r| r.segment_id).collect();
        assert_eq!(uploaded_ids, vec![5]);
    }

    #[test]
    fn diff_closed_segment_same_size_unchanged() {
        let src = vec![make_ref(3, 0xCC, 999, false)];
        let mut local = HashMap::new();
        // Closed segment; same size → unchanged (Plan 6 T5: size is the signal,
        // not blake3 — the table no longer stores blake3).
        local.insert(3, make_local(6, 999));
        let diff = diff_segments(&src, &local);
        assert!(diff.to_upload.is_empty());
        assert!(diff.to_drop.is_empty());
    }

    #[test]
    fn diff_closed_segment_different_size_classified_as_changed() {
        let src = vec![make_ref(3, 0xCC, 999, false)];
        let mut local = HashMap::new();
        // Closed segment; different size → treat as changed and re-upload.
        local.insert(3, make_local(6, 500));
        let diff = diff_segments(&src, &local);
        let uploaded_ids: Vec<u32> = diff.to_upload.iter().map(|r| r.segment_id).collect();
        assert_eq!(uploaded_ids, vec![3]);
        assert!(diff.to_drop.is_empty());
    }

    #[test]
    fn hash_segments_canonical_is_deterministic() {
        let refs = vec![make_ref(1, 0xAA, 100, false), make_ref(2, 0xBB, 200, true)];
        let a = hash_segments_canonical(&refs);
        let b = hash_segments_canonical(&refs);
        assert_eq!(a, b);

        // Different segment list → different hash.
        let refs2 = vec![make_ref(1, 0xAA, 100, false), make_ref(2, 0xBB, 201, true)];
        let c = hash_segments_canonical(&refs2);
        assert_ne!(a, c);
    }

    /// The ledger's generation is the source's SAVED counter, not the greatest
    /// live id plus one: a source whose top segment
    /// compacted to nothing lists a lower maximum while its counter stands, and
    /// the ledger must carry the counter or a device pinning it would alarm on
    /// that honest nest.
    #[test]
    fn the_ledger_carries_the_saved_counter_over_the_derived_one() {
        // Live ids 1 and 2; the source's counter is 5 (ids 3 and 4 were retired).
        let listing = SegmentListing {
            segments: vec![make_ref(1, 0x11, 100, false), make_ref(2, 0x22, 200, false)],
            next_segment_id: 5,
        };
        assert_eq!(live_manifest_mirror(&listing).next_segment_id_seen, 5);

        let torn = SegmentListing {
            next_segment_id: 1,
            ..listing.clone()
        };
        assert_eq!(
            live_manifest_mirror(&torn).next_segment_id_seen,
            3,
            "a counter read across a write never lands below max(live) + 1"
        );

        let empty = SegmentListing::default();
        assert_eq!(live_manifest_mirror(&empty).next_segment_id_seen, 0);
    }

    #[test]
    fn live_manifest_mirror_round_trips() {
        let mirror = LiveManifestMirror {
            next_segment_id_seen: 42,
            live: vec![make_ref(1, 0x11, 100, false), make_ref(2, 0x22, 200, true)],
            extra: BTreeMap::new(),
        };
        let bytes = mirror.to_bytes().unwrap();
        let decoded = LiveManifestMirror::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, mirror);
    }

    /// Discriminating red→green: the at-rest `LiveManifestMirror` blob must be
    /// canonical dag-cbor (`serialization.md:29` — every at-rest byte through
    /// one canonical encoder), not ciborium. ciborium emits struct fields in
    /// declaration order (`next_segment_id_seen` before `live`), which is NOT
    /// canonical (length-first: `live`(4) < `next_segment_id_seen`(20)), so its
    /// bytes fail strict canonical decode; canonical dag-cbor passes. (The
    /// `SegmentRef::extra` `serde(flatten)` path is proven separately by
    /// fauna-cbor's `serde_flatten_btreemap_round_trips_canonically`.)
    #[test]
    fn live_manifest_mirror_at_rest_is_canonical_dagcbor() {
        let mirror = LiveManifestMirror {
            next_segment_id_seen: 2,
            live: vec![make_ref(1, 0xAB, 100, false)],
            extra: BTreeMap::new(),
        };
        let bytes = mirror.to_bytes().unwrap();
        fauna_core::encoding::canonical_decode::<LiveManifestMirror>(&bytes)
            .expect("LiveManifestMirror at-rest blob must be canonical dag-cbor");
    }

    // ── The nest-side source arm (2026-07-23 redesign, slice 1) ──────────────

    /// A stand-in for the source nest's own local-file segment source: it lists
    /// and serves segment pairs with **no WS-RPC and no HTTP**, and holds no
    /// push client. Shaped exactly like the arm the nest supplies in slice 2.
    struct LocalTestSource {
        segments: Vec<(SegmentRef, SegmentPair)>,
    }

    #[async_trait::async_trait]
    impl SegmentSource for LocalTestSource {
        fn source_id(&self) -> &str {
            "local-nest"
        }

        async fn list_segments(&self, _kind: &str, _scope_hex: &str) -> Result<SegmentListing> {
            Ok(SegmentListing {
                segments: self.segments.iter().map(|(r, _)| r.clone()).collect(),
                ..Default::default()
            })
        }

        async fn segment_pair(
            &self,
            _kind: &str,
            _scope_hex: &str,
            segment_id: u32,
        ) -> Result<SegmentPair> {
            self.segments
                .iter()
                .find(|(r, _)| r.segment_id == segment_id)
                .map(|(_, p)| p.clone())
                .ok_or_else(|| anyhow::anyhow!("no local segment {segment_id}"))
        }

        async fn segment_meta_bytes(
            &self,
            kind: &str,
            scope_hex: &str,
            segment_id: u32,
        ) -> Result<Vec<u8>> {
            Ok(self.segment_pair(kind, scope_hex, segment_id).await?.meta)
        }
    }

    /// The capability the [`SegmentSource`] seam exists to unlock: a backup
    /// pass can be driven by a source that reaches no network at all — the
    /// in-process nest arm. This pins that the seam is real: listing and
    /// pair-fetching both dispatch through the trait, and `push_client()`
    /// defaulting to `None` is a supported source shape rather than a panic.
    #[tokio::test]
    async fn a_local_source_with_no_push_client_serves_the_seam() {
        let pair = SegmentPair {
            dat: b"seg1".to_vec(),
            meta: b"meta1".to_vec(),
        };
        let source = Arc::new(LocalTestSource {
            segments: vec![(make_ref(1, 0xAA, 4, false), pair.clone())],
        });
        assert!(
            source.push_client().is_none(),
            "the nest arm subscribes to no pushes — it observes its own writes",
        );

        // Reached through the trait object, exactly as the coordinator does.
        let via_trait: Arc<dyn SegmentSource> = source;
        let listed = via_trait
            .list_segments("mail", "deadbeef")
            .await
            .unwrap()
            .segments;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].segment_id, 1);
        assert_eq!(
            via_trait.segment_pair("mail", "deadbeef", 1).await.unwrap(),
            pair,
        );
        assert_eq!(
            via_trait
                .segment_meta_bytes("mail", "deadbeef", 1)
                .await
                .unwrap(),
            b"meta1",
        );
        assert!(
            via_trait
                .segment_pair("mail", "deadbeef", 99)
                .await
                .is_err(),
            "a missing segment must fail loudly, never silently upload nothing",
        );

        assert_eq!(via_trait.source_id(), "local-nest");
    }

    #[test]
    fn push_matches_actor_correctly() {
        let actor = [0xAB; 32];
        let payload_match = SegmentsChangedPayload {
            kind: "mail".to_string(),
            actor_id: hex::encode(actor),
            segment_id: 7,
            change: fauna_protocol::push_events::SegmentChange::Finalized,
            extra: BTreeMap::new(),
        };
        assert!(push_matches_actor(&payload_match, &actor, &["mail"]));

        // Wrong actor.
        let payload_wrong_actor = SegmentsChangedPayload {
            actor_id: hex::encode([0xCD; 32]),
            ..payload_match.clone()
        };
        assert!(!push_matches_actor(&payload_wrong_actor, &actor, &["mail"]));

        // Wrong kind.
        let payload_wrong_kind = SegmentsChangedPayload {
            kind: "conv".to_string(),
            ..payload_match.clone()
        };
        assert!(!push_matches_actor(&payload_wrong_kind, &actor, &["mail"]));

        // Malformed hex.
        let payload_malformed = SegmentsChangedPayload {
            actor_id: "not hex".to_string(),
            ..payload_match
        };
        assert!(!push_matches_actor(&payload_malformed, &actor, &["mail"]));
    }
}
