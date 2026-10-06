//! Calendar-kind nest-side segment coordination (S6.4).
//!
//! Sibling of [`super::mail`] / [`super::post`] for CalDAV event bodies. The
//! file/manifest state machine is the same kind-agnostic
//! `fauna_segment_store::SegmentManager` (`AppState.cal_segments`, registered
//! with `kind = "calendar"`); what remains nest-specific is the coordination
//! between it and the `segment_records` SQLite mirror — these free functions
//! over `(&SegmentManager, &CacheDb, …)`.
//!
//! ### Where calendar sits between mail and post
//!
//! Like **post**, calendar is *point-read-by-CID*: there is no per-scope `seq`
//! cursor, so no per-actor append lock and no `seq` mirror column.
//!
//! Like **mail**, each record rides a typed envelope + floor, and — since the
//! 2026-08-17 record-identity cutover — its CID is the **content hash of the
//! encoded envelope bytes** (`CalRecordEnvelope::encode_record`;
//! `message-segment-store.md` § Record identity per kind). The CID is derived
//! at append and STORED on the `bridge_caldav_events` row (`record_cid`
//! column): `event_id` stays the placement key, but the filing identity can no
//! longer be re-derived from it, so every read path resolves through the
//! stored cid. The envelope is what makes snapshot restore able to rebuild a
//! whole `bridge_caldav_events` row from disk (restore reads the cids off the
//! CARv2 records themselves).
//!
//! ### Scoping
//!
//! Records are scoped by **actor**, not by calendar: `__calendar/<actor_hex>/`.
//! `calendar_id` is a sub-scope carried in the floor, exactly as mail carries
//! `mailbox`. Reads therefore resolve through the *actor-scoped* mirror lookup
//! ([`crate::db::CacheDb::segment_records_lookup_record`]) rather than post's
//! cross-scope `lookup_scope_and_segment`: an event CID must never resolve into
//! another actor's segment.

use anyhow::{Context, Result};
use fauna_calendar::segments::{CalFloorMetadata, CalRecordEnvelope};
use fauna_cbor::Cid;
use fauna_mls::wrapped_blob::SealedRecordBytes;
use fauna_segment_store::{CompactionPlan, SegmentManager};

use super::mail::BucketCompactionOutcome;
use super::records_db::{self, NewPointReadSegmentRecord};

use crate::db::CacheDb;
use crate::db::bridge_caldav::EventRow;

pub const KIND: &str = "calendar";

/// Result of [`append_record`].
#[derive(Debug, Clone, Copy)]
pub struct CalAppendOutcome {
    /// The record's filing CID — `Cid::of_dag_cbor(<encoded envelope bytes>)`,
    /// derived at the append (never caller-supplied) and stored on the
    /// `bridge_caldav_events` row, since the identity is no longer re-derivable
    /// from the PK.
    pub record_cid: Cid,
    /// Segment id the record landed in (live tail of the manifest).
    pub seg_id: u32,
    /// `Some(closed_seg_id)` if this append rotated a previously-open segment
    /// closed; `None` otherwise.
    pub finalized: Option<u32>,
}

/// Append one calendar event record to its owner's `__calendar` segment store,
/// then mirror it into `segment_records`.
///
/// The body and index hint arrive as [`SealedRecordBytes`] — the S6.12
/// structural seal gate. `__calendar` is backup-eligible, so nothing unsealed
/// may enter it; demanding the proven-sealed type here (instead of trusting
/// the caller's convention) makes a forgotten seal at a new ingest site a
/// compile error, and the [`CalRecordEnvelope`] is constructed only inside
/// this module so no caller can smuggle raw bytes around the gate.
///
/// The bucket keys on `floor.created_at` — the **server receive time**, in epoch
/// **seconds**. Two traps a mail-shaped copy would fall into: mail's
/// `received_at` is milliseconds (hence its `/1000`), and bucketing on
/// `internal_date` — the event's *own* time — would file a meeting scheduled for
/// 2030 into a 2030 bucket, stranding it from compaction and backup.
pub async fn append_record(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    body: &SealedRecordBytes,
    hint: &SealedRecordBytes,
    floor: &CalFloorMetadata,
) -> Result<CalAppendOutcome> {
    let envelope = CalRecordEnvelope::new(body.as_slice().to_vec(), hint.as_slice().to_vec());
    let (cid, env_bytes) = envelope
        .encode_record()
        .map_err(|e| anyhow::anyhow!("serialize cal envelope: {e}"))?;
    append_encoded(mgr, cache_db, actor, cid, &env_bytes, floor).await
}

