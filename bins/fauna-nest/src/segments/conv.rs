//! Conv-kind nest-side segment coordination.
//!
//! Sibling of [`super::mail`] for the conversation kind. The file/manifest
//! state machine is the same kind-agnostic `fauna_segment_store::SegmentManager`
//! (`AppState.conv_segments`, registered with `kind = "conv"`); the
//! conv-specific encoding lives in `fauna_mls::segments`. What remains
//! genuinely conv-specific *and* nest-specific is the coordination between the
//! `SegmentManager` and the `segment_records` SQLite mirror — that lives here
//! as free functions taking `(&SegmentManager, &CacheDb, …)`, mirroring
//! `segments::mail`'s shape. The shared *pattern* extracts once two kinds have
//! shaped it (deferred, same as `segments::mail`'s note).
//!
//! ### What's different from mail
//!
//! Conv records carry a per-channel monotonic `seq` — the genuinely
//! cross-member identity, which every member's dedup, ordering, read
//! watermarks, `target_seq` references and history-fetch wire shapes key on.
//! It is **not** the record's filing identity: since the 2026-08-17 cutover
//! that is `Cid::of_dag_cbor(envelope_bytes)`, the content hash, exactly as for
//! every other kind (`message-segment-store.md` § *Record identity per kind*;
//! the retired `derive_record_id(channel_id, seq, body)` is deleted).
//!
//! ### Per-channel seq lock — WHY
//!
//! `seq` is a per-channel monotonic counter, so the seq-query → append →
//! mirror-insert chain must be atomic per channel or two concurrent sends
//! allocate the same seq. The lock covers one more thing since the cutover: the
//! scoped pre-append dedup, whose check-then-append would otherwise race a
//! concurrent identical ingest. `SegmentManager`'s per-scope mutex is not
//! reentrant (a held-lock append variant would reshape the kind-agnostic
//! manager), so conv serializes here with its own per-channel lock. Lock order:
//! conv-channel-lock → segment-scope-mutex → CacheDb conn-mutex. Reads need no
//! conv lock.
//!
//! ### Retained invariants (shared with mail via `SegmentManager`)
//!
//! * Segment file write + `segment_records` mirror INSERT both happen while the
//!   per-channel seq lock is held (the manager's per-scope mutex covers only
//!   the file write).
//! * read-your-own-writes: read paths go through `SegmentManager`, which
//!   finalizes the open segment before reading.

use std::sync::Arc;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use dashmap::DashMap;
use fauna_cbor::Cid;
use fauna_mls::types::ChannelEnvelope;
use fauna_segment_store::{CompactionPlan, ManagerError, SegmentManager};
use tokio::sync::Mutex;

use super::mail::BucketCompactionOutcome;
use super::records_db::{self, NewConvSegmentRecord};
use crate::db::CacheDb;

/// Per-channel seq-allocation locks. See the module doc for the WHY: seq is a
/// per-channel monotonic counter, so the dedup→query→append→mirror-insert chain
/// must be atomic per channel.
static SEQ_LOCKS: LazyLock<DashMap<[u8; 32], Arc<Mutex<()>>>> = LazyLock::new(DashMap::new);

fn seq_lock_for(channel_id: &[u8; 32]) -> Arc<Mutex<()>> {
    super::keyed_seq_lock(&SEQ_LOCKS, channel_id)
}

/// Result of [`append`].
#[derive(Debug, Clone, Copy)]
pub struct ConvAppendOutcome {
    /// Per-channel monotonic seq assigned to this record.
    pub seq: i64,
    /// Segment id the record landed in (live tail of the manifest).
    pub seg_id: u32,
    /// `Some(closed_seg_id)` if this append rotated a previously-open segment
    /// closed; `None` otherwise (mirrors `AppendOutcome.finalized`).
    pub finalized: Option<u32>,
}

/// Outcome of a commit-gated [`append_gated`] chain.
#[derive(Debug, Clone, Copy)]
pub enum ConvAppendResult {
    /// The record landed at the returned outcome's `seq`.
    Appended(ConvAppendOutcome),
    /// The device-owned-epoch precondition failed: a `ChannelEnvelope::Commit`
    /// landed on this channel *after* the caller's `expect_no_commit_since` seq,
    /// so the caller is committing from a stale epoch. `latest_commit_seq` is the
    /// current commit high-water mark — what the caller must process past before
    /// retrying (`docs/goal/behavior/devices.md` § Cross-device MLS group-state
    /// sync; the client's rebase = clear pending → process intervening records →
    /// retry with the new seq).
    StaleCommit { latest_commit_seq: i64 },
}

