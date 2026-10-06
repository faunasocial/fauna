//! Routing tables the I2b bridge surface needs in addition to the
//! wrapped-blob storage from slice 1. Currently:
//!   * actor_mls_pubkeys / actor_index_pubkeys — per-actor public
//!     encryption keys for `fauna.bridges.fetch_recipient_mls_pubkey`
//!     and `fauna.bridges.fetch_recipient_index_key`. Production
//!     provisioning RPCs are out of scope for this module.
//!   * Submission quota counters (B.1) and inbound-mail insert paths
//!     (B.2/B.3).
//!
//! Recipient routing previously lived here as `recipient_routes`
//! (`put_recipient_route` / `get_recipient_route` / `delete_recipient_route`).
//! That table was retired: production
//! `fauna.bridges.validate_recipient` now resolves through
//! `account_aliases` (`db::mail_aliases::lookup_exact_alias`), the
//! production source of truth per
//! `docs/goal/behavior/mail-aliases.md` § Storage.

use anyhow::{Context, Result};
use fauna_mail::segments::MailFloorMetadata;
use fauna_mls::wrapped_blob::SealedRecordBytes;
use rusqlite::OptionalExtension;

use super::{CacheDb, blob_col_to_array, blob_to_array, now_epoch_millis, now_epoch_secs};
use fauna_segment_store::SegmentManager;

/// Per-actor row cap for `actor_epoch_seal_keys`:
/// every write path prunes the OLDEST epochs past this count, so the table
/// is bounded (~64 × 1216 B ≈ 78 KB per actor) no matter how many provisions
/// an actor issues. 64 = the per-provision wire cap, comfortably above the
/// honest steady state (the 27-entry publish horizon plus refresh drift);
/// prune-oldest is safe because selection only ever reads the newest row
/// ≤ e_now (design § 3 — long-past rows are "prunable but harmless").
pub(crate) const MAX_ACTOR_EPOCH_SEAL_KEY_ROWS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionQuotaOutcome {
    Allowed,
    OverQuota { remaining: u32 },
}

/// The recipient's standing seal key halves, resolved through
/// [`CacheDb::get_recipient_seal_key`] — the single Phase-3 D2 seam every
/// seal site keys on. Both halves are always on file (the columns are
/// `NOT NULL`), so a seal to this key selects the X-Wing hybrid suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientSealKey {
    pub mls_pubkey: [u8; 32],
    pub mlkem_ek: Vec<u8>,
}

/// Result of `insert_inbound_mail` / `insert_appended_mail`. Plan 5 T6
/// added `finalized` so the calling handler can emit a
/// `fauna.segments.changed { Finalized }` push when this append closed
/// a previously-open segment via rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailInsertOutcome {
    /// The 32-byte digest of the record's content-hash filing CID
    /// (`message-segment-store.md` § Record identity per kind) — the opaque
    /// carried key every aux table and wire surface uses for this message.
    pub message_id: [u8; 32],
    /// `true` if the record was newly written; `false` on idempotent
    /// retry (the `segment_records` row already existed).
    pub inserted: bool,
    /// `Some(closed_seg_id)` if this append rotated a previously-open
    /// segment closed (bucket boundary). Always `None` on idempotent
    /// retries — `inserted=false` implies no segment-store write.
    pub finalized: Option<u32>,
    /// The **nest's own** receipt instant for this append (epoch millis) —
    /// the single `now_epoch_millis()` reading that also became the record's
    /// `MailFloorMetadata::received_at`.
    ///
    /// Returned so the caller can stamp the same instant into the surfaces
    /// that must agree with the floor — `bridge_imap_messages.internal_date`
    /// and the `MailPlacementRecord::Append` journal entry above all
    /// (`imap-server.md` § SEARCH → *INTERNALDATE is the nest's own receipt
    /// time*). Handing it back rather than letting the handler take its own
    /// clock reading is what makes the floor's promise — "the two are the
    /// same at the origin nest" (`MailFloorMetadata::stored_at`) — true by
    /// construction instead of approximately true.
    ///
    /// Meaningful only when `inserted`: on an idempotent byte-replay this is
    /// the instant of the *retry*, and the stored record keeps the original.
    /// Every caller already gates its placement on `inserted`, so the replay
    /// value is never consumed.
    pub received_at: i64,
}

/// Inputs to `insert_inbound_mail`. The handler maps wire-protocol
/// `IngestInboundMailRequest` plus its `SpamDisposition` enum into
/// this shape (string-typed so the table column is plain TEXT).
#[derive(Debug, Clone)]
pub struct InboundMailFields {
    pub actor_id: [u8; 32],
    pub timestamp: i64,
    pub ciphertext_size: u32,
    /// Proven-sealed (S6.12b): builders mint via `SealedRecordBytes::verify`
    /// at the wire edge, or wrap their own `seal_recipient_blob` output — so
    /// a raw body cannot be spelled into the `__mail` segment store.
    pub encrypted_body: SealedRecordBytes,
    /// Same gate as the body — the hint is content-derived token bytes.
    pub encrypted_index_hint: SealedRecordBytes,
    pub sender_domain: String,
    pub spf: String,
    pub dkim: String,
    pub dmarc: String,
    pub dmarc_policy: String,
    pub arc: String,
    pub spam_score: u32,
    pub spam_disposition: String,
    pub is_own_submission: bool,
    /// The uniform scoring-metadata bus rows for this item (wire-supplied or
    /// derived from the legacy fields by the handler; empty for an
    /// own-submission Sent copy, which nothing scored). Mirrored into the
    /// segment footer (`MailFloorMetadata.scores` — authoritative, so recovery
    /// can rebuild the SQL bus) and the `content_scores` table (hot-path read).
    pub scores: Vec<fauna_core::scoring::ScoreEntry>,
    /// Canonical 32-byte report-hash from the perimeter (`report-sharing.md`
    /// § Content identity), empty = absent (nest-side writers such as IMAP APPEND and
    /// import have no perimeter hash — those messages cannot aggregate). Mirrored into the segment footer
    /// (`MailFloorMetadata.report_hash` — authoritative) and the
    /// `segment_records.report_hash` column (hot-path lookup by message id).
    pub report_hash: Vec<u8>,
}

/// A row for `message_scan_results` (T1.4 content scanning). Admin-visible
/// data, plaintext (verdicts aren't user content, so they sit outside the
/// sealed floor); holds only the verdict, never the scanned body. The handler
/// maps `IngestInboundMailRequest`'s scan-verdict fields
/// into this shape (rspamd scores as milli-ints; flagged-rules / breakdown as
/// JSON strings). See `docs/goal/behavior/mail-content-scanning.md`.
#[derive(Debug, Clone)]
pub struct ScanResultRow {
    pub message_id: [u8; 32],
    pub received_at: i64,
    pub scanned_at: i64,
    /// `clean` / `infected` / `error` / `bypassed_oversize` / `not_scanned`
    /// (the last only beside an rspamd score: a message no scanner touched
    /// gets no row — `mail-content-scanning.md` § Per-message scan-result
    /// storage).
    pub clamav_verdict: String,
    /// Non-null only when `clamav_verdict = 'infected'`.
    pub clamav_signature: Option<String>,
    /// rspamd native score × 1000 (`None` when rspamd disabled).
    pub rspamd_score_raw: Option<i64>,
    /// Scaled score × 1000 (`None` when rspamd disabled).
    pub rspamd_score_scaled: Option<i64>,
    /// JSON array of fired rule names.
    pub rspamd_flagged_rules: Option<String>,
    /// JSON object `{rule: milli_score}`.
    pub rspamd_score_breakdown: Option<String>,
    /// `delivered` / `rejected_malware` / `junked` / `tagged`.
    pub action_taken: String,
    /// Recipient actor; `None` for reject-at-perimeter forensic rows.
    pub delivered_to_actor: Option<[u8; 32]>,
}

impl CacheDb {
    /// Upsert both halves of an actor's standing recipient seal key in one
    /// write: the 32-byte X25519 `mls_pubkey` and the ML-KEM-768
    /// encapsulation key (`mlkem_ek`, S3c) the MTA pairs with it to build the
    /// recipient's X-Wing public key. The provision door's writer — the only
    /// writer of `actor_mls_pubkeys` — and the column is `NOT NULL`, so a
    /// recipient row never rests without its post-quantum half.
    pub async fn put_actor_recipient_seal_key(
        &self,
        actor_id: &[u8; 32],
        pubkey: &[u8; 32],
        mlkem_ek: &[u8],
    ) -> Result<()> {
        let actor = *actor_id;
        let pubkey = *pubkey;
        let mlkem_ek = mlkem_ek.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO actor_mls_pubkeys (actor_id, mls_pubkey, mlkem_ek, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(actor_id) DO UPDATE SET
                mls_pubkey = excluded.mls_pubkey,
                mlkem_ek = excluded.mlkem_ek,
                updated_at = excluded.updated_at",
            rusqlite::params![&actor[..], &pubkey[..], &mlkem_ek[..], now],
        )
        .context("put actor recipient seal key")?;
        Ok(())
    }

