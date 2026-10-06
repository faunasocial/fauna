//! CardDAV placement layer for the I2b mail-bridge absorption (Phase D).
//!
//! Three tables (defined in `db::migrations::MIGRATIONS_BRIDGE_ABSORPTION`):
//!   * `bridge_carddav_addressbooks` — per-(actor, addressbook_id) collection
//!     state: sealed metadata, ctag, highestmodseq.
//!   * `bridge_carddav_cards`        — per-(actor, addressbook_id, card_id) card
//!     placement: ciphertext body + sealed index hint + etag/modseq + UID hash.
//!   * `bridge_carddav_expunged`     — deletion tombstones for RFC 6578
//!     `REPORT sync-collection` VANISHED-equivalent responses.
//!
//! Every public method takes a `&self` receiver and operates under a single
//! `self.conn.lock().await` scope. All `modseq` bumps allocate **one** new
//! modseq per operation (CONDSTORE semantics, mirrored from Phase C IMAP).
//!
//! This is CardDAV's **only** store path: unlike CalDAV/IMAP, CardDAV seals in
//! both storage modes and has **no plaintext-at-rest branch** at all
//! (`carddav-server.md` § seal-always), so there is no sibling plaintext table
//! set — the ciphertext-only sidecar below *is* the whole store.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::CacheDb;
use super::bridge_dav_common::format_etag;
use super::{blob_col_to_array, blob_to_array};

const PUT_CARD_DST: &[u8] = b"fauna.bridges.put_card_ciphertext.v1";

// ── Helpers ──────────────────────────────────────────────────────────────────

/// `card_id = blake3(domain_tag || actor || timestamp_le_i64 || encrypted_body)`.
/// Domain-tagged so distinct call paths can never share a card_id, even on
/// byte-identical bodies. Length-prefix the domain tag so two tags of
/// different lengths but a coincidentally-shared concatenation can't collide.
pub(crate) fn derive_carddav_card_id(
    actor: &[u8; 32],
    timestamp: i64,
    encrypted_body: &[u8],
) -> [u8; 32] {
    super::bridge_dav_common::derive_dav_content_id(PUT_CARD_DST, actor, timestamp, encrypted_body)
}

/// Twin of `bridge_caldav::caldav_record_cid` — the record identity a
/// `(body, hint)` pair is filed under, `Cid::of_dag_cbor(<encoded
/// CardRecordEnvelope>)`. See that function's docs.
pub(crate) fn carddav_record_cid(
    encrypted_body: &[u8],
    encrypted_index_hint: &[u8],
) -> Result<fauna_cbor::Cid> {
    let (cid, _) = fauna_contacts::segments::envelope::CardRecordEnvelope::new(
        encrypted_body.to_vec(),
        encrypted_index_hint.to_vec(),
    )
    .encode_record()
    .map_err(|e| anyhow::anyhow!("encode CardRecordEnvelope: {e}"))?;
    Ok(cid)
}

/// Twin of `bridge_caldav::tombstone_cal_record_if_unreferenced`: tombstone
/// `actor`'s `__card` content record `cid` unless a card row, in ANY of the
/// actor's address books, still references it (see that function's docs). Returns whether it tombstoned. Call it on the write transaction
/// AFTER the row DELETE and any replacement row's INSERT.
fn tombstone_card_record_if_unreferenced(
    conn: &rusqlite::Connection,
    actor: &[u8; 32],
    cid: &fauna_cbor::Cid,
) -> Result<bool> {
    let live_refs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bridge_carddav_cards \
             WHERE actor_id = ?1 AND record_cid = ?2",
            rusqlite::params![&actor[..], &cid.as_bytes()[..]],
            |row| row.get(0),
        )
        .context("count live card rows referencing the record")?;
    if live_refs > 0 {
        return Ok(false);
    }
    crate::segments::records_db::tombstone_by_cid(conn, actor, crate::segments::card::KIND, cid)?;
    Ok(true)
}

// ── DB-side enums ────────────────────────────────────────────────────────────

/// Outcome of `insert_bridge_carddav_addressbook` (MKCOL path) and
/// `update_bridge_carddav_addressbook_metadata` (PROPPATCH path). The two
/// helpers have disjoint result spaces: `insert_*` returns `Created |
/// AlreadyExists | Conflict`; `update_*` returns `Updated | NotFound`. One
/// unified enum keeps the wire-side `ProvisionAddressbookReply` mapping in the
/// handler tight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// A new address book row was inserted (MKCOL path).
    Created,
    /// A row with the same addressbook_id already exists and its metadata bytes
    /// are byte-identical to the supplied bytes (idempotent MKCOL retry).
    AlreadyExists,
    /// A row with the same addressbook_id exists but its metadata bytes differ
    /// (MKCOL path; row unchanged).
    Conflict,
    /// The existing row's metadata was overwritten (PROPPATCH path).
    /// `highestmodseq` + `ctag` bumped unless the new bytes are byte-identical
    /// to the stored bytes (idempotent retry — no bump).
    Updated,
    /// No row exists for `(actor, addressbook_id)` (PROPPATCH path only — MKCOL
    /// never returns this; it inserts a fresh row instead).
    NotFound,
}

/// Outcome of `place_carddav_card`.
///
/// `place_carddav_card` never updates an existing row — it either creates a
/// fresh card_id row, is collapsed by an identical-bytes retry, or refuses
/// because the address book doesn't exist. Use `replace_carddav_card_by_uid`
/// for the full create-or-update + if_match path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceCarddavCardOutcome {
    Created {
        card_id: [u8; 32],
        etag: String,
        modseq: i64,
    },
    /// Transport-retry: a row with the deterministic card_id already exists.
    /// modseq + etag are the existing row's values; no bump.
    Idempotent {
        card_id: [u8; 32],
        etag: String,
        modseq: i64,
    },
    /// The (actor, addressbook_id) address book isn't provisioned. No insert.
    AddressbookMissing,
}

/// Outcome of `replace_carddav_card_by_uid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceCarddavCardOutcome {
    /// No prior row for this uid_hash; inserted a fresh row.
    ///
    /// `encrypted_fauna_ext` on the write outcomes is the row's **effective**
    /// sidecar after the write — only this method knows it (a MUA write
    /// carries `None` on the wire but *preserves* the prior row's sidecar).
    /// The handler journals it into the v2 `PutCard` placement record, so
    /// snapshot restore can rebuild the sidecar column
    /// (`message-segment-store.md` § Invariants, rule 2).
    Created {
        card_id: [u8; 32],
        etag: String,
        modseq: i64,
        encrypted_fauna_ext: Option<Vec<u8>>,
    },
    /// A prior row existed for this uid_hash; replaced it (delete-old-and-
    /// insert-new) and wrote a tombstone for the old card_id.
    Updated {
        card_id: [u8; 32],
        etag: String,
        modseq: i64,
        encrypted_fauna_ext: Option<Vec<u8>>,
    },
    /// The new body bytes hash to the same card_id as the prior row for
    /// this uid_hash — transport retry, no bump.
    Idempotent {
        card_id: [u8; 32],
        etag: String,
        modseq: i64,
        encrypted_fauna_ext: Option<Vec<u8>>,
    },
    /// `if_match` was supplied and didn't match the prior row's etag.
    PreconditionFailed { current_etag: String },
    /// The (actor, addressbook_id) address book isn't provisioned.
    AddressbookMissing,
}

/// Outcome of `delete_carddav_card_by_uid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteCarddavCardOutcome {
    Deleted {
        card_id: [u8; 32],
        modseq: i64,
    },
    /// Address book absent OR card absent (don't leak which — handler returns
    /// a single `NotFound` reply variant).
    NotFound,
    PreconditionFailed {
        current_etag: String,
    },
}

/// Outcome of `delete_carddav_addressbook`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteCarddavAddressbookOutcome {
    /// The address-book row and every card it held were deleted. `cards_deleted`
    /// is the number of card rows cascade-removed (`0` for an empty book).
    Deleted { cards_deleted: u32 },
    /// No address-book row exists for `(actor, addressbook_id)` — idempotent
    /// re-delete.
    NotFound,
}

// ── Data types ───────────────────────────────────────────────────────────────

/// A single address book row, returned by `list_bridge_carddav_addressbooks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressbookRow {
    pub addressbook_id: [u8; 32],
    pub encrypted_metadata: Vec<u8>,
    pub ctag: i64,
    pub highestmodseq: i64,
    pub created_at: i64,
}

/// A single card row, returned by `query_carddav_cards`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRow {
    pub card_id: [u8; 32],
    pub uid_hash: Vec<u8>,
    pub encrypted_index_hint: Vec<u8>,
    pub etag: String,
    pub modseq: i64,
    pub ciphertext_size: u32,
    pub internal_date: i64,
    /// Sealed Fauna-extension sidecar; `None` for MUA-written rows
    /// (carddav-server.md § Card resources). Never served to
    /// a MUA — surfaced only to Fauna apps via `CardEntry`.
    pub encrypted_fauna_ext: Option<Vec<u8>>,
    /// The record's stored content-hash filing CID (36-byte blob;
    /// `message-segment-store.md` § Record identity per kind). `None` cannot
    /// occur on a live row post-cutover; readers treat it as record-absent.
    pub record_cid: Option<Vec<u8>>,
}

impl CardRow {
    /// Parse the stored `record_cid` blob. `Ok(None)` = column NULL (treated
    /// as record-absent by readers); `Err` = a blob that is not a valid Cid.
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

/// Pagination wrapper around card rows.
#[derive(Debug, Clone)]
pub struct CardPage {
    pub cards: Vec<CardRow>,
    /// `true` iff there were strictly more rows than `limit`; the handler
    /// trims the extra and reports `more: true` upstream.
    pub more: bool,
}

/// A single tombstone row, returned by `query_carddav_expunged_since`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpungedCardRow {
    pub card_id: [u8; 32],
    pub uid_hash: Vec<u8>,
    pub modseq: i64,
}

// ── Row mappers (private) ────────────────────────────────────────────────────

