//! Content labels, obligation actions, quarantine, and spam methods.

use super::spam_baseline::BaselineDeparture;
use super::{CacheDb, now_epoch_millis};
use super::{ContentLabelRow, ObligationActionRow, SpamPreferences, SpamTrainingHistoryRecord};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use uuid::Uuid;

/// Who may appeal an enforcement record — [`CacheDb::appeal_subject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppealSubject {
    /// A post-side record whose author this nest knows: only they appeal it
    /// (`moderation.md` § Wire — "the **author's** appeal").
    Author([u8; 32]),
    /// A record with no attributable author — a conversation takedown (sealed
    /// records persist no sender) or a post-side attribution gap. Any
    /// authenticated caller who holds the record's id appeals it.
    Unattributed,
}

/// The outcome of [`CacheDb::record_appeal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppealRecord {
    /// A new appeal row landed on the audit chain.
    Recorded,
    /// This appellant already has an appeal pending on this content — nothing
    /// was written.
    AlreadyPending,
}

/// The audit actions that record a **decision** on appealed content — each
/// one closes every appeal pending against its target
/// ([`CacheDb::record_appeal`]).
pub const APPEAL_DECISION_ACTIONS: [&str; 2] = [
    "moderation:legal-takedown",
    "moderation:legal-takedown-restore",
];

/// The training-history mutation a client-path `put_spam_model` write applies
/// atomically with the model re-seal (`CacheDb::put_spam_model_with_history` —
/// option (a) of the co-design; `mail-spam.md` § Training-sample retention). The
/// nest-internal projection of `fauna_protocol::bridge_routing::SpamHistoryOp`
/// (the handler maps the wire enum → these borrowed primitives + the snake_case
/// label/source strings). `sealed_subject` / `sealed_delta` are **holder-sealed
/// opaque bytes** stored verbatim — the handler has already refused an empty
/// or plaintext one.
pub enum SpamHistoryDbOp<'a> {
    /// A client-path train: insert one sealed audit row.
    Insert {
        /// The trained message's stable content id (stored opaque).
        message_id: &'a [u8],
        /// The mailbox the message was in at train time.
        mailbox: &'a str,
        /// The subject sealed to the actor's own key (→ `sealed_subject` column).
        sealed_subject: &'a [u8],
        /// The n-gram delta sealed to the actor's own key (→ `model_delta_applied`).
        sealed_delta: &'a [u8],
        /// snake_case wire label (`spam` / `ham`).
        label: &'a str,
        /// snake_case wire source (`imap_junk_flag` / …).
        source: &'a str,
    },
    /// A client-path undo: delete one of the caller's own rows by id.
    Delete {
        /// The 16-byte history id from a prior list.
        history_id: &'a [u8],
    },
}

/// The outcome of [`CacheDb::put_spam_model_with_history`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpamModelWrite {
    /// The model (and the history op, if any) committed. Carries the
    /// freshly-minted 16-byte `history_id` for an [`SpamHistoryDbOp::Insert`],
    /// `None` for a `Delete` or a model-only write, and — for a `Delete` that
    /// removed a row — the lesson it removed, so the handler can withdraw the
    /// report that lesson captured.
    Written {
        history_id: Option<Vec<u8>>,
        undone: Option<UndoneLesson>,
    },
    /// The `Insert` repeated the actor's newest recorded lesson for that
    /// message — the one-lesson rule (`mail-spam.md` § 3) — so NOTHING was
    /// written: not the model, not the row, not the holder copy. The stored
    /// model stays byte-identical to the last accepted write.
    DuplicateSignal,
}

/// The lesson a `put_spam_model` history `Delete` removed — the plaintext row
/// metadata (`message_id`, `label`) the report capture keys on
/// (`report-sharing.md` § Report capture, *Symmetry*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoneLesson {
    /// The trained message's stable 32-byte content id.
    pub message_id: Vec<u8>,
    /// The snake_case label the removed lesson carried (`spam` / `ham`).
    pub label: String,
}

/// The one-lesson rule's predicate (`mail-spam.md` § 3): the label of the
/// actor's NEWEST `spam_training_history` row for `message_id`, or `None` when
/// no lesson for that message is on record. A signal whose label equals it is
/// a duplicate — the lesson is on record and nothing has un-taught it since.
/// Causal, never temporal: no clock, no process-local map, and a nest restart
/// forgets nothing. Evaluated inside the sealed write's transaction
/// (`put_spam_model` with an `Insert`), the one training entry point. Newest = the latest
/// `created_at`, ties broken by insertion order (`rowid`) — two rows for one
/// message within a millisecond arrive only through a bug, and even then the
/// later insert wins.
fn newest_training_label(
    conn: &rusqlite::Connection,
    actor_id: &[u8],
    message_id: &[u8],
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT label FROM spam_training_history
          WHERE actor_id = ?1 AND message_id = ?2
          ORDER BY created_at DESC, rowid DESC
          LIMIT 1",
        rusqlite::params![actor_id, message_id],
        |row| row.get(0),
    )
    .optional()
}

/// The connection-level form of
/// [`CacheDb::withdraw_spam_baseline_if_contributor`], for a caller already
/// holding the lock — the account-deletion walk, which must withdraw and then
/// delete the contributor's rows without letting go of it in between.
///
/// One statement, so "was summed" and "withdraw" cannot interleave with a
/// publish. Only a SUMMED contributor withdraws — an actor holding a
/// non-departed row in the inclusion record the last served publish wrote
/// (`mail-spam.md` § Cold start Path 2 → *A contributor's departure withdraws
/// the baseline*, narrowed 2026-09-22) — so an actor who opts in after that
/// publish and leaves again withdraws nothing, and a summed one withdraws once
/// per publish. Withdrawn = the row absent, the never-published state:
/// `spam_baseline` is a derived aggregate any admin publish recreates from the
/// still-intact per-user models.
///
/// It is also where every departure reaches the delta floor's inclusion record
/// (`mail-spam.md` § Cold start Path 2 → *The floor applies to every published
/// DELTA*), so no departure site needs a second call:
/// [`BaselineDeparture::AccountStands`] marks the actor's inclusion row
/// departed; [`BaselineDeparture::AccountDeleted`] deletes it and counts the
/// purge. Either bumps the run state's `departures` generation when the actor
/// stands or was summed, so a publish already in flight does not land the
/// departed counts ([`CacheDb::land_spam_baseline_publish`]).
///
/// No transaction — the purge walk calls this under its own lock — so the
/// statements run in the order a crash between them leaves safe: the
/// generation first (at worst it defers one run), the withdrawal next, the
/// record last (a lost mark or count only UNDER-counts changes, which defers
/// rather than lands).
pub(super) fn withdraw_spam_baseline_if_contributor(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
    departure: BaselineDeparture,
) -> Result<bool> {
    let actor = actor_id.as_slice();
    let stands_or_summed: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1
                        FROM spam_models m
                        JOIN spam_preferences p ON p.actor_id = m.actor_id
                        WHERE m.actor_id = ?1 AND p.contribute_baseline = 1)
             OR EXISTS (SELECT 1 FROM spam_baseline_inclusions WHERE actor_id = ?1)",
        rusqlite::params![actor],
        |row| row.get(0),
    )?;
    if stands_or_summed {
        super::spam_baseline::bump_departures(conn)?;
    }
    let withdrawn = conn.execute(
        "DELETE FROM spam_baseline
         WHERE id = 0
           AND EXISTS (SELECT 1 FROM spam_baseline_inclusions
                       WHERE actor_id = ?1 AND departed = 0)",
        rusqlite::params![actor],
    )?;
    super::spam_baseline::record_departure(conn, actor_id, departure)?;
    Ok(withdrawn > 0)
}

impl CacheDb {
    // ── Content label CRUD ─────────────────────────────────────────────

    /// Insert or update a content label.
    ///
    /// **A label on a POST is a revoking write, so it marks in this
    /// transaction** (`web-content-hosting.md` § Routing, render, serving →
    /// *A revoke is durable*: a door marks by what its state change CAN do).
    /// The nest-as-publisher fold folds each post's labels through the region
    /// content policy in force, so an arriving label can turn a shown post into
    /// a blocked or collapsed one on every page that carries it at the next
    /// render (`region-blocking.md` § The nest-as-publisher leg). Latent while
    /// the compiled-in registry enrols no authority, and marked anyway: the
    /// census is decided on the state change's reach, not on today's registry.
    ///
    /// Gated on `content_type == "post"` — `mark_web_render_owed_for_post` is a
    /// no-op for an unpublished post anyway, and a `"channel"` label (the
    /// behavioral-anomaly writer) names no post at all. The door that renders
    /// what this marked is `fauna.labels.attach`, through
    /// [`CacheDb::web_publishers_of_post`].
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_content_label(
        &self,
        content_type: &str,
        content_id: &str,
        category: &str,
        confidence: f64,
        mechanism_type: u8,
        classifier_id: &[u8],
        classifier_version: u64,
        attestation_type: u8,
        attestation_data: Option<&[u8]>,
        obligation_id: Option<&[u8]>,
        created_at: i64,
        scanner_id: &[u8],
        signature: &[u8],
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO content_labels
                (content_type, content_id, category, confidence,
                 mechanism_type, classifier_id, classifier_version,
                 attestation_type, attestation_data, obligation_id,
                 created_at, scanner_id, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(content_type, content_id, category, classifier_id)
             DO UPDATE SET confidence = excluded.confidence,
                           attestation_type = excluded.attestation_type,
                           attestation_data = excluded.attestation_data,
                           created_at = excluded.created_at,
                           signature = excluded.signature",
            rusqlite::params![
                content_type,
                content_id,
                category,
                confidence,
                mechanism_type,
                classifier_id,
                classifier_version as i64,
                attestation_type,
                attestation_data,
                obligation_id,
                created_at,
                scanner_id,
                signature,
            ],
        )?;
        // The revoking half — see the doc comment. A content_id that is not a
        // 32-byte post hex names no post, so it marks nothing.
        if content_type == "post"
            && let Some(post_id) = crate::routes::parse_32_bytes(content_id)
        {
            super::web::mark_web_render_owed_for_post(&tx, &post_id)?;
        }
        let id = tx.last_insert_rowid();
        tx.commit().context("commit upsert_content_label")?;
        Ok(id)
    }

    /// Whoever has `post_id` web-published — the reader half of
    /// `db::web::mark_web_render_owed_for_post`, for a door that marked inside
    /// its transaction and now has to render what it owes after the commit
    /// (`fauna.labels.attach`). Empty for an unpublished post, the common case.
    pub async fn web_publishers_of_post(&self, post_id: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let rows: Vec<Vec<u8>> = conn
            .prepare(
                "SELECT DISTINCT actor_id FROM content_links \
                 WHERE link_type = 'web_published' AND source_id = ?1 AND actor_id IS NOT NULL",
            )
            .context("prepare web_publishers_of_post")?
            .query_map(rusqlite::params![post_id.as_slice()], |row| row.get(0))
            .context("query web_publishers_of_post")?
            .collect::<rusqlite::Result<_>>()
            .context("read web_publishers_of_post")?;
        Ok(rows
            .into_iter()
            .filter_map(|raw| <[u8; 32]>::try_from(raw.as_slice()).ok())
            .collect())
    }

