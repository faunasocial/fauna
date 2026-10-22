//! Mailbox-migration import sessions + per-actor message-dedup index
//! (`docs/goal/behavior/mailbox-migration.md` § Progress lives nest-side,
//! § Dedup).
//!
//! Every method is **actor-scoped**: the caller passes the authenticated
//! actor and every query filters by it, so one actor can never read or
//! mutate another's sessions or dedup rows (the RPC layer's User-class
//! caller-scoping, enforced again here).
//!
//! Sessions are ephemeral bookkeeping (the imported *messages* live in
//! `bridge_imap_messages`); a row expires 30 days after its last progress
//! (`expires_at`), swept lazily by `create_import_session` /
//! `list_import_sessions` — there is no background task, matching the
//! "nest GC-removes the row after that" contract with zero scheduler
//! footprint.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::blob_col_to_array;

/// 30 days — `mailbox-migration.md` § Progress lives nest-side (`expires_at`).
pub const IMPORT_SESSION_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// The states the `import_sessions` machine allows — the export twin's
/// `ExportState` shape (`mail-export.md` § Architectural rules, *Symmetric
/// session row with imports*). Every write that sets a state takes this type,
/// never a string, so a spelling the table's `CHECK` would refuse is
/// unrepresentable at the call site. Rows read back carry the string form
/// ([`ImportSessionRow::state`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportState {
    Running,
    Paused,
    Errored,
    Completed,
    Cancelled,
}

impl ImportState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Errored => "errored",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A client-supplied counter as the `INTEGER` column stores it: `u64` above
/// `i64::MAX` has no exact `i64`, and an `as` cast would store a negative
/// count that reads back as an enormous `u64`. The handlers refuse such a
/// value as malformed first; this is the floor under them.
pub fn import_counter_to_sql(name: &str, value: u64) -> Result<i64> {
    i64::try_from(value).with_context(|| format!("import {name} {value} exceeds i64::MAX"))
}

/// `col + param` for a signed fold delta: saturating at `i64::MAX` instead of
/// overflowing (SQLite turns an overflowing integer sum into a REAL that
/// [`row_from_sql`] cannot decode), and floored at zero (a re-walk can fold a
/// negative delta). A non-positive `param` cannot overflow the sum, so only a
/// positive one takes the saturating arm.
fn saturating_add_floor_sql(col: &str, param: &str) -> String {
    let max = i64::MAX;
    format!(
        "CASE WHEN {param} > 0 AND {col} > {max} - {param} THEN {max} \
         ELSE MAX({col} + {param}, 0) END"
    )
}

/// One `import_sessions` row. `cursors` maps mailbox → (last processed
/// source UID, source UIDVALIDITY) — the resume cursor pair.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportSessionRow {
    pub session_id: String,
    pub actor_id: [u8; 32],
    pub source_descriptor: String,
    pub state: String,
    pub started_at: i64,
    pub last_progress_at: i64,
    pub total_count: u64,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    pub cursors: BTreeMap<String, (u32, u32)>,
    pub error_reason: String,
    pub expires_at: i64,
    /// The source mailbox names selected at `create_import_session`
    /// (`mailbox-migration.md` § Resume protocol) — recorded once, never
    /// mutated after. Empty when the session was created with no mailbox
    /// selection.
    pub scope: Vec<String>,
    /// The scope step's "since" date, as the bare `YYYY-MM-DD` the user
    /// typed (`mailbox-migration.md` § Wizard steps step 3) — recorded once
    /// beside `scope`, never mutated after; empty means unbounded. What lets
    /// a resumed walk re-apply the range rather than import everything the
    /// user excluded.
    pub date_from: String,
    /// The scope step's "Import duplicates anyway" choice
    /// (`mailbox-migration.md` § Opt-out per session) — recorded once beside
    /// `date_from`, never mutated after. What lets a resume after a client
    /// restart send every message's `skip_dedup` as the user chose it rather
    /// than guess (§ Resume protocol step 3).
    pub skip_dedup: bool,
    /// The client-minted sealed label over `source_descriptor`. `None` for a
    /// row a keyless client opened, which rests sealless by design.
    pub source_sealed: Option<Vec<u8>>,
    /// `source_hash` — the seal's salt, and the key the multi-device source
    /// lock reads. Travels with `source_sealed` to every reader: the plaintext
    /// it derives from is scrubbed once the seal rests.
    pub source_hash: Option<Vec<u8>>,
}

/// Outcome of `create_import_session` — `SourceLocked` is the multi-device
/// per-source lock (`mailbox-migration.md` § Architectural rules).
#[derive(Debug, PartialEq, Eq)]
pub enum CreateImportSessionOutcome {
    Created,
    SourceLocked,
}

/// Outcome of `discard_import_session` — a failed import's Dismiss
/// (`mailbox-migration.md` § A failed import is a record the wizard shows
/// until the user moves on). `Live` carries the state of a row the discard
/// refused: a running or paused import is cancelled, never discarded.
#[derive(Debug, PartialEq, Eq)]
pub enum DiscardImportSessionOutcome {
    Deleted,
    Missing,
    Live(String),
}

/// How one processed source message counts (`mailbox-migration.md`
/// § Progress lives nest-side): `QuotaRefused` is the `quota_exceeded` skip,
/// counted in `skipped_count` like any skip but the one outcome that holds the
/// resume cursor (§ Failure handling → *Nest-side errors*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFold {
    Imported,
    Skipped,
    QuotaRefused,
    Errored,
}

/// One mailbox's quota hold, resting in the row's `quota_holds` column while
/// a message the full mailbox turned away is still unresolved — or while a
/// resumed walk is still re-sending what the hold kept it from passing.
///
/// The cursor stands just short of the lowest `QuotaRefused` UID in `seen`, so
/// a resume after a restart, which reads only the row, takes the import up at
/// the first message that did not fit (§ Failure handling). `seen` records
/// every UID folded at or above that hold and how it counted, so the re-walk
/// the hold forces counts each message once: a message re-sent after it
/// counted moves no counter, except a turned-away one that now lands
/// elsewhere, which moves from `skipped_count` to its new counter.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuotaHold {
    /// The UIDVALIDITY every UID here belongs to.
    pub uid_validity: u32,
    /// The highest UID folded in this namespace: where the cursor stands once
    /// the hold lifts.
    pub high: u32,
    /// Each UID folded at or above the hold, with how it counted.
    pub seen: BTreeMap<u32, ImportFold>,
}

