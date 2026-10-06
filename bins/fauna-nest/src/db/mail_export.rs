//! Mailbox-export sessions (`docs/goal/behavior/mail-export.md` § Session row
//! model, § Quota composition, § Expiry).
//!
//! The export twin of [`super::mail_import`], deliberately built to the same
//! shape: § Architectural rules requires `export_sessions` to carry "the same
//! column shape as `import_sessions`, the same state machine, the same 30-day
//! GC, the same push-event naming pattern", so a future feature unifying the
//! two is forecast-compatible.
//!
//! Every method is **actor-scoped**: the caller passes the authenticated actor
//! and every query filters by it, so one actor can never read or mutate
//! another's export sessions. That is § Cross-actor isolation's floor restated
//! at the storage layer — the RPC layer's User-class caller-scoping enforces it
//! first, and nothing here takes a target-actor parameter that could be used to
//! reach past it.
//!
//! Sessions are ephemeral bookkeeping. A row expires 30 days after its last
//! progress (`expires_at`). From that instant the per-actor entry points stop
//! seeing it — it is out of the listing, the concurrency cap and the footprint
//! — but none of them deletes it: a row is the only record of where its blob
//! is, so the one thing that removes an expired row is the nest's periodic
//! expiry tick (`mail_export_blobs::run_export_expiry_tick`), which unlinks
//! the file first and deletes the row only after (§ Reclaim rule 2).
//!
//! # What this module deliberately does NOT hold
//!
//! The per-session key. `blob_decryption_key_wrapped_for_actor` is the
//! **client-minted, client-wrapped** blob (§ Key material): the nest stores it
//! verbatim so any of the user's clients can fetch it and unwrap under the
//! actor key, and never opens it. There is no code path here that needs the
//! unwrapped key, and § Don't do these names a nest-side unwrap appearing in
//! any implementation as a design violation rather than an optimization.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::blob_col_to_array;
use super::mail_policy::ExportCeilings;

/// 30 days — § Expiry, the same window the import twin uses.
pub const EXPORT_SESSION_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// The states § Session row model's machine allows.
///
/// Every write that sets a state takes this type, never a string: the table's
/// `CHECK` would refuse any other spelling, but only at runtime and only as an
/// opaque error, so a mistyped state is made unrepresentable at the call site
/// instead. Rows read back carry the string form ([`ExportSessionRow::state`]),
/// compared against the `EXPORT_STATE_*` spellings derived from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportState {
    Running,
    Paused,
    Errored,
    Completed,
    Cancelled,
}

impl ExportState {
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

pub const EXPORT_STATE_RUNNING: &str = ExportState::Running.as_str();
pub const EXPORT_STATE_PAUSED: &str = ExportState::Paused.as_str();
pub const EXPORT_STATE_ERRORED: &str = ExportState::Errored.as_str();
pub const EXPORT_STATE_COMPLETED: &str = ExportState::Completed.as_str();
pub const EXPORT_STATE_CANCELLED: &str = ExportState::Cancelled.as_str();

/// The two non-terminal states that count against the concurrency cap and that
/// a pause/resume/cancel may act on.
pub const IN_FLIGHT_STATES: [ExportState; 2] = [ExportState::Running, ExportState::Paused];

/// The bytes `actor` holds on nest disk across its live sessions — the sum
/// [`ExportCeilings::held_bytes`] bounds. Parameters: `?1` actor, `?2` now,
/// `?3` the session being written, counted whatever its expiry says (an
/// append is about to re-arm it; a create has not inserted it yet).
///
/// The two **disposed** states are excluded, because reaching either unlinks
/// the blob while the row — and its `blob_bytes` — stay for the wizard to
/// list: `cancelled` (§ Wire shapes: "abort + unlink blob") and `errored`
/// (`fail_export_session` disposes of a partial blob that has no terminator
/// and can never be opened). Counting either would ration a user against disk
/// nobody holds. An expired session is excluded because its owner can no
/// longer see it and the expiry tick is already due to reclaim it; counting it
/// would refuse a user over an export their own listing no longer shows.
/// `completed` is what counts: its file rests until discard or expiry, and a
/// finished export resting 30 days is exactly what the concurrency cap never
/// saw.
const HELD_BYTES_SQL: &str = "SELECT COALESCE(SUM(blob_bytes), 0) FROM export_sessions
     WHERE actor_id = ?1 AND state NOT IN ('cancelled','errored')
       AND (expires_at >= ?2 OR session_id = ?3)";

/// A client-supplied counter as the `INTEGER` column stores it. `u64` above
/// `i64::MAX` has no exact `i64`, and an `as` cast would store a negative
/// counter that reads back as an enormous `u64`; the handlers refuse such a
/// value as malformed before it reaches here, so this is the floor under them.
pub fn export_counter_to_sql(name: &str, value: u64) -> Result<i64> {
    i64::try_from(value).with_context(|| format!("export {name} {value} exceeds i64::MAX"))
}

/// `col + param`, saturating at `i64::MAX` instead of overflowing. SQLite turns
/// an overflowing integer sum into a REAL, which [`row_from_sql`] cannot decode
/// — so one oversized fold would wedge its owner's listing until the row
/// expired. `param` is non-negative ([`export_counter_to_sql`]), so the
/// subtraction in the guard cannot overflow either.
fn saturating_add_sql(col: &str, param: &str) -> String {
    let max = i64::MAX;
    format!("CASE WHEN {col} > {max} - {param} THEN {max} ELSE {col} + {param} END")
}

/// One `export_sessions` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSessionRow {
    pub session_id: String,
    pub actor_id: [u8; 32],
    /// `mbox` / `maildir` / `eml-zip` — § Session row model's `format`.
    pub format: String,
    /// DAG-CBOR blob recording mailbox selection, date range and the
    /// header-strip flag (§ Session row model). Opaque here: the nest never
    /// interprets it, it hands it back to the client that wrote it.
    pub scope_descriptor: Vec<u8>,
    pub state: String,
    pub started_at: i64,
    pub last_progress_at: i64,
    pub total_count: u64,
    pub exported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    /// Resume cursor — § Session row model's `last_processed_message_id`.
    pub last_processed_message_id: String,
    pub error_reason: String,
    /// Nest-relative path to the on-disk blob
    /// (`exports/<session-id>.zip.zst.sealed` — § Blob shape on disk pins the
    /// suffix; the bytes are the framed per-chunk ciphertext, not a mountable
    /// `.zip.zst`). An internal handle: § Don't do these forbids surfacing it
    /// in any user-facing surface.
    pub blob_path: String,
    /// Finalized size; 0 until completion.
    pub blob_bytes: u64,
    /// The client-minted key wrapped for the actor (§ Key material). Stored
    /// verbatim, never opened here.
    pub blob_decryption_key_wrapped_for_actor: Option<Vec<u8>>,
    pub expires_at: i64,
    /// The next frame index [`super::CacheDb::append_export_blob_bytes`] will
    /// accept (§ Blob shape on disk). Starts at 0 and advances by one per
    /// accepted chunk; a resuming client reads it instead of guessing where
    /// its predecessor stopped.
    pub next_chunk_idx: u64,
    /// Which stream of this export `blob_path` currently holds (§ Resume): 0 at
    /// start, + 1 per [`super::CacheDb::restart_export_session`]. Every driver
    /// call is checked against it, because it is the only thing that tells a
    /// device that was restarted over from the device that restarted.
    pub stream_generation: u64,
}

/// Outcome of [`super::CacheDb::create_export_session`].
///
/// `ConcurrencyCapReached` is § Quota composition's per-user cap surfacing as a
/// typed outcome rather than an opaque error, so the wizard can say "finish or
/// cancel one of your running exports" instead of "something went wrong" — the
/// same reason the import twin types its `SourceLocked`.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateExportSessionOutcome {
    Created,
    ConcurrencyCapReached,
    /// The caller's live sessions already hold [`ExportCeilings::held_bytes`]
    /// (§ Quota composition's per-user footprint), so no chunk of a new export
    /// could land. Refused here rather than at the first upload so the user
    /// learns it before opening a session that would hold a concurrency slot
    /// and write nothing.
    FootprintCapReached {
        held_bytes: u64,
        ceiling: u64,
    },
}