    pub async fn get_actor_mls_pubkey(&self, actor_id: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT mls_pubkey FROM actor_mls_pubkeys WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| {
                    let v: Vec<u8> = row.get(0)?;
                    Ok(v)
                },
            )
            .ok();
        match row {
            None => Ok(None),
            Some(v) => {
                let pk: [u8; 32] = blob_to_array(v.as_slice(), "mls_pubkey")?;
                Ok(Some(pk))
            }
        }
    }

    /// Resolve the recipient's standing seal key — **the ONE seal-pubkey
    /// resolution seam (Phase-3 D2; design tracked internally)**.
    /// Every nest-side seal site (`seal_and_persist_local`, the spam-model
    /// seal-on-read) and the `fetch_recipient_mls_pubkey`
    /// RPC feeding the Go bridge's seal sites resolve through here, so when
    /// content-sealing epochs land this single function becomes
    /// epoch-indexed (Slice 0 § 3/B3)
    /// with no seal-site rework. One row read returns both halves: the
    /// 32-byte X25519 MSEK-derived pubkey and the ML-KEM-768 ek that pairs
    /// with it into the recipient's X-Wing public key.
    pub async fn get_recipient_seal_key(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<RecipientSealKey>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT mls_pubkey, mlkem_ek FROM actor_mls_pubkeys WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .context("get recipient seal key")?;
        match row {
            None => Ok(None),
            Some((pk, mlkem_ek)) => {
                let mls_pubkey: [u8; 32] = blob_to_array(pk.as_slice(), "mls_pubkey")?;
                Ok(Some(RecipientSealKey {
                    mls_pubkey,
                    mlkem_ek,
                }))
            }
        }
    }

    /// Upsert a batch of per-epoch mail sealing public keys for `actor_id`
    /// (`actor_epoch_seal_keys`, v27) — the owner-published schedule the
    /// content-sealing-epochs design § 3 has the MTA seal against once the
    /// write flip lands. Idempotent per `(actor, epoch)`: re-publication
    /// replaces the row (the keys are deterministic in `(MSEK, e)`, so a
    /// replace is byte-identical unless the MSEK rotated — in which case
    /// replacing is exactly right: the new lineage supersedes the old
    /// schedule's future epochs).
    pub async fn put_actor_epoch_seal_keys(
        &self,
        actor_id: &[u8; 32],
        keys: &[(u64, [u8; 32], Vec<u8>)],
    ) -> Result<()> {
        let actor = *actor_id;
        let keys = keys.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        for (epoch, pubkey, mlkem_ek) in &keys {
            conn.execute(
                "INSERT INTO actor_epoch_seal_keys
                    (actor_id, epoch, mls_pubkey, mlkem_ek, published_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(actor_id, epoch) DO UPDATE SET
                    mls_pubkey = excluded.mls_pubkey,
                    mlkem_ek = excluded.mlkem_ek,
                    published_at = excluded.published_at",
                rusqlite::params![
                    &actor[..],
                    i64::try_from(*epoch).unwrap_or(i64::MAX),
                    &pubkey[..],
                    &mlkem_ek[..],
                    now
                ],
            )
            .context("put actor epoch seal key")?;
        }
        // Per-actor bound: drop the OLDEST epochs past
        // [`MAX_ACTOR_EPOCH_SEAL_KEY_ROWS`]. Keeping the newest rows
        // preserves everything selection can ever use — the § 3 read picks
        // the newest row ≤ e_now, so a pruned row was shadowed by a newer
        // one or decades stale; the honest 27-row horizon never comes near
        // the cap.
        conn.execute(
            "DELETE FROM actor_epoch_seal_keys
              WHERE actor_id = ?1
                AND epoch NOT IN (
                    SELECT epoch FROM actor_epoch_seal_keys
                     WHERE actor_id = ?1
                     ORDER BY epoch DESC
                     LIMIT ?2)",
            rusqlite::params![&actor[..], MAX_ACTOR_EPOCH_SEAL_KEY_ROWS as i64],
        )
        .context("prune actor epoch seal keys")?;
        Ok(())
    }

    /// Test-only row count for `actor_epoch_seal_keys` — asserts the
    /// per-actor cap (`MAX_ACTOR_EPOCH_SEAL_KEY_ROWS`) actually holds.
    #[cfg(test)]
    pub(crate) async fn count_actor_epoch_seal_keys(&self, actor_id: &[u8; 32]) -> Result<i64> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let n = conn.query_row(
            "SELECT COUNT(*) FROM actor_epoch_seal_keys WHERE actor_id = ?1",
            rusqlite::params![&actor[..]],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(n)
    }

    /// The newest published epoch seal key at or before sealing epoch
    /// `e_now` — the § 3 selection steps 1–2 in one query: the row for
    /// `e_now` itself when the schedule is current, else the newest earlier
    /// row (the never-bounce degradation: the client is offline past its
    /// published horizon, mail keeps flowing sealed under the stale epoch
    /// key, expiry is temporarily coarser). Returns `(epoch, key)` so the
    /// caller can log staleness when `epoch < e_now`. `None` = no schedule
    /// published → the caller falls through to the
    /// standing `get_recipient_seal_key` (step 3, today's behavior).
    pub async fn get_actor_epoch_seal_key(
        &self,
        actor_id: &[u8; 32],
        e_now: u64,
    ) -> Result<Option<(u64, RecipientSealKey)>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT epoch, mls_pubkey, mlkem_ek FROM actor_epoch_seal_keys
                 WHERE actor_id = ?1 AND epoch <= ?2
                 ORDER BY epoch DESC LIMIT 1",
                rusqlite::params![&actor[..], i64::try_from(e_now).unwrap_or(i64::MAX)],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()
            .context("get actor epoch seal key")?;
        match row {
            None => Ok(None),
            Some((epoch, pk, mlkem_ek)) => {
                let mls_pubkey: [u8; 32] = blob_to_array(pk.as_slice(), "epoch mls_pubkey")?;
                Ok(Some((
                    u64::try_from(epoch).unwrap_or(0),
                    RecipientSealKey {
                        mls_pubkey,
                        mlkem_ek,
                    },
                )))
            }
        }
    }

    /// The content-sealing-epochs § 3 three-step selection for a **genuine
    /// new-mail-ingest** seal site: current epoch → newest published earlier
    /// (never-bounce degradation) → the standing key (§ 3 steps 1–2 via
    /// [`Self::get_actor_epoch_seal_key`]; step 3 via [`Self::get_recipient_seal_key`]).
    ///
    /// `epoch_sealing_enabled` — pass `MAIL_EPOCH_SEALING_WRITE_DEFAULT`
    /// (`fauna_mls::wrapped_blob`; `true` since the 2026-07-19 flip) at
    /// production call sites; `false` behaves byte-identically to the
    /// pre-flip world (ignores any published schedule). Taking it as a
    /// parameter rather than reading the constant directly keeps both arms
    /// independently testable.
    ///
    /// **Callers: mail new-ingest ONLY** — `seal_and_persist_local` (in-domain
    /// delivery), `fetch_recipient_mls_pubkey_handler` (feeds the Go MTA's
    /// per-delivery seal), and the mailbox-migration importer. [`Self::get_recipient_seal_key`]
    /// is shared by several NON-mail-new-ingest seal sites too — nostr DM
    /// self-seal and the spam-model self-seal — none of which know how to open an
    /// epoch-sealed record, so they must keep calling
    /// [`Self::get_recipient_seal_key`] directly, never this wrapper.
    pub async fn get_recipient_mail_seal_key(
        &self,
        actor_id: &[u8; 32],
        epoch_sealing_enabled: bool,
    ) -> Result<Option<RecipientSealKey>> {
        if !epoch_sealing_enabled {
            return self.get_recipient_seal_key(actor_id).await;
        }
        let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(now_epoch_secs() as u64);
        match self.get_actor_epoch_seal_key(actor_id, e_now).await? {
            Some((epoch, key)) => {
                if epoch < e_now {
                    tracing::warn!(
                        actor = %hex::encode(actor_id),
                        stale_epoch = epoch,
                        e_now,
                        "epoch schedule stale for actor: sealing under an earlier published epoch"
                    );
                }
                Ok(Some(key))
            }
            None => self.get_recipient_seal_key(actor_id).await,
        }
    }

    /// Upsert the per-actor index public encryption key. Sibling of
    /// `put_actor_recipient_seal_key` — the MTA bridge encrypts the
    /// canonical-token-set index hint to this key, while the body
    /// encrypts to the MLS pubkey, so a future deployment can grant the
    /// index-builder access to one without the other. Provisioning by
    /// the recipient's client is a Phase E concern.
    pub async fn put_actor_index_pubkey(
        &self,
        actor_id: &[u8; 32],
        pubkey: &[u8; 32],
    ) -> Result<()> {
        let actor = *actor_id;
        let pubkey = *pubkey;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO actor_index_pubkeys (actor_id, index_pubkey, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET
                index_pubkey = excluded.index_pubkey,
                updated_at = excluded.updated_at",
            rusqlite::params![&actor[..], &pubkey[..], now],
        )
        .context("put actor index pubkey")?;
        Ok(())
    }

    pub async fn get_actor_index_pubkey(&self, actor_id: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT index_pubkey FROM actor_index_pubkeys WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| {
                    let v: Vec<u8> = row.get(0)?;
                    Ok(v)
                },
            )
            .ok();
        match row {
            None => Ok(None),
            Some(v) => {
                let pk: [u8; 32] = blob_to_array(v.as_slice(), "index_pubkey")?;
                Ok(Some(pk))
            }
        }
    }

    /// Insert an encrypted inbound-mail record. Returns a
    /// [`MailInsertOutcome`] carrying the derived `message_id`, an
    /// `inserted` flag (`false` = idempotent retry — the
    /// `segment_records` row already exists for this message_id), and
    /// `finalized: Option<u32>` — Plan 5 T6's hook for the
    /// `fauna.segments.changed { Finalized }` push when this append
    /// rotated a previously-open segment closed.
    ///
    /// Writes route through `segments::mail::append_record`: the
    /// `MailRecordEnvelope` is appended to the actor's open segment,
    /// `MailFloorMetadata` lands in the segment footer, the
    /// `segment_records` mirror row is inserted, and the manifest is
    /// saved atomically — all under the manager's per-actor lock.
    pub async fn insert_inbound_mail(
        &self,
        manager: &SegmentManager,
        f: &InboundMailFields,
    ) -> Result<MailInsertOutcome> {
        let now_millis = now_epoch_millis();
        self.append_via_manager(manager, f, now_millis).await
    }

    /// Insert an encrypted mail record for an APPEND upload (user-generated
    /// message).
    ///
    /// Identical to `insert_inbound_mail` — the message_id is the content
    /// hash of the stored envelope bytes on both paths (`message-segment-
    /// store.md` § Record identity per kind). The retired per-path domain
    /// tags kept "identical bytes on different paths" from colliding; under
    /// content-hash identity such a collision is a literal byte replay of one
    /// seal, which the scoped dedup inside `append_record` absorbs (mail
    /// seals are HPKE-ephemeral-fresh, so independent messages never share
    /// bytes).
    pub async fn insert_appended_mail(
        &self,
        manager: &SegmentManager,
        f: &InboundMailFields,
    ) -> Result<MailInsertOutcome> {
        let now_millis = now_epoch_millis();
        self.append_via_manager(manager, f, now_millis).await
    }

    /// Shared body for the two write paths: builds the `MailFloorMetadata`
    /// from `InboundMailFields` and invokes `segments::mail::append_record`,
    /// which derives the record's content-hash identity and runs the scoped
    /// idempotency check under its own seq lock (`inserted: false` on a
    /// byte-replay hit).
    async fn append_via_manager(
        &self,
        manager: &SegmentManager,
        f: &InboundMailFields,
        received_at_millis: i64,
    ) -> Result<MailInsertOutcome> {
        let floor = MailFloorMetadata {
            format_version: fauna_mail::segments::MAIL_FLOOR_FORMAT_VERSION,
            received_at: received_at_millis,
            timestamp: f.timestamp,
            ciphertext_size: f.ciphertext_size,
            sender_domain: f.sender_domain.clone(),
            spam_disposition: f.spam_disposition.clone(),
            is_own_submission: f.is_own_submission,
            spf: f.spf.clone(),
            dkim: f.dkim.clone(),
            dmarc: f.dmarc.clone(),
            dmarc_policy: f.dmarc_policy.clone(),
            arc: f.arc.clone(),
            spam_score: f.spam_score,
            // Placeholder — `append_record` allocates the per-actor monotonic
            // seq under its lock and overwrites this before encoding the floor.
            seq: 0,
            // Placeholder likewise — `append_record` stamps the local storage
            // time at the append itself (0 would mean "unknown" if it leaked).
            stored_at: 0,
            // Field-for-field into the floor's mirror type (the segments codec
            // can't dep fauna-core; see `FloorScoreEntry`).
            scores: f
                .scores
                .iter()
                .map(|e| fauna_mail::segments::FloorScoreEntry {
                    factor: e.factor.clone(),
                    score: e.score,
                    tier: e.tier,
                    scorer_version: e.scorer_version,
                })
                .collect(),
            report_hash: f.report_hash.clone(),
            // An ordinary inbound record. The continuation writer stamps
            // PART/HEAD roles on its own records (message-segment-store.md
            // § Continuation records); every leg through here is a normal
            // single-record message.
            continuation_role: fauna_mail::segments::CONTINUATION_ROLE_NORMAL,
        };
        let outcome = crate::segments::mail::append_record(
            manager,
            self,
            &f.actor_id,
            &f.encrypted_body,
            &f.encrypted_index_hint,
            floor,
        )
        .await
        .context("append mail record to segment store")?;
        Ok(MailInsertOutcome {
            message_id: outcome.cid.digest(),
            inserted: outcome.inserted,
            finalized: outcome.finalized,
            received_at: received_at_millis,
        })
    }

    /// Atomically read+update today's submission counter for `actor_id`.
    /// Returns `Allowed` and increments by `recipient_count` when the
    /// resulting total stays within `daily_limit`; otherwise returns
    /// `OverQuota { remaining }` (= headroom *before* the call) and
    /// leaves the counter unchanged so a smaller follow-up retry can
    /// still consume the leftover.
    pub async fn try_consume_submission_quota(
        &self,
        actor_id: &[u8; 32],
        day_bucket: i64,
        recipient_count: u32,
        daily_limit: u32,
    ) -> Result<SubmissionQuotaOutcome> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let used: u32 = conn
            .query_row(
                "SELECT used FROM bridge_submission_quota
                 WHERE actor_id = ?1 AND day_bucket = ?2",
                rusqlite::params![&actor[..], day_bucket],
                |row| row.get::<_, i64>(0).map(|v| v.max(0) as u32),
            )
            .optional()
            .context("read submission quota")?
            .unwrap_or(0);
        let new_used = used.saturating_add(recipient_count);
        if new_used > daily_limit {
            return Ok(SubmissionQuotaOutcome::OverQuota {
                remaining: daily_limit.saturating_sub(used),
            });
        }
        conn.execute(
            "INSERT INTO bridge_submission_quota (actor_id, day_bucket, used)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id, day_bucket) DO UPDATE SET
                used = excluded.used",
            rusqlite::params![&actor[..], day_bucket, new_used as i64],
        )
        .context("upsert submission quota")?;
        Ok(SubmissionQuotaOutcome::Allowed)
    }

    /// Insert (or idempotently replace, on a deterministic-message_id retry)
    /// the per-message content-scan result. Metadata only — the scanned body
    /// never reaches the nest (`content-scoring.md`). Called from
    /// `persist_inbound_mail_request` after the segment-store append.
    pub async fn insert_scan_result(&self, row: &ScanResultRow) -> Result<()> {
        let actor_bytes = row.delivered_to_actor.map(|a| a.to_vec());
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO message_scan_results (
                 message_id, received_at, direction, clamav_verdict, clamav_signature,
                 rspamd_score_raw, rspamd_score_scaled, rspamd_flagged_rules,
                 rspamd_score_breakdown, scanned_at, action_taken, delivered_to_actor)
             VALUES (?1, ?2, 'inbound', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                &row.message_id[..],
                row.received_at,
                row.clamav_verdict,
                row.clamav_signature,
                row.rspamd_score_raw,
                row.rspamd_score_scaled,
                row.rspamd_flagged_rules,
                row.rspamd_score_breakdown,
                row.scanned_at,
                row.action_taken,
                actor_bytes,
            ],
        )
        .context("insert message_scan_results")?;
        Ok(())
    }

    /// Read one content-scan result by message_id. The future
    /// `fauna.bridges.get_message_scan_result` user-read RPC reads through
    /// this; today it backs the ingest-path tests. `None` if absent.
    pub async fn get_scan_result(&self, message_id: &[u8; 32]) -> Result<Option<ScanResultRow>> {
        let mid = *message_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT received_at, scanned_at, clamav_verdict, clamav_signature,
                    rspamd_score_raw, rspamd_score_scaled, rspamd_flagged_rules,
                    rspamd_score_breakdown, action_taken, delivered_to_actor
             FROM message_scan_results WHERE message_id = ?1",
            rusqlite::params![&mid[..]],
            |r| {
                let actor_blob: Option<Vec<u8>> = r.get(9)?;
                Ok(ScanResultRow {
                    message_id: mid,
                    received_at: r.get(0)?,
                    scanned_at: r.get(1)?,
                    clamav_verdict: r.get(2)?,
                    clamav_signature: r.get(3)?,
                    rspamd_score_raw: r.get(4)?,
                    rspamd_score_scaled: r.get(5)?,
                    rspamd_flagged_rules: r.get(6)?,
                    rspamd_score_breakdown: r.get(7)?,
                    action_taken: r.get(8)?,
                    delivered_to_actor: actor_blob
                        .map(|v| blob_col_to_array(v, 9, "delivered_to_actor"))
                        .transpose()?,
                })
            },
        )
        .optional()
        .context("read message_scan_results")
    }

    /// Upsert the uniform scoring-metadata bus rows for one content item —
    /// one row per factor, idempotent on the deterministic-content_id retry
    /// (PK `(content_id, factor)` + INSERT OR REPLACE, like
    /// `insert_scan_result`). Metadata only, both storage modes
    /// (`content-scoring.md` § The scoring-metadata bus). A factor-row write
    /// is the `content.label-write` operation at the key-access layer
    /// (capability-mediated content-processing design § capability-scope
    /// taxonomy); scope enforcement ships with the capability substrate — the
    /// callers today are the nest's own ingest path.
    pub async fn insert_content_scores(
        &self,
        content_id: &[u8; 32],
        content_kind: &str,
        actor_id: Option<&[u8; 32]>,
        scored_at: i64,
        entries: &[fauna_core::scoring::ScoreEntry],
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let cid = *content_id;
        let actor_bytes = actor_id.map(|a| a.to_vec());
        let conn = self.conn.lock().await;
        for e in entries {
            conn.execute(
                "INSERT OR REPLACE INTO content_scores (
                     content_id, content_kind, factor, score, tier,
                     scorer_version, scored_at, actor_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    &cid[..],
                    content_kind,
                    e.factor,
                    e.score,
                    e.tier as i64,
                    e.scorer_version as i64,
                    scored_at,
                    actor_bytes,
                ],
            )
            .context("insert content_scores row")?;
        }
        Ok(())
    }

    /// Read every scoring-bus row for one content item, factor-ordered — the
    /// uniform downstream-consumer read: any factor (spam, clamav, rspamd,
    /// auth_*, a future community label) comes back through this one shape
    /// instead of a per-kind column.
    pub async fn get_content_scores(
        &self,
        content_id: &[u8; 32],
    ) -> Result<Vec<fauna_core::scoring::ScoreEntry>> {
        let cid = *content_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT factor, score, tier, scorer_version
                 FROM content_scores WHERE content_id = ?1 ORDER BY factor",
            )
            .context("prepare content_scores read")?;
        let rows = stmt
            .query_map(rusqlite::params![&cid[..]], |r| {
                Ok(fauna_core::scoring::ScoreEntry {
                    factor: r.get(0)?,
                    score: r.get(1)?,
                    tier: r.get::<_, i64>(2)?.clamp(0, u8::MAX as i64) as u8,
                    scorer_version: r.get::<_, i64>(3)?.clamp(0, u32::MAX as i64) as u32,
                })
            })
            .context("read content_scores")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect content_scores rows")?;
        Ok(rows)
    }

    /// The stored owner (`actor_id`) of a scored content item, from any of its
    /// existing `content_scores` rows (all factors of one `content_id` share a
    /// single owner). `None` when the content has no owner-attributed score row.
    /// A projection of [`Self::content_score_owner_and_kind`].
    pub async fn content_score_owner(&self, content_id: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        Ok(self
            .content_score_owner_and_kind(content_id)
            .await?
            .map(|(owner, _)| owner))
    }

    /// The stored owner (`actor_id`) AND stored `content_kind` of a scored
    /// content item, read from the same owner-attributed `content_scores` row.
    /// `None` when the content has no owner-attributed score row.
    ///
    /// Backs `submit_scores`'s content-bound authorization: a re-score write
    /// MUST be authorized against the content's TRUE owner and TRUE kind, never
    /// the holder-claimed `owner_actor_id` / `content_kind`. Without the owner,
    /// a holder legitimately holding `content.label-write` over owner A but
    /// only `content.read` over owner B could learn B's `content_id` via the
    /// worklist and overwrite + re-attribute B's score row by *claiming* owner
    /// A;
    /// without the kind, a `label-write{K1}` grant would reach the owner's
    /// K2 rows by claiming K1 and relabel them out of their worklist bucket. The lookup is always satisfiable for a legitimate
    /// re-score — `submit_scores` only ever touches content that was ingested +
    /// scored at least once, and the drain echoes the worklist's stored kind.
    pub async fn content_score_owner_and_kind(
        &self,
        content_id: &[u8; 32],
    ) -> Result<Option<([u8; 32], String)>> {
        let cid = *content_id;
        let conn = self.conn.lock().await;
        let row: Option<(Vec<u8>, String)> = conn
            .query_row(
                "SELECT actor_id, content_kind FROM content_scores
                 WHERE content_id = ?1 AND actor_id IS NOT NULL LIMIT 1",
                rusqlite::params![&cid[..]],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("read content_scores owner")?;
        match row {
            Some((bytes, kind)) => {
                let arr: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .context("content_scores.actor_id not 32 bytes")?;
                Ok(Some((arr, kind)))
            }
            None => Ok(None),
        }
    }

    /// Delete `message_scan_results` rows past their retention window. Backs the
    /// periodic GC (`mail-content-scanning.md` § Retention): rows with
    /// `action_taken = 'rejected_malware'` (admin-only forensic records,
    /// `delivered_to_actor = NULL`) are pruned at `rejected_malware_cutoff_ms`,
    /// every other row at `general_cutoff_ms` — honoring the per-action-category
    /// override (§ Retention with action_taken = 'rejected_malware'). Returns the
    /// number deleted. Mirrors `mail_aliases::prune_alias_hits_older_than`.
    pub async fn prune_scan_results_older_than(
        &self,
        general_cutoff_ms: i64,
        rejected_malware_cutoff_ms: i64,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM message_scan_results
                 WHERE (action_taken =  'rejected_malware' AND received_at < ?1)
                    OR (action_taken <> 'rejected_malware' AND received_at < ?2)",
                rusqlite::params![rejected_malware_cutoff_ms, general_cutoff_ms],
            )
            .context("prune message_scan_results")?;
        Ok(n)
    }

    // ── Greylist tuples ─────────────────────
    //
    // Nest-side greylist state so it is uniform across bridge restart
    // (`smtp-server.md` § Greylisting: "The MTA stores no greylist state
    // locally"). The pure tuple-key + defer/pass decision live in shared
    // `fauna_mail::greylist`; these methods are the I/O the
    // `fauna.bridges.check_greylist` handler wraps them in. Timestamps are
    // Unix **seconds** (the `outbound_now()`/`now_epoch_secs()` clock).

    /// Read the greylist row for a tuple, if present.
    pub async fn get_greylist_row(
        &self,
        tuple: &fauna_mail::greylist::GreylistTuple,
    ) -> Result<Option<fauna_mail::greylist::GreylistRow>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT first_seen, last_attempt, accepted_at
             FROM greylist_tuples
             WHERE sender_domain = ?1 AND recipient = ?2 AND subnet = ?3",
            rusqlite::params![tuple.sender_domain, tuple.recipient, tuple.subnet],
            |r| {
                Ok(fauna_mail::greylist::GreylistRow {
                    first_seen: r.get(0)?,
                    last_attempt: r.get(1)?,
                    accepted_at: r.get(2)?,
                })
            },
        )
        .optional()
        .context("read greylist_tuples")
    }

    /// Upsert the greylist row for a tuple after a `decide` verdict.
    pub async fn upsert_greylist_row(
        &self,
        tuple: &fauna_mail::greylist::GreylistTuple,
        row: &fauna_mail::greylist::GreylistRow,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO greylist_tuples
                 (sender_domain, recipient, subnet, first_seen, last_attempt, accepted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(sender_domain, recipient, subnet) DO UPDATE SET
                 first_seen   = excluded.first_seen,
                 last_attempt = excluded.last_attempt,
                 accepted_at  = excluded.accepted_at",
            rusqlite::params![
                tuple.sender_domain,
                tuple.recipient,
                tuple.subnet,
                row.first_seen,
                row.last_attempt,
                row.accepted_at,
            ],
        )
        .context("upsert greylist_tuples")?;
        Ok(())
    }

    /// Delete greylist rows untouched since `cutoff_secs` (Unix seconds).
    /// Backs the periodic GC: a row whose `last_attempt` predates the longest
    /// window (the 30 d whitelist) can never affect a decision again
    /// (`accepted_at ≤ last_attempt`, and an un-accepted row only matters for
    /// the 4 h retry window). Returns the number deleted.
    pub async fn prune_greylist_rows_older_than(&self, cutoff_secs: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM greylist_tuples WHERE last_attempt < ?1",
                rusqlite::params![cutoff_secs],
            )
            .context("prune greylist_tuples")?;
        Ok(n)
    }
}