    /// Get all labels for a content item.
    pub async fn get_content_labels(
        &self,
        content_type: &str,
        content_id: &str,
    ) -> Result<Vec<ContentLabelRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, category, confidence, mechanism_type, created_at
             FROM content_labels
             WHERE content_type = ?1 AND content_id = ?2
             ORDER BY confidence DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![content_type, content_id], |row| {
                Ok(ContentLabelRow {
                    id: row.get(0)?,
                    category: row.get(1)?,
                    confidence: row.get(2)?,
                    mechanism_type: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Check if content has a label at or above a confidence threshold.
    pub async fn has_label_above(
        &self,
        content_type: &str,
        content_id: &str,
        category: &str,
        min_confidence: f64,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM content_labels
             WHERE content_type = ?1 AND content_id = ?2
             AND category = ?3 AND confidence >= ?4",
            rusqlite::params![content_type, content_id, category, min_confidence],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get aggregate label statistics for the moderation stats endpoint.
    ///
    /// A community room's verdicts are left out. They were derived under a
    /// wrap the room's members granted for purposes that end at the room's own
    /// members (`conversation-rooms.md` § The three classes → *What the home
    /// nest does with its read* — "the whole of the grant"), and this read
    /// answers any user of the deployment.
    pub async fn get_label_stats(&self) -> Result<Vec<(String, i64, f64)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT category, COUNT(*), AVG(confidence)
             FROM content_labels
             WHERE content_type NOT IN ('room_message', 'room_post')
             GROUP BY category
             ORDER BY COUNT(*) DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ── Obligation action records ─────────────────────────────────────

    /// Insert an obligation action record.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_obligation_action(
        &self,
        content_type: &str,
        content_id: &str,
        author_hex: &str,
        obligation_id: &[u8],
        rule_index: i64,
        category: &str,
        confidence: f64,
        action_taken: u8,
        label_id: Option<i64>,
        timestamp: i64,
        signature: &[u8],
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO obligation_action_records
                (content_type, content_id, author_hex, obligation_id, rule_index,
                 category, confidence, action_taken, label_id, timestamp, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                content_type,
                content_id,
                author_hex,
                obligation_id,
                rule_index,
                category,
                confidence,
                action_taken,
                label_id,
                timestamp,
                signature,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get obligation actions for a specific content item.
    pub async fn get_obligation_actions(
        &self,
        content_type: &str,
        content_id: &str,
    ) -> Result<Vec<ObligationActionRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, content_type, content_id, obligation_id, rule_index,
                    category, confidence, action_taken, timestamp
             FROM obligation_action_records
             WHERE content_type = ?1 AND content_id = ?2
             ORDER BY action_taken ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![content_type, content_id], |row| {
                Ok(ObligationActionRow {
                    id: row.get(0)?,
                    content_type: row.get(1)?,
                    content_id: row.get(2)?,
                    obligation_id: row.get(3)?,
                    rule_index: row.get(4)?,
                    category: row.get(5)?,
                    confidence: row.get(6)?,
                    action_taken: row.get(7)?,
                    timestamp: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get all obligation actions for content authored by an actor.
    ///
    /// Uses the denormalized `author_hex` column so this works even for
    /// Reject actions where the post was never stored in `post_index`.
    pub async fn get_obligation_actions_for_author(
        &self,
        author_hex: &str,
    ) -> Result<Vec<ObligationActionRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, content_type, content_id, obligation_id,
                    rule_index, category, confidence, action_taken, timestamp
             FROM obligation_action_records
             WHERE author_hex = ?1
             ORDER BY timestamp DESC
             LIMIT 100",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![author_hex], |row| {
                Ok(ObligationActionRow {
                    id: row.get(0)?,
                    content_type: row.get(1)?,
                    content_id: row.get(2)?,
                    obligation_id: row.get(3)?,
                    rule_index: row.get(4)?,
                    category: row.get(5)?,
                    confidence: row.get(6)?,
                    action_taken: row.get(7)?,
                    timestamp: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Is there a decision here to appeal — and **whose** appeal is it?
    /// `None` = this nest holds no enforcement record for `content_id`.
    ///
    /// The gate behind `fauna.moderation.appeal`. The transparency triple
    /// (`moderation.md` § Legal takedown) promises the **author** an appeal
    /// handle *for an actioned record*, and for nothing else; without this check
    /// the handler audit-logs any string a caller sends, which is not an appeal
    /// trail but an unauthenticated write surface onto it.
    ///
    /// Four sources, because the two takedown kinds and the two post orderings
    /// record the compelled fact in different places — and each post-side
    /// source names the author, so the handle is scoped to them:
    ///
    /// 1. **`obligation_action_records`** — any action, any content type: the
    ///    author-queue row every post takedown writes (live or already-deleted),
    ///    and the row a future mail quarantine/reject would write (§
    ///    Implementation status today's open gap — the appeal surface is built
    ///    so a second kind of actioned row can use it). Author: the row's own
    ///    `author_hex`. This arm is what makes the handle outlive a restore: the
    ///    flag clears, the row stays as additive history (§ Persistence).
    /// 2. **`content_meta.legal_takedown_ref`** — the live post's serve-withhold
    ///    flag. Redundant with (1) on every production path today, and kept
    ///    deliberately: the flag *is* the takedown's primary state, so a path
    ///    that ever sets it without queueing a row still owes its author a
    ///    handle. Author: `content.author`, the takedown handler's own lookup.
    /// 3. **`legal_takedown_deleted_posts`** — the permanent floor row for a
    ///    post taken down and then deleted, in either ordering. The compelled
    ///    fact outlives the content it was compelled against. Author: the floor
    ///    row's `author_id`.
    /// 4. **`segment_records.legal_takedown_ref`** — the conversation arm, and
    ///    the one a gate reading obligation rows alone would get *wrong*: conv
    ///    records persist no sender, so a conv takedown writes no obligation row
    ///    at all, and the appeal is precisely how its triple holds (§ Legal
    ///    takedown → *Conversations*). [`AppealSubject::Unattributed`]: there is
    ///    no author to check against.
    ///
    /// A post-side record whose author no arm can name (an obligation row with
    /// an empty or undecodable `author_hex` and no other source) is `Unattributed` too —
    /// the safe failure direction of an attribution gap is to admit, since a
    /// false refusal denies the author the handle the triple guarantees.
    ///
    /// `content_id` is matched as given; the handler canonicalizes a 32-byte hex
    /// id to lowercase first (arms 2–4 decode it and are case-blind; arm 1
    /// compares the stored string).
    pub async fn appeal_subject(&self, content_id: &str) -> Result<Option<AppealSubject>> {
        let conn = self.conn.lock().await;

        // Arm 1. Any row makes the content appealable; a row naming an author
        // scopes it. A takedown writes `hex::encode(author)`, so a decodable
        // value is the author and anything else is an attribution gap.
        let mut stmt = conn
            .prepare_cached(
                "SELECT author_hex FROM obligation_action_records WHERE content_id = ?1",
            )
            .context("appeal_subject: prepare obligation_action_records")?;
        let authors = stmt
            .query_map(rusqlite::params![content_id], |row| row.get::<_, String>(0))
            .context("appeal_subject: obligation_action_records")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("appeal_subject: read obligation_action_records")?;
        drop(stmt);
        let obligation_actioned = !authors.is_empty();
        if let Some(author) = authors
            .iter()
            .find_map(|a| fauna_core::hex32::decode(a).ok())
        {
            return Ok(Some(AppealSubject::Author(author)));
        }

        // The remaining three arms are keyed by the 32-byte id. A `content_id`
        // that is not one simply matches none of them — never an error, and
        // never a scan.
        let Ok(id) = fauna_core::hex32::decode(content_id) else {
            return Ok(obligation_actioned.then_some(AppealSubject::Unattributed));
        };

        // Arm 2: the flag, attributed through the content row.
        let flagged: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT c.author FROM content_meta m
                   LEFT JOIN content c ON c.id = m.content_id
                  WHERE m.content_id = ?1 AND m.legal_takedown_ref IS NOT NULL
                  LIMIT 1",
                rusqlite::params![id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("appeal_subject: content_meta")?;
        let flag_actioned = flagged.is_some();
        if let Some(author) = flagged.flatten().and_then(|a| <[u8; 32]>::try_from(a).ok()) {
            return Ok(Some(AppealSubject::Author(author)));
        }

        // Arm 3: the post-delete floor carries its author.
        let floor: Option<Vec<u8>> = conn
            .query_row(
                "SELECT author_id FROM legal_takedown_deleted_posts WHERE post_id = ?1 LIMIT 1",
                rusqlite::params![id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("appeal_subject: legal_takedown_deleted_posts")?;
        if let Some(author) = floor.as_deref().and_then(|a| <[u8; 32]>::try_from(a).ok()) {
            return Ok(Some(AppealSubject::Author(author)));
        }

        // Arm 4: the conv mirror is keyed by the record's dag-cbor Cid over its
        // id — the same derivation the takedown handler makes from the same
        // wire `content_id` (`moderation_handlers::conversation_legal_takedown`).
        let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(id);
        let conv: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM segment_records
                  WHERE kind = 'conv' AND record_cid = ?1
                    AND legal_takedown_ref IS NOT NULL
                  LIMIT 1",
                rusqlite::params![&record_cid.as_bytes()[..]],
                |row| row.get(0),
            )
            .optional()
            .context("appeal_subject: segment_records")?;
        let actioned = obligation_actioned || flag_actioned || floor.is_some() || conv.is_some();
        Ok(actioned.then_some(AppealSubject::Unattributed))
    }

    /// Record `appellant`'s appeal against `content_id` on the permanent audit
    /// chain — **at most one pending appeal per (appellant, content_id)**.
    ///
    /// An appeal is *pending* until a decision on the content is recorded after
    /// it: a `moderation:legal-takedown` or `moderation:legal-takedown-restore`
    /// audit row targeting the same id ([`APPEAL_DECISION_ACTIONS`]). A repeat
    /// while one is pending collapses — [`AppealRecord::AlreadyPending`], no row
    /// written — so the chain grows by one bounded row per appellant per
    /// decision, never per call (`moderation.md` § Errors & edge cases). The
    /// check and the append share one lock hold, so two racing calls cannot both
    /// see "none pending".
    ///
    /// The appellant rides in `audit_log.actor_id` — the party who performed
    /// the act, the column the registry ruling reads (`actor_tables.rs`, the
    /// `audit_log` entry) — and `detail` carries only the reason. Appeal rows
    /// written before this shape carry `actor_id = NULL`, so they pend nothing.
    pub async fn record_appeal(
        &self,
        content_id: &str,
        appellant: &[u8; 32],
        reason: &str,
    ) -> Result<AppealRecord> {
        let conn = self.conn.lock().await;
        let last_decision: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM audit_log
                  WHERE target = ?1 AND action IN (?2, ?3)",
                rusqlite::params![
                    content_id,
                    APPEAL_DECISION_ACTIONS[0],
                    APPEAL_DECISION_ACTIONS[1]
                ],
                |row| row.get(0),
            )
            .context("record_appeal: last decision")?;
        let pending: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM audit_log
                  WHERE target = ?1 AND action = 'moderation:appeal'
                    AND actor_id = ?2 AND id > ?3
                  LIMIT 1",
                rusqlite::params![content_id, appellant.as_slice(), last_decision],
                |row| row.get(0),
            )
            .optional()
            .context("record_appeal: pending appeal")?;
        if pending.is_some() {
            return Ok(AppealRecord::AlreadyPending);
        }
        super::admin::audit_on_conn(
            &conn,
            Some(appellant.as_slice()),
            "moderation:appeal",
            Some(content_id),
            Some(&format!("reason={reason}")),
        )?;
        Ok(AppealRecord::Recorded)
    }

    // ── Quarantine / suppress ─────────────────────────────────────────

    /// Set the quarantine flag on a post.
    pub async fn set_post_quarantined(&self, post_id: &[u8; 32], quarantined: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE content_meta SET quarantined = ?1 WHERE content_id = ?2",
            rusqlite::params![quarantined as i64, post_id.as_slice()],
        )?;
        Ok(())
    }

    /// Set the suppressed flag on a post.
    pub async fn set_post_suppressed(&self, post_id: &[u8; 32], suppressed: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE content_meta SET suppressed = ?1 WHERE content_id = ?2",
            rusqlite::params![suppressed as i64, post_id.as_slice()],
        )?;
        Ok(())
    }

    // ── Legal-obligation takedown ─────────────────────────────────────

    /// Atomically apply (`reference = Some`) or overturn (`None`) a post's
    /// legal-obligation takedown (`moderation.md` § Categories & enforcement
    /// item 1): the `content_meta.legal_takedown_ref` flag write, the
    /// author-facing `obligation_action_records` row (takedown only — a restore
    /// keeps the historical row), and the permanent audit row commit in ONE
    /// SQLite transaction. Two invariants this shape enforces (review
    /// 2026-07-06 §§ F1/F4; `nest/common.md` § single atomic decision point):
    ///
    /// - **No false compliance (F1):** the flag `UPDATE` must match exactly one
    ///   `content_meta` row, else the whole transaction rolls back with an
    ///   error — the handler can then never reply or audit `"taken_down"` for a
    ///   flag that was not actually written. The handler's prior existence
    ///   check reads `content.author` (a *different* table), and the post-store
    ///   path's `content_meta` co-write is best-effort (`put_post` ignores
    ///   `write_post_index` errors; the raw/undecodable branch skips it
    ///   entirely), so "author exists but no `content_meta` row" is reachable.
    /// - **No transparency gap (F4):** flag + obligation + audit land
    ///   all-or-nothing, so no crash or storage error can leave content
    ///   withheld without its transparency rows (or vice versa).
    ///
    /// **Tombstone, not delete** — only a `content_meta` flag toggles; the
    /// content row is never dropped, so a restore is always possible (no
    /// user-irrecoverable data loss, alpha invariant).
    #[allow(clippy::too_many_arguments)]
    pub async fn post_legal_takedown_txn(
        &self,
        post_id: &[u8; 32],
        content_id_hex: &str,
        reference: Option<&str>,
        author: &[u8; 32],
        admin_actor: &[u8],
        audit_detail: &str,
        timestamp_us: i64,
    ) -> Result<()> {
        use anyhow::Context;
        let author_hex = hex::encode(author);
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin legal-takedown tx")?;
        let n = tx.execute(
            "UPDATE content_meta SET legal_takedown_ref = ?1 WHERE content_id = ?2",
            rusqlite::params![reference, post_id.as_slice()],
        )?;
        if n != 1 {
            // Dropping `tx` rolls back: nothing is committed, so no audit or
            // obligation row can claim a takedown/restore whose flag write
            // did not land (F1 — false-compliance guard).
            anyhow::bail!(
                "legal-takedown flag write matched {n} content_meta rows for post {content_id_hex} (want exactly 1); rolled back"
            );
        }
        if reference.is_some() {
            // A web-published post's rendered page, index entry and feed item
            // still carry the body this transaction just withheld, and the
            // render that drops them runs after the commit — so the owed
            // render rides it, for the boot drain to honour if the nest stops
            // in between (`web-content-hosting.md` § Routing, render, serving
            // → *A revoke is durable*). Takedown only: an overturn ADDS the
            // post back, which no one is harmed by waiting for.
            super::web::mark_web_render_owed_for_post(&tx, post_id)?;

            // The author-queue row (`fauna.moderation.actions`) — takedown
            // only. `category="illegal"` is the enforcement descriptor (not a
            // classifier emission); `obligation_id` records the issuing admin;
            // confidence 1.0 = a definitive legal action.
            tx.execute(
                "INSERT INTO obligation_action_records
                    (content_type, content_id, author_hex, obligation_id, rule_index,
                     category, confidence, action_taken, label_id, timestamp, signature)
                 VALUES ('post', ?1, ?2, ?3, 0, 'illegal', 1.0, ?4, NULL, ?5, ?6)",
                rusqlite::params![
                    content_id_hex,
                    author_hex,
                    admin_actor,
                    fauna_core::obligation::ObligationAction::TakenDown as u8,
                    timestamp_us,
                    &[][..],
                ],
            )
            .context("insert takedown obligation row")?;

            // The ATProto retraction witness — the same `tombstone/post`
            // journal row `delete_post_core` writes, in the SAME transaction
            // and `?`-propagated for the same reason: a witness this write
            // loses is a retraction the bridge can never learn about, and the
            // F4 no-transparency-gap invariant extends to it (a post withheld
            // off-box without its retraction is the gap that matters most).
            //
            // Dated at the TAKEDOWN instant, so it interleaves ahead of a
            // bridge watermark that long ago passed the post's own row. The
            // shared servability predicate already withholds the post itself
            // from the stream; the tombstone arm deliberately bypasses that
            // predicate, which is what lets the retraction still reach the
            // network for content that is no longer servable.
            //
            // Deterministic id (schema + post hex) ⇒ at most one witness per
            // post, and `insert_content`'s `INSERT OR REPLACE` makes a later
            // user delete of the same post rewrite this row at the LATER
            // instant. That can only move the row FORWARD in the stream — a
            // second retraction event is always later than the first — so it
            // can never fall behind a watermark that already consumed it, and
            // a re-consumed tombstone for an already-unmapped post is a clean
            // no-op on the bridge (`projector.go::projectTombstone`).
            //
            // An overturn (`reference == None`) deliberately writes NO
            // witness: see `moderation.md` § Legal takedown for the ruling.
            let witness = fauna_core::data::Tombstone {
                author: fauna_core::identity::ActorId(*author),
                post_id: fauna_cbor::Cid::from_digest_dag_cbor(*post_id),
                created_at: fauna_core::data::Timestamp(timestamp_us as u64),
            };
            let witness_bytes = fauna_core::encoding::canonical_encode(&witness)
                .context("encode atproto takedown retraction witness")?;
            // Derived from the post digest, NOT the caller-supplied
            // `content_id_hex`: that string comes straight off the wire, so an
            // uppercase-hex request would mint a SECOND witness id for the same
            // post and defeat the deterministic-id invariant this row and
            // `delete_post_core`'s share.
            let witness_id = super::content_id_for_document(
                super::atproto_projection::POST_TOMBSTONE_SCHEMA,
                &hex::encode(post_id),
            );
            super::content::insert_content(
                &tx,
                &witness_id,
                super::atproto_projection::POST_TOMBSTONE_SCHEMA,
                author,
                timestamp_us,
                &witness_bytes,
                None,
                "fauna",
                None,
            )
            .context("insert atproto takedown retraction witness")?;
        }
        let audit_action = if reference.is_some() {
            "moderation:legal-takedown"
        } else {
            "moderation:legal-takedown-restore"
        };
        super::admin::audit_on_conn(
            &tx,
            None,
            audit_action,
            Some(content_id_hex),
            Some(audit_detail),
        )?;
        tx.commit().context("commit legal-takedown tx")
    }

    /// Atomically apply a legal takedown against a post whose CONTENT ROW is
    /// already gone — its own author deleted it before the compelled order
    /// could land (`moderation.md` § Legal takedown → *Posts*, "deleted,
    /// then the order arrives"; the mirror-image ordering —  takedown, then
    /// delete — is what
    /// [`post_legal_takedown_txn`](Self::post_legal_takedown_txn) handles,
    /// with [`record_taken_down_post_deleted`](Self::record_taken_down_post_deleted)
    /// capturing it at delete time instead).
    ///
    /// Writes the author-facing obligation row (so `fauna.moderation.actions`
    /// still carries "taken_down" and the author can appeal) + the permanent
    /// audit row + `legal_takedown_deleted_posts` (the export/blob-door
    /// withhold's tombstone-inclusive source; `INSERT OR REPLACE` on
    /// `post_id`, so a retry after a crash lands the same row) in ONE
    /// transaction — the same F1/F4 no-transparency-gap guarantee
    /// [`post_legal_takedown_txn`](Self::post_legal_takedown_txn) gives the
    /// live case, minus the `content_meta` flag write this path has no row
    /// for.
    ///
    /// Deliberately does **not** repeat any leg the author's own delete
    /// already ran on its way past: no second ATProto retraction witness
    /// (the delete's own `tombstone/post` row already exists), no Nostr
    /// kind-5, no web re-render (the post is off the site's published list —
    /// its `content_links` row was cascaded away with the delete). `deleted_at`
    /// on the written row is the TAKEDOWN instant, not the true delete
    /// instant — a tombstoned segment mirror row carries no timestamp of its
    /// own, and nothing downstream reads this column for anything but the
    /// audit trail.
    #[allow(clippy::too_many_arguments)]
    pub async fn post_legal_takedown_of_deleted_post_txn(
        &self,
        post_id: &[u8; 32],
        content_id_hex: &str,
        reference: &str,
        author: &[u8; 32],
        admin_actor: &[u8],
        blob_digests: &[[u8; 32]],
        audit_detail: &str,
        timestamp_us: i64,
    ) -> Result<()> {
        use anyhow::Context;
        let author_hex = hex::encode(author);
        let flat_digests: Vec<u8> = blob_digests
            .iter()
            .flat_map(|d| d.iter().copied())
            .collect();
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin legal-takedown-of-deleted-post tx")?;

        tx.execute(
            "INSERT INTO obligation_action_records
                (content_type, content_id, author_hex, obligation_id, rule_index,
                 category, confidence, action_taken, label_id, timestamp, signature)
             VALUES ('post', ?1, ?2, ?3, 0, 'illegal', 1.0, ?4, NULL, ?5, ?6)",
            rusqlite::params![
                content_id_hex,
                author_hex,
                admin_actor,
                fauna_core::obligation::ObligationAction::TakenDown as u8,
                timestamp_us,
                &[][..],
            ],
        )
        .context("insert takedown obligation row (already-deleted post)")?;

        // The permanent compelled-fact record — the same table + shape
        // `record_taken_down_post_deleted` writes for the opposite
        // ordering, `INSERT OR REPLACE` on `post_id`.
        tx.execute(
            "INSERT OR REPLACE INTO legal_takedown_deleted_posts
                (post_id, author_id, legal_reference, blob_digests, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                post_id.as_slice(),
                author.as_slice(),
                reference,
                flat_digests,
                timestamp_us,
            ],
        )
        .context("insert legal_takedown_deleted_posts (takedown of already-deleted post)")?;

        super::admin::audit_on_conn(
            &tx,
            None,
            "moderation:legal-takedown",
            Some(content_id_hex),
            Some(audit_detail),
        )?;
        tx.commit()
            .context("commit legal-takedown-of-deleted-post tx")
    }

    /// Set (or clear) the legal-obligation takedown flag on a post
    /// (`moderation.md` § Categories & enforcement item 1). `reference = Some(r)`
    /// takes the post down under the legal-obligation reference `r`; `None`
    /// restores it (an overturned appeal). **Tombstone, not delete** — this only
    /// toggles a `content_meta` flag; the content row is never dropped, so a
    /// restore is always possible (no user-irrecoverable data loss, alpha
    /// invariant). Sibling of [`set_post_quarantined`](Self::set_post_quarantined).
    ///
    /// **Bare flag write, no row-count assert and no transparency rows** — the
    /// admin takedown handler must use
    /// [`post_legal_takedown_txn`](Self::post_legal_takedown_txn) instead; this
    /// stays as a test-seeding / serve-gate helper.
    pub async fn set_post_legal_takedown(
        &self,
        post_id: &[u8; 32],
        reference: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE content_meta SET legal_takedown_ref = ?1 WHERE content_id = ?2",
            rusqlite::params![reference, post_id.as_slice()],
        )?;
        Ok(())
    }

    /// Read a post's legal-obligation takedown reference, or `None` if it is not
    /// taken down. The single serve-time gate for the tombstone: `get_post_core`
    /// checks this **before** the quarantine gate and, when `Some`, withholds the
    /// body from every viewer and returns the tombstone. Keys on `content_id`
    /// only (no body inspection) so it functions identically in both storage
    /// modes. A missing row / read error is treated as "not taken down"
    /// (world-serve the body), matching the fail-open posture of the sibling
    /// quarantine read — a takedown is a positive, explicitly-written flag.
    pub async fn get_post_legal_takedown(&self, post_id: &[u8; 32]) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let val: Option<String> = conn
            .query_row(
                "SELECT legal_takedown_ref FROM content_meta WHERE content_id = ?1",
                rusqlite::params![post_id.as_slice()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(val)
    }

    /// Check if a post is quarantined.
    pub async fn is_post_quarantined(&self, post_id: &[u8; 32]) -> Result<bool> {
        let conn = self.conn.lock().await;
        let val: i64 = conn
            .query_row(
                "SELECT quarantined FROM content_meta WHERE content_id = ?1",
                rusqlite::params![post_id.as_slice()],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(val != 0)
    }

    /// Read-authz for a single post/content — the canonical "may `caller` read
    /// `content_id`" check, the **existing per-actor authorization pattern**
    /// `mail-spam.md` § Cross-actor isolation refers to. A non-quarantined post
    /// is world-readable; a quarantined post is readable only by its author or
    /// an admin. `caller == None` is the anonymous/unauthenticated reader (never
    /// author/admin).
    ///
    /// Single source of truth for both the post-serving gate
    /// ([`get_post_core`](crate::routes::get_post_core)) and the
    /// `fauna.moderation.train` body-ingest gate — training must not read a body
    /// the caller can't read, else `train(arbitrary content_id)` + a model
    /// readback is a content-reconstruction oracle.
    ///
    /// Returns a plain `bool`, absorbing DB errors exactly as the serving gate
    /// always has: a quarantine-flag read error is treated as "not quarantined"
    /// (the common case is world-readable), an author/admin lookup error as
    /// "not author / not admin" (deny). This keeps the two call sites identical
    /// and the hot read path behavior-preserving.
    pub async fn caller_may_read_content(
        &self,
        caller: Option<&[u8; 32]>,
        content_id: &[u8; 32],
    ) -> bool {
        if !self.is_post_quarantined(content_id).await.unwrap_or(false) {
            return true;
        }
        let Some(id) = caller else { return false };
        let is_author = self.get_content_author(content_id).await.ok().flatten() == Some(*id);
        let is_admin = self.is_admin(&id[..]).await.unwrap_or(false);
        is_author || is_admin
    }

    // ── Spam models ───────────────────────────────────────────────────

    /// Get an actor's stored per-user model — always a sealed blob the nest
    /// cannot read (`put_spam_model_with_history` is its one writer). Returns
    /// None if the actor has no model.
    pub async fn get_spam_model(&self, actor_id: &[u8; 32]) -> Result<Option<(Vec<u8>, i64, i64)>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT model_json, ham_count, spam_count FROM spam_models WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(result)
    }

    // ── Spam preferences ──────────────────────────────────────────────

    /// Get spam preferences for a user. Returns defaults if not set.
    pub async fn get_spam_preferences(&self, actor_id: &[u8; 32]) -> Result<SpamPreferences> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT spam_threshold, phishing_threshold, contribute_baseline
                 FROM spam_preferences WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| {
                    Ok(SpamPreferences {
                        spam_threshold: row.get(0)?,
                        phishing_threshold: row.get(1)?,
                        contribute_baseline: row.get::<_, i64>(2)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(result.unwrap_or_default())
    }

    /// Update spam preferences for a user.
    pub async fn upsert_spam_preferences(
        &self,
        actor_id: &[u8; 32],
        prefs: &SpamPreferences,
    ) -> Result<()> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO spam_preferences (actor_id, spam_threshold, phishing_threshold, contribute_baseline, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(actor_id) DO UPDATE SET
                spam_threshold = excluded.spam_threshold,
                phishing_threshold = excluded.phishing_threshold,
                contribute_baseline = excluded.contribute_baseline,
                updated_at = excluded.updated_at",
            rusqlite::params![
                actor_id.as_slice(),
                prefs.spam_threshold,
                prefs.phishing_threshold,
                prefs.contribute_baseline as i64,
                now,
            ],
        )?;
        Ok(())
    }

    // ── Deployment spam baseline ──────────────────────────────────────

    /// Get the published deployment baseline model bytes, or `None` if no
    /// baseline has been published yet. The single-row table key is `id = 0`.
    pub async fn get_spam_baseline(&self) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT model_json FROM spam_baseline WHERE id = 0",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        Ok(result)
    }

    /// Replace the published deployment baseline with a freshly-aggregated
    /// model. `contributors` is the number of contributing models merged (an
    /// aggregate count — it does not identify any user). Republish is a full
    /// overwrite of the single `id = 0` row.
    pub async fn upsert_spam_baseline(
        &self,
        model_json: &[u8],
        ham_count: i64,
        spam_count: i64,
        contributors: i64,
    ) -> Result<()> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO spam_baseline (id, model_json, ham_count, spam_count, contributors, published_at)
             VALUES (0, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                model_json = excluded.model_json,
                ham_count = excluded.ham_count,
                spam_count = excluded.spam_count,
                contributors = excluded.contributors,
                published_at = excluded.published_at",
            rusqlite::params![model_json, ham_count, spam_count, contributors, now],
        )?;
        Ok(())
    }

    /// Withdraw the published baseline if `actor_id` is a summed contributor
    /// of it — the one act behind every departure (`mail-spam.md` § Cold
    /// start Path 2 → *A contributor's departure withdraws the baseline*).
    /// Returns whether a baseline was withdrawn.
    ///
    /// ⚠ **Call it BEFORE the departure itself.** Whether the actor stands as
    /// a contributor — what defers a publish already in flight — is read off
    /// the very rows the departure is about to change (the opt-in bit, the
    /// model row); called after, it finds a non-contributor and bumps nothing.
    ///
    /// Every caller of this form is a departure that leaves the account
    /// standing (opt-out, model reset, grant revoke), so it marks the actor's
    /// inclusion row departed; the account-deletion walk calls the
    /// connection-level form with [`BaselineDeparture::AccountDeleted`].
    pub async fn withdraw_spam_baseline_if_contributor(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let conn = self.conn.lock().await;
        withdraw_spam_baseline_if_contributor(&conn, actor_id, BaselineDeparture::AccountStands)
    }

    // ── Per-user model reset + training-history audit trail ───────────

    /// Delete an actor's per-user spam model row (`reset_spam_model`,
    /// `mail-spam.md` § Reset). Idempotent — deleting an absent row is a no-op.
    pub async fn delete_spam_model(&self, actor_id: &[u8; 32]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM spam_models WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )?;
        Ok(())
    }

    // ── Deployment-baseline holder copies (`mail-spam.md` § Encrypted-mode
    //    interaction, ratified 2026-07-13) ────────────────────────────────

    /// Delete **all** of an actor's sealed-to-holder model copies — fired on
    /// baseline opt-out (`set_baseline_contribution(false)`) and on model
    /// reset (`reset_spam_model`; the model the copies mirror is gone).
    /// Idempotent; the copies are recreatable derived data (the agent
    /// re-seals a fresh copy on its next opted-in write).
    pub async fn delete_spam_model_holder_copies(&self, actor_id: &[u8; 32]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM spam_model_holder_copies WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )?;
        Ok(())
    }

    /// Delete one `(actor, holder)` copy — fired when a
    /// `content.read{spam-model}` grant to that holder is revoked (the copy's
    /// authorization record is gone, so the at-rest artifact goes with it).
    pub async fn delete_spam_model_holder_copy_for_holder(
        &self,
        actor_id: &[u8; 32],
        holder_pubkey: &[u8],
    ) -> Result<()> {
        let holder_pubkey = holder_pubkey.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM spam_model_holder_copies WHERE actor_id = ?1 AND holder_pubkey = ?2",
            rusqlite::params![actor_id.as_slice(), holder_pubkey],
        )?;
        Ok(())
    }

    /// List the sealed-to-holder copies resting for one holder —
    /// `(actor_id, sealed_copy)` rows, ordered by actor for determinism. The
    /// `publish_spam_baseline` drain worklist intersects this with the
    /// holder's standing `content.read{spam-model}` grants (the copy alone is
    /// NOT authorization — the grant is; a revoked contributor's residual row
    /// is filtered there and deleted by the revoke path).
    pub async fn list_spam_model_holder_copies(
        &self,
        holder_pubkey: &[u8],
    ) -> Result<Vec<([u8; 32], Vec<u8>)>> {
        let holder_pubkey = holder_pubkey.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id, sealed_copy FROM spam_model_holder_copies
             WHERE holder_pubkey = ?1
             ORDER BY actor_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![holder_pubkey], |row| {
            let actor: Vec<u8> = row.get(0)?;
            let copy: Vec<u8> = row.get(1)?;
            Ok((actor, copy))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (actor, copy) = row?;
            let Ok(actor32): std::result::Result<[u8; 32], _> = actor.as_slice().try_into() else {
                continue;
            };
            out.push((actor32, copy));
        }
        Ok(out)
    }

    /// Atomically write a **holder-re-sealed** model + apply an optional
    /// training-history mutation, in **one transaction** (`put_spam_model` with
    /// `history_op` — option (a) of the co-design; `mail-spam.md`
    /// § Training-sample retention, build-item 3 write side). Carrying the model
    /// re-seal and the audit-row INSERT/DELETE on one commit makes a client-path
    /// train (`{model-upsert + history-INSERT}`) or undo (`{model-upsert +
    /// history-DELETE}`) crash-atomic — there is no half-state where the model
    /// updated but the undo row is missing, or (the dangerous one) a client-path
    /// undo mutated the model but left the row, which a re-undo would then apply a
    /// second time (`SpamModel::apply_inverse_delta_ngrams` re-decrements the class
    /// counter on the already-inverted model). Counts are stored `0/0` — the
    /// `sealed_model` is opaque (the nest holds only the actor's public half and
    /// cannot decode it, exactly like `put_spam_model`'s non-history path). Returns
    /// [`SpamModelWrite::Written`] carrying the freshly-minted 16-byte
    /// `history_id` for an [`SpamHistoryDbOp::Insert`] (`None` for a `Delete` or
    /// a model-only `op = None` write) and the [`UndoneLesson`] a `Delete`
    /// removed — or [`SpamModelWrite::DuplicateSignal`]
    /// when the `Insert` repeats the actor's newest recorded lesson for that
    /// message (the one-lesson rule, `mail-spam.md` § 3, evaluated FIRST inside
    /// this transaction), in which case nothing at all is written. Caller-scoped by
    /// `actor_id` (the connection's authenticated actor; no cross-actor surface).
    /// `holder_copy` is an optional deployment-baseline holder copy —
    /// `(holder_pubkey, sealed_copy)` — replaced for `(actor, holder)` in the
    /// **same transaction**, so the holder-readable copy never lags the model
    /// it mirrors (`mail-spam.md` § Encrypted-mode interaction, ratified
    /// 2026-07-13). `None` leaves any stored copy untouched (a writer that opted out or has no
    /// holder merely leaves stale weights, never drops the contributor).
    pub async fn put_spam_model_with_history(
        &self,
        actor_id: &[u8; 32],
        sealed_model: &[u8],
        op: Option<SpamHistoryDbOp<'_>>,
        holder_copy: Option<(&[u8], &[u8])>,
    ) -> Result<SpamModelWrite> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        // The one-lesson rule, checked before anything is written: dropping
        // `tx` un-committed rolls back, and nothing above this point wrote.
        if let Some(SpamHistoryDbOp::Insert {
            message_id, label, ..
        }) = &op
            && newest_training_label(&tx, actor_id.as_slice(), message_id)?.as_deref()
                == Some(*label)
        {
            return Ok(SpamModelWrite::DuplicateSignal);
        }
        if let Some((holder_pubkey, sealed_copy)) = holder_copy {
            tx.execute(
                "INSERT INTO spam_model_holder_copies (actor_id, holder_pubkey, sealed_copy, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(actor_id, holder_pubkey) DO UPDATE SET
                    sealed_copy = excluded.sealed_copy,
                    updated_at = excluded.updated_at",
                rusqlite::params![actor_id.as_slice(), holder_pubkey, sealed_copy, now],
            )?;
        }
        // Store the holder's re-sealed blob VERBATIM (opaque; counts 0/0),
        // inside the shared transaction so the history op below cannot commit
        // without it. `updated_at` strictly increases on every write to a row:
        // the spam baseline's delta floor reads "written since it was summed"
        // as `updated_at` differing from the value the inclusion record kept,
        // and two writes inside one millisecond must still read as a change
        // (`db/spam_baseline.rs`).
        tx.execute(
            "INSERT INTO spam_models (actor_id, model_json, ham_count, spam_count, updated_at)
             VALUES (?1, ?2, 0, 0, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET
                model_json = excluded.model_json,
                ham_count = excluded.ham_count,
                spam_count = excluded.spam_count,
                updated_at = MAX(excluded.updated_at, spam_models.updated_at + 1)",
            rusqlite::params![actor_id.as_slice(), sealed_model, now],
        )?;
        let (history_id, undone) = match op {
            Some(SpamHistoryDbOp::Insert {
                message_id,
                mailbox,
                sealed_subject,
                sealed_delta,
                label,
                source,
            }) => {
                let history_id = Uuid::new_v4().as_bytes().to_vec();
                // The subject rests only in `sealed_subject`, the delta only in
                // `model_delta_applied` — both holder-sealed, stored verbatim.
                tx.execute(
                    "INSERT INTO spam_training_history
                        (history_id, actor_id, message_id, mailbox, label, source, model_delta_applied, created_at, sealed_subject)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        history_id,
                        actor_id.as_slice(),
                        message_id,
                        mailbox,
                        label,
                        source,
                        sealed_delta,
                        now,
                        sealed_subject,
                    ],
                )?;
                (Some(history_id), None)
            }
            Some(SpamHistoryDbOp::Delete { history_id }) => {
                // Caller-scoped delete — an id that isn't this actor's own row
                // matches nothing (no cross-actor undo). The removed lesson's
                // plaintext metadata is read back so the handler can withdraw
                // the report it captured.
                let undone = tx
                    .query_row(
                        "DELETE FROM spam_training_history WHERE history_id = ?1 AND actor_id = ?2
                         RETURNING message_id, label",
                        rusqlite::params![history_id, actor_id.as_slice()],
                        |row| {
                            Ok(UndoneLesson {
                                message_id: row.get(0)?,
                                label: row.get(1)?,
                            })
                        },
                    )
                    .optional()?;
                (None, undone)
            }
            None => (None, None),
        };
        tx.commit()?;
        Ok(SpamModelWrite::Written { history_id, undone })
    }

    /// List an actor's training-history rows, newest-first, capped at `limit`
    /// (`list_spam_training_history`). When `before_history_id` is `Some`, only
    /// rows strictly older than that row are returned (keyset "load more"); an
    /// unknown/GC'd cursor yields an empty page. Caller-scoped by `actor_id`.
    pub async fn list_spam_training_history(
        &self,
        actor_id: &[u8; 32],
        limit: u32,
        before_history_id: Option<&[u8]>,
    ) -> Result<Vec<SpamTrainingHistoryRecord>> {
        let conn = self.conn.lock().await;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(SpamTrainingHistoryRecord {
                history_id: row.get::<_, Vec<u8>>(0)?,
                mailbox: row.get::<_, String>(1)?,
                label: row.get::<_, String>(2)?,
                source: row.get::<_, String>(3)?,
                model_delta_applied: row.get::<_, Vec<u8>>(4)?,
                created_at: row.get::<_, i64>(5)?,
                sealed_subject: row.get::<_, Vec<u8>>(6)?,
            })
        };
        // Keyset pagination on `(created_at, history_id)` matching the
        // `idx_spam_training_history_actor` ordering. The cursor's own
        // (created_at, history_id) is resolved by a correlated subquery; an
        // absent cursor row makes the comparison NULL ⇒ no rows (refetch).
        let rows: Vec<SpamTrainingHistoryRecord> = match before_history_id {
            Some(cursor) => {
                let mut stmt = conn.prepare(
                    "SELECT history_id, mailbox, label, source, model_delta_applied, created_at, sealed_subject
                     FROM spam_training_history
                     WHERE actor_id = ?1
                       AND (created_at, history_id) <
                           (SELECT created_at, history_id FROM spam_training_history
                            WHERE history_id = ?2 AND actor_id = ?1)
                     ORDER BY created_at DESC, history_id DESC
                     LIMIT ?3",
                )?;
                let mapped = stmt.query_map(
                    rusqlite::params![actor_id.as_slice(), cursor, limit],
                    map_row,
                )?;
                mapped.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT history_id, mailbox, label, source, model_delta_applied, created_at, sealed_subject
                     FROM spam_training_history
                     WHERE actor_id = ?1
                     ORDER BY created_at DESC, history_id DESC
                     LIMIT ?2",
                )?;
                let mapped =
                    stmt.query_map(rusqlite::params![actor_id.as_slice(), limit], map_row)?;
                mapped.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    /// Delete all of an actor's training-history rows (`reset_spam_model`).
    pub async fn delete_all_spam_training_history(&self, actor_id: &[u8; 32]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM spam_training_history WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
        )?;
        Ok(())
    }

    /// Garbage-collect training-history rows older than `cutoff_ms`
    /// (`mail.spam.training_history_retention_days`, default 30 d —
    /// `mail-spam.md` § Training-sample retention). The n-gram weights persist in
    /// `spam_models`; only the per-event audit/undo trail ages out. Returns the
    /// number of rows pruned. Called daily by
    /// `bridge_routing_handlers::run_spam_training_history_gc` (wired into the
    /// nest daily maintenance loop in `main.rs`) with the admin-effective
    /// retention.
    pub async fn gc_spam_training_history(&self, cutoff_ms: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn.execute(
            "DELETE FROM spam_training_history WHERE created_at < ?1",
            rusqlite::params![cutoff_ms],
        )?;
        Ok(n)
    }
}

// ── User-initiated abuse reports (`moderation.md` § User-initiated reporting) ──

/// One `abuse_reports` row, as the handlers read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbuseReportRow {
    pub id: String,
    pub created_at: i64,
    pub reporter_actor: Option<Vec<u8>>,
    pub origin_nest_id: Option<String>,
    pub origin_report_ref: Option<String>,
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_channel: Option<String>,
    pub subject_actor: Option<String>,
    pub reason: String,
    pub note: Option<String>,
    pub excerpt: Option<String>,
    pub block_author: bool,
    pub forwarded_to: Option<String>,
    pub status: String,
    pub outcome: Option<String>,
    pub resolved_at: Option<i64>,
    pub resolved_by: Option<Vec<u8>>,
    pub forwarded_nest_id: Option<String>,
}

/// A new local report, as `fauna.moderation.abuse_report.submit` hands it to
/// [`CacheDb::insert_abuse_report`].
#[derive(Debug, Clone)]
pub struct NewAbuseReport {
    pub reporter_actor: [u8; 32],
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_channel: Option<String>,
    pub subject_actor: Option<String>,
    pub reason: String,
    pub note: Option<String>,
    pub excerpt: Option<String>,
    pub block_author: bool,
}

/// A copy of a report forwarded from a peer nest, as
/// `fauna.federation.abuse_report.deliver` hands it to
/// [`CacheDb::insert_forwarded_abuse_report`]. No reporter identity: it never
/// crosses a nest boundary.
#[derive(Debug, Clone)]
pub struct ForwardedAbuseReport {
    /// The channel-verified origin nest's id, hex.
    pub origin_nest_id: String,
    /// The origin nest's own report id.
    pub origin_report_ref: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_channel: Option<String>,
    pub subject_actor: Option<String>,
    pub reason: String,
    pub note: Option<String>,
    pub excerpt: Option<String>,
}

/// One pending federation triad call in `abuse_report_outbox`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbuseReportOutboxEntry {
    pub id: i64,
    /// The local report the call is about (on a home nest, the forwarded
    /// copy's own id).
    pub report_id: String,
    /// `deliver` / `withdraw` / `outcome`.
    pub kind: String,
    pub peer_url: String,
    /// The peer's verified nest id (hex) the dial is pinned to, when known.
    pub peer_nest_id: Option<String>,
    /// The canonical dag-cbor request.
    pub payload: Vec<u8>,
    pub attempts: i64,
    /// Epoch seconds.
    pub created_at: i64,
}

/// The outcome of [`CacheDb::insert_abuse_report`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbuseReportInsert {
    /// A new open report landed.
    Inserted(AbuseReportRow),
    /// The reporter already has an open report on this subject — nothing was
    /// written (one open report per (reporter, subject); a retry lands here).
    AlreadyOpen(AbuseReportRow),
    /// The reporter is past the per-hour cap — nothing was written.
    RateLimited,
}