/// Append one conv record to a channel's segment store (ungated).
///
/// Holds the per-channel seq lock for the whole chain: encode the envelope
/// (which mints the content-hash filing CID), dedup that CID within the
/// channel, allocate the next seq, encode the floor, append to the segment,
/// then INSERT the `segment_records` mirror row. Also advances the channel's
/// MLS **commit high-water mark** when the body is a `ChannelEnvelope::Commit`
/// (maintained for every append regardless of caller, so the [`append_gated`]
/// precondition is always accurate).
///
/// **Idempotent on a byte replay:** re-appending the identical envelope to the
/// same channel returns the first record's `seq` and writes nothing (the dedup
/// hit), rather than storing the message twice under two seqs.
///
/// `received_at` is epoch **milliseconds** (the nest's `now_epoch_millis()`
/// shape); the bucket is computed from epoch seconds, same as `mail::append`.
///
/// Names **no sending actor**: every caller of this entry point is a nest-side
/// writer with no authenticated member behind it (compaction, restore, the
/// subscription routes), so a commit it re-appends
/// leaves the commit watermark's authorship empty rather than borrowing
/// someone's. A room whose mark names no sender has its positioned reports
/// admitted unchecked, as they were before schema 64 (`conversation-rooms.md`
/// § The floor roster). Callers that *do* authenticate a member use
/// [`append_with_refs`] or [`append_gated`].
pub async fn append(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    channel_id: &[u8; 32],
    body: &[u8],
    received_at: i64,
) -> Result<ConvAppendOutcome> {
    append_with_refs(mgr, cache_db, channel_id, body, received_at, &[], None).await
}

/// [`append`] for a record whose sealed body names attachment blobs: the
/// sender's plaintext `attachment_refs` (`ChannelSendRequest::attachment_refs`
/// — the conversation kind's blob-reachability floor, `encryption-at-rest.md`
/// § Per-content-kind conformance → Conversation messages row, 2026-09-08)
/// land in `conv_attachment_refs` in the same transaction as the mirror row,
/// so the blob GC can pin them for as long as the record is live. A dedup
/// hit (byte replay) returns the first record's seq and writes nothing — the
/// first append's refs stand. `sender_actor` — the actor authenticated on the
/// send, which for the MDA scheduling gateway is the organizer
/// `channel_send_core` acts as — is recorded as the record's attested author
/// (`conv_record_authors`). Every nest-side writer (compaction, restores) has
/// neither attachments nor an actor and calls [`append`].
pub async fn append_with_refs(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    channel_id: &[u8; 32],
    body: &[u8],
    received_at: i64,
    attachment_refs: &[[u8; 32]],
    sender_actor: Option<&[u8; 32]>,
) -> Result<ConvAppendOutcome> {
    // Ungated (`expect_no_commit_since = None`) ⇒ the gate is a no-op, so the
    // result is always `Appended`.
    match append_locked(
        mgr,
        cache_db,
        channel_id,
        body,
        received_at,
        None,
        attachment_refs,
        sender_actor,
    )
    .await?
    {
        ConvAppendResult::Appended(outcome) => Ok(outcome),
        ConvAppendResult::StaleCommit { .. } => {
            unreachable!(
                "an ungated append (expect_no_commit_since = None) can never be StaleCommit"
            )
        }
    }
}

/// Append one conv record under the **device-owned-epoch commit gate**.
///
/// Same locked chain as [`append`], but first — under the per-channel seq lock,
/// so no commit can race — checks the channel's commit high-water mark against
/// `expect_no_commit_since`: if a `ChannelEnvelope::Commit` has landed at a seq
/// greater than the caller's, returns [`ConvAppendResult::StaleCommit`] **without
/// allocating a seq or writing anything** (the caller's commit is from a stale
/// epoch). Otherwise appends exactly like [`append`]. Backs the
/// `fauna.conversations.channel.send` `expect_no_commit_since` precondition.
pub async fn append_gated(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    channel_id: &[u8; 32],
    body: &[u8],
    received_at: i64,
    expect_no_commit_since: i64,
    attachment_refs: &[[u8; 32]],
    sender_actor: Option<&[u8; 32]>,
) -> Result<ConvAppendResult> {
    append_locked(
        mgr,
        cache_db,
        channel_id,
        body,
        received_at,
        Some(expect_no_commit_since),
        attachment_refs,
        sender_actor,
    )
    .await
}

