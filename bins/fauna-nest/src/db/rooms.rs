//! The **room plane** — the floor roster and the room record.
//!
//! Authority: `docs/goal/behavior/conversation-rooms.md` § The floor roster.
//!
//! Every room has a nest-side roster on its home nest: one row per principal
//! with its kind, role, inviter, home nest and join stamp. Its *authority*
//! differs by class — for a community room the floor roster is authoritative
//! and the nest refuses a write whose signer's role does not permit it; for
//! an end-to-end room it is a **member-reported mirror**, because the MLS
//! group is the membership authority and the nest cannot see inside it.
//!
//! What the roster is used for is the same either way: routing fan-out, the
//! custody serve door, the cross-nest relay gate, succession targets. What it
//! never decides is confidentiality — in an end-to-end room a wrong report
//! cannot make a non-member read a message, because reading is MLS's
//! (§ Architectural rules, rule 3).
//!
//! **It is not `actor_channels`.** That is the channel's *routing* roster,
//! self-registered by any local actor's first `channel.send`, so a row there
//! proves knowledge of a channel id, not membership.

use super::{CacheDb, blob_col_to_array, now_epoch_millis};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

/// The moderation gate on a room post's **derived views** — the search map
/// ([`CacheDb::room_post_ids_for_docs`]) and the verdict map
/// ([`CacheDb::rooms_indexing_posts`]).
///
/// Both serve something derived from the post's body (a hit is an existence
/// oracle over its text; a verdict was scored from it) to the room's floor —
/// a **non-author** audience — so the gate is
/// [`MODERATION_SERVABLE`](super::public_servability::MODERATION_SERVABLE),
/// the fragment every non-author surface shares, never a private copy of the
/// flags. A `LEFT JOIN content_meta cm` is the caller's contract (an unmatched
/// row carries no flag; a takedown cannot land on one). Owner of the ruling:
/// `moderation.md` § Legal takedown → *The blob-serve door* → *What the
/// withhold binds on owner- and admin-scoped routes*, path 4.
const ROOM_POST_VIEW_MODERATION: &str = super::public_servability::MODERATION_SERVABLE;

/// A room record as stored — the class derived from its member set, never a
/// stored choice a caller made (`conversation-rooms.md` § The three classes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRecord {
    pub room_id: [u8; 32],
    /// `end_to_end` / `community` / `transport_only`.
    pub class: String,
    /// The room's home nest; empty means this nest.
    pub home_node_url: String,
    /// The single owner, when the roster names one. `None` on a policy-less room,
    /// whose roster carries no roles at all.
    pub owner_id: Option<Vec<u8>>,
    pub policy_version: Option<u64>,
    /// The room log's position of the commit whose report the floor holds —
    /// what orders a member-reported mirror's reports
    /// ([`CacheDb::replace_floor_roster`]). `None` until the first report that
    /// names one, and on every floor-authoritative room.
    pub roster_commit_seq: Option<i64>,
    /// The birth record's salt — set only by the create ceremony
    /// (`fauna.conversations.room.create`). See
    /// [`RoomRecord::is_floor_authoritative`].
    pub birth_salt: Option<Vec<u8>>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl RoomRecord {
    /// Whether this room's **membership authority is its floor** — i.e. the
    /// nest's own ceremonies write its roster and the member-reported mirror
    /// door must not.
    ///
    /// The answer is the room's *provenance*, and provenance is a historical
    /// fact rather than a choice anyone made or a class anyone stored: a
    /// room founded by the create ceremony carries a birth record, and one
    /// that first appeared through a report does not. That distinction is
    /// what the report door's declared bootstrap bound was waiting for
    /// (`conversation-rooms.md` § Implementation status today) — before it,
    /// the door could not tell a room it should never write from one it
    /// exists to write, so a legitimate member of a community room could
    /// replace that room's authoritative roster wholesale and name itself
    /// owner.
    ///
    /// ⚠ Deliberately **not** derived from `class`. Class is a function of
    /// the member set (§ Architectural rules, rule 1) and says who can read;
    /// this says who may write the roster. They agree in practice today —
    /// every ceremony-born room seats its home nest and so derives
    /// `community` — but conflating them would make the write authority
    /// swing with a membership change, which is exactly the property an
    /// attacker would aim at.
    pub fn is_floor_authoritative(&self) -> bool {
        self.birth_salt.is_some()
    }
}

/// One principal on a room's floor roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomMemberRow {
    pub principal_id: [u8; 32],
    /// `user` / `nest` / `bridge`.
    pub principal_kind: String,
    /// `owner` / `admin` / `member`; `None` on a policy-less room.
    pub role: Option<String>,
    pub home_node_url: String,
    pub joined_at: i64,
    pub reported_at: i64,
    /// Set when a later roster dropped this principal — the row is absorbed
    /// as history rather than deleted (§ The home nest → *The succession
    /// axis*). A live member has `None`.
    pub removed_at: Option<i64>,
    /// This seating's 32-byte roster-entry id — the slot every wrap of a room
    /// generation key is sealed to
    /// (`fauna_mls::room_policy::derive_room_entry_id`). `None` on a member
    /// seated before the sealing plane existed, and on every member of a
    /// mirror room, which has no generations.
    pub entry_id: Option<Vec<u8>>,
    /// The member's group-reception X-Wing public half — its wrap target.
    /// `None` means "no wrap target yet", which is the state the scheme's
    /// member top-up heals rather than an error.
    pub reception_pubkey: Option<Vec<u8>>,
    /// The principal's handle as **this** nest knows it, `LEFT JOIN`ed from
    /// its own `users` row: `Some("alice")` for a local user, `Some("")` for a
    /// local user with no handle set, `None` for a principal that has no
    /// `users` row here at all — a member homed on another nest, or a nest or
    /// bridge principal. Exactly `ContactRow.handle`'s three-way answer.
    ///
    /// Joined here rather than stored on the membership row, so a rename is
    /// reflected by the next read and a roster *report* can never assert one
    /// (`conversation-rooms.md` § The floor roster — an end-to-end room's
    /// roster is a member-reported mirror).
    pub handle: Option<String>,
    /// For a principal homed on **another** nest: the handle its own home
    /// nest announced on the member's relayed drain and this nest verified
    /// (`federation.md` § Cross-nest shared folders + channel append, the
    /// id→handle bullet), `LEFT JOIN`ed from the member's
    /// `channel_foreign_members` binding — a room id *is* its channel id.
    /// `None` until the member's home nest has announced one. Never set for
    /// a local principal (that is [`Self::handle`]'s job) and, like it, joined
    /// at read time so a rename lands on the next read.
    pub foreign_handle: Option<String>,
    /// The announced handle's domain, `None` whenever
    /// [`Self::foreign_handle`] is. Verified by this nest to resolve to the
    /// announcing nest's key before it was stored.
    pub foreign_domain: Option<String>,
}

/// One room generation as stored — the recipient-set scheme's keying row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomGenerationRow {
    pub generation_id: [u8; 32],
    pub parent_id: Option<Vec<u8>>,
    /// What a member checks the unwrapped key against.
    pub key_commitment: Vec<u8>,
    /// The owner or admin whose act this mint is.
    pub minted_by: Vec<u8>,
    /// The signed `GroupGenerationMintRecord`, kept whole.
    pub mint_blob: Vec<u8>,
    pub minted_at_ms: i64,
}

/// What [`CacheDb::insert_room_generation`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomGenerationInsert {
    /// Stored, with its wraps; it is now the room's tip.
    Stored,
    /// Nothing stored: by the time the insert held the lock, the mint's parent
    /// was no longer the room's tip — another rotation landed first. The same
    /// refusal the handler gives a stale mint, reached through the race.
    NotTheTip,
}

/// What [`CacheDb::set_room_member_reception_key`] did to the caller's seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceptionKeyBound {
    /// The seat's roster entry — kept across a rotation, minted for a seat
    /// that had none.
    pub entry_id: [u8; 32],
    /// True when a *different* key was already bound and this write replaced
    /// it.
    pub rotated: bool,
}

/// A pending invitation, as the accept door reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRoomInvite {
    pub inviter_id: [u8; 32],
    /// `admin` or `member` — never `owner`.
    pub role: String,
    pub invitee_node_url: String,
    /// The **verified** nest identity the invitation was delivered to, when
    /// the invitee is homed elsewhere — resolved by the room's home from the
    /// federation dial that carried the delivery, never from anything the
    /// inviter declared. `None` for an invitee homed here. It is the gate the
    /// relayed accept runs behind: only that nest may seat this invitee
    /// (`conversation-rooms.md` § Join rules and invites → *A cross-nest
    /// invitation*).
    pub invitee_nest_id: Option<[u8; 32]>,
}

/// What the room's home holds about an invitation's **home-nest binding**,
/// pending or accepted — the read behind the relayed accept's gate and its
/// converged re-send arm ([`CacheDb::room_invite_home_binding`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomInviteHomeBinding {
    /// As [`PendingRoomInvite::invitee_nest_id`].
    pub invitee_nest_id: Option<[u8; 32]>,
    /// The invitee's home nest URL as the inviter declared it — what the
    /// foreign-member binding records as the nest's dial address.
    pub invitee_node_url: String,
    /// Whether the invitation has been accepted (the row is then history the
    /// roster's `invited_by` points at, and the seat it produced is what a
    /// re-sent accept is answered with).
    pub accepted: bool,
}

/// One pending invitation of a room, as the list door reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomPendingInviteRow {
    pub invitee_id: [u8; 32],
    pub inviter_id: [u8; 32],
    /// `admin` or `member` — never `owner`.
    pub role: String,
    /// Epoch millis of the (latest) issue.
    pub invited_at: i64,
    /// This nest's own `users.handle` for each, when it has one.
    pub invitee_handle: Option<String>,
    pub inviter_handle: Option<String>,
}

/// What [`CacheDb::accept_room_invite`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomInviteAccept {
    /// The invitation is stamped and the invitee holds a live seat.
    Seated,
    /// No invitation is pending — never issued, already accepted, lapsed, or
    /// cleared by a removal.
    NotPending,
    /// The room's policy is no longer at the version the caller judged the
    /// invitation under; nothing was written. Judge again and retry.
    PolicyMoved,
}

impl RoomInviteAccept {
    pub fn seated(self) -> bool {
        self == Self::Seated
    }
}

/// What [`CacheDb::replace_floor_roster`] did with one report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterReplace {
    /// The roster was replaced; `live` principals stand on it now.
    Replaced { live: u32 },
    /// Nothing written: the floor already holds the roster of the commit at
    /// log position `stored`, at or after the one this report follows, and
    /// `live` principals stand on it.
    Superseded { stored: i64, live: u32 },
}

/// One reported principal, as the report door hands it down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedMember {
    pub principal_id: [u8; 32],
    pub principal_kind: String,
    pub role: Option<String>,
    pub home_node_url: String,
    /// The principal's group-reception public half, when the seating act
    /// carried one. Empty on every member-reported roster: an end-to-end
    /// room has no generations to wrap, so its mirror rows carry no wrap
    /// target and the sealing columns stay NULL.
    pub reception_pubkey: Vec<u8>,
}

impl CacheDb {
    // ==================== The floor roster ====================

    /// Replace a room's floor roster **wholesale** with `members`, minting
    /// the room record on first report.
    ///
    /// Wholesale is the contract, not an optimization: a membership commit
    /// produces a complete roster, and a delta protocol between an authority
    /// the nest cannot read and a mirror it can would have no way to
    /// resynchronize after one missed report.
    ///
    /// A principal the new roster omits is **`Removed`-absorbed** — its row
    /// stays with a removal stamp rather than being deleted — which is the
    /// shape the succession axis declares for a membership row, and what
    /// lets a re-add come back on the same row instead of colliding on the
    /// primary key. A re-added principal's `removed_at` is cleared and its
    /// `joined_at` preserved.
    ///
    /// The whole replacement runs in **one transaction**: a roster half-way
    /// between two membership commits is a membership record no commit ever
    /// produced, and the custody serve door reads this table per request.
    ///
    /// **Ordered by `commit_seq`** — the room log's position of the commit
    /// the report follows (`conversation-rooms.md` § The floor roster). A
    /// position at or below the one the floor already holds is
    /// [`RosterReplace::Superseded`] and writes nothing, so a report that
    /// arrives after a later commit's report can roll back neither membership
    /// nor roles. The compare runs inside the same transaction as the write:
    /// two reports racing in must not both read the old position and both
    /// write. `None` — a report that follows no commit (a leave, the birth
    /// report, the floor backfill) — replaces unordered and leaves the stored position where it was. That
    /// the position is one the room's log really carried is the caller's
    /// check (the report door bounds it by the channel's commit high-water
    /// mark); this layer only orders.
    pub async fn replace_floor_roster(
        &self,
        room_id: &[u8; 32],
        class: &str,
        home_node_url: &str,
        policy_version: Option<u64>,
        commit_seq: Option<i64>,
        members: &[ReportedMember],
    ) -> Result<RosterReplace> {
        let room_id = *room_id;
        let class = class.to_string();
        let home_node_url = home_node_url.to_string();
        let members = members.to_vec();
        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();
        let owner_id: Option<Vec<u8>> = members
            .iter()
            .find(|m| m.role.as_deref() == Some("owner"))
            .map(|m| m.principal_id.to_vec());

        let tx = conn.transaction().context("begin roster replace")?;

        if let Some(position) = commit_seq {
            let stored: Option<i64> = tx
                .query_row(
                    "SELECT roster_commit_seq FROM rooms WHERE room_id = ?1",
                    rusqlite::params![room_id.as_slice()],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()
                .context("read the floor's commit position")?
                .flatten();
            if let Some(stored) = stored
                && position <= stored
            {
                let live: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM room_members
                         WHERE room_id = ?1 AND removed_at IS NULL",
                        rusqlite::params![room_id.as_slice()],
                        |row| row.get(0),
                    )
                    .context("count the live floor")?;
                // Nothing was written; dropping the transaction rolls back
                // the read.
                return Ok(RosterReplace::Superseded {
                    stored,
                    live: live as u32,
                });
            }
        }

        tx.execute(
            "INSERT INTO rooms (room_id, class, home_node_url, owner_id, policy_version,
                                roster_commit_seq, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(room_id) DO UPDATE SET
                 class = excluded.class,
                 home_node_url = excluded.home_node_url,
                 owner_id = excluded.owner_id,
                 policy_version = excluded.policy_version,
                 roster_commit_seq = COALESCE(excluded.roster_commit_seq, roster_commit_seq),
                 updated_at = excluded.updated_at",
            rusqlite::params![
                room_id.as_slice(),
                class,
                home_node_url,
                owner_id,
                policy_version.map(|v| v as i64),
                commit_seq,
                now,
            ],
        )
        .context("upsert room record")?;

        for m in &members {
            // `joined_at` is preserved on an existing row (including one
            // coming back from a removal) — the stamp answers "since when
            // has this principal been part of this room", and a re-add is
            // not a new membership fact the reporter asserted.
            tx.execute(
                "INSERT INTO room_members (room_id, principal_id, principal_kind, role,
                                           home_node_url, joined_at, reported_at, removed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, NULL)
                 ON CONFLICT(room_id, principal_id) DO UPDATE SET
                     principal_kind = excluded.principal_kind,
                     role = excluded.role,
                     home_node_url = excluded.home_node_url,
                     reported_at = excluded.reported_at,
                     removed_at = NULL",
                rusqlite::params![
                    room_id.as_slice(),
                    m.principal_id.as_slice(),
                    m.principal_kind,
                    m.role,
                    m.home_node_url,
                    now,
                ],
            )
            .context("upsert floor roster row")?;
        }

