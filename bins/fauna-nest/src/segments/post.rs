//! Post-kind nest-side segment coordination.
//!
//! Sibling of [`super::mail`] / [`super::conv`] for social-feed posts. The
//! file/manifest state machine is the same kind-agnostic
//! `fauna_segment_store::SegmentManager` (`AppState.post_segments`, registered
//! with `kind = "post"`); what remains nest-specific is the coordination
//! between the `SegmentManager` and the `segment_records` SQLite mirror — these
//! free functions over `(&SegmentManager, &CacheDb, …)`.
//!
//! ### Why posts are simpler than mail/conv (no seq, no envelope, no lock)
//!
//! Mail and conv are **range-read-by-`seq`** (`read_after_seq`): a per-scope
//! monotonic counter orders the stream, so allocation + append + mirror-insert
//! must be atomic under a per-scope lock, and each record rides a typed
//! envelope. **Posts are point-read-by-CID.** Every post reader holds the
//! `post_id` (which is `blake3(body)`); the cross-actor feed *list* runs off the
//! SQLite projection (`content` + `content_meta`/`content_fts`/…), never the
//! segments (`feed.md` § The read model). So:
//!
//! * **No `seq`.** A post is found by deriving its record CID from the `post_id`
//!   a reader already holds — `Cid::of_dag_cbor(body)`, whose digest equals
//!   `blake3(body) = post_id`. The mirror's `record_cid` index resolves
//!   `(scope, segment)` for that CID. The mirror row leaves `seq` + every
//!   mail-floor column NULL.
//! * **No per-record envelope.** The segment block **is the raw canonical post
//!   body** — the same bytes today in `content.payload` / the blob store. A
//!   wrapper would break the `post_id == record_cid.digest()` identity (and
//!   force a redundant `post_id → record_cid` mirror column) for no gain; the
//!   body is already self-describing (a versioned canonical `Post`). The seal is
//!   unchanged (public posts are plaintext signed bytes; restricted posts carry
//!   the preview-body envelope — the `encrypted_ref` blob + `KeyBlob` live in
//!   subscription storage, out of this path).
//! * **No per-scope lock.** Distinct posts are distinct CID-keyed records; the
//!   `SegmentManager`'s per-scope mutex already serializes the file write, and
//!   each mirror insert is independent (no read-modify-write). Lock order:
//!   segment-scope-mutex → CacheDb conn-mutex.
//!
//! The `scope_id` is the post's **author** actor id → `__post/<author_hex>/`.

use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_cbor::Cid;
use fauna_segment_store::{CompactionPlan, ManagerError, SegmentManager};
use serde::{Deserialize, Serialize};

use super::mail::BucketCompactionOutcome;
use super::records_db::{self, NewPointReadSegmentRecord};
use crate::db::CacheDb;

/// Kind tag — the on-disk dir is `__post/<author_hex>/`.
pub const KIND: &str = "post";

/// Floor metadata for one post record. Posts have no `seq` and no auth/sender
/// floor (those are mail-only); the only floor datum is `received_at` (the
/// post's `created_at`), kept so compaction can recover the calendar bucket
/// when rebuilding the mirror. Encoded canonical dag-cbor, opaque to the
/// segment store (`serialization.md` — every on-disk byte goes through one
/// canonical encoder).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostFloorMetadata {
    pub format_version: u8,
    pub received_at: i64,
}

impl PostFloorMetadata {
    pub fn new(received_at: i64) -> Self {
        Self {
            format_version: 1,
            received_at,
        }
    }
}

fn serialize_floor(floor: &PostFloorMetadata) -> Result<Vec<u8>> {
    fauna_cbor::encode_canonical(floor).map_err(|e| anyhow::anyhow!("serialize post floor: {e}"))
}

pub(crate) fn parse_floor(bytes: &[u8]) -> Result<PostFloorMetadata> {
    fauna_cbor::decode_strict(bytes).map_err(|e| anyhow::anyhow!("parse post floor: {e}"))
}

/// Result of [`append_body`].
#[derive(Debug, Clone, Copy)]
pub struct PostAppendOutcome {
    /// The record's CID — `Cid::of_dag_cbor(body)` (digest == `post_id`).
    pub record_cid: Cid,
    /// Segment id the record landed in (live tail of the manifest).
    pub seg_id: u32,
    /// `Some(closed_seg_id)` if this append rotated a previously-open segment
    /// closed; `None` otherwise.
    pub finalized: Option<u32>,
}

/// Append one post body to its author's `__post` segment store.
///
/// The segment block IS the raw `body`; the record CID is
/// `Cid::of_dag_cbor(body)` (its digest equals the `post_id`). `received_at` is
/// the post's `created_at` in epoch **milliseconds** (the bucket is computed
/// from epoch seconds, same convention as `mail::append` / `conv::append`).
pub async fn append_body(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    author: &[u8; 32],
    body: &[u8],
    received_at: i64,
) -> Result<PostAppendOutcome> {
    let cid = Cid::of_dag_cbor(body);
    let floor = PostFloorMetadata::new(received_at);
    let floor_bytes = serialize_floor(&floor)?;
    let bucket = fauna_segment_store::bucket_for(received_at / 1000);

    let outcome = mgr
        .append_record_with_bucket(author, cid, body, &floor_bytes, &bucket)
        .await
        .map_err(|e| anyhow::anyhow!("append_record_with_bucket (post): {e}"))?;

    cache_db
        .segment_records_insert_post(author, outcome.segment_id, &cid, &bucket, received_at)
        .await?;

    Ok(PostAppendOutcome {
        record_cid: cid,
        seg_id: outcome.segment_id,
        finalized: outcome.finalized,
    })
}

/// Idempotently ensure `body` is present in `author`'s `__post` segment store.
///
/// Appends only if the author's segment does not already hold the body's record
/// CID. The mirror `record_cid` index is **non-UNIQUE**, so this author-scoped
/// skip-if-present guard is load-bearing: it makes re-ingest (crash recovery, a
/// federation re-forward) idempotent — it never double-appends the same body.
/// `created_at_ms` is the post's `created_at` in epoch milliseconds (the
/// [`append_body`] convention).
pub async fn ensure_in_segment(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    author: &[u8; 32],
    body: &[u8],
    created_at_ms: i64,
) -> Result<()> {
    let cid = Cid::of_dag_cbor(body);
    let already = matches!(
        cache_db
            .segment_records_lookup_scope_and_segment(KIND, &cid)
            .await?,
        Some((found_author, _)) if &found_author == author
    );
    if !already {
        append_body(mgr, cache_db, author, body, created_at_ms).await?;
    }
    Ok(())
}