/// The shared tail of [`append_record`] / [`ensure_in_segment`]: file
/// pre-encoded envelope bytes under their (already-derived) content-hash CID.
async fn append_encoded(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    cid: Cid,
    env_bytes: &[u8],
    floor: &CalFloorMetadata,
) -> Result<CalAppendOutcome> {
    let floor_bytes = floor
        .encode()
        .map_err(|e| anyhow::anyhow!("serialize cal floor: {e}"))?;
    let bucket = fauna_segment_store::bucket_for(floor.created_at);

    let outcome = mgr
        .append_record_with_bucket(actor, cid, env_bytes, &floor_bytes, &bucket)
        .await
        .map_err(|e| anyhow::anyhow!("append_record_with_bucket (calendar): {e}"))?;

    // Mirror INSERT — the kind-agnostic SegmentManager has no DB coupling; this
    // is the sole place a calendar `segment_records` row is written on append.
    cache_db
        .segment_records_insert_calendar(actor, outcome.segment_id, &cid, &bucket, floor.created_at)
        .await
        .context("mirror calendar segment record")?;

    Ok(CalAppendOutcome {
        record_cid: cid,
        seg_id: outcome.segment_id,
        finalized: outcome.finalized,
    })
}

/// Append one **recovered** calendar record — the lived-in recovery's write
/// (`segment-backup-protocol.md` § Client-device custodian (pull) → *Restore*
/// → *Recovery's calendar and contacts arms*). The envelope bytes and floor
/// ride verbatim; `place` files the event row in the mirror row's own
/// transaction. `Ok(None)`: the target holds the record, live or tombstoned,
/// and nothing was written. Body: [`super::append_recovered_point_read_record`].
pub(crate) async fn append_recovered_record<T>(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    envelope_bytes: &[u8],
    floor: &CalFloorMetadata,
    place: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
) -> Result<Option<(u32, T)>> {
    let floor_bytes = floor
        .encode()
        .map_err(|e| anyhow::anyhow!("serialize cal floor: {e}"))?;
    super::append_recovered_point_read_record(
        mgr,
        cache_db,
        actor,
        KIND,
        envelope_bytes,
        &floor_bytes,
        floor.created_at,
        records_db::insert_calendar,
        place,
    )
    .await
}

/// Idempotently ensure the record is present in `actor`'s `__calendar` segment
/// store; append only when the mirror has no live row for its CID. Returns the
/// record's content-hash CID either way — the caller stores it on the
/// `bridge_caldav_events` row (reads can no longer re-derive it from the PK).
///
/// The mirror's `record_cid` index is **non-UNIQUE**, so this skip-if-present
/// guard is load-bearing: it is what makes the PUT handler's crash window
/// (append durable, row insert not yet) heal on retry rather than
/// double-append. Under content-hash identity the guard is a *byte-replay*
/// dedup — deliberately sound here because the PUT path files the client's
/// already-sealed bytes verbatim, so a retry re-encodes the identical envelope
/// and re-derives the identical CID.
pub async fn ensure_in_segment(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    body: &SealedRecordBytes,
    hint: &SealedRecordBytes,
    floor: &CalFloorMetadata,
) -> Result<Cid> {
    let envelope = CalRecordEnvelope::new(body.as_slice().to_vec(), hint.as_slice().to_vec());
    let (cid, env_bytes) = envelope
        .encode_record()
        .map_err(|e| anyhow::anyhow!("serialize cal envelope: {e}"))?;
    if cache_db
        .segment_records_lookup_record(actor, KIND, &cid)
        .await?
        .is_none()
    {
        append_encoded(mgr, cache_db, actor, cid, &env_bytes, floor).await?;
    }
    Ok(cid)
}