        // Absorb every live row the new roster did not name. Stamped with
        // the same `now` as the upserts above, so one report is one instant.
        {
            let reported: Vec<Vec<u8>> = members.iter().map(|m| m.principal_id.to_vec()).collect();
            let mut stmt = tx
                .prepare(
                    "SELECT principal_id FROM room_members
                     WHERE room_id = ?1 AND removed_at IS NULL",
                )
                .context("prepare live roster scan")?;
            let live: Vec<Vec<u8>> = stmt
                .query_map(rusqlite::params![room_id.as_slice()], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .context("scan live roster")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect live roster")?;
            drop(stmt);
            for principal in live.into_iter().filter(|p| !reported.contains(p)) {
                tx.execute(
                    "UPDATE room_members SET removed_at = ?3, reported_at = ?3
                     WHERE room_id = ?1 AND principal_id = ?2",
                    rusqlite::params![room_id.as_slice(), principal, now],
                )
                .context("absorb a dropped principal")?;
            }
        }

        tx.commit().context("commit roster replace")?;
        Ok(RosterReplace::Replaced {
            live: members.len() as u32,
        })
    }

    /// Found a room by the **birth ceremony** — mint the room record with
    /// its birth salt and signed policy, and seat the founding roster.
    ///
    /// Idempotent on `room_id`, which is content-derived from the birth
    /// record: a replay of the byte-identical ceremony re-derives the same
    /// id and rewrites the same rows — and, once the room's policy has moved
    /// past its founding version, writes nothing at all rather than rolling
    /// it back (see the guard below). The whole thing is one transaction for
    /// the same reason [`CacheDb::replace_floor_roster`] is — a room record
    /// without its founding roster is a room nobody can act in, and the
    /// custody serve door reads these tables per request.
    ///
    /// Returns `false` when the id already names a room founded by a
    /// **different** owner. That cannot happen honestly (the id commits to
    /// the owner's key, so a second owner would be a BLAKE3 collision), but
    /// the check is what makes that a property of the storage rather than an
    /// inference from the hash: the caller turns it into a refusal instead
    /// of silently re-homing somebody else's room.
    #[allow(clippy::too_many_arguments)]
    pub async fn found_room(
        &self,
        room_id: &[u8; 32],
        class: &str,
        owner_id: &[u8; 32],
        policy_version: u64,
        policy_blob: &[u8],
        birth_salt: &[u8; 32],
        members: &[ReportedMember],
    ) -> Result<bool> {
        let room_id = *room_id;
        let class = class.to_string();
        let owner_id = *owner_id;
        let policy_blob = policy_blob.to_vec();
        let birth_salt = birth_salt.to_vec();
        let members = members.to_vec();
        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();

        let tx = conn.transaction().context("begin room founding")?;
        let existing: Option<(Option<Vec<u8>>, Option<i64>)> = tx
            .query_row(
                "SELECT owner_id, policy_version FROM rooms WHERE room_id = ?1",
                rusqlite::params![room_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("read existing room")?;
        if let Some((ref owner, _)) = existing
            && owner.as_deref() != Some(&owner_id[..])
        {
            return Ok(false);
        }
        // ⚠ A replay of the birth record after the room's policy has MOVED ON
        // is a no-op, never a rollback.
        //
        // The upsert below is unconditional on the fields it writes, so
        // without this the owner could re-send its own original, still-valid
        // birth record and roll `policy_version` and `policy_blob` back to
        // the founding version — undoing a tightened join rule or a removed
        // admin — and re-seat the founding rows by clearing their
        // `removed_at`. The version any later reader keys off is supposed to
        // be monotonic; this door is the one place it was not.
        //
        // A *former* owner cannot reach even that: the owner check above
        // refuses a replay once `transfer_ownership` has moved the row. What
        // is closed here is the current owner rolling back its own room's
        // governance — a ratchet break rather than an escalation, which is
        // why it is a bound rather than a bug today, when nothing can raise
        // the version yet. It is closed NOW so that the replay-safety claim
        // in `kind.rs` stays true when `set_policy` lands rather than
        // silently expiring with it.
        //
        // Returning `true` and writing nothing is the honest answer to a
        // replay: the room this record founds exists and is the caller's, so
        // the ceremony's post-condition holds. An error would be wrong — a
        // recovered connection re-sending a birth record has done nothing
        // improper.
        if let Some((_, Some(version))) = existing
            && version > 1
        {
            return Ok(true);
        }

        tx.execute(
            "INSERT INTO rooms (room_id, class, home_node_url, owner_id, policy_version,
                                policy_blob, birth_salt, created_at, updated_at)
             VALUES (?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(room_id) DO UPDATE SET
                 class = excluded.class,
                 owner_id = excluded.owner_id,
                 policy_version = excluded.policy_version,
                 policy_blob = excluded.policy_blob,
                 birth_salt = excluded.birth_salt,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                room_id.as_slice(),
                class,
                owner_id.as_slice(),
                policy_version as i64,
                policy_blob,
                birth_salt,
                now,
            ],
        )
        .context("mint the room record")?;
        Self::retain_policy_version(&tx, &room_id, policy_version, &policy_blob, now)
            .context("retain the founding policy version")?;

        for m in &members {
            // `invited_by` is NULL for a founding principal: nobody invited
            // the owner into its own room, and the home nest is seated by
            // the ceremony itself. A `joined_at` already on the row is
            // preserved — the same rule the report door follows.
            //
            // The two sealing columns are written here because seating and
            // wrap-targeting are one fact: a founding principal that arrives
            // with no reception key is seated with none, and a member top-up
            // is what heals it later.
            let entry_id =
                fauna_mls::room_policy::derive_room_entry_id(&room_id, &m.principal_id, now)
                    .context("derive a founding principal's roster-entry id")?;
            tx.execute(
                "INSERT INTO room_members (room_id, principal_id, principal_kind, role,
                                           home_node_url, joined_at, reported_at, removed_at,
                                           entry_id, reception_pubkey)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, NULL, ?7, ?8)
                 ON CONFLICT(room_id, principal_id) DO UPDATE SET
                     principal_kind = excluded.principal_kind,
                     role = excluded.role,
                     home_node_url = excluded.home_node_url,
                     reported_at = excluded.reported_at,
                     removed_at = NULL,
                     entry_id = excluded.entry_id,
                     reception_pubkey = excluded.reception_pubkey",
                rusqlite::params![
                    room_id.as_slice(),
                    m.principal_id.as_slice(),
                    m.principal_kind,
                    m.role,
                    m.home_node_url,
                    now,
                    entry_id.as_slice(),
                    if m.reception_pubkey.is_empty() {
                        None
                    } else {
                        Some(m.reception_pubkey.as_slice())
                    },
                ],
            )
            .context("seat a founding principal")?;
        }

