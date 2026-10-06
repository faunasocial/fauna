//! The region tier's nest state — the declared region, the last-known-good
//! artifact store, and the refresh log (`dynamic-features.md` § The region tier,
//! § Fail posture; `region-blocking.md` § The region/authority plumbing).
//!
//! Four things live here and each answers a different question:
//!
//! - **which log history this nest has accepted** (`region_log_anchor`) — the
//!   last transparency-log head inclusion evidence was checked against
//!   (`region-blocking.md` § The transparency log, *head monotonicity*).
//!   Absent until the first witnessed acceptance; for a build whose
//!   compiled-in anchor is `None`, absent **is** the pre-log era.
//! - **which region claims this deployment** (`nest_region`) — *declared, never
//!   detected*, so this is a value an admin wrote and nothing on the box ever
//!   infers. Absent is the ratified fresh-install state.
//! - **which artifact is in force** (`region_artifacts`) — the whole signed
//!   envelope plus the sequence that makes accepting a newer one the *only* way
//!   to change it. **This table is the fail posture**: § Fail posture's
//!   last-known-good is not a code path, it is the fact that a refused or failed
//!   refresh writes nothing, so the row that was binding keeps binding.
//! - **when the channel was last reached** (`region_refresh.reached_at`) — the
//!   input to the admin-side staleness *warning*, which § Fail posture is
//!   explicit is *"a warning, not an outage"*. `region_refresh.checked_at` is
//!   the last *attempt* (success or not) and never drives staleness on its
//!   own — a worker that keeps trying and failing must not read as fresh.
//!
//! ⚠ **`reached_at` and `accepted_at` are deliberately different clocks.** A
//! nest whose fetches all succeed while the authority publishes nothing is
//! perfectly current; keying staleness off the last *acceptance* would warn
//! about a quiet authority, which is exactly the false alarm that teaches an
//! admin to ignore the warning.

use anyhow::{Context, Result};
use fauna_core::region_authority::{
    AnchorState, ObjectId, PolicyArtifact, RegionCode, VerifiedArtifact,
};
use rusqlite::OptionalExtension;

/// One accepted artifact, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredArtifact {
    pub region: RegionCode,
    pub payload_kind: String,
    pub sequence: u64,
    pub issued_at: u64,
    pub key_id: String,
    /// The authority's name as the registry gave it **when this artifact was
    /// accepted** — stored rather than re-looked-up, so a document that is still
    /// binding always has a name to show (see the migration's note).
    pub authority_name: String,
    pub envelope: Vec<u8>,
    pub accepted_at: i64,
}

impl StoredArtifact {
    /// Decode the stored envelope.
    ///
    /// Fallible rather than infallible on purpose: the bytes were verified when
    /// they were stored, but a *newer* build could in principle read them with a
    /// stricter decoder, and the caller's answer to that is to ignore the row —
    /// never to panic on its own database.
    pub fn artifact(&self) -> Result<PolicyArtifact> {
        fauna_protocol::decode_strict(&self.envelope).context("decode stored region artifact")
    }
}

/// The outcome of the most recent refresh attempt for one payload kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshState {
    /// The last *attempt* (success or not) — diagnostic only; staleness never
    /// reads this directly (see [`Self::reached_at`]).
    pub checked_at: i64,
    pub ok: bool,
    pub detail: Option<String>,
    /// The first attempt ever recorded for this payload kind — set only on
    /// the row's `INSERT`, never updated afterward. The anchor a kind that has
    /// never been reached is measured from.
    pub first_attempted_at: i64,
    /// The last time the channel **answered** (an accepted or a refused
    /// artifact — a refusal is a reached channel; a fetch failure is not).
    /// `None` until the channel has been reached even once. What the
    /// admin-side staleness warning reads (`crate::region_tier::refresh_staleness`),
    /// matching the relay's `region_relay_cache.reached_at`.
    pub reached_at: Option<i64>,
}

/// The declaration as the row actually is — the three-state read the *report*
/// paths and the declaration write seam use (`nest/common.md` § Unreadable
/// stored values; `dynamic-features.md` § Fail posture, the
/// unreadable-declaration clause).
///
/// Enforcement paths keep using [`CacheDb::get_declared_region`], which folds
/// `Unreadable` into `Undeclared` — for *enforcement* the two are ratified to
/// mean the same thing (an unreadable situs restricts nobody). The reads that
/// *name* the state must not inherit that fold: "the admin never declared" and
/// "the declaration row is corrupt, and the last-accepted document still binds"
/// are different answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredRegion {
    /// No row — the ratified fresh-install state.
    Undeclared,
    /// A well-formed declaration.
    Declared(RegionCode),
    /// A row exists but its text is not a well-formed [`RegionCode`] —
    /// corruption, not absence. Deliberately carries no payload: the raw text
    /// is garbage by definition, and handing it out invites rendering it.
    Unreadable,
}

