//! The generalized account-data feed's class-2 storage leg — W2.3 (account-data-plane.md § Workstreams) of
//! `docs/goal/architecture/account-sync-plane.md` (§ Feeds and cursors,
//! § The class-2 entry form).
//!
//! **What this plane is.** The account-state scope carries every class-2 item —
//! settings keys, relationship rows, read/ack markers, the seen-set,
//! device-endpoint entries — as sealed per-writer entries. Nest-side it is a
//! per-actor reserved folder (`__state`, minted by
//! [`super::snapshots::get_or_create_reserved_folder`]), and each entry is one
//! `sync_changes` row: `item_class = 'state-entry'`, the **blinded** item key in
//! the shipped `path_hash` slot, the authoring device's coordinates in
//! `origin_writer` / `origin_seq`, and the sealed T14 envelope inline in
//! `entry_sealed`. The nest is a relay — it holds no key that opens one, and the
//! envelope's AAD binds those very coordinates, so it cannot splice an entry
//! under different ones either.
//!
//! **Why the rows collapse.** § Store logical schema states the plane's ground
//! rule outright: *"ground truth is blocks + merged state entries and
//! tombstones; the journal is the bounded, ordered change feed over them — never
//! an infinitely-retained event source"*, and *"a writer may compact its own
//! log's superseded class-2 rows"*, because per-entry full-state reconcile is
//! the backstop and class-2 merge is state-based. So a put marks that writer's
//! own predecessors for the same item `superseded_at` — the shipped mechanism
//! both feed reads already filter on, and the same one the reserved rails use
//! (`collapse_reserved_rail_history`). The row is never deleted; only its liveness moves.
//!
//! ⚠ **Collapse is keyed per WRITER, not per item.** A newer entry supersedes
//! only its *own* writer's older entries for that item. Collapsing across
//! writers would delete the other replicas' entries from the feed — and those
//! are precisely the inputs the reader-side merge seam (W2.4) needs, so a
//! multi-master item would silently converge to whoever wrote last. This is the
//! one place where the reserved-rail precedent must **not** be copied verbatim:
//! `collapse_reserved_rail_history_in_conn` keys on `(folder_id, path_hash)`
//! alone, which is right for a single-writer rail and wrong here.

use super::CacheDb;
use anyhow::Context;
use fauna_protocol::account_state::{
    ACCOUNT_STATE_FLEET_SCOPE, ItemClass, MAX_STATE_ENTRIES_PER_SCOPE,
};
use rusqlite::OptionalExtension;

/// Why a [`CacheDb::record_account_state_entry`] call was refused. Each maps to
/// its own wire error code at the handler, so a client can tell a losing CAS
/// race (retry after re-reading) from a stale replay (drop it) from a full
/// scope (surface it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEntryError {
    /// [`super::super::db::CacheDb::record_account_state_entry`] was handed a
    /// `writer_seq` that does not advance this writer's high-water for the item
    /// — a replay or an out-of-order retry. Refused rather than stored, so the
    /// feed stays monotone per writer and a replayed put can never allocate a
    /// second row.
    SeqNotAdvancing { head: i64 },
    /// A `(scope, writer, writer_seq)` coordinate this writer already used —
    /// on ANY item, live or since collapsed. A writer's journal is the only
    /// home of its seq counter, and every replica keys row uniqueness and its
    /// equivocation refusal on exactly this coordinate, so a second row at it
    /// (a surviving slot reused over a fresh journal, a store dir restored
    /// from an older backup — `account-replica-posture.md` § The store device
    /// principal, refinement 11) would be accepted here and then equivocate
    /// at every other replica that holds the first. Refused under the same
    /// wire code as [`Self::SeqNotAdvancing`]: the client's remedy is the
    /// same — its journal is burnt, and the walk's own verdict heals it —
    /// and so is what the client does at the refusal itself: it retires the
    /// relay row it recorded for the send (`fauna_protocol::RpcError::
    /// is_account_state_stale_writer_seq`; refinement 11 → *a refused row's
    /// relay residue*), because either variant is this nest's final word on
    /// the coordinate.
    SeqReused { seq: i64 },
    /// A nest-arbitrated write whose `cas_base` is not the item's current head
    /// for this writer — the concurrent-writer loss the CAS variant exists to
    /// make visible.
    CasMismatch { head: Option<i64> },
    /// The scope already holds [`MAX_STATE_ENTRIES_PER_SCOPE`] live entries and
    /// this put would introduce a new one.
    ScopeFull,
}

impl std::fmt::Display for StateEntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SeqNotAdvancing { head } => {
                write!(f, "writer_seq must advance this writer's head ({head})")
            }
            Self::SeqReused { seq } => write!(
                f,
                "writer_seq {seq} was already recorded for this writer in this scope — a \
                 coordinate is never used twice (the client's journal is burnt)"
            ),
            Self::CasMismatch { head } => match head {
                Some(h) => write!(f, "cas_base does not match the item's head ({h})"),
                None => write!(f, "cas_base was given for an item with no head yet"),
            },
            Self::ScopeFull => write!(
                f,
                "the scope already holds its maximum of {MAX_STATE_ENTRIES_PER_SCOPE} live entries"
            ),
        }
    }
}

/// One row a put names as covered (`fauna.account.state.put`'s `replaces`) —
/// its cleartext coordinates, decoded and validated by the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplacedCoordinate {
    pub item_key: [u8; 32],
    pub writer: [u8; 32],
    pub writer_seq: i64,
}

/// What a [`CacheDb::record_account_state_entry`] call that landed did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedStateEntry {
    /// The nest-log `seq` the entry landed at.
    pub seq: i64,
    /// How many of the named rows this put superseded (0 when it named none).
    pub replaced: u32,
}

/// Why a [`CacheDb::retire_account_state_entry`] call was refused. Each maps
/// to its own wire error code at the handler; both mean "not now, ask again
/// next pass", never "never".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireError {
    /// This many marked walkers holding a live grant have not walked past the
    /// row yet — the retention gate.
    NotYetStable { lagging: usize },
    /// This many live rows of the scope are still sealed under the generation
    /// the caller asserted dataless.
    GenerationInUse { rows: usize },
}

impl std::fmt::Display for RetireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotYetStable { lagging } => write!(
                f,
                "{lagging} walker(s) with a live grant have not walked past this row yet"
            ),
            Self::GenerationInUse { rows } => write!(
                f,
                "{rows} live row(s) are still sealed under the named generation"
            ),
        }
    }
}

/// The reserved-rail `kind` that backs a scope name — `state` → the `__state`
/// folder. Kept as its own function so the placement detail (that a scope is
/// a reserved folder at all) has exactly one spelling.
fn reserved_kind_for_scope(scope: &str) -> &str {
    scope
}

impl CacheDb {
    /// The actor's folder backing `scope`, minting it if absent — the write
    /// path's resolver (`get_or_create_reserved_folder` is idempotent under
    /// `UNIQUE(name, actor_id)`).
    pub async fn get_or_create_state_scope(
        &self,
        actor_id: &[u8; 32],
        scope: &str,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().await;
        super::snapshots::get_or_create_reserved_folder(
            &conn,
            actor_id,
            reserved_kind_for_scope(scope),
        )
    }