        tx.commit().context("commit room founding")?;
        Ok(true)
    }

    /// The role a principal holds on a room's floor roster **right now**, or
    /// `None` when it is not a live member.
    ///
    /// ⚠ A live member of a *policy-less* room — one with no room
    /// policy — has no role, so this answers `Some(None)`-shaped
    /// information as `Ok(None)` would be wrong. Callers that need
    /// membership rather than rank must use [`CacheDb::is_room_member`]:
    /// this returns the role, and a role-less member is not absent.
    pub async fn get_room_member_role(
        &self,
        room_id: &[u8; 32],
        principal_id: &[u8; 32],
    ) -> Result<Option<String>> {
        let room_id = *room_id;
        let principal_id = *principal_id;
        let conn = self.conn.lock().await;
        let role: Option<Option<String>> = conn
            .query_row(
                "SELECT role FROM room_members
                 WHERE room_id = ?1 AND principal_id = ?2 AND removed_at IS NULL",
                rusqlite::params![room_id.as_slice(), principal_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("get_room_member_role")?;
        Ok(role.flatten())
    }

    /// Whether a principal is a **live member** of a room's floor roster —
    /// the membership question, independent of rank.
    ///
    /// This is what the custody serve door asks (`account-replica-posture.md`
    /// § Shared-audience carve-out: the member-mint rule reads membership,
    /// not rank), and it is deliberately not [`CacheDb::get_room_member_role`]
    /// — a policy-less room's members carry no role, and a door keyed on the role
    /// would fail closed for exactly the rooms that exist today.
    pub async fn is_room_member(
        &self,
        room_id: &[u8; 32],
        principal_id: &[u8; 32],
    ) -> Result<bool> {
        let room_id = *room_id;
        let principal_id = *principal_id;
        let conn = self.conn.lock().await;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM room_members
                               WHERE room_id = ?1 AND principal_id = ?2
                                 AND removed_at IS NULL)",
                rusqlite::params![room_id.as_slice(), principal_id.as_slice()],
                |row| row.get(0),
            )
            .context("is_room_member")?;
        Ok(exists)
    }

    /// The ceremony-born rooms whose live **owner** seat `owner` holds and in
    /// which somebody could take ownership over — another live **user** member
    /// homed on this nest, exactly the incoming owner the transfer door admits.
    ///
    /// What a user's own account deletion is refused on
    /// (`conversation-rooms.md` § Roles and authorization, the owner rule): a
    /// room in this list would be left with no owner seat while an honest
    /// transfer was one act away. A room NOT in it — the owner beside only its
    /// home nest, or beside members homed elsewhere, whom no transfer can reach
    /// until re-homing exists — is not held against the deletion, because a
    /// refusal nothing can lift is the unrecoverable state
    /// `nest/common.md` § Client-state recoverability forbids.
    pub async fn rooms_awaiting_an_ownership_transfer(
        &self,
        owner: &[u8; 32],
    ) -> Result<Vec<[u8; 32]>> {
        let owner = *owner;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT r.room_id FROM rooms r
                   JOIN room_members o
                     ON o.room_id = r.room_id AND o.principal_id = ?1
                    AND o.removed_at IS NULL AND o.role = 'owner'
                  WHERE r.birth_salt IS NOT NULL
                    AND EXISTS (SELECT 1 FROM room_members m
                                 WHERE m.room_id = r.room_id AND m.principal_id != ?1
                                   AND m.removed_at IS NULL AND m.principal_kind = 'user'
                                   AND m.home_node_url = '')
                  ORDER BY r.room_id",
            )
            .context("prepare rooms_awaiting_an_ownership_transfer")?;
        let rows = stmt
            .query_map(rusqlite::params![owner.as_slice()], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .context("rooms_awaiting_an_ownership_transfer")?;
        let mut rooms = Vec::new();
        for row in rows {
            let id = row.context("room id row")?;
            rooms.push(<[u8; 32]>::try_from(id.as_slice()).context("room id width")?);
        }
        Ok(rooms)
    }

    /// Whether a room's floor **positively says** this principal is gone — a
    /// row that exists and carries `removed_at`.
    ///
    /// The verdict is the inverse of [`CacheDb::is_room_member`]'s; the domain
    /// deliberately is *not* its negation, and that asymmetry is the whole
    /// reason this query exists. `is_room_member` answers false both for a
    /// principal the floor watched leave and for one it has never heard of;
    /// this answers true only for the first. That is what lets the relayed
    /// room doors refuse a removed foreign member without stacking a seat
    /// check that would deny the **newest-seated** one — the member the
    /// binding-only gate is ratified to protect, whose co-members are still
    /// nameless (`federation.md` § Federation residue surface, the *room
    /// roster read* row). A principal with no row at all is not refused: that
    /// covers the newest-seated member, and every folder channel's member,
    /// since a folder holds no `room_members` row in the first place.
    pub async fn room_member_removed(
        &self,
        room_id: &[u8; 32],
        principal_id: &[u8; 32],
    ) -> Result<bool> {
        let room_id = *room_id;
        let principal_id = *principal_id;
        let conn = self.conn.lock().await;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM room_members
                               WHERE room_id = ?1 AND principal_id = ?2
                                 AND removed_at IS NOT NULL)",
                rusqlite::params![room_id.as_slice(), principal_id.as_slice()],
                |row| row.get(0),
            )
            .context("room_member_removed")?;
        Ok(exists)
    }

    /// A room's live floor roster.
    pub async fn list_floor_roster(&self, room_id: &[u8; 32]) -> Result<Vec<RoomMemberRow>> {
        self.list_floor_roster_inner(room_id, false).await
    }

    /// A room's floor roster including the rows later reports absorbed —
    /// the membership *history* the succession axis keeps.
    pub async fn list_floor_roster_including_removed(
        &self,
        room_id: &[u8; 32],
    ) -> Result<Vec<RoomMemberRow>> {
        self.list_floor_roster_inner(room_id, true).await
    }

    async fn list_floor_roster_inner(
        &self,
        room_id: &[u8; 32],
        include_removed: bool,
    ) -> Result<Vec<RoomMemberRow>> {
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        // `LEFT JOIN` each principal's local `users` row to enrich the roster
        // with the handle this nest already holds — the `list_contacts_full`
        // shape (`u.handle` NULL for a principal with no local `users` row,
        // `''` for a local user with none set). One query keeps the roster
        // read cheap, and the join is what makes the handle *nest-joined*
        // rather than member-reported (`conversation-rooms.md` § The floor
        // roster). The second `LEFT JOIN` is the foreign twin: a member homed
        // elsewhere has no `users` row here, but its own home nest may have
        // announced its handle on the member's relayed drain, which this nest
        // verified and stored on the binding row (`federation.md` § Cross-nest
        // shared folders + channel append, the id→handle bullet). A room id is
        // its channel id, so the binding is keyed exactly as the roster is.
        let sql = if include_removed {
            "SELECT m.principal_id, m.principal_kind, m.role, m.home_node_url, m.joined_at,
                    m.reported_at, m.removed_at, m.entry_id, m.reception_pubkey, u.handle,
                    f.handle, f.handle_domain
             FROM room_members m
             LEFT JOIN users u ON u.actor_id = m.principal_id
             LEFT JOIN channel_foreign_members f
                    ON f.channel_id = m.room_id AND f.actor_id = m.principal_id
             WHERE m.room_id = ?1 ORDER BY m.joined_at, m.principal_id"
        } else {
            "SELECT m.principal_id, m.principal_kind, m.role, m.home_node_url, m.joined_at,
                    m.reported_at, m.removed_at, m.entry_id, m.reception_pubkey, u.handle,
                    f.handle, f.handle_domain
             FROM room_members m
             LEFT JOIN users u ON u.actor_id = m.principal_id
             LEFT JOIN channel_foreign_members f
                    ON f.channel_id = m.room_id AND f.actor_id = m.principal_id
             WHERE m.room_id = ?1 AND m.removed_at IS NULL
             ORDER BY m.joined_at, m.principal_id"
        };
        let mut stmt = conn.prepare(sql).context("prepare floor roster read")?;
        let rows = stmt
            .query_map(rusqlite::params![room_id.as_slice()], |row| {
                let principal: Vec<u8> = row.get(0)?;
                Ok(RoomMemberRow {
                    principal_id: blob_col_to_array(principal, 0, "principal_id")?,
                    principal_kind: row.get(1)?,
                    role: row.get(2)?,
                    home_node_url: row.get(3)?,
                    joined_at: row.get(4)?,
                    reported_at: row.get(5)?,
                    removed_at: row.get(6)?,
                    entry_id: row.get(7)?,
                    reception_pubkey: row.get(8)?,
                    handle: row.get(9)?,
                    foreign_handle: row.get(10)?,
                    foreign_domain: row.get(11)?,
                })
            })
            .context("query floor roster")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect floor roster")?;
        Ok(rows)
    }

    // ==================== Invitations and seating ====================

    /// Record an invitation **and deliver it to the invitee's inbox**, in one
    /// transaction. Idempotent on `(room_id, invitee)`: re-inviting someone
    /// already invited refreshes the pending invite rather than erroring, which
    /// is what a client retrying a lost reply produces.
    ///
    /// Returns `false` when the invitee has **already accepted** — the
    /// caller turns that into "already a member" rather than silently
    /// re-opening a settled invitation and letting a later `accept` re-seat
    /// a principal the room may since have removed. Nothing is delivered in
    /// that case: the invitation is settled, so there is no knock to raise.
    ///
    /// **Why one transaction.** The recorded row is what `accept_invite` reads;
    /// the inbox envelope is the only way an invitee ever learns the room id to
    /// accept *with* (`conversation-rooms.md` § Join rules and invites — an
    /// invite is "delivered to the invitee's home nest through the inbox
    /// plane"). Written separately they can diverge in both directions, and both
    /// halves of the divergence are silent: a recorded invitation nobody can
    /// discover leaves the invitee waiting on a knock that never comes, and a
    /// delivered one with no row behind it is an invitation the accept door
    /// refuses. Same shape and same reason as [`Self::accept_room_invite`],
    /// which seats and stamps together.
    ///
    /// **One standing envelope per open invitation, not one per call**. `room_invites.inbox_link_id` tracks the
    /// invitation's current un-acked delivery; a re-invitation acks + refunds
    /// that envelope before delivering the fresh one, so N calls for one
    /// (room, invitee) leave exactly one un-acked inbox row, never N — the
    /// invitee may still have declined (acked) it since, in which case there
    /// is nothing to consume and this is a plain fresh delivery. The fresh
    /// envelope is charged against the invitee's inbox quota exactly as
    /// [`Self::push_inbox_with_quota`] charges any other delivery, when
    /// `enforce_quota` is set (off on the single-user desktop nest, which
    /// tracks no inbox quota for anyone). The quota check itself runs
    /// **inside** this transaction, after the refund above: a re-invitation
    /// that nets zero must never be refused by the very envelope it is about
    /// to replace.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_room_invite_and_deliver(
        &self,
        room_id: &[u8; 32],
        invitee_id: &[u8; 32],
        inviter_id: &[u8; 32],
        role: &str,
        invitee_node_url: &str,
        signed_invite: &[u8],
        inbox_payload: &[u8],
        enforce_quota: bool,
    ) -> Result<bool> {
        let room_id = *room_id;
        let invitee_id = *invitee_id;
        let inviter_id = *inviter_id;
        let role = role.to_string();
        let invitee_node_url = invitee_node_url.to_string();
        let signed_invite = signed_invite.to_vec();
        let inbox_payload = inbox_payload.to_vec();

        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();

        let tx = conn.transaction().context("begin invite")?;
        let existing: Option<(Option<i64>, Option<i64>)> = tx
            .query_row(
                "SELECT accepted_at, inbox_link_id FROM room_invites WHERE room_id = ?1 AND invitee_id = ?2",
                rusqlite::params![room_id.as_slice(), invitee_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("read existing invite")?;
        if matches!(existing, Some((Some(_), _))) {
            return Ok(false);
        }
        if let Some((None, Some(prior_link_id))) = existing {
            Self::ack_and_refund_row(&tx, &invitee_id, prior_link_id)
                .context("consume the prior room-invite envelope")?;
        }

        // Checked here — inside the transaction, after the prior envelope's
        // refund above and before this one is charged below — so a
        // re-invitation that merely replaces its own standing envelope nets
        // zero instead of being refused by the envelope it is about to
        // consume. `check_quota_in_tx` reads `self.conn` directly rather
        // than through `check_quota`/`get_user`/`get_tier`, which lock it
        // themselves and would deadlock against this transaction's
        // (non-reentrant) hold on the same mutex — the same reason
        // `ack_and_refund_row` above takes `&tx` rather than calling
        // `ack_inbox`. A refusal here returns without `tx.commit()`, so the
        // refund just recorded rolls back with everything else.
        if enforce_quota {
            Self::check_quota_in_tx(&tx, &invitee_id, inbox_payload.len())
                .context("room invite exceeds invitee's inbox quota")?;
        }

        // Charged against the invitee's inbox quota only when enforced; the
        // envelope's link records which, so its ack refunds exactly that.
        let link_id = Self::insert_inbox_row(&tx, &invitee_id, &inbox_payload, None, enforce_quota)
            .context("deliver room invite")?;

        tx.execute(
            "INSERT INTO room_invites (room_id, invitee_id, inviter_id, role,
                                       invitee_node_url, signed_invite, invited_at, accepted_at,
                                       inbox_link_id, invitee_nest_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, NULL)
             ON CONFLICT(room_id, invitee_id) DO UPDATE SET
                 inviter_id = excluded.inviter_id,
                 role = excluded.role,
                 invitee_node_url = excluded.invitee_node_url,
                 signed_invite = excluded.signed_invite,
                 invited_at = excluded.invited_at,
                 inbox_link_id = excluded.inbox_link_id,
                 invitee_nest_id = excluded.invitee_nest_id",
            rusqlite::params![
                room_id.as_slice(),
                invitee_id.as_slice(),
                inviter_id.as_slice(),
                role,
                invitee_node_url,
                signed_invite,
                now,
                link_id,
            ],
        )
        .context("record room invite")?;
        tx.commit().context("commit invite")?;
        Ok(true)
    }

    /// Record an invitation whose invitee is homed on **another nest** — the
    /// row alone, with no local envelope: the knock the invitee sees is
    /// delivered by the room's home to the invitee's own nest over
    /// `fauna.federation.conversation.room.invite`, and lands in that nest's
    /// inbox, not this one's (`conversation-rooms.md` § Join rules and invites
    /// → *A cross-nest invitation*). The row is recorded **before** the
    /// delivery is originated and consumed by the caller if the delivery does
    /// not land, so a delivered knock always has a row behind it — the
    /// [`Self::record_room_invite_and_deliver`] invariant, kept across a
    /// boundary one transaction cannot span.
    ///
    /// `invitee_nest_id` is the delivered-to nest's **verified** identity,
    /// resolved from the federation dial and stored on the row as the gate the
    /// relayed accept runs behind ([`PendingRoomInvite::invitee_nest_id`]).
    /// Idempotent on `(room_id, invitee)` exactly as the local writer is, and
    /// answers `false` for an invitee who has already accepted. A standing
    /// local envelope from an earlier same-nest invitation of the same
    /// principal — a home re-declared — is acked and refunded, so exactly one
    /// knock stands wherever it stands.
    pub async fn record_room_invite_for_foreign_delivery(
        &self,
        room_id: &[u8; 32],
        invitee_id: &[u8; 32],
        inviter_id: &[u8; 32],
        role: &str,
        invitee_node_url: &str,
        signed_invite: &[u8],
        invitee_nest_id: &[u8; 32],
    ) -> Result<bool> {
        let room_id = *room_id;
        let invitee_id = *invitee_id;
        let inviter_id = *inviter_id;
        let invitee_nest_id = *invitee_nest_id;
        let role = role.to_string();
        let invitee_node_url = invitee_node_url.to_string();
        let signed_invite = signed_invite.to_vec();

        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();

        let tx = conn.transaction().context("begin foreign invite")?;
        let existing: Option<(Option<i64>, Option<i64>)> = tx
            .query_row(
                "SELECT accepted_at, inbox_link_id FROM room_invites WHERE room_id = ?1 AND invitee_id = ?2",
                rusqlite::params![room_id.as_slice(), invitee_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("read existing invite")?;
        if matches!(existing, Some((Some(_), _))) {
            return Ok(false);
        }
        if let Some((None, Some(prior_link_id))) = existing {
            Self::ack_and_refund_row(&tx, &invitee_id, prior_link_id)
                .context("consume the prior room-invite envelope")?;
        }
        tx.execute(
            "INSERT INTO room_invites (room_id, invitee_id, inviter_id, role,
                                       invitee_node_url, signed_invite, invited_at, accepted_at,
                                       inbox_link_id, invitee_nest_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8)
             ON CONFLICT(room_id, invitee_id) DO UPDATE SET
                 inviter_id = excluded.inviter_id,
                 role = excluded.role,
                 invitee_node_url = excluded.invitee_node_url,
                 signed_invite = excluded.signed_invite,
                 invited_at = excluded.invited_at,
                 inbox_link_id = NULL,
                 invitee_nest_id = excluded.invitee_nest_id",
            rusqlite::params![
                room_id.as_slice(),
                invitee_id.as_slice(),
                inviter_id.as_slice(),
                role,
                invitee_node_url,
                signed_invite,
                now,
                invitee_nest_id.as_slice(),
            ],
        )
        .context("record foreign room invite")?;
        tx.commit().context("commit foreign invite")?;
        Ok(true)
    }

    /// A pending (not yet accepted) invitation's inviter and role.
    pub async fn get_pending_room_invite(
        &self,
        room_id: &[u8; 32],
        invitee_id: &[u8; 32],
    ) -> Result<Option<PendingRoomInvite>> {
        let room_id = *room_id;
        let invitee_id = *invitee_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT inviter_id, role, invitee_node_url, invitee_nest_id FROM room_invites
                 WHERE room_id = ?1 AND invitee_id = ?2 AND accepted_at IS NULL",
                rusqlite::params![room_id.as_slice(), invitee_id.as_slice()],
                |row| {
                    let inviter: Vec<u8> = row.get(0)?;
                    let nest: Option<Vec<u8>> = row.get(3)?;
                    Ok(PendingRoomInvite {
                        inviter_id: blob_col_to_array(inviter, 0, "inviter_id")?,
                        role: row.get(1)?,
                        invitee_node_url: row.get(2)?,
                        invitee_nest_id: nest
                            .map(|b| blob_col_to_array(b, 3, "invitee_nest_id"))
                            .transpose()?,
                    })
                },
            )
            .optional()
            .context("get_pending_room_invite")?;
        Ok(row)
    }

    /// An invitation's home-nest binding, pending **or accepted** — the one
    /// read the relayed accept door gates on
    /// ([`RoomInviteHomeBinding`]). An accepted row is deliberately served: it
    /// is what proves, on a §4.D re-send that arrives after the seat landed,
    /// that the re-sending nest is the one the invitation went to. A consumed
    /// (lapsed or withdrawn) invitation has no row and answers `None`.
    pub async fn room_invite_home_binding(
        &self,
        room_id: &[u8; 32],
        invitee_id: &[u8; 32],
    ) -> Result<Option<RoomInviteHomeBinding>> {
        let room_id = *room_id;
        let invitee_id = *invitee_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT invitee_nest_id, invitee_node_url, accepted_at FROM room_invites
                 WHERE room_id = ?1 AND invitee_id = ?2",
                rusqlite::params![room_id.as_slice(), invitee_id.as_slice()],
                |row| {
                    let nest: Option<Vec<u8>> = row.get(0)?;
                    let accepted_at: Option<i64> = row.get(2)?;
                    Ok(RoomInviteHomeBinding {
                        invitee_nest_id: nest
                            .map(|b| blob_col_to_array(b, 0, "invitee_nest_id"))
                            .transpose()?,
                        invitee_node_url: row.get(1)?,
                        accepted: accepted_at.is_some(),
                    })
                },
            )
            .optional()
            .context("room_invite_home_binding")?;
        Ok(row)
    }

    /// Accept a pending invitation: stamp it accepted and seat the invitee on
    /// the floor, **in one transaction**. A seated member with no accepted
    /// invitation, or an accepted invitation with no roster row, is a
    /// membership state no ceremony produced — and the custody serve door
    /// reads the roster per request.
    ///
    /// **Consumes the invitation's standing envelope in the same
    /// transaction**: the invitee is being seated right
    /// here, so nothing should be left asking it to accept an invitation it
    /// just accepted. This is the nest's own act, not a substitute for the
    /// accepting client's `fauna.inbox.ack` of the one row it polled — both
    /// are idempotent on `status = 'undelivered'`, so whichever runs first
    /// wins and the other is a no-op.
    ///
    /// [`RoomInviteAccept::NotPending`] when there is no pending invitation, so
    /// a replay of an accept lands as a refusal rather than re-seating a
    /// principal the room may have removed in between.
    ///
    /// **`judged_at_version` binds the seating to the judgement that allowed
    /// it.** The accept door judges the invitation against the room's current
    /// policy before it calls this (`conversation-rooms.md` § Join rules and
    /// invites → *An invitation is a standing offer*), in reads of its own;
    /// a `set_policy` landing between that judgement and this transaction
    /// reconciles live seats only, so it would miss the seat written here —
    /// an admin the new policy just dropped, seated as admin. Same
    /// compare-and-swap [`Self::set_room_policy`] makes on its own write.
    /// `None` skips the guard, for a caller that made no judgement (a db-level
    /// fixture).
    pub async fn accept_room_invite(
        &self,
        room_id: &[u8; 32],
        invitee_id: &[u8; 32],
        reception_pubkey: &[u8],
        judged_at_version: Option<u64>,
    ) -> Result<RoomInviteAccept> {
        let reception_pubkey = reception_pubkey.to_vec();
        let room_id = *room_id;
        let invitee_id = *invitee_id;
        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();

        let tx = conn.transaction().context("begin accept")?;
        if let Some(judged) = judged_at_version {
            let held: Option<i64> = tx
                .query_row(
                    "SELECT policy_version FROM rooms WHERE room_id = ?1",
                    rusqlite::params![room_id.as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .context("read the room's policy version")?
                .flatten();
            if held != Some(judged as i64) {
                return Ok(RoomInviteAccept::PolicyMoved);
            }
        }
        let pending: Option<(Vec<u8>, String, String, Option<i64>)> = tx
            .query_row(
                "SELECT inviter_id, role, invitee_node_url, inbox_link_id FROM room_invites
                 WHERE room_id = ?1 AND invitee_id = ?2 AND accepted_at IS NULL",
                rusqlite::params![room_id.as_slice(), invitee_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .context("read pending invite")?;
        let Some((inviter, role, node_url, inbox_link_id)) = pending else {
            return Ok(RoomInviteAccept::NotPending);
        };

        tx.execute(
            "UPDATE room_invites SET accepted_at = ?3
             WHERE room_id = ?1 AND invitee_id = ?2",
            rusqlite::params![room_id.as_slice(), invitee_id.as_slice(), now],
        )
        .context("stamp invite accepted")?;

        if let Some(link_id) = inbox_link_id {
            Self::ack_and_refund_row(&tx, &invitee_id, link_id)
                .context("consume the accepted room invite's envelope")?;
        }

        // A returning principal comes back on its own row with its original
        // `joined_at` — the same rule the report door follows, so "since when
        // has this principal been part of this room" survives a departure and
        // a re-invitation.
        //
        // Its **roster entry** is the opposite: derived from `now`, so a
        // re-admission lands on a fresh slot. That is the scheme's own rule
        // ("re-admission is a fresh entry id, so add-wins resurrection is
        // unrepresentable"), and here it is what stops a returning member
        // replaying a wrap minted for the seat it was removed from — the
        // wraps of the generations it was severed from stay bound to the old
        // entry id and open nothing at the new one.
        let entry_id = fauna_mls::room_policy::derive_room_entry_id(&room_id, &invitee_id, now)
            .context("derive the accepting invitee's roster-entry id")?;
        tx.execute(
            "INSERT INTO room_members (room_id, principal_id, principal_kind, role,
                                       invited_by, home_node_url, joined_at, reported_at, removed_at,
                                       entry_id, reception_pubkey)
             VALUES (?1, ?2, 'user', ?3, ?4, ?5, ?6, ?6, NULL, ?7, ?8)
             ON CONFLICT(room_id, principal_id) DO UPDATE SET
                 role = excluded.role,
                 invited_by = excluded.invited_by,
                 home_node_url = excluded.home_node_url,
                 reported_at = excluded.reported_at,
                 removed_at = NULL,
                 entry_id = excluded.entry_id,
                 reception_pubkey = excluded.reception_pubkey",
            rusqlite::params![
                room_id.as_slice(),
                invitee_id.as_slice(),
                role,
                inviter,
                node_url,
                now,
                entry_id.as_slice(),
                if reception_pubkey.is_empty() {
                    None
                } else {
                    Some(reception_pubkey.as_slice())
                },
            ],
        )
        .context("seat the accepting invitee")?;

        tx.commit().context("commit accept")?;
        Ok(RoomInviteAccept::Seated)
    }

    /// Consume a pending invitation: the row, its standing envelope and that
    /// envelope's quota charge, **in one transaction** — the three facts
    /// [`Self::record_room_invite_and_deliver`] wrote as one act leave as one.
    /// Two callers, one writer (`conversation-rooms.md` § Join rules and
    /// invites): the accept door, when its judgement refuses an invitation
    /// (a **lapse**), and the revoke door (a **withdrawal**). Neither tells
    /// the invitee anything; the invitation simply stops standing.
    ///
    /// The row is deleted rather than stamped: a consumed invitation is not
    /// history anybody reads, and a row left behind would block nothing (a
    /// re-invitation upserts over it) while still naming two people. The
    /// invitee may already have declined — acked the envelope — in which case
    /// there is nothing to consume and only the row goes. An *accepted*
    /// invitation is never touched.
    ///
    /// **`issued_by` narrows the act to one inviter's invitation**, inside the
    /// transaction. The accept door passes the inviter it judged: a
    /// re-invitation upserts over a pending row, so one landing between the
    /// judgement and this transaction is a different invitation — from
    /// somebody who may well hold the authority — and it stands. The revoke
    /// door passes a plain member's own id, since such a caller withdraws only
    /// what it issued, and `None` for an owner or admin, who withdraw any.
    ///
    /// Returns `false` when no such invitation was pending.
    pub async fn consume_pending_room_invite(
        &self,
        room_id: &[u8; 32],
        invitee_id: &[u8; 32],
        issued_by: Option<&[u8; 32]>,
    ) -> Result<bool> {
        let room_id = *room_id;
        let invitee_id = *invitee_id;
        let issued_by = issued_by.map(|id| id.to_vec());
        let mut conn = self.conn.lock().await;

        let tx = conn.transaction().context("begin consume invite")?;
        let pending: Option<Option<i64>> = tx
            .query_row(
                "SELECT inbox_link_id FROM room_invites
                 WHERE room_id = ?1 AND invitee_id = ?2
                   AND (?3 IS NULL OR inviter_id = ?3)
                   AND accepted_at IS NULL",
                rusqlite::params![room_id.as_slice(), invitee_id.as_slice(), issued_by],
                |row| row.get(0),
            )
            .optional()
            .context("read pending invite")?;
        let Some(inbox_link_id) = pending else {
            return Ok(false);
        };
        if let Some(link_id) = inbox_link_id {
            Self::ack_and_refund_row(&tx, &invitee_id, link_id)
                .context("consume the room invite's envelope")?;
        }
        tx.execute(
            "DELETE FROM room_invites
             WHERE room_id = ?1 AND invitee_id = ?2 AND accepted_at IS NULL",
            rusqlite::params![room_id.as_slice(), invitee_id.as_slice()],
        )
        .context("delete the consumed invite")?;
        tx.commit().context("commit consume invite")?;
        Ok(true)
    }

    /// A room's **pending** invitations, oldest first — the read behind
    /// `fauna.conversations.room.list_invites`. Two invitations sharing a
    /// millisecond stamp list in the order they were issued (`rowid`), never in
    /// the order of the invitees' keys. Room-scoped, unlike
    /// [`Self::get_pending_room_invite`] (one invitee's own row) and unlike the
    /// invitee-side inbox walk in `fauna-client-inbox`: this is what the ROOM
    /// holds, served to whoever may withdraw it.
    ///
    /// `issued_by` narrows it to one inviter's — a plain member's view; `None`
    /// is the owner's and admins'. Handles are joined from this nest's own
    /// `users` rows, the floor roster read's rule: display only, and never
    /// taken from anything a client sent. The table's primary key leads with
    /// `room_id`, so the scan needs no index of its own.
    pub async fn pending_invites_for_room(
        &self,
        room_id: &[u8; 32],
        issued_by: Option<&[u8; 32]>,
    ) -> Result<Vec<RoomPendingInviteRow>> {
        let room_id = *room_id;
        let issued_by = issued_by.map(|id| id.to_vec());
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT i.invitee_id, i.inviter_id, i.role, i.invited_at, ue.handle, ur.handle
                 FROM room_invites i
                 LEFT JOIN users ue ON ue.actor_id = i.invitee_id
                 LEFT JOIN users ur ON ur.actor_id = i.inviter_id
                 WHERE i.room_id = ?1 AND i.accepted_at IS NULL
                   AND (?2 IS NULL OR i.inviter_id = ?2)
                 ORDER BY i.invited_at, i.rowid",
            )
            .context("prepare pending_invites_for_room")?;
        let rows = stmt
            .query_map(rusqlite::params![room_id.as_slice(), issued_by], |row| {
                let invitee: Vec<u8> = row.get(0)?;
                let inviter: Vec<u8> = row.get(1)?;
                Ok(RoomPendingInviteRow {
                    invitee_id: blob_col_to_array(invitee, 0, "invitee_id")?,
                    inviter_id: blob_col_to_array(inviter, 1, "inviter_id")?,
                    role: row.get(2)?,
                    invited_at: row.get(3)?,
                    invitee_handle: row.get(4)?,
                    inviter_handle: row.get(5)?,
                })
            })
            .context("pending_invites_for_room")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("read pending invites")?;
        Ok(rows)
    }

    /// Bind — or rotate — the wrap target of a **user** principal's own live
    /// seat, the write behind `fauna.conversations.room.set_reception_key`.
    ///
    /// The two seating writers ([`Self::found_room`], [`Self::accept_room_invite`])
    /// carry the key with the row; this is the one writer for a seat that
    /// already exists. Two facts about the seat are decided here and nowhere
    /// else:
    ///
    /// - **The entry survives a rotation.** A wrap is bound to
    ///   `(generation, entry)` and sealed to the key of its moment; the account
    ///   retains every reception key it held, so the wraps already stored at
    ///   this entry stay openable. A fresh entry is the scheme's re-admission
    ///   rule (`Removed` → `Enrolled`), and a live seat changing its key is
    ///   not that transition. A seat with **no** entry is given one, derived
    ///   from now, so it is healed by the same statement.
    /// - **Only a user's seat, and only its own.** The caller names itself:
    ///   the home nest's read is a grant the members make at a mint, and a
    ///   bridge is seated by its own enrollment.
    ///
    /// Returns `None` when the principal holds no live user seat in the room.
    /// A call naming the key already bound is answered, not refused —
    /// `rotated == false` and nothing rewritten but the stamp.
    pub async fn set_room_member_reception_key(
        &self,
        room_id: &[u8; 32],
        principal_id: &[u8; 32],
        reception_pubkey: &[u8],
    ) -> Result<Option<ReceptionKeyBound>> {
        let room_id = *room_id;
        let principal_id = *principal_id;
        let reception_pubkey = reception_pubkey.to_vec();
        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();

        let tx = conn.transaction().context("begin set reception key")?;
        let seat: Option<(Option<Vec<u8>>, Option<Vec<u8>>)> = tx
            .query_row(
                "SELECT entry_id, reception_pubkey FROM room_members
                 WHERE room_id = ?1 AND principal_id = ?2
                   AND removed_at IS NULL AND principal_kind = 'user'",
                rusqlite::params![room_id.as_slice(), principal_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("read the caller's own seat")?;
        let Some((entry, held)) = seat else {
            return Ok(None);
        };
        let entry_id: [u8; 32] = match entry.as_deref().and_then(|e| e.try_into().ok()) {
            Some(entry) => entry,
            None => fauna_mls::room_policy::derive_room_entry_id(&room_id, &principal_id, now)
                .context("derive a roster-entry id for a seat with no entry")?,
        };
        let rotated = held
            .as_deref()
            .is_some_and(|k| !k.is_empty() && k != reception_pubkey.as_slice());
        tx.execute(
            "UPDATE room_members
                SET reception_pubkey = ?3, entry_id = ?4, reported_at = ?5
              WHERE room_id = ?1 AND principal_id = ?2 AND removed_at IS NULL",
            rusqlite::params![
                room_id.as_slice(),
                principal_id.as_slice(),
                reception_pubkey,
                entry_id.as_slice(),
                now,
            ],
        )
        .context("bind the seat's wrap target")?;
        tx.commit().context("commit set reception key")?;
        Ok(Some(ReceptionKeyBound { entry_id, rotated }))
    }

    /// Unseat a principal — `Removed`-absorbing its row rather than deleting
    /// it, the shape the succession axis declares for a membership row
    /// (§ The home nest → *The succession axis*). Any pending invitation is
    /// cleared in the same transaction, so a removal cannot be undone by an
    /// `accept` of an invite that predates it.
    ///
    /// Returns `false` when the principal was not a live member.
    pub async fn unseat_room_member(
        &self,
        room_id: &[u8; 32],
        principal_id: &[u8; 32],
    ) -> Result<bool> {
        let room_id = *room_id;
        let principal_id = *principal_id;
        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();

        let tx = conn.transaction().context("begin unseat")?;
        let touched = tx
            .execute(
                "UPDATE room_members SET removed_at = ?3, reported_at = ?3
                 WHERE room_id = ?1 AND principal_id = ?2 AND removed_at IS NULL",
                rusqlite::params![room_id.as_slice(), principal_id.as_slice(), now],
            )
            .context("absorb the removed principal")?;
        if touched == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM room_invites WHERE room_id = ?1 AND invitee_id = ?2",
            rusqlite::params![room_id.as_slice(), principal_id.as_slice()],
        )
        .context("clear a stale invitation")?;
        tx.commit().context("commit unseat")?;
        Ok(true)
    }

    /// Store a room's new signed policy and **reconcile the floor roster's
    /// roles to it**, in one transaction.
    ///
    /// The signed policy is the authority for who is an admin — members
    /// verify the owner's signature on it — and `room_members.role` is its
    /// projection, the column every nest-side gate actually reads. Letting
    /// the two drift would let the nest enforce a rank the policy members
    /// render does not grant, which is exactly the divergence
    /// `conversation-rooms.md` § Roles and authorization rule 6 exists to
    /// prevent. So: a live **user** member the admin set names becomes
    /// `admin`; one it does not becomes `member`.
    ///
    /// Two rows the reconciliation deliberately does not touch. The
    /// **owner's** row keeps `owner` — the owner is not in its own admin set
    /// by construction (`RoomPolicy::validate`), so reconciling it from the
    /// admin set would demote the owner on every policy change. And a
    /// **non-user principal** — the home nest — keeps its role: it holds no
    /// operation in the roles table, and the admin set is a set of actors.
    ///
    /// `expected_version` is the strict ratchet: the write applies only if
    /// the stored version is still what the caller read, so two concurrent
    /// policy changes cannot interleave into a version that skips one.
    /// Returns `false` when it has moved.
    pub async fn set_room_policy(
        &self,
        room_id: &[u8; 32],
        expected_version: u64,
        new_version: u64,
        policy_blob: &[u8],
        admins: &[[u8; 32]],
    ) -> Result<bool> {
        let room_id = *room_id;
        let policy_blob = policy_blob.to_vec();
        let admins: Vec<Vec<u8>> = admins.iter().map(|a| a.to_vec()).collect();
        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();

        let tx = conn.transaction().context("begin set_room_policy")?;
        let touched = tx
            .execute(
                "UPDATE rooms SET policy_version = ?3, policy_blob = ?4, updated_at = ?5
                 WHERE room_id = ?1 AND policy_version = ?2",
                rusqlite::params![
                    room_id.as_slice(),
                    expected_version as i64,
                    new_version as i64,
                    policy_blob,
                    now,
                ],
            )
            .context("store the room policy")?;
        if touched == 0 {
            return Ok(false);
        }
        Self::retain_policy_version(&tx, &room_id, new_version, &policy_blob, now)
            .context("retain the new policy version")?;

        Self::reconcile_roster_to_admin_set(&tx, &room_id, &admins, now)
            .context("reconcile roster to the new admin set")?;

        tx.commit().context("commit set_room_policy")?;
        Ok(true)
    }

    /// Reconcile every live, non-`owner` user member's floor role to a
    /// stored policy's admin set — `admin` if it names them, `member`
    /// otherwise. This is what keeps `room_members.role` a projection of the
    /// signed policy, which every nest-side gate (`floor_role` /
    /// `is_admin_or_owner()`) reads instead of the policy blob itself.
    ///
    /// Runs inside the caller's transaction. Excludes `role = 'owner'`, so
    /// call it only once the room's `owner` row is already settled — for
    /// [`Self::transfer_room_ownership`] that means after the outgoing
    /// owner's row has been rewritten away from `owner` and before the
    /// incoming owner is seated, so at no point does a live row read `owner`
    /// while this scan is also free to touch it.
    fn reconcile_roster_to_admin_set(
        tx: &rusqlite::Transaction<'_>,
        room_id: &[u8; 32],
        admins: &[Vec<u8>],
        now: i64,
    ) -> Result<()> {
        let live: Vec<Vec<u8>> = {
            let mut stmt = tx
                .prepare(
                    "SELECT principal_id FROM room_members
                     WHERE room_id = ?1 AND removed_at IS NULL
                       AND principal_kind = 'user' AND role != 'owner'",
                )
                .context("prepare roster reconcile scan")?;
            stmt.query_map(rusqlite::params![room_id.as_slice()], |row| row.get(0))
                .context("scan roster for reconcile")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect roster for reconcile")?
        };
        for principal in live {
            let role = if admins.contains(&principal) {
                "admin"
            } else {
                "member"
            };
            tx.execute(
                "UPDATE room_members SET role = ?3, reported_at = ?4
                 WHERE room_id = ?1 AND principal_id = ?2",
                rusqlite::params![room_id.as_slice(), principal, role, now],
            )
            .context("reconcile a roster role")?;
        }
        Ok(())
    }

    /// Move a room's ownership: store the new signed policy, hand the
    /// `owner` role to `new_owner`, and reconcile every other live user
    /// member's floor role to the new policy's admin set — the outgoing
    /// owner included, who lands on `admin` if the new policy names them
    /// and `member` otherwise. One transaction, because a room with two
    /// owners or none is a membership state no ceremony produces and every
    /// role gate reads this table.
    ///
    /// This is the same roster projection [`Self::set_room_policy`] keeps —
    /// a transfer stores a policy exactly as `set_policy` does, so it must
    /// leave the floor agreeing with it for every member, not just the
    /// outgoing owner ().
    ///
    /// Returns `false` when the version ratchet has moved under the caller.
    #[allow(clippy::too_many_arguments)]
    pub async fn transfer_room_ownership(
        &self,
        room_id: &[u8; 32],
        expected_version: u64,
        new_version: u64,
        policy_blob: &[u8],
        old_owner: &[u8; 32],
        new_owner: &[u8; 32],
        admins: &[[u8; 32]],
    ) -> Result<bool> {
        let room_id = *room_id;
        let policy_blob = policy_blob.to_vec();
        let old_owner = *old_owner;
        let new_owner = *new_owner;
        let admins: Vec<Vec<u8>> = admins.iter().map(|a| a.to_vec()).collect();
        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();

        let tx = conn.transaction().context("begin transfer")?;
        let touched = tx
            .execute(
                "UPDATE rooms SET policy_version = ?3, policy_blob = ?4, owner_id = ?5,
                                  updated_at = ?6
                 WHERE room_id = ?1 AND policy_version = ?2",
                rusqlite::params![
                    room_id.as_slice(),
                    expected_version as i64,
                    new_version as i64,
                    policy_blob,
                    new_owner.as_slice(),
                    now,
                ],
            )
            .context("store the transferred room record")?;
        if touched == 0 {
            return Ok(false);
        }
        Self::retain_policy_version(&tx, &room_id, new_version, &policy_blob, now)
            .context("retain the transfer-minted policy version")?;

        // The outgoing owner first — so that at no point inside the
        // transaction do two rows read `owner`, and so the general reconcile
        // below (which excludes `role = 'owner'`) is free to also touch this
        // row.
        let outgoing_role = if admins.contains(&old_owner.to_vec()) {
            "admin"
        } else {
            "member"
        };
        tx.execute(
            "UPDATE room_members SET role = ?3, reported_at = ?4
             WHERE room_id = ?1 AND principal_id = ?2",
            rusqlite::params![room_id.as_slice(), old_owner.as_slice(), outgoing_role, now],
        )
        .context("demote the outgoing owner")?;

        // Every other live user member: `admin` if the new policy's admin
        // set names it, `member` otherwise () — the same
        // projection `set_room_policy` keeps. Runs before the incoming
        // owner is seated, so its `role != 'owner'` scan may also
        // (redundantly, harmlessly) revisit the just-demoted outgoing owner
        // without ever touching the not-yet-seated incoming one's final row.
        Self::reconcile_roster_to_admin_set(&tx, &room_id, &admins, now)
            .context("reconcile roster to the new admin set")?;

        tx.execute(
            "UPDATE room_members SET role = 'owner', reported_at = ?3
             WHERE room_id = ?1 AND principal_id = ?2 AND removed_at IS NULL",
            rusqlite::params![room_id.as_slice(), new_owner.as_slice(), now],
        )
        .context("seat the incoming owner")?;

        tx.commit().context("commit transfer")?;
        Ok(true)
    }

    /// File one signed policy version in the room's history
    /// (`room_policy_versions`), inside the transaction that makes it the
    /// floor's current one. **Every policy writer calls this** — founding,
    /// `set_room_policy`, an ownership transfer — because a member judges a
    /// floor delete record against the policy *of the version the record
    /// names* (`conversation-rooms.md` § Roles and authorization → *Delete any
    /// message — the mechanism* → *Community rooms*), and a version this table
    /// never received can never be served: every record made under it would
    /// fail closed on every member, for good.
    ///
    /// An upsert rather than an insert for one caller's sake: the founding
    /// door is itself an upsert at version 1, and its replay must converge.
    ///
    /// ⚠ **But it converges only on IDENTICAL bytes — a differing blob at a
    /// version already held is refused, never overwritten.** This table is
    /// *history*, and a history that can be rewritten answers a different
    /// question from the one every member asks of it: a floor delete record
    /// names a version, and the member's verdict is "does this record's author
    /// hold rank in the policy **of that version**" (`conversation-rooms.md`
    /// § Roles and authorization → *Members verify what they paint*). Letting a
    /// later write replace version *N*'s bytes would let the answer to a
    /// settled question change underneath every record already made under it.
    ///
    /// Today only a founding replay at version 1 can even reach the conflict —
    /// both other writers compare-and-set on `expected_version`, so they never
    /// re-write a number the room already has — and version 1 is owner-only
    /// with no admins, so no verdict could turn. **The refusal is here for the
    /// readers this table is about to acquire, not for a live bug:** a
    /// too-permissive rule that is merely unreachable is exactly the kind that
    /// stops being unreachable without anyone deciding it should.
    ///
    /// A member that has already anchored version *N* is unaffected either way
    /// — it keeps the chain it proved first (`FaunaMlsBackend::room_policy_chains`)
    /// — but a FRESH session re-proves from whatever the nest serves then, and
    /// that is the reader this protects.
    fn retain_policy_version(
        tx: &rusqlite::Transaction<'_>,
        room_id: &[u8; 32],
        version: u64,
        policy_blob: &[u8],
        now: i64,
    ) -> rusqlite::Result<()> {
        // `WHERE policy_blob = excluded.policy_blob` makes the conflict arm a
        // no-op for an identical replay and leaves the stored row untouched
        // for a differing one; the read below turns "untouched" into a loud
        // refusal rather than a silent one. Both in the caller's transaction,
        // so a refusal rolls back the policy move that carried it.
        tx.execute(
            "INSERT INTO room_policy_versions (room_id, version, policy_blob, stored_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(room_id, version) DO UPDATE SET stored_at = excluded.stored_at
             WHERE room_policy_versions.policy_blob = excluded.policy_blob",
            rusqlite::params![room_id.as_slice(), version as i64, policy_blob, now],
        )?;
        let held: Vec<u8> = tx.query_row(
            "SELECT policy_blob FROM room_policy_versions WHERE room_id = ?1 AND version = ?2",
            rusqlite::params![room_id.as_slice(), version as i64],
            |row| row.get(0),
        )?;
        if held != policy_blob {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "room policy version {version} is already retained with different bytes; \
                     a retained version is history and is never rewritten"
                )),
            ));
        }
        Ok(())
    }

    /// The signed policy a room held at `version` — current or superseded —
    /// exactly as its author signed it; `None` when the room never held that
    /// version here.
    pub async fn get_room_policy_version(
        &self,
        room_id: &[u8; 32],
        version: u64,
    ) -> Result<Option<Vec<u8>>> {
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT policy_blob FROM room_policy_versions WHERE room_id = ?1 AND version = ?2",
            rusqlite::params![room_id.as_slice(), version as i64],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get_room_policy_version")
    }

    /// A room's stored signed policy — the owner-signed record the floor
    /// applies on every write (`conversation-rooms.md` § Roles and
    /// authorization → *Community rooms — enforced at the floor*). `None`
    /// for a room the create ceremony did not found, which carries no
    /// policy the nest can read.
    pub async fn get_room_policy_blob(&self, room_id: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        let blob = conn
            .query_row(
                "SELECT policy_blob FROM rooms WHERE room_id = ?1",
                rusqlite::params![room_id.as_slice()],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()
            .context("get_room_policy_blob")?
            .flatten();
        Ok(blob)
    }

    /// The room record, when one exists.
    pub async fn get_room(&self, room_id: &[u8; 32]) -> Result<Option<RoomRecord>> {
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        let rec = conn
            .query_row(
                "SELECT class, home_node_url, owner_id, policy_version, birth_salt,
                        created_at, updated_at, roster_commit_seq
                 FROM rooms WHERE room_id = ?1",
                rusqlite::params![room_id.as_slice()],
                |row| {
                    Ok(RoomRecord {
                        room_id,
                        class: row.get(0)?,
                        home_node_url: row.get(1)?,
                        owner_id: row.get(2)?,
                        policy_version: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                        roster_commit_seq: row.get(7)?,
                        birth_salt: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .context("get_room")?;
        Ok(rec)
    }

    // ==================== The sealing plane ====================

    /// Store one admitted room generation mint and its wraps, in one
    /// transaction.
    ///
    /// The nest stores; it never mints (`conversation-rooms.md` § Don't do
    /// these). Admission — signer role, parent, roster coverage — is the
    /// handler's; what is atomic here is that a generation never becomes the
    /// room's tip without the wraps that make it openable, which would leave
    /// every member holding ciphertext nobody can read.
    ///
    /// `wraps` is `(entry_id, wrap bytes)`. A wrap naming an entry that is no
    /// longer live is stored and inert, per the scheme: severance is wrap
    /// *targeting* on future mints, never deletion of past ones.
    ///
    /// **The ratchet is re-checked here, under the lock the insert holds** —
    /// the row's parent must be the room's tip *at this instant*, else nothing
    /// is stored and the answer is [`RoomGenerationInsert::NotTheTip`]. The
    /// handler checks the same thing first, for its clear refusal; that check
    /// alone was a read followed by a write, so two concurrent rotations could
    /// both pass it and both land as children of one tip — the fork this plane
    /// has no arbiter to resolve. Every access to this database goes through
    /// the one connection mutex held below, so the check and the insert are
    /// indivisible.
    pub async fn insert_room_generation(
        &self,
        room_id: &[u8; 32],
        row: &RoomGenerationRow,
        wraps: &[([u8; 32], Vec<u8>)],
    ) -> Result<RoomGenerationInsert> {
        let room_id = *room_id;
        let row = row.clone();
        let wraps = wraps.to_vec();
        let mut conn = self.conn.lock().await;
        let now = now_epoch_millis();
        let tip = order_generations_by_chain(query_room_generations(&conn, &room_id)?)
            .pop()
            .map(|t| t.generation_id);
        let named_parent = row
            .parent_id
            .as_deref()
            .and_then(|p| <[u8; 32]>::try_from(p).ok());
        if named_parent != tip {
            return Ok(RoomGenerationInsert::NotTheTip);
        }
        let tx = conn.transaction().context("begin generation publish")?;
        tx.execute(
            "INSERT OR REPLACE INTO room_generations
                 (room_id, generation_id, parent_id, key_commitment, minted_by,
                  mint_blob, minted_at_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                room_id.as_slice(),
                row.generation_id.as_slice(),
                row.parent_id.as_deref(),
                row.key_commitment,
                row.minted_by,
                row.mint_blob,
                row.minted_at_ms,
                now,
            ],
        )
        .context("store the room generation")?;
        for (entry_id, wrap) in &wraps {
            tx.execute(
                "INSERT OR REPLACE INTO room_generation_wraps
                     (room_id, generation_id, entry_id, wrap)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    room_id.as_slice(),
                    row.generation_id.as_slice(),
                    entry_id.as_slice(),
                    wrap,
                ],
            )
            .context("store a room generation wrap")?;
        }
        tx.commit().context("commit generation publish")?;
        Ok(RoomGenerationInsert::Stored)
    }

    /// The room's generations, oldest first — the tip is the last.
    ///
    /// **Ordered by the parent chain, never by `minted_at_ms`**
    /// ([`order_generations_by_chain`]). The chain is the one ordering this
    /// plane enforces: every generation's id is a hash over its parent, and
    /// admission requires a mint to name the current tip as its parent
    /// (`community-rooms.md` § Implementation status today → *The sealing
    /// lands*: a strict ratchet — the community class's build record split
    /// out of `conversation-rooms.md` on 2026-09-10). The stamp is the minting device's wall clock
    /// — its own type calls it advisory — so two admins whose clocks disagree
    /// would otherwise order the room by their skew rather than by what
    /// replaced what, and a revoke stamped earlier than its parent would never
    /// become the tip: the reply would say the nest's read was withdrawn while
    /// the nest went on reading.
    pub async fn list_room_generations(
        &self,
        room_id: &[u8; 32],
    ) -> Result<Vec<RoomGenerationRow>> {
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        Ok(order_generations_by_chain(query_room_generations(
            &conn, &room_id,
        )?))
    }

    /// The generation new content seals under, or `None` for a room that has
    /// never been keyed.
    pub async fn room_generation_tip(
        &self,
        room_id: &[u8; 32],
    ) -> Result<Option<RoomGenerationRow>> {
        Ok(self.list_room_generations(room_id).await?.pop())
    }

    /// The roster entries holding a wrap for the room's **tip**, or `None` for
    /// a room that has never been keyed — the roster read's `tip_wrapped`
    /// answer. Entry ids only: which slots are covered, never the wraps.
    pub async fn room_tip_wrapped_entries(
        &self,
        room_id: &[u8; 32],
    ) -> Result<Option<std::collections::BTreeSet<Vec<u8>>>> {
        let Some(tip) = self.room_generation_tip(room_id).await? else {
            return Ok(None);
        };
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT entry_id FROM room_generation_wraps
                 WHERE room_id = ?1 AND generation_id = ?2",
            )
            .context("prepare tip wrap entries")?;
        let entries = stmt
            .query_map(
                rusqlite::params![room_id.as_slice(), tip.generation_id.as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .context("query tip wrap entries")?
            .collect::<rusqlite::Result<std::collections::BTreeSet<_>>>()
            .context("collect tip wrap entries")?;
        Ok(Some(entries))
    }

    /// One wrap, by `(generation, entry)` — how the home nest opens its own,
    /// and how a member reads back exactly its own and nothing else.
    pub async fn get_room_generation_wrap(
        &self,
        room_id: &[u8; 32],
        generation_id: &[u8; 32],
        entry_id: &[u8; 32],
    ) -> Result<Option<Vec<u8>>> {
        let room_id = *room_id;
        let generation_id = *generation_id;
        let entry_id = *entry_id;
        let conn = self.conn.lock().await;
        let wrap: Option<Vec<u8>> = conn
            .query_row(
                "SELECT wrap FROM room_generation_wraps
                 WHERE room_id = ?1 AND generation_id = ?2 AND entry_id = ?3",
                rusqlite::params![
                    room_id.as_slice(),
                    generation_id.as_slice(),
                    entry_id.as_slice()
                ],
                |row| row.get(0),
            )
            .optional()
            .context("get_room_generation_wrap")?;
        Ok(wrap)
    }

    /// Store top-up wraps for **one** roster entry over generations that
    /// already exist — the recipient-set scheme's archival backfill, which is
    /// what an ADD does instead of minting (`account-data-taxonomy.md` § The
    /// recipient-set scheme → *Mint triggers*).
    ///
    /// Deliberately cannot reach `room_generations`: a backfill covers a new
    /// member, it never changes which key the room seals under. Every wrap is
    /// authorized by the handler before it gets here, and the batch is one
    /// transaction so a partial bundle is never observable.
    ///
    /// **Adds coverage, never replaces it.** A `(room_id, generation_id,
    /// entry_id)` row already stored is left exactly as it is — there is no
    /// sanctioned path that re-seals a wrap already at an entry
    /// (`community-rooms.md` § Implementation status today → *A seat gains or
    /// rotates its wrap target*, rule (b): "the wraps already stored at the
    /// entry stay openable and nothing is re-sealed"). Returns how many rows
    /// were actually inserted, which can be less than `wraps.len()` when some
    /// of the batch was already covered.
    ///
    /// `wraps` is `(generation_id, wrap bytes)`.
    pub async fn backfill_room_generation_wraps(
        &self,
        room_id: &[u8; 32],
        entry_id: &[u8; 32],
        wraps: &[([u8; 32], Vec<u8>)],
    ) -> Result<usize> {
        let room_id = *room_id;
        let entry_id = *entry_id;
        let wraps = wraps.to_vec();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin generation backfill")?;
        let mut stored = 0usize;
        for (generation_id, wrap) in &wraps {
            tx.execute(
                "INSERT INTO room_generation_wraps
                     (room_id, generation_id, entry_id, wrap)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (room_id, generation_id, entry_id) DO NOTHING",
                rusqlite::params![
                    room_id.as_slice(),
                    generation_id.as_slice(),
                    entry_id.as_slice(),
                    wrap,
                ],
            )
            .context("store a backfilled room generation wrap")?;
            stored += tx.changes() as usize;
        }
        tx.commit().context("commit generation backfill")?;
        Ok(stored)
    }

    /// The FTS `schema` value a room's derived views are indexed under.
    ///
    /// One class per room, deliberately: the materialization grant's revoke
    /// is "delete this room's derived views and nothing else"
    /// (`conversation-rooms.md` § The three classes → *Community*;
    /// `principles.md` § The user always controls their data), and a per-room
    /// class makes that the existing one-statement purge the per-bridge
    /// policy already uses rather than a bespoke scan.
    pub fn room_view_schema(room_id: &[u8; 32]) -> String {
        format!("room-message:{}", hex::encode(room_id))
    }

    /// The FTS `schema` value the room-restricted **posts** addressed to a
    /// room are indexed under — a sibling of [`Self::room_view_schema`], still
    /// one class per room, for the same revoke.
    ///
    /// Apart from the message class rather than inside it so a caller that
    /// asks only for messages (every caller built before post hits existed)
    /// gets exactly the page it always got: in one shared class a post match
    /// would take a slot of the top-N window and then be dropped, silently
    /// shortening the page.
    pub fn room_post_view_schema(room_id: &[u8; 32]) -> String {
        format!("room-post:{}", hex::encode(room_id))
    }

    /// Delete every derived view the nest built from this room's sealed log —
    /// the materialization grant's **revoke**, run when a generation mint
    /// drops the home nest's wrap or the floor removes it.
    ///
    /// Returns how many rows went, so the caller can report what the revoke
    /// actually did rather than assuming.
    pub async fn purge_room_derived_views(&self, room_id: &[u8; 32]) -> Result<usize> {
        let schema = Self::room_view_schema(room_id);
        let post_schema = Self::room_post_view_schema(room_id);
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        let purged = super::search::purge_schema(&conn, &schema)?
            + super::search::purge_schema(&conn, &post_schema)?;
        // The seq map goes in the SAME act. It carries no text, but it is a
        // derived view all the same — it says which positions in the room's
        // log the nest was able to read — and the revoke is "delete this
        // room's derived views", not "delete the searchable half of them".
        conn.execute(
            "DELETE FROM room_message_views WHERE room_id = ?1",
            rusqlite::params![room_id.as_slice()],
        )
        .context("purge a room's message-view map")?;
        // ...and the post-id map, for the same reason: the room-restricted
        // posts the nest opened at reception are this room's derived views too
        // (their FTS rows went with the post class above).
        conn.execute(
            "DELETE FROM room_post_views WHERE room_id = ?1",
            rusqlite::params![room_id.as_slice()],
        )
        .context("purge a room's post-view map")?;
        // And the label plane: the category verdicts and factor rows the
        // room's named labelers derived (`super::room_labels`). The signed
        // labeler SET stays — it is the room's choice, not something the nest
        // read — so a nest the members rotate back in resumes under it.
        let labels = super::room_labels::purge_room_bus(&conn, &room_id)?;
        Ok(purged + labels)
    }

    /// Record that the room-restricted post `post_id` addressed to this room
    /// is in the derived corpus, under the FTS document key the reception
    /// pass wrote it as — the post twin of [`Self::record_room_message_view`].
    ///
    /// Idempotent on `(room_id, post_id)`: a re-index replaces the FTS row,
    /// so the map replaces its own row too.
    pub async fn record_room_post_view(
        &self,
        room_id: &[u8; 32],
        post_id: &[u8; 32],
        doc_key: &[u8; 32],
        generation_id: &[u8; 32],
    ) -> Result<()> {
        let room_id = *room_id;
        let post_id = *post_id;
        let doc_key = *doc_key;
        let generation_id = *generation_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO room_post_views (room_id, post_id, doc_key, generation_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(room_id, post_id) DO UPDATE SET
                 doc_key = excluded.doc_key,
                 generation_id = excluded.generation_id",
            rusqlite::params![
                room_id.as_slice(),
                post_id.as_slice(),
                doc_key.as_slice(),
                generation_id.as_slice()
            ],
        )
        .context("record a room post view")?;
        Ok(())
    }

    /// How many post-view rows this room's derived view holds — the half a
    /// search cannot see, for the same reason as
    /// [`Self::count_room_message_views`].
    pub async fn count_room_post_views(&self, room_id: &[u8; 32]) -> Result<usize> {
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM room_post_views WHERE room_id = ?1",
                rusqlite::params![room_id.as_slice()],
                |r| r.get(0),
            )
            .context("count a room's post views")?;
        Ok(n as usize)
    }

    /// Resolve FTS document keys back to the room-restricted posts they came
    /// from **for one roster entry**, preserving the caller's (relevance)
    /// order. A key with no row is `None` — a message hit, or a map that
    /// disagrees with the corpus; the door names neither as a post.
    ///
    /// ⚠ Wrap-bounded exactly as [`Self::room_message_seqs_for_docs`] is, and
    /// for the same reason: a room post seals under the room's generation, so
    /// a post the entry holds no wrap for is a post it is not told about.
    ///
    /// **Moderation-bounded too** ([`ROOM_POST_VIEW_MODERATION`]): a post a
    /// moderation flag withholds from a non-author audience yields no hit, so a
    /// legal takedown's withhold reaches the derived view at its serve path
    /// (`moderation.md` § Legal takedown → *The blob-serve door* → *What the
    /// withhold binds on owner- and admin-scoped routes*, path 4). The view row
    /// stays — tombstone-not-delete — so an overturn re-serves it with nothing
    /// re-derived.
    pub async fn room_post_ids_for_docs(
        &self,
        room_id: &[u8; 32],
        doc_keys: &[Vec<u8>],
        entry_id: &[u8; 32],
    ) -> Result<Vec<Option<[u8; 32]>>> {
        let room_id = *room_id;
        let entry_id = *entry_id;
        let doc_keys: Vec<Vec<u8>> = doc_keys.to_vec();
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT v.post_id FROM room_post_views v
              LEFT JOIN content_meta cm ON cm.content_id = v.post_id
              WHERE v.room_id = ?1 AND v.doc_key = ?2
                AND v.generation_id IS NOT NULL
                AND EXISTS (SELECT 1 FROM room_generation_wraps w
                             WHERE w.room_id = v.room_id
                               AND w.generation_id = v.generation_id
                               AND w.entry_id = ?3)
                AND {ROOM_POST_VIEW_MODERATION}"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare room post view lookup")?;
        let mut out = Vec::with_capacity(doc_keys.len());
        for key in &doc_keys {
            let id: Option<Vec<u8>> = stmt
                .query_row(
                    rusqlite::params![room_id.as_slice(), key.as_slice(), entry_id.as_slice()],
                    |r| r.get(0),
                )
                .optional()
                .context("look up a room post view")?;
            out.push(id.and_then(|id| <[u8; 32]>::try_from(id.as_slice()).ok()));
        }
        Ok(out)
    }

    /// Delete every derived view built from the post `post_id` — its FTS rows
    /// and its map rows, in whichever room indexed it. A deleted post must not
    /// outlive its derivations (`ui/feed.md` § Post deletion → *Propagation*),
    /// and a search hit naming a post that no longer exists is exactly that.
    /// Returns how many views went.
    pub async fn purge_room_post_views_for_post(&self, post_id: &[u8; 32]) -> Result<usize> {
        let post_id = *post_id;
        let conn = self.conn.lock().await;
        let views: Vec<(Vec<u8>, Vec<u8>)> = {
            let mut stmt = conn
                .prepare("SELECT room_id, doc_key FROM room_post_views WHERE post_id = ?1")
                .context("prepare a post's room views")?;
            stmt.query_map(rusqlite::params![post_id.as_slice()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .context("list a post's room views")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect a post's room views")?
        };
        let mut purged = 0;
        for (room_id, key) in &views {
            if let Ok(key) = <[u8; 32]>::try_from(key.as_slice()) {
                super::search::remove_content(&conn, &key)?;
                purged += 1;
            }
            // The verdicts the room's labelers derived for it are this post's
            // derivations as much as its FTS row — read off the map BEFORE the
            // map row goes, because the bus id is `(room, post)` and the room
            // is what the map remembers (`room_labels` § The content id).
            if let Ok(room_id) = <[u8; 32]>::try_from(room_id.as_slice()) {
                purged += super::room_labels::purge_room_post_bus(&conn, &room_id, &post_id)?;
            }
        }
        conn.execute(
            "DELETE FROM room_post_views WHERE post_id = ?1",
            rusqlite::params![post_id.as_slice()],
        )
        .context("purge a post's room views")?;
        Ok(purged)
    }

    /// Which rooms indexed which of `post_ids` at reception — the `(room,
    /// post)` pairs of the map, in no particular order. What
    /// `fauna.posts.room_labels` asks first: a caller names posts, and the
    /// floor that gates each verdict is the room that derived it.
    ///
    /// Moderation-bounded like [`Self::room_post_ids_for_docs`], and for the
    /// same reason: a verdict is derived from the body, so a post a flag
    /// withholds from a non-author audience is reported in no room — the
    /// verdict read then serves nothing for it, exactly as for a post nobody
    /// labelled. The map row stays for the overturn. (The post's DELETE still
    /// finds every view through its own unconditional walk in
    /// [`Self::purge_room_post_views_for_post`], flag or no flag.)
    pub async fn rooms_indexing_posts(
        &self,
        post_ids: &[[u8; 32]],
    ) -> Result<Vec<([u8; 32], [u8; 32])>> {
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT v.room_id FROM room_post_views v
              LEFT JOIN content_meta cm ON cm.content_id = v.post_id
              WHERE v.post_id = ?1
                AND {ROOM_POST_VIEW_MODERATION}"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare the post → room lookup")?;
        let mut out = Vec::new();
        for post_id in post_ids {
            let rooms = stmt
                .query_map(rusqlite::params![post_id.as_slice()], |r| {
                    r.get::<_, Vec<u8>>(0)
                })
                .context("look up a post's rooms")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect a post's rooms")?;
            for room in rooms {
                if let Ok(room) = <[u8; 32]>::try_from(room.as_slice()) {
                    out.push((room, *post_id));
                }
            }
        }
        Ok(out)
    }

    /// Record that this room's message at `seq` is in the derived corpus,
    /// under the FTS document key the indexer wrote it as.
    ///
    /// Idempotent on `(room_id, seq)`: a re-index replaces the FTS row, so
    /// the map replaces its own row too rather than growing a second one.
    pub async fn record_room_message_view(
        &self,
        room_id: &[u8; 32],
        seq: i64,
        doc_key: &[u8; 32],
        generation_id: &[u8; 32],
    ) -> Result<()> {
        let room_id = *room_id;
        let doc_key = *doc_key;
        let generation_id = *generation_id;
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO room_message_views (room_id, seq, doc_key, generation_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(room_id, seq) DO UPDATE SET
                 doc_key = excluded.doc_key,
                 generation_id = excluded.generation_id",
            rusqlite::params![
                room_id.as_slice(),
                seq,
                doc_key.as_slice(),
                generation_id.as_slice()
            ],
        )
        .context("record a room message view")?;
        Ok(())
    }

    /// How many message-view rows this room's derived view holds.
    ///
    /// The half of the view a search query cannot see: a caller reads the
    /// corpus through hits, so nothing else can tell whether the map behind
    /// them survived a revoke that was supposed to take it.
    pub async fn count_room_message_views(&self, room_id: &[u8; 32]) -> Result<usize> {
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM room_message_views WHERE room_id = ?1",
                rusqlite::params![room_id.as_slice()],
                |r| r.get(0),
            )
            .context("count a room's message views")?;
        Ok(n as usize)
    }

    /// Resolve FTS document keys back to the room log positions they came
    /// from **for one roster entry**, preserving the caller's order — which is
    /// relevance order, so the door must not re-sort.
    ///
    /// A key with no row is **dropped, not zero-filled**: it means the map
    /// and the corpus disagree, and the only honest answer about a message
    /// the nest cannot name is to not name it.
    ///
    /// ⚠ **`entry_id` is the reader, and the lookup is where a position the
    /// reader could not open is dropped**. The map row records which
    /// generation the reception pass indexed the message under, and this join
    /// keeps only the rows that entry holds a wrap for
    /// (`../../../docs/goal/behavior/community-rooms.md` § The three classes →
    /// *The door answers where, never what*: "a hit is served only for a
    /// position the caller could open"). The drop lives here rather than in
    /// the handler so the door cannot hold a seq it may not serve — the same
    /// shape as [`Self::get_room_generation_wrap`] serving only the reader's
    /// own wrap.
    pub async fn room_message_seqs_for_docs(
        &self,
        room_id: &[u8; 32],
        doc_keys: &[Vec<u8>],
        entry_id: &[u8; 32],
    ) -> Result<Vec<Option<i64>>> {
        let room_id = *room_id;
        let entry_id = *entry_id;
        let doc_keys: Vec<Vec<u8>> = doc_keys.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT v.seq FROM room_message_views v
                  WHERE v.room_id = ?1 AND v.doc_key = ?2
                    AND EXISTS (SELECT 1 FROM room_generation_wraps w
                                 WHERE w.room_id = v.room_id
                                   AND w.generation_id = v.generation_id
                                   AND w.entry_id = ?3)",
            )
            .context("prepare room message view lookup")?;
        let mut out = Vec::with_capacity(doc_keys.len());
        for key in &doc_keys {
            out.push(
                stmt.query_row(
                    rusqlite::params![room_id.as_slice(), key.as_slice(), entry_id.as_slice()],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
                .context("look up a room message view")?,
            );
        }
        Ok(out)
    }
}

/// Every stored generation of `room_id`, in no particular order — the one read
/// both the chain order ([`CacheDb::list_room_generations`]) and the publish's
/// in-lock tip check ([`CacheDb::insert_room_generation`]) start from.
fn query_room_generations(
    conn: &rusqlite::Connection,
    room_id: &[u8; 32],
) -> Result<Vec<RoomGenerationRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT generation_id, parent_id, key_commitment, minted_by, mint_blob,
                    minted_at_ms
             FROM room_generations WHERE room_id = ?1",
        )
        .context("prepare generation list")?;
    let rows = stmt
        .query_map(rusqlite::params![room_id.as_slice()], |row| {
            let id: Vec<u8> = row.get(0)?;
            Ok(RoomGenerationRow {
                generation_id: blob_col_to_array(id, 0, "generation_id")?,
                parent_id: row.get(1)?,
                key_commitment: row.get(2)?,
                minted_by: row.get(3)?,
                mint_blob: row.get(4)?,
                minted_at_ms: row.get(5)?,
            })
        })
        .context("query generations")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collect generations")?;
    Ok(rows)
}