impl crate::db::CacheDb {
    /// The region this deployment declares, or `None` if the admin has never
    /// declared one.
    ///
    /// A row whose text is not a well-formed [`RegionCode`] reads as `None`
    /// rather than propagating: an unreadable declaration is *no* declaration,
    /// which lands on § Fail posture's ratified "a deployment no region claims"
    /// arm — gated features ON at tier-1 constants.
    ///
    /// ⚠ **This is the opposite direction from an unreadable policy *document*,
    /// and the difference is which object is unreadable.** A document is an
    /// authored restriction, so failing to read one denies at its tier
    /// (§ Fail posture — the undecodable-document clause,
    /// `db::feature_gate::feature_policies_for`). A *declaration* is the
    /// deployment's situs — an unreadable one means nobody is known to claim
    /// this nest, which is a ratified, fully-open state, not a restriction with
    /// unknown contents. Note what does **not** move either way: the Region-tier
    /// rows a successful fold already wrote keep binding, because the re-fold
    /// simply stops running rather than clearing them.
    ///
    /// ⚠ **This fold is for enforcement only.** A surface that *reports* the
    /// declaration (the admin read, the transparency read) or *retires* state
    /// keyed on it (the declaration write seam) must use
    /// [`Self::declared_region_state`] instead — folding corruption into
    /// absence there silently mis-names a still-binding document
    /// (`nest/common.md` § Unreadable stored values).
    pub async fn get_declared_region(&self) -> Result<Option<RegionCode>> {
        Ok(match self.declared_region_state().await? {
            DeclaredRegion::Declared(code) => Some(code),
            DeclaredRegion::Undeclared | DeclaredRegion::Unreadable => None,
        })
    }

    /// The declaration without the enforcement fold — see [`DeclaredRegion`]
    /// for who reads which.
    pub async fn declared_region_state(&self) -> Result<DeclaredRegion> {
        let raw: Option<String> = self.get_singleton_column("nest_region", "region").await?;
        Ok(match raw {
            None => DeclaredRegion::Undeclared,
            Some(raw) => match RegionCode::parse(raw) {
                Ok(code) => DeclaredRegion::Declared(code),
                Err(e) => {
                    tracing::warn!("declared region is unreadable, treating as undeclared: {e}");
                    DeclaredRegion::Unreadable
                }
            },
        })
    }

    /// Declare (or re-declare) this deployment's region.
    ///
    /// `mark_publishing_renders` carries the owed-render mark for every
    /// publishing site **in this write's own transaction** — see
    /// [`Self::put_relay_artifact`] for why the four policy-moving writes all
    /// take it. The caller passes whether the declaration actually moved: a
    /// re-declaration of the same region changes nothing the public pages
    /// fold, and owes no render.
    pub async fn set_declared_region(
        &self,
        region: &RegionCode,
        mark_publishing_renders: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin set_declared_region tx")?;
        super::singleton::set_singleton_column_in(&tx, "nest_region", "region", region.as_str())?;
        if mark_publishing_renders {
            super::web::mark_web_render_owed_for_publishing_actors(&tx)?;
        }
        tx.commit().context("commit set_declared_region")?;
        Ok(())
    }

