//! Community-labeler registry storage (labeler-registry design § 3).
//!
//! Two additive tables (in `migrations::MIGRATIONS_LABELERS`):
//! - `labelers` — the published-artifact catalog. The WASM module bytes are
//!   stored opaque in-row (like `capability_grants.blob`), size-capped; the
//!   columns beside `metadata_blob`/`wasm_bytes` are projections of the
//!   canonical-CBOR `AlgorithmLabeler`, indexed for `list`.
//! - `labeler_subscriptions` — the user↔labeler subscription; a subscription row
//!   *is* the user's choice (never dropped; no-user-data-loss invariant).
//!
//! CRUD only — the `fauna.labelers.*` handlers, gate, signature/hash/compile
//! validation, and the `model_versions` registration are the nest handler slice
//! (`labeler_handlers`); this module never interprets `metadata_blob` bytes.

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_millis, now_epoch_secs};

/// Maximum bytes for a stored WASM labeler module (labeler-registry design § 2).
/// 1 MiB is a generous ceiling for a self-contained scoring module; a blob-store
/// ref is a future optimization if modules outgrow it.
pub const MAX_LABELER_WASM_BYTES: usize = 1024 * 1024;

/// Maximum bytes for a stored `text-model` labeler artifact
/// (`topic-factors.md` § Publishing a trained factor: the bucket-1 constant
/// `TEXT_MODEL_ARTIFACT_MAX_BYTES`). 64 KiB, sixteen times tighter than the
/// WASM ceiling above, because a text model is a **bounded vocabulary** — at
/// most `TEXT_MODEL_PUBLISH_MAX_NGRAMS` entries — and not an arbitrary program.
/// The vocabulary cap is the *review* bound; this is the worst-case *byte*
/// bound, and the two are independent: 512 pathologically long n-grams satisfy
/// the first and blow this one.
pub const TEXT_MODEL_ARTIFACT_MAX_BYTES: usize = 64 * 1024;

/// Maximum number of distinct labelers (rows) one publisher may hold
/// (labeler-registry design § 2). Bounds a single publisher's storage
/// contribution; a *re-publish* of an existing `labeler_id` (a version bump) is a
/// replace, not a new row, so it is always allowed even at the cap.
///
/// ⚠ This cap keys on `publisher_actor` == the self-signed `algorithm_id`, a
/// **free, off-box-rotatable** Ed25519 keypair — so it is **evadable on its own**
/// (a fresh keypair per labeler never trips it). It is retained as a per-artifact
/// bound, but the DoS-relevant cap is [`MAX_LABELERS_PER_CALLER`] below, keyed on
/// the un-rotatable authenticated caller.
pub const MAX_LABELERS_PER_PUBLISHER: usize = 64;

/// Maximum number of distinct labelers (rows) one **authenticated caller** may
/// hold. This is the cap that actually bounds a single
/// identity's storage contribution: it keys on the router `actor_id` (the
/// un-rotatable enrolled-user identity), not the freely-rotatable `algorithm_id`,
/// so a `publish` loop with a fresh signing keypair each time still hits it. The
/// service-keypair model (a labeler as a distinct signed artifact) is preserved —
/// a caller may publish under many `algorithm_id`s, just not unbounded ones. A
/// re-publish (version bump) of an existing `labeler_id` is a replace, not a new
/// row, so it is allowed even at the cap. Equal to the per-publisher cap so a
/// caller holds at most 64 labelers total however they distribute keypairs.
pub const MAX_LABELERS_PER_CALLER: usize = 64;

/// Deployment-wide byte budget for the whole labeler catalog (security review
/// F1). A hard backstop against the stated ENOSPC-DoS impact: even if many
/// distinct callers each publish up to their [`MAX_LABELERS_PER_CALLER`] quota,
/// the total stored WASM cannot exceed this. 1 GiB = up to 1024 max-size
/// ([`MAX_LABELER_WASM_BYTES`]) modules — far beyond any realistic legitimate
/// catalog for an alpha nest, yet a finite ceiling on the shared DB. Not a
/// user/admin choice (product invariant: a DoS backstop, not a preference) — a
/// hard-coded constant, never a config knob.
pub const MAX_TOTAL_LABELER_BYTES: u64 = 1024 * 1024 * 1024;

/// Metadata-only projection of a `labelers` row — the `list` browse surface
/// (no `metadata_blob`, no `wasm_bytes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelerSummaryRow {
    pub labeler_id: Vec<u8>,
    pub version: u64,
    pub publisher_actor: Vec<u8>,
    pub content_kind: String,
    pub factor: String,
    pub wasm_hash: Vec<u8>,
    pub wasm_size: u64,
    /// `'wasm'` | `'list'` (design Block A, D8).
    pub artifact_kind: String,
    /// A `'text-model'` artifact's tokenizer/schema version; `0` = not
    /// applicable.
    pub artifact_version: u64,
}

/// A full `labelers` row including the opaque `metadata_blob` + `wasm_bytes` —
/// the `inspect` / subscribe read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelerRecord {
    pub labeler_id: Vec<u8>,
    pub version: u64,
    pub publisher_actor: Vec<u8>,
    pub content_kind: String,
    pub factor: String,
    pub wasm_hash: Vec<u8>,
    pub wasm_size: u64,
    pub metadata_blob: Vec<u8>,
    pub wasm_bytes: Vec<u8>,
    /// `'wasm'` | `'list'` (design Block A, D8).
    pub artifact_kind: String,
}

/// A `labeler_subscriptions` row (the `get_subscription` test/read helper).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelerSubscriptionRow {
    pub owner_actor: Vec<u8>,
    pub labeler_id: Vec<u8>,
    pub grant_id: Option<Vec<u8>>,
    pub subscribed_ver: u64,
}

/// Typed publisher-quota rejection so the publish handler can map it to a
/// client-actionable error class (mirrors `capability_grants::GrantQuotaExceeded`).
#[derive(Debug, thiserror::Error)]
#[error("publisher has too many labelers: {count} (max {max})")]
pub struct LabelerQuotaExceeded {
    pub count: i64,
    pub max: usize,
}