/// Read one post body by its `post_id` (the `blake3(body)` digest a reader
/// already holds). Derives the record CID, resolves `(author, segment)` via the
/// mirror's `record_cid` index, and reads the raw body from the segment.
///
/// Returns `None` when the post is unknown to this nest OR the mirror points at
/// a record absent from the segment file (divergence — warned-and-skipped,
/// mirroring `conv::read_one`). The caller need not know the author.
pub async fn read_body_by_post_id(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    post_id: &[u8; 32],
) -> Result<Option<Vec<u8>>> {
    let cid = Cid::from_digest_dag_cbor(*post_id);
    read_body_by_cid(mgr, cache_db, &cid).await
}

/// Read one post body by its record CID. Sibling of [`read_body_by_post_id`]
/// for callers that already hold the CID.
pub async fn read_body_by_cid(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    cid: &Cid,
) -> Result<Option<Vec<u8>>> {
    let Some((author, segment_id)) = cache_db
        .segment_records_lookup_scope_and_segment(KIND, cid)
        .await?
    else {
        return Ok(None);
    };
    match mgr.read_envelope_bytes(&author, segment_id, cid).await {
        Ok(body) => Ok(Some(body)),
        Err(ManagerError::RecordNotFound { .. }) => {
            tracing::warn!(
                author = ?author,
                record_cid = ?cid,
                segment_id,
                "segment_records mirror diverged from segment file (post read)"
            );
            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!("read_envelope_bytes (post): {e}")),
    }
}

/// [`read_body_by_cid`], tombstone included — the sibling that resolves
/// scope through [`lookup_scope_by_post_id_including_tombstoned`]'s CID twin
/// (`segment_records_lookup_scope_and_segment_including_tombstoned`) instead
/// of the live-only lookup.
///
/// Needed wherever a record's mirror row may already be tombstoned but its
/// bytes are not yet compacted away: the legal-takedown-of-an-already-
/// deleted-post arm (`moderation.md` § Legal takedown → *Posts*, "deleted,
/// then the order arrives") — the author's own delete tombstones the mirror
/// the instant it lands, so [`read_body_by_cid`] answers `None` for exactly
/// the record this path exists to find. A **reader**, not a serve path:
/// classified in `every_flag_blind_post_body_read_is_partitioned` alongside
/// its live-only sibling.
///
/// Also answers `None`, rather than erroring, when the segment FILE itself
/// can no longer be opened at all (`SegmentStoreError::is_unfinalized_crash_tail`
/// — physical reclaim after compaction closes the same code path a crash
/// before `finalize()` would; `segment_records` rows are tombstoned, never
/// deleted, so the mirror lookup above stays `Some` long after the bytes are
/// gone). [`read_body_by_cid`] deliberately does not extend this same slack
/// to a LIVE record — an unreadable segment file behind a live mirror row is
/// a genuine anomaly worth surfacing, never an expected steady state.
pub async fn read_body_by_cid_including_tombstoned(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    cid: &Cid,
) -> Result<Option<Vec<u8>>> {
    let Some((author, segment_id)) = cache_db
        .segment_records_lookup_scope_and_segment_including_tombstoned(KIND, cid)
        .await?
    else {
        return Ok(None);
    };
    match mgr.read_envelope_bytes(&author, segment_id, cid).await {
        Ok(body) => Ok(Some(body)),
        Err(ManagerError::RecordNotFound { .. }) => {
            tracing::warn!(
                author = ?author,
                record_cid = ?cid,
                segment_id,
                "segment_records mirror diverged from segment file (post read, tombstone-inclusive)"
            );
            Ok(None)
        }
        Err(ManagerError::Segment(e)) if e.is_unfinalized_crash_tail() => {
            tracing::warn!(
                author = ?author,
                record_cid = ?cid,
                segment_id,
                "tombstoned record's segment file is gone (physically reclaimed) — \
                 tombstone-inclusive post read"
            );
            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!(
            "read_envelope_bytes (post, tombstone-inclusive): {e}"
        )),
    }
}

/// [`read_body_by_cid_including_tombstoned`] keyed by `post_id` — the form
/// the legal-takedown handler holds. Sibling of [`read_body_by_post_id`].
pub async fn read_body_by_post_id_including_tombstoned(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    post_id: &[u8; 32],
) -> Result<Option<Vec<u8>>> {
    let cid = Cid::from_digest_dag_cbor(*post_id);
    read_body_by_cid_including_tombstoned(mgr, cache_db, &cid).await
}

/// Load a post body for serving: try the `__post` segment store first, then
/// fall back to the inline `content.payload`.
///
/// This is the read entry point shared by every post-body reader (single-post
/// `get`, references, AP outbox, video manifests, export, moderation,
/// web-content rendering). The fallback serves the one post shape that rests
/// inline: a body [`store_post`] cannot decode has no author, so no segment
/// scope, and is kept in `content.payload`. Every decodable post rests in its
/// author's `__post` segment — bridged ones included, the expiring nostr sweep
/// through [`store_post_with_expiry`]. A segment-stored post has an empty `content.payload`; the segment lookup
/// serves it and the fallback returns `None` (the segment miss already proved
/// the body absent — never an empty body). No post body rests in the blob
/// store, so the row's `blob_hash` is never consulted.
///
/// Returns `None` when the post is unknown to this nest (no live segment record
/// and no inline body).
pub async fn load_post_body(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    post_id: &[u8; 32],
) -> Result<Option<Vec<u8>>> {
    // Segment-first: the post-cutover authoritative body store.
    if let Some(body) = read_body_by_post_id(mgr, cache_db, post_id).await? {
        return Ok(Some(body));
    }
    // Fallback: an inline `content.payload` body.
    let Some((payload, _blob_hash)) = cache_db.get_post(post_id).await? else {
        return Ok(None);
    };
    // A segment-stored post has an empty payload — the body lives in the
    // segment, so a miss above means it is genuinely absent; resolve to None
    // rather than an empty body.
    if payload.is_empty() {
        return Ok(None);
    }
    Ok(Some(payload))
}

