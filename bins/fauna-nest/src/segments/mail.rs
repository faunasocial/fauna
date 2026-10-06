//! Mail-kind nest-side segment coordination.
//!
//! Plan 6 retired the `MailSegmentManager` wrapper: the file/manifest state
//! machine is the kind-agnostic `fauna_segment_store::SegmentManager`
//! (`AppState.mail_segments`, registered with `kind = "mail"`), and the
//! mail-specific encoding lives in `fauna_mail::segments::ops`. What remains
//! genuinely mail-specific *and* nest-specific is the coordination between the
//! `SegmentManager` and the `segment_records` SQLite mirror — that lives here,
//! as free functions taking `(&SegmentManager, &CacheDb, …)`. The mirror is a
//! nest type (`CacheDb`), so this glue cannot move into the shared `fauna-mail`
//! crate; conv/calendar grow sibling modules (`segments::conv`, …) when their
//! plans land, each composing the same `SegmentManager` against their own
//! mirror columns. The shared *pattern* extracts once two kinds exist to shape
//! it (deferred, same as the Plan 7 backup-coordinator registry).
//!
//! ### Retained invariants (unchanged from the former manager)
//!
//! * Segment file write + `segment_records` mirror INSERT happen under the
//!   per-scope mutex held by `SegmentManager::append_record_with_bucket`.
//! * read-your-own-writes policy: every read path ([`read_envelope`],
//!   [`read_envelopes_bulk`], [`read_record_with_floor`]) flushes the open
//!   segment (via `SegmentManager`) before opening segments.
//! * Manifest is saved atomically to disk by `SegmentManager` (the flush is
//!   implicit in the manager's inner append/compact paths).
//!
//! ### Layout on disk
//!
//! `<data_dir>/__mail/<actor_hex>/seg-NNNNNNNN.dat` and
//! `<data_dir>/__mail/<actor_hex>/manifest.mail`. `SegmentManager` derives the
//! same layout via `kind = "mail"`.

use anyhow::{Context, Result};
use dashmap::DashMap;
use fauna_cbor::Cid;
use fauna_mail::segments::{
    CONTINUATION_ROLE_HEAD, CONTINUATION_ROLE_PART, MailContinuationHead, MailFloorMetadata,
    MailRecord, MailRecordEnvelope, bucket_for,
};
use fauna_mail::transport_limits::MAIL_BODY_PART_CAP_BYTES;
use fauna_mls::wrapped_blob::SealedRecordBytes;
use fauna_segment_store::{CompactionPlan, SegmentManager};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use super::records_db::SegmentRecordRef;
use crate::db::CacheDb;

/// This nest's local storage time (epoch ms) for a record about to be appended —
/// the value of [`fauna_mail::segments::MailFloorMetadata::stored_at`].
///
/// Deliberately a wall-clock read at the append itself rather than a caller-
/// supplied argument: every append path must stamp it locally, and the relay
/// path must *overwrite* a peer's value with it. Handing it to callers would let
/// one forget and silently reintroduce the reaper data-loss bug this exists to
/// prevent. A clock that fails to read yields `0` = unknown, which the reaper
/// treats as not-reapable — the fail-safe direction (leak, never lose).
fn stored_at_now_ms() -> i64 {
    fauna_core::data::Timestamp::now_millis_or_zero() as i64
}

// The client-feed REFERENCE serve (an over-frame envelope crosses as
// `InboxMessage.body_ref`) was gated on a deployed-reader flag until every
// client carried the feed-side resolver; the gate flipped 2026-07-18 and left
// with the compat-remnant sweep (`version-compatibility.md` § Dimension 2) —
// `mailbox_fetch` always serves by reference now.

/// Per-actor seq-allocation locks. Sibling of `segments::conv`'s `SEQ_LOCKS`:
/// mail's per-actor `seq` is a monotonic cursor (`MAX(seq)+1`), so the
/// query→append→mirror-insert chain must be atomic per actor or two concurrent
/// appends could allocate the same seq. Unlike conv, mail's `record_id` is
/// externally derived (a message-id hash), not seq-derived — but the seq stamp
/// still needs serializing. Lock order: mail-actor-lock → segment-scope-mutex
/// (inside `ops::append`) → CacheDb conn-mutex (inside the mirror insert). Reads
/// need no lock.
static SEQ_LOCKS: LazyLock<DashMap<[u8; 32], Arc<Mutex<()>>>> = LazyLock::new(DashMap::new);

fn seq_lock_for(actor_id: &[u8; 32]) -> Arc<Mutex<()>> {
    super::keyed_seq_lock(&SEQ_LOCKS, actor_id)
}

/// Result of [`append_record`]. Plan 5 T6 extended the prior
/// `(segment_id, byte_offset)` tuple with `finalized: Option<u32>` so callers
/// can emit `fauna.segments.changed { change: Finalized }` pushes on rotation:
/// the wire-level signal a client-mediated backup destination listens for
/// (per `docs/goal/behavior/mail-segment-backup.md`).
///
/// `finalized.is_some()` exactly when the append rotated the actor's open
/// segment (bucket change, or first append after a process restart). The
/// returned id is the segment that just closed — *not* the segment the new
/// record landed in (that's `seg_id`).
///
/// `byte_offset` is gone post-Layer 3 — the CARv2 segment-store API addresses
/// records by Cid, not `(offset, length)`. The `segment_records` mirror has no
/// `byte_offset` column (retired); reads resolve the Cid through the CARv2 index.
#[derive(Debug, Clone, Copy)]
pub struct AppendOutcome {
    /// Segment id the record landed in (live tail of the manifest after this
    /// append). On a dedup hit (`inserted == false`), the segment the existing
    /// record already lives in.
    pub seg_id: u32,
    /// `Some(closed_seg_id)` if this append caused a rotation — the
    /// previously-open segment was finalized to disk in the process. `None` on
    /// no-rotation appends.
    pub finalized: Option<u32>,
    /// The record's filing identity: `Cid::of_dag_cbor(<stored block bytes>)`,
    /// derived — never caller-supplied — at the append itself
    /// (`message-segment-store.md` § Record identity per kind). Its 32-byte
    /// digest is what the bridge/aux layers carry as the `message_id`.
    pub cid: Cid,
    /// `false` when the scoped pre-append dedup found `(scope_id, "mail", cid)`
    /// already live in the mirror — a literal byte replay within this scope —
    /// and appended nothing. The dedup is scope-qualified by construction: the
    /// actor no longer lives inside the hash, so the same bytes landing in a
    /// different scope are that scope's own record.
    pub inserted: bool,
}

/// Result of [`compact_bucket`]. Plan 5 T6 extended the prior `Option<u32>`
/// return with the list of consumed input segment ids so callers can emit
/// `fauna.segments.changed { CompactedIn / CompactedOut }` pushes —
/// `CompactedIn` for `new_segment`, `CompactedOut` for each `consumed`.
#[derive(Debug, Clone, Default)]
pub struct BucketCompactionOutcome {
    /// New segment id, `Some` if at least one record survived the compaction.
    /// `None` when every input was tombstoned.
    pub new_segment: Option<u32>,
    /// Input segment ids that compaction consumed (regardless of
    /// `new_segment.is_some()`). These move to the manifest's
    /// `tombstoned_segments` list and become eligible for retention GC.
    pub consumed: Vec<u32>,
}

/// Per-segment metadata for `fauna.segments.list` (Plan 5). Read once per
/// segment via [`describe_segment_for_backup`].
#[derive(Debug, Clone)]
pub struct SegmentBackupMeta {
    /// BLAKE3 of the framed segment file as a whole (header + payload + footer +
    /// trailer). Authoritative integrity hash advertised in the list reply;
    /// destinations re-hash after chunked download.
    pub file_blake3: [u8; 32],
    /// BLAKE3 of the `.meta` sidecar — the pair's other half, advertised as
    /// `SegmentRef.meta_blake3_hex` so the backup corpus anchors both files
    /// (`message-segment-store.md` § Cross-location backup protocol).
    pub meta_blake3: [u8; 32],
    pub size_bytes: u64,
    pub bucket: String,
    pub record_count: u32,
    /// Count of tombstoned `segment_records` rows for this segment. Live
    /// segments with non-zero tombstone counts are compaction candidates;
    /// backup destinations replicate the framed file verbatim either way.
    pub tombstone_count: u32,
    pub created_at_secs: u64,
}