    /// Withdraw the declaration.
    ///
    /// **Withdrawing does not by itself unbind an accepted policy** — the caller
    /// clears the tier's documents, because "no region claims this deployment"
    /// and "this region's authority allows everything" are different states and
    /// only the first is expressed by an absent declaration. Returns whether a
    /// declaration existed.
    /// `mark_publishing_renders` is [`Self::set_declared_region`]'s.
    pub async fn clear_declared_region(&self, mark_publishing_renders: bool) -> Result<bool> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin clear_declared_region tx")?;
        let n = tx
            .execute("DELETE FROM nest_region WHERE id = 1", [])
            .context("clear declared region")?;
        if mark_publishing_renders {
            super::web::mark_web_render_owed_for_publishing_actors(&tx)?;
        }
        tx.commit().context("commit clear_declared_region")?;
        Ok(n > 0)
    }

    /// The artifact currently in force for this (region, payload kind), if one
    /// has ever been accepted.
    pub async fn get_region_artifact(
        &self,
        region: &RegionCode,
        payload_kind: &str,
    ) -> Result<Option<StoredArtifact>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT sequence, issued_at, key_id, authority_name, envelope, accepted_at
             FROM region_artifacts WHERE region = ?1 AND payload_kind = ?2",
            rusqlite::params![region.as_str(), payload_kind],
            |row| {
                Ok(StoredArtifact {
                    region: region.clone(),
                    payload_kind: payload_kind.to_string(),
                    sequence: row.get::<_, i64>(0)? as u64,
                    issued_at: row.get::<_, i64>(1)? as u64,
                    key_id: row.get(2)?,
                    authority_name: row.get(3)?,
                    envelope: row.get(4)?,
                    accepted_at: row.get(5)?,
                })
            },
        )
        .optional()
        .context("get region artifact")
    }

    /// The highest sequence ever accepted from `authority_name` for this
    /// (region, payload kind) — what `verify_artifact` refuses a replay
    /// against.
    ///
    /// **Read from `region_sequence_floor`, deliberately NOT from the stored
    /// artifact row.** It used to be `get_region_artifact(..).map(|a|
    /// a.sequence)`, which made the replay floor a projection of a row that
    /// [`Self::clear_region_artifacts_except`] and [`Self::clear_region_artifacts`]
    /// both hard-delete: withdraw-then-re-declare the same region reset the
    /// floor to `None`, and a `None` floor makes `verify_artifact` skip the
    /// monotonicity check entirely, so an older still-validly-signed artifact
    /// was accepted again. The floor now outlives
    /// every retirement — `region-blocking.md` § Fail posture.
    ///
    /// Scoped to the authority because the sequence counter is the
    /// *authority's*; see the `region_sequence_floor` note in `migrations.rs`
    /// for why rotation keeps the floor and re-curation does not.
    pub async fn accepted_region_sequence(
        &self,
        region: &RegionCode,
        payload_kind: &str,
        authority_name: &str,
    ) -> Result<Option<u64>> {
        let region = region.as_str().to_string();
        let payload_kind = payload_kind.to_string();
        let authority_name = authority_name.to_string();
        let conn = self.conn.lock().await;
        let sequence: Option<i64> = conn
            .query_row(
                "SELECT sequence FROM region_sequence_floor
                 WHERE region = ?1 AND payload_kind = ?2 AND authority_name = ?3",
                rusqlite::params![region, payload_kind, authority_name],
                |row| row.get(0),
            )
            .optional()
            .context("read region sequence floor")?;
        Ok(sequence.map(|s| s as u64))
    }

    /// Record an accepted artifact as the one now in force.
    ///
    /// Takes a [`VerifiedArtifact`] rather than raw bytes so that "stored"
    /// implies "verified" at the type level — the store cannot be handed
    /// something that merely looks like an artifact.
    pub async fn put_region_artifact(&self, verified: &VerifiedArtifact) -> Result<()> {
        let artifact = verified.artifact();
        let envelope =
            fauna_protocol::encode_canonical(artifact).context("encode region artifact")?;
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        // Finding: the artifact row and the floor raise must commit
        // atomically — two independent autocommit `execute`s let a crash, OOM
        // kill or SQLITE_FULL between them land the artifact at sequence N with
        // the floor still at f < N, and any validly-signed older artifact in
        // (f, N) then passes `accepted_region_sequence`'s monotonicity check
        // (the exposure by another route). `unchecked_transaction`,
        // not two bare `execute`s — the idiom this db layer already uses at
        // `nest_rotation.rs`/`posts.rs`/`bridge_carddav.rs`.
        let tx = conn
            .unchecked_transaction()
            .context("begin put_region_artifact tx")?;
        tx.execute(
            "INSERT INTO region_artifacts
                 (region, payload_kind, sequence, issued_at, key_id, authority_name,
                  envelope, accepted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (region, payload_kind) DO UPDATE SET
                 sequence = excluded.sequence,
                 issued_at = excluded.issued_at,
                 key_id = excluded.key_id,
                 authority_name = excluded.authority_name,
                 envelope = excluded.envelope,
                 accepted_at = excluded.accepted_at",
            rusqlite::params![
                artifact.region.as_str(),
                artifact.payload_kind,
                artifact.sequence as i64,
                artifact.issued_at as i64,
                artifact.key_id,
                verified.authority_name(),
                envelope.to_vec(),
                now,
            ],
        )
        .context("put region artifact")?;
        // Raise the replay floor in the same TRANSACTION that records the
        // acceptance, so the two can never disagree. `MAX` rather than
        // assignment: the floor only ever climbs, and it must survive a
        // retirement that the artifact row does not.
        tx.execute(
            "INSERT INTO region_sequence_floor
                 (region, payload_kind, authority_name, sequence, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (region, payload_kind, authority_name) DO UPDATE SET
                 sequence = MAX(sequence, excluded.sequence),
                 updated_at = excluded.updated_at",
            rusqlite::params![
                artifact.region.as_str(),
                artifact.payload_kind,
                verified.authority_name(),
                artifact.sequence as i64,
                now,
            ],
        )
        .context("raise region sequence floor")?;
        tx.commit().context("commit put_region_artifact")?;
        Ok(())
    }

    /// Drop every accepted artifact for a region — the withdrawal path, and the
    /// only way a stored artifact ever leaves the store (with
    /// [`Self::clear_region_artifacts_except`], its change-of-situs sibling).
    ///
    /// **Leaves `region_sequence_floor` alone, on purpose.** This clears the
    /// *binding*, never the *replay defence*: an artifact this nest already
    /// accepted must stay un-replayable even after its document stops binding,
    /// because it stays validly signed forever
    /// ([`Self::accepted_region_sequence`], row 183).
    pub async fn clear_region_artifacts(&self, region: &RegionCode) -> Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM region_artifacts WHERE region = ?1",
            rusqlite::params![region.as_str()],
        )
        .context("clear region artifacts")
    }

    /// Drop every accepted artifact **not** belonging to `keep` (every artifact
    /// at all when `keep` is `None`) — the change-of-situs retirement, keyed on
    /// what must *survive* rather than on what must go.
    ///
    /// That inversion is load-bearing: the declaration write seam cannot name
    /// the outgoing region when the stored declaration is unreadable, and a
    /// retirement keyed on the (unparseable) previous value would silently skip
    /// — leaving the old region's artifacts to coexist with the new one's,
    /// which is exactly the multi-region state the store must never hold
    /// (`nest/common.md` § Unreadable stored values).
    ///
    /// **`region_sequence_floor` is deliberately not swept with it.** Retiring
    /// a region's binding must not retire the sequences already accepted for
    /// it: before row 183 the floor was projected off the very rows this
    /// statement deletes, so a withdraw/re-declare round trip re-opened the
    /// replay window on the region it just restored.
    pub async fn clear_region_artifacts_except(&self, keep: Option<&RegionCode>) -> Result<usize> {
        let conn = self.conn.lock().await;
        match keep {
            Some(region) => conn
                .execute(
                    "DELETE FROM region_artifacts WHERE region != ?1",
                    rusqlite::params![region.as_str()],
                )
                .context("clear other regions' artifacts"),
            None => conn
                .execute("DELETE FROM region_artifacts", [])
                .context("clear all region artifacts"),
        }
    }

    /// The artifact in force for one payload kind, **without knowing the
    /// region** — `Some` only when the store holds exactly one row for the
    /// kind.
    ///
    /// This is the identity-recovery read behind an unreadable declaration
    /// (`dynamic-features.md` § Fail posture): the declaration is the normal
    /// lookup key, but the store structurally holds at most one region's rows —
    /// the write seam retires every other region's artifacts on any declaration
    /// change — so the one row present *is* the last-accepted document, whose
    /// folded bounds are still binding. More than one row for the kind means
    /// that invariant broke; recovering an identity by picking one would be
    /// guessing, so the read says so and yields nothing.
    pub async fn sole_region_artifact(&self, payload_kind: &str) -> Result<Option<StoredArtifact>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT region, sequence, issued_at, key_id, authority_name, envelope, accepted_at
                 FROM region_artifacts WHERE payload_kind = ?1",
            )
            .context("prepare sole region artifact")?;
        let mut rows: Vec<StoredArtifact> = stmt
            .query_map(rusqlite::params![payload_kind], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .context("query sole region artifact")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("read sole region artifact rows")?
            .into_iter()
            .filter_map(
                |(region, sequence, issued_at, key_id, authority_name, envelope, accepted_at)| {
                    // A region column that no longer parses cannot name a
                    // document either — skip it (it is unreachable by the keyed
                    // read too, and the next declaration change retires it).
                    let region = RegionCode::parse(region).ok()?;
                    Some(StoredArtifact {
                        region,
                        payload_kind: payload_kind.to_string(),
                        sequence: sequence as u64,
                        issued_at: issued_at as u64,
                        key_id,
                        authority_name,
                        envelope,
                        accepted_at,
                    })
                },
            )
            .collect();
        if rows.len() > 1 {
            tracing::warn!(
                payload_kind,
                count = rows.len(),
                "region_artifacts holds rows for more than one region; \
                 refusing to guess which document is in force"
            );
            return Ok(None);
        }
        Ok(rows.pop())
    }

    /// Stamp the outcome of a refresh attempt (success **or** failure).
    /// `reached` is whether the channel **answered** — a refused artifact is a
    /// reached channel, a failed fetch is not — and only a reach moves
    /// `reached_at`, which is what the staleness warning reads (mirrors
    /// [`Self::record_relay_attempt`]). `first_attempted_at` is set only on the
    /// row's first `INSERT`, never by the upsert's `DO UPDATE`, so it stays the
    /// true first try.
    pub async fn record_region_refresh(
        &self,
        payload_kind: &str,
        detail: Option<&str>,
        reached: bool,
    ) {
        let now = super::now_epoch_secs();
        let ok = detail.is_none();
        let conn = self.conn.lock().await;
        let written = conn.execute(
            "INSERT INTO region_refresh
                 (payload_kind, checked_at, ok, detail, first_attempted_at, reached_at)
             VALUES (?1, ?2, ?3, ?4, ?2, CASE WHEN ?5 THEN ?2 ELSE NULL END)
             ON CONFLICT(payload_kind) DO UPDATE SET
                 checked_at = excluded.checked_at,
                 ok = excluded.ok,
                 detail = excluded.detail,
                 reached_at = CASE WHEN ?5 THEN excluded.checked_at ELSE reached_at END",
            rusqlite::params![payload_kind, now, ok as i64, detail, reached],
        );
        // Deliberately swallowed: this row is an observation about the refresh,
        // never a precondition for one. Failing a refresh because its bookkeeping
        // failed would turn a warning surface into the outage § Fail posture says
        // it must never be.
        if let Err(e) = written {
            tracing::warn!("could not record region refresh outcome: {e}");
        }
    }

    pub async fn region_refresh_state(&self, payload_kind: &str) -> Result<Option<RefreshState>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT checked_at, ok, detail, first_attempted_at, reached_at
             FROM region_refresh WHERE payload_kind = ?1",
            rusqlite::params![payload_kind],
            |row| {
                Ok(RefreshState {
                    checked_at: row.get(0)?,
                    ok: row.get::<_, i64>(1)? != 0,
                    detail: row.get(2)?,
                    first_attempted_at: row.get(3)?,
                    reached_at: row.get(4)?,
                })
            },
        )
        .optional()
        .context("get region refresh state")
    }

    /// What this nest knows about the transparency log's history: the build's
    /// compiled-in anchor plus the last head it accepted evidence against
    /// (`region_log_anchor`; absent until the first witnessed acceptance).
    ///
    /// A stored head that is not 32 bytes is corruption, not absence, and is
    /// surfaced as an error rather than folded into the pre-log era — folding
    /// it would silently re-open the trust-on-first-fetch window § The
    /// transparency log's anchor exists to close.
    pub async fn region_log_anchor(&self) -> Result<AnchorState> {
        let conn = self.conn.lock().await;
        let stored: Option<Vec<u8>> = conn
            .query_row(
                "SELECT head FROM region_log_anchor WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("get region log anchor")?;
        let last_accepted = match stored {
            None => None,
            Some(bytes) => {
                let bytes: [u8; ObjectId::LEN] = bytes.try_into().map_err(|b: Vec<u8>| {
                    anyhow::anyhow!(
                        "stored region log anchor is {} bytes, not a SHA-256 object id",
                        b.len()
                    )
                })?;
                Some(ObjectId::from_bytes(bytes))
            }
        };
        Ok(AnchorState::with_last_accepted(last_accepted))
    }

    /// Persist the anchor an acceptance advanced to. Writing `None` is not
    /// representable on purpose: the log's history only ever moves forward.
    pub async fn put_region_log_anchor(&self, anchor: &AnchorState) -> Result<()> {
        let Some(head) = anchor.last_accepted else {
            return Ok(());
        };
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO region_log_anchor (id, head, accepted_at) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET head = excluded.head, accepted_at = excluded.accepted_at",
            rusqlite::params![head.as_bytes().as_slice(), now],
        )
        .context("put region log anchor")?;
        Ok(())
    }

    // ==================== The relay cache ====================
    //
    // `fauna.region.artifact.get`'s store (`region-blocking.md` § The content
    // plane → *How an app obtains its region's policy*). A row exists only for
    // an ENROLLED (region, kind) an app has asked for: the row IS the demand,
    // which is how the module's rule 2 generalises — a region nobody declares
    // is never fetched. The enrolment check is the caller's
    // (`region_relay::demand_relay_artifact`), before this method ever runs —
    // this insert itself has no opinion on enrolment, so a caller who skips
    // the check does not get one for free. See the migration's note for why
    // it is not `region_artifacts`.

    /// Record that an app asked for `(region, payload_kind)`. `true` on the
    /// **first** ask — the caller's cue to schedule a refill rather than wait a
    /// whole cadence for one — and `false` when the pair was already wanted.
    pub async fn request_relay_artifact(
        &self,
        region: &RegionCode,
        payload_kind: &str,
    ) -> Result<bool> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO region_relay_cache (region, payload_kind, requested_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![region.as_str(), payload_kind, now],
            )
            .context("record a relay request")?;
        Ok(inserted == 1)
    }

    /// The cache row for `(region, payload_kind)`, or `None` if no app has
    /// asked for it.
    pub async fn relay_artifact(
        &self,
        region: &RegionCode,
        payload_kind: &str,
    ) -> Result<Option<RelayCached>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT envelope, evidence, requested_at, attempted_at, reached_at
             FROM region_relay_cache WHERE region = ?1 AND payload_kind = ?2",
            rusqlite::params![region.as_str(), payload_kind],
            |row| {
                Ok(RelayCached {
                    envelope: row.get(0)?,
                    evidence: row.get(1)?,
                    requested_at: row.get(2)?,
                    attempted_at: row.get(3)?,
                    reached_at: row.get(4)?,
                })
            },
        )
        .optional()
        .context("read the relay cache")
    }

    /// Every `(region, payload_kind)` some app has asked for — the refresh
    /// worker's worklist. A stored region code that no longer parses is skipped
    /// with a warning rather than failing the whole worklist: it names nothing
    /// this build can fetch.
    pub async fn relay_demand(&self) -> Result<Vec<(RegionCode, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT region, payload_kind FROM region_relay_cache
                 ORDER BY region, payload_kind",
            )
            .context("prepare relay demand")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("query relay demand")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect relay demand")?;
        Ok(rows
            .into_iter()
            .filter_map(|(region, kind)| match RegionCode::parse(&region) {
                Ok(code) => Some((code, kind)),
                Err(e) => {
                    tracing::warn!(%region, "relay cache holds an unparseable region code: {e}");
                    None
                }
            })
            .collect())
    }

    /// Store a verified artifact as the relay's answer for its (region, kind),
    /// with the evidence the log served beside it, and **raise the replay floor
    /// in the same transaction** — the `put_region_artifact` rule verbatim, and
    /// the same floor table, because the counter is the authority's whichever
    /// store the artifact lands in.
    ///
    /// Takes a [`VerifiedArtifact`] so "cached" implies "verified" at the type
    /// level. The row is upserted, so an artifact for a pair nobody asked for
    /// still lands (the caller only ever stores what it fetched for demand).
    ///
    /// **`mark_publishing_renders` marks every publishing site owed a render in
    /// this same transaction** — the *A revoke is durable* rule applied to the
    /// nest-as-publisher leg (`web-content-hosting.md` § Routing, render,
    /// serving): a document landing in this cell can take a page off the public
    /// site, and the walk that re-renders them runs after the commit, so a nest
    /// that stops in between must restart owing those renders rather than
    /// serving pages the policy now in force withholds. The caller decides it
    /// (`region_tier::binds_the_public_render`: is this a *content policy* for
    /// a region on the declared situs's chain?) and decides it BEFORE the
    /// write, which is sound because that question reads the situs and the
    /// registry, never the artifact — and a situs moving concurrently is marked
    /// by the declaration write's own transaction above.
    pub async fn put_relay_artifact(
        &self,
        verified: &VerifiedArtifact,
        evidence: Option<&[u8]>,
        mark_publishing_renders: bool,
    ) -> Result<()> {
        let artifact = verified.artifact();
        let envelope =
            fauna_protocol::encode_canonical(artifact).context("encode relay artifact")?;
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin put_relay_artifact tx")?;
        tx.execute(
            "INSERT INTO region_relay_cache
                 (region, payload_kind, requested_at, envelope, evidence, sequence,
                  authority_name, accepted_at, attempted_at, reached_at, last_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?3, ?3, ?3, NULL)
             ON CONFLICT (region, payload_kind) DO UPDATE SET
                 envelope = excluded.envelope,
                 evidence = excluded.evidence,
                 sequence = excluded.sequence,
                 authority_name = excluded.authority_name,
                 accepted_at = excluded.accepted_at,
                 attempted_at = excluded.attempted_at,
                 reached_at = excluded.reached_at,
                 last_error = NULL",
            rusqlite::params![
                artifact.region.as_str(),
                artifact.payload_kind,
                now,
                envelope.to_vec(),
                evidence,
                artifact.sequence as i64,
                verified.authority_name(),
            ],
        )
        .context("put relay artifact")?;
        tx.execute(
            "INSERT INTO region_sequence_floor
                 (region, payload_kind, authority_name, sequence, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (region, payload_kind, authority_name) DO UPDATE SET
                 sequence = MAX(sequence, excluded.sequence),
                 updated_at = excluded.updated_at",
            rusqlite::params![
                artifact.region.as_str(),
                artifact.payload_kind,
                verified.authority_name(),
                artifact.sequence as i64,
                now,
            ],
        )
        .context("raise region sequence floor (relay)")?;
        if mark_publishing_renders {
            super::web::mark_web_render_owed_for_publishing_actors(&tx)?;
        }
        tx.commit().context("commit put_relay_artifact")?;
        Ok(())
    }

    /// Stamp a refresh attempt that did **not** land an artifact. `reached` is
    /// whether the log answered — a refused artifact is a reached channel, a
    /// failed fetch is not — and only a reach moves `reached_at`, which is what
    /// staleness reads. Swallowed like `record_region_refresh`: an observation
    /// about the refresh, never a precondition for one.
    pub async fn record_relay_attempt(
        &self,
        region: &RegionCode,
        payload_kind: &str,
        reached: bool,
        detail: &str,
    ) {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        let written = conn.execute(
            "UPDATE region_relay_cache
             SET attempted_at = ?3,
                 reached_at = CASE WHEN ?4 THEN ?3 ELSE reached_at END,
                 last_error = ?5
             WHERE region = ?1 AND payload_kind = ?2",
            rusqlite::params![region.as_str(), payload_kind, now, reached, detail],
        );
        if let Err(e) = written {
            tracing::warn!("could not record relay refresh outcome: {e}");
        }
    }

    /// Clear a pair's cached envelope — the de-listing path (the module's
    /// *de-listing an authority retires its document*, applied to the relay).
    /// Keeps the demand row, so the pair is fetched again if its region is
    /// re-enrolled, and leaves `region_sequence_floor` alone, as
    /// `clear_region_artifacts` does: an artifact this nest once accepted stays
    /// validly signed forever and must stay un-replayable.
    ///
    /// `mark_publishing_renders` is [`Self::put_relay_artifact`]'s, for the
    /// mirror case: retiring the document in force *restores* pages it
    /// withheld, and a restart between this commit and the walk would leave
    /// them withheld with nothing owed.
    pub async fn retire_relay_artifact(
        &self,
        region: &RegionCode,
        payload_kind: &str,
        mark_publishing_renders: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin retire_relay_artifact tx")?;
        tx.execute(
            "UPDATE region_relay_cache
             SET envelope = NULL, evidence = NULL, sequence = NULL,
                 authority_name = NULL, accepted_at = NULL
             WHERE region = ?1 AND payload_kind = ?2",
            rusqlite::params![region.as_str(), payload_kind],
        )
        .context("retire relay artifact")?;
        if mark_publishing_renders {
            super::web::mark_web_render_owed_for_publishing_actors(&tx)?;
        }
        tx.commit().context("commit retire_relay_artifact")?;
        Ok(())
    }
}