/// The shared locked append chain behind [`append`] and [`append_gated`]. Holds
/// the per-channel seq lock across the optional gate check, the seq allocation,
/// the segment write, and the commit-watermark advance — one lock hold, so the
/// gate can't race the very commit it serializes (a gate checked outside the
/// lock would let two devices both pass before either's commit lands).
async fn append_locked(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    channel_id: &[u8; 32],
    body: &[u8],
    received_at: i64,
    expect_no_commit_since: Option<i64>,
    attachment_refs: &[[u8; 32]],
    sender_actor: Option<&[u8; 32]>,
) -> Result<ConvAppendResult> {
    // conv-channel-lock → segment-scope-mutex → CacheDb conn-mutex.
    let lock = seq_lock_for(channel_id);
    let _guard = lock.lock().await;

    // Device-owned-epoch gate: reject a stale commit BEFORE allocating a seq or
    // writing anything, so a rejected send consumes no seq. `∃ commit after
    // `since` ⟺ high-water mark > `since`` (commits are monotonic in seq), and
    // the mark survives tombstoning/compaction, so this is both O(1) and correct
    // where a live-record scan could miss a compacted-away commit.
    if let Some(since) = expect_no_commit_since {
        let watermark = cache_db.channel_commit_watermark(channel_id).await?;
        if watermark > since {
            return Ok(ConvAppendResult::StaleCommit {
                latest_commit_seq: watermark,
            });
        }
    }

    // Is this record an MLS Commit? Lenient decode: encrypted mode strict-decodes
    // the `ChannelEnvelope` at ingest, but plaintext mode imposes no envelope
    // shape on the uploader, so a body that doesn't decode is simply "not a
    // commit" — a non-MLS-envelope body can never own an MLS epoch.
    let is_commit = matches!(
        ChannelEnvelope::from_bytes(body),
        Ok(ChannelEnvelope::Commit(_))
    );

    // Encode once, and let the encode mint the identity: the filing CID is the
    // content hash of the very bytes stored under it, `seq` no longer part of
    // the pre-image (`message-segment-store.md` § Record identity per kind).
    // This is why the mint moved ABOVE the seq allocation — identity no longer
    // depends on it, and the dedup below must run before a seq is consumed.
    let envelope = fauna_mls::segments::ConvRecordEnvelope::new(body.to_vec());
    let (cid, env_bytes) = fauna_mls::segments::encode_record(&envelope)
        .map_err(|e| anyhow::anyhow!("encode conv record envelope: {e}"))?;

    // Scoped pre-append dedup — `(scope_id, kind, record_cid)`, the obligation
    // the identity rule carries (§ Record identity per kind, pre-check 2): the
    // channel is no longer inside the hash, so the same bytes on a different
    // channel are a different channel's record. A hit is a literal byte replay
    // of this channel's own record — the segment append is not idempotent (a
    // duplicate cid in one segment is an error), so it must be caught here.
    // Returning the FIRST record's seq makes a retried `channel.send` of the
    // identical envelope idempotent instead of duplicating the message at a
    // second seq. Inside the seq lock, so check-then-append cannot race.
    if let Some(existing) = cache_db
        .segment_records_lookup_record(channel_id, fauna_mls::segments::KIND, &cid)
        .await?
    {
        let seq = existing.seq.ok_or_else(|| {
            anyhow::anyhow!(
                "conv segment_records row for {cid} carries no seq — every conv row \
                 is written with one, so this mirror row is corrupt"
            )
        })?;
        return Ok(ConvAppendResult::Appended(ConvAppendOutcome {
            seq,
            seg_id: existing.segment_id,
            finalized: None,
        }));
    }

    let seq = cache_db.segment_records_next_conv_seq(channel_id).await?;

    let floor = fauna_mls::segments::ConvFloorMetadata {
        format_version: 1,
        received_at,
        seq,
    };
    let floor_bytes = fauna_mls::segments::serialize_floor(&floor)
        .map_err(|e| anyhow::anyhow!("serialize conv floor: {e}"))?;
    let bucket = fauna_segment_store::bucket_for(received_at / 1000);

    let outcome = mgr
        .append_record_with_bucket(channel_id, cid, &env_bytes, &floor_bytes, &bucket)
        .await
        .map_err(|e| anyhow::anyhow!("append_record_with_bucket: {e}"))?;

    cache_db
        .segment_records_insert_conv(
            channel_id,
            outcome.segment_id,
            &cid,
            &bucket,
            received_at,
            seq,
            attachment_refs,
            sender_actor,
        )
        .await?;

    // Advance the commit high-water mark AFTER the record is durably in the
    // mirror, so the mark never points past a record that isn't there. Monotonic
    // upsert — a commit only ever moves the mark forward (`MAX`).
    //
    // `sender_actor` rides along as the mark's authorship: the actor this nest
    // authenticated on the send that carried the commit — same-nest, the
    // `channel.send` caller; relayed, the `requesting_actor_id` the home bound
    // at `require_foreign_member`. It is what the floor roster's commit-order
    // guard checks a positioned report against (`conversation-rooms.md` § The
    // floor roster), and it is written here rather than derived later because
    // the envelope is sealed: the nest can never learn from the record itself
    // whose commit it was.
    if is_commit {
        cache_db
            .set_channel_commit_watermark(channel_id, seq, sender_actor)
            .await?;
    }

    Ok(ConvAppendResult::Appended(ConvAppendOutcome {
        seq,
        seg_id: outcome.segment_id,
        finalized: outcome.finalized,
    }))
}