/// Outcome of [`super::CacheDb::append_export_blob_bytes`].
///
/// `BlobOversize` is § Quota composition's per-session disk ceiling. The doc
/// names the wire error `session_blob_oversize`; typing it here keeps the
/// decision at the storage layer that owns the running total, so a handler
/// cannot forget to check.
#[derive(Debug, PartialEq, Eq)]
pub enum AppendExportChunkOutcome {
    Appended {
        blob_bytes: u64,
        next_chunk_idx: u64,
        /// The file this frame goes to — read under the same lock as the
        /// generation check. ⚠ The caller appends to THIS path and never
        /// re-reads the row for it: a restart landing between the reservation
        /// and the append repoints the row at the new generation's file, and a
        /// re-read would put this stale frame at the head of the new stream
        /// (§ Resume — the race a file per generation exists to close).
        blob_path: String,
    },
    /// The caller drives a stream generation that is no longer the session's
    /// (§ Resume): another of the user's devices restarted the export. Checked
    /// before the chunk index, so a superseded driver is never told to "resume
    /// from chunk n" of a stream that is not its own.
    StreamSuperseded {
        current: u64,
    },
    BlobOversize {
        blob_bytes: u64,
        ceiling: u64,
    },
    /// The chunk fits this session's own ceiling but would take the actor's
    /// live sessions past [`ExportCeilings::held_bytes`] — § Quota
    /// composition's per-user footprint. `held_bytes` is the total before this
    /// chunk; nothing was reserved.
    FootprintExceeded {
        held_bytes: u64,
        ceiling: u64,
    },
    /// The caller's `chunk_idx` is not the one this session expects next
    /// (§ Blob shape on disk — the blob is frames in `chunk_idx` order).
    ///
    /// Covers both directions: a replayed chunk (index already consumed) and
    /// a chunk that jumped ahead of one still in flight. Neither may be
    /// appended, because the nest cannot reorder what it cannot parse — and
    /// the expected index is carried back so the client resumes rather than
    /// restarts.
    ChunkOutOfOrder {
        expected: u64,
    },
    NoSuchSession,
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExportSessionRow> {
    let actor: Vec<u8> = row.get(1)?;
    Ok(ExportSessionRow {
        session_id: row.get(0)?,
        actor_id: blob_col_to_array(actor, 1, "actor_id")?,
        format: row.get(2)?,
        scope_descriptor: row.get(3)?,
        state: row.get(4)?,
        started_at: row.get(5)?,
        last_progress_at: row.get(6)?,
        total_count: row.get::<_, i64>(7)? as u64,
        exported_count: row.get::<_, i64>(8)? as u64,
        skipped_count: row.get::<_, i64>(9)? as u64,
        errored_count: row.get::<_, i64>(10)? as u64,
        last_processed_message_id: row.get(11)?,
        error_reason: row.get(12)?,
        blob_path: row.get(13)?,
        blob_bytes: row.get::<_, i64>(14)? as u64,
        blob_decryption_key_wrapped_for_actor: row.get(15)?,
        expires_at: row.get(16)?,
        next_chunk_idx: row.get::<_, i64>(17)? as u64,
        stream_generation: row.get::<_, i64>(18)? as u64,
    })
}

const SELECT_COLS: &str = "session_id, actor_id, format, scope_descriptor, state, started_at, \
     last_progress_at, total_count, exported_count, skipped_count, errored_count, \
     last_processed_message_id, error_reason, blob_path, blob_bytes, \
     blob_decryption_key_wrapped_for_actor, expires_at, next_chunk_idx, stream_generation";

impl crate::db::CacheDb {
    /// Open a fresh `running` session at the wizard's durable commit point
    /// (§ UX shape step 3).
    ///
    /// Returns `ConcurrencyCapReached` when the caller already holds
    /// `ceilings.concurrent` sessions in `running`/`paused`, and
    /// `FootprintCapReached` when its live sessions already hold
    /// `ceilings.held_bytes`. The counts and the insert run under the
    /// connection mutex, so they are race-free in-process.
    ///
    /// An expired session consumes neither: the user can no longer see it.
    /// It is **not deleted here** — that would drop the only record of where
    /// its blob is (§ Reclaim rule 2); the expiry tick reclaims it, file first.
    ///
    /// `wrapped_session_key` is the client's own wrapped key (§ Key material).
    /// `None` is accepted so the column can carry a session opened before the
    /// key is minted, but a client that never supplies one has produced a blob
    /// no client can open — the handler, not this layer, is where that becomes
    /// a refusal.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_export_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        format: &str,
        scope_descriptor: &[u8],
        blob_path: &str,
        wrapped_session_key: Option<&[u8]>,
        ceilings: &ExportCeilings,
    ) -> Result<CreateExportSessionOutcome> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let in_flight: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM export_sessions
                 WHERE actor_id = ?1 AND state IN ('running','paused') AND expires_at >= ?2",
                rusqlite::params![&actor[..], now],
                |row| row.get(0),
            )
            .context("count in-flight export sessions")?;
        if in_flight >= i64::from(ceilings.concurrent) {
            return Ok(CreateExportSessionOutcome::ConcurrencyCapReached);
        }
        let held = conn
            .query_row(
                HELD_BYTES_SQL,
                rusqlite::params![&actor[..], now, session_id],
                |row| row.get::<_, i64>(0),
            )
            .context("sum held export blob bytes")? as u64;
        if held >= ceilings.held_bytes {
            return Ok(CreateExportSessionOutcome::FootprintCapReached {
                held_bytes: held,
                ceiling: ceilings.held_bytes,
            });
        }
        conn.execute(
            "INSERT INTO export_sessions
               (session_id, actor_id, format, scope_descriptor, state, started_at,
                last_progress_at, blob_path, blob_decryption_key_wrapped_for_actor,
                expires_at)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                session_id,
                &actor[..],
                format,
                scope_descriptor,
                now,
                blob_path,
                wrapped_session_key,
                now + EXPORT_SESSION_TTL_SECS,
            ],
        )
        .context("insert export_session")?;
        Ok(CreateExportSessionOutcome::Created)
    }

    /// Fetch one of the caller's sessions.
    pub async fn get_export_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
    ) -> Result<Option<ExportSessionRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {SELECT_COLS} FROM export_sessions
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![&actor[..], session_id],
            row_from_sql,
        )
        .optional()
        .context("get export_session")
    }

    /// The blob path of every session `actor` holds — expired ones included —
    /// for account deletion's unlink leg
    /// (`mail_export_blobs::unlink_export_blobs_for_actor`).
    ///
    /// Deliberately NOT [`Self::list_export_sessions`]: that one hides the
    /// actor's expired rows, whose blobs are still on disk until the expiry
    /// tick reclaims them — and an account deletion must unlink those too.
    pub async fn export_blob_paths_for_actor(&self, actor: &[u8; 32]) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT blob_path FROM export_sessions WHERE actor_id = ?1")
            .context("prepare export blob paths for actor")?;
        let paths = stmt
            .query_map(rusqlite::params![&actor[..]], |row| row.get::<_, String>(0))
            .context("query export blob paths for actor")?
            .collect::<rusqlite::Result<Vec<String>>>()
            .context("collect export blob paths for actor")?;
        Ok(paths)
    }

    /// Every blob path any session row names — the "still referenced" set the
    /// orphan reclaim (`mail_export_blobs::reclaim_orphaned_export_blobs`)
    /// checks a file against. Cross-actor on purpose, like
    /// [`Self::expired_export_sessions`]: it is a janitor's read, reached by no
    /// RPC.
    pub async fn all_export_blob_paths(&self) -> Result<std::collections::HashSet<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT blob_path FROM export_sessions")
            .context("prepare all export blob paths")?;
        let paths = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("query all export blob paths")?
            .collect::<rusqlite::Result<std::collections::HashSet<String>>>()
            .context("collect all export blob paths")?;
        Ok(paths)
    }

    /// List the caller's non-expired sessions, newest-started first — the
    /// resume list `list_export_sessions` serves.
    ///
    /// Filters expired rows rather than deleting them: a delete here would
    /// return no blob path and orphan the file (§ Reclaim rule 2). The expiry
    /// tick is what removes them, file first.
    pub async fn list_export_sessions(&self, actor: &[u8; 32]) -> Result<Vec<ExportSessionRow>> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SELECT_COLS} FROM export_sessions
                 WHERE actor_id = ?1 AND expires_at >= ?2 ORDER BY started_at DESC"
            ))
            .context("prepare list export_sessions")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..], now], row_from_sql)
            .context("query list export_sessions")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list export_sessions")?);
        }
        Ok(out)
    }

    /// Conditional state transition: moves the session to `new_state` iff its
    /// current state is in `allowed_from`, recording `error_reason` when given.
    ///
    /// Returns the post-transition row, or `None` when the session doesn't
    /// exist / isn't the caller's / is in a state the transition doesn't apply
    /// to. Making the guard part of the UPDATE is what keeps the state machine
    /// honest under concurrent clients: a `completed` session can never be
    /// dragged back to `running` by a late pause from another device.
    ///
    /// `expected_generation` is § Resume's only-while-I-am-still-the-driver
    /// condition: `Some(g)` moves the row only if `g` is its current stream
    /// generation, so a driver that was restarted over cannot pause, finish or
    /// cancel the stream that replaced its own. `None` is the user's own
    /// control and applies whatever the generation.
    pub async fn transition_export_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        new_state: ExportState,
        allowed_from: &[ExportState],
        error_reason: Option<&str>,
        expected_generation: Option<u64>,
    ) -> Result<Option<ExportSessionRow>> {
        let now = super::now_epoch_secs();
        let placeholders = allowed_from
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 7))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE export_sessions
             SET state = ?3, last_progress_at = ?4, expires_at = ?4 + {EXPORT_SESSION_TTL_SECS},
                 error_reason = COALESCE(?5, error_reason)
             WHERE actor_id = ?1 AND session_id = ?2
               AND (?6 IS NULL OR stream_generation = ?6)
               AND state IN ({placeholders})"
        );
        let conn = self.conn.lock().await;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(actor.to_vec()),
            Box::new(session_id.to_string()),
            Box::new(new_state.as_str()),
            Box::new(now),
            Box::new(error_reason.map(str::to_string)),
            Box::new(expected_generation.map(|g| g as i64)),
        ];
        for s in allowed_from {
            params.push(Box::new(s.as_str()));
        }
        let changed = conn
            .execute(
                &sql,
                rusqlite::params_from_iter(params.iter().map(|p| &**p)),
            )
            .context("transition export_session")?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            &format!(
                "SELECT {SELECT_COLS} FROM export_sessions
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![&actor[..], session_id],
            row_from_sql,
        )
        .optional()
        .context("reread export_session after transition")
    }

    /// Fold one processed batch into the session: bump the counters, advance
    /// the resume cursor, refresh `last_progress_at`/`expires_at`, and
    /// optionally revise `total_count` as the client finishes enumerating.
    ///
    /// Returns the post-update row (`None` = no such session for this actor,
    /// or one no longer in flight) — the row a `BridgeExportProgress` push
    /// event is rendered from.
    ///
    /// **Only an in-flight session folds** — `running`, or `paused` for the
    /// batch whose frame was appended just before a pause landed. The guard is
    /// in the UPDATE, like every other write here: this fold refreshes
    /// `expires_at`, so without it one call on a `completed` session re-armed
    /// its 30-day window, and every further call re-armed it again — § Expiry
    /// unbounded in time for a blob that is already finished.
    ///
    /// `expected_generation: Some(g)` folds only while `g` is still the
    /// session's stream generation (`None` otherwise, like a missing row). The
    /// up-leg passes the generation it reserved under: a restart can land
    /// between an upload's reservation and its fold, and the restart has just
    /// zeroed these counters for the new stream — a stale batch folded on top
    /// would leave the new export's progress permanently over-counted.
    ///
    /// The counters saturate at `i64::MAX`, and a delta or total that has no
    /// exact `i64` is an error ([`export_counter_to_sql`]) — both are the
    /// client's numbers, stored in `INTEGER` columns.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_export_progress(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        exported_delta: u64,
        skipped_delta: u64,
        errored_delta: u64,
        last_processed_message_id: Option<&str>,
        revised_total_count: Option<u64>,
        expected_generation: Option<u64>,
    ) -> Result<Option<ExportSessionRow>> {
        let exported_delta = export_counter_to_sql("exported_delta", exported_delta)?;
        let skipped_delta = export_counter_to_sql("skipped_delta", skipped_delta)?;
        let errored_delta = export_counter_to_sql("errored_delta", errored_delta)?;
        let revised_total_count = revised_total_count
            .map(|v| export_counter_to_sql("total_count", v))
            .transpose()?;
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                &format!(
                    "UPDATE export_sessions
                     SET exported_count = {exported},
                         skipped_count = {skipped},
                         errored_count = {errored},
                         last_processed_message_id =
                             COALESCE(?6, last_processed_message_id),
                         total_count = COALESCE(?7, total_count),
                         last_progress_at = ?8,
                         expires_at = ?8 + {EXPORT_SESSION_TTL_SECS}
                     WHERE actor_id = ?1 AND session_id = ?2
                       AND state IN ('running','paused')
                       AND (?9 IS NULL OR stream_generation = ?9)",
                    exported = saturating_add_sql("exported_count", "?3"),
                    skipped = saturating_add_sql("skipped_count", "?4"),
                    errored = saturating_add_sql("errored_count", "?5"),
                ),
                rusqlite::params![
                    &actor[..],
                    session_id,
                    exported_delta,
                    skipped_delta,
                    errored_delta,
                    last_processed_message_id,
                    revised_total_count,
                    now,
                    expected_generation.map(|g| g as i64),
                ],
            )
            .context("update export_session progress")?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            &format!(
                "SELECT {SELECT_COLS} FROM export_sessions
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![&actor[..], session_id],
            row_from_sql,
        )
        .optional()
        .context("reread export_session after progress")
    }

    /// Reserve `chunk_len` bytes of the session's blob for frame `chunk_idx`,
    /// refusing an append that is out of order or would cross § Quota
    /// composition's per-session ceiling.
    ///
    /// Three decisions live here rather than in the handler, all for one
    /// reason: the connection mutex serializes them against every other
    /// append, so two concurrent uploads cannot both observe a state that
    /// admits them. **(a) The session must be `running`** — the SELECT says
    /// so, which is what stops a `completed`, `cancelled` or `errored` session
    /// growing (the progress fold refuses one in its own UPDATE as well).
    /// **(a′) The caller must drive the
    /// session's current stream generation** (§ Resume), decided before the
    /// index: a driver another device restarted over is told exactly that.
    /// **(b) The frame index must be
    /// the expected one** (§ Blob shape on disk): the nest appends opaque
    /// bytes and parses no frame, so it can neither reorder an upload nor
    /// notice afterwards that one was reordered. **(c) The running total
    /// decides the ceiling**, and a refused chunk must not advance it — a
    /// client that narrows its scope and retries would otherwise be locked out
    /// by its own rejected chunk. **(d) So does the actor's footprint** — the
    /// bytes every live session of this actor holds, this one included,
    /// against `ceilings.held_bytes`; under the same mutex, so two sessions
    /// uploading at once cannot both fit into the last gigabyte.
    ///
    /// The caller writes bytes to disk only after this returns `Appended`.
    /// That ordering is the reverse of § Reclaim rule 1's row-before-file, and
    /// deliberately so: here the counter is a *reservation*, so a crash
    /// between the two leaves a counter ahead of the file — a short blob whose
    /// missing terminator frame makes it refuse to open — rather than bytes on
    /// disk the ceiling never counted.
    pub async fn append_export_blob_bytes(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        chunk_idx: u64,
        chunk_len: u64,
        ceilings: &ExportCeilings,
        stream_generation: u64,
    ) -> Result<AppendExportChunkOutcome> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let current: Option<(i64, i64, i64, String)> = conn
            .query_row(
                "SELECT blob_bytes, next_chunk_idx, stream_generation, blob_path
                 FROM export_sessions
                 WHERE actor_id = ?1 AND session_id = ?2 AND state = 'running'",
                rusqlite::params![&actor[..], session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .context("read export_session blob_bytes")?;
        let Some((current, next_idx, row_generation, blob_path)) = current else {
            return Ok(AppendExportChunkOutcome::NoSuchSession);
        };
        if row_generation as u64 != stream_generation {
            return Ok(AppendExportChunkOutcome::StreamSuperseded {
                current: row_generation as u64,
            });
        }
        let next_idx = next_idx as u64;
        if chunk_idx != next_idx {
            return Ok(AppendExportChunkOutcome::ChunkOutOfOrder { expected: next_idx });
        }
        let current = current as u64;
        let proposed = current.saturating_add(chunk_len);
        if proposed > ceilings.blob_bytes {
            return Ok(AppendExportChunkOutcome::BlobOversize {
                blob_bytes: current,
                ceiling: ceilings.blob_bytes,
            });
        }
        let held = conn
            .query_row(
                HELD_BYTES_SQL,
                rusqlite::params![&actor[..], now, session_id],
                |row| row.get::<_, i64>(0),
            )
            .context("sum held export blob bytes")? as u64;
        if held.saturating_add(chunk_len) > ceilings.held_bytes {
            return Ok(AppendExportChunkOutcome::FootprintExceeded {
                held_bytes: held,
                ceiling: ceilings.held_bytes,
            });
        }
        conn.execute(
            &format!(
                "UPDATE export_sessions
                 SET blob_bytes = ?3, next_chunk_idx = ?5, last_progress_at = ?4,
                     expires_at = ?4 + {EXPORT_SESSION_TTL_SECS}
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![
                &actor[..],
                session_id,
                proposed as i64,
                now,
                (next_idx + 1) as i64
            ],
        )
        .context("update export_session blob_bytes")?;
        Ok(AppendExportChunkOutcome::Appended {
            blob_bytes: proposed,
            next_chunk_idx: next_idx + 1,
            blob_path,
        })
    }

    /// The **cold resume** (§ Resume): open a new stream generation on one of
    /// the caller's in-flight sessions, returning the post-restart row and the
    /// *previous* generation's `blob_path` for the caller to unlink.
    ///
    /// One UPDATE, so there is no half-restarted row to observe: the state goes
    /// (back) to `running`, the generation advances, `blob_path` is repointed
    /// at the new generation's file, the cursor, the byte total, the counters
    /// and the resume marker return to zero, and the wrapped key is replaced by
    /// the restarting client's fresh one. `total_count` is revised when the
    /// client sent an estimate and kept otherwise. `error_reason` is untouched:
    /// only `running`/`paused` rows are eligible, and neither carries one.
    ///
    /// ⚠ **The row moves first and the old file is unlinked after** — not
    /// § Reclaim rule 2's file-before-row, because this row survives. From the
    /// instant of this UPDATE the old file is named by no row, so it is rule
    /// 3's garbage whether the caller's unlink runs, fails, or is lost to a
    /// crash; unlinking first would instead leave a `running` row naming a
    /// missing file if this UPDATE then failed. And the new file does not
    /// exist yet — frame 0 of the new stream creates it — which is rule 1.
    ///
    /// `None` when the session is not the caller's, or is not in flight:
    /// `completed` has nothing to redo, and `cancelled`/`errored` have released
    /// their concurrency slot, so reviving one would be a second way past
    /// § Quota composition's cap.
    pub async fn restart_export_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        wrapped_session_key: &[u8],
        total_count: Option<u64>,
    ) -> Result<Option<(ExportSessionRow, String)>> {
        let total_count = total_count
            .map(|v| export_counter_to_sql("total_count", v))
            .transpose()?;
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let current: Option<(i64, String)> = conn
            .query_row(
                "SELECT stream_generation, blob_path FROM export_sessions
                 WHERE actor_id = ?1 AND session_id = ?2 AND state IN ('running','paused')",
                rusqlite::params![&actor[..], session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("read export_session for restart")?;
        let Some((generation, previous_blob_path)) = current else {
            return Ok(None);
        };
        let generation = generation as u64 + 1;
        let blob_path =
            crate::mail_export_blobs::export_blob_path_for_generation(session_id, generation);
        conn.execute(
            &format!(
                "UPDATE export_sessions
                 SET state = 'running', stream_generation = ?3, blob_path = ?4,
                     next_chunk_idx = 0, blob_bytes = 0,
                     exported_count = 0, skipped_count = 0, errored_count = 0,
                     last_processed_message_id = '',
                     blob_decryption_key_wrapped_for_actor = ?5,
                     total_count = COALESCE(?6, total_count),
                     last_progress_at = ?7, expires_at = ?7 + {EXPORT_SESSION_TTL_SECS}
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![
                &actor[..],
                session_id,
                generation as i64,
                blob_path,
                wrapped_session_key,
                total_count,
                now,
            ],
        )
        .context("restart export_session")?;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM export_sessions
                     WHERE actor_id = ?1 AND session_id = ?2"
                ),
                rusqlite::params![&actor[..], session_id],
                row_from_sql,
            )
            .context("reread export_session after restart")?;
        Ok(Some((row, previous_blob_path)))
    }

    /// Delete one of the caller's sessions **whose blob the caller has already
    /// unlinked** — the second half of § Reclaim rule 2 (file before row).
    ///
    /// Serves "Discard now" (§ UX shape step 5). There is deliberately no form
    /// of this that hands a path back for unlinking afterwards: a row deleted
    /// before its file is a whole-mailbox snapshot nothing can find until the
    /// orphan reclaim's next pass, and an unlink that then fails leaves no
    /// record at all. So the caller reads the row, unlinks the file it names,
    /// and passes that same `unlinked_blob_path` here.
    ///
    /// The delete applies only while the row **still names that path**. A
    /// cold resume (§ Resume) repoints `blob_path` at a new generation's file
    /// between the caller's read and this delete; deleting then would drop the
    /// only record of a file nobody unlinked. `false` means nothing was
    /// deleted — the session is gone (a second discard: idempotent) or was
    /// repointed (the caller re-reads to tell which).
    pub async fn delete_export_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        unlinked_blob_path: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM export_sessions
                 WHERE actor_id = ?1 AND session_id = ?2 AND blob_path = ?3",
                rusqlite::params![&actor[..], session_id, unlinked_blob_path],
            )
            .context("delete export_session")?;
        Ok(deleted > 0)
    }

    /// Every expired session across all actors (§ Expiry) — the rows the
    /// expiry tick (`mail_export_blobs::sweep_expired_export_blobs`) unlinks
    /// the blob of and then deletes. A pure read: deleting here, before the
    /// files go, is the ordering § Reclaim rule 2 forbids.
    ///
    /// Cross-actor because the per-actor entry points only ever see an actor
    /// who comes *back*: a user who exports once and never opens the wizard
    /// again would otherwise leave a blob on disk past its 30 days, which is
    /// exactly the obligation § Expiry refuses to take on. Reached by no RPC.
    pub async fn expired_export_sessions(&self) -> Result<Vec<ExpiredExportSession>> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT actor_id, session_id, blob_path FROM export_sessions
                 WHERE expires_at < ?1",
            )
            .context("prepare expired export_sessions")?;
        let rows = stmt
            .query_map(rusqlite::params![now], |row| {
                let actor: Vec<u8> = row.get(0)?;
                Ok(ExpiredExportSession {
                    actor_id: blob_col_to_array(actor, 0, "actor_id")?,
                    session_id: row.get(1)?,
                    blob_path: row.get(2)?,
                })
            })
            .context("query expired export_sessions")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect expired export_sessions")?;
        Ok(rows)
    }

    /// The expiry tick's row half: delete `expired` once its blob is unlinked,
    /// under the same two conditions as [`Self::delete_export_session`] plus
    /// one — the row must **still** be expired. A session re-armed between the
    /// tick's read and this delete (its driver woke after 30 silent days) is
    /// not the tick's to remove; it keeps its row and loses a blob it was
    /// already past owning, which its client recovers from by restarting or
    /// discarding (`nest/common.md` § Client-state recoverability).
    pub async fn delete_expired_export_session(
        &self,
        expired: &ExpiredExportSession,
    ) -> Result<bool> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM export_sessions
                 WHERE actor_id = ?1 AND session_id = ?2 AND blob_path = ?3
                   AND expires_at < ?4",
                rusqlite::params![
                    &expired.actor_id[..],
                    expired.session_id,
                    expired.blob_path,
                    now
                ],
            )
            .context("delete expired export_session")?;
        Ok(deleted > 0)
    }
}

