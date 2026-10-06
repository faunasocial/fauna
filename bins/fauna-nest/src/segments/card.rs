//! Card-kind nest-side segment coordination (S6.5).
//!
//! Variant-for-variant twin of [`super::cal`] for CardDAV vCard bodies —
//! `bridge_carddav_cards` is a deliberate structural mirror of
//! `bridge_caldav_events` (`carddav-server.md` § Storage model), so the two DR
//! postures must not diverge (priority #4). Read [`super::cal`]'s module doc for
//! the design rationale; everything there applies with
//! `calendar_id → addressbook_id` and `event_id → card_id`.
//!
//! One asymmetry: cards are sealed at ingest in **both** storage modes and have
//! never rested raw, so — unlike calendar — there is no S4 seal back-fill arm
//! for this kind and none should be added (`bridge_carddav.rs:15-18`).

use anyhow::{Context, Result};
use fauna_cbor::Cid;
use fauna_contacts::segments::{CardFloorMetadata, CardRecordEnvelope};
use fauna_mls::wrapped_blob::SealedRecordBytes;
use fauna_segment_store::{CompactionPlan, SegmentManager};

use super::mail::BucketCompactionOutcome;
use super::records_db::{self, NewPointReadSegmentRecord};

use crate::db::CacheDb;
use crate::db::bridge_carddav::CardRow;

pub const KIND: &str = "card";

/// Result of [`append_record`].
#[derive(Debug, Clone, Copy)]
pub struct CardAppendOutcome {
    /// The record's filing CID — `Cid::of_dag_cbor(<encoded envelope bytes>)`,
    /// derived at the append (never caller-supplied) and stored on the
    /// `bridge_carddav_cards` row.
    pub record_cid: Cid,
    /// Segment id the record landed in (live tail of the manifest).
    pub seg_id: u32,
    /// `Some(closed_seg_id)` if this append rotated a previously-open segment
    /// closed; `None` otherwise.
    pub finalized: Option<u32>,
}

/// Append one card record to its owner's `__card` segment store, then mirror it
/// into `segment_records`.
///
/// The body and index hint arrive as [`SealedRecordBytes`] — the S6.12
/// structural seal gate; see [`super::cal::append_record`] for the rationale.
///
/// The bucket keys on `floor.created_at` — server receive time, epoch
/// **seconds** (mail's `received_at` is milliseconds; do not copy its `/1000`).
pub async fn append_record(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    body: &SealedRecordBytes,
    hint: &SealedRecordBytes,
    floor: &CardFloorMetadata,
) -> Result<CardAppendOutcome> {
    let envelope = CardRecordEnvelope::new(body.as_slice().to_vec(), hint.as_slice().to_vec());
    let (cid, env_bytes) = envelope
        .encode_record()
        .map_err(|e| anyhow::anyhow!("serialize card envelope: {e}"))?;
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
    floor: &CardFloorMetadata,
) -> Result<CardAppendOutcome> {
    let floor_bytes = floor
        .encode()
        .map_err(|e| anyhow::anyhow!("serialize card floor: {e}"))?;
    let bucket = fauna_segment_store::bucket_for(floor.created_at);

    let outcome = mgr
        .append_record_with_bucket(actor, cid, env_bytes, &floor_bytes, &bucket)
        .await
        .map_err(|e| anyhow::anyhow!("append_record_with_bucket (card): {e}"))?;

    cache_db
        .segment_records_insert_card(actor, outcome.segment_id, &cid, &bucket, floor.created_at)
        .await
        .context("mirror card segment record")?;

    Ok(CardAppendOutcome {
        record_cid: cid,
        seg_id: outcome.segment_id,
        finalized: outcome.finalized,
    })
}

