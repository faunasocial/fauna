//! CalDAV placement layer for the I2b mail-bridge absorption (Phase D).
//!
//! Three tables (defined in `db::migrations::MIGRATIONS_BRIDGE_ABSORPTION`):
//!   * `bridge_caldav_calendars` — per-(actor, calendar_id) collection state:
//!     sealed metadata, ctag, highestmodseq.
//!   * `bridge_caldav_events`    — per-(actor, calendar_id, event_id) event
//!     placement: ciphertext body + sealed index hint + etag/modseq + UID hash.
//!   * `bridge_caldav_expunged`  — deletion tombstones for RFC 6578
//!     `REPORT sync-collection` VANISHED-equivalent responses.
//!
//! Every public method takes a `&self` receiver and operates under a single
//! `self.conn.lock().await` scope. All `modseq` bumps allocate **one** new
//! modseq per operation (CONDSTORE semantics, mirrored from Phase C IMAP).
//!
//! These tables are the nest's only CalDAV store: every event rests sealed.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::CacheDb;
use super::bridge_dav_common::format_etag;
use super::{blob_col_to_array, blob_to_array};

const PUT_EVENT_DST: &[u8] = b"fauna.bridges.put_event_ciphertext.v1";

// ── Helpers ──────────────────────────────────────────────────────────────────

/// `event_id = blake3(domain_tag || actor || timestamp_le_i64 || encrypted_body)`.
/// Domain-tagged so distinct call paths can never share an event_id, even on
/// byte-identical bodies. Length-prefix the domain tag so two tags of
/// different lengths but a coincidentally-shared concatenation can't collide.
pub(crate) fn derive_caldav_event_id(
    actor: &[u8; 32],
    timestamp: i64,
    encrypted_body: &[u8],
) -> [u8; 32] {
    super::bridge_dav_common::derive_dav_content_id(PUT_EVENT_DST, actor, timestamp, encrypted_body)
}

/// The record identity a `(body, hint)` pair is filed under —
/// `Cid::of_dag_cbor(<encoded CalRecordEnvelope>)`, the mint
/// `segments::cal::append_record` performs (`message-segment-store.md`
/// § Record identity per kind).
///
/// Exists so the test seeder [`CacheDb::place_caldav_event`] and the tests that
/// then look the record up derive the identity from ONE place; production never
/// calls it (the real path takes the cid the append returns, which is the only
/// way to be sure the bytes are actually durable).
pub(crate) fn caldav_record_cid(
    encrypted_body: &[u8],
    encrypted_index_hint: &[u8],
) -> Result<fauna_cbor::Cid> {
    let (cid, _) = fauna_calendar::segments::envelope::CalRecordEnvelope::new(
        encrypted_body.to_vec(),
        encrypted_index_hint.to_vec(),
    )
    .encode_record()
    .map_err(|e| anyhow::anyhow!("encode CalRecordEnvelope: {e}"))?;
    Ok(cid)
}

/// Tombstone `actor`'s `__calendar` content record `cid` — unless an event
/// row, in ANY of the actor's calendars, still references it. Returns whether
/// it tombstoned.
///
/// The record CID hashes only the sealed (body, hint) envelope, while
/// `event_id` also mixes in the timestamp, so row identity says nothing about
/// record identity: a re-PUT of byte-identical sealed bytes under a new
/// timestamp lands a new row on the SAME record, and one actor's calendars
/// holding identical bytes share one record. A tombstoned record is
/// reclaimable, so tombstoning one a live row points at is user-irrecoverable
/// loss. Call it on the write transaction AFTER the row DELETE and
/// after any replacement row's INSERT, so the count sees the final row set.
fn tombstone_cal_record_if_unreferenced(
    conn: &rusqlite::Connection,
    actor: &[u8; 32],
    cid: &fauna_cbor::Cid,
) -> Result<bool> {
    let live_refs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bridge_caldav_events \
             WHERE actor_id = ?1 AND record_cid = ?2",
            rusqlite::params![&actor[..], &cid.as_bytes()[..]],
            |row| row.get(0),
        )
        .context("count live event rows referencing the record")?;
    if live_refs > 0 {
        return Ok(false);
    }
    crate::segments::records_db::tombstone_by_cid(conn, actor, crate::segments::cal::KIND, cid)?;
    Ok(true)
}

// ── DB-side enums ────────────────────────────────────────────────────────────

/// Outcome of `insert_bridge_caldav_calendar` (MKCOL path) and
/// `update_bridge_caldav_calendar_metadata` (PROPPATCH path). The two helpers
/// have disjoint result spaces: `insert_*` returns `Created | AlreadyExists |
/// Conflict`; `update_*` returns `Updated | NotFound`. One unified enum keeps
/// the wire-side `ProvisionCalendarReply` mapping in the handler tight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// A new calendar row was inserted (MKCOL path).
    Created,
    /// A row with the same calendar_id already exists and its metadata bytes
    /// are byte-identical to the supplied bytes (idempotent MKCOL retry).
    AlreadyExists,
    /// A row with the same calendar_id exists but its metadata bytes differ
    /// (MKCOL path; row unchanged).
    Conflict,
    /// The existing row's metadata was overwritten (PROPPATCH path).
    /// `highestmodseq` + `ctag` bumped unless the new bytes are byte-identical
    /// to the stored bytes (idempotent retry — no bump).
    Updated,
    /// No row exists for `(actor, calendar_id)` (PROPPATCH path only — MKCOL
    /// never returns this; it inserts a fresh row instead).
    NotFound,
}

/// Outcome of `place_caldav_event`.
///
/// `place_caldav_event` never updates an existing row — it either creates a
/// fresh event_id row, is collapsed by an identical-bytes retry, or refuses
/// because the calendar doesn't exist. Use `replace_caldav_event_by_uid` for
/// the full create-or-update + if_match path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceCaldavEventOutcome {
    Created {
        event_id: [u8; 32],
        etag: String,
        modseq: i64,
    },
    /// Transport-retry: a row with the deterministic event_id already exists.
    /// modseq + etag are the existing row's values; no bump.
    Idempotent {
        event_id: [u8; 32],
        etag: String,
        modseq: i64,
    },
    /// The (actor, calendar_id) calendar isn't provisioned. No insert.
    CalendarMissing,
}

/// Outcome of `replace_caldav_event_by_uid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceCaldavEventOutcome {
    /// No prior row for this uid_hash; inserted a fresh row.
    ///
    /// `encrypted_fauna_ext` on the write outcomes is the row's **effective**
    /// sidecar after the write — only this method knows it (a MUA write
    /// carries `None` on the wire but *preserves* the prior row's sidecar).
    /// The handler journals it into the v2 `PutEvent` placement record, so
    /// snapshot restore can rebuild the sidecar column
    /// (`message-segment-store.md` § Invariants, rule 2).
    Created {
        event_id: [u8; 32],
        etag: String,
        modseq: i64,
        encrypted_fauna_ext: Option<Vec<u8>>,
    },
    /// A prior row existed for this uid_hash; replaced it (delete-old-and-
    /// insert-new) and wrote a tombstone for the old event_id.
    Updated {
        event_id: [u8; 32],
        etag: String,
        modseq: i64,
        encrypted_fauna_ext: Option<Vec<u8>>,
    },
    /// The new body bytes hash to the same event_id as the prior row for
    /// this uid_hash — transport retry, no bump.
    Idempotent {
        event_id: [u8; 32],
        etag: String,
        modseq: i64,
        encrypted_fauna_ext: Option<Vec<u8>>,
    },
    /// `if_match` was supplied and didn't match the prior row's etag.
    PreconditionFailed { current_etag: String },
    /// The (actor, calendar_id) calendar isn't provisioned.
    CalendarMissing,
}

/// Outcome of `delete_caldav_event_by_uid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteCaldavEventOutcome {
    Deleted {
        event_id: [u8; 32],
        modseq: i64,
    },
    /// Calendar absent OR event absent (don't leak which — handler returns
    /// a single `NotFound` reply variant).
    NotFound,
    PreconditionFailed {
        current_etag: String,
    },
}

// ── Data types ───────────────────────────────────────────────────────────────

/// A single calendar row, returned by `list_bridge_caldav_calendars`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarRow {
    pub calendar_id: [u8; 32],
    pub encrypted_metadata: Vec<u8>,
    pub ctag: i64,
    pub highestmodseq: i64,
    pub created_at: i64,
}

/// A single event row, returned by `query_caldav_events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub event_id: [u8; 32],
    pub uid_hash: Vec<u8>,
    pub encrypted_index_hint: Vec<u8>,
    pub etag: String,
    pub modseq: i64,
    pub ciphertext_size: u32,
    pub internal_date: i64,
    /// Sealed Fauna-extension sidecar; `None` for MUA-written rows
    /// (caldav-server.md § Event resources). Never served to
    /// a MUA — surfaced only to Fauna apps via `EventEntry`.
    pub encrypted_fauna_ext: Option<Vec<u8>>,
    /// The record's stored content-hash filing CID (36-byte blob;
    /// `message-segment-store.md` § Record identity per kind). `None` cannot
    /// occur on a live row post-cutover; readers treat it as record-absent.
    pub record_cid: Option<Vec<u8>>,
}

impl EventRow {
    /// Parse the stored `record_cid` blob. `Ok(None)` = column NULL (treated
    /// as record-absent by readers); `Err` = a blob that is not a valid Cid
    /// (corruption).
    pub fn record_cid(&self) -> Result<Option<fauna_cbor::Cid>> {
        let Some(blob) = &self.record_cid else {
            return Ok(None);
        };
        let arr: [u8; 36] = blob_to_array(blob.as_slice(), "record_cid")?;
        Ok(Some(fauna_cbor::Cid::from_bytes(arr).map_err(|e| {
            anyhow::anyhow!("record_cid not a Cid: {e}")
        })?))
    }
}

/// Pagination wrapper around event rows.
#[derive(Debug, Clone)]
pub struct EventPage {
    pub events: Vec<EventRow>,
    /// `true` iff there were strictly more rows than `limit`; the handler
    /// trims the extra and reports `more: true` upstream.
    pub more: bool,
}

/// A single tombstone row, returned by `query_caldav_expunged_since`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpungedRow {
    pub event_id: [u8; 32],
    pub uid_hash: Vec<u8>,
    pub modseq: i64,
}

// ── Row mappers (private) ────────────────────────────────────────────────────

fn map_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    let event_id: [u8; 32] = blob_col_to_array(row.get(0)?, 0, "event_id")?;
    Ok(EventRow {
        event_id,
        uid_hash: row.get(1)?,
        encrypted_index_hint: row.get(2)?,
        etag: row.get(3)?,
        modseq: row.get(4)?,
        ciphertext_size: row.get::<_, i64>(5)? as u32,
        internal_date: row.get(6)?,
        encrypted_fauna_ext: row.get(7)?,
        record_cid: row.get(8)?,
    })
}

fn map_expunged_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExpungedRow> {
    let event_id: [u8; 32] = blob_col_to_array(row.get(0)?, 0, "event_id")?;
    Ok(ExpungedRow {
        event_id,
        uid_hash: row.get(1)?,
        modseq: row.get(2)?,
    })
}

// ── CacheDb methods ──────────────────────────────────────────────────────────