/// Append one mail record. Coordinates: encode envelope, encode floor
/// metadata, append to the segment, INSERT `segment_records` via `CacheDb`,
/// save manifest atomically.
///
/// The body and index hint arrive as [`SealedRecordBytes`] — the S6.12b
/// structural seal gate (see `segments::cal::append_record` for the
/// rationale; `__mail` is backup-eligible, so nothing unsealed may enter it,
/// and demanding the proven-sealed type here is what made the `import_message`
/// class of miss a compile error). The outer `MailRecordEnvelope` is
/// constructed only here, so no caller can smuggle raw bytes around the gate;
/// the verbatim at-rest carrier [`append_sealed_record`] operates on
/// pre-encoded outer-envelope bytes below this boundary.
///
/// Returns an [`AppendOutcome`]:
/// - `seg_id` — segment the record landed in (live tail).
/// - `finalized` — `Some(closed_seg_id)` if the append rotated a
///   previously-open segment closed; `None` otherwise. Plan 5 T6 callers emit
///   `fauna.segments.changed { change: Finalized }` when this is `Some`.
///
/// `floor.received_at` is the server-assigned receive time (epoch ms); the
/// bucket is computed by dividing it to seconds.
pub async fn append_record(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    encrypted_body: &SealedRecordBytes,
    encrypted_index_hint: &SealedRecordBytes,
    mut floor: MailFloorMetadata,
) -> Result<AppendOutcome> {
    // Continuation route: a sealed body over the per-part cap rests as N part
    // records + one v3 head, so every stored record stays frame-sized. This is
    // the route every over-1-MiB body takes — which is why the per-record guard
    // below only ever fires as an internal invariant on the <= 1 MiB path, never
    // as a message ceiling (message-segment-store.md § Continuation records;
    // smtp-server.md § Message size limits).
    if encrypted_body.as_slice().len() > MAIL_BODY_PART_CAP_BYTES as usize {
        return append_continuation_record(
            mgr,
            cache_db,
            actor_id,
            encrypted_body,
            encrypted_index_hint,
            floor,
        )
        .await;
    }

    let envelope = MailRecordEnvelope::new(
        encrypted_body.as_slice().to_vec(),
        encrypted_index_hint.as_slice().to_vec(),
    );

    // Encode once: the same bytes feed the size guard, the filing identity, and
    // the file append (the filing CID is their content hash, so the encode is
    // no longer conditional — identity needs it on every path).
    let (cid, env_bytes) = fauna_mail::segments::ops::encode_record(&envelope)
        .map_err(|e| anyhow::anyhow!("encode mail record envelope: {e}"))?;

    // A record larger than `MAX_RECORD_LEN` is refused **on read** by the CARv2
    // parser — but nothing refused it on write, so such a record was stored happily
    // and then became permanently unreadable: mail accepted with a `250` and lost.
    // That could not be reached while the SMTP perimeter clamped at ~1.5 MB; it
    // becomes reachable the moment a body can cross the wire by reference, so the
    // guard lands with the legs that make it reachable.
    //
    // This sits at the single choke point every ingest leg passes through (inbound
    // ingest, IMAP APPEND, mailbox import), so no caller can route around it — the
    // APPEND leg in particular has no perimeter ceiling of its own today.
    if env_bytes.len() > fauna_carv2::v1::MAX_RECORD_LEN {
        anyhow::bail!(
            "sealed mail record would be {} bytes at rest, over the {} byte \
             CARv2 record cap — it would be unreadable once written. Refusing to store it \
             (smtp-server.md § Message size limits).",
            env_bytes.len(),
            fauna_carv2::v1::MAX_RECORD_LEN,
        );
    }

    // Hold the per-actor seq lock across dedup → allocate → append →
    // mirror-insert so the per-actor monotonic cursor never duplicates under
    // concurrent appends (mirrors conv's per-channel seq lock), and so the
    // scoped dedup check-then-append is atomic per scope (two concurrent
    // identical ingests serialize here; the second sees the first's mirror
    // row). The allocated seq is stamped into BOTH the floor (segment footer —
    // the authority) and the mirror row, so a mirror rebuild from the floor
    // recovers the same cursor.
    let lock = seq_lock_for(actor_id);
    let _guard = lock.lock().await;

    // Scoped pre-append dedup — `(scope_id, kind, record_cid)`, never
    // scope-agnostic: the actor is no longer inside the hash, so the same bytes
    // in a different scope are a different scope's record (§ Record identity
    // per kind, pre-check 2's obligation). A hit is a literal byte replay of
    // this scope's own record: the segment file append is not idempotent
    // (a duplicate offset for the same cid), so it must be caught here.
    if let Some(existing) = cache_db
        .segment_records_lookup_record(actor_id, "mail", &cid)
        .await?
    {
        return Ok(AppendOutcome {
            seg_id: existing.segment_id,
            finalized: None,
            cid,
            inserted: false,
        });
    }

    let bucket = bucket_for(floor.received_at / 1000);
    let seq = cache_db.segment_records_next_mail_seq(actor_id).await?;
    floor.seq = seq;
    // Local storage time — always now, never the message's `received_at` (they
    // coincide here at the origin, but not on the relay path). See
    // `MailFloorMetadata::stored_at`.
    floor.stored_at = stored_at_now_ms();

    let outcome = fauna_mail::segments::ops::append_encoded(mgr, actor_id, cid, &env_bytes, &floor)
        .await
        .map_err(|e| anyhow::anyhow!("ops::append_encoded: {e}"))?;

    // Mirror INSERT — the kind-agnostic SegmentManager has no DB coupling; this
    // call is the sole place where the segment_records row is written for mail
    // appends. No byte_offset column: reads address the record by its Cid
    // through the CARv2 MultihashIndexSorted index, never a mirrored offset.
    cache_db
        .segment_records_insert_mail(
            actor_id,
            outcome.segment_id,
            &cid,
            &bucket,
            floor.received_at,
            &floor.sender_domain,
            &floor.spam_disposition,
            floor.is_own_submission,
            seq,
            (!floor.report_hash.is_empty()).then_some(floor.report_hash.as_slice()),
            floor.continuation_role,
            floor.stored_at,
        )
        .await
        .context("insert segment_records mirror row")?;

    Ok(AppendOutcome {
        seg_id: outcome.segment_id,
        finalized: outcome.finalized,
        cid,
        inserted: true,
    })
}

/// Store an over-cap sealed body as **continuation records**: N *part* records
/// (raw ciphertext ranges of the one seal) followed by one *head* record
/// (envelope v3, pinning the ordered part-CID list + total). Every emitted
/// record is frame-sized, so the whole message relays inside the 2 MiB
/// federation frame (`message-segment-store.md` § Continuation records;
/// `deployment-home-with-public-relay.md` § Relay frame budget rule 2).
///
/// **Invariant 3 (nothing unsealed enters a segment) holds** — the caller has
/// already produced a verified [`SealedRecordBytes`] for the *whole* body (the
/// seal check happens once, at the same wire edge as an inline record), and this
/// function only slices those already-verified bytes into ranges. Parts are not
/// themselves decodable sealed envelopes; they are admitted solely through this
/// choke point.
///
/// Ordering is load-bearing: **parts append first (consecutive seqs), the head
/// last** (the commit point — invariant 1). A crash between parts and head
/// leaves unreachable part records, reclaimed by the age-watermarked
/// headless-part reaper. Because parts carry strictly-lower seqs than the head,
/// the relay forwards them first and the ack's `tombstone_up_to_seq(head_seq)`
/// reclaims the whole family in one sweep.
///
/// Called from [`append_record`] when the continuation-write gate is on and the
/// body exceeds [`MAIL_BODY_PART_CAP_BYTES`]; also called directly by tests to
/// exercise the at-rest machinery independent of the gate.
pub async fn append_continuation_record(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    encrypted_body: &SealedRecordBytes,
    encrypted_index_hint: &SealedRecordBytes,
    mut head_floor: MailFloorMetadata,
) -> Result<AppendOutcome> {
    let body = encrypted_body.as_slice();
    let hint = encrypted_index_hint.as_slice();
    let bucket = bucket_for(head_floor.received_at / 1000);
    let part_cap = MAIL_BODY_PART_CAP_BYTES as usize;

    // Split the sealed body into consecutive ranges (each its own content-
    // addressed record: the CID digest IS blake3(range), so a relayed part
    // reconstructs the same CID at the destination) and pre-compute the head's
    // ordered part-CID list.
    let ranges: Vec<&[u8]> = body.chunks(part_cap).collect();
    let part_cids: Vec<Cid> = ranges.iter().map(|r| Cid::of_dag_cbor(r)).collect();
    let part_digests: Vec<[u8; 32]> = part_cids.iter().map(|c| c.digest()).collect();

    let head = MailContinuationHead::new(part_digests, body.len() as u64, hint.to_vec());
    let head_bytes = head
        .encode()
        .map_err(|e| anyhow::anyhow!("encode v3 continuation head: {e}"))?;
    // Every emitted record must be frame-sized (rule 2). Parts are cap-bounded by
    // construction; the head is tiny (a CID list + hint). Refuse rather than
    // silently write an unrelayable record.
    if head_bytes.len() >= fauna_carv2::v1::MAX_RECORD_LEN {
        anyhow::bail!(
            "continuation head is {} bytes, over the {} CARv2 record cap — too many parts",
            head_bytes.len(),
            fauna_carv2::v1::MAX_RECORD_LEN,
        );
    }
    // The head's filing identity is the content hash of its stored bytes, the
    // same rule as every record (§ Record identity per kind). It is the
    // message's identity: deterministic in (body, hint), so a byte replay of
    // the whole message re-derives it and dedups below.
    let head_cid = Cid::of_dag_cbor(&head_bytes);

    // One seq-lock hold for the whole family (mirrors append_record's chain).
    let lock = seq_lock_for(actor_id);
    let _guard = lock.lock().await;

    // Whole-message dedup on the head, before any part lands: parts are
    // reachable only through a head, so a present head means the full family is
    // already stored (parts append first, head last — the commit point — and a
    // headless part family is reaped, never resurrected).
    if let Some(existing) = cache_db
        .segment_records_lookup_record(actor_id, "mail", &head_cid)
        .await?
    {
        return Ok(AppendOutcome {
            seg_id: existing.segment_id,
            finalized: None,
            cid: head_cid,
            inserted: false,
        });
    }

    let mut last_finalized: Option<u32> = None;

    // Parts first — the head is the commit point, appended last.
    for (range, part_cid) in ranges.iter().zip(part_cids.iter()) {
        let seq = cache_db.segment_records_next_mail_seq(actor_id).await?;
        // A part's floor carries only what it needs: the message receive time
        // (the co-location bucket), the local storage time (the reaper's age
        // watermark), its seq, and the PART role. It has no sender/spam/placement
        // semantics — parts are invisible to every content surface.
        let part_floor = MailFloorMetadata {
            received_at: head_floor.received_at,
            stored_at: stored_at_now_ms(),
            seq,
            continuation_role: CONTINUATION_ROLE_PART,
            ..Default::default()
        };
        let part_floor_bytes = part_floor
            .encode()
            .map_err(|e| anyhow::anyhow!("encode part floor: {e}"))?;
        let outcome = mgr
            .append_record_with_bucket(actor_id, *part_cid, range, &part_floor_bytes, &bucket)
            .await
            .map_err(|e| anyhow::anyhow!("append continuation part: {e}"))?;
        if outcome.finalized.is_some() {
            last_finalized = outcome.finalized;
        }
        cache_db
            .segment_records_insert_mail(
                actor_id,
                outcome.segment_id,
                part_cid,
                &bucket,
                part_floor.received_at,
                "",
                "",
                false,
                seq,
                None,
                CONTINUATION_ROLE_PART,
                part_floor.stored_at,
            )
            .await
            .context("insert continuation part mirror row")?;
    }

    // Head last — carries the message's real floor (so placement + IMAP treat it
    // exactly like a normal record) with the HEAD role.
    let head_seq = cache_db.segment_records_next_mail_seq(actor_id).await?;
    head_floor.seq = head_seq;
    head_floor.stored_at = stored_at_now_ms();
    head_floor.continuation_role = CONTINUATION_ROLE_HEAD;
    let head_floor_bytes = head_floor
        .encode()
        .map_err(|e| anyhow::anyhow!("encode head floor: {e}"))?;
    let head_outcome = mgr
        .append_record_with_bucket(actor_id, head_cid, &head_bytes, &head_floor_bytes, &bucket)
        .await
        .map_err(|e| anyhow::anyhow!("append continuation head: {e}"))?;
    if head_outcome.finalized.is_some() {
        last_finalized = head_outcome.finalized;
    }
    cache_db
        .segment_records_insert_mail(
            actor_id,
            head_outcome.segment_id,
            &head_cid,
            &bucket,
            head_floor.received_at,
            &head_floor.sender_domain,
            &head_floor.spam_disposition,
            head_floor.is_own_submission,
            head_seq,
            (!head_floor.report_hash.is_empty()).then_some(head_floor.report_hash.as_slice()),
            CONTINUATION_ROLE_HEAD,
            head_floor.stored_at,
        )
        .await
        .context("insert continuation head mirror row")?;

    Ok(AppendOutcome {
        seg_id: head_outcome.segment_id,
        finalized: last_finalized,
        cid: head_cid,
        inserted: true,
    })
}

