//! IMAP placement layer for the I2b mail-bridge absorption (Phase C).
//!
//! Three tables:
//!   * `bridge_imap_messages`      — per-(actor, mailbox, uid) placement row
//!   * `bridge_imap_mailbox_state` — per-(actor, mailbox) UID + modseq counters
//!   * `bridge_imap_expunged`      — per-(actor, mailbox, uid) expunge log
//!
//! Every public method takes a `&self` receiver and operates under
//! `self.conn.lock().await` exactly like the rest of `CacheDb`.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use super::{CacheDb, blob_col_to_array, now_epoch_millis, now_epoch_secs};
use fauna_cbor::Cid;
use fauna_segment_store::SegmentManager;

/// Decode a `record_cid` BLOB column (the full 36-byte `fauna_cbor::Cid`) inside
/// a rusqlite row mapper. `col` is the 0-based column index for error reporting.
pub(super) fn cid_from_col(blob: Vec<u8>, col: usize) -> rusqlite::Result<Cid> {
    let arr: [u8; 36] = blob_col_to_array(blob, col, "record_cid")?;
    Cid::from_bytes(arr).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Blob,
            "record_cid invalid Cid prefix".into(),
        )
    })
}

// ── Private connection-level helpers ─────────────────────────────────────────

/// The six standard IMAP mailboxes from `docs/goal/behavior/imap-server.md`
/// § Standard mailboxes. Seeded on first AUTH (idempotent), reserved
/// against CREATE/DELETE/RENAME — the names map to RFC 6154 SPECIAL-USE
/// attributes `\Inbox` / `\Archive` / `\Drafts` / `\Sent` / `\Trash` /
/// `\Junk`. INBOX is renamable per RFC 9051 §6.3.6's special-case; the
/// other five are immovable.
pub const STANDARD_MAILBOXES: &[&str] = &["INBOX", "Archive", "Drafts", "Sent", "Trash", "Junk"];

// The ward's held mailbox (`family-safety.md` § The mail gate) — never `Junk`,
// never a seventh standard mailbox; the constant carries the full rationale. It
// lives in `fauna_protocol::email`, beside the `FileInto` target check that
// refuses it, and is re-exported here for the mail store, the hold path and the
// IMAP guards.
pub use fauna_protocol::email::GUARDIAN_HELD_MAILBOX;

/// True iff `name` is one of the six standard mailboxes. Case-sensitive
/// match against the canonical names — `inbox` (lowercase) is not
/// reserved (`SELECT inbox` is mapped to INBOX at the wire layer per
/// RFC 9051 §5.1, but the DB layer enforces the literal name).
pub fn is_reserved_mailbox(name: &str) -> bool {
    STANDARD_MAILBOXES.contains(&name)
}

/// True iff `name` is refused by the IMAP CREATE/DELETE/RENAME wire guards:
/// the six standard mailboxes plus [`GUARDIAN_HELD_MAILBOX`]. The held
/// mailbox is not *standard* (not seeded, no SPECIAL-USE attribute — see its
/// doc), but its name is reserved for the hold path: a ward must not be able
/// to DELETE/RENAME it out from under a live hold, and nobody squats the name
/// before a first hold auto-creates it (`family-safety.md` § The mail gate).
pub fn is_protected_mailbox(name: &str) -> bool {
    is_reserved_mailbox(name) || name == GUARDIAN_HELD_MAILBOX
}

/// Return the RFC 6154 SPECIAL-USE attribute(s) for a standard mailbox
/// name, or an empty Vec for user-created mailboxes. Used to populate
/// `MailPlacementRecord::Create::attrs` when emitting placement events
/// for the six bootstrap mailboxes seeded by
/// `ensure_bridge_imap_mailboxes` (per goal-doc `imap-server.md` §
/// Standard mailboxes table).
pub fn standard_mailbox_attrs(name: &str) -> Vec<String> {
    match name {
        "INBOX" => vec!["\\Inbox".to_string()],
        "Archive" => vec!["\\Archive".to_string()],
        "Drafts" => vec!["\\Drafts".to_string()],
        "Sent" => vec!["\\Sent".to_string()],
        "Trash" => vec!["\\Trash".to_string()],
        "Junk" => vec!["\\Junk".to_string()],
        _ => Vec::new(),
    }
}

/// One mailbox row newly inserted by `ensure_bridge_imap_mailboxes` in
/// the current call. Empty Vec on a no-op call (every standard mailbox
/// already present from a prior `ensure_*` invocation). Callers that
/// participate in the placement journal emit one
/// `MailPlacementRecord::Create` per entry — see spec § D2 record table
/// and § D6 (ε) for the SQLite-commit-then-journal-append crash-window
/// note (closed by Plan 2 T9's divergence detection at SELECT / QRESYNC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewlySeededMailbox {
    pub name: String,
    pub uid_validity: u32,
    pub attrs: Vec<String>,
}

/// Validate a user-supplied mailbox name per RFC 9051 §5.1. Returns
/// `Err(reason)` with a short human-readable explanation when the
/// name violates the rule set, `Ok(())` otherwise. The bridge
/// surfaces `reason` verbatim in the IMAP `BAD` response text.
///
/// Rules enforced here:
///   * non-empty (RFC 9051 §5.1 implicitly — zero-length is a parse
///     error upstream; we double-check)
///   * byte length ≤ 255 (RFC 9051 §5.1 mailbox-name token cap)
///   * no NUL (`\0`) — IMAP wire token-stuffing safety
///   * no CR or LF — IMAP wire framing
///   * no leading or trailing `/` — `/` is the canonical hierarchy
///     separator (per `imap-server.md` § Standard mailboxes); a
///     leading/trailing separator yields an empty path component
///     which has no defined semantics
///   * no consecutive `//` — same reason as above
///
/// The valid character set is otherwise UTF-8 (post modified-UTF-7
/// decode at the wire layer); we do not enforce ASCII because RFC
/// 6855 (UTF8=ACCEPT) callers want Unicode mailbox names.
pub fn validate_mailbox_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("empty".into());
    }
    if name.len() > 255 {
        return Err("too long".into());
    }
    if name.contains('\0') {
        return Err("contains NUL".into());
    }
    if name.contains('\r') || name.contains('\n') {
        return Err("contains CR/LF".into());
    }
    if name.starts_with('/') || name.ends_with('/') {
        return Err("leading/trailing separator".into());
    }
    if name.contains("//") {
        return Err("empty path component".into());
    }
    Ok(())
}

/// Maximum byte length of a single `*_norm` column on
/// `bridge_imap_messages`. Pins the index cost to a fixed ceiling no
/// matter how grotesque the inbound header is.
pub(crate) const NORM_COLUMN_MAX_BYTES: usize = 1024;

/// Case-fold + length-cap a header substring for the `*_norm` columns
/// on `bridge_imap_messages`. Lowercases via ASCII fold (the search
/// path lowercases its pattern the same way), truncates to a UTF-8
/// char boundary at `NORM_COLUMN_MAX_BYTES`. Empty input → empty
/// output (the column default).
pub(crate) fn norm_for_storage(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let lower = input.to_ascii_lowercase();
    fauna_core::encoding::truncate_to_char_boundary(&lower, NORM_COLUMN_MAX_BYTES).to_string()
}

/// Ensure a `bridge_imap_mailbox_state` row exists for `(actor, mailbox)`.
/// If it already exists this is a no-op.
fn ensure_mailbox_state_row(
    conn: &Connection,
    actor: &[u8; 32],
    mailbox: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO bridge_imap_mailbox_state \
             (actor_id, mailbox, uid_validity, uid_next, highestmodseq) \
             VALUES (?1, ?2, 1, 1, 1)",
        rusqlite::params![&actor[..], mailbox],
    )?;
    Ok(())
}

/// Allocate the next UID for `(actor, mailbox)` and bump `highestmodseq`.
/// Returns `(new_uid, new_modseq)`.
/// The state row **must** already exist (call `ensure_mailbox_state_row` first).
pub(crate) fn allocate_uid(
    conn: &Connection,
    actor: &[u8; 32],
    mailbox: &str,
) -> rusqlite::Result<(u32, i64)> {
    let (uid_next, hms): (i64, i64) = conn.query_row(
        "SELECT uid_next, highestmodseq \
             FROM bridge_imap_mailbox_state \
             WHERE actor_id = ?1 AND mailbox = ?2",
        rusqlite::params![&actor[..], mailbox],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let new_uid = uid_next as u32;
    let new_modseq = hms + 1;
    conn.execute(
        "UPDATE bridge_imap_mailbox_state \
             SET uid_next = ?1, highestmodseq = ?2 \
             WHERE actor_id = ?3 AND mailbox = ?4",
        rusqlite::params![uid_next + 1, new_modseq, &actor[..], mailbox],
    )?;
    Ok((new_uid, new_modseq))
}

/// File one message into `mailbox` under a fresh UID — the arrival's own
/// placement, on the caller's connection so it can share a transaction.
///
/// Ensures the mailbox's state row (a non-standard name is auto-created at
/// `uid_validity = 1`, exactly as an arrival auto-creates it), allocates the
/// next UID and modseq, and inserts the `bridge_imap_messages` row.
/// `from_norm` is the already-normalized plaintext-floor sender domain;
/// to/cc/subject_norm stay empty (encrypted-mode floor — `imap-server.md`
/// § SEARCH). Returns `(uid, modseq)`.
///
/// Two callers: [`CacheDb::place_inbound_mail`]'s shared body, and the lived-in
/// recovery (`crate::backup::recover`), which files each recovered record in
/// the same transaction as its mirror row.
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_new_message_in(
    conn: &Connection,
    actor: &[u8; 32],
    message_id: &[u8; 32],
    mailbox: &str,
    internal_date: i64,
    flags: &str,
    from_norm: &str,
    created_at: i64,
) -> rusqlite::Result<(u32, i64)> {
    ensure_mailbox_state_row(conn, actor, mailbox)?;
    let (new_uid, new_modseq) = allocate_uid(conn, actor, mailbox)?;
    conn.execute(
        "INSERT INTO bridge_imap_messages \
             (actor_id, mailbox, uid, message_id, flags, modseq, internal_date, created_at, from_norm) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            &actor[..],
            mailbox,
            new_uid as i64,
            &message_id[..],
            flags,
            new_modseq,
            internal_date,
            created_at,
            from_norm,
        ],
    )?;
    Ok((new_uid, new_modseq))
}

/// Shared copy logic executed inside an already-held connection lock.
///
/// Callers (`apply_copy` and `apply_move`) take the lock once and then call
/// this function; it must NOT acquire the lock itself.
///
/// Returns `(dest_uid_validity, copied_pairs, dest_highestmodseq)` where
/// `copied_pairs` is `Vec<(source_uid, dest_uid)>` in request order (missing
/// source UIDs excluded).  When `copied_pairs` is empty the dest state row's
/// counters are left unchanged.
fn copy_within_locked(
    conn: &Connection,
    actor: &[u8; 32],
    source_mailbox: &str,
    uids: &[u32],
    dest_mailbox: &str,
) -> rusqlite::Result<(u32, Vec<(u32, u32)>, i64)> {
    // 1. Ensure dest mailbox-state row (no-op if it already exists).
    ensure_mailbox_state_row(conn, actor, dest_mailbox)?;

    // 2. Read dest state.
    let (dest_validity, mut dest_uid_next, mut dest_hms): (i64, i64, i64) = conn.query_row(
        "SELECT uid_validity, uid_next, highestmodseq \
         FROM bridge_imap_mailbox_state \
         WHERE actor_id = ?1 AND mailbox = ?2",
        rusqlite::params![&actor[..], dest_mailbox],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let dest_uid_validity = dest_validity as u32;
    let now = now_epoch_millis();

    let mut pairs: Vec<(u32, u32)> = Vec::new();

    // 3. For each requested UID, look up the source row.
    for &uid in uids {
        let row_opt: Option<(Vec<u8>, String, i64, String, String, String, String)> = conn
            .query_row(
                "SELECT message_id, flags, internal_date, \
                        from_norm, to_norm, cc_norm, subject_norm \
                 FROM bridge_imap_messages \
                 WHERE actor_id = ?1 AND mailbox = ?2 AND uid = ?3",
                rusqlite::params![&actor[..], source_mailbox, uid as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;

        let (message_id_blob, flags_str, internal_date, from_norm, to_norm, cc_norm, subject_norm) =
            match row_opt {
                None => continue, // missing UID — skip silently
                Some(r) => r,
            };

        // Filter out \Recent (RFC 3501: \Recent is not preserved by COPY).
        let filtered_flags: String = {
            use std::collections::BTreeSet;
            let set: BTreeSet<&str> = flags_str
                .split_whitespace()
                .filter(|&t| t != "\\Recent")
                .collect();
            set.into_iter().collect::<Vec<_>>().join(" ")
        };

        let dest_uid = dest_uid_next as u32;
        let dest_modseq = dest_hms + 1;

        conn.execute(
            "INSERT INTO bridge_imap_messages \
             (actor_id, mailbox, uid, message_id, flags, modseq, internal_date, created_at, \
              from_norm, to_norm, cc_norm, subject_norm) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                &actor[..],
                dest_mailbox,
                dest_uid as i64,
                &message_id_blob[..],
                &filtered_flags,
                dest_modseq,
                internal_date,
                now,
                &from_norm,
                &to_norm,
                &cc_norm,
                &subject_norm,
            ],
        )?;

        pairs.push((uid, dest_uid));

        dest_uid_next += 1;
        dest_hms = dest_modseq;
    }

    // 4. If anything was copied, update dest state row in one shot.
    if !pairs.is_empty() {
        conn.execute(
            "UPDATE bridge_imap_mailbox_state \
             SET uid_next = ?1, highestmodseq = ?2 \
             WHERE actor_id = ?3 AND mailbox = ?4",
            rusqlite::params![dest_uid_next, dest_hms, &actor[..], dest_mailbox],
        )?;
    }

    Ok((dest_uid_validity, pairs, dest_hms))
}

// ── DB-internal enums ─────────────────────────────────────────────────────────

/// Which flag mutation to apply. DB-internal mirror of the protocol
/// `StoreFlagsOp`; the protocol type does not leak into this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreFlagsDbOp {
    Set,
    Add,
    Remove,
}

/// Outcome of `apply_store_flags`.
pub struct StoreFlagsDbOutcome {
    /// One entry per UID that had a placement row and was updated.
    /// Tuple: `(uid, before_flags_string, after_flags_string, new_modseq)`.
    ///
    /// `before_flags_string` is captured inside the same conn-mutex
    /// critical section as the UPDATE, so a parallel writer cannot
    /// interleave between the SELECT and the UPDATE. Carrying the
    /// before-state lets the IMAP-handler emit a `MailPlacementRecord::
    /// StoreFlags` whose `before_flags` / `after_flags` pair feeds the
    /// (γ) divergence-detection surface (spec § D6 (γ)).
    pub updated: Vec<(u32, String, String, i64)>,
    /// New mailbox `highestmodseq` after the bump (unchanged if nothing
    /// was touched).
    pub highestmodseq: i64,
    /// UIDs whose current modseq exceeded the supplied `unchanged_since`
    /// (RFC 7162 §3.1.3) and were therefore skipped. Always empty when
    /// `unchanged_since` is `None`.
    pub modified: Vec<u32>,
}

/// Outcome of `apply_expunge`.
pub struct ExpungeDbOutcome {
    /// UIDs deleted, ascending.
    pub expunged_uids: Vec<u32>,
    /// New mailbox `highestmodseq` after the bump (unchanged if nothing
    /// was expunged).
    pub highestmodseq: i64,
}

/// Outcome of `apply_copy`.
pub struct CopyDbOutcome {
    pub dest_uid_validity: u32,
    /// `(source_uid, dest_uid)` pairs in request order; missing source UIDs
    /// are silently excluded.
    pub copied: Vec<(u32, u32)>,
    pub dest_highestmodseq: i64,
}

/// Outcome of `apply_move`.
pub struct MoveDbOutcome {
    pub dest_uid_validity: u32,
    /// `(source_uid, dest_uid)` pairs for messages that were moved.
    pub moved: Vec<(u32, u32)>,
    pub source_highestmodseq: i64,
    pub dest_highestmodseq: i64,
    /// Epoch seconds the move was stamped — the `expunged_at` of the source
    /// rows' expunge-log entries, and what the handler reuses for the
    /// placement journal's `Move.deleted_at` so the two agree.
    pub moved_at: i64,
}

/// Outcome of `create_bridge_imap_mailbox`. The handler maps this to
/// the wire-level `CreateMailboxReply` — `Reserved` and `InvalidName`
/// outcomes are decided by the handler before it reaches the DB layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateMailboxDbOutcome {
    Created { uid_validity: u32 },
    AlreadyExists,
}

/// Outcome of `delete_bridge_imap_mailbox`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteMailboxDbOutcome {
    Deleted,
    NoSuchMailbox,
    NotEmpty,
}

/// Outcome of `rename_bridge_imap_mailbox`. `ReservedSource` and
/// `TargetReserved` outcomes are decided by the handler before it
/// reaches the DB layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameMailboxDbOutcome {
    Renamed,
    NoSuchSource,
    TargetExists,
}