    /// The actor's folder backing `scope`, or `None` if they have never
    /// written to it — the read path's resolver. Deliberately non-creating: a
    /// list against an untouched scope answers an empty feed, and a read must
    /// not mint rows.
    pub async fn find_state_scope(
        &self,
        actor_id: &[u8; 32],
        scope: &str,
    ) -> anyhow::Result<Option<i64>> {
        let name = format!("__{}", reserved_kind_for_scope(scope));
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id FROM folders WHERE name = ?1 AND actor_id = ?2",
            rusqlite::params![name, actor_id.as_slice()],
            |r| r.get(0),
        )
        .optional()
        .context("look up state scope folder")
    }

    /// The actor's `ext:<kind>` scopes whose feed advanced past `cursor`, each
    /// with its head — the nest-log `seq` of its newest row — in head order,
    /// at most `limit` of them. What the events poll answers from
    /// (`transport.md` § Push events → *Third-party event doors*): `seq` is
    /// one nest-wide order, so a single scalar is a cursor over every scope,
    /// and a capped page loses nothing — every scope left out has its head
    /// above the last one returned.
    pub async fn ext_scope_heads_since(
        &self,
        actor_id: &[u8; 32],
        cursor: i64,
        limit: u32,
    ) -> anyhow::Result<Vec<(String, i64)>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare_cached(
            "SELECT f.name, MAX(c.seq) AS head
               FROM sync_changes c JOIN folders f ON f.id = c.folder_id
              WHERE f.actor_id = ?1 AND f.name LIKE '\\_\\_ext:%' ESCAPE '\\'
                AND c.seq > ?2
              GROUP BY f.id
              ORDER BY head
              LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice(), cursor, limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("read ext scope heads")?;
        Ok(rows
            .into_iter()
            .filter_map(|(name, head)| Some((name.strip_prefix("__")?.to_string(), head)))
            .collect())
    }

    /// Append one sealed class-2 entry to `scope_folder_id`'s feed and collapse
    /// this writer's superseded predecessors for the same item, under one
    /// connection lock so the head the collapse runs against is the row this
    /// call just inserted (the `record_drafts_blob_change` discipline).
    ///
    /// Returns the nest-log `seq` the entry landed at — the coordinate the
    /// nest-writer frontier slot (`since`) walks.
    ///
    /// The checks, in the order they run and why that order:
    ///
    /// 1. **CAS** (when the caller supplied a base) before anything is written,
    ///    so a losing nest-arbitrated writer changes no state at all.
    /// 2. **Per-writer monotonicity.** A `writer_seq` at or below this writer's
    ///    head for the item is refused. The entry's AAD already binds its
    ///    `writer_seq`, so a relay cannot *forge* a newer coordinate; what this
    ///    adds is that a **replay** of a genuine older entry cannot re-enter the
    ///    feed above the row that superseded it, which is what makes the kind
    ///    safe to hand the queued/offline path without an idempotency key.
    /// 3. **The named rows** (`replaces`, `delegable-scope-reclamation.md`
    ///    § Delegable-scope reclamation, part (2)): each row live at exactly
    ///    its `(item_key, writer, writer_seq)` in this scope is marked
    ///    superseded — the retire's own `UPDATE`, its bytes and coordinate
    ///    kept, so a replay of it is still refused [`StateEntryError::SeqReused`]
    ///    — and one not live there is skipped, never an error. **No retention
    ///    gate and no walk mark is read:** the row covering a replaced row
    ///    lands in this same transaction, and the feed serves it to every
    ///    walker that has not walked it. The handler refuses a list on the
    ///    fleet scope, whose retires carry orderings the gate enforces.
    /// 4. **Scope cap**, counted after step 3 and charged only when the put
    ///    introduces a *new* `(item_key, writer)` pair — an update to an
    ///    existing entry is free, which is what keeps the cap a bound on the
    ///    key space rather than on how often a user changes a setting; a put
    ///    that replaces a row is count-neutral.
    ///
    /// Steps 3 to the insert run in one transaction: a put refused at any
    /// step supersedes nothing.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_account_state_entry(
        &self,
        actor_id: &[u8; 32],
        scope_folder_id: i64,
        item_key: &[u8; 32],
        writer_id: &[u8; 32],
        writer_seq: i64,
        op: &str,
        entry_sealed: &[u8],
        cas_base: Option<i64>,
        replaces: &[ReplacedCoordinate],
    ) -> anyhow::Result<Result<RecordedStateEntry, StateEntryError>> {
        let actor_id = *actor_id;
        let item_key = *item_key;
        let writer_id = *writer_id;
        let op = op.to_string();
        let entry_sealed = entry_sealed.to_vec();
        let conn = self.conn.lock().await;

        // This writer's live head for this item, if any.
        let head: Option<i64> = conn
            .query_row(
                "SELECT origin_seq FROM sync_changes
                 WHERE folder_id = ?1 AND path_hash = ?2 AND origin_writer = ?3
                   AND item_class = ?4 AND superseded_at IS NULL
                 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![
                    scope_folder_id,
                    item_key.as_slice(),
                    writer_id.as_slice(),
                    ItemClass::StateEntry.as_wire(),
                ],
                |r| r.get(0),
            )
            .optional()
            .context("probe state-entry head")?
            .flatten();

        if let Some(base) = cas_base
            && head != Some(base)
        {
            return Ok(Err(StateEntryError::CasMismatch { head }));
        }
        if let Some(h) = head
            && writer_seq <= h
        {
            return Ok(Err(StateEntryError::SeqNotAdvancing { head: h }));
        }
        // The head is per item; the coordinate is per writer. A writer's
        // seq never recurs in a scope, on any item, live or collapsed — see
        // [`StateEntryError::SeqReused`]. Collapse keeps the row (only its
        // liveness moves), so this memory is complete.
        let reused: bool = conn
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sync_changes
                     WHERE folder_id = ?1 AND origin_writer = ?2 AND origin_seq = ?3
                       AND item_class = ?4)",
                rusqlite::params![
                    scope_folder_id,
                    writer_id.as_slice(),
                    writer_seq,
                    ItemClass::StateEntry.as_wire(),
                ],
                |r| r.get(0),
            )
            .context("probe state-entry coordinate reuse")?;
        if reused {
            return Ok(Err(StateEntryError::SeqReused { seq: writer_seq }));
        }

        let now = super::now_epoch_millis();
        // Dropped without a commit on every refusal below, which rolls the
        // supersedes back: a refused put supersedes nothing.
        let tx = conn
            .unchecked_transaction()
            .context("begin state-entry transaction")?;
        let mut replaced: u32 = 0;
        for row in replaces {
            let changed = tx
                .execute(
                    "UPDATE sync_changes SET superseded_at = ?1
                     WHERE folder_id = ?2 AND path_hash = ?3 AND origin_writer = ?4
                       AND origin_seq = ?5 AND item_class = ?6 AND superseded_at IS NULL",
                    rusqlite::params![
                        now,
                        scope_folder_id,
                        row.item_key.as_slice(),
                        row.writer.as_slice(),
                        row.writer_seq,
                        ItemClass::StateEntry.as_wire(),
                    ],
                )
                .context("supersede a replaced state entry")?;
            replaced = replaced.saturating_add(u32::try_from(changed).unwrap_or(u32::MAX));
        }

        if head.is_none() {
            let live: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM sync_changes
                     WHERE folder_id = ?1 AND item_class = ?2 AND superseded_at IS NULL",
                    rusqlite::params![scope_folder_id, ItemClass::StateEntry.as_wire()],
                    |r| r.get(0),
                )
                .context("count live state entries")?;
            if live >= MAX_STATE_ENTRIES_PER_SCOPE {
                return Ok(Err(StateEntryError::ScopeFull));
            }
        }

        tx.execute(
            "INSERT INTO sync_changes
                (actor_id, path_hash, manifest_hash, size_bytes, change_type, created_at,
                 folder_id, item_class, origin_writer, origin_seq, entry_sealed)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                actor_id.as_slice(),
                item_key.as_slice(),
                entry_sealed.len() as i64,
                op,
                now,
                scope_folder_id,
                ItemClass::StateEntry.as_wire(),
                writer_id.as_slice(),
                writer_seq,
                entry_sealed,
            ],
        )
        .context("insert state entry")?;
        let seq = tx.last_insert_rowid();

        // Collapse THIS writer's predecessors only — see the module header for
        // why the per-item reserved-rail predicate would be wrong here.
        tx.execute(
            "UPDATE sync_changes SET superseded_at = ?1
             WHERE folder_id = ?2 AND path_hash = ?3 AND origin_writer = ?4
               AND item_class = ?5 AND seq < ?6 AND superseded_at IS NULL",
            rusqlite::params![
                now,
                scope_folder_id,
                item_key.as_slice(),
                writer_id.as_slice(),
                ItemClass::StateEntry.as_wire(),
                seq,
            ],
        )
        .context("collapse superseded state entries")?;

        tx.commit().context("commit state entry")?;
        Ok(Ok(RecordedStateEntry { seq, replaced }))
    }

    /// Record `walker`'s claim to hold every row of `scope_folder_id` at or
    /// below `seq` — the `held_through_seq` + `walker_id` pair a walk sends —
    /// as its mark for the feed's retention gate
    /// ([`Self::retire_account_state_entry`]). Monotone: a lower claim never
    /// lowers a mark.
    pub async fn record_state_walk_mark(
        &self,
        scope_folder_id: i64,
        walker: &[u8; 32],
        seq: i64,
    ) -> anyhow::Result<()> {
        let walker = *walker;
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO state_walk_marks (folder_id, walker, seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(folder_id, walker) DO UPDATE SET seq = MAX(seq, excluded.seq)",
            rusqlite::params![scope_folder_id, walker.as_slice(), seq],
        )
        .context("record state walk mark")?;
        Ok(())
    }

    /// **The gate's watermark** — the lowest mark among `scope_folder_id`'s
    /// counted walkers (a mark counts under exactly the predicate
    /// [`Self::retire_account_state_entry`] gates on: the walker's key holds
    /// a live, non-tombstoned grant on `actor_id`). A live row with `seq` at
    /// or below it passes the retention gate now; one above it is refused
    /// `not_yet_stable`. `None` when no counted mark exists — then the gate
    /// refuses nothing, and the feed says nothing either. Served on the
    /// class-2 feed reply as `retirable_through_seq` so a replica withholds
    /// the retires the gate would refuse instead of asking one at a time
    /// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
    /// reclamation*, clause (1) → *the gate's watermark*).
    pub async fn retirable_through_seq(
        &self,
        scope_folder_id: i64,
        actor_id: &[u8; 32],
    ) -> anyhow::Result<Option<i64>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT MIN(m.seq) FROM state_walk_marks m
             WHERE m.folder_id = ?1
               AND EXISTS (SELECT 1 FROM sync_devices d
                           WHERE d.actor_id = ?2 AND d.auth_device_key = m.walker)
               AND NOT EXISTS (SELECT 1 FROM revoked_device_grants r
                               WHERE r.actor_id = ?2 AND r.auth_device_key = m.walker)",
            rusqlite::params![scope_folder_id, actor_id.as_slice()],
            |r| r.get::<_, Option<i64>>(0),
        )
        .context("lowest counted walk mark")
    }

    /// Mark the live class-2 row at exactly `(item_key, writer, writer_seq)`
    /// superseded **without inserting anything** — `fauna.account.state.retire`
    /// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
    /// reclamation*, clause (1)). The row and its coordinate stay, so the
    /// seq-reuse memory behind [`StateEntryError::SeqReused`] is intact and a
    /// replay of the retired row is refused as before.
    ///
    /// **The retention gate.** Refused [`RetireError::NotYetStable`] while any
    /// walker marked on this scope ([`Self::record_state_walk_mark`]) whose
    /// key holds a live, non-tombstoned grant on `actor_id` has a mark below
    /// the row's `seq`: such a walker may still hold — and serve a peer — the
    /// state this row supersedes, and only its own walk past the row can
    /// prove otherwise. A mark whose key is tombstoned (`revoked_device_grants`)
    /// or holds no grant on the actor counts for nothing: a signed-out or
    /// deleted device stops blocking with its grant, and a dead machine blocks
    /// exactly until the user removes it.
    ///
    /// **The generation belt.** With `no_rows_sealed_under`, refused
    /// [`RetireError::GenerationInUse`] while any live row of the scope names
    /// that generation in its form-v2 cleartext header — the nest-side check
    /// behind a client's "this generation is dataless" finding, atomic with
    /// the retire under one connection lock. The belt scans this scope only;
    /// it is complete for the account because `scope` is required to be the
    /// fleet scope (below) and every generation-sealed kind seals into the
    /// fleet scope (`fauna_protocol::merge_policy`, the tip-sealed set is
    /// fleet-only) — a delegable generation-sealed kind would need the scan
    /// widened.
    ///
    /// `no_rows_sealed_under` (and with it `delete_escrow_wraps`) is refused
    /// outright — an internal-caller-error `anyhow` bail, not a wire
    /// [`RetireError`] — unless `scope` is
    /// [`ACCOUNT_STATE_FLEET_SCOPE`]: the handler enforces the identical gate
    /// as `invalid_request` before this call ever runs, so the completeness
    /// claim above holds by construction rather than by the accident of who
    /// calls it today .
    ///
    /// **The escrow sweep** (clause (3e)). With `delete_escrow_wraps`, the
    /// retire that lands also deletes every escrow wrap this nest holds for
    /// the belted generation (`generation_escrow_wraps`) — under this
    /// account and every identity its successions retired into it — in the
    /// same transaction: a shredded, dataless generation has nothing left to
    /// recover, and the belt just proved it dataless. The flag rides the belt
    /// and nothing else — it is an error without `no_rows_sealed_under` (the
    /// handler refuses the combination `invalid_request` before it reaches
    /// here) — and a retire that lands nothing (`Ok(Ok(false))`) deletes
    /// nothing: the wrap goes with the receipt row that named it.
    ///
    /// `Ok(Ok(false))` when no live row sits at those coordinates (already
    /// retired, superseded by a newer row, or never published): idempotent,
    /// never an error.
    pub async fn retire_account_state_entry(
        &self,
        actor_id: &[u8; 32],
        scope: &str,
        scope_folder_id: i64,
        item_key: &[u8; 32],
        writer_id: &[u8; 32],
        writer_seq: i64,
        no_rows_sealed_under: Option<&[u8; 32]>,
        delete_escrow_wraps: bool,
    ) -> anyhow::Result<Result<bool, RetireError>> {
        let actor_id = *actor_id;
        let item_key = *item_key;
        let writer_id = *writer_id;
        let no_rows_sealed_under = no_rows_sealed_under.copied();
        if delete_escrow_wraps && no_rows_sealed_under.is_none() {
            anyhow::bail!("delete_escrow_wraps without no_rows_sealed_under");
        }
        // The belt's completeness claim (clause (3e)) holds only for the
        // fleet scope — every generation-sealed kind seals there
        // (`fauna_protocol::merge_policy`) — so a belt named on any other
        // scope would scan a folder no generation-sealed row can ever live
        // in and pass vacuously, then delete escrow wraps still recoverable
        // through a live fleet-scope row .
        if no_rows_sealed_under.is_some() && scope != ACCOUNT_STATE_FLEET_SCOPE {
            anyhow::bail!(
                "no_rows_sealed_under (the generation belt) is only valid for the fleet scope"
            );
        }
        let conn = self.conn.lock().await;

        let row_seq: Option<i64> = conn
            .query_row(
                "SELECT seq FROM sync_changes
                 WHERE folder_id = ?1 AND path_hash = ?2 AND origin_writer = ?3
                   AND origin_seq = ?4 AND item_class = ?5 AND superseded_at IS NULL",
                rusqlite::params![
                    scope_folder_id,
                    item_key.as_slice(),
                    writer_id.as_slice(),
                    writer_seq,
                    ItemClass::StateEntry.as_wire(),
                ],
                |r| r.get(0),
            )
            .optional()
            .context("probe state entry to retire")?;
        let Some(row_seq) = row_seq else {
            return Ok(Ok(false));
        };

        // The gate: every counted walker must have walked past the row.
        let lagging: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM state_walk_marks m
                 WHERE m.folder_id = ?1 AND m.seq < ?2
                   AND EXISTS (SELECT 1 FROM sync_devices d
                               WHERE d.actor_id = ?3 AND d.auth_device_key = m.walker)
                   AND NOT EXISTS (SELECT 1 FROM revoked_device_grants r
                                   WHERE r.actor_id = ?3 AND r.auth_device_key = m.walker)",
                rusqlite::params![scope_folder_id, row_seq, actor_id.as_slice()],
                |r| r.get(0),
            )
            .context("count lagging walk marks")?;
        if lagging > 0 {
            return Ok(Err(RetireError::NotYetStable {
                lagging: usize::try_from(lagging).unwrap_or(usize::MAX),
            }));
        }

        if let Some(generation) = no_rows_sealed_under {
            // Form-v2 envelopes carry the generation id in cleartext right
            // after the version byte (`fauna_core::account_entry_crypto::
            // peek_generation_id`); a v1 envelope names none.
            let mut stmt = conn
                .prepare(
                    "SELECT entry_sealed FROM sync_changes
                     WHERE folder_id = ?1 AND item_class = ?2 AND superseded_at IS NULL
                       AND entry_sealed IS NOT NULL",
                )
                .context("prepare generation-use scan")?;
            let rows = stmt
                .query_map(
                    rusqlite::params![scope_folder_id, ItemClass::StateEntry.as_wire()],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .context("scan generation use")?;
            let mut in_use = 0usize;
            for entry in rows {
                let entry = entry.context("read sealed entry")?;
                if fauna_core::account_entry_crypto::peek_generation_id(&entry) == Some(generation)
                {
                    in_use += 1;
                }
            }
            if in_use > 0 {
                return Ok(Err(RetireError::GenerationInUse { rows: in_use }));
            }
        }

        let now = super::now_epoch_millis();
        let tx = conn
            .unchecked_transaction()
            .context("begin retire transaction")?;
        let changed = tx
            .execute(
                "UPDATE sync_changes SET superseded_at = ?1
                 WHERE seq = ?2 AND superseded_at IS NULL",
                rusqlite::params![now, row_seq],
            )
            .context("retire state entry")?;
        // The sweep: only a retire that landed, only with the flag, only the
        // belted generation — the belt above having just found it dataless.
        let sweep = if changed > 0 && delete_escrow_wraps {
            no_rows_sealed_under
        } else {
            None
        };
        if let Some(generation) = sweep {
            // Across the chain: a predecessor's kept wrap of the generation
            // goes with it (`generation_escrow.rs`, the kept wrap).
            let chain = super::generation_escrow::escrow_chain(&tx, &actor_id)?;
            super::generation_escrow::delete_chain_generation_wraps(&tx, &chain, &generation)
                .context("delete the retired generation's escrow wraps")?;
        }
        tx.commit().context("commit retire")?;
        Ok(Ok(changed > 0))
    }

    /// The scope's class-2 feed, filtered by a frontier vector and a serve-order
    /// watermark, and returned in nest-log (`seq`) order.
    ///
    /// `since` is the **nest-writer slot** (the shipped scalar cursor, which is
    /// what the charter means by "an omitted frontier is `{nest: since}`");
    /// `frontier` carries the device writers, hex `writer_id` → high-water
    /// `writer_seq`, a writer absent from it standing at 0 — unless
    /// `held_through_seq` is given, which is the **third gate**
    /// (`account-sync-plane.md` § Feeds and cursors → *Compaction is a
    /// serve-order watermark*): an UNNAMED device writer's row at or below it is
    /// held, so only its rows above it are served. A named writer keeps its own
    /// slot whatever the watermark says, and `None` is the two-gate filter
    /// byte-for-byte.
    ///
    /// The filter runs in Rust rather than SQL because the map is caller-shaped
    /// and unbounded in arity, and because the candidate set is the scope's
    /// **live** entries — one row per `(item_key, writer)` by construction, since
    /// every put collapses its own predecessors. Reading a superseded row is
    /// therefore impossible here, which is also what makes a `since = 0` walk the
    /// per-entry full-state reconcile that § Store logical schema names as the
    /// backstop: it returns exactly the current merged truth, not a replay of
    /// history.
    ///
    /// **Paging is an ordered prefix per writer** (`account-sync-plane.md`
    /// § Feeds and cursors → *What makes an entry redundant is a serve
    /// order*): rows come back in `seq` order and the caller truncates, and
    /// because a writer's rows enter the feed in its own `writer_seq` order, any `seq`
    /// prefix is an ordered prefix of every writer's rows — close early, never
    /// skip. The same law is what makes a `seq` watermark sound: every writer's
    /// rows at or below it are a prefix of that writer's rows.
    ///
    /// `sealed_under` narrows the serve to rows whose form-v2 cleartext header
    /// names that generation (the reclamation pass's "is anything still
    /// sealed under G" question — `account-data-taxonomy.md` § The generation
    /// machinery → *Fleet-scope reclamation*, clause (3e)); the tip is still
    /// taken over every live row, so the echo stays a claim about the whole
    /// log.
    pub async fn get_account_state_changes(
        &self,
        scope_folder_id: i64,
        since: i64,
        frontier: &std::collections::BTreeMap<String, i64>,
        held_through_seq: Option<i64>,
        sealed_under: Option<&[u8; 32]>,
    ) -> anyhow::Result<StateFeedRead> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {}
                 FROM sync_changes
                 WHERE folder_id = ?1 AND item_class = ?2 AND superseded_at IS NULL
                 ORDER BY seq",
                super::sync_storage::SYNC_CHANGE_COLUMNS,
            ))
            .context("prepare get_account_state_changes")?;
        let rows = stmt
            .query_map(
                rusqlite::params![scope_folder_id, ItemClass::StateEntry.as_wire()],
                super::sync_storage::sync_change_row_from_sql,
            )
            .context("query state entries")?;
        let mut read = StateFeedRead::default();
        for row in rows {
            let row = row.context("read state entry row")?;
            // Every live row moves the tip, gated or not: the tip is what the
            // scope's log holds, not what this caller is owed.
            read.tip = Some(row.seq);
            let seen = match (&row.origin_writer, row.origin_seq) {
                // A device writer's row: gated by that writer's own slot when
                // the caller names it, and — the third gate — by the
                // watermark when it does not.
                (Some(w), Some(s)) => {
                    let slot = frontier.get(&hex::encode(w)).copied();
                    s <= slot.unwrap_or(0)
                        || (slot.is_none() && held_through_seq.is_some_and(|held| row.seq <= held))
                }
                // The nest is the writer: its slot is `since`.
                _ => row.seq <= since,
            };
            if seen {
                continue;
            }
            if let Some(generation) = sealed_under
                && row
                    .entry_sealed
                    .as_deref()
                    .and_then(fauna_core::account_entry_crypto::peek_generation_id)
                    != Some(*generation)
            {
                continue;
            }
            read.rows.push(row);
        }
        Ok(read)
    }
}