/// Default retention for `message_scan_results` rows, derived from the shared
/// [`fauna_mail::scan::SCAN_RESULT_RETENTION_DAYS`] (30 d). The nest spawns
/// [`spawn_scan_result_retention_sweeper`] with this for the general window and
/// (until the admin knob lands) the fallback rejected-malware window.
pub const DEFAULT_SCAN_RESULT_RETENTION: std::time::Duration = std::time::Duration::from_secs(
    fauna_mail::scan::SCAN_RESULT_RETENTION_DAYS as u64 * 24 * 60 * 60,
);

/// Spawns a tokio task that periodically deletes `message_scan_results` rows
/// past their retention. `rejected_malware_retention` is the per-action-category
/// override for `action_taken = 'rejected_malware'` forensic rows
/// (`mail-content-scanning.md` § Retention with action_taken =
/// 'rejected_malware'); `None` falls back to `general_retention`. Cadence is
/// 1/24 of `general_retention` (a 30-day window sweeps every ~1.25 days),
/// mirroring `bridge_audit::spawn_audit_retention_sweeper` /
/// `mail_aliases::spawn_alias_hits_retention_sweeper` — an independent per-table
/// sweeper (there is no shared GC scheduler; the doc's "at the same time as the
/// `smtp_verdicts` GC" is operational symmetry, and `smtp_verdicts` itself is
/// unbuilt). Keying the cadence off the general (shorter) window is correct: the
/// rejected-malware override is only ever longer, so those rows survive between
/// sweeps until their own cutoff passes.
pub fn spawn_scan_result_retention_sweeper(
    db: std::sync::Arc<CacheDb>,
    general_retention: std::time::Duration,
    rejected_malware_retention: Option<std::time::Duration>,
) -> tokio::task::JoinHandle<()> {
    let rejected_retention = rejected_malware_retention.unwrap_or(general_retention);
    super::spawn_retention_sweeper(general_retention, move || {
        let db = db.clone();
        async move {
            let now = now_epoch_millis();
            let general_cutoff_ms = now.saturating_sub(general_retention.as_millis() as i64);
            let rejected_cutoff_ms = now.saturating_sub(rejected_retention.as_millis() as i64);
            match db
                .prune_scan_results_older_than(general_cutoff_ms, rejected_cutoff_ms)
                .await
            {
                Ok(n) if n > 0 => tracing::info!(
                    target: "mail_scan",
                    pruned = n,
                    general_cutoff_ms,
                    rejected_cutoff_ms,
                    "pruned message scan results"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    target: "mail_scan",
                    error = %e,
                    "scan-result retention sweep failed"
                ),
            }
        }
    })
}