impl CacheDb {
    /// Return `true` iff `bridge_caldav_calendars` has a row for
    /// `(actor, calendar_id)`. Used by handler "not found" paths.
    pub async fn ensure_bridge_caldav_calendar_exists(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
    ) -> Result<bool> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let conn = self.conn.lock().await;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &calendar_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("ensure_bridge_caldav_calendar_exists")?;
        Ok(found.is_some())
    }

    /// Whether an event with `uid_hash` rests on ANY of `actor`'s calendars —
    /// the create-only test for placing an emailed invitation, which carries no
    /// collection hint (`caldav-server.md` § Server-side auto-schedule).
    pub async fn caldav_uid_on_any_calendar(
        &self,
        actor: &[u8; 32],
        uid_hash: &[u8; 32],
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND uid_hash = ?2 LIMIT 1",
                rusqlite::params![&actor[..], &uid_hash[..]],
                |row| row.get(0),
            )
            .optional()
            .context("caldav_uid_on_any_calendar")?;
        Ok(found.is_some())
    }

    /// The `ciphertext_size` of the event with `uid_hash` in `calendar_id`, or
    /// `None` when there is none — the replaced half of a write's quota delta
    /// (`caldav-server.md` § QUOTA → § Enforcement points).
    pub async fn caldav_event_ciphertext_size(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        uid_hash: &[u8],
    ) -> Result<Option<u32>> {
        let conn = self.conn.lock().await;
        let size: Option<i64> = conn
            .query_row(
                "SELECT ciphertext_size FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &calendar_id[..], uid_hash],
                |row| row.get(0),
            )
            .optional()
            .context("caldav_event_ciphertext_size")?;
        Ok(size.map(|s| s as u32))
    }

    /// Insert a `bridge_caldav_calendars` row, or report what's already
    /// there. Idempotent on byte-identical metadata; a metadata mismatch on
    /// the same `(actor, calendar_id)` returns `Conflict` with the existing
    /// row untouched.
    pub async fn insert_bridge_caldav_calendar(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        encrypted_metadata: &[u8],
        now: i64,
    ) -> Result<ProvisionOutcome> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let metadata_owned = encrypted_metadata.to_vec();
        let conn = self.conn.lock().await;

        // INSERT OR IGNORE — atomic on the PK (actor, calendar_id).
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO bridge_caldav_calendars \
                 (actor_id, calendar_id, encrypted_metadata, ctag, highestmodseq, created_at) \
                 VALUES (?1, ?2, ?3, 0, 1, ?4)",
                rusqlite::params![&actor[..], &calendar_id[..], &metadata_owned, now],
            )
            .context("insert_bridge_caldav_calendar: insert or ignore")?;

        if changed == 1 {
            return Ok(ProvisionOutcome::Created);
        }

        // Row already existed — compare metadata bytes.
        let existing: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_metadata FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &calendar_id[..]],
                |row| row.get(0),
            )
            .context("insert_bridge_caldav_calendar: read existing metadata")?;

        if existing == metadata_owned {
            Ok(ProvisionOutcome::AlreadyExists)
        } else {
            Ok(ProvisionOutcome::Conflict)
        }
    }

    /// PROPPATCH-style metadata update for a `bridge_caldav_calendars` row.
    /// Overwrites `encrypted_metadata` and bumps `highestmodseq` + `ctag` in
    /// lockstep on byte-different bytes; returns `Updated` without bumping on
    /// byte-identical retries (so a retried PROPPATCH doesn't mislead
    /// sync-collection clients with a spurious change-notification). Returns
    /// `NotFound` when no row exists for `(actor, calendar_id)` — the handler
    /// maps that to `ProvisionCalendarReply::NotFound`.
    pub async fn update_bridge_caldav_calendar_metadata(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        new_encrypted_metadata: &[u8],
    ) -> Result<ProvisionOutcome> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let new_owned = new_encrypted_metadata.to_vec();
        let conn = self.conn.lock().await;

        let existing: Option<(Vec<u8>, i64)> = conn
            .query_row(
                "SELECT encrypted_metadata, highestmodseq FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &calendar_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("update_bridge_caldav_calendar_metadata: read existing row")?;

        let (existing_bytes, current_hms) = match existing {
            Some(pair) => pair,
            None => return Ok(ProvisionOutcome::NotFound),
        };

        if existing_bytes == new_owned {
            // Byte-identical retry: leave modseq/ctag/encrypted_metadata
            // untouched. The placement journal must mirror the no-op (handler
            // skips the append on this path), same pattern as the MKCOL
            // AlreadyExists branch.
            return Ok(ProvisionOutcome::Updated);
        }

        let new_modseq = current_hms + 1;
        conn.execute(
            "UPDATE bridge_caldav_calendars \
             SET encrypted_metadata = ?1, highestmodseq = ?2, ctag = ?2 \
             WHERE actor_id = ?3 AND calendar_id = ?4",
            rusqlite::params![&new_owned, new_modseq, &actor[..], &calendar_id[..]],
        )
        .context("update_bridge_caldav_calendar_metadata: overwrite and bump")?;

        Ok(ProvisionOutcome::Updated)
    }

    /// List every calendar belonging to `actor`, ordered by `created_at` ASC.
    pub async fn list_bridge_caldav_calendars(&self, actor: &[u8; 32]) -> Result<Vec<CalendarRow>> {
        let actor = *actor;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT calendar_id, encrypted_metadata, ctag, highestmodseq, created_at \
                 FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 \
                 ORDER BY created_at ASC, calendar_id ASC",
            )
            .context("prepare list_bridge_caldav_calendars")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], |row| {
                let calendar_id: [u8; 32] = blob_col_to_array(row.get(0)?, 0, "calendar_id")?;
                Ok(CalendarRow {
                    calendar_id,
                    encrypted_metadata: row.get(1)?,
                    ctag: row.get(2)?,
                    highestmodseq: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .context("query list_bridge_caldav_calendars")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_bridge_caldav_calendars")?;
        Ok(rows)
    }

    /// Count the events in `(actor, calendar_id)`. Returns `0` when the
    /// calendar has no events or doesn't exist.
    pub async fn count_bridge_caldav_events(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
    ) -> Result<u32> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &calendar_id[..]],
                |row| row.get(0),
            )
            .context("count_bridge_caldav_events")?;
        Ok(count as u32)
    }

    /// Return `Some(highestmodseq)` for `(actor, calendar_id)` if the
    /// calendar is provisioned, else `None`.
    pub async fn caldav_calendar_highestmodseq(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
    ) -> Result<Option<i64>> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let conn = self.conn.lock().await;
        let hms: Option<i64> = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &calendar_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("caldav_calendar_highestmodseq")?;
        Ok(hms)
    }

    /// Raise `(actor, calendar_id)`'s `highestmodseq` — and `ctag`, kept in
    /// lockstep — to at least `floor`. Monotonic and idempotent; a no-op for a
    /// calendar that is not provisioned.
    ///
    /// The lived-in recovery's counter floor (`segment-backup-protocol.md`
    /// § *Recovery's calendar and contacts arms*, collision (c)): a client holds
    /// a sync token from before the regression, so every recovered row must be
    /// numbered above the pre-regression counter the journal pins.
    pub async fn floor_bridge_caldav_calendar_counter(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        floor: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE bridge_caldav_calendars \
             SET highestmodseq = MAX(highestmodseq, ?1), ctag = MAX(ctag, ?1) \
             WHERE actor_id = ?2 AND calendar_id = ?3",
            rusqlite::params![floor, &actor[..], &calendar_id[..]],
        )
        .context("floor_bridge_caldav_calendar_counter")?;
        Ok(())
    }

    /// What the target holds of one resource `(calendar_id, uid_hash)`: the
    /// live row's server receive time (`created_at`, epoch seconds) if one
    /// exists, and the newest expunge tombstone's `expunged_at` for the UID.
    /// The lived-in recovery's collision (a) reads both against a lost
    /// record's own receive time.
    pub async fn caldav_resource_history(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        uid_hash: &[u8],
    ) -> Result<(Option<i64>, Option<i64>)> {
        let conn = self.conn.lock().await;
        let live: Option<i64> = conn
            .query_row(
                "SELECT MAX(created_at) FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &calendar_id[..], uid_hash],
                |row| row.get(0),
            )
            .context("caldav_resource_history: live row")?;
        let expunged: Option<i64> = conn
            .query_row(
                "SELECT MAX(expunged_at) FROM bridge_caldav_expunged \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &calendar_id[..], uid_hash],
                |row| row.get(0),
            )
            .context("caldav_resource_history: expunge tombstone")?;
        Ok((live, expunged))
    }

    /// Insert a fresh event into `(actor, calendar_id)`. **Never** updates an
    /// existing row — if `event_id` (deterministic blake3) collides, returns
    /// `Idempotent`. Missing calendar returns `CalendarMissing` without
    /// touching anything.
    ///
    /// ⚠ **Not a production writer, and test-only.** The sole production writer
    /// is `put_event_ciphertext_handler` → [`Self::replace_caldav_event_by_uid`],
    /// which appends the content record to the `__calendar` segment **before**
    /// inserting the row. A new production write path must do the same — wiring
    /// this method into one would file a row whose body rests nowhere.
    ///
    /// ⚠ **It writes the ROW only — no content record is appended, and the body
    /// is not stored.** The body is taken only to derive `event_id` and
    /// `record_cid`; the serve path resolves a body *exclusively* through the
    /// row's stored `record_cid`, so a row seeded here serves an EMPTY body
    /// until the caller also appends the record — from the *same* body+hint, so
    /// the derived cid matches. A test that seeds and then reads a body must do
    /// both; one that only needs a row to exist (placement, successions,
    /// compaction) needs nothing more.
    ///
    /// `record_cid` is **derived here, not taken**, as
    /// `Cid::of_dag_cbor(<encoded CalRecordEnvelope of body+hint>)` — bit-for-bit
    /// the mint `segments::cal::append_record` performs
    /// (`message-segment-store.md` § Record identity per kind). So a test that
    /// appends the same body+hint through the real append gets a row whose
    /// stored cid *matches* that record, and a test that seeds the row alone
    /// gets a row pointing at a record which genuinely does not exist — the
    /// orphan/guard case, honestly represented. Deriving beats a caller-supplied
    /// stand-in: a seeder handing out cids that no body hashes to would let a
    /// test pass while asserting an identity production could never mint.
    ///
    /// All SQL runs under a **single `conn.lock()` scope**, and the write
    /// sequence (event INSERT + calendar-state bump) runs in one
    /// `conn.unchecked_transaction()` so a crash can't leave an event without
    /// its lockstep (ctag, highestmodseq) bump. The single shared modseq bump
    /// is applied to the calendar state row exactly once iff a row is
    /// actually inserted.
    pub async fn place_caldav_event(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        uid_hash: &[u8],
        encrypted_body: &[u8],
        encrypted_index_hint: &[u8],
        timestamp: i64,
        ciphertext_size: u32,
        now: i64,
    ) -> Result<PlaceCaldavEventOutcome> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let uid_hash_owned = uid_hash.to_vec();
        let body_owned = encrypted_body.to_vec();
        let hint_owned = encrypted_index_hint.to_vec();
        let event_id = derive_caldav_event_id(&actor, timestamp, &body_owned);
        // The record identity this body+hint WOULD be filed under — see the doc
        // comment. Same envelope, same encoding, same mint as the real append.
        let record_cid = caldav_record_cid(&body_owned, &hint_owned)
            .context("place_caldav_event: derive record_cid")?;
        let conn = self.conn.lock().await;

        // 1. Check the calendar exists.
        let current_hms: Option<i64> = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &calendar_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("place_caldav_event: lookup calendar")?;
        let current_hms = match current_hms {
            None => return Ok(PlaceCaldavEventOutcome::CalendarMissing),
            Some(h) => h,
        };

        // 2. Idempotency: if event_id already exists, return its etag/modseq.
        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT etag, modseq FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND event_id = ?3",
                rusqlite::params![&actor[..], &calendar_id[..], &event_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("place_caldav_event: check existing event_id")?;
        if let Some((etag, modseq)) = existing {
            return Ok(PlaceCaldavEventOutcome::Idempotent {
                event_id,
                etag,
                modseq,
            });
        }

        // 3. Allocate the new modseq and insert, and 4. bump calendar state —
        // both in one transaction (all-or-nothing).
        let new_modseq = current_hms + 1;
        let etag = format_etag(new_modseq);
        let tx = conn
            .unchecked_transaction()
            .context("place_caldav_event: begin tx")?;
        tx.execute(
            "INSERT INTO bridge_caldav_events \
             (actor_id, calendar_id, event_id, uid_hash, \
              encrypted_index_hint, etag, modseq, ciphertext_size, internal_date, created_at, \
              record_cid) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                &actor[..],
                &calendar_id[..],
                &event_id[..],
                &uid_hash_owned,
                &hint_owned,
                &etag,
                new_modseq,
                ciphertext_size as i64,
                timestamp,
                now,
                &record_cid.as_bytes()[..],
            ],
        )
        .context("place_caldav_event: insert event")?;

        // 4. Bump calendar state row's (ctag, highestmodseq) in lockstep.
        tx.execute(
            "UPDATE bridge_caldav_calendars \
             SET highestmodseq = ?1, ctag = ?1 \
             WHERE actor_id = ?2 AND calendar_id = ?3",
            rusqlite::params![new_modseq, &actor[..], &calendar_id[..]],
        )
        .context("place_caldav_event: bump calendar state")?;
        tx.commit().context("place_caldav_event: commit")?;

        Ok(PlaceCaldavEventOutcome::Created {
            event_id,
            etag,
            modseq: new_modseq,
        })
    }

    /// PUT-by-UID: create on first call, replace + tombstone on subsequent
    /// calls (delete old event_id, insert new with bumped modseq).
    ///
    /// All SQL runs under a **single `conn.lock()` scope**, and each write arm
    /// (create / sidecar-only refine / real replace-and-tombstone) runs in one
    /// `conn.unchecked_transaction()` — the replace arm's tombstone-INSERT +
    /// old-DELETE + new-INSERT + bump commit all-or-nothing, so a crash can
    /// never leave a tombstone for a still-present event. A single shared
    /// modseq bump is applied per operation.
    ///
    /// `if_match`: when `Some(etag)`, compare against the prior row's etag
    /// (only meaningful when a prior row exists for this uid_hash); mismatch
    /// returns `PreconditionFailed` without touching anything. `None` means
    /// "unconditional".
    ///
    /// Idempotency: if the **new** body hashes to the same event_id as the
    /// **prior** row's event_id (same body bytes + same timestamp), this is
    /// a transport retry — return `Idempotent` with the existing etag/modseq
    /// and skip the bump.
    ///
    /// `new_event_id` is derived by the **caller** — `derive_caldav_event_id`
    /// over the request's body — because the body never reaches this method (it
    /// rests only in the `__calendar` segment), so this method cannot hash it.
    /// Deriving identity once, at the ingest perimeter, and carrying it
    /// opaquely thereafter is the rule the whole segment rollout rests on
    /// (`message-segment-store.md` § Per-kind rollout).
    ///
    /// Every write is accepted only when the `segment_records` mirror already
    /// proves the content record under `new_record_cid` durable — see the guard
    /// in [`replace_caldav_event_by_uid_in`].
    pub async fn replace_caldav_event_by_uid(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        uid_hash: &[u8],
        if_match: Option<&str>,
        new_event_id: &[u8; 32],
        // The appended record's content-hash filing CID (from
        // `segments::cal::ensure_in_segment`) — stored on the row, since the
        // identity is not re-derivable from `event_id`
        // (`message-segment-store.md` § Record identity per kind).
        new_record_cid: &fauna_cbor::Cid,
        new_encrypted_index_hint: &[u8],
        // The sealed Fauna-extension sidecar for this write, or `None` when the
        // write carries no sidecar (a MUA PUT). On UPDATE, `None` **preserves**
        // the prior row's sidecar; `Some(..)` replaces it (Fauna write).
        new_encrypted_fauna_ext: Option<&[u8]>,
        timestamp: i64,
        ciphertext_size: u32,
        now: i64,
    ) -> Result<ReplaceCaldavEventOutcome> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("replace_caldav_event_by_uid: begin tx")?;
        let outcome = replace_caldav_event_by_uid_in(
            &tx,
            actor,
            calendar_id,
            uid_hash,
            if_match,
            new_event_id,
            new_record_cid,
            new_encrypted_index_hint,
            new_encrypted_fauna_ext,
            timestamp,
            ciphertext_size,
            now,
        )?;
        tx.commit().context("replace_caldav_event_by_uid: commit")?;
        Ok(outcome)
    }

    /// DELETE-by-UID: remove the event row for `(actor, calendar_id, uid_hash)`
    /// and write a tombstone. All SQL under a **single `conn.lock()` scope**;
    /// the tombstone-INSERT + row-DELETE + calendar-state bump commit in one
    /// `conn.unchecked_transaction()`, so a crash can never leave a tombstone
    /// for a still-present event. One shared modseq bump per delete iff a row
    /// is actually removed.
    pub async fn delete_caldav_event_by_uid(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        uid_hash: &[u8],
        if_match: Option<&str>,
        now: i64,
    ) -> Result<DeleteCaldavEventOutcome> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let uid_hash_owned = uid_hash.to_vec();
        let if_match_owned: Option<String> = if_match.map(|s| s.to_string());
        let conn = self.conn.lock().await;

        // 1. Calendar exists?
        let current_hms: Option<i64> = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &calendar_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("delete_caldav_event_by_uid: lookup calendar")?;
        let current_hms = match current_hms {
            None => return Ok(DeleteCaldavEventOutcome::NotFound),
            Some(h) => h,
        };

        // 2. Look up the event row (record_cid included — the content record
        // is tombstoned by the STORED identity below).
        let row: Option<(Vec<u8>, String, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT event_id, etag, record_cid FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND uid_hash = ?3 \
                 ORDER BY modseq DESC LIMIT 1",
                rusqlite::params![&actor[..], &calendar_id[..], &uid_hash_owned],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("delete_caldav_event_by_uid: lookup event")?;

        let (event_id_blob, etag, record_cid_blob) = match row {
            None => return Ok(DeleteCaldavEventOutcome::NotFound),
            Some(r) => r,
        };

        // 3. If if_match supplied, gate on it.
        if let Some(m) = if_match_owned
            && m != etag
        {
            return Ok(DeleteCaldavEventOutcome::PreconditionFailed { current_etag: etag });
        }

        let event_id: [u8; 32] = event_id_blob.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "delete_caldav_event_by_uid: event_id wrong length: {}",
                event_id_blob.len()
            )
        })?;

        // 4. Bump modseq, write tombstone, delete row, update calendar state —
        // all in one transaction (all-or-nothing).
        let new_modseq = current_hms + 1;
        let tx = conn
            .unchecked_transaction()
            .context("delete_caldav_event_by_uid: begin tx")?;
        tx.execute(
            "INSERT INTO bridge_caldav_expunged \
             (actor_id, calendar_id, event_id, uid_hash, modseq, expunged_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                &actor[..],
                &calendar_id[..],
                &event_id[..],
                &uid_hash_owned,
                new_modseq,
                now,
            ],
        )
        .context("delete_caldav_event_by_uid: insert tombstone")?;
        tx.execute(
            "DELETE FROM bridge_caldav_events \
             WHERE actor_id = ?1 AND calendar_id = ?2 AND event_id = ?3",
            rusqlite::params![&actor[..], &calendar_id[..], &event_id[..]],
        )
        .context("delete_caldav_event_by_uid: delete row")?;

        // Row gone → unless another calendar's row shares it, the `__calendar`
        // content record is unreachable (every read starts from a row).
        // Tombstone it — by the STORED record_cid — so compaction reclaims the
        // bytes. AFTER the DELETE: a tombstoned record is reclaimable, so
        // tombstoning ahead of the row's removal would risk losing a live body
        // ([`tombstone_cal_record_if_unreferenced`]). A row without a stored
        // cid cannot exist post-cutover; if one shows up, leave its record to
        // the orphan reaper.
        match record_cid_blob
            .as_deref()
            .and_then(|b| <[u8; 36]>::try_from(b).ok())
            .and_then(|a| fauna_cbor::Cid::from_bytes(a).ok())
        {
            Some(cid) => {
                tombstone_cal_record_if_unreferenced(&tx, &actor, &cid)
                    .context("delete_caldav_event_by_uid: tombstone content record")?;
            }
            None => tracing::warn!(
                event_id = %hex::encode(event_id),
                "deleted event row had no record_cid — leaving its record to the orphan reaper"
            ),
        }

        tx.execute(
            "UPDATE bridge_caldav_calendars \
             SET highestmodseq = ?1, ctag = ?1 \
             WHERE actor_id = ?2 AND calendar_id = ?3",
            rusqlite::params![new_modseq, &actor[..], &calendar_id[..]],
        )
        .context("delete_caldav_event_by_uid: bump calendar state")?;
        tx.commit().context("delete_caldav_event_by_uid: commit")?;

        Ok(DeleteCaldavEventOutcome::Deleted {
            event_id,
            modseq: new_modseq,
        })
    }

    /// Paginated query over `bridge_caldav_events`.
    ///
    /// - `since_modseq`: when `Some`, restrict to `modseq > since_modseq`.
    /// - `after_event_id`: when `Some`, restrict to `event_id > since_event_id`
    ///   (lexicographic on BLOB) for pagination resume.
    /// - `limit`: caller passes `wire_limit + 1`; the returned `EventPage`
    ///   trims to `wire_limit` and reports `more = true` iff a `limit+1`th
    ///   row was returned.
    ///
    /// Empty (not an error) when the calendar is absent.
    pub async fn query_caldav_events(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        since_modseq: Option<i64>,
        after_event_id: Option<&[u8; 32]>,
        limit: u32,
    ) -> Result<EventPage> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let after_owned: Option<[u8; 32]> = after_event_id.copied();

        // Build SQL with the optional filters.
        let mut sql = String::from(
            "SELECT event_id, uid_hash, encrypted_index_hint, \
                    etag, modseq, ciphertext_size, internal_date, encrypted_fauna_ext, \
                    record_cid \
             FROM bridge_caldav_events \
             WHERE actor_id = ?1 AND calendar_id = ?2",
        );
        let mut param_idx = 3usize;
        let mut since_param: Option<i64> = None;
        let mut after_param: Option<[u8; 32]> = None;

        if let Some(seq) = since_modseq {
            sql.push_str(&format!(" AND modseq > ?{param_idx}"));
            since_param = Some(seq);
            param_idx += 1;
        }
        if let Some(after) = after_owned {
            sql.push_str(&format!(" AND event_id > ?{param_idx}"));
            after_param = Some(after);
            param_idx += 1;
        }
        sql.push_str(" ORDER BY event_id ASC");
        // Apply LIMIT only when > 0 — wire limit==0 → unbounded (handler may
        // explicitly pass req.limit+1 for pagination detection).
        let fetch_limit = limit;
        if fetch_limit > 0 {
            sql.push_str(&format!(" LIMIT {fetch_limit}"));
        }
        let _ = param_idx;

        let (events, more) = super::bridge_dav_common::paged_dav_query(
            &self.conn,
            &sql,
            &actor,
            &calendar_id,
            since_param,
            after_param,
            fetch_limit,
            map_event_row,
            "query_caldav_events",
        )
        .await?;

        Ok(EventPage { events, more })
    }

    /// Paginated sync query over `bridge_caldav_events`, ordered by `modseq ASC`.
    ///
    /// Used exclusively by the `sync_calendar_since` handler where pagination
    /// is by modseq window, not by event_id cursor.  Ordering by modseq
    /// guarantees that the cursor (`new_sync_token = last.modseq`) is always
    /// correct: the next call with `since_modseq = last.modseq` resumes from
    /// exactly the next un-returned event.
    ///
    /// - `since_modseq`: restrict to `modseq > since_modseq`.
    /// - `limit`: caller passes `wire_limit + 1`; the returned `EventPage`
    ///   trims to `wire_limit` and reports `more = true` iff a `limit+1`th row
    ///   was returned.  `limit = 0` → unbounded.
    ///
    /// Empty (not an error) when the calendar is absent.
    pub async fn query_caldav_changes_since(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        since_modseq: i64,
        limit: u32,
    ) -> Result<EventPage> {
        let actor = *actor;
        let calendar_id = *calendar_id;

        let mut sql = String::from(
            "SELECT event_id, uid_hash, encrypted_index_hint, \
                    etag, modseq, ciphertext_size, internal_date, encrypted_fauna_ext, \
                    record_cid \
             FROM bridge_caldav_events \
             WHERE actor_id = ?1 AND calendar_id = ?2 AND modseq > ?3 \
             ORDER BY modseq ASC",
        );
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare query_caldav_changes_since")?;
        let mut rows: Vec<EventRow> = stmt
            .query_map(
                rusqlite::params![&actor[..], &calendar_id[..], since_modseq],
                map_event_row,
            )
            .context("query_caldav_changes_since")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect query_caldav_changes_since")?;

        // Pagination detection: caller passed `wire_limit + 1`. If we hit
        // the limit (rows.len() == limit) AND limit > 0, trim and report
        // `more`. If limit == 0 (unbounded), `more` is always false.
        let more = limit > 0 && rows.len() as u32 == limit;
        if more {
            rows.pop();
        }

        Ok(EventPage { events: rows, more })
    }

    /// Return ascending-modseq tombstones from `bridge_caldav_expunged` with
    /// `modseq > since_modseq` for `(actor, calendar_id)`.
    pub async fn query_caldav_expunged_since(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        since_modseq: i64,
    ) -> Result<Vec<ExpungedRow>> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT event_id, uid_hash, modseq FROM bridge_caldav_expunged \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND modseq > ?3 \
                 ORDER BY modseq ASC",
            )
            .context("prepare query_caldav_expunged_since")?;
        let rows = stmt
            .query_map(
                rusqlite::params![&actor[..], &calendar_id[..], since_modseq],
                map_expunged_row,
            )
            .context("query_caldav_expunged_since")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect query_caldav_expunged_since")?;
        Ok(rows)
    }

    /// Returns `true` iff any tombstone newer than `since_modseq` was
    /// expunged strictly before `cutoff_ts` (epoch seconds) — i.e. the
    /// supplied sync-token predates the tombstone-retention window, so the
    /// set of deletions since that token can no longer be honestly
    /// enumerated. Drives the `SyncCalendarSinceReply::Ok { stale: true }`
    /// past-retention signal (`caldav-server.md` § Stale sync-token
    /// handling); the MDA then emits `DAV:valid-sync-token` and the MUA
    /// full-resyncs. Pure read; the `(actor_id, calendar_id, modseq)` index
    /// covers the `modseq >` range, then `expunged_at` is checked per row.
    pub async fn caldav_has_expunged_past_retention(
        &self,
        actor: &[u8; 32],
        calendar_id: &[u8; 32],
        since_modseq: i64,
        cutoff_ts: i64,
    ) -> Result<bool> {
        let actor = *actor;
        let calendar_id = *calendar_id;
        let conn = self.conn.lock().await;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS( \
                     SELECT 1 FROM bridge_caldav_expunged \
                     WHERE actor_id = ?1 AND calendar_id = ?2 \
                       AND modseq > ?3 AND expunged_at < ?4)",
                rusqlite::params![&actor[..], &calendar_id[..], since_modseq, cutoff_ts],
                |row| row.get(0),
            )
            .context("caldav_has_expunged_past_retention")?;
        Ok(exists)
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