/// Append a **pre-sealed** mail record verbatim — the relay-destination sibling
/// of [`append_record`]. The relay carries the source nest's already-sealed
/// `MailRecordEnvelope` bytes and its [`MailFloorMetadata`]; the destination
/// must NOT re-seal (it has no key), so it appends the bytes as-is and writes
/// the mirror row from the forwarded floor. It allocates a **fresh local seq**
/// (the destination's own cursor) and overwrites `floor.seq` with it, so the
/// destination's relay/compaction invariants hold independent of the source's
/// numbering.
///
/// It likewise overwrites `floor.stored_at` with the local now — the two are the
/// floor's only *local* facts, and a forwarded `stored_at` is not merely useless
/// but unsafe (the headless-part reaper's grace keys on it). Everything else,
/// `received_at` included, is the message's own and is forwarded verbatim.
///
/// The filing identity is derived from the verbatim bytes —
/// `Cid::of_dag_cbor(sealed_envelope_bytes)` — never carried: because the relay
/// forwards the source's stored bytes unchanged, the destination re-derives the
/// **same** identity the source filed under (for normal records, continuation
/// parts, and heads alike), so the idempotency/dedup key is stable across the
/// relay by construction. The scoped dedup runs here, inside the seq lock;
/// callers need no pre-check of their own.
pub async fn append_sealed_record(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    sealed_envelope_bytes: &[u8],
    mut floor: MailFloorMetadata,
) -> Result<AppendOutcome> {
    let bucket = bucket_for(floor.received_at / 1000);
    let cid = Cid::of_dag_cbor(sealed_envelope_bytes);

    let lock = seq_lock_for(actor_id);
    let _guard = lock.lock().await;

    // Scoped dedup, same rule and same rationale as `append_record`'s: a hit is
    // a replayed relay delivery of this scope's own record.
    if let Some(existing) = cache_db
        .segment_records_lookup_record(actor_id, "mail", &cid)
        .await?
    {
        return Ok(AppendOutcome {
            seg_id: existing.segment_id,
            finalized: None,
            cid,
            inserted: false,
        });
    }

    let seq = cache_db.segment_records_next_mail_seq(actor_id).await?;
    floor.seq = seq;
    // Overwrite the peer's `stored_at` with ours, for the same reason `seq` is
    // overwritten just above: both are facts about *this* nest's store, and a
    // forwarded value is meaningless here. For `stored_at` it is also unsafe —
    // the headless-part reaper's grace keys on it, and a relayed value is the
    // MESSAGE's age (days, on a backfill relay), not the record's age here, so
    // honouring it would let the reaper tombstone a just-arrived part before its
    // head lands. `received_at` is deliberately NOT overwritten: it is the
    // message's own receive time and must survive the relay.
    floor.stored_at = stored_at_now_ms();
    let floor_bytes = floor
        .encode()
        .map_err(|e| anyhow::anyhow!("encode forwarded floor: {e}"))?;

    // Verbatim append: the envelope bytes are already-sealed source bytes — we
    // append them as-is (the same low-level call `ops::append` ends in, minus
    // the re-serialize). The destination holds no key to open them.
    let outcome = mgr
        .append_record_with_bucket(actor_id, cid, sealed_envelope_bytes, &floor_bytes, &bucket)
        .await
        .map_err(|e| anyhow::anyhow!("append_record_with_bucket (sealed): {e}"))?;

    cache_db
        .segment_records_insert_mail(
            actor_id,
            outcome.segment_id,
            &cid,
            &bucket,
            floor.received_at,
            &floor.sender_domain,
            &floor.spam_disposition,
            floor.is_own_submission,
            seq,
            (!floor.report_hash.is_empty()).then_some(floor.report_hash.as_slice()),
            floor.continuation_role,
            floor.stored_at,
        )
        .await
        .context("insert segment_records mirror row (sealed relay append)")?;

    Ok(AppendOutcome {
        seg_id: outcome.segment_id,
        finalized: outcome.finalized,
        cid,
        inserted: true,
    })
}

/// Append one **recovered** mail record — the lived-in recovery's write
/// (`segment-backup-protocol.md` § Client-device custodian (pull) → *Restore*
/// → *Recovery into the lived-in nest that regressed*, "the write is the
/// ordinary one, per record").
///
/// The relay's verbatim carrier, [`append_sealed_record`], with two
/// differences and nothing else:
///
/// - **held means held EVER.** The dedup is against live **and tombstoned**
///   mirror rows ([`super::records_db::held_ever`]): the target's own history
///   wins, so a message the owner deleted after the rollback stays deleted.
///   Returns `Ok(None)` for a held record and writes nothing.
/// - **the placement commits WITH the mirror row.** `place` runs inside the
///   one transaction that inserts the mirror row, so no crash leaves a
///   recovered record live and in no mailbox. It returns whatever the caller
///   must journal once the rows are committed.
///
/// Like the relay's, the floor rides verbatim save its two *local* facts —
/// `seq`, a fresh per-actor coordinate (to a puller a recovered record is a
/// new arrival, which it is), and `stored_at`, now — which are restamped into
/// the footer as into the mirror row, so a later rebuild from the footers
/// recovers the same cursor. The bytes, the CID (re-derived from them) and the
/// bucket are the record's own.
pub(crate) async fn append_recovered_record<T>(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    sealed_envelope_bytes: &[u8],
    bucket: &str,
    mut floor: MailFloorMetadata,
    place: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
) -> Result<Option<(u32, T)>> {
    let cid = Cid::of_dag_cbor(sealed_envelope_bytes);

    let lock = seq_lock_for(actor_id);
    let _guard = lock.lock().await;

    let (held, seq) = {
        let conn = cache_db.conn().await;
        let held = super::records_db::held_ever(&conn, actor_id, "mail", &cid)?;
        let seq = super::records_db::next_mail_seq(&conn, actor_id)?;
        (held, seq)
    };
    if held {
        return Ok(None);
    }
    floor.seq = seq;
    floor.stored_at = stored_at_now_ms();
    let floor_bytes = floor
        .encode()
        .map_err(|e| anyhow::anyhow!("encode recovered floor: {e}"))?;

    let outcome = mgr
        .append_record_with_bucket(actor_id, cid, sealed_envelope_bytes, &floor_bytes, bucket)
        .await
        .map_err(|e| anyhow::anyhow!("append_record_with_bucket (recovered): {e}"))?;

    let conn = cache_db.conn().await;
    let tx = conn
        .unchecked_transaction()
        .context("begin the recovered-record transaction")?;
    super::records_db::insert_mail(
        &tx,
        actor_id,
        outcome.segment_id,
        &cid,
        bucket,
        floor.received_at,
        &floor.sender_domain,
        &floor.spam_disposition,
        floor.is_own_submission,
        seq,
        (!floor.report_hash.is_empty()).then_some(floor.report_hash.as_slice()),
        floor.continuation_role,
        floor.stored_at,
    )
    .context("insert the recovered record's mirror row")?;
    let placed = place(&tx)?;
    tx.commit()
        .context("commit the recovered record with its placement")?;
    Ok(Some((outcome.segment_id, placed)))
}

/// Read live mail records for one actor with `seq > after_seq`, oldest first,
/// up to `limit`. The relay's after-cursor reader — sibling of
/// `segments::conv::read_for_scopes_after_seq`, scoped to a single actor (mail
/// is keyed by `actor_id`, not by many channels). Returns, per record, the
/// `seq`, the 32-byte `record_id`, the **verbatim sealed envelope bytes** (the
/// relay forwards these as-is — no decode/re-seal), and the decoded
/// [`MailFloorMetadata`] (so the destination can reconstruct a faithful mirror
/// row + footer). Records present in the mirror but missing on disk are skipped
/// with a warning (same divergence policy as [`read_envelopes_bulk`]).
pub async fn read_after_seq(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    after_seq: i64,
    limit: i64,
) -> Result<Vec<(i64, [u8; 32], Vec<u8>, MailFloorMetadata)>> {
    let rows = cache_db
        .segment_records_list_mail_after_seq(actor_id, after_seq, limit)
        .await?;
    // Flush-on-read once before opening any segment (idempotent).
    mgr.flush(actor_id)
        .await
        .map_err(|e| anyhow::anyhow!("flush before relay read: {e}"))?;
    let mut out = Vec::with_capacity(rows.len());
    for (seq, segment_id, cid) in rows {
        // The relay wire carries the 32-byte digest (`message_id`); the mirror
        // now stores the full Cid, so strip the digest for the forwarded tuple.
        let rid = cid.digest();
        match mgr
            .read_record_with_floor_bytes(actor_id, segment_id, &cid)
            .await
        {
            Ok((env_bytes, floor_bytes)) => {
                let floor = MailFloorMetadata::decode(&floor_bytes)
                    .map_err(|e| anyhow::anyhow!("decode floor in relay read: {e}"))?;
                out.push((seq, rid, env_bytes, floor));
            }
            Err(fauna_segment_store::ManagerError::RecordNotFound { .. }) => {
                tracing::warn!(
                    actor = ?actor_id,
                    record_id = ?rid,
                    segment_id,
                    "segment_records mirror diverged from segment file (relay read)"
                );
            }
            Err(e) => return Err(anyhow::anyhow!("read_record_with_floor_bytes (relay): {e}")),
        }
    }
    Ok(out)
}

/// Tombstone every live mail record for `actor_id` with `seq <= up_to_seq` — the
/// relay-ack purge step (sibling of `segments::conv::tombstone_up_to_seq`). The
/// tombstoned bytes are physically reclaimed by the [`compact_bucket`]
/// compaction worker per the actor's retention policy. Returns the number of
/// rows newly tombstoned (idempotent: re-acking the same cursor returns 0).
pub async fn tombstone_up_to_seq(
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    up_to_seq: i64,
) -> Result<usize> {
    cache_db
        .segment_records_tombstone_mail_up_to_seq(actor_id, up_to_seq)
        .await
}

/// Compact one bucket per the supplied `CompactionPlan`.
///
/// Transactional outline (same invariants as the former manager):
///
/// 1. Pre-fetch the live `(segment_id, record_cid)` set from `segment_records`
///    for every `plan.inputs` segment (the alive-filter closure consults this
///    set).
/// 2. `compact_with_filter` on `SegmentManager`: finalize-on-read, file-level
///    compaction, manifest swap + atomic save.
/// 3. Under one SQLite transaction:
///    - Tombstone every `segment_records` row whose `segment_id` is in
///      `plan.inputs`.
///    - INSERT new `segment_records` rows for the survivors in the
///      newly-written segment (walked via `read_envelopes_bulk`).
///    - Commit.
///
/// **Ordering rationale**: file-compact-first → SQL-tx-second →
/// manifest-already-swapped-inside-compact_with_filter. The on-disk segment
/// file is the floor authority; the SQLite mirror is rebuildable. Crash between
/// compact and SQL commit: next recovery walk can detect the orphaned
/// new-segment file and re-run SQL.
///
/// **Lock ordering invariant.** `compact_with_filter` holds the per-scope mutex
/// internally. The SQL tx acquires the `CacheDb` conn mutex afterward. The
/// process-wide order is `per-scope-mutex → conn-mutex`; future code paths that
/// touch both must preserve this order.
pub async fn compact_bucket(
    mgr: &SegmentManager,
    cache_db: &Arc<CacheDb>,
    actor_id: &[u8; 32],
    kind: &str,
    plan: &CompactionPlan,
) -> Result<BucketCompactionOutcome> {
    debug_assert_eq!(kind, "mail", "non-mail kinds not yet wired");

    super::compact_bucket_with(
        mgr,
        cache_db,
        actor_id,
        kind,
        plan,
        |rid, floor_bytes| {
            let floor = MailFloorMetadata::decode(floor_bytes)
                .map_err(|e| anyhow::anyhow!("decode floor metadata: {e}"))?;
            // Mail floors carry epoch **milliseconds** — hence the `/ 1000`
            // before `bucket_for`, which calendar/card must NOT copy.
            // `rid` is the per-record Cid; the mirror stores it verbatim.
            Ok(super::records_db::NewSegmentRecord {
                record_cid: rid,
                bucket: bucket_for(floor.received_at / 1000),
                received_at: floor.received_at,
                sender_domain: floor.sender_domain.clone(),
                spam_disposition: floor.spam_disposition.clone(),
                is_own_submission: floor.is_own_submission,
                // Carry the relay cursor through the rewrite — the survivor's
                // floor is the authority (0 for pre-relay records).
                seq: floor.seq,
                // Carry the continuation role too — a rewrite must not unmark a
                // head/part (message-segment-store.md § Continuation records).
                continuation_role: floor.continuation_role,
                // And the local storage time — re-stamping it here would restart
                // every part's reaper grace on each compaction pass (0 = unknown after a
                // failed clock read, which stays NULL = not reapable).
                stored_at: floor.stored_at,
            })
        },
        |tx, new_segment, new_records| {
            super::records_db::apply_compaction_tx(
                tx,
                actor_id,
                kind,
                &plan.inputs,
                new_segment,
                new_records,
            )
            .context("apply_compaction_tx")
        },
    )
    .await
}