/// Read live conv records for one channel with `seq > after_seq`, oldest first,
/// up to `limit`. Returns `(seq, body, legal_takedown_ref)` where `body` is the
/// inner sealed payload (the envelope's `sealed_payload`).
///
/// **Legal-obligation relay-withhold gate.** When a record's
/// `legal_takedown_ref` is `Some`, its sealed body is **withheld**: this returns
/// an empty `body` + the reference and **never reads the sealed bytes** from the
/// segment file. Every conv serve surface (local `channel.fetch`, cross-nest
/// `federation.channel.fetch`, paired-nest `mls.pull`) flows through here, so the
/// withhold is enforced once, at the read primitive — no serve path can bypass
/// it (`moderation.md` § Categories & enforcement item 1). The record keeps its
/// `seq` slot so the caller renders a tombstone in place.
///
/// A `segment_records` row whose record is absent from the segment file
/// (mirror divergence) is warned-and-skipped, mirroring
/// `mail::read_envelopes_bulk`. Reads take no conv lock.
pub async fn read_after_seq(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    channel_id: &[u8; 32],
    after_seq: i64,
    limit: i64,
) -> Result<Vec<(i64, Vec<u8>, Option<String>)>> {
    let rows = cache_db
        .segment_records_list_conv_after_seq(channel_id, after_seq, limit)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (seq, segment_id, cid, legal_ref) in rows {
        if legal_ref.is_some() {
            // Withheld: never load the sealed bytes; the tombstone stands in.
            out.push((seq, Vec::new(), legal_ref));
        } else if let Some(body) = read_one(mgr, channel_id, segment_id, &cid).await? {
            out.push((seq, body, None));
        }
    }
    Ok(out)
}

/// Read live conv records across many channels with `seq > after_seq`, oldest
/// first, up to `limit` total. Returns `(seq, channel_id, body,
/// legal_takedown_ref)`. Withholds taken-down bodies exactly like
/// [`read_after_seq`].
pub async fn read_for_scopes_after_seq(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    channel_ids: &[[u8; 32]],
    after_seq: i64,
    limit: i64,
) -> Result<Vec<(i64, [u8; 32], Vec<u8>, Option<String>)>> {
    let rows = cache_db
        .segment_records_list_conv_for_scopes_after_seq(channel_ids, after_seq, limit)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (seq, channel_id, segment_id, cid, legal_ref) in rows {
        if legal_ref.is_some() {
            out.push((seq, channel_id, Vec::new(), legal_ref));
        } else if let Some(body) = read_one(mgr, &channel_id, segment_id, &cid).await? {
            out.push((seq, channel_id, body, None));
        }
    }
    Ok(out)
}

/// Tombstone every live conv record with `seq <= up_to_seq` across the supplied
/// channels. Pure mirror update — no `SegmentManager` needed (the segment file
/// is reclaimed later by compaction). Returns the total rows newly tombstoned.
pub async fn tombstone_up_to_seq(
    cache_db: &CacheDb,
    channel_ids: &[[u8; 32]],
    up_to_seq: i64,
) -> Result<usize> {
    cache_db
        .segment_records_tombstone_conv_up_to_seq(channel_ids, up_to_seq)
        .await
}

/// Compact one bucket of a channel's conv segments per the supplied
/// `CompactionPlan`. Conv sibling of [`super::mail::compact_bucket`].
///
/// The kind-agnostic mechanics are identical to mail's, and are now literally
/// shared as [`super::compact_bucket_with`] — pre-fetch the live
/// `(segment_id, record_id)` set, run `SegmentManager::compact_with_filter`
/// (file-level rewrite + manifest swap under the per-scope mutex), then rebuild
/// the `segment_records` mirror for the new segment under one SQL tx. The
/// **only** conv-specific bit is the floor decode: each survivor's floor blob
/// is a [`fauna_mls::segments::ConvFloorMetadata`] (not `MailFloorMetadata`),
/// and the rebuilt mirror row carries its `seq` (mail-floor columns NULL) via
/// [`NewConvSegmentRecord`] / [`records_db::apply_conv_compaction_tx`].
///
/// **Lock ordering invariant.** `compact_with_filter` holds the per-scope mutex
/// internally and releases it before the shared skeleton takes the `CacheDb`
/// conn mutex; the process-wide order is `per-scope-mutex → conn-mutex`.
pub async fn compact_bucket(
    mgr: &SegmentManager,
    cache_db: &Arc<CacheDb>,
    channel_id: &[u8; 32],
    plan: &CompactionPlan,
) -> Result<BucketCompactionOutcome> {
    super::compact_bucket_with(
        mgr,
        cache_db,
        channel_id,
        "conv",
        plan,
        |rid, floor_bytes| {
            // Each survivor's floor blob decodes to a ConvFloorMetadata
            // carrying `seq`; `received_at` is epoch **milliseconds**.
            let floor = fauna_mls::segments::parse_floor(floor_bytes)
                .map_err(|e| anyhow::anyhow!("decode conv floor metadata: {e}"))?;
            Ok(NewConvSegmentRecord {
                record_cid: rid,
                bucket: fauna_segment_store::bucket_for(floor.received_at / 1000),
                received_at: floor.received_at,
                seq: floor.seq,
            })
        },
        |tx, new_segment, new_records| {
            records_db::apply_conv_compaction_tx(
                tx,
                channel_id,
                &plan.inputs,
                new_segment,
                new_records,
            )
            .context("apply_conv_compaction_tx")
        },
    )
    .await
}