fn map_card_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CardRow> {
    let card_id: [u8; 32] = blob_col_to_array(row.get(0)?, 0, "card_id")?;
    Ok(CardRow {
        card_id,
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

fn map_expunged_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExpungedCardRow> {
    let card_id: [u8; 32] = blob_col_to_array(row.get(0)?, 0, "card_id")?;
    Ok(ExpungedCardRow {
        card_id,
        uid_hash: row.get(1)?,
        modseq: row.get(2)?,
    })
}

// ── CacheDb methods ──────────────────────────────────────────────────────────

impl CacheDb {
    /// Return `true` iff `bridge_carddav_addressbooks` has a row for
    /// `(actor, addressbook_id)`. Used by handler "not found" paths.
    pub async fn ensure_bridge_carddav_addressbook_exists(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
    ) -> Result<bool> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let conn = self.conn.lock().await;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("ensure_bridge_carddav_addressbook_exists")?;
        Ok(found.is_some())
    }

    /// The `ciphertext_size` of the card with `uid_hash` in `addressbook_id`,
    /// or `None` when there is none — the calendar twin's
    /// `caldav_event_ciphertext_size` (the replaced half of a write's quota
    /// delta, `caldav-server.md` § QUOTA → § Enforcement points).
    pub async fn carddav_card_ciphertext_size(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        uid_hash: &[u8],
    ) -> Result<Option<u32>> {
        let conn = self.conn.lock().await;
        let size: Option<i64> = conn
            .query_row(
                "SELECT ciphertext_size FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &addressbook_id[..], uid_hash],
                |row| row.get(0),
            )
            .optional()
            .context("carddav_card_ciphertext_size")?;
        Ok(size.map(|s| s as u32))
    }

    /// Insert a `bridge_carddav_addressbooks` row, or report what's already
    /// there. Idempotent on byte-identical metadata; a metadata mismatch on
    /// the same `(actor, addressbook_id)` returns `Conflict` with the existing
    /// row untouched.
    pub async fn insert_bridge_carddav_addressbook(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        encrypted_metadata: &[u8],
        now: i64,
    ) -> Result<ProvisionOutcome> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let metadata_owned = encrypted_metadata.to_vec();
        let conn = self.conn.lock().await;

        // INSERT OR IGNORE — atomic on the PK (actor, addressbook_id).
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO bridge_carddav_addressbooks \
                 (actor_id, addressbook_id, encrypted_metadata, ctag, highestmodseq, created_at) \
                 VALUES (?1, ?2, ?3, 0, 1, ?4)",
                rusqlite::params![&actor[..], &addressbook_id[..], &metadata_owned, now],
            )
            .context("insert_bridge_carddav_addressbook: insert or ignore")?;

        if changed == 1 {
            return Ok(ProvisionOutcome::Created);
        }

        // Row already existed — compare metadata bytes.
        let existing: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_metadata FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
                |row| row.get(0),
            )
            .context("insert_bridge_carddav_addressbook: read existing metadata")?;

        if existing == metadata_owned {
            Ok(ProvisionOutcome::AlreadyExists)
        } else {
            Ok(ProvisionOutcome::Conflict)
        }
    }

    /// PROPPATCH-style metadata update for a `bridge_carddav_addressbooks` row.
    /// Overwrites `encrypted_metadata` and bumps `highestmodseq` + `ctag` in
    /// lockstep on byte-different bytes; returns `Updated` without bumping on
    /// byte-identical retries (so a retried PROPPATCH doesn't mislead
    /// sync-collection clients with a spurious change-notification). Returns
    /// `NotFound` when no row exists for `(actor, addressbook_id)` — the handler
    /// maps that to `ProvisionAddressbookReply::NotFound`.
    pub async fn update_bridge_carddav_addressbook_metadata(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        new_encrypted_metadata: &[u8],
    ) -> Result<ProvisionOutcome> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let new_owned = new_encrypted_metadata.to_vec();
        let conn = self.conn.lock().await;

        let existing: Option<(Vec<u8>, i64)> = conn
            .query_row(
                "SELECT encrypted_metadata, highestmodseq FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("update_bridge_carddav_addressbook_metadata: read existing row")?;

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
            "UPDATE bridge_carddav_addressbooks \
             SET encrypted_metadata = ?1, highestmodseq = ?2, ctag = ?2 \
             WHERE actor_id = ?3 AND addressbook_id = ?4",
            rusqlite::params![&new_owned, new_modseq, &actor[..], &addressbook_id[..]],
        )
        .context("update_bridge_carddav_addressbook_metadata: overwrite and bump")?;

        Ok(ProvisionOutcome::Updated)
    }

    /// List every address book belonging to `actor`, ordered by `created_at`
    /// ASC.
    pub async fn list_bridge_carddav_addressbooks(
        &self,
        actor: &[u8; 32],
    ) -> Result<Vec<AddressbookRow>> {
        let actor = *actor;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT addressbook_id, encrypted_metadata, ctag, highestmodseq, created_at \
                 FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 \
                 ORDER BY created_at ASC, addressbook_id ASC",
            )
            .context("prepare list_bridge_carddav_addressbooks")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], |row| {
                let addressbook_id: [u8; 32] = blob_col_to_array(row.get(0)?, 0, "addressbook_id")?;
                Ok(AddressbookRow {
                    addressbook_id,
                    encrypted_metadata: row.get(1)?,
                    ctag: row.get(2)?,
                    highestmodseq: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .context("query list_bridge_carddav_addressbooks")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_bridge_carddav_addressbooks")?;
        Ok(rows)
    }

    /// Count the cards in `(actor, addressbook_id)`. Returns `0` when the
    /// address book has no cards or doesn't exist.
    pub async fn count_bridge_carddav_cards(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
    ) -> Result<u32> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
                |row| row.get(0),
            )
            .context("count_bridge_carddav_cards")?;
        Ok(count as u32)
    }

    /// Return `Some(highestmodseq)` for `(actor, addressbook_id)` if the
    /// address book is provisioned, else `None`.
    pub async fn carddav_addressbook_highestmodseq(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
    ) -> Result<Option<i64>> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let conn = self.conn.lock().await;
        let hms: Option<i64> = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("carddav_addressbook_highestmodseq")?;
        Ok(hms)
    }

    /// Raise `(actor, addressbook_id)`'s `highestmodseq` — and `ctag`, kept in
    /// lockstep — to at least `floor`. Monotonic and idempotent; a no-op for an
    /// address book that is not provisioned. Twin of
    /// [`CacheDb::floor_bridge_caldav_calendar_counter`]; see it for the why.
    pub async fn floor_bridge_carddav_addressbook_counter(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        floor: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE bridge_carddav_addressbooks \
             SET highestmodseq = MAX(highestmodseq, ?1), ctag = MAX(ctag, ?1) \
             WHERE actor_id = ?2 AND addressbook_id = ?3",
            rusqlite::params![floor, &actor[..], &addressbook_id[..]],
        )
        .context("floor_bridge_carddav_addressbook_counter")?;
        Ok(())
    }

    /// What the target holds of one resource `(addressbook_id, uid_hash)`: the
    /// live row's `created_at` and the newest expunge tombstone's
    /// `expunged_at` (both epoch seconds). Twin of
    /// [`CacheDb::caldav_resource_history`].
    pub async fn carddav_resource_history(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        uid_hash: &[u8],
    ) -> Result<(Option<i64>, Option<i64>)> {
        let conn = self.conn.lock().await;
        let live: Option<i64> = conn
            .query_row(
                "SELECT MAX(created_at) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &addressbook_id[..], uid_hash],
                |row| row.get(0),
            )
            .context("carddav_resource_history: live row")?;
        let expunged: Option<i64> = conn
            .query_row(
                "SELECT MAX(expunged_at) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &addressbook_id[..], uid_hash],
                |row| row.get(0),
            )
            .context("carddav_resource_history: expunge tombstone")?;
        Ok((live, expunged))
    }

    /// Insert a fresh card into `(actor, addressbook_id)`. **Never** updates an
    /// existing row — if `card_id` (deterministic blake3) collides, returns
    /// `Idempotent`. Missing address book returns `AddressbookMissing` without
    /// touching anything.
    ///
    /// ⚠ **Not a production writer, and test-only** — twin of
    /// [`Self::place_caldav_event`]; see its docs for why `record_cid` is
    /// derived here rather than taken, and for the ROW-only caveat (a row
    /// seeded here serves an empty body until the caller appends the matching
    /// record). The sole production writer is `put_card_ciphertext_handler` →
    /// [`Self::replace_carddav_card_by_uid`].
    ///
    /// All SQL runs under a **single `conn.lock()` scope**, and the write
    /// sequence (card INSERT + addressbook-state bump) runs in one
    /// `conn.unchecked_transaction()` so a crash can't leave a card without its
    /// lockstep (ctag, highestmodseq) bump. The single shared modseq bump is
    /// applied to the address book state row exactly once iff a row is actually
    /// inserted.
    pub async fn place_carddav_card(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        uid_hash: &[u8],
        encrypted_body: &[u8],
        encrypted_index_hint: &[u8],
        timestamp: i64,
        ciphertext_size: u32,
        now: i64,
    ) -> Result<PlaceCarddavCardOutcome> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let uid_hash_owned = uid_hash.to_vec();
        let body_owned = encrypted_body.to_vec();
        let hint_owned = encrypted_index_hint.to_vec();
        let card_id = derive_carddav_card_id(&actor, timestamp, &body_owned);
        // The record identity this body+hint WOULD be filed under — same mint as
        // the real append. See `place_caldav_event`'s doc comment.
        let record_cid = carddav_record_cid(&body_owned, &hint_owned)
            .context("place_carddav_card: derive record_cid")?;
        let conn = self.conn.lock().await;

        // 1. Check the address book exists.
        let current_hms: Option<i64> = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("place_carddav_card: lookup addressbook")?;
        let current_hms = match current_hms {
            None => return Ok(PlaceCarddavCardOutcome::AddressbookMissing),
            Some(h) => h,
        };

        // 2. Idempotency: if card_id already exists, return its etag/modseq.
        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT etag, modseq FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND card_id = ?3",
                rusqlite::params![&actor[..], &addressbook_id[..], &card_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("place_carddav_card: check existing card_id")?;
        if let Some((etag, modseq)) = existing {
            return Ok(PlaceCarddavCardOutcome::Idempotent {
                card_id,
                etag,
                modseq,
            });
        }

        // 3. Allocate the new modseq and insert. The card INSERT and the
        //    addressbook-state bump are one transaction (all-or-nothing).
        let new_modseq = current_hms + 1;
        let etag = format_etag(new_modseq);
        let tx = conn
            .unchecked_transaction()
            .context("place_carddav_card: begin tx")?;
        tx.execute(
            "INSERT INTO bridge_carddav_cards \
             (actor_id, addressbook_id, card_id, uid_hash, \
              encrypted_index_hint, etag, modseq, ciphertext_size, internal_date, created_at, \
              record_cid) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                &actor[..],
                &addressbook_id[..],
                &card_id[..],
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
        .context("place_carddav_card: insert card")?;

        // 4. Bump address book state row's (ctag, highestmodseq) in lockstep.
        tx.execute(
            "UPDATE bridge_carddav_addressbooks \
             SET highestmodseq = ?1, ctag = ?1 \
             WHERE actor_id = ?2 AND addressbook_id = ?3",
            rusqlite::params![new_modseq, &actor[..], &addressbook_id[..]],
        )
        .context("place_carddav_card: bump addressbook state")?;
        tx.commit().context("place_carddav_card: commit")?;

        Ok(PlaceCarddavCardOutcome::Created {
            card_id,
            etag,
            modseq: new_modseq,
        })
    }

    /// PUT-by-UID: create on first call, replace + tombstone on subsequent
    /// calls (delete old card_id, insert new with bumped modseq).
    ///
    /// All SQL runs under a **single `conn.lock()` scope**, and each write arm
    /// (create / sidecar-only refine / real replace-and-tombstone) runs in one
    /// `conn.unchecked_transaction()` — the replace arm's tombstone-INSERT +
    /// old-DELETE + new-INSERT + bump commit all-or-nothing, so a crash can
    /// never leave a tombstone for a still-present card. A single shared modseq
    /// bump is applied per operation.
    ///
    /// `if_match`: when `Some(etag)`, compare against the prior row's etag
    /// (only meaningful when a prior row exists for this uid_hash); mismatch
    /// returns `PreconditionFailed` without touching anything. `None` means
    /// "unconditional".
    ///
    /// Idempotency: if the **new** body hashes to the same card_id as the
    /// **prior** row's card_id (same body bytes + same timestamp), this is
    /// a transport retry — return `Idempotent` with the existing etag/modseq
    /// and skip the bump.
    pub async fn replace_carddav_card_by_uid(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        uid_hash: &[u8],
        if_match: Option<&str>,
        // Derived by the caller (`derive_carddav_card_id` over the request body)
        // — the body never reaches this method (it rests only in the `__card`
        // segment), so this method cannot hash it. Twin of
        // `replace_caldav_event_by_uid`; see its docs for the full rationale.
        new_card_id: &[u8; 32],
        // The appended record's content-hash filing CID (from
        // `segments::card::ensure_in_segment`) — stored on the row
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
    ) -> Result<ReplaceCarddavCardOutcome> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("replace_carddav_card_by_uid: begin tx")?;
        let outcome = replace_carddav_card_by_uid_in(
            &tx,
            actor,
            addressbook_id,
            uid_hash,
            if_match,
            new_card_id,
            new_record_cid,
            new_encrypted_index_hint,
            new_encrypted_fauna_ext,
            timestamp,
            ciphertext_size,
            now,
        )?;
        tx.commit().context("replace_carddav_card_by_uid: commit")?;
        Ok(outcome)
    }

    /// DELETE-by-UID: remove the card row for `(actor, addressbook_id, uid_hash)`
    /// and write a tombstone. All SQL under a **single `conn.lock()` scope**;
    /// the tombstone-INSERT + row-DELETE + addressbook-state bump commit in one
    /// `conn.unchecked_transaction()`, so a crash can never leave a tombstone
    /// for a still-present card. One shared modseq bump per delete iff a row is
    /// actually removed.
    pub async fn delete_carddav_card_by_uid(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        uid_hash: &[u8],
        if_match: Option<&str>,
        now: i64,
    ) -> Result<DeleteCarddavCardOutcome> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let uid_hash_owned = uid_hash.to_vec();
        let if_match_owned: Option<String> = if_match.map(|s| s.to_string());
        let conn = self.conn.lock().await;

        // 1. Address book exists?
        let current_hms: Option<i64> = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("delete_carddav_card_by_uid: lookup addressbook")?;
        let current_hms = match current_hms {
            None => return Ok(DeleteCarddavCardOutcome::NotFound),
            Some(h) => h,
        };

        // 2. Look up the card row (record_cid included — the content record is
        // tombstoned by the STORED identity below).
        let row: Option<(Vec<u8>, String, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT card_id, etag, record_cid FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND uid_hash = ?3 \
                 ORDER BY modseq DESC LIMIT 1",
                rusqlite::params![&actor[..], &addressbook_id[..], &uid_hash_owned],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("delete_carddav_card_by_uid: lookup card")?;

        let (card_id_blob, etag, record_cid_blob) = match row {
            None => return Ok(DeleteCarddavCardOutcome::NotFound),
            Some(r) => r,
        };

        // 3. If if_match supplied, gate on it.
        if let Some(m) = if_match_owned
            && m != etag
        {
            return Ok(DeleteCarddavCardOutcome::PreconditionFailed { current_etag: etag });
        }

        let card_id: [u8; 32] = card_id_blob.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "delete_carddav_card_by_uid: card_id wrong length: {}",
                card_id_blob.len()
            )
        })?;

        // 4. Bump modseq, write tombstone, delete row, update addressbook
        //    state — all in one transaction (all-or-nothing).
        let new_modseq = current_hms + 1;
        let tx = conn
            .unchecked_transaction()
            .context("delete_carddav_card_by_uid: begin tx")?;
        tx.execute(
            "INSERT INTO bridge_carddav_expunged \
             (actor_id, addressbook_id, card_id, uid_hash, modseq, expunged_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                &actor[..],
                &addressbook_id[..],
                &card_id[..],
                &uid_hash_owned,
                new_modseq,
                now,
            ],
        )
        .context("delete_carddav_card_by_uid: insert tombstone")?;
        tx.execute(
            "DELETE FROM bridge_carddav_cards \
             WHERE actor_id = ?1 AND addressbook_id = ?2 AND card_id = ?3",
            rusqlite::params![&actor[..], &addressbook_id[..], &card_id[..]],
        )
        .context("delete_carddav_card_by_uid: delete row")?;

        // Row gone → unless another book's row shares it, the `__card` content
        // record is unreachable. Tombstone it — by the STORED record_cid — so
        // compaction reclaims the bytes; AFTER the DELETE, and inside the same
        // tx so the two commit together. A row without a stored cid cannot
        // exist post-cutover; if one shows up, leave its record to the orphan
        // reaper.
        match record_cid_blob
            .as_deref()
            .and_then(|b| <[u8; 36]>::try_from(b).ok())
            .and_then(|a| fauna_cbor::Cid::from_bytes(a).ok())
        {
            Some(cid) => {
                tombstone_card_record_if_unreferenced(&tx, &actor, &cid)
                    .context("delete_carddav_card_by_uid: tombstone content record")?;
            }
            None => tracing::warn!(
                card_id = %hex::encode(card_id),
                "deleted card row had no record_cid — leaving its record to the orphan reaper"
            ),
        }
        tx.execute(
            "UPDATE bridge_carddav_addressbooks \
             SET highestmodseq = ?1, ctag = ?1 \
             WHERE actor_id = ?2 AND addressbook_id = ?3",
            rusqlite::params![new_modseq, &actor[..], &addressbook_id[..]],
        )
        .context("delete_carddav_card_by_uid: bump addressbook state")?;
        tx.commit().context("delete_carddav_card_by_uid: commit")?;

        Ok(DeleteCarddavCardOutcome::Deleted {
            card_id,
            modseq: new_modseq,
        })
    }

    /// DELETE a whole address book and **cascade-delete all its cards**. The
    /// statements run under a **single `conn.lock()` scope** (serializing against
    /// any concurrent writer) and commit in one `conn.unchecked_transaction()`,
    /// so the cascade is **literally atomic** (all-or-nothing) — matching the
    /// rest of this store's multi-statement writes. A torn transaction leaves a
    /// fully consistent state either way: on rollback the book and every card it
    /// held remain; on commit both are gone — never cards orphaned without their
    /// book. The operation also stays **idempotent** — re-issuing the collection
    /// DELETE returns `NotFound` once the row is gone (step 1). So the *No
    /// client-causable unrecoverable nest state* invariant (`carddav-server.md`
    /// § Address-book collection model) holds by both atomicity and idempotency.
    /// (Landed the store-wide `unchecked_transaction()` family change, fixing a
    /// non-transactional cascading collection delete.)
    ///
    /// Writes **no per-card tombstones**: once the whole collection is gone a
    /// client re-syncing its URL gets 404 (the WebDAV "collection removed"
    /// signal), so no `sync-collection` ever runs against it and per-card
    /// `bridge_carddav_expunged` rows would be dead. Worse, they are keyed by
    /// `(actor, addressbook_id, …)`, so they would leak a false "expunged"
    /// history into a later book that re-uses the same `addressbook_id` — the
    /// lazy "Contacts" book always does (`blake3("contacts")[:32]`). For the
    /// same reason this also **clears the book's existing tombstones** (written
    /// by prior `delete_card`s) so a re-provisioned same-id book starts with a
    /// clean sync history. (Decision 1.)
    ///
    /// Idempotent: an absent book returns `NotFound` (mirror `delete_card`).
    pub async fn delete_carddav_addressbook(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
    ) -> Result<DeleteCarddavAddressbookOutcome> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let conn = self.conn.lock().await;

        // 1. Address book exists? Idempotent NotFound if not.
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("delete_carddav_addressbook: lookup addressbook")?;
        if exists.is_none() {
            return Ok(DeleteCarddavAddressbookOutcome::NotFound);
        }

        // Steps 2-4 commit in one transaction (all-or-nothing cascade).
        let tx = conn
            .unchecked_transaction()
            .context("delete_carddav_addressbook: begin tx")?;

        // 2. Cascade-delete the cards (row count → observability). Collect the
        //    STORED record cids FIRST — once the rows are gone there is nothing
        //    left to name their `__card` content records, and an un-tombstoned
        //    record is unreachable-but-immortal: every future compaction copies
        //    it forward. (A NULL record_cid cannot exist post-cutover; skip it
        //    with a warn — the orphan reaper is the backstop.)
        let doomed_record_cids: Vec<fauna_cbor::Cid> = {
            let mut stmt = tx
                .prepare(
                    "SELECT record_cid FROM bridge_carddav_cards \
                     WHERE actor_id = ?1 AND addressbook_id = ?2",
                )
                .context("delete_carddav_addressbook: prepare record_cid scan")?;
            let rows = stmt
                .query_map(rusqlite::params![&actor[..], &addressbook_id[..]], |r| {
                    r.get::<_, Option<Vec<u8>>>(0)
                })
                .context("delete_carddav_addressbook: scan record_cids")?;
            let mut out = Vec::new();
            for r in rows {
                let blob = r.context("delete_carddav_addressbook: read record_cid")?;
                match blob
                    .as_deref()
                    .and_then(|b| <[u8; 36]>::try_from(b).ok())
                    .and_then(|a| fauna_cbor::Cid::from_bytes(a).ok())
                {
                    Some(cid) => out.push(cid),
                    None => tracing::warn!(
                        "cascade-deleted card row had no record_cid — leaving its \
                         record to the orphan reaper"
                    ),
                }
            }
            out
        };

        let cards_deleted = tx
            .execute(
                "DELETE FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &addressbook_id[..]],
            )
            .context("delete_carddav_addressbook: cascade-delete cards")?;

        // Rows gone → tombstone each content record no other book's card still
        // references, inside the same tx.
        for cid in &doomed_record_cids {
            tombstone_card_record_if_unreferenced(&tx, &actor, cid)
                .context("delete_carddav_addressbook: tombstone content record")?;
        }

        // 3. Clear the book's deletion tombstones (decision 1 — no stale
        //    expunged history leaking into a re-provisioned same-id book).
        tx.execute(
            "DELETE FROM bridge_carddav_expunged \
             WHERE actor_id = ?1 AND addressbook_id = ?2",
            rusqlite::params![&actor[..], &addressbook_id[..]],
        )
        .context("delete_carddav_addressbook: clear tombstones")?;

        // 4. Delete the address-book row itself.
        tx.execute(
            "DELETE FROM bridge_carddav_addressbooks \
             WHERE actor_id = ?1 AND addressbook_id = ?2",
            rusqlite::params![&actor[..], &addressbook_id[..]],
        )
        .context("delete_carddav_addressbook: delete addressbook row")?;
        tx.commit().context("delete_carddav_addressbook: commit")?;

        Ok(DeleteCarddavAddressbookOutcome::Deleted {
            cards_deleted: cards_deleted as u32,
        })
    }

    /// Paginated query over `bridge_carddav_cards`.
    ///
    /// - `since_modseq`: when `Some`, restrict to `modseq > since_modseq`.
    /// - `after_card_id`: when `Some`, restrict to `card_id > since_card_id`
    ///   (lexicographic on BLOB) for pagination resume.
    /// - `limit`: caller passes `wire_limit + 1`; the returned `CardPage`
    ///   trims to `wire_limit` and reports `more = true` iff a `limit+1`th
    ///   row was returned.
    ///
    /// Empty (not an error) when the address book is absent.
    pub async fn query_carddav_cards(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        since_modseq: Option<i64>,
        after_card_id: Option<&[u8; 32]>,
        limit: u32,
    ) -> Result<CardPage> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let after_owned: Option<[u8; 32]> = after_card_id.copied();

        // Build SQL with the optional filters.
        let mut sql = String::from(
            "SELECT card_id, uid_hash, encrypted_index_hint, \
                    etag, modseq, ciphertext_size, internal_date, encrypted_fauna_ext, \
                    record_cid \
             FROM bridge_carddav_cards \
             WHERE actor_id = ?1 AND addressbook_id = ?2",
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
            sql.push_str(&format!(" AND card_id > ?{param_idx}"));
            after_param = Some(after);
            param_idx += 1;
        }
        sql.push_str(" ORDER BY card_id ASC");
        // Apply LIMIT only when > 0 — wire limit==0 → unbounded (handler may
        // explicitly pass req.limit+1 for pagination detection).
        let fetch_limit = limit;
        if fetch_limit > 0 {
            sql.push_str(&format!(" LIMIT {fetch_limit}"));
        }
        let _ = param_idx;

        let (cards, more) = super::bridge_dav_common::paged_dav_query(
            &self.conn,
            &sql,
            &actor,
            &addressbook_id,
            since_param,
            after_param,
            fetch_limit,
            map_card_row,
            "query_carddav_cards",
        )
        .await?;

        Ok(CardPage { cards, more })
    }

    /// Paginated sync query over `bridge_carddav_cards`, ordered by `modseq ASC`.
    ///
    /// Used exclusively by the `sync_addressbook_since` handler where pagination
    /// is by modseq window, not by card_id cursor.  Ordering by modseq
    /// guarantees that the cursor (`new_sync_token = last.modseq`) is always
    /// correct: the next call with `since_modseq = last.modseq` resumes from
    /// exactly the next un-returned card.
    ///
    /// - `since_modseq`: restrict to `modseq > since_modseq`.
    /// - `limit`: caller passes `wire_limit + 1`; the returned `CardPage`
    ///   trims to `wire_limit` and reports `more = true` iff a `limit+1`th row
    ///   was returned.  `limit = 0` → unbounded.
    ///
    /// Empty (not an error) when the address book is absent.
    pub async fn query_carddav_changes_since(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        since_modseq: i64,
        limit: u32,
    ) -> Result<CardPage> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;

        let mut sql = String::from(
            "SELECT card_id, uid_hash, encrypted_index_hint, \
                    etag, modseq, ciphertext_size, internal_date, encrypted_fauna_ext, \
                    record_cid \
             FROM bridge_carddav_cards \
             WHERE actor_id = ?1 AND addressbook_id = ?2 AND modseq > ?3 \
             ORDER BY modseq ASC",
        );
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare query_carddav_changes_since")?;
        let mut rows: Vec<CardRow> = stmt
            .query_map(
                rusqlite::params![&actor[..], &addressbook_id[..], since_modseq],
                map_card_row,
            )
            .context("query_carddav_changes_since")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect query_carddav_changes_since")?;

        // Pagination detection: caller passed `wire_limit + 1`. If we hit
        // the limit (rows.len() == limit) AND limit > 0, trim and report
        // `more`. If limit == 0 (unbounded), `more` is always false.
        let more = limit > 0 && rows.len() as u32 == limit;
        if more {
            rows.pop();
        }

        Ok(CardPage { cards: rows, more })
    }

    /// Return ascending-modseq tombstones from `bridge_carddav_expunged` with
    /// `modseq > since_modseq` for `(actor, addressbook_id)`.
    pub async fn query_carddav_expunged_since(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        since_modseq: i64,
    ) -> Result<Vec<ExpungedCardRow>> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT card_id, uid_hash, modseq FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND modseq > ?3 \
                 ORDER BY modseq ASC",
            )
            .context("prepare query_carddav_expunged_since")?;
        let rows = stmt
            .query_map(
                rusqlite::params![&actor[..], &addressbook_id[..], since_modseq],
                map_expunged_row,
            )
            .context("query_carddav_expunged_since")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect query_carddav_expunged_since")?;
        Ok(rows)
    }

    /// Returns `true` iff any tombstone newer than `since_modseq` was
    /// expunged strictly before `cutoff_ts` (epoch seconds) — i.e. the
    /// supplied sync-token predates the tombstone-retention window, so the
    /// set of deletions since that token can no longer be honestly
    /// enumerated. Drives the `SyncAddressbookSinceReply::Ok { stale: true }`
    /// past-retention signal (`carddav-server.md` § Stale sync-token
    /// handling); the MDA then emits `DAV:valid-sync-token` and the MUA
    /// full-resyncs. Pure read; the `(actor_id, addressbook_id, modseq)` index
    /// covers the `modseq >` range, then `expunged_at` is checked per row.
    pub async fn carddav_has_expunged_past_retention(
        &self,
        actor: &[u8; 32],
        addressbook_id: &[u8; 32],
        since_modseq: i64,
        cutoff_ts: i64,
    ) -> Result<bool> {
        let actor = *actor;
        let addressbook_id = *addressbook_id;
        let conn = self.conn.lock().await;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS( \
                     SELECT 1 FROM bridge_carddav_expunged \
                     WHERE actor_id = ?1 AND addressbook_id = ?2 \
                       AND modseq > ?3 AND expunged_at < ?4)",
                rusqlite::params![&actor[..], &addressbook_id[..], since_modseq, cutoff_ts],
                |row| row.get(0),
            )
            .context("carddav_has_expunged_past_retention")?;
        Ok(exists)
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