/// One row [`crate::db::CacheDb::expired_export_sessions`] found past its
/// `expires_at`: enough to unlink its blob and then delete exactly that row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredExportSession {
    pub actor_id: [u8; 32],
    pub session_id: String,
    pub blob_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    const ACTOR: [u8; 32] = [0xA1u8; 32];
    /// A second actor, for the isolation pins. § Cross-actor isolation is the
    /// product invariant this table exists under, so it gets real coverage
    /// rather than a comment.
    const OTHER: [u8; 32] = [0xB2u8; 32];

    /// The client-minted wrapped key (§ Key material). Opaque to the nest —
    /// the test only pins that it round-trips byte-for-byte, which is the
    /// whole of this column's contract.
    const WRAPPED_KEY: &[u8] = b"wrapped-per-session-key";

    async fn open_with_session(db: &CacheDb, actor: &[u8; 32], id: &str) {
        assert_eq!(
            db.create_export_session(
                actor,
                id,
                "mbox",
                b"\x01scope-cbor",
                &format!("exports/{id}.zip.zst"),
                Some(WRAPPED_KEY),
                &ExportCeilings::default(),
            )
            .await
            .unwrap(),
            CreateExportSessionOutcome::Created
        );
    }

    async fn fresh() -> CacheDb {
        let db = CacheDb::open_in_memory().unwrap();
        open_with_session(&db, &ACTOR, "s1").await;
        db
    }

    /// The default ceilings with the per-session one replaced.
    fn ceil(blob_bytes: u64) -> ExportCeilings {
        ExportCeilings {
            blob_bytes,
            ..ExportCeilings::default()
        }
    }

    /// The default ceilings with the concurrency cap replaced.
    fn cap(concurrent: u32) -> ExportCeilings {
        ExportCeilings {
            concurrent,
            ..ExportCeilings::default()
        }
    }

    #[tokio::test]
    async fn a_created_session_round_trips_through_get_and_list() {
        let db = fresh().await;
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.state, ExportState::Running.as_str());
        assert_eq!(row.format, "mbox");
        assert_eq!(row.scope_descriptor, b"\x01scope-cbor");
        assert_eq!(row.blob_path, "exports/s1.zip.zst");
        assert_eq!(row.blob_bytes, 0);
        assert_eq!(row.exported_count, 0);
        assert_eq!(row.started_at, row.last_progress_at);
        assert_eq!(row.expires_at, row.started_at + EXPORT_SESSION_TTL_SECS);

        let listed = db.list_export_sessions(&ACTOR).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], row);
    }

    #[tokio::test]
    async fn the_wrapped_session_key_round_trips_verbatim() {
        // § Key material: the nest stores the client's wrapped key so ANY of
        // the user's clients can fetch and unwrap it. If this column did not
        // round-trip exactly, a second device could not open the download —
        // and nothing else in the system would notice, because the nest never
        // opens the blob itself.
        let db = fresh().await;
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(
            row.blob_decryption_key_wrapped_for_actor.as_deref(),
            Some(WRAPPED_KEY)
        );
    }

    // ── § Cross-actor isolation ────────────────────────────────────────────

    #[tokio::test]
    async fn another_actor_can_neither_read_nor_mutate_the_session() {
        let db = fresh().await;

        assert!(
            db.get_export_session(&OTHER, "s1").await.unwrap().is_none(),
            "a foreign actor must not read the row"
        );
        assert!(
            db.list_export_sessions(&OTHER).await.unwrap().is_empty(),
            "a foreign actor must not see it in a listing"
        );
        assert!(
            db.transition_export_session(
                &OTHER,
                "s1",
                ExportState::Cancelled,
                &IN_FLIGHT_STATES,
                None,
                None
            )
            .await
            .unwrap()
            .is_none(),
            "a foreign actor must not cancel it"
        );
        assert_eq!(
            db.append_export_blob_bytes(&OTHER, "s1", 0, 10, &ceil(1 << 20), 0)
                .await
                .unwrap(),
            AppendExportChunkOutcome::NoSuchSession,
            "a foreign actor must not append to the blob"
        );
        assert!(
            !db.delete_export_session(&OTHER, "s1", "exports/s1.zip.zst")
                .await
                .unwrap(),
            "a foreign actor must not discard it"
        );

        // And none of those refusals damaged the real owner's session.
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.state, ExportState::Running.as_str());
        assert_eq!(row.blob_bytes, 0);
    }

    // ── § Quota composition — the concurrency cap ──────────────────────────

    #[tokio::test]
    async fn the_concurrency_cap_refuses_the_session_past_the_ceiling() {
        let db = CacheDb::open_in_memory().unwrap();
        for i in 0..ExportCeilings::default().concurrent {
            open_with_session(&db, &ACTOR, &format!("s{i}")).await;
        }
        assert_eq!(
            db.create_export_session(
                &ACTOR,
                "one-too-many",
                "mbox",
                b"",
                "exports/x.zip.zst",
                Some(WRAPPED_KEY),
                &ExportCeilings::default(),
            )
            .await
            .unwrap(),
            CreateExportSessionOutcome::ConcurrencyCapReached
        );
    }

    #[tokio::test]
    async fn a_paused_session_still_holds_a_concurrency_slot() {
        // § Quota composition spells this out — "a user with 3 paused exports
        // can have 4th-attempt rejected" — because each in-flight session
        // reserves disk whether or not it is actively running.
        let db = CacheDb::open_in_memory().unwrap();
        open_with_session(&db, &ACTOR, "s1").await;
        db.transition_export_session(
            &ACTOR,
            "s1",
            ExportState::Paused,
            &[ExportState::Running],
            None,
            None,
        )
        .await
        .unwrap()
        .expect("running -> paused");

        assert_eq!(
            db.create_export_session(&ACTOR, "s2", "mbox", b"", "p", None, &cap(1))
                .await
                .unwrap(),
            CreateExportSessionOutcome::ConcurrencyCapReached
        );
    }

    #[tokio::test]
    async fn a_terminal_session_releases_its_slot() {
        let db = CacheDb::open_in_memory().unwrap();
        open_with_session(&db, &ACTOR, "s1").await;
        db.transition_export_session(
            &ACTOR,
            "s1",
            ExportState::Completed,
            &[ExportState::Running],
            None,
            None,
        )
        .await
        .unwrap()
        .expect("running -> completed");

        assert_eq!(
            db.create_export_session(&ACTOR, "s2", "mbox", b"", "p", None, &cap(1))
                .await
                .unwrap(),
            CreateExportSessionOutcome::Created,
            "a completed export must not keep occupying a slot"
        );
    }

    #[tokio::test]
    async fn the_cap_is_per_actor_not_per_deployment() {
        let db = CacheDb::open_in_memory().unwrap();
        open_with_session(&db, &ACTOR, "s1").await;
        assert_eq!(
            db.create_export_session(&OTHER, "s2", "mbox", b"", "p", None, &cap(1))
                .await
                .unwrap(),
            CreateExportSessionOutcome::Created,
            "one user's exports must not ration another's"
        );
    }

    // ── § Session row model — the state machine ────────────────────────────

    #[tokio::test]
    async fn the_transition_guard_refuses_a_move_from_a_state_it_does_not_apply_to() {
        let db = fresh().await;
        db.transition_export_session(
            &ACTOR,
            "s1",
            ExportState::Completed,
            &[ExportState::Running],
            None,
            None,
        )
        .await
        .unwrap()
        .expect("running -> completed");

        // A late pause from a second device must not drag a finished export
        // back into flight.
        assert!(
            db.transition_export_session(
                &ACTOR,
                "s1",
                ExportState::Paused,
                &[ExportState::Running],
                None,
                None
            )
            .await
            .unwrap()
            .is_none()
        );
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.state, ExportState::Completed.as_str());
    }

    #[tokio::test]
    async fn an_error_reason_is_recorded_and_survives_a_later_transition() {
        let db = fresh().await;
        let row = db
            .transition_export_session(
                &ACTOR,
                "s1",
                ExportState::Errored,
                &[ExportState::Running],
                Some("session_blob_oversize"),
                None,
            )
            .await
            .unwrap()
            .expect("running -> errored");
        assert_eq!(row.error_reason, "session_blob_oversize");

        // COALESCE keeps the reason when a later transition passes None — the
        // wizard's error log must not be blanked by the cancel that follows.
        let row = db
            .transition_export_session(
                &ACTOR,
                "s1",
                ExportState::Cancelled,
                &[ExportState::Errored],
                None,
                None,
            )
            .await
            .unwrap()
            .expect("errored -> cancelled");
        assert_eq!(row.error_reason, "session_blob_oversize");
    }

    // ── Progress ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn progress_folds_counters_and_advances_the_resume_cursor() {
        let db = fresh().await;
        let row = db
            .record_export_progress(&ACTOR, "s1", 7, 1, 2, Some("msg-42"), Some(900), None)
            .await
            .unwrap()
            .expect("session exists");
        assert_eq!(
            (row.exported_count, row.skipped_count, row.errored_count),
            (7, 1, 2)
        );
        assert_eq!(row.last_processed_message_id, "msg-42");
        assert_eq!(row.total_count, 900, "the enumeration revised the estimate");

        // Deltas accumulate, and a None cursor leaves the old one standing.
        let row = db
            .record_export_progress(&ACTOR, "s1", 3, 0, 0, None, None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.exported_count, 10);
        assert_eq!(row.last_processed_message_id, "msg-42");
        assert_eq!(row.total_count, 900);
    }

    #[tokio::test]
    async fn progress_against_an_unknown_session_is_none_not_a_silent_insert() {
        let db = fresh().await;
        assert!(
            db.record_export_progress(&ACTOR, "nope", 1, 0, 0, None, None, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    // ── § Quota composition — the per-session disk ceiling ─────────────────

    #[tokio::test]
    async fn blob_appends_accumulate_until_the_ceiling_refuses_one() {
        let db = fresh().await;
        assert_eq!(
            db.append_export_blob_bytes(&ACTOR, "s1", 0, 600, &ceil(1000), 0)
                .await
                .unwrap(),
            AppendExportChunkOutcome::Appended {
                blob_bytes: 600,
                next_chunk_idx: 1,
                blob_path: "exports/s1.zip.zst".into(),
            }
        );
        // The chunk that would cross the ceiling is refused...
        assert_eq!(
            db.append_export_blob_bytes(&ACTOR, "s1", 1, 600, &ceil(1000), 0)
                .await
                .unwrap(),
            AppendExportChunkOutcome::BlobOversize {
                blob_bytes: 600,
                ceiling: 1000
            }
        );
        // ...and — the part that matters — the refusal did not advance the
        // total, so a client that narrows its scope and retries is not
        // permanently locked out by its own rejected chunk.
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.blob_bytes, 600);

        // A chunk that exactly reaches the ceiling is allowed: the ceiling is
        // a maximum size, not a size it must stay under.
        assert_eq!(
            db.append_export_blob_bytes(&ACTOR, "s1", 1, 400, &ceil(1000), 0)
                .await
                .unwrap(),
            AppendExportChunkOutcome::Appended {
                blob_bytes: 1000,
                next_chunk_idx: 2,
                blob_path: "exports/s1.zip.zst".into(),
            }
        );
    }

    #[tokio::test]
    async fn a_paused_session_accepts_no_chunks() {
        // Pause must actually stop the upload leg, not merely relabel the row
        // while chunks keep landing on disk.
        let db = fresh().await;
        db.transition_export_session(
            &ACTOR,
            "s1",
            ExportState::Paused,
            &[ExportState::Running],
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            db.append_export_blob_bytes(&ACTOR, "s1", 0, 10, &ceil(1 << 20), 0)
                .await
                .unwrap(),
            AppendExportChunkOutcome::NoSuchSession
        );
    }

    // ── Discard and § Expiry ──────────────────────────────────────────────

    #[tokio::test]
    async fn delete_removes_the_row_only_while_it_names_the_unlinked_blob() {
        // § Reclaim rule 2: the caller unlinks first and names what it
        // unlinked. A delete that does not match leaves the row — the only
        // record of where the blob is — standing.
        let db = fresh().await;
        assert!(
            !db.delete_export_session(&ACTOR, "s1", "exports/other.zip.zst")
                .await
                .unwrap(),
            "a row naming a different file must not go"
        );
        assert!(db.get_export_session(&ACTOR, "s1").await.unwrap().is_some());
        assert!(
            db.delete_export_session(&ACTOR, "s1", "exports/s1.zip.zst")
                .await
                .unwrap()
        );
        assert!(
            !db.delete_export_session(&ACTOR, "s1", "exports/s1.zip.zst")
                .await
                .unwrap(),
            "a second discard is a no-op, not an error"
        );
        assert!(db.list_export_sessions(&ACTOR).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_restart_between_the_unlink_and_the_delete_keeps_the_row() {
        // The discard read generation 0's path and unlinked it; a cold resume
        // then repointed the row at generation 1's file, which nobody has
        // unlinked. Deleting now would orphan it.
        let db = fresh().await;
        let unlinked = db
            .get_export_session(&ACTOR, "s1")
            .await
            .unwrap()
            .unwrap()
            .blob_path;
        db.restart_export_session(&ACTOR, "s1", b"k", None)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !db.delete_export_session(&ACTOR, "s1", &unlinked)
                .await
                .unwrap()
        );
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(row.blob_path, "exports/s1.1.zip.zst.sealed");
    }

    #[tokio::test]
    async fn an_expired_session_is_hidden_from_its_owner_but_never_deleted_by_them() {
        // The per-actor entry points used to DELETE expired rows and return no
        // path, orphaning the blob. Now they only stop seeing the row; the
        // expiry tick is what removes it, file first.
        let db = fresh().await;
        force_expire(&db).await;
        assert!(db.list_export_sessions(&ACTOR).await.unwrap().is_empty());
        // ...the expired row released its concurrency slot...
        assert_eq!(
            db.create_export_session(&ACTOR, "s2", "mbox", b"", "p", None, &cap(1))
                .await
                .unwrap(),
            CreateExportSessionOutcome::Created
        );
        // ...and it still names its blob, for the tick to unlink.
        assert!(
            db.all_export_blob_paths()
                .await
                .unwrap()
                .contains("exports/s1.zip.zst"),
            "a row a create or list deleted would leave its file to nobody"
        );
    }

    #[tokio::test]
    async fn the_expiry_read_finds_every_actors_expired_sessions_and_deletes_nothing() {
        // The per-actor views only run for a user who comes BACK. § Expiry's
        // 30-day promise has to hold for the user who exports once and never
        // opens the wizard again, which is what this cross-actor read is for.
        let db = CacheDb::open_in_memory().unwrap();
        open_with_session(&db, &ACTOR, "s1").await;
        open_with_session(&db, &OTHER, "s2").await;
        force_expire(&db).await;
        open_with_session(&db, &ACTOR, "live").await;

        let mut expired = db.expired_export_sessions().await.unwrap();
        expired.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(
            expired,
            vec![
                ExpiredExportSession {
                    actor_id: ACTOR,
                    session_id: "s1".into(),
                    blob_path: "exports/s1.zip.zst".into(),
                },
                ExpiredExportSession {
                    actor_id: OTHER,
                    session_id: "s2".into(),
                    blob_path: "exports/s2.zip.zst".into(),
                },
            ]
        );
        assert_eq!(
            db.all_export_blob_paths().await.unwrap().len(),
            3,
            "reading what expired must not delete it — the files have not gone yet"
        );
    }

    #[tokio::test]
    async fn an_expired_row_goes_only_while_it_is_still_expired_and_names_its_blob() {
        let db = fresh().await;
        force_expire(&db).await;
        let expired = db.expired_export_sessions().await.unwrap().remove(0);

        // Its driver woke between the tick's read and its delete.
        db.record_export_progress(&ACTOR, "s1", 1, 0, 0, None, None, None)
            .await
            .unwrap()
            .expect("a running session folds progress");
        assert!(
            !db.delete_expired_export_session(&expired).await.unwrap(),
            "a re-armed session is not the tick's to remove"
        );

        force_expire(&db).await;
        let repointed = ExpiredExportSession {
            blob_path: "exports/not-the-one.zip.zst".into(),
            ..expired.clone()
        };
        assert!(!db.delete_expired_export_session(&repointed).await.unwrap());
        assert!(db.delete_expired_export_session(&expired).await.unwrap());
        assert!(db.get_export_session(&ACTOR, "s1").await.unwrap().is_none());
    }

    // ── § Quota composition — the per-user footprint ───────────────────────

    /// Open `id`, fill it with `bytes`, and finish it — the export that frees
    /// its concurrency slot and keeps its blob for the whole § Expiry window.
    async fn finished_with(
        db: &CacheDb,
        actor: &[u8; 32],
        id: &str,
        bytes: u64,
        c: &ExportCeilings,
    ) {
        assert_eq!(
            db.create_export_session(actor, id, "mbox", b"", &format!("exports/{id}"), None, c)
                .await
                .unwrap(),
            CreateExportSessionOutcome::Created
        );
        assert!(matches!(
            db.append_export_blob_bytes(actor, id, 0, bytes, c, 0)
                .await
                .unwrap(),
            AppendExportChunkOutcome::Appended { .. }
        ));
        db.transition_export_session(
            actor,
            id,
            ExportState::Completed,
            &[ExportState::Running],
            None,
            None,
        )
        .await
        .unwrap()
        .expect("running -> completed");
    }

    #[tokio::test]
    async fn finished_exports_hold_the_footprint_the_concurrency_cap_released() {
        // The measured defect's shape: every finished export releases its slot
        // (`a_terminal_session_releases_its_slot`) and keeps its blob, so the
        // slot count alone let one user hold 12 × the per-session ceiling. The
        // footprint is what makes "3 × 10 GiB = 30 GiB per user" true.
        let db = CacheDb::open_in_memory().unwrap();
        let c = ExportCeilings::new(100, 3);
        assert_eq!(c.held_bytes, 300, "derived: per-session × concurrent");
        for id in ["a", "b", "c"] {
            finished_with(&db, &ACTOR, id, 100, &c).await;
        }
        assert_eq!(
            db.create_export_session(&ACTOR, "d", "mbox", b"", "exports/d", None, &c)
                .await
                .unwrap(),
            CreateExportSessionOutcome::FootprintCapReached {
                held_bytes: 300,
                ceiling: 300
            },
            "no slot is in use, but the disk is"
        );
    }

    #[tokio::test]
    async fn an_append_past_the_footprint_is_refused_without_reserving_anything() {
        let db = CacheDb::open_in_memory().unwrap();
        let c = ExportCeilings::new(100, 3);
        finished_with(&db, &ACTOR, "a", 100, &c).await;
        finished_with(&db, &ACTOR, "b", 100, &c).await;
        finished_with(&db, &ACTOR, "d", 50, &c).await;
        assert_eq!(
            db.create_export_session(&ACTOR, "c", "mbox", b"", "exports/c", None, &c)
                .await
                .unwrap(),
            CreateExportSessionOutcome::Created
        );
        db.append_export_blob_bytes(&ACTOR, "c", 0, 40, &c, 0)
            .await
            .unwrap();
        // 290 held; 20 more keeps this session at 60 of its own 100, but
        // takes the user to 310 of 300.
        assert_eq!(
            db.append_export_blob_bytes(&ACTOR, "c", 1, 20, &c, 0)
                .await
                .unwrap(),
            AppendExportChunkOutcome::FootprintExceeded {
                held_bytes: 290,
                ceiling: 300
            }
        );
        let row = db.get_export_session(&ACTOR, "c").await.unwrap().unwrap();
        assert_eq!(
            (row.blob_bytes, row.next_chunk_idx),
            (40, 1),
            "a refused chunk must not advance the total or the cursor"
        );
        // Exactly reaching the footprint is allowed, like the per-session one.
        assert_eq!(
            db.append_export_blob_bytes(&ACTOR, "c", 1, 10, &c, 0)
                .await
                .unwrap(),
            AppendExportChunkOutcome::Appended {
                blob_bytes: 50,
                next_chunk_idx: 2,
                blob_path: "exports/c".into(),
            }
        );
    }

    #[tokio::test]
    async fn disposed_expired_and_foreign_sessions_hold_no_footprint() {
        let db = CacheDb::open_in_memory().unwrap();
        let c = ExportCeilings::new(100, 3);
        // An expired finished export: its owner no longer sees it, and the
        // expiry tick is due to reclaim it.
        finished_with(&db, &ACTOR, "old", 100, &c).await;
        force_expire(&db).await;
        // Both disposals unlink the blob; the row keeps its `blob_bytes` for
        // the wizard to list. Counting either would ration the user against
        // disk nobody holds — and an export that FAILED is the case where that
        // would bite hardest, since the user's remedy is to run it again.
        for (id, disposed) in [("x", ExportState::Cancelled), ("y", ExportState::Errored)] {
            assert_eq!(
                db.create_export_session(
                    &ACTOR,
                    id,
                    "mbox",
                    b"",
                    &format!("exports/{id}"),
                    None,
                    &c
                )
                .await
                .unwrap(),
                CreateExportSessionOutcome::Created
            );
            db.append_export_blob_bytes(&ACTOR, id, 0, 100, &c, 0)
                .await
                .unwrap();
            db.transition_export_session(&ACTOR, id, disposed, &IN_FLIGHT_STATES, None, None)
                .await
                .unwrap()
                .unwrap();
        }
        // Another user's exports ration nobody else.
        for id in ["o1", "o2", "o3"] {
            finished_with(&db, &OTHER, id, 100, &c).await;
        }
        // None of the above counts against ACTOR: the whole 300 still fits.
        for id in ["a", "b", "c"] {
            finished_with(&db, &ACTOR, id, 100, &c).await;
        }
    }

    // ── § Expiry — the window a finished export cannot re-arm ──────────────

    #[tokio::test]
    async fn a_terminal_session_takes_no_progress_and_keeps_its_expiry() {
        // The measured defect's shape: a `completed` session past its expiry had
        // its 30-day window pushed out by one progress call, and every further
        // call pushed it again. `append_export_blob_bytes` already refused the
        // session; the fold now refuses it in its own UPDATE.
        for terminal in [
            ExportState::Completed,
            ExportState::Cancelled,
            ExportState::Errored,
        ] {
            let db = fresh().await;
            db.transition_export_session(
                &ACTOR,
                "s1",
                terminal,
                &[ExportState::Running],
                None,
                None,
            )
            .await
            .unwrap()
            .unwrap();
            force_expire(&db).await;
            assert!(
                db.record_export_progress(&ACTOR, "s1", 5, 0, 0, None, Some(9), None)
                    .await
                    .unwrap()
                    .is_none(),
                "{terminal:?} must not fold progress"
            );
            let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
            assert_eq!(
                (row.expires_at, row.exported_count, row.total_count),
                (1, 0, 0),
                "{terminal:?}: the window must stay closed and the counters untouched"
            );
        }
    }

    #[tokio::test]
    async fn a_paused_session_still_folds_the_batch_appended_before_the_pause() {
        let db = fresh().await;
        db.transition_export_session(
            &ACTOR,
            "s1",
            ExportState::Paused,
            &[ExportState::Running],
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let row = db
            .record_export_progress(&ACTOR, "s1", 3, 0, 0, None, None, None)
            .await
            .unwrap()
            .expect("a paused session is still in flight");
        assert_eq!(row.exported_count, 3);
    }

    // ── Counter range — the client's numbers in INTEGER columns ────────────

    #[tokio::test]
    async fn a_counter_with_no_exact_i64_is_refused_not_stored_negative() {
        let db = fresh().await;
        let too_big = i64::MAX as u64 + 1;
        for (exported, skipped, errored, total) in [
            (too_big, 0, 0, None),
            (0, too_big, 0, None),
            (0, 0, too_big, None),
            (0, 0, 0, Some(too_big)),
        ] {
            assert!(
                db.record_export_progress(
                    &ACTOR, "s1", exported, skipped, errored, None, total, None
                )
                .await
                .is_err()
            );
        }
        assert!(
            db.restart_export_session(&ACTOR, "s1", b"k", Some(too_big))
                .await
                .is_err()
        );
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!(
            (
                row.exported_count,
                row.skipped_count,
                row.errored_count,
                row.total_count
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(
            row.stream_generation, 0,
            "the refused restart moved nothing"
        );
    }

    #[tokio::test]
    async fn counters_saturate_instead_of_wedging_the_owners_listing() {
        // Two in-range deltas whose sum is not: SQLite would store a REAL that
        // no reader of this table can decode, and every listing would fail.
        let db = fresh().await;
        let max = i64::MAX as u64;
        for _ in 0..2 {
            db.record_export_progress(&ACTOR, "s1", max, max, max, None, None, None)
                .await
                .unwrap()
                .unwrap();
        }
        let listed = db.list_export_sessions(&ACTOR).await.unwrap();
        assert_eq!(
            (
                listed[0].exported_count,
                listed[0].skipped_count,
                listed[0].errored_count
            ),
            (max, max, max)
        );
    }

    #[tokio::test]
    async fn progress_pushes_the_expiry_window_out() {
        // § Expiry counts from `last_progress_at`, so a long-running export
        // must not expire out from under itself mid-run.
        let db = fresh().await;
        force_expire(&db).await;
        let row = db
            .record_export_progress(&ACTOR, "s1", 1, 0, 0, None, None, None)
            .await
            .unwrap()
            .expect("still there — nothing has swept yet");
        assert!(
            row.expires_at > super::super::now_epoch_secs(),
            "progress must re-arm the 30-day window"
        );
        assert_eq!(db.list_export_sessions(&ACTOR).await.unwrap().len(), 1);
    }

    // ── § Resume — the cold resume opens a new stream generation ───────────

    #[tokio::test]
    async fn a_restart_opens_a_new_generation_in_one_row_update() {
        let db = fresh().await;
        db.append_export_blob_bytes(&ACTOR, "s1", 0, 600, &ceil(1 << 20), 0)
            .await
            .unwrap();
        db.record_export_progress(&ACTOR, "s1", 7, 1, 2, Some("INBOX:9"), None, Some(0))
            .await
            .unwrap()
            .unwrap();

        let (row, previous) = db
            .restart_export_session(&ACTOR, "s1", b"fresh-wrapped-key", Some(40))
            .await
            .unwrap()
            .expect("a running session restarts");
        assert_eq!(
            previous, "exports/s1.zip.zst",
            "the old file is handed back"
        );
        assert_eq!(row.state, ExportState::Running.as_str());
        assert_eq!(row.stream_generation, 1);
        assert_eq!(row.blob_path, "exports/s1.1.zip.zst.sealed");
        assert_ne!(row.blob_path, previous, "a generation never reuses a file");
        assert_eq!(
            (row.next_chunk_idx, row.blob_bytes),
            (0, 0),
            "the new stream starts at frame 0 of an empty blob"
        );
        assert_eq!(
            (row.exported_count, row.skipped_count, row.errored_count),
            (0, 0, 0)
        );
        assert_eq!(row.last_processed_message_id, "");
        assert_eq!(row.total_count, 40);
        assert_eq!(
            row.blob_decryption_key_wrapped_for_actor.as_deref(),
            Some(&b"fresh-wrapped-key"[..]),
            "one key per generation — the restart replaces the wrapped key"
        );
    }

    #[tokio::test]
    async fn a_paused_session_restarts_to_running_and_keeps_its_total_without_an_estimate() {
        let db = fresh().await;
        db.record_export_progress(&ACTOR, "s1", 0, 0, 0, None, Some(900), None)
            .await
            .unwrap();
        db.transition_export_session(
            &ACTOR,
            "s1",
            ExportState::Paused,
            &[ExportState::Running],
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let (row, _) = db
            .restart_export_session(&ACTOR, "s1", b"k", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, ExportState::Running.as_str());
        assert_eq!(row.total_count, 900);
    }

    #[tokio::test]
    async fn terminal_and_foreign_sessions_do_not_restart() {
        // `cancelled`/`errored` released their concurrency slot, so reviving
        // one would be a second way past § Quota composition's cap; and the
        // storage layer's actor filter holds for this kind like every other.
        let db = fresh().await;
        assert!(
            db.restart_export_session(&OTHER, "s1", b"k", None)
                .await
                .unwrap()
                .is_none()
        );
        for terminal in [
            ExportState::Cancelled,
            ExportState::Errored,
            ExportState::Completed,
        ] {
            let db = fresh().await;
            db.transition_export_session(
                &ACTOR,
                "s1",
                terminal,
                &[ExportState::Running],
                None,
                None,
            )
            .await
            .unwrap()
            .unwrap();
            assert!(
                db.restart_export_session(&ACTOR, "s1", b"k", None)
                    .await
                    .unwrap()
                    .is_none(),
                "{terminal:?} must not restart"
            );
            let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
            assert_eq!(
                (row.state.as_str(), row.stream_generation),
                (terminal.as_str(), 0)
            );
        }
    }

    #[tokio::test]
    async fn a_superseded_driver_is_refused_before_its_chunk_index_is_read() {
        let db = fresh().await;
        db.append_export_blob_bytes(&ACTOR, "s1", 0, 10, &ceil(1 << 20), 0)
            .await
            .unwrap();
        db.restart_export_session(&ACTOR, "s1", b"k", None)
            .await
            .unwrap()
            .unwrap();
        // Index 1 is what the old driver would send next, and index 0 is what
        // the NEW stream expects: both must read as "not the driver", never as
        // an accepted frame or an out-of-order one.
        for idx in [0, 1] {
            assert_eq!(
                db.append_export_blob_bytes(&ACTOR, "s1", idx, 10, &ceil(1 << 20), 0)
                    .await
                    .unwrap(),
                AppendExportChunkOutcome::StreamSuperseded { current: 1 }
            );
        }
        // …and the refusals reserved nothing on the new stream.
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!((row.next_chunk_idx, row.blob_bytes), (0, 0));
        // The new driver's frame 0 goes to the new generation's file.
        assert_eq!(
            db.append_export_blob_bytes(&ACTOR, "s1", 0, 10, &ceil(1 << 20), 1)
                .await
                .unwrap(),
            AppendExportChunkOutcome::Appended {
                blob_bytes: 10,
                next_chunk_idx: 1,
                blob_path: "exports/s1.1.zip.zst.sealed".into(),
            }
        );
    }

    #[tokio::test]
    async fn a_stale_generation_neither_folds_progress_nor_moves_the_session() {
        let db = fresh().await;
        db.restart_export_session(&ACTOR, "s1", b"k", None)
            .await
            .unwrap()
            .unwrap();
        // A fold whose upload reserved under generation 0, landing after the
        // restart zeroed the counters for generation 1.
        assert!(
            db.record_export_progress(&ACTOR, "s1", 5, 0, 0, Some("INBOX:5"), None, Some(0))
                .await
                .unwrap()
                .is_none()
        );
        // The old driver's failure path: a cancel conditioned on ITS generation.
        assert!(
            db.transition_export_session(
                &ACTOR,
                "s1",
                ExportState::Cancelled,
                &IN_FLIGHT_STATES,
                None,
                Some(0),
            )
            .await
            .unwrap()
            .is_none(),
            "a superseded driver must not cancel the stream that replaced its own"
        );
        let row = db.get_export_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert_eq!((row.state.as_str(), row.exported_count), ("running", 0));
        // The user's own Cancel carries no generation and still applies.
        assert!(
            db.transition_export_session(
                &ACTOR,
                "s1",
                ExportState::Cancelled,
                &IN_FLIGHT_STATES,
                None,
                None,
            )
            .await
            .unwrap()
            .is_some()
        );
    }

    /// Force every row to look 30 days idle.
    async fn force_expire(db: &CacheDb) {
        let conn = db.conn.lock().await;
        conn.execute("UPDATE export_sessions SET expires_at = 1", [])
            .unwrap();
    }
}