/// Store a post body at-rest: append it to the author's `__post` segment store
/// (the post-cutover authoritative body store) and write the `content` row +
/// feed-index projection with an EMPTY `content.payload` (the body is in the
/// segment, no longer inline). The post-cutover write entry point shared by
/// every post writer (`ingest_post_core`, federation forward, the ActivityPub
/// inbox handlers, the worker read-fallback cache).
///
/// `source` is `Some(protocol)` for bridge-/federation-ingested posts (the
/// `put_post_with_source` indexing path, no FTS — preserved as-was) and `None`
/// for native signed posts (the `put_post` indexing path, FTS-indexed).
///
/// Idempotent by `post_id`: re-ingesting the same post (crash recovery, a
/// federation re-forward) appends to the segment only if the author's segment
/// does not already hold this record CID, and the `content`-row write is an
/// upsert on the content-addressed id. The segment append runs (and fully
/// releases its locks) before the projection write, so a crash between the two
/// leaves the body readable-by-id and re-ingest converges.
///
/// Undecodable bodies (no extractable author → no segment scope) fall back to
/// the inline store ([`CacheDb::put_post`] / `put_post_with_source`),
/// served via [`load_post_body`]'s fallback. A degenerate, rare path.
pub async fn store_post(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    post_id: &[u8; 32],
    body: &[u8],
    source: Option<&str>,
) -> Result<()> {
    store_post_with_expiry(mgr, cache_db, post_id, body, source, None).await
}

/// [`store_post`] carrying a source-side expiry onto `content.expires_at` — the
/// door for a bridged post whose foreign protocol states one (a NIP-40
/// `expiration` on the nostr sweep, `nostr::inbound_lifecycle`), which that
/// plane's expiry sweep later finds. The body rests in the author's `__post`
/// segment like every other decodable post; only the projection row carries
/// the expiry.
///
/// An expiry is a bridged plane's fact, so it needs a `source`, and it needs a
/// decodable body: the inline fallback has no expiring writer, and storing the
/// post without its expiry would leave a row no sweep retracts. Either shape
/// is refused before any write.
pub async fn store_post_with_expiry(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    post_id: &[u8; 32],
    body: &[u8],
    source: Option<&str>,
    expires_at: Option<i64>,
) -> Result<()> {
    if expires_at.is_some() && source.is_none() {
        anyhow::bail!("an expiring post needs a bridge source");
    }
    // One id, one plane. `db::content::insert_content` refuses a cross-plane
    // replace at the row; this refuses before the segment append as well, so an
    // id another plane's row holds (a row keyed by the digest a signed wire of
    // its post names) never gains a body in the author's segment.
    if let Some(schema) = cache_db
        .content_schema(post_id)
        .await?
        .filter(|s| !crate::db::content::is_post_schema(s))
    {
        anyhow::bail!(
            "post id {} already holds a `{schema}` row",
            hex::encode(post_id)
        );
    }

    let Some(post) = crate::db::posts::decode_stored_post(body) else {
        if expires_at.is_some() {
            anyhow::bail!(
                "post {} carries an expiry but does not decode",
                hex::encode(post_id)
            );
        }
        // Undecodable → no author → keep inline (degenerate fallback).
        match source {
            Some(s) => cache_db.put_post_with_source(post_id, body, s).await?,
            None => cache_db.put_post(post_id, body, None).await?,
        }
        return Ok(());
    };

    let author = post.author.0;
    let created_at_ms = (post.created_at.0 / 1000) as i64;

    ensure_in_segment(mgr, cache_db, &author, body, created_at_ms).await?;

    // Projection only — content row + FTS/meta/links, EMPTY payload.
    match source {
        Some(s) => {
            cache_db
                .put_post_with_source_index_only(post_id, body, s, expires_at)
                .await?
        }
        None => cache_db.put_post_index_only(post_id, body).await?,
    }
    Ok(())
}

/// Resolve a post's author scope + segment id from its 32-byte digest — the
/// delete path's ownership lookup (`feed.md` § State & data shape → *Post
/// deletion*). `None` = the post has no segment record (an inline row, or gone).
pub async fn lookup_scope_by_post_id(
    cache_db: &CacheDb,
    post_id: &[u8; 32],
) -> Result<Option<([u8; 32], u32)>> {
    let cid = Cid::from_digest_dag_cbor(*post_id);
    cache_db
        .segment_records_lookup_scope_and_segment(KIND, &cid)
        .await
}

/// [`lookup_scope_by_post_id`], tombstone included — which segment still holds
/// the record's bytes, whether or not the mirror row was tombstoned.
///
/// The legal-takedown segment-pair withhold asks this, and must: a post its
/// author deleted while taken down is tombstoned the moment the delete lands,
/// but [`tombstone_by_cid`] is mirror-only and compaction keeps the bytes, so
/// the live-only lookup answers `None` for exactly the record the withhold
/// exists to keep out of the archive.
pub async fn lookup_scope_by_post_id_including_tombstoned(
    cache_db: &CacheDb,
    post_id: &[u8; 32],
) -> Result<Option<([u8; 32], u32)>> {
    let cid = Cid::from_digest_dag_cbor(*post_id);
    cache_db
        .segment_records_lookup_scope_and_segment_including_tombstoned(KIND, &cid)
        .await
}

/// Tombstone one post record (mirror-only — the segment file is reclaimed later
/// by compaction). Idempotent; returns the number of rows newly tombstoned
/// (0 if already gone / unknown, 1 if newly tombstoned).
pub async fn tombstone_by_cid(
    cache_db: &CacheDb,
    author: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
) -> Result<usize> {
    cache_db
        .segment_records_mark_tombstoned(author, KIND, segment_id, cid)
        .await
}

/// Compact one bucket of an author's post segments per the supplied
/// `CompactionPlan`. Post sibling of [`super::conv::compact_bucket`] — the
/// kind-agnostic mechanics are literally shared now
/// ([`super::compact_bucket_with`]); the only post-specific bits are the floor
/// decode ([`PostFloorMetadata`], no `seq`) when rebuilding the mirror and the
/// named `apply_post_compaction_tx` wrapper.
pub async fn compact_bucket(
    mgr: &SegmentManager,
    cache_db: &Arc<CacheDb>,
    author: &[u8; 32],
    plan: &CompactionPlan,
) -> Result<BucketCompactionOutcome> {
    super::compact_bucket_with(
        mgr,
        cache_db,
        author,
        KIND,
        plan,
        |rid, floor_bytes| {
            let floor = parse_floor(floor_bytes)?;
            // Post floors carry epoch **milliseconds** — hence the `/ 1000`
            // before `bucket_for`, which calendar/card must NOT copy.
            Ok(NewPointReadSegmentRecord {
                record_cid: rid,
                bucket: fauna_segment_store::bucket_for(floor.received_at / 1000),
                received_at: floor.received_at,
            })
        },
        |tx, new_segment, new_records| {
            records_db::apply_post_compaction_tx(tx, author, &plan.inputs, new_segment, new_records)
                .context("apply_post_compaction_tx")
        },
    )
    .await
}