impl CacheDb {
    /// This nest's **replica id** (`account-sync-plane.md` § The bind leg,
    /// ruling 2): the 16 random bytes minted once with the database
    /// (`migrations::MIGRATIONS_NEST_REPLICA`), stable across every boot over
    /// the same data dir and new on an empty one, whatever identity the box
    /// boots under. Served on every class-2 feed reply so a device can tell
    /// the replica it banked a watermark against from any other.
    pub async fn nest_replica_id(&self) -> anyhow::Result<[u8; 16]> {
        let conn = self.conn.lock().await;
        let id: Vec<u8> = conn
            .query_row(
                "SELECT replica_id FROM nest_replica WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .context("read the nest replica id")?;
        id.try_into()
            .map_err(|_| anyhow::anyhow!("nest replica id is not 16 bytes"))
    }
}

/// One read of a scope's class-2 feed
/// ([`CacheDb::get_account_state_changes`]).
#[derive(Default)]
pub struct StateFeedRead {
    /// The rows the gates let through, in `seq` order.
    pub rows: Vec<super::SyncChangeRow>,
    /// The highest `seq` among ALL the scope's live entries as of this read —
    /// gated or not — or `None` for a scope holding none. Taken in the same
    /// scan, under the same connection lock, as [`Self::rows`], which is what
    /// lets a reply claim completeness through it: `seq` is `AUTOINCREMENT`,
    /// so any row landing after the scan lands above it.
    pub tip: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::account_state::{ACCOUNT_STATE_SCOPE, OP_STATE_PUT};
    use std::collections::BTreeMap;