/// Append one **recovered** card record — twin of
/// [`super::cal::append_recovered_record`]; body:
/// [`super::append_recovered_point_read_record`].
pub(crate) async fn append_recovered_record<T>(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    envelope_bytes: &[u8],
    floor: &CardFloorMetadata,
    place: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
) -> Result<Option<(u32, T)>> {
    let floor_bytes = floor
        .encode()
        .map_err(|e| anyhow::anyhow!("serialize card floor: {e}"))?;
    super::append_recovered_point_read_record(
        mgr,
        cache_db,
        actor,
        KIND,
        envelope_bytes,
        &floor_bytes,
        floor.created_at,
        records_db::insert_card,
        place,
    )
    .await
}

/// Idempotently ensure the card is present in `actor`'s `__card` segment
/// store. Returns the record's content-hash CID either way — the caller stores
/// it on the `bridge_carddav_cards` row. The skip-if-present guard is a
/// byte-replay dedup, sound for the same reason as the calendar twin's (the
/// PUT path files the client's already-sealed bytes verbatim, so a retry
/// re-derives the identical CID).
pub async fn ensure_in_segment(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    body: &SealedRecordBytes,
    hint: &SealedRecordBytes,
    floor: &CardFloorMetadata,
) -> Result<Cid> {
    let envelope = CardRecordEnvelope::new(body.as_slice().to_vec(), hint.as_slice().to_vec());
    let (cid, env_bytes) = envelope
        .encode_record()
        .map_err(|e| anyhow::anyhow!("serialize card envelope: {e}"))?;
    if cache_db
        .segment_records_lookup_record(actor, KIND, &cid)
        .await?
        .is_none()
    {
        append_encoded(mgr, cache_db, actor, cid, &env_bytes, floor).await?;
    }
    Ok(cid)
}

/// Tombstone `actor`'s orphaned `__card` content records — the structural twin
/// of [`super::cal::reap_orphan_records`]; see it for the safety argument, which
/// applies verbatim (the live/kind pairing, one critical section, an age
/// watermark, and a fail-closed empty-live-set guard). Deletes of a whole
/// addressbook already cascade (S6.8a), so what lands here is the
/// rejected-PUT / crash residue.
pub async fn reap_orphan_records(cache_db: &CacheDb, actor: &[u8; 32], now: i64) -> Result<u32> {
    super::reap_orphan_records(cache_db, actor, now, super::SegmentKind::Card).await
}

/// The table whose live rows hold card's reachable record CIDs — across all of
/// the actor's addressbooks. Read through [`super::live_record_cids`], the same
/// fail-closed live-set read the calendar twin uses. `pub(super)` only:
/// reachable exclusively through [`super::SegmentKind::Card`], never as a
/// standalone string a caller could pair with the wrong `kind`.
pub(super) const LIVE_ROWS_TABLE: &str = "bridge_carddav_cards";

/// Read one card's envelope + floor from `actor`'s segment store. `None` when
/// the mirror has no live row, or points at a record the segment file does not
/// hold (divergence — warned and treated as absent, so one bad record never
/// fails a whole addressbook-multiget).
pub async fn read_record(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    record_cid: &Cid,
) -> Result<Option<(CardRecordEnvelope, CardFloorMetadata)>> {
    let Some((env_bytes, floor_bytes)) =
        super::read_point_read_record(mgr, cache_db, actor, KIND, record_cid).await?
    else {
        return Ok(None);
    };
    let envelope = CardRecordEnvelope::decode(&env_bytes)
        .map_err(|e| anyhow::anyhow!("parse card envelope: {e}"))?;
    let floor = CardFloorMetadata::decode(&floor_bytes)
        .map_err(|e| anyhow::anyhow!("parse card floor: {e}"))?;
    Ok(Some((envelope, floor)))
}