/// Read one record's envelope bytes (sealed body + sealed index hint). Caller
/// decodes via `MailRecordEnvelope::decode` if it needs the inner pieces.
///
/// Applies the read-your-own-writes policy: flushes any currently-open segment
/// for the actor before reading.
pub async fn read_envelope(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    record_id: &[u8],
) -> Result<Option<Vec<u8>>> {
    let rid: [u8; 32] = record_id
        .try_into()
        .map_err(|_| anyhow::anyhow!("record_id must be 32 bytes, got {}", record_id.len()))?;
    let cid = Cid::from_digest_dag_cbor(rid);
    let Some(rec) = cache_db
        .segment_records_lookup_record(actor_id, "mail", &cid)
        .await?
    else {
        return Ok(None);
    };
    let bytes = mgr
        .read_envelope_bytes(actor_id, rec.segment_id, &cid)
        .await
        .map_err(|e| anyhow::anyhow!("read_envelope_bytes: {e}"))?;
    Ok(Some(bytes))
}

/// Bulk read multiple records, grouped by segment for one open() per segment.
/// Order of the output Vec matches the input `record_ids`. Missing records are
/// returned as `None`.
///
/// Applies the read-your-own-writes policy: finalises any open segment for the
/// actor before reading.
pub async fn read_envelopes_bulk(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    record_ids: &[&[u8]],
) -> Result<Vec<Option<Vec<u8>>>> {
    // Resolve every record_id to its mirror row (`segment_id`). The mirror is
    // keyed by the full Cid; reconstruct it from each 32-byte digest.
    let mut refs: Vec<Option<SegmentRecordRef>> = Vec::with_capacity(record_ids.len());
    for rid in record_ids {
        let digest: [u8; 32] = (*rid)
            .try_into()
            .map_err(|_| anyhow::anyhow!("record_id must be 32 bytes, got {}", rid.len()))?;
        let cid = Cid::from_digest_dag_cbor(digest);
        refs.push(
            cache_db
                .segment_records_lookup_record(actor_id, "mail", &cid)
                .await?,
        );
    }

    // Flush-on-read once before opening any segment in the loop. Idempotent —
    // no-op when nothing's open.
    mgr.flush(actor_id)
        .await
        .map_err(|e| anyhow::anyhow!("flush before bulk read: {e}"))?;

    // Group present refs by segment_id, preserving the input order.
    let mut by_seg: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, r) in refs.iter().enumerate() {
        if let Some(r) = r {
            by_seg.entry(r.segment_id).or_default().push(i);
        }
    }

    let mut output: Vec<Option<Vec<u8>>> = vec![None; record_ids.len()];
    for (seg_id, indices) in by_seg {
        for i in indices {
            let rid_slice: &[u8] = record_ids[i];
            let rid: [u8; 32] = rid_slice
                .try_into()
                .map_err(|_| anyhow::anyhow!("record_id wrong length"))?;
            // The flush above already ran; read_envelope_bytes also does
            // flush-on-read (idempotent) so this is safe.
            let cid = Cid::from_digest_dag_cbor(rid);
            match mgr.read_envelope_bytes(actor_id, seg_id, &cid).await {
                Ok(bytes) => output[i] = Some(bytes),
                Err(fauna_segment_store::ManagerError::RecordNotFound { .. }) => {
                    // record in segment_records but not on disk — divergence
                    tracing::warn!(
                        actor = ?actor_id,
                        record_id = ?rid,
                        segment_id = seg_id,
                        "segment_records mirror diverged from segment file (bulk read)"
                    );
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("read_envelope_bytes bulk: {e}"));
                }
            }
        }
    }

    Ok(output)
}

/// Look up one mail record's CARv2 block byte-length — the sealed-ciphertext
/// size IMAP RFC822.SIZE reports — from the segment's `MultihashIndexSorted`
/// index by Cid, **not** a `segment_records` SQL byte column (now removed)
/// (`imap-server.md` §§ SEARCH, QUOTA). The block body is never read. The `Cid`
/// comes straight from the `segment_records.record_cid` mirror column.
///
/// Returns `Ok(None)` when the record's segment can't be opened or the record
/// is absent from it — a mirror/disk divergence the size path tolerates (the
/// caller reports size 0 for that one record rather than failing the whole
/// IMAP command). Mirrors [`read_envelopes_bulk`]'s divergence handling.
pub async fn record_size(
    mgr: &SegmentManager,
    actor_id: &[u8; 32],
    segment_id: u32,
    record_cid: Cid,
) -> Result<Option<u64>> {
    let sizes = super::record_sizes(mgr, actor_id, &[(segment_id, record_cid)]).await?;
    Ok(sizes.into_iter().next().flatten())
}

/// Read one record's envelope alongside its decoded floor metadata.
///
/// Applies the read-your-own-writes policy.
pub async fn read_record_with_floor(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    record_id: &[u8],
) -> Result<Option<(MailRecordEnvelope, MailFloorMetadata)>> {
    let rid: [u8; 32] = record_id
        .try_into()
        .map_err(|_| anyhow::anyhow!("record_id must be 32 bytes, got {}", record_id.len()))?;
    let cid = Cid::from_digest_dag_cbor(rid);
    let Some(rec) = cache_db
        .segment_records_lookup_record(actor_id, "mail", &cid)
        .await?
    else {
        return Ok(None);
    };
    let (env_bytes, floor_bytes) = mgr
        .read_record_with_floor_bytes(actor_id, rec.segment_id, &cid)
        .await
        .map_err(|e| anyhow::anyhow!("read_record_with_floor_bytes: {e}"))?;
    let envelope = MailRecordEnvelope::decode(&env_bytes)
        .map_err(|e| anyhow::anyhow!("decode envelope: {e}"))?;
    let floor = MailFloorMetadata::decode(&floor_bytes)
        .map_err(|e| anyhow::anyhow!("decode floor: {e}"))?;
    Ok(Some((envelope, floor)))
}

/// Read one record's **full sealed body + sealed index hint**, transparently
/// resolving a v3 continuation head into the concatenation of its part records.
/// The serve-join primitive slice 4's read paths use — the head→parts join
/// happens here, once, so every serve leg (IMAP `fetch_message_ciphertext`, the
/// client feed) gets a self-contained body regardless of how it rests.
///
/// For an inline (v1/v2) record this is exactly the decoded envelope's two
/// fields. For a head it reads each part by CID (in the head's declared order)
/// and concatenates, **failing closed** if a part is missing from the mirror or
/// the rejoined length does not match the head's `total_body_len` — an honest
/// error, never a truncated body. A missing part is the accepted transient of a
/// relay/crash mid-write (message-segment-store.md § Continuation records); the
/// re-pull / retry heals it.
///
/// Returns `Ok(None)` if the record id is unknown. Applies read-your-own-writes.
pub async fn read_sealed_body_with_floor(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    record_id: &[u8],
) -> Result<Option<(Vec<u8>, Vec<u8>, MailFloorMetadata)>> {
    let rid: [u8; 32] = record_id
        .try_into()
        .map_err(|_| anyhow::anyhow!("record_id must be 32 bytes, got {}", record_id.len()))?;
    let cid = Cid::from_digest_dag_cbor(rid);
    let Some(rec) = cache_db
        .segment_records_lookup_record(actor_id, "mail", &cid)
        .await?
    else {
        return Ok(None);
    };
    let (env_bytes, floor_bytes) = mgr
        .read_record_with_floor_bytes(actor_id, rec.segment_id, &cid)
        .await
        .map_err(|e| anyhow::anyhow!("read_record_with_floor_bytes: {e}"))?;
    let floor = MailFloorMetadata::decode(&floor_bytes)
        .map_err(|e| anyhow::anyhow!("decode floor: {e}"))?;
    let (body, hint) = match MailRecord::decode(&env_bytes)
        .map_err(|e| anyhow::anyhow!("decode mail record: {e}"))?
    {
        MailRecord::Inline(env) => (env.encrypted_body, env.encrypted_index_hint),
        MailRecord::Head(head) => {
            let body = join_continuation_parts(mgr, cache_db, actor_id, &head).await?;
            (body, head.encrypted_index_hint)
        }
    };
    Ok(Some((body, hint, floor)))
}

/// Concatenate a continuation head's part records into the full sealed body, in
/// the head's declared order. Reads each part by CID through the same
/// mirror→`read_envelope_bytes` mechanism every record read uses (a part's block
/// bytes are the raw ciphertext range, stored verbatim). Fails closed on a
/// missing part or a total-length mismatch.
///
/// Treats the head as **untrusted input** — it is reachable verbatim from a relay
/// peer — so it never sizes an allocation from the declared `total_body_len`; see
/// the rules inline below.
async fn join_continuation_parts(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    head: &MailContinuationHead,
) -> Result<Vec<u8>> {
    // `total_body_len` is UNTRUSTED: the relay stores a peer's head verbatim
    // (envelope opaque, no ingest-side validation), so this `u64` is attacker-
    // reachable. Sizing the join from it is what made a corrupt/hostile head a
    // panic (`> isize::MAX`) or an OOM abort of the whole nest process — and the
    // head persists, so every retry re-aborts: a crash-loop DoS for that
    // mailbox. Rules 1-2 make that unrepresentable, mirroring the client-side
    // resolver (`fauna_mail::body_ref::join_sealed_mail_body`), which sizes from
    // the chunks it actually fetched and treats the declared total as a check:
    //
    // 1. The head's own part list bounds what it may honestly declare. A part is
    //    one CARv2 record, refused on read above `MAX_RECORD_LEN`, so
    //    `parts × MAX_RECORD_LEN` is an upper bound no writer of any version can
    //    honestly exceed. Deliberately NOT the tighter `MAIL_BODY_PART_CAP_BYTES`:
    //    that cap may rise, and rejecting a future peer's honest body would be
    //    the worse failure. This is a garbage guard, not a product ceiling — the
    //    exact length check below is what pins correctness.
    // 2. Never pre-allocate the declared total. Start at one part and let the
    //    bytes actually fetched grow it, so memory tracks records that exist.
    // 3. A part is named at most once, checked before any read. Rule 1
    //    alone admits a head naming ONE stored part tens of thousands of times
    //    under a matching total, and every repeat is a full read. An honest
    //    writer never repeats one: each part is a distinct range of one AEAD
    //    ciphertext. Deliberately not a count bound from `total_body_len` over
    //    the part cap — an older peer's smaller parts would fail it.
    // 4. A running total: the part that carries the join past the declared
    //    total is refused as it arrives, not after the whole list is read.
    let mut seen = std::collections::HashSet::with_capacity(head.part_digests.len());
    if let Some(dup) = head.part_digests.iter().find(|d| !seen.insert(**d)) {
        anyhow::bail!(
            "continuation head names part {} more than once — refusing a corrupt/hostile head",
            hex::encode(dup),
        );
    }
    let max_declarable =
        (head.part_digests.len() as u64).saturating_mul(fauna_carv2::v1::MAX_RECORD_LEN as u64);
    if head.total_body_len > max_declarable {
        anyhow::bail!(
            "continuation head declares a {}-byte body but its {} part(s) can hold at most {} \
             — refusing a corrupt/hostile head",
            head.total_body_len,
            head.part_digests.len(),
            max_declarable,
        );
    }
    let mut body =
        Vec::with_capacity(head.total_body_len.min(MAIL_BODY_PART_CAP_BYTES as u64) as usize);
    for digest in &head.part_digests {
        let part_cid = Cid::from_digest_dag_cbor(*digest);
        let Some(rec) = cache_db
            .segment_records_lookup_record(actor_id, "mail", &part_cid)
            .await?
        else {
            anyhow::bail!(
                "continuation part {} is missing from the mirror — the head references a part \
                 not yet stored (relay/crash mid-write; re-pull heals it)",
                hex::encode(digest),
            );
        };
        let part_bytes = mgr
            .read_envelope_bytes(actor_id, rec.segment_id, &part_cid)
            .await
            .map_err(|e| anyhow::anyhow!("read continuation part: {e}"))?;
        if (body.len() + part_bytes.len()) as u64 > head.total_body_len {
            anyhow::bail!(
                "continuation parts passed the head's declared {} bytes — refusing a \
                 corrupt/hostile head",
                head.total_body_len,
            );
        }
        body.extend_from_slice(&part_bytes);
    }
    if body.len() as u64 != head.total_body_len {
        anyhow::bail!(
            "continuation body rejoined to {} bytes, head declared {} — refusing a \
             corrupt/incomplete body",
            body.len(),
            head.total_body_len,
        );
    }
    Ok(body)
}