    const ACTOR: [u8; 32] = [0xA1; 32];
    const WRITER_A: [u8; 32] = [0x0A; 32];
    const WRITER_B: [u8; 32] = [0x0B; 32];
    const ITEM: [u8; 32] = [0x11; 32];
    const OTHER_ITEM: [u8; 32] = [0x22; 32];

    async fn scope(db: &CacheDb) -> i64 {
        db.get_or_create_state_scope(&ACTOR, ACCOUNT_STATE_SCOPE)
            .await
            .unwrap()
    }

    async fn fleet_scope(db: &CacheDb) -> i64 {
        db.get_or_create_state_scope(&ACTOR, ACCOUNT_STATE_FLEET_SCOPE)
            .await
            .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    async fn put(
        db: &CacheDb,
        fs: i64,
        item: &[u8; 32],
        writer: &[u8; 32],
        writer_seq: i64,
        entry: &[u8],
        cas_base: Option<i64>,
    ) -> anyhow::Result<Result<i64, StateEntryError>> {
        Ok(
            put_replacing(db, fs, item, writer, writer_seq, entry, cas_base, &[])
                .await?
                .map(|r| r.seq),
        )
    }

    /// [`put`] naming the rows it covers (`replaces`).
    #[allow(clippy::too_many_arguments)]
    async fn put_replacing(
        db: &CacheDb,
        fs: i64,
        item: &[u8; 32],
        writer: &[u8; 32],
        writer_seq: i64,
        entry: &[u8],
        cas_base: Option<i64>,
        replaces: &[ReplacedCoordinate],
    ) -> anyhow::Result<Result<RecordedStateEntry, StateEntryError>> {
        db.record_account_state_entry(
            &ACTOR,
            fs,
            item,
            writer,
            writer_seq,
            OP_STATE_PUT,
            entry,
            cas_base,
            replaces,
        )
        .await
    }

    #[tokio::test]
    async fn the_replica_id_is_minted_once_with_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nest.db");
        let first = {
            let db = CacheDb::open(&path).unwrap();
            let id = db.nest_replica_id().await.unwrap();
            assert_eq!(
                id,
                db.nest_replica_id().await.unwrap(),
                "stable across calls"
            );
            id
        };
        // A restart over the same data dir keeps it.
        let reopened = CacheDb::open(&path).unwrap();
        assert_eq!(reopened.nest_replica_id().await.unwrap(), first);
        // A fresh database is a different replica.
        let other = CacheDb::open(dir.path().join("rebuilt.db")).unwrap();
        assert_ne!(other.nest_replica_id().await.unwrap(), first);
    }

    async fn live(db: &CacheDb, fs: i64) -> Vec<super::super::SyncChangeRow> {
        db.get_account_state_changes(fs, 0, &BTreeMap::new(), None, None)
            .await
            .unwrap()
            .rows
    }

    #[tokio::test]
    async fn a_put_lands_a_sealed_entry_the_feed_serves_back_verbatim() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        let seq = put(&db, fs, &ITEM, &WRITER_A, 1, b"sealed-1", None)
            .await
            .unwrap()
            .unwrap();