/// Typed per-caller-quota rejection: the authenticated
/// caller already holds [`MAX_LABELERS_PER_CALLER`] labelers. Distinct from
/// [`LabelerQuotaExceeded`] so a cold-read (and the client) can tell which cap
/// fired — this is the un-rotatable-identity cap that closes the keypair-rotation
/// evasion.
#[derive(Debug, thiserror::Error)]
#[error("caller has too many labelers: {count} (max {max})")]
pub struct LabelerCallerQuotaExceeded {
    pub count: i64,
    pub max: usize,
}

/// Typed global-byte-budget rejection: storing this labeler
/// would push the deployment-wide catalog past [`MAX_TOTAL_LABELER_BYTES`]. The
/// coarse aggregate backstop for the ENOSPC-DoS impact.
#[derive(Debug, thiserror::Error)]
#[error("labeler catalog byte budget exceeded: {projected} bytes would exceed max {max}")]
pub struct LabelerByteQuotaExceeded {
    pub projected: i64,
    pub max: u64,
}

/// Typed monotonic-version rejection: a publish whose `version` does not
/// strictly exceed the stored version is refused (labeler-registry design § 5).
#[derive(Debug, thiserror::Error)]
#[error("labeler version {submitted} is not newer than stored version {stored}")]
pub struct StaleLabelerVersion {
    pub submitted: u64,
    pub stored: u64,
}

/// One publish's stored columns — the handler passes the validated projections
/// alongside the opaque blobs so this module never decodes the metadata.
pub struct PutLabeler<'a> {
    pub labeler_id: &'a [u8],
    pub version: u64,
    pub publisher_actor: &'a [u8],
    /// The authenticated router `actor_id` that issued this publish (the
    /// un-rotatable identity the per-caller quota keys on).
    /// Distinct from `publisher_actor` (== the self-signed, rotatable
    /// `algorithm_id`).
    pub caller_actor: &'a [u8],
    pub content_kind: &'a str,
    pub factor: &'a str,
    pub wasm_hash: &'a [u8],
    pub wasm_size: u64,
    pub metadata_blob: &'a [u8],
    pub wasm_bytes: &'a [u8],
    /// `'wasm'` | `'list'` (design Block A, D8) — validated by the handler
    /// (`resolve_artifact_kind`); this module stores it verbatim.
    pub artifact_kind: &'a str,
    /// A `'text-model'` artifact's tokenizer/schema version, decoded by the
    /// handler (`validate_text_model_artifact`) so this module never interprets
    /// artifact bytes — the `artifact_kind` / `list_entries` division of labour
    /// verbatim. `0` for every other kind.
    pub artifact_version: u64,
    /// For a `'list'` artifact: the decoded, validated `(content_id, score)`
    /// entries — the handler decodes (`validate_list_artifact`) so this module
    /// never interprets the artifact bytes. Replaces the
    /// `labeler_list_entries` projection in the same transaction as the row,
    /// and resyncs the factor's materialized `content_scores` (design D12b:
    /// upsert current entries at the new version where a subscriber exists,
    /// withdraw rows for entries the new version dropped). Empty for `'wasm'`.
    pub list_entries: &'a [(Vec<u8>, i64)],
}