/// The outcome of [`CacheDb::withdraw_abuse_report`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbuseReportWithdraw {
    /// The open report is now withdrawn and its note + excerpt deleted.
    Withdrawn(Box<AbuseReportRow>),
    /// It was already withdrawn — nothing changed (idempotent).
    AlreadyWithdrawn,
    /// It is resolved — the audit record stands; a resolved report is not
    /// withdrawable.
    Resolved,
    /// No report of this reporter's has this id.
    NotFound,
}

/// The outcome of [`CacheDb::resolve_abuse_report`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbuseReportResolve {
    /// The open report is now resolved with the outcome.
    Resolved(Box<AbuseReportRow>),
    /// Already resolved with this same outcome — nothing changed (a retry).
    AlreadyResolved,
    /// Resolved with a different outcome, or withdrawn — the record stands.
    Conflict,
    NotFound,
}

const ABUSE_REPORT_COLUMNS: &str = "id, created_at, reporter_actor, origin_nest_id, \
     origin_report_ref, subject_kind, subject_id, subject_channel, subject_actor, reason, \
     note, excerpt, block_author, forwarded_to, status, outcome, resolved_at, resolved_by, \
     forwarded_nest_id";

fn abuse_report_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AbuseReportRow> {
    Ok(AbuseReportRow {
        id: row.get(0)?,
        created_at: row.get(1)?,
        reporter_actor: row.get(2)?,
        origin_nest_id: row.get(3)?,
        origin_report_ref: row.get(4)?,
        subject_kind: row.get(5)?,
        subject_id: row.get(6)?,
        subject_channel: row.get(7)?,
        subject_actor: row.get(8)?,
        reason: row.get(9)?,
        note: row.get(10)?,
        excerpt: row.get(11)?,
        block_author: row.get::<_, i64>(12)? != 0,
        forwarded_to: row.get(13)?,
        status: row.get(14)?,
        outcome: row.get(15)?,
        resolved_at: row.get(16)?,
        resolved_by: row.get(17)?,
        forwarded_nest_id: row.get(18)?,
    })
}