/// Restore an author's posts into this nest's `segment_records` mirror + feed-
/// index `content` projection from a pinned snapshot `Manifest` whose
/// `live_segments` are already on disk (pulled via fauna-sync before this runs).
///
/// **Additive / idempotent — never DELETEs the author's existing post records.**
/// This is the post-specific divergence from `restore_conv`/`restore_mail`
/// (which DELETE-then-rebuild, a user-chosen *revert*), for two principled
/// reasons:
///
/// 1. **No-data-loss.** Posts are public, user-authored content; a restore
///    RECOVERS the snapshot's posts (the cross-location-backup → fresh/recovery-
///    nest use case) and must never silently drop the author's *newer* posts
///    (which a DELETE-all-then-rebuild-from-pinned-segments would). Re-asserting
///    is no-data-loss by construction and crash-/rerun-safe.
/// 2. **Separate read-model projection.** Unlike conv (whose `segment_records`
///    mirror *is* the read model `fetch` scans), posts carry a separate cross-
///    actor `content` feed-index projection that `query_feed` scans. So the
///    rebuild re-asserts BOTH: the mirror (so `load_post_body` resolves the body
///    by CID) AND the projection (so `query_feed` lists the post).
///
/// Each re-assertion is idempotent: the mirror by a skip-if-present author-
/// scoped CID guard (the `record_cid` index is non-UNIQUE — same guard as
/// [`ensure_in_segment`]), the projection by content-addressed upsert
/// ([`CacheDb::put_post_index_only`], which derives the source from the body and
/// FTS-indexes it — faithful for the author's own native posts, the backed-up
/// scope). An undecodable body writes no projection (logged) but its bytes stay
/// readable via the mirror. Returns the number of records **newly inserted**
/// into the mirror this run (0 on an idempotent re-run; the projection is
/// upserted for every record regardless).
///
/// The caller ([`crate::filesync_handlers`]) decodes + kind-checks the manifest,
/// `finalize_open`s the store, and verifies each `live_segments` entry is on
/// disk before invoking this.
pub async fn restore_from_manifest(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    author: &[u8; 32],
    manifest: &fauna_segment_store::Manifest,
) -> Result<usize> {
    let records = read_restored_records(mgr, author, &manifest.kind_manifest.live_segments).await?;
    let conn = cache_db.conn().await;
    let tx = conn
        .unchecked_transaction()
        .context("begin the post restore transaction")?;
    let restored = reassert_records(&tx, author, &records)?;
    tx.commit().context("commit the post restore transaction")?;
    Ok(restored)
}

/// One post record read back off disk for a restore: `(segment_id, cid, body,
/// floor_bytes)`.
pub(crate) type RestoredPost = (u32, Cid, Vec<u8>, Vec<u8>);

/// Read every record of `live_segments` for `author` — the async half of a
/// post restore, taken before the transaction [`reassert_records`] writes in.
pub(crate) async fn read_restored_records(
    mgr: &SegmentManager,
    author: &[u8; 32],
    live_segments: &[u32],
) -> Result<Vec<RestoredPost>> {
    let mut out = Vec::new();
    for &seg_id in live_segments {
        let rows = mgr
            .read_envelopes_bulk(author, seg_id)
            .await
            .map_err(|e| anyhow::anyhow!("read restored post segment {seg_id}: {e}"))?;
        out.extend(
            rows.into_iter()
                .map(|(cid, body, floor)| (seg_id, cid, body, floor)),
        );
    }
    Ok(out)
}

