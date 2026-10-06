//! Key package and channel message methods.

use super::{CacheDb, blob_col_to_array, blob_to_array, now_epoch_secs};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

/// Most peer addresses one succession push fans out to
/// ([`CacheDb::succession_push_targets`]).
///
/// The address directory is nest-wide and any User-class caller can mint a
/// binding into it, so the target set is **attacker-extendable** and the fan-out
/// over it must be bounded — the same reasoning
/// [`crate::succession_pull::MAX_ANCHOR_CANDIDATES`] applies to the pull walk.
/// Unbounded, one crowded identity would turn a single succession into an
/// arbitrarily large outbound call storm.
///
/// Generous relative to any honest deployment: a succession is a rare human
/// ceremony, targets are ordered proven-first, and the pull leg is the backstop
/// for any peer this cap drops.
pub(crate) const MAX_PUSH_TARGETS: usize = 32;

/// Per-identity retention cap on the `nest_addresses` directory — first-N-wins
/// (fix): the write path refuses NEW rows beyond this many
/// per `nest_id`, capping the address mint at the source, while a retained
/// row's re-sighting still updates in place (proof stays monotonic). Matches
/// [`crate::succession_pull::MAX_ANCHOR_CANDIDATES`] deliberately: the pull
/// walk never reads more than that many candidates per identity, so retaining
/// more would only ever feed the push fan-out. After that fix, every row is
/// dial-backed — `(honest_id, attacker_url)` cannot be minted — so first-N-wins
/// caps only an identity's OWN address set and cannot be turned against a
/// victim's directory.
pub(crate) const MAX_DIRECTORY_ADDRESSES_PER_IDENTITY: usize = 8;

/// Outcome of [`CacheDb::claim_folder_channel`] — the first-binder-wins claim on
/// the `group_id -> ChannelId` namespace (shared-folders Slice 3 piece 5d-SEC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelClaimOutcome {
    /// The caller is the channel's authorized folder owner (newly claimed, or an
    /// idempotent re-bind by the existing claimant). For a freshly-claimed channel
    /// the caller was also registered on the `actor_channels` roster.
    Allowed,
    /// The channel is claimed by a **different** actor (a cross-owner clobber or a
    /// removed member's `share`-rebind), or the channel is **roster-populated** —
    /// via `actor_channels` (same-nest) or `channel_foreign_members` (cross-nest) —
    /// a conversation's channel is claimable by nobody, member or not, because the
    /// resulting claim is permanent and would freeze that group's commits (see
    /// [`CacheDb::claim_folder_channel`]). Reject the `share` /
    /// `content_key.put` / `members.evict`.
    Denied,
}

/// A member's access row on a claimed shared folder channel
/// (`folder_member_access`, multi-writer Phase 1). An **absent** row means
/// `reader` — this struct only exists for explicitly-granted rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMemberRoleRow {
    /// `"reader"` or `"writer"` (table CHECK-enforced).
    pub access: String,
    /// Per-member byte cap for writer records; `None` = uncapped (ratified).
    pub byte_cap: Option<i64>,
    /// Abuse counter of bytes this member's records currently contribute
    /// (floored at 0 on reclaim) — NOT exact attribution.
    pub bytes_used: i64,
}

/// What [`CacheDb::register_successor_carrying_seat`] moved besides seating
/// the successor. Counts, for the handler's log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeatCarried {
    /// Predecessor `actor_channels` rows removed from the channel.
    pub seats_retired: usize,
    /// Predecessor `folder_member_access` rows now naming the successor (a
    /// predecessor's grant dropped on a collision is not counted).
    pub grants: usize,
}

/// How much power a Welcome-relay caller holds over an EXISTING foreign-member
/// grant — the verdict `conversations_handlers::may_rebind_foreign_member`
/// hands [`CacheDb::register_foreign_channel_member`]'s conflict arm. The
/// insert arm ignores it entirely: a first grant has nothing to move, and
/// cross-nest DM initiation depends on it staying open to a caller on no
/// roster (`federation.md` § Cross-nest shared folders + channel append, the
/// *inviter-asserted (TOFU)* bullet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebindPower {
    /// No standing on the channel: the write may INSERT a first grant, never
    /// move an existing one.
    InsertOnly,
    /// Standing on the channel (a rostered actor on the unclaimed
    /// conversation rail): may move an existing grant only while it is
    /// UNCONFIRMED — the first authenticated use by the bound home nest pins
    /// it, which is what stops an MLS-removed ex-member's
    /// immortal roster row from moving an exercised grant.
    Standing,
    /// The claimant (a claimed folder channel's owner): may move the grant
    /// even once confirmed — the owner's rebind power is the accepted TOFU
    /// premise, and the folder rail's recovery flows (evict + re-invite)
    /// already run through the claimant. A claimant move resets the pin.
    Claimant,
}

impl CacheDb {
    // ==================== Key Packages ====================