impl CacheDb {
    /// Store (publish) a labeler artifact. `INSERT OR REPLACE` keyed on
    /// `labeler_id`, gated on a **strictly increasing** version (a lower/equal
    /// version is rejected `StaleLabelerVersion`). A brand-new `labeler_id` is
    /// rejected if it would exceed **either** count cap — `LabelerQuotaExceeded`
    /// (per-publisher, keyed on the rotatable `algorithm_id`) or
    /// `LabelerCallerQuotaExceeded` (per authenticated caller, the un-rotatable
    /// identity that closes the F1 keypair-rotation evasion) — or the
    /// deployment-wide `MAX_TOTAL_LABELER_BYTES` budget (`LabelerByteQuotaExceeded`,
    /// the ENOSPC-DoS backstop, checked delta-accurately so a re-publish that
    /// grows a module is bounded too). A re-publish (version bump) of an existing
    /// `labeler_id` is a replace — the count caps don't apply (no new row), but
    /// the byte budget still does. All checks + the write run under the
    /// connection lock so check-then-write is atomic (no TOCTOU).
    pub async fn put_labeler(&self, p: PutLabeler<'_>) -> Result<()> {
        if p.wasm_bytes.len() > MAX_LABELER_WASM_BYTES {
            return Err(anyhow!(
                "labeler wasm too large: {} bytes (max {})",
                p.wasm_bytes.len(),
                MAX_LABELER_WASM_BYTES
            ));
        }
        let labeler_id = p.labeler_id.to_vec();
        let publisher_actor = p.publisher_actor.to_vec();
        let caller_actor = p.caller_actor.to_vec();
        let content_kind = p.content_kind.to_string();
        let factor = p.factor.to_string();
        let wasm_hash = p.wasm_hash.to_vec();
        let metadata_blob = p.metadata_blob.to_vec();
        let wasm_bytes = p.wasm_bytes.to_vec();
        let version = p.version;
        let wasm_size = p.wasm_size;
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;

        // Existing row (if any): its stored version gates monotonicity, its stored
        // `wasm_size` lets the byte budget account a replace's delta accurately,
        // and its stored `artifact_kind` is what the kind-change withdrawal
        // clause below compares against.
        let existing: Option<(i64, i64, String)> = conn
            .query_row(
                "SELECT version, wasm_size, artifact_kind FROM labelers WHERE labeler_id = ?1",
                rusqlite::params![labeler_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .context("check existing labeler")?;
        let previous_kind: Option<String> = existing.as_ref().map(|(_, _, k)| k.clone());

        // Monotonic version gate — a re-publish must strictly increase the version.
        if let Some((stored_ver, _, _)) = &existing {
            let stored_ver = (*stored_ver).clamp(0, i64::MAX) as u64;
            if version <= stored_ver {
                return Err(StaleLabelerVersion {
                    submitted: version,
                    stored: stored_ver,
                }
                .into());
            }
        }

        // Deployment-wide byte budget. Delta-accurate: a
        // re-publish replaces its own row, so subtract the stored size before
        // adding the new one. Bounds the whole catalog regardless of how growth is
        // distributed across callers/keypairs — the hard backstop for the
        // publish-a-1MiB-blob-per-fresh-keypair ENOSPC DoS.
        let total_all: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(wasm_size), 0) FROM labelers",
                [],
                |r| r.get(0),
            )
            .context("sum labeler catalog bytes")?;
        let existing_size = existing.as_ref().map(|(_, s, _)| *s).unwrap_or(0);
        let projected = total_all - existing_size + wasm_size as i64;
        if projected as u64 > MAX_TOTAL_LABELER_BYTES {
            return Err(LabelerByteQuotaExceeded {
                projected,
                max: MAX_TOTAL_LABELER_BYTES,
            }
            .into());
        }

        // A brand-new `labeler_id` (not a re-publish) — enforce both count caps.
        // The per-publisher cap bounds one signing keypair; the per-CALLER cap is
        // the one that survives keypair rotation (F1), keyed on the authenticated
        // router identity.
        if existing.is_none() {
            let publisher_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM labelers WHERE publisher_actor = ?1",
                    rusqlite::params![publisher_actor],
                    |r| r.get(0),
                )
                .context("count publisher labelers")?;
            if publisher_count as usize >= MAX_LABELERS_PER_PUBLISHER {
                return Err(LabelerQuotaExceeded {
                    count: publisher_count,
                    max: MAX_LABELERS_PER_PUBLISHER,
                }
                .into());
            }
            let caller_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM labelers WHERE caller_actor = ?1",
                    rusqlite::params![caller_actor],
                    |r| r.get(0),
                )
                .context("count caller labelers")?;
            if caller_count as usize >= MAX_LABELERS_PER_CALLER {
                return Err(LabelerCallerQuotaExceeded {
                    count: caller_count,
                    max: MAX_LABELERS_PER_CALLER,
                }
                .into());
            }
        }

        // Row + projection + bus resync are one atomic publish (a reader never
        // sees a new List version with the old projection or stale bus rows).
        let tx = conn.unchecked_transaction().context("begin put_labeler")?;
        tx.execute(
            "INSERT OR REPLACE INTO labelers
                (labeler_id, version, publisher_actor, caller_actor, content_kind,
                 factor, wasm_hash, wasm_size, metadata_blob, wasm_bytes, updated_at,
                 artifact_kind, artifact_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                labeler_id,
                version as i64,
                publisher_actor,
                caller_actor,
                content_kind,
                factor,
                wasm_hash,
                wasm_size as i64,
                metadata_blob,
                wasm_bytes,
                now,
                p.artifact_kind,
                p.artifact_version as i64,
            ],
        )
        .context("put labeler")?;

        // The derived id→score projection (design D11): replaced wholesale on
        // every publish (a re-publish IS the new list). Recreatable from
        // `wasm_bytes`, so this is derived data, not user-irrecoverable state.
        tx.execute(
            "DELETE FROM labeler_list_entries WHERE labeler_id = ?1",
            rusqlite::params![labeler_id],
        )
        .context("clear list projection")?;
        for (content_id, score) in p.list_entries {
            tx.execute(
                "INSERT INTO labeler_list_entries (labeler_id, content_id, score)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![labeler_id, content_id, score],
            )
            .context("insert list projection entry")?;
        }

        // Bus resync on (re)publish (design D12b) — mirrors the report:spam
        // nest-materialization shape. Withdraw rows for entries the new version
        // dropped (a no-op when no subscriber ever materialized any), then
        // re-materialize the current entries at the new version iff at least one
        // subscription exists (materialization is subscription-gated; publish
        // alone creates no bus rows). Rows attach only to content this nest has
        // (`content_meta` — the public/feed-participating gate; never a blind
        // row) and carry `actor_id = NULL` (a public post has no owner scope).
        // Every delete here is scoped to `content_kind = 'post'`: a List's rows
        // are always posts, and the same `labeler:<id>` factor may also key a
        // community room's rows (`super::room_labels`), which are the room's
        // derived view — deleted by the room's revoke, never by a List's
        // lifecycle.
        if p.artifact_kind == "list" {
            tx.execute(
                "DELETE FROM content_scores
                  WHERE factor = ?1
                    AND content_kind = 'post'
                    AND content_id NOT IN (
                        SELECT content_id FROM labeler_list_entries
                         WHERE labeler_id = ?2)",
                rusqlite::params![factor, labeler_id],
            )
            .context("withdraw dropped list entries")?;
            tx.execute(
                "INSERT OR REPLACE INTO content_scores
                    (content_id, content_kind, factor, score, tier,
                     scorer_version, scored_at, actor_id)
                 SELECT lle.content_id, 'post', ?1, lle.score, 3,
                        MIN(?2, 4294967295), ?3, NULL
                   FROM labeler_list_entries lle
                   JOIN content_meta cm ON cm.content_id = lle.content_id
                  WHERE lle.labeler_id = ?4
                    AND EXISTS (SELECT 1 FROM labeler_subscriptions s
                                 WHERE s.labeler_id = ?4)",
                rusqlite::params![factor, version as i64, now_epoch_millis(), labeler_id],
            )
            .context("resync list bus rows")?;
        } else if previous_kind.as_deref() == Some("list") {
            // ⚠ The frame's **republish resync clause** (§ Tier-3 artifact
            // kinds): when a new version's kind is not `list`, the nest
            // withdraws that labeler's materialized List rows.
            //
            // Scoped to a *previous* `list` on purpose. These rows exist only
            // because the nest itself materialized them from the old List
            // projection, and nothing will ever refresh them again — the new
            // kind is evaluated at the subscriber's client (`text-model`) or by
            // a holder's drain (`wasm`). Left standing they would keep composing
            // into every subscriber's feed under the same factor key the client
            // is now *also* scoring, double-counting a model the publisher has
            // already replaced.
            //
            // A `wasm` → `wasm` republish is deliberately NOT touched: those
            // rows are the drain's output, not this projection's, and the proven
            // behaviour is that the drain simply rescores them.
            tx.execute(
                "DELETE FROM content_scores WHERE factor = ?1 AND content_kind = 'post'",
                rusqlite::params![factor],
            )
            .context("withdraw list rows on a kind-changing republish")?;
        }
        tx.commit().context("commit put_labeler")?;
        Ok(())
    }

    /// Materialize a List labeler's `content_scores` rows (design D12a — the
    /// subscribe path; the report:spam nest-materialization shape). Idempotent
    /// (`INSERT OR REPLACE`, PK `(content_id, factor)`); writes only ids present
    /// in `content_meta` (the public/feed-participating gate — never a blind
    /// row), `actor_id = NULL` (a public post has no owner scope; feed
    /// composition JOINs by `content_id`), `tier = 3`, `scorer_version` = the
    /// stored labeler version (clamped to u32 like `subscribe_labeler_core`).
    /// A no-op for a `wasm` labeler (its projection is empty).
    pub async fn materialize_list_labeler_scores(&self, labeler_id: &[u8]) -> Result<usize> {
        let labeler_id = labeler_id.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT OR REPLACE INTO content_scores
                    (content_id, content_kind, factor, score, tier,
                     scorer_version, scored_at, actor_id)
                 SELECT lle.content_id, 'post', l.factor, lle.score, 3,
                        MIN(l.version, 4294967295), ?2, NULL
                   FROM labeler_list_entries lle
                   JOIN labelers l ON l.labeler_id = lle.labeler_id
                   JOIN content_meta cm ON cm.content_id = lle.content_id
                  WHERE lle.labeler_id = ?1",
                rusqlite::params![labeler_id, now],
            )
            .context("materialize list bus rows")?;
        Ok(n)
    }

    /// Withdraw every materialized `content_scores` row of one labeler factor
    /// (design D12c — the last-unsubscribe path; the report:spam withdraw
    /// precedent). The rows are derived from the stored artifact, so deleting
    /// them loses nothing a re-subscribe can't recreate.
    ///
    /// Posts only — the rows a List materializes. A community room's rows under
    /// the same factor are the room's derived view, and outlive any
    /// subscriber's unsubscribe (`super::room_labels`).
    pub async fn withdraw_labeler_factor_scores(&self, factor: &str) -> Result<usize> {
        let factor = factor.to_string();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM content_scores WHERE factor = ?1 AND content_kind = 'post'",
                rusqlite::params![factor],
            )
            .context("withdraw labeler bus rows")?;
        Ok(n)
    }

    /// Serve `fauna.labelers.list`: every catalog entry, metadata-only (no bytes),
    /// `labeler_id`-ordered for a stable browse.
    pub async fn list_labelers(&self) -> Result<Vec<LabelerSummaryRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT labeler_id, version, publisher_actor, content_kind, factor,
                    wasm_hash, wasm_size, artifact_kind, artifact_version
               FROM labelers
              ORDER BY labeler_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(LabelerSummaryRow {
                labeler_id: row.get(0)?,
                version: row.get::<_, i64>(1)?.clamp(0, i64::MAX) as u64,
                publisher_actor: row.get(2)?,
                content_kind: row.get(3)?,
                factor: row.get(4)?,
                wasm_hash: row.get(5)?,
                wasm_size: row.get::<_, i64>(6)?.clamp(0, i64::MAX) as u64,
                artifact_kind: row.get(7)?,
                artifact_version: row.get::<_, i64>(8)?.clamp(0, i64::MAX) as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Fetch one labeler's full record (incl. `metadata_blob` + `wasm_bytes`) by
    /// its `labeler_id` — the `inspect` read + the subscribe lookup. `None` if
    /// absent.
    pub async fn get_labeler(&self, labeler_id: &[u8]) -> Result<Option<LabelerRecord>> {
        let labeler_id = labeler_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT labeler_id, version, publisher_actor, content_kind, factor,
                    wasm_hash, wasm_size, metadata_blob, wasm_bytes, artifact_kind
               FROM labelers WHERE labeler_id = ?1",
            rusqlite::params![labeler_id],
            |row| {
                Ok(LabelerRecord {
                    labeler_id: row.get(0)?,
                    version: row.get::<_, i64>(1)?.clamp(0, i64::MAX) as u64,
                    publisher_actor: row.get(2)?,
                    content_kind: row.get(3)?,
                    factor: row.get(4)?,
                    wasm_hash: row.get(5)?,
                    wasm_size: row.get::<_, i64>(6)?.clamp(0, i64::MAX) as u64,
                    metadata_blob: row.get(7)?,
                    wasm_bytes: row.get(8)?,
                    artifact_kind: row.get(9)?,
                })
            },
        )
        .optional()
        .context("get labeler")
    }

    /// Record a subscription. `INSERT OR REPLACE` keyed on `(owner_actor,
    /// labeler_id)` — idempotent + re-subscribe-updates-the-registered-version.
    /// `grant_id` is `None` for a public-only subscription.
    pub async fn put_subscription(
        &self,
        owner_actor: &[u8],
        labeler_id: &[u8],
        grant_id: Option<&[u8]>,
        subscribed_ver: u64,
    ) -> Result<()> {
        let owner_actor = owner_actor.to_vec();
        let labeler_id = labeler_id.to_vec();
        let grant_id = grant_id.map(|g| g.to_vec());
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO labeler_subscriptions
                (owner_actor, labeler_id, grant_id, subscribed_ver, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                owner_actor,
                labeler_id,
                grant_id,
                subscribed_ver as i64,
                now
            ],
        )
        .context("put labeler subscription")?;
        Ok(())
    }

    /// Drop a subscription (unsubscribe). Idempotent — returns whether a row
    /// existed.
    pub async fn delete_subscription(&self, owner_actor: &[u8], labeler_id: &[u8]) -> Result<bool> {
        let owner_actor = owner_actor.to_vec();
        let labeler_id = labeler_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM labeler_subscriptions
                  WHERE owner_actor = ?1 AND labeler_id = ?2",
                rusqlite::params![owner_actor, labeler_id],
            )
            .context("delete labeler subscription")?;
        Ok(n > 0)
    }

    /// Read one subscription by its `(owner_actor, labeler_id)` key — `None` if
    /// absent (the subscribe/unsubscribe test read).
    pub async fn get_subscription(
        &self,
        owner_actor: &[u8],
        labeler_id: &[u8],
    ) -> Result<Option<LabelerSubscriptionRow>> {
        let owner_actor = owner_actor.to_vec();
        let labeler_id = labeler_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT owner_actor, labeler_id, grant_id, subscribed_ver
               FROM labeler_subscriptions
              WHERE owner_actor = ?1 AND labeler_id = ?2",
            rusqlite::params![owner_actor, labeler_id],
            |row| {
                Ok(LabelerSubscriptionRow {
                    owner_actor: row.get(0)?,
                    labeler_id: row.get(1)?,
                    grant_id: row.get(2)?,
                    subscribed_ver: row.get::<_, i64>(3)?.clamp(0, i64::MAX) as u64,
                })
            },
        )
        .optional()
        .context("get labeler subscription")
    }

    /// Count subscriptions for one labeler (the `idx_labeler_subs_labeler` scan);
    /// a small helper for tests / a future "how many subscribers" projection.
    pub async fn count_subscriptions_for_labeler(&self, labeler_id: &[u8]) -> Result<i64> {
        let labeler_id = labeler_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM labeler_subscriptions WHERE labeler_id = ?1",
            rusqlite::params![labeler_id],
            |r| r.get(0),
        )
        .context("count labeler subscriptions")
    }

    /// The caller's own subscribed `labeler_id`s (the `list` browse surface's
    /// per-row `subscribed` flag — `list_labelers_core`). A plain owner-scoped
    /// scan of `labeler_subscriptions`; small (a user subscribes to at most a
    /// handful of community labelers), so an in-memory set is the simplest shape
    /// for the caller to test membership against.
    pub async fn list_subscribed_labeler_ids(
        &self,
        owner_actor: &[u8],
    ) -> Result<std::collections::HashSet<Vec<u8>>> {
        let owner_actor = owner_actor.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT labeler_id FROM labeler_subscriptions WHERE owner_actor = ?1")?;
        let rows = stmt.query_map(rusqlite::params![owner_actor], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Seed the per-user labeler re-score obligation for ONE freshly-delivered
    /// mail item — the **delivery-time analog** of
    /// [`CacheDb::seed_factor_backlog`] (which seeds the whole backlog at
    /// subscribe time). For each of `owner`'s subscribed **WASM** mail labelers,
    /// `INSERT OR IGNORE` a placeholder `labeler:<hex>` `content_scores` row
    /// (`score 0`, `tier` COMMUNITY, `scorer_version 0`) keyed on this item, so it
    /// reads as "behind version 0 < the registered version" and a capability
    /// holder drains it — closing the gap that a mail delivered *after*
    /// subscription owes zero drain work (design spec
    /// `2026-07-07-phase-3-sealed-both-modes-design.md` D5; `content-scoring.md`
    /// § Timing → *Delivery-time fast path*).
    ///
    /// Only WASM/mail labelers: List labelers (`artifact_kind='list'`) are
    /// deliberately kept out of `model_versions` (they materialize scores
    /// directly, no drain — `subscribe_labeler_core`), so a version-0 placeholder
    /// would never drain; a non-mail labeler has no mail item. Idempotent
    /// (`INSERT OR IGNORE` on the `(content_id, factor)` PK); owner-scoped (the
    /// same confidentiality boundary as the owner-scoped worklist). Returns the
    /// number of obligation rows seeded — 0 (no subscribed WASM mail labeler)
    /// means no obligation and the caller emits no rescore-ready nudge.
    pub async fn seed_new_mail_labeler_obligations(
        &self,
        content_id: &[u8; 32],
        owner: &[u8; 32],
    ) -> Result<usize> {
        let content_id = content_id.to_vec();
        let owner_vec = owner.to_vec();
        let scored_at = now_epoch_secs();
        let conn = self.conn.lock().await;
        let seeded = conn
            .execute(
                "INSERT OR IGNORE INTO content_scores
                    (content_id, content_kind, factor, score, tier,
                     scorer_version, scored_at, actor_id)
                 SELECT ?1, 'mail', l.factor, 0, ?2, 0, ?3, ?4
                   FROM labeler_subscriptions s
                   JOIN labelers l ON l.labeler_id = s.labeler_id
                  WHERE s.owner_actor = ?4
                    AND l.artifact_kind = 'wasm'
                    AND l.content_kind = 'mail'",
                rusqlite::params![
                    content_id,
                    fauna_core::scoring::TIER_COMMUNITY as i64,
                    scored_at,
                    owner_vec,
                ],
            )
            .context("seed new-mail labeler obligations")?;
        Ok(seeded)
    }
}

/// Post-arrival join (design D12d): when a `content_meta` row is created for a
/// content id a **subscribed** List labeler already lists, write its bus row
/// immediately — the ingest-time report-aggregate join posture
/// (`bridge_routing_handlers.rs` § ingest-time join). Sync, called under the
/// caller's connection lock from the two `content_meta` insert sites
/// (`meta::upsert_meta`, `feeds::insert_post_index_entry`); cheap — one
/// indexed lookup on `labeler_list_entries(content_id)`, and the common case
/// (no list mentions the id) touches nothing. Callers treat a failure as
/// best-effort (log, never fail the content insert).
pub(crate) fn join_list_labeler_scores_for_content(
    conn: &rusqlite::Connection,
    content_id: &[u8],
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR REPLACE INTO content_scores
            (content_id, content_kind, factor, score, tier,
             scorer_version, scored_at, actor_id)
         SELECT lle.content_id, 'post', l.factor, lle.score, 3,
                MIN(l.version, 4294967295), ?2, NULL
           FROM labeler_list_entries lle
           JOIN labelers l ON l.labeler_id = lle.labeler_id
          WHERE lle.content_id = ?1
            AND l.artifact_kind = 'list'
            AND EXISTS (SELECT 1 FROM labeler_subscriptions s
                         WHERE s.labeler_id = lle.labeler_id)",
        rusqlite::params![content_id, now_epoch_millis()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: u8, version: u64) -> PutLabelerOwned {
        PutLabelerOwned {
            labeler_id: vec![id; 32],
            version,
            publisher_actor: vec![id; 32],
            caller_actor: vec![id; 32],
            content_kind: "post".into(),
            factor: format!("labeler:{}", hex::encode([id; 32])),
            wasm_hash: vec![0xAB; 36],
            wasm_size: 128,
            metadata_blob: vec![0xCD; 64],
            wasm_bytes: vec![0x00; 128],
            artifact_kind: "wasm".into(),
            artifact_version: 0,
            list_entries: vec![],
        }
    }

    /// Owned mirror of `PutLabeler<'_>` so tests can hold the buffers.
    struct PutLabelerOwned {
        labeler_id: Vec<u8>,
        version: u64,
        publisher_actor: Vec<u8>,
        caller_actor: Vec<u8>,
        content_kind: String,
        factor: String,
        wasm_hash: Vec<u8>,
        wasm_size: u64,
        metadata_blob: Vec<u8>,
        wasm_bytes: Vec<u8>,
        artifact_kind: String,
        artifact_version: u64,
        list_entries: Vec<(Vec<u8>, i64)>,
    }

    impl PutLabelerOwned {
        fn as_ref(&self) -> PutLabeler<'_> {
            PutLabeler {
                labeler_id: &self.labeler_id,
                version: self.version,
                publisher_actor: &self.publisher_actor,
                caller_actor: &self.caller_actor,
                content_kind: &self.content_kind,
                factor: &self.factor,
                wasm_hash: &self.wasm_hash,
                wasm_size: self.wasm_size,
                metadata_blob: &self.metadata_blob,
                wasm_bytes: &self.wasm_bytes,
                artifact_kind: &self.artifact_kind,
                artifact_version: self.artifact_version,
                list_entries: &self.list_entries,
            }
        }
    }

    /// The `meta::upsert_meta` post-arrival hook (design D12d): a List entry's
    /// content arriving via `write_post_index` gets its bus row. (The
    /// `feeds::insert_post_index_entry` hook is pinned by the integration
    /// lifecycle test; this covers the second `content_meta` insert site.)
    #[tokio::test]
    async fn upsert_meta_join_writes_subscribed_list_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let content_id = [0x0Cu8; 32];
        let mut l = sample(0x77, 1);
        l.artifact_kind = "list".into();
        l.list_entries = vec![(content_id.to_vec(), 650)];
        db.put_labeler(l.as_ref()).await.unwrap();
        db.put_subscription(&[0x42u8; 32], &l.labeler_id, None, 1)
            .await
            .unwrap();
        // Content not on the nest yet → no row was materialized at subscribe
        // (put_subscription alone doesn't materialize; the handler core does —
        // this test drives the arrival side only).
        assert!(db.get_content_scores(&content_id).await.unwrap().is_empty());
        {
            let conn = db.conn.lock().await;
            crate::db::meta::upsert_meta(&conn, &content_id, 0.0, false, false, None, None, None)
                .unwrap();
        }
        let scores = db.get_content_scores(&content_id).await.unwrap();
        assert_eq!(scores.len(), 1, "upsert_meta must fire the arrival join");
        assert_eq!(scores[0].factor, l.factor);
        assert_eq!(scores[0].score, 650);
        assert_eq!(scores[0].tier, 3);
        // An UNSUBSCRIBED list never joins: re-publish at v2 with a second
        // entry, drop the subscription, then that entry's content arrives →
        // no row.
        let content_id2 = [0x0Du8; 32];
        let mut l2 = sample(0x77, 2);
        l2.artifact_kind = "list".into();
        l2.list_entries = vec![(content_id.to_vec(), 650), (content_id2.to_vec(), 700)];
        db.put_labeler(l2.as_ref()).await.unwrap();
        db.delete_subscription(&[0x42u8; 32], &l.labeler_id)
            .await
            .unwrap();
        {
            let conn = db.conn.lock().await;
            crate::db::meta::upsert_meta(&conn, &content_id2, 0.0, false, false, None, None, None)
                .unwrap();
        }
        assert!(
            db.get_content_scores(&content_id2)
                .await
                .unwrap()
                .is_empty(),
            "an unsubscribed list must not join on arrival"
        );
    }

    /// S5 Arm 1: the delivery-time seed creates one behind-version-0
    /// `labeler:<hex>` obligation per subscribed WASM/mail labeler, idempotently,
    /// and ignores List labelers, non-mail labelers, and unsubscribed owners.
    #[tokio::test]
    async fn seed_new_mail_labeler_obligations_seeds_subscribed_wasm_mail_only() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x42u8; 32];
        let content_id = [0xA1u8; 32];

        // A subscribed WASM mail labeler → seeded.
        let mut mail_l = sample(0x11, 1);
        mail_l.content_kind = "mail".into();
        db.put_labeler(mail_l.as_ref()).await.unwrap();
        db.put_subscription(&owner, &mail_l.labeler_id, Some(&[0x01; 16]), 1)
            .await
            .unwrap();

        // A subscribed WASM *post* labeler → NOT seeded (wrong kind for a mail item).
        let post_l = sample(0x22, 1); // content_kind defaults to "post"
        db.put_labeler(post_l.as_ref()).await.unwrap();
        db.put_subscription(&owner, &post_l.labeler_id, Some(&[0x02; 16]), 1)
            .await
            .unwrap();

        // A subscribed *List* mail labeler → NOT seeded (materializes directly, no drain).
        let mut list_l = sample(0x33, 1);
        list_l.content_kind = "mail".into();
        list_l.artifact_kind = "list".into();
        db.put_labeler(list_l.as_ref()).await.unwrap();
        db.put_subscription(&owner, &list_l.labeler_id, None, 1)
            .await
            .unwrap();

        let seeded = db
            .seed_new_mail_labeler_obligations(&content_id, &owner)
            .await
            .unwrap();
        assert_eq!(
            seeded, 1,
            "only the subscribed WASM mail labeler seeds a row"
        );

        let scores = db.get_content_scores(&content_id).await.unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].factor, mail_l.factor);
        assert_eq!(scores[0].score, 0);
        assert_eq!(scores[0].tier, fauna_core::scoring::TIER_COMMUNITY);
        assert_eq!(scores[0].scorer_version, 0);

        // Idempotent: a second call seeds nothing (INSERT OR IGNORE on the PK) and
        // never clobbers a since-drained score.
        let again = db
            .seed_new_mail_labeler_obligations(&content_id, &owner)
            .await
            .unwrap();
        assert_eq!(again, 0, "re-seeding the same item is a no-op");

        // An owner with no subscription owes nothing.
        let other = [0x99u8; 32];
        let none = db
            .seed_new_mail_labeler_obligations(&content_id, &other)
            .await
            .unwrap();
        assert_eq!(none, 0, "an owner with no subscription owes nothing");
    }

    #[tokio::test]
    async fn round_trip_labeler_and_subscription() {
        let db = CacheDb::open_in_memory().unwrap();
        let l = sample(0x11, 1);
        db.put_labeler(l.as_ref()).await.unwrap();

        // list projects metadata-only
        let list = db.list_labelers().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].labeler_id, l.labeler_id);
        assert_eq!(list[0].version, 1);
        assert_eq!(list[0].factor, l.factor);
        assert_eq!(list[0].content_kind, "post");

        // get returns the full record incl. bytes
        let rec = db.get_labeler(&l.labeler_id).await.unwrap().unwrap();
        assert_eq!(rec.metadata_blob, l.metadata_blob);
        assert_eq!(rec.wasm_bytes, l.wasm_bytes);

        // subscribe → row present, then unsubscribe → gone (idempotent)
        let owner = [0x99u8; 32];
        db.put_subscription(&owner, &l.labeler_id, Some(&[0x77; 16]), 1)
            .await
            .unwrap();
        let sub = db.get_subscription(&owner, &l.labeler_id).await.unwrap();
        assert_eq!(
            sub,
            Some(LabelerSubscriptionRow {
                owner_actor: owner.to_vec(),
                labeler_id: l.labeler_id.clone(),
                grant_id: Some(vec![0x77; 16]),
                subscribed_ver: 1,
            })
        );
        assert_eq!(
            db.count_subscriptions_for_labeler(&l.labeler_id)
                .await
                .unwrap(),
            1
        );
        assert!(db.delete_subscription(&owner, &l.labeler_id).await.unwrap());
        assert!(!db.delete_subscription(&owner, &l.labeler_id).await.unwrap());
        assert!(
            db.get_subscription(&owner, &l.labeler_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_republish_never_withdraws_a_rooms_rows_under_its_factor() {
        // Both of `put_labeler`'s List withdrawals — the dropped-entry resync
        // and the kind-changing republish — delete by factor. A community
        // room's rows can carry the same factor, and they are the room's
        // derived view (`super::super::room_labels`), not this projection.
        let db = CacheDb::open_in_memory().unwrap();
        let mut l = sample(0x5a, 1);
        l.artifact_kind = "list".into();
        db.put_labeler(l.as_ref()).await.unwrap();
        let room = [0x5bu8; 32];
        let factor_row = fauna_core::scoring::ScoreEntry {
            factor: l.factor.clone(),
            score: 900,
            tier: fauna_core::scoring::TIER_COMMUNITY,
            scorer_version: 1,
        };
        db.record_room_message_bus(&room, 1, &[], &[factor_row], &[0u8; 32])
            .await
            .unwrap();

        l.version = 2; // list → list: the dropped-entry resync
        db.put_labeler(l.as_ref()).await.unwrap();
        l.version = 3; // list → wasm: the kind-changing withdrawal
        l.artifact_kind = "wasm".into();
        db.put_labeler(l.as_ref()).await.unwrap();

        assert_eq!(db.count_room_message_bus_rows(&room).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn public_only_subscription_has_null_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        let l = sample(0x22, 3);
        db.put_labeler(l.as_ref()).await.unwrap();
        let owner = [0x33u8; 32];
        db.put_subscription(&owner, &l.labeler_id, None, 3)
            .await
            .unwrap();
        let sub = db
            .get_subscription(&owner, &l.labeler_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sub.grant_id, None);
        assert_eq!(sub.subscribed_ver, 3);
    }

    #[tokio::test]
    async fn put_rejects_stale_or_equal_version() {
        let db = CacheDb::open_in_memory().unwrap();
        let mut l = sample(0x44, 5);
        db.put_labeler(l.as_ref()).await.unwrap();

        // equal version rejected
        let err = db.put_labeler(l.as_ref()).await.unwrap_err();
        assert!(err.downcast_ref::<StaleLabelerVersion>().is_some());

        // lower version rejected
        l.version = 4;
        let err = db.put_labeler(l.as_ref()).await.unwrap_err();
        assert!(err.downcast_ref::<StaleLabelerVersion>().is_some());

        // strictly-newer version accepted (a re-publish), and bytes update
        l.version = 6;
        l.wasm_bytes = vec![0x01; 200];
        db.put_labeler(l.as_ref()).await.unwrap();
        let rec = db.get_labeler(&l.labeler_id).await.unwrap().unwrap();
        assert_eq!(rec.version, 6);
        assert_eq!(rec.wasm_bytes, vec![0x01; 200]);
        // still one row (replace, not insert)
        assert_eq!(db.list_labelers().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn put_rejects_oversize_wasm() {
        let db = CacheDb::open_in_memory().unwrap();
        let mut l = sample(0x55, 1);
        l.wasm_bytes = vec![0u8; MAX_LABELER_WASM_BYTES + 1];
        let err = db.put_labeler(l.as_ref()).await.unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[tokio::test]
    async fn put_rejects_over_publisher_quota() {
        let db = CacheDb::open_in_memory().unwrap();
        let publisher = vec![0x66u8; 32];
        for n in 0..MAX_LABELERS_PER_PUBLISHER {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            let p = PutLabeler {
                labeler_id: &id,
                version: 1,
                publisher_actor: &publisher,
                // A DISTINCT caller per row (== the unique labeler_id) so the
                // per-caller cap never fires — this test isolates the per-publisher
                // cap (both caps share the value 64).
                caller_actor: &id,
                content_kind: "post",
                factor: "labeler:x",
                wasm_hash: &[0xAB; 36],
                wasm_size: 4,
                metadata_blob: &[0xCD; 8],
                wasm_bytes: &[0u8; 4],
                artifact_kind: "wasm",
                artifact_version: 0,
                list_entries: &[],
            };
            db.put_labeler(p).await.unwrap();
        }
        // The next NEW labeler_id for this publisher is rejected.
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&(MAX_LABELERS_PER_PUBLISHER as u64).to_le_bytes());
        let over = PutLabeler {
            labeler_id: &id,
            version: 1,
            publisher_actor: &publisher,
            caller_actor: &id,
            content_kind: "post",
            factor: "labeler:x",
            wasm_hash: &[0xAB; 36],
            wasm_size: 4,
            metadata_blob: &[0xCD; 8],
            wasm_bytes: &[0u8; 4],
            artifact_kind: "wasm",
            artifact_version: 0,
            list_entries: &[],
        };
        let err = db.put_labeler(over).await.unwrap_err();
        assert!(err.downcast_ref::<LabelerQuotaExceeded>().is_some());
        // A different publisher still gets its own quota.
        let other = vec![0x77u8; 32];
        let p = PutLabeler {
            labeler_id: &id,
            version: 1,
            publisher_actor: &other,
            caller_actor: &id,
            content_kind: "post",
            factor: "labeler:x",
            wasm_hash: &[0xAB; 36],
            wasm_size: 4,
            metadata_blob: &[0xCD; 8],
            wasm_bytes: &[0u8; 4],
            artifact_kind: "wasm",
            artifact_version: 0,
            list_entries: &[],
        };
        db.put_labeler(p)
            .await
            .expect("a different publisher's quota is independent");
    }

    /// The DoS is a `publish` loop with a **fresh signing
    /// keypair per labeler** (a rotating `publisher_actor == algorithm_id`), which
    /// never trips the per-publisher cap (each publisher owns exactly one row). The
    /// per-CALLER cap — keyed on the un-rotatable authenticated `actor_id` — is
    /// what stops it: with the caller fixed, the 65th fresh-keypair publish is
    /// rejected `LabelerCallerQuotaExceeded`.
    #[tokio::test]
    async fn put_rejects_over_caller_quota_despite_rotating_publisher() {
        let db = CacheDb::open_in_memory().unwrap();
        let caller = vec![0xAAu8; 32]; // the fixed, un-rotatable enrolled identity
        for n in 0..MAX_LABELERS_PER_CALLER {
            // Both the labeler_id AND the publisher_actor rotate per row — modelling
            // a fresh Ed25519 keypair each publish (the evasion the old cap missed).
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            let p = PutLabeler {
                labeler_id: &id,
                version: 1,
                publisher_actor: &id, // fresh keypair each time → per-publisher cap never fires
                caller_actor: &caller,
                content_kind: "post",
                factor: "labeler:x",
                wasm_hash: &[0xAB; 36],
                wasm_size: 4,
                metadata_blob: &[0xCD; 8],
                wasm_bytes: &[0u8; 4],
                artifact_kind: "wasm",
                artifact_version: 0,
                list_entries: &[],
            };
            db.put_labeler(p)
                .await
                .expect("each fresh-keypair publish is under the per-publisher cap");
        }
        // The 65th fresh-keypair publish by the same caller is now rejected — the
        // rotation-proof cap.
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&(MAX_LABELERS_PER_CALLER as u64).to_le_bytes());
        let over = PutLabeler {
            labeler_id: &id,
            version: 1,
            publisher_actor: &id,
            caller_actor: &caller,
            content_kind: "post",
            factor: "labeler:x",
            wasm_hash: &[0xAB; 36],
            wasm_size: 4,
            metadata_blob: &[0xCD; 8],
            wasm_bytes: &[0u8; 4],
            artifact_kind: "wasm",
            artifact_version: 0,
            list_entries: &[],
        };
        let err = db.put_labeler(over).await.unwrap_err();
        assert!(
            err.downcast_ref::<LabelerCallerQuotaExceeded>().is_some(),
            "rotating the signing keypair must not evade the per-caller cap"
        );
        // A different caller still has its own independent quota.
        let other_caller = vec![0xBBu8; 32];
        let p = PutLabeler {
            labeler_id: &id,
            version: 1,
            publisher_actor: &id,
            caller_actor: &other_caller,
            content_kind: "post",
            factor: "labeler:x",
            wasm_hash: &[0xAB; 36],
            wasm_size: 4,
            metadata_blob: &[0xCD; 8],
            wasm_bytes: &[0u8; 4],
            artifact_kind: "wasm",
            artifact_version: 0,
            list_entries: &[],
        };
        db.put_labeler(p)
            .await
            .expect("a different caller's quota is independent");
    }

    /// The deployment-wide byte budget bounds the total stored
    /// catalog even when count caps don't (many callers, or a re-publish that
    /// grows a module). `wasm_size` is decoupled from `wasm_bytes` at the DB layer
    /// (the handler binds them; the per-labeler size cap gates the actual bytes),
    /// so this drives the SUM accounting with a huge declared `wasm_size` over tiny
    /// bytes — no gigabyte allocation.
    #[tokio::test]
    async fn put_rejects_over_global_byte_budget() {
        let db = CacheDb::open_in_memory().unwrap();
        // Row 1 declares the whole budget — projected == max is allowed (not `>`).
        let full = PutLabeler {
            labeler_id: &[0x01u8; 32],
            version: 1,
            publisher_actor: &[0x01u8; 32],
            caller_actor: &[0x01u8; 32],
            content_kind: "post",
            factor: "labeler:x",
            wasm_hash: &[0xAB; 36],
            wasm_size: MAX_TOTAL_LABELER_BYTES,
            metadata_blob: &[0xCD; 8],
            wasm_bytes: &[0u8; 4],
            artifact_kind: "wasm",
            artifact_version: 0,
            list_entries: &[],
        };
        db.put_labeler(full)
            .await
            .expect("a catalog exactly at the budget is allowed");
        // Row 2 (distinct caller + publisher, so no count cap fires) tips it over.
        let over = PutLabeler {
            labeler_id: &[0x02u8; 32],
            version: 1,
            publisher_actor: &[0x02u8; 32],
            caller_actor: &[0x02u8; 32],
            content_kind: "post",
            factor: "labeler:x",
            wasm_hash: &[0xAB; 36],
            wasm_size: 1,
            metadata_blob: &[0xCD; 8],
            wasm_bytes: &[0u8; 4],
            artifact_kind: "wasm",
            artifact_version: 0,
            list_entries: &[],
        };
        let err = db.put_labeler(over).await.unwrap_err();
        assert!(
            err.downcast_ref::<LabelerByteQuotaExceeded>().is_some(),
            "the deployment-wide byte budget must reject a catalog past the cap"
        );
    }
}