fn abuse_report_by_id(
    conn: &rusqlite::Connection,
    id: &str,
) -> rusqlite::Result<Option<AbuseReportRow>> {
    conn.query_row(
        &format!("SELECT {ABUSE_REPORT_COLUMNS} FROM abuse_reports WHERE id = ?1"),
        rusqlite::params![id],
        abuse_report_from_row,
    )
    .optional()
}

/// One hour in microseconds — the rate window of `ABUSE_REPORTS_PER_HOUR`.
const ABUSE_REPORT_RATE_WINDOW_MICROS: i64 = 3_600 * 1_000_000;

impl CacheDb {
    /// File a local report: the one-open-per-(reporter, subject) dedupe, the
    /// per-reporter rate check and the insert, under one lock so two
    /// concurrent submits cannot both pass either bound. `now` is
    /// microseconds. The dedupe runs **before** the rate check, so a retry of
    /// a report that landed is answered with it even at the cap.
    pub async fn insert_abuse_report(
        &self,
        report: NewAbuseReport,
        per_hour: u32,
        now: i64,
    ) -> Result<AbuseReportInsert> {
        let conn = self.conn.lock().await;
        let reporter = report.reporter_actor.as_slice();
        let existing = conn
            .query_row(
                &format!(
                    "SELECT {ABUSE_REPORT_COLUMNS} FROM abuse_reports
                      WHERE reporter_actor = ?1 AND subject_kind = ?2
                        AND subject_id = ?3 AND status = 'open'
                      LIMIT 1"
                ),
                rusqlite::params![reporter, report.subject_kind, report.subject_id],
                abuse_report_from_row,
            )
            .optional()
            .context("insert_abuse_report: dedupe")?;
        if let Some(row) = existing {
            return Ok(AbuseReportInsert::AlreadyOpen(row));
        }
        let recent: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM abuse_reports
                  WHERE reporter_actor = ?1 AND created_at > ?2",
                rusqlite::params![reporter, now - ABUSE_REPORT_RATE_WINDOW_MICROS],
                |row| row.get(0),
            )
            .context("insert_abuse_report: rate")?;
        if recent >= i64::from(per_hour) {
            return Ok(AbuseReportInsert::RateLimited);
        }
        let id = hex::encode(Uuid::new_v4().as_bytes());
        conn.execute(
            "INSERT INTO abuse_reports
                (id, created_at, reporter_actor, subject_kind, subject_id,
                 subject_channel, subject_actor, reason, note, excerpt,
                 block_author, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'open')",
            rusqlite::params![
                id,
                now,
                reporter,
                report.subject_kind,
                report.subject_id,
                report.subject_channel,
                report.subject_actor,
                report.reason,
                report.note,
                report.excerpt,
                i64::from(report.block_author),
            ],
        )
        .context("insert_abuse_report: insert")?;
        let row = abuse_report_by_id(&conn, &id)
            .context("insert_abuse_report: read back")?
            .context("insert_abuse_report: row vanished")?;
        Ok(AbuseReportInsert::Inserted(row))
    }

    /// One report by id.
    pub async fn get_abuse_report(&self, id: &str) -> Result<Option<AbuseReportRow>> {
        let conn = self.conn.lock().await;
        abuse_report_by_id(&conn, id).context("get_abuse_report")
    }

    /// The reporter's own reports, newest first (the *Your reports* ledger).
    pub async fn list_abuse_reports_by_reporter(
        &self,
        reporter: &[u8; 32],
    ) -> Result<Vec<AbuseReportRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {ABUSE_REPORT_COLUMNS} FROM abuse_reports
                  WHERE reporter_actor = ?1 ORDER BY created_at DESC, id"
            ))
            .context("list_abuse_reports_by_reporter: prepare")?;
        let rows = stmt
            .query_map(
                rusqlite::params![reporter.as_slice()],
                abuse_report_from_row,
            )
            .context("list_abuse_reports_by_reporter: query")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list_abuse_reports_by_reporter: read")?;
        Ok(rows)
    }

    /// Every open report on this nest, local and forwarded, oldest first (the
    /// admin queue).
    pub async fn list_open_abuse_reports(&self) -> Result<Vec<AbuseReportRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {ABUSE_REPORT_COLUMNS} FROM abuse_reports
                  WHERE status = 'open' ORDER BY created_at, id"
            ))
            .context("list_open_abuse_reports: prepare")?;
        let rows = stmt
            .query_map([], abuse_report_from_row)
            .context("list_open_abuse_reports: query")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list_open_abuse_reports: read")?;
        Ok(rows)
    }

    /// Withdraw one of `reporter`'s open reports: status `withdrawn`, note and
    /// excerpt deleted (the reporter's data). The returned row still carries
    /// `forwarded_to` so the caller can propagate the withdrawal.
    pub async fn withdraw_abuse_report(
        &self,
        reporter: &[u8; 32],
        id: &str,
    ) -> Result<AbuseReportWithdraw> {
        let conn = self.conn.lock().await;
        let Some(row) = abuse_report_by_id(&conn, id).context("withdraw_abuse_report: read")?
        else {
            return Ok(AbuseReportWithdraw::NotFound);
        };
        if row.reporter_actor.as_deref() != Some(reporter.as_slice()) {
            // Someone else's report reads exactly as a missing one.
            return Ok(AbuseReportWithdraw::NotFound);
        }
        match row.status.as_str() {
            "withdrawn" => return Ok(AbuseReportWithdraw::AlreadyWithdrawn),
            "open" => {}
            _ => return Ok(AbuseReportWithdraw::Resolved),
        }
        conn.execute(
            "UPDATE abuse_reports SET status = 'withdrawn', note = NULL, excerpt = NULL
              WHERE id = ?1 AND status = 'open'",
            rusqlite::params![id],
        )
        .context("withdraw_abuse_report: update")?;
        let row = abuse_report_by_id(&conn, id)
            .context("withdraw_abuse_report: read back")?
            .context("withdraw_abuse_report: row vanished")?;
        Ok(AbuseReportWithdraw::Withdrawn(Box::new(row)))
    }

    /// The account-deletion leg of `abuse_reports` (`moderation.md` § Where it
    /// lands → the deletion ruling; the table's `Policy::Partial` entry in
    /// `actor_tables.rs`): a deleted reporter's every OPEN report is withdrawn
    /// exactly as the living door withdraws one ([`Self::withdraw_abuse_report`]
    /// followed by `abuse_report_federation::propagate_withdrawal`) — status
    /// `withdrawn`, note and excerpt deleted, a queued delivery dropped, a
    /// delivery that reached the home nest followed by a queued `withdraw`; a
    /// delivery in flight is covered by its landing, which queues the
    /// withdrawal on finding the row withdrawn
    /// ([`Self::mark_abuse_report_forwarded`]). Resolved rows and rows already
    /// withdrawn are not touched: the admin's audit record, and the living
    /// withdrawal's own residue. Returns how many reports were withdrawn; `now`
    /// is epoch seconds, the queue's clock.
    ///
    /// `pub(super)` and connection-taking, like `outbox_purge_for_deleted_author`:
    /// the purge walk calls it under its own lock, so the withdrawal and the
    /// queued propagation land in one step — the living door's two calls can
    /// be parted by a crash, this one cannot.
    pub(super) fn withdraw_abuse_reports_for_deleted_reporter(
        conn: &rusqlite::Connection,
        reporter: &[u8; 32],
        now: i64,
    ) -> Result<usize> {
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {ABUSE_REPORT_COLUMNS} FROM abuse_reports
                  WHERE reporter_actor = ?1 AND status = 'open' ORDER BY created_at, id"
            ))
            .context("withdraw_abuse_reports_for_deleted_reporter: prepare")?;
        let open = stmt
            .query_map(
                rusqlite::params![reporter.as_slice()],
                abuse_report_from_row,
            )
            .context("withdraw_abuse_reports_for_deleted_reporter: query")?
            .collect::<rusqlite::Result<Vec<AbuseReportRow>>>()
            .context("withdraw_abuse_reports_for_deleted_reporter: read")?;
        for row in &open {
            conn.execute(
                "UPDATE abuse_reports SET status = 'withdrawn', note = NULL, excerpt = NULL
                  WHERE id = ?1",
                rusqlite::params![row.id],
            )
            .context("withdraw_abuse_reports_for_deleted_reporter: withdraw")?;
            // `propagate_withdrawal`'s rule: a delivery that never left is
            // dropped and needs no withdrawal; one that landed is followed.
            let dropped = conn
                .execute(
                    "DELETE FROM abuse_report_outbox WHERE report_id = ?1 AND kind = ?2",
                    rusqlite::params![row.id, ABUSE_CALL_DELIVER],
                )
                .context("withdraw_abuse_reports_for_deleted_reporter: cancel deliver")?;
            if dropped > 0 {
                continue;
            }
            if let Some(peer_url) = row.forwarded_to.as_deref() {
                enqueue_abuse_report_call_in(
                    conn,
                    &row.id,
                    ABUSE_CALL_WITHDRAW,
                    peer_url,
                    row.forwarded_nest_id.as_deref(),
                    &crate::abuse_report_federation::withdraw_payload(&row.id)?,
                    now,
                )?;
            }
        }
        Ok(open.len())
    }

    /// Record an open report's outcome (a record, not an action). `outcome` is
    /// the stored token (`acted` / `dismissed`); `now` is microseconds.
    pub async fn resolve_abuse_report(
        &self,
        id: &str,
        outcome: &str,
        resolved_by: &[u8; 32],
        now: i64,
    ) -> Result<AbuseReportResolve> {
        let conn = self.conn.lock().await;
        let Some(row) = abuse_report_by_id(&conn, id).context("resolve_abuse_report: read")? else {
            return Ok(AbuseReportResolve::NotFound);
        };
        match row.status.as_str() {
            "open" => {}
            "resolved" if row.outcome.as_deref() == Some(outcome) => {
                return Ok(AbuseReportResolve::AlreadyResolved);
            }
            _ => return Ok(AbuseReportResolve::Conflict),
        }
        conn.execute(
            "UPDATE abuse_reports
                SET status = 'resolved', outcome = ?2, resolved_at = ?3, resolved_by = ?4
              WHERE id = ?1 AND status = 'open'",
            rusqlite::params![id, outcome, now, resolved_by.as_slice()],
        )
        .context("resolve_abuse_report: update")?;
        let row = abuse_report_by_id(&conn, id)
            .context("resolve_abuse_report: read back")?
            .context("resolve_abuse_report: row vanished")?;
        Ok(AbuseReportResolve::Resolved(Box::new(row)))
    }

    /// The author of a post this nest stores or projects, if any — the
    /// report's `subject_actor` for a post subject.
    pub async fn abuse_report_post_author(&self, post_id: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let author: Option<Vec<u8>> = conn
            .query_row(
                "SELECT author FROM content WHERE id = ?1 LIMIT 1",
                rusqlite::params![post_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("abuse_report_post_author")?;
        Ok(author.and_then(|a| <[u8; 32]>::try_from(a).ok()))
    }

    /// The home nest URL this nest last saw one of `author`'s posts arrive
    /// from, if any — a foreign author's home for an actor or message report.
    pub async fn abuse_report_author_origin_url(
        &self,
        author: &[u8; 32],
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT origin_nest_url FROM content
              WHERE author = ?1 AND origin_nest_url IS NOT NULL AND origin_nest_url <> ''
              ORDER BY rowid DESC LIMIT 1",
            rusqlite::params![author.as_slice()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("abuse_report_author_origin_url")
    }

    /// Record a report forwarded from a peer nest. `Ok(None)` when this origin
    /// already delivered this `origin_report_ref` — a retry, which writes
    /// nothing. `now` is microseconds.
    pub async fn insert_forwarded_abuse_report(
        &self,
        report: ForwardedAbuseReport,
        now: i64,
    ) -> Result<Option<AbuseReportRow>> {
        let conn = self.conn.lock().await;
        let id = hex::encode(Uuid::new_v4().as_bytes());
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO abuse_reports
                    (id, created_at, origin_nest_id, origin_report_ref, subject_kind,
                     subject_id, subject_channel, subject_actor, reason, note, excerpt,
                     status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'open')",
                rusqlite::params![
                    id,
                    now,
                    report.origin_nest_id,
                    report.origin_report_ref,
                    report.subject_kind,
                    report.subject_id,
                    report.subject_channel,
                    report.subject_actor,
                    report.reason,
                    report.note,
                    report.excerpt,
                ],
            )
            .context("insert_forwarded_abuse_report")?;
        if inserted == 0 {
            return Ok(None);
        }
        abuse_report_by_id(&conn, &id).context("insert_forwarded_abuse_report: read back")
    }

    /// A peer withdrew a report it forwarded: the reporter's note and excerpt
    /// are deleted whatever the copy's state (their data), and an open copy
    /// leaves the queue. A resolved copy keeps its status — the audit record
    /// stands. Unknown refs are a no-op.
    pub async fn withdraw_forwarded_abuse_report(
        &self,
        origin_nest_id: &str,
        origin_report_ref: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE abuse_reports
                SET note = NULL, excerpt = NULL,
                    status = CASE status WHEN 'open' THEN 'withdrawn' ELSE status END
              WHERE origin_nest_id = ?1 AND origin_report_ref = ?2",
            rusqlite::params![origin_nest_id, origin_report_ref],
        )
        .context("withdraw_forwarded_abuse_report")?;
        Ok(())
    }

    /// The author's home nest returned the outcome of a report this nest
    /// forwarded to it. Accepted only from the nest the report was delivered
    /// to (`forwarded_nest_id`) and only while the report is open; returns the
    /// newly resolved row, or `None` when nothing changed. `now` is
    /// microseconds.
    pub async fn record_forwarded_abuse_outcome(
        &self,
        report_id: &str,
        from_nest_id: &str,
        outcome: &str,
        now: i64,
    ) -> Result<Option<AbuseReportRow>> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE abuse_reports
                    SET status = 'resolved', outcome = ?3, resolved_at = ?4
                  WHERE id = ?1 AND forwarded_nest_id = ?2 AND status = 'open'",
                rusqlite::params![report_id, from_nest_id, outcome, now],
            )
            .context("record_forwarded_abuse_outcome")?;
        if changed == 0 {
            return Ok(None);
        }
        abuse_report_by_id(&conn, report_id).context("record_forwarded_abuse_outcome: read back")
    }

    /// The author's home nest accepted a forwarded report: record where it
    /// went. If the reporter withdrew it while the delivery was in flight,
    /// the withdrawal is queued now (`withdraw_payload`), so a note that
    /// reached the peer never outlives the withdrawal. `now` is epoch seconds.
    pub async fn mark_abuse_report_forwarded(
        &self,
        report_id: &str,
        peer_url: &str,
        peer_nest_id: &str,
        withdraw_payload: &[u8],
        now: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE abuse_reports SET forwarded_to = ?2, forwarded_nest_id = ?3 WHERE id = ?1",
            rusqlite::params![report_id, peer_url, peer_nest_id],
        )
        .context("mark_abuse_report_forwarded")?;
        let withdrawn = abuse_report_by_id(&conn, report_id)
            .context("mark_abuse_report_forwarded: read")?
            .is_some_and(|row| row.status == "withdrawn");
        if withdrawn {
            enqueue_abuse_report_call_in(
                &conn,
                report_id,
                ABUSE_CALL_WITHDRAW,
                peer_url,
                Some(peer_nest_id),
                withdraw_payload,
                now,
            )?;
        }
        Ok(())
    }

    /// Queue one federation triad call. `now` is epoch seconds; the entry is
    /// due at once.
    pub async fn enqueue_abuse_report_call(
        &self,
        report_id: &str,
        kind: &str,
        peer_url: &str,
        peer_nest_id: Option<&str>,
        payload: &[u8],
        now: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        enqueue_abuse_report_call_in(&conn, report_id, kind, peer_url, peer_nest_id, payload, now)
    }

    /// A report's queued `deliver` is dropped before it ever reached the peer
    /// (the reporter withdrew it first). `true` when one was dropped.
    pub async fn cancel_abuse_report_deliver(&self, report_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM abuse_report_outbox WHERE report_id = ?1 AND kind = ?2",
                rusqlite::params![report_id, ABUSE_CALL_DELIVER],
            )
            .context("cancel_abuse_report_deliver")?;
        Ok(n > 0)
    }

    /// Calls due at `now` (epoch seconds), oldest first.
    pub async fn due_abuse_report_calls(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<AbuseReportOutboxEntry>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {ABUSE_OUTBOX_COLUMNS} FROM abuse_report_outbox
                  WHERE next_attempt_at <= ?1 ORDER BY next_attempt_at, id LIMIT ?2"
            ))
            .context("due_abuse_report_calls: prepare")?;
        let rows = stmt
            .query_map(rusqlite::params![now, limit], abuse_outbox_from_row)
            .context("due_abuse_report_calls: query")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("due_abuse_report_calls: read")?;
        Ok(rows)
    }

    /// The queued call of `kind` for `report_id`, if any.
    pub async fn abuse_report_call(
        &self,
        report_id: &str,
        kind: &str,
    ) -> Result<Option<AbuseReportOutboxEntry>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {ABUSE_OUTBOX_COLUMNS} FROM abuse_report_outbox
                  WHERE report_id = ?1 AND kind = ?2 ORDER BY id LIMIT 1"
            ),
            rusqlite::params![report_id, kind],
            abuse_outbox_from_row,
        )
        .optional()
        .context("abuse_report_call")
    }

    /// The call is done — answered, refused for good, or aged out.
    pub async fn finish_abuse_report_call(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM abuse_report_outbox WHERE id = ?1",
            rusqlite::params![id],
        )
        .context("finish_abuse_report_call")?;
        Ok(())
    }

    /// The call failed transiently: count the attempt and wait until
    /// `next_attempt_at` (epoch seconds).
    pub async fn defer_abuse_report_call(&self, id: i64, next_attempt_at: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE abuse_report_outbox
                SET attempts = attempts + 1, next_attempt_at = ?2
              WHERE id = ?1",
            rusqlite::params![id, next_attempt_at],
        )
        .context("defer_abuse_report_call")?;
        Ok(())
    }
}