    /// Store a key package for an actor.
    pub async fn put_key_package(
        &self,
        id: &str,
        actor_id: &[u8; 32],
        data: &[u8],
        published_at: u64,
        expires_at: u64,
    ) -> Result<()> {
        let id = id.to_string();
        let actor_id = *actor_id;
        let data = data.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO key_packages (id, actor_id, key_package_data, published_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, actor_id.as_slice(), data, published_at as i64, expires_at as i64],
        )
        .context("put key package")?;
        Ok(())
    }

    /// Store a reusable **last-resort** key package for an actor (Spec Y2).
    /// Unlike one-time KPs, `take_key_package` returns this without consuming it
    /// once the one-time pool is empty, so a target stays reachable after its
    /// pool drains. See `docs/goal/architecture/federation.md` § Key packages.
    ///
    /// **One last-resort row per actor.** The goal doc speaks of *the* (singular)
    /// reusable last-resort row, and every client re-publishes its last-resort KP
    /// on each login (`ensure_last_resort_keypackage`). To keep that idempotent —
    /// the random per-upload `id` would otherwise APPEND a fresh row every login —
    /// this deletes the actor's existing last-resort rows before inserting the new
    /// one, in a single transaction. (`take_key_package` returns the newest by
    /// `published_at`, so even a stray duplicate would be harmless, but a single
    /// row matches the spec and avoids unbounded growth.)
    pub async fn put_last_resort_key_package(
        &self,
        id: &str,
        actor_id: &[u8; 32],
        data: &[u8],
        published_at: u64,
        expires_at: u64,
    ) -> Result<()> {
        let id = id.to_string();
        let actor_id = *actor_id;
        let data = data.to_vec();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin last-resort tx")?;
        tx.execute(
            "DELETE FROM key_packages WHERE actor_id = ?1 AND last_resort = 1",
            rusqlite::params![actor_id.as_slice()],
        )
        .context("clear prior last-resort key packages")?;
        tx.execute(
            "INSERT OR REPLACE INTO key_packages (id, actor_id, key_package_data, published_at, expires_at, last_resort)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            rusqlite::params![id, actor_id.as_slice(), data, published_at as i64, expires_at as i64],
        )
        .context("put last-resort key package")?;
        tx.commit().context("commit last-resort tx")?;
        Ok(())
    }

    /// Fetch the oldest non-expired **one-time** key package for an actor,
    /// delete it, and return the data. When the one-time pool is empty, fall
    /// back to the actor's reusable **last-resort** key package and return it
    /// **without deleting** (Spec Y2 exhaustion defense). `None` only when the
    /// actor has neither.
    pub async fn take_key_package(&self, actor_id: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        // Lazy cleanup: remove expired key packages
        let _ = conn.execute(
            "DELETE FROM key_packages WHERE expires_at < ?1",
            rusqlite::params![now],
        );
        // One-time pool first (oldest first, consumed on take).
        let result = conn.query_row(
            "SELECT id, key_package_data FROM key_packages
             WHERE actor_id = ?1 AND expires_at > ?2 AND last_resort = 0
             ORDER BY published_at ASC LIMIT 1",
            rusqlite::params![actor_id.as_slice(), now],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        );
        match result {
            Ok((id, data)) => {
                conn.execute(
                    "DELETE FROM key_packages WHERE id = ?1",
                    rusqlite::params![id],
                )
                .context("delete taken key package")?;
                Ok(Some(data))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Fall back to the reusable last-resort KP — returned WITHOUT
                // deleting so the actor stays reachable under pool exhaustion.
                let last_resort = conn
                    .query_row(
                        "SELECT key_package_data FROM key_packages
                         WHERE actor_id = ?1 AND expires_at > ?2 AND last_resort = 1
                         ORDER BY published_at DESC LIMIT 1",
                        rusqlite::params![actor_id.as_slice(), now],
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                    .optional()
                    .context("take last-resort key package")?;
                Ok(last_resort)
            }
            Err(e) => Err(e).context("take key package"),
        }
    }

    /// Count the number of non-expired **one-time** key packages for an actor.
    /// Excludes the reusable last-resort KP so the client's top-up gauge
    /// (`ensure_keypackages`) measures only the consumable pool.
    pub async fn count_key_packages(&self, actor_id: &[u8; 32]) -> Result<u64> {
        let actor_id = *actor_id;
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        // Lazy cleanup: remove expired key packages
        let _ = conn.execute(
            "DELETE FROM key_packages WHERE expires_at < ?1",
            rusqlite::params![now],
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM key_packages
                 WHERE actor_id = ?1 AND expires_at > ?2 AND last_resort = 0",
                rusqlite::params![actor_id.as_slice(), now],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count as u64)
    }

    /// Whether an actor has at least one **usable** key package — one-time
    /// **or** last-resort. This is the server-side `addressable` probe folded
    /// into the anonymous `fauna.actor.by_handle` reply: it leaks yes/no, not a
    /// number (Spec Y2 § Key packages). Non-destructive.
    pub async fn has_usable_key_package(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor_id = *actor_id;
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM key_packages
                 WHERE actor_id = ?1 AND expires_at > ?2)",
                rusqlite::params![actor_id.as_slice(), now],
                |row| row.get(0),
            )
            .context("has_usable_key_package")?;
        Ok(exists)
    }

    /// List all non-expired key packages for an actor.
    pub async fn list_key_packages_for_actor(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<(String, Vec<u8>, i64, i64)>> {
        let actor_id = *actor_id;
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, key_package_data, published_at, expires_at
                 FROM key_packages WHERE actor_id = ?1 AND expires_at > ?2
                 ORDER BY published_at",
            )
            .context("prepare list_key_packages_for_actor")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice(), now], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .context("query key_packages_for_actor")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read key_package row")?);
        }
        Ok(results)
    }

    // ==================== Actor Channels ====================

    /// Register that an actor is a member of a channel (idempotent).
    pub async fn register_actor_channel(
        &self,
        actor_id: &[u8; 32],
        channel_id: &[u8; 32],
    ) -> Result<()> {
        let actor_id = *actor_id;
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT OR IGNORE INTO actor_channels (actor_id, channel_id, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![actor_id.as_slice(), channel_id.as_slice(), now],
        )
        .context("register_actor_channel")?;
        Ok(())
    }

    /// Register `successor` on a claimed folder channel and, **in the same
    /// transaction**, retire every recorded predecessor's seat there with its
    /// grant carried over — the home leg of *the grant follows the seat*
    /// (`writer-signed-change-records.md` § Writer-signed change records,
    /// ruling (8)(j)(2) and (4)).
    ///
    /// The caller is `welcome_deliver_core`'s same-nest roster register, and
    /// only once the claimant gate admitted the Welcome's sender: this is the
    /// sweep's add-successor Welcome, the propagation leg that seats a
    /// successor. For each local predecessor `P` of `successor` (the whole
    /// `actor_successions` walk, nearest hop first — one hop normally, more
    /// when a middle identity was succeeded before its own Welcome landed)
    /// that holds an `actor_channels` row on this channel:
    ///
    /// - `P`'s `folder_member_access` row is re-pointed to `successor`,
    ///   `bytes_used` and all, **unless `successor` already holds one** — then
    ///   the successor's own grant stands and `P`'s is dropped (the collision
    ///   rule: both are the owner's acts, and the one naming the successor is
    ///   the one the owner meant for it). With two seated predecessors the
    ///   nearest hop's grant wins for the same reason;
    /// - `P`'s `actor_channels` row is deleted.
    ///
    /// A predecessor with no seat here is left alone, grant included: a grant
    /// never moves without its seat. A recipient nobody succeeded into gets a
    /// plain register.
    ///
    /// ⚠ This deletes roster rows `succession_push_targets` reads under the
    /// retired id. That is sound only because the ceremony reads its push
    /// targets before its reply is sent (`recovery_handlers`'s
    /// `succession_push_targets_for_reply`), so no Welcome for the successor
    /// can arrive first.
    pub async fn register_successor_carrying_seat(
        &self,
        successor: &[u8; 32],
        channel_id: &[u8; 32],
    ) -> Result<SeatCarried> {
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction().context("begin tx")?;
        let now = now_epoch_secs();
        tx.execute(
            "INSERT OR IGNORE INTO actor_channels (actor_id, channel_id, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![successor.as_slice(), channel_id.as_slice(), now],
        )
        .context("register the successor's seat")?;

        let mut carried = SeatCarried::default();
        for old in super::successions::local_predecessors(&tx, successor)? {
            let seated: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM actor_channels
                                    WHERE actor_id = ?1 AND channel_id = ?2)",
                    rusqlite::params![old.as_slice(), channel_id.as_slice()],
                    |row| row.get(0),
                )
                .context("read the predecessor's seat")?;
            if !seated {
                continue;
            }
            carried.grants += tx
                .execute(
                    "INSERT INTO folder_member_access
                        (channel_id, actor_id, access, byte_cap, bytes_used, updated_at)
                     SELECT channel_id, ?3, access, byte_cap, bytes_used, updated_at
                       FROM folder_member_access
                      WHERE channel_id = ?2 AND actor_id = ?1
                     ON CONFLICT(channel_id, actor_id) DO NOTHING",
                    rusqlite::params![old.as_slice(), channel_id.as_slice(), successor.as_slice()],
                )
                .context("carry the predecessor's grant to the successor")?;
            tx.execute(
                "DELETE FROM folder_member_access WHERE channel_id = ?2 AND actor_id = ?1",
                rusqlite::params![old.as_slice(), channel_id.as_slice()],
            )
            .context("drop the predecessor's grant")?;
            carried.seats_retired += tx
                .execute(
                    "DELETE FROM actor_channels WHERE actor_id = ?1 AND channel_id = ?2",
                    rusqlite::params![old.as_slice(), channel_id.as_slice()],
                )
                .context("retire the predecessor's seat")?;
        }
        tx.commit().context("commit the successor's seat")?;
        Ok(carried)
    }

    /// Evict an actor from a channel roster — the **scoped** inverse of
    /// [`Self::register_actor_channel`]. The roster is otherwise append-only
    /// (`INSERT OR IGNORE`) but for one other delete, the retired seat
    /// [`Self::register_successor_carrying_seat`] removes when it seats a
    /// successor;
    /// shared-folder rotate-on-removal (F1/OBS-1,
    /// `docs/goal/architecture/mls-group-key-material.md` § M2) introduces this
    /// eviction path so a removed member loses discovery-metadata reads.
    /// **Always** scoped to the exact `(actor_id, channel_id)` pair (the composite
    /// PK) — never a blanket per-actor or per-channel delete. Returns whether a row
    /// was removed (`false` ⇒ the actor was already absent — idempotent).
    pub async fn evict_actor_from_channel(
        &self,
        actor_id: &[u8; 32],
        channel_id: &[u8; 32],
    ) -> Result<bool> {
        let actor_id = *actor_id;
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let removed = conn
            .execute(
                "DELETE FROM actor_channels WHERE actor_id = ?1 AND channel_id = ?2",
                rusqlite::params![actor_id.as_slice(), channel_id.as_slice()],
            )
            .context("evict_actor_from_channel")?;
        Ok(removed > 0)
    }

    /// Check whether an actor is a member of a channel.
    pub async fn is_actor_in_channel(
        &self,
        actor_id: &[u8; 32],
        channel_id: &[u8; 32],
    ) -> Result<bool> {
        let actor_id = *actor_id;
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM actor_channels WHERE actor_id = ?1 AND channel_id = ?2)",
            rusqlite::params![actor_id.as_slice(), channel_id.as_slice()],
            |row| row.get(0),
        )
        .context("is_actor_in_channel")?;
        Ok(exists)
    }

    /// First-binder-wins claim on the `group_id -> ChannelId` namespace for shared
    /// folders (`mls-group-key-material.md` § M2). Run
    /// **atomically** under the connection lock (no claim/register TOCTOU). The
    /// caller (`owner`) wants to bind a folder to `channel_id`:
    ///
    /// - **existing claim == `owner`** → [`ChannelClaimOutcome::Allowed`] (idempotent
    ///   re-bind; the owner is already on the roster from the first claim);
    /// - **existing claim != `owner`** → [`ChannelClaimOutcome::Denied`] — a removed
    ///   member's `share`-rebind or a cross-owner clobber;
    /// - **no claim, roster already populated** (a pre-existing channel — the
    ///   canonical case being a *conversation*, whose members were registered via
    ///   `welcome.deliver`): claimable **only** by an actor already on the roster.
    ///   An existing member claims it (recorded; not re-registered — already
    ///   present); a non-member is [`ChannelClaimOutcome::Denied`] (closes the
    ///   conv-roster-injection vector — this is the whole of the "treat a conv-born
    ///   channel as claimed by the conversation" caveat, resolved by reading the
    ///   shared roster with no conversations-side change);
    /// - **no claim, empty roster** (a fresh folder channel): the caller is the
    ///   first binder → record the claim **and** register them on the roster.
    ///
    /// The companion guards live in `folder_handlers` (`share` calls this; `put`
    /// and `evict` check [`Self::folder_channel_claimed_by`]).
    pub async fn claim_folder_channel(
        &self,
        owner: &[u8; 32],
        channel_id: &[u8; 32],
    ) -> Result<ChannelClaimOutcome> {
        let owner = *owner;
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();

        // Existing claim wins — idempotent for the claimant, denied for anyone else.
        let existing: Option<Vec<u8>> = conn
            .query_row(
                "SELECT claimed_by FROM folder_channel_claims WHERE channel_id = ?1",
                rusqlite::params![channel_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("claim_folder_channel lookup")?;
        if let Some(claimed_by) = existing {
            return Ok(if claimed_by == owner.as_slice() {
                ChannelClaimOutcome::Allowed
            } else {
                ChannelClaimOutcome::Denied
            });
        }

        // Unclaimed. Is the channel already populated (a conversation's roster
        // — same-nest members in `actor_channels`, cross-nest members in
        // `channel_foreign_members`)? The two are the same union
        // `list_channel_actors_union` reads and `folder_handlers.rs:1708`
        // documents: a `channel_foreign_members` row IS membership, not
        // bookkeeping, so a channel carrying only foreign grants must read as
        // populated exactly like one carrying only same-nest rows. Widening
        // costs nothing new: both tables are written by the identical
        // `claim_permits`-gated auto-register in `welcome.deliver`
        // (`register_actor_channel_gated_with_claim` /
        // `register_foreign_channel_member`'s insert arm), so a stranger who
        // could already plant an `actor_channels` row to permanently deny a
        // claim (accepted below) could equally plant a
        // `channel_foreign_members` one — this is not a new denial-of-service
        // surface, only the missing other half of the same check.
        let roster_populated: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM actor_channels WHERE channel_id = ?1)
                    OR EXISTS(SELECT 1 FROM channel_foreign_members WHERE channel_id = ?1)",
                rusqlite::params![channel_id.as_slice()],
                |row| row.get(0),
            )
            .context("claim_folder_channel roster check")?;
        if roster_populated {
            // **A pre-populated channel is not claimable by anyone**.
            //
            // This branch used to admit an existing roster member as "legit
            // conv-reuse". That made an ordinary group chat's channel claimable by
            // any one of its members, and a claim is **permanent**: INSERT and
            // SELECT are this table's only operations tree-wide — no DELETE, no
            // cascade, no admin surface, no migration cleanup. Once held, the
            // claim gate of that era refused every non-claimant `Commit` (so the
            // group's owner could no longer add, remove or rotate — that half is
            // since discharged, `federation.md` § Cross-nest shared folders +
            // channel append, 2026-08-24: Commit admission is now roster-
            // membership) and the claim-read gate suppresses the owner's roster
            // writes on later Welcomes, silently. The claimant could then abandon
            // the account with **no client able to repair the group** — precisely
            // the client-causable unrecoverable nest state
            // `docs/goal/architecture/nest/common.md` § Client-state
            // recoverability calls a bug rather than a deferred feature.
            //
            // Refusing outright makes that unrepresentable instead of merely
            // documented, and costs nothing today: `FoldersAuthor::share_set` mints
            // a *fresh* group for a first share (so its channel has no roster rows
            // and takes the branch below), and a 2nd..Nth share re-sends the set's
            // own group id, which the claimant fast-path above already answers
            // `Allowed` before this check. Verified at the only production caller,
            // `folder_handlers::share_core`.
            //
            // When a real conversation-binding flow is built it must make the
            // claim-read gate conv-reuse-aware (it must not suppress the
            // conversation owner — the Commit gate itself needs no further work
            // here: since 2026-08-24 it already admits any rostered member's
            // Commit regardless of claim state) and add a claim-release path —
            // not re-open this branch on its own. It must also decide,
            // explicitly, what happens to any pre-existing
            // `channel_foreign_members` rows on the channel it binds (inherit them
            // deliberately, or refuse to bind a channel carrying foreign grants the
            // claimant never issued) — the roster check above already routes such a
            // channel here rather than to the fresh-claim branch below, so the flow
            // WILL face this question the day it opens the branch; it cannot defer
            // it a second time.
            return Ok(ChannelClaimOutcome::Denied);
        }

        // Fresh folder channel: first binder claims it AND joins the roster.
        conn.execute(
            "INSERT INTO folder_channel_claims (channel_id, claimed_by, claimed_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![channel_id.as_slice(), owner.as_slice(), now],
        )
        .context("claim_folder_channel insert (fresh)")?;
        conn.execute(
            "INSERT OR IGNORE INTO actor_channels (actor_id, channel_id, created_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![owner.as_slice(), channel_id.as_slice(), now],
        )
        .context("claim_folder_channel register owner")?;
        Ok(ChannelClaimOutcome::Allowed)
    }

    /// The actor that holds the first-binder-wins claim on `channel_id`'s folder
    /// namespace, or `None` if unclaimed. `content_key.put` and `members.evict`
    /// require the caller to equal this (the envelope-slot publisher /
    /// the roster-evictor must be the channel's authorized owner, not merely the
    /// owner of *some* set bound to the group).
    ///
    /// A `claimed_by` blob that is not exactly 32 bytes is an `Err`, never a
    /// silent `Ok(None)`: every caller either propagates this error or
    /// gates on it, so surfacing it here is the only place a corrupt blob and
    /// a genuinely-unclaimed channel can be told apart at all.
    pub async fn folder_channel_claimed_by(
        &self,
        channel_id: &[u8; 32],
    ) -> Result<Option<[u8; 32]>> {
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let claimed_by: Option<Vec<u8>> = conn
            .query_row(
                "SELECT claimed_by FROM folder_channel_claims WHERE channel_id = ?1",
                rusqlite::params![channel_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("folder_channel_claimed_by")?;
        claimed_by
            .map(|b| blob_to_array(b.as_slice(), "folder_channel_claims.claimed_by"))
            .transpose()
    }

    /// List all actor IDs that are members of a channel.
    pub async fn list_channel_actors(&self, channel_id: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT actor_id FROM actor_channels WHERE channel_id = ?1")?;
        let rows = stmt
            .query_map(rusqlite::params![channel_id.as_slice()], |row| {
                blob_col_to_array(row.get(0)?, 0, "actor_id")
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("list_channel_actors")?;
        Ok(rows)
    }

    /// Grant or change a member's access on a claimed shared folder channel —
    /// the write behind the owner+claimant-gated `fauna.folders.members.
    /// set_access` and the share-time `access` param (multi-writer Phase 1;
    /// `ui/folders.md` § Sharing owns the access model). Upsert preserving
    /// `bytes_used` (the abuse counter survives a role edit); `byte_cap = None`
    /// = uncapped (ratified — the warning lives client-side). `access` is
    /// validated here (belt) and by the table CHECK (braces).
    pub async fn set_folder_member_access(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
        access: &str,
        byte_cap: Option<i64>,
    ) -> Result<()> {
        anyhow::ensure!(
            access == "reader" || access == "writer",
            "invalid folder member access {access:?} (reader|writer)"
        );
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let access = access.to_string();
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO folder_member_access (channel_id, actor_id, access, byte_cap, bytes_used, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)
             ON CONFLICT(channel_id, actor_id) DO UPDATE SET
                 access = excluded.access,
                 byte_cap = excluded.byte_cap,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                channel_id.as_slice(),
                actor_id.as_slice(),
                access,
                byte_cap,
                now
            ],
        )
        .context("set_folder_member_access")?;
        Ok(())
    }

    /// A member's access row on a claimed folder channel, or `None` — and an
    /// absent row means **reader** (callers apply that
    /// default, never store it). Read by the `writable_folder` gate, the
    /// metering cap check, and the `members.list_actors` enrichment.
    pub async fn get_folder_member_role(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<Option<FolderMemberRoleRow>> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT access, byte_cap, bytes_used FROM folder_member_access \
             WHERE channel_id = ?1 AND actor_id = ?2",
            rusqlite::params![channel_id.as_slice(), actor_id.as_slice()],
            |row| {
                Ok(FolderMemberRoleRow {
                    access: row.get(0)?,
                    byte_cap: row.get(1)?,
                    bytes_used: row.get(2)?,
                })
            },
        )
        .optional()
        .context("get_folder_member_role")
    }

    /// Every access row on a channel, keyed by actor — the batch read backing
    /// the `members.list_actors` enrichment (one query, not N).
    pub async fn list_folder_member_access(
        &self,
        channel_id: &[u8; 32],
    ) -> Result<Vec<([u8; 32], FolderMemberRoleRow)>> {
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id, access, byte_cap, bytes_used FROM folder_member_access \
             WHERE channel_id = ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![channel_id.as_slice()], |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok((
                    blob,
                    FolderMemberRoleRow {
                        access: row.get(1)?,
                        byte_cap: row.get(2)?,
                        bytes_used: row.get(3)?,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("list_folder_member_access")?;
        Ok(rows
            .into_iter()
            .filter_map(|(blob, role)| {
                <[u8; 32]>::try_from(blob.as_slice())
                    .ok()
                    .map(|a| (a, role))
            })
            .collect())
    }

    /// Drop a member's access row — called when the member leaves the roster
    /// (`members.evict` rotate-on-removal, `folders.leave`). The grant dies
    /// with the membership so a later re-add starts from the fail-safe default
    /// (absent row = reader), never a resurrected `writer`. Idempotent.
    pub async fn delete_folder_member_role(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<()> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM folder_member_access WHERE channel_id = ?1 AND actor_id = ?2",
            rusqlite::params![channel_id.as_slice(), actor_id.as_slice()],
        )
        .context("delete_folder_member_role")?;
        Ok(())
    }

    /// Register a **foreign** channel member — an actor on a peer nest that THIS
    /// nest (the channel's home) relayed an MLS Welcome to. Records the member's
    /// home `nest_id` so a later `fauna.federation.channel.fetch` from that nest can
    /// be authorized to pull the channel's application messages (`direct-messages.md`
    /// § Technical Flow — Cross-Nest, step 3; `federation.md` § Trust model — the
    /// membership gate that holds against a hostile signer). Idempotent; updates the
    /// home `nest_id` on a re-invite (e.g. the member migrated nests) — but only
    /// when the caller's [`RebindPower`] reaches the stored row, which is a
    /// strictly narrower question than being authorized to write.
    ///
    /// **`power` splits insert-if-absent from update-existing**
    /// (`federation.md` § Cross-nest shared folders + channel append, the
    /// *inviter-asserted (TOFU)* bullet). The row's `home_nest_id` IS an
    /// authorization: `require_foreign_member` serves `channel.fetch`,
    /// `channel.actors`, `channel.append`, `folder.changes.fetch` and
    /// `folder.content_key.fetch` off it — `channel.leave` reads the row
    /// directly instead (an absent row must fold to an idempotent success,
    /// which the shared gate can't express, and the row is deleted so no pin
    /// is owed). The ratified
    /// premise accepts re-binding *"within the inviter's existing power"* — so an
    /// existing grant may be moved only by a caller who has standing on the
    /// channel (the claimant, or a current roster member), never by everyone the
    /// permissive `Unclaimed` arm lets write a FIRST grant. The arm's own
    /// justification — a first DM Welcome's caller is on no roster yet, and
    /// delivery depends on the grant — is an argument about the first write
    /// alone, and an insert has nothing to overwrite, so DM initiation is
    /// untouched.
    ///
    /// **A CONFIRMED binding additionally refuses the standing-based arm**
    /// (`RebindPower::Standing`; the first-use pin): once the bound home nest has exercised the grant on an
    /// authenticated federation connection ([`Self::confirm_foreign_member`],
    /// fired by `require_foreign_member`'s first success), "was ever rostered" —
    /// which is all the conversation rail's monotone roster can attest, an
    /// MLS-removed ex-member included — no longer reaches it. Only the claimant
    /// ([`RebindPower::Claimant`], a claimed folder channel's owner, whose
    /// rebind power is the accepted premise) may still move a confirmed grant,
    /// and a successful move RESETS the pin: the incoming nest must earn its
    /// own first-use confirmation, and a stale stamp can never vouch for a
    /// value it did not witness.
    ///
    /// A refused rebind keeps the stored `home_nest_id`; the write still
    /// succeeds (delivery is best-effort and must not fail on it) and the
    /// address columns below still refresh, which is harmless: propagation dials
    /// the `nest_addresses` directory keyed by the *stored* `home_nest_id`, never
    /// this row's `nest_url`.
    ///
    /// `nest_url` is the member's home nest as an **address** rather than an
    /// identity — the caller already holds it (it is the URL it just relayed the
    /// Welcome to), and identity-succession propagation needs to originate *to*
    /// this peer, which a bare `nest_id` cannot express. `None` is accepted so a
    /// caller with no URL is not forced to invent one; such a row simply is not a
    /// propagation target.
    pub async fn register_foreign_channel_member(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
        home_nest_id: &[u8; 32],
        nest_url: Option<&str>,
        power: RebindPower,
    ) -> Result<()> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let home_nest_id = *home_nest_id;
        let nest_url = nest_url.map(|u| u.trim_end_matches('/').to_string());
        let may_rebind = matches!(power, RebindPower::Standing | RebindPower::Claimant);
        let pin_exempt = matches!(power, RebindPower::Claimant);
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "INSERT INTO channel_foreign_members
                (channel_id, actor_id, home_nest_id, created_at, nest_url)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(channel_id, actor_id) DO UPDATE SET
                 -- The rebind gate, decided in SQL so the row-exists question is
                 -- answered atomically with the write rather than in a racing
                 -- read above it (both the standing half ?6 and the first-use
                 -- pin, which only the claimant ?7 passes). An unauthorized
                 -- re-invite is a no-op here, not an error.
                 home_nest_id = CASE WHEN ?6
                         AND (?7 OR channel_foreign_members.confirmed_at IS NULL)
                     THEN excluded.home_nest_id
                     ELSE channel_foreign_members.home_nest_id END,
                 -- A move lands an UNEXERCISED binding: reset the pin so the
                 -- incoming nest must earn its own confirmation. A refused or
                 -- same-value write keeps the stamp. (Every SET right-hand side
                 -- reads the PRE-update row, so this CASE and the one above
                 -- agree on which arm fired.)
                 confirmed_at = CASE WHEN ?6
                         AND (?7 OR channel_foreign_members.confirmed_at IS NULL)
                         AND excluded.home_nest_id <> channel_foreign_members.home_nest_id
                     THEN NULL
                     ELSE channel_foreign_members.confirmed_at END,
                 -- Never blank a known address with a re-invite that lacks one:
                 -- losing it would silently drop this peer from propagation.
                 nest_url = COALESCE(excluded.nest_url, channel_foreign_members.nest_url)",
            rusqlite::params![
                channel_id.as_slice(),
                actor_id.as_slice(),
                home_nest_id.as_slice(),
                now,
                nest_url,
                may_rebind,
                pin_exempt
            ],
        )
        .context("register_foreign_channel_member")?;
        // Record the address in the nest-wide directory, where it JOINS rather
        // than displaces. The membership row above is keyed on
        // `(channel_id, actor_id)`, so its upsert necessarily overwrites the
        // address column; propagation reads the directory instead, and this row
        // is what keeps the honest address alive across a plant.
        //
        // Recorded **unproven**: at this point the URL is only what the caller
        // asserted. The proof is stamped where it actually happens — a
        // successful federation handshake in `federation_pool::get_or_dial`.
        if let Some(url) = nest_url.as_deref() {
            Self::record_nest_address_in(&conn, &home_nest_id, url, false, now)?;
        }
        Ok(())
    }

    /// Record that `nest_id` was seen at `nest_url`, in the nest-wide id→URL
    /// directory (`migrations::MIGRATIONS_NEST_ADDRESSES`).
    ///
    /// **Addresses only ever join.** A differing URL for a known identity is a
    /// new row, never a rewrite — that is the whole point of the table, and what
    /// makes an erasure unrepresentable rather than merely guarded.
    ///
    /// `first_seen` never moves later on a re-sighting (re-registering an
    /// address must not reorder the candidate walk), and `proven_at` is only
    /// ever *set*, never cleared: a later merely-sighted arrival cannot demote
    /// an address a federation handshake already bound to this identity.
    fn record_nest_address_in(
        conn: &rusqlite::Connection,
        nest_id: &[u8; 32],
        nest_url: &str,
        proven: bool,
        now: i64,
    ) -> Result<()> {
        let url = nest_url.trim_end_matches('/');
        if url.is_empty() {
            return Ok(());
        }
        conn.execute(
            "INSERT INTO nest_addresses (nest_id, nest_url, first_seen, proven_at)
             SELECT ?1, ?2, ?3, ?4
              -- First-N-wins per identity (fix): a NEW row is
              -- admitted only below the per-identity cap, counting the OTHER
              -- retained addresses so a re-sighting of a retained URL still
              -- reaches the conflict-update arms at the cap. After that fix, every
              -- row is dial-backed, so this caps an identity's own mint only.
              WHERE (SELECT COUNT(*) FROM nest_addresses
                      WHERE nest_id = ?1 AND nest_url <> ?2) < ?5
             ON CONFLICT(nest_id, nest_url) DO UPDATE SET
                 -- Keep the earliest sighting: a re-registration must not move
                 -- an address later in the oldest-first walk.
                 first_seen = MIN(nest_addresses.first_seen, excluded.first_seen),
                 -- Proof is monotonic — set once, never cleared by a later
                 -- unproven sighting of the same address.
                 proven_at  = COALESCE(nest_addresses.proven_at, excluded.proven_at)",
            rusqlite::params![
                nest_id.as_slice(),
                url,
                now,
                if proven { Some(now) } else { None },
                MAX_DIRECTORY_ADDRESSES_PER_IDENTITY as i64
            ],
        )
        .context("record_nest_address")?;
        Ok(())
    }

    /// Record a peer address sighting in the id→URL directory.
    ///
    /// `proven` asserts that a federation `hello` handshake actually bound this
    /// identity to this URL ([`crate::federation_channel::dial`] — a signed
    /// possession proof), rather than the URL merely having been *asserted* by
    /// some caller. Readers prefer proven addresses, which an attacker cannot
    /// reach without the honest nest's key.
    pub async fn record_nest_address(
        &self,
        nest_id: &[u8; 32],
        nest_url: &str,
        proven: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        Self::record_nest_address_in(&conn, nest_id, nest_url, proven, now)
    }

    /// Distinct peer-nest URLs holding cross-nest residue about `actor_id` — the
    /// **push** target set of identity-succession propagation
    /// (`identity-succession.md:81`).
    ///
    /// Two directions, one query. A peer is a target if it hosts a foreign member
    /// of a channel `actor_id` belongs to (the peers whose users share a
    /// conversation or folder with them), or if `actor_id` is itself the
    /// foreign member recorded there (this nest hosting the channel, the peer
    /// being their own home — a self-succession the peer must also learn).
    ///
    /// Rows with no recorded URL are skipped, not an error: an un-addressable
    /// peer is the declared residual (`identity-succession.md:103`), and the
    /// pull leg is what covers it.
    ///
    /// Addresses come from the nest-wide `nest_addresses` directory keyed by the
    /// residue rows' `home_nest_id`, **not** from the membership row's own
    /// column: that column is overwritten in place by the
    /// membership upsert, so reading it sent the push to whatever address was
    /// written *last* — with no walk and no fallback, the sharpest form of the
    /// same defect the pull leg had. A push is only a hint (the receiver
    /// re-verifies against its own anchor), so a stale address costs one
    /// wasted call, never correctness.
    ///
    /// **The cap is per-identity-fair, never a global sort.**
    /// Addresses are ranked *within* their identity (proven-first, then
    /// first-seen, rowid tiebreak) and the global bound consumes rank-1 rows of
    /// every identity before any identity's rank-2 row — so one identity
    /// holding many early proven addresses (each costs only a dial: 32 URLs
    /// behind one wildcard cert) cannot evict a later-seen honest peer's best
    /// address. Under this interleave, evicting a peer entirely requires
    /// [`MAX_PUSH_TARGETS`] *distinct dial-proven identities* with residue,
    /// not [`MAX_PUSH_TARGETS`] URLs; the hourly pull remains the declared
    /// backstop for anything past the bound. The `rowid` legs pin the order
    /// under `first_seen` ties — second-granular stamps make ties the common
    /// case for rows written in one burst.
    pub async fn succession_push_targets(&self, actor_id: &[u8; 32]) -> Result<Vec<String>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT nest_url FROM (
                SELECT a.nest_url AS nest_url,
                       (a.proven_at IS NULL) AS unproven,
                       a.first_seen AS first_seen,
                       a.rowid AS arowid,
                       ROW_NUMBER() OVER (
                           PARTITION BY a.nest_id
                           ORDER BY (a.proven_at IS NULL) ASC, a.first_seen ASC, a.rowid ASC
                       ) AS addr_rank
                  FROM nest_addresses a
                 WHERE a.nest_url <> ''
                   AND a.nest_id IN (
                         SELECT f.home_nest_id
                           FROM channel_foreign_members f
                          WHERE f.actor_id = ?1
                             OR f.channel_id IN (SELECT channel_id FROM actor_channels
                                                  WHERE actor_id = ?1)
                             OR f.channel_id IN (SELECT channel_id FROM channel_foreign_members
                                                  WHERE actor_id = ?1)
                       )
             )
             ORDER BY addr_rank ASC, unproven ASC, first_seen ASC, arowid ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(
                rusqlite::params![actor_id.as_slice(), MAX_PUSH_TARGETS as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("succession_push_targets")?;
        // Two identities can legitimately share one URL (an SNI-fronted box);
        // the push dials URLs, so dedup keeping first occurrence. A duplicate
        // spends one LIMIT slot — bounded and rare, not worth a GROUP BY.
        let mut seen = std::collections::HashSet::new();
        Ok(rows
            .into_iter()
            .filter(|u| seen.insert(u.clone()))
            .collect())
    }

    /// Every distinct remote actor this nest holds foreign-channel residue about
    /// — the **pull** leg's work list. The pull resolves each one's anchor
    /// separately ([`Self::oldest_foreign_member_nest_id`] +
    /// [`Self::resolve_foreign_nest_urls`]); returning bare actors keeps the
    /// anchor logic in one place rather than duplicated into a join. (The URL
    /// resolver is [`Self::resolve_foreign_nest_urls`].)
    pub async fn distinct_foreign_member_actors(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT DISTINCT actor_id FROM channel_foreign_members")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("distinct_foreign_member_actors")?;
        Ok(rows
            .into_iter()
            .filter_map(|id| <[u8; 32]>::try_from(id.as_slice()).ok())
            .collect())
    }

    /// The **anchor nest identity** for one remote actor: the `home_nest_id` of
    /// its **oldest residue row overall** (`created_at`, then insertion order),
    /// addressable or not. This is the identity a peer is willing to trust as
    /// the chain source at first contact — the binding it learned *first*.
    ///
    /// Deliberately the oldest row *overall*, not the oldest *addressable* one:
    /// a binding can carry no URL (NULL), so skipping forward to the first addressable row would hand the
    /// anchor to whatever binding — including an attacker's — first carried a
    /// URL. The URL to dial is resolved separately, from a
    /// binding that shares *this* identity ([`Self::resolve_foreign_nest_urls`]),
    /// and the dial itself must prove the identity, so a wrong or planted URL
    /// fails closed rather than becoming the anchor — and since that resolver
    /// returns *every* candidate, a planted URL cannot deny the anchor either.
    ///
    /// "Oldest" is the row, not the id: a legitimate re-invite may rewrite the
    /// oldest row's `home_nest_id` (the member genuinely migrated nests) at
    /// first contact — but once this nest has verified the identity once, the
    /// **persisted** `foreign_recovery_heads.anchor_nest_id` pin overrides this
    /// value, so a later rewrite cannot move a proven anchor.
    pub async fn oldest_foreign_member_nest_id(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<[u8; 32]>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT home_nest_id FROM channel_foreign_members
              WHERE actor_id = ?1
              ORDER BY created_at ASC, rowid ASC
              LIMIT 1",
            rusqlite::params![actor_id.as_slice()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("oldest_foreign_member_nest_id")?
        .map(|id| {
            <[u8; 32]>::try_from(id.as_slice())
                .map_err(|_| anyhow::anyhow!("stored home_nest_id is not 32 bytes"))
        })
        .transpose()
    }

    /// Resolve the **candidate** dialable URLs for a nest identity — every
    /// address the `nest_addresses` directory holds for `nest_id`,
    /// **dial-proven first, then oldest first**, capped at `limit`. The
    /// directory is nest-wide, so a single home's addresses are shared by every
    /// binding to it. Empty when no address names the identity: the anchor is
    /// then un-resolvable and both propagation legs refuse
    /// (`identity-succession.md:104`'s declared residual) — never falling back
    /// to another identity's URL.
    ///
    /// **A list, not one row, because availability is the property under attack
    /// here**. Every dial must *prove* the identity, so a
    /// planted row naming an honest identity at an attacker's box cannot cause a
    /// takeover — but until this returned a list, one such row (mintable by any
    /// User-class caller through `welcome_deliver_core`, for *any* actor, since
    /// this directory is nest-wide) was the single URL both propagation legs
    /// resolved, and its failed dial ended the pass. That silently denied the
    /// owner's succession forever, which is precisely the seed thief's win.
    /// Handing the caller every candidate makes the walk fail over instead.
    ///
    /// **Proven first, then oldest first.** An earlier revision ordered by age
    /// alone, on the premise that *"a row an attacker adds later is all they can
    /// mint, so the honest address is normally the first dial"*. That premise
    /// was **false**: the membership write is an upsert on
    /// `(channel_id, actor_id)`, so a plant rewrote an existing row in place —
    /// inheriting its sighting *and* deleting the honest address, which left the
    /// walk nothing to fail over to. The directory closes that by construction
    /// (addresses join, never displace), and `proven_at` then orders what
    /// survives: an address a federation handshake bound to this identity
    /// outranks one a caller merely asserted, and an attacker cannot reach
    /// proven status without the honest nest's key.
    ///
    /// A genuine address migration still resolves — both addresses are
    /// candidates and the stale one simply fails its proof as the walk moves on
    /// — so this costs correctness nothing and one wasted dial in the rare
    /// migration case.
    ///
    /// One row per address by construction (PK `(nest_id, nest_url)`), ordered
    /// by each URL's *first* sighting: many bindings naming one address must not
    /// spend the caller's budget on the same box, and re-registering an address
    /// must not move it later in the order.
    pub async fn resolve_foreign_nest_urls(
        &self,
        nest_id: &[u8; 32],
        limit: usize,
    ) -> Result<Vec<String>> {
        let nest_id = *nest_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT nest_url
               FROM nest_addresses
              WHERE nest_id = ?1 AND nest_url <> ''
              ORDER BY (proven_at IS NULL) ASC, first_seen ASC, rowid ASC
              LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![nest_id.as_slice(), limit as i64], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("resolve_foreign_nest_urls")?;
        Ok(rows)
    }

    /// The **dial-proven** addresses a federation handshake bound to `nest_id`,
    /// oldest first — the subset of [`Self::resolve_foreign_nest_urls`] an
    /// attacker cannot reach without the honest nest's key (`proven_at` is set
    /// only by [`crate::federation_channel::dial`]'s signed possession proof,
    /// and never cleared — the same monotonicity `record_nest_address_in`
    /// relies on).
    ///
    /// `welcome_deliver_core`'s cross-nest relay uses this to bind a relayed
    /// Welcome's home URL to the connection's **verified** `origin_nest_id`
    /// rather than the per-request, peer-declared `origin_nest_url` — a hostile
    /// peer cannot then point the recipient's channel drain at a URL of its
    /// choosing. Empty ⇒ no
    /// proven address yet (first contact), where the caller falls back to the
    /// declared URL so an honest first cross-nest invite still routes.
    pub async fn proven_foreign_nest_urls(&self, nest_id: &[u8; 32]) -> Result<Vec<String>> {
        let nest_id = *nest_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT nest_url
               FROM nest_addresses
              WHERE nest_id = ?1 AND nest_url <> '' AND proven_at IS NOT NULL
              ORDER BY first_seen ASC, rowid ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![nest_id.as_slice()], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("proven_foreign_nest_urls")?;
        Ok(rows)
    }

    /// List a channel's **foreign** members (actors on peer nests, recorded at
    /// Welcome-relay time) — the owner-side "Shared with" roster union
    /// (`members.list_actors` marks them `remote: true`; Phase 2,
    /// `ui/folders.md` § Sharing → Cross-nest members).
    pub async fn list_foreign_channel_members(
        &self,
        channel_id: &[u8; 32],
    ) -> Result<Vec<[u8; 32]>> {
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id FROM channel_foreign_members WHERE channel_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![channel_id.as_slice()], |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(blob)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("list_foreign_channel_members")?;
        Ok(rows
            .into_iter()
            .filter_map(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .collect())
    }

    /// Whether one actor is a **foreign** member of the channel — the targeted
    /// twin of [`Self::list_foreign_channel_members`]. Read by the
    /// `p2p-share.member.admit` gate's newness check: a cross-nest recipient a
    /// Welcome was already relayed to is *established*, so an idempotent
    /// re-delivery must not re-spend the unrefundable counterparty quota
    /// (`dynamic-features.md` § The quota grammar — newness resolves against
    /// the feature's own records).
    pub async fn foreign_channel_member_exists(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<bool> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM channel_foreign_members
                     WHERE channel_id = ?1 AND actor_id = ?2)",
                rusqlite::params![channel_id.as_slice(), actor_id.as_slice()],
                |row| row.get(0),
            )
            .context("foreign_channel_member_exists")?;
        Ok(exists)
    }

    /// The channel's WHOLE roster — `actor_channels` (this nest's own users)
    /// unioned with `channel_foreign_members` (members this nest relayed a
    /// Welcome to) — the read behind `fauna.conversations.channel.actors` and
    /// its federation relay twin `fauna.federation.channel.actors`. Only the
    /// channel's HOME nest holds both halves; any other nest's union is still
    /// partial, which is why a foreign-homed caller rides the relay
    /// (`mls-group-key-material.md` § M2, chat bullet). Dedup is defensive
    /// only: the Welcome relay's early exit means no actor lands in both.
    pub async fn list_channel_actors_union(&self, channel_id: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let mut actors = self.list_channel_actors(channel_id).await?;
        let foreign = self.list_foreign_channel_members(channel_id).await?;
        let seen: std::collections::BTreeSet<[u8; 32]> = actors.iter().copied().collect();
        actors.extend(foreign.into_iter().filter(|f| !seen.contains(f)));
        Ok(actors)
    }

    /// Delete a foreign channel member's row — the self-scoped
    /// `fauna.federation.channel.leave` (a member's own home nest relaying their
    /// leave) and the owner-side `members.evict` purge (S8: the fetch
    /// authorization must die with the membership) both land here. PK-scoped like
    /// `evict_actor_from_channel`; idempotent. Returns whether a row was deleted.
    /// Revokes future federated discovery only — never key material already held
    /// (parity with same-nest voluntary leave, `ui/folders.md` § Sharing).
    pub async fn remove_foreign_channel_member(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<bool> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM channel_foreign_members WHERE channel_id = ?1 AND actor_id = ?2",
                rusqlite::params![channel_id.as_slice(), actor_id.as_slice()],
            )
            .context("remove_foreign_channel_member")?;
        Ok(deleted > 0)
    }

    /// The recorded home nest of a foreign channel member **plus whether the
    /// binding is confirmed** (`confirmed_at` set — the bound nest has
    /// exercised the grant on an authenticated federation connection at least
    /// once), or `None` if the actor is not a recorded foreign member. The
    /// read half of `require_foreign_member`'s first-use pin.
    pub async fn foreign_member_binding(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<Option<([u8; 32], bool)>> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let row: Option<(Vec<u8>, bool)> = conn
            .query_row(
                "SELECT home_nest_id, confirmed_at IS NOT NULL FROM channel_foreign_members \
                 WHERE channel_id = ?1 AND actor_id = ?2",
                rusqlite::params![channel_id.as_slice(), actor_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("foreign_member_binding")?;
        Ok(row.and_then(|(home, confirmed)| {
            <[u8; 32]>::try_from(home.as_slice())
                .ok()
                .map(|h| (h, confirmed))
        }))
    }

    /// The `handle@domain` a foreign member's own home nest announced for it
    /// and this nest verified (`federation.md` § Cross-nest shared folders +
    /// channel append, the id→handle bullet), or `None` when the member is
    /// not a recorded foreign member or no verified announce has landed yet.
    /// Display-only: the roster read joins it, nothing decides on it.
    pub async fn foreign_member_handle(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<Option<(String, String)>> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT handle, handle_domain FROM channel_foreign_members \
                 WHERE channel_id = ?1 AND actor_id = ?2",
                rusqlite::params![channel_id.as_slice(), actor_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("foreign_member_handle")?;
        Ok(row.and_then(|(handle, domain)| Some((handle?, domain?))))
    }

    /// Record the verified `handle@domain` a foreign member's home nest
    /// announced — the write half of [`Self::foreign_member_handle`]. The
    /// caller has already bound the domain to the announcing nest's key
    /// (`federation_handlers::record_announced_handle`); this only stores.
    /// Scoped to an existing binding row: an announce for an actor that is
    /// not a recorded foreign member writes nothing (a handle is never the
    /// thing that seats someone). Returns whether a row was updated.
    pub async fn record_foreign_member_handle(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
        handle: &str,
        domain: &str,
    ) -> Result<bool> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let handle = handle.to_string();
        let domain = domain.to_string();
        let conn = self.conn.lock().await;
        let updated = conn
            .execute(
                "UPDATE channel_foreign_members SET handle = ?3, handle_domain = ?4 \
                 WHERE channel_id = ?1 AND actor_id = ?2",
                rusqlite::params![channel_id.as_slice(), actor_id.as_slice(), handle, domain],
            )
            .context("record_foreign_member_handle")?;
        Ok(updated > 0)
    }

    /// Stamp a foreign member's binding CONFIRMED — the first-use pin, fired by `require_foreign_member` on the bound home nest's first
    /// successfully-served federated call. Idempotent (`confirmed_at` is only
    /// ever set once per bound value), and guarded on `home_nest_id` so a
    /// confirm raced by a concurrent legitimate move can never vouch for a
    /// value its caller did not just verify: the moved row keeps NULL and pins
    /// itself on the NEW nest's own first contact.
    pub async fn confirm_foreign_member(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
        home_nest_id: &[u8; 32],
    ) -> Result<()> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let home_nest_id = *home_nest_id;
        let conn = self.conn.lock().await;
        let now = now_epoch_secs();
        conn.execute(
            "UPDATE channel_foreign_members SET confirmed_at = ?4 \
             WHERE channel_id = ?1 AND actor_id = ?2 AND home_nest_id = ?3 \
               AND confirmed_at IS NULL",
            rusqlite::params![
                channel_id.as_slice(),
                actor_id.as_slice(),
                home_nest_id.as_slice(),
                now
            ],
        )
        .context("confirm_foreign_member")?;
        Ok(())
    }

    /// The recorded home `nest_id` of a foreign channel member, or `None` if the
    /// actor is not a recorded foreign member of the channel. The authorization gate
    /// for `fauna.federation.channel.fetch`: the verified originating `nest_id` must
    /// equal this value.
    pub async fn foreign_member_home_nest(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<Option<[u8; 32]>> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let row: Option<Vec<u8>> = conn
            .query_row(
                "SELECT home_nest_id FROM channel_foreign_members \
                 WHERE channel_id = ?1 AND actor_id = ?2",
                rusqlite::params![channel_id.as_slice(), actor_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("foreign_member_home_nest")?;
        Ok(row.and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()))
    }

    /// The recorded `nest_url` of a foreign channel member's home nest, or
    /// `None` when the actor is not a recorded foreign member of the channel or
    /// its row records no URL. Where the folder's home nest asks a member's
    /// seat back (`federation_handlers::folder_serve_announce_handler`).
    pub async fn foreign_member_nest_url(
        &self,
        channel_id: &[u8; 32],
        actor_id: &[u8; 32],
    ) -> Result<Option<String>> {
        let channel_id = *channel_id;
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let row: Option<Option<String>> = conn
            .query_row(
                "SELECT nest_url FROM channel_foreign_members \
                 WHERE channel_id = ?1 AND actor_id = ?2",
                rusqlite::params![channel_id.as_slice(), actor_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("foreign_member_nest_url")?;
        Ok(row.flatten().filter(|u| !u.is_empty()))
    }

    /// List all channel IDs that an actor belongs to.
    pub async fn list_actor_channels(&self, actor_id: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT channel_id FROM actor_channels WHERE actor_id = ?1")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], |row| {
                blob_col_to_array(row.get(0)?, 0, "channel_id")
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("list_actor_channels")?;
        Ok(rows)
    }

    // ==================== MLS commit high-water mark ====================

    /// Read a channel's MLS **commit high-water mark** — the highest conv `seq`
    /// at which a `ChannelEnvelope::Commit` record landed — or `0` if the
    /// channel has never carried a commit. Backs the device-owned-epoch commit
    /// gate (`fauna.conversations.channel.send`'s `expect_no_commit_since`
    /// precondition; `docs/goal/behavior/devices.md` § Cross-device MLS
    /// group-state sync). A commit landed after `since` iff the mark exceeds
    /// `since` (commits are monotonic in `seq`), so the gate is a single indexed
    /// compare rather than a scan of the live record set — and it stays correct
    /// even after the commit record is tombstoned/compacted away (the mark is
    /// set at append time, never derived from the live rows).
    pub async fn channel_commit_watermark(&self, channel_id: &[u8; 32]) -> Result<i64> {
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let seq: Option<i64> = conn
            .query_row(
                "SELECT last_commit_seq FROM channel_commit_watermark WHERE channel_id = ?1",
                rusqlite::params![channel_id.as_slice()],
                |r| r.get(0),
            )
            .optional()
            .context("read channel_commit_watermark")?;
        Ok(seq.unwrap_or(0))
    }

    /// [`Self::channel_commit_watermark`] plus the actor this nest observed
    /// sending the commit that set the mark — `None` when the channel has
    /// carried no commit, or when the mark was set by a writer with no acting
    /// actor (compaction, restore, the gateway) and so names no sender.
    ///
    /// The **authorship** half of the floor roster's commit-order guard
    /// (`conversation-rooms.md` § The floor roster). The guard's bound asks
    /// which commit a report names; this asks whose it was, which is what keeps
    /// the newest position — the one a committing device's own honest report is
    /// about to claim — from being claimable by a bystander.
    pub async fn channel_commit_watermark_with_sender(
        &self,
        channel_id: &[u8; 32],
    ) -> Result<(i64, Option<[u8; 32]>)> {
        let channel_id = *channel_id;
        let conn = self.conn.lock().await;
        let row: Option<(i64, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT last_commit_seq, last_commit_sender
                   FROM channel_commit_watermark WHERE channel_id = ?1",
                rusqlite::params![channel_id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("read channel_commit_watermark")?;
        let Some((seq, sender)) = row else {
            return Ok((0, None));
        };
        // A stored sender of the wrong width is treated as absent rather than
        // as an error: the mark itself is still sound, and the guard's
        // fail-open on an unknown sender is the declared behaviour.
        let sender = sender.and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
        Ok((seq, sender))
    }

    /// Advance a channel's MLS commit high-water mark to `seq` (a
    /// `ChannelEnvelope::Commit` just landed there), recording `sender` as the
    /// actor this nest observed sending it. Monotonic upsert: the stored value
    /// only ever moves forward (`MAX(existing, seq)`), so an out-of-order or
    /// replayed call can never lower it. Called from `segments::conv::append`
    /// **under the per-channel conv seq lock**, atomic with the record write, so
    /// the gate-read and this update can't interleave with another commit on the
    /// same channel.
    ///
    /// The sender is rewritten **only when the mark actually advances**, so the
    /// pair never splits: a replayed or out-of-order call that does not move the
    /// seq must not move the authorship either, or a late commit at a lower seq
    /// would stamp its own sender onto a newer commit's position. `None` — a
    /// writer with no acting actor to name (compaction, restore, the scheduling
    /// gateway) — clears the sender with the advance rather than leaving the
    /// previous commit's actor stranded on a position that is not theirs.
    pub async fn set_channel_commit_watermark(
        &self,
        channel_id: &[u8; 32],
        seq: i64,
        sender: Option<&[u8; 32]>,
    ) -> Result<()> {
        let channel_id = *channel_id;
        let sender = sender.map(|s| s.to_vec());
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO channel_commit_watermark
                 (channel_id, last_commit_seq, last_commit_sender)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(channel_id) DO UPDATE SET
                 last_commit_sender = CASE
                     WHEN excluded.last_commit_seq > last_commit_seq
                     THEN excluded.last_commit_sender
                     ELSE last_commit_sender
                 END,
                 last_commit_seq = MAX(last_commit_seq, excluded.last_commit_seq)",
            rusqlite::params![channel_id.as_slice(), seq, sender],
        )
        .context("upsert channel_commit_watermark")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;
    use crate::db::channels::{ChannelClaimOutcome, RebindPower};

    const FAR_FUTURE: u64 = 4_000_000_000; // ~2096, well past any test clock.

    /// The first-use pin on a foreign member's home-nest binding:
    /// while UNCONFIRMED, a caller with standing may move the grant (the
    /// healing window `federation.md`'s TOFU bullet keeps open); once the
    /// bound nest has exercised it (`confirm_foreign_member`), `Standing` is
    /// refused and only the claimant moves it — and that move RESETS the pin,
    /// so the incoming nest must earn its own confirmation. A same-value
    /// re-invite never clears the stamp, and a confirm raced by a move (its
    /// `home_nest_id` guard) is a no-op rather than a false vouch.
    #[tokio::test]
    async fn confirmed_binding_pins_home_nest_against_standing_rebind() {
        let db = CacheDb::open_in_memory().unwrap();
        let ch = [0x71u8; 32];
        let member = [0x72u8; 32];
        let n1 = [0xA1u8; 32];
        let n2 = [0xA2u8; 32];
        let n3 = [0xA3u8; 32];

        // First grant: the permissive insert arm needs no standing at all.
        db.register_foreign_channel_member(&ch, &member, &n1, None, RebindPower::InsertOnly)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((n1, false)),
            "a fresh grant starts unconfirmed"
        );

        // Unconfirmed: standing may move it (the healing window).
        db.register_foreign_channel_member(&ch, &member, &n2, None, RebindPower::Standing)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((n2, false)),
            "standing must move an UNCONFIRMED grant — the healing window"
        );

        // The bound nest exercises the grant: pinned.
        db.confirm_foreign_member(&ch, &member, &n2).await.unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((n2, true)),
            "first authenticated use must stamp the pin"
        );

        // Pinned: standing no longer reaches it — the ex-member's immortal
        // roster row stops here.
        db.register_foreign_channel_member(&ch, &member, &n3, None, RebindPower::Standing)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((n2, true)),
            "a CONFIRMED grant moved on a standing-based rebind — the \
             first-use pin is not holding"
        );

        // A same-value re-invite keeps the stamp (idempotent re-welcome).
        db.register_foreign_channel_member(&ch, &member, &n2, None, RebindPower::Standing)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((n2, true)),
            "an idempotent re-invite to the SAME nest must not clear the pin"
        );

        // The claimant still moves it — and the move resets the pin.
        db.register_foreign_channel_member(&ch, &member, &n3, None, RebindPower::Claimant)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((n3, false)),
            "the claimant's move must land AND reset the pin for the new nest"
        );

        // A confirm that raced a move guards on the CURRENT home value: the
        // stale vouch is a no-op, never a false confirmation of n3.
        db.confirm_foreign_member(&ch, &member, &n2).await.unwrap();
        assert_eq!(
            db.foreign_member_binding(&ch, &member).await.unwrap(),
            Some((n3, false)),
            "a stale confirm (raced by a legitimate move) must not stamp the \
             moved binding"
        );
    }

    /// First-binder-wins: a fresh channel is claimed by the first binder (who is
    /// also registered on the roster), an idempotent re-claim by the same owner is
    /// Allowed, and any other actor is Denied — closing two gaps (a removed
    /// member's `share`-rebind) and (a cross-owner envelope clobber).
    #[tokio::test]
    async fn claim_folder_channel_first_binder_wins() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner: [u8; 32] = [0xa1u8; 32];
        let other: [u8; 32] = [0xbbu8; 32];
        let channel: [u8; 32] = [0xc1u8; 32];

        // First binder claims a fresh channel AND lands on the roster.
        assert_eq!(
            db.claim_folder_channel(&owner, &channel).await.unwrap(),
            ChannelClaimOutcome::Allowed
        );
        assert!(
            db.is_actor_in_channel(&owner, &channel).await.unwrap(),
            "the first binder is registered on the roster"
        );
        assert_eq!(
            db.folder_channel_claimed_by(&channel).await.unwrap(),
            Some(owner)
        );

        // Idempotent re-claim by the same owner.
        assert_eq!(
            db.claim_folder_channel(&owner, &channel).await.unwrap(),
            ChannelClaimOutcome::Allowed
        );

        // A different actor is denied — and is NOT added to the roster.
        assert_eq!(
            db.claim_folder_channel(&other, &channel).await.unwrap(),
            ChannelClaimOutcome::Denied
        );
        assert!(
            !db.is_actor_in_channel(&other, &channel).await.unwrap(),
            "a denied claim must not register the actor on the roster"
        );
        assert_eq!(
            db.folder_channel_claimed_by(&channel).await.unwrap(),
            Some(owner),
            "the claim stays with the first binder"
        );
    }

    /// A pre-populated channel (a conversation whose roster `welcome.deliver`
    /// already wrote) is claimable by **nobody** — not an outsider, and **not a
    /// member either**.
    ///
    /// The member half is the one that matters, and this test formerly asserted
    /// the opposite. A claim is permanent (INSERT + SELECT are this table's only
    /// operations tree-wide), and holding one freezes the group: the claim-read
    /// gate suppresses the owner's roster writes and no client anywhere can
    /// release the claim (the channel-send gate's own half of that freeze —
    /// refusing every non-claimant `Commit`, so the owner could no longer add,
    /// remove or rotate — is since discharged, `federation.md` § Cross-nest
    /// shared folders + channel append, 2026-08-24: Commit admission is now
    /// roster-membership). A member could do that to their own group and walk
    /// away — the client-causable unrecoverable nest state `nest/common.md`
    /// § Client-state recoverability forbids. Nothing shipped needs the branch:
    /// a first share mints a fresh group (empty roster), and a re-share is
    /// answered by the claimant fast-path before the roster is ever consulted.
    #[tokio::test]
    async fn claim_folder_channel_refuses_a_populated_conversation_channel() {
        let db = CacheDb::open_in_memory().unwrap();
        let conv_member: [u8; 32] = [0xa1u8; 32];
        let outsider: [u8; 32] = [0xeeu8; 32];
        let channel: [u8; 32] = [0xc2u8; 32];

        // A conversation already populated this channel's roster.
        db.register_actor_channel(&conv_member, &channel)
            .await
            .unwrap();

        // An outsider (not on the roster) cannot claim it — and stays off-roster.
        assert_eq!(
            db.claim_folder_channel(&outsider, &channel).await.unwrap(),
            ChannelClaimOutcome::Denied
        );
        assert!(
            !db.is_actor_in_channel(&outsider, &channel).await.unwrap(),
            "a denied claim must not inject the outsider onto the roster"
        );

        // …and neither can a member, which is the whole point: an accepted claim
        // here is unreleasable and would freeze their own group's commits.
        assert_eq!(
            db.claim_folder_channel(&conv_member, &channel)
                .await
                .unwrap(),
            ChannelClaimOutcome::Denied,
            "a roster member claiming their own conversation's channel takes a \
             permanent, unreleasable lock on it — refuse rather than document it"
        );
        assert_eq!(
            db.folder_channel_claimed_by(&channel).await.unwrap(),
            None,
            "the channel stays unclaimed after both denied attempts"
        );

        // The shipped path is untouched: a fresh (roster-empty) channel still
        // claims, so refusing above closes the hole without costing a real flow.
        let fresh_owner: [u8; 32] = [0xb7u8; 32];
        let fresh_channel: [u8; 32] = [0xb8u8; 32];
        assert_eq!(
            db.claim_folder_channel(&fresh_owner, &fresh_channel)
                .await
                .unwrap(),
            ChannelClaimOutcome::Allowed
        );
        assert_eq!(
            db.claim_folder_channel(&fresh_owner, &fresh_channel)
                .await
                .unwrap(),
            ChannelClaimOutcome::Allowed,
            "and the 2nd..Nth share's idempotent claimant re-bind still passes — \
             it is answered before the roster check, which is why refusing the \
             populated branch cannot break multi-member sharing"
        );
    }

    /// The `actor_channels`/`channel_foreign_members` union: a channel carrying **only** a
    /// foreign-member grant — no `actor_channels` row at all — must refuse a
    /// claim exactly like one carrying only same-nest rows, not silently hand
    /// the claimant every grant planted while the channel looked unclaimed.
    #[tokio::test]
    async fn claim_folder_channel_refuses_a_channel_with_only_foreign_members() {
        let db = CacheDb::open_in_memory().unwrap();
        let foreign_member: [u8; 32] = [0xa2u8; 32];
        let home_nest: [u8; 32] = [0xf1u8; 32];
        let claimant: [u8; 32] = [0xb3u8; 32];
        let channel: [u8; 32] = [0xc3u8; 32];

        // A `channel_foreign_members` row, no `actor_channels` row — the exact
        // shape `list_channel_actors_union`/`folder_handlers.rs:1708` treat as
        // real membership, not bookkeeping.
        db.register_foreign_channel_member(
            &channel,
            &foreign_member,
            &home_nest,
            None,
            RebindPower::InsertOnly,
        )
        .await
        .unwrap();
        assert!(
            !db.is_actor_in_channel(&foreign_member, &channel)
                .await
                .unwrap(),
            "a foreign member's membership lives in channel_foreign_members, \
             never actor_channels"
        );

        assert_eq!(
            db.claim_folder_channel(&claimant, &channel).await.unwrap(),
            ChannelClaimOutcome::Denied,
            "a channel with only foreign grants must refuse a claim, not \
             silently hand the claimant every grant planted while it looked \
             unclaimed"
        );
        assert_eq!(
            db.folder_channel_claimed_by(&channel).await.unwrap(),
            None,
            "the channel stays unclaimed after the denied attempt"
        );
    }

    #[tokio::test]
    async fn list_channels_for_actor_returns_registered_channels() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [0xAAu8; 32];
        let ch1: [u8; 32] = [0x11u8; 32];
        let ch2: [u8; 32] = [0x22u8; 32];

        db.register_actor_channel(&actor, &ch1).await.unwrap();
        db.register_actor_channel(&actor, &ch2).await.unwrap();

        let channels = db.list_actor_channels(&actor).await.unwrap();
        assert_eq!(channels.len(), 2);
        assert!(channels.contains(&ch1));
        assert!(channels.contains(&ch2));

        let other: [u8; 32] = [0xBBu8; 32];
        let empty = db.list_actor_channels(&other).await.unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn take_key_package_consumes_one_time_then_falls_back_to_last_resort() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [0x42u8; 32];

        // One one-time KP + one last-resort KP.
        db.put_key_package("ot-1", &actor, b"one-time-pkg", 100, FAR_FUTURE)
            .await
            .unwrap();
        db.put_last_resort_key_package("lr-1", &actor, b"last-resort-pkg", 50, FAR_FUTURE)
            .await
            .unwrap();

        // First take consumes the one-time KP.
        let first = db.take_key_package(&actor).await.unwrap();
        assert_eq!(first.as_deref(), Some(&b"one-time-pkg"[..]));

        // Pool now empty → falls back to the last-resort KP, repeatedly,
        // WITHOUT consuming it.
        for _ in 0..3 {
            let again = db.take_key_package(&actor).await.unwrap();
            assert_eq!(
                again.as_deref(),
                Some(&b"last-resort-pkg"[..]),
                "last-resort KP must be reusable, never consumed"
            );
        }
    }

    #[tokio::test]
    async fn take_key_package_none_when_actor_has_neither() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [0x01u8; 32];
        assert!(db.take_key_package(&actor).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn count_key_packages_excludes_last_resort() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [0x07u8; 32];
        db.put_key_package("ot-1", &actor, b"a", 100, FAR_FUTURE)
            .await
            .unwrap();
        db.put_key_package("ot-2", &actor, b"b", 101, FAR_FUTURE)
            .await
            .unwrap();
        db.put_last_resort_key_package("lr-1", &actor, b"lr", 50, FAR_FUTURE)
            .await
            .unwrap();
        // Only the two one-time KPs count toward the top-up gauge.
        assert_eq!(db.count_key_packages(&actor).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn has_usable_key_package_true_with_only_last_resort() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [0x09u8; 32];

        assert!(!db.has_usable_key_package(&actor).await.unwrap());

        db.put_last_resort_key_package("lr-1", &actor, b"lr", 50, FAR_FUTURE)
            .await
            .unwrap();
        assert!(
            db.has_usable_key_package(&actor).await.unwrap(),
            "addressable must be true when only a last-resort KP exists"
        );
    }

    /// Re-publishing a last-resort KP (every login, fresh random `id`) keeps a
    /// SINGLE last-resort row per actor — Spec Y2 speaks of *the* reusable
    /// last-resort row, and the client `ensure_last_resort_keypackage` uploads
    /// unconditionally each login. Without the replace this would accumulate.
    #[tokio::test]
    async fn put_last_resort_key_package_keeps_one_row_per_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [0x11u8; 32];

        db.put_last_resort_key_package("lr-1", &actor, b"first", 50, FAR_FUTURE)
            .await
            .unwrap();
        db.put_last_resort_key_package("lr-2", &actor, b"second", 60, FAR_FUTURE)
            .await
            .unwrap();
        db.put_last_resort_key_package("lr-3", &actor, b"third", 70, FAR_FUTURE)
            .await
            .unwrap();

        // Exactly one last-resort row survives, and it is the newest one.
        let rows = db.list_key_packages_for_actor(&actor).await.unwrap();
        assert_eq!(rows.len(), 1, "only one last-resort row per actor");
        assert_eq!(rows[0].1, b"third", "the newest publication wins");

        // It is still served by take_key_package (reusable, never consumed).
        for _ in 0..2 {
            assert_eq!(
                db.take_key_package(&actor).await.unwrap().as_deref(),
                Some(&b"third"[..])
            );
        }

        // One-time KPs published alongside are unaffected by a last-resort replace.
        db.put_key_package("ot-1", &actor, b"one-time", 80, FAR_FUTURE)
            .await
            .unwrap();
        db.put_last_resort_key_package("lr-4", &actor, b"fourth", 90, FAR_FUTURE)
            .await
            .unwrap();
        assert_eq!(
            db.count_key_packages(&actor).await.unwrap(),
            1,
            "the one-time pool survives a last-resort replace"
        );
    }

    /// The commit high-water mark reads 0 for a virgin channel, advances on a
    /// commit, and — being monotonic — never regresses when a lower seq is
    /// recorded (a replay or out-of-order call must not re-open a closed gate).
    #[tokio::test]
    async fn channel_commit_watermark_is_monotonic() {
        let db = CacheDb::open_in_memory().unwrap();
        let channel: [u8; 32] = [0xd1u8; 32];

        // Virgin channel → 0 (no commit has ever landed).
        assert_eq!(db.channel_commit_watermark(&channel).await.unwrap(), 0);

        // A commit at seq 5 advances the mark.
        db.set_channel_commit_watermark(&channel, 5, None)
            .await
            .unwrap();
        assert_eq!(db.channel_commit_watermark(&channel).await.unwrap(), 5);

        // A later commit at seq 9 advances it further.
        db.set_channel_commit_watermark(&channel, 9, None)
            .await
            .unwrap();
        assert_eq!(db.channel_commit_watermark(&channel).await.unwrap(), 9);

        // A stale/replayed lower seq MUST NOT lower the mark.
        db.set_channel_commit_watermark(&channel, 4, None)
            .await
            .unwrap();
        assert_eq!(
            db.channel_commit_watermark(&channel).await.unwrap(),
            9,
            "the mark is monotonic — a lower seq can never re-open the gate"
        );

        // Marks are per-channel: a different channel is independent.
        let other: [u8; 32] = [0xd2u8; 32];
        assert_eq!(db.channel_commit_watermark(&other).await.unwrap(), 0);
    }

    /// The mark's **authorship** travels with the mark and never splits from
    /// it: the sender is rewritten exactly when the seq advances, so the pair
    /// always names one commit. A late or replayed commit at a lower seq moves
    /// neither — otherwise it would stamp its own sender onto a newer commit's
    /// position, which is precisely the claim the floor roster's guard reads
    /// (`conversation-rooms.md` § The floor roster).
    #[tokio::test]
    async fn the_commit_marks_sender_is_paired_with_its_seq() {
        let db = CacheDb::open_in_memory().unwrap();
        let channel: [u8; 32] = [0xd3u8; 32];
        let alice: [u8; 32] = [0xa1u8; 32];
        let bob: [u8; 32] = [0xb0u8; 32];

        // A virgin channel names no commit and nobody.
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (0, None)
        );

        // Alice's commit at seq 5 sets both halves.
        db.set_channel_commit_watermark(&channel, 5, Some(&alice))
            .await
            .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (5, Some(alice))
        );

        // Bob's later commit at seq 9 advances both.
        db.set_channel_commit_watermark(&channel, 9, Some(&bob))
            .await
            .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (9, Some(bob))
        );

        // A stale commit at a LOWER seq moves neither half. Were the sender to
        // follow it, Alice would be recorded as the sender of Bob's commit at
        // 9 — and a report at 9 from Alice would then be the one admitted.
        db.set_channel_commit_watermark(&channel, 4, Some(&alice))
            .await
            .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (9, Some(bob)),
            "a lower seq moves neither the mark nor its authorship"
        );

        // An equal-seq replay is not an advance either, so it cannot rewrite
        // the authorship of a position already recorded.
        db.set_channel_commit_watermark(&channel, 9, Some(&alice))
            .await
            .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (9, Some(bob)),
            "a replay at the same seq cannot re-author that position"
        );

        // A writer with no acting actor (compaction, restore, the scheduling
        // gateway) advances the mark and clears the authorship rather than
        // stranding the previous sender on a position that is not theirs.
        db.set_channel_commit_watermark(&channel, 12, None)
            .await
            .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&channel)
                .await
                .unwrap(),
            (12, None),
            "an unattributed advance leaves the position unattributed"
        );
    }
}