/// Tombstone `actor`'s orphaned `__calendar` content records — live mirror rows
/// whose `bridge_caldav_events` row is gone (a rejected PUT or a crash between the
/// append and the row INSERT). Returns
/// the number tombstoned, which compaction then physically reclaims.
///
/// The live-CID read and the tombstones share **one** held connection lock: a
/// PUT's row INSERT takes the same lock, so it cannot commit a row pointing at a
/// record between our read and our write. See
/// [`records_db::reap_orphan_point_read_records`] for the full safety argument
/// (the live/kind pairing, that ordering, the age watermark, and the
/// empty-live-set fail-closed guard are each load-bearing — dropping any one
/// can tombstone a live body).
///
/// Callers must hold the `"gc"` op lock (the compaction worker does). S6.9
/// restore MUST take it too, or a restore caught between its mirror rebuild and
/// its row rebuild will present its whole corpus as orphaned.
pub async fn reap_orphan_records(cache_db: &CacheDb, actor: &[u8; 32], now: i64) -> Result<u32> {
    super::reap_orphan_records(cache_db, actor, now, super::SegmentKind::Calendar).await
}

/// The table whose live rows hold calendar's reachable record CIDs — across all
/// of the actor's calendars (the mirror is actor-scoped; `calendar_id` is a
/// sub-scope inside the floor, not a separate segment store).
///
/// Read through [`super::live_record_cids`], which reads the STORED
/// `record_cid` (the identity is the content hash, not re-derivable from the
/// PK) and fails the reap closed on a NULL — see it for the full argument.
/// `pub(super)` only: reachable exclusively through
/// [`super::SegmentKind::Calendar`], never as a standalone string a caller
/// could pair with the wrong `kind`.
pub(super) const LIVE_ROWS_TABLE: &str = "bridge_caldav_events";

/// Read one event's envelope + floor from `actor`'s segment store, by its
/// STORED content-hash `record_cid` (the caller holds it on the
/// `bridge_caldav_events` row — the identity is not re-derivable from the PK).
///
/// `None` when the mirror has no live row, or when it points at a record the
/// segment file does not hold (mirror/disk divergence — warned and treated as
/// absent, mirroring `post::read_body_by_cid`, so one bad record never fails a
/// whole REPORT).
pub async fn read_record(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    record_cid: &Cid,
) -> Result<Option<(CalRecordEnvelope, CalFloorMetadata)>> {
    let Some((env_bytes, floor_bytes)) =
        super::read_point_read_record(mgr, cache_db, actor, KIND, record_cid).await?
    else {
        return Ok(None);
    };
    let envelope = CalRecordEnvelope::decode(&env_bytes)
        .map_err(|e| anyhow::anyhow!("parse cal envelope: {e}"))?;
    let floor = CalFloorMetadata::decode(&floor_bytes)
        .map_err(|e| anyhow::anyhow!("parse cal floor: {e}"))?;
    Ok(Some((envelope, floor)))
}

/// Resolve one event's sealed body from the segment store, by the row's
/// stored `record_cid`.
///
/// The row carries no body of its own — the body rests only in the segment,
/// filed under the row's stored `record_cid`. A row missing its `record_cid`
/// is therefore corruption, warned and served as absent.
///
/// `None` means the event has no body — the segment miss proves it absent.
pub async fn load_event_body(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    row: &EventRow,
) -> Result<Option<Vec<u8>>> {
    super::load_body_via_record_cid(
        row.record_cid()?,
        || {
            tracing::warn!(
                actor = %hex::encode(actor),
                event_id = %hex::encode(row.event_id),
                "bridge_caldav_events row has no record_cid — cannot resolve its body"
            );
        },
        |cid| async move {
            Ok(read_record(mgr, cache_db, actor, &cid)
                .await?
                .map(|(envelope, _floor)| envelope.encrypted_body))
        },
    )
    .await
}