        let rows = live(&db, fs).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, seq);
        assert_eq!(rows[0].path_hash, ITEM.to_vec(), "the blinded item key");
        assert_eq!(rows[0].origin_writer.as_deref(), Some(&WRITER_A[..]));
        assert_eq!(rows[0].origin_seq, Some(1));
        assert_eq!(
            rows[0].entry_sealed.as_deref(),
            Some(&b"sealed-1"[..]),
            "the nest echoes the sealed envelope byte-for-byte"
        );
        assert_eq!(
            rows[0].item_class.as_deref(),
            Some(ItemClass::StateEntry.as_wire())
        );
        assert_eq!(
            rows[0].size_bytes, 8,
            "ciphertext size is the cleartext floor's own field"
        );
    }

    /// The retention half of the plane: a writer's newer entry collapses its own
    /// predecessors, so the scope rests at live-set size instead of pinning
    /// every version forever.
    #[tokio::test]
    async fn a_writers_newer_entry_supersedes_its_own_predecessor() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        put(&db, fs, &ITEM, &WRITER_A, 1, b"v1", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &ITEM, &WRITER_A, 2, b"v2", None)
            .await
            .unwrap()
            .unwrap();

        let rows = live(&db, fs).await;
        assert_eq!(rows.len(), 1, "only the head stays live");
        assert_eq!(rows[0].entry_sealed.as_deref(), Some(&b"v2"[..]));
    }

    /// ⚠ The load-bearing multi-master pin. Collapsing across writers would
    /// delete the very entries the reader-side merge seam (W2.4) needs, so a
    /// concurrently-edited item would converge to whoever wrote last instead of
    /// merging. The reserved-rail helper keys on `(folder, path_hash)` alone,
    /// which is exactly the mistake this refuses.
    #[tokio::test]
    async fn one_writers_entry_never_supersedes_anothers_for_the_same_item() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        put(&db, fs, &ITEM, &WRITER_A, 1, b"from-a", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &ITEM, &WRITER_B, 1, b"from-b", None)
            .await
            .unwrap()
            .unwrap();

        let rows = live(&db, fs).await;
        assert_eq!(rows.len(), 2, "both writers' entries stay live");
        let mut seen: Vec<&[u8]> = rows
            .iter()
            .map(|r| r.entry_sealed.as_deref().unwrap())
            .collect();
        seen.sort();
        assert_eq!(seen, vec![&b"from-a"[..], &b"from-b"[..]]);
    }

    /// The head is per item; the coordinate is per writer. A seq this writer
    /// already used in the scope is refused on any OTHER item too, and the
    /// memory outlives the first row's collapse — the nest half of the
    /// journal-bound writer (refinement 11): a burnt journal re-issuing seqs
    /// would otherwise land a second row at a coordinate every replica keys
    /// its uniqueness on.
    #[tokio::test]
    async fn a_coordinate_this_writer_already_used_is_refused_on_any_item() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        put(&db, fs, &ITEM, &WRITER_A, 5, b"v5", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &ITEM, &WRITER_A, 7, b"v7", None)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            put(&db, fs, &OTHER_ITEM, &WRITER_A, 5, b"reissued", None)
                .await
                .unwrap(),
            Err(StateEntryError::SeqReused { seq: 5 }),
            "seq 5 is spent for writer A in this scope, even collapsed and on another item"
        );
        // Another writer's 5 is another coordinate; A's next unused seq lands.
        put(&db, fs, &OTHER_ITEM, &WRITER_B, 5, b"b5", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &OTHER_ITEM, &WRITER_A, 8, b"a8", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live(&db, fs).await.len(), 3);
    }

    /// A replayed put cannot re-enter the feed above the row that superseded it
    /// — which is what makes the kind safe for the offline/queued path with no
    /// idempotency key.
    #[tokio::test]
    async fn a_non_advancing_writer_seq_is_refused_as_stale() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        put(&db, fs, &ITEM, &WRITER_A, 5, b"v5", None)
            .await
            .unwrap()
            .unwrap();

        for replay in [5, 4, 0] {
            assert_eq!(
                put(&db, fs, &ITEM, &WRITER_A, replay, b"replay", None)
                    .await
                    .unwrap(),
                Err(StateEntryError::SeqNotAdvancing { head: 5 }),
                "writer_seq {replay} does not advance the head"
            );
        }
        let rows = live(&db, fs).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].entry_sealed.as_deref(),
            Some(&b"v5"[..]),
            "the refused replay wrote nothing"
        );
    }

    #[tokio::test]
    async fn cas_refuses_a_stale_base_and_accepts_the_live_head() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        // A base against an item with no head at all.
        assert_eq!(
            put(&db, fs, &ITEM, &WRITER_A, 1, b"v1", Some(7))
                .await
                .unwrap(),
            Err(StateEntryError::CasMismatch { head: None })
        );

        put(&db, fs, &ITEM, &WRITER_A, 1, b"v1", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            put(&db, fs, &ITEM, &WRITER_A, 2, b"v2", Some(99))
                .await
                .unwrap(),
            Err(StateEntryError::CasMismatch { head: Some(1) }),
            "a losing arbitrated writer changes no state"
        );
        assert!(
            put(&db, fs, &ITEM, &WRITER_A, 2, b"v2", Some(1))
                .await
                .unwrap()
                .is_ok(),
            "the live head is accepted"
        );
    }

    /// The bound: the key space is capped by COUNT, because blinding makes it
    /// uncapped by name. An update to an existing entry must stay free, or the
    /// cap would meter how often a user changes a setting rather than how many
    /// settings exist.
    #[tokio::test]
    async fn the_scope_cap_refuses_a_new_item_but_never_an_update() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        // Fill to the ceiling — one writer, one seq per row: a writer's seq
        // never recurs in a scope, whatever the item (refinement 11).
        for i in 0..MAX_STATE_ENTRIES_PER_SCOPE {
            let mut item = [0u8; 32];
            item[..8].copy_from_slice(&(i as u64).to_be_bytes());
            put(&db, fs, &item, &WRITER_A, i + 1, b"x", None)
                .await
                .unwrap()
                .unwrap();
        }
        let next_seq = MAX_STATE_ENTRIES_PER_SCOPE + 1;

        assert_eq!(
            put(
                &db,
                fs,
                &OTHER_ITEM,
                &WRITER_A,
                next_seq,
                b"one-too-many",
                None
            )
            .await
            .unwrap(),
            Err(StateEntryError::ScopeFull)
        );

        let mut existing = [0u8; 32];
        existing[..8].copy_from_slice(&0u64.to_be_bytes());
        assert!(
            put(&db, fs, &existing, &WRITER_A, next_seq, b"updated", None)
                .await
                .unwrap()
                .is_ok(),
            "updating an entry that already exists adds no key and is never refused"
        );
    }

    // ── Delegable-scope reclamation, part (2): a put names the rows it covers ──

    fn named(item: &[u8; 32], writer: &[u8; 32], writer_seq: i64) -> ReplacedCoordinate {
        ReplacedCoordinate {
            item_key: *item,
            writer: *writer,
            writer_seq,
        }
    }

    /// Fill `fs` to the cap with one-row items of `WRITER_A` (seqs
    /// `1..=MAX`), leaving item `ITEM` as the LAST row, so a caller can
    /// name it. Returns the next free `WRITER_A` seq.
    async fn fill_to_the_cap_ending_at_item(db: &CacheDb, fs: i64) -> i64 {
        for i in 0..MAX_STATE_ENTRIES_PER_SCOPE - 1 {
            let mut item = [0u8; 32];
            item[..8].copy_from_slice(&(i as u64).to_be_bytes());
            item[31] = 0xF0;
            put(db, fs, &item, &WRITER_A, i + 1, b"x", None)
                .await
                .unwrap()
                .unwrap();
        }
        put(
            db,
            fs,
            &ITEM,
            &WRITER_A,
            MAX_STATE_ENTRIES_PER_SCOPE,
            b"a-item",
            None,
        )
        .await
        .unwrap()
        .unwrap();
        MAX_STATE_ENTRIES_PER_SCOPE + 1
    }

    async fn live_count(db: &CacheDb, fs: i64) -> usize {
        live(db, fs).await.len()
    }

    async fn is_live(db: &CacheDb, fs: i64, item: &[u8; 32], writer: &[u8; 32], seq: i64) -> bool {
        live(db, fs).await.iter().any(|r| {
            r.path_hash.as_slice() == item.as_slice()
                && r.origin_writer.as_deref() == Some(writer.as_slice())
                && r.origin_seq == Some(seq)
        })
    }

    /// (i) A full scope refuses a new pair, and takes the same put when it
    /// names a live row of another writer: the named row leaves before the
    /// cap is counted, so the count is unchanged and the row is no longer
    /// served.
    #[tokio::test]
    async fn a_full_scope_takes_a_new_pair_that_names_a_live_row_it_replaces() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        fill_to_the_cap_ending_at_item(&db, fs).await;
        let full = live_count(&db, fs).await;
        assert_eq!(full as i64, MAX_STATE_ENTRIES_PER_SCOPE);

        assert_eq!(
            put(&db, fs, &ITEM, &WRITER_B, 1, b"b-item", None)
                .await
                .unwrap(),
            Err(StateEntryError::ScopeFull),
            "B's first row of ITEM is a new pair"
        );
        let landed = put_replacing(
            &db,
            fs,
            &ITEM,
            &WRITER_B,
            1,
            b"b-item",
            None,
            &[named(&ITEM, &WRITER_A, MAX_STATE_ENTRIES_PER_SCOPE)],
        )
        .await
        .unwrap()
        .expect("naming A's row makes B's put count-neutral");
        assert_eq!(landed.replaced, 1);
        assert_eq!(live_count(&db, fs).await, full);
        assert!(!is_live(&db, fs, &ITEM, &WRITER_A, MAX_STATE_ENTRIES_PER_SCOPE).await);
        assert!(is_live(&db, fs, &ITEM, &WRITER_B, 1).await);
    }

    /// (ii) A named row at a stale `writer_seq` (its writer has a newer live
    /// row) is not live at those coordinates: it is skipped, nothing is
    /// superseded, and the put is charged as the new pair it is.
    #[tokio::test]
    async fn a_row_named_at_a_stale_seq_is_skipped_and_the_put_is_charged() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        let next = fill_to_the_cap_ending_at_item(&db, fs).await;
        // A re-puts ITEM: its row at MAX collapses, the newer one is live.
        put(&db, fs, &ITEM, &WRITER_A, next, b"a-item-2", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            put_replacing(
                &db,
                fs,
                &ITEM,
                &WRITER_B,
                1,
                b"b-item",
                None,
                &[named(&ITEM, &WRITER_A, MAX_STATE_ENTRIES_PER_SCOPE)],
            )
            .await
            .unwrap(),
            Err(StateEntryError::ScopeFull)
        );
        assert!(is_live(&db, fs, &ITEM, &WRITER_A, next).await);

        // With room, the same put lands, replaces nothing, and adds a pair.
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &ITEM, &WRITER_A, 2, b"a2", None)
            .await
            .unwrap()
            .unwrap();
        let landed = put_replacing(
            &db,
            fs,
            &ITEM,
            &WRITER_B,
            1,
            b"b1",
            None,
            &[named(&ITEM, &WRITER_A, 1)],
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(landed.replaced, 0);
        assert!(is_live(&db, fs, &ITEM, &WRITER_A, 2).await);
        assert_eq!(live_count(&db, fs).await, 2);
    }

    /// (iii) A put refused `stale_writer_seq` or `cas_mismatch` supersedes
    /// nothing: its named rows stay live. (A refusal for room never follows
    /// a supersede — a superseded row frees the pair — and a put refused for
    /// room naming no live row is (ii).)
    #[tokio::test]
    async fn a_refused_put_supersedes_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &ITEM, &WRITER_B, 5, b"b5", None)
            .await
            .unwrap()
            .unwrap();
        let names = [named(&ITEM, &WRITER_A, 1)];

        // Not advancing B's head.
        assert!(matches!(
            put_replacing(&db, fs, &ITEM, &WRITER_B, 4, b"b4", None, &names)
                .await
                .unwrap(),
            Err(StateEntryError::SeqNotAdvancing { .. })
        ));
        // A coordinate B already used, on another item.
        assert!(matches!(
            put_replacing(&db, fs, &OTHER_ITEM, &WRITER_B, 5, b"b5", None, &names)
                .await
                .unwrap(),
            Err(StateEntryError::SeqReused { .. })
        ));
        // A losing CAS.
        assert!(matches!(
            put_replacing(&db, fs, &ITEM, &WRITER_B, 6, b"b6", Some(4), &names)
                .await
                .unwrap(),
            Err(StateEntryError::CasMismatch { .. })
        ));
        assert!(is_live(&db, fs, &ITEM, &WRITER_A, 1).await);
    }

    /// (iv) A replaced row keeps its coordinate: a replay of it is refused
    /// `SeqReused`, never re-admitted.
    #[tokio::test]
    async fn a_replay_of_a_replaced_row_is_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        put_replacing(
            &db,
            fs,
            &ITEM,
            &WRITER_B,
            1,
            b"b1",
            None,
            &[named(&ITEM, &WRITER_A, 1)],
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!is_live(&db, fs, &ITEM, &WRITER_A, 1).await);
        assert_eq!(
            put(&db, fs, &OTHER_ITEM, &WRITER_A, 1, b"a1", None)
                .await
                .unwrap(),
            Err(StateEntryError::SeqReused { seq: 1 })
        );
    }

    /// (v) No retention gate: a named row leaves although a counted walker's
    /// mark sits below it, where the plain retire of the same row answers
    /// `NotYetStable`.
    #[tokio::test]
    async fn a_replaced_row_waits_on_no_retention_gate() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        let a1 = put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        grant(&db, &WALKER_A).await;
        db.record_state_walk_mark(fs, &WALKER_A, a1 - 1)
            .await
            .unwrap();
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Err(RetireError::NotYetStable { lagging: 1 })
        );
        let landed = put_replacing(
            &db,
            fs,
            &ITEM,
            &WRITER_B,
            1,
            b"b1",
            None,
            &[named(&ITEM, &WRITER_A, 1)],
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(landed.replaced, 1);
        assert!(!is_live(&db, fs, &ITEM, &WRITER_A, 1).await);
    }

    fn bodies(rows: &[super::super::SyncChangeRow]) -> Vec<Vec<u8>> {
        rows.iter()
            .map(|r| r.entry_sealed.clone().unwrap())
            .collect()
    }

    /// The third gate (`account-sync-plane.md` § Feeds and cursors →
    /// *Compaction is a serve-order watermark*): with a watermark, an UNNAMED
    /// writer's rows at or below it are held and its rows above it are served,
    /// while a NAMED writer is gated by its own slot alone — its row below the
    /// watermark is still served when that slot says so.
    #[tokio::test]
    async fn the_watermark_gates_unnamed_writers_and_leaves_named_ones_to_their_slot() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        let a2 = put(&db, fs, &OTHER_ITEM, &WRITER_A, 2, b"a2", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &ITEM, &WRITER_B, 9, b"b9", None)
            .await
            .unwrap()
            .unwrap();

        let unnamed = db
            .get_account_state_changes(fs, 0, &BTreeMap::new(), Some(a2), None)
            .await
            .unwrap();
        assert_eq!(
            bodies(&unnamed.rows),
            vec![b"b9".to_vec()],
            "both of A's rows sit at or below the watermark; B's is above it"
        );

        let named_a = BTreeMap::from([(hex::encode(WRITER_A), 1i64)]);
        let named = db
            .get_account_state_changes(fs, 0, &named_a, Some(a2), None)
            .await
            .unwrap();
        assert_eq!(
            bodies(&named.rows),
            vec![b"a2".to_vec(), b"b9".to_vec()],
            "A is named, so its slot — not the watermark — decides its seq-2 row"
        );
    }

    /// No watermark is the two-gate serve byte-for-byte — the new nest facing
    /// an old requester — and the read's tip is the scope's highest live `seq`
    /// whether or not the caller is owed that row.
    #[tokio::test]
    async fn without_a_watermark_the_serve_is_unchanged_and_the_tip_counts_gated_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        let tip = put(&db, fs, &ITEM, &WRITER_B, 9, b"b9", None)
            .await
            .unwrap()
            .unwrap();

        let all = db
            .get_account_state_changes(fs, 0, &BTreeMap::new(), None, None)
            .await
            .unwrap();
        assert_eq!(
            bodies(&all.rows),
            vec![b"a1".to_vec(), b"b9".to_vec()],
            "an absent writer stands at 0"
        );
        assert_eq!(all.tip, Some(tip));

        let caught_up = BTreeMap::from([(hex::encode(WRITER_A), 1i64), (hex::encode(WRITER_B), 9)]);
        let gated = db
            .get_account_state_changes(fs, 0, &caught_up, None, None)
            .await
            .unwrap();
        assert!(gated.rows.is_empty());
        assert_eq!(
            gated.tip,
            Some(tip),
            "the tip is the scope's log, not the caller's page"
        );

        let untouched = db
            .get_or_create_state_scope(
                &ACTOR,
                fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE,
            )
            .await
            .unwrap();
        assert_eq!(
            db.get_account_state_changes(untouched, 0, &BTreeMap::new(), None, None)
                .await
                .unwrap()
                .tip,
            None,
            "a scope with no live entry has no tip"
        );
    }

    /// A per-writer high-water hides that writer's rows and nothing else — the
    /// frontier is a vector, not a scalar.
    #[tokio::test]
    async fn the_frontier_gates_each_writer_by_its_own_high_water() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;

        put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &OTHER_ITEM, &WRITER_A, 2, b"a2", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &ITEM, &WRITER_B, 9, b"b9", None)
            .await
            .unwrap()
            .unwrap();

        let mut frontier = BTreeMap::new();
        frontier.insert(hex::encode(WRITER_A), 1i64);
        let rows = db
            .get_account_state_changes(fs, 0, &frontier, None, None)
            .await
            .unwrap()
            .rows;
        let bodies: Vec<&[u8]> = rows
            .iter()
            .map(|r| r.entry_sealed.as_deref().unwrap())
            .collect();
        assert_eq!(
            bodies,
            vec![&b"a2"[..], &b"b9"[..]],
            "A's seq-1 row is below its slot; A's seq-2 and B's are not, \
             and an absent writer stands at 0"
        );

        // Both slots satisfied → nothing owed.
        frontier.insert(hex::encode(WRITER_A), 2i64);
        frontier.insert(hex::encode(WRITER_B), 9i64);
        assert!(
            db.get_account_state_changes(fs, 0, &frontier, None, None)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
    }

    // ── Fleet-scope reclamation: the retire kind and its retention gate ──

    const WALKER_A: [u8; 32] = [0x5A; 32];
    const WALKER_B: [u8; 32] = [0x5B; 32];

    /// A device row carrying `walker` as its live grant key — what makes a
    /// walk mark COUNT for the gate.
    async fn grant(db: &CacheDb, walker: &[u8; 32]) {
        db.register_sync_device(&ACTOR, walker, "walker", None, "")
            .await
            .unwrap();
        let outcome = db
            .set_sync_device_grant(&ACTOR, walker, walker, b"grant")
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            super::super::sync_storage::GrantStoreOutcome::Stored
        ));
    }

    #[allow(clippy::too_many_arguments)]
    async fn retire(
        db: &CacheDb,
        scope: &str,
        fs: i64,
        item: &[u8; 32],
        writer: &[u8; 32],
        writer_seq: i64,
        no_rows_sealed_under: Option<&[u8; 32]>,
    ) -> Result<bool, RetireError> {
        db.retire_account_state_entry(
            &ACTOR,
            scope,
            fs,
            item,
            writer,
            writer_seq,
            no_rows_sealed_under,
            false,
        )
        .await
        .unwrap()
    }

    /// [`retire`] carrying the escrow sweep beside the belt.
    #[allow(clippy::too_many_arguments)]
    async fn retire_sweeping(
        db: &CacheDb,
        scope: &str,
        fs: i64,
        item: &[u8; 32],
        writer: &[u8; 32],
        writer_seq: i64,
        generation: &[u8; 32],
    ) -> Result<bool, RetireError> {
        db.retire_account_state_entry(
            &ACTOR,
            scope,
            fs,
            item,
            writer,
            writer_seq,
            Some(generation),
            true,
        )
        .await
        .unwrap()
    }

    async fn escrow_wraps_of(db: &CacheDb, generation: &[u8; 32]) -> usize {
        db.get_generation_escrow_wraps(&ACTOR, Some(generation))
            .await
            .unwrap()
            .len()
    }

    /// The retire marks exactly the named live row superseded, inserts
    /// nothing, keeps the coordinate memory (a replay of the retired row is
    /// still refused), is idempotent, and leaves a newer row of the same
    /// `(item, writer)` alone when asked for the older seq.
    #[tokio::test]
    async fn a_retire_collapses_the_named_row_without_inserting_and_keeps_its_coordinate() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        let a1 = put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        let b1 = put(&db, fs, &OTHER_ITEM, &WRITER_B, 1, b"b1", None)
            .await
            .unwrap()
            .unwrap();
        assert!(a1 < b1);

        // No walkers marked: the gate is trivially open.
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Ok(true)
        );
        let rows = live(&db, fs).await;
        assert_eq!(rows.len(), 1, "only B's row is live: {rows:?}");
        assert_eq!(rows[0].seq, b1);
        // Idempotent: a second retire of the same coordinates is a no-op.
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Ok(false)
        );
        // The coordinate memory survives the retire: A cannot re-use seq 1.
        assert_eq!(
            put(&db, fs, &ITEM, &WRITER_A, 1, b"a1-again", None)
                .await
                .unwrap(),
            Err(StateEntryError::SeqReused { seq: 1 })
        );
        // The retired row does not count toward the cap: A's fresh row is a
        // new live entry, and the live count is what the cap reads.
        put(&db, fs, &ITEM, &WRITER_A, 2, b"a2", None)
            .await
            .unwrap()
            .unwrap();
        // A retire naming the OLDER seq of an item with a newer live row
        // retires nothing — the newer row is the live one.
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Ok(false)
        );
        assert_eq!(live(&db, fs).await.len(), 2);
    }

    /// The retention gate: a marked walker holding a live grant that has not
    /// walked past the row blocks the retire; walking past it (a higher mark)
    /// opens the gate; a mark whose grant is revoked, or that never had a
    /// grant, counts for nothing; a mark never lowers.
    #[tokio::test]
    async fn a_retire_waits_for_every_marked_walker_with_a_live_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        let a1 = put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        grant(&db, &WALKER_A).await;
        grant(&db, &WALKER_B).await;
        // Both walked, but only up to just before A's row.
        db.record_state_walk_mark(fs, &WALKER_A, a1 - 1)
            .await
            .unwrap();
        db.record_state_walk_mark(fs, &WALKER_B, a1 - 1)
            .await
            .unwrap();
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Err(RetireError::NotYetStable { lagging: 2 })
        );
        // A walks past it.
        db.record_state_walk_mark(fs, &WALKER_A, a1).await.unwrap();
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Err(RetireError::NotYetStable { lagging: 1 })
        );
        // A lower claim never lowers A's mark.
        db.record_state_walk_mark(fs, &WALKER_A, 0).await.unwrap();
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Err(RetireError::NotYetStable { lagging: 1 })
        );
        // A walker with NO grant on the actor blocks nothing, however stale.
        db.record_state_walk_mark(fs, &[0x5C; 32], 0).await.unwrap();
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Err(RetireError::NotYetStable { lagging: 1 })
        );
        // B's grant is revoked (a sign-out, a device delete): B's mark drops
        // out with it, and the gate opens.
        db.revoke_device_grant(&ACTOR, &WALKER_B).await.unwrap();
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Ok(true)
        );
    }

    /// The gate's watermark is the lowest COUNTED mark — the same predicate
    /// the gate refuses on, so a retire at or below it lands and one above
    /// it is refused: a mark whose key holds no grant on the actor, or a
    /// tombstoned one, holds it down no more than it holds the gate.
    #[tokio::test]
    async fn the_retirable_watermark_is_the_lowest_counted_walk_mark() {
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        // No mark at all: nothing is withheld, nothing is refused.
        assert_eq!(db.retirable_through_seq(fs, &ACTOR).await.unwrap(), None);
        let a1 = put(&db, fs, &ITEM, &WRITER_A, 1, b"a1", None)
            .await
            .unwrap()
            .unwrap();
        grant(&db, &WALKER_A).await;
        grant(&db, &WALKER_B).await;
        db.record_state_walk_mark(fs, &WALKER_A, a1).await.unwrap();
        db.record_state_walk_mark(fs, &WALKER_B, a1 - 1)
            .await
            .unwrap();
        // A walker with NO grant on the actor is not counted, however stale.
        db.record_state_walk_mark(fs, &[0x5C; 32], 0).await.unwrap();
        assert_eq!(
            db.retirable_through_seq(fs, &ACTOR).await.unwrap(),
            Some(a1 - 1)
        );
        // The watermark and the gate agree: A's row sits above it and is
        // refused; the moment B's grant is tombstoned the watermark is A's
        // mark, the row is at it, and the retire lands.
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Err(RetireError::NotYetStable { lagging: 1 })
        );
        db.revoke_device_grant(&ACTOR, &WALKER_B).await.unwrap();
        assert_eq!(
            db.retirable_through_seq(fs, &ACTOR).await.unwrap(),
            Some(a1)
        );
        assert_eq!(
            retire(&db, ACCOUNT_STATE_SCOPE, fs, &ITEM, &WRITER_A, 1, None).await,
            Ok(true)
        );
    }

    /// The generation belt: with `no_rows_sealed_under`, a live form-v2 row
    /// naming that generation in its cleartext header refuses the retire;
    /// rows under other generations, v1 rows, and retired rows do not.
    #[tokio::test]
    async fn a_retire_naming_a_generation_is_refused_while_a_live_row_is_sealed_under_it() {
        use fauna_core::account_entry_crypto::SEALED_ENTRY_V2;
        let db = CacheDb::open_in_memory().unwrap();
        let fs = fleet_scope(&db).await;
        let g = [0xAAu8; 32];
        let other = [0xBBu8; 32];
        let v2 = |generation: &[u8; 32]| {
            let mut e = vec![SEALED_ENTRY_V2];
            e.extend_from_slice(generation);
            e.extend_from_slice(b"ciphertext");
            e
        };
        // The "mint row" (v1, gen-0) A wants to retire, and a data row under
        // G by B.
        put(&db, fs, &ITEM, &WRITER_A, 1, b"\x01mint", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &OTHER_ITEM, &WRITER_B, 1, &v2(&g), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            retire(
                &db,
                ACCOUNT_STATE_FLEET_SCOPE,
                fs,
                &ITEM,
                &WRITER_A,
                1,
                Some(&g)
            )
            .await,
            Err(RetireError::GenerationInUse { rows: 1 })
        );
        // B re-seals its row under another generation (same item, same
        // writer — the old row collapses): G is dataless now.
        put(&db, fs, &OTHER_ITEM, &WRITER_B, 2, &v2(&other), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            retire(
                &db,
                ACCOUNT_STATE_FLEET_SCOPE,
                fs,
                &ITEM,
                &WRITER_A,
                1,
                Some(&g)
            )
            .await,
            Ok(true)
        );
    }

    /// The escrow sweep rides the belt (clause (3e)): a retire carrying
    /// `delete_escrow_wraps` beside `no_rows_sealed_under: G` takes the
    /// account's escrow wraps of G with it, in the same transaction — and
    /// only then. Refused `generation_in_use` it deletes nothing; without
    /// the flag it deletes nothing; landing no row it deletes nothing;
    /// another generation's wraps are never touched; and the flag without a
    /// belted generation is not a retire at all.
    #[tokio::test]
    async fn a_belted_retire_that_lands_sweeps_the_generations_escrow_wraps_and_nothing_else() {
        use fauna_core::account_entry_crypto::SEALED_ENTRY_V2;
        let db = CacheDb::open_in_memory().unwrap();
        let fs = fleet_scope(&db).await;
        let g = [0xAAu8; 32];
        let other = [0xBBu8; 32];
        let v2 = |generation: &[u8; 32]| {
            let mut e = vec![SEALED_ENTRY_V2];
            e.extend_from_slice(generation);
            e.extend_from_slice(b"ciphertext");
            e
        };
        for (generation, hash, wrap) in [
            (&g, [1u8; 32], &b"wrap of g"[..]),
            (&g, [2u8; 32], &b"a second holder's wrap of g"[..]),
            (&other, [3u8; 32], &b"wrap of the other generation"[..]),
        ] {
            db.put_generation_escrow_wrap(&ACTOR, generation, &hash, wrap)
                .await
                .unwrap();
        }
        // The receipt row A wants to retire, and a data row under G by B.
        put(&db, fs, &ITEM, &WRITER_A, 1, b"\x01receipt", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fs, &OTHER_ITEM, &WRITER_B, 1, &v2(&g), None)
            .await
            .unwrap()
            .unwrap();

        // The belt refuses: the wraps stay.
        assert_eq!(
            retire_sweeping(&db, ACCOUNT_STATE_FLEET_SCOPE, fs, &ITEM, &WRITER_A, 1, &g).await,
            Err(RetireError::GenerationInUse { rows: 1 })
        );
        assert_eq!(escrow_wraps_of(&db, &g).await, 2);

        // G goes dataless. A belted retire WITHOUT the flag lands and leaves
        // the wraps alone.
        put(&db, fs, &OTHER_ITEM, &WRITER_B, 2, &v2(&other), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            retire(
                &db,
                ACCOUNT_STATE_FLEET_SCOPE,
                fs,
                &ITEM,
                &WRITER_A,
                1,
                Some(&g)
            )
            .await,
            Ok(true)
        );
        assert_eq!(escrow_wraps_of(&db, &g).await, 2);

        // A retire that lands nothing (the row is already retired) deletes
        // nothing.
        assert_eq!(
            retire_sweeping(&db, ACCOUNT_STATE_FLEET_SCOPE, fs, &ITEM, &WRITER_A, 1, &g).await,
            Ok(false)
        );
        assert_eq!(escrow_wraps_of(&db, &g).await, 2);

        // The next receipt row of G retired with the flag: G's wraps go with
        // it, the other generation's stay.
        put(&db, fs, &ITEM, &WRITER_A, 2, b"\x01receipt", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            retire_sweeping(&db, ACCOUNT_STATE_FLEET_SCOPE, fs, &ITEM, &WRITER_A, 2, &g).await,
            Ok(true)
        );
        assert_eq!(escrow_wraps_of(&db, &g).await, 0);
        assert_eq!(escrow_wraps_of(&db, &other).await, 1);

        // The flag names no generation of its own: without the belt it is
        // refused outright, never honoured.
        put(&db, fs, &ITEM, &WRITER_A, 3, b"\x01receipt", None)
            .await
            .unwrap()
            .unwrap();
        assert!(
            db.retire_account_state_entry(
                &ACTOR,
                ACCOUNT_STATE_FLEET_SCOPE,
                fs,
                &ITEM,
                &WRITER_A,
                3,
                None,
                true,
            )
            .await
            .is_err()
        );
        assert_eq!(escrow_wraps_of(&db, &other).await, 1);
        assert_eq!(
            retire(
                &db,
                ACCOUNT_STATE_FLEET_SCOPE,
                fs,
                &ITEM,
                &WRITER_A,
                3,
                None
            )
            .await,
            Ok(true),
            "the refused sweep retired nothing"
        );
    }

    /// Regression for the cross-scope belt/sweep asymmetry : the belt used to scan only the request's own scope while
    /// the sweep deleted the actor's escrow wraps regardless of scope, so a
    /// belted sweep named in the delegable `state` scope — where no
    /// generation-sealed row ever lives — passed the belt vacuously and
    /// deleted the wraps of a generation still sealing a live row in
    /// `state-fleet`. `no_rows_sealed_under` (and `delete_escrow_wraps` with
    /// it) is now refused outright unless named in the fleet scope, so the
    /// belt named off the fleet scope never runs at all.
    #[tokio::test]
    async fn a_belted_sweep_is_refused_off_the_fleet_scope_even_though_the_belt_would_pass() {
        use fauna_core::account_entry_crypto::SEALED_ENTRY_V2;
        let db = CacheDb::open_in_memory().unwrap();
        let state_fs = scope(&db).await;
        let fleet_fs = fleet_scope(&db).await;
        let g = [0xAAu8; 32];
        let v2 = |generation: &[u8; 32]| {
            let mut e = vec![SEALED_ENTRY_V2];
            e.extend_from_slice(generation);
            e.extend_from_slice(b"ciphertext");
            e
        };
        db.put_generation_escrow_wrap(&ACTOR, &g, &[1u8; 32], b"wrap of g")
            .await
            .unwrap();
        // The receipt row A wants to retire, in the delegable `state` scope —
        // and a gen-0 row plus a live row sealed under G, both in the fleet
        // scope only, which is what the belt must see to refuse.
        put(&db, state_fs, &ITEM, &WRITER_A, 1, b"\x01receipt", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fleet_fs, &ITEM, &WRITER_A, 1, b"\x01gen0", None)
            .await
            .unwrap()
            .unwrap();
        put(&db, fleet_fs, &OTHER_ITEM, &WRITER_B, 1, &v2(&g), None)
            .await
            .unwrap()
            .unwrap();

        // Control: named in the fleet scope, the belt sees the live row and
        // refuses — the wrap stays.
        assert_eq!(
            retire_sweeping(
                &db,
                ACCOUNT_STATE_FLEET_SCOPE,
                fleet_fs,
                &ITEM,
                &WRITER_A,
                1,
                &g
            )
            .await,
            Err(RetireError::GenerationInUse { rows: 1 })
        );
        assert_eq!(escrow_wraps_of(&db, &g).await, 1);

        // Named in the delegable `state` scope instead, the belt must never
        // run at all — not scan `state_fs`, see nothing, and pass vacuously.
        assert!(
            db.retire_account_state_entry(
                &ACTOR,
                ACCOUNT_STATE_SCOPE,
                state_fs,
                &ITEM,
                &WRITER_A,
                1,
                Some(&g),
                true,
            )
            .await
            .is_err(),
            "a belted sweep named off the fleet scope must be refused outright"
        );
        assert_eq!(
            escrow_wraps_of(&db, &g).await,
            1,
            "G still seals a live fleet row — its wrap must survive"
        );
    }

    /// The feed's `sealed_under` filter serves exactly the live rows whose
    /// v2 header names the generation, and the echo's tip is still the whole
    /// log's.
    #[tokio::test]
    async fn the_sealed_under_filter_serves_only_rows_under_that_generation() {
        use fauna_core::account_entry_crypto::SEALED_ENTRY_V2;
        let db = CacheDb::open_in_memory().unwrap();
        let fs = scope(&db).await;
        let g = [0xAAu8; 32];
        let v2 = |generation: &[u8; 32]| {
            let mut e = vec![SEALED_ENTRY_V2];
            e.extend_from_slice(generation);
            e.extend_from_slice(b"ciphertext");
            e
        };
        put(&db, fs, &ITEM, &WRITER_A, 1, b"\x01gen0", None)
            .await
            .unwrap()
            .unwrap();
        let under_g = put(&db, fs, &OTHER_ITEM, &WRITER_B, 1, &v2(&g), None)
            .await
            .unwrap()
            .unwrap();
        let last = put(&db, fs, &[0x33; 32], &WRITER_B, 2, &v2(&[0xBBu8; 32]), None)
            .await
            .unwrap()
            .unwrap();
        let read = db
            .get_account_state_changes(fs, 0, &BTreeMap::new(), None, Some(&g))
            .await
            .unwrap();
        assert_eq!(read.rows.len(), 1);
        assert_eq!(read.rows[0].seq, under_g);
        assert_eq!(read.tip, Some(last), "the tip is the whole log's");
        let read = db
            .get_account_state_changes(fs, 0, &BTreeMap::new(), None, Some(&[0xCCu8; 32]))
            .await
            .unwrap();
        assert!(read.rows.is_empty());
        assert_eq!(read.tip, Some(last));
    }
}