/// Tombstone `actor`'s **headless continuation parts** — the mail sibling of the
/// calendar/card orphan-reaper ([`super::cal::reap_orphan_records`]), run as a
/// pre-pass by the compaction worker under the `"gc"` op lock. A part is
/// headless when no live head references it: a crash between parts and head, or
/// a fanned-out delete that missed a part. The age-watermark + fan-out safety
/// argument lives on [`super::records_db::reap_headless_parts`].
///
/// Enumerates the actor's live continuation heads, decodes each (**fail-closed**:
/// a head that can't be read, or a `role=HEAD` row that isn't v3, aborts the
/// reap rather than risk tombstoning a still-referenced part), unions their part
/// CIDs, then reaps aged unreferenced parts. `now_ms` is epoch **milliseconds**
/// (mail's floor `received_at` unit — unlike calendar/card seconds).
pub async fn reap_headless_parts(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    now_ms: i64,
) -> Result<u32> {
    // Snapshot the live heads; their part lists define what is still referenced.
    let head_refs = {
        let conn = cache_db.conn().await;
        super::records_db::list_live_continuation_heads(&conn, actor_id)?
    };
    // A no-head actor has nothing to reap against — but there may still be
    // crash-orphaned parts (a single continuation write that never committed its
    // head). An empty live set means every candidate part is reapable; the age
    // watermark is what keeps that safe (parts are young when written).
    mgr.flush(actor_id)
        .await
        .map_err(|e| anyhow::anyhow!("flush before reap: {e}"))?;
    let mut live_part_cids = std::collections::HashSet::new();
    for (segment_id, head_cid) in &head_refs {
        let bytes = mgr
            .read_envelope_bytes(actor_id, *segment_id, head_cid)
            .await
            .map_err(|e| anyhow::anyhow!("reap: read continuation head: {e}"))?;
        match MailRecord::decode(&bytes)
            .map_err(|e| anyhow::anyhow!("reap: decode continuation head: {e}"))?
        {
            MailRecord::Head(h) => {
                for d in &h.part_digests {
                    live_part_cids.insert(Cid::from_digest_dag_cbor(*d));
                }
            }
            MailRecord::Inline(_) => anyhow::bail!(
                "reap: a record marked continuation_role=HEAD decoded as inline — \
                 mirror/segment divergence, refusing to reap"
            ),
        }
    }
    let conn = cache_db.conn().await;
    super::records_db::reap_headless_parts(&conn, actor_id, now_ms, &live_part_cids)
}

/// Per-segment metadata needed by `fauna.segments.list` (Plan 5): the on-disk
/// blake3 + size of the framed segment file, plus the bucket / record_count /
/// created_at from the segment header and the live-tombstone count from the
/// `segment_records` mirror.
///
/// Caller is expected to have already invoked `SegmentManager::finalize_open`
/// so that any currently-open segment's bytes are on disk before the
/// blake3/size are read.
pub async fn describe_segment_for_backup(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor_id: &[u8; 32],
    segment_id: u32,
) -> Result<SegmentBackupMeta> {
    let meta = mgr
        .describe_segment_for_backup(actor_id, segment_id)
        .await
        .map_err(|e| anyhow::anyhow!("describe_segment_for_backup: {e}"))?;
    let tombstone_count = cache_db
        .count_tombstoned_segment_records(actor_id, "mail", segment_id)
        .await? as u32;
    Ok(SegmentBackupMeta {
        file_blake3: meta.file_blake3,
        meta_blake3: meta.meta_blake3,
        size_bytes: meta.byte_size,
        bucket: meta.bucket,
        record_count: meta.record_count,
        tombstone_count,
        created_at_secs: meta.created_at_secs,
    })
}

