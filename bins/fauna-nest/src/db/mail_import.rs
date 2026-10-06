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
    })
}

const SELECT_COLS: &str = "session_id, actor_id, source_descriptor, state, started_at, \
     last_progress_at, total_count, imported_count, skipped_count, errored_count, \
     cursors, error_reason, expires_at, scope, source_sealed, source_hash, date_from";

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
    /// arm): the row rests sealless and the scrub's never-sealed arm clears the
    /// plaintext at the next boot.
    pub async fn create_import_session(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        source_descriptor: &str,
        total_count: u64,
        scope: &[String],
        date_from: &str,
        source_sealed: Option<&[u8]>,
    ) -> Result<CreateImportSessionOutcome> {
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
        conn.execute(
            "INSERT INTO import_sessions
               (session_id, actor_id, source_descriptor, state, started_at,
                last_progress_at, total_count, expires_at, source_hash, scope,
                source_sealed, date_from)
             VALUES (?1, ?2, ?3, 'running', ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                session_id,
                &actor[..],
                source_descriptor,
                now,
                total_count as i64,
                now + IMPORT_SESSION_TTL_SECS,
                // The lock companion AND the seal's salt, written at insert so
                // the partial UNIQUE index constrains a fresh session rather
                // than only post-boot.
                source_hash.as_slice(),
                scope_json,
                source_sealed,
                date_from,
            ],
        )
        .context("insert import_session")?;
        Ok(CreateImportSessionOutcome::Created)
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
        new_state: &str,
        allowed_from: &[&str],
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
            Box::new(new_state.to_string()),
            Box::new(now),
            Box::new(error_reason.map(str::to_string)),
        ];
        for s in allowed_from {
            params.push(Box::new(s.to_string()));
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

    /// Fold one processed message into the session: bump the matching
    /// counter, advance the per-mailbox resume cursor, refresh
    /// `last_progress_at`/`expires_at`, and optionally revise `total_count`.
    /// Returns the post-update row (`None` = no such session for this actor).
    #[allow(clippy::too_many_arguments)]
    pub async fn record_import_progress(
        &self,
        actor: &[u8; 32],
        session_id: &str,
        imported_delta: u64,
        skipped_delta: u64,
        errored_delta: u64,
        cursor: Option<(&str, u32, u32)>,
        revised_total_count: Option<u64>,
    ) -> Result<Option<ImportSessionRow>> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let existing = conn
            .query_row(
                "SELECT cursors FROM import_sessions
                 WHERE actor_id = ?1 AND session_id = ?2",
                rusqlite::params![&actor[..], session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("read import_session cursors")?;
        let Some(cursors_json) = existing else {
            return Ok(None);
        };
        // Same verified fold as `row_from_sql` above — idempotent via
        // `actor_message_dedup`, so a corrupt blob costs redundant re-scan
        // work, not duplicate messages.
        let mut cursors: BTreeMap<String, (u32, u32)> =
            serde_json::from_str(&cursors_json).unwrap_or_default();
        if let Some((mailbox, uid, uid_validity)) = cursor {
            cursors.insert(mailbox.to_string(), (uid, uid_validity));
        }
        let cursors_json = serde_json::to_string(&cursors).context("encode cursors")?;
        conn.execute(
            &format!(
                "UPDATE import_sessions
                 SET imported_count = imported_count + ?3,
                     skipped_count = skipped_count + ?4,
                     errored_count = errored_count + ?5,
                     cursors = ?6,
                     total_count = COALESCE(?7, total_count),
                     last_progress_at = ?8,
                     expires_at = ?8 + {IMPORT_SESSION_TTL_SECS}
                 WHERE actor_id = ?1 AND session_id = ?2"
            ),
            rusqlite::params![
                &actor[..],
                session_id,
                imported_delta as i64,
                skipped_delta as i64,
                errored_delta as i64,
                cursors_json,
                revised_total_count.map(|v| v as i64),
                now,
            ],
        )
        .context("update import_session progress")?;
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

    /// An unbounded import reads back as the empty string, never an error.
    #[tokio::test]
    async fn an_unbounded_import_reads_back_an_empty_since_date() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_import_session(&ACTOR, "s1", "src", 5, &[], "", None)
            .await
            .unwrap();
        let row = db.get_import_session(&ACTOR, "s1").await.unwrap().unwrap();
        assert!(row.date_from.is_empty());
    }

    #[tokio::test]
    async fn an_old_clients_empty_scope_reads_back_empty_not_an_error() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_import_session(&ACTOR, "s1", "src", 5, &[], "", None)
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

        // (a) The seal rests. Without it the graduated scrub arm cannot fire
        // and the never-sealed arm DESTROYS the label instead of migrating it.
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
            db.create_import_session(&ACTOR, "s2", SOURCE, 5, &[], "", Some(&client_seal(SOURCE)))
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
                Some(&client_seal("icloud:imap.mail.me.com:alice"))
            )
            .await
            .unwrap(),
            CreateImportSessionOutcome::Created
        );
        // Cancelled sessions release the lock.
        db.transition_import_session(&ACTOR, "s1", "cancelled", &["running", "paused"], None)
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
            db.transition_import_session(&other, "s1", "paused", &["running"], None)
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
            .transition_import_session(&ACTOR, "s1", "paused", &["running"], None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "paused");
        // Pause again: not in allowed_from → None.
        assert!(
            db.transition_import_session(&ACTOR, "s1", "paused", &["running"], None)
                .await
                .unwrap()
                .is_none()
        );
        db.transition_import_session(&ACTOR, "s1", "running", &["paused"], None)
            .await
            .unwrap()
            .unwrap();
        let row = db
            .transition_import_session(&ACTOR, "s1", "completed", &["running"], None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "completed");
        // Terminal: no way back.
        assert!(
            db.transition_import_session(&ACTOR, "s1", "running", &["paused"], None)
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
                "errored",
                &["running", "paused"],
                Some("auth_failed"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "errored");
        assert_eq!(row.error_reason, "auth_failed");
    }

    #[tokio::test]
    async fn progress_updates_counters_cursor_and_total() {
        let db = open_with_session("s1", "src").await;
        let row = db
            .record_import_progress(&ACTOR, "s1", 1, 0, 0, Some(("INBOX", 44, 7)), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.imported_count, 1);
        assert_eq!(row.cursors.get("INBOX"), Some(&(44, 7)));

        let row = db
            .record_import_progress(&ACTOR, "s1", 0, 1, 2, Some(("INBOX", 45, 7)), Some(250))
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
            .record_import_progress(&ACTOR, "s1", 1, 0, 0, Some(("Sent", 3, 9)), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.cursors.len(), 2);
        assert_eq!(row.cursors.get("Sent"), Some(&(3, 9)));
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
            db.create_import_session(&ACTOR, "s2", "src", 5, &[], "", Some(&client_seal("src")))
                .await
                .unwrap(),
            CreateImportSessionOutcome::Created
        );
    }
}