/// Order a room's generations **along the parent chain**: the root (the one
/// generation naming no parent) first, each generation followed by the one
/// that names it as parent, and the **tip last**.
///
/// The chain is the only order this plane enforces — a generation's id is a
/// hash over its parent, so the edge cannot be rewritten, and admission
/// requires every mint to name the current tip. `minted_at_ms` is never
/// consulted for order: it is the minting device's wall clock and its own type
/// calls it advisory.
///
/// **A fork is resolved deterministically, and cannot be created any more.**
/// Admission reads the tip from this very function and the publish re-checks
/// it under the same lock it inserts under ([`CacheDb::insert_room_generation`]),
/// so no new generation can become a second child. One could exist only from
/// before this ordering (when a mint stamped earlier than its parent left the
/// old tip in place and let a sibling in). Such a room follows its **deepest**
/// branch — the one the room went on extending — ties broken by the lowest
/// generation id, so every reader agrees; the fork is logged, since this plane
/// has no arbiter to resolve one and a person should know it exists.
/// Generations off the followed branch are kept, ordered before the tip, so a
/// member still gets the wrap for content sealed under them.
///
/// Pure over its input, so the same rows order the same way on every read.
pub(crate) fn order_generations_by_chain(rows: Vec<RoomGenerationRow>) -> Vec<RoomGenerationRow> {
    use std::collections::{BTreeMap, BTreeSet};

    let parent_of = |r: &RoomGenerationRow| -> Option<[u8; 32]> {
        r.parent_id
            .as_deref()
            .and_then(|p| <[u8; 32]>::try_from(p).ok())
    };
    // parent → children, children sorted by id so every walk is deterministic.
    let mut children: BTreeMap<Option<[u8; 32]>, Vec<[u8; 32]>> = BTreeMap::new();
    let mut by_id: BTreeMap<[u8; 32], RoomGenerationRow> = BTreeMap::new();
    for r in rows {
        children
            .entry(parent_of(&r))
            .or_default()
            .push(r.generation_id);
        by_id.insert(r.generation_id, r);
    }
    for kids in children.values_mut() {
        kids.sort();
        kids.dedup();
    }

    // Depth of the deepest chain under each generation, iteratively, guarding
    // against a cycle (impossible by construction — an id hashes its parent —
    // but a walk over stored rows must not trust that to terminate).
    fn depth(
        id: [u8; 32],
        children: &BTreeMap<Option<[u8; 32]>, Vec<[u8; 32]>>,
        memo: &mut BTreeMap<[u8; 32], usize>,
        on_path: &mut BTreeSet<[u8; 32]>,
    ) -> usize {
        if let Some(d) = memo.get(&id) {
            return *d;
        }
        if !on_path.insert(id) {
            return 0;
        }
        let d = 1 + children
            .get(&Some(id))
            .map(|kids| {
                kids.iter()
                    .map(|k| depth(*k, children, memo, on_path))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        on_path.remove(&id);
        memo.insert(id, d);
        d
    }

    let mut memo = BTreeMap::new();
    let mut chain: Vec<[u8; 32]> = Vec::new();
    let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
    let mut cursor: Option<[u8; 32]> = None;
    while let Some(kids) = children.get(&cursor) {
        let next = if kids.len() == 1 {
            kids[0]
        } else {
            // A fork: the deepest branch, then the lowest id (kids are sorted,
            // and `max_by_key` keeps the LAST of equals — so reverse first).
            let pick = kids
                .iter()
                .rev()
                .max_by_key(|k| depth(**k, &children, &mut memo, &mut BTreeSet::new()))
                .copied()
                .expect("a fork has children");
            tracing::warn!(
                parent = ?cursor.map(hex::encode),
                children = kids.len(),
                followed = %hex::encode(pick),
                "a room's generation chain forks — following the deepest branch; \
                 this plane has no arbiter, so the fork is a standing anomaly"
            );
            pick
        };
        if !seen.insert(next) {
            break; // a cycle in stored rows: stop rather than loop
        }
        chain.push(next);
        cursor = Some(next);
    }

    // The tip comes out first, so nothing below can reorder it off the end.
    let tip_row = chain.pop().and_then(|id| by_id.remove(&id));
    let mut ordered: Vec<RoomGenerationRow> =
        chain.iter().filter_map(|id| by_id.remove(id)).collect();
    // Off-chain generations (a fork's other branch, or rows no root
    // reaches): kept, before the tip, in a stable order among themselves.
    let mut rest: Vec<RoomGenerationRow> = by_id.into_values().collect();
    rest.sort_by_key(|r| (r.minted_at_ms, r.generation_id));
    ordered.extend(rest);
    ordered.extend(tip_row);
    ordered
}

#[cfg(test)]
mod tests {
    use super::ReportedMember;
    use crate::db::CacheDb;

    // ── The generation chain's order ───────────────────────────────────

    /// A generation row with only what ordering reads.
    fn generation(id: u8, parent: Option<u8>, stamp: i64) -> super::RoomGenerationRow {
        super::RoomGenerationRow {
            generation_id: [id; 32],
            parent_id: parent.map(|p| vec![p; 32]),
            key_commitment: Vec::new(),
            minted_by: Vec::new(),
            mint_blob: Vec::new(),
            minted_at_ms: stamp,
        }
    }

    fn ids(rows: &[super::RoomGenerationRow]) -> Vec<u8> {
        rows.iter().map(|r| r.generation_id[0]).collect()
    }

    /// **The chain orders a room, the stamp never does.** Stamps here run
    /// exactly backwards against the chain — the shape two admins with skewed
    /// clocks produce — and the order is still root to tip. Ordered by stamp,
    /// the tip would be the ROOT, and every rotation after it would read as
    /// never having happened.
    #[test]
    fn the_chain_orders_the_generations_whatever_their_stamps_say() {
        let rows = vec![
            generation(3, Some(2), 1_000),
            generation(1, None, 3_000),
            generation(2, Some(1), 2_000),
        ];
        assert_eq!(ids(&super::order_generations_by_chain(rows)), vec![1, 2, 3]);
    }

    /// **A fork follows its deepest branch; a tie takes the lowest id; the
    /// other branch is kept, before the tip.** Forks can no longer be
    /// created — the publish re-checks the tip under its own lock — but a room
    /// forked before the chain ordering must still resolve the same way on
    /// every reader, and a member must still find the wrap for content sealed
    /// under the branch not followed.
    #[test]
    fn a_fork_follows_the_deepest_branch_and_keeps_the_other_before_the_tip() {
        // 1 ← 2 ← 4   and   1 ← 3 : the deeper branch (via 2) is followed.
        let rows = vec![
            generation(1, None, 1_000),
            generation(2, Some(1), 5_000),
            generation(3, Some(1), 9_000),
            generation(4, Some(2), 2_000),
        ];
        let ordered = super::order_generations_by_chain(rows);
        assert_eq!(
            ordered.last().unwrap().generation_id,
            [4; 32],
            "the deepest branch's end is the tip, though 3 carries the latest stamp"
        );
        assert_eq!(
            ids(&ordered),
            vec![1, 2, 3, 4],
            "the chain 1, 2, then the off-branch 3, then the tip"
        );

        // 1 ← 2   and   1 ← 3 : equal depth, so the lowest id.
        let tied = vec![
            generation(1, None, 1_000),
            generation(3, Some(1), 1_000),
            generation(2, Some(1), 9_000),
        ];
        assert_eq!(
            super::order_generations_by_chain(tied)
                .last()
                .unwrap()
                .generation_id,
            [2; 32],
            "a tie resolves by id, never by stamp, so every reader agrees"
        );
    }

    /// Nothing stored is nothing ordered; and rows no root reaches are kept,
    /// before the tip, without the walk entering them.
    ///
    /// With one parent per row, a cycle can only exist in a component that
    /// has NO root — every node in it names a parent — so the walk from the
    /// root never meets one. Such rows (not constructible: an id hashes its
    /// parent) and orphans whose parent was never stored are the same case
    /// to the order: off the chain, still listed, never the tip.
    #[test]
    fn an_empty_room_has_no_tip_and_rows_no_root_reaches_are_kept_before_it() {
        assert!(super::order_generations_by_chain(Vec::new()).is_empty());
        let rows = vec![
            generation(1, None, 1_000),
            generation(2, Some(1), 2_000),
            // A rootless cycle: 7 and 8 each name the other.
            generation(7, Some(8), 500),
            generation(8, Some(7), 600),
            // An orphan: its parent 9 was never stored.
            generation(5, Some(9), 700),
        ];
        let ordered = super::order_generations_by_chain(rows);
        assert_eq!(
            ordered.last().unwrap().generation_id,
            [2; 32],
            "the tip is the end of the rooted chain"
        );
        assert_eq!(
            ids(&ordered),
            vec![1, 7, 8, 5, 2],
            "the chain, then everything it does not reach (by stamp), then the tip"
        );
    }

    /// **The ratchet is re-checked under the insert's own lock** — the witness
    /// for the race the handler's check alone could not close.
    ///
    /// The handler reads the tip and refuses a mint that does not name it,
    /// but that is a read followed by a write: two rotations admitted against
    /// the same tip before either stored would both land, as two children of
    /// it. This drives the store directly, as the second of those two would
    /// arrive — naming a parent that stopped being the tip after it was
    /// admitted — and the store refuses it and stores nothing.
    #[tokio::test]
    async fn a_generation_whose_parent_is_no_longer_the_tip_is_not_stored() {
        let db = CacheDb::open_in_memory().unwrap();
        let room = [9u8; 32];
        let owner = [1u8; 32];
        db.found_room(
            &room,
            "community",
            &owner,
            1,
            b"founding",
            &[3u8; 32],
            &[founder(owner, "user", "owner")],
        )
        .await
        .expect("the ceremony founds the room");

        let first = generation(1, None, 1_000);
        let second = generation(2, Some(1), 2_000);
        let racer = generation(3, Some(1), 3_000);
        for row in [&first, &second] {
            assert_eq!(
                db.insert_room_generation(&room, row, &[]).await.unwrap(),
                super::RoomGenerationInsert::Stored
            );
        }
        assert_eq!(
            db.insert_room_generation(&room, &racer, &[]).await.unwrap(),
            super::RoomGenerationInsert::NotTheTip,
            "its parent (1) was the tip when it was admitted, but (2) landed first"
        );
        assert_eq!(
            ids(&db.list_room_generations(&room).await.unwrap()),
            vec![1, 2],
            "nothing stored — the chain is still 1 ← 2, no fork"
        );
    }

    /// ⚠ **A retained policy version is history: an identical replay
    /// converges, a DIFFERING blob at the same number is refused.**
    ///
    /// The whole point of the table is that a member can ask "what did the
    /// policy say at version *N*" and get the answer every record made under
    /// *N* was judged against (`conversation-rooms.md` § Roles and
    /// authorization → *Members verify what they paint*). An upsert that
    /// replaced the bytes would let that answer change under records already
    /// filed. The founding door is the one writer that can reach the conflict
    /// at all — both others compare-and-set on `expected_version` — so it is
    /// the door this drives.
    #[tokio::test]
    async fn a_retained_policy_version_converges_on_a_replay_and_refuses_different_bytes() {
        let db = CacheDb::open_in_memory().unwrap();
        let room = [9u8; 32];
        let owner = [1u8; 32];
        let salt = [3u8; 32];
        let members = [founder(owner, "user", "owner")];

        db.found_room(&room, "community", &owner, 1, b"founding", &salt, &members)
            .await
            .expect("the ceremony founds the room");
        // The identical birth record again — a recovered connection's replay,
        // which has done nothing improper and must converge.
        db.found_room(&room, "community", &owner, 1, b"founding", &salt, &members)
            .await
            .expect("an identical replay converges");
        assert_eq!(
            db.get_room_policy_version(&room, 1).await.unwrap().unwrap(),
            b"founding".to_vec()
        );

        // Different bytes at the number already held: refused, and the stored
        // history is untouched.
        db.found_room(&room, "community", &owner, 1, b"rewritten", &salt, &members)
            .await
            .expect_err("a retained version is never rewritten");
        assert_eq!(
            db.get_room_policy_version(&room, 1).await.unwrap().unwrap(),
            b"founding".to_vec(),
            "the version every record under it was judged against still stands"
        );
        // …and the refusal rolled back the policy move that carried it, rather
        // than leaving the room pointing at a version the history disowns.
        let room_row = db.get_room(&room).await.unwrap().expect("the room exists");
        assert_eq!(room_row.policy_version, Some(1));
        assert_eq!(
            db.get_room_policy_blob(&room).await.unwrap().unwrap(),
            b"founding".to_vec(),
            "the floor's current answer was rolled back with the history write"
        );
    }

    fn founder(id: [u8; 32], kind: &str, role: &str) -> ReportedMember {
        ReportedMember {
            principal_id: id,
            principal_kind: kind.into(),
            role: Some(role.into()),
            home_node_url: String::new(),
            reception_pubkey: Vec::new(),
        }
    }

    /// A replay of the birth record after the room's policy has MOVED ON is
    /// a no-op, never a rollback.
    ///
    /// The owner's original birth record stays valid forever — correctly
    /// signed, naming the owner, sent by that owner — so every gate in
    /// `room_create_handler` passes on a replay and the call reaches this
    /// layer. Without the guard the unconditional upsert would roll
    /// `policy_version` and `policy_blob` back to the founding version,
    /// undoing a tightened join rule or a removed admin, and would re-seat
    /// the founding rows by clearing their `removed_at`.
    ///
    /// `set_policy` is not built yet, so the raised version is written
    /// directly here. That is the point: the defect is in the CREATE door,
    /// and pinning it now is what keeps the replay-safety claim in `kind.rs`
    /// from expiring silently when `set_policy` lands and nobody re-reads
    /// this door.
    #[tokio::test]
    async fn a_birth_record_replayed_after_the_policy_moved_on_rolls_nothing_back() {
        let db = CacheDb::open_in_memory().unwrap();
        let room = [9u8; 32];
        let owner = [1u8; 32];
        let nest = [2u8; 32];
        let salt = [3u8; 32];
        let members = [
            founder(owner, "user", "owner"),
            founder(nest, "nest", "member"),
        ];

        assert!(
            db.found_room(&room, "community", &owner, 1, b"founding", &salt, &members)
                .await
                .expect("the ceremony founds the room")
        );
        db.found_room(&room, "community", &owner, 1, b"founding", &salt, &members)
            .await
            .expect("a replay while still at version 1 is the ordinary no-op");

        // The room's governance moves on: a member is seated, then removed,
        // and the policy is ratcheted past its founding version.
        let joiner = [4u8; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO room_members (room_id, principal_id, principal_kind, role,
                                           home_node_url, joined_at, reported_at, removed_at)
                 VALUES (?1, ?2, 'user', 'member', '', 1, 1, NULL)",
                rusqlite::params![room.as_slice(), joiner.as_slice()],
            )
            .unwrap();
            conn.execute(
                "UPDATE rooms SET policy_version = 7, policy_blob = ?2 WHERE room_id = ?1",
                rusqlite::params![room.as_slice(), b"ratcheted".as_slice()],
            )
            .unwrap();
        }
        db.unseat_room_member(&room, &joiner)
            .await
            .expect("the room removes the joiner");

        // The replay: byte-identical, still valid, still the owner's.
        assert!(
            db.found_room(&room, "community", &owner, 1, b"founding", &salt, &members)
                .await
                .expect("a replay is not an error — the room exists and is the caller's"),
            "the ceremony's post-condition still holds"
        );

        let rec = db.get_room(&room).await.unwrap().expect("the room record");
        assert_eq!(
            rec.policy_version,
            Some(7),
            "the replay did not roll the ratcheted policy version back to 1"
        );
        assert!(
            !db.is_room_member(&room, &joiner).await.unwrap(),
            "and it did not re-seat a principal the room had removed"
        );
    }

    /// A replay by a principal that is not the room's owner is refused
    /// outright — the case that would be an escalation rather than a ratchet
    /// break, and the reason a *former* owner cannot take a room back once
    /// `transfer_ownership` moves the row.
    #[tokio::test]
    async fn a_birth_record_cannot_re_found_another_principals_room() {
        let db = CacheDb::open_in_memory().unwrap();
        let room = [9u8; 32];
        let owner = [1u8; 32];
        let usurper = [5u8; 32];
        let salt = [3u8; 32];

        db.found_room(
            &room,
            "community",
            &owner,
            1,
            b"founding",
            &salt,
            &[founder(owner, "user", "owner")],
        )
        .await
        .unwrap();

        assert!(
            !db.found_room(
                &room,
                "community",
                &usurper,
                1,
                b"forged",
                &salt,
                &[founder(usurper, "user", "owner")],
            )
            .await
            .expect("the refusal is a verdict, not an error"),
            "a room id already founded by another principal is not re-foundable"
        );
        let rec = db.get_room(&room).await.unwrap().expect("the room record");
        assert_eq!(rec.owner_id.as_deref(), Some(&owner[..]));
    }

    // ── A seat's own wrap target ───────────────────────────────────────

    /// **A seat with no roster entry gains one with its first key.** Such a row
    /// has neither column; the door mints the entry the seat will be wrapped
    /// at, so the member is coverable after one call.
    #[tokio::test]
    async fn a_seat_with_no_entry_gains_one_with_its_first_key() {
        let db = CacheDb::open_in_memory().unwrap();
        let room = [9u8; 32];
        let owner = [1u8; 32];
        let nest = [2u8; 32];
        let salt = [3u8; 32];
        let members = [
            founder(owner, "user", "owner"),
            founder(nest, "nest", "member"),
        ];
        db.found_room(&room, "community", &owner, 1, b"founding", &salt, &members)
            .await
            .unwrap();
        let elder = [4u8; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO room_members (room_id, principal_id, principal_kind, role,
                                           home_node_url, joined_at, reported_at, removed_at)
                 VALUES (?1, ?2, 'user', 'member', '', 1, 1, NULL)",
                rusqlite::params![room.as_slice(), elder.as_slice()],
            )
            .unwrap();
        }
        let key = vec![0xA5u8; 16];
        let bound = db
            .set_room_member_reception_key(&room, &elder, &key)
            .await
            .unwrap()
            .expect("a live user seat");
        assert!(!bound.rotated, "a first key is not a rotation");
        let row = db
            .list_floor_roster(&room)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.principal_id == elder)
            .expect("still seated");
        assert_eq!(
            row.entry_id.as_deref(),
            Some(bound.entry_id.as_slice()),
            "the entry minted with the key is the one the floor serves"
        );
        assert_eq!(row.reception_pubkey.as_deref(), Some(key.as_slice()));
    }

    /// **A rotation keeps the entry; the bound key again changes nothing; and
    /// only a live USER seat is ever bound** — not the nest's, not a stranger's,
    /// not a `Removed`-absorbed one.
    #[tokio::test]
    async fn a_rotation_keeps_the_entry_and_only_a_live_user_seat_is_bound() {
        let db = CacheDb::open_in_memory().unwrap();
        let room = [9u8; 32];
        let owner = [1u8; 32];
        let nest = [2u8; 32];
        let salt = [3u8; 32];
        let members = [
            founder(owner, "user", "owner"),
            founder(nest, "nest", "member"),
        ];
        db.found_room(&room, "community", &owner, 1, b"founding", &salt, &members)
            .await
            .unwrap();
        let first = vec![1u8; 8];
        let second = vec![2u8; 8];

        let a = db
            .set_room_member_reception_key(&room, &owner, &first)
            .await
            .unwrap()
            .expect("the owner's own seat");
        assert!(!a.rotated);
        let b = db
            .set_room_member_reception_key(&room, &owner, &first)
            .await
            .unwrap()
            .expect("the owner's own seat");
        assert!(
            !b.rotated,
            "the key already bound is a no-op, not a rotation"
        );
        assert_eq!(b.entry_id, a.entry_id);
        let c = db
            .set_room_member_reception_key(&room, &owner, &second)
            .await
            .unwrap()
            .expect("the owner's own seat");
        assert!(c.rotated, "a different key is a rotation");
        assert_eq!(
            c.entry_id, a.entry_id,
            "on the same entry — the wraps already sealed to it stay openable"
        );

        assert!(
            db.set_room_member_reception_key(&room, &nest, &second)
                .await
                .unwrap()
                .is_none(),
            "the home nest's read is a mint-time grant, never a key it binds"
        );
        assert!(
            db.set_room_member_reception_key(&room, &[7u8; 32], &second)
                .await
                .unwrap()
                .is_none(),
            "a stranger holds no seat"
        );

        let joiner = [4u8; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO room_members (room_id, principal_id, principal_kind, role,
                                           home_node_url, joined_at, reported_at, removed_at)
                 VALUES (?1, ?2, 'user', 'member', '', 1, 1, NULL)",
                rusqlite::params![room.as_slice(), joiner.as_slice()],
            )
            .unwrap();
        }
        db.unseat_room_member(&room, &joiner).await.unwrap();
        assert!(
            db.set_room_member_reception_key(&room, &joiner, &first)
                .await
                .unwrap()
                .is_none(),
            "a Removed-absorbed seat is history, not a wrap target"
        );
    }

    /// **Account deletion takes the deleted invitee's invitations with it —
    /// pending and accepted alike — and nobody else's.**
    ///
    /// `room_invites` names its people `invitee_id` / `inviter_id`, two words
    /// the actor census had no root for, so the table sat in no registry and
    /// the purge walk never opened it: a deleted account's id rested here for
    /// good, in the clear and indexed. This drives the walk account deletion
    /// actually runs (`purge_orphaned_actor_rows`), not a hand-written DELETE,
    /// so it reds the day the table's `ACTOR_TABLES` entry goes missing.
    ///
    /// The deleted INVITER's column is deliberately not asserted empty: an
    /// invitation is the invitee's row, and who issued it is attribution that
    /// stays with it (`SUCCESSION_REFERENCES`, `room_invites.inviter_id`).
    #[tokio::test]
    async fn deleting_an_account_removes_the_invitations_addressed_to_it() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [1u8; 32];
        let leaving = [2u8; 32];
        let staying = [3u8; 32];
        let pending_room = [8u8; 32];
        let accepted_room = [9u8; 32];
        for room in [pending_room, accepted_room] {
            db.found_room(
                &room,
                "community",
                &owner,
                1,
                b"founding",
                &[3u8; 32],
                &[founder(owner, "user", "owner")],
            )
            .await
            .unwrap();
        }
        for (room, invitee) in [
            (pending_room, leaving),
            (accepted_room, leaving),
            (pending_room, staying),
        ] {
            assert!(
                db.record_room_invite_and_deliver(
                    &room,
                    &invitee,
                    &owner,
                    "member",
                    "",
                    b"signed",
                    b"envelope",
                    false,
                )
                .await
                .unwrap()
            );
        }
        assert!(
            db.accept_room_invite(&accepted_room, &leaving, &[], None)
                .await
                .unwrap()
                .seated()
        );

        db.purge_orphaned_actor_rows(&leaving).await.unwrap();

        let invitees: Vec<Vec<u8>> = {
            let conn = db.conn.lock().await;
            let mut stmt = conn
                .prepare("SELECT invitee_id FROM room_invites ORDER BY invitee_id")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            invitees,
            vec![staying.to_vec()],
            "the deleted account's pending AND accepted invitations are gone, and \
             the other invitee's stands"
        );
    }

    /// **A seating is bound to the policy version its judgement read.** The
    /// accept door judges in reads of its own, and the policy reconcile
    /// touches live seats only — so a policy landing between the two must
    /// stop the seat being written, or an admin the new policy just dropped
    /// is seated as admin. No race needed to pin it: name a version the room
    /// does not hold.
    #[tokio::test]
    async fn an_accept_judged_under_a_policy_the_room_has_moved_past_writes_nothing() {
        use super::RoomInviteAccept;
        let db = CacheDb::open_in_memory().unwrap();
        let (owner, guest, room) = ([1u8; 32], [2u8; 32], [8u8; 32]);
        db.found_room(
            &room,
            "community",
            &owner,
            1,
            b"founding",
            &[3u8; 32],
            &[founder(owner, "user", "owner")],
        )
        .await
        .unwrap();
        assert!(
            db.record_room_invite_and_deliver(
                &room,
                &guest,
                &owner,
                "member",
                "",
                b"signed",
                b"envelope",
                false,
            )
            .await
            .unwrap()
        );

        assert_eq!(
            db.accept_room_invite(&room, &guest, &[], Some(2))
                .await
                .unwrap(),
            RoomInviteAccept::PolicyMoved
        );
        assert!(!db.is_room_member(&room, &guest).await.unwrap());
        assert!(
            db.get_pending_room_invite(&room, &guest)
                .await
                .unwrap()
                .is_some(),
            "and the invitation still stands, to be judged again"
        );

        assert_eq!(
            db.accept_room_invite(&room, &guest, &[], Some(1))
                .await
                .unwrap(),
            RoomInviteAccept::Seated
        );
    }

    /// **A lapse removes the invitation that was judged, and only that one.**
    /// A re-invitation upserts over a pending row, so one landing between the
    /// judgement and the lapse is somebody else's invitation and stands; an
    /// accepted invitation is never a lapse's to touch.
    #[tokio::test]
    async fn a_lapse_consumes_only_the_pending_invitation_its_judgement_named() {
        let db = CacheDb::open_in_memory().unwrap();
        let (owner, admin, guest, room) = ([1u8; 32], [4u8; 32], [2u8; 32], [8u8; 32]);
        db.found_room(
            &room,
            "community",
            &owner,
            1,
            b"founding",
            &[3u8; 32],
            &[founder(owner, "user", "owner")],
        )
        .await
        .unwrap();
        let invite = |inviter: [u8; 32]| {
            let db = &db;
            async move {
                db.record_room_invite_and_deliver(
                    &room,
                    &guest,
                    &inviter,
                    "member",
                    "",
                    b"signed",
                    b"envelope",
                    false,
                )
                .await
                .unwrap()
            }
        };
        let standing = || async {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT COUNT(*) FROM content_links WHERE status = 'undelivered'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };

        // The judged invitation was the admin's; the owner re-invited since.
        assert!(invite(admin).await);
        assert!(invite(owner).await);
        assert!(
            !db.consume_pending_room_invite(&room, &guest, Some(&admin))
                .await
                .unwrap()
        );
        assert_eq!(standing().await, 1, "the owner's invitation stands");

        assert!(
            db.consume_pending_room_invite(&room, &guest, Some(&owner))
                .await
                .unwrap()
        );
        assert!(
            db.get_pending_room_invite(&room, &guest)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(standing().await, 0, "the envelope left with its row");

        // Accepted is settled: nothing lapses it.
        assert!(invite(owner).await);
        assert!(
            db.accept_room_invite(&room, &guest, &[], None)
                .await
                .unwrap()
                .seated()
        );
        assert!(
            !db.consume_pending_room_invite(&room, &guest, Some(&owner))
                .await
                .unwrap()
        );
        assert!(db.is_room_member(&room, &guest).await.unwrap());
    }

    /// **"Oldest first" is a total order, also within one millisecond.**
    /// `invited_at` is epoch milliseconds, so two invitations issued back to
    /// back can carry one stamp; the tie is settled by issue order, never by
    /// the invitee's id — a key nobody chose for its sort position, which would
    /// reshuffle the owner's list from one run of the same script to the next.
    #[tokio::test]
    async fn pending_invitations_list_in_issue_order_within_one_millisecond() {
        let db = CacheDb::open_in_memory().unwrap();
        let (owner, room) = ([1u8; 32], [8u8; 32]);
        // The ids sort AGAINST the order they are invited in, so an id
        // tie-break lists them backwards.
        let (first, second) = ([9u8; 32], [2u8; 32]);
        db.found_room(
            &room,
            "community",
            &owner,
            1,
            b"founding",
            &[3u8; 32],
            &[founder(owner, "user", "owner")],
        )
        .await
        .unwrap();
        for invitee in [&first, &second] {
            assert!(
                db.record_room_invite_and_deliver(
                    &room,
                    invitee,
                    &owner,
                    "member",
                    "",
                    b"signed",
                    b"envelope",
                    false,
                )
                .await
                .unwrap()
            );
        }
        // Pin both to one millisecond: what a fast box does on its own.
        db.conn
            .lock()
            .await
            .execute("UPDATE room_invites SET invited_at = 1700000000000", [])
            .unwrap();

        let listed = db.pending_invites_for_room(&room, None).await.unwrap();
        assert_eq!(
            listed.iter().map(|row| row.invitee_id).collect::<Vec<_>>(),
            vec![first, second],
        );
    }
}