impl QuotaHold {
    /// The lowest UID still turned away, if any — what holds the cursor.
    fn held_at(&self) -> Option<u32> {
        self.seen
            .iter()
            .find(|(_, k)| **k == ImportFold::QuotaRefused)
            .map(|(uid, _)| *uid)
    }
}

/// Signed counter moves from one [`fold_mailbox`] call.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FoldDelta {
    pub imported: i64,
    pub skipped: i64,
    pub errored: i64,
}

impl FoldDelta {
    fn add(&mut self, kind: ImportFold, by: i64) {
        match kind {
            ImportFold::Imported => self.imported += by,
            ImportFold::Skipped | ImportFold::QuotaRefused => self.skipped += by,
            ImportFold::Errored => self.errored += by,
        }
    }
}

/// Fold processed messages of one mailbox into its resume cursor and quota
/// hold (`mailbox-migration.md` § Progress lives nest-side, § Failure handling
/// → *Nest-side errors*). Both nest paths that move a cursor — a sent
/// message's outcome and `record_import_outcomes` — come through here, so
/// neither can carry the cursor past a message the mailbox had no room for.
///
/// - **The cursor moves forward only within one UID namespace**: a call that
///   reports a lower UID than one already folded (the client's skips for a
///   window are reported after that window's sends) never walks it back. A
///   different UIDVALIDITY is a rebuilt mailbox and replaces cursor and hold.
/// - **A `QuotaRefused` UID holds it** one short of the lowest such UID, for
///   as long as that message stays turned away. The hold lifts when the
///   message is folded again as anything else: the resume that imports it
///   once there is room, or the "skip remaining" choice recording it as
///   skipped for good. The cursor then moves on to `high`.
/// - **Each message counts once.** A UID in the hold's `seen` already
///   counted; folded again it moves nothing, except a turned-away message now
///   landing elsewhere, which moves out of `skipped`. The hold is dropped once
///   it has lifted and the walk passes its highest seen UID.
pub fn fold_mailbox(
    cursor: Option<(u32, u32)>,
    hold: Option<QuotaHold>,
    uid_validity: u32,
    folds: &[(u32, ImportFold)],
) -> (Option<(u32, u32)>, Option<QuotaHold>, FoldDelta) {
    let mut delta = FoldDelta::default();
    let mut hold = hold.filter(|h| h.uid_validity == uid_validity);
    // The furthest point already processed in this namespace.
    let mut high = match (&hold, cursor) {
        (Some(h), _) => Some(h.high),
        (None, Some((uid, v))) if v == uid_validity => Some(uid),
        _ => None,
    };
    for &(uid, kind) in folds {
        if hold
            .as_ref()
            .is_some_and(|h| h.held_at().is_none() && h.seen.keys().next_back() < Some(&uid))
        {
            hold = None;
        }
        match hold.as_mut().and_then(|h| h.seen.get_mut(&uid)) {
            Some(old) => {
                if *old == ImportFold::QuotaRefused && kind != ImportFold::QuotaRefused {
                    delta.add(*old, -1);
                    delta.add(kind, 1);
                    *old = kind;
                }
            }
            None => {
                delta.add(kind, 1);
                let above_hold = hold
                    .as_ref()
                    .and_then(QuotaHold::held_at)
                    .is_some_and(|at| uid >= at);
                if kind == ImportFold::QuotaRefused || above_hold {
                    hold.get_or_insert_with(|| QuotaHold {
                        uid_validity,
                        ..QuotaHold::default()
                    })
                    .seen
                    .insert(uid, kind);
                }
            }
        }
        high = Some(high.map_or(uid, |h| h.max(uid)));
    }
    if let (Some(h), Some(high)) = (hold.as_mut(), high) {
        h.high = high;
    }
    let cursor = match hold.as_ref().and_then(QuotaHold::held_at) {
        Some(at) => Some((at.saturating_sub(1), uid_validity)),
        None => high.map(|h| (h, uid_validity)).or(cursor),
    };
    (cursor, hold, delta)
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportSessionRow> {
    let actor: Vec<u8> = row.get(1)?;
    let cursors_json: String = row.get(10)?;
    let scope_json: String = row.get(13)?;
    Ok(ImportSessionRow {
        session_id: row.get(0)?,
        actor_id: blob_col_to_array(actor, 1, "actor_id")?,
        source_descriptor: row.get(2)?,
        state: row.get(3)?,
        started_at: row.get(4)?,
        last_progress_at: row.get(5)?,
        total_count: row.get::<_, i64>(6)? as u64,
        imported_count: row.get::<_, i64>(7)? as u64,
        skipped_count: row.get::<_, i64>(8)? as u64,
        errored_count: row.get::<_, i64>(9)? as u64,
        // Verified fold (`nest/common.md` § Unreadable stored values, row 90):
        // an undecodable `cursors` blob folds to an empty map, i.e. every
        // mailbox resumes its scan from UID 0. This re-does I/O, it does not
        // duplicate messages — `bridge_import_handlers.rs::import_message`
        // checks `dedup_hit` against the actor-wide, cursor-independent
        // `actor_message_dedup` table before persisting anything, so an
        // already-imported message is skipped on re-scan regardless of what
        // the lost cursor would have said. Only the wizard's explicit "import
        // duplicates anyway" (`skip_dedup`) mode can duplicate on replay, and
        // that is the user's own opted-in consequence of that mode, not one
        // this fold introduces.
        cursors: serde_json::from_str(&cursors_json).unwrap_or_default(),
        error_reason: row.get(11)?,
        expires_at: row.get(12)?,
        // Same verified fold as `cursors` above: an undecodable scope blob
        // (shouldn't happen — this nest wrote it) folds to empty rather than
        // erroring the whole row read. An empty scope only degrades resume
        // (§ Resume protocol has nothing to iterate), it never loses mail.
        scope: serde_json::from_str(&scope_json).unwrap_or_default(),
        source_sealed: row.get(14)?,
        source_hash: row.get(15)?,
        date_from: row.get(16)?,
        skip_dedup: row.get::<_, i64>(17)? != 0,
    })
}

const SELECT_COLS: &str = "session_id, actor_id, source_descriptor, state, started_at, \
     last_progress_at, total_count, imported_count, skipped_count, errored_count, \
     cursors, error_reason, expires_at, scope, source_sealed, source_hash, date_from, \
     skip_dedup";

impl crate::db::CacheDb {
    /// Open a fresh `running` session at the wizard's durable commit point.
    /// Returns `SourceLocked` when another `running`/`paused` session for the
    /// same `(actor, source)` exists (checked under the connection mutex, so
    /// the check-then-insert is race-free in-process; the partial UNIQUE index
    /// backstops it at the schema level).
    ///
    /// ⚠ **The lock reads `source_hash`, never `source_descriptor`.** The
    /// plaintext is blanked by the boot scrub the moment the row rests sealed
    /// (`migrations::SCRUB_PLANES`), so a plaintext-keyed check silently stops
    /// matching after the first reboot: it falls through to the INSERT and
    /// degrades the typed [`CreateImportSessionOutcome::SourceLocked`] — and
    /// with it the wizard's "already being imported on another device"
    /// (`mailbox-migration.md` § Architectural rules) — into whatever opaque
    /// error the partial UNIQUE backstop raises. `encryption-at-rest.md`
    /// § Implementation status today bullet 15 (b) names this rebuild as part
    /// of the surface's sealed posture ("its plaintext lock index rebuilt onto
    /// `source_hash`"); the index moved at the v32 flip, and this is the query
    /// half.
    ///
    /// `source_sealed` is the client-minted label over `source_descriptor`
    /// (`fauna_core::label_custody::seal_import_source`). The nest stores it
    /// verbatim and never opens it — it holds no root on this plane. `None` is
    /// the keyless writer's ratified degrade (bearer-only connection, web
    /// arm): the row rests sealless, and its plaintext stays for the row's
    /// life — the boot scrub clears only a row whose seal rests (the one-time
    /// never-sealed clear was the S9 flip's, retired at the genesis), so this
    /// is the named keyless-writer gap (`path-sealing.md`), not a scrubbed row.
    pub async fn create_import_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        source_descriptor: &str,
        total_count: u64,
        scope: &[String],
        date_from: &str,
        skip_dedup: bool,
        source_sealed: Option<&[u8]>,
    ) -> Result<CreateImportSessionOutcome> {
        let total_count = import_counter_to_sql("total_count", total_count)?;
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        // Lazy GC — expired rows must not hold the source lock.
        conn.execute(
            "DELETE FROM import_sessions WHERE actor_id = ?1 AND expires_at < ?2",
            rusqlite::params![&actor[..], now],
        )
        .context("gc import_sessions")?;
        let source_hash = fauna_core::path_crypto::import_source_hash(source_descriptor);
        let locked: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM import_sessions
                 WHERE actor_id = ?1 AND source_hash = ?2
                   AND state IN ('running','paused')",
                rusqlite::params![&actor[..], source_hash.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("check import source lock")?;
        if locked.is_some() {
            return Ok(CreateImportSessionOutcome::SourceLocked);
        }
        let scope_json = serde_json::to_string(scope).context("encode import scope")?;
        // § A failed import is a record the wizard shows until the user moves
        // on: a new start is the user's answer to a failure, so the caller's
        // `errored` records go in the same write as the insert (the export
        // twin's `create_export_session` rule).
        let tx = conn
            .unchecked_transaction()
            .context("begin start import_session")?;
        tx.execute(
            "DELETE FROM import_sessions WHERE actor_id = ?1 AND state = 'errored'",
            rusqlite::params![&actor[..]],
        )
        .context("retire errored import_sessions")?;
        tx.execute(
            "INSERT INTO import_sessions
               (session_id, actor_id, source_descriptor, state, started_at,
                last_progress_at, total_count, expires_at, source_hash, scope,
                source_sealed, date_from, skip_dedup)
             VALUES (?1, ?2, ?3, 'running', ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                session_id,
                &actor[..],
                source_descriptor,
                now,
                total_count,
                now + IMPORT_SESSION_TTL_SECS,
                // The lock companion AND the seal's salt, written at insert so
                // the partial UNIQUE index constrains a fresh session rather
                // than only post-boot.
                source_hash.as_slice(),
                scope_json,
                source_sealed,
                date_from,
                i64::from(skip_dedup),
            ],
        )
        .context("insert import_session")?;
        tx.commit().context("commit start import_session")?;
        Ok(CreateImportSessionOutcome::Created)
    }

    /// Delete one of the caller's terminal sessions (`errored` / `completed` /
    /// `cancelled`). One conditional `DELETE`, so a row that turned live again
    /// between a read and this call can never be removed; a zero-row result is
    /// then told apart by a read.
    pub async fn discard_import_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
    ) -> Result<DiscardImportSessionOutcome> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM import_sessions
                 WHERE actor_id = ?1 AND session_id = ?2
                   AND state IN ('errored','completed','cancelled')",
                rusqlite::params![&actor[..], session_id],
            )
            .context("discard import_session")?;
        if deleted > 0 {
            return Ok(DiscardImportSessionOutcome::Deleted);
        }
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM import_sessions WHERE actor_id = ?1 AND session_id = ?2",
                rusqlite::params![&actor[..], session_id],
                |row| row.get(0),
            )
            .optional()
            .context("reread import_session after discard")?;
        Ok(match state {
            None => DiscardImportSessionOutcome::Missing,
            Some(state) => DiscardImportSessionOutcome::Live(state),
        })
    }

    /// Fetch one of the caller's sessions.
    pub async fn get_import_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
    ) -> Result<Option<ImportSessionRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {SELECT_COLS} FROM import_sessions
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![&actor[..], session_id],
            row_from_sql,
        )
        .optional()
        .context("get import_session")
    }

    /// List the caller's non-expired sessions, newest-started first
    /// (resume protocol step 1). Sweeps expired rows first.
    pub async fn list_import_sessions(&self, actor: &[u8; 32]) -> Result<Vec<ImportSessionRow>> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM import_sessions WHERE actor_id = ?1 AND expires_at < ?2",
            rusqlite::params![&actor[..], now],
        )
        .context("gc import_sessions")?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SELECT_COLS} FROM import_sessions
                 WHERE actor_id = ?1 ORDER BY started_at DESC"
            ))
            .context("prepare list import_sessions")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], row_from_sql)
            .context("query list import_sessions")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list import_sessions")?);
        }
        Ok(out)
    }

    /// Conditional state transition: moves the session to `new_state` iff its
    /// current state is in `allowed_from`, recording `error_reason` when
    /// given. Returns the post-transition row, or `None` when the session
    /// doesn't exist / isn't the caller's / is in a state the transition
    /// doesn't apply to (the handler maps `None` to a typed error).
    pub async fn transition_import_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        new_state: ImportState,
        allowed_from: &[ImportState],
        error_reason: Option<&str>,
    ) -> Result<Option<ImportSessionRow>> {
        let now = super::now_epoch_secs();
        let placeholders = allowed_from
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 6))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE import_sessions
             SET state = ?3, last_progress_at = ?4, expires_at = ?4 + {IMPORT_SESSION_TTL_SECS},
                 error_reason = COALESCE(?5, error_reason)
             WHERE actor_id = ?1 AND session_id = ?2 AND state IN ({placeholders})"
        );
        let conn = self.conn.lock().await;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(actor.to_vec()),
            Box::new(session_id.to_string()),
            Box::new(new_state.as_str().to_string()),
            Box::new(now),
            Box::new(error_reason.map(str::to_string)),
        ];
        for s in allowed_from {
            params.push(Box::new(s.as_str().to_string()));
        }
        let changed = conn
            .execute(
                &sql,
                rusqlite::params_from_iter(params.iter().map(|p| &**p)),
            )
            .context("transition import_session")?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            &format!(
                "SELECT {SELECT_COLS} FROM import_sessions
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![&actor[..], session_id],
            row_from_sql,
        )
        .optional()
        .context("reread import_session after transition")
    }

    /// Fold processed messages of one source mailbox into the session: move
    /// the counters, the mailbox's resume cursor and its quota hold
    /// ([`fold_mailbox`]), refresh `last_progress_at`/`expires_at`, and
    /// optionally revise `total_count`. Returns the post-update row (`None` =
    /// no such session for this actor, or one that is no longer `running` /
    /// `paused` — the store's own refusal of a fold into a finished session).
    pub async fn record_import_progress(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        mailbox: &str,
        uid_validity: u32,
        folds: &[(u32, ImportFold)],
        revised_total_count: Option<u64>,
    ) -> Result<Option<ImportSessionRow>> {
        let revised_total_count = revised_total_count
            .map(|v| import_counter_to_sql("total_count", v))
            .transpose()?;
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let existing = conn
            .query_row(
                "SELECT cursors, quota_holds FROM import_sessions
                 WHERE actor_id = ?1 AND session_id = ?2",
                rusqlite::params![&actor[..], session_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .context("read import_session cursors")?;
        let Some((cursors_json, holds_json)) = existing else {
            return Ok(None);
        };
        // Same verified fold as `row_from_sql` above — idempotent via
        // `actor_message_dedup`, so a corrupt blob costs redundant re-scan
        // work, not duplicate messages. A corrupt hold blob costs at most a
        // counter re-counted on the re-walk it would have absorbed.
        let mut cursors: BTreeMap<String, (u32, u32)> =
            serde_json::from_str(&cursors_json).unwrap_or_default();
        let mut holds: BTreeMap<String, QuotaHold> =
            serde_json::from_str(&holds_json).unwrap_or_default();
        let (cursor, hold, delta) = fold_mailbox(
            cursors.get(mailbox).copied(),
            holds.remove(mailbox),
            uid_validity,
            folds,
        );
        if let Some(c) = cursor {
            cursors.insert(mailbox.to_string(), c);
        }
        if let Some(h) = hold {
            holds.insert(mailbox.to_string(), h);
        }
        let cursors_json = serde_json::to_string(&cursors).context("encode cursors")?;
        let holds_json = serde_json::to_string(&holds).context("encode quota holds")?;
        // The state guard rides inside the UPDATE (the export twin's shape): a
        // cancel or complete landing between the handler's read and this fold
        // matches no row, so a finished session's window, counters, total and
        // cursors never move. `None` is the same answer as for a missing row;
        // the handler's re-read tells them apart.
        let changed = conn
            .execute(
                &format!(
                    "UPDATE import_sessions
                 SET imported_count = {imported},
                     skipped_count = {skipped},
                     errored_count = {errored},
                     cursors = ?6,
                     quota_holds = ?9,
                     total_count = COALESCE(?7, total_count),
                     last_progress_at = ?8,
                     expires_at = ?8 + {IMPORT_SESSION_TTL_SECS}
                 WHERE actor_id = ?1 AND session_id = ?2
                   AND state IN ('running','paused')",
                    imported = saturating_add_floor_sql("imported_count", "?3"),
                    skipped = saturating_add_floor_sql("skipped_count", "?4"),
                    errored = saturating_add_floor_sql("errored_count", "?5"),
                ),
                rusqlite::params![
                    &actor[..],
                    session_id,
                    delta.imported,
                    delta.skipped,
                    delta.errored,
                    cursors_json,
                    revised_total_count,
                    now,
                    holds_json,
                ],
            )
            .context("update import_session progress")?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            &format!(
                "SELECT {SELECT_COLS} FROM import_sessions
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![&actor[..], session_id],
            row_from_sql,
        )
        .optional()
        .context("reread import_session after progress")
    }

    /// Does the actor hold a row for this key, in any mailbox? The raw index
    /// lookup — it says nothing about whether an import should skip; that is
    /// [`Self::dedup_hit`].
    pub async fn has_dedup_key(&self, actor: &[u8; 32], dedup_key: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let hit: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM actor_message_dedup WHERE actor_id = ?1 AND dedup_key = ?2",
                rusqlite::params![&actor[..], dedup_key],
                |row| row.get(0),
            )
            .optional()
            .context("dedup lookup")?;
        Ok(hit.is_some())
    }

    /// The import skip decision (`mailbox-migration.md` § Dedup scope, § The
    /// envelope key confirms a Message-ID hit): does the actor already hold a
    /// row for this key, in any mailbox, whose envelope key agrees with the
    /// candidate's? A Message-ID alone is sender-chosen, so a row a stranger's
    /// delivery planted with other content does not make the real message
    /// skip — decided by the shared [`fauna_mail::envelope_keys_agree`].
    pub async fn dedup_hit(
        &self,
        actor: &[u8; 32],
        dedup_key: &str,
        candidate_envelope_key: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let stored: Option<String> = conn
            .query_row(
                "SELECT envelope_key FROM actor_message_dedup
                 WHERE actor_id = ?1 AND dedup_key = ?2",
                rusqlite::params![&actor[..], dedup_key],
                |row| row.get(0),
            )
            .optional()
            .context("dedup lookup")?;
        Ok(match stored {
            None => false,
            Some(stored) => fauna_mail::envelope_keys_agree(&stored, candidate_envelope_key),
        })
    }

    /// Record a stored message's dedup keys. First writer wins (`INSERT OR
    /// IGNORE`) — with "Import duplicates anyway", or two messages sharing a
    /// Message-ID whose envelope keys disagree, the same key can be stored
    /// twice; the index keeps pointing at the first copy and its envelope key.
    pub async fn insert_dedup_key(
        &self,
        actor: &[u8; 32],
        dedup_key: &str,
        envelope_key: &str,
        message_uri: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO actor_message_dedup
                 (actor_id, dedup_key, envelope_key, message_uri)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![&actor[..], dedup_key, envelope_key, message_uri],
        )
        .context("insert dedup key")?;
        Ok(())
    }

    /// The envelope key recorded for a row: `None` = no row. Test
    /// observability for the producers' write paths.
    #[cfg(test)]
    pub(crate) async fn dedup_envelope_key(
        &self,
        actor: &[u8; 32],
        dedup_key: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT envelope_key FROM actor_message_dedup
             WHERE actor_id = ?1 AND dedup_key = ?2",
            rusqlite::params![&actor[..], dedup_key],
            |row| row.get(0),
        )
        .optional()
        .context("dedup envelope-key lookup")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    const ACTOR: [u8; 32] = [0xA1u8; 32];

    /// A seal minted the way a real client mints one — through
    /// `label_custody::seal_import_source`, so these tests pin the actual
    /// envelope (tag `import_sessions.source_descriptor`, salt
    /// `import_source_hash`) rather than a stand-in blob. A hand-rolled
    /// `x'33'` would keep passing if the salt or tag ever drifted, which is
    /// how the two pre-existing pins missed that nothing wrote this column.
    fn client_seal(source: &str) -> Vec<u8> {
        let key = fauna_core::crypto::BackupKey::derive(&[0x5eu8; 32]);
        let root = fauna_core::path_crypto::LabelRoot::owner_of(&key);
        fauna_core::label_custody::seal_import_source(&root, source).unwrap()
    }

    async fn open_with_session(id: &str, source: &str) -> CacheDb {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.create_import_session(
                &ACTOR,
                id,
                source,
                100,
                &["INBOX".to_string(), "Sent".to_string()],
                "2023-11-14",
                false,
                Some(&client_seal(source)),
            )
            .await
            .unwrap(),
            CreateImportSessionOutcome::Created
        );
        db
    }

    #[tokio::test]
    async fn scope_is_recorded_at_create_and_survives_get_and_list() {
        // § Resume protocol: the scope must be readable BEFORE any mailbox
        // has been touched by `record_import_progress` — a fresh session has
        // no cursors yet, but a resumed client still needs to know which
        // mailboxes to `EXAMINE`.
        let db = open_with_session("s1", "gmail:imap.gmail.com:alice").await;
        let row = db.get_import_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.scope, vec!["INBOX".to_string(), "Sent".to_string()]);
        assert!(row.cursors.is_empty(), "no mailbox processed yet");

        let listed = db.list_import_sessions(&ACTOR).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].scope, row.scope);
    }

    /// The since date is recorded beside the scope and read back by both
    /// query paths — the row is the only durable record of the range a
    /// resumed walk must re-apply (`mailbox-migration.md` § Wizard steps
    /// step 3).
    #[tokio::test]
    async fn the_since_date_is_recorded_and_read_back_by_get_and_list() {
        let db = open_with_session("s1", "gmail:imap.gmail.com:alice").await;
        let row = db.get_import_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.date_from, "2023-11-14");
        let listed = db.list_import_sessions(&ACTOR).await.unwrap();
        assert_eq!(listed[0].date_from, "2023-11-14");
    }

    /// The duplicates choice is recorded beside the since date and read back
    /// by both query paths; a session started without it kept dedup on
    /// (`mailbox-migration.md` § Opt-out per session, § Resume protocol).
    #[tokio::test]
    async fn the_duplicates_choice_is_recorded_and_read_back_by_get_and_list() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_import_session(&ACTOR, "on", "src-a", 5, &[], "", true, None)
            .await
            .unwrap();
        db.create_import_session(&ACTOR, "off", "src-b", 5, &[], "", false, None)
            .await
            .unwrap();
        assert!(
            db.get_import_session(&ACTOR, "on")
                .await
                .unwrap()
                .unwrap()
                .skip_dedup
        );
        assert!(
            !db.get_import_session(&ACTOR, "off")
                .await
                .unwrap()
                .unwrap()
                .skip_dedup
        );
        let listed = db.list_import_sessions(&ACTOR).await.unwrap();
        let on = listed.iter().find(|r| r.session_id == "on").unwrap();
        let off = listed.iter().find(|r| r.session_id == "off").unwrap();
        assert!(on.skip_dedup && !off.skip_dedup);
    }

    /// An unbounded import reads back as the empty string, never an error.
    #[tokio::test]
    async fn an_unbounded_import_reads_back_an_empty_since_date() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_import_session(&ACTOR, "s1", "src", 5, &[], "", false, None)
            .await
            .unwrap();
        let row = db.get_import_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert!(row.date_from.is_empty());
    }

    #[tokio::test]
    async fn an_old_clients_empty_scope_reads_back_empty_not_an_error() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_import_session(&ACTOR, "s1", "src", 5, &[], "", false, None)
            .await
            .unwrap();
        let row = db.get_import_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert!(row.scope.is_empty());
    }

    /// **The production-path assertion** — the one the pre-existing coverage
    /// lacked, and the gap that let `source_sealed` ship with no writer at all.
    /// `conformance_at_rest_byte_scan.rs` and `db/mod.rs`'s scrub pin both
    /// INSERT the sealed column themselves, so both read green while
    /// production wrote nothing there: they observe the schema, never the
    /// writer. This one drives the real `create_import_session` and asserts on
    /// what actually rests afterwards.
    ///
    /// Covers both halves `encryption-at-rest.md:15` (b) claims for this
    /// surface — the seal, and the lock keyed off `source_hash` rather than
    /// the plaintext the boot scrub is about to blank.
    #[tokio::test]
    async fn a_created_session_rests_sealed_and_keeps_its_lock_across_the_scrub() {
        const SOURCE: &str = "gmail:imap.gmail.com:alice";
        let db = open_with_session("s1", SOURCE).await;

        // (a) The seal rests. Without it the scrub cannot fire, and the
        // descriptor rests plaintext for the row's whole life.
        {
            let conn = db.conn().await;
            let sealed: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT source_sealed FROM import_sessions WHERE session_id = 's1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                sealed.is_some_and(|b| !b.is_empty()),
                "create_import_session must stamp source_sealed — the descriptor is the \
                 user's external mailbox identity and rests for the row's 30-day life"
            );
        }

        // (b) The boot scrub then MIGRATES the label rather than destroying it,
        // and the multi-device lock keeps working afterwards — it must key off
        // `source_hash`, which survives, not the plaintext, which does not.
        {
            let conn = db.conn().await;
            // The production funnel itself (`SCRUB_PLANES` + its runner, run by
            // `run_migrations` on every boot), not a divergent
            // copy — the `test-hooks` wrapper on `CacheDb` wraps this same
            // function, and calling it directly keeps this pin in the DEFAULT
            // feature set, where the writer regression it guards would show.
            crate::db::migrations::run_scrub_plaintext(&conn).unwrap();
        }
        {
            let conn = db.conn().await;
            let plain: String = conn
                .query_row(
                    "SELECT source_descriptor FROM import_sessions WHERE session_id = 's1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                plain, "",
                "the graduated arm scrubs the plaintext once sealed"
            );
        }
        assert_eq!(
            db.create_import_session(
                &ACTOR,
                "s2",
                SOURCE,
                5,
                &[],
                "",
                false,
                Some(&client_seal(SOURCE))
            )
            .await
            .unwrap(),
            CreateImportSessionOutcome::SourceLocked,
            "the per-source lock must survive the scrub — reading it off the plaintext \
             silently degrades the typed SourceLocked into an opaque UNIQUE-index error"
        );
    }

    #[tokio::test]
    async fn create_then_get_and_source_lock() {
        let db = open_with_session("s1", "gmail:imap.gmail.com:alice").await;
        let row = db.get_import_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.state, "running");
        assert_eq!(row.total_count, 100);
        assert_eq!(
            row.expires_at,
            row.last_progress_at + IMPORT_SESSION_TTL_SECS
        );

        // Same source while running → locked; different source → fine.
        assert_eq!(
            db.create_import_session(
                &ACTOR,
                "s2",
                "gmail:imap.gmail.com:alice",
                5,
                &[],
                "",
                false,
                Some(&client_seal("gmail:imap.gmail.com:alice"))
            )
            .await
            .unwrap(),
            CreateImportSessionOutcome::SourceLocked
        );
        assert_eq!(
            db.create_import_session(
                &ACTOR,
                "s3",
                "icloud:imap.mail.me.com:alice",
                5,
                &[],
                "",
                false,
                Some(&client_seal("icloud:imap.mail.me.com:alice"))
            )
            .await
            .unwrap(),
            CreateImportSessionOutcome::Created
        );
        // Cancelled sessions release the lock.
        db.transition_import_session(
            &ACTOR,
            "s1",
            ImportState::Cancelled,
            &[ImportState::Running, ImportState::Paused],
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            db.create_import_session(
                &ACTOR,
                "s4",
                "gmail:imap.gmail.com:alice",
                5,
                &[],
                "",
                false,
                Some(&client_seal("gmail:imap.gmail.com:alice"))
            )
            .await
            .unwrap(),
            CreateImportSessionOutcome::Created
        );
    }

    #[tokio::test]
    async fn sessions_are_actor_scoped() {
        let db = open_with_session("s1", "src").await;
        let other = [0xB2u8; 32];
        assert!(db.get_import_session(&other, "s1").await.unwrap().is_none());
        assert!(db.list_import_sessions(&other).await.unwrap().is_empty());
        assert!(
            db.transition_import_session(
                &other,
                "s1",
                ImportState::Paused,
                &[ImportState::Running],
                None
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    async fn state_machine_transitions() {
        let db = open_with_session("s1", "src").await;
        // running → paused → running → completed.
        let row = db
            .transition_import_session(
                &ACTOR,
                "s1",
                ImportState::Paused,
                &[ImportState::Running],
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "paused");
        // Pause again: not in allowed_from → None.
        assert!(
            db.transition_import_session(
                &ACTOR,
                "s1",
                ImportState::Paused,
                &[ImportState::Running],
                None
            )
            .await
            .unwrap()
            .is_none()
        );
        db.transition_import_session(
            &ACTOR,
            "s1",
            ImportState::Running,
            &[ImportState::Paused],
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let row = db
            .transition_import_session(
                &ACTOR,
                "s1",
                ImportState::Completed,
                &[ImportState::Running],
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "completed");
        // Terminal: no way back.
        assert!(
            db.transition_import_session(
                &ACTOR,
                "s1",
                ImportState::Running,
                &[ImportState::Paused],
                None
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    async fn fail_records_reason() {
        let db = open_with_session("s1", "src").await;
        let row = db
            .transition_import_session(
                &ACTOR,
                "s1",
                ImportState::Errored,
                &[ImportState::Running, ImportState::Paused],
                Some("auth_failed"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "errored");
        assert_eq!(row.error_reason, "auth_failed");
    }

    async fn fail(db: &CacheDb, id: &str) {
        db.transition_import_session(
            &ACTOR,
            id,
            ImportState::Errored,
            &[ImportState::Running, ImportState::Paused],
            Some("auth_failed"),
        )
        .await
        .unwrap()
        .unwrap();
    }

    async fn create(db: &CacheDb, id: &str, source: &str) {
        assert_eq!(
            db.create_import_session(&ACTOR, id, source, 1, &[], "", false, None)
                .await
                .unwrap(),
            CreateImportSessionOutcome::Created
        );
    }

    /// § A failed import is a record …: a new start is the user's answer to a
    /// failure, so the caller's `errored` rows go in the same write as the
    /// insert — and only those: a finished or cancelled row, and another
    /// actor's errored row, stay.
    #[tokio::test]
    async fn a_start_retires_the_callers_errored_rows_and_nothing_else() {
        let db = open_with_session("failed", "src").await;
        fail(&db, "failed").await;
        create(&db, "done", "src2").await;
        db.transition_import_session(
            &ACTOR,
            "done",
            ImportState::Completed,
            &[ImportState::Running],
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let other = [0xB2u8; 32];
        db.create_import_session(&other, "theirs", "src", 1, &[], "", false, None)
            .await
            .unwrap();
        db.transition_import_session(
            &other,
            "theirs",
            ImportState::Errored,
            &[ImportState::Running],
            Some("x"),
        )
        .await
        .unwrap()
        .unwrap();

        create(&db, "next", "src3").await;

        assert!(
            db.get_import_session(&ACTOR, "failed")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_import_session(&ACTOR, "done")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_import_session(&ACTOR, "next")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_import_session(&other, "theirs")
                .await
                .unwrap()
                .is_some()
        );
    }

    /// A refused start (the source lock) is not an answer to anything: the
    /// failure record stays.
    #[tokio::test]
    async fn a_locked_start_keeps_the_errored_record() {
        let db = open_with_session("live", "b").await;
        create(&db, "failed", "a").await;
        fail(&db, "failed").await;
        assert_eq!(
            db.create_import_session(&ACTOR, "again", "b", 1, &[], "", false, None)
                .await
                .unwrap(),
            CreateImportSessionOutcome::SourceLocked
        );
        assert!(
            db.get_import_session(&ACTOR, "failed")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn discard_deletes_a_terminal_row_and_refuses_a_live_one() {
        let db = open_with_session("s1", "src").await;
        assert_eq!(
            db.discard_import_session(&ACTOR, "s1").await.unwrap(),
            DiscardImportSessionOutcome::Live("running".into())
        );
        assert!(db.get_import_session(&ACTOR, "s1").await.unwrap().is_some());
        fail(&db, "s1").await;
        // Another actor cannot reach it: to them it is missing.
        assert_eq!(
            db.discard_import_session(&[0xB2u8; 32], "s1")
                .await
                .unwrap(),
            DiscardImportSessionOutcome::Missing
        );
        assert_eq!(
            db.discard_import_session(&ACTOR, "s1").await.unwrap(),
            DiscardImportSessionOutcome::Deleted
        );
        assert!(db.get_import_session(&ACTOR, "s1").await.unwrap().is_none());
        assert_eq!(
            db.discard_import_session(&ACTOR, "s1").await.unwrap(),
            DiscardImportSessionOutcome::Missing
        );
    }

    #[tokio::test]
    async fn progress_updates_counters_cursor_and_total() {
        use ImportFold::*;
        let db = open_with_session("s1", "src").await;
        let row = db
            .record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(44, Imported)], None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.imported_count, 1);
        assert_eq!(row.cursors.get("INBOX"), Some(&(44, 7)));

        let row = db
            .record_import_progress(
                &ACTOR,
                "s1",
                "INBOX",
                7,
                &[(45, Skipped), (40, Errored), (41, Errored)],
                Some(250),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.imported_count, 1);
        assert_eq!(row.skipped_count, 1);
        assert_eq!(row.errored_count, 2);
        assert_eq!(row.total_count, 250);
        assert_eq!(row.cursors.get("INBOX"), Some(&(45, 7)));
        // Second mailbox gets its own cursor.
        let row = db
            .record_import_progress(&ACTOR, "s1", "Sent", 9, &[(3, Imported)], None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.cursors.len(), 2);
        assert_eq!(row.cursors.get("Sent"), Some(&(3, 9)));
    }

    /// A finished session takes no fold: the store refuses it inside the
    /// UPDATE (the export twin's shape), so a cancel or complete landing
    /// between the handler's state read and the fold cannot re-arm the 30-day
    /// window or move a counter of a finished row.
    #[tokio::test]
    async fn a_finished_session_takes_no_progress_fold() {
        use ImportFold::*;
        let db = open_with_session("s1", "src").await;
        db.transition_import_session(
            &ACTOR,
            "s1",
            ImportState::Completed,
            &[ImportState::Running],
            None,
        )
        .await
        .unwrap()
        .unwrap();
        {
            let conn = db.conn.lock().await;
            conn.execute("UPDATE import_sessions SET expires_at = 1", [])
                .unwrap();
        }
        let folded = db
            .record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(1, Imported)], Some(99))
            .await
            .unwrap();
        assert!(folded.is_none(), "a finished session refuses the fold");
        let row = db.get_import_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.expires_at, 1, "the window is not re-armed");
        assert_eq!(row.imported_count, 0, "the counters do not move");
        assert_eq!(row.total_count, 100, "the total is not revised");
        assert!(row.cursors.is_empty(), "the cursor does not move");
    }

    /// A client counter above `i64::MAX` is refused, never stored as a
    /// negative number that reads back as an enormous `u64`.
    #[tokio::test]
    async fn an_out_of_range_total_is_refused_not_stored() {
        use ImportFold::*;
        let db = open_with_session("s1", "src").await;
        assert!(
            db.record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(1, Imported)], Some(u64::MAX))
                .await
                .is_err()
        );
        let row = db.get_import_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.total_count, 100);
        assert_eq!(row.imported_count, 0, "a refused fold moves nothing");

        let created = db
            .create_import_session(&ACTOR, "s2", "other", u64::MAX, &[], "", false, None)
            .await;
        assert!(created.is_err(), "create refuses it too");
        assert!(db.get_import_session(&ACTOR, "s2").await.unwrap().is_none());
    }

    /// The sums saturate at `i64::MAX` instead of overflowing into a REAL the
    /// row decoder cannot read, and still floor at zero for a negative delta.
    #[tokio::test]
    async fn counter_sums_saturate_and_floor() {
        use ImportFold::*;
        let db = open_with_session("s1", "src").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE import_sessions SET imported_count = 9223372036854775807",
                [],
            )
            .unwrap();
        }
        let row = db
            .record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(1, Imported)], None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.imported_count, i64::MAX as u64);
    }

    /// The row's cursor stays short of the first message a full mailbox
    /// turned away, through the rest of its batch and through a too-large
    /// report above it (`mailbox-migration.md` § Failure handling), and the
    /// hold rests on the row for the next process to honour.
    #[tokio::test]
    async fn a_quota_refusal_holds_the_rows_cursor_on_both_paths() {
        use ImportFold::*;
        let db = open_with_session("s1", "src").await;
        for (uid, fold) in [(10, Imported), (11, QuotaRefused), (12, Imported)] {
            db.record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(uid, fold)], None)
                .await
                .unwrap();
        }
        // `record_import_outcomes`' path, reporting a too-large message above
        // the refusal after the window's sends.
        let row = db
            .record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(13, Skipped)], None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.cursors.get("INBOX"), Some(&(10, 7)));
        assert_eq!(
            (row.imported_count, row.skipped_count, row.errored_count),
            (2, 2, 0)
        );

        // The resume after room was freed re-walks 11..=13: the turned-away
        // message lands and moves out of `skipped`, the rest count nothing
        // again, and the cursor moves on past everything processed.
        let row = db
            .record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(11, Imported)], None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.cursors.get("INBOX"), Some(&(13, 7)));
        db.record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(12, Skipped)], None)
            .await
            .unwrap();
        let row = db
            .record_import_progress(
                &ACTOR,
                "s1",
                "INBOX",
                7,
                &[(13, Skipped), (14, Imported)],
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.cursors.get("INBOX"), Some(&(14, 7)));
        assert_eq!(
            (row.imported_count, row.skipped_count, row.errored_count),
            (4, 1, 0)
        );
    }

    /// `ImportFold`'s rule-3 answer is `skip` (`enum_ledger.txt`): a
    /// `quota_holds` blob this build cannot decode — a fold kind a newer nest
    /// wrote — folds to no holds, and the progress write still lands rather
    /// than erroring the import.
    #[tokio::test]
    async fn an_undecodable_quota_hold_folds_to_none_and_the_write_lands() {
        use ImportFold::*;
        let db = open_with_session("s1", "src").await;
        db.record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(10, Imported)], None)
            .await
            .unwrap();
        {
            let conn = db.conn().await;
            conn.execute(
                "UPDATE import_sessions SET quota_holds = ?1 WHERE session_id = 's1'",
                [r#"{"INBOX":{"uid_validity":7,"high":11,"seen":{"11":"a_future_fold"}}}"#],
            )
            .unwrap();
        }
        let row = db
            .record_import_progress(&ACTOR, "s1", "INBOX", 7, &[(11, Imported)], None)
            .await
            .unwrap()
            .expect("the fold lands despite the undecodable hold");
        assert_eq!(row.cursors.get("INBOX"), Some(&(11, 7)));
        assert_eq!(row.imported_count, 2);
    }

    #[test]
    fn fold_holds_at_the_lowest_refusal_and_lifts_when_it_resolves() {
        use ImportFold::*;
        let (cursor, hold, delta) = fold_mailbox(
            Some((9, 7)),
            None,
            7,
            &[(10, QuotaRefused), (11, Imported), (12, QuotaRefused)],
        );
        assert_eq!(cursor, Some((9, 7)));
        assert_eq!(
            delta,
            FoldDelta {
                imported: 1,
                skipped: 2,
                errored: 0
            }
        );
        let hold = hold.expect("held");
        assert_eq!(hold.high, 12);

        // The first refusal resolves; the second still holds.
        let (cursor, hold, delta) = fold_mailbox(cursor, Some(hold), 7, &[(10, Imported)]);
        assert_eq!(cursor, Some((11, 7)));
        assert_eq!(
            delta,
            FoldDelta {
                imported: 1,
                skipped: -1,
                errored: 0
            }
        );

        // Refused again: nothing counts twice, the hold stays.
        let (cursor, hold, delta) = fold_mailbox(cursor, hold, 7, &[(12, QuotaRefused)]);
        assert_eq!(cursor, Some((11, 7)));
        assert_eq!(delta, FoldDelta::default());

        // "Skip remaining" records it as skipped for good: no counter moves
        // (it was already a skip), and the cursor moves on.
        let (cursor, hold, delta) = fold_mailbox(cursor, hold, 7, &[(12, Skipped)]);
        assert_eq!(cursor, Some((12, 7)));
        assert_eq!(delta, FoldDelta::default());
        assert!(hold.is_some(), "kept until the walk passes it");

        // The walk passes the hold's last UID: it is dropped, and new UIDs
        // count as ever.
        let (cursor, hold, delta) = fold_mailbox(cursor, hold, 7, &[(13, Imported)]);
        assert_eq!(cursor, Some((13, 7)));
        assert!(hold.is_none());
        assert_eq!(
            delta,
            FoldDelta {
                imported: 1,
                skipped: 0,
                errored: 0
            }
        );
    }

    #[test]
    fn fold_never_walks_back_and_a_new_uidvalidity_replaces_the_hold() {
        use ImportFold::*;
        // A lower UID reported late never walks the cursor back.
        let (cursor, hold, _) = fold_mailbox(Some((50, 7)), None, 7, &[(3, Skipped)]);
        assert_eq!(cursor, Some((50, 7)));
        assert!(hold.is_none());

        let (cursor, hold, _) = fold_mailbox(cursor, None, 7, &[(51, QuotaRefused)]);
        assert_eq!(cursor, Some((50, 7)));
        // The source mailbox was rebuilt: its old UIDs mean nothing.
        let (cursor, hold, _) = fold_mailbox(cursor, hold, 8, &[(2, Imported)]);
        assert_eq!(cursor, Some((2, 8)));
        assert!(hold.is_none());
    }

    #[tokio::test]
    async fn dedup_insert_and_lookup_are_actor_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        let other = [0xB2u8; 32];
        assert!(!db.has_dedup_key(&ACTOR, "k1").await.unwrap());
        db.insert_dedup_key(&ACTOR, "k1", "env:v1:e1", "aa11")
            .await
            .unwrap();
        assert!(db.has_dedup_key(&ACTOR, "k1").await.unwrap());
        // No cross-actor dedup (§ Don't do these).
        assert!(!db.has_dedup_key(&other, "k1").await.unwrap());
        assert!(!db.dedup_hit(&other, "k1", "env:v1:e1").await.unwrap());
        // First writer wins; re-insert is a no-op, not an error — and it never
        // overwrites the first writer's envelope key.
        db.insert_dedup_key(&ACTOR, "k1", "env:v1:e2", "bb22")
            .await
            .unwrap();
        assert!(db.has_dedup_key(&ACTOR, "k1").await.unwrap());
        assert_eq!(
            db.dedup_envelope_key(&ACTOR, "k1").await.unwrap(),
            Some("env:v1:e1".to_string())
        );
    }

    /// § The envelope key confirms a Message-ID hit: the skip decision is the
    /// key lookup AND the shared agree rule.
    #[tokio::test]
    async fn dedup_hit_applies_the_envelope_agree_rule() {
        let db = CacheDb::open_in_memory().unwrap();
        // No row → never a hit.
        assert!(!db.dedup_hit(&ACTOR, "k", "env:v1:a").await.unwrap());

        db.insert_dedup_key(&ACTOR, "k", "env:v1:a", "u1")
            .await
            .unwrap();
        assert!(db.dedup_hit(&ACTOR, "k", "env:v1:a").await.unwrap());
        assert!(!db.dedup_hit(&ACTOR, "k", "env:v1:b").await.unwrap());
    }

    #[tokio::test]
    async fn expired_sessions_are_swept_and_release_lock() {
        let db = open_with_session("s1", "src").await;
        // Force-expire the row (simulate 30 days idle).
        {
            let conn = db.conn.lock().await;
            conn.execute("UPDATE import_sessions SET expires_at = 1", [])
                .unwrap();
        }
        // list sweeps it...
        assert!(db.list_import_sessions(&ACTOR).await.unwrap().is_empty());
        // ...and a new same-source session starts cleanly.
        assert_eq!(
            db.create_import_session(
                &ACTOR,
                "s2",
                "src",
                5,
                &[],
                "",
                false,
                Some(&client_seal("src"))
            )
            .await
            .unwrap(),
            CreateImportSessionOutcome::Created
        );
    }
}