/// Default retention for `greylist_tuples` rows (30 d), from the shared
/// [`fauna_mail::greylist::GREYLIST_RETENTION_SECONDS`]. A row untouched this
/// long is past the whitelist window and can never affect a decision again.
pub const DEFAULT_GREYLIST_RETENTION: std::time::Duration =
    std::time::Duration::from_secs(fauna_mail::greylist::GREYLIST_RETENTION_SECONDS as u64);

/// Spawns a tokio task that periodically deletes `greylist_tuples` rows past
/// their retention (by `last_attempt`). Cadence is 1/24 of `retention`
/// (a 30 d window sweeps every ~1.25 d), mirroring
/// [`spawn_scan_result_retention_sweeper`]. NB greylist timestamps are Unix
/// **seconds** (the `now_epoch_secs`/`outbound_now` clock), not millis.
pub fn spawn_greylist_retention_sweeper(
    db: std::sync::Arc<CacheDb>,
    retention: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    super::spawn_retention_sweeper(retention, move || {
        let db = db.clone();
        async move {
            let cutoff_secs = now_epoch_secs().saturating_sub(retention.as_secs() as i64);
            match db.prune_greylist_rows_older_than(cutoff_secs).await {
                Ok(n) if n > 0 => tracing::info!(
                    target: "mail_greylist",
                    pruned = n,
                    cutoff_secs,
                    "pruned greylist tuples"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    target: "mail_greylist",
                    error = %e,
                    "greylist retention sweep failed"
                ),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mail::segments::MailRecordEnvelope;
    use tempfile::TempDir;

    fn sample_inbound_fields(actor: &[u8; 32], timestamp: i64, body: &[u8]) -> InboundMailFields {
        InboundMailFields {
            actor_id: *actor,
            timestamp,
            ciphertext_size: body.len() as u32,
            encrypted_body: SealedRecordBytes::carried_at_rest_unchecked(body.to_vec()),
            encrypted_index_hint: SealedRecordBytes::carried_at_rest_unchecked(
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
            scores: vec![fauna_core::scoring::ScoreEntry {
                factor: "spam".into(),
                score: 0,
                tier: 1,
                scorer_version: 1,
            }],
            report_hash: vec![0xCD; 32],
        }
    }

    /// Returns (tempdir, manager, cache_db). Mirrors `setup` in
    /// `bins/fauna-nest/src/segments/mail.rs` — drop the tempdir last
    /// so the on-disk segment files outlive the manager.
    fn setup() -> (TempDir, SegmentManager, CacheDb) {
        let tmp = TempDir::new().expect("tmp");
        let manager = SegmentManager::new(tmp.path().to_path_buf(), "mail");
        let cache_db = CacheDb::open_in_memory().expect("in-memory cache db");
        (tmp, manager, cache_db)
    }

    #[tokio::test]
    async fn insert_appended_mail_assigns_deterministic_message_id() {
        let (_tmp, manager, db) = setup();
        let actor = [4u8; 32];
        let fields = sample_inbound_fields(&actor, 1_700_000_000, b"hello-encrypted");
        let o1 = db.insert_appended_mail(&manager, &fields).await.unwrap();
        assert!(o1.inserted, "first call must report inserted=true");
        // Second call with byte-identical fields hits the segment_records
        // dedupe check and returns the same id without re-appending.
        let o2 = db.insert_appended_mail(&manager, &fields).await.unwrap();
        assert_eq!(o1.message_id, o2.message_id, "same fields → same id");
        assert!(!o2.inserted, "duplicate call must report inserted=false");
        assert!(
            o2.finalized.is_none(),
            "idempotent retry must not report a finalized seg_id"
        );
        assert_eq!(o1.message_id.len(), 32);
    }

    #[tokio::test]
    async fn insert_scan_result_round_trips_and_is_idempotent() {
        let (_tmp, _manager, db) = setup();
        let row = ScanResultRow {
            message_id: [9u8; 32],
            received_at: 1_700_000_000,
            scanned_at: 1_700_000_001,
            clamav_verdict: "infected".into(),
            clamav_signature: Some("Eicar-Test-Signature".into()),
            rspamd_score_raw: Some(2400),
            rspamd_score_scaled: Some(1200),
            rspamd_flagged_rules: Some(r#"["BAYES_HAM","URIBL_BLACK"]"#.into()),
            rspamd_score_breakdown: Some(r#"{"BAYES_HAM":-2900,"URIBL_BLACK":5400}"#.into()),
            action_taken: "junked".into(),
            delivered_to_actor: Some([7u8; 32]),
        };
        db.insert_scan_result(&row).await.unwrap();

        {
            let conn = db.conn.lock().await;
            #[allow(clippy::type_complexity)]
            let (verdict, sig, raw, scaled, rules, action, actor): (
                String,
                Option<String>,
                Option<i64>,
                Option<i64>,
                Option<String>,
                String,
                Option<Vec<u8>>,
            ) = conn
                .query_row(
                    "SELECT clamav_verdict, clamav_signature, rspamd_score_raw,
                            rspamd_score_scaled, rspamd_flagged_rules, action_taken,
                            delivered_to_actor
                     FROM message_scan_results WHERE message_id = ?1",
                    rusqlite::params![&row.message_id[..]],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(verdict, "infected");
            assert_eq!(sig.as_deref(), Some("Eicar-Test-Signature"));
            assert_eq!(raw, Some(2400));
            assert_eq!(scaled, Some(1200));
            assert_eq!(rules.as_deref(), Some(r#"["BAYES_HAM","URIBL_BLACK"]"#));
            assert_eq!(action, "junked");
            assert_eq!(actor.as_deref(), Some(&[7u8; 32][..]));
        }

        // Idempotent retry (deterministic message_id) → OR REPLACE, one row.
        db.insert_scan_result(&row).await.unwrap();
        let conn = db.conn.lock().await;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM message_scan_results", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn prune_scan_results_honors_per_action_category_window() {
        let (_tmp, _manager, db) = setup();
        let mk = |id: u8, received_at: i64, action: &str| ScanResultRow {
            message_id: [id; 32],
            received_at,
            scanned_at: received_at,
            clamav_verdict: if action == "rejected_malware" {
                "infected"
            } else {
                "clean"
            }
            .into(),
            clamav_signature: None,
            rspamd_score_raw: None,
            rspamd_score_scaled: None,
            rspamd_flagged_rules: None,
            rspamd_score_breakdown: None,
            action_taken: action.into(),
            // Reject-at-perimeter forensic rows carry no recipient actor.
            delivered_to_actor: (action != "rejected_malware").then_some([1u8; 32]),
        };
        // a: delivered, old → pruned at the general cutoff.
        db.insert_scan_result(&mk(0xa1, 400, "delivered"))
            .await
            .unwrap();
        // b: delivered, fresh → survives.
        db.insert_scan_result(&mk(0xb2, 1500, "delivered"))
            .await
            .unwrap();
        // c: rejected_malware, very old → pruned at the (longer) rejected cutoff.
        db.insert_scan_result(&mk(0xc3, 400, "rejected_malware"))
            .await
            .unwrap();
        // d: rejected_malware, between the two cutoffs → survives the longer
        //    forensic window even though it's older than the general cutoff.
        db.insert_scan_result(&mk(0xd4, 700, "rejected_malware"))
            .await
            .unwrap();

        let pruned = db
            .prune_scan_results_older_than(/* general */ 1000, /* rejected_malware */ 500)
            .await
            .unwrap();
        assert_eq!(
            pruned, 2,
            "a (delivered < 1000) + c (rejected_malware < 500)"
        );

        assert!(db.get_scan_result(&[0xa1; 32]).await.unwrap().is_none());
        assert!(db.get_scan_result(&[0xb2; 32]).await.unwrap().is_some());
        assert!(db.get_scan_result(&[0xc3; 32]).await.unwrap().is_none());
        assert!(
            db.get_scan_result(&[0xd4; 32]).await.unwrap().is_some(),
            "rejected_malware row inside the longer forensic window survives the general cutoff"
        );
    }

    #[tokio::test]
    async fn greylist_row_round_trips_upserts_and_prunes() {
        use fauna_mail::greylist::{GreylistRow, GreylistTuple};
        let (_tmp, _manager, db) = setup();
        let tuple = GreylistTuple {
            sender_domain: "example.com".into(),
            recipient: "bob@fauna.test".into(),
            subnet: "203.0.113.0/24".into(),
        };

        // Absent tuple reads None.
        assert!(db.get_greylist_row(&tuple).await.unwrap().is_none());

        // First write: un-accepted hold.
        db.upsert_greylist_row(
            &tuple,
            &GreylistRow {
                first_seen: 1_000,
                last_attempt: 1_000,
                accepted_at: None,
            },
        )
        .await
        .unwrap();
        let got = db.get_greylist_row(&tuple).await.unwrap().unwrap();
        assert_eq!(got.first_seen, 1_000);
        assert_eq!(got.accepted_at, None);

        // Second write (same tuple) upserts in place, marking acceptance.
        db.upsert_greylist_row(
            &tuple,
            &GreylistRow {
                first_seen: 1_000,
                last_attempt: 1_090,
                accepted_at: Some(1_090),
            },
        )
        .await
        .unwrap();
        let got = db.get_greylist_row(&tuple).await.unwrap().unwrap();
        assert_eq!(got.last_attempt, 1_090);
        assert_eq!(got.accepted_at, Some(1_090));
        {
            let conn = db.conn.lock().await;
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM greylist_tuples", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "upsert must not create a second row");
        }

        // Prune by last_attempt: cutoff after 1_090 removes the row.
        assert_eq!(db.prune_greylist_rows_older_than(1_091).await.unwrap(), 1);
        assert!(db.get_greylist_row(&tuple).await.unwrap().is_none());
    }

    /// Post-cutover semantics (`message-segment-store.md` § Record identity
    /// per kind): identity is the content hash of the stored envelope bytes,
    /// so the retired per-path domain-tag distinction is gone — byte-identical
    /// fields on the ingest and APPEND paths are ONE record, and the second
    /// arrival dedups. (Independent messages never share bytes: mail seals are
    /// HPKE-ephemeral-fresh, so a collision is a literal byte replay.)
    #[tokio::test]
    async fn ingest_and_append_of_identical_bytes_are_one_record() {
        let (_tmp, manager, db) = setup();
        let actor = [4u8; 32];
        let fields = sample_inbound_fields(&actor, 1_700_000_000, b"same-body");
        let ingest = db.insert_inbound_mail(&manager, &fields).await.unwrap();
        assert!(ingest.inserted);
        let appended = db.insert_appended_mail(&manager, &fields).await.unwrap();
        assert_eq!(
            ingest.message_id, appended.message_id,
            "one content, one identity — regardless of arrival path"
        );
        assert!(!appended.inserted, "the byte replay dedups");
    }

    #[tokio::test]
    async fn insert_inbound_mail_assigns_deterministic_message_id() {
        let (_tmp, manager, db) = setup();
        let actor = [4u8; 32];
        let fields = sample_inbound_fields(&actor, 1_700_000_000, b"hello-encrypted");
        let o1 = db.insert_inbound_mail(&manager, &fields).await.unwrap();
        assert!(o1.inserted, "first call must report inserted=true");
        // Second call with byte-identical fields must hit the scoped
        // segment_records dedupe check and return the same id.
        let o2 = db.insert_inbound_mail(&manager, &fields).await.unwrap();
        assert_eq!(o1.message_id, o2.message_id);
        assert!(!o2.inserted, "duplicate call must report inserted=false");
        assert_eq!(o1.message_id.len(), 32);

        // The identity is the content hash of the stored bytes: a replay at a
        // different timestamp is the SAME sealed bytes, hence the same record
        // (the timestamp lives in the floor, not the identity) — while a
        // different body is a different record.
        let replayed_later = sample_inbound_fields(&actor, 1_700_000_001, b"hello-encrypted");
        let o3 = db
            .insert_inbound_mail(&manager, &replayed_later)
            .await
            .unwrap();
        assert_eq!(o1.message_id, o3.message_id);
        assert!(!o3.inserted, "a later byte replay still dedups");
        let other_body = sample_inbound_fields(&actor, 1_700_000_000, b"different-encrypted");
        let o4 = db.insert_inbound_mail(&manager, &other_body).await.unwrap();
        assert_ne!(o1.message_id, o4.message_id);
        assert!(o4.inserted);
    }

    /// The dedup is scope-qualified: the actor is no longer inside the hash,
    /// so identical bytes delivered to two actors share an id but each scope
    /// keeps its OWN record (pre-check 2's obligation — scope-agnostic dedup
    /// would silently drop the second actor's mail).
    #[tokio::test]
    async fn insert_inbound_mail_isolates_by_actor() {
        let (_tmp, manager, db) = setup();
        let alice = [1u8; 32];
        let bob = [2u8; 32];
        let f1 = sample_inbound_fields(&alice, 1700, b"body");
        let f2 = sample_inbound_fields(&bob, 1700, b"body");
        let o1 = db.insert_inbound_mail(&manager, &f1).await.unwrap();
        let o2 = db.insert_inbound_mail(&manager, &f2).await.unwrap();
        assert_eq!(o1.message_id, o2.message_id, "same bytes, same identity");
        assert!(o1.inserted, "alice's record inserts");
        assert!(o2.inserted, "bob's record inserts despite the shared id");
        for actor in [&alice, &bob] {
            assert!(
                crate::segments::mail::read_record_with_floor(&manager, &db, actor, &o1.message_id)
                    .await
                    .unwrap()
                    .is_some(),
                "each scope holds its own record"
            );
        }
    }

    /// Insert via the segment-store path and verify every floor field
    /// + envelope payload survives the round-trip through
    /// `segments::mail::read_record_with_floor`. T9 replaced the
    /// prior `get_inbound_mail` round-trip (which read from the dropped
    /// `bridge_inbound_mail` table) with this.
    #[tokio::test]
    async fn insert_inbound_mail_persists_all_fields() {
        let (_tmp, manager, db) = setup();
        let actor = [7u8; 32];
        let fields = sample_inbound_fields(&actor, 1_700_000_000, b"body");
        let o = db.insert_inbound_mail(&manager, &fields).await.unwrap();
        let id = o.message_id;

        let (envelope, floor) =
            crate::segments::mail::read_record_with_floor(&manager, &db, &actor, &id)
                .await
                .unwrap()
                .expect("record exists");
        assert_eq!(
            envelope.encrypted_body.as_slice(),
            fields.encrypted_body.as_slice()
        );
        assert_eq!(
            envelope.encrypted_index_hint.as_slice(),
            fields.encrypted_index_hint.as_slice()
        );
        assert_eq!(floor.timestamp, fields.timestamp);
        assert_eq!(floor.ciphertext_size, fields.ciphertext_size);
        assert_eq!(floor.sender_domain, fields.sender_domain);
        assert_eq!(floor.spam_score, fields.spam_score);
        assert_eq!(floor.spam_disposition, "accept");
        assert_eq!(floor.spf, fields.spf);
        assert_eq!(floor.dkim, fields.dkim);
        assert_eq!(floor.dmarc, fields.dmarc);
        assert_eq!(floor.dmarc_policy, fields.dmarc_policy);
        assert_eq!(floor.arc, fields.arc);
        assert!(!floor.is_own_submission);
        // The scoring-bus rows ride the authoritative footer (mirror type,
        // field-for-field).
        assert_eq!(floor.scores.len(), fields.scores.len());
        assert_eq!(floor.scores[0].factor, fields.scores[0].factor);
        assert_eq!(floor.scores[0].score, fields.scores[0].score);
        assert_eq!(floor.scores[0].tier, fields.scores[0].tier);
        assert_eq!(
            floor.scores[0].scorer_version,
            fields.scores[0].scorer_version
        );

        // The report-hash rides the authoritative footer verbatim
        // (report-sharing.md § Content identity).
        assert_eq!(floor.report_hash, fields.report_hash);

        // The segment_records mirror holds the record in the owning scope.
        assert!(
            db.segment_records_lookup_record(
                &actor,
                "mail",
                &fauna_cbor::Cid::from_digest_dag_cbor(id)
            )
            .await
            .unwrap()
            .is_some()
        );

        // ... and the report-hash mirror column (the hot-path lookup Slice 2
        // consumes) carries the same bytes.
        let cid = fauna_cbor::Cid::from_digest_dag_cbor(id);
        let stored: Option<Vec<u8>> = {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT report_hash FROM segment_records WHERE record_cid = ?1",
                rusqlite::params![&cid.as_bytes()[..]],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(stored.as_deref(), Some(fields.report_hash.as_slice()));
    }

    /// The scoped mirror lookup returns `None` for an unknown id. T9 replaced
    /// the prior `get_inbound_mail` None-on-miss check with this — same
    /// semantics, segment-store path.
    #[tokio::test]
    async fn lookup_returns_none_for_unknown_message_id() {
        let (_tmp, _manager, db) = setup();
        let missing = [9u8; 32];
        assert!(
            db.segment_records_lookup_record(
                &[4u8; 32],
                "mail",
                &fauna_cbor::Cid::from_digest_dag_cbor(missing)
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    /// Write-path end-to-end: insert via the segment-store route, then
    /// read the envelope back through `segments::mail::read_envelope`.
    ///
    /// Demonstrates the read-your-own-writes policy: T9 made
    /// `read_envelope` auto-finalise the actor's open segment, so the
    /// caller never needs an explicit `flush(&actor)` between write
    /// and read. See the module-level read-your-own-writes POLICY note
    /// in `segments::mail`.
    #[tokio::test]
    async fn insert_inbound_mail_round_trips_through_segment_store() {
        let (_tmp, manager, db) = setup();
        let actor = [0x5au8; 32];
        let body = b"sealed-body-bytes".to_vec();
        let hint = b"sealed-index-hint".to_vec();
        let fields = InboundMailFields {
            actor_id: actor,
            timestamp: 1_715_000_000,
            ciphertext_size: body.len() as u32,
            encrypted_body: SealedRecordBytes::carried_at_rest_unchecked(body.clone()),
            encrypted_index_hint: SealedRecordBytes::carried_at_rest_unchecked(hint.clone()),
            sender_domain: "example.com".into(),
            spf: "pass".into(),
            dkim: "pass".into(),
            dmarc: "pass".into(),
            dmarc_policy: "reject".into(),
            arc: "pass".into(),
            spam_score: 7,
            spam_disposition: "accept".into(),
            is_own_submission: false,
            scores: vec![],
            report_hash: vec![],
        };

        let o1 = db.insert_inbound_mail(&manager, &fields).await.unwrap();
        assert!(o1.inserted, "first call must append to segment store");
        assert_eq!(o1.message_id.len(), 32);

        // Second call with byte-identical fields hits the segment_records
        // dedupe pre-check; no second append, same id returned.
        let o2 = db.insert_inbound_mail(&manager, &fields).await.unwrap();
        assert_eq!(o1.message_id, o2.message_id);
        assert!(!o2.inserted, "duplicate call must skip append");

        // No explicit flush — `read_envelope` auto-finalises the open
        // segment per the T9 read-your-own-writes policy.
        let env_bytes = crate::segments::mail::read_envelope(&manager, &db, &actor, &o1.message_id)
            .await
            .expect("read envelope")
            .expect("record present");
        let decoded = MailRecordEnvelope::decode(&env_bytes).expect("decode envelope");
        assert_eq!(decoded.encrypted_body, body);
        assert_eq!(decoded.encrypted_index_hint, hint);
    }

    #[tokio::test]
    async fn insert_and_get_content_scores_uniform_read() {
        use fauna_core::scoring::{ScoreEntry, TIER_ADMIN, TIER_USER, factor};
        let db = CacheDb::open_in_memory().unwrap();
        let cid = [3u8; 32];
        let actor = [4u8; 32];
        let entries = vec![
            ScoreEntry {
                factor: factor::SPAM.into(),
                score: 875,
                tier: TIER_USER,
                scorer_version: 1,
            },
            ScoreEntry {
                factor: factor::RSPAMD.into(),
                score: -1200,
                tier: TIER_ADMIN,
                scorer_version: 1,
            },
        ];
        db.insert_content_scores(&cid, "mail", Some(&actor), 1_700_000_000, &entries)
            .await
            .unwrap();
        // Uniform any-factor read: every factor comes back through the one
        // shape (factor-ordered), signed milli scores intact.
        let read = db.get_content_scores(&cid).await.unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].factor, factor::RSPAMD);
        assert_eq!(read[0].score, -1200);
        assert_eq!(read[1].factor, factor::SPAM);
        assert_eq!(read[1].tier, TIER_USER);

        // Idempotent on the deterministic-content_id retry (INSERT OR
        // REPLACE on the (content_id, factor) PK) — a re-run with an updated
        // row replaces, never duplicates.
        let rescored = vec![ScoreEntry {
            factor: factor::SPAM.into(),
            score: 250,
            tier: TIER_USER,
            scorer_version: 2,
        }];
        db.insert_content_scores(&cid, "mail", Some(&actor), 1_700_000_100, &rescored)
            .await
            .unwrap();
        let read = db.get_content_scores(&cid).await.unwrap();
        assert_eq!(read.len(), 2, "replace, not duplicate");
        let spam = read.iter().find(|e| e.factor == factor::SPAM).unwrap();
        assert_eq!((spam.score, spam.scorer_version), (250, 2));

        // Unknown item → empty, not an error.
        assert!(db.get_content_scores(&[9u8; 32]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn put_and_get_actor_recipient_seal_key() {
        // Both halves ride one `actor_mls_pubkeys` row, written together.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [4u8; 32];
        let pubkey = [9u8; 32];
        let ek = vec![7u8; 1184];
        assert!(db.get_actor_mls_pubkey(&actor).await.unwrap().is_none());
        assert!(db.get_recipient_seal_key(&actor).await.unwrap().is_none());
        db.put_actor_recipient_seal_key(&actor, &pubkey, &ek)
            .await
            .unwrap();
        assert_eq!(db.get_actor_mls_pubkey(&actor).await.unwrap(), Some(pubkey));
        assert_eq!(
            db.get_recipient_seal_key(&actor).await.unwrap(),
            Some(RecipientSealKey {
                mls_pubkey: pubkey,
                mlkem_ek: ek,
            })
        );
    }

    #[tokio::test]
    async fn put_actor_recipient_seal_key_is_upsert() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        db.put_actor_recipient_seal_key(&actor, &[2u8; 32], &[5u8; 1184])
            .await
            .unwrap();
        db.put_actor_recipient_seal_key(&actor, &[3u8; 32], &[6u8; 1184])
            .await
            .unwrap();
        assert_eq!(
            db.get_recipient_seal_key(&actor).await.unwrap(),
            Some(RecipientSealKey {
                mls_pubkey: [3u8; 32],
                mlkem_ek: vec![6u8; 1184],
            }),
            "the upsert replaces both halves"
        );
    }

    #[tokio::test]
    async fn epoch_seal_key_selection_prefers_current_then_newest_earlier() {
        // The § 3 selection steps 1–2 (content-sealing epochs, v27): the row
        // for e_now itself when the schedule is current, else the newest
        // earlier row (never-bounce degradation), else None (step 3 falls
        // through to the standing key).
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [6u8; 32];

        // No schedule published → None.
        assert!(
            db.get_actor_epoch_seal_key(&actor, 3000)
                .await
                .unwrap()
                .is_none()
        );

        // Publish epochs 2998..=3001 (a small horizon).
        let rows: Vec<(u64, [u8; 32], Vec<u8>)> = (2998u64..=3001)
            .map(|e| (e, [e as u8; 32], vec![e as u8; 1184]))
            .collect();
        db.put_actor_epoch_seal_keys(&actor, &rows).await.unwrap();

        // Current epoch covered → exactly that row.
        let (e, key) = db
            .get_actor_epoch_seal_key(&actor, 3000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(e, 3000);
        assert_eq!(key.mls_pubkey, [3000u64 as u8; 32]);
        assert_eq!(key.mlkem_ek, vec![3000u64 as u8; 1184]);

        // Past the horizon → the newest published earlier row (stale, coarser).
        let (e, key) = db
            .get_actor_epoch_seal_key(&actor, 3050)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(e, 3001);
        assert_eq!(key.mls_pubkey, [3001u64 as u8; 32]);

        // Before the schedule's first epoch → None (nothing usable).
        assert!(
            db.get_actor_epoch_seal_key(&actor, 2997)
                .await
                .unwrap()
                .is_none()
        );

        // Re-publication upserts: replacing epoch 3000's key wins.
        db.put_actor_epoch_seal_keys(&actor, &[(3000, [0xEEu8; 32], vec![0xEFu8; 1184])])
            .await
            .unwrap();
        let (_, key) = db
            .get_actor_epoch_seal_key(&actor, 3000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.mls_pubkey, [0xEEu8; 32]);
        assert_eq!(
            key.mlkem_ek,
            vec![0xEFu8; 1184],
            "upsert replaces the whole row"
        );

        // Another actor's schedule is invisible.
        assert!(
            db.get_actor_epoch_seal_key(&[7u8; 32], 3000)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn epoch_seal_keys_prune_oldest_past_row_cap() {
        // The table is bounded per actor — every
        // write path prunes the OLDEST epochs once the per-actor row count
        // exceeds `MAX_ACTOR_EPOCH_SEAL_KEY_ROWS`. Prune-oldest is safe by
        // design § 3: correctness needs only the newest row ≤ e_now (the
        // never-bounce degradation); long-past rows are "prunable but
        // harmless".
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [11u8; 32];
        let cap = MAX_ACTOR_EPOCH_SEAL_KEY_ROWS as u64;

        // Grow well past the cap across many small puts (the adversarial
        // many-provisions shape).
        let base = 3000u64;
        for e in base..base + cap + 20 {
            db.put_actor_epoch_seal_keys(&actor, &[(e, [e as u8; 32], vec![7u8; 1184])])
                .await
                .unwrap();
        }
        let n = db.count_actor_epoch_seal_keys(&actor).await.unwrap();
        assert!(
            n as u64 <= cap,
            "row count {n} must stay within the per-actor cap {cap}"
        );

        // The NEWEST rows survive (prune drops oldest): the top-of-range
        // epoch still selects itself…
        let last = base + cap + 19;
        let (e, _) = db
            .get_actor_epoch_seal_key(&actor, last)
            .await
            .unwrap()
            .expect("newest epoch survives the prune");
        assert_eq!(e, last);
        // …and the oldest epoch is gone.
        assert!(
            db.get_actor_epoch_seal_key(&actor, base)
                .await
                .unwrap()
                .is_none(),
            "oldest epoch must have been pruned"
        );

        // Honest horizon shape never trips the cap: a 27-row publish plus a
        // refresh republish keeps every current-horizon row intact.
        let honest = [12u8; 32];
        let horizon = fauna_mls::wrapped_blob::MAIL_EPOCH_PUBLISH_HORIZON;
        for start in [5000u64, 5001] {
            let rows: Vec<(u64, [u8; 32], Vec<u8>)> = (start..=start + horizon)
                .map(|e| (e, [7u8; 32], vec![7u8; 1184]))
                .collect();
            db.put_actor_epoch_seal_keys(&honest, &rows).await.unwrap();
        }
        let n = db.count_actor_epoch_seal_keys(&honest).await.unwrap();
        assert_eq!(n as u64, horizon + 2, "27-row publish + 1 refreshed epoch");
        let (e, _) = db
            .get_actor_epoch_seal_key(&honest, 5000)
            .await
            .unwrap()
            .expect("honest current epoch intact");
        assert_eq!(e, 5000);
    }

    #[tokio::test]
    async fn recipient_mail_seal_key_gates_on_epoch_sealing_enabled() {
        // B4: `epoch_sealing_enabled = false` is byte-identical to today
        // (ignores any published schedule); `true` selects the § 3 epoch key
        // when one covers `e_now`.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [8u8; 32];
        let standing_pubkey = [0xAAu8; 32];
        db.put_actor_recipient_seal_key(&actor, &standing_pubkey, &[7u8; 1184])
            .await
            .unwrap();

        let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(now_epoch_secs() as u64);
        let epoch_pubkey = [0xBBu8; 32];
        db.put_actor_epoch_seal_keys(&actor, &[(e_now, epoch_pubkey, vec![7u8; 1184])])
            .await
            .unwrap();

        let off = db
            .get_recipient_mail_seal_key(&actor, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            off.mls_pubkey, standing_pubkey,
            "gate off ignores the published schedule"
        );

        let on = db
            .get_recipient_mail_seal_key(&actor, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            on.mls_pubkey, epoch_pubkey,
            "gate on seals under the current epoch's key"
        );
    }

    #[tokio::test]
    async fn recipient_mail_seal_key_falls_back_to_standing_when_no_schedule() {
        // A recipient who never published a schedule (mail provisioned but no
        // horizon on record yet) must never bounce ingest — falls straight
        // through to the standing key.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let standing_pubkey = [0xCCu8; 32];
        db.put_actor_recipient_seal_key(&actor, &standing_pubkey, &[7u8; 1184])
            .await
            .unwrap();

        let key = db
            .get_recipient_mail_seal_key(&actor, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.mls_pubkey, standing_pubkey);
    }

    #[tokio::test]
    async fn recipient_mail_seal_key_degrades_to_newest_earlier_epoch_when_stale() {
        // The never-bounce degradation (§ 3 step 2): a client offline past
        // its published horizon still gets mail — sealed under the newest
        // epoch it DID publish, not the standing key.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [10u8; 32];
        db.put_actor_recipient_seal_key(&actor, &[0xDDu8; 32], &[7u8; 1184])
            .await
            .unwrap();

        let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(now_epoch_secs() as u64);
        let stale_epoch = e_now.saturating_sub(5);
        let stale_pubkey = [0xEEu8; 32];
        db.put_actor_epoch_seal_keys(&actor, &[(stale_epoch, stale_pubkey, vec![7u8; 1184])])
            .await
            .unwrap();

        let key = db
            .get_recipient_mail_seal_key(&actor, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            key.mls_pubkey, stale_pubkey,
            "never-bounce degradation: seals under the newest published earlier epoch"
        );
    }

    #[tokio::test]
    async fn try_consume_submission_quota_increments_within_limit() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [4u8; 32];
        let day = 1234;
        assert_eq!(
            db.try_consume_submission_quota(&actor, day, 5, 10)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::Allowed,
        );
        assert_eq!(
            db.try_consume_submission_quota(&actor, day, 5, 10)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::Allowed,
        );
        // Used now = 10. Next attempt with any positive recipient_count must reject.
        assert_eq!(
            db.try_consume_submission_quota(&actor, day, 1, 10)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::OverQuota { remaining: 0 },
        );
    }

    #[tokio::test]
    async fn try_consume_submission_quota_over_limit_does_not_increment() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [4u8; 32];
        let day = 1234;
        // First call lands at used=8.
        let _ = db
            .try_consume_submission_quota(&actor, day, 8, 10)
            .await
            .unwrap();
        // Second asks for 5; would push to 13 > 10. Reject with remaining=2 (10-8),
        // and the rejected request must NOT consume any of its 5.
        assert_eq!(
            db.try_consume_submission_quota(&actor, day, 5, 10)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::OverQuota { remaining: 2 },
        );
        // Third asks for 2; should still fit (used stayed at 8).
        assert_eq!(
            db.try_consume_submission_quota(&actor, day, 2, 10)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::Allowed,
        );
    }

    #[tokio::test]
    async fn try_consume_submission_quota_isolates_by_day_and_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let alice = [1u8; 32];
        let bob = [2u8; 32];
        // Alice maxes day 100.
        let _ = db
            .try_consume_submission_quota(&alice, 100, 10, 10)
            .await
            .unwrap();
        // Same actor, next day — independent counter.
        assert_eq!(
            db.try_consume_submission_quota(&alice, 101, 1, 10)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::Allowed,
        );
        // Different actor, same day — independent counter.
        assert_eq!(
            db.try_consume_submission_quota(&bob, 100, 1, 10)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::Allowed,
        );
        // Re-checking Alice on day 100 still sees her at 10/10 → over_quota.
        assert_eq!(
            db.try_consume_submission_quota(&alice, 100, 1, 10)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::OverQuota { remaining: 0 },
        );
    }
}