/// Rewrite one bucket of `actor`'s `__calendar` segments, dropping every record
/// the mirror marks tombstoned (S6.8c). Sibling of [`super::post::compact_bucket`].
///
/// The rewrite skeleton — live-set pre-fetch, candidate id, filtered file
/// rewrite, mirror transaction — is [`super::compact_bucket_with`], shared with
/// the other four kinds. What stays here is the pair that genuinely differs:
/// the floor decode (calendar's `created_at` is epoch **seconds**) and the
/// named `apply_calendar_compaction_tx` wrapper.
pub async fn compact_bucket(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    plan: &CompactionPlan,
) -> Result<BucketCompactionOutcome> {
    super::compact_bucket_with(
        mgr,
        cache_db,
        actor,
        KIND,
        plan,
        |rid, floor_bytes| {
            let floor = CalFloorMetadata::decode(floor_bytes)
                .map_err(|e| anyhow::anyhow!("parse cal floor during compaction: {e}"))?;
            // `created_at` is epoch **seconds** — do NOT copy post's `/ 1000`.
            // And the bucket keys on `created_at` (server receive), never
            // `internal_date`: a 2030 meeting must not create a 2030 bucket.
            Ok(NewPointReadSegmentRecord {
                record_cid: rid,
                bucket: fauna_segment_store::bucket_for(floor.created_at),
                received_at: floor.created_at,
            })
        },
        |tx, new_segment, new_records| {
            records_db::apply_calendar_compaction_tx(
                tx,
                actor,
                &plan.inputs,
                new_segment,
                new_records,
            )
            .context("apply_calendar_compaction_tx")
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_calendar::segments::CAL_ENVELOPE_FORMAT_VERSION;
    use fauna_mls::wrapped_blob::derive_recipient_hpke_keypair;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn floor_for(event_id: [u8; 32], created_at: i64) -> CalFloorMetadata {
        CalFloorMetadata {
            calendar_id: [0x11u8; 32],
            event_id,
            uid_hash: vec![0x33u8; 32],
            ciphertext_size: 11,
            internal_date: 1_714_999_900,
            created_at,
            ..Default::default()
        }
    }

    /// A fixed recipient pubkey for test seals — the seed is arbitrary; all that
    /// matters is that [`seal`] produces a genuine envelope `verify` accepts.
    fn test_pubkey() -> [u8; 32] {
        derive_recipient_hpke_keypair(&[0x5Eu8; 32]).1
    }

    /// Mint a proven-sealed record payload from `plaintext` — the S6.12 typed
    /// currency for a body or hint. Randomized (HPKE encapsulation), so bind it
    /// once and reuse the bytes when a test needs the same body twice.
    fn sealed_bytes(plaintext: &[u8]) -> SealedRecordBytes {
        SealedRecordBytes::verify(seal(plaintext, &test_pubkey())).expect("fixture verifies")
    }

    /// A serve-path row. `record_cid` is the only thing that resolves a body,
    /// so it is the parameter.
    fn row_for(event_id: [u8; 32], record_cid: Option<Cid>) -> EventRow {
        EventRow {
            event_id,
            uid_hash: vec![0x33u8; 32],
            encrypted_index_hint: b"sealed-hint".to_vec(),
            etag: "0000000000000002".to_string(),
            modseq: 2,
            ciphertext_size: 11,
            internal_date: 1_714_999_900,
            encrypted_fauna_ext: None,
            record_cid: record_cid.map(|c| c.as_bytes().to_vec()),
        }
    }

    async fn fixture() -> (TempDir, SegmentManager, Arc<CacheDb>) {
        let dir = TempDir::new().unwrap();
        let mgr = SegmentManager::new(dir.path().to_path_buf(), KIND);
        let db = Arc::new(CacheDb::open_in_memory().expect("db"));
        (dir, mgr, db)
    }

    /// A genuinely sealed body — `is_sealed_mail_record` must accept it, since
    /// the back-fill refuses to move anything else.
    fn seal(body: &[u8], pubkey: &[u8; 32]) -> Vec<u8> {
        crate::bridge_routing_handlers::seal_recipient_blob(body, pubkey, None, "body")
            .expect("seal body")
    }

    /// The stock hint a seeded row carries — pinned here so a test can
    /// re-derive the row's record identity (identity is the hash of the
    /// `body + hint` envelope).
    ///
    /// ⚠ **Randomized** — `seal` is HPKE, so every call returns different
    /// bytes. A caller that needs the row and its record to share an identity
    /// must bind this ONCE and pass the same bytes to both.
    fn seeded_hint() -> Vec<u8> {
        seal(b"sealed-hint", &test_pubkey())
    }

    /// The record cid a seeded row carries, given the exact `body` and `hint`
    /// bytes it was seeded with — the same mint `place_caldav_event` performs.
    fn seeded_row_cid(body: &[u8], hint: &[u8]) -> Cid {
        crate::db::bridge_caldav::caldav_record_cid(body, hint).expect("derive row cid")
    }

    /// Seed a `bridge_caldav_events` row whose content record was never
    /// appended: the row carries the cid its `body + hint` hash to, but
    /// nothing is in the segment. That is the orphaned-row half of the
    /// reaper/guard cases. The hint is explicit because `seeded_hint` is
    /// randomized — a caller linking the row to a real record must pass the
    /// same bytes it appends.
    async fn seed_row_only(
        db: &CacheDb,
        actor: &[u8; 32],
        cal: &[u8; 32],
        body: &[u8],
        hint: &[u8],
        internal_date: i64,
        created_at: i64,
    ) -> [u8; 32] {
        db.insert_bridge_caldav_calendar(actor, cal, b"meta", created_at)
            .await
            .unwrap();
        match db
            .place_caldav_event(
                actor,
                cal,
                &[0x33u8; 32],
                body,
                hint,
                internal_date,
                body.len() as u32,
                created_at,
            )
            .await
            .unwrap()
        {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("expected Created, got {other:?}"),
        }
    }

    /// The cutover's central rule: a calendar record's identity IS the hash of
    /// its envelope bytes (`message-segment-store.md` § Record identity per
    /// kind). This is what makes the kind adoptable at all — `segments::admit`
    /// re-hashes every block against the cid the container files it under, so a
    /// sequenced id (what this kind filed under before the cutover) is refused
    /// by construction.
    #[tokio::test]
    async fn record_identity_is_the_hash_of_the_envelope_bytes() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let body = sealed_bytes(b"sealed-body");
        let hint = sealed_bytes(b"sealed-hint");

        let outcome = append_record(
            &mgr,
            &db,
            &actor,
            &body,
            &hint,
            &floor_for([0xA0u8; 32], 1_715_000_000),
        )
        .await
        .unwrap();

        let (expected, env_bytes) =
            CalRecordEnvelope::new(body.as_slice().to_vec(), hint.as_slice().to_vec())
                .encode_record()
                .unwrap();
        assert_eq!(
            outcome.record_cid, expected,
            "the filing cid must be the content hash of the envelope"
        );
        assert_eq!(
            outcome.record_cid,
            Cid::of_dag_cbor(&env_bytes),
            "and that hash is plain dag-cbor over the encoded envelope"
        );
    }

    /// The identity is content-derived, so it cannot be a function of the PK:
    /// two records under the same `event_id` with different bodies must file
    /// under different cids. The pre-cutover `Cid::from_digest_dag_cbor(event_id)`
    /// wrapping made these collide, which is exactly why it had to go.
    #[tokio::test]
    async fn two_bodies_under_one_event_id_file_under_different_cids() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let event_id = [0xA9u8; 32];
        let hint = sealed_bytes(b"sealed-hint");
        let floor = floor_for(event_id, 1_715_000_000);

        let first = append_record(
            &mgr,
            &db,
            &actor,
            &sealed_bytes(b"first-body"),
            &hint,
            &floor,
        )
        .await
        .unwrap();
        let second = append_record(
            &mgr,
            &db,
            &actor,
            &sealed_bytes(b"second-body"),
            &hint,
            &floor,
        )
        .await
        .unwrap();

        assert_ne!(
            first.record_cid, second.record_cid,
            "identity follows the bytes, never the primary key"
        );
    }

    #[tokio::test]
    async fn append_then_read_round_trips_envelope_and_floor() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let event_id = [0xA1u8; 32];
        let body = sealed_bytes(b"sealed-body");
        let hint = sealed_bytes(b"sealed-hint");
        let floor = floor_for(event_id, 1_715_000_000);

        let outcome = append_record(&mgr, &db, &actor, &body, &hint, &floor)
            .await
            .unwrap();

        let (env_back, floor_back) = read_record(&mgr, &db, &actor, &outcome.record_cid)
            .await
            .unwrap()
            .expect("record present");
        let expected = CalRecordEnvelope::new(body.as_slice().to_vec(), hint.as_slice().to_vec());
        assert_eq!(env_back, expected);
        assert_eq!(floor_back, floor);
        assert_eq!(env_back.format_version, CAL_ENVELOPE_FORMAT_VERSION);
    }

    /// Records are actor-scoped. Another actor holding the same record CID must
    /// not resolve into this actor's segment (`__calendar/<actor_hex>/`). Under
    /// content-hash identity this is sharper than it was: two actors storing
    /// byte-identical events genuinely DO share a cid, so actor scoping is the
    /// only thing keeping their reads apart.
    #[tokio::test]
    async fn read_is_actor_scoped() {
        let (_dir, mgr, db) = fixture().await;
        let owner = [7u8; 32];
        let stranger = [8u8; 32];
        let event_id = [0xA2u8; 32];
        let body = sealed_bytes(b"sealed-body");
        let hint = sealed_bytes(b"sealed-hint");
        let outcome = append_record(
            &mgr,
            &db,
            &owner,
            &body,
            &hint,
            &floor_for(event_id, 1_715_000_000),
        )
        .await
        .unwrap();

        assert!(
            read_record(&mgr, &db, &owner, &outcome.record_cid)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            read_record(&mgr, &db, &stranger, &outcome.record_cid)
                .await
                .unwrap()
                .is_none(),
            "a record CID must not resolve into another actor's segment"
        );
    }

    /// The PUT handler's crash window (append durable, row INSERT not yet) heals
    /// on retry; the mirror's record_cid index is non-UNIQUE, so the
    /// skip-if-present guard is what keeps that idempotent rather than
    /// double-appending.
    #[tokio::test]
    async fn ensure_in_segment_is_idempotent() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let event_id = [0xA3u8; 32];
        let body = sealed_bytes(b"sealed-body");
        let hint = sealed_bytes(b"sealed-hint");
        let floor = floor_for(event_id, 1_715_000_000);

        let mut cid = None;
        for _ in 0..3 {
            let seen = ensure_in_segment(&mgr, &db, &actor, &body, &hint, &floor)
                .await
                .unwrap();
            assert!(
                cid.is_none_or(|prior| prior == seen),
                "every pass must return the same content-derived cid"
            );
            cid = Some(seen);
        }
        let cid = cid.expect("three passes ran");

        let conn = db.conn().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM segment_records \
                 WHERE scope_id=?1 AND kind='calendar' AND record_cid=?2",
                rusqlite::params![&actor[..], &cid.as_bytes()[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "exactly one mirror row after three ensure passes");
    }

    /// A row with no `record_cid` resolves to `None` — **fail closed**. The
    /// row carries no body of its own, so a NULL cid is the only shape of
    /// "this row's body is unreachable".
    #[tokio::test]
    async fn load_event_body_of_a_row_without_a_record_cid_is_none() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let row = row_for([0xB1u8; 32], None);
        assert_eq!(
            load_event_body(&mgr, &db, &actor, &row).await.unwrap(),
            None,
            "a NULL record_cid must resolve to no body"
        );
    }

    /// The ordinary post-cutover row: the body resolves through the stored cid.
    #[tokio::test]
    async fn load_event_body_reads_the_segment_through_the_stored_cid() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let event_id = [0xB2u8; 32];
        let body = sealed_bytes(b"segment-body");
        let hint = sealed_bytes(b"segment-hint");
        let outcome = append_record(
            &mgr,
            &db,
            &actor,
            &body,
            &hint,
            &floor_for(event_id, 1_715_000_000),
        )
        .await
        .unwrap();

        let row = row_for(event_id, Some(outcome.record_cid));
        assert_eq!(
            load_event_body(&mgr, &db, &actor, &row).await.unwrap(),
            Some(body.as_slice().to_vec())
        );
    }

    /// The row names a cid the segment does not hold: resolve to `None`, never
    /// to an empty body.
    #[tokio::test]
    async fn load_event_body_of_a_fully_absent_event_is_none() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let row = row_for([0xB4u8; 32], Some(Cid::of_dag_cbor(b"never-appended")));
        assert_eq!(
            load_event_body(&mgr, &db, &actor, &row).await.unwrap(),
            None
        );
    }

    /// The bucket keys on server-receive time, not the event's own time — a
    /// meeting scheduled years out must not create a far-future bucket.
    #[tokio::test]
    async fn bucket_keys_on_created_at_not_internal_date() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let event_id = [0xB5u8; 32];
        let mut floor = floor_for(event_id, 1_715_000_000); // 2024-05-06
        floor.internal_date = 1_900_000_000; // 2030
        let body = sealed_bytes(b"body");
        let hint = sealed_bytes(b"hint");
        let cid = append_record(&mgr, &db, &actor, &body, &hint, &floor)
            .await
            .unwrap()
            .record_cid;

        let conn = db.conn().await;
        let bucket: String = conn
            .query_row(
                "SELECT bucket FROM segment_records \
                 WHERE scope_id=?1 AND kind='calendar' AND record_cid=?2",
                rusqlite::params![&actor[..], &cid.as_bytes()[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            bucket,
            fauna_segment_store::bucket_for(1_715_000_000),
            "bucket must come from created_at (epoch seconds), not internal_date"
        );
    }

    // ---- S6.8b: orphan mirror-row reclaim -------------------------------

    /// A wall-clock "now" for the reaper tests; every seeded `created_at` is
    /// expressed relative to it so the age watermark is exercised explicitly.
    const NOW: i64 = 1_800_000_000;
    /// Comfortably past [`records_db::ORPHAN_REAP_MIN_AGE_SECS`].
    const OLD: i64 = NOW - 2 * records_db::ORPHAN_REAP_MIN_AGE_SECS;

    /// Append a content record that has **no** metadata row pointing at it —
    /// exactly what a rejected PUT leaves behind. Returns its cid; `tag` keeps
    /// distinct orphans distinct (identity is content-derived, so two orphans
    /// seeded with the same bytes would be the same record).
    async fn seed_orphan_record(
        mgr: &SegmentManager,
        db: &CacheDb,
        actor: &[u8; 32],
        tag: &[u8],
        created_at: i64,
    ) -> Cid {
        let body = sealed_bytes(tag);
        let hint = sealed_bytes(b"orphan-hint");
        append_record(
            mgr,
            db,
            actor,
            &body,
            &hint,
            &floor_for([0xAAu8; 32], created_at),
        )
        .await
        .unwrap()
        .record_cid
    }

    /// Seed a healthy event: a real `bridge_caldav_events` row **and** the live
    /// content record it points at.
    ///
    /// ⚠ The append must use the row's OWN body+hint. Identity is the hash of
    /// those bytes, so appending anything else would produce a record the row
    /// does not point at — and the "reaper spares a referenced record" test
    /// would then pass while proving nothing.
    async fn seed_live_event(
        mgr: &SegmentManager,
        db: &CacheDb,
        actor: &[u8; 32],
        cal: &[u8; 32],
        created_at: i64,
    ) -> ([u8; 32], Cid) {
        let sealed = seal(b"BEGIN:VCALENDAR...", &test_pubkey());
        // Bind the hint ONCE: `seeded_hint` is randomized, so re-deriving it
        // would seed the row under one identity and append the record under
        // another — the record would then read as an orphan and be reaped.
        let hint_bytes = seeded_hint();
        let event_id = seed_row_only(
            db,
            actor,
            cal,
            &sealed,
            &hint_bytes,
            1_714_999_900,
            created_at,
        )
        .await;

        let body = SealedRecordBytes::verify(sealed.clone()).expect("seeded body verifies");
        let hint = SealedRecordBytes::verify(hint_bytes.clone()).expect("seeded hint verifies");
        let cid = append_record(
            mgr,
            db,
            actor,
            &body,
            &hint,
            &floor_for(event_id, created_at),
        )
        .await
        .unwrap()
        .record_cid;
        assert_eq!(
            cid,
            seeded_row_cid(&sealed, &hint_bytes),
            "the seeded row must point at the record just appended"
        );
        (event_id, cid)
    }

    /// The slice's heart: an unreachable record left by a rejected PUT is
    /// tombstoned, so compaction can finally reclaim its bytes.
    #[tokio::test]
    async fn reap_tombstones_an_orphan_record() {
        let (_dir, mgr, db) = fixture().await;
        let (actor, cal) = ([0x51u8; 32], [0x52u8; 32]);
        seed_live_event(&mgr, &db, &actor, &cal, OLD).await;
        let orphan = seed_orphan_record(&mgr, &db, &actor, b"orphan-a", OLD).await;

        assert_eq!(reap_orphan_records(&db, &actor, NOW).await.unwrap(), 1);
        assert!(
            !super::super::is_live(&db, &actor, KIND, &orphan).await,
            "orphan must tombstone"
        );
    }

    /// Invariant 3, inverted: a tombstoned record is eligible for physical
    /// reclaim, so tombstoning one a live row still points at is
    /// user-irrecoverable loss. Mutating the reaper to skip its `live_cids`
    /// membership check fails exactly here.
    #[tokio::test]
    async fn reap_never_tombstones_a_record_a_live_row_points_at() {
        let (_dir, mgr, db) = fixture().await;
        let (actor, cal) = ([0x51u8; 32], [0x52u8; 32]);
        let (_event_id, live) = seed_live_event(&mgr, &db, &actor, &cal, OLD).await;
        seed_orphan_record(&mgr, &db, &actor, b"orphan-a", OLD).await;

        let reaped = reap_orphan_records(&db, &actor, NOW).await.unwrap();
        assert!(
            super::super::is_live(&db, &actor, KIND, &live).await,
            "a record its row still points at must survive the reaper — tombstoning it \
             makes the body eligible for physical reclaim (user-irrecoverable loss)"
        );
        assert_eq!(reaped, 1, "only the orphan is reclaimable");
    }

    /// The age watermark. A PUT commits its content record before its metadata
    /// row, so a record seconds old is indistinguishable from an orphan — and
    /// reaping it there makes the DAO's empty-body guard refuse the write.
    #[tokio::test]
    async fn reap_spares_a_young_orphan() {
        let (_dir, mgr, db) = fixture().await;
        let (actor, cal) = ([0x51u8; 32], [0x52u8; 32]);
        seed_live_event(&mgr, &db, &actor, &cal, OLD).await;
        let in_flight = seed_orphan_record(&mgr, &db, &actor, b"in-flight", NOW - 60).await;

        assert_eq!(reap_orphan_records(&db, &actor, NOW).await.unwrap(), 0);
        assert!(
            super::super::is_live(&db, &actor, KIND, &in_flight).await,
            "a record younger than the watermark may be an in-flight PUT"
        );
    }

    /// Zero metadata rows + live mirror rows is a nest whose rows have not been
    /// rebuilt yet (pure-backup destination, or a restore caught between its
    /// mirror rebuild and its row rebuild) — not a pile of orphans. Reaping
    /// there would tombstone the actor's entire corpus.
    #[tokio::test]
    async fn reap_fails_closed_when_the_actor_has_no_metadata_rows() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [0x51u8; 32];
        let a = seed_orphan_record(&mgr, &db, &actor, b"orphan-a", OLD).await;
        let b = seed_orphan_record(&mgr, &db, &actor, b"orphan-b", OLD).await;

        assert_eq!(reap_orphan_records(&db, &actor, NOW).await.unwrap(), 0);
        assert!(
            super::super::is_live(&db, &actor, KIND, &a).await
                && super::super::is_live(&db, &actor, KIND, &b).await
        );
    }

    /// The reaper is actor-scoped: another actor's records are never candidates,
    /// and never satisfy the fail-closed live-set check either.
    #[tokio::test]
    async fn reap_is_actor_scoped() {
        let (_dir, mgr, db) = fixture().await;
        let (actor, cal) = ([0x51u8; 32], [0x52u8; 32]);
        let other = [0x77u8; 32];
        seed_live_event(&mgr, &db, &actor, &cal, OLD).await;
        seed_orphan_record(&mgr, &db, &actor, b"orphan-a", OLD).await;
        let other_orphan = seed_orphan_record(&mgr, &db, &other, b"other-orphan", OLD).await;

        assert_eq!(reap_orphan_records(&db, &actor, NOW).await.unwrap(), 1);
        assert!(
            super::super::is_live(&db, &other, KIND, &other_orphan).await,
            "another actor's records are out of scope"
        );
    }
}