/// DB-internal mirror of `fauna_protocol::bridge_routing::SearchTerm`.
/// The handler translates the protocol enum to this; the DB layer
/// does not depend on `fauna-protocol`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchTermDb {
    HasFlag(String),
    LacksFlag(String),
    HeaderContains(SearchHeaderFieldDb, String),
    /// Inclusive: `internal_date >= ts`.
    SinceInternalDate(i64),
    /// Exclusive: `internal_date < ts`.
    BeforeInternalDate(i64),
    /// Strict: `ciphertext_size > size`.
    Larger(u32),
    /// Strict: `ciphertext_size < size`.
    Smaller(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchHeaderFieldDb {
    From,
    To,
    Cc,
    Subject,
}

// ── Data types ────────────────────────────────────────────────────────────────

/// A single mailbox-state row, used by C.1 list/select handlers.
pub struct MailboxStateRow {
    pub name: String,
    pub uid_validity: u32,
    pub uid_next: u32,
    pub highestmodseq: i64,
}

/// DB-side index-segment row returned by `query_bridge_imap_index_segments`.
/// Joins `bridge_imap_messages` with `segment_records` (placement +
/// segment ref) and decodes each record's `MailRecordEnvelope` to lift
/// `encrypted_index_hint` out of the sealed body's sibling slot.
pub struct IndexSegmentRow {
    pub message_id: [u8; 32],
    pub mailbox: String,
    pub modseq: i64,
    pub encrypted_index_hint: Vec<u8>,
    /// `segment_records.stored_at` (epoch **ms**, the SQL mirror of the
    /// authoritative `MailFloorMetadata::stored_at`) — the content-sealing-
    /// epochs classification basis for the sealed hint. `0` = unknown: the
    /// append-time clock read failed (`segments::mail`'s `stored_at_now_ms`),
    /// which rests the column NULL; such a record is standing-sealed.
    pub stored_at: i64,
}

/// DB-side message-metadata row returned by `query_bridge_imap_messages`.
///
/// `flags` is the raw space-separated token string from the DB column.
/// The handler splits it into `Vec<String>` by `split_whitespace()`;
/// an empty string becomes an empty Vec (no flags set).
pub struct MessageMetaRow {
    pub uid: u32,
    pub message_id: [u8; 32],
    pub modseq: i64,
    pub flags: String,
    pub internal_date: i64,
    /// The record's segment, for the handler to look the record's size up by
    /// `record_cid` through the CARv2 index (`segments::record_sizes`).
    /// RFC822.SIZE is **not** a SQL mirror column (`imap-server.md` § SEARCH).
    pub segment_id: u32,
    /// The record's full 36-byte `Cid` (the `segment_records.record_cid` key) —
    /// passed straight to `segments::record_sizes` for RFC822.SIZE.
    pub record_cid: Cid,
    /// 1-based IMAP sequence number: the row's rank in the FULL mailbox's
    /// ascending-UID order (RFC 9051 §6.4.5), computed nest-side via
    /// `ROW_NUMBER() OVER (ORDER BY m.uid ASC)` over the whole live mailbox —
    /// so a UID-subset fetch still carries each row's true seqNum. Lets the Go
    /// bridge stop re-deriving seqNums from a whole-mailbox snapshot (F1).
    pub seq_num: u32,
    /// `segment_records.stored_at` (epoch **ms**, the SQL mirror of
    /// `MailFloorMetadata::stored_at`) — the record's seal instant, the
    /// content-sealing-epochs classification basis the client feed ships as
    /// `InboxMessage.stored_at` (epoch seconds, ms/1000). `0` = unknown, as
    /// on [`IndexSegmentRow::stored_at`]. Distinct from `internal_date` (the
    /// sealed hint's basis diverges for imported mail).
    pub stored_at: i64,
}

// ── Row mapper ───────────────────────────────────────────────────────────────

fn map_message_meta_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageMetaRow> {
    let uid = row.get::<_, i64>(0)? as u32;
    let message_id: [u8; 32] = blob_col_to_array(row.get(1)?, 1, "message_id")?;
    Ok(MessageMetaRow {
        uid,
        message_id,
        modseq: row.get(2)?,
        flags: row.get(3)?,
        internal_date: row.get(4)?,
        segment_id: row.get::<_, i64>(5)? as u32,
        record_cid: cid_from_col(row.get::<_, Vec<u8>>(6)?, 6)?,
        seq_num: row.get::<_, i64>(7)? as u32,
        stored_at: row.get(8)?,
    })
}

/// One SEARCH hit: the matching `uid` plus the `(segment_id, record_cid)` the
/// handler needs to size the record by the CARv2 index when a `LARGER`/
/// `SMALLER` size predicate is present (`imap-server.md` § SEARCH). Size
/// predicates are applied post-query against the index, not in SQL.
pub struct SearchHitRow {
    pub uid: u32,
    pub segment_id: u32,
    pub record_cid: Cid,
}

// ── CacheDb methods ───────────────────────────────────────────────────────────

impl CacheDb {
    /// Ensure the six standard IMAP mailboxes exist for `actor`.
    /// Idempotent: calling twice leaves exactly 6 rows. The six names
    /// come from `STANDARD_MAILBOXES` and match the goal-doc
    /// `imap-server.md` § Standard mailboxes table — INBOX +
    /// Archive + Drafts + Sent + Trash + Junk.
    ///
    /// Returns the subset of mailboxes that were freshly inserted by
    /// this call (empty Vec on a no-op repeat call). Production
    /// callers participating in the placement journal emit one
    /// `MailPlacementRecord::Create` per entry, before any other
    /// placement event in the same handler — so a fresh actor's first
    /// APPEND / STORE / EXPUNGE finds the seeded mailboxes already
    /// present in `manifest.mailboxes` (spec § D2 record table; spec
    /// § D6 (ε) atomic-with-SQL note). Each mailbox carries the
    /// seeded `uid_validity = 1` and its RFC 6154 SPECIAL-USE
    /// attribute per `standard_mailbox_attrs`.
    pub async fn ensure_bridge_imap_mailboxes(
        &self,
        actor: &[u8; 32],
    ) -> Result<Vec<NewlySeededMailbox>> {
        let actor = *actor;
        let conn = self.conn.lock().await;
        let mut newly_seeded: Vec<NewlySeededMailbox> = Vec::new();
        for name in STANDARD_MAILBOXES {
            let rows_affected = conn
                .execute(
                    "INSERT OR IGNORE INTO bridge_imap_mailbox_state \
                     (actor_id, mailbox, uid_validity, uid_next, highestmodseq) \
                     VALUES (?1, ?2, 1, 1, 1)",
                    rusqlite::params![&actor[..], name],
                )
                .context("ensure bridge imap mailbox")?;
            if rows_affected > 0 {
                newly_seeded.push(NewlySeededMailbox {
                    name: (*name).to_string(),
                    uid_validity: 1,
                    attrs: standard_mailbox_attrs(name),
                });
            }
        }
        Ok(newly_seeded)
    }

    /// Create a user-named mailbox for `actor`. Idempotent in the
    /// already-exists sense: if a row exists (either user-created or
    /// seeded by `ensure_bridge_imap_mailboxes`), returns
    /// `AlreadyExists`. The caller is responsible for rejecting
    /// `STANDARD_MAILBOXES` names with `Reserved` and rejecting
    /// `validate_mailbox_name` failures with `InvalidName` before
    /// calling this — the DB layer assumes the name is acceptable.
    ///
    /// `uid_validity` is supplied by the caller (the handler reads
    /// the wall clock) so this function is deterministic for tests
    /// and the handler owns the freshness policy (per goal-doc
    /// `imap-server.md` CREATE row — `(unix-millis) as u32` truncated
    /// to fit RFC 9051 §2.3.1.1's 32-bit width).
    pub async fn create_bridge_imap_mailbox(
        &self,
        actor: &[u8; 32],
        name: &str,
        uid_validity: u32,
    ) -> Result<CreateMailboxDbOutcome> {
        let actor = *actor;
        let name = name.to_string();
        let conn = self.conn.lock().await;
        let rows_affected = conn
            .execute(
                "INSERT OR IGNORE INTO bridge_imap_mailbox_state \
                     (actor_id, mailbox, uid_validity, uid_next, highestmodseq) \
                     VALUES (?1, ?2, ?3, 1, 1)",
                rusqlite::params![&actor[..], &name, uid_validity as i64],
            )
            .context("create bridge imap mailbox")?;
        if rows_affected == 0 {
            return Ok(CreateMailboxDbOutcome::AlreadyExists);
        }
        Ok(CreateMailboxDbOutcome::Created { uid_validity })
    }

    /// Delete a user-created mailbox for `actor`. The handler must
    /// reject `STANDARD_MAILBOXES` names with `Reserved` before
    /// calling this; this function rejects only the in-DB conditions
    /// `NoSuchMailbox` (no row) and `NotEmpty` (when
    /// `allow_nonempty == false` and the message-placement table has
    /// at least one row).
    ///
    /// When `allow_nonempty == true` the helper tombstones every
    /// placed UID into `bridge_imap_expunged` and deletes the
    /// `bridge_imap_messages` rows in the same transaction as the
    /// mailbox-state-row delete. The tombstones use the post-delete
    /// `highestmodseq` so QRESYNC clients reconnecting after the
    /// delete see the rows as VANISHED EARLIER.
    pub async fn delete_bridge_imap_mailbox(
        &self,
        actor: &[u8; 32],
        name: &str,
        allow_nonempty: bool,
    ) -> Result<DeleteMailboxDbOutcome> {
        let actor = *actor;
        let name = name.to_string();
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction()
            .context("begin delete bridge imap mailbox tx")?;

        // 1. Verify the state row exists.
        let state_exists: Option<(i64, i64, i64)> = tx
            .query_row(
                "SELECT uid_validity, uid_next, highestmodseq \
                     FROM bridge_imap_mailbox_state \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("read bridge imap mailbox state for delete")?;
        let Some((_, _, prev_hms)) = state_exists else {
            return Ok(DeleteMailboxDbOutcome::NoSuchMailbox);
        };

        // 2. Count current placements.
        let placement_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &name],
                |row| row.get(0),
            )
            .context("count bridge imap mailbox placements")?;
        if placement_count > 0 && !allow_nonempty {
            return Ok(DeleteMailboxDbOutcome::NotEmpty);
        }

        // 3. Under `allow_nonempty`, tombstone each placement first.
        if placement_count > 0 {
            let new_hms = prev_hms + 1;
            let expunged_at = now_epoch_secs();
            // Read the UIDs so we can record them in the expunge log.
            let mut stmt = tx
                .prepare(
                    "SELECT uid FROM bridge_imap_messages \
                         WHERE actor_id = ?1 AND mailbox = ?2 \
                         ORDER BY uid ASC",
                )
                .context("prep select uids for delete-tombstone")?;
            let uids: Vec<i64> = stmt
                .query_map(rusqlite::params![&actor[..], &name], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("read uids for delete-tombstone")?;
            drop(stmt);
            for uid in &uids {
                tx.execute(
                    "INSERT OR IGNORE INTO bridge_imap_expunged \
                         (actor_id, mailbox, uid, modseq, expunged_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![&actor[..], &name, uid, new_hms, expunged_at],
                )
                .context("insert delete-tombstone")?;
            }
            tx.execute(
                "DELETE FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &name],
            )
            .context("delete bridge imap messages")?;
        }

        // 4. Drop the mailbox-state row.
        tx.execute(
            "DELETE FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = ?2",
            rusqlite::params![&actor[..], &name],
        )
        .context("delete bridge imap mailbox state row")?;

        tx.commit().context("commit delete bridge imap mailbox")?;
        Ok(DeleteMailboxDbOutcome::Deleted)
    }

    /// Rename a mailbox for `actor`. Two shapes:
    ///   * `old_name == "INBOX"` — RFC 9051 §6.3.6 special-case: move
    ///     all INBOX placements to `new_name` (a fresh mailbox row
    ///     with the supplied `new_uid_validity`); re-seed an empty
    ///     INBOX row with `inbox_uid_validity` (caller supplies, the
    ///     handler picks a fresh value per the same scheme as
    ///     CREATE); both rows reset `uid_next = 1` and
    ///     `highestmodseq = 1`. The INBOX-tombstone path is NOT
    ///     traversed — the UIDs migrate intact to `new_name`, which
    ///     a QRESYNC-aware client will rediscover via UIDVALIDITY
    ///     re-sync. (RFC 9051 §6.3.6 does not require a tombstone
    ///     trail for INBOX-rename.)
    ///   * `old_name != "INBOX"` — flat rename: update
    ///     `bridge_imap_mailbox_state.mailbox = new_name`; update
    ///     every `bridge_imap_messages.mailbox = new_name` for the
    ///     actor; preserve `uid_validity`. The handler must reject
    ///     `STANDARD_MAILBOXES` non-INBOX names as `ReservedSource`
    ///     before calling.
    ///
    /// For both shapes: rejects if `new_name` already exists for the
    /// actor (`TargetExists`); rejects if `old_name` does not exist
    /// (`NoSuchSource`).
    pub async fn rename_bridge_imap_mailbox(
        &self,
        actor: &[u8; 32],
        old_name: &str,
        new_name: &str,
        new_uid_validity: u32,
        inbox_uid_validity: u32,
    ) -> Result<RenameMailboxDbOutcome> {
        let actor = *actor;
        let old_name = old_name.to_string();
        let new_name = new_name.to_string();
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction()
            .context("begin rename bridge imap mailbox tx")?;

        // 1. Verify old exists.
        let old_state: Option<i64> = tx
            .query_row(
                "SELECT uid_validity FROM bridge_imap_mailbox_state \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &old_name],
                |row| row.get(0),
            )
            .optional()
            .context("read old state row")?;
        if old_state.is_none() {
            return Ok(RenameMailboxDbOutcome::NoSuchSource);
        }

        // 2. Verify new does NOT already exist.
        let new_exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM bridge_imap_mailbox_state \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &new_name],
                |row| row.get(0),
            )
            .optional()
            .context("check new state row")?;
        if new_exists.is_some() {
            return Ok(RenameMailboxDbOutcome::TargetExists);
        }

        if old_name == "INBOX" {
            // INBOX-rename special-case: migrate placements to a fresh
            // `new_name` row, re-seed empty INBOX.
            //
            // (a) Insert the new mailbox-state row carrying the
            //     supplied `new_uid_validity` + reset counters.
            tx.execute(
                "INSERT INTO bridge_imap_mailbox_state \
                     (actor_id, mailbox, uid_validity, uid_next, highestmodseq) \
                     VALUES (?1, ?2, ?3, 1, 1)",
                rusqlite::params![&actor[..], &new_name, new_uid_validity as i64],
            )
            .context("insert new mailbox-state row for INBOX-rename")?;

            // (b) Re-allocate UIDs in the new mailbox for every
            //     existing INBOX placement, in ascending UID order
            //     so the new UIDs preserve the original ordering.
            let mut stmt = tx
                .prepare(
                    "SELECT uid, message_id, flags, modseq, internal_date, created_at, \
                            from_norm, to_norm, cc_norm, subject_norm \
                         FROM bridge_imap_messages \
                         WHERE actor_id = ?1 AND mailbox = 'INBOX' \
                         ORDER BY uid ASC",
                )
                .context("prep select INBOX placements")?;
            #[allow(clippy::type_complexity)]
            let rows: Vec<(
                i64,
                Vec<u8>,
                String,
                i64,
                i64,
                i64,
                String,
                String,
                String,
                String,
            )> = stmt
                .query_map(rusqlite::params![&actor[..]], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("read INBOX placements")?;
            drop(stmt);

            let mut next_uid: u32 = 1;
            let mut next_modseq: i64 = 1;
            for (
                _old_uid,
                message_id,
                flags,
                _old_modseq,
                internal_date,
                created_at,
                from_n,
                to_n,
                cc_n,
                subject_n,
            ) in rows
            {
                tx.execute(
                    "INSERT INTO bridge_imap_messages \
                         (actor_id, mailbox, uid, message_id, flags, modseq, internal_date, created_at, \
                          from_norm, to_norm, cc_norm, subject_norm) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    rusqlite::params![
                        &actor[..],
                        &new_name,
                        next_uid as i64,
                        &message_id,
                        &flags,
                        next_modseq,
                        internal_date,
                        created_at,
                        &from_n,
                        &to_n,
                        &cc_n,
                        &subject_n,
                    ],
                )
                .context("insert migrated INBOX placement")?;
                next_uid += 1;
                next_modseq += 1;
            }
            tx.execute(
                "DELETE FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![&actor[..]],
            )
            .context("delete original INBOX placements")?;

            // (c) Bump counters on the new mailbox to match the
            //     migrated row count.
            tx.execute(
                "UPDATE bridge_imap_mailbox_state \
                     SET uid_next = ?1, highestmodseq = ?2 \
                     WHERE actor_id = ?3 AND mailbox = ?4",
                rusqlite::params![next_uid as i64, next_modseq, &actor[..], &new_name],
            )
            .context("bump new mailbox counters after INBOX migration")?;

            // (d) Re-seed the empty INBOX row with a freshly-allocated
            //     uid_validity. uid_next + highestmodseq reset to 1
            //     per the plan (cleaner semantics than continuing
            //     pre-rename counters; RFC §6.3.6 leaves it to
            //     implementation choice).
            tx.execute(
                "UPDATE bridge_imap_mailbox_state \
                     SET uid_validity = ?1, uid_next = 1, highestmodseq = 1 \
                     WHERE actor_id = ?2 AND mailbox = 'INBOX'",
                rusqlite::params![inbox_uid_validity as i64, &actor[..]],
            )
            .context("re-seed empty INBOX after rename")?;
        } else {
            // Flat rename: preserve uid_validity, just relabel the
            // mailbox name on the state row + every dependent row.
            tx.execute(
                "UPDATE bridge_imap_mailbox_state \
                     SET mailbox = ?1 \
                     WHERE actor_id = ?2 AND mailbox = ?3",
                rusqlite::params![&new_name, &actor[..], &old_name],
            )
            .context("rename bridge imap mailbox-state row")?;
            tx.execute(
                "UPDATE bridge_imap_messages \
                     SET mailbox = ?1 \
                     WHERE actor_id = ?2 AND mailbox = ?3",
                rusqlite::params![&new_name, &actor[..], &old_name],
            )
            .context("rename bridge imap message rows")?;
        }

        tx.commit().context("commit rename bridge imap mailbox")?;
        Ok(RenameMailboxDbOutcome::Renamed)
    }

    /// Place an encrypted inbound mail message into `(actor, mailbox)`.
    ///
    /// Returns `Ok(None)` when `content_was_new` is `false` — a transport
    /// retry of a duplicate ingest the MTA has already delivered: no UID
    /// allocation, no placement row, no modseq bump. The caller must NOT
    /// emit a duplicate `MailPlacementRecord::Append` in this case (the
    /// original ingest already wrote one).
    ///
    /// Returns `Ok(Some((uid, modseq)))` on a fresh placement, where:
    ///
    /// 1. The mailbox-state row was ensured (auto-creates for non-standard
    ///    mailboxes like "Archive").
    /// 2. `uid` and the new `modseq` were allocated by bumping
    ///    `uid_next` / `highestmodseq` in `bridge_imap_mailbox_state`.
    /// 3. The placement row was inserted into `bridge_imap_messages`.
    ///
    /// `modseq` is the freshly-bumped value the caller passes through to
    /// `MailPlacementRecord::Append::modseq` (spec § D2).
    ///
    /// `internal_date` is epoch **seconds** (IMAP INTERNALDATE); `created_at`
    /// is epoch millis from `now_epoch_millis()`. `sender_domain` populates
    /// the `from_norm` column case-folded for the SEARCH header axis; in
    /// encrypted mode this is the only `*_norm` column the ingest path
    /// can populate (the MTA has no MSEK and the other headers are sealed
    /// inside the body) — to/cc/subject_norm stay empty.
    pub async fn place_inbound_mail(
        &self,
        actor: &[u8; 32],
        message_id: &[u8; 32],
        mailbox: &str,
        internal_date: i64,
        initial_flags: &str,
        sender_domain: &str,
        content_was_new: bool,
    ) -> Result<Option<(u32, i64)>> {
        self.place_inbound_mail_inner(
            actor,
            message_id,
            mailbox,
            internal_date,
            initial_flags,
            sender_domain,
            content_was_new,
            None,
        )
        .await
    }

    /// Place a **held** inbound message into the ward's [`GUARDIAN_HELD_MAILBOX`]
    /// and write its `guardian_mail_holds` envelope sidecar **in one
    /// transaction** (`family-safety.md` § The mail gate).
    ///
    /// The two must never diverge: the message's *presence in the held mailbox*
    /// **is** the hold, and the sidecar is the one fact the guardian's queue
    /// renders and the approve path allowlists. A sidecar without a placement
    /// would show the guardian a phantom entry; a placement without a sidecar
    /// would strand mail in a mailbox no queue entry can release.
    ///
    /// `sender_address` must already be normalized (`normalize_mail_address`) so
    /// the approve path's allowlist insert matches this row byte-for-byte.
    /// Flags are empty — a held message is unread.
    pub async fn place_held_inbound_mail(
        &self,
        actor: &[u8; 32],
        message_id: &[u8; 32],
        internal_date: i64,
        sender_domain: &str,
        sender_address: &str,
        content_was_new: bool,
    ) -> Result<Option<(u32, i64)>> {
        self.place_inbound_mail_inner(
            actor,
            message_id,
            GUARDIAN_HELD_MAILBOX,
            internal_date,
            "",
            sender_domain,
            content_was_new,
            Some(sender_address),
        )
        .await
    }

    /// The shared body of [`Self::place_inbound_mail`] and
    /// [`Self::place_held_inbound_mail`]. `hold_sender` is `Some` only on the
    /// held path, where it adds the sidecar INSERT to the same transaction.
    #[allow(clippy::too_many_arguments)]
    async fn place_inbound_mail_inner(
        &self,
        actor: &[u8; 32],
        message_id: &[u8; 32],
        mailbox: &str,
        internal_date: i64,
        initial_flags: &str,
        sender_domain: &str,
        content_was_new: bool,
        hold_sender: Option<&str>,
    ) -> Result<Option<(u32, i64)>> {
        if !content_was_new {
            return Ok(None);
        }
        let actor = *actor;
        let message_id = *message_id;
        let mailbox = mailbox.to_string();
        let initial_flags = initial_flags.to_string();
        let from_norm = norm_for_storage(sender_domain);
        let created_at = now_epoch_millis();
        let conn = self.conn.lock().await;
        // The UID allocation, the placement row and (on the held path) the
        // envelope sidecar commit together or not at all.
        let tx = conn
            .unchecked_transaction()
            .context("begin place_inbound_mail tx")?;

        // 1.–3. Ensure the state row (handles non-standard mailbox names — the
        //    held mailbox is auto-created here on a ward's first hold), allocate
        //    the UID + modseq, insert the placement row.
        let (new_uid, new_modseq) = place_new_message_in(
            &tx,
            &actor,
            &message_id,
            &mailbox,
            internal_date,
            &initial_flags,
            &from_norm,
            created_at,
        )
        .context("place bridge imap message")?;

        // 4. Held path only: the envelope sidecar, atomic with the placement.
        if let Some(sender_address) = hold_sender {
            tx.execute(
                "INSERT OR IGNORE INTO guardian_mail_holds \
                     (message_id, supervised_actor_id, sender_address, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    &message_id[..],
                    &actor[..],
                    sender_address,
                    created_at / 1000,
                ],
            )
            .context("insert guardian mail hold sidecar")?;
        }

        tx.commit().context("commit place_inbound_mail")?;
        Ok(Some((new_uid, new_modseq)))
    }

    /// The UID of `message_id` in `actor`'s `mailbox`, or `None` when it is not
    /// placed there (already moved, expunged, or never placed). Used by the
    /// guardian mail-hold release/discard paths, which key on the stable
    /// `message_id` rather than on a per-mailbox UID that a move invalidates.
    pub async fn find_message_uid(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
        message_id: &[u8; 32],
    ) -> Result<Option<u32>> {
        let actor = *actor;
        let message_id = *message_id;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        let uid: Option<i64> = conn
            .query_row(
                "SELECT uid FROM bridge_imap_messages \
                 WHERE actor_id = ?1 AND mailbox = ?2 AND message_id = ?3",
                rusqlite::params![&actor[..], &mailbox, &message_id[..]],
                |row| row.get(0),
            )
            .optional()
            .context("find message uid")?;
        Ok(uid.map(|u| u as u32))
    }

    /// The UIDs in `actor`'s [`GUARDIAN_HELD_MAILBOX`] whose message still has
    /// a live `guardian_mail_holds` sidecar row — the set the IMAP handler
    /// gates MOVE/EXPUNGE on (`family-safety.md` § The mail gate). Keyed on
    /// the sidecar, never on mailbox membership: a non-held message the ward
    /// parked in the mailbox is not returned. `uids` empty = the whole
    /// mailbox; non-empty = restrict to those UIDs.
    pub async fn list_held_uids(&self, actor: &[u8; 32], uids: &[u32]) -> Result<Vec<u32>> {
        let actor = *actor;
        let uid_filter: Vec<u32> = uids.to_vec();
        let conn = self.conn.lock().await;
        let sql = if uid_filter.is_empty() {
            "SELECT m.uid FROM bridge_imap_messages m \
             JOIN guardian_mail_holds h \
               ON h.message_id = m.message_id AND h.supervised_actor_id = m.actor_id \
             WHERE m.actor_id = ?1 AND m.mailbox = ?2 \
             ORDER BY m.uid ASC"
                .to_string()
        } else {
            let uid_csv: String = uid_filter
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "SELECT m.uid FROM bridge_imap_messages m \
                 JOIN guardian_mail_holds h \
                   ON h.message_id = m.message_id AND h.supervised_actor_id = m.actor_id \
                 WHERE m.actor_id = ?1 AND m.mailbox = ?2 AND m.uid IN ({uid_csv}) \
                 ORDER BY m.uid ASC"
            )
        };
        let mut stmt = conn.prepare(&sql).context("list_held_uids: prepare")?;
        let rows = stmt
            .query_map(
                rusqlite::params![&actor[..], GUARDIAN_HELD_MAILBOX],
                |row| Ok(row.get::<_, i64>(0)? as u32),
            )
            .context("list_held_uids: query")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list_held_uids: collect")?;
        Ok(rows)
    }

    /// Return `true` iff `message_id` is currently placed in at
    /// least one of `actor`'s mailboxes (any mailbox, not just
    /// INBOX). Reads `bridge_imap_messages` via
    /// `idx_bridge_imap_msgs_msgid (actor_id, message_id)` — an
    /// indexed point lookup.
    pub async fn bridge_imap_message_placed_for_actor(
        &self,
        actor: &[u8; 32],
        message_id: &[u8; 32],
    ) -> Result<bool> {
        let actor = *actor;
        let msg_id = *message_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND message_id = ?2 LIMIT 1",
            )
            .context("prepare bridge_imap_message_placed_for_actor")?;
        let found = stmt
            .query_row(rusqlite::params![&actor[..], &msg_id[..]], |_| Ok(()))
            .optional()
            .context("query bridge_imap_message_placed_for_actor")?
            .is_some();
        Ok(found)
    }

    /// Return all `bridge_imap_mailbox_state` rows for `actor`, ordered
    /// alphabetically by mailbox name.
    pub async fn list_bridge_imap_mailbox_state(
        &self,
        actor: &[u8; 32],
    ) -> Result<Vec<MailboxStateRow>> {
        let actor = *actor;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT mailbox, uid_validity, uid_next, highestmodseq \
                     FROM bridge_imap_mailbox_state \
                     WHERE actor_id = ?1 \
                     ORDER BY mailbox ASC",
            )
            .context("prepare list_bridge_imap_mailbox_state")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], |row| {
                Ok(MailboxStateRow {
                    name: row.get(0)?,
                    uid_validity: row.get::<_, i64>(1)? as u32,
                    uid_next: row.get::<_, i64>(2)? as u32,
                    highestmodseq: row.get(3)?,
                })
            })
            .context("query list_bridge_imap_mailbox_state")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_bridge_imap_mailbox_state")?;
        Ok(rows)
    }

    /// Return the `bridge_imap_mailbox_state` rows for `actor` whose
    /// `mailbox` name appears in `bridge_imap_subscriptions`, ordered
    /// alphabetically. Backs `LSUB` and `LIST (SUBSCRIBED)` via
    /// `ListMailboxesRequest { subscribed_only: true }`. A
    /// subscription that points at a mailbox without a matching
    /// state row (legal per RFC 9051 §6.3.7 — SUBSCRIBE before
    /// CREATE, or after DELETE) is silently dropped here; the LSUB
    /// surface returns only mailboxes that currently exist. (This
    /// matches the documented MUA expectation; clients that need to
    /// see deleted-but-subscribed mailboxes track them client-side.)
    pub async fn list_bridge_imap_subscribed_mailbox_state(
        &self,
        actor: &[u8; 32],
    ) -> Result<Vec<MailboxStateRow>> {
        let actor = *actor;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT s.mailbox, s.uid_validity, s.uid_next, s.highestmodseq \
                     FROM bridge_imap_mailbox_state AS s \
                     INNER JOIN bridge_imap_subscriptions AS sub \
                         ON sub.actor_id = s.actor_id AND sub.mailbox = s.mailbox \
                     WHERE s.actor_id = ?1 \
                     ORDER BY s.mailbox ASC",
            )
            .context("prepare list_bridge_imap_subscribed_mailbox_state")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], |row| {
                Ok(MailboxStateRow {
                    name: row.get(0)?,
                    uid_validity: row.get::<_, i64>(1)? as u32,
                    uid_next: row.get::<_, i64>(2)? as u32,
                    highestmodseq: row.get(3)?,
                })
            })
            .context("query list_bridge_imap_subscribed_mailbox_state")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_bridge_imap_subscribed_mailbox_state")?;
        Ok(rows)
    }

    /// Insert one row into `bridge_imap_subscriptions`. Idempotent on
    /// PK conflict (`INSERT OR IGNORE`) — RFC 9051 §6.3.7 requires
    /// SUBSCRIBE-twice to succeed. The bridge does not pre-check
    /// mailbox existence; the spec explicitly permits SUBSCRIBE on a
    /// not-yet-CREATEd or already-DELETEd mailbox.
    pub async fn insert_bridge_imap_subscription(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
    ) -> Result<()> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO bridge_imap_subscriptions (actor_id, mailbox) \
                 VALUES (?1, ?2)",
            rusqlite::params![&actor[..], &mailbox],
        )
        .context("insert_bridge_imap_subscription")?;
        Ok(())
    }

    /// Delete one row from `bridge_imap_subscriptions`. Idempotent —
    /// missing rows are not an error per RFC 9051 §6.3.8 (UNSUBSCRIBE
    /// on a not-subscribed mailbox succeeds).
    pub async fn delete_bridge_imap_subscription(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
    ) -> Result<()> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM bridge_imap_subscriptions \
                 WHERE actor_id = ?1 AND mailbox = ?2",
            rusqlite::params![&actor[..], &mailbox],
        )
        .context("delete_bridge_imap_subscription")?;
        Ok(())
    }

    /// Return the state row for a single `(actor, mailbox)`, or `None` if it
    /// doesn't exist.
    pub async fn get_bridge_imap_mailbox_state(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
    ) -> Result<Option<MailboxStateRow>> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT mailbox, uid_validity, uid_next, highestmodseq \
                     FROM bridge_imap_mailbox_state \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &mailbox],
                |row| {
                    Ok(MailboxStateRow {
                        name: row.get(0)?,
                        uid_validity: row.get::<_, i64>(1)? as u32,
                        uid_next: row.get::<_, i64>(2)? as u32,
                        highestmodseq: row.get(3)?,
                    })
                },
            )
            .optional()
            .context("get_bridge_imap_mailbox_state")?;
        Ok(row)
    }

    /// Count messages in `(actor, mailbox)`.
    ///
    /// Returns `(exists, unseen)` where:
    /// - `exists` = total rows in `bridge_imap_messages` for this (actor, mailbox)
    /// - `unseen` = rows whose space-separated `flags` string does not contain
    ///   the token `\Seen` (exact byte match; the flag is case-normalised as
    ///   RFC 3501 §2.3.2 specifies `\Seen` with capital S).
    pub async fn count_bridge_imap_mailbox(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
    ) -> Result<(u32, u32)> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT flags FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = ?2",
            )
            .context("prepare count_bridge_imap_mailbox")?;
        let flags_list: Vec<String> = stmt
            .query_map(rusqlite::params![&actor[..], &mailbox], |row| {
                row.get::<_, String>(0)
            })
            .context("query count_bridge_imap_mailbox")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect count_bridge_imap_mailbox")?;
        let exists = flags_list.len() as u32;
        let unseen = flags_list
            .iter()
            .filter(|flags| !flags.split_whitespace().any(|t| t == "\\Seen"))
            .count() as u32;
        Ok((exists, unseen))
    }

    /// Return the lowest UID in `(actor, mailbox)` whose flags lack `\Seen`,
    /// or `None` if all messages have been seen (or the mailbox is empty).
    pub async fn first_unseen_uid_in_mailbox(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
    ) -> Result<Option<u32>> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT uid, flags FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = ?2 \
                     ORDER BY uid ASC",
            )
            .context("prepare first_unseen_uid_in_mailbox")?;
        let rows: Vec<(u32, String)> = stmt
            .query_map(rusqlite::params![&actor[..], &mailbox], |row| {
                Ok((row.get::<_, i64>(0)? as u32, row.get::<_, String>(1)?))
            })
            .context("query first_unseen_uid_in_mailbox")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect first_unseen_uid_in_mailbox")?;
        Ok(rows
            .into_iter()
            .find(|(_, flags)| !flags.split_whitespace().any(|t| t == "\\Seen"))
            .map(|(uid, _)| uid))
    }

    /// Query `bridge_imap_messages JOIN segment_records` (kind='mail') for the
    /// given `(actor, mailbox)`, returning `MessageMetaRow`s ordered by uid
    /// ascending. Each row carries the record's `segment_id`; the handler
    /// derives RFC822.SIZE from the CARv2 index by `record_cid`
    /// (`segments::record_sizes`), not a SQL byte column.
    ///
    /// Optional filters:
    /// - `since_modseq`: only rows with `modseq > since_modseq` (CONDSTORE).
    /// - `after_uid`: only rows with `uid > after_uid` (pagination resume token).
    /// - `uids`: only rows whose uid is in the given slice. `Some(&[])` (empty
    ///   slice) is treated identically to `None` — no UID filter — to match the
    ///   `FetchMessageMetadataRequest.uids = vec![]` semantics ("all messages").
    /// - `limit`: `None` or `Some(0)` = no LIMIT clause; otherwise `LIMIT n`.
    ///
    /// Use `LIMIT req.limit + 1` in the caller for pagination detection: if
    /// `rows.len() > req.limit`, there are more pages.
    pub async fn query_bridge_imap_messages(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
        since_modseq: Option<i64>,
        after_uid: Option<u32>,
        uids: Option<&[u32]>,
        limit: Option<u32>,
    ) -> Result<Vec<MessageMetaRow>> {
        let actor = *actor;
        let mailbox = mailbox.to_string();

        // Treat Some(&[]) the same as None — "all messages", no UID filter.
        let uids_filter = uids.filter(|s| !s.is_empty());

        // Build the query dynamically based on which optional filters are set.
        // JOIN onto `segment_records` (kind = 'mail') instead of the dropped
        // `bridge_inbound_mail` table. Projecting `sr.segment_id` + `sr.record_cid`
        // (size is never a SQL column) lets the handler size each
        // record by the full Cid through the CARv2 index (`imap-server.md`
        // § SEARCH). The mirror key is the 36-byte `record_cid`; the wire
        // `message_id` is its 32-byte digest tail, so the join compares
        // `substr(sr.record_cid, 5) = m.message_id`. `sr.scope_id = m.actor_id`
        // keeps the join inside the actor's mail rows via the PK prefix.
        // Inner query = the full live mailbox, ranked by ascending UID via
        // ROW_NUMBER so each row carries its true IMAP sequence number
        // (RFC 9051 §6.4.5) — the rank over the WHOLE mailbox, not the
        // filtered subset. The since/after/uid/limit filters are applied in
        // the OUTER query so a UID-subset fetch still returns each row's
        // correct full-mailbox seqNum (F1: lets the Go bridge stop
        // re-fetching the whole mailbox just to number a few rows).
        let mut sql = String::from(
            "SELECT uid, message_id, modseq, flags, internal_date, \
                    segment_id, record_cid, seq_num, stored_at \
             FROM ( \
                SELECT m.uid AS uid, m.message_id AS message_id, m.modseq AS modseq, \
                       m.flags AS flags, m.internal_date AS internal_date, \
                       sr.segment_id AS segment_id, sr.record_cid AS record_cid, \
                       COALESCE(sr.stored_at, 0) AS stored_at, \
                       ROW_NUMBER() OVER (ORDER BY m.uid ASC) AS seq_num \
                FROM bridge_imap_messages m \
                JOIN segment_records sr \
                  ON sr.kind = 'mail' \
                 AND sr.scope_id = m.actor_id \
                 AND substr(sr.record_cid, 5) = m.message_id \
                 AND sr.tombstoned = 0 \
                WHERE m.actor_id = ?1 AND m.mailbox = ?2 \
             ) WHERE 1 = 1",
        );
        let mut param_idx = 3usize;
        let mut since_param: Option<i64> = None;
        let mut after_param: Option<u32> = None;

        if let Some(seq) = since_modseq {
            sql.push_str(&format!(" AND modseq > ?{param_idx}"));
            since_param = Some(seq);
            param_idx += 1;
        }
        if let Some(uid) = after_uid {
            sql.push_str(&format!(" AND uid > ?{param_idx}"));
            after_param = Some(uid);
            param_idx += 1;
        }

        // UID IN-list: build as literal CSV since rusqlite params_from_iter
        // can't be spliced into a positional IN-list cleanly without unsafe
        // SQL injection risk. UIDs are u32s (not user strings), so this is safe.
        if let Some(uid_slice) = uids_filter {
            let list: Vec<String> = uid_slice.iter().map(|u| u.to_string()).collect();
            sql.push_str(&format!(" AND uid IN ({})", list.join(",")));
        }

        sql.push_str(" ORDER BY uid ASC");

        let limit_val = limit.filter(|&n| n > 0);
        if let Some(n) = limit_val {
            sql.push_str(&format!(" LIMIT {n}"));
        }

        let _ = param_idx; // consumed above

        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare query_bridge_imap_messages")?;

        let rows = match (since_param, after_param) {
            (Some(s), Some(a)) => stmt
                .query_map(
                    rusqlite::params![&actor[..], &mailbox, s, a as i64],
                    map_message_meta_row,
                )
                .context("query bridge imap messages (since+after)")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect bridge imap messages (since+after)")?,
            (Some(s), None) => stmt
                .query_map(
                    rusqlite::params![&actor[..], &mailbox, s],
                    map_message_meta_row,
                )
                .context("query bridge imap messages (since)")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect bridge imap messages (since)")?,
            (None, Some(a)) => stmt
                .query_map(
                    rusqlite::params![&actor[..], &mailbox, a as i64],
                    map_message_meta_row,
                )
                .context("query bridge imap messages (after)")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect bridge imap messages (after)")?,
            (None, None) => stmt
                .query_map(
                    rusqlite::params![&actor[..], &mailbox],
                    map_message_meta_row,
                )
                .context("query bridge imap messages (base)")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect bridge imap messages (base)")?,
        };
        Ok(rows)
    }

    /// Count of live (non-tombstoned) messages in `(actor, mailbox)` — the
    /// IMAP mailbox total (EXISTS). Uses the same live join
    /// `query_bridge_imap_messages` ranks over, so this total and the per-row
    /// `seq_num` agree. Cheap indexed COUNT; lets the IDLE present-UID events
    /// (Append/Move-dst) resolve EXISTS with a single-UID metadata fetch
    /// instead of a whole-mailbox snapshot (F1).
    pub async fn count_bridge_imap_live_messages(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
    ) -> Result<u32> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) \
                 FROM bridge_imap_messages m \
                 JOIN segment_records sr \
                   ON sr.kind = 'mail' \
                  AND sr.scope_id = m.actor_id \
                  AND substr(sr.record_cid, 5) = m.message_id \
                  AND sr.tombstoned = 0 \
                 WHERE m.actor_id = ?1 AND m.mailbox = ?2",
                rusqlite::params![&actor[..], &mailbox],
                |row| row.get(0),
            )
            .context("count bridge imap live messages")?;
        Ok(n as u32)
    }

    /// Run a conjunctive IMAP SEARCH query against
    /// `bridge_imap_messages JOIN segment_records` for `(actor, mailbox)`.
    ///
    /// Every entry in `terms` becomes one SQL `AND` predicate; the
    /// reply is the UID set whose row satisfies all of them, in
    /// ascending UID order. An empty `terms` slice returns every UID
    /// in the mailbox (mirrors the wire-shape: empty terms = no
    /// filter, callers that don't want that should not issue the
    /// RPC).
    ///
    /// Predicate semantics (mirrors `imap-server.md` § SEARCH):
    /// - `HasFlag(f)`  ↔ ` ` -bounded substring match on the
    ///   `flags` column (`flags` is a space-separated token list;
    ///   bounding both sides defends against a substring of a
    ///   keyword matching a system flag and vice versa).
    /// - `LacksFlag(f)` ↔ negation of the same.
    /// - `HeaderContains(field, v)` ↔ `instr(field_norm, lower(v)) > 0`.
    /// - `SinceInternalDate(ts)` ↔ `internal_date >= ts` (inclusive).
    /// - `BeforeInternalDate(ts)` ↔ `internal_date < ts` (exclusive).
    /// - `Larger(n)` / `Smaller(n)` (size axes) are **not** applied here — record
    ///   block length comes from the CARv2 index by `record_cid`, not a SQL
    ///   column (`imap-server.md` § SEARCH). Each hit carries its
    ///   `(segment_id, record_cid)` so the handler sizes candidates through
    ///   `segments::record_sizes` and applies the size predicates as a
    ///   post-filter. The caller partitions size terms out before calling in.
    pub async fn search_bridge_imap_messages(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
        terms: &[SearchTermDb],
    ) -> Result<Vec<SearchHitRow>> {
        let actor = *actor;
        let mailbox = mailbox.to_string();

        let mut sql = String::from(
            "SELECT m.uid, sr.segment_id, sr.record_cid \
             FROM bridge_imap_messages m \
             JOIN segment_records sr \
               ON sr.kind = 'mail' \
              AND sr.scope_id = m.actor_id \
              AND substr(sr.record_cid, 5) = m.message_id \
              AND sr.tombstoned = 0 \
             WHERE m.actor_id = ?1 AND m.mailbox = ?2",
        );

        // Parameters that follow ?1 (actor_id) + ?2 (mailbox).
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        let mut next_idx = 3usize;

        for term in terms {
            match term {
                SearchTermDb::HasFlag(flag) => {
                    // Wrap both sides with spaces so a substring of a
                    // keyword does not accidentally match a system
                    // flag (e.g. searching for "\\Seen" would otherwise
                    // false-positive on a custom "\\Seenarchive"
                    // keyword). Bounding with " " mirrors the canonical
                    // token-list storage convention.
                    sql.push_str(&format!(
                        " AND instr(' ' || m.flags || ' ', ' ' || ?{idx} || ' ') > 0",
                        idx = next_idx
                    ));
                    params.push(rusqlite::types::Value::Text(flag.clone()));
                    next_idx += 1;
                }
                SearchTermDb::LacksFlag(flag) => {
                    sql.push_str(&format!(
                        " AND instr(' ' || m.flags || ' ', ' ' || ?{idx} || ' ') = 0",
                        idx = next_idx
                    ));
                    params.push(rusqlite::types::Value::Text(flag.clone()));
                    next_idx += 1;
                }
                SearchTermDb::HeaderContains(field, value) => {
                    let column = match field {
                        SearchHeaderFieldDb::From => "m.from_norm",
                        SearchHeaderFieldDb::To => "m.to_norm",
                        SearchHeaderFieldDb::Cc => "m.cc_norm",
                        SearchHeaderFieldDb::Subject => "m.subject_norm",
                    };
                    sql.push_str(&format!(" AND instr({column}, ?{idx}) > 0", idx = next_idx));
                    // The handler lowercases the wire value before
                    // calling in; the column is stored case-folded the
                    // same way (see `norm_for_storage`).
                    params.push(rusqlite::types::Value::Text(value.clone()));
                    next_idx += 1;
                }
                SearchTermDb::SinceInternalDate(ts) => {
                    sql.push_str(&format!(" AND m.internal_date >= ?{idx}", idx = next_idx));
                    params.push(rusqlite::types::Value::Integer(*ts));
                    next_idx += 1;
                }
                SearchTermDb::BeforeInternalDate(ts) => {
                    sql.push_str(&format!(" AND m.internal_date < ?{idx}", idx = next_idx));
                    params.push(rusqlite::types::Value::Integer(*ts));
                    next_idx += 1;
                }
                SearchTermDb::Larger(_) | SearchTermDb::Smaller(_) => {
                    // Size axes are post-filtered by the handler against the
                    // CARv2 segment index (record block length is not a SQL
                    // column — imap-server.md § SEARCH). The search_messages
                    // handler partitions these out before calling in, so this
                    // arm is unreachable in practice; ignoring it here keeps the
                    // match exhaustive without re-introducing a byte column read.
                }
            }
        }

        sql.push_str(" ORDER BY m.uid ASC");

        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare search_bridge_imap_messages")?;

        // Build a positional parameter slice. The first two are actor_id
        // and mailbox; the rest come from the `params` Vec built above.
        let mut all_params: Vec<rusqlite::types::Value> = Vec::with_capacity(2 + params.len());
        all_params.push(rusqlite::types::Value::Blob(actor.to_vec()));
        all_params.push(rusqlite::types::Value::Text(mailbox));
        all_params.extend(params);

        let rows = stmt
            .query_map(rusqlite::params_from_iter(all_params.iter()), |row| {
                Ok(SearchHitRow {
                    uid: row.get::<_, i64>(0)? as u32,
                    segment_id: row.get::<_, i64>(1)? as u32,
                    record_cid: cid_from_col(row.get::<_, Vec<u8>>(2)?, 2)?,
                })
            })
            .context("query search_bridge_imap_messages")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect search_bridge_imap_messages")?;
        Ok(rows)
    }

    /// Return the `(segment_id, record_cid)` of each live placement among
    /// `uids` in `(actor, mailbox)`. Drives the COPY/MOVE quota pre-check: the
    /// handler sizes each ref through the CARv2 index
    /// (`segments::record_sizes`) and sums — each copied placement
    /// re-counts its content bytes against the actor's quota root
    /// (`imap-server.md` § Quota root model — "each COPY-duplicated placement
    /// counts"); the placement count is the returned `Vec` length. UIDs without
    /// a live placement (already expunged / never existed) are silently
    /// excluded via the same `bridge_imap_messages ⋈ segment_records
    /// (tombstoned = 0)` join `list_bridge_imap_quota_size_refs` uses, so the
    /// delta lines up with what `apply_copy` would actually copy.
    pub async fn list_bridge_imap_uid_size_refs(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
        uids: &[u32],
    ) -> Result<Vec<(u32, Cid)>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let actor = *actor;
        let mailbox = mailbox.to_string();

        // IN-list placeholders start at ?3 (?1 = actor_id, ?2 = mailbox).
        let placeholders: String = (0..uids.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT sr.segment_id, sr.record_cid \
             FROM bridge_imap_messages m \
             JOIN segment_records sr \
               ON sr.kind = 'mail' \
              AND sr.scope_id = m.actor_id \
              AND substr(sr.record_cid, 5) = m.message_id \
              AND sr.tombstoned = 0 \
             WHERE m.actor_id = ?1 AND m.mailbox = ?2 AND m.uid IN ({placeholders})"
        );

        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(2 + uids.len());
        params.push(rusqlite::types::Value::Blob(actor.to_vec()));
        params.push(rusqlite::types::Value::Text(mailbox));
        for u in uids {
            params.push(rusqlite::types::Value::Integer(*u as i64));
        }

        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare list_bridge_imap_uid_size_refs")?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)? as u32,
                    cid_from_col(row.get::<_, Vec<u8>>(1)?, 1)?,
                ))
            })
            .context("query list_bridge_imap_uid_size_refs")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_bridge_imap_uid_size_refs")?;
        Ok(rows)
    }

    /// Return the `(segment_id, record_cid)` of every live (non-tombstoned)
    /// mail placement the actor owns across every mailbox — the rows the
    /// handler sizes through the CARv2 index (`segments::record_sizes`)
    /// for the actor's QUOTA `storage_bytes_used`. The `message_count_used` is
    /// the returned `Vec` length: each placement counts toward storage,
    /// mirroring Dovecot's QUOTA accounting for COPY-duplicated messages.
    ///
    /// Tombstoned segments are excluded — they represent expunged content
    /// that's been GC'd from the storage accounting.
    pub async fn list_bridge_imap_quota_size_refs(
        &self,
        actor: &[u8; 32],
    ) -> Result<Vec<(u32, Cid)>> {
        let actor = *actor;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT sr.segment_id, sr.record_cid \
                 FROM bridge_imap_messages m \
                 JOIN segment_records sr \
                   ON sr.kind = 'mail' \
                  AND sr.scope_id = m.actor_id \
                  AND substr(sr.record_cid, 5) = m.message_id \
                  AND sr.tombstoned = 0 \
                 WHERE m.actor_id = ?1",
            )
            .context("prepare list_bridge_imap_quota_size_refs")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u32,
                    cid_from_col(row.get::<_, Vec<u8>>(1)?, 1)?,
                ))
            })
            .context("query list_bridge_imap_quota_size_refs")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_bridge_imap_quota_size_refs")?;
        Ok(rows)
    }

    /// Return UIDs of messages expunged from `(actor, mailbox)` after
    /// `since_modseq`, ordered by modseq ascending — or `None` when
    /// `since_modseq` is below the mailbox's prune floor
    /// (`bridge_imap_mailbox_state.pruned_modseq`, set by a restore from a
    /// retention-pruned placement manifest): the log is then missing at
    /// least one expunge after `since_modseq`, and a partial list would tell
    /// a QRESYNC client a deleted message still exists (`imap-server.md`
    /// § QRESYNC — no VANISHED, `OK [HIGHESTMODSEQ]` only).
    pub async fn list_bridge_imap_expunged_since(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
        since_modseq: i64,
    ) -> Result<Option<Vec<u32>>> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        let floor: i64 = conn
            .query_row(
                "SELECT pruned_modseq FROM bridge_imap_mailbox_state \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &mailbox],
                |row| row.get(0),
            )
            .optional()
            .context("read bridge_imap_mailbox_state.pruned_modseq")?
            .unwrap_or(0);
        if since_modseq < floor {
            return Ok(None);
        }
        let mut stmt = conn
            .prepare(
                "SELECT uid FROM bridge_imap_expunged \
                     WHERE actor_id = ?1 AND mailbox = ?2 AND modseq > ?3 \
                     ORDER BY modseq ASC",
            )
            .context("prepare list_bridge_imap_expunged_since")?;
        let uids = stmt
            .query_map(
                rusqlite::params![&actor[..], &mailbox, since_modseq],
                |row| Ok(row.get::<_, i64>(0)? as u32),
            )
            .context("query list_bridge_imap_expunged_since")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list_bridge_imap_expunged_since")?;
        Ok(Some(uids))
    }

    /// Query `bridge_imap_messages JOIN segment_records` for the given
    /// actor, returning `IndexSegmentRow`s ordered by modseq ascending.
    /// The sealed `encrypted_index_hint` per row is decoded from each
    /// record's `MailRecordEnvelope` on the segment store; T9 cutover
    /// replaced the prior `bridge_inbound_mail` lookup.
    ///
    /// Optional filters:
    /// - `mailbox`: when `Some`, restricts to a single mailbox; when `None`,
    ///   returns rows across all of the actor's mailboxes.
    /// - `since_modseq`: only rows with `modseq > since_modseq`.
    /// - `limit`: `None` = no LIMIT; `Some(n)` applies `LIMIT n`.
    ///
    /// The handler passes `None` when the wire `limit == 0` (no limit).
    /// For pagination detection the handler passes `Some(req.limit + 1)`.
    pub async fn query_bridge_imap_index_segments(
        &self,
        manager: &SegmentManager,
        actor: &[u8; 32],
        mailbox: Option<&str>,
        since_modseq: i64,
        limit: Option<u32>,
    ) -> Result<Vec<IndexSegmentRow>> {
        // 1. SQL: pull (message_id, mailbox, modseq) for the actor.
        //    JOIN segment_records so we only surface rows whose record
        //    still lives in the segment store (tombstoned=0). The
        //    envelope read in step 2 supplies the index hint.
        #[derive(Debug)]
        struct Meta {
            message_id: [u8; 32],
            mailbox: String,
            modseq: i64,
            stored_at: i64,
        }
        let actor_owned = *actor;
        let mailbox_owned: Option<String> = mailbox.map(|s| s.to_string());

        let mut sql = String::from(
            "SELECT im.message_id, im.mailbox, im.modseq, COALESCE(sr.stored_at, 0) \
             FROM bridge_imap_messages im \
             JOIN segment_records sr \
               ON sr.kind = 'mail' \
              AND sr.scope_id = im.actor_id \
              AND substr(sr.record_cid, 5) = im.message_id \
              AND sr.tombstoned = 0 \
             WHERE im.actor_id = ?1 AND im.modseq > ?2",
        );
        if mailbox_owned.is_some() {
            sql.push_str(" AND im.mailbox = ?3");
        }
        sql.push_str(" ORDER BY im.modseq ASC");
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }

        let metas: Vec<Meta> = {
            let conn = self.conn.lock().await;
            let mut stmt = conn
                .prepare(&sql)
                .context("prepare query_bridge_imap_index_segments")?;
            let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Meta> {
                let message_id: [u8; 32] = blob_col_to_array(row.get(0)?, 0, "message_id")?;
                Ok(Meta {
                    message_id,
                    mailbox: row.get(1)?,
                    modseq: row.get(2)?,
                    stored_at: row.get(3)?,
                })
            };
            if let Some(ref mb) = mailbox_owned {
                stmt.query_map(
                    rusqlite::params![&actor_owned[..], since_modseq, mb],
                    map_row,
                )
                .context("query bridge imap index segments (with mailbox)")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect bridge imap index segments (with mailbox)")?
            } else {
                stmt.query_map(rusqlite::params![&actor_owned[..], since_modseq], map_row)
                    .context("query bridge imap index segments (all mailboxes)")?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .context("collect bridge imap index segments (all mailboxes)")?
            }
        };

        if metas.is_empty() {
            return Ok(Vec::new());
        }

        // 2. Bulk read the envelopes. The manager groups by segment so
        //    we open each segment at most once.
        let ids: Vec<&[u8]> = metas.iter().map(|m| m.message_id.as_slice()).collect();
        let envs = crate::segments::mail::read_envelopes_bulk(manager, self, &actor_owned, &ids)
            .await
            .context("bulk read envelopes for index-segments query")?;

        // 3. Decode each envelope; skip rows whose segment file diverged
        //    from the segment_records mirror (loud-log).
        let mut out: Vec<IndexSegmentRow> = Vec::with_capacity(metas.len());
        for (m, env_opt) in metas.into_iter().zip(envs) {
            let Some(env_bytes) = env_opt else {
                tracing::warn!(
                    actor = ?actor_owned,
                    message_id = ?m.message_id,
                    "segment_records mirror points at a missing record"
                );
                continue;
            };
            // The sealed index hint rides inline on both an inline record and a
            // v3 continuation head (the body is what moves to parts, not the
            // hint), so extract it from either shape — a head must not abort the
            // whole index feed. (message-segment-store.md § Continuation records.)
            let encrypted_index_hint = match fauna_mail::segments::MailRecord::decode(&env_bytes)
                .map_err(|e| anyhow::anyhow!("decode mail record: {e}"))
                .context("decode envelope for index-segments query")?
            {
                fauna_mail::segments::MailRecord::Inline(env) => env.encrypted_index_hint,
                fauna_mail::segments::MailRecord::Head(head) => head.encrypted_index_hint,
            };
            out.push(IndexSegmentRow {
                message_id: m.message_id,
                mailbox: m.mailbox,
                modseq: m.modseq,
                encrypted_index_hint,
                stored_at: m.stored_at,
            });
        }
        Ok(out)
    }

    /// Return `MAX(highestmodseq)` over `bridge_imap_mailbox_state` rows for
    /// `actor`, optionally restricted to a single `mailbox`.  Returns `1`
    /// (not `0`) when no rows match — consistent with the schema default.
    pub async fn max_highestmodseq_for_actor(
        &self,
        actor: &[u8; 32],
        mailbox: Option<&str>,
    ) -> Result<i64> {
        let actor = *actor;
        let mailbox_owned: Option<String> = mailbox.map(|s| s.to_string());

        let sql = if mailbox_owned.is_some() {
            "SELECT COALESCE(MAX(highestmodseq), 1) FROM bridge_imap_mailbox_state \
             WHERE actor_id = ?1 AND mailbox = ?2"
        } else {
            "SELECT COALESCE(MAX(highestmodseq), 1) FROM bridge_imap_mailbox_state \
             WHERE actor_id = ?1"
        };

        let conn = self.conn.lock().await;
        let val: i64 = if let Some(ref mb) = mailbox_owned {
            conn.query_row(sql, rusqlite::params![&actor[..], mb], |row| row.get(0))
                .context("max_highestmodseq_for_actor (with mailbox)")?
        } else {
            conn.query_row(sql, rusqlite::params![&actor[..]], |row| row.get(0))
                .context("max_highestmodseq_for_actor (all mailboxes)")?
        };
        Ok(val)
    }

    /// The flag delta of one `(actor, mailbox)` past a `(since_modseq,
    /// after_uid)` cursor — the app-facing `fauna.email.inbox.flag_changes`
    /// read (`mail-app-surface.md` § Read state). Returns up to `limit` rows
    /// `(uid, flags, modseq)` ordered by `(modseq, uid)`, whether more remain,
    /// and the mailbox `highestmodseq` (0 when the mailbox has no state row).
    ///
    /// `after_uid == 0` selects every row with `modseq > since_modseq`; a
    /// non-zero `after_uid` also selects the rows at exactly `since_modseq`
    /// whose `uid > after_uid` — the resume point inside one flag write, which
    /// stamps every row it touches with one shared modseq.
    ///
    /// The `highestmodseq` is read under the same connection lock as the rows,
    /// so every row a later write touches carries a modseq above it: a client
    /// that advances its cursor to it misses nothing.
    pub async fn list_bridge_imap_flag_changes(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
        since_modseq: i64,
        after_uid: u32,
        limit: u32,
    ) -> Result<(Vec<(u32, String, i64)>, bool, i64)> {
        let actor = *actor;
        let conn = self.conn.lock().await;
        let highestmodseq: i64 = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], mailbox],
                |row| row.get(0),
            )
            .optional()
            .context("list_bridge_imap_flag_changes: read mailbox state")?
            .unwrap_or(0);
        let cursor = if after_uid == 0 {
            "modseq > ?3"
        } else {
            "(modseq > ?3 OR (modseq = ?3 AND uid > ?4))"
        };
        let sql = format!(
            "SELECT uid, flags, modseq FROM bridge_imap_messages \
             WHERE actor_id = ?1 AND mailbox = ?2 AND {cursor} \
             ORDER BY modseq ASC, uid ASC LIMIT ?5"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("list_bridge_imap_flag_changes: prepare")?;
        // Fetch one extra row to learn whether more remain.
        let mut rows = stmt
            .query_map(
                rusqlite::params![
                    &actor[..],
                    mailbox,
                    since_modseq,
                    after_uid as i64,
                    limit as i64 + 1
                ],
                |row| Ok((row.get::<_, i64>(0)? as u32, row.get(1)?, row.get(2)?)),
            )
            .context("list_bridge_imap_flag_changes: query")?
            .collect::<rusqlite::Result<Vec<(u32, String, i64)>>>()
            .context("list_bridge_imap_flag_changes: collect")?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        Ok((rows, more, highestmodseq))
    }

    /// Apply an IMAP STORE FLAGS mutation to `(actor, mailbox)` for the given `uids`.
    ///
    /// The entire sequence (read current state, compute new flags, write updates)
    /// runs under a **single `conn.lock()` scope** to prevent interleaved mutations
    /// from producing an inconsistent modseq.
    ///
    /// # Semantics
    /// - Missing UIDs are silently skipped (IMAP STORE does not error on absent UIDs).
    /// - If `mailbox` has no state row, the mailbox is treated as non-existent:
    ///   return `updated = vec![]`, `highestmodseq = 1`, **no** INSERT of a state row.
    /// - A single shared `highestmodseq + 1` bump is applied for the whole operation
    ///   (CONDSTORE semantics — all touched rows get the same new modseq).
    /// - If no rows are actually updated, the bump is skipped and the current
    ///   `highestmodseq` is returned unchanged.
    /// - `\Recent` is filtered out defensively even if the handler already rejected it.
    ///
    /// # No-op STOREs are free, and that is what makes the kind replayable
    ///
    /// A UID whose flag set already equals what the op would produce is
    /// reported in `updated` (carrying its **existing** modseq) but is neither
    /// written nor counted toward the bump — so `STORE +FLAGS \Seen` on an
    /// already-seen message costs no `HIGHESTMODSEQ` churn, wakes no IDLE
    /// subscriber, and litters no placement record. That falls out of ordinary
    /// IMAP hygiene, but it is also load-bearing for
    /// `fauna.bridges.store_flags`'s `forbid_replay = false`: re-issuing the
    /// identical STORE (what `request_auto_retry` does after a reconnect —
    /// `transport.md` § Idempotency and reconnect-with-resume) now returns a
    /// byte-identical reply instead of advancing modseq and reporting a false
    /// `MODIFIED` conflict. Before this, every one of Set/Add/Remove converged
    /// in *flag state* while diverging in modseq and in the reply.
    pub async fn apply_store_flags(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
        uids: &[u32],
        op: StoreFlagsDbOp,
        flags: &[String],
        unchanged_since: Option<i64>,
    ) -> Result<StoreFlagsDbOutcome> {
        use std::collections::BTreeSet;

        let actor = *actor;
        let mailbox = mailbox.to_string();
        let flags_owned: Vec<String> = flags.to_vec();
        let uids_owned: Vec<u32> = uids.to_vec();

        let conn = self.conn.lock().await;

        // 1. Read mailbox state. Missing mailbox → treat as no rows matched.
        let state: Option<(i64, i64)> = conn
            .query_row(
                "SELECT uid_next, highestmodseq \
                 FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &mailbox],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("apply_store_flags: read mailbox state")?;

        let (_uid_next, current_hms) = match state {
            None => {
                return Ok(StoreFlagsDbOutcome {
                    updated: vec![],
                    highestmodseq: 1,
                    modified: vec![],
                });
            }
            Some(s) => s,
        };

        // 2. For each UID, fetch current flags + modseq and compute the new
        //    set. When `unchanged_since` is set (RFC 7162 §3.1.3 CONDSTORE
        //    `UNCHANGEDSINCE`), rows whose modseq exceeds the supplied value
        //    are skipped and reported in `modified`. The check happens
        //    inside the same conn-mutex critical section as the writes so
        //    no other writer can advance modseq between read and write.
        let incoming: BTreeSet<String> = flags_owned
            .iter()
            .filter(|f| f.as_str() != "\\Recent")
            .cloned()
            .collect();

        // Each entry carries `(uid, before_flags_string, after_flags_string,
        // settled_modseq)`. before_flags is the column's value captured inside
        // this same conn-mutex section — used downstream to emit a placement
        // `StoreFlags` record with both directions (spec § D2).
        //
        // `settled_modseq` distinguishes the two outcomes, and building both in
        // ONE vec is what keeps the reply in request-UID order:
        //   * `None`  — the flag set genuinely changes; the row is written below
        //               and stamped with the single shared `new_modseq`.
        //   * `Some(m)` — the row ALREADY holds the requested set, so there is
        //               nothing to write; it reports its existing modseq `m`.
        // A `Some` entry therefore always has `before == after`, which is what
        // the handler keys its journal-append and IDLE-push suppression on.
        let mut entries: Vec<(u32, String, String, Option<i64>)> = Vec::new();
        let mut changed_count = 0usize;
        let mut modified: Vec<u32> = Vec::new();

        for uid in &uids_owned {
            let row_opt: Option<(String, i64)> = conn
                .query_row(
                    "SELECT flags, modseq FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = ?2 AND uid = ?3",
                    rusqlite::params![&actor[..], &mailbox, *uid as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .context("apply_store_flags: read message flags")?;

            let (current_flags_str, current_modseq) = match row_opt {
                None => continue, // missing UID — skip silently
                Some(r) => r,
            };

            let existing: BTreeSet<String> = current_flags_str
                .split_whitespace()
                .map(String::from)
                .collect();

            let new_set: BTreeSet<String> = match op {
                StoreFlagsDbOp::Set => incoming.clone(),
                StoreFlagsDbOp::Add => existing.union(&incoming).cloned().collect(),
                StoreFlagsDbOp::Remove => existing.difference(&incoming).cloned().collect(),
            };

            let new_flags_string: String = new_set.into_iter().collect::<Vec<_>>().join(" ");

            // The no-op branch is deliberately tested BEFORE the CONDSTORE
            // `UNCHANGEDSINCE` gate below. RFC 7162's `MODIFIED` response means
            // "someone else changed this message, so I refused to apply yours" —
            // but when the row already holds exactly the set being requested
            // there is no change to refuse and no conflict to report. Ordering
            // the gates the other way is what made STORE non-idempotent: the
            // first call stamps `new_modseq > unchanged_since`, so an identical
            // re-issue (which is precisely what `request_auto_retry` sends after
            // a reconnect) was answered `MODIFIED` — a *false* conflict report
            // where the first call had answered success. See the
            // `forbid_replay = false` rationale at the kind's declaration site.
            if new_flags_string == current_flags_str {
                entries.push((
                    *uid,
                    current_flags_str.clone(),
                    current_flags_str,
                    Some(current_modseq),
                ));
                continue;
            }

            if let Some(n) = unchanged_since
                && current_modseq > n
            {
                modified.push(*uid);
                continue;
            }

            entries.push((*uid, current_flags_str, new_flags_string, None));
            changed_count += 1;
        }

        // 3. Nothing actually changes → no modseq bump, no writes. The already-
        //    satisfied UIDs are still reported (they hold the requested state),
        //    which is what makes a replayed STORE answer byte-identically.
        if changed_count == 0 {
            return Ok(StoreFlagsDbOutcome {
                updated: entries
                    .into_iter()
                    .map(|(uid, before, after, m)| (uid, before, after, m.unwrap_or(current_hms)))
                    .collect(),
                highestmodseq: current_hms,
                modified,
            });
        }

        // 4. Single shared modseq bump, applied only to the rows that change.
        let new_modseq = current_hms + 1;

        for (uid, _before, new_flags, settled) in &entries {
            if settled.is_some() {
                continue;
            }
            conn.execute(
                "UPDATE bridge_imap_messages \
                 SET flags = ?1, modseq = ?2 \
                 WHERE actor_id = ?3 AND mailbox = ?4 AND uid = ?5",
                rusqlite::params![new_flags, new_modseq, &actor[..], &mailbox, *uid as i64],
            )
            .context("apply_store_flags: update message row")?;
        }

        // 5. Bump mailbox state once.
        conn.execute(
            "UPDATE bridge_imap_mailbox_state \
             SET highestmodseq = ?1 \
             WHERE actor_id = ?2 AND mailbox = ?3",
            rusqlite::params![new_modseq, &actor[..], &mailbox],
        )
        .context("apply_store_flags: update mailbox state")?;

        let result: Vec<(u32, String, String, i64)> = entries
            .into_iter()
            .map(|(uid, before, after, settled)| {
                (uid, before, after, settled.unwrap_or(new_modseq))
            })
            .collect();

        Ok(StoreFlagsDbOutcome {
            updated: result,
            highestmodseq: new_modseq,
            modified,
        })
    }

    /// Expunge `\Deleted`-flagged messages from `(actor, mailbox)`.
    ///
    /// The entire sequence runs under a **single `conn.lock()` scope**.
    ///
    /// # Semantics
    /// - If `uids` is empty: expunge all `\Deleted`-flagged messages (plain EXPUNGE).
    /// - If `uids` is non-empty: expunge the intersection of `uids` and `\Deleted`-flagged
    ///   messages (UID EXPUNGE, RFC 4315).
    /// - Missing mailbox-state row: return empty result, `highestmodseq = 1`. No bump.
    /// - Empty target set: return empty result, `highestmodseq` unchanged. No bump.
    /// - Non-empty target: single shared `highestmodseq + 1` bump; rows are written to
    ///   `bridge_imap_expunged` before deletion from `bridge_imap_messages`.
    ///
    /// # Note on orphaned content rows
    /// Expunged messages leave their `bridge_inbound_mail` content rows intact.
    /// This is intentional: a message copied to another mailbox may still have a
    /// placement row there that references the same content. A future GC sweep
    /// removes content rows that have no remaining placement rows anywhere.
    ///
    /// `exclude_uids` is subtracted from the target set after the `\Deleted`
    /// filter — the IMAP handler passes the live guardian-hold UIDs so a
    /// ward's expunge skips them (`family-safety.md` § The mail gate), while
    /// the guardian's own discard path passes `&[]` and expunges freely.
    ///
    /// `now` (epoch seconds) is the caller-supplied delete time, written to
    /// `bridge_imap_expunged.expunged_at` — the same reading the caller
    /// reuses for the placement journal's `Expunge.deleted_at` (tombstone
    /// retention pruning; `imap-server.md` § Tombstone retention).
    pub async fn apply_expunge(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
        uids: &[u32],
        exclude_uids: &[u32],
        now: i64,
    ) -> Result<ExpungeDbOutcome> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let uids_filter: Vec<u32> = uids.to_vec();
        let exclude: Vec<u32> = exclude_uids.to_vec();

        let conn = self.conn.lock().await;

        // 1. Read mailbox state. Missing → no-op.
        let current_hms: i64 = match conn
            .query_row(
                "SELECT highestmodseq FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &mailbox],
                |row| row.get(0),
            )
            .optional()
            .context("apply_expunge: read mailbox state")?
        {
            None => {
                return Ok(ExpungeDbOutcome {
                    expunged_uids: vec![],
                    highestmodseq: 1,
                });
            }
            Some(hms) => hms,
        };

        // 2. Resolve the target set (rows with \Deleted in their flag set).
        let mut candidates: Vec<(u32, String)> = if uids_filter.is_empty() {
            // Plain EXPUNGE: all messages in the mailbox.
            let mut stmt = conn
                .prepare(
                    "SELECT uid, flags FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = ?2 \
                     ORDER BY uid ASC",
                )
                .context("apply_expunge: prepare all-messages query")?;
            stmt.query_map(rusqlite::params![&actor[..], &mailbox], |row| {
                Ok((row.get::<_, i64>(0)? as u32, row.get::<_, String>(1)?))
            })
            .context("apply_expunge: query all messages")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("apply_expunge: collect all messages")?
        } else {
            // UID EXPUNGE: only the listed UIDs.
            let uid_csv: String = uids_filter
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT uid, flags FROM bridge_imap_messages \
                 WHERE actor_id = ?1 AND mailbox = ?2 AND uid IN ({uid_csv}) \
                 ORDER BY uid ASC"
            );
            let mut stmt = conn
                .prepare(&sql)
                .context("apply_expunge: prepare uid-filter query")?;
            stmt.query_map(rusqlite::params![&actor[..], &mailbox], |row| {
                Ok((row.get::<_, i64>(0)? as u32, row.get::<_, String>(1)?))
            })
            .context("apply_expunge: query uid-filtered messages")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("apply_expunge: collect uid-filtered messages")?
        };

        // Filter to rows that actually have \Deleted in their flag set.
        candidates.retain(|(_, flags)| flags.split_whitespace().any(|t| t == "\\Deleted"));
        // Handler-supplied exclusions (live guardian holds) survive expunge.
        candidates.retain(|(uid, _)| !exclude.contains(uid));
        // Already ordered ascending by uid from the SQL ORDER BY.

        // 3. Empty target → no-op.
        if candidates.is_empty() {
            return Ok(ExpungeDbOutcome {
                expunged_uids: vec![],
                highestmodseq: current_hms,
            });
        }

        // 4. Single shared modseq bump.
        let new_modseq = current_hms + 1;

        for (uid, _) in &candidates {
            // Write to expunge log.
            conn.execute(
                "INSERT INTO bridge_imap_expunged \
                 (actor_id, mailbox, uid, modseq, expunged_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![&actor[..], &mailbox, *uid as i64, new_modseq, now],
            )
            .context("apply_expunge: insert expunge log")?;
            // Delete from messages.
            conn.execute(
                "DELETE FROM bridge_imap_messages \
                 WHERE actor_id = ?1 AND mailbox = ?2 AND uid = ?3",
                rusqlite::params![&actor[..], &mailbox, *uid as i64],
            )
            .context("apply_expunge: delete message")?;
        }

        // 5. Update mailbox state.
        conn.execute(
            "UPDATE bridge_imap_mailbox_state \
             SET highestmodseq = ?1 \
             WHERE actor_id = ?2 AND mailbox = ?3",
            rusqlite::params![new_modseq, &actor[..], &mailbox],
        )
        .context("apply_expunge: update mailbox state")?;

        let expunged_uids: Vec<u32> = candidates.into_iter().map(|(uid, _)| uid).collect();

        Ok(ExpungeDbOutcome {
            expunged_uids,
            highestmodseq: new_modseq,
        })
    }

    /// Expunge the IMAP placements of every mail record whose content `seq <=
    /// up_to_seq` — the relay-purge counterpart of the MTA-ingest placement.
    ///
    /// When the home box pulls + acks relayed mail, the public relay box
    /// tombstones the `__mail` segment (the *content*) via
    /// [`crate::segments::mail::tombstone_up_to_seq`]; this removes the
    /// `bridge_imap_messages` placement rows the public box created at ingest so
    /// its MDA shows NO message afterward (the no-readable/persistent-copy
    /// property — `deployment-home-with-public-relay.md` § Done definition).
    /// Unlike [`Self::apply_expunge`] this is keyed by content `seq`, not `uid` +
    /// `\Deleted`: the relay forcibly removes the placement regardless of flags.
    ///
    /// Logs each expunged uid to `bridge_imap_expunged` and bumps the per-mailbox
    /// `highestmodseq` (so a QRESYNC client that *is* served by this box sees the
    /// removal), then returns, per affected mailbox, the expunged uids + the new
    /// modseq so the caller can emit the matching `MailPlacementRecord::Expunge`
    /// journal record. A no-op (empty result) when no placement matches.
    ///
    /// `now` (epoch seconds) is the caller-supplied delete time, written to
    /// `bridge_imap_expunged.expunged_at` and reused for the placement
    /// journal's `Expunge.deleted_at` — see [`Self::apply_expunge`].
    pub async fn purge_mail_placements_up_to_seq(
        &self,
        actor: &[u8; 32],
        up_to_seq: i64,
        now: i64,
    ) -> Result<Vec<(String, Vec<u32>, i64)>> {
        let actor = *actor;
        let conn = self.conn.lock().await;

        // The placements whose mail content is being purged. Joined on
        // `substr(record_cid, 5) = message_id` (the 32-byte wire id is the Cid's
        // digest tail) to the mail `segment_records` rows with `seq <=
        // up_to_seq` (the tombstone keeps the row, only flagging it, so this
        // join still resolves after `tombstone_up_to_seq`).
        let mut stmt = conn
            .prepare(
                "SELECT bim.mailbox, bim.uid \
                 FROM bridge_imap_messages bim \
                 JOIN segment_records sr \
                   ON sr.kind = 'mail' \
                  AND sr.scope_id = bim.actor_id \
                  AND substr(sr.record_cid, 5) = bim.message_id \
                 WHERE bim.actor_id = ?1 AND sr.seq <= ?2 \
                 ORDER BY bim.mailbox ASC, bim.uid ASC",
            )
            .context("purge_mail_placements: prepare query")?;
        let rows: Vec<(String, u32)> = stmt
            .query_map(rusqlite::params![&actor[..], up_to_seq], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u32))
            })
            .context("purge_mail_placements: query")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("purge_mail_placements: collect")?;
        drop(stmt);

        if rows.is_empty() {
            return Ok(vec![]);
        }

        // Group uids by mailbox so each mailbox's highestmodseq bumps once.
        let mut by_mailbox: std::collections::BTreeMap<String, Vec<u32>> =
            std::collections::BTreeMap::new();
        for (mailbox, uid) in rows {
            by_mailbox.entry(mailbox).or_default().push(uid);
        }

        let mut out = Vec::with_capacity(by_mailbox.len());
        for (mailbox, uids) in by_mailbox {
            let current_hms: i64 = conn
                .query_row(
                    "SELECT highestmodseq FROM bridge_imap_mailbox_state \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                    rusqlite::params![&actor[..], &mailbox],
                    |row| row.get(0),
                )
                .optional()
                .context("purge_mail_placements: read mailbox hms")?
                .unwrap_or(1);
            let new_modseq = current_hms + 1;
            for uid in &uids {
                conn.execute(
                    "INSERT INTO bridge_imap_expunged \
                     (actor_id, mailbox, uid, modseq, expunged_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![&actor[..], &mailbox, *uid as i64, new_modseq, now],
                )
                .context("purge_mail_placements: insert expunge log")?;
                conn.execute(
                    "DELETE FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = ?2 AND uid = ?3",
                    rusqlite::params![&actor[..], &mailbox, *uid as i64],
                )
                .context("purge_mail_placements: delete placement")?;
            }
            conn.execute(
                "UPDATE bridge_imap_mailbox_state \
                 SET highestmodseq = ?1 \
                 WHERE actor_id = ?2 AND mailbox = ?3",
                rusqlite::params![new_modseq, &actor[..], &mailbox],
            )
            .context("purge_mail_placements: update mailbox hms")?;
            out.push((mailbox, uids, new_modseq));
        }
        Ok(out)
    }

    /// Copy messages from `source_mailbox` to `dest_mailbox` for `actor`.
    ///
    /// All SQL runs under a **single `conn.lock()` scope**.  The dest mailbox is
    /// auto-created (via `INSERT OR IGNORE`) when it has no state row yet.
    ///
    /// # Semantics
    /// - Missing source UIDs are silently skipped (IMAP COPY skips absent UIDs).
    /// - Flags are copied verbatim from the source, **minus `\Recent`** (RFC 3501).
    /// - The underlying `bridge_inbound_mail` content row is shared — COPY just
    ///   creates a new placement pointing at the same `message_id`.
    /// - A single UPDATE bumps the dest state row's `(uid_next, highestmodseq)`
    ///   once at the end of the loop, not once per row.
    /// - If no source UIDs exist, the state counters are left unchanged and
    ///   `copied = vec![]` is returned.
    pub async fn apply_copy(
        &self,
        actor: &[u8; 32],
        source_mailbox: &str,
        uids: &[u32],
        dest_mailbox: &str,
    ) -> Result<CopyDbOutcome> {
        let actor = *actor;
        let source_mailbox = source_mailbox.to_string();
        let dest_mailbox = dest_mailbox.to_string();
        let uids_owned: Vec<u32> = uids.to_vec();

        let conn = self.conn.lock().await;
        let (dest_uid_validity, copied, dest_highestmodseq) =
            copy_within_locked(&conn, &actor, &source_mailbox, &uids_owned, &dest_mailbox)
                .context("apply_copy")?;
        Ok(CopyDbOutcome {
            dest_uid_validity,
            copied,
            dest_highestmodseq,
        })
    }

    /// Move messages from `source_mailbox` to `dest_mailbox` for `actor`.
    ///
    /// Implemented as an atomic copy-then-expunge under a **single lock scope**.
    /// Uses `copy_within_locked` internally; does NOT call `apply_copy` (which
    /// would re-acquire the lock and deadlock).
    ///
    /// # Semantics
    /// - If all source UIDs are missing: `moved = vec![]`, no source-side touches.
    /// - A single shared `highestmodseq + 1` bump is applied to the source after
    ///   all its copied rows are expunged.
    /// - `source_mailbox == dest_mailbox` is legal (copy-then-expunge results in
    ///   new UIDs for the same content in the same mailbox).
    pub async fn apply_move(
        &self,
        actor: &[u8; 32],
        source_mailbox: &str,
        uids: &[u32],
        dest_mailbox: &str,
    ) -> Result<MoveDbOutcome> {
        let actor = *actor;
        let source_mailbox = source_mailbox.to_string();
        let dest_mailbox = dest_mailbox.to_string();
        let uids_owned: Vec<u32> = uids.to_vec();

        let conn = self.conn.lock().await;
        // Epoch seconds, matching `apply_expunge`/`purge_mail_placements_up_to_seq`
        // — every writer of this column must agree on the unit (the placement
        // manifest's tombstone `deleted_at` is epoch seconds too).
        let now = now_epoch_secs();

        // Step 1: copy.
        let (dest_uid_validity, pairs, dest_highestmodseq) =
            copy_within_locked(&conn, &actor, &source_mailbox, &uids_owned, &dest_mailbox)
                .context("apply_move: copy step")?;

        // Step 2: if nothing was copied, skip source-side operations.
        if pairs.is_empty() {
            // Read current source highestmodseq for the reply.
            let source_hms: i64 = conn
                .query_row(
                    "SELECT highestmodseq FROM bridge_imap_mailbox_state \
                     WHERE actor_id = ?1 AND mailbox = ?2",
                    rusqlite::params![&actor[..], &source_mailbox],
                    |row| row.get(0),
                )
                .optional()
                .context("apply_move: read source hms for empty result")?
                .unwrap_or(1);
            return Ok(MoveDbOutcome {
                dest_uid_validity,
                moved: vec![],
                source_highestmodseq: source_hms,
                dest_highestmodseq,
                moved_at: now,
            });
        }

        // Step 3: read the current source highestmodseq (state row MUST exist
        // because we just successfully sourced from it in the copy step).
        let source_current_hms: i64 = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = ?2",
                rusqlite::params![&actor[..], &source_mailbox],
                |row| row.get(0),
            )
            .context("apply_move: read source highestmodseq")?;

        let source_new_modseq = source_current_hms + 1;

        // Step 4: expunge source rows that were copied.
        for &(source_uid, _dest_uid) in &pairs {
            conn.execute(
                "INSERT INTO bridge_imap_expunged \
                 (actor_id, mailbox, uid, modseq, expunged_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    &actor[..],
                    &source_mailbox,
                    source_uid as i64,
                    source_new_modseq,
                    now
                ],
            )
            .context("apply_move: insert expunge log")?;
            conn.execute(
                "DELETE FROM bridge_imap_messages \
                 WHERE actor_id = ?1 AND mailbox = ?2 AND uid = ?3",
                rusqlite::params![&actor[..], &source_mailbox, source_uid as i64],
            )
            .context("apply_move: delete source message")?;
        }

        // Step 5: bump source state row's highestmodseq once.
        conn.execute(
            "UPDATE bridge_imap_mailbox_state \
             SET highestmodseq = ?1 \
             WHERE actor_id = ?2 AND mailbox = ?3",
            rusqlite::params![source_new_modseq, &actor[..], &source_mailbox],
        )
        .context("apply_move: update source mailbox state")?;

        Ok(MoveDbOutcome {
            dest_uid_validity,
            moved: pairs,
            source_highestmodseq: source_new_modseq,
            dest_highestmodseq,
            moved_at: now,
        })
    }

    /// Place a message — or return the existing placement UID — for an APPEND upload.
    ///
    /// Unlike `place_inbound_mail` (which returns `None` on retry), this method
    /// always returns a valid UID.  Two cases:
    ///
    /// - `content_was_new = true`: allocate a fresh UID exactly as
    ///   `place_inbound_mail` would.
    /// - `content_was_new = false`: the content row already existed from a prior
    ///   call.  Look up the existing placement for `(actor, message_id, mailbox)`.
    ///   If found, return its UID (idempotent retry).  If NOT found (the content
    ///   exists from a different path, e.g. a prior ingest placed it elsewhere),
    ///   allocate a new placement now and return the new UID.
    ///
    /// `sender_domain` populates `from_norm` for the SEARCH header axis (see
    /// `place_inbound_mail` for the encrypted-mode degradation rationale).
    ///
    /// All operations run under a **single `conn.lock()` scope**.
    /// Returns `(uid, new_placement_modseq)`. The `Option` discriminator
    /// reflects the per-call write semantics, NOT the `content_was_new`
    /// input:
    ///
    /// - `Some(modseq)` when a fresh placement row was created — both
    ///   case 1 (`content_was_new = true`) and case 3 (`content_was_new
    ///   = false` AND no prior placement existed in this mailbox) take
    ///   this arm. The caller should emit a `MailPlacementRecord::Append`.
    /// - `None` when an existing placement was recovered by `(actor,
    ///   message_id, mailbox)` lookup — case 2. The caller must NOT
    ///   emit a duplicate placement event (the original placement
    ///   already wrote one).
    pub async fn place_or_get_existing_placement(
        &self,
        actor: &[u8; 32],
        message_id: &[u8; 32],
        mailbox: &str,
        internal_date: i64,
        initial_flags: &str,
        sender_domain: &str,
        content_was_new: bool,
    ) -> Result<(u32, Option<i64>)> {
        let actor = *actor;
        let message_id = *message_id;
        let mailbox = mailbox.to_string();
        let initial_flags = initial_flags.to_string();
        let from_norm = norm_for_storage(sender_domain);
        let created_at = now_epoch_millis();
        let conn = self.conn.lock().await;

        if !content_was_new {
            // Look up an existing placement for (actor, message_id, mailbox).
            let existing_uid: Option<u32> = conn
                .query_row(
                    "SELECT uid FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND message_id = ?2 AND mailbox = ?3 \
                     LIMIT 1",
                    rusqlite::params![&actor[..], &message_id[..], &mailbox],
                    |row| Ok(row.get::<_, i64>(0)? as u32),
                )
                .optional()
                .context("place_or_get_existing_placement: lookup existing")?;

            if let Some(uid) = existing_uid {
                return Ok((uid, None));
            }
            // Fall through to allocation — same content exists elsewhere (or was
            // placed in a different mailbox on a prior ingest call) but has no
            // placement in this specific mailbox yet.
        }

        // Ensure mailbox-state row exists (handles non-standard mailbox names).
        ensure_mailbox_state_row(&conn, &actor, &mailbox)
            .context("place_or_get_existing_placement: ensure mailbox state")?;

        // Allocate UID + bump modseq.
        let (new_uid, new_modseq) = allocate_uid(&conn, &actor, &mailbox)
            .context("place_or_get_existing_placement: allocate uid")?;

        // Insert placement row.
        conn.execute(
            "INSERT INTO bridge_imap_messages \
                 (actor_id, mailbox, uid, message_id, flags, modseq, internal_date, created_at, from_norm) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                &actor[..],
                &mailbox,
                new_uid as i64,
                &message_id[..],
                &initial_flags,
                new_modseq,
                internal_date,
                created_at,
                &from_norm,
            ],
        )
        .context("place_or_get_existing_placement: insert message")?;

        Ok((new_uid, Some(new_modseq)))
    }

    /// List placement rows for `(actor, mailbox)`, returning `(uid, flags, internal_date)`.
    /// Ordered by uid ascending.  Minimal helper used by C.0 handler tests;
    /// C.2 adds the richer `query_bridge_imap_messages` query.
    pub async fn list_bridge_imap_messages(
        &self,
        actor: &[u8; 32],
        mailbox: &str,
    ) -> Result<Vec<(u32, String, i64)>> {
        let actor = *actor;
        let mailbox = mailbox.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT uid, flags, internal_date \
                     FROM bridge_imap_messages \
                     WHERE actor_id = ?1 AND mailbox = ?2 \
                     ORDER BY uid ASC",
            )
            .context("prepare list bridge imap messages")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..], &mailbox], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u32,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .context("query list bridge imap messages")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect list bridge imap messages")?;
        Ok(rows)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::bridge_routing::InboundMailFields;

    fn make_actor(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn make_message_id(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn sample_inbound_fields(actor: &[u8; 32], body: &[u8]) -> InboundMailFields {
        InboundMailFields {
            actor_id: *actor,
            timestamp: 1_700_000_000,
            ciphertext_size: body.len() as u32,
            encrypted_body: fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                body.to_vec(),
            ),
            encrypted_index_hint:
                fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    b"index-hint".to_vec(),
                ),
            sender_domain: "example.com".into(),
            spf: "pass".into(),
            dkim: "pass".into(),
            dmarc: "pass".into(),
            dmarc_policy: "none".into(),
            arc: "none".into(),
            spam_score: 0,
            spam_disposition: "accept".into(),
            is_own_submission: false,
            scores: vec![],
            report_hash: vec![],
        }
    }

    #[tokio::test]
    async fn ensure_bridge_imap_mailboxes_seeds_standard_names() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(1);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 6, "six standard mailboxes");
    }

    #[tokio::test]
    async fn ensure_bridge_imap_mailboxes_is_idempotent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(2);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 6, "still six rows after second call");
    }

    #[tokio::test]
    async fn place_inbound_mail_allocates_uids_sequentially() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(3);
        let msg1 = make_message_id(10);
        let msg2 = make_message_id(11);

        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let placement1 = db
            .place_inbound_mail(&actor, &msg1, "INBOX", 1_700_000_000, "", "", true)
            .await
            .unwrap();
        let placement2 = db
            .place_inbound_mail(&actor, &msg2, "INBOX", 1_700_000_001, "", "", true)
            .await
            .unwrap();

        let (uid1, modseq1) = placement1.expect("first placement returned None");
        let (uid2, modseq2) = placement2.expect("second placement returned None");
        assert_eq!(uid1, 1, "first message gets uid 1");
        assert_eq!(uid2, 2, "second message gets uid 2");
        assert!(modseq1 > 0, "first placement modseq is positive");
        assert!(
            modseq2 > modseq1,
            "second placement modseq strictly greater"
        );
    }

    /// Row 134(a): `delete_bridge_imap_mailbox`'s tombstone leg stamped
    /// `expunged_at` in **millis** while its three sibling writers
    /// (`apply_expunge`, `purge_mail_placements_up_to_seq`, `apply_move`) all
    /// write **seconds** — the same column, two units. Asserted by range
    /// rather than exact equality: a millis stamp is ~1000x the seconds one,
    /// so the two units cannot both land within a tight delta of "now in
    /// seconds", making this deterministic without controlling the clock.
    #[tokio::test]
    async fn delete_nonempty_mailbox_tombstones_in_seconds_not_millis() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(30);
        let msg = make_message_id(31);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();
        db.place_inbound_mail(&actor, &msg, "INBOX", 1_700_000_000, "", "", true)
            .await
            .unwrap()
            .expect("placement lands");

        let outcome = db
            .delete_bridge_imap_mailbox(&actor, "INBOX", true)
            .await
            .unwrap();
        assert_eq!(outcome, DeleteMailboxDbOutcome::Deleted);

        let conn = db.conn().await;
        let expunged_at: i64 = conn
            .query_row(
                "SELECT expunged_at FROM bridge_imap_expunged WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        let now = now_epoch_secs();
        assert!(
            (expunged_at - now).abs() < 5,
            "expunged_at ({expunged_at}) must be seconds-since-epoch, within \
             5s of now ({now}) — a millis stamp would be ~1000x too large"
        );
    }

    #[tokio::test]
    async fn place_inbound_mail_uid_isolated_per_mailbox() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(4);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg_inbox = make_message_id(20);
        let msg_junk = make_message_id(21);

        let placement_inbox = db
            .place_inbound_mail(&actor, &msg_inbox, "INBOX", 1_700_000_000, "", "", true)
            .await
            .unwrap();
        let placement_junk = db
            .place_inbound_mail(&actor, &msg_junk, "Junk", 1_700_000_000, "", "", true)
            .await
            .unwrap();

        let (uid_inbox, _modseq_inbox) = placement_inbox.expect("INBOX placement returned None");
        let (uid_junk, _modseq_junk) = placement_junk.expect("Junk placement returned None");
        assert_eq!(uid_inbox, 1, "INBOX starts at uid 1");
        assert_eq!(uid_junk, 1, "Junk starts at uid 1 independently");
    }

    #[tokio::test]
    async fn place_inbound_mail_uid_isolated_per_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let alice = make_actor(5);
        let bob = make_actor(6);
        db.ensure_bridge_imap_mailboxes(&alice).await.unwrap();
        db.ensure_bridge_imap_mailboxes(&bob).await.unwrap();

        let msg_alice = make_message_id(30);
        let msg_bob = make_message_id(31);

        let placement_alice = db
            .place_inbound_mail(&alice, &msg_alice, "INBOX", 1_700_000_000, "", "", true)
            .await
            .unwrap();
        let placement_bob = db
            .place_inbound_mail(&bob, &msg_bob, "INBOX", 1_700_000_000, "", "", true)
            .await
            .unwrap();

        let (uid_alice, _) = placement_alice.expect("alice placement returned None");
        let (uid_bob, _) = placement_bob.expect("bob placement returned None");
        assert_eq!(uid_alice, 1);
        assert_eq!(uid_bob, 1, "different actor starts at uid 1");
    }

    #[tokio::test]
    async fn place_inbound_mail_bumps_highestmodseq() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(7);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg1 = make_message_id(40);
        let msg2 = make_message_id(41);
        db.place_inbound_mail(&actor, &msg1, "INBOX", 1_700_000_000, "", "", true)
            .await
            .unwrap();
        db.place_inbound_mail(&actor, &msg2, "INBOX", 1_700_000_001, "", "", true)
            .await
            .unwrap();

        let conn = db.conn().await;
        let hms: i64 = conn
            .query_row(
                "SELECT highestmodseq FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hms, 3, "starts at 1, +1 per placement → 3 after two places");
    }

    #[tokio::test]
    async fn place_inbound_mail_duplicate_content_returns_none() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(8);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg = make_message_id(50);

        // First call (content_was_new=true) creates the row.
        let placement = db
            .place_inbound_mail(&actor, &msg, "INBOX", 1_700_000_000, "", "", true)
            .await
            .unwrap();
        let (uid, modseq) = placement.expect("fresh placement returned None");
        assert_eq!(uid, 1);
        assert!(modseq > 0, "fresh placement modseq is positive");

        // Second call (content_was_new=false, duplicate transport retry).
        let retry = db
            .place_inbound_mail(&actor, &msg, "INBOX", 1_700_000_000, "", "", false)
            .await
            .unwrap();
        assert_eq!(retry, None, "duplicate must return None");

        // Confirm no second placement row was created.
        let rows = db.list_bridge_imap_messages(&actor, "INBOX").await.unwrap();
        assert_eq!(rows.len(), 1, "still only one row");

        // Confirm uid_next is still 2 (not bumped further).
        let conn = db.conn().await;
        let uid_next: i64 = conn
            .query_row(
                "SELECT uid_next FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            uid_next, 2,
            "uid_next should be 2 (first ingest allocated uid 1)"
        );
    }

    #[tokio::test]
    async fn place_inbound_mail_non_standard_mailbox_auto_creates_state() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(9);
        // Deliberately do NOT call ensure_bridge_imap_mailboxes first.
        let msg = make_message_id(60);

        let placement = db
            .place_inbound_mail(&actor, &msg, "Archive", 1_700_000_000, "", "", true)
            .await
            .unwrap();
        let (uid, _modseq) = placement.expect("fresh placement returned None");
        assert_eq!(
            uid, 1,
            "non-standard mailbox auto-created and uid=1 allocated"
        );
    }

    // ── place_or_get_existing_placement tests ────────────────────────────────

    #[tokio::test]
    async fn place_or_get_existing_content_new_allocates_uid() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(70);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg = make_message_id(70);
        let (uid, modseq) = db
            .place_or_get_existing_placement(
                &actor,
                &msg,
                "Drafts",
                1_700_000_000,
                "\\Draft",
                "",
                true,
            )
            .await
            .unwrap();
        assert_eq!(uid, 1, "first placement in Drafts gets uid 1");
        assert!(modseq.is_some(), "fresh placement returns Some(modseq)");
    }

    #[tokio::test]
    async fn place_or_get_existing_content_not_new_existing_placement_returns_same_uid() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(71);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg = make_message_id(71);
        // First call: content_was_new=true → allocates uid 1.
        let (uid1, modseq1) = db
            .place_or_get_existing_placement(
                &actor,
                &msg,
                "Drafts",
                1_700_000_000,
                "\\Draft",
                "",
                true,
            )
            .await
            .unwrap();
        assert_eq!(uid1, 1);
        assert!(modseq1.is_some(), "first call: fresh placement");

        // Second call: content_was_new=false + existing placement → same uid.
        let (uid2, modseq2) = db
            .place_or_get_existing_placement(
                &actor,
                &msg,
                "Drafts",
                1_700_000_000,
                "\\Draft",
                "",
                false,
            )
            .await
            .unwrap();
        assert_eq!(uid2, uid1, "idempotent retry returns same uid");
        assert!(
            modseq2.is_none(),
            "idempotent retry: no new placement → None"
        );

        // Confirm only one row exists.
        let rows = db
            .list_bridge_imap_messages(&actor, "Drafts")
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "no duplicate placement");
    }

    #[tokio::test]
    async fn place_or_get_existing_content_not_new_no_prior_placement_allocates_uid() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(72);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg = make_message_id(72);
        // content_was_new=false but NO prior placement in "Drafts" → should allocate.
        let (uid, modseq) = db
            .place_or_get_existing_placement(&actor, &msg, "Drafts", 1_700_000_000, "", "", false)
            .await
            .unwrap();
        assert_eq!(
            uid, 1,
            "new placement allocated even though content_was_new=false"
        );
        assert!(
            modseq.is_some(),
            "no prior placement → fresh allocation, Some(modseq)"
        );

        let rows = db
            .list_bridge_imap_messages(&actor, "Drafts")
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn place_or_get_existing_auto_creates_non_standard_mailbox() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(73);
        // No ensure_bridge_imap_mailboxes call — mailbox must be lazy-created.
        let msg = make_message_id(73);
        let (uid, modseq) = db
            .place_or_get_existing_placement(&actor, &msg, "MyFolder", 1_700_000_000, "", "", true)
            .await
            .unwrap();
        assert_eq!(uid, 1, "custom mailbox auto-created, uid=1");
        assert!(modseq.is_some(), "fresh placement in new mailbox");
    }

    /// The scoped mirror lookup resolves an inserted record inside its own
    /// scope and misses outside it — the scope-check shape every production
    /// consumer uses (the scope-agnostic actor-for-record resolution is
    /// retired: under content-hash identity a byte replay can file one cid in
    /// two scopes, making a LIMIT-1 owner pick arbitrary).
    #[tokio::test]
    async fn scoped_mirror_lookup_resolves_within_the_scope_only() {
        let db = CacheDb::open_in_memory().unwrap();
        let tmp = tempfile::TempDir::new().expect("tmp");
        let manager = SegmentManager::new(tmp.path().to_path_buf(), "mail");
        let actor = make_actor(10);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();
        crate::test_support::seed_recipient_seal_key(
            &db,
            &actor,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;

        // Insert via the routing helper (segment-store path post-T7).
        let fields = sample_inbound_fields(&actor, b"body-x");
        let msg_id = db
            .insert_inbound_mail(&manager, &fields)
            .await
            .unwrap()
            .message_id;

        let cid = Cid::from_digest_dag_cbor(msg_id);
        assert!(
            db.segment_records_lookup_record(&actor, "mail", &cid)
                .await
                .unwrap()
                .is_some(),
            "resolves inside the owning scope"
        );
        let stranger = make_actor(11);
        assert!(
            db.segment_records_lookup_record(&stranger, "mail", &cid)
                .await
                .unwrap()
                .is_none(),
            "misses outside the owning scope"
        );
    }

    // ── C.1 DB tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_bridge_imap_mailbox_state_returns_five_standard_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(20);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let rows = db.list_bridge_imap_mailbox_state(&actor).await.unwrap();
        assert_eq!(rows.len(), 6, "six standard mailboxes");

        // Names (returned alphabetically by SQL ORDER BY).
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"INBOX"));
        assert!(names.contains(&"Archive"));
        assert!(names.contains(&"Sent"));
        assert!(names.contains(&"Drafts"));
        assert!(names.contains(&"Trash"));
        assert!(names.contains(&"Junk"));

        // Each starts at uid_next=1, highestmodseq=1.
        for row in &rows {
            assert_eq!(row.uid_next, 1, "uid_next initially 1 for {}", row.name);
            assert_eq!(
                row.highestmodseq, 1,
                "highestmodseq initially 1 for {}",
                row.name
            );
        }
    }

    #[tokio::test]
    async fn count_bridge_imap_mailbox_counts_seen_vs_unseen() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(21);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg_seen = make_message_id(100);
        let msg_unseen = make_message_id(101);

        db.place_inbound_mail(
            &actor,
            &msg_seen,
            "INBOX",
            1_700_000_000,
            "\\Seen",
            "",
            true,
        )
        .await
        .unwrap();
        db.place_inbound_mail(&actor, &msg_unseen, "INBOX", 1_700_000_001, "", "", true)
            .await
            .unwrap();

        let (exists, unseen) = db.count_bridge_imap_mailbox(&actor, "INBOX").await.unwrap();
        assert_eq!(exists, 2, "two messages placed");
        assert_eq!(unseen, 1, "one without \\Seen");
    }

    #[tokio::test]
    async fn first_unseen_uid_in_mailbox_finds_lowest_unseen() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(22);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg_seen = make_message_id(110);
        let msg_unseen = make_message_id(111);

        // Place seen first (uid 1), unseen second (uid 2).
        db.place_inbound_mail(
            &actor,
            &msg_seen,
            "INBOX",
            1_700_000_000,
            "\\Seen",
            "",
            true,
        )
        .await
        .unwrap();
        db.place_inbound_mail(&actor, &msg_unseen, "INBOX", 1_700_000_001, "", "", true)
            .await
            .unwrap();

        let first_unseen = db
            .first_unseen_uid_in_mailbox(&actor, "INBOX")
            .await
            .unwrap();
        // uid 1 is seen; uid 2 is unseen.
        assert_eq!(first_unseen, Some(2));
    }

    #[tokio::test]
    async fn get_bridge_imap_mailbox_state_returns_none_for_nonexistent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(23);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let row = db
            .get_bridge_imap_mailbox_state(&actor, "Nonexistent")
            .await
            .unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn get_bridge_imap_mailbox_state_returns_some_for_existing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(24);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let row = db
            .get_bridge_imap_mailbox_state(&actor, "INBOX")
            .await
            .unwrap();
        assert!(row.is_some());
        let row = row.unwrap();
        assert_eq!(row.name, "INBOX");
        assert_eq!(row.uid_next, 1);
        assert_eq!(row.highestmodseq, 1);
    }

    // ── C.2 DB tests ──────────────────────────────────────────────────────────

    /// Helper: seed a mail record in `segment_records` (kind='mail') and
    /// place it in the given mailbox. Returns the uid assigned by
    /// `place_inbound_mail`.
    ///
    /// Choice (b) from Plan 2 Task 8: write directly into `segment_records`
    /// via raw SQL rather than going through `segments::mail`. These
    /// tests verify the SQL-side joins / placement logic; they don't need
    /// an on-disk segment file. `segment_id` is a synthetic placeholder (1) —
    /// no segment-file consumer fires in these Db-layer tests (RFC822.SIZE is
    /// derived by the handler from the CARv2 index, not this query). The seeded
    /// `record_cid` is `from_digest_dag_cbor(msg_id)` so the `substr(record_cid,
    /// 5) = message_id` join resolves.
    async fn seed_message(
        db: &CacheDb,
        actor: &[u8; 32],
        msg_id: &[u8; 32],
        mailbox: &str,
        _body: &[u8],
        internal_date: i64,
        flags: &str,
    ) -> u32 {
        let record_cid = Cid::from_digest_dag_cbor(*msg_id);
        db.conn()
            .await
            .execute(
                "INSERT OR IGNORE INTO segment_records \
                    (scope_id, kind, segment_id, record_cid, bucket, \
                     tombstoned, \
                     received_at, sender_dom, spam_disp, is_own_submission) \
                 VALUES (?1, 'mail', 1, ?2, '2026-05', 0, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &actor[..],
                    &record_cid.as_bytes()[..],
                    internal_date,
                    "example.com",
                    "accept",
                    0i64,
                ],
            )
            .unwrap();
        let (uid, _modseq) = db
            .place_inbound_mail(actor, msg_id, mailbox, internal_date, flags, "", true)
            .await
            .unwrap()
            .expect("place_inbound_mail returned None (content_was_new=true)");
        uid
    }

    #[tokio::test]
    async fn query_bridge_imap_messages_returns_all_in_ascending_uid_order() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(30);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg1 = make_message_id(30);
        let msg2 = make_message_id(31);
        let msg3 = make_message_id(32);

        seed_message(&db, &actor, &msg1, "INBOX", b"body1", 1_700_000_001, "").await;
        seed_message(
            &db,
            &actor,
            &msg2,
            "INBOX",
            b"body12",
            1_700_000_002,
            "\\Seen",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &msg3,
            "INBOX",
            b"body123",
            1_700_000_003,
            "\\Seen \\Answered",
        )
        .await;

        let rows = db
            .query_bridge_imap_messages(&actor, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3, "all three messages");
        assert_eq!(rows[0].uid, 1);
        assert_eq!(rows[1].uid, 2);
        assert_eq!(rows[2].uid, 3);

        // Check flags strings are preserved correctly.
        assert_eq!(rows[0].flags, "");
        assert_eq!(rows[1].flags, "\\Seen");
        assert_eq!(rows[2].flags, "\\Seen \\Answered");

        // Check internal_dates.
        assert_eq!(rows[0].internal_date, 1_700_000_001);
        assert_eq!(rows[1].internal_date, 1_700_000_002);
        assert_eq!(rows[2].internal_date, 1_700_000_003);

        // Each row carries its record's segment_id (all seeded into segment 1);
        // RFC822.SIZE itself is derived by the handler from the CARv2 index by
        // record_cid, not by this Db query (imap-server.md § SEARCH).
        assert_eq!(rows[0].segment_id, 1);
        assert_eq!(rows[1].segment_id, 1);
        assert_eq!(rows[2].segment_id, 1);

        // Check message_ids were stored correctly.
        assert_eq!(rows[0].message_id, msg1);
        assert_eq!(rows[1].message_id, msg2);
        assert_eq!(rows[2].message_id, msg3);
    }

    #[tokio::test]
    async fn query_bridge_imap_messages_after_uid_filters() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(31);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message(
            &db,
            &actor,
            &make_message_id(40),
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(41),
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(42),
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        let rows = db
            .query_bridge_imap_messages(&actor, "INBOX", None, Some(1), None, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "uid > 1 → uids 2 and 3");
        assert_eq!(rows[0].uid, 2);
        assert_eq!(rows[1].uid, 3);
    }

    #[tokio::test]
    async fn query_bridge_imap_messages_limit_truncates() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(32);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message(
            &db,
            &actor,
            &make_message_id(50),
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(51),
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(52),
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        let rows = db
            .query_bridge_imap_messages(&actor, "INBOX", None, None, None, Some(2))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "limit 2 returns first 2 rows");
        assert_eq!(rows[0].uid, 1);
        assert_eq!(rows[1].uid, 2);
    }

    #[tokio::test]
    async fn query_bridge_imap_messages_since_modseq_filters() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(33);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message(
            &db,
            &actor,
            &make_message_id(60),
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(61),
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(62),
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        // Record current highestmodseq (should be 4 after 3 placements starting at 1).
        let state = db
            .get_bridge_imap_mailbox_state(&actor, "INBOX")
            .await
            .unwrap()
            .unwrap();
        let old_hms = state.highestmodseq;

        // Simulate a flag update on uid 2 by bumping its modseq directly.
        db.conn()
            .await
            .execute(
                "UPDATE bridge_imap_messages SET modseq = ?1 WHERE actor_id = ?2 AND uid = 2",
                rusqlite::params![old_hms + 1, &actor[..]],
            )
            .unwrap();

        let rows = db
            .query_bridge_imap_messages(&actor, "INBOX", Some(old_hms), None, None, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "only uid 2 changed after old_hms");
        assert_eq!(rows[0].uid, 2);
    }

    #[tokio::test]
    async fn query_bridge_imap_messages_uid_filter() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(34);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message(
            &db,
            &actor,
            &make_message_id(70),
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(71),
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(72),
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;

        let rows = db
            .query_bridge_imap_messages(&actor, "INBOX", None, None, Some(&[1, 3]), None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].uid, 1);
        assert_eq!(rows[1].uid, 3);
        // F1: seq_num is the row's rank in the FULL mailbox (3 messages), not
        // its position in the filtered subset — so uid 3 is seqNum 3, not 2.
        // This is what lets a UID-subset FETCH emit correct RFC 9051 seqNums
        // without the Go bridge re-fetching the whole mailbox.
        assert_eq!(rows[0].seq_num, 1);
        assert_eq!(rows[1].seq_num, 3);
    }

    #[tokio::test]
    async fn query_bridge_imap_messages_empty_uid_slice_returns_all() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(35);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message(
            &db,
            &actor,
            &make_message_id(80),
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(81),
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;

        // Empty slice = "all messages" per spec.
        let rows = db
            .query_bridge_imap_messages(&actor, "INBOX", None, None, Some(&[]), None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "Some(&[]) means no UID filter → all rows");
    }

    #[tokio::test]
    async fn query_bridge_imap_messages_unknown_mailbox_returns_empty() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(36);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let rows = db
            .query_bridge_imap_messages(&actor, "Nonexistent", None, None, None, None)
            .await
            .unwrap();
        assert!(rows.is_empty(), "unknown mailbox returns no rows");
    }

    #[tokio::test]
    async fn query_bridge_imap_messages_junk_isolated() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(37);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message(
            &db,
            &actor,
            &make_message_id(90),
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(91),
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(92),
            "Junk",
            b"bj",
            1_700_000_003,
            "",
        )
        .await;

        let inbox_rows = db
            .query_bridge_imap_messages(&actor, "INBOX", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(inbox_rows.len(), 2, "INBOX has 2 messages");

        let junk_rows = db
            .query_bridge_imap_messages(&actor, "Junk", None, None, None, None)
            .await
            .unwrap();
        assert_eq!(junk_rows.len(), 1, "Junk has 1 message");
        assert_eq!(junk_rows[0].uid, 1, "Junk uid starts at 1 independently");
    }

    #[tokio::test]
    async fn list_bridge_imap_expunged_since_basic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(38);

        // Insert expunged rows directly (include expunged_at NOT NULL column).
        let conn = db.conn().await;
        conn.execute(
            "INSERT INTO bridge_imap_expunged (actor_id, mailbox, uid, modseq, expunged_at) \
             VALUES (?1, 'INBOX', 5, 10, 1700000001)",
            rusqlite::params![&actor[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bridge_imap_expunged (actor_id, mailbox, uid, modseq, expunged_at) \
             VALUES (?1, 'INBOX', 7, 12, 1700000002)",
            rusqlite::params![&actor[..]],
        )
        .unwrap();
        drop(conn);

        let uids = db
            .list_bridge_imap_expunged_since(&actor, "INBOX", 8)
            .await
            .unwrap();
        assert_eq!(uids, Some(vec![5, 7]), "modseq 10 and 12 both > 8");

        let uids2 = db
            .list_bridge_imap_expunged_since(&actor, "INBOX", 11)
            .await
            .unwrap();
        assert_eq!(uids2, Some(vec![7]), "only modseq 12 > 11");

        let uids3 = db
            .list_bridge_imap_expunged_since(&actor, "INBOX", 12)
            .await
            .unwrap();
        assert_eq!(uids3, Some(vec![]), "modseq 12 is not > 12");

        // A restore from a retention-pruned manifest set a prune floor of 9:
        // an expunge with modseq 9 is gone from the log, so the list since 8
        // is incomplete and refused; since 9 and later stay complete.
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();
        db.conn()
            .await
            .execute(
                "UPDATE bridge_imap_mailbox_state SET pruned_modseq = 9 \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![&actor[..]],
            )
            .unwrap();
        let below = db
            .list_bridge_imap_expunged_since(&actor, "INBOX", 8)
            .await
            .unwrap();
        assert_eq!(below, None, "below the prune floor the log is incomplete");
        let at = db
            .list_bridge_imap_expunged_since(&actor, "INBOX", 9)
            .await
            .unwrap();
        assert_eq!(at, Some(vec![5, 7]), "at the floor the log is complete");
    }

    // ── C.3 DB tests ──────────────────────────────────────────────────────────

    /// Like `seed_message` but writes a real segment file via the
    /// supplied `SegmentManager` so the segment store has a record
    /// to read back. The `encrypted_index_hint` lands inside the
    /// `MailRecordEnvelope` payload; T9 cutover replaced the prior
    /// `bridge_inbound_mail` row write.
    ///
    /// `msg_id` is ignored — the returned id is the manager-assigned
    /// derived id from `insert_inbound_mail`. Tests should use the
    /// returned id for subsequent assertions.
    async fn seed_message_with_hint(
        db: &CacheDb,
        manager: &SegmentManager,
        actor: &[u8; 32],
        mailbox: &str,
        body: &[u8],
        internal_date: i64,
        flags: &str,
        encrypted_index_hint: &[u8],
    ) -> (u32, [u8; 32]) {
        let fields = InboundMailFields {
            actor_id: *actor,
            timestamp: internal_date,
            ciphertext_size: body.len() as u32,
            encrypted_body: fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                body.to_vec(),
            ),
            encrypted_index_hint:
                fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    encrypted_index_hint.to_vec(),
                ),
            sender_domain: "example.com".into(),
            spf: "pass".into(),
            dkim: "pass".into(),
            dmarc: "pass".into(),
            dmarc_policy: "none".into(),
            arc: "none".into(),
            spam_score: 0,
            spam_disposition: "accept".into(),
            is_own_submission: false,
            scores: vec![],
            report_hash: vec![],
        };
        let msg_id = db
            .insert_inbound_mail(manager, &fields)
            .await
            .unwrap()
            .message_id;
        let (uid, _modseq) = db
            .place_inbound_mail(actor, &msg_id, mailbox, internal_date, flags, "", true)
            .await
            .unwrap()
            .expect("place_inbound_mail returned None");
        (uid, msg_id)
    }

    #[tokio::test]
    async fn query_bridge_imap_index_segments_ascending_with_hints() {
        let db = CacheDb::open_in_memory().unwrap();
        let tmp = tempfile::TempDir::new().expect("tmp");
        let manager = SegmentManager::new(tmp.path().to_path_buf(), "mail");
        let actor = make_actor(50);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
            b"hint-a",
        )
        .await;
        seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
            b"hint-b",
        )
        .await;
        seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "Junk",
            b"bj",
            1_700_000_003,
            "",
            b"hint-j",
        )
        .await;

        // All mailboxes (None).
        let rows = db
            .query_bridge_imap_index_segments(&manager, &actor, None, 0, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3, "3 total across all mailboxes");
        // Verify all three hints surface; ordering is by per-mailbox
        // modseq so don't hardcode positions, just set membership.
        let hints: Vec<&[u8]> = rows
            .iter()
            .map(|r| r.encrypted_index_hint.as_slice())
            .collect();
        assert!(hints.contains(&b"hint-a".as_slice()));
        assert!(hints.contains(&b"hint-b".as_slice()));
        assert!(hints.contains(&b"hint-j".as_slice()));

        // Filter by mailbox=Some("INBOX") — only the 2 INBOX rows.
        let inbox_rows = db
            .query_bridge_imap_index_segments(&manager, &actor, Some("INBOX"), 0, None)
            .await
            .unwrap();
        assert_eq!(inbox_rows.len(), 2);
        assert_eq!(inbox_rows[0].mailbox, "INBOX");
        assert_eq!(inbox_rows[1].mailbox, "INBOX");
        // Ascending modseq: hint-a (placed first) then hint-b.
        assert_eq!(inbox_rows[0].encrypted_index_hint, b"hint-a");
        assert_eq!(inbox_rows[1].encrypted_index_hint, b"hint-b");
    }

    #[tokio::test]
    async fn query_bridge_imap_index_segments_since_modseq_filter() {
        let db = CacheDb::open_in_memory().unwrap();
        let tmp = tempfile::TempDir::new().expect("tmp");
        let manager = SegmentManager::new(tmp.path().to_path_buf(), "mail");
        let actor = make_actor(51);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
            b"h1",
        )
        .await;
        seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
            b"h2",
        )
        .await;
        let (_, msg3_id) = seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
            b"h3",
        )
        .await;

        // After seeding 3 messages, modseqs are 2, 3, 4.
        // since_modseq=3 should return only msg3 (modseq=4).
        let rows = db
            .query_bridge_imap_index_segments(&manager, &actor, Some("INBOX"), 3, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].encrypted_index_hint, b"h3");
        assert_eq!(rows[0].message_id, msg3_id);
    }

    #[tokio::test]
    async fn query_bridge_imap_index_segments_limit_truncates() {
        let db = CacheDb::open_in_memory().unwrap();
        let tmp = tempfile::TempDir::new().expect("tmp");
        let manager = SegmentManager::new(tmp.path().to_path_buf(), "mail");
        let actor = make_actor(52);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
            b"h1",
        )
        .await;
        seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
            b"h2",
        )
        .await;
        seed_message_with_hint(
            &db,
            &manager,
            &actor,
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
            b"h3",
        )
        .await;

        // limit=2: should return first 2 ascending by modseq.
        let rows = db
            .query_bridge_imap_index_segments(&manager, &actor, Some("INBOX"), 0, Some(2))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].encrypted_index_hint, b"h1");
        assert_eq!(rows[1].encrypted_index_hint, b"h2");
    }

    #[tokio::test]
    async fn max_highestmodseq_for_actor_returns_1_with_no_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(60);

        // No mailboxes seeded — should return 1.
        let val = db.max_highestmodseq_for_actor(&actor, None).await.unwrap();
        assert_eq!(val, 1);
    }

    #[tokio::test]
    async fn max_highestmodseq_for_actor_returns_max_across_mailboxes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(61);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        // Place 3 messages in INBOX (bumps highestmodseq to 4) and 1 in Junk (→ 2).
        seed_message(
            &db,
            &actor,
            &make_message_id(80),
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(81),
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(82),
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;
        seed_message(
            &db,
            &actor,
            &make_message_id(83),
            "Junk",
            b"bj",
            1_700_000_004,
            "",
        )
        .await;

        // Max across all mailboxes = max(4, 2) = 4.
        let all = db.max_highestmodseq_for_actor(&actor, None).await.unwrap();
        assert_eq!(all, 4);

        // Scoped to INBOX only → 4.
        let inbox = db
            .max_highestmodseq_for_actor(&actor, Some("INBOX"))
            .await
            .unwrap();
        assert_eq!(inbox, 4);

        // Scoped to Junk only → 2.
        let junk = db
            .max_highestmodseq_for_actor(&actor, Some("Junk"))
            .await
            .unwrap();
        assert_eq!(junk, 2);
    }

    // ── C.4 DB tests ──────────────────────────────────────────────────────────

    /// Seed 3 messages (UIDs 1/2/3) in INBOX for the given actor. Returns
    /// the db so the caller can keep calling methods.
    async fn setup_three_messages(db: &CacheDb, actor: &[u8; 32]) {
        db.ensure_bridge_imap_mailboxes(actor).await.unwrap();
        seed_message(
            db,
            actor,
            &make_message_id(200),
            "INBOX",
            b"b1",
            1_700_000_001,
            "",
        )
        .await;
        seed_message(
            db,
            actor,
            &make_message_id(201),
            "INBOX",
            b"b2",
            1_700_000_002,
            "",
        )
        .await;
        seed_message(
            db,
            actor,
            &make_message_id(202),
            "INBOX",
            b"b3",
            1_700_000_003,
            "",
        )
        .await;
    }

    #[tokio::test]
    async fn apply_store_flags_set_updates_two_uids_shared_modseq() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(70);
        setup_three_messages(&db, &actor).await;

        let flags = vec!["\\Seen".to_string()];
        let outcome = db
            .apply_store_flags(&actor, "INBOX", &[1, 2], StoreFlagsDbOp::Set, &flags, None)
            .await
            .unwrap();

        assert_eq!(outcome.updated.len(), 2, "two UIDs touched");
        // Tuple shape: (uid, before_flags, after_flags, modseq).
        assert_eq!(outcome.updated[0].0, 1);
        assert_eq!(outcome.updated[0].1, "", "before_flags empty");
        assert_eq!(outcome.updated[0].2, "\\Seen", "after_flags");
        assert_eq!(outcome.updated[1].0, 2);
        assert_eq!(outcome.updated[1].1, "", "before_flags empty");
        assert_eq!(outcome.updated[1].2, "\\Seen", "after_flags");
        // All touched rows share the same bumped modseq.
        assert_eq!(outcome.updated[0].3, outcome.updated[1].3, "shared modseq");
        assert_eq!(outcome.highestmodseq, outcome.updated[0].3);
    }

    #[tokio::test]
    async fn apply_store_flags_add_accumulates_flags() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(71);
        setup_three_messages(&db, &actor).await;

        // First: set \Seen on uid 1.
        db.apply_store_flags(
            &actor,
            "INBOX",
            &[1],
            StoreFlagsDbOp::Set,
            &["\\Seen".to_string()],
            None,
        )
        .await
        .unwrap();

        // Then: add \Flagged to uid 1 → should have both \Flagged \Seen (BTreeSet order).
        let outcome = db
            .apply_store_flags(
                &actor,
                "INBOX",
                &[1],
                StoreFlagsDbOp::Add,
                &["\\Flagged".to_string()],
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.updated.len(), 1);
        // Tuple shape: (uid, before_flags, after_flags, modseq).
        assert_eq!(
            outcome.updated[0].1, "\\Seen",
            "before_flags = prior \\Seen"
        );
        // BTreeSet lexicographic: \Flagged < \Seen.
        assert_eq!(outcome.updated[0].2, "\\Flagged \\Seen", "after_flags");
    }

    #[tokio::test]
    async fn apply_store_flags_remove_drops_flag() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(72);
        setup_three_messages(&db, &actor).await;

        // Set \Flagged and \Seen on uid 1.
        db.apply_store_flags(
            &actor,
            "INBOX",
            &[1],
            StoreFlagsDbOp::Set,
            &["\\Flagged".to_string(), "\\Seen".to_string()],
            None,
        )
        .await
        .unwrap();

        // Remove \Seen → only \Flagged should remain.
        let outcome = db
            .apply_store_flags(
                &actor,
                "INBOX",
                &[1],
                StoreFlagsDbOp::Remove,
                &["\\Seen".to_string()],
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.updated.len(), 1);
        // Tuple shape: (uid, before_flags, after_flags, modseq).
        assert_eq!(
            outcome.updated[0].1, "\\Flagged \\Seen",
            "before_flags = prior set"
        );
        assert_eq!(
            outcome.updated[0].2, "\\Flagged",
            "after_flags drops \\Seen"
        );
    }

    #[tokio::test]
    async fn apply_store_flags_missing_uid_returns_empty() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(73);
        setup_three_messages(&db, &actor).await;

        let hms_before = db
            .get_bridge_imap_mailbox_state(&actor, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        let outcome = db
            .apply_store_flags(
                &actor,
                "INBOX",
                &[999],
                StoreFlagsDbOp::Set,
                &["\\Seen".to_string()],
                None,
            )
            .await
            .unwrap();

        assert!(outcome.updated.is_empty(), "missing UID → no updates");
        // highestmodseq MUST NOT be bumped when nothing was touched.
        assert_eq!(outcome.highestmodseq, hms_before, "highestmodseq unchanged");
    }

    #[tokio::test]
    async fn apply_store_flags_nonexistent_mailbox_returns_empty_hms_1() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(74);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let outcome = db
            .apply_store_flags(
                &actor,
                "Nonexistent",
                &[1],
                StoreFlagsDbOp::Set,
                &["\\Seen".to_string()],
                None,
            )
            .await
            .unwrap();

        assert!(outcome.updated.is_empty());
        assert_eq!(
            outcome.highestmodseq, 1,
            "nonexistent mailbox → highestmodseq=1"
        );
    }

    #[tokio::test]
    async fn apply_expunge_plain_expunges_deleted_messages() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(75);
        setup_three_messages(&db, &actor).await;

        // Set \Deleted on uids 1 and 3.
        db.apply_store_flags(
            &actor,
            "INBOX",
            &[1, 3],
            StoreFlagsDbOp::Set,
            &["\\Deleted".to_string()],
            None,
        )
        .await
        .unwrap();

        let hms_before = db
            .get_bridge_imap_mailbox_state(&actor, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        // Plain EXPUNGE (empty uids).
        let outcome = db
            .apply_expunge(&actor, "INBOX", &[], &[], 1_752_000_000)
            .await
            .unwrap();

        assert_eq!(
            outcome.expunged_uids,
            vec![1, 3],
            "uids 1 and 3 expunged, ascending"
        );
        assert!(outcome.highestmodseq > hms_before, "highestmodseq bumped");

        // Verify rows are gone from bridge_imap_messages.
        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "uid 2 should remain");

        // Verify bridge_imap_expunged has 2 entries with the shared modseq.
        let exp_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_expunged \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX' AND modseq = ?2",
                rusqlite::params![&actor[..], outcome.highestmodseq],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exp_count, 2, "2 expunge log entries with shared modseq");
    }

    #[tokio::test]
    async fn apply_expunge_uid_not_deleted_returns_empty() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(76);
        setup_three_messages(&db, &actor).await;

        // uid 2 has no \Deleted flag.
        let hms_before = db
            .get_bridge_imap_mailbox_state(&actor, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        let outcome = db
            .apply_expunge(&actor, "INBOX", &[2], &[], 1_752_000_000)
            .await
            .unwrap();

        assert!(
            outcome.expunged_uids.is_empty(),
            "uid 2 not \\Deleted => nothing expunged"
        );
        assert_eq!(outcome.highestmodseq, hms_before, "highestmodseq unchanged");
    }

    #[tokio::test]
    async fn apply_expunge_uid_expunge_only_removes_listed_deleted_uid() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(77);
        setup_three_messages(&db, &actor).await;

        // Set \Deleted on all three.
        db.apply_store_flags(
            &actor,
            "INBOX",
            &[1, 2, 3],
            StoreFlagsDbOp::Set,
            &["\\Deleted".to_string()],
            None,
        )
        .await
        .unwrap();

        // UID EXPUNGE with only uid 2.
        let outcome = db
            .apply_expunge(&actor, "INBOX", &[2], &[], 1_752_000_000)
            .await
            .unwrap();

        assert_eq!(outcome.expunged_uids, vec![2], "only uid 2 removed");

        let conn = db.conn().await;
        let remaining: Vec<i64> = conn
            .prepare("SELECT uid FROM bridge_imap_messages WHERE actor_id = ?1 AND mailbox = 'INBOX' ORDER BY uid")
            .unwrap()
            .query_map(rusqlite::params![&actor[..]], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(remaining, vec![1i64, 3i64], "uids 1 and 3 still present");
    }

    #[tokio::test]
    async fn apply_expunge_nonexistent_mailbox_returns_empty_hms_1() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(78);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let outcome = db
            .apply_expunge(&actor, "Nonexistent", &[], &[], 1_752_000_000)
            .await
            .unwrap();

        assert!(outcome.expunged_uids.is_empty());
        assert_eq!(
            outcome.highestmodseq, 1,
            "nonexistent mailbox → highestmodseq=1"
        );
    }

    #[tokio::test]
    async fn max_highestmodseq_for_actor_with_nonexistent_mailbox_returns_1() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(62);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        // Querying a mailbox that has no state row returns 1.
        let val = db
            .max_highestmodseq_for_actor(&actor, Some("Nonexistent"))
            .await
            .unwrap();
        assert_eq!(val, 1);
    }

    // ── C.5 DB tests: apply_copy + apply_move ─────────────────────────────────

    /// Seed a mail record in `segment_records` (kind='mail') and place it
    /// in the given mailbox. Returns the allocated UID.
    ///
    /// Choice (b) from Plan 2 Task 8 — raw SQL insert into `segment_records`
    /// (no segment file). These apply_*/copy_*/move_*/expunge_* tests
    /// exercise `bridge_imap_messages` mutations, not the segment-file
    /// content, so a synthetic segment_id is fine. (No byte_offset column.)
    async fn seed_msg(
        db: &CacheDb,
        actor: &[u8; 32],
        msg_id: &[u8; 32],
        mailbox: &str,
        flags: &str,
    ) -> u32 {
        let fields = sample_inbound_fields(actor, b"test-body");
        let record_cid = Cid::from_digest_dag_cbor(*msg_id);
        db.conn()
            .await
            .execute(
                "INSERT OR IGNORE INTO segment_records \
                (scope_id, kind, segment_id, record_cid, bucket, \
                 tombstoned, \
                 received_at, sender_dom, spam_disp, is_own_submission) \
             VALUES (?1, 'mail', 1, ?2, '2026-05', 0, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &actor[..],
                    &record_cid.as_bytes()[..],
                    fields.timestamp,
                    &fields.sender_domain,
                    &fields.spam_disposition,
                    fields.is_own_submission,
                ],
            )
            .unwrap();
        let (uid, _modseq) = db
            .place_inbound_mail(actor, msg_id, mailbox, fields.timestamp, flags, "", true)
            .await
            .unwrap()
            .expect("seed_msg: place_inbound_mail returned None");
        uid
    }

    #[tokio::test]
    async fn apply_copy_basic_two_messages() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(80);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg1 = make_message_id(81);
        let msg2 = make_message_id(82);
        seed_msg(&db, &actor, &msg1, "INBOX", "").await;
        seed_msg(&db, &actor, &msg2, "INBOX", "").await;

        // Copy uids 1 and 2 to Archive.
        let outcome = db
            .apply_copy(&actor, "INBOX", &[1, 2], "Archive")
            .await
            .unwrap();
        assert_eq!(outcome.dest_uid_validity, 1);
        assert_eq!(outcome.copied, vec![(1, 1), (2, 2)]);
        assert_eq!(
            outcome.dest_highestmodseq, 3,
            "start 1, +1 per row → 3 after two"
        );

        // INBOX still has uids 1 and 2.
        let inbox_rows = db.list_bridge_imap_messages(&actor, "INBOX").await.unwrap();
        assert_eq!(inbox_rows.len(), 2, "INBOX untouched");

        // Archive has 2 rows pointing at msg1 and msg2.
        let archive_rows = db
            .list_bridge_imap_messages(&actor, "Archive")
            .await
            .unwrap();
        assert_eq!(archive_rows.len(), 2, "Archive has 2 rows");
        assert_eq!(archive_rows[0].0, 1, "first dest uid=1");
        assert_eq!(archive_rows[1].0, 2, "second dest uid=2");

        // Dest state row.
        let state = db
            .get_bridge_imap_mailbox_state(&actor, "Archive")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.uid_next, 3);
        assert_eq!(state.highestmodseq, 3);
    }

    #[tokio::test]
    async fn apply_copy_second_copy_gets_higher_uids() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(83);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg1 = make_message_id(84);
        let msg2 = make_message_id(85);
        seed_msg(&db, &actor, &msg1, "INBOX", "").await;
        seed_msg(&db, &actor, &msg2, "INBOX", "").await;

        // First copy: uids 1 and 2 → Archive uids 1 and 2.
        db.apply_copy(&actor, "INBOX", &[1, 2], "Archive")
            .await
            .unwrap();

        // Second copy of the same source UIDs → Archive uids 3 and 4.
        let outcome2 = db
            .apply_copy(&actor, "INBOX", &[1, 2], "Archive")
            .await
            .unwrap();
        assert_eq!(outcome2.copied, vec![(1, 3), (2, 4)]);
        assert_eq!(outcome2.dest_highestmodseq, 5);
    }

    #[tokio::test]
    async fn apply_copy_preserves_flagged_strips_recent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(86);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        // Seed a message with \Flagged and \Recent.
        let msg1 = make_message_id(87);
        seed_msg(&db, &actor, &msg1, "INBOX", "").await;
        // Set \Flagged and \Recent directly.
        db.conn()
            .await
            .execute(
                "UPDATE bridge_imap_messages SET flags = '\\Flagged \\Recent' \
             WHERE actor_id = ?1 AND mailbox = 'INBOX' AND uid = 1",
                rusqlite::params![&actor[..]],
            )
            .unwrap();

        let outcome = db
            .apply_copy(&actor, "INBOX", &[1], "Archive")
            .await
            .unwrap();
        assert_eq!(outcome.copied.len(), 1);

        // Check the Archive row's flags.
        let archive_rows = db
            .list_bridge_imap_messages(&actor, "Archive")
            .await
            .unwrap();
        assert_eq!(archive_rows.len(), 1);
        let flags = &archive_rows[0].1;
        assert!(flags.contains("\\Flagged"), "\\Flagged must be preserved");
        assert!(!flags.contains("\\Recent"), "\\Recent must be stripped");
    }

    #[tokio::test]
    async fn apply_copy_missing_source_uid_skipped() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(88);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg1 = make_message_id(89);
        seed_msg(&db, &actor, &msg1, "INBOX", "").await;

        // Request uids [1, 99] — uid 99 doesn't exist.
        let outcome = db
            .apply_copy(&actor, "INBOX", &[1, 99], "Archive")
            .await
            .unwrap();
        assert_eq!(outcome.copied.len(), 1, "uid 99 skipped");
        assert_eq!(outcome.copied[0], (1, 1));
    }

    #[tokio::test]
    async fn apply_copy_all_missing_returns_empty_copied() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(90);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let outcome = db
            .apply_copy(&actor, "INBOX", &[42, 99], "Archive")
            .await
            .unwrap();
        assert!(outcome.copied.is_empty(), "no source rows → empty copied");
        // Dest state row was auto-created but counters unchanged (no bump).
        let state = db
            .get_bridge_imap_mailbox_state(&actor, "Archive")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.highestmodseq, 1, "no bump when nothing copied");
    }

    #[tokio::test]
    async fn apply_move_basic() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(91);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let msg1 = make_message_id(92);
        let msg2 = make_message_id(93);
        seed_msg(&db, &actor, &msg1, "INBOX", "").await;
        seed_msg(&db, &actor, &msg2, "INBOX", "").await;

        let inbox_hms_before = db
            .get_bridge_imap_mailbox_state(&actor, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        // Move uid 1 to Archive.
        let outcome = db
            .apply_move(&actor, "INBOX", &[1], "Archive")
            .await
            .unwrap();
        assert_eq!(outcome.moved, vec![(1, 1)]);
        assert_eq!(outcome.dest_uid_validity, 1);
        assert!(
            outcome.source_highestmodseq > inbox_hms_before,
            "source modseq bumped"
        );

        // INBOX uid 1 gone, uid 2 still there.
        let inbox_rows = db.list_bridge_imap_messages(&actor, "INBOX").await.unwrap();
        assert_eq!(inbox_rows.len(), 1);
        assert_eq!(inbox_rows[0].0, 2, "uid 2 remains");

        // Archive has the row.
        let archive_rows = db
            .list_bridge_imap_messages(&actor, "Archive")
            .await
            .unwrap();
        assert_eq!(archive_rows.len(), 1);
        assert_eq!(archive_rows[0].0, 1);

        // bridge_imap_expunged has a log entry for the source, with the
        // SAME modseq as the move's bumped source_highestmodseq (single
        // shared bump per MOVE batch — RFC 7162 CONDSTORE).
        let expunged: Vec<(i64, i64)> = db
            .conn()
            .await
            .prepare(
                "SELECT uid, modseq FROM bridge_imap_expunged \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX' ORDER BY uid",
            )
            .unwrap()
            .query_map(rusqlite::params![&actor[..]], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            expunged,
            vec![(1i64, outcome.source_highestmodseq)],
            "uid 1 logged with the bumped source modseq"
        );
    }

    #[tokio::test]
    async fn apply_copy_preserves_deleted_strips_recent() {
        // IMAP COPY (RFC 3501 §6.4.7): destination message gets all of the
        // source's flags EXCEPT \Recent. \Deleted is explicitly preserved
        // even though intuitively it might seem dropped — COPY is purely a
        // placement op; cleanup is a subsequent EXPUNGE.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(60);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();
        let msg = make_message_id(61);
        seed_msg(&db, &actor, &msg, "INBOX", "\\Deleted \\Flagged \\Recent").await;

        let outcome = db
            .apply_copy(&actor, "INBOX", &[1], "Archive")
            .await
            .unwrap();
        assert_eq!(outcome.copied, vec![(1, 1)]);

        let archive_rows = db
            .list_bridge_imap_messages(&actor, "Archive")
            .await
            .unwrap();
        assert_eq!(archive_rows.len(), 1);
        let copied_flags = &archive_rows[0].1;
        let flag_tokens: std::collections::BTreeSet<&str> =
            copied_flags.split_whitespace().collect();
        assert!(
            flag_tokens.contains("\\Deleted"),
            "\\Deleted preserved on COPY"
        );
        assert!(
            flag_tokens.contains("\\Flagged"),
            "\\Flagged preserved on COPY"
        );
        assert!(
            !flag_tokens.contains("\\Recent"),
            "\\Recent stripped on COPY"
        );
    }

    #[tokio::test]
    async fn apply_move_empty_all_missing_no_source_touches() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = make_actor(94);
        db.ensure_bridge_imap_mailboxes(&actor).await.unwrap();

        let source_hms_before = db
            .get_bridge_imap_mailbox_state(&actor, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;

        // Move uids that don't exist.
        let outcome = db
            .apply_move(&actor, "INBOX", &[99, 100], "Archive")
            .await
            .unwrap();
        assert!(outcome.moved.is_empty(), "nothing moved");

        // Source modseq unchanged.
        let source_hms_after = db
            .get_bridge_imap_mailbox_state(&actor, "INBOX")
            .await
            .unwrap()
            .unwrap()
            .highestmodseq;
        assert_eq!(
            source_hms_before, source_hms_after,
            "source highestmodseq unchanged"
        );

        // No expunge log entries.
        let count: i64 = db
            .conn()
            .await
            .query_row(
                "SELECT COUNT(*) FROM bridge_imap_expunged WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "no expunge log entries");
    }
}