/// Re-assert `records` into the author's `segment_records` mirror and the
/// `content` feed-index projection, inside the caller's transaction — the one
/// post rebuild both restore sources share: the snapshot restore
/// ([`restore_from_manifest`]) and the backup materialize
/// (`crate::backup::materialize`), which commits it atomically so a crash can
/// never leave a half-restored corpus the empty-target rule then refuses for
/// ever.
///
/// Each record's cid must be the content hash of its body — post files under
/// `Cid::of_dag_cbor(body)` (`message-segment-store.md` § *Record identity per
/// kind*) — and a mismatch refuses the whole rebuild, as every other kind's
/// restore does. Returns how many mirror rows were newly inserted.
pub(crate) fn reassert_records(
    conn: &rusqlite::Connection,
    author: &[u8; 32],
    records: &[RestoredPost],
) -> Result<usize> {
    let mut restored = 0usize;
    for (seg_id, cid, body, floor_bytes) in records {
        if !cid.matches(body) {
            anyhow::bail!(
                "post seg {seg_id} carries record {cid} whose body does not hash to it; \
                 refusing the whole restore"
            );
        }
        let floor = parse_floor(floor_bytes)?;
        let bucket = fauna_segment_store::bucket_for(floor.received_at / 1000);
        // Mirror: idempotent author-scoped insert (skip-if-present, since the
        // record_cid index is non-UNIQUE — restoring onto a nest that already
        // holds some of these posts, or re-running after a crash, must not
        // double-insert).
        let present = matches!(
            records_db::lookup_scope_and_segment(conn, KIND, cid)?,
            Some((found, _)) if &found == author
        );
        if !present {
            records_db::insert_post(conn, author, *seg_id, cid, &bucket, floor.received_at)?;
            restored += 1;
        }
        // Projection: rebuild the `content` feed-index row from the body
        // (content-addressed upsert — `post_id == cid.digest()`, the
        // no-wrapper identity). Upserted for every record regardless of the
        // mirror skip, so a re-run that rebuilds only a missing projection
        // still converges.
        crate::db::posts::put_post_index_only_on(conn, &cid.digest(), body)?;
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Returns (tempdir, manager, cache_db). Drop the tempdir last so segment
    /// files outlive the manager.
    fn setup() -> (TempDir, SegmentManager, Arc<CacheDb>) {
        let tmp = TempDir::new().expect("tmp");
        let manager = SegmentManager::new(tmp.path().to_path_buf(), KIND);
        let cache_db = Arc::new(CacheDb::open_in_memory().expect("in-memory cache db"));
        (tmp, manager, cache_db)
    }

    #[tokio::test]
    async fn append_then_read_by_post_id_round_trip() {
        let (_tmp, mgr, cache_db) = setup();
        let author = [0x31u8; 32];
        let body = b"the canonical post body bytes".to_vec();
        // The reader's handle is the post_id = blake3(body).
        let post_id: [u8; 32] = *blake3::hash(&body).as_bytes();

        let outcome = append_body(&mgr, &cache_db, &author, &body, 1_715_000_000_000)
            .await
            .expect("append");
        assert_eq!(outcome.seg_id, 1);
        assert!(outcome.finalized.is_none());
        // The record CID's digest IS the post_id — the no-wrapper identity.
        assert_eq!(&outcome.record_cid.as_bytes()[4..], &post_id[..]);

        let read = read_body_by_post_id(&mgr, &cache_db, &post_id)
            .await
            .expect("read");
        assert_eq!(read, Some(body));
    }

    /// The list-card preview (`ui/feed.md` § The read model → *The list-card
    /// preview*): every post a feed lists carries its text on the card —
    /// native AND each bridge's, never Bluesky-only. A bridged post rests with
    /// a `source` and no FTS row, so before `content_meta.preview` its card
    /// read the FTS row it never had and painted empty on every app.
    #[tokio::test]
    async fn preview_every_source_reaches_the_feed_card_with_its_text() {
        let (_tmp, mgr, cache_db) = setup();
        let mut want = std::collections::HashMap::new();
        for (i, source) in [None, Some("activitypub"), Some("nostr"), Some("bluesky")]
            .into_iter()
            .enumerate()
        {
            let kp = fauna_core::identity::ActorKeypair::generate();
            let text = format!("card text from {}", source.unwrap_or("fauna"));
            let post = fauna_core::data::Post {
                author: kp.actor_id(),
                created_at: fauna_core::data::Timestamp(1_700_000_000_000_000 + i as u64),
                body: fauna_core::data::PostBody::Text {
                    content: text.clone(),
                    facets: vec![],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin: None,
            };
            // A native post arrives signed; a bridge rests the canonical
            // encoding under its blake3 — each transit point's real shape.
            let (id, wire) = match source {
                None => (
                    fauna_core::encoding::compute_post_id(&post)
                        .unwrap()
                        .digest(),
                    fauna_core::encoding::sign_and_pack(&kp, &post).unwrap(),
                ),
                Some(_) => {
                    let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
                    (*blake3::hash(&payload).as_bytes(), payload)
                }
            };
            store_post(&mgr, &cache_db, &id, &wire, source)
                .await
                .expect("store");
            want.insert(hex::encode(id), text);
        }

        let rows = cache_db
            .query_feed(
                &[],
                fauna_core::scoring::FilterCombination::All,
                &[],
                None,
                50,
            )
            .await
            .unwrap();
        let got: std::collections::HashMap<String, String> = rows
            .into_iter()
            .map(|r| (hex::encode(r.post_id), r.body))
            .collect();
        assert_eq!(got, want, "every source's card carries its own text");
    }

    /// One id, one plane — refused before anything is appended. A row of another
    /// plane (an inbox message here) can be keyed by a signed post's CID digest,
    /// so a validly signed wire of that post names the same id. Storing it as a
    /// post would wipe the member-only payload, mint a public `content_meta` row
    /// and leave the body in the author's `__post` segment.
    #[tokio::test]
    async fn store_post_refuses_an_id_holding_another_planes_row() {
        let (_tmp, mgr, cache_db) = setup();
        let kp = fauna_core::identity::ActorKeypair::generate();
        let post = fauna_core::data::Post {
            author: kp.actor_id(),
            created_at: fauna_core::data::Timestamp(1_700_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "aliasing text".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let wire = fauna_core::encoding::sign_and_pack(&kp, &post).unwrap();
        let id = fauna_core::encoding::compute_post_id(&post)
            .unwrap()
            .digest();
        {
            let conn = cache_db.conn().await;
            crate::db::content::insert_content(
                &conn,
                &id,
                "inbox/message",
                &kp.actor_id().0,
                1_000,
                b"members-only",
                None,
                "fauna",
                None,
            )
            .unwrap();
        }

        store_post(&mgr, &cache_db, &id, &wire, None)
            .await
            .expect_err("a post must not take an id that holds an inbox message");

        {
            let conn = cache_db.conn().await;
            assert_eq!(
                crate::db::content::get_content(&conn, &id)
                    .unwrap()
                    .map(|(p, _)| p),
                Some(b"members-only".to_vec()),
                "the inbox message's payload is untouched"
            );
        }
        assert!(
            !cache_db.content_meta_exists(&id).await.unwrap(),
            "no content_meta row was minted for the inbox message's id"
        );
        assert!(
            cache_db
                .segment_records_lookup_scope_and_segment(KIND, &Cid::of_dag_cbor(&wire))
                .await
                .unwrap()
                .is_none(),
            "nothing was appended to the author's post segment"
        );
    }

    #[tokio::test]
    async fn load_post_body_segment_first_then_inline_fallback() {
        let (_tmp, mgr, cache_db) = setup();
        let author = [0x40u8; 32];

        // (1) A post in the `__post` segment store → segment hit.
        let seg_body = b"body in the segment store".to_vec();
        let seg_pid: [u8; 32] = *blake3::hash(&seg_body).as_bytes();
        append_body(&mgr, &cache_db, &author, &seg_body, 1_715_000_000_000)
            .await
            .expect("append");
        assert_eq!(
            load_post_body(&mgr, &cache_db, &seg_pid)
                .await
                .expect("load seg"),
            Some(seg_body)
        );

        // (2) A post inline in `content.payload` (no segment record — the
        // undecodable-body shape `store_post` keeps inline) → the fallback
        // resolves it.
        let inline_body = b"inline body".to_vec();
        let inline_pid: [u8; 32] = *blake3::hash(&inline_body).as_bytes();
        cache_db
            .put_post(&inline_pid, &inline_body, None)
            .await
            .expect("put inline");
        assert_eq!(
            load_post_body(&mgr, &cache_db, &inline_pid)
                .await
                .expect("load inline"),
            Some(inline_body)
        );

        // (3) Unknown post → None (no segment record, no inline row).
        let missing: [u8; 32] = *blake3::hash(b"never stored anywhere").as_bytes();
        assert_eq!(
            load_post_body(&mgr, &cache_db, &missing)
                .await
                .expect("load missing"),
            None
        );
    }

    #[tokio::test]
    async fn read_unknown_post_returns_none() {
        let (_tmp, mgr, cache_db) = setup();
        let missing: [u8; 32] = *blake3::hash(b"never stored").as_bytes();
        let read = read_body_by_post_id(&mgr, &cache_db, &missing)
            .await
            .expect("read");
        assert_eq!(read, None);
    }

    #[tokio::test]
    async fn large_body_over_inline_threshold_round_trips() {
        let (_tmp, mgr, cache_db) = setup();
        let author = [0x32u8; 32];
        // > 64 KiB — the payload_store inline threshold that, pre-rollout, would
        // have spilled this body to the blob store. Segments hold it directly.
        let body = vec![0xABu8; 80 * 1024];
        let post_id: [u8; 32] = *blake3::hash(&body).as_bytes();

        append_body(&mgr, &cache_db, &author, &body, 1_715_000_000_000)
            .await
            .expect("append");
        let read = read_body_by_post_id(&mgr, &cache_db, &post_id)
            .await
            .expect("read");
        assert_eq!(read, Some(body));
    }

    #[tokio::test]
    async fn distinct_authors_isolate_by_scope() {
        let (_tmp, mgr, cache_db) = setup();
        let a1 = [0x33u8; 32];
        let a2 = [0x34u8; 32];
        let b1 = b"author one body".to_vec();
        let b2 = b"author two body".to_vec();
        let p1: [u8; 32] = *blake3::hash(&b1).as_bytes();
        let p2: [u8; 32] = *blake3::hash(&b2).as_bytes();

        append_body(&mgr, &cache_db, &a1, &b1, 1_715_000_000_000)
            .await
            .expect("a1");
        append_body(&mgr, &cache_db, &a2, &b2, 1_715_000_000_000)
            .await
            .expect("a2");

        // Each post resolves to its own author's segment via the CID index —
        // the reader never had to know which author.
        assert_eq!(
            read_body_by_post_id(&mgr, &cache_db, &p1)
                .await
                .expect("p1"),
            Some(b1)
        );
        assert_eq!(
            read_body_by_post_id(&mgr, &cache_db, &p2)
                .await
                .expect("p2"),
            Some(b2)
        );
    }

    #[tokio::test]
    async fn tombstone_hides_post_from_read() {
        let (_tmp, mgr, cache_db) = setup();
        let author = [0x35u8; 32];
        let body = b"to be tombstoned".to_vec();
        let post_id: [u8; 32] = *blake3::hash(&body).as_bytes();

        let outcome = append_body(&mgr, &cache_db, &author, &body, 1_715_000_000_000)
            .await
            .expect("append");
        let n = tombstone_by_cid(&cache_db, &author, outcome.seg_id, &outcome.record_cid)
            .await
            .expect("tombstone");
        assert_eq!(n, 1);

        let read = read_body_by_post_id(&mgr, &cache_db, &post_id)
            .await
            .expect("read");
        assert_eq!(read, None, "tombstoned post is no longer resolvable");
    }

    /// The tombstone-inclusive reader resolves exactly the record the
    /// live-only one goes blind to — the load-bearing half of the
    /// legal-takedown-of-an-already-deleted-post arm (`moderation.md` §
    /// Legal takedown → *Posts*, "deleted, then the order arrives").
    #[tokio::test]
    async fn read_including_tombstoned_finds_what_the_live_only_reader_cannot() {
        let (_tmp, mgr, cache_db) = setup();
        let author = [0x36u8; 32];
        let body = b"deleted before the order could land".to_vec();
        let post_id: [u8; 32] = *blake3::hash(&body).as_bytes();

        let outcome = append_body(&mgr, &cache_db, &author, &body, 1_715_000_000_000)
            .await
            .expect("append");
        tombstone_by_cid(&cache_db, &author, outcome.seg_id, &outcome.record_cid)
            .await
            .expect("tombstone");

        assert_eq!(
            read_body_by_post_id(&mgr, &cache_db, &post_id)
                .await
                .expect("live-only read"),
            None,
            "the live-only reader answers None for a tombstoned record"
        );
        assert_eq!(
            read_body_by_post_id_including_tombstoned(&mgr, &cache_db, &post_id)
                .await
                .expect("tombstone-inclusive read"),
            Some(body),
            "the tombstone-inclusive reader still finds the bytes, uncompacted"
        );
    }

    /// Once the segment FILE is physically gone — `segment_records` rows are
    /// tombstoned, never deleted, so the mirror lookup stays `Some` forever —
    /// the tombstone-inclusive reader answers `None` rather than erroring.
    /// `FramedSegment::open` reports this the same way it reports an
    /// unfinalized crash tail (`is_unfinalized_crash_tail`), which is exactly
    /// why the reader must check for it explicitly rather than only
    /// `RecordNotFound`.
    #[tokio::test]
    async fn read_including_tombstoned_answers_none_once_the_segment_file_is_gone() {
        let (_tmp, mgr, cache_db) = setup();
        let author = [0x37u8; 32];
        let body = b"physically reclaimed after the delete".to_vec();
        let post_id: [u8; 32] = *blake3::hash(&body).as_bytes();

        let outcome = append_body(&mgr, &cache_db, &author, &body, 1_715_000_000_000)
            .await
            .expect("append");
        tombstone_by_cid(&cache_db, &author, outcome.seg_id, &outcome.record_cid)
            .await
            .expect("tombstone");
        mgr.finalize_open(&author).await.expect("finalize");

        std::fs::remove_file(mgr.segment_file_path(&author, outcome.seg_id))
            .expect("remove the segment file to simulate physical reclaim");
        std::fs::remove_file(mgr.segment_meta_path(&author, outcome.seg_id)).ok();

        assert!(
            cache_db
                .segment_records_lookup_scope_and_segment_including_tombstoned(
                    KIND,
                    &outcome.record_cid,
                )
                .await
                .unwrap()
                .is_some(),
            "the tombstoned mirror row outlives the file"
        );
        assert_eq!(
            read_body_by_post_id_including_tombstoned(&mgr, &cache_db, &post_id)
                .await
                .expect("tombstone-inclusive read must not error on a reclaimed file"),
            None,
            "no bytes left to read once the segment file is gone"
        );
    }

    #[tokio::test]
    async fn compact_bucket_drops_tombstoned_and_keeps_survivors() {
        let (_tmp, mgr, cache_db) = setup();
        let author = [0x3au8; 32];

        // Three posts in the same bucket (May 2024) → all land in segment 1.
        let mut outcomes = Vec::new();
        for i in 0..3 {
            let body = format!("post-body-{i}").into_bytes();
            let o = append_body(&mgr, &cache_db, &author, &body, 1_715_000_000_000)
                .await
                .expect("append");
            assert_eq!(o.seg_id, 1, "same bucket → one segment");
            outcomes.push((body, o));
        }
        mgr.finalize_open(&author).await.expect("finalize");

        // Tombstone the first post.
        let (_, first) = &outcomes[0];
        tombstone_by_cid(&cache_db, &author, first.seg_id, &first.record_cid)
            .await
            .expect("tombstone");

        let plan = CompactionPlan {
            inputs: vec![1],
            bucket: "2024-05".to_string(),
        };
        let outcome = compact_bucket(&mgr, &cache_db, &author, &plan)
            .await
            .expect("compact");
        assert_eq!(outcome.consumed, vec![1]);
        assert!(
            outcome.new_segment.is_some(),
            "two records survived → a new segment was written"
        );

        // The two survivors still resolve by post_id; the tombstoned one is gone.
        let p0: [u8; 32] = *blake3::hash(&outcomes[0].0).as_bytes();
        let p1: [u8; 32] = *blake3::hash(&outcomes[1].0).as_bytes();
        let p2: [u8; 32] = *blake3::hash(&outcomes[2].0).as_bytes();
        assert_eq!(
            read_body_by_post_id(&mgr, &cache_db, &p0).await.unwrap(),
            None
        );
        assert_eq!(
            read_body_by_post_id(&mgr, &cache_db, &p1).await.unwrap(),
            Some(outcomes[1].0.clone())
        );
        assert_eq!(
            read_body_by_post_id(&mgr, &cache_db, &p2).await.unwrap(),
            Some(outcomes[2].0.clone())
        );
    }

    #[tokio::test]
    async fn restore_from_manifest_rebuilds_mirror_additively() {
        let (_tmp, mgr, cache_db) = setup();
        let author = [0x4bu8; 32];

        // Append 3 posts → on-disk segment + mirror rows in `cache_db`.
        let mut bodies = Vec::new();
        for i in 0..3 {
            let body = format!("restore-body-{i}").into_bytes();
            append_body(&mgr, &cache_db, &author, &body, 1_715_000_000_000)
                .await
                .expect("append");
            bodies.push(body);
        }
        mgr.finalize_open(&author).await.expect("finalize");
        let manifest = mgr.load_manifest(&author).await.expect("load manifest");

        // Simulate a fresh/recovery nest: the segment files are on disk (the
        // snapshot's pinned data) but the mirror is empty. read_body misses.
        let fresh_db = Arc::new(CacheDb::open_in_memory().expect("fresh db"));
        let p0: [u8; 32] = *blake3::hash(&bodies[0]).as_bytes();
        assert_eq!(
            read_body_by_post_id(&mgr, &fresh_db, &p0).await.unwrap(),
            None,
            "fresh nest has no mirror yet"
        );

        // Restore rebuilds the mirror from the on-disk segments.
        let n = restore_from_manifest(&mgr, &fresh_db, &author, &manifest)
            .await
            .expect("restore");
        assert_eq!(n, 3, "all 3 records re-asserted");

        // Every post is now resolvable by post_id in the fresh nest.
        for body in &bodies {
            let pid: [u8; 32] = *blake3::hash(body).as_bytes();
            assert_eq!(
                read_body_by_post_id(&mgr, &fresh_db, &pid).await.unwrap(),
                Some(body.clone())
            );
        }

        // Idempotent: a second restore re-asserts nothing new (skip-if-present)
        // yet leaves every post readable.
        let n2 = restore_from_manifest(&mgr, &fresh_db, &author, &manifest)
            .await
            .expect("restore again");
        assert_eq!(n2, 0, "second restore inserts no new mirror rows");
        assert_eq!(
            read_body_by_post_id(&mgr, &fresh_db, &p0).await.unwrap(),
            Some(bodies[0].clone())
        );
    }

    use crate::partition_scan::Gate;

    /// What a production call site of a flag-blind post-body primitive does with
    /// the bytes. The strings are the partition's reasons, kept beside the
    /// entries so a reader of the table never has to re-derive them.
    enum Use {
        /// The bytes, or anything derived from them, can reach a caller outside
        /// the nest process. The string names the gate that withholds a flagged
        /// post on the way, as a flow — never "a predicate is nearby" — and the
        /// [`Gate`] is the call the test holds the path's production body to.
        Serve(Gate, &'static str),
        /// Nothing derived from the body leaves the process.
        Reader(&'static str),
        /// The primitive itself, or the post-read core that IS the gate.
        Primitive(&'static str),
    }

    /// Every production call site of the flag-blind post-body primitives, keyed
    /// by `(path under src/, enclosing fn)`.
    const PARTITION: &[(&str, &str, Use)] = &[
        // ── serve paths ──
        (
            "activitypub/actor_routes.rs",
            "get_outbox",
            Use::Serve(
                Gate::Calls(&["public_outbox_page("]),
                "reads only the ids `db_helpers::public_outbox_page` returns, which \
                 PUBLIC_POST_SERVABLE filters",
            ),
        ),
        (
            "activitypub/actor_routes.rs",
            "get_note",
            Use::Serve(
                Gate::Calls(&["public_note_exists("]),
                "`db_helpers::public_note_exists` (PUBLIC_POST_SERVABLE on this very \
                 id) answers 404 before the read",
            ),
        ),
        (
            "bridge_atproto_handlers.rs",
            "fetch_atproto_public_posts_handler",
            Use::Serve(
                Gate::Calls(&["list_public_projection_page("]),
                "reads only the post rows `list_public_projection_page` returns \
                 (PUBLIC_POST_SERVABLE); its tombstone arm reads no body",
            ),
        ),
        (
            "nostr/store.rs",
            "materialize_account",
            Use::Serve(
                Gate::Via {
                    hops: &[
                        ("nostr/store.rs", "unmaterialized_exposed_post_ids"),
                        ("nostr/store.rs", "unmaterialized_posts_sql"),
                    ],
                    gates: &["PUBLIC_POST_SERVABLE"],
                },
                "reads only the ids `unmaterialized_exposed_post_ids` returns \
                 (PUBLIC_POST_SERVABLE + the permanent TakenDown-obligation exclusion). \
                 `unmaterialized_exposed_post_ids` is itself a seam — the SQL predicate lives \
                 one hop below it, in `unmaterialized_posts_sql` — so both links are held, not \
                 only the door's own call of the seam",
            ),
        ),
        (
            "feed_routes.rs",
            "remote_query_feed_core",
            Use::Serve(
                Gate::Calls(&["query_feed(", "query_feed_for_authors("]),
                "reads only the candidates `query_feed` / `query_feed_for_authors` \
                 return (MODERATION_SERVABLE); the peer receives the references \
                 extracted from each body",
            ),
        ),
        // The serving read is the INNER `render_published_posts_now`, not the
        // public `render_published_posts` that wraps it: the outer fn only
        // reads the owed-render/owed-restore nonces, mints the render's claim
        // and races the withdrawal deadline around the call, and the listing
        // plus every body read sit inside. Re-pointed rather than
        // re-gated — `render_published_posts_now` has exactly ONE caller, the
        // unconditional `?`-propagating call in `render_published_posts`, so
        // no path reaches a body without crossing the cap.
        (
            "web_content/service.rs",
            "render_published_posts_now",
            Use::Serve(
                Gate::Calls(&["list_web_published_servable_capped("]),
                "renders only what `list_web_published_servable_capped` lists \
                 (MODERATION_SERVABLE), and the takedown handler re-renders \
                 fail-closed (`rerender_after_moderation_change`)",
            ),
        ),
        (
            "export_routes.rs",
            "gather_export_data",
            Use::Serve(
                Gate::Calls(&["get_post_legal_takedown("]),
                "`get_post_legal_takedown` is checked before the read and a withheld \
                 entry carries no body; the archive is the author's, so quarantine \
                 (author-visible) does not bind",
            ),
        ),
        // ── the gate and the primitives ──
        (
            "routes.rs",
            "get_post_core",
            Use::Primitive(
                "the post-read core: legal takedown, then quarantine, then the read — \
                 what every serve path without an enumeration gate routes through",
            ),
        ),
        (
            "segments/post.rs",
            "load_post_body",
            Use::Primitive("segment-first, then the inline `content.payload` fallback"),
        ),
        (
            "segments/post.rs",
            "read_body_by_post_id",
            Use::Primitive("derives the record CID and delegates to `read_body_by_cid`"),
        ),
        (
            "segments/post.rs",
            "read_body_by_post_id_including_tombstoned",
            Use::Primitive(
                "derives the record CID and delegates to \
                 `read_body_by_cid_including_tombstoned`",
            ),
        ),
        (
            "moderation_handlers.rs",
            "legal_takedown_of_deleted_post",
            Use::Reader(
                "reads a tombstoned-but-uncompacted post's bytes, on the \
                 legal-takedown-of-an-already-deleted-post arm, only to recover the \
                 blob digests `legal_takedown_deleted_posts` records — nothing derived \
                 from the body leaves the process",
            ),
        ),
        // ── readers ──
        (
            "routes.rs",
            "delete_post_core",
            Use::Reader(
                "reads the references a post being deleted carried, to reverse its \
                 counters; returns nothing of the body",
            ),
        ),
        (
            "interact_routes.rs",
            "interact_with_post_core",
            Use::Reader(
                "`unrepost` decodes the caller's own post only to confirm it is a \
                 repost before deleting it; the one bit it yields authorizes the \
                 delete, and no body content is returned",
            ),
        ),
        (
            "moderation_withhold.rs",
            "recompute",
            Use::Reader(
                "reads FLAGGED posts on purpose, to compute the blob withhold set — \
                 the reason the primitive must stay flag-blind",
            ),
        ),
        (
            "backup/gc.rs",
            "collect_reachable_hashes",
            Use::Reader("the GC walk pins the blobs each live record names"),
        ),
        (
            "nest_link/client.rs",
            "handle_fetch",
            Use::Reader(
                "the worker answering its own primary's read-fallback, inside the \
                 deployment; the primary asks only for a post it holds no row for, \
                 and a flag lives on that row",
            ),
        ),
    ];

    /// **The flag-blind post-body primitives have a closed, classified set of
    /// callers** (`moderation.md` § Legal takedown → *Posts*).
    ///
    /// `load_post_body` and the reads beneath it answer for a taken-down or
    /// quarantined post exactly as for any other — deliberately, because
    /// `moderation_withhold` must read flagged posts through them. So the
    /// withholding lives on each SERVE path instead, and the goal doc states
    /// that as a standing duty: a new path is enrolled on the day it is
    /// written. That duty went unchecked long enough for five serve paths to
    /// ship ungated — two HLS manifest doors, the web-site render, the account
    /// export and `moderation.train` (all closed by 2026-09-10) — each found only
    /// when someone happened to look.
    ///
    /// This turns the duty into a gate. It walks every production source file
    /// (unit-test modules cut), finds each call of a primitive, and requires the
    /// enclosing fn to appear in [`PARTITION`] — as a serve path naming its gate,
    /// a reader, or the primitive itself. A new caller fails here until someone
    /// decides which it is; a removed one fails until its entry goes, so the
    /// table can never describe callers that no longer exist. And a serve path's
    /// gate is held to its production body, not only named in the table, so
    /// deleting the gate call fails here too rather than only in the path's own
    /// witness.
    #[test]
    fn every_flag_blind_post_body_read_is_partitioned() {
        // `fn load_post_body(` is the definition, not a call — the shared
        // scanner knows that, and knows to cut `#[cfg(test)]` items.
        let found = crate::partition_scan::callers_of(&[
            "load_post_body(",
            "read_body_by_post_id(",
            "read_body_by_cid(",
            ".get_post(",
            "read_body_by_post_id_including_tombstoned(",
            "read_body_by_cid_including_tombstoned(",
        ]);

        let table: std::collections::BTreeSet<(String, String)> = PARTITION
            .iter()
            .map(|(file, func, _)| (file.to_string(), func.to_string()))
            .collect();
        assert_eq!(
            table.len(),
            PARTITION.len(),
            "a (file, fn) pair is listed twice in PARTITION"
        );
        for (_, _, used) in PARTITION {
            let (Use::Serve(_, why) | Use::Reader(why) | Use::Primitive(why)) = used;
            assert!(
                !why.trim().is_empty(),
                "every partition entry states its reason"
            );
        }
        let served: Vec<(&str, &str, &Gate)> = PARTITION
            .iter()
            .filter_map(|(file, func, used)| match used {
                Use::Serve(gate, _) => Some((*file, *func, gate)),
                _ => None,
            })
            .collect();
        crate::partition_scan::assert_gates_hold(&served);

        crate::partition_scan::assert_partitioned(
            &found,
            &table,
            "The primitive answers for a taken-down or quarantined post like any other, \
             so decide what this caller does with the bytes and add it to PARTITION: a \
             Serve path (anything derived from the body can leave the process) must name \
             the gate that withholds a flagged post — route it through \
             `routes::get_post_core`, or read only ids an enumeration gated on \
             `db::public_servability` returned — and enrol it in moderation.md § Legal \
             takedown → Posts; a Reader must say why nothing leaves.",
        );
    }
}