/// Read one conv record's inner sealed payload from its segment. Returns `None`
/// (with a warning) when the mirror points at a record absent from the segment
/// file — a divergence we tolerate rather than fail the whole batch on.
async fn read_one(
    mgr: &SegmentManager,
    channel_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
) -> Result<Option<Vec<u8>>> {
    match mgr.read_envelope_bytes(channel_id, segment_id, cid).await {
        Ok(bytes) => {
            let env = fauna_mls::segments::ConvRecordEnvelope::decode(&bytes)
                .map_err(|e| anyhow::anyhow!("decode conv envelope: {e}"))?;
            Ok(Some(env.sealed_payload))
        }
        Err(ManagerError::RecordNotFound { .. }) => {
            tracing::warn!(
                channel = ?channel_id,
                record_cid = ?cid,
                segment_id,
                "segment_records mirror diverged from segment file (conv read)"
            );
            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!("read_envelope_bytes (conv): {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Returns (tempdir, manager, cache_db). Drop the tempdir last so segment
    /// files outlive the manager. The `CacheDb` is wrapped in `Arc` because
    /// `compact_bucket` (Plan 8) takes `&Arc<CacheDb>`; the append/read paths
    /// take `&CacheDb` and deref-coerce from `&Arc<CacheDb>` cleanly.
    fn setup() -> (TempDir, SegmentManager, Arc<CacheDb>) {
        let tmp = TempDir::new().expect("tmp");
        let manager = SegmentManager::new(tmp.path().to_path_buf(), "conv");
        let cache_db = Arc::new(CacheDb::open_in_memory().expect("in-memory cache db"));
        (tmp, manager, cache_db)
    }

    /// The filing identity of a conv record with this sealed payload, through
    /// the production mint — never a hand-rolled twin (the cal/card leg's trap
    /// 1: a test-only stand-in lets a test assert an identity production could
    /// never produce).
    fn record_cid_of(body: &[u8]) -> Cid {
        fauna_mls::segments::derive_record_cid(body).expect("derive conv record cid")
    }

    #[tokio::test]
    async fn append_then_read_round_trip() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x21u8; 32];
        let body = b"sealed-conv-payload".to_vec();

        let outcome = append(&mgr, &cache_db, &channel, &body, 1_715_000_000_000)
            .await
            .expect("append");
        assert_eq!(outcome.seq, 1);
        assert_eq!(outcome.seg_id, 1);
        assert!(outcome.finalized.is_none());

        let rows = read_after_seq(&mgr, &cache_db, &channel, 0, 100)
            .await
            .expect("read");
        assert_eq!(rows, vec![(1i64, body, None)]);
    }

    /// The nest-attested author (`conv_record_authors`): the actor authenticated
    /// on the send lands beside the mirror row under the record's
    /// `(channel, seq)`; a send that authenticated nobody records none (the
    /// wire field then stays unset — *no answer*); and a byte replay by a
    /// DIFFERENT actor dedups to the first seq without rewriting whose record
    /// it was — an attestation is written once.
    #[tokio::test]
    async fn append_records_the_attested_author_once_per_seq() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x3du8; 32];
        let organizer = [0xa1u8; 32];
        let other = [0xb2u8; 32];
        let body = b"sealed-scheduling-imip".to_vec();

        let first = append_with_refs(
            &mgr,
            &cache_db,
            &channel,
            &body,
            1_715_000_000_000,
            &[],
            Some(&organizer),
        )
        .await
        .expect("append");
        let unattested = append_with_refs(
            &mgr,
            &cache_db,
            &channel,
            b"a record nobody was authenticated for",
            1_715_000_001_000,
            &[],
            None,
        )
        .await
        .expect("append unattested");
        let replay = append_with_refs(
            &mgr,
            &cache_db,
            &channel,
            &body,
            1_715_000_002_000,
            &[],
            Some(&other),
        )
        .await
        .expect("replay");
        assert_eq!(
            replay.seq, first.seq,
            "a byte replay dedups to the first seq"
        );

        let authors = cache_db
            .conv_record_authors_in_range(&channel, 0, unattested.seq)
            .await
            .expect("authors");
        assert_eq!(authors.get(&first.seq), Some(&organizer));
        assert_eq!(authors.get(&unattested.seq), None);
        assert_eq!(authors.len(), 1);

        // The range is `(after, up_to]` — the page the serve joins it onto.
        assert!(
            cache_db
                .conv_record_authors_in_range(&channel, first.seq, unattested.seq)
                .await
                .expect("authors past the cursor")
                .is_empty()
        );
    }

    /// The conversation kind's blob-reachability floor: the sender's plaintext
    /// `attachment_refs` land beside the mirror row in the same transaction,
    /// keyed by the record's `(channel, seq)`; a byte replay of the same
    /// envelope dedups to the first record's seq and leaves the first
    /// append's refs standing (it writes nothing). A record with no
    /// attachments records no rows.
    #[tokio::test]
    async fn append_with_refs_records_them_beside_the_mirror_row_and_a_replay_keeps_them() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x2cu8; 32];
        let body = b"sealed-conv-payload-with-attachments".to_vec();
        let photo = [0xa1u8; 32];
        let file = [0xb2u8; 32];

        let first = append_with_refs(
            &mgr,
            &cache_db,
            &channel,
            &body,
            1_715_000_000_000,
            &[file, photo],
            None,
        )
        .await
        .expect("append");
        assert_eq!(first.seq, 1);
        assert_eq!(
            cache_db
                .conv_attachment_refs(&channel, 1)
                .await
                .expect("refs"),
            vec![photo, file],
            "both refs recorded under the record's seq, sorted by hash"
        );
        assert_eq!(
            cache_db
                .list_live_conv_attachment_refs()
                .await
                .expect("live")
                .len(),
            2,
            "a live record's refs are what the GC pins"
        );

        // Byte replay: same seq back, no second row, the first refs stand.
        let replay = append_with_refs(
            &mgr,
            &cache_db,
            &channel,
            &body,
            1_715_000_001_000,
            &[],
            None,
        )
        .await
        .expect("replay");
        assert_eq!(
            replay.seq, 1,
            "a byte replay dedups to the first record's seq"
        );
        assert_eq!(
            cache_db
                .conv_attachment_refs(&channel, 1)
                .await
                .expect("refs"),
            vec![photo, file]
        );

        // A plain record names nothing.
        let plain = append(
            &mgr,
            &cache_db,
            &channel,
            b"sealed-plain-text",
            1_715_000_002_000,
        )
        .await
        .expect("append plain");
        assert_eq!(plain.seq, 2);
        assert!(
            cache_db
                .conv_attachment_refs(&channel, 2)
                .await
                .expect("refs")
                .is_empty()
        );

        // Tombstoning the record drops it from the live set — the GC's
        // liveness predicate — without touching the rows themselves.
        let n = tombstone_up_to_seq(&cache_db, &[channel], 1)
            .await
            .expect("tombstone");
        assert_eq!(n, 1);
        assert!(
            cache_db
                .list_live_conv_attachment_refs()
                .await
                .expect("live")
                .is_empty(),
            "a tombstoned record pins nothing"
        );
        assert_eq!(
            cache_db
                .conv_attachment_refs(&channel, 1)
                .await
                .expect("refs")
                .len(),
            2,
            "the rows themselves are never deleted"
        );
    }

    /// Legal-obligation relay-withhold: a conv record flagged with a
    /// `legal_takedown_ref` is served with its sealed body **withheld** (empty)
    /// and the reference surfaced, and the sealed bytes are never read from the
    /// segment file. Clearing the flag re-serves the original body
    /// (tombstone-not-delete). This is the single gate every conv serve surface
    /// (`channel.fetch`, `federation.channel.fetch`, `mls.pull`) inherits.
    #[tokio::test]
    async fn legal_takedown_withholds_body_and_restore_re_serves() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x2bu8; 32];
        let body = b"sealed-illegal-payload".to_vec();

        let outcome = append(&mgr, &cache_db, &channel, &body, 1_715_000_000_000)
            .await
            .expect("append");
        let record_cid = record_cid_of(&body);

        // Take it down.
        let n = cache_db
            .set_conv_legal_takedown(&record_cid, Some("EU-DSA-2024/999"))
            .await
            .expect("set");
        assert_eq!(n, 1, "one conv record flagged");

        let rows = read_after_seq(&mgr, &cache_db, &channel, 0, 100)
            .await
            .expect("read withheld");
        assert_eq!(rows.len(), 1, "the record keeps its seq slot (tombstone)");
        assert_eq!(rows[0].0, outcome.seq);
        assert!(rows[0].1.is_empty(), "sealed body is withheld");
        assert_eq!(rows[0].2.as_deref(), Some("EU-DSA-2024/999"));

        // The cross-scope read withholds identically.
        let scoped = read_for_scopes_after_seq(&mgr, &cache_db, &[channel], 0, 100)
            .await
            .expect("scoped read withheld");
        assert_eq!(scoped.len(), 1);
        assert!(scoped[0].2.is_empty(), "cross-scope body withheld too");
        assert_eq!(scoped[0].3.as_deref(), Some("EU-DSA-2024/999"));

        // Restore (tombstone-not-delete): the original body re-serves.
        let n = cache_db
            .set_conv_legal_takedown(&record_cid, None)
            .await
            .expect("clear");
        assert_eq!(n, 1);
        let rows = read_after_seq(&mgr, &cache_db, &channel, 0, 100)
            .await
            .expect("read restored");
        assert_eq!(rows, vec![(outcome.seq, body, None)]);
    }

    /// `conv_record_scope_and_takedown` yields the channel + current flag, and
    /// `None` for an unknown record (→ the handler's `not_found`).
    #[tokio::test]
    async fn conv_record_scope_and_takedown_lookup() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x2cu8; 32];
        let body = b"lookup-body".to_vec();
        append(&mgr, &cache_db, &channel, &body, 1_715_000_000_000)
            .await
            .expect("append");
        let record_cid = record_cid_of(&body);

        let (scope, flag) = cache_db
            .conv_record_scope_and_takedown(&record_cid)
            .await
            .expect("lookup")
            .expect("record exists");
        assert_eq!(scope, channel);
        assert_eq!(flag, None, "live message has no takedown ref");

        // Unknown record → None.
        let missing = fauna_cbor::Cid::from_digest_dag_cbor([0x99u8; 32]);
        assert!(
            cache_db
                .conv_record_scope_and_takedown(&missing)
                .await
                .expect("lookup missing")
                .is_none()
        );
    }

    /// **The identity rule, at the production append.** The record a channel
    /// stores is filed under the content hash of its own stored bytes — the
    /// predicate `fauna_account_store::segments::admit` re-checks on every
    /// adopted block, which is what makes conv adoptable at all
    /// (`message-segment-store.md` § Record identity per kind). Asserted by
    /// reading the block back out of the segment file and re-hashing it, not by
    /// re-deriving the same expression the writer used.
    #[tokio::test]
    async fn a_stored_record_hashes_to_the_cid_it_is_filed_under() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x2du8; 32];
        let body = b"sealed-identity-payload".to_vec();

        let outcome = append(&mgr, &cache_db, &channel, &body, 1_715_000_000_000)
            .await
            .expect("append");

        // The mirror's key for this record, straight from the DB.
        let cid = record_cid_of(&body);
        let found = cache_db
            .segment_records_lookup_record(&channel, "conv", &cid)
            .await
            .expect("lookup")
            .expect("the record is filed under its content hash");
        assert_eq!(found.segment_id, outcome.seg_id);
        assert_eq!(found.seq, Some(outcome.seq));

        // …and the bytes at rest hash to exactly that key.
        let stored = mgr
            .read_envelope_bytes(&channel, outcome.seg_id, &cid)
            .await
            .expect("read stored envelope bytes");
        assert!(
            cid.matches(&stored),
            "the filing cid must be blake3 over the very bytes stored under it"
        );
    }

    /// **A byte replay is one record, not two.** Re-appending the identical
    /// envelope to the same channel hits the scoped pre-append dedup: the first
    /// record's `seq` comes back, no second seq is consumed, and nothing is
    /// written — which is what makes a retried `channel.send` idempotent
    /// instead of duplicating the message. (Pre-cutover this was impossible:
    /// seq was inside the identity, so a replay simply became a second record.)
    #[tokio::test]
    async fn an_identical_replay_dedups_to_the_first_records_seq() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x2eu8; 32];
        let body = b"sealed-replayed-payload".to_vec();

        let first = append(&mgr, &cache_db, &channel, &body, 1_715_000_000_000)
            .await
            .expect("first append");
        let replay = append(&mgr, &cache_db, &channel, &body, 1_715_000_999_000)
            .await
            .expect("replayed append");

        assert_eq!(replay.seq, first.seq, "the replay answers the first seq");
        assert_eq!(replay.seg_id, first.seg_id);
        assert!(replay.finalized.is_none());

        let rows = read_after_seq(&mgr, &cache_db, &channel, 0, 100)
            .await
            .expect("read");
        assert_eq!(rows, vec![(first.seq, body.clone(), None)], "one record");

        // The next distinct body still takes the next seq — the dedup consumed
        // no coordinate.
        let next = append(
            &mgr,
            &cache_db,
            &channel,
            b"a different body",
            1_715_001_000_000,
        )
        .await
        .expect("next append");
        assert_eq!(next.seq, first.seq + 1);
    }

    /// The same bytes on a DIFFERENT channel are a different channel's record —
    /// the reason the dedup is `(scope_id, kind, record_cid)`-scoped and never
    /// scope-agnostic. The channel left the hash with the cutover, so a
    /// scope-blind dedup would silently drop the second channel's message.
    #[tokio::test]
    async fn the_same_bytes_on_another_channel_are_a_separate_record() {
        let (_tmp, mgr, cache_db) = setup();
        let ch_a = [0x2fu8; 32];
        let ch_b = [0x30u8; 32];
        let body = b"identical-across-channels".to_vec();

        let a = append(&mgr, &cache_db, &ch_a, &body, 1_715_000_000_000)
            .await
            .expect("append a");
        let b = append(&mgr, &cache_db, &ch_b, &body, 1_715_000_000_000)
            .await
            .expect("append b");
        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 1, "channel b allocates its own first seq");

        let rows_a = read_after_seq(&mgr, &cache_db, &ch_a, 0, 100)
            .await
            .expect("read a");
        let rows_b = read_after_seq(&mgr, &cache_db, &ch_b, 0, 100)
            .await
            .expect("read b");
        assert_eq!(rows_a, vec![(1i64, body.clone(), None)]);
        assert_eq!(rows_b, vec![(1i64, body, None)], "b kept its own copy");
    }

    #[tokio::test]
    async fn seq_is_monotonic_per_channel() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x22u8; 32];
        for expected in 1..=3i64 {
            let outcome = append(
                &mgr,
                &cache_db,
                &channel,
                format!("body-{expected}").as_bytes(),
                1_715_000_000_000,
            )
            .await
            .expect("append");
            assert_eq!(outcome.seq, expected);
        }
    }

    #[tokio::test]
    async fn cross_scope_read_merges_ordered_by_seq() {
        let (_tmp, mgr, cache_db) = setup();
        let ch1 = [0x23u8; 32];
        let ch2 = [0x24u8; 32];
        // ch1 gets seqs 1,2; ch2 gets seq 1.
        append(&mgr, &cache_db, &ch1, b"c1-1", 1_715_000_000_000)
            .await
            .expect("c1-1");
        append(&mgr, &cache_db, &ch1, b"c1-2", 1_715_000_000_000)
            .await
            .expect("c1-2");
        append(&mgr, &cache_db, &ch2, b"c2-1", 1_715_000_000_000)
            .await
            .expect("c2-1");

        let rows = read_for_scopes_after_seq(&mgr, &cache_db, &[ch1, ch2], 0, 100)
            .await
            .expect("read");
        // Ordered by seq ASC: 1, 1, 2.
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 1, 2]);
        // Both channels' bodies present.
        let bodies: Vec<&[u8]> = rows.iter().map(|r| r.2.as_slice()).collect();
        assert!(bodies.contains(&b"c1-1".as_slice()));
        assert!(bodies.contains(&b"c1-2".as_slice()));
        assert!(bodies.contains(&b"c2-1".as_slice()));
    }

    #[tokio::test]
    async fn tombstone_hides_records_from_read() {
        let (_tmp, mgr, cache_db) = setup();
        let channel = [0x25u8; 32];
        append(&mgr, &cache_db, &channel, b"first", 1_715_000_000_000)
            .await
            .expect("first");
        append(&mgr, &cache_db, &channel, b"second", 1_715_000_000_000)
            .await
            .expect("second");

        let n = tombstone_up_to_seq(&cache_db, &[channel], 1)
            .await
            .expect("tombstone");
        assert_eq!(n, 1);

        let rows = read_after_seq(&mgr, &cache_db, &channel, 0, 100)
            .await
            .expect("read");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 2, "only seq 2 survives");
        assert_eq!(rows[0].1, b"second".to_vec());
    }

    /// Plan 8 T1: `compact_bucket` rewrites a tombstoned bucket. Append three
    /// conv records in the same bucket, finalize, tombstone seq <= 1, then
    /// compact the bucket's segment. The surviving records (seq 2, 3) are
    /// readable via `read_after_seq` with their original seq values, and the
    /// tombstoned record (seq 1) is gone.
    #[tokio::test]
    async fn compact_bucket_drops_tombstoned_and_keeps_survivors() {
        let (_tmp, mgr, cache_db) = setup();
        // Distinct channel id — for-test segment dirs are PID-shared.
        let channel = [0x2au8; 32];

        // Three appends, same bucket (May 2024) → all land in segment 1.
        for s in 1..=3i64 {
            let outcome = append(
                &mgr,
                &cache_db,
                &channel,
                format!("conv-body-{s}").as_bytes(),
                1_715_000_000_000,
            )
            .await
            .expect("append");
            assert_eq!(outcome.seq, s);
            assert_eq!(outcome.seg_id, 1, "same bucket → one segment");
        }
        // Finalize the open segment so compaction can read it back.
        mgr.finalize_open(&channel).await.expect("finalize");

        // Tombstone seq <= 1 in the mirror (the dead record).
        let n = tombstone_up_to_seq(&cache_db, &[channel], 1)
            .await
            .expect("tombstone");
        assert_eq!(n, 1);

        // Compact segment 1's bucket.
        let plan = CompactionPlan {
            inputs: vec![1],
            bucket: "2024-05".to_string(),
        };
        let outcome = compact_bucket(&mgr, &cache_db, &channel, &plan)
            .await
            .expect("compact");
        assert_eq!(outcome.consumed, vec![1]);
        assert!(
            outcome.new_segment.is_some(),
            "two records survived → a new segment was written"
        );

        // The survivors (seq 2, 3) are readable with their original seq values;
        // the tombstoned seq 1 is gone.
        let rows = read_after_seq(&mgr, &cache_db, &channel, 0, 100)
            .await
            .expect("read");
        assert_eq!(
            rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![2, 3],
            "only the non-tombstoned records survive compaction, original seq preserved"
        );
        assert_eq!(rows[0].1, b"conv-body-2".to_vec());
        assert_eq!(rows[1].1, b"conv-body-3".to_vec());
    }
}