// NOTE: the scope-agnostic `lookup_actor(record_id) -> actor` resolution is
// RETIRED with the record-identity cutover (`message-segment-store.md`
// § Record identity per kind): the actor no longer lives inside the record id,
// so a byte replay can file one cid in two scopes and a LIMIT-1 owner pick is
// arbitrary. Every consumer scope-checks with
// `CacheDb::segment_records_lookup_record(scope, "mail", cid)` instead.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segments::test_helpers::floor;
    use tempfile::TempDir;

    /// Returns (tempdir, manager, cache_db). Drop the tempdir last so segment
    /// files outlive the manager.
    fn setup() -> (TempDir, SegmentManager, CacheDb) {
        let tmp = TempDir::new().expect("tmp");
        let manager = SegmentManager::new(tmp.path().to_path_buf(), "mail");
        // CacheDb::open_in_memory is sync (see bins/fauna-nest/src/db/mod.rs).
        let cache_db = CacheDb::open_in_memory().expect("in-memory cache db");
        (tmp, manager, cache_db)
    }

    /// Wrap a byte literal as an at-rest sealed carrier. These mod tests
    /// exercise segment placement / read-back / rotation mechanics, not seal
    /// genuineness, so `carried_at_rest_unchecked` (S6.12b) keeps the served
    /// bytes byte-for-byte comparable to the literal while satisfying the typed
    /// `append_record` gate.
    fn at_rest(bytes: Vec<u8>) -> SealedRecordBytes {
        SealedRecordBytes::carried_at_rest_unchecked(bytes)
    }

    /// Row 61 — the record-identity cutover (`message-segment-store.md`
    /// § Record identity per kind): a mail record's filing CID is the content
    /// hash of the stored block bytes — exactly what
    /// `fauna_account_store::segments::admit` re-hashes every block against,
    /// the property that makes mail custodian-adoptable.
    #[tokio::test]
    async fn mail_filing_cid_is_the_content_hash_of_the_stored_block() {
        let (_tmp, mgr, db) = setup();
        let actor = [0x51u8; 32];
        append_record(
            &mgr,
            &db,
            &actor,
            &at_rest(b"sealed-body".to_vec()),
            &at_rest(b"sealed-hint".to_vec()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append");
        let pulled = read_after_seq(&mgr, &db, &actor, 0, 10)
            .await
            .expect("read");
        assert_eq!(pulled.len(), 1);
        let (_seq, rid, env_bytes, _floor) = &pulled[0];
        assert_eq!(
            Cid::from_digest_dag_cbor(*rid),
            Cid::of_dag_cbor(env_bytes),
            "the filing CID must be the content hash of the stored block bytes \
             (message-segment-store.md § Record identity per kind)"
        );
    }

    /// The cutover's dedup corollary: the pre-append dedup is scoped to
    /// `(scope_id, kind, record_cid)` — a literal byte replay within one scope
    /// dedups (inserted=false), while the same bytes landing in a DIFFERENT
    /// scope keep that scope's own record (the actor is no longer inside the
    /// hash, so scope-agnostic dedup would silently drop the second actor's
    /// record — the pre-check 2 obligation).
    #[tokio::test]
    async fn mail_dedup_is_scoped_a_byte_replay_across_scopes_keeps_both() {
        let (_tmp, mgr, db) = setup();
        let (a, b) = ([0x0au8; 32], [0x0bu8; 32]);
        let body = at_rest(b"replayed-sealed-body".to_vec());
        let hint = at_rest(b"replayed-sealed-hint".to_vec());
        let one = append_record(&mgr, &db, &a, &body, &hint, floor(1_715_000_000_000))
            .await
            .expect("first append");
        assert!(one.inserted, "first append inserts");
        let replay = append_record(&mgr, &db, &a, &body, &hint, floor(1_715_000_099_000))
            .await
            .expect("same-scope replay");
        assert!(!replay.inserted, "same-scope byte replay dedups");
        assert_eq!(replay.cid, one.cid, "dedup returns the existing identity");
        let other_scope = append_record(&mgr, &db, &b, &body, &hint, floor(1_715_000_000_000))
            .await
            .expect("other-scope append");
        assert!(
            other_scope.inserted,
            "identical bytes in a different scope are that scope's own record"
        );
    }

    /// The shared-Rust crux of the public→private mail relay (Slice 1), at the
    /// `segments::mail` level: append on the public nest stamps a per-actor
    /// monotonic seq; `read_after_seq` returns verbatim sealed bytes + floor;
    /// the private nest re-appends them with `append_sealed_record` and serves
    /// the verbatim bytes back; the public nest acks+purges so no readable copy
    /// remains. (The two-nest channel-wire capstone is the tier_3 T7 test.)
    #[tokio::test]
    async fn relay_round_trip_read_reseal_and_purge() {
        // PUBLIC side: append 3 records for one actor.
        let (_pub_tmp, pub_mgr, pub_db) = setup();
        let actor = [0x42u8; 32];
        for i in 0..3u8 {
            append_record(
                &pub_mgr,
                &pub_db,
                &actor,
                &at_rest(format!("sealed-body-{i}").into_bytes()),
                &at_rest(format!("sealed-hint-{i}").into_bytes()),
                floor(1_715_000_000_000 + i as i64 * 1000),
            )
            .await
            .expect("public append");
        }

        // read_after_seq(0) returns all 3 in seq order with the stamped cursor.
        let pulled = read_after_seq(&pub_mgr, &pub_db, &actor, 0, 100)
            .await
            .expect("read_after_seq");
        assert_eq!(pulled.len(), 3);
        assert_eq!(
            pulled.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "append stamped a per-actor monotonic seq"
        );

        // PRIVATE side: re-append each sealed record verbatim.
        let (_priv_tmp, priv_mgr, priv_db) = setup();
        for (_seq, rid, env_bytes, floor_md) in &pulled {
            assert!(
                priv_db
                    .segment_records_lookup_record(&actor, "mail", &Cid::from_digest_dag_cbor(*rid))
                    .await
                    .unwrap()
                    .is_none(),
                "idempotency: record absent before relay append"
            );
            let relayed =
                append_sealed_record(&priv_mgr, &priv_db, &actor, env_bytes, floor_md.clone())
                    .await
                    .expect("sealed relay append");
            assert_eq!(
                relayed.cid.digest(),
                *rid,
                "verbatim bytes re-derive the source's identity at the destination"
            );
        }
        priv_mgr.flush(&actor).await.expect("flush private");

        // The private side holds the VERBATIM sealed bytes (no re-seal) and the
        // forwarded floor metadata.
        for (_seq, rid, env_bytes, floor_md) in &pulled {
            let got = read_envelope(&priv_mgr, &priv_db, &actor, rid)
                .await
                .expect("read")
                .expect("present on private side");
            assert_eq!(
                &got, env_bytes,
                "private side holds the verbatim sealed envelope"
            );
            let (_env, got_floor) = read_record_with_floor(&priv_mgr, &priv_db, &actor, rid)
                .await
                .expect("read floor")
                .expect("present");
            assert_eq!(
                got_floor.sender_domain, floor_md.sender_domain,
                "forwarded floor metadata preserved"
            );
        }
        // Private side stamped its OWN local cursor (independent of the source).
        let priv_rows = priv_db
            .segment_records_list_mail_after_seq(&actor, 0, 100)
            .await
            .expect("list private");
        assert_eq!(
            priv_rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        // PUBLIC side: ack+purge up to the highest pulled seq → no readable copy.
        let highest = pulled.iter().map(|r| r.0).max().unwrap();
        let purged = tombstone_up_to_seq(&pub_db, &actor, highest)
            .await
            .expect("tombstone");
        assert_eq!(purged, 3);
        let remaining = read_after_seq(&pub_mgr, &pub_db, &actor, 0, 100)
            .await
            .expect("read after purge");
        assert!(
            remaining.is_empty(),
            "public nest holds no readable mail after ack+purge"
        );
    }

    #[tokio::test]
    async fn append_then_read_round_trip() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x77u8; 32];
        let body = b"sealed-body".to_vec();
        let hint = b"sealed-hint".to_vec();

        let outcome = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(body.clone()),
            &at_rest(hint.clone()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append");
        assert_eq!(outcome.seg_id, 1);
        assert!(
            outcome.finalized.is_none(),
            "first-ever append doesn't finalize anything"
        );
        let record_id = outcome.cid.digest();

        // Finalize the open segment so it can be read back.
        manager.flush(&actor).await.expect("flush");

        let env = read_envelope(&manager, &cache_db, &actor, &record_id)
            .await
            .expect("read")
            .expect("present");
        let decoded = MailRecordEnvelope::decode(&env).expect("decode");
        assert_eq!(decoded.encrypted_body, body);
        assert_eq!(decoded.encrypted_index_hint, hint);
    }

    // (`reseal_record_in_place` and its test are RETIRED — the record-identity
    // cutover deleted the S4 mail reseal arm: identity-preserving byte swaps
    // are structurally impossible when identity IS the hash of the bytes.)

    #[tokio::test]
    async fn record_size_from_carv2_index() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x55u8; 32];
        let rid_a = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"body-aaaa".to_vec()),
            &at_rest(b"hint-a".to_vec()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append a")
        .cid
        .digest();
        let rid_b = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"body-bbbbbbbbbb".to_vec()),
            &at_rest(b"hint-bb".to_vec()),
            floor(1_715_000_001_000),
        )
        .await
        .expect("append b")
        .cid
        .digest();
        manager.flush(&actor).await.expect("flush");

        // Each record's size is looked up from the CARv2 MultihashIndexSorted
        // index by `record_cid` (no SQL byte column) — a non-zero block length,
        // and identical whether resolved single or in bulk (below).
        for rid in [&rid_a, &rid_b] {
            let cid = Cid::from_digest_dag_cbor(*rid);
            let row = cache_db
                .segment_records_lookup_record(&actor, "mail", &cid)
                .await
                .expect("lookup")
                .expect("row present");
            let size = record_size(&manager, &actor, row.segment_id, row.record_cid)
                .await
                .expect("size ok")
                .expect("size present");
            assert!(size > 0, "index-derived block length is non-zero");
        }

        // Bulk variant: order preserved; longer body → larger block; an
        // unknown record in an existing segment → None.
        let unknown = [0xffu8; 32];
        let refs: Vec<(u32, Cid)> = vec![
            (1, Cid::from_digest_dag_cbor(rid_a)),
            (1, Cid::from_digest_dag_cbor(rid_b)),
            (1, Cid::from_digest_dag_cbor(unknown)),
        ];
        let sizes = crate::segments::record_sizes(&manager, &actor, &refs)
            .await
            .expect("sizes ok");
        assert_eq!(sizes.len(), 3);
        let (sa, sb) = (sizes[0].expect("a sized"), sizes[1].expect("b sized"));
        assert!(sb > sa, "longer body → larger CARv2 block ({sb} > {sa})");
        assert_eq!(sizes[2], None, "unknown record id → None");
    }

    #[tokio::test]
    async fn record_sizes_tolerates_missing_segment() {
        let (_tmp, manager, _cache_db) = setup();
        let actor = [0x66u8; 32];
        // No segment was ever written for this actor: a mirror row that points
        // at segment 1 is a divergence. The size path must report None, never
        // error — one bad record can't 500 the whole IMAP listing.
        let rid = [0x01u8; 32];
        let refs: Vec<(u32, Cid)> = vec![(1, Cid::from_digest_dag_cbor(rid))];
        let sizes = crate::segments::record_sizes(&manager, &actor, &refs)
            .await
            .expect("missing segment is tolerated, not an error");
        assert_eq!(sizes, vec![None]);
    }

    /// Plan 5 T6: `AppendOutcome.finalized` carries the just-closed segment id
    /// when an append rotates the bucket. Two appends in 2024-05 keep
    /// `finalized = None`; a third append in 2024-06 closes seg 1, returning
    /// `Some(1)`. (The first-ever append also returns None — there was no
    /// previously-open segment.)
    #[tokio::test]
    async fn append_outcome_finalized_carries_rotated_seg_id() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0xb1u8; 32];

        let outcome_a = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"a".to_vec()),
            &at_rest(b"".to_vec()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("a");
        assert_eq!(outcome_a.seg_id, 1);
        assert!(outcome_a.finalized.is_none());

        let outcome_b = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"b".to_vec()),
            &at_rest(b"".to_vec()),
            floor(1_715_000_001_000),
        )
        .await
        .expect("b");
        assert_eq!(outcome_b.seg_id, 1);
        assert!(outcome_b.finalized.is_none(), "same bucket → no rotation");

        let outcome_c = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"c".to_vec()),
            &at_rest(b"".to_vec()),
            floor(1_718_000_000_000),
        )
        .await
        .expect("c");
        assert_eq!(outcome_c.seg_id, 2, "new bucket → next segment");
        assert_eq!(
            outcome_c.finalized,
            Some(1),
            "rotation closed seg 1 → AppendOutcome.finalized = Some(1)"
        );
    }

    /// `SegmentManager::flush` returns the closed segment id, propagating the
    /// underlying `FramedSegmentStore::finalize_open` return.
    #[tokio::test]
    async fn flush_returns_closed_segment_id() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0xb2u8; 32];
        // Nothing open yet → None.
        assert!(manager.flush(&actor).await.expect("flush none").is_none());
        // After one append, flush returns Some(1).
        append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"x".to_vec()),
            &at_rest(b"".to_vec()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append");
        assert_eq!(manager.flush(&actor).await.expect("flush"), Some(1));
        // Idempotent — second flush returns None.
        assert!(manager.flush(&actor).await.expect("flush again").is_none());
    }

    #[tokio::test]
    async fn bucket_change_rotates_and_two_segments_appear() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x88u8; 32];

        // Two appends in the same bucket, then one in the next.
        let rid_a = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"a".to_vec()),
            &at_rest(b"".to_vec()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("a")
        .cid
        .digest();
        let rid_b = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"b".to_vec()),
            &at_rest(b"".to_vec()),
            floor(1_715_000_001_000),
        )
        .await
        .expect("b")
        .cid
        .digest();
        // Different bucket — one month forward.
        let rid_c = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"c".to_vec()),
            &at_rest(b"".to_vec()),
            floor(1_718_000_000_000),
        )
        .await
        .expect("c")
        .cid
        .digest();

        // Finalize the open segment (segment 2, containing record 3) so the
        // bulk read can open it. Segment 1 was already finalized when the third
        // append rotated buckets.
        manager.flush(&actor).await.expect("flush");

        let envs = read_envelopes_bulk(&manager, &cache_db, &actor, &[&rid_a, &rid_b, &rid_c])
            .await
            .expect("bulk");
        assert!(envs[0].is_some());
        assert!(envs[1].is_some());
        assert!(envs[2].is_some());
    }

    #[tokio::test]
    async fn missing_record_returns_none() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x99u8; 32];
        let r = read_envelope(&manager, &cache_db, &actor, &[0u8; 32])
            .await
            .expect("ok");
        assert!(r.is_none());
    }

    /// `read_envelope` finalises the open segment as part of the read, so a
    /// write followed by a read in the same bucket Just Works without an
    /// explicit `flush` call from the caller. This is the read-your-own-writes
    /// policy in action.
    #[tokio::test]
    async fn read_after_write_in_same_bucket_does_not_need_explicit_flush() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0xabu8; 32];
        let body = b"hot-body".to_vec();
        let hint = b"hot-hint".to_vec();

        let record_id = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(body.clone()),
            &at_rest(hint.clone()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append")
        .cid
        .digest();

        // No explicit manager.flush() call — read_envelope's flush-on-read
        // should finalise the open segment.
        let env = read_envelope(&manager, &cache_db, &actor, &record_id)
            .await
            .expect("read")
            .expect("present");
        let decoded = MailRecordEnvelope::decode(&env).expect("decode");
        assert_eq!(decoded.encrypted_body, body);
        assert_eq!(decoded.encrypted_index_hint, hint);
    }

    /// `read_record_with_floor` returns both the envelope and the decoded
    /// `MailFloorMetadata`.
    #[tokio::test]
    async fn read_record_with_floor_returns_envelope_and_floor() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0xeeu8; 32];
        let body = b"floor-body".to_vec();
        let hint = b"floor-hint".to_vec();
        let f = floor(1_715_000_000_000);

        let record_id = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(body.clone()),
            &at_rest(hint.clone()),
            f.clone(),
        )
        .await
        .expect("append")
        .cid
        .digest();

        let (env, floor_meta) = read_record_with_floor(&manager, &cache_db, &actor, &record_id)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(env.encrypted_body, body);
        assert_eq!(env.encrypted_index_hint, hint);
        assert_eq!(floor_meta.received_at, f.received_at);
        assert_eq!(floor_meta.sender_domain, f.sender_domain);
        assert_eq!(floor_meta.spam_disposition, f.spam_disposition);
        assert_eq!(floor_meta.spf, f.spf);
    }

    /// `describe_segment_for_backup` returns the framed file's blake3 + size
    /// and the segment header's bucket / record_count / created_at, plus the
    /// `segment_records` mirror's tombstone count for that segment. Plan 5 T4 —
    /// feeds the `fauna.segments.list` reply.
    #[tokio::test]
    async fn describe_segment_for_backup_round_trip() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x12u8; 32];

        // Unique bodies — identical bytes would dedup under content-hash
        // identity, and the test needs 3 records.
        let mut first_cid = None;
        for i in 0u8..3 {
            let outcome = append_record(
                &manager,
                &cache_db,
                &actor,
                &at_rest(format!("body-{i}").into_bytes()),
                &at_rest(b"hint".to_vec()),
                floor(1_715_000_000_000),
            )
            .await
            .expect("append");
            first_cid.get_or_insert(outcome.cid);
        }
        // Finalize so size/blake3 are stable (otherwise the open segment's
        // footer hasn't been written).
        manager.finalize_open(&actor).await.expect("finalize");

        // Tombstone one of the records so tombstone_count is non-zero.
        cache_db
            .segment_records_mark_tombstoned(&actor, "mail", 1, &first_cid.unwrap())
            .await
            .expect("tombstone");

        let meta = describe_segment_for_backup(&manager, &cache_db, &actor, 1)
            .await
            .expect("describe");
        assert_eq!(meta.bucket, "2024-05");
        assert_eq!(meta.record_count, 3);
        assert_eq!(meta.tombstone_count, 1);
        assert!(meta.size_bytes > 0);
        // file_blake3 matches the on-disk file's hash.
        let path = manager.segment_file_path(&actor, 1);
        let bytes = std::fs::read(&path).expect("read seg");
        let expected = blake3::hash(&bytes);
        assert_eq!(meta.file_blake3, *expected.as_bytes());
    }

    /// `SegmentManager::segment_file_path` formats the on-disk path the same way
    /// `FramedSegmentStore::segment_path` does — `seg-NNNNNNNN.dat` under the
    /// actor's mail segment root. Plan 5 T5 reads from this path.
    #[tokio::test]
    async fn segment_file_path_matches_on_disk_layout() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x34u8; 32];
        append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(b"body".to_vec()),
            &at_rest(b"hint".to_vec()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append");
        manager.finalize_open(&actor).await.expect("finalize");

        let path = manager.segment_file_path(&actor, 1);
        assert!(
            path.exists(),
            "segment_file_path must locate the actual on-disk file"
        );
        assert!(path.file_name().unwrap().to_string_lossy() == "seg-00000001.dat");
    }

    #[tokio::test]
    async fn read_record_with_floor_missing_returns_none() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x11u8; 32];
        let r = read_record_with_floor(&manager, &cache_db, &actor, &[0u8; 32])
            .await
            .expect("ok");
        assert!(r.is_none());
    }

    /// A body over the part cap rests as parts + a v3 head, and
    /// `read_sealed_body_with_floor` rejoins it byte-for-byte. Pins the
    /// continuation writer ⋈ serve-join round trip (message-segment-store.md
    /// § Continuation records).
    #[tokio::test]
    async fn continuation_writer_and_serve_join_round_trip() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x9au8; 32];
        // 2.5 MiB of position-varied bytes → 3 parts (1 MiB cap) with distinct
        // content (so no accidental content-address dedup between parts).
        let body: Vec<u8> = (0..2_500_000u32).map(|i| (i % 251) as u8).collect();
        let hint = b"sealed-index-hint".to_vec();

        let outcome = append_continuation_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(body.clone()),
            &at_rest(hint.clone()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append continuation");
        manager.flush(&actor).await.expect("flush");

        // The head resolves under the message id — the content hash of the
        // head's stored bytes — like a normal record, and decodes as v3.
        let head_cid = outcome.cid;
        let record_id = head_cid.digest();
        let head_row = cache_db
            .segment_records_lookup_record(&actor, "mail", &head_cid)
            .await
            .unwrap()
            .expect("head row present");
        assert_eq!(head_row.segment_id, outcome.seg_id);
        let head_bytes = manager
            .read_envelope_bytes(&actor, head_row.segment_id, &head_cid)
            .await
            .expect("read head bytes");
        let head = match MailRecord::decode(&head_bytes).expect("decode head") {
            MailRecord::Head(h) => h,
            MailRecord::Inline(_) => panic!("head must decode as v3"),
        };
        assert_eq!(head.part_digests.len(), 3, "2.5 MiB / 1 MiB = 3 parts");
        assert_eq!(head.total_body_len, body.len() as u64);

        // Serve-join rejoins the body byte-for-byte + returns the head's hint.
        let (joined, joined_hint, joined_floor) =
            read_sealed_body_with_floor(&manager, &cache_db, &actor, &record_id)
                .await
                .expect("serve-join")
                .expect("record present");
        assert_eq!(joined, body, "rejoined body must equal the original");
        assert_eq!(joined_hint, hint);
        assert_eq!(joined_floor.continuation_role, CONTINUATION_ROLE_HEAD);

        // Parts append before the head (lower seqs) so tombstone_up_to_seq(head)
        // reclaims the family; the head carries the highest seq.
        assert_eq!(joined_floor.seq, 4, "3 parts (seq 1-3) then head (seq 4)");

        // The inline-only decoder refuses the head — never a silent empty body.
        let inline = read_record_with_floor(&manager, &cache_db, &actor, &record_id).await;
        assert!(
            inline.is_err(),
            "read_record_with_floor (inline-only) must error on a v3 head, not misread it"
        );
    }

    /// A **relayed** part must not be reaped just because the MESSAGE it belongs
    /// to is old. The reaper's grace asks "has this record been here a while?",
    /// and only `stored_at` answers that: `received_at` is forwarded verbatim by
    /// the relay, so on a backfill/catch-up relay a part is born days past the
    /// grace. Reaping it destroys the body before its head lands — and the source
    /// has already purged on ack, so the loss is user-irrecoverable
    /// (message-segment-store.md § Continuation records; the No-user-data-loss
    /// invariant).
    ///
    /// Clock note: `stored_at` is stamped from the real wall clock at append, so
    /// the reap instants are expressed relative to a real `now` rather than a
    /// frozen fixture epoch. The grace is an hour — no ms-scale flakiness.
    #[tokio::test]
    async fn a_relayed_historical_part_is_not_reaped_before_its_head_arrives() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x2bu8; 32];
        let now_ms = stored_at_now_ms();

        // Backfill relay: the peer forwards a 30-day-old message, and its parts
        // arrive in an earlier page than their head.
        let mut part_floor = floor(now_ms - 30 * 24 * 3_600_000);
        part_floor.continuation_role = CONTINUATION_ROLE_PART;
        // The peer's own stored_at is old too — and must be ignored, not honoured.
        part_floor.stored_at = now_ms - 30 * 24 * 3_600_000;
        append_sealed_record(
            &manager,
            &cache_db,
            &actor,
            b"a-raw-ciphertext-range",
            part_floor,
        )
        .await
        .expect("relay stores the peer's part verbatim");

        // The head has not arrived yet, so nothing references the part. The
        // compaction worker's reap pre-pass runs in exactly that gap.
        let reaped = reap_headless_parts(&manager, &cache_db, &actor, now_ms)
            .await
            .expect("reap");
        assert_eq!(
            reaped, 0,
            "a part relayed seconds ago is inside the grace no matter how old the \
             MESSAGE is — reaping it loses the body before its head lands"
        );

        // ...and age alone never licenses the reap either: the head is still in
        // flight (a family always spans pull pages, and the relay acked
        // mid-family, so the source has already purged these parts). Reaping here
        // — however genuinely aged the part now is — is exactly the
        // mid-family-ack body loss.
        let reaped = reap_headless_parts(&manager, &cache_db, &actor, now_ms + 2 * 3_600_000)
            .await
            .expect("reap");
        assert_eq!(
            reaped, 0,
            "an aged part whose head has not relayed yet is awaiting its head, not \
             orphaned — reaping it loses the body the source already purged"
        );

        // ...and the reaper is still a reaper: once a LATER head proves this
        // part's own head was never coming (its family write crashed — parts and
        // head take consecutive seqs under one lock, and the relay never skips),
        // the aged part is reclaimed. Without this, the two asserts above could be
        // 'achieved' by never reaping anything.
        // A real, decodable v3 head referencing its OWN part (never the orphan
        // above — that must stay unreferenced, or the live-head rule would spare
        // it for a different reason and this assert would prove nothing).
        let later_head = MailContinuationHead::new(vec![[0xbbu8; 32]], 16, b"later-hint".to_vec());
        let later_head_bytes = later_head.encode().expect("encode v3 head");
        let mut later_head_floor = floor(now_ms);
        later_head_floor.continuation_role = CONTINUATION_ROLE_HEAD;
        append_sealed_record(
            &manager,
            &cache_db,
            &actor,
            &later_head_bytes,
            later_head_floor,
        )
        .await
        .expect("a subsequent family's head relays");

        let reaped = reap_headless_parts(&manager, &cache_db, &actor, now_ms + 2 * 3_600_000)
            .await
            .expect("reap");
        assert_eq!(
            reaped, 1,
            "a genuinely aged headless part must still be reclaimed once a head \
             above it proves it orphaned"
        );
    }

    /// A head whose declared `total_body_len` its own part list could never hold
    /// is refused **before** anything is allocated — the head is untrusted input
    /// (a relay stores a peer's head verbatim, envelope opaque), so sizing the
    /// join from it would hand a corrupt/hostile peer a panic or an OOM abort of
    /// the whole nest process, re-triggered on every retry because the head
    /// persists.
    ///
    /// `u64::MAX` is deliberate: it makes the *old* behaviour fail as a clean
    /// `Vec::with_capacity` capacity-overflow panic. The realistic attack value
    /// (a plausible 8–100 GiB) is asserted below only because the guard now
    /// rejects it without allocating — do **not** reorder that case ahead of the
    /// guard to "prove" the red, it would really try to allocate and take the
    /// OOM killer to a shared dev machine.
    #[tokio::test]
    async fn a_continuation_head_over_declaring_its_body_errors_without_allocating() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x3fu8; 32];

        // One part digest that is never stored: the guard must fire before the
        // fetch loop, so whether the part exists is irrelevant.
        let head = MailContinuationHead::new(vec![[0xaau8; 32]], u64::MAX, b"hint".to_vec());
        let head_bytes = head.encode().expect("encode v3 head");
        let mut head_floor = floor(1_715_000_000_000);
        head_floor.continuation_role = CONTINUATION_ROLE_HEAD;

        let record_id = append_sealed_record(&manager, &cache_db, &actor, &head_bytes, head_floor)
            .await
            .expect("relay stores the peer's head verbatim")
            .cid
            .digest();
        manager.flush(&actor).await.expect("flush");

        let err = read_sealed_body_with_floor(&manager, &cache_db, &actor, &record_id)
            .await
            .expect_err("an over-declaring head must be an honest error, not a panic/abort");
        let msg = err.to_string();
        assert!(
            msg.contains("part(s) can hold at most"),
            "expected the structural over-declaration error, got: {msg}"
        );

        // The realistic shape: a plausible total that would have been allocated.
        let head =
            MailContinuationHead::new(vec![[0xabu8; 32]], 8 * 1024 * 1024 * 1024, b"hint".to_vec());
        let head_bytes = head.encode().expect("encode v3 head");
        let mut head_floor = floor(1_715_000_000_000);
        head_floor.continuation_role = CONTINUATION_ROLE_HEAD;
        let record_id = append_sealed_record(&manager, &cache_db, &actor, &head_bytes, head_floor)
            .await
            .expect("relay stores the peer's head verbatim")
            .cid
            .digest();
        manager.flush(&actor).await.expect("flush");
        let err = read_sealed_body_with_floor(&manager, &cache_db, &actor, &record_id)
            .await
            .expect_err("a plausible over-declaration must error, not allocate 8 GiB");
        assert!(
            err.to_string().contains("part(s) can hold at most"),
            "expected the structural over-declaration error, got: {err}"
        );
    }

    /// **The repeat walk** — a head that names one stored part many times is refused
    /// before the repeats are read. The head is relay-authored and untrusted,
    /// and a declared total that matches the repeats passes the
    /// `parts × MAX_RECORD_LEN` guard, so without this one small part named
    /// tens of thousands of times would be read into memory whole. An honest
    /// writer never repeats a part: each is a distinct range of one AEAD
    /// ciphertext.
    #[tokio::test]
    async fn a_continuation_head_repeating_one_part_is_refused() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x4du8; 32];

        let part = b"a-raw-ciphertext-range";
        let mut part_floor = floor(1_715_000_000_000);
        part_floor.continuation_role = CONTINUATION_ROLE_PART;
        let part_digest = append_sealed_record(&manager, &cache_db, &actor, part, part_floor)
            .await
            .expect("relay stores the peer's part verbatim")
            .cid
            .digest();

        let repeats = 5_000usize;
        let head = MailContinuationHead::new(
            vec![part_digest; repeats],
            (part.len() * repeats) as u64,
            b"hint".to_vec(),
        );
        let head_bytes = head.encode().expect("encode v3 head");
        let mut head_floor = floor(1_715_000_000_000);
        head_floor.continuation_role = CONTINUATION_ROLE_HEAD;
        let record_id = append_sealed_record(&manager, &cache_db, &actor, &head_bytes, head_floor)
            .await
            .expect("relay stores the peer's head verbatim")
            .cid
            .digest();
        manager.flush(&actor).await.expect("flush");

        let err = read_sealed_body_with_floor(&manager, &cache_db, &actor, &record_id)
            .await
            .expect_err("a head repeating one part must be refused, not joined");
        assert!(
            err.to_string().contains("more than once"),
            "expected the repeated-part refusal, got: {err}"
        );
    }

    /// A continuation family relays as **frame-sized parts + head** and the
    /// destination serves the body byte-for-byte — the deployment-critical
    /// property (a >2 MiB body can now cross the 2 MiB federation frame). Models
    /// the relay at the `segments::mail` level (source `read_after_seq` →
    /// destination `append_sealed_record`), the same seam
    /// `relay_round_trip_read_reseal_and_purge` proves for inline records.
    #[tokio::test]
    async fn continuation_family_relays_and_serves_byte_for_byte() {
        // SOURCE (public relay box): store an over-frame body as a family.
        let (_src_tmp, src_mgr, src_db) = setup();
        let actor = [0x5cu8; 32];
        // 3 MiB → 3 parts, each a ~1 MiB record well under the 2 MiB frame.
        let body: Vec<u8> = (0..3_000_000u32)
            .map(|i| (i.wrapping_mul(31) % 253) as u8)
            .collect();
        let hint = b"idx-hint".to_vec();
        let record_id = append_continuation_record(
            &src_mgr,
            &src_db,
            &actor,
            &at_rest(body.clone()),
            &at_rest(hint.clone()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append family on source")
        .cid
        .digest();
        src_mgr.flush(&actor).await.expect("flush src");

        // Drain the relay page (parts first by seq, head last) and assert every
        // relayed record fits the 2 MiB federation frame.
        let relayed = read_after_seq(&src_mgr, &src_db, &actor, 0, 500)
            .await
            .expect("read_after_seq");
        assert_eq!(relayed.len(), 4, "3 parts + 1 head");
        let frame = fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES as usize;
        for (_seq, _rid, env_bytes, fl) in &relayed {
            assert!(
                env_bytes.len() < frame,
                "every relayed record must fit the federation frame; got {}",
                env_bytes.len()
            );
            let _ = fl; // roles carried on the floor
        }
        // Parts (role=1) precede the head (role=2) in seq order.
        assert_eq!(relayed[0].3.continuation_role, CONTINUATION_ROLE_PART);
        assert_eq!(relayed[3].3.continuation_role, CONTINUATION_ROLE_HEAD);

        // DESTINATION (home box): re-append each relayed record verbatim.
        let (_dst_tmp, dst_mgr, dst_db) = setup();
        for (_seq, _rid, env_bytes, fl) in relayed {
            append_sealed_record(&dst_mgr, &dst_db, &actor, &env_bytes, fl)
                .await
                .expect("append_sealed_record on destination");
        }
        dst_mgr.flush(&actor).await.expect("flush dst");

        // Serve-join on the destination recovers the body byte-for-byte — the
        // head's part-CID list resolves because a part's wire id is its content
        // digest, so the destination reconstructs the same CIDs.
        let (joined, joined_hint, _f) =
            read_sealed_body_with_floor(&dst_mgr, &dst_db, &actor, &record_id)
                .await
                .expect("serve-join on destination")
                .expect("head present on destination");
        assert_eq!(
            joined, body,
            "destination serves the relayed body byte-for-byte"
        );
        assert_eq!(joined_hint, hint);
    }

    /// **The mid-family-ack stall, end to end**. The relay's purge-on-ack is not family-atomic, and a family
    /// *always* spans pull pages (parts are 1 MiB, the page budget is one 2 MiB
    /// frame minus headroom). So the destination stores a page of parts, acks its
    /// last seq — **mid-family** — and the source purges the parts it just handed
    /// over. The parts now exist *only* on the destination.
    ///
    /// If the head's page is then delayed past the reap grace (a reboot, a VPS
    /// migration, a source outage — hours-long stalls are routine) and a
    /// compaction pass lands in the gap, the destination's reaper used to eat its
    /// own only copy: the head arrived later, its join found nothing to rejoin,
    /// and the re-pull could not heal because the source had purged. That is
    /// user-irrecoverable body loss with the head IMAP-visible — the
    /// No-user-data-loss class, needing no hostile actor, just bad luck.
    ///
    /// This drives the whole sequence: parts page → mid-family ack → source purge
    /// → stall past the grace → reap pass → head arrives → join.
    #[tokio::test]
    async fn a_family_whose_head_relays_after_a_long_stall_still_serves_its_body() {
        // SOURCE (public relay box): an over-cap body rests as a family.
        let (_src_tmp, src_mgr, src_db) = setup();
        let actor = [0x5du8; 32];
        let body: Vec<u8> = (0..3_000_000u32)
            .map(|i| (i.wrapping_mul(17) % 251) as u8)
            .collect();
        let hint = b"idx-hint".to_vec();
        let record_id = append_continuation_record(
            &src_mgr,
            &src_db,
            &actor,
            &at_rest(body.clone()),
            &at_rest(hint.clone()),
            floor(1_715_000_000_000),
        )
        .await
        .expect("append family on source")
        .cid
        .digest();
        src_mgr.flush(&actor).await.expect("flush src");

        let relayed = read_after_seq(&src_mgr, &src_db, &actor, 0, 500)
            .await
            .expect("read_after_seq");
        assert_eq!(relayed.len(), 4, "3 parts + 1 head");

        // PAGE 1 — the parts, without their head: the page boundary lands
        // mid-family, which is the norm and not the exception.
        let (_dst_tmp, dst_mgr, dst_db) = setup();
        let mut last_part_seq = 0i64;
        for (seq, _rid, env_bytes, fl) in relayed.iter().take(3) {
            assert_eq!(fl.continuation_role, CONTINUATION_ROLE_PART);
            append_sealed_record(&dst_mgr, &dst_db, &actor, env_bytes, fl.clone())
                .await
                .expect("destination stores the relayed part");
            last_part_seq = *seq;
        }
        dst_mgr.flush(&actor).await.expect("flush dst");

        // The destination acks the page's last seq (mid-family) and the source
        // purges through it. The only copy of the parts is now the destination's.
        let purged = tombstone_up_to_seq(&src_db, &actor, last_part_seq)
            .await
            .expect("source purges on the mid-family ack");
        assert_eq!(purged, 3, "the source purged the parts it handed over");

        // THE STALL: the head's page is delayed past the grace, and the
        // compaction worker's reap pre-pass runs in exactly that gap.
        let reaped = reap_headless_parts(
            &dst_mgr,
            &dst_db,
            &actor,
            stored_at_now_ms() + 2 * 3_600_000,
        )
        .await
        .expect("reap");
        assert_eq!(
            reaped, 0,
            "the parts are awaiting their head, not orphaned — reaping them here \
             loses a body no re-pull can heal, because the source already purged"
        );

        // PAGE 2: the head finally relays.
        let (_seq, _rid, env_bytes, fl) = relayed[3].clone();
        assert_eq!(fl.continuation_role, CONTINUATION_ROLE_HEAD);
        append_sealed_record(&dst_mgr, &dst_db, &actor, &env_bytes, fl)
            .await
            .expect("destination stores the relayed head");
        dst_mgr.flush(&actor).await.expect("flush dst");

        // The body survives the whole sequence, byte for byte.
        let (joined, joined_hint, _f) =
            read_sealed_body_with_floor(&dst_mgr, &dst_db, &actor, &record_id)
                .await
                .expect("serve-join on destination")
                .expect("head present on destination");
        assert_eq!(
            joined, body,
            "the body survives a mid-family ack + a relay stall past the reap grace"
        );
        assert_eq!(joined_hint, hint);
    }

    /// `append_record` routes an over-cap body to the continuation writer.
    #[tokio::test]
    async fn append_record_routes_an_over_cap_body_to_continuation() {
        let (_tmp, manager, cache_db) = setup();
        let actor = [0x33u8; 32];
        // > MAIL_BODY_PART_CAP_BYTES (1 MiB) so the split triggers.
        let big: Vec<u8> = (0..1_500_000u32).map(|i| (i % 250) as u8).collect();

        let res = append_record(
            &manager,
            &cache_db,
            &actor,
            &at_rest(big.clone()),
            &at_rest(b"h".to_vec()),
            floor(1_715_000_000_000),
        )
        .await;
        let record_id = res.expect("append via gate").cid.digest();
        manager.flush(&actor).await.expect("flush");

        // append_record split it: the record at the message id is a v3 head, and
        // serve-join recovers the body.
        let stored = read_envelope(&manager, &cache_db, &actor, &record_id)
            .await
            .expect("read")
            .expect("present");
        assert!(
            matches!(
                MailRecord::decode(&stored).expect("decode"),
                MailRecord::Head(_)
            ),
            "gate ON → append_record must route an over-cap body to the continuation writer"
        );
        let (joined, _h, _f) = read_sealed_body_with_floor(&manager, &cache_db, &actor, &record_id)
            .await
            .expect("join")
            .expect("present");
        assert_eq!(joined, big);
    }
}