/// [`CacheDb::replace_carddav_card_by_uid`] on the caller's connection, so a caller can share
/// its transaction — the lived-in recovery (`crate::backup::recover`) files a
/// recovered record's row in the same transaction as its mirror row, as
/// [`super::bridge_imap::place_new_message_in`] is to mail's. Opens no
/// transaction of its own: every write arm's statements commit or roll back
/// with the caller's.
#[allow(clippy::too_many_arguments)]
pub(crate) fn replace_carddav_card_by_uid_in(
    conn: &rusqlite::Connection,
    actor: &[u8; 32],
    addressbook_id: &[u8; 32],
    uid_hash: &[u8],
    if_match: Option<&str>,
    // Derived by the caller (`derive_carddav_card_id` over the request body)
    // — the body never reaches this method (it rests only in the `__card`
    // segment), so this method cannot hash it. Twin of
    // `replace_caldav_event_by_uid`; see its docs for the full rationale.
    new_card_id: &[u8; 32],
    // The appended record's content-hash filing CID (from
    // `segments::card::ensure_in_segment`) — stored on the row
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
) -> Result<ReplaceCarddavCardOutcome> {
    let actor = *actor;
    let addressbook_id = *addressbook_id;
    let uid_hash_owned = uid_hash.to_vec();
    let hint_owned = new_encrypted_index_hint.to_vec();
    let new_fauna_ext_owned: Option<Vec<u8>> = new_encrypted_fauna_ext.map(|s| s.to_vec());
    let if_match_owned: Option<String> = if_match.map(|s| s.to_string());
    let new_card_id = *new_card_id;
    let new_record_cid = *new_record_cid;

    // 0. No-data-loss guard — twin of the calendar arm. The body rests ONLY
    // in the `__card` segment, so every row asserts the record under
    // `new_record_cid` is durable; refuse to record that claim unless the
    // mirror proves it, or a crash between this row and a later append would
    // strand the body nowhere and the client's retry would take the
    // idempotent path.
    if crate::segments::records_db::lookup_record(
        conn,
        &actor,
        crate::segments::card::KIND,
        &new_record_cid,
    )
    .context("replace_carddav_card_by_uid: mirror lookup for segment-record guard")?
    .is_none()
    {
        anyhow::bail!(
            "refusing to store card {}: no live \
             __card segment record — append the content record first",
            hex::encode(new_card_id),
        );
    }

    // 1. Confirm address book exists.
    let current_hms: Option<i64> = conn
        .query_row(
            "SELECT highestmodseq FROM bridge_carddav_addressbooks \
             WHERE actor_id = ?1 AND addressbook_id = ?2",
            rusqlite::params![&actor[..], &addressbook_id[..]],
            |row| row.get(0),
        )
        .optional()
        .context("replace_carddav_card_by_uid: lookup addressbook")?;
    let current_hms = match current_hms {
        None => return Ok(ReplaceCarddavCardOutcome::AddressbookMissing),
        Some(h) => h,
    };

    // 2. Look up the prior row (if any) for this (actor, addressbook, uid_hash).
    // If multiple exist, take the most recent by modseq DESC — but our
    // schema shouldn't produce duplicates; defensive ordering only. The
    // prior `encrypted_fauna_ext` is fetched so a MUA write (no sidecar)
    // can preserve it onto the replacement row.
    let prior: Option<(Vec<u8>, String, i64, Option<Vec<u8>>, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT card_id, etag, modseq, encrypted_fauna_ext, record_cid \
             FROM bridge_carddav_cards \
             WHERE actor_id = ?1 AND addressbook_id = ?2 AND uid_hash = ?3 \
             ORDER BY modseq DESC LIMIT 1",
            rusqlite::params![&actor[..], &addressbook_id[..], &uid_hash_owned],
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
        .context("replace_carddav_card_by_uid: lookup prior row")?;

    match prior {
        None => {
            // No prior row — fall through to a fresh insert. Mirrors
            // place_carddav_card but allocates a Created outcome.
            if if_match_owned.is_some() {
                // If the caller supplied if_match but no row exists, we
                // still go ahead and create — the standard MUA behavior
                // is "If-Match: *" on create (which our wire form treats
                // as None) or no If-Match. A specific etag against an
                // absent row is a Created on our side; the upper layer
                // can pre-check if it really means "must exist."
                //
                // (CardDAV PUT with If-Match against a missing resource
                // is server's choice per RFC 7232 § 3.1; we choose to
                // treat it as a create.)
            }

            // Idempotency: extremely unlikely for "no prior row" but
            // check anyway — if the deterministic card_id already
            // exists (e.g. inserted under a *different* uid_hash by an
            // earlier path), surface it as Idempotent.
            let existing: Option<(String, i64, Option<Vec<u8>>)> = conn
                .query_row(
                    "SELECT etag, modseq, encrypted_fauna_ext FROM bridge_carddav_cards \
                     WHERE actor_id = ?1 AND addressbook_id = ?2 AND card_id = ?3",
                    rusqlite::params![&actor[..], &addressbook_id[..], &new_card_id[..]],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .context("replace_carddav_card_by_uid: check card_id collision")?;
            if let Some((etag, modseq, encrypted_fauna_ext)) = existing {
                return Ok(ReplaceCarddavCardOutcome::Idempotent {
                    card_id: new_card_id,
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
                "INSERT INTO bridge_carddav_cards \
                 (actor_id, addressbook_id, card_id, uid_hash, \
                  encrypted_index_hint, etag, modseq, ciphertext_size, internal_date, created_at, \
                  encrypted_fauna_ext, record_cid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    &actor[..],
                    &addressbook_id[..],
                    &new_card_id[..],
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
            .context("replace_carddav_card_by_uid: insert (no prior)")?;
            conn.execute(
                "UPDATE bridge_carddav_addressbooks \
                 SET highestmodseq = ?1, ctag = ?1 \
                 WHERE actor_id = ?2 AND addressbook_id = ?3",
                rusqlite::params![new_modseq, &actor[..], &addressbook_id[..]],
            )
            .context("replace_carddav_card_by_uid: bump addressbook state (no prior)")?;
            Ok(ReplaceCarddavCardOutcome::Created {
                card_id: new_card_id,
                etag,
                modseq: new_modseq,
                encrypted_fauna_ext: new_fauna_ext_owned,
            })
        }
        Some((prior_card_id_blob, prior_etag, prior_modseq, prior_fauna_ext, prior_record_cid)) => {
            // 3a. If if_match supplied, gate on it.
            if let Some(ref m) = if_match_owned
                && m != &prior_etag
            {
                return Ok(ReplaceCarddavCardOutcome::PreconditionFailed {
                    current_etag: prior_etag,
                });
            }

            // Effective sidecar: a Fauna write (`Some`) replaces; a MUA
            // write (`None`) preserves the prior row's sidecar — so a
            // generic-client edit keeps the Fauna refinement attached
            // (carddav-server.md § Card resources, invariant 2).
            let effective_fauna_ext: Option<Vec<u8>> = new_fauna_ext_owned
                .clone()
                .or_else(|| prior_fauna_ext.clone());

            // 3b. Idempotency check: same card_id as before? Skip bump —
            // *unless* the effective sidecar changed (a Fauna write that
            // refines only the sidecar with a byte-identical vCard body +
            // timestamp; the body-derived card_id collides but the row
            // genuinely changed). A transport retry carries the same
            // sidecar, so it still collapses to Idempotent.
            let prior_card_id: [u8; 32] =
                prior_card_id_blob.as_slice().try_into().map_err(|_| {
                    anyhow::anyhow!(
                        "replace_carddav_card_by_uid: prior card_id wrong length: {}",
                        prior_card_id_blob.len()
                    )
                })?;
            if prior_card_id == new_card_id {
                if effective_fauna_ext == prior_fauna_ext {
                    return Ok(ReplaceCarddavCardOutcome::Idempotent {
                        card_id: new_card_id,
                        etag: prior_etag,
                        modseq: prior_modseq,
                        encrypted_fauna_ext: prior_fauna_ext,
                    });
                }
                // Sidecar-only change on an identical body: update the
                // sidecar in place + bump modseq/etag. No tombstone — the
                // card_id is unchanged, so this is a metadata refinement,
                // not a delete+re-add.
                let new_modseq = current_hms + 1;
                let etag = format_etag(new_modseq);
                conn.execute(
                    "UPDATE bridge_carddav_cards \
                     SET encrypted_fauna_ext = ?1, etag = ?2, modseq = ?3, internal_date = ?4 \
                     WHERE actor_id = ?5 AND addressbook_id = ?6 AND card_id = ?7",
                    rusqlite::params![
                        effective_fauna_ext.as_deref(),
                        &etag,
                        new_modseq,
                        timestamp,
                        &actor[..],
                        &addressbook_id[..],
                        &new_card_id[..],
                    ],
                )
                .context("replace_carddav_card_by_uid: update sidecar in place")?;
                conn.execute(
                    "UPDATE bridge_carddav_addressbooks \
                     SET highestmodseq = ?1, ctag = ?1 \
                     WHERE actor_id = ?2 AND addressbook_id = ?3",
                    rusqlite::params![new_modseq, &actor[..], &addressbook_id[..]],
                )
                .context("replace_carddav_card_by_uid: bump addressbook state (sidecar)")?;
                return Ok(ReplaceCarddavCardOutcome::Updated {
                    card_id: new_card_id,
                    etag,
                    modseq: new_modseq,
                    encrypted_fauna_ext: effective_fauna_ext,
                });
            }

            // 3c. Real update: bump modseq, write tombstone for old
            // card_id, delete old row, insert new row. All four writes
            // commit in one transaction, so no half-applied state (e.g. a
            // tombstone for a still-present card) is ever observable.
            let new_modseq = current_hms + 1;
            let etag = format_etag(new_modseq);

            conn.execute(
                "INSERT INTO bridge_carddav_expunged \
                 (actor_id, addressbook_id, card_id, uid_hash, modseq, expunged_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &actor[..],
                    &addressbook_id[..],
                    &prior_card_id[..],
                    &uid_hash_owned,
                    new_modseq,
                    now,
                ],
            )
            .context("replace_carddav_card_by_uid: insert tombstone")?;

            conn.execute(
                "DELETE FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND card_id = ?3",
                rusqlite::params![&actor[..], &addressbook_id[..], &prior_card_id[..]],
            )
            .context("replace_carddav_card_by_uid: delete prior row")?;

            conn.execute(
                "INSERT INTO bridge_carddav_cards \
                 (actor_id, addressbook_id, card_id, uid_hash, \
                  encrypted_index_hint, etag, modseq, ciphertext_size, internal_date, created_at, \
                  encrypted_fauna_ext, record_cid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    &actor[..],
                    &addressbook_id[..],
                    &new_card_id[..],
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
            .context("replace_carddav_card_by_uid: insert new row")?;

            // Tombstone the superseded content record if no row references
            // it any more, so compaction can reclaim it (see the calendar
            // twin for the full reasoning). AFTER the DELETE and the new
            // row's INSERT, so the reference count sees the final row set —
            // the new row may point at the SAME record.
            match prior_record_cid
                .as_deref()
                .and_then(|b| <[u8; 36]>::try_from(b).ok())
                .and_then(|a| fauna_cbor::Cid::from_bytes(a).ok())
            {
                Some(prior_cid) => {
                    tombstone_card_record_if_unreferenced(conn, &actor, &prior_cid)
                        .context("replace_carddav_card_by_uid: tombstone superseded record")?;
                }
                // A prior row without a stored cid cannot exist
                // post-cutover; leave its record to the orphan reaper.
                None => tracing::warn!(
                    card_id = %hex::encode(prior_card_id),
                    "superseded card row had no record_cid — leaving its \
                     record to the orphan reaper"
                ),
            }

            conn.execute(
                "UPDATE bridge_carddav_addressbooks \
                 SET highestmodseq = ?1, ctag = ?1 \
                 WHERE actor_id = ?2 AND addressbook_id = ?3",
                rusqlite::params![new_modseq, &actor[..], &addressbook_id[..]],
            )
            .context("replace_carddav_card_by_uid: bump addressbook state")?;

            Ok(ReplaceCarddavCardOutcome::Updated {
                card_id: new_card_id,
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
    use fauna_protocol::dav_identity::contacts_addressbook_id;

    impl CacheDb {
        /// Test shim — twin of `replace_caldav_event_by_uid_deriving`; see its
        /// docs for why the cid is derived rather than invented and why the
        /// record's mirror row is filed first.
        #[allow(clippy::too_many_arguments)]
        async fn replace_carddav_card_by_uid_deriving(
            &self,
            actor: &[u8; 32],
            addressbook_id: &[u8; 32],
            uid_hash: &[u8],
            if_match: Option<&str>,
            new_encrypted_body: &[u8],
            new_encrypted_index_hint: &[u8],
            new_encrypted_fauna_ext: Option<&[u8]>,
            timestamp: i64,
            ciphertext_size: u32,
            now: i64,
        ) -> Result<ReplaceCarddavCardOutcome> {
            let card_id = derive_carddav_card_id(actor, timestamp, new_encrypted_body);
            let record_cid = carddav_record_cid(new_encrypted_body, new_encrypted_index_hint)
                .expect("derive record_cid");
            ensure_card_mirror_record(self, actor, &record_cid).await;
            self.replace_carddav_card_by_uid(
                actor,
                addressbook_id,
                uid_hash,
                if_match,
                &card_id,
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

    /// Twin of `bridge_caldav`'s `ensure_cal_mirror_record`: file a live
    /// `__card` mirror row for `cid` unless one already exists — the
    /// precondition of `replace_carddav_card_by_uid`'s no-data-loss guard.
    async fn ensure_card_mirror_record(db: &CacheDb, actor: &[u8; 32], cid: &fauna_cbor::Cid) {
        if db
            .segment_records_lookup_record(actor, crate::segments::card::KIND, cid)
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
                rusqlite::params![&actor[..], crate::segments::card::KIND, &cid.as_bytes()[..]],
                |row| row.get(0),
            )
            .unwrap()
        };
        db.segment_records_insert_card(actor, 1 + prior_rows as u32, cid, "2023-11", 1_700_000_000)
            .await
            .unwrap();
    }

    fn make_actor(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn make_addressbook_id(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn make_uid_hash(byte: u8) -> Vec<u8> {
        vec![byte; 32]
    }

    // ── insert_bridge_carddav_addressbook ────────────────────────────────────

    #[tokio::test]
    async fn insert_bridge_carddav_addressbook_creates_first_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(1);
        let ab = make_addressbook_id(2);
        let outcome = db
            .insert_bridge_carddav_addressbook(&actor, &ab, b"meta-v1", 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(outcome, ProvisionOutcome::Created);

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_addressbooks WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn insert_bridge_carddav_addressbook_idempotent_on_identical_bytes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(3);
        let ab = make_addressbook_id(4);
        let first = db
            .insert_bridge_carddav_addressbook(&actor, &ab, b"meta-bytes", 1_700_000_000)
            .await
            .unwrap();
        let second = db
            .insert_bridge_carddav_addressbook(&actor, &ab, b"meta-bytes", 1_700_000_001)
            .await
            .unwrap();
        assert_eq!(first, ProvisionOutcome::Created);
        assert_eq!(second, ProvisionOutcome::AlreadyExists);

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_addressbooks WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "no second row");
    }

    #[tokio::test]
    async fn insert_bridge_carddav_addressbook_conflict_on_differing_bytes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(5);
        let ab = make_addressbook_id(6);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"meta-A", 1_700_000_000)
            .await
            .unwrap();
        let second = db
            .insert_bridge_carddav_addressbook(&actor, &ab, b"meta-B", 1_700_000_001)
            .await
            .unwrap();
        assert_eq!(second, ProvisionOutcome::Conflict);

        // Original metadata unchanged.
        let conn = db.conn().await;
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_metadata FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, b"meta-A");
    }

    // ── update_bridge_carddav_addressbook_metadata ───────────────────────────

    #[tokio::test]
    async fn update_bridge_carddav_addressbook_metadata_overwrites_and_bumps_modseq() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(40);
        let ab = make_addressbook_id(41);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"meta-v1", 1_700_000_000)
            .await
            .unwrap();
        let hms_before = db
            .carddav_addressbook_highestmodseq(&actor, &ab)
            .await
            .unwrap()
            .unwrap();

        let outcome = db
            .update_bridge_carddav_addressbook_metadata(&actor, &ab, b"meta-v2")
            .await
            .unwrap();
        assert_eq!(outcome, ProvisionOutcome::Updated);

        let rows = db.list_bridge_carddav_addressbooks(&actor).await.unwrap();
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
    async fn update_bridge_carddav_addressbook_metadata_returns_not_found_when_missing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(42);
        let ab = make_addressbook_id(43);
        // No prior insert.
        let outcome = db
            .update_bridge_carddav_addressbook_metadata(&actor, &ab, b"meta-v1")
            .await
            .unwrap();
        assert_eq!(outcome, ProvisionOutcome::NotFound);

        // Update must not create a row.
        let rows = db.list_bridge_carddav_addressbooks(&actor).await.unwrap();
        assert!(rows.is_empty(), "NotFound path must not create rows");
    }

    #[tokio::test]
    async fn update_bridge_carddav_addressbook_metadata_bumps_modseq_each_distinct_call() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(44);
        let ab = make_addressbook_id(45);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"meta-v1", 1_700_000_000)
            .await
            .unwrap();
        let hms0 = db
            .carddav_addressbook_highestmodseq(&actor, &ab)
            .await
            .unwrap()
            .unwrap();
        db.update_bridge_carddav_addressbook_metadata(&actor, &ab, b"meta-v2")
            .await
            .unwrap();
        let hms1 = db
            .carddav_addressbook_highestmodseq(&actor, &ab)
            .await
            .unwrap()
            .unwrap();
        db.update_bridge_carddav_addressbook_metadata(&actor, &ab, b"meta-v3")
            .await
            .unwrap();
        let hms2 = db
            .carddav_addressbook_highestmodseq(&actor, &ab)
            .await
            .unwrap()
            .unwrap();
        assert!(hms1 > hms0, "first update bumps");
        assert!(hms2 > hms1, "second update bumps");
    }

    #[tokio::test]
    async fn update_bridge_carddav_addressbook_metadata_is_idempotent_on_byte_identical_metadata() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(46);
        let ab = make_addressbook_id(47);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"meta-v1", 1_700_000_000)
            .await
            .unwrap();
        let outcome_first = db
            .update_bridge_carddav_addressbook_metadata(&actor, &ab, b"meta-v2")
            .await
            .unwrap();
        assert_eq!(outcome_first, ProvisionOutcome::Updated);
        let hms_after_first = db
            .carddav_addressbook_highestmodseq(&actor, &ab)
            .await
            .unwrap()
            .unwrap();

        // Byte-identical retry: same outcome from the caller's POV, but no
        // spurious modseq bump (would mislead sync-collection clients).
        let outcome_retry = db
            .update_bridge_carddav_addressbook_metadata(&actor, &ab, b"meta-v2")
            .await
            .unwrap();
        assert_eq!(outcome_retry, ProvisionOutcome::Updated);
        let hms_after_retry = db
            .carddav_addressbook_highestmodseq(&actor, &ab)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            hms_after_retry, hms_after_first,
            "byte-identical retry must not bump highestmodseq"
        );
    }

    #[tokio::test]
    async fn ensure_bridge_carddav_addressbook_exists_reflects_presence() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(7);
        let ab = make_addressbook_id(8);
        let other = make_addressbook_id(9);

        assert!(
            !db.ensure_bridge_carddav_addressbook_exists(&actor, &ab)
                .await
                .unwrap()
        );

        db.insert_bridge_carddav_addressbook(&actor, &ab, b"x", 1_700_000_000)
            .await
            .unwrap();

        assert!(
            db.ensure_bridge_carddav_addressbook_exists(&actor, &ab)
                .await
                .unwrap()
        );
        assert!(
            !db.ensure_bridge_carddav_addressbook_exists(&actor, &other)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn list_bridge_carddav_addressbooks_empty_when_none() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(10);
        let rows = db.list_bridge_carddav_addressbooks(&actor).await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn list_bridge_carddav_addressbooks_returns_provisioned_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(11);
        let ab_a = make_addressbook_id(12);
        let ab_b = make_addressbook_id(13);
        db.insert_bridge_carddav_addressbook(&actor, &ab_a, b"A", 1_700_000_000)
            .await
            .unwrap();
        db.insert_bridge_carddav_addressbook(&actor, &ab_b, b"B", 1_700_000_001)
            .await
            .unwrap();
        let rows = db.list_bridge_carddav_addressbooks(&actor).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].addressbook_id, ab_a);
        assert_eq!(rows[0].encrypted_metadata, b"A");
        assert_eq!(rows[0].ctag, 0, "fresh address book has ctag=0");
        assert_eq!(rows[0].highestmodseq, 1, "fresh address book has hms=1");
        assert_eq!(rows[1].addressbook_id, ab_b);
        assert_eq!(rows[1].encrypted_metadata, b"B");
    }

    // ── carddav_addressbook_highestmodseq ────────────────────────────────────

    #[tokio::test]
    async fn carddav_addressbook_highestmodseq_returns_none_when_absent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(14);
        let ab = make_addressbook_id(15);
        let hms = db
            .carddav_addressbook_highestmodseq(&actor, &ab)
            .await
            .unwrap();
        assert_eq!(hms, None);
    }

    #[tokio::test]
    async fn carddav_addressbook_highestmodseq_returns_some_when_present() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(16);
        let ab = make_addressbook_id(17);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let hms = db
            .carddav_addressbook_highestmodseq(&actor, &ab)
            .await
            .unwrap();
        assert_eq!(hms, Some(1));
    }

    // ── place_carddav_card ───────────────────────────────────────────────────

    #[tokio::test]
    async fn place_carddav_card_allocates_sequential_modseqs() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(20);
        let ab = make_addressbook_id(21);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"meta", 1_700_000_000)
            .await
            .unwrap();

        let uid_a = make_uid_hash(30);
        let uid_b = make_uid_hash(31);
        let o1 = db
            .place_carddav_card(
                &actor,
                &ab,
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
            .place_carddav_card(
                &actor,
                &ab,
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
                PlaceCarddavCardOutcome::Created {
                    modseq: m1,
                    etag: e1,
                    ..
                },
                PlaceCarddavCardOutcome::Created {
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
                "SELECT highestmodseq, ctag FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(hms, 3);
        assert_eq!(ctag, hms, "ctag bumps in lockstep with highestmodseq");
    }

    #[tokio::test]
    async fn place_carddav_card_isolated_per_addressbook() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(40);
        let ab_a = make_addressbook_id(41);
        let ab_b = make_addressbook_id(42);
        db.insert_bridge_carddav_addressbook(&actor, &ab_a, b"A", 1_700_000_000)
            .await
            .unwrap();
        db.insert_bridge_carddav_addressbook(&actor, &ab_b, b"B", 1_700_000_001)
            .await
            .unwrap();

        let uid = make_uid_hash(50);
        let o_a = db
            .place_carddav_card(
                &actor,
                &ab_a,
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
            .place_carddav_card(
                &actor,
                &ab_b,
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
                PlaceCarddavCardOutcome::Created { modseq: m_a, .. },
                PlaceCarddavCardOutcome::Created { modseq: m_b, .. },
            ) => {
                assert_eq!(m_a, 2);
                assert_eq!(m_b, 2, "each address book has its own modseq counter");
            }
            other => panic!("expected two Created; got {:?}", other),
        }
    }

    #[tokio::test]
    async fn place_carddav_card_idempotent_on_identical_body() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(60);
        let ab = make_addressbook_id(61);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"meta", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(70);
        let first = db
            .place_carddav_card(
                &actor,
                &ab,
                &uid,
                b"same-body",
                b"hint",
                1_700_000_000,
                9,
                1_700_000_100,
            )
            .await
            .unwrap();
        let (first_card_id, first_modseq, first_etag) = match first {
            PlaceCarddavCardOutcome::Created {
                card_id,
                modseq,
                etag,
            } => (card_id, modseq, etag),
            other => panic!("expected Created; got {:?}", other),
        };

        let retry = db
            .place_carddav_card(
                &actor,
                &ab,
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
            PlaceCarddavCardOutcome::Idempotent {
                card_id,
                modseq,
                etag,
            } => {
                assert_eq!(card_id, first_card_id);
                assert_eq!(modseq, first_modseq, "modseq must NOT bump on retry");
                assert_eq!(etag, first_etag);
            }
            other => panic!("expected Idempotent; got {:?}", other),
        }

        // Confirm only one card row.
        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn place_carddav_card_missing_addressbook_returns_addressbook_missing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(80);
        let ab = make_addressbook_id(81);
        let uid = make_uid_hash(90);
        let o = db
            .place_carddav_card(
                &actor,
                &ab,
                &uid,
                b"body",
                b"hint",
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .unwrap();
        assert_eq!(o, PlaceCarddavCardOutcome::AddressbookMissing);

        // No card row, no address book row.
        let conn = db.conn().await;
        let card_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        let ab_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_addressbooks WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(card_count, 0);
        assert_eq!(ab_count, 0);
    }

    // ── replace_carddav_card_by_uid ──────────────────────────────────────────

    #[tokio::test]
    async fn replace_carddav_card_by_uid_creates_when_absent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(100);
        let ab = make_addressbook_id(101);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(110);
        let o = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
            ReplaceCarddavCardOutcome::Created { modseq, etag, .. } => {
                assert_eq!(modseq, 2);
                assert_eq!(etag, format_etag(2));
            }
            other => panic!("expected Created; got {:?}", other),
        }
    }

    #[tokio::test]
    async fn replace_carddav_card_by_uid_updates_and_tombstones_existing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(120);
        let ab = make_addressbook_id(121);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(130);

        let first = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
        let (first_card_id, first_modseq) = match first {
            ReplaceCarddavCardOutcome::Created {
                card_id, modseq, ..
            } => (card_id, modseq),
            other => panic!("expected Created; got {:?}", other),
        };

        let second = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
            ReplaceCarddavCardOutcome::Updated {
                card_id, modseq, ..
            } => {
                assert_ne!(card_id, first_card_id, "new card_id on update");
                assert_eq!(modseq, first_modseq + 1, "modseq bumped exactly once");
            }
            other => panic!("expected Updated; got {:?}", other),
        }

        // Tombstone for the old card_id exists.
        let conn = db.conn().await;
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND card_id = ?3",
                rusqlite::params![&actor[..], &ab[..], &first_card_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tombstone_count, 1);

        // Exactly one live row for this uid_hash.
        let live_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &ab[..], &uid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live_count, 1);
    }

    #[tokio::test]
    async fn replace_carddav_card_by_uid_if_match_mismatch_returns_precondition_failed() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(140);
        let ab = make_addressbook_id(141);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(150);

        let first = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
            ReplaceCarddavCardOutcome::Created { etag, .. } => etag,
            other => panic!("expected Created; got {:?}", other),
        };

        // Wrong if_match.
        let bad = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
            ReplaceCarddavCardOutcome::PreconditionFailed { current_etag } => {
                assert_eq!(current_etag, first_etag);
            }
            other => panic!("expected PreconditionFailed; got {:?}", other),
        }

        // Correct if_match succeeds.
        let good = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
        assert!(matches!(good, ReplaceCarddavCardOutcome::Updated { .. }));
    }

    #[tokio::test]
    async fn replace_carddav_card_by_uid_addressbook_missing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(160);
        let ab = make_addressbook_id(161);
        let uid = make_uid_hash(170);
        let o = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
        assert_eq!(o, ReplaceCarddavCardOutcome::AddressbookMissing);
    }

    #[tokio::test]
    async fn replace_carddav_card_by_uid_sidecar_preserve_replace_and_in_place() {
        // carddav-server.md § Card resources, sidecar invariant 2: a MUA write
        // (no sidecar) on UPDATE preserves the prior `encrypted_fauna_ext`;
        // only a Fauna write (Some) replaces both halves.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(190);
        let ab = make_addressbook_id(191);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(195);

        // Helper closures are awkward across `&self` futures; query inline.
        // 1. Fauna write: body-v1 + sidecar S1.
        db.replace_carddav_card_by_uid_deriving(
            &actor,
            &ab,
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
            .query_carddav_cards(&actor, &ab, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 1);
        assert_eq!(
            page.cards[0].encrypted_fauna_ext.as_deref(),
            Some(&b"sidecar-v1"[..])
        );
        assert_eq!(
            page.cards[0].record_cid().unwrap(),
            Some(carddav_record_cid(b"body-v1", b"hint").unwrap())
        );

        // 2. MUA write: body-v2, NO sidecar (None) → preserves S1, replaces body.
        db.replace_carddav_card_by_uid_deriving(
            &actor,
            &ab,
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
            .query_carddav_cards(&actor, &ab, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 1);
        assert_eq!(
            page.cards[0].encrypted_fauna_ext.as_deref(),
            Some(&b"sidecar-v1"[..]),
            "MUA write preserves prior sidecar"
        );
        assert_eq!(
            page.cards[0].record_cid().unwrap(),
            Some(carddav_record_cid(b"body-v2", b"hint").unwrap()),
            "MUA write replaced body"
        );

        // 3. Fauna write: body-v3 + sidecar S2 → replaces both halves.
        db.replace_carddav_card_by_uid_deriving(
            &actor,
            &ab,
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
            .query_carddav_cards(&actor, &ab, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 1);
        assert_eq!(
            page.cards[0].encrypted_fauna_ext.as_deref(),
            Some(&b"sidecar-v2"[..]),
            "Fauna write replaces sidecar"
        );
        assert_eq!(
            page.cards[0].record_cid().unwrap(),
            Some(carddav_record_cid(b"body-v3", b"hint").unwrap())
        );
        let modseq_v3 = page.cards[0].modseq;

        // 4. Sidecar-only change (identical body + timestamp ⇒ same card_id):
        // in-place sidecar update + modseq bump, still exactly one live row.
        let o = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
        assert!(matches!(o, ReplaceCarddavCardOutcome::Updated { .. }));
        let page = db
            .query_carddav_cards(&actor, &ab, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.cards.len(),
            1,
            "still one live row after sidecar-only change"
        );
        assert_eq!(
            page.cards[0].encrypted_fauna_ext.as_deref(),
            Some(&b"sidecar-v3"[..])
        );
        let modseq_v4 = page.cards[0].modseq;
        assert_eq!(
            modseq_v4,
            modseq_v3 + 1,
            "sidecar-only change bumps modseq once"
        );

        // 5. Exact retry (same body + ts + sidecar) ⇒ Idempotent, no bump.
        let o = db
            .replace_carddav_card_by_uid_deriving(
                &actor,
                &ab,
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
            matches!(o, ReplaceCarddavCardOutcome::Idempotent { .. }),
            "byte-identical retry is idempotent"
        );
        let page = db
            .query_carddav_cards(&actor, &ab, None, None, 0)
            .await
            .unwrap();
        assert_eq!(
            page.cards[0].modseq, modseq_v4,
            "idempotent retry does not bump"
        );
    }

    // ── delete_carddav_card_by_uid ───────────────────────────────────────────

    #[tokio::test]
    async fn delete_carddav_card_by_uid_succeeds_and_tombstones() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(180);
        let ab = make_addressbook_id(181);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(190);
        let placed = db
            .place_carddav_card(
                &actor,
                &ab,
                &uid,
                b"body",
                b"hint",
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .unwrap();
        let placed_card_id = match placed {
            PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("expected Created; got {:?}", other),
        };

        let o = db
            .delete_carddav_card_by_uid(&actor, &ab, &uid, None, 1_700_000_200)
            .await
            .unwrap();
        match o {
            DeleteCarddavCardOutcome::Deleted { card_id, modseq } => {
                assert_eq!(card_id, placed_card_id);
                assert_eq!(modseq, 3, "place bumped to 2, delete bumps to 3");
            }
            other => panic!("expected Deleted; got {:?}", other),
        }

        // Card row gone, tombstone present.
        let conn = db.conn().await;
        let card_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND card_id = ?3",
                rusqlite::params![&actor[..], &ab[..], &placed_card_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(card_count, 0);
        assert_eq!(tombstone_count, 1);
    }

    #[tokio::test]
    async fn delete_carddav_card_by_uid_missing_addressbook_returns_not_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(200);
        let ab = make_addressbook_id(201);
        let uid = make_uid_hash(210);
        let o = db
            .delete_carddav_card_by_uid(&actor, &ab, &uid, None, 1_700_000_100)
            .await
            .unwrap();
        assert_eq!(o, DeleteCarddavCardOutcome::NotFound);
    }

    #[tokio::test]
    async fn delete_carddav_card_by_uid_missing_card_returns_not_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(220);
        let ab = make_addressbook_id(221);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(230);
        let o = db
            .delete_carddav_card_by_uid(&actor, &ab, &uid, None, 1_700_000_100)
            .await
            .unwrap();
        assert_eq!(o, DeleteCarddavCardOutcome::NotFound);
    }

    #[tokio::test]
    async fn delete_carddav_card_by_uid_if_match_mismatch_returns_precondition_failed() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(240);
        let ab = make_addressbook_id(241);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(250);
        let placed = db
            .place_carddav_card(
                &actor,
                &ab,
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
            PlaceCarddavCardOutcome::Created { etag, .. } => etag,
            other => panic!("expected Created; got {:?}", other),
        };

        let bad = db
            .delete_carddav_card_by_uid(&actor, &ab, &uid, Some("ffffffffffffffff"), 1_700_000_200)
            .await
            .unwrap();
        match bad {
            DeleteCarddavCardOutcome::PreconditionFailed { current_etag } => {
                assert_eq!(current_etag, placed_etag);
            }
            other => panic!("expected PreconditionFailed; got {:?}", other),
        }

        // Card still present after a failed if_match.
        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND uid_hash = ?3",
                rusqlite::params![&actor[..], &ab[..], &uid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // ── delete_carddav_addressbook ───────────────────────────────────────────

    #[tokio::test]
    async fn delete_carddav_addressbook_cascades_cards_and_returns_count() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(50);
        let ab = make_addressbook_id(51);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        // Three distinct (uid, body) pairs → three card rows.
        for (i, body) in [b"body-1", b"body-2", b"body-3"].iter().enumerate() {
            db.place_carddav_card(
                &actor,
                &ab,
                &make_uid_hash(60 + i as u8),
                *body,
                b"hint",
                1_700_000_000 + i as i64,
                body.len() as u32,
                1_700_000_100,
            )
            .await
            .unwrap();
        }

        let o = db.delete_carddav_addressbook(&actor, &ab).await.unwrap();
        assert_eq!(
            o,
            DeleteCarddavAddressbookOutcome::Deleted { cards_deleted: 3 }
        );

        // Book row gone, all card rows gone, and NO per-card tombstones written
        // (decision 1: a whole-book delete writes no expunged rows).
        let conn = db.conn().await;
        let ab_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        let card_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ab_count, 0, "address-book row deleted");
        assert_eq!(card_count, 0, "all cards cascade-deleted");
        assert_eq!(tombstone_count, 0, "no per-card tombstones written");
    }

    #[tokio::test]
    async fn delete_carddav_addressbook_empty_book_returns_zero() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(52);
        let ab = make_addressbook_id(53);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let o = db.delete_carddav_addressbook(&actor, &ab).await.unwrap();
        assert_eq!(
            o,
            DeleteCarddavAddressbookOutcome::Deleted { cards_deleted: 0 }
        );
    }

    #[tokio::test]
    async fn delete_carddav_addressbook_missing_returns_not_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(54);
        let ab = make_addressbook_id(55);
        let o = db.delete_carddav_addressbook(&actor, &ab).await.unwrap();
        assert_eq!(o, DeleteCarddavAddressbookOutcome::NotFound);
    }

    #[tokio::test]
    async fn delete_carddav_addressbook_idempotent_second_call_not_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(56);
        let ab = make_addressbook_id(57);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let first = db.delete_carddav_addressbook(&actor, &ab).await.unwrap();
        assert_eq!(
            first,
            DeleteCarddavAddressbookOutcome::Deleted { cards_deleted: 0 }
        );
        let second = db.delete_carddav_addressbook(&actor, &ab).await.unwrap();
        assert_eq!(second, DeleteCarddavAddressbookOutcome::NotFound);
    }

    #[tokio::test]
    async fn delete_carddav_addressbook_clears_prior_tombstones() {
        // A prior card-level delete leaves a tombstone; deleting the whole book
        // must clear it so a re-provisioned same-id book has a clean sync history
        // (decision 1: no stale expunged rows leaking across re-provision).
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(58);
        let ab = make_addressbook_id(59);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(70);
        db.place_carddav_card(
            &actor,
            &ab,
            &uid,
            b"body",
            b"hint",
            1_700_000_000,
            4,
            1_700_000_100,
        )
        .await
        .unwrap();
        // Card-level delete → writes a tombstone.
        db.delete_carddav_card_by_uid(&actor, &ab, &uid, None, 1_700_000_200)
            .await
            .unwrap();
        let conn = db.conn().await;
        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, 1, "card-level delete left a tombstone");
        drop(conn);

        db.delete_carddav_addressbook(&actor, &ab).await.unwrap();

        let conn = db.conn().await;
        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 0, "book delete cleared the prior tombstone");
    }

    #[tokio::test]
    async fn delete_carddav_addressbook_isolated_per_book() {
        // Deleting one book leaves a sibling book's cards untouched.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(60);
        let ab_a = make_addressbook_id(61);
        let ab_b = make_addressbook_id(62);
        for ab in [&ab_a, &ab_b] {
            db.insert_bridge_carddav_addressbook(&actor, ab, b"m", 1_700_000_000)
                .await
                .unwrap();
            db.place_carddav_card(
                &actor,
                ab,
                &make_uid_hash(80),
                b"body",
                b"hint",
                1_700_000_000,
                4,
                1_700_000_100,
            )
            .await
            .unwrap();
        }

        let o = db.delete_carddav_addressbook(&actor, &ab_a).await.unwrap();
        assert_eq!(
            o,
            DeleteCarddavAddressbookOutcome::Deleted { cards_deleted: 1 }
        );

        // Book B untouched.
        let conn = db.conn().await;
        let b_book: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab_b[..]],
                |row| row.get(0),
            )
            .unwrap();
        let b_cards: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab_b[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(b_book, 1, "sibling book row survives");
        assert_eq!(b_cards, 1, "sibling book cards survive");
    }

    // ── query_carddav_cards ─────────────────────────────────────────────────

    #[tokio::test]
    async fn query_carddav_cards_empty_when_addressbook_absent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa0);
        let ab = make_addressbook_id(0xa1);
        let page = db
            .query_carddav_cards(&actor, &ab, None, None, 10)
            .await
            .unwrap();
        assert!(page.cards.is_empty());
        assert!(!page.more);
    }

    #[tokio::test]
    async fn query_carddav_cards_returns_all_when_no_filter() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa2);
        let ab = make_addressbook_id(0xa3);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        // Three distinct bodies → three distinct card_ids.
        for (i, body) in [b"body-a", b"body-b", b"body-c"].iter().enumerate() {
            db.place_carddav_card(
                &actor,
                &ab,
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
            .query_carddav_cards(&actor, &ab, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 3);
        assert!(!page.more);
    }

    #[tokio::test]
    async fn query_carddav_cards_pagination_with_limit_plus_one() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa4);
        let ab = make_addressbook_id(0xa5);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        for i in 0..3 {
            db.place_carddav_card(
                &actor,
                &ab,
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
            .query_carddav_cards(&actor, &ab, None, None, 3)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 2, "trimmed to wire_limit");
        assert!(page.more, "more flag set");

        // Resume from the last returned card_id.
        let last = page.cards.last().unwrap().card_id;
        let page2 = db
            .query_carddav_cards(&actor, &ab, None, Some(&last), 3)
            .await
            .unwrap();
        assert_eq!(page2.cards.len(), 1, "third card returned");
        assert!(!page2.more);
    }

    #[tokio::test]
    async fn query_carddav_cards_since_modseq_filters() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa6);
        let ab = make_addressbook_id(0xa7);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let o1 = db
            .place_carddav_card(
                &actor,
                &ab,
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
            PlaceCarddavCardOutcome::Created { modseq, .. } => modseq,
            _ => panic!(),
        };
        db.place_carddav_card(
            &actor,
            &ab,
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
            .query_carddav_cards(&actor, &ab, Some(modseq_1), None, 0)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 1, "only the second card");
        assert_eq!(page.cards[0].modseq, modseq_1 + 1);
    }

    // ── query_carddav_expunged_since ────────────────────────────────────────

    #[tokio::test]
    async fn query_carddav_expunged_since_filters_and_orders() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xa8);
        let ab = make_addressbook_id(0xa9);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        // Place + delete twice with distinct uid_hashes.
        let uid_a = make_uid_hash(10);
        let uid_b = make_uid_hash(11);
        db.place_carddav_card(
            &actor,
            &ab,
            &uid_a,
            b"body-a",
            b"hint",
            1_700_000_000,
            6,
            1_700_000_100,
        )
        .await
        .unwrap();
        db.place_carddav_card(
            &actor,
            &ab,
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
            .delete_carddav_card_by_uid(&actor, &ab, &uid_a, None, 1_700_000_200)
            .await
            .unwrap();
        let modseq_d1 = match d1 {
            DeleteCarddavCardOutcome::Deleted { modseq, .. } => modseq,
            _ => panic!(),
        };
        db.delete_carddav_card_by_uid(&actor, &ab, &uid_b, None, 1_700_000_201)
            .await
            .unwrap();

        // since_modseq=0 → both tombstones, ascending modseq.
        let all = db
            .query_carddav_expunged_since(&actor, &ab, 0)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].modseq < all[1].modseq, "ascending modseq");
        assert_eq!(all[0].uid_hash, uid_a);
        assert_eq!(all[1].uid_hash, uid_b);

        // since_modseq=modseq_d1 → only the second tombstone.
        let some = db
            .query_carddav_expunged_since(&actor, &ab, modseq_d1)
            .await
            .unwrap();
        assert_eq!(some.len(), 1);
        assert_eq!(some[0].uid_hash, uid_b);
    }

    #[tokio::test]
    async fn carddav_has_expunged_past_retention_detects_only_aged_tombstones() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xc8);
        let ab = make_addressbook_id(0xc9);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(20);
        db.place_carddav_card(
            &actor,
            &ab,
            &uid,
            b"body",
            b"hint",
            1_700_000_000,
            4,
            1_700_000_100,
        )
        .await
        .unwrap();
        // Expunge the card at expunged_at = 1_700_001_000 (epoch seconds).
        let d = db
            .delete_carddav_card_by_uid(&actor, &ab, &uid, None, 1_700_001_000)
            .await
            .unwrap();
        let modseq = match d {
            DeleteCarddavCardOutcome::Deleted { modseq, .. } => modseq,
            _ => panic!("expected Deleted"),
        };

        // cutoff after the tombstone's expunged_at → the tombstone is past
        // retention (aged out): a token at/below its modseq is stale.
        assert!(
            db.carddav_has_expunged_past_retention(&actor, &ab, 0, 1_700_002_000)
                .await
                .unwrap(),
            "tombstone expunged_at 1_700_001_000 < cutoff 1_700_002_000 → past retention"
        );

        // cutoff before the tombstone's expunged_at → still within the
        // retention window: not stale.
        assert!(
            !db.carddav_has_expunged_past_retention(&actor, &ab, 0, 1_700_000_500)
                .await
                .unwrap(),
            "tombstone is newer than the cutoff → still within retention"
        );

        // since_modseq at the tombstone's modseq → no tombstone is strictly
        // newer, so nothing is missed regardless of age: not stale.
        assert!(
            !db.carddav_has_expunged_past_retention(&actor, &ab, modseq, 1_700_002_000)
                .await
                .unwrap(),
            "no tombstone with modseq > since_modseq → not stale"
        );
    }

    // ── query_carddav_changes_since ─────────────────────────────────────────

    #[tokio::test]
    async fn query_carddav_changes_since_ascending_modseq_order_and_more_flag() {
        // Verifies:
        //   1. Cards are returned in modseq ASC order (not card_id ASC).
        //   2. `more == true` when `limit == cards_count` (i.e. limit+1 probe hit).
        //   3. The trimmed card is the one with the LARGEST modseq.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(0xb0);
        let ab = make_addressbook_id(0xb1);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();

        // Place 3 cards (modseqs 2, 3, 4 assigned in creation order).
        let uid_a = make_uid_hash(0xc0);
        let uid_b = make_uid_hash(0xc1);
        let uid_c = make_uid_hash(0xc2);
        let o1 = db
            .place_carddav_card(
                &actor,
                &ab,
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
            .place_carddav_card(
                &actor,
                &ab,
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
            .place_carddav_card(
                &actor,
                &ab,
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
            PlaceCarddavCardOutcome::Created { modseq, .. } => modseq,
            _ => panic!(),
        };
        let modseq_b = match o2 {
            PlaceCarddavCardOutcome::Created { modseq, .. } => modseq,
            _ => panic!(),
        };
        let modseq_c = match o3 {
            PlaceCarddavCardOutcome::Created { modseq, .. } => modseq,
            _ => panic!(),
        };
        assert!(
            modseq_a < modseq_b && modseq_b < modseq_c,
            "sequential modseqs"
        );

        // limit=3 (wire_limit=2 + 1): should return first 2 cards, more=true.
        let page = db
            .query_carddav_changes_since(&actor, &ab, 0, 3)
            .await
            .unwrap();
        assert_eq!(page.cards.len(), 2, "trimmed to wire_limit=2");
        assert!(
            page.more,
            "more must be true when limit+1 rows were available"
        );
        // Cards are in modseq ASC order.
        assert_eq!(
            page.cards[0].modseq, modseq_a,
            "first card has smallest modseq"
        );
        assert_eq!(
            page.cards[1].modseq, modseq_b,
            "second card has middle modseq"
        );
        // The card with the LARGEST modseq (modseq_c) was trimmed.
        assert!(
            !page.cards.iter().any(|c| c.modseq == modseq_c),
            "card with largest modseq must be the one trimmed, not returned"
        );

        // Fetching from modseq_b with limit=3 (wire_limit=2+1): only 1 remains, more=false.
        let page2 = db
            .query_carddav_changes_since(&actor, &ab, modseq_b, 3)
            .await
            .unwrap();
        assert_eq!(page2.cards.len(), 1, "only card C remains after modseq_b");
        assert!(!page2.more, "no more cards after modseq_b with limit=3");
        assert_eq!(page2.cards[0].modseq, modseq_c);
    }

    /// `contacts_addressbook_id()` is an actor-independent blake3 constant
    /// (`dav_identity.rs`, `carddav-server.md` § Address-book collection model
    /// :125) — every user's default address book shares this exact id, byte
    /// for byte. On the MDA path (`require_dav_caller_scope`,
    /// `bridge_routing_handlers.rs:259`) this id comes straight off the
    /// request URL, so `WHERE actor_id = ?1` in this query is the ONLY thing
    /// separating two users' rows.
    #[tokio::test]
    async fn query_carddav_changes_since_is_isolated_per_actor_on_shared_collection_id() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_a = make_actor(0xd0);
        let actor_b = make_actor(0xd1);
        let ab = contacts_addressbook_id();
        db.insert_bridge_carddav_addressbook(&actor_a, &ab, b"a", 1_700_000_000)
            .await
            .unwrap();
        db.insert_bridge_carddav_addressbook(&actor_b, &ab, b"b", 1_700_000_000)
            .await
            .unwrap();
        db.place_carddav_card(
            &actor_a,
            &ab,
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
            .query_carddav_changes_since(&actor_a, &ab, 0, 0)
            .await
            .unwrap();
        assert_eq!(page_a.cards.len(), 1, "actor_a sees its own change");

        let page_b = db
            .query_carddav_changes_since(&actor_b, &ab, 0, 0)
            .await
            .unwrap();
        assert!(
            page_b.cards.is_empty(),
            "actor_b must not see actor_a's change on the SAME shared collection id"
        );
    }

    // ── count_bridge_carddav_cards ───────────────────────────────────────────

    #[tokio::test]
    async fn count_bridge_carddav_cards_returns_zero_when_empty() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(200);
        let ab = make_addressbook_id(201);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"meta", 1_700_000_000)
            .await
            .unwrap();
        let count = db.count_bridge_carddav_cards(&actor, &ab).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn count_bridge_carddav_cards_returns_correct_count_after_inserts() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(202);
        let ab = make_addressbook_id(203);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"meta", 1_700_000_000)
            .await
            .unwrap();
        db.place_carddav_card(
            &actor,
            &ab,
            &make_uid_hash(210),
            b"body-1",
            b"hint-1",
            1_700_000_001,
            6,
            1_700_000_100,
        )
        .await
        .unwrap();
        db.place_carddav_card(
            &actor,
            &ab,
            &make_uid_hash(211),
            b"body-2",
            b"hint-2",
            1_700_000_002,
            6,
            1_700_000_101,
        )
        .await
        .unwrap();
        let count = db.count_bridge_carddav_cards(&actor, &ab).await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn count_bridge_carddav_cards_is_isolated_per_actor_and_addressbook() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_a = make_actor(220);
        let actor_b = make_actor(221);
        let ab_x = make_addressbook_id(222);
        let ab_y = make_addressbook_id(223);
        // Provision all four combinations.
        for (actor, ab) in [(&actor_a, &ab_x), (&actor_a, &ab_y), (&actor_b, &ab_x)] {
            db.insert_bridge_carddav_addressbook(actor, ab, b"meta", 1_700_000_000)
                .await
                .unwrap();
        }
        // Insert one card only into (actor_a, ab_x).
        db.place_carddav_card(
            &actor_a,
            &ab_x,
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
            db.count_bridge_carddav_cards(&actor_a, &ab_x)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            db.count_bridge_carddav_cards(&actor_a, &ab_y)
                .await
                .unwrap(),
            0,
            "different address book, same actor"
        );
        assert_eq!(
            db.count_bridge_carddav_cards(&actor_b, &ab_x)
                .await
                .unwrap(),
            0,
            "same address book, different actor"
        );
    }

    /// The count-level twin above pins isolation via `count_bridge_carddav_cards`;
    /// this pins the same property on `query_carddav_cards` itself — the query
    /// the MDA path actually serves rows from — using the REAL shared collection
    /// id (`contacts_addressbook_id()`, `carddav-server.md` § Address-book
    /// collection model :125) rather than an arbitrary
    /// one.
    #[tokio::test]
    async fn query_carddav_cards_is_isolated_per_actor_on_shared_collection_id() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_a = make_actor(0xd2);
        let actor_b = make_actor(0xd3);
        let ab = contacts_addressbook_id();
        db.insert_bridge_carddav_addressbook(&actor_a, &ab, b"a", 1_700_000_000)
            .await
            .unwrap();
        db.insert_bridge_carddav_addressbook(&actor_b, &ab, b"b", 1_700_000_000)
            .await
            .unwrap();
        db.place_carddav_card(
            &actor_a,
            &ab,
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
            .query_carddav_cards(&actor_a, &ab, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page_a.cards.len(), 1, "actor_a sees its own card");

        let page_b = db
            .query_carddav_cards(&actor_b, &ab, None, None, 0)
            .await
            .unwrap();
        assert!(
            page_b.cards.is_empty(),
            "actor_b must not see actor_a's card on the SAME shared collection id"
        );
    }

    /// Twin of `bridge_caldav`'s `a_row_without_its_segment_record_is_refused`
    /// — the alpha no-data-loss guard on the card arm.
    #[tokio::test]
    async fn a_row_without_its_segment_record_is_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(70);
        let book = make_addressbook_id(71);
        db.insert_bridge_carddav_addressbook(&actor, &book, b"m", 1_700_000_000)
            .await
            .unwrap();

        let body = b"sealed-vcard";
        let ts = 1_700_000_000i64;
        let card_id = derive_carddav_card_id(&actor, ts, body);

        let err = db
            .replace_carddav_card_by_uid(
                &actor,
                &book,
                &make_uid_hash(72),
                None,
                &card_id,
                // A cid the body WOULD hash to — but nothing was ever appended
                // under it, which is exactly what the guard must catch.
                &carddav_record_cid(body, b"hint").expect("derive record_cid"),
                b"hint",
                None,
                ts,
                body.len() as u32,
                1_700_000_100,
            )
            .await
            .expect_err("a row with no segment record must be refused");
        assert!(
            err.to_string().contains("no live __card segment record"),
            "unexpected error: {err}"
        );

        let page = db
            .query_carddav_cards(&actor, &book, None, None, 10)
            .await
            .unwrap();
        assert!(page.cards.is_empty(), "the guard must not write a row");
    }

    /// Seed a row plus a live `__card` mirror record, as a post-cutover card
    /// looks. Returns `(card_id, cid)`.
    async fn seed_card_with_content_record(
        db: &CacheDb,
        actor: &[u8; 32],
        book: &[u8; 32],
        uid: &[u8],
        body: &[u8],
        ts: i64,
    ) -> ([u8; 32], fauna_cbor::Cid) {
        let card_id = match db
            .place_carddav_card(
                actor,
                book,
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
            PlaceCarddavCardOutcome::Created { card_id, .. } => card_id,
            other => panic!("expected Created, got {other:?}"),
        };
        // The cid the seeded row carries — `place_carddav_card` derives the
        // same value from this body+hint.
        let cid = carddav_record_cid(body, b"hint").expect("derive record_cid");
        db.segment_records_insert_card(actor, 1, &cid, "2023-11", ts + 100)
            .await
            .unwrap();
        (card_id, cid)
    }

    async fn record_is_live(db: &CacheDb, actor: &[u8; 32], cid: &fauna_cbor::Cid) -> bool {
        db.segment_records_lookup_record(actor, crate::segments::card::KIND, cid)
            .await
            .unwrap()
            .is_some()
    }

    #[tokio::test]
    async fn deleting_a_card_tombstones_its_content_record() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(90);
        let book = make_addressbook_id(91);
        db.insert_bridge_carddav_addressbook(&actor, &book, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(92);
        let (_card_id, cid) =
            seed_card_with_content_record(&db, &actor, &book, &uid, b"sealed-body", 1_700_000_000)
                .await;
        assert!(record_is_live(&db, &actor, &cid).await);

        let out = db
            .delete_carddav_card_by_uid(&actor, &book, &uid, None, 1_700_000_200)
            .await
            .unwrap();
        assert!(matches!(out, DeleteCarddavCardOutcome::Deleted { .. }));
        assert!(
            !record_is_live(&db, &actor, &cid).await,
            "the deleted card's content record must be tombstoned"
        );
    }

    /// The old record is tombstoned; the new one — appended by the handler just
    /// before this call, and the body of the row being inserted — stays live.
    #[tokio::test]
    async fn superseding_a_card_tombstones_only_the_old_content_record() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(93);
        let book = make_addressbook_id(94);
        db.insert_bridge_carddav_addressbook(&actor, &book, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(95);
        let (_old_id, old_cid) =
            seed_card_with_content_record(&db, &actor, &book, &uid, b"body-v1", 1_700_000_000)
                .await;

        let new_id = derive_carddav_card_id(&actor, 1_700_000_300, b"body-v2");
        let new_cid = carddav_record_cid(b"body-v2", b"hint").expect("derive record_cid");
        db.segment_records_insert_card(&actor, 1, &new_cid, "2023-11", 1_700_000_300)
            .await
            .unwrap();

        let out = db
            .replace_carddav_card_by_uid(
                &actor,
                &book,
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
        assert!(matches!(out, ReplaceCarddavCardOutcome::Updated { .. }));

        assert!(
            !record_is_live(&db, &actor, &old_cid).await,
            "the superseded content record must be tombstoned"
        );
        assert!(
            record_is_live(&db, &actor, &new_cid).await,
            "the NEW content record must stay live — it is the row's body"
        );
    }

    /// Deleting a whole address book cascade-deletes its cards. Every one of
    /// their content records must be tombstoned — the ids have to be collected
    /// before the DELETE, since afterwards nothing names them.
    #[tokio::test]
    async fn deleting_an_addressbook_tombstones_every_cards_content_record() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(96);
        let book = make_addressbook_id(97);
        db.insert_bridge_carddav_addressbook(&actor, &book, b"m", 1_700_000_000)
            .await
            .unwrap();
        let (_a, cid_a) = seed_card_with_content_record(
            &db,
            &actor,
            &book,
            &make_uid_hash(98),
            b"card-a",
            1_700_000_000,
        )
        .await;
        let (_b, cid_b) = seed_card_with_content_record(
            &db,
            &actor,
            &book,
            &make_uid_hash(99),
            b"card-b",
            1_700_000_010,
        )
        .await;
        assert!(record_is_live(&db, &actor, &cid_a).await);
        assert!(record_is_live(&db, &actor, &cid_b).await);

        let out = db.delete_carddav_addressbook(&actor, &book).await.unwrap();
        assert!(matches!(
            out,
            DeleteCarddavAddressbookOutcome::Deleted { .. }
        ));

        assert!(
            !record_is_live(&db, &actor, &cid_a).await,
            "card A's record"
        );
        assert!(
            !record_is_live(&db, &actor, &cid_b).await,
            "card B's record"
        );
    }

    ///  Twin of the calendar pin: `card_id` mixes in the timestamp,
    /// the record CID does not, so a re-PUT of byte-identical sealed bytes under
    /// a new timestamp replaces the row onto the SAME record — which must stay
    /// live.
    #[tokio::test]
    async fn resealing_identical_bytes_under_a_new_timestamp_keeps_the_record_live() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(110);
        let book = make_addressbook_id(111);
        db.insert_bridge_carddav_addressbook(&actor, &book, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(112);
        let (old_id, cid) =
            seed_card_with_content_record(&db, &actor, &book, &uid, b"same-bytes", 1_700_000_000)
                .await;

        let new_id = derive_carddav_card_id(&actor, 1_700_000_300, b"same-bytes");
        assert_ne!(old_id, new_id, "the timestamp must change the card_id");
        let out = db
            .replace_carddav_card_by_uid(
                &actor,
                &book,
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
        assert!(matches!(out, ReplaceCarddavCardOutcome::Updated { .. }));
        assert!(
            record_is_live(&db, &actor, &cid).await,
            "the live row still points at this record — tombstoning it loses the body"
        );
    }

    /// Seed book A and book B of one actor with one byte-identical card each —
    /// two rows, one shared per-actor record. Returns `(book_a, book_b, uid, cid)`.
    async fn seed_one_card_in_two_books(
        db: &CacheDb,
        actor: &[u8; 32],
        seed: u8,
    ) -> ([u8; 32], [u8; 32], Vec<u8>, fauna_cbor::Cid) {
        let book_a = make_addressbook_id(seed);
        let book_b = make_addressbook_id(seed + 1);
        for book in [&book_a, &book_b] {
            db.insert_bridge_carddav_addressbook(actor, book, b"m", 1_700_000_000)
                .await
                .unwrap();
        }
        let uid = make_uid_hash(seed + 2);
        let (_a, cid) =
            seed_card_with_content_record(db, actor, &book_a, &uid, b"shared", 1_700_000_000).await;
        let placed = db
            .place_carddav_card(
                actor,
                &book_b,
                &uid,
                b"shared",
                b"hint",
                1_700_000_050,
                6,
                1_700_000_150,
            )
            .await
            .unwrap();
        assert!(matches!(placed, PlaceCarddavCardOutcome::Created { .. }));
        (book_a, book_b, uid, cid)
    }

    ///  Deleting the card from one of two books sharing its record
    /// keeps the record live; deleting the last reference tombstones it.
    #[tokio::test]
    async fn deleting_one_of_two_cards_sharing_a_record_keeps_it_live() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(113);
        let (book_a, book_b, uid, cid) = seed_one_card_in_two_books(&db, &actor, 114).await;

        db.delete_carddav_card_by_uid(&actor, &book_a, &uid, None, 1_700_000_200)
            .await
            .unwrap();
        assert!(
            record_is_live(&db, &actor, &cid).await,
            "book B's row still points at this record"
        );

        db.delete_carddav_card_by_uid(&actor, &book_b, &uid, None, 1_700_000_300)
            .await
            .unwrap();
        assert!(
            !record_is_live(&db, &actor, &cid).await,
            "the last reference is gone — the record must be tombstoned"
        );
    }

    ///  The address-book cascade is the same hazard in bulk: deleting
    /// book A must not tombstone a record book B's card still points at.
    #[tokio::test]
    async fn deleting_an_addressbook_keeps_records_another_book_references() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(117);
        let (book_a, book_b, _uid, cid) = seed_one_card_in_two_books(&db, &actor, 118).await;

        db.delete_carddav_addressbook(&actor, &book_a)
            .await
            .unwrap();
        assert!(
            record_is_live(&db, &actor, &cid).await,
            "book B's card still points at this record"
        );

        db.delete_carddav_addressbook(&actor, &book_b)
            .await
            .unwrap();
        assert!(
            !record_is_live(&db, &actor, &cid).await,
            "the last reference is gone — the record must be tombstoned"
        );
    }

    // ── Row 768: write-arm atomicity ────────────────────────────────────────
    //
    // Twin of the CalDAV pins in `bridge_caldav.rs`. Every per-card arm below
    // ends with the address-book-state (ctag, highestmodseq) bump as its LAST
    // statement; `delete_carddav_addressbook`'s arm instead ends with the
    // book-row DELETE, so its pin needs a `BEFORE DELETE` trigger — an
    // `UPDATE` trigger on that table would never fire, since nothing in the
    // arm ever updates the book row. A trigger that raises forces the
    // failure after every earlier statement in the arm has already run, so
    // these pins can only pass if the whole arm commits as one transaction:
    // drop the `unchecked_transaction()` wrapper and the earlier statements
    // commit on their own, in their own autocommit transactions, before the
    // last statement ever fires — redding every assertion below.
    // `RAISE(ABORT)` is required, not a dropped row: an `UPDATE`/`DELETE`
    // matching nothing returns `Ok(0)`, not an error, so it can't force a
    // failure this deep into an arm, and every function here holds
    // `conn.lock()` for its whole body, so a test can't act between two of
    // its statements either.

    /// Install a trigger that fails the next `bridge_carddav_addressbooks`
    /// UPDATE — the shared last statement of every per-card arm pinned below.
    async fn install_addressbook_bump_failure(db: &CacheDb) {
        let conn = db.conn().await;
        conn.execute_batch(
            "CREATE TRIGGER row768_fail_addressbook_bump \
             BEFORE UPDATE ON bridge_carddav_addressbooks \
             BEGIN SELECT RAISE(ABORT, 'row768: injected addressbook-bump failure'); END;",
        )
        .unwrap();
    }

    #[tokio::test]
    async fn place_carddav_card_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(200);
        let ab = make_addressbook_id(201);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(202);

        install_addressbook_bump_failure(&db).await;

        db.place_carddav_card(
            &actor,
            &ab,
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
        let card_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            card_count, 0,
            "the card insert must not survive the failed bump"
        );
        let hms: i64 = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hms, 1, "the address book state must not move");
    }

    #[tokio::test]
    async fn replace_carddav_card_by_uid_create_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(203);
        let ab = make_addressbook_id(204);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(205);
        let body = b"body-v1";
        let card_id = derive_carddav_card_id(&actor, 1_700_000_000, body);
        let cid = carddav_record_cid(body, b"hint").expect("derive record_cid");
        ensure_card_mirror_record(&db, &actor, &cid).await;

        install_addressbook_bump_failure(&db).await;

        db.replace_carddav_card_by_uid(
            &actor,
            &ab,
            &uid,
            None,
            &card_id,
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
        let card_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            card_count, 0,
            "the create-arm insert must not survive the failed bump"
        );
        let hms: i64 = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hms, 1, "the address book state must not move");
    }

    #[tokio::test]
    async fn replace_carddav_card_by_uid_sidecar_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(206);
        let ab = make_addressbook_id(207);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(208);
        let body = b"identical-body";
        let ts = 1_700_000_000;
        let card_id = derive_carddav_card_id(&actor, ts, body);
        let cid = carddav_record_cid(body, b"hint").expect("derive record_cid");
        ensure_card_mirror_record(&db, &actor, &cid).await;

        let created = db
            .replace_carddav_card_by_uid(
                &actor,
                &ab,
                &uid,
                None,
                &card_id,
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
            ReplaceCarddavCardOutcome::Created { etag, modseq, .. } => (etag, modseq),
            other => panic!("expected Created; got {:?}", other),
        };

        install_addressbook_bump_failure(&db).await;

        db.replace_carddav_card_by_uid(
            &actor,
            &ab,
            &uid,
            None,
            &card_id,
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
                "SELECT etag, modseq, encrypted_fauna_ext FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND card_id = ?2",
                rusqlite::params![&actor[..], &card_id[..]],
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
    async fn replace_carddav_card_by_uid_replace_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(209);
        let ab = make_addressbook_id(210);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(211);
        let (prior_card_id, prior_cid) =
            seed_card_with_content_record(&db, &actor, &ab, &uid, b"body-v1", 1_700_000_000).await;
        let (prior_etag, prior_modseq): (String, i64) = {
            let conn = db.conn().await;
            conn.query_row(
                "SELECT etag, modseq FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND card_id = ?2",
                rusqlite::params![&actor[..], &prior_card_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };

        install_addressbook_bump_failure(&db).await;

        let new_card_id = derive_carddav_card_id(&actor, 1_700_000_300, b"body-v2");
        let new_cid = carddav_record_cid(b"body-v2", b"hint").expect("derive record_cid");
        ensure_card_mirror_record(&db, &actor, &new_cid).await;
        db.replace_carddav_card_by_uid(
            &actor,
            &ab,
            &uid,
            None,
            &new_card_id,
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
                "SELECT COUNT(*) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND card_id = ?2",
                rusqlite::params![&actor[..], &prior_card_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            tombstone_count, 0,
            "no tombstone must survive the failed bump"
        );

        let (etag, modseq): (String, i64) = conn
            .query_row(
                "SELECT etag, modseq FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND card_id = ?2",
                rusqlite::params![&actor[..], &prior_card_id[..]],
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
                "SELECT COUNT(*) FROM bridge_carddav_cards WHERE actor_id = ?1 AND card_id = ?2",
                rusqlite::params![&actor[..], &new_card_id[..]],
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
    async fn delete_carddav_card_by_uid_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(212);
        let ab = make_addressbook_id(213);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();
        let uid = make_uid_hash(214);
        let (card_id, cid) =
            seed_card_with_content_record(&db, &actor, &ab, &uid, b"body", 1_700_000_000).await;

        install_addressbook_bump_failure(&db).await;

        db.delete_carddav_card_by_uid(&actor, &ab, &uid, None, 1_700_000_200)
            .await
            .expect_err("the injected trigger must fail the delete arm");

        let conn = db.conn().await;
        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND card_id = ?2",
                rusqlite::params![&actor[..], &card_id[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            tombstone_count, 0,
            "no tombstone must survive the failed bump"
        );

        let live_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards WHERE actor_id = ?1 AND card_id = ?2",
                rusqlite::params![&actor[..], &card_id[..]],
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

    #[tokio::test]
    async fn delete_carddav_addressbook_arm_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(215);
        let ab = make_addressbook_id(216);
        db.insert_bridge_carddav_addressbook(&actor, &ab, b"m", 1_700_000_000)
            .await
            .unwrap();

        // A live card (with a live content record) plus a prior tombstone
        // from an earlier per-card delete — so the pin proves ALL THREE
        // cascade steps (card DELETE, content-record tombstone, expunged-
        // tombstone clear) roll back together with the final row DELETE, not
        // just that last statement alone.
        let live_uid = make_uid_hash(217);
        let (_live_card_id, live_cid) =
            seed_card_with_content_record(&db, &actor, &ab, &live_uid, b"body-live", 1_700_000_000)
                .await;

        let gone_uid = make_uid_hash(218);
        db.place_carddav_card(
            &actor,
            &ab,
            &gone_uid,
            b"body-gone",
            b"hint",
            1_700_000_001,
            9,
            1_700_000_101,
        )
        .await
        .unwrap();
        db.delete_carddav_card_by_uid(&actor, &ab, &gone_uid, None, 1_700_000_200)
            .await
            .unwrap();

        {
            let conn = db.conn().await;
            conn.execute_batch(
                "CREATE TRIGGER row768_fail_addressbook_delete \
                 BEFORE DELETE ON bridge_carddav_addressbooks \
                 BEGIN SELECT RAISE(ABORT, 'row768: injected addressbook-delete failure'); END;",
            )
            .unwrap();
        }

        db.delete_carddav_addressbook(&actor, &ab)
            .await
            .expect_err("the injected trigger must fail the addressbook-delete arm");

        let conn = db.conn().await;
        let ab_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_addressbooks \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            ab_count, 1,
            "the address book row must survive the failed delete"
        );

        let card_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            card_count, 1,
            "the cascade-deleted card must survive the failed delete"
        );

        let tombstone_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_carddav_expunged \
                 WHERE actor_id = ?1 AND addressbook_id = ?2",
                rusqlite::params![&actor[..], &ab[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            tombstone_count, 1,
            "the prior per-card tombstone must survive the failed delete"
        );
        drop(conn);

        assert!(
            record_is_live(&db, &actor, &live_cid).await,
            "the live card's content record must not be tombstoned when the delete arm fails"
        );
    }
}