/// One relay cache row, as `fauna.region.artifact.get` reads it.
///
/// `envelope` / `evidence` are the canonical bytes exactly as verified and
/// stored — the relay forwards them and never decodes the document inside,
/// which is what lets an app newer than this nest read a document version this
/// nest's build does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayCached {
    pub envelope: Option<Vec<u8>>,
    pub evidence: Option<Vec<u8>>,
    pub requested_at: i64,
    pub attempted_at: Option<i64>,
    pub reached_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use ed25519_dalek::SigningKey;
    use fauna_core::region_authority::{
        AuthorityKey, PAYLOAD_KIND_FEATURE_POLICY, RegionEntry, RegionRegistry, sign_artifact,
        verify_artifact,
    };

    fn region() -> RegionCode {
        RegionCode::parse("NO").unwrap()
    }

    fn verified(sequence: u64) -> VerifiedArtifact {
        verified_for(region(), sequence)
    }

    fn verified_for(region: RegionCode, sequence: u64) -> VerifiedArtifact {
        let signing = SigningKey::from_bytes(&[3u8; 32]);
        let registry = RegionRegistry {
            version: 1,
            regions: vec![RegionEntry {
                region: region.clone(),
                authority_name: "Test Authority".into(),
                official_domain: "authority.example".into(),
                parent: None,
                keys: vec![AuthorityKey {
                    key_id: "k1".into(),
                    public_key: signing.verifying_key().to_bytes().to_vec(),
                    enrolled_at: 0,
                    retired_at: None,
                }],
            }],
        };
        let artifact = sign_artifact(
            PolicyArtifact {
                region,
                key_id: "k1".into(),
                sequence,
                issued_at: 1_000,
                payload_kind: PAYLOAD_KIND_FEATURE_POLICY.into(),
                payload: fauna_protocol::encode_canonical(
                    &fauna_core::region_authority::RegionFeaturePolicies::new(),
                )
                .unwrap()
                .to_vec(),
                sig: Vec::new(),
            },
            &signing,
        )
        .unwrap();
        verify_artifact(artifact, &registry, 1_000, None).unwrap()
    }

    #[tokio::test]
    async fn a_fresh_nest_declares_no_region() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_declared_region().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_declaration_is_settable_and_withdrawable() {
        let db = CacheDb::open_in_memory().unwrap();
        db.set_declared_region(&region(), false).await.unwrap();
        assert_eq!(db.get_declared_region().await.unwrap(), Some(region()));

        let other = RegionCode::parse("SE").unwrap();
        db.set_declared_region(&other, false).await.unwrap();
        assert_eq!(db.get_declared_region().await.unwrap(), Some(other));

        assert!(db.clear_declared_region(false).await.unwrap());
        assert_eq!(db.get_declared_region().await.unwrap(), None);
        assert!(!db.clear_declared_region(false).await.unwrap());
    }

    /// The direction that cannot invent a restriction: a declaration this build
    /// cannot parse is *no* declaration, not an error that takes the nest down.
    #[tokio::test]
    async fn an_unreadable_declaration_reads_as_undeclared() {
        let db = CacheDb::open_in_memory().unwrap();
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO nest_region (id, region, set_at) VALUES (1, 'not a region', 0)",
                [],
            )
            .unwrap();
        }
        assert_eq!(db.get_declared_region().await.unwrap(), None);
    }

    /// …but the un-folded read distinguishes corruption from absence — the
    /// report paths and the write seam depend on the difference.
    #[tokio::test]
    async fn the_three_state_read_distinguishes_unreadable_from_undeclared() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.declared_region_state().await.unwrap(),
            DeclaredRegion::Undeclared
        );

        db.set_declared_region(&region(), false).await.unwrap();
        assert_eq!(
            db.declared_region_state().await.unwrap(),
            DeclaredRegion::Declared(region())
        );

        {
            let conn = db.conn.lock().await;
            conn.execute("UPDATE nest_region SET region = 'not a region'", [])
                .unwrap();
        }
        assert_eq!(
            db.declared_region_state().await.unwrap(),
            DeclaredRegion::Unreadable
        );
    }

    /// The change-of-situs retirement keyed on the survivor: everything not the
    /// kept region goes, whether or not the outgoing declaration was readable.
    #[tokio::test]
    async fn clearing_except_a_region_drops_only_the_others() {
        let db = CacheDb::open_in_memory().unwrap();
        let se = RegionCode::parse("SE").unwrap();
        db.put_region_artifact(&verified(4)).await.unwrap();
        db.put_region_artifact(&verified_for(se.clone(), 1))
            .await
            .unwrap();

        assert_eq!(
            db.clear_region_artifacts_except(Some(&se)).await.unwrap(),
            1
        );
        assert!(
            db.get_region_artifact(&se, PAYLOAD_KIND_FEATURE_POLICY)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_region_artifact(&region(), PAYLOAD_KIND_FEATURE_POLICY)
                .await
                .unwrap()
                .is_none()
        );

        assert_eq!(db.clear_region_artifacts_except(None).await.unwrap(), 1);
        assert!(
            db.get_region_artifact(&se, PAYLOAD_KIND_FEATURE_POLICY)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// The keyless identity recovery answers only when the answer is
    /// unambiguous: exactly one row for the kind.
    #[tokio::test]
    async fn the_sole_artifact_read_answers_one_row_and_refuses_ambiguity() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.sole_region_artifact(PAYLOAD_KIND_FEATURE_POLICY)
                .await
                .unwrap(),
            None
        );

        db.put_region_artifact(&verified(4)).await.unwrap();
        let sole = db
            .sole_region_artifact(PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .expect("one row is unambiguous");
        assert_eq!(sole.region, region());
        assert_eq!(sole.sequence, 4);
        assert_eq!(sole.authority_name, "Test Authority");

        // Two regions' rows — the invariant the write seam holds is broken, so
        // recovering an identity would be guessing.
        let se = RegionCode::parse("SE").unwrap();
        db.put_region_artifact(&verified_for(se, 1)).await.unwrap();
        assert_eq!(
            db.sole_region_artifact(PAYLOAD_KIND_FEATURE_POLICY)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn an_accepted_artifact_round_trips_and_advances_the_sequence() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.accepted_region_sequence(&region(), PAYLOAD_KIND_FEATURE_POLICY, "Test Authority")
                .await
                .unwrap(),
            None
        );

        db.put_region_artifact(&verified(4)).await.unwrap();
        let stored = db
            .get_region_artifact(&region(), PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .expect("stored");
        assert_eq!(stored.sequence, 4);
        assert_eq!(stored.key_id, "k1");
        assert_eq!(stored.authority_name, "Test Authority");
        assert_eq!(stored.artifact().unwrap().sequence, 4);

        // One row per (region, kind): a later acceptance replaces, never appends.
        db.put_region_artifact(&verified(9)).await.unwrap();
        assert_eq!(
            db.accepted_region_sequence(&region(), PAYLOAD_KIND_FEATURE_POLICY, "Test Authority")
                .await
                .unwrap(),
            Some(9)
        );
    }

    /// The floor is the *authority's* high-water mark: another authority's
    /// counter is a different space, and reading it must not inherit this
    /// one's.
    #[tokio::test]
    async fn the_sequence_floor_is_scoped_to_the_authority() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_region_artifact(&verified(9)).await.unwrap();
        assert_eq!(
            db.accepted_region_sequence(&region(), PAYLOAD_KIND_FEATURE_POLICY, "Someone Else")
                .await
                .unwrap(),
            None
        );
    }

    /// **The floor outlives every retirement.** Both deleters drop the
    /// binding; neither may drop the replay defence, or an artifact this nest
    /// already accepted becomes replayable again.
    #[tokio::test]
    async fn retiring_the_artifact_does_not_lower_the_floor() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_region_artifact(&verified(9)).await.unwrap();

        db.clear_region_artifacts(&region()).await.unwrap();
        assert_eq!(
            db.accepted_region_sequence(&region(), PAYLOAD_KIND_FEATURE_POLICY, "Test Authority")
                .await
                .unwrap(),
            Some(9),
            "the withdrawal path cleared the replay floor with the binding"
        );

        db.put_region_artifact(&verified(10)).await.unwrap();
        db.clear_region_artifacts_except(None).await.unwrap();
        assert_eq!(
            db.accepted_region_sequence(&region(), PAYLOAD_KIND_FEATURE_POLICY, "Test Authority")
                .await
                .unwrap(),
            Some(10),
            "the change-of-situs retirement cleared the replay floor with the binding"
        );
    }

    #[tokio::test]
    async fn clearing_a_region_drops_its_artifacts() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_region_artifact(&verified(4)).await.unwrap();
        assert_eq!(db.clear_region_artifacts(&region()).await.unwrap(), 1);
        assert_eq!(
            db.get_region_artifact(&region(), PAYLOAD_KIND_FEATURE_POLICY)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_refresh_outcome_is_recorded_both_ways() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
                .await
                .unwrap(),
            None
        );

        db.record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, None, true)
            .await;
        let state = db
            .region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .expect("recorded");
        assert!(state.ok);
        assert_eq!(state.detail, None);

        db.record_region_refresh(
            PAYLOAD_KIND_FEATURE_POLICY,
            Some("channel unreachable"),
            false,
        )
        .await;
        let state = db
            .region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .expect("recorded");
        assert!(!state.ok);
        assert_eq!(state.detail.as_deref(), Some("channel unreachable"));
    }

    /// **`reached_at` only moves on a reach; `first_attempted_at` is set once
    /// and never again.** This is the anti-regression pin for the defect this
    /// row fixes: a `record_region_refresh` that bumped `reached_at` on every
    /// call, reached or not, would reproduce the bug where a failing worker
    /// keeps a silent channel looking fresh.
    #[tokio::test]
    async fn reached_at_moves_only_on_a_reach_and_first_attempted_at_is_stable() {
        let db = CacheDb::open_in_memory().unwrap();

        // First attempt: a fetch failure. Not reached — `reached_at` stays
        // unset — but `first_attempted_at` is now anchored.
        db.record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, Some("fetch failed"), false)
            .await;
        let state = db
            .region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .expect("recorded");
        let first_attempt = state.first_attempted_at;
        assert_eq!(state.reached_at, None);

        // Another failed attempt: `checked_at` moves, `reached_at` stays
        // unset, `first_attempted_at` is unchanged.
        db.record_region_refresh(
            PAYLOAD_KIND_FEATURE_POLICY,
            Some("fetch failed again"),
            false,
        )
        .await;
        let state = db
            .region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .expect("recorded");
        assert_eq!(state.reached_at, None);
        assert_eq!(state.first_attempted_at, first_attempt);

        // A refusal is a REACHED channel: `reached_at` now moves.
        db.record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, Some("bad signature"), true)
            .await;
        let state = db
            .region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .expect("recorded");
        let reached_once = state.reached_at.expect("refusal reached the channel");
        assert_eq!(state.first_attempted_at, first_attempt);

        // A later failed attempt must not move `reached_at` backward-looking
        // stale, i.e. must leave it exactly where the last reach put it.
        db.record_region_refresh(
            PAYLOAD_KIND_FEATURE_POLICY,
            Some("fetch failed once more"),
            false,
        )
        .await;
        let state = db
            .region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .expect("recorded");
        assert_eq!(state.reached_at, Some(reached_once));
        assert_eq!(state.first_attempted_at, first_attempt);
    }
}