/// `abuse_report_outbox.kind` for `fauna.federation.abuse_report.deliver`.
pub const ABUSE_CALL_DELIVER: &str = "deliver";
/// `abuse_report_outbox.kind` for `fauna.federation.abuse_report.withdraw`.
pub const ABUSE_CALL_WITHDRAW: &str = "withdraw";
/// `abuse_report_outbox.kind` for `fauna.federation.abuse_report.outcome`.
pub const ABUSE_CALL_OUTCOME: &str = "outcome";

const ABUSE_OUTBOX_COLUMNS: &str =
    "id, report_id, kind, peer_url, peer_nest_id, payload, attempts, created_at";

fn abuse_outbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AbuseReportOutboxEntry> {
    Ok(AbuseReportOutboxEntry {
        id: row.get(0)?,
        report_id: row.get(1)?,
        kind: row.get(2)?,
        peer_url: row.get(3)?,
        peer_nest_id: row.get(4)?,
        payload: row.get(5)?,
        attempts: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn enqueue_abuse_report_call_in(
    conn: &rusqlite::Connection,
    report_id: &str,
    kind: &str,
    peer_url: &str,
    peer_nest_id: Option<&str>,
    payload: &[u8],
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO abuse_report_outbox
            (report_id, kind, peer_url, peer_nest_id, payload, next_attempt_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        rusqlite::params![report_id, kind, peer_url, peer_nest_id, payload, now],
    )
    .context("enqueue_abuse_report_call")?;
    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod abuse_report_tests {
    use super::*;

    fn report(reporter: u8, subject: &str) -> NewAbuseReport {
        NewAbuseReport {
            reporter_actor: [reporter; 32],
            subject_kind: "post".into(),
            subject_id: subject.into(),
            subject_channel: None,
            subject_actor: Some(hex::encode([9u8; 32])),
            reason: "spam".into(),
            note: Some("note".into()),
            excerpt: Some("text".into()),
            block_author: false,
        }
    }

    fn inserted(r: AbuseReportInsert) -> AbuseReportRow {
        match r {
            AbuseReportInsert::Inserted(row) => row,
            other => panic!("expected Inserted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn one_open_report_per_reporter_and_subject() {
        let db = CacheDb::open_in_memory().unwrap();
        let first = inserted(
            db.insert_abuse_report(report(1, "a"), 20, 1_000)
                .await
                .unwrap(),
        );
        assert_eq!(first.status, "open");
        // A retry — or a second report on the same subject — writes nothing.
        match db
            .insert_abuse_report(report(1, "a"), 20, 2_000)
            .await
            .unwrap()
        {
            AbuseReportInsert::AlreadyOpen(row) => assert_eq!(row.id, first.id),
            other => panic!("expected AlreadyOpen, got {other:?}"),
        }
        // Another reporter on the same subject, and the same reporter on
        // another subject, each land.
        inserted(
            db.insert_abuse_report(report(2, "a"), 20, 3_000)
                .await
                .unwrap(),
        );
        inserted(
            db.insert_abuse_report(report(1, "b"), 20, 4_000)
                .await
                .unwrap(),
        );
        assert_eq!(db.list_open_abuse_reports().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn the_per_hour_cap_refuses_and_the_window_slides() {
        let db = CacheDb::open_in_memory().unwrap();
        for i in 0..3 {
            let r = report(1, &format!("s{i}"));
            inserted(db.insert_abuse_report(r, 3, 1_000 + i).await.unwrap());
        }
        assert_eq!(
            db.insert_abuse_report(report(1, "s3"), 3, 2_000)
                .await
                .unwrap(),
            AbuseReportInsert::RateLimited
        );
        // A different reporter is unaffected.
        inserted(
            db.insert_abuse_report(report(2, "s3"), 3, 2_000)
                .await
                .unwrap(),
        );
        // An hour on, the window has slid past the first three.
        let later = 1_000 + ABUSE_REPORT_RATE_WINDOW_MICROS + 10;
        inserted(
            db.insert_abuse_report(report(1, "s3"), 3, later)
                .await
                .unwrap(),
        );
    }

    #[tokio::test]
    async fn withdraw_deletes_note_and_excerpt_and_is_reporter_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = inserted(
            db.insert_abuse_report(report(1, "a"), 20, 1_000)
                .await
                .unwrap(),
        );
        // Another actor cannot withdraw it — and learns nothing.
        assert_eq!(
            db.withdraw_abuse_report(&[2; 32], &row.id).await.unwrap(),
            AbuseReportWithdraw::NotFound
        );
        match db.withdraw_abuse_report(&[1; 32], &row.id).await.unwrap() {
            AbuseReportWithdraw::Withdrawn(w) => {
                assert_eq!(w.status, "withdrawn");
                assert!(w.note.is_none());
                assert!(w.excerpt.is_none());
            }
            other => panic!("expected Withdrawn, got {other:?}"),
        }
        assert_eq!(
            db.withdraw_abuse_report(&[1; 32], &row.id).await.unwrap(),
            AbuseReportWithdraw::AlreadyWithdrawn
        );
        assert!(db.list_open_abuse_reports().await.unwrap().is_empty());
        // Withdrawn frees the subject for a fresh report.
        inserted(
            db.insert_abuse_report(report(1, "a"), 20, 2_000)
                .await
                .unwrap(),
        );
    }

    /// The account-deletion leg, through the door an account deletion
    /// actually runs (`purge_orphaned_actor_rows`): a deleted reporter's open
    /// reports are withdrawn as the living door would withdraw them — the
    /// words gone here, a queued delivery dropped, a landed one followed by a
    /// queued withdrawal — while their resolved reports stay whole, as does
    /// everyone else's queue (`moderation.md` § Where it lands, the deletion
    /// ruling; `abuse_reports`' `Policy::Partial` entry).
    #[tokio::test]
    async fn a_deleted_reporters_open_reports_are_withdrawn_and_the_rest_stay() {
        let db = CacheDb::open_in_memory().unwrap();
        let (gone, admin) = ([1u8; 32], [7u8; 32]);
        // Open, its delivery still queued (or in flight).
        let queued = inserted(
            db.insert_abuse_report(report(1, "queued"), 20, 1_000)
                .await
                .unwrap(),
        );
        db.enqueue_abuse_report_call(&queued.id, ABUSE_CALL_DELIVER, "https://h", None, b"d", 10)
            .await
            .unwrap();
        // Open, delivered to the home nest.
        let landed = inserted(
            db.insert_abuse_report(report(1, "landed"), 20, 2_000)
                .await
                .unwrap(),
        );
        db.mark_abuse_report_forwarded(&landed.id, "https://h", "ab", b"w", 11)
            .await
            .unwrap();
        // Resolved: the admin's record of what was decided, and on what.
        let resolved = inserted(
            db.insert_abuse_report(report(1, "resolved"), 20, 3_000)
                .await
                .unwrap(),
        );
        db.resolve_abuse_report(&resolved.id, "acted", &admin, 4_000)
            .await
            .unwrap();
        // Someone else's open report, delivered.
        let theirs = inserted(
            db.insert_abuse_report(report(2, "landed"), 20, 5_000)
                .await
                .unwrap(),
        );
        db.mark_abuse_report_forwarded(&theirs.id, "https://h", "ab", b"w", 12)
            .await
            .unwrap();

        db.purge_orphaned_actor_rows(&gone).await.unwrap();

        let queued = db.get_abuse_report(&queued.id).await.unwrap().unwrap();
        assert_eq!(queued.status, "withdrawn");
        assert!(queued.note.is_none() && queued.excerpt.is_none());
        assert!(
            db.abuse_report_call(&queued.id, ABUSE_CALL_DELIVER)
                .await
                .unwrap()
                .is_none(),
            "a delivery that never left is dropped"
        );
        assert!(
            db.abuse_report_call(&queued.id, ABUSE_CALL_WITHDRAW)
                .await
                .unwrap()
                .is_none(),
            "and needs no withdrawal"
        );

        let landed = db.get_abuse_report(&landed.id).await.unwrap().unwrap();
        assert_eq!(landed.status, "withdrawn");
        assert!(landed.note.is_none() && landed.excerpt.is_none());
        let follow = db
            .abuse_report_call(&landed.id, ABUSE_CALL_WITHDRAW)
            .await
            .unwrap()
            .expect("a landed delivery is followed by a queued withdrawal");
        assert_eq!(follow.peer_url, "https://h");
        assert_eq!(follow.peer_nest_id.as_deref(), Some("ab"), "pinned");
        assert_eq!(
            follow.payload,
            crate::abuse_report_federation::withdraw_payload(&landed.id).unwrap(),
            "the living door's own request"
        );

        let resolved = db.get_abuse_report(&resolved.id).await.unwrap().unwrap();
        assert_eq!(resolved.status, "resolved");
        assert_eq!(resolved.note.as_deref(), Some("note"));
        assert_eq!(resolved.excerpt.as_deref(), Some("text"));
        assert_eq!(
            resolved.reporter_actor.as_deref(),
            Some(gone.as_slice()),
            "the audit record names the retired id"
        );

        let theirs = db.get_abuse_report(&theirs.id).await.unwrap().unwrap();
        assert_eq!(theirs.status, "open");
        assert_eq!(theirs.note.as_deref(), Some("note"));
        assert!(
            db.abuse_report_call(&theirs.id, ABUSE_CALL_WITHDRAW)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(db.list_open_abuse_reports().await.unwrap().len(), 1);
        assert_eq!(
            db.list_abuse_reports_by_reporter(&gone)
                .await
                .unwrap()
                .len(),
            3,
            "the skeletons stay under the retired id"
        );

        // A retried deletion changes nothing more: one withdrawal queued.
        db.purge_orphaned_actor_rows(&gone).await.unwrap();
        assert_eq!(
            db.due_abuse_report_calls(i64::MAX, 10).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn resolve_is_a_record_that_stands() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = inserted(
            db.insert_abuse_report(report(1, "a"), 20, 1_000)
                .await
                .unwrap(),
        );
        match db
            .resolve_abuse_report(&row.id, "acted", &[7; 32], 5)
            .await
            .unwrap()
        {
            AbuseReportResolve::Resolved(r) => {
                assert_eq!(r.outcome.as_deref(), Some("acted"));
                assert_eq!(r.resolved_by.as_deref(), Some([7u8; 32].as_slice()));
                // The audit record keeps the evidence.
                assert_eq!(r.note.as_deref(), Some("note"));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
        assert_eq!(
            db.resolve_abuse_report(&row.id, "acted", &[7; 32], 6)
                .await
                .unwrap(),
            AbuseReportResolve::AlreadyResolved
        );
        assert_eq!(
            db.resolve_abuse_report(&row.id, "dismissed", &[7; 32], 6)
                .await
                .unwrap(),
            AbuseReportResolve::Conflict
        );
        // A resolved report is not withdrawable, and stays on the ledger.
        assert_eq!(
            db.withdraw_abuse_report(&[1; 32], &row.id).await.unwrap(),
            AbuseReportWithdraw::Resolved
        );
        assert_eq!(
            db.list_abuse_reports_by_reporter(&[1; 32])
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.resolve_abuse_report("nope", "acted", &[7; 32], 6)
                .await
                .unwrap(),
            AbuseReportResolve::NotFound
        );
    }

    fn forwarded(origin: &str, report_ref: &str) -> ForwardedAbuseReport {
        ForwardedAbuseReport {
            origin_nest_id: origin.into(),
            origin_report_ref: report_ref.into(),
            subject_kind: "post".into(),
            subject_id: "s".into(),
            subject_channel: None,
            subject_actor: None,
            reason: "hate".into(),
            note: Some("note".into()),
            excerpt: Some("text".into()),
        }
    }

    /// A delivery retry writes nothing; the same ref from another origin is a
    /// different report. A peer's withdrawal deletes the reporter's words even
    /// from a resolved copy, whose status (the audit record) stands.
    #[tokio::test]
    async fn forwarded_copies_dedupe_per_origin_and_withdraw_deletes_the_words() {
        let db = CacheDb::open_in_memory().unwrap();
        let copy = db
            .insert_forwarded_abuse_report(forwarded("aa", "r1"), 1)
            .await
            .unwrap()
            .expect("first delivery lands");
        assert!(copy.reporter_actor.is_none());
        assert!(
            db.insert_forwarded_abuse_report(forwarded("aa", "r1"), 2)
                .await
                .unwrap()
                .is_none(),
            "a retry is the report on record"
        );
        let other = db
            .insert_forwarded_abuse_report(forwarded("bb", "r1"), 3)
            .await
            .unwrap()
            .expect("another origin's ref is its own report");

        db.resolve_abuse_report(&other.id, "acted", &[7; 32], 4)
            .await
            .unwrap();
        db.withdraw_forwarded_abuse_report("aa", "r1")
            .await
            .unwrap();
        db.withdraw_forwarded_abuse_report("bb", "r1")
            .await
            .unwrap();
        let open = db.list_open_abuse_reports().await.unwrap();
        assert!(open.is_empty(), "the open copy left the queue");
        let conn = db.conn.lock().await;
        let a = abuse_report_by_id(&conn, &copy.id).unwrap().unwrap();
        assert_eq!(a.status, "withdrawn");
        assert_eq!((a.note, a.excerpt), (None, None));
        let b = abuse_report_by_id(&conn, &other.id).unwrap().unwrap();
        assert_eq!(b.status, "resolved");
        assert_eq!((b.note, b.excerpt), (None, None));
    }

    /// A withdrawal racing an in-flight delivery: whichever lands second
    /// still queues the withdrawal, so the note never outlives it on the peer.
    #[tokio::test]
    async fn a_withdrawal_during_delivery_is_queued_when_the_delivery_lands() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = inserted(db.insert_abuse_report(report(1, "a"), 20, 1).await.unwrap());
        db.enqueue_abuse_report_call(&row.id, ABUSE_CALL_DELIVER, "https://h", None, b"d", 10)
            .await
            .unwrap();
        // Withdrawn before the delivery is known to have landed: the queued
        // deliver is dropped (it may be in flight — the landing below covers it).
        db.withdraw_abuse_report(&[1; 32], &row.id).await.unwrap();
        assert!(db.cancel_abuse_report_deliver(&row.id).await.unwrap());
        assert!(db.due_abuse_report_calls(10, 10).await.unwrap().is_empty());
        // The in-flight delivery lands after all.
        db.mark_abuse_report_forwarded(&row.id, "https://h", "ab", b"w", 11)
            .await
            .unwrap();
        let due = db.due_abuse_report_calls(11, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, ABUSE_CALL_WITHDRAW);
        assert_eq!(due[0].peer_nest_id.as_deref(), Some("ab"));
        assert_eq!(due[0].payload, b"w");

        // A deferred call waits; a finished one is gone.
        db.defer_abuse_report_call(due[0].id, 50).await.unwrap();
        assert!(db.due_abuse_report_calls(49, 10).await.unwrap().is_empty());
        let later = db.due_abuse_report_calls(50, 10).await.unwrap();
        assert_eq!(later[0].attempts, 1);
        db.finish_abuse_report_call(later[0].id).await.unwrap();
        assert!(db.due_abuse_report_calls(99, 10).await.unwrap().is_empty());
    }

    /// An outcome is accepted only from the nest the report was delivered to,
    /// and only once.
    #[tokio::test]
    async fn a_forwarded_outcome_is_taken_only_from_the_home_it_went_to() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = inserted(db.insert_abuse_report(report(1, "a"), 20, 1).await.unwrap());
        assert!(
            db.record_forwarded_abuse_outcome(&row.id, "ab", "acted", 2)
                .await
                .unwrap()
                .is_none(),
            "never forwarded: no peer's outcome applies"
        );
        db.mark_abuse_report_forwarded(&row.id, "https://h", "ab", b"w", 3)
            .await
            .unwrap();
        assert!(db.due_abuse_report_calls(99, 10).await.unwrap().is_empty());
        assert!(
            db.record_forwarded_abuse_outcome(&row.id, "cd", "acted", 4)
                .await
                .unwrap()
                .is_none(),
            "another nest's word is ignored"
        );
        let resolved = db
            .record_forwarded_abuse_outcome(&row.id, "ab", "dismissed", 5)
            .await
            .unwrap()
            .expect("the home nest's outcome lands");
        assert_eq!(resolved.status, "resolved");
        assert_eq!(resolved.outcome.as_deref(), Some("dismissed"));
        assert!(resolved.resolved_by.is_none());
        assert!(
            db.record_forwarded_abuse_outcome(&row.id, "ab", "acted", 6)
                .await
                .unwrap()
                .is_none(),
            "a resolved record stands"
        );
    }
}

#[cfg(test)]
mod quarantine_access_tests {
    use super::*;
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::{canonical_decode, canonical_encode};
    use fauna_core::identity::ActorId;

    #[tokio::test]
    async fn quarantine_flag_and_author_lookup() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [1u8; 32];
        let author_id = [2u8; 32];

        // Build a minimal dag-cbor-encoded Post so put_post records the correct author
        let post = Post {
            author: ActorId(author_id),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "test".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let encoded = canonical_encode(&post).unwrap();

        // Store post (also creates content_meta row via write_post_index)
        db.put_post(&post_id, &encoded, None).await.unwrap();

        // Initially not quarantined
        assert!(!db.is_post_quarantined(&post_id).await.unwrap());

        // Quarantine it
        db.set_post_quarantined(&post_id, true).await.unwrap();
        assert!(db.is_post_quarantined(&post_id).await.unwrap());

        // Author lookup returns the correct author
        let author = db.get_content_author(&post_id).await.unwrap();
        assert_eq!(author, Some(author_id));

        // Un-quarantine
        db.set_post_quarantined(&post_id, false).await.unwrap();
        assert!(!db.is_post_quarantined(&post_id).await.unwrap());
    }

    /// `set_post_legal_takedown` / `get_post_legal_takedown` — the serve-time
    /// tombstone flag. NULL = live; a reference = taken down; clearing restores.
    /// Tombstone, not delete: the content row is never touched.
    #[tokio::test]
    async fn legal_takedown_flag_set_get_clear() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [7u8; 32];
        let author_id = [8u8; 32];
        let post = Post {
            author: ActorId(author_id),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "hi".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        db.put_post(&post_id, &canonical_encode(&post).unwrap(), None)
            .await
            .unwrap();

        // Live by default.
        assert_eq!(db.get_post_legal_takedown(&post_id).await.unwrap(), None);

        // Take down under a legal reference.
        db.set_post_legal_takedown(&post_id, Some("EU-DSA-2024/12345"))
            .await
            .unwrap();
        assert_eq!(
            db.get_post_legal_takedown(&post_id)
                .await
                .unwrap()
                .as_deref(),
            Some("EU-DSA-2024/12345")
        );
        // Tombstone, not delete: the content row still resolves.
        assert!(db.get_post(&post_id).await.unwrap().is_some());

        // Restore (overturned appeal) clears the flag; content unaffected.
        db.set_post_legal_takedown(&post_id, None).await.unwrap();
        assert_eq!(db.get_post_legal_takedown(&post_id).await.unwrap(), None);
        assert!(db.get_post(&post_id).await.unwrap().is_some());

        // An unknown post reads as not-taken-down (fail-open, no row).
        assert_eq!(db.get_post_legal_takedown(&[99u8; 32]).await.unwrap(), None);
    }

    /// `post_legal_takedown_txn` — flag + obligation + audit are one atomic
    /// transaction (review 2026-07-06 §§ F1/F4). Success writes all three;
    /// a flag write matching no `content_meta` row errors and commits NOTHING
    /// (no false "taken_down" audit/obligation row).
    #[tokio::test]
    async fn legal_takedown_txn_is_atomic() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [21u8; 32];
        let author_id = [22u8; 32];
        let content_id_hex = hex::encode(post_id);
        let admin = [23u8; 32];
        let post = Post {
            author: ActorId(author_id),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "hi".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        db.put_post(&post_id, &canonical_encode(&post).unwrap(), None)
            .await
            .unwrap();

        // Happy path: all three rows land together.
        db.post_legal_takedown_txn(
            &post_id,
            &content_id_hex,
            Some("EU-DSA-2024/12345"),
            &author_id,
            admin.as_slice(),
            "admin=… reference=EU-DSA-2024/12345",
            1_000_000,
        )
        .await
        .unwrap();
        assert_eq!(
            db.get_post_legal_takedown(&post_id)
                .await
                .unwrap()
                .as_deref(),
            Some("EU-DSA-2024/12345")
        );
        assert_eq!(
            db.get_obligation_actions("post", &content_id_hex)
                .await
                .unwrap()
                .len(),
            1
        );
        let audits = db.list_audit(10, None).await.unwrap();
        assert!(
            audits
                .iter()
                .any(|a| a.action == "moderation:legal-takedown"),
            "the takedown audit row commits with the flag"
        );

        // F1: a post with no content_meta row (raw/undecodable put_post branch
        // skips the meta co-write) → Err, and NOTHING commits.
        let orphan_id = [24u8; 32];
        let orphan_hex = hex::encode(orphan_id);
        db.put_post(&orphan_id, &[0xff, 0xff, 0xff], None)
            .await
            .unwrap();
        let n_audits_before = db.list_audit(50, None).await.unwrap().len();
        db.post_legal_takedown_txn(
            &orphan_id,
            &orphan_hex,
            Some("EU-DSA-2024/1"),
            &[0u8; 32],
            admin.as_slice(),
            "detail",
            1_000_000,
        )
        .await
        .expect_err("no content_meta row → the txn must fail, never a silent no-op");
        assert!(
            db.get_obligation_actions("post", &orphan_hex)
                .await
                .unwrap()
                .is_empty(),
            "rolled back: no obligation row for a takedown that withheld nothing"
        );
        assert_eq!(
            db.list_audit(50, None).await.unwrap().len(),
            n_audits_before,
            "rolled back: no audit row for a takedown that withheld nothing"
        );
    }

    /// `post_legal_takedown_of_deleted_post_txn` — the "deleted, then the
    /// order arrives" ordering (`moderation.md` § Legal takedown → *Posts*).
    /// No `content_meta` row exists to flag, so the three writes are the
    /// obligation row, the audit row, and `legal_takedown_deleted_posts` —
    /// all three or none.
    #[tokio::test]
    async fn legal_takedown_of_deleted_post_txn_writes_the_floor_not_the_flag() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [31u8; 32];
        let author_id = [32u8; 32];
        let content_id_hex = hex::encode(post_id);
        let admin = [33u8; 32];
        let digest = [0xABu8; 32];

        // No `put_post` at all — the content row genuinely does not exist,
        // matching the state after the author's own delete.
        db.post_legal_takedown_of_deleted_post_txn(
            &post_id,
            &content_id_hex,
            "EU-DSA-2026/716",
            &author_id,
            admin.as_slice(),
            &[digest],
            "admin=… author=… reference=EU-DSA-2026/716",
            1_000_000,
        )
        .await
        .unwrap();

        // No content_meta row was ever created, so there is nothing to flag
        // — the floor is what carries the fact instead.
        assert_eq!(db.get_post_legal_takedown(&post_id).await.unwrap(), None);

        let actions = db
            .get_obligation_actions("post", &content_id_hex)
            .await
            .unwrap();
        assert_eq!(actions.len(), 1, "the author still sees a takedown row");
        assert_eq!(
            actions[0].action_taken,
            fauna_core::obligation::ObligationAction::TakenDown as u8
        );

        let audits = db.list_audit(10, None).await.unwrap();
        assert!(
            audits
                .iter()
                .any(|a| a.action == "moderation:legal-takedown"),
            "the takedown audit row commits with the floor"
        );

        let (recorded_author, recorded_ref) = db
            .get_taken_down_deleted_post(&post_id)
            .await
            .unwrap()
            .expect("the floor row lands");
        assert_eq!(recorded_author, author_id);
        assert_eq!(recorded_ref, "EU-DSA-2026/716");
        assert_eq!(
            db.taken_down_deleted_post_blob_digests().await.unwrap(),
            vec![digest],
            "the digest reaches the blob door's withheld side"
        );
    }

    /// The takedown retraction (`moderation.md` § Legal takedown → the off-box
    /// publish surfaces): taking down an **already-projected** post writes the
    /// same `tombstone/post` witness `delete_post_core` writes, so the bridge's
    /// built `deleteRecord` path retracts the Bluesky record. Without it the
    /// predicate stops only *future* publication and a legally compelled
    /// removal is silently defeated exactly where it matters most.
    ///
    /// Also pins the ruled restore asymmetry: an overturn writes **no** witness
    /// (§ Legal takedown — an overturn is not a fresh publication consent).
    #[tokio::test]
    async fn legal_takedown_writes_the_atproto_retraction_witness() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [31u8; 32];
        let author_id = [32u8; 32];
        let admin = [33u8; 32];
        let content_id_hex = hex::encode(post_id);
        let post = Post {
            author: ActorId(author_id),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "already on bluesky".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        db.put_post(&post_id, &canonical_encode(&post).unwrap(), None)
            .await
            .unwrap();

        // Before: the projection stream serves the live post, no tombstone.
        let page = db
            .list_public_projection_page(&author_id, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(page.len(), 1, "the public post projects");
        assert!(!page[0].is_tombstone);

        db.post_legal_takedown_txn(
            &post_id,
            &content_id_hex,
            Some("EU-DSA-2024/9"),
            &author_id,
            admin.as_slice(),
            "admin=… reference=EU-DSA-2024/9",
            2_000_000,
        )
        .await
        .unwrap();

        // After: the post is withheld (the shared servability predicate) and a
        // retraction witness rides the stream at the TAKEDOWN instant, so a
        // bridge cursor long past the post's own row still sees the delete.
        let page = db
            .list_public_projection_page(&author_id, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(
            page.len(),
            1,
            "the post is withheld and replaced by its retraction witness"
        );
        assert!(
            page[0].is_tombstone,
            "the witness is the tombstone journal row"
        );
        assert_eq!(
            page[0].created_at, 2_000_000,
            "dated at the takedown instant, not the post's creation"
        );
        let ts: fauna_core::data::Tombstone =
            canonical_decode(&page[0].inline_payload).expect("witness decodes as a bare Tombstone");
        assert_eq!(
            ts.post_id.digest(),
            post_id,
            "the witness names the taken-down post, which is what the bridge \
             resolves against its PostId->AT-URI map"
        );
        assert_eq!(ts.author.0, author_id);

        // The ruled asymmetry: an overturn re-serves on Fauna but writes NO
        // witness, so the incremental stream never re-publishes. The post row
        // returns to the stream at its ORIGINAL position — behind any bridge
        // watermark — which is precisely why the overturn does not propagate.
        let witnesses_before = page.len();
        db.post_legal_takedown_txn(
            &post_id,
            &content_id_hex,
            None,
            &author_id,
            admin.as_slice(),
            "restore",
            3_000_000,
        )
        .await
        .unwrap();
        let page = db
            .list_public_projection_page(&author_id, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(
            page.iter().filter(|r| r.is_tombstone).count(),
            witnesses_before,
            "an overturn writes no new witness"
        );
        assert!(
            page.iter().any(|r| !r.is_tombstone),
            "the post itself re-serves on Fauna at its original stream position"
        );
        assert!(
            !page
                .iter()
                .any(|r| r.is_tombstone && r.created_at == 3_000_000),
            "no restore-instant row exists: nothing re-publishes incrementally"
        );
    }

    #[tokio::test]
    async fn admin_check_roundtrip() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = [3u8; 32];

        assert!(!db.is_admin(&actor_id).await.unwrap());
        db.add_admin_actor(&actor_id).await.unwrap();
        assert!(db.is_admin(&actor_id).await.unwrap());
    }

    /// `caller_may_read_content` — the canonical post read-authz gate reused by
    /// `get_post_core` and `fauna.moderation.train`. A
    /// non-quarantined post is world-readable; a quarantined post is readable
    /// only by its author or an admin; the anonymous reader never reads a
    /// quarantined post.
    #[tokio::test]
    async fn caller_may_read_content_quarantine_gate() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [10u8; 32];
        let author_id = [11u8; 32];
        let stranger_id = [12u8; 32];
        let admin_id = [13u8; 32];

        let post = Post {
            author: ActorId(author_id),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "hello".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        db.put_post(&post_id, &canonical_encode(&post).unwrap(), None)
            .await
            .unwrap();
        db.add_admin_actor(&admin_id).await.unwrap();

        // Not quarantined ⇒ world-readable, including the anonymous reader.
        for caller in [Some(&author_id), Some(&stranger_id), Some(&admin_id), None] {
            assert!(
                db.caller_may_read_content(caller, &post_id).await,
                "a non-quarantined post is readable by {caller:?}"
            );
        }

        // Quarantined ⇒ author + admin only; stranger and anonymous denied.
        db.set_post_quarantined(&post_id, true).await.unwrap();
        assert!(
            db.caller_may_read_content(Some(&author_id), &post_id).await,
            "the author may read their own quarantined post"
        );
        assert!(
            db.caller_may_read_content(Some(&admin_id), &post_id).await,
            "an admin may read a quarantined post"
        );
        assert!(
            !db.caller_may_read_content(Some(&stranger_id), &post_id)
                .await,
            "a stranger may NOT read a quarantined post"
        );
        assert!(
            !db.caller_may_read_content(None, &post_id).await,
            "the anonymous reader may NOT read a quarantined post"
        );

        // Un-quarantine ⇒ world-readable again.
        db.set_post_quarantined(&post_id, false).await.unwrap();
        assert!(
            db.caller_may_read_content(Some(&stranger_id), &post_id)
                .await,
            "un-quarantining restores world-readability"
        );

        // An unknown content id is treated as not-quarantined (world-readable);
        // train's downstream body-load is what reports the not-found.
        assert!(
            db.caller_may_read_content(Some(&stranger_id), &[99u8; 32])
                .await,
            "an unknown content id is not quarantined ⇒ gate passes (body-load reports absence)"
        );
    }

    /// `appeal_subject` — the appeal handle's gate (`moderation.md`
    /// § Legal takedown, the transparency triple). A **post** takedown is
    /// appealable through its obligation row AND through the `content_meta`
    /// flag, so a restore (which clears the flag but keeps the row as additive
    /// history) never revokes the appeal handle — and the handle is the
    /// AUTHOR's: the obligation row names them, so only they appeal.
    #[tokio::test]
    async fn a_taken_down_post_is_appealable_and_stays_so_after_a_restore() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [41u8; 32];
        let author_id = [42u8; 32];
        let content_id_hex = hex::encode(post_id);

        assert_eq!(
            db.appeal_subject(&content_id_hex).await.unwrap(),
            None,
            "nothing has been actioned yet — an appeal here is unfounded"
        );

        let post = Post {
            author: ActorId(author_id),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "hi".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        db.put_post(&post_id, &canonical_encode(&post).unwrap(), None)
            .await
            .unwrap();
        assert_eq!(
            db.appeal_subject(&content_id_hex).await.unwrap(),
            None,
            "a post merely existing is not an enforcement action"
        );

        db.post_legal_takedown_txn(
            &post_id,
            &content_id_hex,
            Some("EU-DSA-2026/741"),
            &author_id,
            [43u8; 32].as_slice(),
            "admin=… reference=EU-DSA-2026/741",
            2_000_000,
        )
        .await
        .unwrap();
        assert_eq!(
            db.appeal_subject(&content_id_hex).await.unwrap(),
            Some(AppealSubject::Author(author_id)),
            "the takedown's obligation row is the author's appeal handle"
        );

        // Overturn: the flag clears, the obligation row stays (additive
        // history, § Persistence) — so the appeal handle survives.
        db.set_post_legal_takedown(&post_id, None).await.unwrap();
        assert_eq!(
            db.appeal_subject(&content_id_hex).await.unwrap(),
            Some(AppealSubject::Author(author_id)),
            "an overturned takedown is still a decision that was made"
        );
    }

    /// The arm that would break if the gate read `obligation_action_records`
    /// alone: a **conversation** takedown writes NO obligation row — conv
    /// records persist no sender, so there is no author queue to key one to
    /// (`moderation.md` § Legal takedown → *Conversations*) — and its appeal
    /// handle is `segment_records.legal_takedown_ref`, which a member reaches
    /// by the record's own 32-byte hex id.
    #[tokio::test]
    async fn a_taken_down_conversation_record_is_appealable_with_no_obligation_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let record_id = [51u8; 32];
        let scope = [52u8; 32];
        let content_id_hex = hex::encode(record_id);
        let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(record_id);

        db.segment_records_insert_conv(&scope, 0, &record_cid, "b", 1_000, 1, &[], None)
            .await
            .unwrap();
        assert_eq!(
            db.appeal_subject(&content_id_hex).await.unwrap(),
            None,
            "an ordinary relayed message is not an enforcement action"
        );

        let n = db
            .set_conv_legal_takedown(&record_cid, Some("EU-DSA-2026/742"))
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            db.appeal_subject(&content_id_hex).await.unwrap(),
            Some(AppealSubject::Unattributed),
            "the conv triple's appeal handle — no obligation row exists for it, \
             and no sender to scope it to"
        );
        assert!(
            db.get_obligation_actions("conversation", &content_id_hex)
                .await
                .unwrap()
                .is_empty(),
            "and there is genuinely no obligation row to have gated on",
        );
    }

    /// The post-delete floor (`legal_takedown_deleted_posts`) is an appeal
    /// handle too — the author whose taken-down post is gone still appeals the
    /// takedown itself, and the floor row's `author_id` scopes it to them.
    #[tokio::test]
    async fn the_post_delete_floor_row_is_an_appeal_handle() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [61u8; 32];
        let author_id = [62u8; 32];
        let content_id_hex = hex::encode(post_id);

        db.record_taken_down_post_deleted(&post_id, &author_id, "EU-DSA-2026/743", &[], 3_000_000)
            .await
            .unwrap();
        assert_eq!(
            db.appeal_subject(&content_id_hex).await.unwrap(),
            Some(AppealSubject::Author(author_id)),
            "the compelled fact outlives the content it was compelled against"
        );
    }

    /// A `content_id` no enforcement ever touched is NOT appealable — including
    /// one that is not even a well-formed 32-byte hex id (no panic, no scan).
    #[tokio::test]
    async fn an_unactioned_content_id_is_not_appealable() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.appeal_subject(&hex::encode([71u8; 32])).await.unwrap(),
            None
        );
        assert_eq!(db.appeal_subject("not-hex-at-all").await.unwrap(), None);
        assert_eq!(db.appeal_subject("").await.unwrap(), None);
    }

    /// The `content_meta` flag arm names the author too — through the
    /// content row, the same lookup the takedown handler makes — so a flag
    /// with no obligation row beside it is still scoped to its author.
    #[tokio::test]
    async fn a_flag_without_an_obligation_row_is_scoped_to_the_posts_author() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [81u8; 32];
        let author_id = [82u8; 32];
        let post = Post {
            author: ActorId(author_id),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "hi".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        db.put_post(&post_id, &canonical_encode(&post).unwrap(), None)
            .await
            .unwrap();
        db.set_post_legal_takedown(&post_id, Some("EU-DSA-2026/744"))
            .await
            .unwrap();
        assert_eq!(
            db.appeal_subject(&hex::encode(post_id)).await.unwrap(),
            Some(AppealSubject::Author(author_id)),
        );
    }

    fn appeal_rows(conn: &rusqlite::Connection, target: &str) -> Vec<(Option<Vec<u8>>, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT actor_id, detail FROM audit_log
                  WHERE action = 'moderation:appeal' AND target = ?1 ORDER BY id",
            )
            .unwrap();
        stmt.query_map([target], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// `record_appeal` — at most ONE pending appeal per (appellant,
    /// content_id): a repeat collapses and writes nothing until a decision
    /// (a takedown or restore audit row for the content) lands after it. The
    /// appellant rides in `audit_log.actor_id` — the column the registry
    /// ruling reads as who performed the act.
    #[tokio::test]
    async fn a_second_pending_appeal_writes_no_second_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let content = hex::encode([91u8; 32]);
        let author = [92u8; 32];
        let other = [93u8; 32];

        assert_eq!(
            db.record_appeal(&content, &author, "first").await.unwrap(),
            AppealRecord::Recorded
        );
        assert_eq!(
            db.record_appeal(&content, &author, "again").await.unwrap(),
            AppealRecord::AlreadyPending,
            "a repeat before any decision collapses onto the pending appeal"
        );
        {
            let conn = db.conn.lock().await;
            let rows = appeal_rows(&conn, &content);
            assert_eq!(rows.len(), 1, "the collapsed repeat wrote no row");
            assert_eq!(rows[0].0.as_deref(), Some(author.as_slice()));
            assert_eq!(rows[0].1, "reason=first");
        }

        // Another appellant (a conversation's other member) is their own key.
        assert_eq!(
            db.record_appeal(&content, &other, "mine").await.unwrap(),
            AppealRecord::Recorded
        );

        // A decision recorded after the appeal re-opens the handle.
        db.audit(
            None,
            "moderation:legal-takedown-restore",
            Some(&content),
            Some("x"),
        )
        .await
        .unwrap();
        assert_eq!(
            db.record_appeal(&content, &author, "after the overturn")
                .await
                .unwrap(),
            AppealRecord::Recorded
        );
        let conn = db.conn.lock().await;
        assert_eq!(appeal_rows(&conn, &content).len(), 3);
    }
}

#[cfg(test)]
mod spam_history_tests {
    use super::*;

    fn insert<'a>(message_id: &'a [u8; 32], label: &'a str) -> SpamHistoryDbOp<'a> {
        SpamHistoryDbOp::Insert {
            message_id,
            mailbox: "Junk",
            sealed_subject: &[0x22u8; 16],
            sealed_delta: &[0x33u8; 16],
            label,
            source: "imap_junk_flag",
        }
    }

    /// A history `Delete` removes the caller's own row in the model write's
    /// transaction and hands back the lesson it removed (the report capture's
    /// key); a second delete of the same id finds nothing and is a no-op, so
    /// a replayed undo withdraws nothing twice.
    #[tokio::test]
    async fn a_history_delete_returns_the_lesson_it_removed_once() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let msg = [0x11u8; 32];
        let SpamModelWrite::Written {
            history_id: Some(hid),
            undone: None,
        } = db
            .put_spam_model_with_history(&actor, &[0xEEu8; 300], Some(insert(&msg, "spam")), None)
            .await
            .unwrap()
        else {
            panic!("insert returns a history id");
        };
        let first = db
            .put_spam_model_with_history(
                &actor,
                &[0xEFu8; 300],
                Some(SpamHistoryDbOp::Delete { history_id: &hid }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            first,
            SpamModelWrite::Written {
                history_id: None,
                undone: Some(UndoneLesson {
                    message_id: msg.to_vec(),
                    label: "spam".into(),
                }),
            }
        );
        let second = db
            .put_spam_model_with_history(
                &actor,
                &[0xEFu8; 300],
                Some(SpamHistoryDbOp::Delete { history_id: &hid }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            second,
            SpamModelWrite::Written {
                history_id: None,
                undone: None,
            },
            "the row was already gone"
        );
        assert!(
            db.list_spam_training_history(&actor, 10, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Another actor's history id matches nothing — no cross-actor undo.
    #[tokio::test]
    async fn a_history_delete_is_caller_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        let (alice, bob) = ([1u8; 32], [2u8; 32]);
        let SpamModelWrite::Written {
            history_id: Some(hid),
            ..
        } = db
            .put_spam_model_with_history(
                &alice,
                &[0xEEu8; 300],
                Some(insert(&[3u8; 32], "ham")),
                None,
            )
            .await
            .unwrap()
        else {
            panic!("insert returns a history id");
        };
        let by_bob = db
            .put_spam_model_with_history(
                &bob,
                &[0xEEu8; 300],
                Some(SpamHistoryDbOp::Delete { history_id: &hid }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            by_bob,
            SpamModelWrite::Written {
                history_id: None,
                undone: None,
            }
        );
        assert_eq!(
            db.list_spam_training_history(&alice, 10, None)
                .await
                .unwrap()
                .len(),
            1,
            "alice's row survives bob's delete"
        );
    }
}