/// [`CacheDb::replace_caldav_event_by_uid`] on the caller's connection, so a caller can share
/// its transaction — the lived-in recovery (`crate::backup::recover`) files a
/// recovered record's row in the same transaction as its mirror row, as
/// [`super::bridge_imap::place_new_message_in`] is to mail's. Opens no
/// transaction of its own: every write arm's statements commit or roll back
/// with the caller's.
#[allow(clippy::too_many_arguments)]
pub(crate) fn replace_caldav_event_by_uid_in(
    conn: &rusqlite::Connection,
    actor: &[u8; 32],
    calendar_id: &[u8; 32],
    uid_hash: &[u8],
    if_match: Option<&str>,
    new_event_id: &[u8; 32],
    // The appended record's content-hash filing CID (from
    // `segments::cal::ensure_in_segment`) — stored on the row, since the
    // identity is not re-derivable from `event_id`
    // (`message-segment-store.md` § Record identity per kind).
    new_record_cid: &fauna_cbor::Cid,
    new_encrypted_index_hint: &[u8],
    // The sealed Fauna-extension sidecar for this write, or `None` when the
    // write carries no sidecar (a MUA PUT). On UPDATE, `None` **preserves**
    // the prior row's sidecar; `Some(..)` replaces it (Fauna write).
    new_encrypted_fauna_ext: Option<&[u8]>,
    timestamp: i64,
    ciphertext_size: u32,
    now: i64,
) -> Result<ReplaceCaldavEventOutcome> {
    let actor = *actor;
    let calendar_id = *calendar_id;
    let uid_hash_owned = uid_hash.to_vec();
    let hint_owned = new_encrypted_index_hint.to_vec();
    let new_fauna_ext_owned: Option<Vec<u8>> = new_encrypted_fauna_ext.map(|s| s.to_vec());
    let if_match_owned: Option<String> = if_match.map(|s| s.to_string());
    let new_event_id = *new_event_id;
    let new_record_cid = *new_record_cid;

    // 0. No-data-loss guard (alpha; `encryption-at-rest.md` expand→migrate→
    // contract discipline). The body rests ONLY in the `__calendar` segment,
    // so every row this writes asserts "the record under `new_record_cid` is
    // durable". If that assertion were ever false — a mis-ordered caller, or
    // a crash between the row INSERT and a *later* append — the row would
    // reference a body that exists nowhere, and the client's retry would
    // derive the same `event_id`, take the idempotent path, and never
    // re-append it. Silent, user-irrecoverable loss. The mirror is the
    // cheapest proof of durability we can consult from inside this
    // transaction (plain SQL — `Db` never sees a `SegmentManager`, per
    // `message-segment-store.md:355`).
    if crate::segments::records_db::lookup_record(
        conn,
        &actor,
        crate::segments::cal::KIND,
        &new_record_cid,
    )
    .context("replace_caldav_event_by_uid: mirror lookup for segment-record guard")?
    .is_none()
    {
        anyhow::bail!(
            "refusing to store event {}: no live \
             __calendar segment record — append the content record first",
            hex::encode(new_event_id),
        );
    }

    // 1. Confirm calendar exists.
    let current_hms: Option<i64> = conn
        .query_row(
            "SELECT highestmodseq FROM bridge_caldav_calendars \
             WHERE actor_id = ?1 AND calendar_id = ?2",
            rusqlite::params![&actor[..], &calendar_id[..]],
            |row| row.get(0),
        )
        .optional()
        .context("replace_caldav_event_by_uid: lookup calendar")?;
    let current_hms = match current_hms {
        None => return Ok(ReplaceCaldavEventOutcome::CalendarMissing),
        Some(h) => h,
    };

    // 2. Look up the prior row (if any) for this (actor, calendar, uid_hash).
    // If multiple exist, take the most recent by modseq DESC — but our
    // schema shouldn't produce duplicates; defensive ordering only. The
    // prior `encrypted_fauna_ext` is fetched so a MUA write (no sidecar)
    // can preserve it onto the replacement row.
    let prior: Option<(Vec<u8>, String, i64, Option<Vec<u8>>, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT event_id, etag, modseq, encrypted_fauna_ext, record_cid \
             FROM bridge_caldav_events \
             WHERE actor_id = ?1 AND calendar_id = ?2 AND uid_hash = ?3 \
             ORDER BY modseq DESC LIMIT 1",
            rusqlite::params![&actor[..], &calendar_id[..], &uid_hash_owned],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .context("replace_caldav_event_by_uid: lookup prior row")?;

    match prior {
        None => {
            // No prior row — fall through to a fresh insert. Mirrors
            // place_caldav_event but allocates a Created outcome.
            if if_match_owned.is_some() {
                // If the caller supplied if_match but no row exists, we
                // still go ahead and create — the standard MUA behavior
                // is "If-Match: *" on create (which our wire form treats
                // as None) or no If-Match. A specific etag against an
                // absent row is a Created on our side; the upper layer
                // can pre-check if it really means "must exist."
                //
                // (CalDAV PUT with If-Match against a missing resource
                // is server's choice per RFC 7232 § 3.1; we choose to
                // treat it as a create.)
            }

            // Idempotency: extremely unlikely for "no prior row" but
            // check anyway — if the deterministic event_id already
            // exists (e.g. inserted under a *different* uid_hash by an
            // earlier path), surface it as Idempotent.
            let existing: Option<(String, i64, Option<Vec<u8>>)> = conn
                .query_row(
                    "SELECT etag, modseq, encrypted_fauna_ext FROM bridge_caldav_events \
                     WHERE actor_id = ?1 AND calendar_id = ?2 AND event_id = ?3",
                    rusqlite::params![&actor[..], &calendar_id[..], &new_event_id[..]],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .context("replace_caldav_event_by_uid: check event_id collision")?;
            if let Some((etag, modseq, encrypted_fauna_ext)) = existing {
                return Ok(ReplaceCaldavEventOutcome::Idempotent {
                    event_id: new_event_id,
                    etag,
                    modseq,
                    encrypted_fauna_ext,
                });
            }

            let new_modseq = current_hms + 1;
            let etag = format_etag(new_modseq);
            // No prior row → nothing to preserve; the sidecar is whatever
            // this write carried (Some for a Fauna write, None for a MUA).
            conn.execute(
                "INSERT INTO bridge_caldav_events \
                 (actor_id, calendar_id, event_id, uid_hash, \
                  encrypted_index_hint, etag, modseq, ciphertext_size, internal_date, created_at, \
                  encrypted_fauna_ext, record_cid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    &actor[..],
                    &calendar_id[..],
                    &new_event_id[..],
                    &uid_hash_owned,
                    &hint_owned,
                    &etag,
                    new_modseq,
                    ciphertext_size as i64,
                    timestamp,
                    now,
                    new_fauna_ext_owned.as_deref(),
                    &new_record_cid.as_bytes()[..],
                ],
            )
            .context("replace_caldav_event_by_uid: insert (no prior)")?;
            conn.execute(
                "UPDATE bridge_caldav_calendars \
                 SET highestmodseq = ?1, ctag = ?1 \
                 WHERE actor_id = ?2 AND calendar_id = ?3",
                rusqlite::params![new_modseq, &actor[..], &calendar_id[..]],
            )
            .context("replace_caldav_event_by_uid: bump calendar state (no prior)")?;
            Ok(ReplaceCaldavEventOutcome::Created {
                event_id: new_event_id,
                etag,
                modseq: new_modseq,
                encrypted_fauna_ext: new_fauna_ext_owned,
            })
        }
        Some((
            prior_event_id_blob,
            prior_etag,
            prior_modseq,
            prior_fauna_ext,
            prior_record_cid,
        )) => {
            // 3a. If if_match supplied, gate on it.
            if let Some(ref m) = if_match_owned
                && m != &prior_etag
            {
                return Ok(ReplaceCaldavEventOutcome::PreconditionFailed {
                    current_etag: prior_etag,
                });
            }

            // Effective sidecar: a Fauna write (`Some`) replaces; a MUA
            // write (`None`) preserves the prior row's sidecar — so a
            // generic-client edit keeps the Fauna refinement attached
            // (caldav-server.md § Event resources, invariant 2).
            let effective_fauna_ext: Option<Vec<u8>> = new_fauna_ext_owned
                .clone()
                .or_else(|| prior_fauna_ext.clone());

            // 3b. Idempotency check: same event_id as before? Skip bump —
            // *unless* the effective sidecar changed (a Fauna write that
            // refines only the sidecar with a byte-identical VEVENT body +
            // timestamp; the body-derived event_id collides but the row
            // genuinely changed). A transport retry carries the same
            // sidecar, so it still collapses to Idempotent.
            let prior_event_id: [u8; 32] =
                prior_event_id_blob.as_slice().try_into().map_err(|_| {
                    anyhow::anyhow!(
                        "replace_caldav_event_by_uid: prior event_id wrong length: {}",
                        prior_event_id_blob.len()
                    )
                })?;
            if prior_event_id == new_event_id {
                if effective_fauna_ext == prior_fauna_ext {
                    return Ok(ReplaceCaldavEventOutcome::Idempotent {
                        event_id: new_event_id,
                        etag: prior_etag,
                        modseq: prior_modseq,
                        encrypted_fauna_ext: prior_fauna_ext,
                    });
                }
                // Sidecar-only change on an identical body: update the
                // sidecar in place + bump modseq/etag. No tombstone — the
                // event_id is unchanged, so this is a metadata refinement,
                // not a delete+re-add.
                let new_modseq = current_hms + 1;
                let etag = format_etag(new_modseq);
                conn.execute(
                    "UPDATE bridge_caldav_events \
                     SET encrypted_fauna_ext = ?1, etag = ?2, modseq = ?3, internal_date = ?4 \
                     WHERE actor_id = ?5 AND calendar_id = ?6 AND event_id = ?7",
                    rusqlite::params![
                        effective_fauna_ext.as_deref(),
                        &etag,
                        new_modseq,
                        timestamp,
                        &actor[..],
                        &calendar_id[..],
                        &new_event_id[..],
                    ],
                )
                .context("replace_caldav_event_by_uid: update sidecar in place")?;
                conn.execute(
                    "UPDATE bridge_caldav_calendars \
                     SET highestmodseq = ?1, ctag = ?1 \
                     WHERE actor_id = ?2 AND calendar_id = ?3",
                    rusqlite::params![new_modseq, &actor[..], &calendar_id[..]],
                )
                .context("replace_caldav_event_by_uid: bump calendar state (sidecar)")?;
                return Ok(ReplaceCaldavEventOutcome::Updated {
                    event_id: new_event_id,
                    etag,
                    modseq: new_modseq,
                    encrypted_fauna_ext: effective_fauna_ext,
                });
            }

            // 3c. Real update: bump modseq, write tombstone for old
            // event_id, delete old row, insert new row. All writes commit
            // in one transaction, so no half-applied state (e.g. a
            // tombstone for a still-present event) is ever observable.
            let new_modseq = current_hms + 1;
            let etag = format_etag(new_modseq);

            // Tombstone first so a half-applied transaction can't lose
            // sync-collection information.
            conn.execute(
                "INSERT INTO bridge_caldav_expunged \
                 (actor_id, calendar_id, event_id, uid_hash, modseq, expunged_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &actor[..],
                    &calendar_id[..],
                    &prior_event_id[..],
                    &uid_hash_owned,
                    new_modseq,
                    now,
                ],
            )
            .context("replace_caldav_event_by_uid: insert tombstone")?;

            conn.execute(
                "DELETE FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND event_id = ?3",
                rusqlite::params![&actor[..], &calendar_id[..], &prior_event_id[..]],
            )
            .context("replace_caldav_event_by_uid: delete prior row")?;

            conn.execute(
                "INSERT INTO bridge_caldav_events \
                 (actor_id, calendar_id, event_id, uid_hash, \
                  encrypted_index_hint, etag, modseq, ciphertext_size, internal_date, created_at, \
                  encrypted_fauna_ext, record_cid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    &actor[..],
                    &calendar_id[..],
                    &new_event_id[..],
                    &uid_hash_owned,
                    &hint_owned,
                    &etag,
                    new_modseq,
                    ciphertext_size as i64,
                    timestamp,
                    now,
                    effective_fauna_ext.as_deref(),
                    &new_record_cid.as_bytes()[..],
                ],
            )
            .context("replace_caldav_event_by_uid: insert new row")?;

            // The superseded body now rests in the `__calendar` segment. If
            // no row references it any more, tombstone it so compaction can
            // reclaim the bytes — otherwise every PUT-replace leaks an
            // unreachable record that each future compaction copies forward.
            //
            // AFTER the DELETE and the new row's INSERT, so the reference
            // count sees the final row set: the new row may point at the
            // SAME record (identical sealed bytes re-PUT under a new
            // timestamp), and another calendar's row may too
            // ([`tombstone_cal_record_if_unreferenced`]).
            match prior_record_cid
                .as_deref()
                .and_then(|b| <[u8; 36]>::try_from(b).ok())
                .and_then(|a| fauna_cbor::Cid::from_bytes(a).ok())
            {
                Some(prior_cid) => {
                    tombstone_cal_record_if_unreferenced(conn, &actor, &prior_cid)
                        .context("replace_caldav_event_by_uid: tombstone superseded record")?;
                }
                // A prior row without a stored cid cannot exist
                // post-cutover; if one shows up, leave its record for
                // the orphan reaper rather than guessing an identity.
                None => tracing::warn!(
                    event_id = %hex::encode(prior_event_id),
                    "superseded event row had no record_cid — leaving its \
                     record to the orphan reaper"
                ),
            }

            conn.execute(
                "UPDATE bridge_caldav_calendars \
                 SET highestmodseq = ?1, ctag = ?1 \
                 WHERE actor_id = ?2 AND calendar_id = ?3",
                rusqlite::params![new_modseq, &actor[..], &calendar_id[..]],
            )
            .context("replace_caldav_event_by_uid: bump calendar state")?;

            Ok(ReplaceCaldavEventOutcome::Updated {
                event_id: new_event_id,
                etag,
                modseq: new_modseq,
                encrypted_fauna_ext: effective_fauna_ext,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::dav_identity::personal_calendar_id;

    impl CacheDb {
        /// Test shim: derive `event_id` **and** `record_cid` from the write
        /// exactly as `put_event_ciphertext_handler` does, file the record's
        /// `__calendar` mirror row (standing in for the handler's
        /// append-before-insert), then delegate.
        ///
        /// The cid is derived, not invented, so the row this writes carries the
        /// identity its body+hint genuinely hash to — see
        /// [`caldav_record_cid`]. The body itself is never stored on the row.
        #[allow(clippy::too_many_arguments)]
        async fn replace_caldav_event_by_uid_deriving(
            &self,
            actor: &[u8; 32],
            calendar_id: &[u8; 32],
            uid_hash: &[u8],
            if_match: Option<&str>,
            new_encrypted_body: &[u8],
            new_encrypted_index_hint: &[u8],
            new_encrypted_fauna_ext: Option<&[u8]>,
            timestamp: i64,
            ciphertext_size: u32,
            now: i64,
        ) -> Result<ReplaceCaldavEventOutcome> {
            let event_id = derive_caldav_event_id(actor, timestamp, new_encrypted_body);
            let record_cid = caldav_record_cid(new_encrypted_body, new_encrypted_index_hint)
                .expect("derive record_cid");
            ensure_cal_mirror_record(self, actor, &record_cid).await;
            self.replace_caldav_event_by_uid(
                actor,
                calendar_id,
                uid_hash,
                if_match,
                &event_id,
                &record_cid,
                new_encrypted_index_hint,
                new_encrypted_fauna_ext,
                timestamp,
                ciphertext_size,
                now,
            )
            .await
        }
    }

    /// File a live `__calendar` mirror row for `cid` unless one already
    /// exists — the precondition `replace_caldav_event_by_uid`'s no-data-loss
    /// guard checks. A cid whose earlier row was tombstoned (a superseded body
    /// re-PUT later) gets a fresh row under the next segment id, as a real
    /// re-append would.
    async fn ensure_cal_mirror_record(db: &CacheDb, actor: &[u8; 32], cid: &fauna_cbor::Cid) {
        if db
            .segment_records_lookup_record(actor, crate::segments::cal::KIND, cid)
            .await
            .unwrap()
            .is_some()
        {
            return;
        }
        let prior_rows: i64 = {
            let conn = db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM segment_records \
                 WHERE scope_id = ?1 AND kind = ?2 AND record_cid = ?3",
                rusqlite::params![&actor[..], crate::segments::cal::KIND, &cid.as_bytes()[..]],
                |row| row.get(0),
            )
            .unwrap()
        };
        db.segment_records_insert_calendar(
            actor,
            1 + prior_rows as u32,
            cid,
            "2023-11",
            1_700_000_000,
        )
        .await
        .unwrap();
    }

    fn make_actor(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn make_calendar_id(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn make_uid_hash(byte: u8) -> Vec<u8> {
        vec![byte; 32]
    }

    // ── insert_bridge_caldav_calendar ────────────────────────────────────────

    #[tokio::test]
    async fn insert_bridge_caldav_calendar_creates_first_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(1);
        let cal = make_calendar_id(2);
        let outcome = db
            .insert_bridge_caldav_calendar(&actor, &cal, b"meta-v1", 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(outcome, ProvisionOutcome::Created);

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_calendars WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn insert_bridge_caldav_calendar_idempotent_on_identical_bytes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(3);
        let cal = make_calendar_id(4);
        let first = db
            .insert_bridge_caldav_calendar(&actor, &cal, b"meta-bytes", 1_700_000_000)
            .await
            .unwrap();
        let second = db
            .insert_bridge_caldav_calendar(&actor, &cal, b"meta-bytes", 1_700_000_001)
            .await
            .unwrap();
        assert_eq!(first, ProvisionOutcome::Created);
        assert_eq!(second, ProvisionOutcome::AlreadyExists);

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_calendars WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "no second row");
    }

    #[tokio::test]
    async fn insert_bridge_caldav_calendar_conflict_on_differing_bytes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(5);
        let cal = make_calendar_id(6);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"meta-A", 1_700_000_000)
            .await
            .unwrap();
        let second = db
            .insert_bridge_caldav_calendar(&actor, &cal, b"meta-B", 1_700_000_001)
            .await
            .unwrap();
        assert_eq!(second, ProvisionOutcome::Conflict);

        // Original metadata unchanged.
        let conn = db.conn().await;
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_metadata FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &cal[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, b"meta-A");
    }

    // ── update_bridge_caldav_calendar_metadata ───────────────────────────────

    #[tokio::test]
    async fn update_bridge_caldav_calendar_metadata_overwrites_and_bumps_modseq() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(40);
        let cal = make_calendar_id(41);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"meta-v1", 1_700_000_000)
            .await
            .unwrap();
        let hms_before = db
            .caldav_calendar_highestmodseq(&actor, &cal)
            .await
            .unwrap()
            .unwrap();

        let outcome = db
            .update_bridge_caldav_calendar_metadata(&actor, &cal, b"meta-v2")
            .await
            .unwrap();
        assert_eq!(outcome, ProvisionOutcome::Updated);

        let rows = db.list_bridge_caldav_calendars(&actor).await.unwrap();
        assert_eq!(rows.len(), 1, "still exactly one row");
        assert_eq!(rows[0].encrypted_metadata, b"meta-v2");
        assert!(
            rows[0].highestmodseq > hms_before,
            "highestmodseq must bump (was {}, now {})",
            hms_before,
            rows[0].highestmodseq,
        );
        assert_eq!(
            rows[0].ctag, rows[0].highestmodseq,
            "ctag bumps in lockstep with highestmodseq",
        );
    }

    #[tokio::test]
    async fn update_bridge_caldav_calendar_metadata_returns_not_found_when_missing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(42);
        let cal = make_calendar_id(43);
        // No prior insert.
        let outcome = db
            .update_bridge_caldav_calendar_metadata(&actor, &cal, b"meta-v1")
            .await
            .unwrap();
        assert_eq!(outcome, ProvisionOutcome::NotFound);

        // Update must not create a row.
        let rows = db.list_bridge_caldav_calendars(&actor).await.unwrap();
        assert!(rows.is_empty(), "NotFound path must not create rows");
    }

    #[tokio::test]
    async fn update_bridge_caldav_calendar_metadata_bumps_modseq_each_distinct_call() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(44);
        let cal = make_calendar_id(45);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"meta-v1", 1_700_000_000)
            .await
            .unwrap();
        let hms0 = db
            .caldav_calendar_highestmodseq(&actor, &cal)
            .await
            .unwrap()
            .unwrap();
        db.update_bridge_caldav_calendar_metadata(&actor, &cal, b"meta-v2")
            .await
            .unwrap();
        let hms1 = db
            .caldav_calendar_highestmodseq(&actor, &cal)
            .await
            .unwrap()
            .unwrap();
        db.update_bridge_caldav_calendar_metadata(&actor, &cal, b"meta-v3")
            .await
            .unwrap();
        let hms2 = db
            .caldav_calendar_highestmodseq(&actor, &cal)
            .await
            .unwrap()
            .unwrap();
        assert!(hms1 > hms0, "first update bumps");
        assert!(hms2 > hms1, "second update bumps");
    }

    #[tokio::test]
    async fn update_bridge_caldav_calendar_metadata_is_idempotent_on_byte_identical_metadata() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(46);
        let cal = make_calendar_id(47);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"meta-v1", 1_700_000_000)
            .await
            .unwrap();
        let outcome_first = db
            .update_bridge_caldav_calendar_metadata(&actor, &cal, b"meta-v2")
            .await
            .unwrap();
        assert_eq!(outcome_first, ProvisionOutcome::Updated);
        let hms_after_first = db
            .caldav_calendar_highestmodseq(&actor, &cal)
            .await
            .unwrap()
            .unwrap();

        // Byte-identical retry: same outcome from the caller's POV, but no
        // spurious modseq bump (would mislead sync-collection clients).
        let outcome_retry = db
            .update_bridge_caldav_calendar_metadata(&actor, &cal, b"meta-v2")
            .await
            .unwrap();
        assert_eq!(outcome_retry, ProvisionOutcome::Updated);
        let hms_after_retry = db
            .caldav_calendar_highestmodseq(&actor, &cal)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            hms_after_retry, hms_after_first,
            "byte-identical retry must not bump highestmodseq"
        );
    }

    #[tokio::test]
    async fn ensure_bridge_caldav_calendar_exists_reflects_presence() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(7);
        let cal = make_calendar_id(8);
        let other = make_calendar_id(9);

        assert!(
            !db.ensure_bridge_caldav_calendar_exists(&actor, &cal)
                .await
                .unwrap()
        );

        db.insert_bridge_caldav_calendar(&actor, &cal, b"x", 1_700_000_000)
            .await
            .unwrap();

        assert!(
            db.ensure_bridge_caldav_calendar_exists(&actor, &cal)
                .await
                .unwrap()
        );
        assert!(
            !db.ensure_bridge_caldav_calendar_exists(&actor, &other)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn list_bridge_caldav_calendars_empty_when_none() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(10);
        let rows = db.list_bridge_caldav_calendars(&actor).await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn list_bridge_caldav_calendars_returns_provisioned_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(11);
        let cal_a = make_calendar_id(12);
        let cal_b = make_calendar_id(13);
        db.insert_bridge_caldav_calendar(&actor, &cal_a, b"A", 1_700_000_000)
            .await
            .unwrap();
        db.insert_bridge_caldav_calendar(&actor, &cal_b, b"B", 1_700_000_001)
            .await
            .unwrap();
        let rows = db.list_bridge_caldav_calendars(&actor).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].calendar_id, cal_a);
        assert_eq!(rows[0].encrypted_metadata, b"A");
        assert_eq!(rows[0].ctag, 0, "fresh calendar has ctag=0");
        assert_eq!(rows[0].highestmodseq, 1, "fresh calendar has hms=1");
        assert_eq!(rows[1].calendar_id, cal_b);
        assert_eq!(rows[1].encrypted_metadata, b"B");
    }

    // ── caldav_calendar_highestmodseq ────────────────────────────────────────

    #[tokio::test]
    async fn caldav_calendar_highestmodseq_returns_none_when_absent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(14);
        let cal = make_calendar_id(15);
        let hms = db
            .caldav_calendar_highestmodseq(&actor, &cal)
            .await
            .unwrap();
        assert_eq!(hms, None);
    }

    #[tokio::test]
    async fn caldav_calendar_highestmodseq_returns_some_when_present() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(16);
        let cal = make_calendar_id(17);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let hms = db
            .caldav_calendar_highestmodseq(&actor, &cal)
            .await
            .unwrap();
        assert_eq!(hms, Some(1));
    }

    // ── place_caldav_event ───────────────────────────────────────────────────

    #[tokio::test]
    async fn place_caldav_event_allocates_sequential_modseqs() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(20);
        let cal = make_calendar_id(21);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"meta", 1_700_000_000)
            .await
            .unwrap();

        let uid_a = make_uid_hash(30);
        let uid_b = make_uid_hash(31);
        let o1 = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid_a,
                b"body-1",
                b"hint-1",
                1_700_000_000,
                6,
                1_700_000_100,
            )
            .await
            .unwrap();
        let o2 = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid_b,
                b"body-2",
                b"hint-2",
                1_700_000_001,
                6,
                1_700_000_101,
            )
            .await
            .unwrap();
        match (o1, o2) {
            (
                PlaceCaldavEventOutcome::Created {
                    modseq: m1,
                    etag: e1,
                    ..
                },
                PlaceCaldavEventOutcome::Created {
                    modseq: m2,
                    etag: e2,
                    ..
                },
            ) => {
                assert_eq!(m1, 2, "first PUT bumps from baseline 1 to 2");
                assert_eq!(m2, 3, "second PUT bumps to 3");
                assert_eq!(e1, format_etag(2));
                assert_eq!(e2, format_etag(3));
            }
            other => panic!("expected two Created; got {:?}", other),
        }

        // ctag tracks highestmodseq.
        let conn = db.conn().await;
        let (hms, ctag): (i64, i64) = conn
            .query_row(
                "SELECT highestmodseq, ctag FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &cal[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(hms, 3);
        assert_eq!(ctag, hms, "ctag bumps in lockstep with highestmodseq");
    }

    #[tokio::test]
    async fn place_caldav_event_isolated_per_calendar() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(40);
        let cal_a = make_calendar_id(41);
        let cal_b = make_calendar_id(42);
        db.insert_bridge_caldav_calendar(&actor, &cal_a, b"A", 1_700_000_000)
            .await
            .unwrap();
        db.insert_bridge_caldav_calendar(&actor, &cal_b, b"B", 1_700_000_001)
            .await
            .unwrap();

        let uid = make_uid_hash(50);
        let o_a = db
            .place_caldav_event(
                &actor,
                &cal_a,
                &uid,
                b"body-A",
                b"hint",
                1_700_000_000,
                6,
                1_700_000_100,
            )
            .await
            .unwrap();
        let o_b = db
            .place_caldav_event(
                &actor,
                &cal_b,
                &uid,
                b"body-B",
                b"hint",
                1_700_000_000,
                6,
                1_700_000_101,
            )
            .await
            .unwrap();
        match (o_a, o_b) {
            (
                PlaceCaldavEventOutcome::Created { modseq: m_a, .. },
                PlaceCaldavEventOutcome::Created { modseq: m_b, .. },
            ) => {
                assert_eq!(m_a, 2);
                assert_eq!(m_b, 2, "each calendar has its own modseq counter");
            }
            other => panic!("expected two Created; got {:?}", other),
        }
    }

    #[tokio::test]
    async fn place_caldav_event_idempotent_on_identical_body() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(60);
        let cal = make_calendar_id(61);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"meta", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(70);
        let first = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid,
                b"same-body",
                b"hint",
                1_700_000_000,
                9,
                1_700_000_100,
            )
            .await
            .unwrap();
        let (first_event_id, first_modseq, first_etag) = match first {
            PlaceCaldavEventOutcome::Created {
                event_id,
                modseq,
                etag,
            } => (event_id, modseq, etag),
            other => panic!("expected Created; got {:?}", other),
        };

        let retry = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid,
                b"same-body",
                b"hint",
                1_700_000_000,
                9,
                1_700_000_200,
            )
            .await
            .unwrap();
        match retry {
            PlaceCaldavEventOutcome::Idempotent {
                event_id,
                modseq,
                etag,
            } => {
                assert_eq!(event_id, first_event_id);
                assert_eq!(modseq, first_modseq, "modseq must NOT bump on retry");
                assert_eq!(etag, first_etag);
            }
            other => panic!("expected Idempotent; got {:?}", other),
        }

        // Confirm only one event row.
        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &cal[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn place_caldav_event_missing_calendar_returns_calendar_missing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(80);
        let cal = make_calendar_id(81);
        let uid = make_uid_hash(90);
        let o = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid,
                b"body",
                b"hint",
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .unwrap();
        assert_eq!(o, PlaceCaldavEventOutcome::CalendarMissing);

        // No event row, no calendar row.
        let conn = db.conn().await;
        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        let cal_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_calendars WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 0);
        assert_eq!(cal_count, 0);
    }

    // ── replace_caldav_event_by_uid ──────────────────────────────────────────

    #[tokio::test]
    async fn replace_caldav_event_by_uid_creates_when_absent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(100);
        let cal = make_calendar_id(101);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(110);
        let o = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                None,
                b"body",
                b"hint",
                None,
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .unwrap();
        match o {
            ReplaceCaldavEventOutcome::Created { modseq, etag, .. } => {
                assert_eq!(modseq, 2);
                assert_eq!(etag, format_etag(2));
            }
            other => panic!("expected Created; got {:?}", other),
        }
    }

    #[tokio::test]
    async fn replace_caldav_event_by_uid_updates_and_tombstones_existing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(120);
        let cal = make_calendar_id(121);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(130);

        let first = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                None,
                b"body-v1",
                b"hint",
                None,
                1_700_000_000,
                7,
                1_700_000_100,
            )
            .await
            .unwrap();
        let (first_event_id, first_modseq) = match first {
            ReplaceCaldavEventOutcome::Created {
                event_id, modseq, ..
            } => (event_id, modseq),
            other => panic!("expected Created; got {:?}", other),
        };

        let second = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                None,
                b"body-v2",
                b"hint",
                None,
                1_700_000_001,
                7,
                1_700_000_200,
            )
            .await
            .unwrap();
        match second {
            ReplaceCaldavEventOutcome::Updated {
                event_id, modseq, ..
            } => {
                assert_ne!(event_id, first_event_id, "new event_id on update");
                assert_eq!(modseq, first_modseq + 1, "modseq bumped exactly once");
            }
            other => panic!("expected Updated; got {:?}", other),
        }

        // Tombstone for the old event_id exists.
        let conn = db.conn().await;
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_expunged \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND event_id = ?3",
                rusqlite::params![&actor[..], &cal[..], &first_event_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tombstone_count, 1);

        // Exactly one live row for this uid_hash.
        let live_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &cal[..], &uid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live_count, 1);
    }

    #[tokio::test]
    async fn replace_caldav_event_by_uid_if_match_mismatch_returns_precondition_failed() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(140);
        let cal = make_calendar_id(141);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(150);

        let first = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                None,
                b"body-v1",
                b"hint",
                None,
                1_700_000_000,
                7,
                1_700_000_100,
            )
            .await
            .unwrap();
        let first_etag = match first {
            ReplaceCaldavEventOutcome::Created { etag, .. } => etag,
            other => panic!("expected Created; got {:?}", other),
        };

        // Wrong if_match.
        let bad = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                Some("ffffffffffffffff"),
                b"body-v2",
                b"hint",
                None,
                1_700_000_001,
                7,
                1_700_000_200,
            )
            .await
            .unwrap();
        match bad {
            ReplaceCaldavEventOutcome::PreconditionFailed { current_etag } => {
                assert_eq!(current_etag, first_etag);
            }
            other => panic!("expected PreconditionFailed; got {:?}", other),
        }

        // Correct if_match succeeds.
        let good = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                Some(&first_etag),
                b"body-v2",
                b"hint",
                None,
                1_700_000_001,
                7,
                1_700_000_300,
            )
            .await
            .unwrap();
        assert!(matches!(good, ReplaceCaldavEventOutcome::Updated { .. }));
    }

    #[tokio::test]
    async fn replace_caldav_event_by_uid_calendar_missing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(160);
        let cal = make_calendar_id(161);
        let uid = make_uid_hash(170);
        let o = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                None,
                b"body",
                b"hint",
                None,
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .unwrap();
        assert_eq!(o, ReplaceCaldavEventOutcome::CalendarMissing);
    }

    #[tokio::test]
    async fn replace_caldav_event_by_uid_sidecar_preserve_replace_and_in_place() {
        // caldav-server.md § Event resources, sidecar invariant 2: a MUA write
        // (no sidecar) on UPDATE preserves the prior `encrypted_fauna_ext`;
        // only a Fauna write (Some) replaces both halves.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(190);
        let cal = make_calendar_id(191);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(195);

        // Helper closures are awkward across `&self` futures; query inline.
        // 1. Fauna write: body-v1 + sidecar S1.
        db.replace_caldav_event_by_uid_deriving(
            &actor,
            &cal,
            &uid,
            None,
            b"body-v1",
            b"hint",
            Some(b"sidecar-v1"),
            1_700_000_000,
            7,
            1_700_000_100,
        )
        .await
        .unwrap();
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(
            page.events[0].encrypted_fauna_ext.as_deref(),
            Some(&b"sidecar-v1"[..])
        );
        assert_eq!(
            page.events[0].record_cid().unwrap(),
            Some(caldav_record_cid(b"body-v1", b"hint").unwrap())
        );

        // 2. MUA write: body-v2, NO sidecar (None) → preserves S1, replaces body.
        db.replace_caldav_event_by_uid_deriving(
            &actor,
            &cal,
            &uid,
            None,
            b"body-v2",
            b"hint",
            None,
            1_700_000_001,
            7,
            1_700_000_200,
        )
        .await
        .unwrap();
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(
            page.events[0].encrypted_fauna_ext.as_deref(),
            Some(&b"sidecar-v1"[..]),
            "MUA write preserves prior sidecar"
        );
        assert_eq!(
            page.events[0].record_cid().unwrap(),
            Some(caldav_record_cid(b"body-v2", b"hint").unwrap()),
            "MUA write replaced body"
        );

        // 3. Fauna write: body-v3 + sidecar S2 → replaces both halves.
        db.replace_caldav_event_by_uid_deriving(
            &actor,
            &cal,
            &uid,
            None,
            b"body-v3",
            b"hint",
            Some(b"sidecar-v2"),
            1_700_000_002,
            7,
            1_700_000_300,
        )
        .await
        .unwrap();
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(
            page.events[0].encrypted_fauna_ext.as_deref(),
            Some(&b"sidecar-v2"[..]),
            "Fauna write replaces sidecar"
        );
        assert_eq!(
            page.events[0].record_cid().unwrap(),
            Some(caldav_record_cid(b"body-v3", b"hint").unwrap())
        );
        let modseq_v3 = page.events[0].modseq;

        // 4. Sidecar-only change (identical body + timestamp ⇒ same event_id):
        // in-place sidecar update + modseq bump, still exactly one live row.
        let o = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                None,
                b"body-v3",
                b"hint",
                Some(b"sidecar-v3"),
                1_700_000_002,
                7,
                1_700_000_400,
            )
            .await
            .unwrap();
        assert!(matches!(o, ReplaceCaldavEventOutcome::Updated { .. }));
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.events.len(),
            1,
            "still one live row after sidecar-only change"
        );
        assert_eq!(
            page.events[0].encrypted_fauna_ext.as_deref(),
            Some(&b"sidecar-v3"[..])
        );
        let modseq_v4 = page.events[0].modseq;
        assert_eq!(
            modseq_v4,
            modseq_v3 + 1,
            "sidecar-only change bumps modseq once"
        );

        // 5. Exact retry (same body + ts + sidecar) ⇒ Idempotent, no bump.
        let o = db
            .replace_caldav_event_by_uid_deriving(
                &actor,
                &cal,
                &uid,
                None,
                b"body-v3",
                b"hint",
                Some(b"sidecar-v3"),
                1_700_000_002,
                7,
                1_700_000_500,
            )
            .await
            .unwrap();
        assert!(
            matches!(o, ReplaceCaldavEventOutcome::Idempotent { .. }),
            "byte-identical retry is idempotent"
        );
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.events[0].modseq, modseq_v4,
            "idempotent retry does not bump"
        );
    }

    // ── delete_caldav_event_by_uid ───────────────────────────────────────────

    #[tokio::test]
    async fn delete_caldav_event_by_uid_succeeds_and_tombstones() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(180);
        let cal = make_calendar_id(181);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(190);
        let placed = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid,
                b"body",
                b"hint",
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .unwrap();
        let placed_event_id = match placed {
            PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("expected Created; got {:?}", other),
        };

        let o = db
            .delete_caldav_event_by_uid(&actor, &cal, &uid, None, 1_700_000_200)
            .await
            .unwrap();
        match o {
            DeleteCaldavEventOutcome::Deleted { event_id, modseq } => {
                assert_eq!(event_id, placed_event_id);
                assert_eq!(modseq, 3, "place bumped to 2, delete bumps to 3");
            }
            other => panic!("expected Deleted; got {:?}", other),
        }

        // Event row gone, tombstone present.
        let conn = db.conn().await;
        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &cal[..]],
                |row| row.get(0),
            )
            .unwrap();
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_expunged \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND event_id = ?3",
                rusqlite::params![&actor[..], &cal[..], &placed_event_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 0);
        assert_eq!(tombstone_count, 1);
    }

    #[tokio::test]
    async fn delete_caldav_event_by_uid_missing_calendar_returns_not_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(200);
        let cal = make_calendar_id(201);
        let uid = make_uid_hash(210);
        let o = db
            .delete_caldav_event_by_uid(&actor, &cal, &uid, None, 1_700_000_100)
            .await
            .unwrap();
        assert_eq!(o, DeleteCaldavEventOutcome::NotFound);
    }

    #[tokio::test]
    async fn delete_caldav_event_by_uid_missing_event_returns_not_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(220);
        let cal = make_calendar_id(221);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(230);
        let o = db
            .delete_caldav_event_by_uid(&actor, &cal, &uid, None, 1_700_000_100)
            .await
            .unwrap();
        assert_eq!(o, DeleteCaldavEventOutcome::NotFound);
    }

    #[tokio::test]
    async fn delete_caldav_event_by_uid_if_match_mismatch_returns_precondition_failed() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(240);
        let cal = make_calendar_id(241);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(250);
        let placed = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid,
                b"body",
                b"hint",
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .unwrap();
        let placed_etag = match placed {
            PlaceCaldavEventOutcome::Created { etag, .. } => etag,
            other => panic!("expected Created; got {:?}", other),
        };

        let bad = db
            .delete_caldav_event_by_uid(&actor, &cal, &uid, Some("ffffffffffffffff"), 1_700_000_200)
            .await
            .unwrap();
        match bad {
            DeleteCaldavEventOutcome::PreconditionFailed { current_etag } => {
                assert_eq!(current_etag, placed_etag);
            }
            other => panic!("expected PreconditionFailed; got {:?}", other),
        }

        // Event still present after a failed if_match.
        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &cal[..], &uid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // ── query_caldav_events ─────────────────────────────────────────────────

    #[tokio::test]
    async fn query_caldav_events_empty_when_calendar_absent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa0);
        let cal = make_calendar_id(0xa1);
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 10)
            .await
            .unwrap();
        assert!(page.events.is_empty());
        assert!(!page.more);
    }

    #[tokio::test]
    async fn query_caldav_events_returns_all_when_no_filter() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa2);
        let cal = make_calendar_id(0xa3);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        // Three distinct bodies → three distinct event_ids.
        for (i, body) in [b"body-a", b"body-b", b"body-c"].iter().enumerate() {
            db.place_caldav_event(
                &actor,
                &cal,
                &make_uid_hash(i as u8 + 1),
                body.as_slice(),
                b"hint",
                1_700_000_000 + i as i64,
                body.len() as u32,
                1_700_000_100 + i as i64,
            )
            .await
            .unwrap();
        }
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 3);
        assert!(!page.more);
    }

    #[tokio::test]
    async fn query_caldav_events_pagination_with_limit_plus_one() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa4);
        let cal = make_calendar_id(0xa5);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        for i in 0..3 {
            db.place_caldav_event(
                &actor,
                &cal,
                &make_uid_hash(i as u8 + 1),
                format!("body-{}", i).as_bytes(),
                b"hint",
                1_700_000_000 + i as i64,
                10,
                1_700_000_100 + i as i64,
            )
            .await
            .unwrap();
        }

        // Request limit=2 → caller passes wire_limit+1 = 3.
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 3)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 2, "trimmed to wire_limit");
        assert!(page.more, "more flag set");

        // Resume from the last returned event_id.
        let last = page.events.last().unwrap().event_id;
        let page2 = db
            .query_caldav_events(&actor, &cal, None, Some(&last), 3)
            .await
            .unwrap();
        assert_eq!(page2.events.len(), 1, "third event returned");
        assert!(!page2.more);
    }

    #[tokio::test]
    async fn query_caldav_events_since_modseq_filters() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa6);
        let cal = make_calendar_id(0xa7);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let o1 = db
            .place_caldav_event(
                &actor,
                &cal,
                &make_uid_hash(1),
                b"body-1",
                b"hint",
                1_700_000_000,
                6,
                1_700_000_100,
            )
            .await
            .unwrap();
        let modseq_1 = match o1 {
            PlaceCaldavEventOutcome::Created { modseq, .. } => modseq,
            _ => panic!(),
        };
        db.place_caldav_event(
            &actor,
            &cal,
            &make_uid_hash(2),
            b"body-2",
            b"hint",
            1_700_000_001,
            6,
            1_700_000_101,
        )
        .await
        .unwrap();

        let page = db
            .query_caldav_events(&actor, &cal, Some(modseq_1), None, 0)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1, "only the second event");
        assert_eq!(page.events[0].modseq, modseq_1 + 1);
    }

    // ── query_caldav_expunged_since ─────────────────────────────────────────

    #[tokio::test]
    async fn query_caldav_expunged_since_filters_and_orders() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa8);
        let cal = make_calendar_id(0xa9);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        // Place + delete twice with distinct uid_hashes.
        let uid_a = make_uid_hash(10);
        let uid_b = make_uid_hash(11);
        db.place_caldav_event(
            &actor,
            &cal,
            &uid_a,
            b"body-a",
            b"hint",
            1_700_000_000,
            6,
            1_700_000_100,
        )
        .await
        .unwrap();
        db.place_caldav_event(
            &actor,
            &cal,
            &uid_b,
            b"body-b",
            b"hint",
            1_700_000_001,
            6,
            1_700_000_101,
        )
        .await
        .unwrap();
        let d1 = db
            .delete_caldav_event_by_uid(&actor, &cal, &uid_a, None, 1_700_000_200)
            .await
            .unwrap();
        let modseq_d1 = match d1 {
            DeleteCaldavEventOutcome::Deleted { modseq, .. } => modseq,
            _ => panic!(),
        };
        db.delete_caldav_event_by_uid(&actor, &cal, &uid_b, None, 1_700_000_201)
            .await
            .unwrap();

        // since_modseq=0 → both tombstones, ascending modseq.
        let all = db
            .query_caldav_expunged_since(&actor, &cal, 0)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].modseq < all[1].modseq, "ascending modseq");
        assert_eq!(all[0].uid_hash, uid_a);
        assert_eq!(all[1].uid_hash, uid_b);

        // since_modseq=modseq_d1 → only the second tombstone.
        let some = db
            .query_caldav_expunged_since(&actor, &cal, modseq_d1)
            .await
            .unwrap();
        assert_eq!(some.len(), 1);
        assert_eq!(some[0].uid_hash, uid_b);
    }

    #[tokio::test]
    async fn caldav_has_expunged_past_retention_detects_only_aged_tombstones() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xc8);
        let cal = make_calendar_id(0xc9);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(20);
        db.place_caldav_event(
            &actor,
            &cal,
            &uid,
            b"body",
            b"hint",
            1_700_000_000,
            4,
            1_700_000_100,
        )
        .await
        .unwrap();
        // Expunge the event at expunged_at = 1_700_001_000 (epoch seconds).
        let d = db
            .delete_caldav_event_by_uid(&actor, &cal, &uid, None, 1_700_001_000)
            .await
            .unwrap();
        let modseq = match d {
            DeleteCaldavEventOutcome::Deleted { modseq, .. } => modseq,
            _ => panic!("expected Deleted"),
        };

        // cutoff after the tombstone's expunged_at → the tombstone is past
        // retention (aged out): a token at/below its modseq is stale.
        assert!(
            db.caldav_has_expunged_past_retention(&actor, &cal, 0, 1_700_002_000)
                .await
                .unwrap(),
            "tombstone expunged_at 1_700_001_000 < cutoff 1_700_002_000 → past retention"
        );

        // cutoff before the tombstone's expunged_at → still within the
        // retention window: not stale.
        assert!(
            !db.caldav_has_expunged_past_retention(&actor, &cal, 0, 1_700_000_500)
                .await
                .unwrap(),
            "tombstone is newer than the cutoff → still within retention"
        );

        // since_modseq at the tombstone's modseq → no tombstone is strictly
        // newer, so nothing is missed regardless of age: not stale.
        assert!(
            !db.caldav_has_expunged_past_retention(&actor, &cal, modseq, 1_700_002_000)
                .await
                .unwrap(),
            "no tombstone with modseq > since_modseq → not stale"
        );
    }

    // ── query_caldav_changes_since ──────────────────────────────────────────

    #[tokio::test]
    async fn query_caldav_changes_since_ascending_modseq_order_and_more_flag() {
        // Verifies:
        //   1. Events are returned in modseq ASC order (not event_id ASC).
        //   2. `more == true` when `limit == events_count` (i.e. limit+1 probe hit).
        //   3. The trimmed event is the one with the LARGEST modseq.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xb0);
        let cal = make_calendar_id(0xb1);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();

        // Place 3 events (modseqs 2, 3, 4 assigned in creation order).
        let uid_a = make_uid_hash(0xc0);
        let uid_b = make_uid_hash(0xc1);
        let uid_c = make_uid_hash(0xc2);
        let o1 = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid_a,
                b"body-a",
                b"hint",
                1_700_000_001,
                6,
                1_700_000_100,
            )
            .await
            .unwrap();
        let o2 = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid_b,
                b"body-b",
                b"hint",
                1_700_000_002,
                6,
                1_700_000_100,
            )
            .await
            .unwrap();
        let o3 = db
            .place_caldav_event(
                &actor,
                &cal,
                &uid_c,
                b"body-c",
                b"hint",
                1_700_000_003,
                6,
                1_700_000_100,
            )
            .await
            .unwrap();
        let modseq_a = match o1 {
            PlaceCaldavEventOutcome::Created { modseq, .. } => modseq,
            _ => panic!(),
        };
        let modseq_b = match o2 {
            PlaceCaldavEventOutcome::Created { modseq, .. } => modseq,
            _ => panic!(),
        };
        let modseq_c = match o3 {
            PlaceCaldavEventOutcome::Created { modseq, .. } => modseq,
            _ => panic!(),
        };
        assert!(
            modseq_a < modseq_b && modseq_b < modseq_c,
            "sequential modseqs"
        );

        // limit=3 (wire_limit=2 + 1): should return first 2 events, more=true.
        let page = db
            .query_caldav_changes_since(&actor, &cal, 0, 3)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 2, "trimmed to wire_limit=2");
        assert!(
            page.more,
            "more must be true when limit+1 rows were available"
        );
        // Events are in modseq ASC order.
        assert_eq!(
            page.events[0].modseq, modseq_a,
            "first event has smallest modseq"
        );
        assert_eq!(
            page.events[1].modseq, modseq_b,
            "second event has middle modseq"
        );
        // The event with the LARGEST modseq (modseq_c) was trimmed.
        assert!(
            !page.events.iter().any(|e| e.modseq == modseq_c),
            "event with largest modseq must be the one trimmed, not returned"
        );

        // Fetching from modseq_b with limit=3 (wire_limit=2+1): only 1 remains, more=false.
        let page2 = db
            .query_caldav_changes_since(&actor, &cal, modseq_b, 3)
            .await
            .unwrap();
        assert_eq!(page2.events.len(), 1, "only event C remains after modseq_b");
        assert!(!page2.more, "no more events after modseq_b with limit=3");
        assert_eq!(page2.events[0].modseq, modseq_c);
    }

    /// `personal_calendar_id()` is an actor-independent blake3 constant
    /// (`dav_identity.rs`, `caldav-server.md` § Collections :117) — every
    /// user's default calendar shares this exact id, byte for byte. On the
    /// MDA path (`require_dav_caller_scope`, `bridge_routing_handlers.rs:259`)
    /// this id comes straight off the request URL, so `WHERE actor_id = ?1`
    /// in this query is the ONLY thing separating two users'
    /// rows.
    #[tokio::test]
    async fn query_caldav_changes_since_is_isolated_per_actor_on_shared_collection_id() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_a = make_actor(0xd0);
        let actor_b = make_actor(0xd1);
        let cal = personal_calendar_id();
        db.insert_bridge_caldav_calendar(&actor_a, &cal, b"a", 1_700_000_000)
            .await
            .unwrap();
        db.insert_bridge_caldav_calendar(&actor_b, &cal, b"b", 1_700_000_000)
            .await
            .unwrap();
        db.place_caldav_event(
            &actor_a,
            &cal,
            &make_uid_hash(1),
            b"body-a",
            b"hint",
            1_700_000_001,
            6,
            1_700_000_100,
        )
        .await
        .unwrap();

        let page_a = db
            .query_caldav_changes_since(&actor_a, &cal, 0, 0)
            .await
            .unwrap();
        assert_eq!(page_a.events.len(), 1, "actor_a sees its own change");

        let page_b = db
            .query_caldav_changes_since(&actor_b, &cal, 0, 0)
            .await
            .unwrap();
        assert!(
            page_b.events.is_empty(),
            "actor_b must not see actor_a's change on the SAME shared collection id"
        );
    }

    // ── count_bridge_caldav_events ───────────────────────────────────────────

    #[tokio::test]
    async fn count_bridge_caldav_events_returns_zero_when_empty() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(200);
        let cal = make_calendar_id(201);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"meta", 1_700_000_000)
            .await
            .unwrap();
        let count = db.count_bridge_caldav_events(&actor, &cal).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn count_bridge_caldav_events_returns_correct_count_after_inserts() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(202);
        let cal = make_calendar_id(203);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"meta", 1_700_000_000)
            .await
            .unwrap();
        db.place_caldav_event(
            &actor,
            &cal,
            &make_uid_hash(210),
            b"body-1",
            b"hint-1",
            1_700_000_001,
            6,
            1_700_000_100,
        )
        .await
        .unwrap();
        db.place_caldav_event(
            &actor,
            &cal,
            &make_uid_hash(211),
            b"body-2",
            b"hint-2",
            1_700_000_002,
            6,
            1_700_000_101,
        )
        .await
        .unwrap();
        let count = db.count_bridge_caldav_events(&actor, &cal).await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn count_bridge_caldav_events_is_isolated_per_actor_and_calendar() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_a = make_actor(220);
        let actor_b = make_actor(221);
        let cal_x = make_calendar_id(222);
        let cal_y = make_calendar_id(223);
        // Provision all four combinations.
        for (actor, cal) in [(&actor_a, &cal_x), (&actor_a, &cal_y), (&actor_b, &cal_x)] {
            db.insert_bridge_caldav_calendar(actor, cal, b"meta", 1_700_000_000)
                .await
                .unwrap();
        }
        // Insert one event only into (actor_a, cal_x).
        db.place_caldav_event(
            &actor_a,
            &cal_x,
            &make_uid_hash(230),
            b"body",
            b"hint",
            1_700_000_001,
            6,
            1_700_000_100,
        )
        .await
        .unwrap();
        assert_eq!(
            db.count_bridge_caldav_events(&actor_a, &cal_x)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            db.count_bridge_caldav_events(&actor_a, &cal_y)
                .await
                .unwrap(),
            0,
            "different calendar, same actor"
        );
        assert_eq!(
            db.count_bridge_caldav_events(&actor_b, &cal_x)
                .await
                .unwrap(),
            0,
            "same calendar, different actor"
        );
    }

    /// The count-level twin above pins isolation via `count_bridge_caldav_events`;
    /// this pins the same property on `query_caldav_events` itself — the query
    /// the MDA path actually serves rows from — using the REAL shared collection
    /// id (`personal_calendar_id()`, `caldav-server.md` § Collections :117)
    /// rather than an arbitrary one.
    #[tokio::test]
    async fn query_caldav_events_is_isolated_per_actor_on_shared_collection_id() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_a = make_actor(0xd2);
        let actor_b = make_actor(0xd3);
        let cal = personal_calendar_id();
        db.insert_bridge_caldav_calendar(&actor_a, &cal, b"a", 1_700_000_000)
            .await
            .unwrap();
        db.insert_bridge_caldav_calendar(&actor_b, &cal, b"b", 1_700_000_000)
            .await
            .unwrap();
        db.place_caldav_event(
            &actor_a,
            &cal,
            &make_uid_hash(2),
            b"body-a",
            b"hint",
            1_700_000_001,
            6,
            1_700_000_100,
        )
        .await
        .unwrap();

        let page_a = db
            .query_caldav_events(&actor_a, &cal, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page_a.events.len(), 1, "actor_a sees its own event");

        let page_b = db
            .query_caldav_events(&actor_b, &cal, None, None, 0)
            .await
            .unwrap();
        assert!(
            page_b.events.is_empty(),
            "actor_b must not see actor_a's event on the SAME shared collection id"
        );
    }

    /// The alpha no-data-loss guard, pinned directly. Every row asserts "the
    /// body is durable in the `__calendar` segment"; recording that claim
    /// without a live mirror row would leave the event's body nowhere, and a
    /// client retry — deriving the same `event_id` from the same bytes — would
    /// take the idempotent path and never re-append it.
    ///
    /// This test is the reason the handler appends *before* it inserts, and it
    /// fails loudly if a future refactor reverses that order.
    #[tokio::test]
    async fn a_row_without_its_segment_record_is_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(70);
        let cal = make_calendar_id(71);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();

        let body = b"sealed-body";
        let ts = 1_700_000_000i64;
        let event_id = derive_caldav_event_id(&actor, ts, body);

        let err = db
            .replace_caldav_event_by_uid(
                &actor,
                &cal,
                &make_uid_hash(72),
                None,
                &event_id,
                // A cid the body WOULD hash to — but nothing was ever appended
                // under it, which is exactly what the guard must catch.
                &caldav_record_cid(body, b"hint").expect("derive record_cid"),
                b"hint",
                None,
                ts,
                body.len() as u32,
                1_700_000_100,
            )
            .await
            .expect_err("a row with no segment record must be refused");
        assert!(
            err.to_string()
                .contains("no live __calendar segment record"),
            "unexpected error: {err}"
        );

        // And the refusal left no row behind.
        let page = db
            .query_caldav_events(&actor, &cal, None, None, 10)
            .await
            .unwrap();
        assert!(page.events.is_empty(), "the guard must not write a row");
    }

    /// Seed a row plus a live `__calendar` mirror record for it, as a
    /// post-cutover event looks. Returns `(event_id, cid)`.
    async fn seed_event_with_content_record(
        db: &CacheDb,
        actor: &[u8; 32],
        cal: &[u8; 32],
        uid: &[u8],
        body: &[u8],
        ts: i64,
    ) -> ([u8; 32], fauna_cbor::Cid) {
        let event_id = match db
            .place_caldav_event(
                actor,
                cal,
                uid,
                body,
                b"hint",
                ts,
                body.len() as u32,
                ts + 100,
            )
            .await
            .unwrap()
        {
            PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("expected Created, got {other:?}"),
        };
        // The cid the seeded row carries — `place_caldav_event` derives the
        // same value from this body+hint.
        let cid = caldav_record_cid(body, b"hint").expect("derive record_cid");
        db.segment_records_insert_calendar(actor, 1, &cid, "2023-11", ts + 100)
            .await
            .unwrap();
        (event_id, cid)
    }

    async fn record_is_live(db: &CacheDb, actor: &[u8; 32], cid: &fauna_cbor::Cid) -> bool {
        db.segment_records_lookup_record(actor, crate::segments::cal::KIND, cid)
            .await
            .unwrap()
            .is_some()
    }

    /// Since S6.6 the body lives in the segment, not the row — so DELETE must
    /// tombstone the content record, else it is unreachable (every read starts
    /// from a row) yet immortal (compaction copies live records forward).
    #[tokio::test]
    async fn deleting_an_event_tombstones_its_content_record() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(90);
        let cal = make_calendar_id(91);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(92);
        let (_event_id, cid) =
            seed_event_with_content_record(&db, &actor, &cal, &uid, b"sealed-body", 1_700_000_000)
                .await;
        assert!(record_is_live(&db, &actor, &cid).await);

        let out = db
            .delete_caldav_event_by_uid(&actor, &cal, &uid, None, 1_700_000_200)
            .await
            .unwrap();
        assert!(matches!(out, DeleteCaldavEventOutcome::Deleted { .. }));
        assert!(
            !record_is_live(&db, &actor, &cid).await,
            "the deleted event's content record must be tombstoned"
        );
    }

    /// A PUT-replace supersedes the old body. The OLD record must be tombstoned
    /// and the NEW one — appended by the handler just before this call — must
    /// stay live. Tombstoning the new record would make the row's own body
    /// reclaimable: user-irrecoverable loss.
    #[tokio::test]
    async fn superseding_an_event_tombstones_only_the_old_content_record() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(93);
        let cal = make_calendar_id(94);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(95);
        let (_old_id, old_cid) =
            seed_event_with_content_record(&db, &actor, &cal, &uid, b"body-v1", 1_700_000_000)
                .await;

        // The handler appends the new record before calling replace.
        let new_id = derive_caldav_event_id(&actor, 1_700_000_300, b"body-v2");
        let new_cid = caldav_record_cid(b"body-v2", b"hint").expect("derive record_cid");
        db.segment_records_insert_calendar(&actor, 1, &new_cid, "2023-11", 1_700_000_300)
            .await
            .unwrap();

        let out = db
            .replace_caldav_event_by_uid(
                &actor,
                &cal,
                &uid,
                None,
                &new_id,
                &new_cid,
                b"hint",
                None,
                1_700_000_300,
                7,
                1_700_000_400,
            )
            .await
            .unwrap();
        assert!(matches!(out, ReplaceCaldavEventOutcome::Updated { .. }));

        assert!(
            !record_is_live(&db, &actor, &old_cid).await,
            "the superseded content record must be tombstoned"
        );
        assert!(
            record_is_live(&db, &actor, &new_cid).await,
            "the NEW content record must stay live — it is the row's body"
        );
    }

    ///  The record CID hashes only the sealed (body, hint) envelope,
    /// while `event_id` also mixes in the timestamp. A re-PUT of byte-identical
    /// sealed bytes under a new timestamp is therefore a real replace (new
    /// `event_id`) whose new row points at the SAME record — which must stay
    /// live, or compaction drops the row's body.
    #[tokio::test]
    async fn resealing_identical_bytes_under_a_new_timestamp_keeps_the_record_live() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(110);
        let cal = make_calendar_id(111);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(112);
        let (old_id, cid) =
            seed_event_with_content_record(&db, &actor, &cal, &uid, b"same-bytes", 1_700_000_000)
                .await;

        // The handler's `ensure_in_segment` dedups to the existing record, so
        // nothing new is appended; only the timestamp differs.
        let new_id = derive_caldav_event_id(&actor, 1_700_000_300, b"same-bytes");
        assert_ne!(old_id, new_id, "the timestamp must change the event_id");
        let out = db
            .replace_caldav_event_by_uid(
                &actor,
                &cal,
                &uid,
                None,
                &new_id,
                &cid,
                b"hint",
                None,
                1_700_000_300,
                10,
                1_700_000_400,
            )
            .await
            .unwrap();
        assert!(matches!(out, ReplaceCaldavEventOutcome::Updated { .. }));
        assert!(
            record_is_live(&db, &actor, &cid).await,
            "the live row still points at this record — tombstoning it loses the body"
        );
    }

    ///  One actor's two calendars holding byte-identical sealed bytes
    /// share one per-actor record. Deleting the event from one calendar must
    /// leave the record live for the other; deleting the last reference then
    /// tombstones it.
    #[tokio::test]
    async fn deleting_one_of_two_rows_sharing_a_record_keeps_it_live() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(113);
        let cal_a = make_calendar_id(114);
        let cal_b = make_calendar_id(115);
        for cal in [&cal_a, &cal_b] {
            db.insert_bridge_caldav_calendar(&actor, cal, b"m", 1_700_000_000)
                .await
                .unwrap();
        }
        let uid = make_uid_hash(116);
        let (_a, cid) =
            seed_event_with_content_record(&db, &actor, &cal_a, &uid, b"shared", 1_700_000_000)
                .await;
        // Same bytes into the second calendar: the row carries the same cid;
        // the record already rests in the segment.
        let placed = db
            .place_caldav_event(
                &actor,
                &cal_b,
                &uid,
                b"shared",
                b"hint",
                1_700_000_050,
                6,
                1_700_000_150,
            )
            .await
            .unwrap();
        assert!(matches!(placed, PlaceCaldavEventOutcome::Created { .. }));

        db.delete_caldav_event_by_uid(&actor, &cal_a, &uid, None, 1_700_000_200)
            .await
            .unwrap();
        assert!(
            record_is_live(&db, &actor, &cid).await,
            "calendar B's row still points at this record"
        );

        db.delete_caldav_event_by_uid(&actor, &cal_b, &uid, None, 1_700_000_300)
            .await
            .unwrap();
        assert!(
            !record_is_live(&db, &actor, &cid).await,
            "the last reference is gone — the record must be tombstoned"
        );
    }

    // ── Row 768: write-arm atomicity ────────────────────────────────────────
    //
    // Every arm below ends with the calendar-state (ctag, highestmodseq) bump
    // as its LAST statement. A trigger that raises on that bump forces the
    // failure after every earlier statement in the arm has already run, so
    // these pins can only pass if the whole arm commits as one transaction:
    // drop the `unchecked_transaction()` wrapper and the earlier statements
    // commit on their own, in their own autocommit transactions, before the
    // bump ever fires — redding every assertion below. `RAISE(ABORT)` is
    // required, not a dropped row: an `UPDATE`/`DELETE` matching nothing
    // returns `Ok(0)`, not an error, so it can't force a failure this deep
    // into an arm, and every function here holds `conn.lock()` for its whole
    // body, so a test can't act between two of its statements either.

    /// Install a trigger that fails the next `bridge_caldav_calendars` UPDATE
    /// — the shared last statement of every arm pinned below.
    async fn install_calendar_bump_failure(db: &CacheDb) {
        let conn = db.conn().await;
        conn.execute_batch(
            "CREATE TRIGGER row768_fail_calendar_bump \
             BEFORE UPDATE ON bridge_caldav_calendars \
             BEGIN SELECT RAISE(ABORT, 'row768: injected calendar-bump failure'); END;",
        )
        .unwrap();
    }

    #[tokio::test]
    async fn place_caldav_event_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(200);
        let cal = make_calendar_id(201);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(202);

        install_calendar_bump_failure(&db).await;

        db.place_caldav_event(
            &actor,
            &cal,
            &uid,
            b"body",
            b"hint",
            1_700_000_000,
            4,
            1_700_000_100,
        )
        .await
        .expect_err("the injected trigger must fail the arm");

        let conn = db.conn().await;
        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            event_count, 0,
            "the event insert must not survive the failed bump"
        );
        let hms: i64 = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &cal[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hms, 1, "the calendar state must not move");
    }

    #[tokio::test]
    async fn replace_caldav_event_by_uid_create_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(203);
        let cal = make_calendar_id(204);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(205);
        let body = b"body-v1";
        let event_id = derive_caldav_event_id(&actor, 1_700_000_000, body);
        let cid = caldav_record_cid(body, b"hint").expect("derive record_cid");
        ensure_cal_mirror_record(&db, &actor, &cid).await;

        install_calendar_bump_failure(&db).await;

        db.replace_caldav_event_by_uid(
            &actor,
            &cal,
            &uid,
            None,
            &event_id,
            &cid,
            b"hint",
            None,
            1_700_000_000,
            7,
            1_700_000_100,
        )
        .await
        .expect_err("the injected trigger must fail the create arm");

        let conn = db.conn().await;
        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            event_count, 0,
            "the create-arm insert must not survive the failed bump"
        );
        let hms: i64 = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_caldav_calendars \
                 WHERE actor_id = ?1 AND calendar_id = ?2",
                rusqlite::params![&actor[..], &cal[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hms, 1, "the calendar state must not move");
    }

    #[tokio::test]
    async fn replace_caldav_event_by_uid_sidecar_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(206);
        let cal = make_calendar_id(207);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(208);
        let body = b"identical-body";
        let ts = 1_700_000_000;
        let event_id = derive_caldav_event_id(&actor, ts, body);
        let cid = caldav_record_cid(body, b"hint").expect("derive record_cid");
        ensure_cal_mirror_record(&db, &actor, &cid).await;

        let created = db
            .replace_caldav_event_by_uid(
                &actor,
                &cal,
                &uid,
                None,
                &event_id,
                &cid,
                b"hint",
                None,
                ts,
                7,
                1_700_000_100,
            )
            .await
            .unwrap();
        let (prior_etag, prior_modseq) = match created {
            ReplaceCaldavEventOutcome::Created { etag, modseq, .. } => (etag, modseq),
            other => panic!("expected Created; got {:?}", other),
        };

        install_calendar_bump_failure(&db).await;

        db.replace_caldav_event_by_uid(
            &actor,
            &cal,
            &uid,
            None,
            &event_id,
            &cid,
            b"hint",
            Some(b"sidecar-v1"),
            ts,
            7,
            1_700_000_200,
        )
        .await
        .expect_err("the injected trigger must fail the sidecar arm");

        let conn = db.conn().await;
        let (etag, modseq, sidecar): (String, i64, Option<Vec<u8>>) = conn
            .query_row(
                "SELECT etag, modseq, encrypted_fauna_ext FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND event_id = ?2",
                rusqlite::params![&actor[..], &event_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(etag, prior_etag, "etag must not move when the bump fails");
        assert_eq!(
            modseq, prior_modseq,
            "modseq must not move when the bump fails"
        );
        assert_eq!(
            sidecar, None,
            "the sidecar write must not survive the failed bump"
        );
    }

    #[tokio::test]
    async fn replace_caldav_event_by_uid_replace_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(209);
        let cal = make_calendar_id(210);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(211);
        let (prior_event_id, prior_cid) =
            seed_event_with_content_record(&db, &actor, &cal, &uid, b"body-v1", 1_700_000_000)
                .await;
        let (prior_etag, prior_modseq): (String, i64) = {
            let conn = db.conn().await;
            conn.query_row(
                "SELECT etag, modseq FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND event_id = ?2",
                rusqlite::params![&actor[..], &prior_event_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };

        install_calendar_bump_failure(&db).await;

        let new_event_id = derive_caldav_event_id(&actor, 1_700_000_300, b"body-v2");
        let new_cid = caldav_record_cid(b"body-v2", b"hint").expect("derive record_cid");
        ensure_cal_mirror_record(&db, &actor, &new_cid).await;
        db.replace_caldav_event_by_uid(
            &actor,
            &cal,
            &uid,
            None,
            &new_event_id,
            &new_cid,
            b"hint",
            None,
            1_700_000_300,
            7,
            1_700_000_400,
        )
        .await
        .expect_err("the injected trigger must fail the replace arm");

        let conn = db.conn().await;
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_expunged \
                 WHERE actor_id = ?1 AND event_id = ?2",
                rusqlite::params![&actor[..], &prior_event_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            tombstone_count, 0,
            "no tombstone must survive the failed bump"
        );

        let (etag, modseq): (String, i64) = conn
            .query_row(
                "SELECT etag, modseq FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND event_id = ?2",
                rusqlite::params![&actor[..], &prior_event_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (etag, modseq),
            (prior_etag, prior_modseq),
            "the prior row must survive unchanged"
        );

        let new_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events WHERE actor_id = ?1 AND event_id = ?2",
                rusqlite::params![&actor[..], &new_event_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_count, 0, "the new row must not survive the failed bump");
        drop(conn);

        assert!(
            record_is_live(&db, &actor, &prior_cid).await,
            "the superseded content record must not be tombstoned when the arm fails"
        );
    }

    #[tokio::test]
    async fn delete_caldav_event_by_uid_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(212);
        let cal = make_calendar_id(213);
        db.insert_bridge_caldav_calendar(&actor, &cal, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(214);
        let (event_id, cid) =
            seed_event_with_content_record(&db, &actor, &cal, &uid, b"body", 1_700_000_000).await;

        install_calendar_bump_failure(&db).await;

        db.delete_caldav_event_by_uid(&actor, &cal, &uid, None, 1_700_000_200)
            .await
            .expect_err("the injected trigger must fail the delete arm");

        let conn = db.conn().await;
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_expunged \
                 WHERE actor_id = ?1 AND event_id = ?2",
                rusqlite::params![&actor[..], &event_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            tombstone_count, 0,
            "no tombstone must survive the failed bump"
        );

        let live_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_caldav_events WHERE actor_id = ?1 AND event_id = ?2",
                rusqlite::params![&actor[..], &event_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live_count, 1, "the prior row must survive the failed bump");
        drop(conn);

        assert!(
            record_is_live(&db, &actor, &cid).await,
            "the content record must not be tombstoned when the delete arm fails"
        );
    }
}