/// Resolve one card's sealed body from the segment store, by the row's stored
/// `record_cid` — the row carries no body of its own (see
/// [`super::cal::load_event_body`]).
pub async fn load_card_body(
    mgr: &SegmentManager,
    cache_db: &CacheDb,
    actor: &[u8; 32],
    row: &CardRow,
) -> Result<Option<Vec<u8>>> {
    super::load_body_via_record_cid(
        row.record_cid()?,
        || {
            tracing::warn!(
                actor = %hex::encode(actor),
                card_id = %hex::encode(row.card_id),
                "bridge_carddav_cards row has no record_cid — cannot resolve its body"
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

/// Rewrite one bucket of `actor`'s `__card` segments, dropping every record the
/// mirror marks tombstoned (S6.8c). Structural twin of
/// [`super::cal::compact_bucket`]; `created_at` is epoch **seconds** there too.
/// Both delegate the kind-agnostic rewrite skeleton to
/// [`super::compact_bucket_with`], keeping only the floor decode and the named
/// `apply_card_compaction_tx` wrapper here.
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
            let floor = CardFloorMetadata::decode(floor_bytes)
                .map_err(|e| anyhow::anyhow!("parse card floor during compaction: {e}"))?;
            // `created_at` is epoch **seconds** — do NOT copy post's `/ 1000`.
            Ok(NewPointReadSegmentRecord {
                record_cid: rid,
                bucket: fauna_segment_store::bucket_for(floor.created_at),
                received_at: floor.created_at,
            })
        },
        |tx, new_segment, new_records| {
            records_db::apply_card_compaction_tx(tx, actor, &plan.inputs, new_segment, new_records)
                .context("apply_card_compaction_tx")
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_mls::wrapped_blob::derive_recipient_hpke_keypair;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn floor_for(card_id: [u8; 32], created_at: i64) -> CardFloorMetadata {
        CardFloorMetadata {
            addressbook_id: [0x11u8; 32],
            card_id,
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
    fn row_for(card_id: [u8; 32], record_cid: Option<Cid>) -> CardRow {
        CardRow {
            card_id,
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
    /// bytes it was seeded with — the same mint `place_carddav_card` performs.
    fn seeded_row_cid(body: &[u8], hint: &[u8]) -> Cid {
        crate::db::bridge_carddav::carddav_record_cid(body, hint).expect("derive row cid")
    }

    /// Seed a `bridge_carddav_cards` row whose content record was never
    /// appended: the row carries the cid its `body + hint` hash to, but
    /// nothing is in the segment. The hint is explicit because `seeded_hint`
    /// is randomized — a caller linking the row to a real record must pass the
    /// same bytes it appends.
    async fn seed_row_only(
        db: &CacheDb,
        actor: &[u8; 32],
        book: &[u8; 32],
        body: &[u8],
        hint: &[u8],
        internal_date: i64,
        created_at: i64,
    ) -> [u8; 32] {
        db.insert_bridge_carddav_addressbook(actor, book, b"meta", created_at)
            .await
            .unwrap();
        match db
            .place_carddav_card(
                actor,
                book,
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
            crate::db::bridge_carddav::PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("expected Created, got {other:?}"),
        }
    }

    /// The cutover's central rule for cards: identity IS the hash of the
    /// envelope bytes (`message-segment-store.md` § Record identity per kind) —
    /// what makes the kind adoptable, since `segments::admit` re-hashes every
    /// block against the cid it is filed under.
    #[tokio::test]
    async fn record_identity_is_the_hash_of_the_envelope_bytes() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let body = sealed_bytes(b"sealed-vcard");
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
            CardRecordEnvelope::new(body.as_slice().to_vec(), hint.as_slice().to_vec())
                .encode_record()
                .unwrap();
        assert_eq!(
            outcome.record_cid, expected,
            "the filing cid must be the content hash of the envelope"
        );
        assert_eq!(outcome.record_cid, Cid::of_dag_cbor(&env_bytes));
    }

    /// Identity cannot be a function of the PK: two records under the same
    /// `card_id` with different bodies must file under different cids. The
    /// pre-cutover `Cid::from_digest_dag_cbor(card_id)` wrapping collided them.
    #[tokio::test]
    async fn two_bodies_under_one_card_id_file_under_different_cids() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let card_id = [0xA9u8; 32];
        let hint = sealed_bytes(b"sealed-hint");
        let floor = floor_for(card_id, 1_715_000_000);

        let first = append_record(&mgr, &db, &actor, &sealed_bytes(b"first"), &hint, &floor)
            .await
            .unwrap();
        let second = append_record(&mgr, &db, &actor, &sealed_bytes(b"second"), &hint, &floor)
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
        let card_id = [0xA1u8; 32];
        let body = sealed_bytes(b"sealed-vcard");
        let hint = sealed_bytes(b"sealed-hint");
        let floor = floor_for(card_id, 1_715_000_000);

        let outcome = append_record(&mgr, &db, &actor, &body, &hint, &floor)
            .await
            .unwrap();

        let (env_back, floor_back) = read_record(&mgr, &db, &actor, &outcome.record_cid)
            .await
            .unwrap()
            .expect("record present");
        let expected = CardRecordEnvelope::new(body.as_slice().to_vec(), hint.as_slice().to_vec());
        assert_eq!(env_back, expected);
        assert_eq!(floor_back, floor);
    }

    /// Cards are actor-scoped: a card CID must never resolve into another
    /// actor's address book.
    #[tokio::test]
    async fn read_is_actor_scoped() {
        let (_dir, mgr, db) = fixture().await;
        let owner = [7u8; 32];
        let stranger = [8u8; 32];
        let card_id = [0xA2u8; 32];
        let body = sealed_bytes(b"sealed-vcard");
        let hint = sealed_bytes(b"sealed-hint");
        let outcome = append_record(
            &mgr,
            &db,
            &owner,
            &body,
            &hint,
            &floor_for(card_id, 1_715_000_000),
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
            "a card CID must not resolve into another actor's segment"
        );
    }

    #[tokio::test]
    async fn ensure_in_segment_is_idempotent() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let card_id = [0xA3u8; 32];
        let body = sealed_bytes(b"sealed-vcard");
        let hint = sealed_bytes(b"sealed-hint");
        let floor = floor_for(card_id, 1_715_000_000);

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
                 WHERE scope_id=?1 AND kind='card' AND record_cid=?2",
                rusqlite::params![&actor[..], &cid.as_bytes()[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "exactly one mirror row after three ensure passes");
    }

    /// A row with no `record_cid` resolves to `None` — **fail closed**: the
    /// row carries no body of its own.
    #[tokio::test]
    async fn load_card_body_of_a_row_without_a_record_cid_is_none() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let row = row_for([0xB1u8; 32], None);
        assert_eq!(
            load_card_body(&mgr, &db, &actor, &row).await.unwrap(),
            None,
            "a NULL record_cid must resolve to no body"
        );
    }

    #[tokio::test]
    async fn load_card_body_reads_the_segment_through_the_stored_cid() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let card_id = [0xB2u8; 32];
        let body = sealed_bytes(b"segment-body");
        let hint = sealed_bytes(b"segment-hint");
        let outcome = append_record(
            &mgr,
            &db,
            &actor,
            &body,
            &hint,
            &floor_for(card_id, 1_715_000_000),
        )
        .await
        .unwrap();

        let row = row_for(card_id, Some(outcome.record_cid));
        assert_eq!(
            load_card_body(&mgr, &db, &actor, &row).await.unwrap(),
            Some(body.as_slice().to_vec())
        );
    }

    #[tokio::test]
    async fn load_card_body_of_a_fully_absent_card_is_none() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let row = row_for([0xB4u8; 32], Some(Cid::of_dag_cbor(b"never-appended")));
        assert_eq!(load_card_body(&mgr, &db, &actor, &row).await.unwrap(), None);
    }

    /// Bucket keys on server-receive time, not the card's own timestamp.
    #[tokio::test]
    async fn bucket_keys_on_created_at_not_internal_date() {
        let (_dir, mgr, db) = fixture().await;
        let actor = [7u8; 32];
        let card_id = [0xB5u8; 32];
        let mut floor = floor_for(card_id, 1_715_000_000);
        floor.internal_date = 1_900_000_000;
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
                 WHERE scope_id=?1 AND kind='card' AND record_cid=?2",
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

    /// Calendar and card records that share a CID must not collide: the mirror
    /// is keyed on `(scope_id, kind, record_cid)`, so one CID lives
    /// independently under kind='calendar' and kind='card'.
    ///
    /// ⚠ Under content-hash identity the collision must be MANUFACTURED from
    /// identical bytes, not from a shared PK. `Cal`/`CardRecordEnvelope` are
    /// shape-identical (same fields, same `format_version`), so one `(body,
    /// hint)` pair encodes to the same bytes and therefore the same cid in both
    /// kinds — a genuine collision, where the pre-cutover version of this test
    /// merely shared an `event_id`/`card_id` and would now file under two
    /// DIFFERENT cids, testing nothing.
    #[tokio::test]
    async fn card_and_calendar_records_do_not_collide_on_kind() {
        let (_dir, card_mgr, db) = fixture().await;
        let cal_dir = TempDir::new().unwrap();
        let cal_mgr = SegmentManager::new(cal_dir.path().to_path_buf(), super::super::cal::KIND);
        let actor = [7u8; 32];
        let shared_id = [0xC1u8; 32];

        let body = sealed_bytes(b"identical-bytes-in-both-kinds");
        let hint = sealed_bytes(b"identical-hint");

        let card_cid = append_record(
            &card_mgr,
            &db,
            &actor,
            &body,
            &hint,
            &floor_for(shared_id, 1_715_000_000),
        )
        .await
        .unwrap()
        .record_cid;
        let cal_cid = super::super::cal::append_record(
            &cal_mgr,
            &db,
            &actor,
            &body,
            &hint,
            &fauna_calendar::segments::CalFloorMetadata {
                event_id: shared_id,
                created_at: 1_715_000_000,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .record_cid;
        assert_eq!(
            card_cid, cal_cid,
            "fixture precondition: identical bytes must collide on cid, else \
             this test proves nothing about kind scoping"
        );

        // The mirror keeps them apart on `kind` alone.
        let rows: i64 = {
            let conn = db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM segment_records \
                 WHERE scope_id = ?1 AND record_cid = ?2",
                rusqlite::params![&actor[..], &card_cid.as_bytes()[..]],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(rows, 2, "one mirror row per kind for the shared cid");

        // And each kind resolves out of its OWN segment store.
        let (card_env, _) = read_record(&card_mgr, &db, &actor, &card_cid)
            .await
            .unwrap()
            .expect("card present");
        assert_eq!(card_env.encrypted_body, body.as_slice().to_vec());

        let (cal_env, _) = super::super::cal::read_record(&cal_mgr, &db, &actor, &cal_cid)
            .await
            .unwrap()
            .expect("event present");
        assert_eq!(cal_env.encrypted_body, body.as_slice().to_vec());
    }

    // ---- S6.8b: orphan mirror-row reclaim (twin of `cal::tests`) ---------

    const NOW: i64 = 1_800_000_000;
    const OLD: i64 = NOW - 2 * records_db::ORPHAN_REAP_MIN_AGE_SECS;

    /// Append a content record with **no** metadata row pointing at it.
    /// Returns its cid; `tag` keeps distinct orphans distinct (identity is
    /// content-derived, so equal bytes would be the same record).
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

    /// Seed a healthy card: a real `bridge_carddav_cards` row **and** the live
    /// content record it points at.
    ///
    /// ⚠ The append must use the row's OWN body+hint — identity is the hash of
    /// those bytes, so appending anything else yields a record the row does not
    /// point at, and the "reaper spares a referenced record" test would pass
    /// while proving nothing.
    async fn seed_live_card(
        mgr: &SegmentManager,
        db: &CacheDb,
        actor: &[u8; 32],
        book: &[u8; 32],
        created_at: i64,
    ) -> ([u8; 32], Cid) {
        let sealed = seal(b"BEGIN:VCARD...", &test_pubkey());
        // Bind the hint ONCE — see the calendar twin: `seeded_hint` is
        // randomized, and a row seeded under one identity with the record
        // appended under another would read as an orphan and be reaped.
        let hint_bytes = seeded_hint();
        let card_id = seed_row_only(
            db,
            actor,
            book,
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
            &floor_for(card_id, created_at),
        )
        .await
        .unwrap()
        .record_cid;
        assert_eq!(
            cid,
            seeded_row_cid(&sealed, &hint_bytes),
            "the seeded row must point at the record just appended"
        );
        (card_id, cid)
    }

    #[tokio::test]
    async fn reap_tombstones_an_orphan_record() {
        let (_dir, mgr, db) = fixture().await;
        let (actor, book) = ([0x51u8; 32], [0x52u8; 32]);
        seed_live_card(&mgr, &db, &actor, &book, OLD).await;
        let orphan = seed_orphan_record(&mgr, &db, &actor, b"orphan-a", OLD).await;

        assert_eq!(reap_orphan_records(&db, &actor, NOW).await.unwrap(), 1);
        assert!(
            !super::super::is_live(&db, &actor, KIND, &orphan).await,
            "orphan must tombstone"
        );
    }

    /// Invariant 3, inverted — the card half. A tombstoned record is eligible
    /// for physical reclaim, so tombstoning one a live row points at is
    /// user-irrecoverable loss.
    #[tokio::test]
    async fn reap_never_tombstones_a_record_a_live_row_points_at() {
        let (_dir, mgr, db) = fixture().await;
        let (actor, book) = ([0x51u8; 32], [0x52u8; 32]);
        let (_card_id, live) = seed_live_card(&mgr, &db, &actor, &book, OLD).await;
        seed_orphan_record(&mgr, &db, &actor, b"orphan-a", OLD).await;

        let reaped = reap_orphan_records(&db, &actor, NOW).await.unwrap();
        assert!(
            super::super::is_live(&db, &actor, KIND, &live).await,
            "a record its row still points at must survive the reaper"
        );
        assert_eq!(reaped, 1, "only the orphan is reclaimable");
    }

    #[tokio::test]
    async fn reap_spares_a_young_orphan() {
        let (_dir, mgr, db) = fixture().await;
        let (actor, book) = ([0x51u8; 32], [0x52u8; 32]);
        seed_live_card(&mgr, &db, &actor, &book, OLD).await;
        let in_flight = seed_orphan_record(&mgr, &db, &actor, b"in-flight", NOW - 60).await;

        assert_eq!(reap_orphan_records(&db, &actor, NOW).await.unwrap(), 0);
        assert!(super::super::is_live(&db, &actor, KIND, &in_flight).await);
    }

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

    /// The calendar twin has pinned this since the reaper landed; card took the
    /// production code and left the test behind. Actor scoping is the one
    /// reaper property whose failure is user-irrecoverable: tombstoning is
    /// eligibility for physical reclaim, so a reap that reached across actors
    /// would delete a stranger's address book — and every other reap test here
    /// seeds exactly one actor, so none of them can see it.
    #[tokio::test]
    async fn reap_is_actor_scoped() {
        let (_dir, mgr, db) = fixture().await;
        let (actor, book) = ([0x51u8; 32], [0x52u8; 32]);
        let other = [0x77u8; 32];
        seed_live_card(&mgr, &db, &actor, &book, OLD).await;
        seed_orphan_record(&mgr, &db, &actor, b"orphan-a", OLD).await;
        let other_orphan = seed_orphan_record(&mgr, &db, &other, b"other-orphan", OLD).await;

        assert_eq!(reap_orphan_records(&db, &actor, NOW).await.unwrap(), 1);
        assert!(
            super::super::is_live(&db, &other, KIND, &other_orphan).await,
            "another actor's records are out of scope"
        );
    }
}
