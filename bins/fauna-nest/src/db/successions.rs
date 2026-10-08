//! The **succession** store — identity-succession slice 3
//! (`docs/goal/behavior/identity-succession.md` § Enforcement on the home nest).
//!
//! Slice 2 gave the nest the RecoveryKey *chain*: which key currently
//! authorizes an identity. This module spends it. [`CacheDb::record_succession`]
//! is the "one transaction" of `identity-succession.md:66` — the single atomic
//! decision point where an account stops belonging to a stolen key and starts
//! belonging to its successor, and [`CacheDb::succession_for`] is "the row every
//! enforcement point consults".
//!
//! **This module never verifies a signature.** Authorization is
//! `SignedIdentitySuccession::verify` against the stored chain head, driven by
//! the handler; the store's job is to apply an already-authorized statement
//! atomically and to refuse the shapes that would make the ceremony a takeover
//! primitive. That split is what keeps the nest *enforcer and distributor, never
//! authorizer* (`identity-succession.md:74`) honest at the storage layer — there
//! is no code path here that could fabricate a succession.
//!
//! ## What moves and what stays
//!
//! The classification is § Re-key scope's: **rows that confer future authority
//! or access move; rows that record history or attribution stay.** Concretely,
//! inside the one transaction:
//!
//! | Row | Action | Why |
//! |---|---|---|
//! | `users` (successor) | inserted, inheriting tier / label / suspension / eviction state | authority + the account's admin-set lifecycle |
//! | `users.handle` | cleared on the old row, set on the new one (the two-step move) | the unique index admits exactly one holder |
//! | `users` (old) | **kept**, handle-less | it is what marks the retired id as *local* — the predecessor walks join on it (`local_predecessors`, the boot heal's `succession_heal_pairs`) and `record_peer_succession` refuses on it. No table declares a foreign key onto `users`, and the quota counters moved. It goes with the chain when the successor is deleted (`CacheDb::delete_user`; `account-data-plane.md` § Nest-side requirements item 1) |
//! | `users.locked_until` | **not** inherited | the thief's emergency lockout must not survive the ceremony that undoes them (`identity-succession.md:66`) |
//! | `admin_actor_ids` | moved | admin role is future authority |
//! | `capability_grants` (owner = old) | deleted | the successor re-mints from its own grant ledger (`fauna.state.succession-ledger`) |
//! | `nest_backup_keys` (owner = old) | deleted | the successor derives and grants a fresh `NestBackupKey` |
//! | `recovery_pending_replacements` (old) | deleted | succession outranks a pending replacement (`identity-succession.md:70`) |
//! | `recovery_escrow` (old) | deleted | the old kit retires with the old identity, and the blob it is sealed to must not keep serving the old seed (`identity-succession.md` § Seed escrow → *Lifecycle on the nest*) |
//!
//! Bearer sessions are revoked by the caller (`token_store::revoke_actor`) —
//! they live in memory, not in this database, so they cannot ride the
//! transaction; the handler does it immediately after the commit, and the
//! supersession consult in `auth_core` refuses the old key regardless of whether
//! any bearer outlived the sweep.

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;

use super::actor_tables::{self, ActorKey};
use super::{CacheDb, now_epoch_millis, now_epoch_secs, table_exists};
use crate::email_handlers::FilterSuccession;

/// Which `contacts` column a succession leg is re-pointing — the predecessor's
/// **own** edges (`actor_id`, the ceremony) or the edges
/// other holders keep **to** it (`peer_id`, every leg). The collapse is the
/// same statement shape under a column swap, so the side is data rather than
/// a second hand-written pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContactCollapseSide {
    /// Re-point `actor_id` = old → new; the holder key is `peer_id`.
    Actor,
    /// Re-point `peer_id` = old → new; the holder key is `actor_id`.
    Peer,
}

impl ContactCollapseSide {
    /// `(moving, holder)` column names, compile-time constants by construction.
    fn columns(self) -> (&'static str, &'static str) {
        match self {
            ContactCollapseSide::Actor => ("actor_id", "peer_id"),
            ContactCollapseSide::Peer => ("peer_id", "actor_id"),
        }
    }
}

/// Execute the registry's plain `Move` legs — **the one place the bulk re-point
/// is written down**, driven by [`actor_tables::plain_move_legs`] rather than by
/// a list typed into each path.
///
/// Returns the number of rows moved.
///
/// Three things this must get right, each of which has cost a security property
/// before:
///
/// 1. **Bind per the table's declared encoding.** The `nostr_*` family declares
///    its actor column `TEXT` holding lowercase hex; everything else binds the
///    raw 32-byte blob. SQLite applies no affinity conversion between a blob
///    operand and a TEXT column, so a mismatched parameter is **not** a type
///    error — it is rowcount 0, which reads exactly like "this account had no
///    rows there".
/// 2. **Guard on existence.** The bridge tables are created by their feature's
///    own `init_db`; on a nest built without it, an unconditional statement
///    aborts the whole transaction.
/// 3. **A bare `UPDATE`, never `OR IGNORE`.** The ceremony refuses
///    `NewAlreadyRegistered`, so the successor owns nothing a move could land on
///    (`succession-aftermath.md` § Re-key scope): the statement is total, and a
///    constraint violation here is a real bug that must abort the ceremony
///    rather than be swallowed.
fn execute_plain_moves(tx: &rusqlite::Transaction<'_>, old: &[u8], new: &[u8]) -> Result<usize> {
    execute_plain_moves_with(tx, old, new, actor_tables::COUPLED_MOVE_FAMILIES)
}

/// [`execute_plain_moves`] with the coupled-family list injected, for the same
/// reason [`execute_coupled_family_moves_with`] has the seam: the list is empty
/// in production until a plane is ruled, and the loop's family **skip** is
/// load-bearing the moment it is not. A family member moved here as well as by
/// the family arm is moved outside the deferred unit — which is the abort this
/// row exists to prevent, re-introduced by an omitted `continue`.
fn execute_plain_moves_with(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
    families: &[actor_tables::CoupledFamily],
) -> Result<usize> {
    let old_hex = hex::encode(old);
    let new_hex = hex::encode(new);
    let mut moved = 0usize;

    for entry in actor_tables::plain_move_legs() {
        // A foreign-key-coupled family cannot ride this loop: its members must
        // move inside one deferred unit. `execute_coupled_family_moves` owns
        // them.
        if families.iter().any(|f| f.tables.contains(&entry.table)) {
            continue;
        }
        if !table_exists(tx, entry.table)? {
            continue;
        }

        let (bound_old, bound_new): (&dyn rusqlite::ToSql, &dyn rusqlite::ToSql) = match entry.key {
            ActorKey::Blob => (&old, &new),
            ActorKey::Hex => (&old_hex, &new_hex),
        };

        let sql = format!(
            "UPDATE {} SET {} = ?2 WHERE {} = ?1",
            entry.table, entry.column, entry.column
        );
        moved += tx
            .execute(&sql, rusqlite::params![bound_old, bound_new])
            .with_context(|| format!("re-point {} at succession", entry.table))?;
    }
    Ok(moved)
}

/// Move the declared foreign-key-coupled families — the legs the plain loop
/// cannot execute, because a foreign key decides whether the transaction that
/// moves them **commits at all**.
///
/// Returns the number of rows moved.
///
/// **Why deferral, and why it is not a weakening.** With `PRAGMA foreign_keys =
/// ON` (`db/mod.rs`), updating a referenced column aborts the statement while
/// any child still names the old value — and both repair orders hit it: parent
/// first orphans the children, children first point at a parent that does not
/// exist yet. `PRAGMA defer_foreign_keys = ON` moves the check from each
/// statement to the `COMMIT`, so the family may be inconsistent *during* the
/// transaction and must be consistent at the end of it. Nothing is disarmed: a
/// family that half-moves still fails, just later and louder. That is why
/// `a_declared_family_moves_as_one` insists every member declares `Move(Plain)`
/// before any of them may move.
///
/// **The pragma is per transaction, and this function sets it without ever
/// clearing it — deliberately.** Clearing it mid-transaction does not restore
/// enforcement, it *discards* the violations accumulated so far, so a
/// half-moved family would commit an orphan silently (the trap is stated in
/// full at the end of this function and pinned by
/// `tests::turning_deferral_off_would_swallow_the_violation`). SQLite clears it
/// at each `COMMIT` and `ROLLBACK`, which is what bounds it to this transaction
/// on both the success and the error path
/// (`tests::deferral_does_not_outlive_its_transaction`).
///
/// The ceremony is collision-free by construction, so every statement is a
/// bare `UPDATE` and any failure is a real bug that aborts the ceremony.
fn execute_coupled_family_moves(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    execute_coupled_family_moves_with(
        tx,
        old,
        new,
        actor_tables::COUPLED_MOVE_FAMILIES,
        actor_tables::table_entry,
    )
}

/// [`execute_coupled_family_moves`] with its two registry lookups injected —
/// **the seam that lets this mechanism be tested at all.**
///
/// `COUPLED_MOVE_FAMILIES` is deliberately empty until a plane is ruled (see its
/// doc comment), so driving the real executor over the real list proves nothing.
/// Rather than let that stand as "no test possible" — the excuse that has cost
/// this project more than any bug it ever covered — the family list and the
/// registry lookup are parameters, and the tests drive **this** function over a
/// synthetic family whose foreign key is shaped exactly like the
/// `subscription_tiers` one waiting in the backlog. Production passes the real
/// pair and nothing else changes.
fn execute_coupled_family_moves_with(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
    families: &[actor_tables::CoupledFamily],
    resolve: fn(&str) -> Option<&'static actor_tables::ActorTable>,
) -> Result<usize> {
    if families.is_empty() {
        return Ok(0);
    }

    let old_hex = hex::encode(old);
    let new_hex = hex::encode(new);
    let mut moved = 0usize;

    tx.execute_batch("PRAGMA defer_foreign_keys = ON;")
        .context("defer foreign keys for the coupled-family moves")?;

    for family in families {
        for table in family.tables {
            // A table absent from this build has no rows to move and no foreign
            // key to violate — the guard is the plain loop's, for the same
            // reason (a feature's `init_db` may never have run here).
            if !table_exists(tx, table)? {
                continue;
            }
            let Some(entry) = resolve(table) else {
                // Unreachable while `a_declared_family_moves_as_one` is green:
                // a member must be in the registry to be ruled at all.
                anyhow::bail!(
                    "coupled family {} names unregistered table {table}",
                    family.name
                );
            };

            let (bound_old, bound_new): (&dyn rusqlite::ToSql, &dyn rusqlite::ToSql) =
                match entry.key {
                    ActorKey::Blob => (&old, &new),
                    ActorKey::Hex => (&old_hex, &new_hex),
                };
            let sql = format!(
                "UPDATE {} SET {} = ?2 WHERE {} = ?1",
                entry.table, entry.column, entry.column
            );
            moved += tx
                .execute(&sql, rusqlite::params![bound_old, bound_new])
                .with_context(|| {
                    format!(
                        "re-point {} at succession (coupled family {})",
                        entry.table, family.name
                    )
                })?;
        }
    }

    // ⚠ **The pragma is NOT turned off here, and turning it off is forbidden.**
    // The obvious tidy-up — restore immediate enforcement now that the families
    // have moved, so the rest of the transaction keeps its per-statement abort —
    // is the one edit that would silently void this whole mechanism.
    // `PRAGMA defer_foreign_keys = OFF` **discards the outstanding deferred
    // violations** rather than checking them: a family that half-moved then
    // commits clean, writing a permanent orphan into `nest.db` that only
    // `PRAGMA foreign_key_check` can find. That is strictly worse than the abort
    // this row set out to prevent — the abort is loud and costs the ceremony,
    // this is silent and costs referential integrity for good. Measured on
    // SQLite 3.50.6, 2026-08-13, and pinned by
    // `tests::turning_deferral_off_would_swallow_the_violation`.
    //
    // So deferral ends where SQLite ends it: at the `COMMIT` or `ROLLBACK` of
    // this transaction, both of which clear it. The cost is that a foreign-key
    // failure in a *later* leg of the same transaction now surfaces at the
    // commit rather than at its own statement — a diagnosability loss, paid
    // only on nests where a family is actually declared, and the alternative
    // was not enforcement-with-diagnostics but no enforcement at all.

    Ok(moved)
}

/// The persisted copy mode a succession **burns** on a queued forward — the
/// one mode whose row is provably a *second* copy (the original rests sealed in
/// the mailbox). Every other value — `redirect`, whose row is the only copy of
/// mail this nest already answered `250` for, and NULL, a row queued before the
/// mode was persisted — is carried, because burning is the one direction that
/// can destroy a message no other copy of exists.
fn burnable_copy_mode() -> &'static str {
    super::forward_queue::copy_mode_sql(fauna_protocol::bridge_routing::ForwardCopyMode::Copy)
}

/// Rule the **forwarding family** — the three mail tables that all answer the
/// same question, *where does a copy of this account's mail go next*, and not
/// one of which a plain move rules correctly (`succession-aftermath.md`
/// § Implementation status today, the axis bullet's forwarding-family segment).
///
/// Every tap here is armable with the **account key alone**, which is exactly
/// what a seed thief read — so each is the `account_aliases` shape: a routing
/// row that outlives the ceremony meant to end the theft.
///
/// **One function**, because "three lists claiming to be the same set" is the
/// drift this module exists to end. Each tap is **disarmed or burned before
/// its row moves**, so each predicate reads the retired identity's rows
/// exactly once.
fn rule_the_forwarding_family(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    let mut moved = 0usize;

    // (a) The settings row follows the account with the forward-all target
    //     cleared as it goes. `fauna.bridges.set_forward_all_to` is User-class
    //     and the MTA reads the value at the perimeter for every inbound
    //     message (`fetch_recipient_forward_config`), so moving the row intact
    //     hands the successor a mailbox that silently copies every future
    //     message to the thief, with no end date. Leg 2's bunker shape exactly
    //     — re-point *and* revoke — and uniform for theft and loss, like the
    //     MSEK burn. `forward_per_hour` beside it is the user's own choice and
    //     rides along.
    tx.execute(
        "UPDATE mail_account_settings SET forward_all_to = NULL WHERE actor_id = ?1",
        rusqlite::params![old],
    )
    .context("disarm the forward-all tap")?;
    moved += tx
        .execute(
            "UPDATE mail_account_settings SET actor_id = ?2 WHERE actor_id = ?1",
            rusqlite::params![old, new],
        )
        .context("move the mail settings row")?;

    // (b) Filter rules are user-authored configuration and move, except where
    //     the rule is standing authority to make this nest **emit
    //     attacker-authored content outward under the successor's recovered
    //     identity** — `forward:` (exfiltration) and `autoreply:` (outbound
    //     content: the MTA sends it DKIM-signed under the recipient's domain).
    //     Both are live — the MTA dispatches a fired `Forward` in either copy
    //     mode — and a burned rule's already-queued copies are ruled by legs
    //     (c) and (d) below.
    //
    //     Those are **deleted, never disarmed in place**: `action_from_string`
    //     falls back to `Discard` for anything it cannot parse, so a rewrite
    //     that missed would convert a forward into silent mail *destruction*.
    //
    //     ⚠ The split is ruled by CLASS, in `email_handlers::filter_succession`,
    //     which matches the action enum **exhaustively** — so a new
    //     `EmailFilterAction` variant is a compile error until someone decides
    //     whether it survives a succession. A `LIKE` list here would be one
    //     more hand-maintained inventory of the kind that let `account_aliases`
    //     sit un-ruled while it cost a security property.
    let filter_rows: Vec<(i64, String)> = {
        let mut stmt = tx
            .prepare("SELECT id, action FROM email_filters WHERE owner = ?1")
            .context("read the account's filter rules")?;
        let rows = stmt
            .query_map(rusqlite::params![old], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .context("read the account's filter rules")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("read the account's filter rules")?
    };
    for (id, action) in filter_rows {
        match crate::email_handlers::filter_succession(&action) {
            FilterSuccession::Burn => {
                tx.execute(
                    "DELETE FROM email_filters WHERE id = ?1",
                    rusqlite::params![id],
                )
                .context("burn an outward-emitting filter rule")?;
            }
            FilterSuccession::MoveDisarmed(disarmed) => {
                tx.execute(
                    "UPDATE email_filters SET action = ?2 WHERE id = ?1",
                    rusqlite::params![id, disarmed],
                )
                .context("disarm a filter rule's attacker-authored text")?;
            }
            FilterSuccession::Move => {}
        }
    }
    moved += tx
        .execute(
            "UPDATE email_filters SET owner = ?2 WHERE owner = ?1",
            rusqlite::params![old, new],
        )
        .context("move the account's remaining filter rules")?;

    // (c) Parked forwards, split by **what a burn would destroy** — and leaving
    //     them put is not the neutral act it is elsewhere in the registry:
    //     `promote_due_forwards` walks `distinct_forward_queue_actors()` and is
    //     succession-blind, so a row parked on the retired identity is still
    //     dispatched to the thief after the ceremony.
    //
    //     The split is keyed on the row's persisted **copy mode**, never its
    //     class (forward-all, a filter rule id, `forwarder`). A `copy` row is a
    //     *second* copy — the original rests sealed in the mailbox — so burning
    //     it costs the user nothing and closes the tap, whichever rule armed
    //     it. A `redirect` row keeps no local copy: it
    //     is the only copy of a message this nest already answered `250` for,
    //     so it moves, and promotes under a live identity whose rate cap and
    //     eviction notice still reach someone.
    //
    //     ⚠ No class is a proxy for the mode. The admin forwarder is always
    //     `redirect`, but a per-rule rule chooses either, and **forward-all
    //     follows the message**: when a redirect rule fires, the MTA sends the
    //     forward-all copy as `redirect` too (`mta/server.go`, the
    //     `redirectRecipient` branch), because that recipient keeps no local
    //     copy. So a `forward-all` literal in the burn would destroy accepted
    //     mail. A row with no mode (queued before the column existed) moves for
    //     the same reason, and says so out loud.
    tx.execute(
        "DELETE FROM forward_queue WHERE actor_id = ?1 AND copy_mode = ?2",
        rusqlite::params![old, burnable_copy_mode()],
    )
    .context("burn queued forward copies")?;
    let unknown_mode_classes: Vec<String> = {
        let mut stmt = tx
            .prepare(
                "SELECT DISTINCT rule_id_or_forward_all FROM forward_queue
                  WHERE actor_id = ?1 AND (copy_mode IS NULL OR copy_mode NOT IN ('copy', 'redirect'))",
            )
            .context("prepare unknown-mode forward classes")?;
        let rows = stmt
            .query_map(rusqlite::params![old], |row| row.get::<_, String>(0))
            .context("read unknown-mode forward classes")?;
        rows.collect::<std::result::Result<_, _>>()
            .context("unknown-mode forward class row")?
    };
    if !unknown_mode_classes.is_empty() {
        tracing::warn!(
            target: "recovery",
            predecessor = %hex::encode(old),
            classes = ?unknown_mode_classes,
            "queued forwards with no persisted copy mode were carried to the successor \
             rather than burned: without the mode a row may be a redirect with no local \
             copy, so burning it could destroy accepted mail"
        );
    }
    moved += tx
        .execute(
            "UPDATE forward_queue SET actor_id = ?2 WHERE actor_id = ?1",
            rusqlite::params![old, new],
        )
        .context("carry queued forwards that may have no local copy")?;

    // (d) The **outbound** queue, which is where a thief's in-flight copies
    //     actually sit: `forward_queue` is only the rate-cap overflow, and a
    //     forward under the cap is enqueued straight here by
    //     `forward_message`. The dispatcher selects on `status = 'pending'`
    //     alone and has no idea the forwarding identity was retired.
    //
    //     Same copy-mode predicate, same reasoning as (c), plus one more
    //     conjunct: only an **undispatched** row is still a tap. A `sent` or
    //     `bounced` row is history of a forward the retired identity really
    //     made, and deleting it would destroy the record rather than close
    //     anything.
    tx.execute(
        "DELETE FROM outbound_mail_queue
          WHERE forward_actor_id = ?1 AND forward_copy_mode = ?2 AND status = 'pending'",
        rusqlite::params![old, burnable_copy_mode()],
    )
    .context("burn undispatched forward copies")?;
    //     Everything left re-attributes, so the SRS rewrite at queue-out and
    //     the NDR route name an identity that still exists — a bounce
    //     addressed to a retired actor reaches nobody. It also carries the
    //     account's recent forward-rate window with it, which is what stops a
    //     succession from resetting the cap.
    moved += tx
        .execute(
            "UPDATE outbound_mail_queue SET forward_actor_id = ?2 WHERE forward_actor_id = ?1",
            rusqlite::params![old, new],
        )
        .context("re-attribute the account's remaining outbound forwards")?;

    Ok(moved)
}

/// Execute **every** declared [`actor_tables::Succession::Burn`] leg — the burn
/// half of what the plain-move pass did for the plain moves.
///
/// Returns per-table row counts, because callers need a specific table's number
/// rather than a total: `SuccessionApplied.capability_grants_revoked` and
/// `.cancelled_pending_replacement` are read by the handler and pinned by tests.
///
/// **What this replaced.** Until 2026-08-15 the burns were seven hand-written
/// functions plus four inline statements, each carrying its own table list — the
/// two-hand-typed-lists-that-must-agree shape the plain-move pass removed from the move side.
/// It is the shape `account_aliases` shipped from, and the shape was found again
/// when the ratified `atproto_authoring_keys` burn turned out to exist only as
/// prose. A table now joins the burn by declaring
/// [`actor_tables::Succession::Burn`] and by nothing else.
///
/// ⚠ **Three things this must get right, each of which the hand-written legs got
/// right individually and could have stopped getting right one at a time.**
///
/// 1. **Bind per the table's declared encoding.** `nostr_zap_signers` is
///    `ActorKey::Hex`; every other burn table is `Blob`. SQLite applies no
///    affinity conversion, so a mismatched parameter is not a type error — it is
///    a `DELETE` matching nothing, which reads exactly like "this account had
///    none". This is [`execute_plain_moves`]' dispatch, copied rather than
///    re-derived.
/// 2. **Guard on existence.** The bridge tables are created by their feature's
///    own `init_db`; on a nest built without it an unconditional statement
///    aborts the whole transaction.
///
/// ⚠ **The rule applies unchanged, and its replacement already exists.**
/// Generating a checked thing from its own declaration destroys the independence
/// the check relied on: demote a table out of `Burn` and this executor simply
/// stops burning it, silently. What catches that is the exact-count ratchet — a
/// demotion reds `the_unruled_backlog_only_shrinks` by name (mutation-graded)
/// — plus the generic sweep, which judges a `Burn` on its *observable*
/// (`on_old == 0 && on_new == 0`) and does not care how the leg is written. As of
/// the fix that sweep's `SEED_BLIND` is empty, so every table in this
/// loop is actually observed rather than trusted.
///
/// **What deliberately does NOT ride this loop.** A *predicated* deletion is
/// [`actor_tables::Succession::Partial`] by definition and stays hand-written by
/// construction.
fn execute_burn_legs(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
) -> Result<std::collections::BTreeMap<&'static str, usize>> {
    let old_hex = hex::encode(old);
    let mut burned = std::collections::BTreeMap::new();

    for entry in actor_tables::burn_legs() {
        if !table_exists(tx, entry.table)? {
            continue;
        }
        let bound: &dyn rusqlite::ToSql = match entry.key {
            ActorKey::Blob => &old,
            ActorKey::Hex => &old_hex,
        };
        let rows = tx
            .execute(
                &format!("DELETE FROM {} WHERE {} = ?1", entry.table, entry.column),
                rusqlite::params![bound],
            )
            .with_context(|| format!("burn {} at succession", entry.table))?;
        burned.insert(entry.table, rows);
    }

    Ok(burned)
}

/// Re-point the **guardian side** of the family plane at the ceremony.
///
/// The family plane's *supervised* side is twelve plain-move registry entries
/// and rides the ordinary loop. These three columns need a leg of their own for
/// a purely mechanical reason: the registry-driven executor moves each table's
/// **one** declared column, and every column here is a *second* actor column on
/// a table whose first one moves for a different ceremony
/// (`actor_tables::SUCCESSION_REFERENCES`, the guardian-side block).
///
/// **Why the guardian's ceremony needs a leg at all.**
/// `family-safety.md` § Lifecycle gates rules that *"a stranded ward is
/// unrepresentable: no path removes a guardian account while a link references
/// it"*, and enforces it by refusing evict/delete while links exist. A
/// succession is neither: it retires the guardian's identity — refused
/// everywhere — while `guardianships.guardian_actor_id` goes on naming it. So
/// the successor guardian's `guardian_accounts_for` returns nothing while the
/// ward stays supervised under a guardian who can never act again. Not a
/// `nest/common.md` § Client-state recoverability breach (an admin can still
/// graduate or transfer the ward), but exactly the half-state that gate exists
/// to make unrepresentable, reached by the one path it never grew a leg for.
///
/// Ordering carries nothing here — the three statements touch three different
/// tables and no foreign key runs between them
/// (`no_foreign_key_crosses_a_succession_leg` is what keeps that true).
fn rule_the_guardian_family(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    let mut moved = 0usize;

    // (a) The link itself: the guardian's wards follow the guardian. A
    //     succession is identity rotation rather than a change of person, so
    //     the transfer handshake's consent requirement is not engaged — nobody
    //     new is being conscripted into the duty.
    moved += tx
        .execute(
            "UPDATE guardianships SET guardian_actor_id = ?2 WHERE guardian_actor_id = ?1",
            rusqlite::params![old, new],
        )
        .context("re-point guardianship links to the successor guardian")?;

    // (b) A pending transfer proposal must keep naming an identity that can
    //     still call `fauna.family.transfer.accept`, which re-validates against
    //     the caller.
    moved += tx
        .execute(
            "UPDATE guardian_transfers SET proposed_guardian_actor_id = ?2 \
              WHERE proposed_guardian_actor_id = ?1",
            rusqlite::params![old, new],
        )
        .context("re-point pending transfer proposals to the successor guardian")?;

    // (c) A third party's ceremony, not either family member's: the peer the
    //     ward asked to contact. Left behind, the guardian's approval would
    //     mint a contact edge to a retired identity.
    moved += tx
        .execute(
            "UPDATE guardian_contact_requests SET peer_actor_id = ?2 WHERE peer_actor_id = ?1",
            rusqlite::params![old, new],
        )
        .context("re-point the ward's pending contact asks at the succeeded peer")?;

    Ok(moved)
}

/// Re-point the **paying reader's** side of the entitlement plane at the
/// ceremony.
///
/// The plane's *author* side is eight registry entries moving as one declared
/// coupled family, and it rides the family executor. These three columns name
/// the reader who paid, and they need a leg of their own for the mechanical
/// reason the guardian leg does — the registry-driven executor moves each
/// table's **one** declared column, and two of these are a *second* actor
/// column on a table whose first one moves for the author's ceremony
/// (`actor_tables::SUCCESSION_REFERENCES`, the paying-reader block, which
/// carries the full reasoning).
///
/// **Why the reader needs a leg at all.** `subscribers.subscriber_id` is the
/// row that says this person paid, and `is_subscriber` /
/// `get_subscribed_tiers` gate `fauna.subscriptions.key_blob.get` — the key
/// material that opens the content. Left behind, a reader who rotates their identity does not just
/// lose the list `list_my_subscriptions` renders: every tier they bought goes
/// dark, with no path back that does not involve paying again.
///
/// **Why the three are one leg and must not be split.**
/// `payment_core::apply_refund` joins them *in code*: for a refund on an
/// already-redeemed claim it reads `payment_claim_codes.redeemed_by` as the
/// buyer, then voids that actor's window with `set_subscriber_valid_until`.
/// Moving the roster row while leaving the redeemer behind therefore aims the
/// void at an identity that no longer holds the row — the successor keeps an
/// entitlement that was refunded. No foreign key expresses that coupling, so no
/// family declaration and no FK gate can catch a split ruling; one function is
/// the enforcement.
///
/// **Why the ek is cleared rather than carried.** `mlkem_encaps_key` is derived
/// from the subscriber's identity *seed*, while the X25519 half of the same
/// X-Wing public is reconstructed by the author from their ActorId. Carried
/// across a rotation the two halves belong to different identities, and every
/// wrap under the pair is unopenable by anyone. `NULL` is a supported state —
/// the author wraps classically — and the successor re-publishes its own ek on
/// its next subscribe, which is exactly what that call's already-subscribed arm
/// exists to do.
fn rule_the_paying_readers_entitlements(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    let mut moved = 0usize;

    // (a) The entitlement itself, with the stale post-quantum key dropped as it
    //     moves. The ek clear is not a tidy-up: it is what stops the author
    //     wrapping the next period key to a hybrid public whose two halves name
    //     two different identities.
    moved += tx
        .execute(
            "UPDATE subscribers SET subscriber_id = ?2, mlkem_encaps_key = NULL \
              WHERE subscriber_id = ?1",
            rusqlite::params![old, new],
        )
        .context("re-point the reader's paid subscriptions to the successor")?;

    // (b) The same person one step earlier: a subscribe/unsubscribe request the
    //     author has not drained. The approval path copies this row's ek onto
    //     the `subscribers` row, so a stale one left here would survive the
    //     ceremony by being copied forward — hence the same clear.
    moved += tx
        .execute(
            "UPDATE subscribe_requests SET subscriber_id = ?2, mlkem_encaps_key = NULL \
              WHERE subscriber_id = ?1",
            rusqlite::params![old, new],
        )
        .context("re-point the reader's pending subscribe requests to the successor")?;

    // (c) The redeemer pointer a refund resolves through — see this function's
    //     doc comment.
    moved += tx
        .execute(
            "UPDATE payment_claim_codes SET redeemed_by = ?2 WHERE redeemed_by = ?1",
            rusqlite::params![old, new],
        )
        .context("re-point redeemed payment claims at the successor")?;

    Ok(moved)
}

/// Move the creator's **claim ledger** to the successor, stamping every
/// unredeemed claim voided as it travels (`actor_tables.rs`, ruled 2026-08-14).
///
/// **Why it moves.** A claim names the tier it buys, and the tier plane moved as
/// a coupled family the day before. Left behind, `payment_core::redeem_claim`'s
/// tier pre-check fails for every code on the ledger, so a buyer who paid gets
/// nothing — and the successor cannot even see the receipt to make it right,
/// because `claims.list` is keyed on `author_id`. The audit trail has to travel
/// with the tiers it names.
///
/// **Why the unredeemed half is voided as it travels.** A claim code is a
/// **bearer** credential: `redeem_claim` finds it by code alone and binds it to
/// whoever presents it, so it consults the retired identity nowhere. A seed
/// thief could mint an unbounded supply at `fauna.payments.claims.mint` and hold
/// the codes through the ceremony, and **no app can revoke one** — there is no
/// `claims.void` gesture; `void_payment_claim` is reachable only from the refund
/// path. Carried live that is `nest_pairings`' shape exactly. Voiding costs the
/// honest buyer nothing a `Stay` would not have cost them anyway, and leaves the
/// row — `external_ref` intact — so the successor can verify the payment in the
/// provider dashboard and mint a fresh code.
///
/// **Why the rows are not simply burned**, which is what the authority argument
/// alone would suggest: `payment_claim_codes` is the payment audit trail and its
/// schema says outright that rows are kept for audit and never deleted. A void
/// ends the authority without destroying the record — the strictly better half
/// of a `Partial` that would have burned the unredeemed ones.
///
/// A **redeemed** row is untouched beyond the move: it is a completed sale, and
/// `payment_core::apply_refund` still resolves through it. Its `redeemed_by`
/// half answers the reader's own ceremony, one leg over in
/// [`rule_the_paying_readers_entitlements`].
fn rule_the_creators_claim_ledger(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    let now = now_epoch_secs();

    // One statement, because the void must not be able to land without the move
    // (or the ledger would be dead on the retired identity AND unrecoverable
    // from the successor's list). `COALESCE` keeps an existing void's timestamp:
    // a claim already voided by a refund is history of that refund, not of this
    // ceremony.
    let moved = tx
        .execute(
            "UPDATE payment_claim_codes \
                SET author_id = ?2, \
                    voided_at = COALESCE(voided_at, \
                                         CASE WHEN redeemed_at IS NULL THEN ?3 END) \
              WHERE author_id = ?1",
            rusqlite::params![old, new, now],
        )
        .context("carry the creator's claim ledger to the successor")?;

    Ok(moved)
}

/// Re-point the **per-caller labeler publication quota** of the retired
/// identity at the ceremony.
///
/// **This is a DoS bound, not attribution — and leaving it behind resets it.**
/// `labelers` carries two actor columns answering opposite questions.
/// `publisher_actor` is the artifact's own self-signed `algorithm_id`, ruled
/// `Stay` in `ACTOR_TABLES` because it names the key the stored bytes were
/// signed with. `caller_actor` is the authenticated enrolled identity, and it
/// exists for one reason: the security review found that a cap keyed on the
/// signing keypair bounds nothing, the keypair being free and off-box-rotatable,
/// so `MAX_LABELERS_PER_CALLER` was keyed on "the un-rotatable enrolled-user
/// identity" instead. `db::labelers::put_labeler` enforces it with
/// `SELECT COUNT(*) FROM labelers WHERE caller_actor = ?`.
///
/// **The failure it prevents.** A succession is self-service — any holder
/// performs one with their own recovery kit (`succession-aftermath.md`
/// § Re-key scope, the limits-move rule) — so a left-behind count hands the
/// successor a fresh 64-labeler allowance while the retired identity's rows
/// stay in the catalog. The bound built to survive *keypair* rotation is then
/// defeated by *identity* rotation, repeatably. The catalog has no delete or
/// retire verb at all (`fauna.labelers.*` is publish/list/inspect/subscribe/
/// unsubscribe), so every row admitted by a reset is permanent: the only
/// remaining ceiling is the deployment-wide `MAX_TOTAL_LABELER_BYTES`.
///
/// **Why moving cannot hand over anything.** The column confers no read, no
/// write and no revoke — it is counted and never resolved through — so the
/// successor gains a consumed allowance and nothing else. That is the accurate
/// reading rather than a penalty: the rows exist, are undeletable, and are
/// theirs now.
///
/// Hand-written rather than registry-driven for the mechanical reason every
/// reference leg is: the registry-driven executor moves each table's **one**
/// `ACTOR_TABLES` column, and this is the second one.
/// Disarm the retired identity's **still-armed scheduled actions** — the
/// delayed destructive operations it queued and a ceremony would otherwise
/// leave running (`actor_tables.rs`, `pending_actions`; ruled 2026-08-15).
///
/// **Why this leg is not about where rows live.** Every other verdict on this
/// axis answers *whose rows are these afterwards*. `pending_actions` is the
/// first table where the dangerous thing is a **timer**: `start_executor` ticks
/// every 60 s, `execute_ready_actions` selects on `status = 'pending' AND
/// execute_after <= now`, and it **authenticates nobody** — the row *is* the
/// authority, the same standing this axis has already refused to
/// `push_subscriptions` and `eviction_tokens`. The delays are 6 h (handle
/// change) to 30 d (backup purge override), with 14 d for an account deletion,
/// so a succession racing a seed thief lands *inside* the window by
/// construction. That is what the window is for.
///
/// **Why neither `Move` nor `Burn`.**
/// - `Move` is the actively dangerous direction here, `eviction_tokens`' shape
///   one plane over: three of the arms resolve their target from `target` /
///   `payload` with **no actor check at all** (`snapshot.delete`,
///   `snapshot.bulk_prune`, `admin.backup_purge_override` — the last a *hard*
///   `delete_snapshots`), so re-pointing a thief's queued prune aims it at the
///   corpus the ceremony just moved to the successor.
/// - `Burn` would delete the ledger's record that the action was ever queued —
///   and a thief's scheduled destruction of the owner's data is precisely the
///   act worth leaving legible to the identity it was aimed at, which is the
///   argument `pending_actions.cancelled_by` already carries in
///   `SUCCESSION_REFERENCES`.
///
/// So the rows **stay** and the ceremony writes the table's own terminal state
/// instead. `cancelled_by` is deliberately left NULL: no person cancelled
/// these, and naming either identity would be a false attribution on a row
/// whose whole remaining value is being an honest record.
///
/// ⚠ **The un-mark is not optional, and skipping it is a silent loss.** A
/// snapshot action marks its targets `deletion_pending` when it is created, and
/// `cancel_pending_action` clears the mark for a reason its own comment states:
/// nothing else ever clears it, so a snapshot left marked is excluded from
/// `count_active_snapshots` — its folder's retention floor and the auto-pruner's
/// candidate population — permanently. `folders.actor_id` moves, so those
/// snapshots are the *successor's* by the time this commits: a disarm without
/// the un-mark hands the rescued account a set of its own snapshots that no
/// longer count as existing.
///
/// ⚠ **This is where the reader will reach for the chain hash, so it is
/// answered here.** `chain_hash` covers `actor_id`, which is a real argument
/// against *moving* or *deleting* a row — but `compute_pending_action_hash`
/// binds the status the row was **created** with, and every existing terminal
/// transition (`cancel_pending_action`, `mark_pending_action_executed`,
/// `mark_pending_action_expired`) already writes `status` without recomputing
/// it. This leg is that same transition and no weaker. The honest half:
/// **nothing verifies that chain** — every reference to `chain_hash` is a
/// write or a `SELECT … LIMIT 1` to seed the next one, with no walker anywhere,
/// unlike `audit_log`, whose linkage is exported to the admin surface and
/// asserted. Cited for what it is rather than leaned on.
fn disarm_the_retired_identitys_scheduled_actions(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
) -> Result<usize> {
    // The predicate is (this actor AND still armed) — never status alone,
    // which would re-write history, and never actor alone, which would
    // re-cancel rows already terminal. Armed is `pending` or `executing`: a
    // row the executor has claimed is mid-run, and the disarm cannot recall
    // that run, but it cancels the row so a run that fails is never re-armed
    // (the executor's release moves only an `executing` row). A run that
    // completes anyway is audited `pending_action.executed_after_disarm`
    // (`nest/common.md` § Pending Actions System → *The executor claims
    // before it acts*).
    let armed: Vec<(i64, String, Option<String>, Option<String>)> = tx
        .prepare(
            "SELECT id, action_type, target, payload
               FROM pending_actions
              WHERE actor_id = ?1 AND status IN ('pending', 'executing')",
        )
        .context("prepare armed scheduled actions")?
        .query_map(rusqlite::params![old], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .context("query armed scheduled actions")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collect armed scheduled actions")?;

    if armed.is_empty() {
        return Ok(0);
    }

    let now = crate::db::now_epoch_secs();
    for (id, action_type, target, payload) in &armed {
        // The version-plane twin of the snapshot un-mark below, same reason:
        // a `version.bulk_prune` left marked excludes its rows from the
        // evaluator's population permanently, on a prune this ceremony just
        // disarmed.
        for seq in super::pending_actions::version_targets_from(action_type, payload.as_deref()) {
            tx.execute(
                "UPDATE sync_changes SET prune_pending = NULL WHERE seq = ?1",
                rusqlite::params![seq],
            )
            .context("clear prune_pending at succession disarm")?;
        }
        for snapshot_id in super::pending_actions::snapshot_targets_from(
            action_type,
            target.as_deref(),
            payload.as_deref(),
        ) {
            tx.execute(
                "UPDATE snapshots SET deletion_pending = 0 WHERE id = ?1",
                rusqlite::params![snapshot_id],
            )
            .context("clear deletion_pending at succession disarm")?;
        }
        tx.execute(
            "UPDATE pending_actions
                SET status = 'cancelled', cancelled_at = ?2
              WHERE id = ?1 AND status IN ('pending', 'executing')",
            rusqlite::params![id, now],
        )
        .context("disarm scheduled action at succession")?;
    }

    tracing::info!(
        target: "recovery",
        predecessor = %hex::encode(old),
        actions = armed.len(),
        "disarmed the retired identity's still-armed scheduled actions"
    );
    Ok(armed.len())
}

fn rule_the_labeler_publication_quota(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    let moved = tx
        .execute(
            "UPDATE labelers SET caller_actor = ?2 WHERE caller_actor = ?1",
            rusqlite::params![old, new],
        )
        .context("re-point labeler publication quota to the successor")?;
    Ok(moved)
}

/// Re-point the three **counterparty** columns — the references that name the
/// *other* person in somebody else's row (`actor_tables.rs`'s
/// `SUCCESSION_REFERENCES`; ruled 2026-08-15).
///
/// **What makes this class different from every ruling before it.** The axis has
/// so far asked what a row grants or limits *its own* actor. These three rows are
/// about somebody else, and so is the damage: every consumer below is counting
/// or matching **people**, so a reference left on a retired identity does not
/// merely go stale — it makes the nest believe there are **two humans where
/// there is one**, and the resulting error lands on an uninvolved third party
/// who can neither see it nor appeal it.
///
/// - `sender_behavior.target_actor` — the sender's spam profile counts DISTINCT
///   values of this column over 1 h / 24 h / 7 d, so one recipient who succeeds
///   mid-window inflates a third party's fan-out. Its other half fails the
///   opposite way: the reply-recording statement requires a matching `dm_sent`
///   row for the same pair, so after the recipient's ceremony their replies stop
///   recording at all and the sender reads *colder* than they are.
/// - `notifications.sender_id` — part of the insert's own dedup key, so a sender
///   who succeeds re-notifies people about events they were already told about.
/// - `knocks.sender_id` — the stranger behind a held knock, whose recipient half
///   already moves.
///
/// ⚠ **Every one of these errors is CONSERVATIVE** (more spam-suspicious, a
/// higher penalty, a duplicate rather than a silence), which is why they waited
/// while the grant-shaped columns went first. It is not a reason to leave them:
/// conservative wrong data is still wrong data, and it is charged to somebody
/// who did nothing.
///
/// ⚠ **A `SUCCESSION_REFERENCES` ruling is executed by NOTHING unless a leg like
/// this exists.** `plain_move_legs()` iterates `ACTOR_TABLES` only, so a
/// declaration here moves no rows on its own, and the leg is pinned by hand.
///
/// Returns the number of rows re-pointed.
fn rule_the_counterparty_references(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    let mut moved = 0usize;
    for (table, column) in [
        ("sender_behavior", "target_actor"),
        ("notifications", "sender_id"),
        ("knocks", "sender_id"),
    ] {
        if !table_exists(tx, table)? {
            continue;
        }
        moved += tx
            .execute(
                &format!("UPDATE {table} SET {column} = ?2 WHERE {column} = ?1"),
                rusqlite::params![old, new],
            )
            .with_context(|| format!("re-point {table}.{column} to the successor"))?;
    }
    Ok(moved)
}

/// **A hand-attached label's two writer columns** — `content_labels.classifier_id`
/// and `scanner_id`, which `fauna.labels.attach` sets to the caller alike, handed
/// to the successor as ONE fact at the ceremony.
///
/// Why they move: a label has no detach door. Its writer revises it by attaching
/// again, and the upsert keys on `classifier_id` — so a row left naming the
/// retired identity is one the successor can never reach. Their revision lands
/// BESIDE it, and every reader takes the highest confidence of the two
/// (`db::feeds::project_content_labels`, the mandatory spam group), so a
/// self-label — or one a seed thief attached in the author's name — could be
/// raised for ever and never lowered.
///
/// Keyed on `classifier_id` alone, and `scanner_id` rides in the same statement:
/// the attach door is the only writer that puts a person in either column, and
/// it puts the same one in both. A room labeler pass names the labeler's
/// artifact key and the nest's own; the channel anomaly label names zeros.
/// Neither matches, so neither is touched.
///
/// **A collision SUPERSEDES** — defensive, since the ceremony's successor has
/// labelled nothing (`NewAlreadyRegistered`), but it keeps the move total. A
/// successor holding a label on the same post and category holds the same
/// writer's LATER verdict on the same question, so the retired identity's row
/// is deleted: keeping it would leave the old confidence outvoting the revision
/// for good, which is the defect itself. Nothing a user cannot recreate is lost
/// — it is their own superseded opinion.
///
/// Returns the number of rows re-pointed.
fn rule_the_label_writer_columns(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    if !table_exists(tx, "content_labels")? {
        return Ok(0);
    }
    tx.execute(
        "DELETE FROM content_labels
          WHERE classifier_id = ?1
            AND EXISTS (SELECT 1 FROM content_labels kept
                         WHERE kept.classifier_id = ?2
                           AND kept.content_type = content_labels.content_type
                           AND kept.content_id = content_labels.content_id
                           AND kept.category = content_labels.category)",
        rusqlite::params![old, new],
    )
    .context("supersede the retired identity's label where the successor re-labelled")?;
    tx.execute(
        "UPDATE content_labels
            SET classifier_id = ?2,
                scanner_id = CASE WHEN scanner_id = ?1 THEN ?2 ELSE scanner_id END
          WHERE classifier_id = ?1",
        rusqlite::params![old, new],
    )
    .context("re-point content_labels' writer columns to the successor")
}

/// **The floor roster's seats** — `room_members` rows in the rooms whose
/// membership authority the nest itself holds, handed from the retired identity
/// to its successor at the ceremony.
///
/// Why this is the one membership plane the ceremony writes: every other
/// participation row stays and is carried by propagation — an end-to-end
/// room's successor entry arrives in the add-successor commit its device
/// reports, a group's in the MLS sweep. A ceremony-born room
/// (`rooms.birth_salt` set, `RoomRecord::is_floor_authoritative`) has neither:
/// its floor IS the authority, and the report door refuses it. Before this leg
/// its successor was nobody on the floor, refused "not a member" at every door,
/// while the retired identity kept the seat — for an owner the unremovable one
/// every owner-only door keys on, so appoint, demote and transfer were gone for
/// the life of the room.
///
/// For every such room where `old` holds a live seat homed on this nest:
///
/// 1. **The successor is seated with the predecessor's role**, at a FRESH roster
///    entry (derived from this moment — the scheme's re-admission rule) and with
///    **no wrap target**. The predecessor's reception key derives from the seed
///    the ceremony exists to retire, so carrying it would keep every new
///    generation wrapped to the thief; the successor's app supplies its own.
/// 2. **The predecessor's row is `Removed`-absorbed**, never purged — it is the
///    attribution of everything that identity did in the room, and exactly the
///    "fresh entry with the predecessor `Removed`" shape
///    `conversation-rooms.md` § The home nest → *Transfer by succession* names.
///
/// `rooms.owner_id` is a plain registry move that has already run, so an
/// owner's seat and the room record agree when this returns. The signed policy
/// still names the predecessor — the nest cannot author one — and the floor
/// resolves its names through the succession chain
/// (`conversations_handlers.rs`, `floor_designee`).
///
/// **Collisions are defensive.** The ceremony's successor holds nothing
/// (`NewAlreadyRegistered`), but the statement stays total should a seat under
/// the successor's id exist anyway: a live successor seat keeps its own roster
/// entry and wrap target (wraps sealed to it must stay openable) and takes the
/// stricter of the two ranks; a removed one comes back at a fresh entry, as any
/// re-admission does.
///
/// **Declared bound — seats homed on another nest.** Only a local identity is
/// ever named here, and a room homed here seats its local members at
/// `home_node_url = ''`. A foreign member's seat is its own home nest's to hand
/// over, and a succession this nest only learns from a peer seats nobody.
fn rule_the_floor_roster_seats(
    tx: &rusqlite::Transaction<'_>,
    old: &[u8],
    new: &[u8],
) -> Result<usize> {
    const RANK: &str =
        "CASE {r} WHEN 'owner' THEN 3 WHEN 'admin' THEN 2 WHEN 'member' THEN 1 ELSE 0 END";
    let successor: [u8; 32] = new.try_into().context("successor id width")?;
    let seats: Vec<(Vec<u8>, Option<String>, Option<Vec<u8>>)> = {
        let mut stmt = tx
            .prepare(
                "SELECT m.room_id, m.role, m.invited_by FROM room_members m
                   JOIN rooms r ON r.room_id = m.room_id
                  WHERE m.principal_id = ?1 AND m.removed_at IS NULL
                    AND m.principal_kind = 'user' AND m.home_node_url = ''
                    AND r.birth_salt IS NOT NULL",
            )
            .context("prepare the retired identity's floor seats")?;
        stmt.query_map(rusqlite::params![old], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .context("read the retired identity's floor seats")?
        .collect::<rusqlite::Result<_>>()
        .context("collect the retired identity's floor seats")?
    };
    let existing_rank = RANK.replace("{r}", "room_members.role");
    let carried_rank = RANK.replace("{r}", "excluded.role");
    let now = now_epoch_millis();
    let mut seated = 0usize;
    for (room_id, role, invited_by) in seats {
        let room: [u8; 32] = room_id.as_slice().try_into().context("room id width")?;
        let entry_id = fauna_mls::room_policy::derive_room_entry_id(&room, &successor, now)
            .context("derive the successor's roster-entry id")?;
        seated += tx
            .execute(
                &format!(
                    "INSERT INTO room_members (room_id, principal_id, principal_kind, role,
                                               invited_by, home_node_url, joined_at, reported_at,
                                               removed_at, entry_id, reception_pubkey)
                     VALUES (?1, ?2, 'user', ?3, ?4, '', ?5, ?5, NULL, ?6, NULL)
                     ON CONFLICT(room_id, principal_id) DO UPDATE SET
                         role = CASE WHEN room_members.removed_at IS NULL
                                      AND ({existing_rank}) >= ({carried_rank})
                                     THEN room_members.role ELSE excluded.role END,
                         entry_id = CASE WHEN room_members.removed_at IS NULL
                                         THEN room_members.entry_id ELSE excluded.entry_id END,
                         reception_pubkey = CASE WHEN room_members.removed_at IS NULL
                                                 THEN room_members.reception_pubkey ELSE NULL END,
                         principal_kind = 'user',
                         home_node_url = '',
                         removed_at = NULL,
                         reported_at = excluded.reported_at"
                ),
                rusqlite::params![room_id, new, role, invited_by, now, entry_id.as_slice()],
            )
            .context("seat the successor on the floor")?;
        tx.execute(
            "UPDATE room_members SET removed_at = ?3, reported_at = ?3
              WHERE room_id = ?1 AND principal_id = ?2 AND removed_at IS NULL",
            rusqlite::params![room_id, old, now],
        )
        .context("absorb the retired identity's floor seat")?;
    }
    Ok(seated)
}

/// An actor id is a 32-byte Ed25519 public key.
pub const ACTOR_ID_LEN: usize = 32;

/// Hard bound on a [`CacheDb::succession_path`] walk.
///
/// A cycle is already structurally hard — each link must advance the *old*
/// identity's chain `seq`, and `old_actor_id` is a PRIMARY KEY, so re-entering a
/// visited identity would require succeeding it twice. But the walk is driven by
/// an **anonymous** caller over a pre-identity kind, so it gets a bound anyway
/// rather than relying on an invariant proved elsewhere in the file. A real
/// identity succeeded 32 times has bigger problems than a truncated reply.
pub const MAX_SUCCESSION_PATH: usize = 32;

/// How many of a retired identity's pairing destinations one succession keeps
/// as its owed nests (`identity-succession.md` § Enforcement on the home nest →
/// *Every nest the identity is linked to*, **The road**).
///
/// A cap because the seed can add pairing rows: `fauna.pair.add` is an ordinary
/// User-class gesture, so a thief can park any number of them, and each kept
/// row is a nest every successor device then tries to reach. The oldest rows
/// are kept first, which puts the rows that predate the theft ahead of the
/// thief's.
pub const MAX_OWED_NESTS: usize = 16;

/// One owed nest: a pairing destination a succession burned and kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwedNestRow {
    /// The retired identity whose pairing named the nest.
    pub old_actor_id: Vec<u8>,
    /// The burned row's `private_nest_id`, verbatim.
    pub nest_id: Vec<u8>,
    /// The burned row's `nest_url`, when it carried one.
    pub nest_url: Option<String>,
}

/// Keep the retired identity's pairing destinations as the succession's owed
/// nests. Runs inside the succession transaction and **before** the burn loop
/// deletes the rows it reads.
///
/// A pairing that had already expired is not kept: it was no standing link when
/// the statement landed, and the consumers that read `nest_pairings` skip it
/// the same way (`db/pairing.rs`, whose clock and unit this uses — `expires_at`
/// is in microseconds, unlike the `now` the transaction stamps in seconds).
/// `OR IGNORE` because the primary key is the retired id and a nest: nothing
/// writes this table for an identity before its one succession, so there is no
/// row to collide with, and a collision must not abort the ceremony.
fn keep_owed_nests(tx: &rusqlite::Transaction<'_>, old: &[u8]) -> Result<usize> {
    let now_micros = fauna_core::data::Timestamp::now().as_i64();
    tx.execute(
        "INSERT OR IGNORE INTO succession_owed_nests
            (old_actor_id, nest_id, nest_url, created_at)
         SELECT actor_id, private_nest_id, nest_url, created_at
           FROM nest_pairings
          WHERE actor_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)
          ORDER BY created_at ASC, rowid ASC
          LIMIT ?3",
        rusqlite::params![old, now_micros, MAX_OWED_NESTS as i64],
    )
    .context("keep the burned pairings' destinations as owed nests")
}

/// One recorded succession: `old_actor_id` was succeeded by `new_actor_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessionRow {
    pub old_actor_id: Vec<u8>,
    pub new_actor_id: Vec<u8>,
    /// The verbatim canonical DAG-CBOR bytes the client signed — replayed
    /// byte-for-byte by the lookup kind, never re-encoded.
    pub statement: Vec<u8>,
    /// The `seq` on the old identity's chain that this statement advanced past.
    pub seq: u64,
    /// Unix seconds the nest applied the succession.
    pub succeeded_at: i64,
}

/// Why a succession could not be applied. Each variant maps to a distinct,
/// actionable wire error — the handler must not collapse them into one
/// "invalid", because "you already succeeded this identity" and "your successor
/// key is already somebody's account" call for completely different client
/// behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuccessionRefusal {
    /// The old identity has no `users` row here — this nest is not its home, so
    /// there is no account to re-point.
    OldNotRegistered,
    /// The old identity was already succeeded. First-succession-wins: the next
    /// ceremony must succeed the *successor*, authorized by the successor's own
    /// registration chain.
    AlreadySucceeded,
    /// The successor actor id already has an account here. The successor must be
    /// a genuinely new identity (`identity-succession.md:542`); re-pointing onto
    /// an existing account would merge two users.
    NewAlreadyRegistered,
    /// A **peer-relayed** statement named an identity homed on this nest. Only
    /// `fauna.recovery.succession.submit` may succeed a local account, because
    /// only it runs the account transaction — see
    /// [`CacheDb::record_peer_succession`].
    OldIsLocal,
    /// A **peer-relayed** statement named a successor this nest already holds a
    /// link into. A nest holds one predecessor per identity (ruling (8)(j)(1)):
    /// the home leg's [`Self::NewAlreadyRegistered`] is this arm's twin, and the
    /// first-landed link stands. Both links are the successor's own `new_sig`,
    /// so the nest withholds one of the member's own words — the power residual
    /// (iii) already names. An honest ceremony cannot produce two, so the
    /// consumers log it at warn.
    NewAlreadySucceeded,
}

impl std::fmt::Display for SuccessionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::OldNotRegistered => "the succeeded identity has no account on this nest",
            Self::AlreadySucceeded => "the identity was already succeeded",
            Self::NewAlreadyRegistered => "the successor identity already has an account here",
            Self::OldIsLocal => {
                "this nest is the identity's home — a succession must be submitted, not relayed"
            }
            Self::NewAlreadySucceeded => {
                "the successor identity already has a predecessor on this nest"
            }
        };
        f.write_str(s)
    }
}

/// What [`CacheDb::record_peer_succession`] re-pointed. Counts, not identities:
/// the caller logs them so an admin can see propagation actually moved
/// residue rather than only recording a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerSuccessionApplied {
    /// `channel_foreign_members` rows now naming the successor.
    pub foreign_memberships: usize,
    /// `folder_member_access` rows carried to the successor with those roster
    /// rows (a predecessor's grant dropped on a collision is not counted).
    pub member_grants: usize,
    /// `contacts` rows whose `peer_id` now names the successor.
    pub contact_edges: usize,
}

/// What [`CacheDb::record_succession`] did, for the handler's log line and the
/// notification it fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessionApplied {
    /// The handle that moved to the successor, if the old account held one.
    pub handle: Option<String>,
    /// Whether the old identity was an admin (its role moved).
    pub was_admin: bool,
    /// Capability grants revoked (grantor = the old identity).
    pub capability_grants_revoked: usize,
    /// Whether a pending seed-initiated RecoveryKey replacement was cancelled.
    pub cancelled_pending_replacement: bool,
    /// Corpus-ownership + local-residue rows re-pointed old→new
    /// (`succession-aftermath.md` § Re-key scope, the ownership blockquote) —
    /// an aggregate for the log line, not a per-table report.
    pub corpus_rows_repointed: usize,
    /// The `actor_successions.succeeded_at` this transaction committed. The
    /// submit handler must reply with this value, not a fresh clock read —
    /// `filter_marks.rs`'s classifier bound is this exact stamp, and a
    /// reply-encode-time read would drift from it across a wall-clock second
    /// boundary.
    pub succeeded_at: i64,
}

impl CacheDb {
    /// Apply an **already-verified** succession statement in one transaction.
    ///
    /// The caller has verified `recovery_sig` against the head of
    /// `old_actor_id`'s registration chain and `new_sig` under `new_actor_id`
    /// (`SignedIdentitySuccession::verify`); everything below is the atomic
    /// consequence. Either all of it lands or none of it does — a half-applied
    /// succession (say, the handle moved but the account did not) would be
    /// exactly the client-unreachable nest state `nest/common.md`
    /// § Client-state recoverability forbids.
    ///
    /// `statement` is stored verbatim, so the lookup kind can replay the bytes
    /// the client signed.
    pub async fn record_succession(
        &self,
        old_actor_id: &[u8],
        new_actor_id: &[u8],
        statement: &[u8],
        seq: u64,
    ) -> Result<std::result::Result<SuccessionApplied, SuccessionRefusal>> {
        if old_actor_id.len() != ACTOR_ID_LEN || new_actor_id.len() != ACTOR_ID_LEN {
            return Err(anyhow!("actor ids must be {ACTOR_ID_LEN} bytes"));
        }
        if old_actor_id == new_actor_id {
            return Err(anyhow!("an identity cannot succeed itself"));
        }
        if statement.is_empty() {
            return Err(anyhow!("succession statement must not be empty"));
        }
        let old = old_actor_id.to_vec();
        let new = new_actor_id.to_vec();
        let statement = statement.to_vec();
        let now = now_epoch_secs();

        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction().context("begin tx")?;

        // The old account must exist here, and must not already have been
        // succeeded. Both reads happen *inside* the transaction, so a
        // concurrent second submission cannot pass its check and then land on
        // top of this one — the `actor_successions` PK is the ultimate
        // arbiter, but reading here yields the honest typed refusal instead of
        // a constraint violation.
        let old_user: Option<(String, String, i64, i64, i64)> = tx
            .query_row(
                "SELECT tier, label, suspended, inbox_bytes_used, storage_bytes_used
                   FROM users WHERE actor_id = ?1",
                rusqlite::params![old],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .context("read succeeded account")?;
        let Some((tier, label, suspended, inbox_bytes_used, storage_bytes_used)) = old_user else {
            return Ok(Err(SuccessionRefusal::OldNotRegistered));
        };

        let already: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM actor_successions WHERE old_actor_id = ?1",
                rusqlite::params![old],
                |row| row.get(0),
            )
            .optional()
            .context("read existing succession")?;
        if already.is_some() {
            return Ok(Err(SuccessionRefusal::AlreadySucceeded));
        }

        let new_exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM users WHERE actor_id = ?1",
                rusqlite::params![new],
                |row| row.get(0),
            )
            .optional()
            .context("read successor account")?;
        if new_exists.is_some() {
            return Ok(Err(SuccessionRefusal::NewAlreadyRegistered));
        }

        // 1. The record every enforcement point consults.
        tx.execute(
            "INSERT INTO actor_successions
                (old_actor_id, new_actor_id, statement, seq, succeeded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![old, new, statement, seq as i64, now],
        )
        .context("record succession")?;

        // 2. The two-step handle move. `idx_users_handle` is UNIQUE over
        //    non-empty handles, so the old row must release before the new one
        //    can take it — that ordering is the whole reason this is a "move"
        //    and not two independent writes. No `handle_cooldowns` row is
        //    written: the cooldown exists to stop a *released* handle being
        //    sniped, and here it never becomes claimable by anyone else.
        let handle: Option<String> = tx
            .query_row(
                "SELECT handle FROM users WHERE actor_id = ?1",
                rusqlite::params![old],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("read handle")?
            .filter(|h| !h.is_empty());
        if handle.is_some() {
            tx.execute(
                "UPDATE users SET handle = '' WHERE actor_id = ?1",
                rusqlite::params![old],
            )
            .context("release handle from the succeeded identity")?;
        }

        // 3. The successor account. It inherits tier (quota), label (display)
        //    and `suspended` — an admin's moderation verdict is about the
        //    account, not the key, so rotating keys must not launder it away.
        //    It deliberately does NOT inherit `locked_until`: the emergency
        //    lockout is precisely what a thief invokes, and carrying it over
        //    would let them keep the recovered account frozen. Both quota
        //    counters ride along (and the old row's are zeroed below): the
        //    accounting follows the bytes, which move in step 5b — a
        //    handle-less old row keeping the counters would strand quota
        //    nobody could ever reclaim.
        tx.execute(
            "INSERT INTO users
                (actor_id, tier, label, suspended, created_at, handle,
                 inbox_bytes_used, storage_bytes_used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                new,
                tier,
                label,
                suspended,
                now,
                handle.clone().unwrap_or_default(),
                inbox_bytes_used,
                storage_bytes_used
            ],
        )
        .context("create successor account")?;
        // The Search corpus's profile row moves with the handle: the
        // predecessor's goes, the successor is findable under the name it now
        // holds (`fts::sync_profile_row`).
        super::fts::sync_profile_row(&tx, &old)?;
        super::fts::sync_profile_row(&tx, &new)?;
        tx.execute(
            "UPDATE users SET inbox_bytes_used = 0, storage_bytes_used = 0 WHERE actor_id = ?1",
            rusqlite::params![old],
        )
        .context("zero the succeeded account's quota counters")?;

        // 4. Admin role rows move (future authority) — WITH their role. The
        //    re-INSERT used to name only (actor_id, added_at), so the column
        //    default ('superadmin') applied to every moved row: a moderator's
        //    succession would silently have promoted the successor to the
        //    superadmin tier. Unreachable while the add door mints only
        //    superadmins, but the floor guards (`db/admin.rs`
        //    `remove_admin_actor` / `set_admin_role`) read this column, so a
        //    role that changes under a succession is a floor the writer cannot
        //    see.
        let old_role: Option<String> = tx
            .query_row(
                "SELECT role FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![old],
                |row| row.get(0),
            )
            .optional()
            .context("read old admin role")?;
        let was_admin = old_role.is_some();
        if let Some(role) = old_role {
            tx.execute(
                "DELETE FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![old],
            )
            .context("remove old admin role")?;
            tx.execute(
                "INSERT OR IGNORE INTO admin_actor_ids (actor_id, added_at, role) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![new, now, role],
            )
            .context("move admin role")?;
        }

        // 5. Every declared burn, in one registry-driven loop. These are
        //    *revocations*, not migrations: standing authority a seed thief
        //    could have minted, which re-granting makes bounded and visible.
        //    The successor re-mints its capability grants from its own
        //    grant ledger and derives a fresh `NestBackupKey`, because
        //    every one of those blobs is sealed to a key the successor does not
        //    hold (§ Re-key scope); the seed-escrow blob goes the same way,
        //    since its plaintext is the old seed — exactly what the
        //    post-succession auth refusal exists to make inert — and the
        //    successor's client puts a fresh one under the new actor id
        //    (`identity-succession.md` § Seed escrow → *Lifecycle on the nest*).
        //
        //    Each table's own reason lives at its `ACTOR_TABLES` declaration.
        //    See `execute_burn_legs` for
        //    why eleven hand-written statements became one loop, and for the two
        //    legs that deliberately do not ride it.
        //
        //    One thing is read out of a burn table first. `nest_pairings` is
        //    the only record of the identity's other nests, so its
        //    destinations are kept as this succession's owed nests before the
        //    loop deletes the rows.
        keep_owed_nests(&tx, &old)?;
        let burned = execute_burn_legs(&tx, &old)?;
        let capability_grants_revoked = burned.get("capability_grants").copied().unwrap_or(0);
        //    Succession outranks a pending seed-initiated replacement
        //    (`identity-succession.md:70`). Riding this loop keeps that
        //    unconditional for the reason the hand-written statement gave: the
        //    delete is inside this transaction, so the landing sweep can never
        //    race a committed succession.
        let cancelled_pending_replacement = burned
            .get("recovery_pending_replacements")
            .copied()
            .unwrap_or(0)
            > 0;

        // 5b. Corpus ownership moves with the account (`succession-aftermath.md`
        //     § Re-key scope, the ownership blockquote, ratified 2026-08-03):
        //     every row that makes the old identity an *owner* of resting data
        //     re-points, so the successor reaches its corpus by ordinary
        //     authenticated reads and the refusal plane keeps its single rule.
        //     Collision-free by construction — the successor has no account
        //     here until this transaction (`NewAlreadyRegistered` above), so
        //     it owns nothing these UPDATEs could land on. Participation in
        //     *other* owners' sets and groups deliberately stays: that moves
        //     by propagation (the MLS sweep, the peer re-point), never here.
        //     `blob_metadata` is untouched — blobs are content-addressed and
        //     ownerless; ownership is exactly these referencing rows. The
        //     matching on-disk segment-dir rename happens *after* commit (fs
        //     ops cannot join a SQL transaction); the boot heal
        //     (`succession_ownership::heal_at_boot`) finishes a rename a crash
        //     between the two interrupted.
        // The bulk re-point, driven from the registry rather than typed here —
        // every table declaring `MoveShape::Plain` moves, and a table joins that
        // set by declaring it and by nothing else. This is the leg-drift fix:
        // `account_aliases` was missing from *this* function for as long as the
        // address existed, keeping a thief's mail password authenticating
        // through the very ceremony meant to end the theft. A hand-typed list
        // drifts; the registry's work list cannot.
        //
        // What each of these tables is, and why it is ownership, stays in the
        // registry beside the declaration (`db/actor_tables.rs`) — including the
        // three that are *not* here because their leg does more than move rows
        // (`contacts` absorbs, `admin_actor_ids` re-grants, `nostr_bunker_apps`
        // revokes as it moves), and `content_links`, which is `Partial` and
        // carries its own predicate below.
        //
        // Satellites (members/roles/content-keys/destinations/snapshots) key on
        // `folder_id` and follow their set with no edit of their own.
        let mut corpus_rows_repointed = execute_plain_moves(&tx, &old, &new)?;
        // The foreign-key-coupled families, which the loop above skips: they
        // move as one unit under `PRAGMA defer_foreign_keys`, because a family
        // moved leg-by-leg does not mis-sort rows — it aborts the ceremony.
        corpus_rows_repointed += execute_coupled_family_moves(&tx, &old, &new)?;
        // `content_links` is the one `Succession::Partial` in the registry, so
        // it stays hand-written: only an **undelivered** payload is access the
        // successor drains with the key material it holds — a delivered one is
        // history and stays. The registry deliberately asserts nothing about a
        // `Partial` table's row-level rule; this predicate is its authority, and
        // `tests::undelivered_deliveries_move_and_delivered_history_stays` is
        // its test (the name this comment carried until 2026-08-12 was of a
        // test that has never existed — the registry entry beside it was
        // right).
        //
        // ⚠ The status literal is load-bearing and must stay in lockstep with
        // `db/inbox.rs`. Before the 2026-08-24 fix, it read `'pending'` — a
        // status the inbox has never written — so this UPDATE matched ZERO
        // rows on every succession that has ever run, and a successor's
        // undelivered inbox silently stayed on the retired identity. The
        // lifecycle is 'undelivered' (insert, `inbox.rs:83`/`:132`) ->
        // 'delivered' (ack, `inbox.rs:233`); there is no third value. The
        // `.context()` below said "undelivered" the whole time — only the SQL
        // was wrong, which is why every read of this leg looked correct.
        corpus_rows_repointed += tx
            .execute(
                "UPDATE content_links SET actor_id = ?2, updated_at = ?3
                  WHERE actor_id = ?1 AND link_type = 'delivery' AND status = 'undelivered'",
                rusqlite::params![old, new, now],
            )
            .context("re-point undelivered inbox links")?;

        // 5c. Local residue — the home nest is peer zero: it re-points the
        //     residue it holds exactly as `record_peer_succession` re-points a
        //     peer's, through the same collision-merging collapse (a local
        //     user may already hold an edge to the successor; the superseded
        //     edge is absorbed on the stricter-status rule, never duplicated).
        //     The actor side is collision-free here (`NewAlreadyRegistered`
        //     is refused, so the successor holds no edges of its own) and
        //     rides the same helper for uniformity. The old id's own edge to
        //     its successor would become a self-edge — it dies instead.
        tx.execute(
            "DELETE FROM contacts WHERE actor_id = ?1 AND peer_id = ?2",
            rusqlite::params![old, new],
        )
        .context("drop the old id's edge to its own successor")?;
        corpus_rows_repointed +=
            Self::collapse_contact_edges(&tx, &old, &new, ContactCollapseSide::Actor)?;
        corpus_rows_repointed +=
            Self::collapse_contact_edges(&tx, &old, &new, ContactCollapseSide::Peer)?;

        // 5d. The Nostr legs (`ui/nostr.md` § Key succession and rotation,
        //     ratified 2026-08-11; summary row in `succession-aftermath.md`
        //     § Re-key scope). The npub **survives** — the deposited nsec rests
        //     under the *nest identity key* and crosses no wire outbound, so a
        //     seed thief gained use, never possession — which is why this is a
        //     re-point rather than the MSEK's burn-by-construction.
        //
        //     ⚠ Leg 1 — the account row and every actor-keyed satellite
        //     (`nostr_accounts`, `nostr_bunker_signers`,
        //     `nostr_federation_cursors`, `nostr_follows`; the DMs are the
        //     bridged-conversation family's since schema 118) — is **not
        //     written here**: those four are plain `Move` rows and rode the registry
        //     loop above with everything else, which is the whole point of the
        //     executor. The satellites are not optional tidiness:
        //     `trusted_zap_signers_for_pubkey` and the follows read both JOIN
        //     `nostr_accounts` on `actor_id`, so re-pointing the account alone
        //     would silently empty them for the successor while their npub kept
        //     working. The relay event store needs nothing — events are keyed by
        //     pubkey, and the pubkey is unchanged. Their hex encoding and their
        //     may-not-exist-at-all guard are both registry data now
        //     (`ActorKey::Hex`, and `execute_plain_moves`' existence check).
        //
        //     What remains below is the part that is *not* a move: the wholesale
        //     revoke.
        let old_hex = hex::encode(&old);
        let new_hex = hex::encode(&new);

        // Leg 2 — **every** bunker connection is revoked, wholesale: standing
        // third-party authority is the MSEK-credential asymmetry verbatim
        // (re-authorizing an app is bounded and visible; a thief-minted
        // connection signs as the npub silently, forever). Uniform for theft
        // and loss, no reason carried — a self-declared reason could not be
        // trusted exactly where it matters.
        //
        // The rows re-point *and* revoke rather than being left behind on the
        // dead identity: the successor's Connected-apps page is per actor, so
        // rows left on the old id would be invisible residue, and what the
        // successor most needs to see is precisely which apps held standing
        // authority. The *signer keypair* row above deliberately survives (it
        // never left the box and identifies the signer, not the user) — that is
        // the one thing this differs from `unlink_account`'s cascade, which
        // drops it.
        if table_exists(&tx, "nostr_bunker_apps")? {
            corpus_rows_repointed += tx
                .execute(
                    "UPDATE nostr_bunker_apps
                        SET actor_id = ?2, status = 'revoked', secret_hash = NULL
                      WHERE actor_id = ?1",
                    rusqlite::params![old_hex, new_hex],
                )
                .context("revoke bunker connections at succession")?;
        }

        // (Leg 2's companion — the **designated zap signer**, standing
        // third-party authority of exactly the bunker class — is declared
        // `Burn` in `ACTOR_TABLES` and rode the registry loop at step 5. Its
        // ruling is recorded in `ui/nostr.md` § Key succession and rotation and
        // its reasoning at its registry entry; the hand-written statement that
        // used to sit here was removed with the other ten.
        //
        // Leg 2's third bridge — the Bluesky link, which burns whole (ruled
        // 2026-08-15) — went the same way: all five of its tables are
        // declared `Burn` in `ACTOR_TABLES` and ride the same loop, by
        // construction rather than by a hand-written statement.
        //
        // Between them these two bridges are why the loop dispatches on the
        // declared `ActorKey`: `nostr_zap_signers` and the five `bluesky_*`
        // tables are the hex-keyed ones, and binding a raw blob against their
        // TEXT columns is not an error — it matches nothing and reports success.
        // See `execute_burn_legs`.)

        // 5e. Clear a catch-all designation that named the retired identity.
        //     Not ownership — this is admin-plane routing, and `mail_domains`
        //     is keyed by domain — which is exactly why the reference is
        //     CLEARED rather than followed: moving it would let one user's
        //     private ceremony silently re-aim a deployment-wide decision an
        //     admin made, possibly at an actor who does not own the domain.
        //
        //     ⚠ Leaving it is not the harmless black-hole it first appears to
        //     be, and the reason is subtle enough to be worth stating here.
        //     The alias re-point closes *retrieval* for the account's own
        //     address; it does nothing for *sealing* on this path. Unmatched
        //     mail resolves through `catch_all_actor_id` straight to the
        //     retired actor, whose `actor_mls_pubkeys` row deliberately
        //     survives (that survival is what arms the `succession_pending`
        //     tempfail for the address that *did* move). So
        //     `get_recipient_mail_seal_key(retired)` returns `Some(k)`, and
        //     that branch reports `succession_pending = false`
        //     unconditionally — the tempfail only fires on `None`. Every
        //     future catch-all delivery would keep being freshly sealed to a
        //     key derived from the MSEK the thief read, with no end date,
        //     which is strictly worse than the residuals the ceremony accepts
        //     elsewhere (those decay because sealing stops at the ceremony).
        //
        //     Clearing also makes the failure loud: with no catch-all,
        //     unmatched mail is rejected where an admin can see it instead of
        //     vanishing into an identity nobody can read. Nothing
        //     irrecoverable dies — the designation is admin-recreatable in one
        //     gesture (`succession-aftermath.md` § Re-key scope, the
        //     in-scope-set blockquote).
        tx.execute(
            "UPDATE mail_domains SET catch_all_actor_id = NULL,
                                      catch_all_cleared_by_succession_at = ?2
              WHERE catch_all_actor_id = ?1",
            rusqlite::params![old, now_epoch_millis()],
        )
        .context("clear a catch-all naming the retired identity")?;

        // 5f. The forwarding family. See the leg's doc.
        corpus_rows_repointed += rule_the_forwarding_family(&tx, &old, &new)?;

        // 5f-bis. The guardian side of the family plane. Its twelve
        //     supervised-side siblings rode the
        //     registry loop above as plain moves; these are the columns naming
        //     the OTHER party, which answer a different ceremony.
        corpus_rows_repointed += rule_the_guardian_family(&tx, &old, &new)?;

        // 5f-ter. The paying reader's side of the entitlement plane. The
        //     author's side is the eight
        //     tables of the `subscription_tiers` coupled family and moved with
        //     the family executor above; these are the columns naming the reader
        //     who paid, which answer that reader's own ceremony.
        corpus_rows_repointed += rule_the_paying_readers_entitlements(&tx, &old, &new)?;

        // 5f-quinquies-bis. The per-caller labeler publication quota.
        //     `labelers`' registry column
        //     (`publisher_actor`) is the artifact's signing key and STAYS; this
        //     is the second column on the same row, the enrolled identity a
        //     security-review DoS bound counts. See the leg's doc.
        corpus_rows_repointed += rule_the_labeler_publication_quota(&tx, &old, &new)?;

        // 5f-sexies. The three COUNTERPARTY references — the columns that name
        //     the other person in somebody else's row. First ruling on this
        //     axis whose damage lands on a
        //     third party rather than on either party to the ceremony: every
        //     consumer counts or matches PEOPLE, so a stale reference makes the
        //     nest believe there are two humans where there is one. See the
        //     leg's doc.
        corpus_rows_repointed += rule_the_counterparty_references(&tx, &old, &new)?;

        // 5f-septies. A hand-attached label's writer columns — the upsert key
        //     its author revises it through. See the leg's doc.
        corpus_rows_repointed += rule_the_label_writer_columns(&tx, &old, &new)?;

        // 5f-quinquies. The creator's claim ledger — the third table of the
        //     entitlement plane, and the one that MOVES rather than burns,
        //     because its rows are the payment audit trail the schema keeps
        //     forever. Every unredeemed code is voided as it travels: a claim is
        //     a bearer credential no app can revoke, so the ceremony is the only
        //     place a thief-minted one can die. See the leg's doc.
        corpus_rows_repointed += rule_the_creators_claim_ledger(&tx, &old, &new)?;

        // 5f-septies. The floor roster's seats. The one membership plane whose
        //     authority the nest
        //     itself holds, and so the one where nothing but this transaction
        //     can seat the successor. See the leg's doc.
        corpus_rows_repointed += rule_the_floor_roster_seats(&tx, &old, &new)?;

        // 5l. The retired identity's still-armed scheduled actions. Unlike
        //     every leg above, this one is not about where rows LIVE — the rows
        //     stay put — but about a timer the ceremony would otherwise leave
        //     running. See the leg's doc.
        disarm_the_retired_identitys_scheduled_actions(&tx, &old)?;

        tx.commit().context("commit succession")?;

        Ok(Ok(SuccessionApplied {
            handle,
            was_admin,
            capability_grants_revoked,
            cancelled_pending_replacement,
            corpus_rows_repointed,
            succeeded_at: now,
        }))
    }

    /// Record a succession this nest learned from a **peer**, and re-point the
    /// residue it holds about the old identity (`identity-succession.md:81`
    /// § Propagation → *Federation peers*).
    ///
    /// The peer half of [`Self::record_succession`], and deliberately writing the
    /// **same** `actor_successions` table. The table is link-shaped — every
    /// home-only consequence (the account, the handle, the admin role, the
    /// revocations) lives in that method, not in the schema — so sharing it is
    /// not a shortcut; it is what makes three things structural rather than
    /// re-implemented:
    ///
    /// - the supersession consult on relayed content
    ///   (`routes::deliver_inbox_payload_core`) starts refusing the stolen key
    ///   here with no new code, which is the whole point of propagating;
    /// - [`Self::succession_path`] serves the learned link onward, so a peer
    ///   *helps* propagate instead of being a dead end;
    /// - `old_actor_id PRIMARY KEY` keeps first-succession-wins structural on
    ///   peers exactly as on the home nest, so a retired kit cannot re-point a
    ///   remote identity a second time.
    ///
    /// **A statement naming a locally-homed identity is refused**
    /// ([`SuccessionRefusal::OldIsLocal`]). This is load-bearing rather than
    /// defensive: a home nest learns its own successions only through
    /// `fauna.recovery.succession.submit`, which runs the account transaction.
    /// Landing a peer-relayed row for a local account would write the refusal
    /// *without* re-pointing the account — stranding a local user behind a
    /// `superseded` error that names a successor with no account here, exactly
    /// the client-unreachable state `nest/common.md` § Client-state
    /// recoverability forbids.
    ///
    /// Verification is the caller's (`fauna_core::recovery::
    /// verify_succession_against_chain` against the delivered registration
    /// chain). As with the home-nest twin, this module never checks a signature.
    /// Fold the predecessor's `contacts` edges into the successor's, one rule
    /// for every succession leg (ceremony and peer statement; both sides).
    /// Where the same holder ends up with an edge to **both**
    /// identities, the surviving row keeps the higher-trust-impact status:
    /// `blocked` absorbs everything, else `confirmed` > `accepted` >
    /// `pending` (`succession-aftermath.md` § Propagation → Contacts). A
    /// succession re-points an identity; it never widens what its holder
    /// reaches — in particular it can never silently dissolve a block, in
    /// either direction of the collision.
    ///
    /// The merge runs BEFORE the move: `(actor_id, peer_id)` is the table's
    /// primary key, so merging first is what makes the bare `UPDATE` total.
    ///
    /// Returns the rows re-pointed by the final move; the merged survivor and
    /// the dropped predecessor rows are preparation, not the move.
    fn collapse_contact_edges(
        tx: &rusqlite::Transaction<'_>,
        old: &[u8],
        new: &[u8],
        side: ContactCollapseSide,
    ) -> Result<usize> {
        const RANK: &str = "CASE {s} WHEN 'blocked' THEN 3 WHEN 'confirmed' THEN 2 WHEN 'accepted' THEN 1 ELSE 0 END";
        let (moving, holder) = side.columns();
        let rank_own = RANK.replace("{s}", "contacts.status");
        let rank_pred = RANK.replace("{s}", "p.status");
        tx.execute(
            &format!(
                "UPDATE contacts SET status = (
                        SELECT p.status FROM contacts p
                         WHERE p.{moving} = ?1 AND p.{holder} = contacts.{holder})
                  WHERE {moving} = ?2
                    AND {holder} IN (SELECT {holder} FROM contacts WHERE {moving} = ?1)
                    AND ({rank_own})
                      < (SELECT {rank_pred} FROM contacts p
                          WHERE p.{moving} = ?1 AND p.{holder} = contacts.{holder})"
            ),
            rusqlite::params![old, new],
        )
        .context("keep the stricter status on a colliding contact edge")?;
        tx.execute(
            &format!(
                "DELETE FROM contacts
                  WHERE {moving} = ?1
                    AND {holder} IN (SELECT {holder} FROM contacts WHERE {moving} = ?2)"
            ),
            rusqlite::params![old, new],
        )
        .context("drop superseded contact edges the successor already holds")?;
        tx.execute(
            &format!("UPDATE contacts SET {moving} = ?2 WHERE {moving} = ?1"),
            rusqlite::params![old, new],
        )
        .context("re-point contact edges")
    }

    pub async fn record_peer_succession(
        &self,
        old_actor_id: &[u8],
        new_actor_id: &[u8],
        statement: &[u8],
        seq: u64,
    ) -> Result<std::result::Result<PeerSuccessionApplied, SuccessionRefusal>> {
        if old_actor_id.len() != ACTOR_ID_LEN || new_actor_id.len() != ACTOR_ID_LEN {
            return Err(anyhow!("actor ids must be {ACTOR_ID_LEN} bytes"));
        }
        if old_actor_id == new_actor_id {
            return Err(anyhow!("an identity cannot succeed itself"));
        }
        if statement.is_empty() {
            return Err(anyhow!("succession statement must not be empty"));
        }
        let old = old_actor_id.to_vec();
        let new = new_actor_id.to_vec();
        let statement = statement.to_vec();
        let now = now_epoch_secs();

        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction().context("begin tx")?;

        // Read inside the transaction, like the home-nest twin: a concurrent
        // registration of the same identity cannot slip between the check and
        // the insert.
        let local: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM users WHERE actor_id = ?1",
                rusqlite::params![old],
                |row| row.get(0),
            )
            .optional()
            .context("read local account")?;
        if local.is_some() {
            return Ok(Err(SuccessionRefusal::OldIsLocal));
        }

        let already: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM actor_successions WHERE old_actor_id = ?1",
                rusqlite::params![old],
                |row| row.get(0),
            )
            .optional()
            .context("read existing succession")?;
        if already.is_some() {
            return Ok(Err(SuccessionRefusal::AlreadySucceeded));
        }

        // One predecessor per identity (ruling (8)(j)(1)): the first-landed link
        // into the successor stands, local or learned.
        let fan_in: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM actor_successions WHERE new_actor_id = ?1",
                rusqlite::params![new],
                |row| row.get(0),
            )
            .optional()
            .context("read existing link into the successor")?;
        if fan_in.is_some() {
            return Ok(Err(SuccessionRefusal::NewAlreadySucceeded));
        }

        tx.execute(
            "INSERT INTO actor_successions
                (old_actor_id, new_actor_id, statement, seq, succeeded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![old, new, statement, seq as i64, now],
        )
        .context("record peer succession")?;

        // Re-point the remote-identity residue. Both tables key on the actor id,
        // so both need the same two-step shape: the successor may already occupy
        // the row (they were separately invited, or a second statement re-points
        // onto an existing edge), and the PK would refuse a blind UPDATE. One of
        // the two colliding rows must go, and *which* one is a security question,
        // answered per table below. What both answers share: the row that
        // survives is re-pointed, so the superseded key is left holding nothing —
        // the access this propagation exists to remove.

        // **A succession re-points the identity and never the binding**. The collapse of a collision IS a
        // rebind of `home_nest_id`, and `federation.md` § Cross-nest shared
        // folders + channel append enumerates every power that may perform one:
        // the claimant, or — while the grant is unconfirmed — a rostered actor.
        // A succession is neither, and its statement carries no home-nest field
        // at all (`identity-succession.md` § The succession statement (wire)), so
        // it has nothing of its own to install. Therefore the **successor's**
        // colliding row is the one dropped, and the predecessor's established
        // binding is what the UPDATE below carries forward, stamp intact.
        //
        // Dropping the predecessor's row instead — which is what this did until
        // a later fix — made the collapse a laundering path for the one power the
        // bullet withholds: `welcome_deliver_core` gates a FIRST grant on
        // `claim_permits` alone, whose `Unclaimed` arm admits everyone, so any
        // caller who knew the 32-byte channel id could plant a grant for the
        // (public) successor id, let the succession delete the genuine pinned row
        // in its favour, and then earn the pin themselves on their own first
        // served call — after which, on an unclaimed conversation channel, no
        // caller of any kind could move it back (`nest/common.md`
        // § Client-state recoverability).
        //
        // Legitimate re-homing through a succession is not lost, only moved onto
        // the documented path: the survivor keeps the `confirmed_at` it had, so
        // where the predecessor's grant was unconfirmed any rostered actor's
        // re-invite still moves it ([`RebindPower::Standing`]), and where it was
        // confirmed the claimant is the one mover — exactly the bullet's rule,
        // rather than an exception carved for the succession path.
        //
        // **The grant rides the roster row** (`writer-signed-change-records.md`
        // ruling (8)(j)(3)–(4)). On a set's home nest this row IS the foreign
        // member's seat, and the seat moves in this transaction, so the
        // predecessor's `folder_member_access` rows move in it too — on
        // exactly the channels where the predecessor holds a roster row, which
        // is why this runs before the roster's own two-step renames them. The
        // collision rule is the reverse of the binding's and deliberately so:
        // only the set's owner mints a grant, so nobody can plant one, and the
        // grant naming the successor is the owner's later, more specific act —
        // it stands and the predecessor's is dropped. A grant the predecessor
        // holds on a channel with NO roster row is left where it is: it admits
        // nothing alone, and a grant never moves without its seat.
        let member_grants = {
            const MOVING_SEATS: &str = "SELECT channel_id FROM channel_foreign_members
                                         WHERE actor_id = ?1";
            tx.execute(
                &format!(
                    "DELETE FROM folder_member_access
                      WHERE actor_id = ?1
                        AND channel_id IN ({MOVING_SEATS})
                        AND channel_id IN (SELECT channel_id FROM folder_member_access
                                            WHERE actor_id = ?2)"
                ),
                rusqlite::params![old, new],
            )
            .context("drop the predecessor's grant where the successor holds its own")?;
            tx.execute(
                &format!(
                    "UPDATE folder_member_access SET actor_id = ?2
                      WHERE actor_id = ?1 AND channel_id IN ({MOVING_SEATS})"
                ),
                rusqlite::params![old, new],
            )
            .context("carry the member's grant with its roster row")?
        };
        let foreign_memberships = {
            Self::warn_on_discarded_foreign_bindings(&tx, &old, &new)?;
            tx.execute(
                "DELETE FROM channel_foreign_members
                  WHERE actor_id = ?2
                    AND channel_id IN (SELECT channel_id FROM channel_foreign_members
                                        WHERE actor_id = ?1)",
                rusqlite::params![old, new],
            )
            .context("drop the successor's colliding grant, which the succession cannot rebind")?;
            tx.execute(
                "UPDATE channel_foreign_members SET actor_id = ?2 WHERE actor_id = ?1",
                rusqlite::params![old, new],
            )
            .context("re-point foreign channel membership")?
        };

        // Contact edges keep their trust flags — the row is re-pointed, never
        // re-created as a stranger (`identity-succession.md:82`), and on a
        // collision the stricter status survives (the helper's rule).
        //
        // Deliberately NOT the membership arm's shape above: a contact edge is
        // the local account's own choice about a peer, not an authorization a
        // remote caller can plant, so the collapse is not the same laundering
        // shape — the successor's ROW survives here, with the predecessor's
        // status absorbed into it where it carried more. The published
        // succession may re-point who the edge names; it may not un-block them
        // behind the holder's back (`succession-aftermath.md` § Propagation →
        // Contacts).
        let contact_edges =
            Self::collapse_contact_edges(&tx, &old, &new, ContactCollapseSide::Peer)?;

        tx.commit().context("commit peer succession")?;

        Ok(Ok(PeerSuccessionApplied {
            foreign_memberships,
            member_grants,
            contact_edges,
        }))
    }

    /// Surface every collision whose two rows *disagree* about the member's home
    /// nest, before the collapse discards one of them.
    ///
    /// This is what is left of "refuse to collapse and surface it" once the
    /// primary key has its say: `(channel_id, actor_id)` admits exactly one row,
    /// so the two bindings cannot both be kept and there is no refusal to
    /// express — only a record that a binding some caller asserted was dropped
    /// unexercised. Agreeing rows are silent (the overwhelmingly common shape:
    /// the successor was re-welcomed at the same nest, and nothing is in
    /// dispute).
    ///
    /// Best-effort by construction — a read that fails must never abort a
    /// verified succession, whose whole point is to strip the superseded key.
    fn warn_on_discarded_foreign_bindings(
        tx: &rusqlite::Transaction<'_>,
        old: &[u8],
        new: &[u8],
    ) -> Result<()> {
        let mut stmt = match tx.prepare(
            "SELECT n.channel_id, n.home_nest_id, o.home_nest_id,
                    n.confirmed_at IS NOT NULL
               FROM channel_foreign_members n
               JOIN channel_foreign_members o
                 ON o.channel_id = n.channel_id AND o.actor_id = ?1
              WHERE n.actor_id = ?2
                AND n.home_nest_id <> o.home_nest_id",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(target: "recovery", "discarded-binding survey unavailable: {e}");
                return Ok(());
            }
        };
        let rows = stmt.query_map(rusqlite::params![old, new], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, bool>(3)?,
            ))
        });
        let Ok(rows) = rows else {
            return Ok(());
        };
        for row in rows.flatten() {
            let (channel, discarded, kept, was_confirmed) = row;
            tracing::warn!(
                target: "recovery",
                channel = %hex::encode(&channel),
                successor = %hex::encode(new),
                discarded_home_nest = %hex::encode(&discarded),
                kept_home_nest = %hex::encode(&kept),
                discarded_was_confirmed = was_confirmed,
                "a succession collapse discarded the successor's differing home-nest binding: a \
                 succession may re-point an identity but never rebind a grant, so the \
                 predecessor's binding is kept — re-home it with a rostered re-invite (or the \
                 claimant's, if the kept grant is confirmed)"
            );
        }
        Ok(())
    }

    /// The work list of the succession-ownership boot heal: every local
    /// predecessor paired with the **terminal** successor of its chain, newest
    /// hop first.
    ///
    /// Rows need no boot pass — [`Self::record_succession`] moves every one in
    /// its own transaction, and [`Self::record_peer_succession`] refuses a
    /// local old actor (`OldIsLocal`). What a crash can split is the
    /// filesystem half: the actor-scoped segment directories rename right
    /// after the commit (`succession_ownership::heal_segment_dirs`), and a
    /// crash between the two leaves them under the retired id. This list is
    /// what the boot heal walks to finish the rename; a pair whose directories
    /// already moved costs one no-op rename.
    ///
    /// The terminal walk (not the direct successor) is what lets a chain
    /// whose middle hop also crashed land in one step, cycle-guarded because
    /// a walk must not hang on a hand-edited table.
    pub async fn succession_heal_pairs(&self) -> Result<Vec<([u8; 32], [u8; 32])>> {
        let conn = self.conn.lock().await;

        // The full link map (terminal walks may cross hops whose old id is
        // not local), plus the processing list: local predecessors, newest
        // hop first.
        let mut forward: std::collections::HashMap<Vec<u8>, Vec<u8>> = Default::default();
        {
            let mut stmt = conn
                .prepare("SELECT old_actor_id, new_actor_id FROM actor_successions")
                .context("prepare link map")?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .context("read link map")?;
            for row in rows {
                let (old, new) = row.context("link row")?;
                forward.insert(old, new);
            }
        }
        let local_olds: Vec<Vec<u8>> = {
            let mut stmt = conn
                .prepare(
                    "SELECT s.old_actor_id FROM actor_successions s
                      JOIN users u ON u.actor_id = s.old_actor_id
                      ORDER BY s.succeeded_at DESC, s.rowid DESC",
                )
                .context("prepare local predecessors")?;
            let rows = stmt
                .query_map([], |row| row.get::<_, Vec<u8>>(0))
                .context("read local predecessors")?;
            rows.collect::<std::result::Result<_, _>>()
                .context("local predecessor row")?
        };

        let mut pairs = Vec::new();
        for old in local_olds {
            let mut seen = std::collections::HashSet::new();
            let mut terminal = old.clone();
            while let Some(next) = forward.get(&terminal) {
                if !seen.insert(terminal.clone()) {
                    break;
                }
                terminal = next.clone();
            }
            if terminal == old {
                continue;
            }
            let (Ok(old_arr), Ok(term_arr)) = (
                <[u8; 32]>::try_from(old.as_slice()),
                <[u8; 32]>::try_from(terminal.as_slice()),
            ) else {
                continue;
            };
            pairs.push((old_arr, term_arr));
        }
        Ok(pairs)
    }

    /// The succession recorded *from* `old_actor_id`, if it was succeeded.
    ///
    /// This is the enforcement consult: a `Some` means every ceremony that
    /// authenticates `old_actor_id` must refuse with `superseded`
    /// (`identity-succession.md:71`).
    pub async fn succession_for(&self, old_actor_id: &[u8]) -> Result<Option<SuccessionRow>> {
        let old = old_actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT old_actor_id, new_actor_id, statement, seq, succeeded_at
             FROM actor_successions WHERE old_actor_id = ?1",
            rusqlite::params![old],
            |row| {
                Ok(SuccessionRow {
                    old_actor_id: row.get(0)?,
                    new_actor_id: row.get(1)?,
                    statement: row.get(2)?,
                    seq: row.get::<_, i64>(3)? as u64,
                    succeeded_at: row.get(4)?,
                })
            },
        )
        .optional()
        .context("read succession")
    }

    /// `true` iff some identity succeeded **into** `new_actor_id` — i.e. this
    /// actor is a succession's successor (`succession-aftermath.md` § Re-key
    /// scope). `record_succession` refuses `NewAlreadyRegistered` when the
    /// successor id already has a `users` row, so this actor id genuinely
    /// never had a mail seal key of its own before the ceremony; it is not
    /// "cleared", it simply has not been provisioned yet. Consulted by
    /// `fetch_recipient_mls_pubkey_handler` to distinguish that expected
    /// pre-provisioning window — bounded by the successor's own first
    /// sign-in, not by anything the nest times out — from a recipient that
    /// was never onboarded at all (`smtp-server.md` § Error / tempfail
    /// strategy).
    pub async fn is_succession_new_actor(&self, new_actor_id: &[u8]) -> Result<bool> {
        let conn = self.conn.lock().await;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM actor_successions WHERE new_actor_id = ?1",
                rusqlite::params![new_actor_id],
                |row| row.get(0),
            )
            .optional()
            .context("read succession by new_actor_id")?;
        Ok(found.is_some())
    }

    /// When the succession that produced `new_actor_id` was **committed** —
    /// `actor_successions.succeeded_at`, in epoch seconds, served by
    /// `fauna.recovery.succession.status` to that actor and to no one else.
    ///
    /// The mirror of [`Self::is_succession_new_actor`], which answers the same
    /// question with less: this returns *when*, and it is the only read that
    /// does. The stamp otherwise reaches a client at exactly one moment — the
    /// submit reply — so a ceremony whose reply was lost could not classify
    /// what the succession carried across at all
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across → the declared residual).
    ///
    /// `None` for an actor that is not a successor — the honest answer,
    /// [`Self::succession_path`]'s non-oracle shape, and safe to serve because
    /// the caller can only ever ask about itself.
    ///
    /// `MAX`, defensively. `new_actor_id` is indexed but not UNIQUE, yet a nest
    /// holds one predecessor per identity (ruling (8)(j)(1)): the home leg
    /// refuses `NewAlreadyRegistered` and [`Self::record_peer_succession`]
    /// refuses `NewAlreadySucceeded`, so there is one row. Were a second ever
    /// found (a hand-edited table), a **later** bound over-marks (re-asks the
    /// owner about a rule they wrote themselves) and an **earlier** one
    /// under-marks (carries a thief's rule across unflagged) — the same
    /// fail-visible direction `SuccessionTime`'s deliberate round-up takes.
    pub async fn succession_committed_at(&self, new_actor_id: &[u8]) -> Result<Option<i64>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT MAX(succeeded_at) FROM actor_successions WHERE new_actor_id = ?1",
            rusqlite::params![new_actor_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .context("read succession commit stamp by new_actor_id")
        .map(Option::flatten)
    }

    /// The full chain of successions starting at `old_actor_id`, **oldest
    /// first** — the wire order `fauna.recovery.succession.lookup` promises.
    ///
    /// A peer holds whatever actor id it last saw, which may be several
    /// successions behind (`identity-succession.md:72` — "discovery must work
    /// *from* them"). Returning the whole path rather than one link costs the
    /// nest one indexed read per hop and saves the consumer a round trip per
    /// hop, and it is not a trust shortcut: every link carries its own
    /// verbatim statement and is verified independently against the RecoveryKey
    /// registered for the identity *it* succeeds (`identity-succession.md:42`).
    ///
    /// Empty for an identity that was never succeeded — the honest answer, and
    /// the same non-oracle shape `registration.chain` uses.
    pub async fn succession_path(&self, old_actor_id: &[u8]) -> Result<Vec<SuccessionRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT old_actor_id, new_actor_id, statement, seq, succeeded_at
             FROM actor_successions WHERE old_actor_id = ?1",
        )?;
        let mut path = Vec::new();
        let mut cursor = old_actor_id.to_vec();
        while path.len() < MAX_SUCCESSION_PATH {
            let row = stmt
                .query_row(rusqlite::params![cursor], |row| {
                    Ok(SuccessionRow {
                        old_actor_id: row.get(0)?,
                        new_actor_id: row.get(1)?,
                        statement: row.get(2)?,
                        seq: row.get::<_, i64>(3)? as u64,
                        succeeded_at: row.get(4)?,
                    })
                })
                .optional()
                .context("walk succession path")?;
            match row {
                Some(row) => {
                    cursor = row.new_actor_id.clone();
                    path.push(row);
                }
                None => break,
            }
        }
        Ok(path)
    }

    /// The landed statements whose chain **ends at** `new_actor_id`, verbatim
    /// and **oldest first** — what the actor-roster read carries on a writer's
    /// row (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(b) source (i)).
    ///
    /// [`Self::succession_path`] walked backward: it follows `new_actor_id`
    /// over every link this nest holds — its own accounts' and the ones
    /// [`Self::record_peer_succession`] learned — so it answers for a
    /// cross-nest member too, which [`local_predecessors`] (a `users` join)
    /// cannot.
    ///
    /// **Exactly one linear path, or less.** The reader proves a predecessor
    /// only from an unbroken chain into the member
    /// (`fauna_core::recovery::proven_predecessors_carried`), and one extra
    /// statement makes the whole carriage prove nothing. So:
    ///
    /// - at most [`fauna_core::recovery::MAX_VERIFIED_CHAIN_LEN`] links, the
    ///   ones **nearest** the member — a longer chain is cut at its old end,
    ///   and what is left still ends at the member;
    /// - one link per identity: a nest holds one predecessor per identity
    ///   (ruling (8)(j)(1) — `record_peer_succession` refuses a second link
    ///   into a successor), so each step reads one row. If a second row is ever
    ///   found (a hand-edited table) the walk logs an error and stops there:
    ///   it carries the unambiguous links between that identity and the member
    ///   and never picks a branch for the reader, who admits no row on a link
    ///   the nest chose for it;
    /// - cycle-guarded like the other walks, though the write paths cannot
    ///   make one.
    ///
    /// Empty for an identity nobody succeeded into.
    pub async fn succession_statements_into(
        &self,
        new_actor_id: &[u8; 32],
    ) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT old_actor_id, statement
                   FROM actor_successions WHERE new_actor_id = ?1 LIMIT 2",
            )
            .context("prepare succession walk into an identity")?;
        let mut seen: std::collections::HashSet<Vec<u8>> =
            [new_actor_id.to_vec()].into_iter().collect();
        let mut cursor = new_actor_id.to_vec();
        let mut chain = Vec::new();
        while chain.len() < fauna_core::recovery::MAX_VERIFIED_CHAIN_LEN {
            let links = stmt
                .query_map(rusqlite::params![cursor], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .context("walk successions into an identity")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("succession link row")?;
            // No link: the chain's old end. Two (the `LIMIT 2` exists only to
            // detect this): the invariant is broken — stop, pick neither.
            if links.len() > 1 {
                tracing::error!(
                    target: "recovery",
                    "two successions name one successor; the walk stops there"
                );
            }
            let Ok([(old, statement)]) = <[(Vec<u8>, Vec<u8>); 1]>::try_from(links) else {
                break;
            };
            if !seen.insert(old.clone()) {
                break;
            }
            chain.push(statement);
            cursor = old;
        }
        chain.reverse();
        Ok(chain)
    }

    /// The owed nests `actor_id` is the one to settle: every entry kept for an
    /// identity on its predecessor path, the earliest hop first and, within a
    /// hop, the oldest pairing first.
    ///
    /// Empty for an actor that succeeded nobody. Empty too for an actor that
    /// has itself been succeeded: the account belongs to its successor now,
    /// which is served these same entries through its own path, and the
    /// retired key is the one a thief may hold.
    pub async fn owed_nests_for_successor(&self, actor_id: &[u8; 32]) -> Result<Vec<OwedNestRow>> {
        let conn = self.conn.lock().await;
        let mut predecessors = live_successor_predecessors(&conn, actor_id)?;
        // The walk returns the nearest hop first; the list is served from the
        // chain's first identity forward.
        predecessors.reverse();
        let mut stmt = conn.prepare(
            "SELECT old_actor_id, nest_id, nest_url FROM succession_owed_nests
              WHERE old_actor_id = ?1
              ORDER BY created_at ASC, rowid ASC",
        )?;
        let mut owed = Vec::new();
        for old in predecessors {
            let rows = stmt
                .query_map(rusqlite::params![&old[..]], |row| {
                    Ok(OwedNestRow {
                        old_actor_id: row.get(0)?,
                        nest_id: row.get(1)?,
                        nest_url: row.get(2)?,
                    })
                })
                .context("read owed nests")?;
            for row in rows {
                owed.push(row.context("owed nest row")?);
            }
        }
        Ok(owed)
    }

    /// Clear the owed nest `(old_actor_id, nest_id)` on behalf of `actor_id`.
    ///
    /// `Ok(false)` when `actor_id` is not the live successor of
    /// `old_actor_id` — nothing is deleted, and the caller refuses. `Ok(true)`
    /// otherwise, whether or not an entry was there: settling is idempotent.
    pub async fn settle_owed_nest(
        &self,
        actor_id: &[u8; 32],
        old_actor_id: &[u8; 32],
        nest_id: &[u8],
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        if !live_successor_predecessors(&conn, actor_id)?.contains(old_actor_id) {
            return Ok(false);
        }
        conn.execute(
            "DELETE FROM succession_owed_nests WHERE old_actor_id = ?1 AND nest_id = ?2",
            rusqlite::params![&old_actor_id[..], nest_id],
        )
        .context("settle owed nest")?;
        Ok(true)
    }

    /// The recovery chain head this nest last verified for a **foreign**
    /// identity — the `known` anchor `verify_registration_chain` consults, so a
    /// chain served later must extend rather than replace what this nest saw.
    pub async fn foreign_recovery_head(
        &self,
        actor_id: &[u8],
    ) -> Result<Option<ForeignRecoveryHead>> {
        let actor = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT recovery_pubkey, seq, anchor_nest_id FROM foreign_recovery_heads WHERE actor_id = ?1",
            rusqlite::params![actor],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()
        .context("read foreign recovery head")?
        .map(|(pubkey, seq, anchor)| {
            let recovery_pubkey = <[u8; 32]>::try_from(pubkey.as_slice())
                .map_err(|_| anyhow!("stored recovery pubkey is not 32 bytes"))?;
            let anchor_nest_id = <[u8; 32]>::try_from(anchor.as_slice())
                .map_err(|_| anyhow!("stored anchor nest id is not 32 bytes"))?;
            Ok(ForeignRecoveryHead {
                recovery_pubkey,
                seq,
                anchor_nest_id,
            })
        })
        .transpose()
    }

    /// Record (or advance) the verified chain head for a foreign identity, and
    /// **pin the anchor nest identity write-once**.
    ///
    /// **Monotonic head:** a write whose `seq` does not advance past the stored
    /// one leaves the head untouched, mirroring the chain's own append rule — so
    /// no caller ordering can regress the head to a state a thief's re-mint would
    /// satisfy. **Write-once anchor:** `anchor_nest_id` is set on the first write
    /// and never rewritten — a later re-invite that rewrites the identity's
    /// oldest residue binding cannot move the endpoint the pull is willing to
    /// trust. The caller has already proven the anchor
    /// identity and verified the chain; this is the storage twin of both.
    pub async fn record_foreign_recovery_head(
        &self,
        actor_id: &[u8],
        recovery_pubkey: &[u8; 32],
        seq: u64,
        anchor_nest_id: &[u8; 32],
    ) -> Result<()> {
        if actor_id.len() != ACTOR_ID_LEN {
            return Err(anyhow!("actor ids must be {ACTOR_ID_LEN} bytes"));
        }
        let actor = actor_id.to_vec();
        let pubkey = recovery_pubkey.to_vec();
        let anchor = anchor_nest_id.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        // The head fields advance only when `seq` grows (the `CASE` guards keep
        // them frozen otherwise), while `anchor_nest_id` is write-once: the
        // conflict arm never names it.
        conn.execute(
            "INSERT INTO foreign_recovery_heads (actor_id, recovery_pubkey, seq, anchor_nest_id, learned_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(actor_id) DO UPDATE SET
                 recovery_pubkey = CASE WHEN excluded.seq > foreign_recovery_heads.seq
                                        THEN excluded.recovery_pubkey ELSE foreign_recovery_heads.recovery_pubkey END,
                 seq             = MAX(foreign_recovery_heads.seq, excluded.seq),
                 learned_at      = CASE WHEN excluded.seq > foreign_recovery_heads.seq
                                        THEN excluded.learned_at ELSE foreign_recovery_heads.learned_at END",
            rusqlite::params![actor, pubkey, seq as i64, anchor, now],
        )
        .context("record foreign recovery head")?;
        Ok(())
    }
}

/// A stored [`CacheDb::foreign_recovery_head`] row — the last chain head this
/// nest verified for a remote identity, plus the anchor nest identity it proved
/// at first contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignRecoveryHead {
    pub recovery_pubkey: [u8; 32],
    pub seq: u64,
    pub anchor_nest_id: [u8; 32],
}

/// Every LOCAL identity this nest's recorded successions retired into
/// `actor_id` — the whole chain, walked back to its first identity, nearest
/// predecessor first. `actor_id` itself is not in the list.
///
/// The consumer is account deletion (`account-data-plane.md` § Nest-side
/// requirements item 1, *Deletion reaches the account's predecessors*): a
/// succession leaves every `Stay` row under the retired id, so the purge walk
/// runs once per id this returns.
///
/// **Local means the old id holds a `users` row.** A home-nest ceremony
/// ([`CacheDb::record_succession`]) requires one and keeps it, handle-less, so
/// every hop of a local chain passes; a peer ceremony
/// ([`CacheDb::record_peer_succession`]) refuses a locally-homed old id, so
/// its retired identity is someone this nest only ever knew as a remote actor.
///
/// **A retried deletion finds the same chain because of ORDER, not because
/// the rows are kept.** `actor_successions` (`Policy::Retain`) outlives every
/// deletion; the predecessors' `users` rows do not — `CacheDb::delete_user`
/// deletes the whole chain's rows in one transaction, and
/// `pending_actions::finalize_user_deletion` runs it AFTER the purge walk. So
/// a deletion that dies before `delete_user` still holds every row this join
/// needs and re-walks the chain, and one that dies after it has nothing left
/// under those ids to walk. `delete_user` itself resolves the chain through
/// this function inside that transaction, before it deletes the first row.
///
/// Cycle-guarded, like `succession_heal_pairs`'s forward walk: a cycle is
/// unrepresentable through the write paths, but a walk must not hang on a
/// hand-edited table.
pub(super) fn local_predecessors(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
) -> Result<Vec<[u8; 32]>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.old_actor_id FROM actor_successions s
               JOIN users u ON u.actor_id = s.old_actor_id
              WHERE s.new_actor_id = ?1",
        )
        .context("prepare local predecessor walk")?;
    let mut seen: std::collections::HashSet<[u8; 32]> = [*actor_id].into_iter().collect();
    let mut frontier = vec![*actor_id];
    let mut chain = Vec::new();
    while let Some(id) = frontier.pop() {
        let olds = stmt
            .query_map(rusqlite::params![&id[..]], |row| row.get::<_, Vec<u8>>(0))
            .context("read local predecessors")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("local predecessor row")?;
        for old in olds {
            let Ok(old) = <[u8; 32]>::try_from(old.as_slice()) else {
                continue;
            };
            if seen.insert(old) {
                chain.push(old);
                frontier.push(old);
            }
        }
    }
    Ok(chain)
}

/// [`local_predecessors`] of `actor_id` when it is the identity its account
/// currently belongs to, and nothing when it has itself been succeeded — the
/// one rule both owed-nest doors share for "the successor alone".
fn live_successor_predecessors(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
) -> Result<Vec<[u8; 32]>> {
    let retired: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM actor_successions WHERE old_actor_id = ?1",
            rusqlite::params![&actor_id[..]],
            |row| row.get(0),
        )
        .optional()
        .context("read whether the caller was itself succeeded")?;
    if retired.is_some() {
        return Ok(Vec::new());
    }
    local_predecessors(conn, actor_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::bridge_routing::SubmissionQuotaOutcome;
    use crate::db::channels::{MAX_PUSH_TARGETS, RebindPower};
    use crate::db::mail_lists::{ListQuotaOutcome, ListSendCaps};

    const OLD: [u8; 32] = [0xA1; 32];
    const NEW: [u8; 32] = [0xB2; 32];
    const THIRD: [u8; 32] = [0xC3; 32];

    // === The foreign-key-coupled family executor ==========================
    //
    // A synthetic family, shaped exactly like the `subscription_tiers` one the
    // backlog must rule next: a parent keyed `(actor, name)` and a child whose
    // composite foreign key names both. `COUPLED_MOVE_FAMILIES` is empty until
    // a plane is ruled, so these drive `execute_coupled_family_moves_with`
    // through its injection seam rather than leaving the mechanism untested
    // until the day someone rules a plane on top of it.

    static FAM_PARENT: actor_tables::ActorTable = actor_tables::ActorTable {
        table: "fam_parent",
        column: "actor_id",
        key: actor_tables::ActorKey::Blob,
        policy: actor_tables::Policy::Purge,
        succession: actor_tables::Succession::Move(actor_tables::MoveShape::Plain),
        export: actor_tables::Export::Unreviewed,
    };
    static FAM_CHILD: actor_tables::ActorTable = actor_tables::ActorTable {
        table: "fam_child",
        column: "actor_id",
        key: actor_tables::ActorKey::Blob,
        policy: actor_tables::Policy::Purge,
        succession: actor_tables::Succession::Move(actor_tables::MoveShape::Plain),
        export: actor_tables::Export::Unreviewed,
    };
    static TEST_FAMILY: &[actor_tables::CoupledFamily] = &[actor_tables::CoupledFamily {
        name: "test family",
        tables: &["fam_parent", "fam_child"],
        reason: "a parent keyed (actor, name) and a child whose composite FK names both",
    }];

    fn resolve_test_family(table: &str) -> Option<&'static actor_tables::ActorTable> {
        match table {
            "fam_parent" => Some(&FAM_PARENT),
            "fam_child" => Some(&FAM_CHILD),
            _ => None,
        }
    }

    /// The coupled pair plus one uncoupled table, so a test can tell "the
    /// family parked" from "the whole transaction was lost".
    fn seed_coupled_family(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE fam_parent (
                 actor_id BLOB NOT NULL,
                 name     TEXT NOT NULL,
                 PRIMARY KEY (actor_id, name)
             );
             CREATE TABLE fam_child (
                 actor_id BLOB NOT NULL,
                 name     TEXT NOT NULL,
                 FOREIGN KEY (actor_id, name) REFERENCES fam_parent (actor_id, name)
             );
             CREATE TABLE fam_unrelated (actor_id BLOB NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fam_parent (actor_id, name) VALUES (?1, 'tier-a')",
            rusqlite::params![OLD.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fam_child (actor_id, name) VALUES (?1, 'tier-a')",
            rusqlite::params![OLD.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fam_unrelated (actor_id) VALUES (?1)",
            rusqlite::params![OLD.as_slice()],
        )
        .unwrap();
    }

    fn rows_on(conn: &rusqlite::Connection, table: &str, actor: &[u8]) -> i64 {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE actor_id = ?1"),
            rusqlite::params![actor],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// **The control that makes every other test in this group mean something.**
    /// Without deferral the family cannot move in either order — which is the
    /// whole reason the executor exists, and is asserted here rather than
    /// trusted from the goal doc.
    #[tokio::test]
    async fn a_coupled_family_cannot_move_without_deferral() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.lock().await;
        seed_coupled_family(&conn);

        for order in [["fam_parent", "fam_child"], ["fam_child", "fam_parent"]] {
            let tx = conn.unchecked_transaction().unwrap();
            let first = tx.execute(
                &format!("UPDATE {} SET actor_id = ?2 WHERE actor_id = ?1", order[0]),
                rusqlite::params![OLD.as_slice(), NEW.as_slice()],
            );
            assert!(
                first.is_err(),
                "moving {} first must abort: a foreign key crosses the leg",
                order[0]
            );
            tx.rollback().unwrap();
        }
    }

    /// The ceremony moves a coupled family whole, and the transaction commits.
    #[tokio::test]
    async fn the_ceremony_moves_a_coupled_family_whole() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.lock().await;
        seed_coupled_family(&conn);

        let tx = conn.unchecked_transaction().unwrap();
        let moved = execute_coupled_family_moves_with(
            &tx,
            OLD.as_slice(),
            NEW.as_slice(),
            TEST_FAMILY,
            resolve_test_family,
        )
        .expect("the coupled family must move under deferral");
        tx.commit().expect("the deferred commit must be clean");

        assert_eq!(moved, 2, "both members' rows are counted");
        assert_eq!(rows_on(&conn, "fam_parent", NEW.as_slice()), 1);
        assert_eq!(rows_on(&conn, "fam_child", NEW.as_slice()), 1);
        assert_eq!(rows_on(&conn, "fam_parent", OLD.as_slice()), 0);
    }

    /// **Deferral does not outlive its transaction — both arms — and it is not
    /// cleared by hand before then.**
    ///
    /// SQLite clears the pragma at `COMMIT` and at `ROLLBACK` alike, so neither
    /// the success path nor the error path can leave a connection with foreign
    /// keys quietly deferred. Within the transaction it stays on, because
    /// turning it off is what would swallow the check
    /// (`turning_deferral_off_would_swallow_the_violation`). A half-moved family
    /// is asserted to still fail at the commit: deferral widens the window, it
    /// does not disarm the constraint.
    #[tokio::test]
    async fn deferral_does_not_outlive_its_transaction() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.lock().await;
        seed_coupled_family(&conn);

        let deferred = |c: &rusqlite::Connection| -> i64 {
            c.query_row("PRAGMA defer_foreign_keys", [], |r| r.get(0))
                .unwrap()
        };

        let tx = conn.unchecked_transaction().unwrap();
        execute_coupled_family_moves_with(
            &tx,
            OLD.as_slice(),
            NEW.as_slice(),
            TEST_FAMILY,
            resolve_test_family,
        )
        .unwrap();
        assert_eq!(
            deferred(&tx),
            1,
            "deferral must still be on when the executor returns — clearing it \
             discards the pending check instead of performing it"
        );
        tx.commit().unwrap();
        assert_eq!(deferred(&conn), 0);

        // A family half-declared is a family half-moved: the child is left
        // naming a parent that no longer exists, and the COMMIT must refuse it.
        static HALF: &[actor_tables::CoupledFamily] = &[actor_tables::CoupledFamily {
            name: "half a family",
            tables: &["fam_parent"],
            reason: "the child is deliberately missing",
        }];
        let tx = conn.unchecked_transaction().unwrap();
        execute_coupled_family_moves_with(
            &tx,
            NEW.as_slice(),
            THIRD.as_slice(),
            HALF,
            resolve_test_family,
        )
        .unwrap();
        let refused = tx.commit();
        assert!(
            refused.is_err(),
            "a half-moved family must still fail — deferral moves the check to \
             the commit, it does not remove it"
        );
        assert_eq!(
            deferred(&conn),
            0,
            "and the pragma must not survive the failed commit"
        );
        assert_eq!(
            rows_on(&conn, "fam_parent", THIRD.as_slice()),
            0,
            "nothing of the refused transaction may be left applied"
        );
        assert_eq!(rows_on(&conn, "fam_parent", NEW.as_slice()), 1);
    }

    /// The plain loop **leaves a declared family alone** — the one line that
    /// keeps a family's rows inside the deferred unit instead of being moved
    /// twice, once outside it. Pinned over a real registry table
    /// (`account_aliases`, the leg whose absence started this whole axis) so
    /// that the skip is observable rather than merely written.
    #[tokio::test]
    async fn the_plain_loop_leaves_a_declared_family_to_its_own_arm() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO account_aliases
                (alias_id, actor_id, local_domain, kind, pattern, created_at)
             VALUES (x'a11a50', ?1, 'example.test', 'exact', 'someone', 1)",
            rusqlite::params![OLD.as_slice()],
        )
        .unwrap();

        static CLAIMED: &[actor_tables::CoupledFamily] = &[actor_tables::CoupledFamily {
            name: "a family that claims account_aliases",
            tables: &["account_aliases", "fam_parent"],
            reason: "test-local: the plain loop must not touch a claimed table",
        }];

        let tx = conn.unchecked_transaction().unwrap();
        execute_plain_moves_with(&tx, OLD.as_slice(), NEW.as_slice(), CLAIMED).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            rows_on(&conn, "account_aliases", OLD.as_slice()),
            1,
            "a claimed table must be left to the family arm — moving it here \
             takes it outside the deferred unit and re-introduces the abort"
        );

        // And the same table DOES move when no family claims it, so the test
        // above cannot pass for the wrong reason.
        let tx = conn.unchecked_transaction().unwrap();
        execute_plain_moves_with(&tx, OLD.as_slice(), NEW.as_slice(), &[]).unwrap();
        tx.commit().unwrap();
        assert_eq!(rows_on(&conn, "account_aliases", NEW.as_slice()), 1);
    }

    /// **Why `execute_coupled_family_moves_with` must never turn the pragma
    /// back off** — the tidy-up a future session will reach for, red-verified
    /// here so that the *absence* of that line is a tested property rather than
    /// a comment nobody has reason to believe.
    ///
    /// `PRAGMA defer_foreign_keys = OFF` does not resume checking the
    /// violations already outstanding; it discards them. The identical
    /// half-move that `deferral_does_not_outlive_its_transaction` proves the
    /// commit refuses is accepted here, silently, leaving an orphan that only
    /// `PRAGMA foreign_key_check` can find — worse than the ceremony abort this
    /// whole mechanism exists to prevent, because it is permanent and quiet.
    #[tokio::test]
    async fn turning_deferral_off_would_swallow_the_violation() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.lock().await;
        seed_coupled_family(&conn);

        let tx = conn.unchecked_transaction().unwrap();
        tx.execute_batch("PRAGMA defer_foreign_keys = ON;").unwrap();
        tx.execute(
            "UPDATE fam_parent SET actor_id = ?2 WHERE actor_id = ?1",
            rusqlite::params![OLD.as_slice(), NEW.as_slice()],
        )
        .unwrap();
        // The line the executor deliberately does not have.
        tx.execute_batch("PRAGMA defer_foreign_keys = OFF;")
            .unwrap();
        tx.commit()
            .expect("this is the hazard: the orphaning commit is ACCEPTED");

        let orphans: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            orphans, 1,
            "flipping deferral off must be shown to leave a real orphan — if this \
             ever reads 0, SQLite changed its behaviour and the executor's \
             no-restore rule should be re-derived rather than trusted"
        );
    }

    /// Give `actor` a hosted ATProto identity with both halves of the plane:
    /// the identity family (DID, sealed key blob, settings, preferences, one
    /// repo record, one blob, one retired-DID record) and the authority family
    /// (an app password, its session, an OAuth grant with its paired session
    /// family, a pending consent request, an authoring delegation).
    ///
    /// `tag` keeps `idx_atproto_identities_did`'s UNIQUE `did` distinct between
    /// two actors in one test.
    async fn seed_atproto_identity(db: &CacheDb, actor: &[u8; 32], tag: &str) {
        let conn = db.conn.lock().await;
        let a = actor.as_slice();
        conn.execute(
            "INSERT INTO atproto_identities
                (actor_id, method, status, did, user_rotation_pub, created_at, updated_at)
             VALUES (?1, 'plc', 'active', ?2, 'did:key:zDn-user', 1, 1)",
            rusqlite::params![a, format!("did:plc:{tag}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_identity_key_blobs (actor_id, blob, created_at)
             VALUES (?1, x'5ea1ed', 1)",
            rusqlite::params![a],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_account_settings
                (actor_id, external_apps_enabled, integration_level, updated_at)
             VALUES (?1, 1, 'hosted_full', 1)",
            rusqlite::params![a],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_preferences (actor_id, preferences, updated_at)
             VALUES (?1, x'a0', 1)",
            rusqlite::params![a],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_native_records
                (actor_id, collection, rkey, cid, record, created_at)
             VALUES (?1, 'app.bsky.feed.post', ?2, 'bafy', x'a0', 1)",
            rusqlite::params![a, format!("rkey-{tag}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_blobs (actor_id, cid, media_ref, created_at)
             VALUES (?1, ?2, x'00', 1)",
            rusqlite::params![a, format!("bafy-{tag}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_retired_identities
                (actor_id, method, did, user_rotation_pub, created_at, retired_at)
             VALUES (?1, 'plc', ?2, 'did:key:zDn-old', 1, 2)",
            rusqlite::params![a, format!("did:plc:retired-{tag}")],
        )
        .unwrap();

        // The authority half — the plane the Fauna identity-key refusal cannot
        // reach, so every row here is one a seed thief could have minted.
        conn.execute(
            "INSERT INTO atproto_app_credentials
                (actor_id, credential_id, label, verifier, dm_allowed, created_at)
             VALUES (?1, ?2, 'a label', 'verifier', 0, 1)",
            rusqlite::params![a, format!("cred-{tag}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_sessions
                (actor_id, session_id, plane, credential_id, created_at, expires_at)
             VALUES (?1, ?2, 'app-password', ?3, 1, 9999999999)",
            rusqlite::params![a, format!("sess-{tag}").as_bytes(), format!("cred-{tag}")],
        )
        .unwrap();
        // The grant and its session family share one id by construction.
        conn.execute(
            "INSERT INTO atproto_oauth_grants
                (actor_id, grant_id, client_id, scopes, created_at, issuer)
             VALUES (?1, ?2, 'https://app.example/client', 'atproto', 1, 'nest')",
            rusqlite::params![a, format!("grant-{tag}").as_bytes()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_sessions
                (actor_id, session_id, plane, created_at, expires_at)
             VALUES (?1, ?2, 'oauth', 1, 9999999999)",
            rusqlite::params![a, format!("grant-{tag}").as_bytes()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_consent_requests
                (consent_id, actor_id, code, client_id, scopes, created_at, expires_at)
             VALUES (?1, ?2, 'AAAA-BBBB', 'https://app.example/client', 'atproto', 1, 9999999999)",
            rusqlite::params![format!("consent-{tag}").as_bytes(), a],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_authoring_keys
                (actor_id, k_pub, k_secret_wrapped, cert, created_at)
             VALUES (?1, x'0b', x'5e', x'ce', 1)",
            rusqlite::params![a],
        )
        .unwrap();
    }

    /// `(identity-family rows, authority rows)` resting on `actor` — the two
    /// halves this plane rules in opposite directions.
    async fn atproto_counts(db: &CacheDb, actor: &[u8; 32]) -> (i64, i64) {
        let conn = db.conn.lock().await;
        let a = actor.as_slice();
        let q = |sql: &str| -> i64 {
            conn.query_row(sql, rusqlite::params![a], |r| r.get(0))
                .unwrap()
        };
        let identity = q("SELECT COUNT(*) FROM atproto_identities WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_identity_key_blobs WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_account_settings WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_preferences WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_native_records WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_blobs WHERE actor_id = ?1");
        let authority = q("SELECT COUNT(*) FROM atproto_app_credentials WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_sessions WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_oauth_grants WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_consent_requests WHERE actor_id = ?1")
            + q("SELECT COUNT(*) FROM atproto_authoring_keys WHERE actor_id = ?1");
        (identity, authority)
    }

    /// **The ceremony's hosted-PDS legs, and the property no data-driven gate
    /// can state:** the plane splits in two directions at once — the identity
    /// follows the account, every credential that authenticates *without* the
    /// Fauna identity key dies.
    ///
    /// The registry sweep does assert each table's own observable, so the
    /// directions are covered table by table. What it cannot say is that the two
    /// halves are one ruling: that burning the authority half is what makes
    /// moving the identity half safe. Left alone, an app password a seed thief
    /// minted goes on authenticating to the successor's own repo through the
    /// ceremony meant to end the theft — the `account_aliases` failure exactly,
    /// on the one plane in this codebase that does not authenticate as the actor.
    #[tokio::test]
    async fn the_ceremony_moves_the_hosted_did_and_burns_every_pds_credential() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_atproto_identity(&db, &OLD, "old").await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            atproto_counts(&db, &OLD).await,
            (0, 0),
            "nothing may rest on the retired identity: the identity family moved \
             and the authority family burned"
        );
        let (identity, authority) = atproto_counts(&db, &NEW).await;
        assert_eq!(
            identity, 6,
            "the DID, its sealed keys, settings, preferences, repo and blobs must \
             follow the account — burning them would take nothing from a thief who \
             already holds the client-side senior rotation key, and would leave a \
             live hosted DID no app can see, manage or retire"
        );
        assert_eq!(
            authority, 0,
            "no app password, OAuth grant, refresh family, pending consent request \
             or authoring delegation may reach the successor: each is standing \
             authority a seed thief could have minted, honoured on its own terms"
        );

        // The one member that is neither: an append-only record of already-dead
        // DIDs, which moves because it is the account's own irrecoverable
        // provenance and merges cleanly by union.
        let conn = db.conn.lock().await;
        let retired: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM atproto_retired_identities WHERE actor_id = ?1",
                rusqlite::params![NEW.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retired, 1, "the retired-DID record follows its account");
    }

    /// A hosted identity provisioned the way production provisions one: real
    /// K-256 keys sealed to `bridge_pk`, the blob and its published halves
    /// written as the one pair, then minted. (`seed_atproto_identity` above
    /// stores placeholder bytes, which is all a row-counting test needs and
    /// nothing a test that OPENS the blob can use.)
    async fn provision_sealed_identity(
        db: &CacheDb,
        actor: &[u8; 32],
        bridge_pk: &[u8; 32],
        did: &str,
    ) {
        db.upsert_atproto_identity_intent(actor, "plc", "did:key:zDnaeUSERSENIOR")
            .await
            .unwrap();
        let keys =
            fauna_provisioning::atproto::seal_atproto_identity_for_provision(actor, bridge_pk)
                .unwrap();
        db.provision_atproto_identity_keys(
            actor,
            &keys.sealed_blob,
            &keys.signing_pub_did_key,
            &keys.rotation_pub_did_key,
        )
        .await
        .unwrap();
        db.record_atproto_minted(actor, did, Some("bafygenesis"))
            .await
            .unwrap();
    }

    /// What the `atproto.pds` bridge does with a `fetch_identity_key_blob`
    /// reply for `holder`: open the row's blob against the row's own published
    /// keys. Returns the actor id the sealed bundle NAMES, or the refusal.
    async fn open_as_the_bridge_would(
        db: &CacheDb,
        holder: &[u8; 32],
        bridge_sk: &[u8; 32],
    ) -> std::result::Result<Vec<u8>, fauna_mls::wrapped_blob::UnwrapError> {
        let row = db
            .get_atproto_identity(holder)
            .await
            .unwrap()
            .expect("the holder has an identity row");
        let bytes = db
            .get_atproto_identity_key_blob(holder)
            .await
            .unwrap()
            .expect("the holder has a sealed key blob");
        let blob = fauna_mls::wrapped_blob::AtprotoIdentityBlob::from_canonical_bytes(&bytes)?;
        let published = fauna_mls::wrapped_blob::AtprotoIdentityPublishedKeys {
            signing_pub_did_key: row.signing_pub.as_deref().unwrap_or_default(),
            rotation_pub_did_key: row.bridge_rotation_pub.as_deref().unwrap_or_default(),
        };
        fauna_mls::wrapped_blob::unseal_atproto_identity(&blob, bridge_sk, &published)
            .map(|bundle| bundle.actor_id.clone())
    }

    /// **The coupling the blob-held ruling named, closed from the succession
    /// side.** The bridge now refuses a sealed identity blob whose keys are not
    /// the ones the identity row publishes. The row and the blob move together
    /// and the bytes cannot be re-sealed, so both ids inside name the FIRST
    /// holder for the life of the DID — had the binding been an actor
    /// comparison, every hop below would strand the DID. It is a binding to the
    /// published keys, which ride the identity row across any number of hops.
    ///
    /// Two ceremonies, hop by hop: the account that ends up holding the DID is
    /// two removed from the actor the blob names.
    #[tokio::test]
    async fn a_hosted_dids_sealed_keys_open_for_the_successor_across_two_ceremonies() {
        let db = CacheDb::open_in_memory().unwrap();
        let (bridge_sk, bridge_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        seed_account(&db, &OLD, "alice").await;
        provision_sealed_identity(&db, &OLD, &bridge_pk, "did:plc:alice").await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            open_as_the_bridge_would(&db, &NEW, &bridge_sk)
                .await
                .expect("one hop: the moved blob opens for the successor"),
            OLD.to_vec(),
        );

        db.record_succession(&NEW, &THIRD, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            open_as_the_bridge_would(&db, &THIRD, &bridge_sk)
                .await
                .expect("two hops: it still opens, for an account two removed"),
            OLD.to_vec(),
            "the ids inside name the first holder for ever — provenance, never compared"
        );
        assert_eq!(atproto_identity_rows(&db, &OLD).await, 0);
        assert_eq!(atproto_identity_rows(&db, &NEW).await, 0);
    }

    /// **The substitution the binding exists to refuse.** A box hosting two
    /// DIDs holds two sealed blobs a routing bug could confuse. Each opens
    /// against its own row, and neither opens against the other's — which is
    /// the refusal, reproduced with real rows: before the published-key binding
    /// both crossed opens succeeded.
    #[tokio::test]
    async fn two_hosted_dids_sealed_keys_open_only_against_their_own_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let (bridge_sk, bridge_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        seed_account(&db, &OLD, "bob").await;
        seed_account(&db, &NEW, "alice").await;
        provision_sealed_identity(&db, &OLD, &bridge_pk, "did:plc:bob").await;
        provision_sealed_identity(&db, &NEW, &bridge_pk, "did:plc:alice").await;

        assert_eq!(
            open_as_the_bridge_would(&db, &OLD, &bridge_sk)
                .await
                .unwrap(),
            OLD.to_vec(),
            "each identity opens against its own row"
        );
        assert_eq!(
            open_as_the_bridge_would(&db, &NEW, &bridge_sk)
                .await
                .unwrap(),
            NEW.to_vec(),
        );

        // The row-level substitution: swap the two blobs under their rows.
        let blob_of_old = db
            .get_atproto_identity_key_blob(&OLD)
            .await
            .unwrap()
            .unwrap();
        let blob_of_new = db
            .get_atproto_identity_key_blob(&NEW)
            .await
            .unwrap()
            .unwrap();
        {
            let conn = db.conn.lock().await;
            for (holder, blob) in [(&OLD, &blob_of_new), (&NEW, &blob_of_old)] {
                conn.execute(
                    "UPDATE atproto_identity_key_blobs SET blob = ?2 WHERE actor_id = ?1",
                    rusqlite::params![holder.as_slice(), blob],
                )
                .unwrap();
            }
        }
        for holder in [&OLD, &NEW] {
            let err = open_as_the_bridge_would(&db, holder, &bridge_sk)
                .await
                .expect_err("another identity's whole blob must not open under this row");
            assert!(
                matches!(
                    err,
                    fauna_mls::wrapped_blob::UnwrapError::PublishedKeyMismatch(_)
                ),
                "refused by the published-key binding, not by accident: {err:?}"
            );
        }
    }

    async fn atproto_identity_rows(db: &CacheDb, actor: &[u8; 32]) -> i64 {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT (SELECT COUNT(*) FROM atproto_identities WHERE actor_id = ?1)
                  + (SELECT COUNT(*) FROM atproto_identity_key_blobs WHERE actor_id = ?1)",
            rusqlite::params![actor.as_slice()],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// A `users` row needs a tier that exists (`users.tier` is an FK), so every
    /// test that touches an account seeds one.
    async fn seed_account(db: &CacheDb, actor: &[u8; 32], handle: &str) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO tiers (name, max_inbox_bytes, max_storage_bytes, max_devices, max_blob_size)
             VALUES ('free', 1, 1, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (actor_id, tier, label, created_at, handle)
             VALUES (?1, 'free', 'the label', 1, ?2)",
            rusqlite::params![actor.as_slice(), handle],
        )
        .unwrap();
    }

    async fn handle_of(db: &CacheDb, actor: &[u8; 32]) -> Option<String> {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT handle FROM users WHERE actor_id = ?1",
            rusqlite::params![actor.as_slice()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
    }

    #[tokio::test]
    async fn an_unsucceeded_identity_has_no_row_and_an_empty_path() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(db.succession_for(&OLD).await.unwrap().is_none());
        assert!(db.succession_path(&OLD).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn is_succession_new_actor_true_only_for_a_registered_successor() {
        let db = CacheDb::open_in_memory().unwrap();
        // Neither identity has succeeded anything yet.
        assert!(!db.is_succession_new_actor(&OLD).await.unwrap());
        assert!(!db.is_succession_new_actor(&NEW).await.unwrap());

        seed_account(&db, &OLD, "alice").await;
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .expect("succession applies");

        // NEW is the successor; OLD is the predecessor — only NEW answers true.
        assert!(db.is_succession_new_actor(&NEW).await.unwrap());
        assert!(!db.is_succession_new_actor(&OLD).await.unwrap());
    }

    #[tokio::test]
    async fn a_succession_moves_the_account_and_the_handle_in_one_transaction() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;

        let applied = db
            .record_succession(&OLD, &NEW, b"statement-bytes", 7)
            .await
            .unwrap()
            .expect("succession applies");
        assert_eq!(applied.handle.as_deref(), Some("alice"));

        // The handle moved — this is what makes `by_handle` resolve the
        // successor with no extra lookup.
        assert_eq!(handle_of(&db, &OLD).await.as_deref(), Some(""));
        assert_eq!(handle_of(&db, &NEW).await.as_deref(), Some("alice"));

        // The successor inherited the account's tier and label.
        let conn = db.conn.lock().await;
        let (tier, label): (String, String) = conn
            .query_row(
                "SELECT tier, label FROM users WHERE actor_id = ?1",
                rusqlite::params![NEW.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(tier, "free");
        assert_eq!(label, "the label");
        drop(conn);

        // The enforcement consult sees it, with the statement bytes verbatim.
        let row = db.succession_for(&OLD).await.unwrap().unwrap();
        assert_eq!(row.new_actor_id, NEW.to_vec());
        assert_eq!(row.statement, b"statement-bytes".to_vec());
        assert_eq!(row.seq, 7);
    }

    #[tokio::test]
    async fn the_old_users_row_survives_so_history_keeps_its_fk_target() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // Deleting the old row would orphan every attribution row that names it
        // — succession never rewrites history (`identity-succession.md:100`).
        assert!(handle_of(&db, &OLD).await.is_some());
    }

    /// One corpus inventory shared by the transaction tests, so each asserts
    /// the same table set (`succession-aftermath.md` § Re-key scope, the
    /// ownership blockquote).
    async fn seed_corpus(db: &CacheDb, owner: &[u8; 32]) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO folders (name, actor_id, created_at) VALUES ('__config', ?1, 1)",
            rusqlite::params![owner.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folder_channel_claims (channel_id, claimed_by, claimed_at)
             VALUES (?2, ?1, 1)",
            rusqlite::params![owner.as_slice(), [0x11u8; 32].as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO segment_records (scope_id, kind, segment_id, record_cid, bucket)
             VALUES (?1, 'mail', 1, x'00', 'hot')",
            rusqlite::params![owner.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO web_files (actor_id, path, blob_hash, content_type, updated_at)
             VALUES (?1, '/index.html', x'22', 'text/html', 1)",
            rusqlite::params![owner.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO web_apex_actor (id, actor_id, set_at) VALUES (1, ?1, 1)",
            rusqlite::params![owner.as_slice()],
        )
        .unwrap();
        conn.execute(
            "UPDATE users SET inbox_bytes_used = 7, storage_bytes_used = 11 WHERE actor_id = ?1",
            rusqlite::params![owner.as_slice()],
        )
        .unwrap();
    }

    /// Rows the given actor owns, per corpus table — the counterpart of
    /// [`seed_corpus`].
    async fn corpus_rows_owned_by(db: &CacheDb, actor: &[u8; 32]) -> [i64; 5] {
        let conn = db.conn.lock().await;
        let q = |sql: &str| -> i64 {
            conn.query_row(sql, rusqlite::params![actor.as_slice()], |r| r.get(0))
                .unwrap()
        };
        [
            q("SELECT COUNT(*) FROM folders WHERE actor_id = ?1"),
            q("SELECT COUNT(*) FROM folder_channel_claims WHERE claimed_by = ?1"),
            q("SELECT COUNT(*) FROM segment_records WHERE scope_id = ?1"),
            q("SELECT COUNT(*) FROM web_files WHERE actor_id = ?1"),
            q("SELECT COUNT(*) FROM web_apex_actor WHERE actor_id = ?1"),
        ]
    }

    async fn quota_counters_of(db: &CacheDb, actor: &[u8; 32]) -> (i64, i64) {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT inbox_bytes_used, storage_bytes_used FROM users WHERE actor_id = ?1",
            rusqlite::params![actor.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn corpus_ownership_moves_in_the_succession_transaction() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_corpus(&db, &OLD).await;

        let applied = db
            .record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // Ownership moves in the transaction; the old identity owns nothing
        // afterwards, and the successor reaches every plane by ordinary
        // authenticated reads (`succession-aftermath.md` § Re-key scope).
        assert_eq!(corpus_rows_owned_by(&db, &OLD).await, [0; 5]);
        assert_eq!(corpus_rows_owned_by(&db, &NEW).await, [1; 5]);
        assert!(applied.corpus_rows_repointed >= 5);

        // The accounting follows the bytes — the handle-less old row cannot
        // strand quota nobody can reclaim.
        assert_eq!(quota_counters_of(&db, &OLD).await, (0, 0));
        assert_eq!(quota_counters_of(&db, &NEW).await, (7, 11));
    }

    #[tokio::test]
    async fn the_mailbox_address_moves_with_the_account() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        // The canonical `<handle>@<domain>` row mail-enable writes
        // (`ensure_canonical_handle_alias`), plus one the user made themselves.
        db.put_exact_alias("fauna.test", "alice", "exact", &OLD)
            .await
            .unwrap();
        db.put_exact_alias("fauna.test", "billing", "exact", &OLD)
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // An address is ownership, and on any nest that has claimed a mail
        // domain it is the ONLY resolution `validate_recipient` performs — the
        // handle→actor fallback beside it fires only for an *unregistered*
        // domain (`bridge_routing_handlers.rs` § validate_recipient). So a
        // stale row here is not cosmetic: bridge AUTH would keep resolving the
        // user's own address to the RETIRED actor, whose resting blobs the
        // successor cannot delete (`revoke_wrapped_mls_blob` refuses a target
        // that is not the caller) — the predecessor's mail password would go on
        // authenticating after the ceremony meant to end the theft, and inbound
        // mail would go on being sealed to the key the thief holds.
        assert_eq!(
            db.lookup_exact_alias("fauna.test", "alice").await.unwrap(),
            Some(NEW)
        );
        assert_eq!(
            db.lookup_exact_alias("fauna.test", "billing")
                .await
                .unwrap(),
            Some(NEW)
        );
    }

    #[tokio::test]
    async fn undelivered_deliveries_move_and_delivered_history_stays() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                // ⚠ `'undelivered'`, not `'pending'` — the same load-bearing
                // literal the production UPDATE carries, for the same reason
                // (`inbox.rs` writes 'undelivered' -> 'delivered' and has never
                // written 'pending'). This fixture said `'pending'` until
                // 2026-08-25, which is why it went green against the bug it
                // exists to catch: production matched 'pending' too, so the row
                // moved and the assertion held. The 2026-08-24 fix corrected the
                // production literal alone and left the fixture behind, turning
                // a test that had never guarded anything into a failing one.
                "INSERT INTO content_links
                    (link_type, actor_id, status, created_at, updated_at)
                 VALUES ('delivery', ?1, 'undelivered', 1, 1),
                        ('delivery', ?1, 'delivered', 1, 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        let undelivered_owner: Vec<u8> = conn
            .query_row(
                "SELECT actor_id FROM content_links WHERE status = 'undelivered'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let delivered_owner: Vec<u8> = conn
            .query_row(
                "SELECT actor_id FROM content_links WHERE status = 'delivered'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // An undelivered payload is access (the successor drains it with the
        // key material it holds); a delivered one is history and stays.
        assert_eq!(undelivered_owner, NEW.to_vec());
        assert_eq!(delivered_owner, OLD.to_vec());
    }

    // ── The mail plane's forwarding family ──────────────────────────
    //
    // Three tables that all answer the same question — *where does a copy of
    // this account's mail go next* — and none of which a plain `Move` rules
    // correctly. A seed thief holds the account key, so every one of them is a
    // tap the thief can arm before the ceremony and keep after it.

    async fn seed_forward_settings(db: &CacheDb, actor: &[u8; 32], target: Option<&str>, cap: i64) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO mail_account_settings (actor_id, forward_all_to, forward_per_hour, updated_at)
             VALUES (?1, ?2, ?3, 1)",
            rusqlite::params![actor.as_slice(), target, cap],
        )
        .unwrap();
    }

    async fn forward_settings_of(db: &CacheDb, actor: &[u8; 32]) -> Option<(Option<String>, i64)> {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT forward_all_to, forward_per_hour FROM mail_account_settings WHERE actor_id = ?1",
            rusqlite::params![actor.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
    }

    #[tokio::test]
    async fn the_ceremony_moves_mail_settings_and_disarms_the_forward_tap() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_forward_settings(&db, &OLD, Some("thief@evil.example"), 7).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The row itself is the account's own mail settings and follows it —
        // but `forward_all_to` is a live exfiltration tap the thief could have
        // set with the key they read (`fauna.bridges.set_forward_all_to` is
        // User-class), and the MTA reads it at the perimeter for every inbound
        // message (`fetch_recipient_forward_config`). A plain `Move` would hand
        // the successor a mailbox that silently copies every future message to
        // the thief, forever — the `nostr_bunker_apps` shape exactly: the row
        // re-points *and* the standing authority is revoked as it goes.
        assert_eq!(
            forward_settings_of(&db, &NEW).await,
            Some((None, 7)),
            "the settings row must land on the successor with the forward tap \
             disarmed and the user's own rate cap intact"
        );
        assert_eq!(forward_settings_of(&db, &OLD).await, None);
    }

    /// One row per succession disposition the action set can produce, so both
    /// path tests below judge the *class* rule rather than the one action that
    /// happened to be noticed first:
    ///   - `forward:` / `autoreply:` — outward emission, burns;
    ///   - `reject:` — narrowing that must survive, carrying attacker-authored
    ///     text that must not: moves disarmed;
    ///   - `fileinto:` / `discard` — local placement, moves intact.
    async fn seed_filters(db: &CacheDb, actor: &[u8; 32]) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO email_filters (owner, name, rules, action, created_at)
             VALUES (?1, 'exfiltrate', x'00', 'forward:thief@evil.example', 1),
                    (?1, 'speak for me', x'00',
                     'autoreply:24:I have moved\nWrite me at thief@evil.example', 1),
                    (?1, 'refuse them', x'00', 'reject:go away, says the thief', 1),
                    (?1, 'file it',    x'00', 'fileinto:Archive', 1),
                    (?1, 'bin it',     x'00', 'discard', 1)",
            rusqlite::params![actor.as_slice()],
        )
        .unwrap();
    }

    async fn filter_names_of(db: &CacheDb, actor: &[u8; 32]) -> Vec<String> {
        let conn = db.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT name FROM email_filters WHERE owner = ?1 ORDER BY name")
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![actor.as_slice()], |row| row.get(0))
            .unwrap();
        rows.collect::<std::result::Result<_, _>>().unwrap()
    }

    async fn filter_action_of(db: &CacheDb, actor: &[u8; 32], name: &str) -> Option<String> {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT action FROM email_filters WHERE owner = ?1 AND name = ?2",
            rusqlite::params![actor.as_slice(), name],
            |row| row.get(0),
        )
        .ok()
    }

    /// The class rule, asserted on the ceremony path. Both burn actions die,
    /// the `Reject` survives with the thief's words gone, and the local
    /// actions move untouched.
    #[tokio::test]
    async fn outward_emitting_filters_die_at_the_ceremony_and_the_rest_move() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_filters(&db, &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // User-authored rules are the successor's own configuration and move.
        // The two rules that make this nest *emit* attacker-authored content
        // under the successor's recovered identity are the same standing
        // authority the bunker leg burns, so they die rather than moving, and
        // re-creating either is bounded and visible.
        assert_eq!(
            filter_names_of(&db, &NEW).await,
            vec!["bin it", "file it", "refuse them"],
            "`forward:` and `autoreply:` are outward emission and burn; the \
             narrowing and placement rules are the successor's own config"
        );
        assert!(
            filter_names_of(&db, &OLD).await.is_empty(),
            "an outward-emitting rule left on the retired identity is \
             invisible residue carrying the thief's words — deleted, not stranded"
        );
        // Burning the `Reject` would *widen* who may reach the successor —
        // the `inbox_modes` asymmetry inverted — so the rule survives and only
        // the thief's reason text dies. A blank reason falls back to a generic
        // 550 message at the perimeter (`sanitizeRejectReason`).
        assert_eq!(
            filter_action_of(&db, &NEW, "refuse them").await.as_deref(),
            Some("reject:"),
            "the block must survive with the thief's reason cleared: burning \
             it would silently re-admit mail the user had arranged to refuse"
        );
        assert_eq!(
            filter_action_of(&db, &NEW, "file it").await.as_deref(),
            Some("fileinto:Archive"),
            "a local placement rule carries no outward authority and moves intact"
        );
    }

    /// One parked forward per (class, copy mode) shape the tree can hold. The
    /// burn is keyed on the persisted mode — never the class:
    /// - `m1` forward-all `copy`, `m3` per-rule `copy`: second copies → burn;
    /// - `m2` forwarder `redirect`, `m4` per-rule `redirect`: the only copy of
    ///   accepted mail → move;
    /// - `m5` forward-all `redirect`: forward-all follows the message, so when a
    ///   redirect rule fired the forward-all copy is the only one too → move;
    /// - `m6` forward-all with no mode (parked before the column) → move.
    async fn seed_parked_forwards(db: &CacheDb, actor: &[u8; 32]) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO forward_queue
                (actor_id, queued_at, source_message_id, original_sender,
                 destination_address, rule_id_or_forward_all, raw_message, copy_mode)
             VALUES (?1, 1, 'm1', 'sender@example.test', 'thief@evil.example',
                     'forward-all', x'00', 'copy'),
                    (?1, 1, 'm2', 'sender@example.test', 'oldaccount@example.test',
                     'forwarder', x'00', 'redirect'),
                    (?1, 1, 'm3', 'sender@example.test', 'thief@evil.example',
                     '42', x'00', 'copy'),
                    (?1, 1, 'm4', 'sender@example.test', 'elsewhere@example.test',
                     '43', x'00', 'redirect'),
                    (?1, 1, 'm5', 'sender@example.test', 'thief@evil.example',
                     'forward-all', x'00', 'redirect'),
                    (?1, 1, 'm6', 'sender@example.test', 'thief@evil.example',
                     'forward-all', x'00', NULL)",
            rusqlite::params![actor.as_slice()],
        )
        .unwrap();
    }

    async fn parked_forwards_of(db: &CacheDb, actor: &[u8; 32]) -> Vec<String> {
        let conn = db.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT source_message_id FROM forward_queue WHERE actor_id = ?1 ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![actor.as_slice()], |row| row.get(0))
            .unwrap();
        rows.collect::<std::result::Result<_, _>>().unwrap()
    }

    /// What survives a succession from [`seed_parked_forwards`]: every row that
    /// may be the only copy of accepted mail.
    const PARKED_SURVIVORS: [&str; 4] = ["m2", "m4", "m5", "m6"];

    #[tokio::test]
    async fn a_parked_forward_copy_burns_at_the_ceremony_and_a_redirect_moves() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_parked_forwards(&db, &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // Leaving these rows put is NOT the status quo the backlog's other
        // tables enjoy: `promote_due_forwards` iterates
        // `distinct_forward_queue_actors()` and is succession-blind, so a row
        // parked on the retired identity is still dispatched to the thief.
        //
        // The split is by what a burn would destroy, read off the persisted
        // copy mode. A `copy` row is a second copy — the original rests sealed
        // in the mailbox — so burning it costs the user nothing and closes the
        // tap, and that holds for a per-rule Forward (`m3`) exactly as for
        // forward-all (`m1`). A `redirect` row keeps NO
        // local copy, so it is the ONLY copy of a message this nest already
        // answered 250 for: it moves, whichever class it is — including a
        // forward-all row sent as `redirect` because a redirect rule fired
        // (`m5`), and a row whose mode was never persisted (`m6`).
        assert_eq!(parked_forwards_of(&db, &NEW).await, PARKED_SURVIVORS);
        assert!(parked_forwards_of(&db, &OLD).await.is_empty());
    }

    /// The outbound twin of [`seed_parked_forwards`], plus a dispatched row:
    /// - `m1` forward-all `copy` pending, `m4` per-rule `copy` pending → burn;
    /// - `m2` forward-all `copy` already **sent** — history, never deleted;
    /// - `m3` forwarder `redirect`, `m5` per-rule `redirect`, `m6` forward-all
    ///   `redirect`, `m7` forward-all with no mode: possibly the only copy →
    ///   re-attribute.
    async fn seed_outbound_forwards(db: &CacheDb, actor: &[u8; 32]) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO outbound_mail_queue
                (original_msgid, original_sender, recipient, raw_message, next_attempt_at,
                 created_at, status, is_forwarded, forward_actor_id, forward_rule_id,
                 forward_copy_mode)
             VALUES ('m1', 's@example.test', 'thief@evil.example', x'00', 1, 1,
                     'pending', 1, ?1, 'forward-all', 'copy'),
                    ('m2', 's@example.test', 'thief@evil.example', x'00', 1, 1,
                     'sent', 1, ?1, 'forward-all', 'copy'),
                    ('m3', 's@example.test', 'oldaccount@example.test', x'00', 1, 1,
                     'pending', 1, ?1, 'forwarder', 'redirect'),
                    ('m4', 's@example.test', 'thief@evil.example', x'00', 1, 1,
                     'pending', 1, ?1, '42', 'copy'),
                    ('m5', 's@example.test', 'elsewhere@example.test', x'00', 1, 1,
                     'pending', 1, ?1, '43', 'redirect'),
                    ('m6', 's@example.test', 'thief@evil.example', x'00', 1, 1,
                     'pending', 1, ?1, 'forward-all', 'redirect'),
                    ('m7', 's@example.test', 'thief@evil.example', x'00', 1, 1,
                     'pending', 1, ?1, 'forward-all', NULL)",
            rusqlite::params![actor.as_slice()],
        )
        .unwrap();
    }

    async fn outbound_forwards_of(db: &CacheDb, actor: &[u8; 32]) -> Vec<(String, String)> {
        let conn = db.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT original_msgid, status FROM outbound_mail_queue
                  WHERE forward_actor_id = ?1 ORDER BY original_msgid",
            )
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![actor.as_slice()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        rows.collect::<std::result::Result<_, _>>().unwrap()
    }

    fn outbound_survivors() -> Vec<(String, String)> {
        [
            ("m2", "sent"),
            ("m3", "pending"),
            ("m5", "pending"),
            ("m6", "pending"),
            ("m7", "pending"),
        ]
        .into_iter()
        .map(|(m, st)| (m.to_string(), st.to_string()))
        .collect()
    }

    #[tokio::test]
    async fn an_undispatched_forward_copy_burns_at_the_ceremony_and_the_rest_reattribute() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_outbound_forwards(&db, &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // `forward_queue` is only the rate-cap OVERFLOW. Under the cap a
        // forward is enqueued straight here, so this is where a thief's
        // in-flight copies actually sit — and the dispatcher selects on
        // `status = 'pending'` alone, with no idea the forwarding identity has
        // been retired.
        //
        // `m1` and `m4` are undispatched `copy` rows — forward-all and a
        // per-rule Forward alike: the destination is
        // thief-chosen and the original rests in the mailbox, so burning them
        // closes the tap and destroys nothing. `m2` is already sent — history,
        // not a tap. `m3`, `m5`, `m6` are redirects and `m7` has no persisted
        // mode: each may be the only copy of accepted mail. All of those
        // survive and re-attribute, so the SRS rewrite at queue-out and the NDR
        // route name an identity that still exists.
        assert_eq!(outbound_forwards_of(&db, &NEW).await, outbound_survivors());
        assert!(outbound_forwards_of(&db, &OLD).await.is_empty());
    }

    async fn seed_spam_plane(db: &CacheDb, actor: &[u8; 32]) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO spam_models (actor_id, model_json, ham_count, spam_count, updated_at)
             VALUES (?1, x'00', 3, 4, 1)",
            rusqlite::params![actor.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO spam_model_holder_copies (actor_id, holder_pubkey, sealed_copy, updated_at)
             VALUES (?1, x'aa', x'00', 1)",
            rusqlite::params![actor.as_slice()],
        )
        .unwrap();
    }

    async fn holder_copy_counts(db: &CacheDb) -> (i64, i64) {
        let conn = db.conn.lock().await;
        let count = |actor: &[u8; 32]| {
            conn.query_row(
                "SELECT COUNT(*) FROM spam_model_holder_copies WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        (count(&OLD), count(&NEW))
    }

    #[tokio::test]
    async fn the_ceremony_burns_the_sealed_baseline_contributions() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_spam_plane(&db, &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // A holder copy is the at-rest artifact of a *consent* — sealed to the
        // deployment's baseline holder, readable by neither identity. The
        // product already treats deleting it as the right answer to consent
        // withdrawal (`delete_spam_model_holder_copies` runs on opt-out), and a
        // succession is a consent boundary: the identity that opted in no
        // longer exists. Nothing irrecoverable dies — the model itself moves,
        // and re-contributing is one toggle.
        assert_eq!(holder_copy_counts(&db).await, (0, 0));
        // The model itself is the user's own trained data and follows them.
        let model_owner: Option<Vec<u8>> = {
            let conn = db.conn.lock().await;
            conn.query_row("SELECT actor_id FROM spam_models", [], |r| r.get(0))
                .optional()
                .unwrap()
        };
        assert_eq!(model_owner, Some(NEW.to_vec()));
    }

    /// A succession is NOT a departure from the deployment baseline: the model
    /// and the opt-in both move, so the same person stands as the same
    /// contributor under the successor id and the published sum is left alone
    /// (`mail-spam.md` § Cold start Path 2 → *A contributor's departure
    /// withdraws the baseline*). Only deletion, opt-out, reset and grant revoke
    /// withdraw it.
    #[tokio::test]
    async fn the_ceremony_leaves_the_published_baseline_standing() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_spam_plane(&db, &OLD).await;
        let mut prefs = db.get_spam_preferences(&OLD).await.unwrap();
        prefs.contribute_baseline = true;
        db.upsert_spam_preferences(&OLD, &prefs).await.unwrap();
        db.upsert_spam_baseline(b"published-sum", 3, 4, 3)
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.get_spam_baseline().await.unwrap(),
            Some(b"published-sum".to_vec())
        );
        assert!(
            db.get_spam_preferences(&NEW)
                .await
                .unwrap()
                .contribute_baseline,
            "the opt-in follows the person"
        );
    }

    /// Seed the reach plane on `actor`: a one-time key package, the reusable
    /// last-resort key package, a nest pairing carrying a capability, and the
    /// two push destinations — the owner's own device, and one a seed thief
    /// could have armed with the same User-class subscribe. The pair matters:
    /// no ruling can tell them apart, which is exactly why the whole plane
    /// burns rather than being filtered.
    async fn seed_reach_plane(db: &CacheDb, actor: &[u8; 32]) {
        db.put_key_package("kp-one-time", actor, b"one-time", 1, 1 << 40)
            .await
            .unwrap();
        db.put_last_resort_key_package("kp-last-resort", actor, b"last-resort", 1, 1 << 40)
            .await
            .unwrap();
        db.store_pairing(
            actor.as_slice(),
            &[0x99u8; 32],
            &["nostr_push".to_string()],
            None,
            Some("https://the-pairing-chose-this-host.example"),
            Some("a paired nest"),
        )
        .await
        .unwrap();
        db.upsert_push_subscription(
            actor.as_slice(),
            "device-the-owner-enrolled",
            "web-push",
            "https://push.example/the-owners-phone",
            Some("p256dh-owner"),
            Some("auth-owner"),
        )
        .await
        .unwrap();
        db.upsert_push_subscription(
            actor.as_slice(),
            "device-a-thief-enrolled",
            "apns",
            "https://the-thief-chose-this-endpoint.example/hook",
            Some("p256dh-thief"),
            Some("auth-thief"),
        )
        .await
        .unwrap();
    }

    /// Seed the backup **enrollment** plane on `actor`: a destination the owner
    /// chose, a second one a seed thief could have registered *without* it ever
    /// reaching the account plane, the custodian check-in that hangs off the first, and
    /// the destination-side writer grant.
    async fn seed_backup_enrollment(db: &CacheDb, actor: &[u8; 32]) {
        db.put_backup_destination(
            actor.as_slice(),
            "dest-the-owner-chose",
            "https://friend.example",
            &[0x77u8; 32],
        )
        .await
        .unwrap();
        db.put_backup_destination(
            actor.as_slice(),
            "dest-a-thief-registered",
            "https://the-thief-chose-this-box.example",
            &[0x78u8; 32],
        )
        .await
        .unwrap();
        db.put_custodian_checkin(
            actor.as_slice(),
            "dest-the-owner-chose",
            42,
            1 << 20,
            "ok",
            true,
            Some("ok"),
            Some(1),
        )
        .await
        .unwrap();
        db.register_backup_writer(actor.as_slice(), &[0x79u8; 32], None)
            .await
            .unwrap();
    }

    /// The backup-enrollment burn, on the product observables: the coordinator
    /// reads `list_backup_destinations`, and the federation write gate reads
    /// `has_backup_writer_grant`.
    ///
    /// **The second destination is the whole reason this plane burns rather than
    /// moves.** A seed thief can register one directly over
    /// `fauna.backup.destination.register` without ever touching the account plane — so
    /// the ratified re-register-then-mark adjudication, which is keyed on the
    /// *plane* list, could never raise a mark against it
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across). A moved row would therefore hand the successor an unflaggable
    /// exfiltration destination; a burned one leaves the successor's list to be
    /// rebuilt from the config that IS adjudicated.
    #[tokio::test]
    async fn the_ceremony_burns_the_retired_identitys_backup_enrollment() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_backup_enrollment(&db, &OLD).await;

        // Armed before the ceremony, so the asserts below cannot pass against a
        // plane that was never seeded.
        assert_eq!(db.list_backup_destinations(&OLD).await.unwrap().len(), 2);
        assert!(
            db.has_backup_writer_grant(&OLD, &[0x79u8; 32])
                .await
                .unwrap()
        );

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // Nothing is left to dial on the retired identity...
        assert!(db.list_backup_destinations(&OLD).await.unwrap().is_empty());
        assert!(
            db.get_custodian_checkin(&OLD, "dest-the-owner-chose")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !db.has_backup_writer_grant(&OLD, &[0x79u8; 32])
                .await
                .unwrap()
        );
        // ...and nothing is handed to the successor either. This is the assert
        // that would fail on a `Move`, and the thief-registered row is why it
        // matters: the successor's list is rebuilt from `fauna.state.backup` by
        // `reconcile_backup_enrollment`, which is the only list any mark can
        // reach.
        assert!(
            db.list_backup_destinations(&NEW).await.unwrap().is_empty(),
            "a destination the successor's own config never named must not be \
             inherited — no adjudication mark could ever raise it"
        );
        assert!(
            !db.has_backup_writer_grant(&NEW, &[0x79u8; 32])
                .await
                .unwrap()
        );
    }

    /// `restore_history`'s move, hand-written because the table is `SEED_BLIND`
    /// (`actor_tables.rs`) — the generic seeder cannot plant a row for it, so
    /// **neither** data-driven gate observes its ruling. Asserted on the product
    /// observable the Backups page reads (`ui/backups.md` § Restore history).
    #[tokio::test]
    async fn a_succession_carries_the_accounts_restore_history() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let fs = db
            .get_or_create_reserved_folder(&OLD, "mail")
            .await
            .unwrap();
        let snap = db
            .create_message_kind_snapshot_row(fs, "mail", None, None)
            .await
            .unwrap();
        db.insert_restore_history(&OLD, snap, "mail", None)
            .await
            .unwrap();
        assert_eq!(db.list_restore_history(&OLD, 0).await.unwrap().len(), 1);

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.list_restore_history(&NEW, 0).await.unwrap().len(),
            1,
            "the account's own record of what it restored must follow the corpus \
             it describes; left behind, the successor's Backups page reports that \
             this account has never restored anything"
        );
        assert!(db.list_restore_history(&OLD, 0).await.unwrap().is_empty());
    }

    /// The tier plane's **product observable**, and it is deliberately written
    /// for the direction this ruling did NOT choose.
    ///
    /// The finding: a
    /// verdict-to-verdict flip trips *nothing* structural. Re-declaring this
    /// family `Burn` instead of `Move` leaves the un-ruled count unchanged, the
    /// executor deletes instead of moving, the declaration agrees with itself —
    /// and agreement is all either data-driven gate checks, so both stay green.
    /// The exact-count ratchet catches a demotion to `Unruled` and nothing else.
    /// So the whole ruling rests on this assert about what the user can see.
    ///
    /// What the user can see: an author who succeeds their identity still has
    /// their tiers, still has the roster of who paid them, and their paying
    /// readers still hold the entitlement they paid for. Left behind — the
    /// un-ruled status quo this ruling replaced — every one of those resolves
    /// under an identity that no longer authenticates anywhere, and no client
    /// can reach it to serve, rotate or even delete it.
    #[tokio::test]
    async fn the_successor_inherits_the_authors_tier_plane() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let reader = [0x77u8; 32];

        db.create_subscription_tier(&OLD, "gold", 1, None, None, None, true, None, None, false)
            .await
            .unwrap();
        db.add_subscriber(&OLD, &reader, "gold", None)
            .await
            .unwrap();
        assert!(db.is_subscriber(&OLD, &reader, "gold").await.unwrap());

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.list_subscription_tiers(&NEW).await.unwrap().len(),
            1,
            "the author's own tiers must follow the account — left behind, the \
             successor's monetization page is empty while the tiers keep selling \
             under an identity that is refused everywhere"
        );
        assert!(
            db.is_subscriber(&NEW, &reader, "gold").await.unwrap(),
            "a reader who PAID for this tier must still hold the entitlement after \
             the author succeeds — this is the assert a `Burn` flip reds, and \
             nothing structural would"
        );
        assert!(
            db.list_subscription_tiers(&OLD).await.unwrap().is_empty(),
            "nothing may be left behind on the retired identity — a duplicated plane \
             is a second seller under a compromised key"
        );
    }

    /// The reach burn, asserted on the **product observables** rather than on
    /// row counts — the registry's own data-driven gate already checks that the
    /// rows are gone from both sides, and what matters here is that the two
    /// consumers which never look at an actor stop answering.
    #[tokio::test]
    async fn the_ceremony_burns_the_retired_identitys_reach() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_reach_plane(&db, &OLD).await;

        // All three consumers answer for the old identity before the ceremony —
        // without this the asserts below could pass against a plane that was
        // never armed.
        assert!(db.take_key_package(&OLD).await.unwrap().is_some());
        assert!(db.any_pairing_with_capability("nostr_push").await.unwrap());
        assert_eq!(
            db.list_push_subscriptions(&OLD).await.unwrap().len(),
            2,
            "the push dispatcher's own query must answer for the old identity \
             before the ceremony, or the assert below proves nothing"
        );

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The addressability is gone, INCLUDING the last-resort row — that is
        // the half that makes a surviving pool a permanent trap rather than a
        // pool-limited one, since `take_key_package` serves it without
        // consuming it. A stale contact's add now fails loudly instead of
        // seating a leaf that can never read the group.
        assert!(db.take_key_package(&OLD).await.unwrap().is_none());
        assert_eq!(db.count_key_packages(&OLD).await.unwrap(), 0);
        // The successor inherits no key packages either: a KP embeds the
        // predecessor's credential and its private half is seed-derived, so
        // moving one would advertise a thief-openable leaf under the new name.
        // The successor's own client republishes its pool at next sign-in.
        assert!(db.take_key_package(&NEW).await.unwrap().is_none());

        // The box-wide pairing predicate — the one the identity-key refusal
        // plane cannot reach, because it reads no actor to refuse.
        assert!(!db.any_pairing_with_capability("nostr_push").await.unwrap());
        assert!(
            db.list_pairings_with_capability("nostr_push")
                .await
                .unwrap()
                .is_empty(),
            "the head's NIP-46 proxy must stop dialling a pairing whose host the \
             thief chose"
        );

        // The push destinations, asserted on the exact query the dispatcher
        // makes (`PushService::maybe_send_push` → `list_push_subscriptions`).
        // This is the member of the plane that is LIVE rather than latent: the
        // inbox path refuses a superseded *sender* and never a superseded
        // recipient, so an un-propagated contact goes on delivering to the
        // retired actor, and every one of those deliveries fired at these
        // endpoints — including one the thief chose — until this leg existed.
        assert!(
            db.list_push_subscriptions(&OLD).await.unwrap().is_empty(),
            "a delivery to the retired identity must reach no endpoint at all"
        );
        // And the successor inherits none: a `Move` would have carried the
        // thief's endpoint onto the account the ceremony just rescued, which is
        // the direction this ruling exists to refuse. Nothing is lost — a
        // subscription is a per-device registration the client re-creates.
        assert!(
            db.list_push_subscriptions(&NEW).await.unwrap().is_empty(),
            "the successor must not inherit a destination nobody can vouch for"
        );
    }

    /// Seed the MUA-facing bridge plane on `actor`: the ten collection tables
    /// that MOVE, the four key-material blobs that BURN, and the two
    /// bridge-reported logs that STAY — one seed for one ceremony, so a pin
    /// cannot miss a leg that moved something it should not have.
    async fn seed_bridge_plane(db: &CacheDb, actor: &[u8; 32]) {
        // ── IMAP: mailbox state, a placement, a subscription, a tombstone ──
        db.ensure_bridge_imap_mailboxes(actor).await.unwrap();
        db.place_inbound_mail(
            actor,
            &BRIDGE_MSG_ID,
            "INBOX",
            1_700_000_000,
            "",
            "sender.example",
            true,
        )
        .await
        .unwrap();
        db.insert_bridge_imap_subscription(actor, "Archive")
            .await
            .unwrap();

        // ── CalDAV / CardDAV: a collection each, with one item ──
        db.insert_bridge_caldav_calendar(actor, &BRIDGE_COLLECTION_ID, b"sealed-cal-meta", 1)
            .await
            .unwrap();
        db.place_caldav_event(
            actor,
            &BRIDGE_COLLECTION_ID,
            &[0xE1u8; 32],
            b"sealed-vevent",
            b"sealed-hint",
            1_700_000_000,
            13,
            1,
        )
        .await
        .unwrap();
        db.insert_bridge_carddav_addressbook(actor, &BRIDGE_COLLECTION_ID, b"sealed-ab-meta", 1)
            .await
            .unwrap();
        db.place_carddav_card(
            actor,
            &BRIDGE_COLLECTION_ID,
            &[0xE2u8; 32],
            b"sealed-vcard",
            b"sealed-hint",
            1_700_000_000,
            11,
            1,
        )
        .await
        .unwrap();

        // ── The four blobs that burn, and the two logs that stay ──
        db.put_wrapped_mls_blob(actor, "default", &[0xB1; 48])
            .await
            .unwrap();
        db.put_wrapped_submission_token(actor, "default", &[0xB2; 48])
            .await
            .unwrap();
        db.put_mls_snapshot_blob(actor, &[0xB3; 64]).await.unwrap();
        db.put_webdav_keys_blob(actor, &[0xB4; 64]).await.unwrap();

        // The three tombstone logs and the two bridge-reported logs have no
        // owner-facing writer, so they are seeded directly — the point of the
        // pins below is where the rows END UP, not how they were made.
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO bridge_imap_expunged (actor_id, mailbox, uid, modseq, expunged_at) \
             VALUES (?1, 'INBOX', 7, 5, 1)",
            rusqlite::params![&actor[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bridge_caldav_expunged \
                 (actor_id, calendar_id, event_id, uid_hash, modseq, expunged_at) \
             VALUES (?1, ?2, ?3, ?4, 5, 1)",
            rusqlite::params![
                &actor[..],
                &BRIDGE_COLLECTION_ID[..],
                &[0xE3u8; 32][..],
                &[0xE4u8; 32][..]
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bridge_carddav_expunged \
                 (actor_id, addressbook_id, card_id, uid_hash, modseq, expunged_at) \
             VALUES (?1, ?2, ?3, ?4, 5, 1)",
            rusqlite::params![
                &actor[..],
                &BRIDGE_COLLECTION_ID[..],
                &[0xE5u8; 32][..],
                &[0xE6u8; 32][..]
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bridge_feed_subscriptions \
                 (actor_id, bridge, feed_uri, name, created_at) \
             VALUES (?1, 'nostr', 'wss://relay.example/feed', 'a feed the owner chose', 1)",
            rusqlite::params![&actor[..]],
        )
        .unwrap();
        // ⚠ `bridge_restore_divergence` is deliberately NOT seeded here, and the
        // omission is the ruling rather than a gap. Its `actor_id` has no
        // production reader at all — the one consumer
        // (`fauna.filesync.snapshot.list_restore_divergence`) selects `WHERE
        // snapshot_id = ?1` and takes its authorization from the snapshot's
        // owner — so a pin here could only seed a read the schema has no reader
        // for and confirm whichever verdict is declared, which is exactly the
        // manufactured observable that was ruled against. Its move is
        // witnessed by the registry's own execution sweep and by nothing else,
        // said plainly in its registry entry.
        conn.execute(
            "INSERT INTO bridge_audit_events \
                 (received_at, bridge_actor_id, actor_id, credential_id, result, \
                  source_ip, occurred_at, reason) \
             VALUES (1, ?1, ?2, 'default', 'fail', '203.0.113.5', 1, 'the theft itself')",
            rusqlite::params![&[0x33u8; 32][..], &actor[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bridge_session_close_events \
                 (received_at, bridge_actor_id, actor_id, credential_id, reason, occurred_at) \
             VALUES (1, ?1, ?2, 'default', 'logout', 1)",
            rusqlite::params![&[0x33u8; 32][..], &actor[..]],
        )
        .unwrap();
    }

    /// The message id the IMAP placement pins follow across the ceremony.
    const BRIDGE_MSG_ID: [u8; 32] = [0xD1u8; 32];
    /// One id reused as the CalDAV calendar and the CardDAV address book —
    /// they are separate tables, so a shared value cannot make either assert
    /// pass for the other's reason.
    const BRIDGE_COLLECTION_ID: [u8; 32] = [0xD2u8; 32];

    /// The ten MUA-facing collection tables land on the successor, asserted on
    /// the doors an MUA actually opens rather than on row counts.
    ///
    /// Written for the direction NOT chosen: every assert
    /// below reds under a `Stay`, and the structural gates would not — the
    /// registry-driven loop reads the declaration, so a demotion stops the move
    /// *and* says it should, which is all either data-driven sweep checks.
    #[tokio::test]
    async fn the_ceremony_moves_the_mua_facing_collection_plane() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_bridge_plane(&db, &OLD).await;

        // The plane answers for the old identity first, or the asserts below
        // prove nothing.
        assert!(
            db.bridge_imap_message_placed_for_actor(&OLD, &BRIDGE_MSG_ID)
                .await
                .unwrap()
        );
        assert_eq!(
            db.list_bridge_caldav_calendars(&OLD).await.unwrap().len(),
            1
        );

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // ── IMAP. The placement, the UID window and the subscription set are
        // one unit: the successor's MUA re-AUTHs to the SAME address, because
        // `account_aliases` moved at this ceremony.
        assert!(
            db.bridge_imap_message_placed_for_actor(&NEW, &BRIDGE_MSG_ID)
                .await
                .unwrap(),
            "the mail bodies are `segment_records` rows that already move, so a \
             placement left behind hides mail the successor owns and is paying \
             quota for"
        );
        let inbox = db
            .get_bridge_imap_mailbox_state(&NEW, "INBOX")
            .await
            .unwrap()
            .expect("the successor's INBOX must have a state row");
        assert!(
            inbox.uid_next > 1,
            "the UID window must travel with the placements it describes — a \
             successor whose `uid_next` restarted at 1 under an unchanged \
             `uid_validity` would hand a cached UID to a *different* message, \
             which RFC 9051 gives an MUA no way to detect"
        );
        assert_eq!(
            db.list_bridge_imap_subscribed_mailbox_state(&NEW)
                .await
                .unwrap()
                .len(),
            1,
            "LSUB must survive the ceremony; nothing can ever remove a stranded \
             subscription row"
        );
        assert_eq!(
            db.list_bridge_imap_expunged_since(&NEW, "INBOX", 0)
                .await
                .unwrap(),
            Some(vec![7]),
            "the VANISHED log is the half whose `Stay` RESURRECTS deleted mail: \
             the messages move, so an MUA syncing from its stored modseq is told \
             about no removals and re-shows everything the account deleted"
        );

        // ── CalDAV / CardDAV, the same three-part unit twice over.
        assert_eq!(
            db.list_bridge_caldav_calendars(&NEW).await.unwrap().len(),
            1,
            "every calendar door is `actor_id`-scoped, so a left-behind calendar \
             is one the successor can neither list nor delete"
        );
        assert_eq!(
            db.count_bridge_caldav_events(&NEW, &BRIDGE_COLLECTION_ID)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_caldav_expunged_since(&NEW, &BRIDGE_COLLECTION_ID, 0)
                .await
                .unwrap()
                .len(),
            1,
            "the CalDAV tombstones carry the same resurrection argument"
        );
        assert_eq!(
            db.list_bridge_carddav_addressbooks(&NEW)
                .await
                .unwrap()
                .len(),
            1,
            "an address book is user-irrecoverable data — no burn was ever \
             available here and a `Stay` loses it to the owner just as surely"
        );
        assert_eq!(
            db.count_bridge_carddav_cards(&NEW, &BRIDGE_COLLECTION_ID)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_carddav_expunged_since(&NEW, &BRIDGE_COLLECTION_ID, 0)
                .await
                .unwrap()
                .len(),
            1
        );

        // ── Nothing is duplicated on the retired identity: a second copy under
        // a key that is refused everywhere is exactly the stranding the ruling
        // refuses.
        assert!(
            !db.bridge_imap_message_placed_for_actor(&OLD, &BRIDGE_MSG_ID)
                .await
                .unwrap()
        );
        assert!(
            db.list_bridge_caldav_calendars(&OLD)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.list_bridge_carddav_addressbooks(&OLD)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The four MSEK-derived blobs are gone from BOTH sides, and the two
    /// bridge-reported logs are still on the retired identity.
    ///
    /// Asserted on the bridge's own fetch doors, which is where the danger is:
    /// those handlers take the target actor straight out of the request and
    /// consult no refusal plane, so "the row is gone" and "no bridge can serve
    /// it" are the same sentence here.
    #[tokio::test]
    async fn the_ceremony_burns_the_retired_identitys_bridge_key_material() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_bridge_plane(&db, &OLD).await;

        assert!(
            db.get_wrapped_mls_blob(&OLD, "default")
                .await
                .unwrap()
                .is_some(),
            "the wrap must be fetchable for the old identity first, or the \
             assert below proves nothing"
        );

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // Gone from the retired identity: a bridge asked for it gets nothing.
        assert!(
            db.get_wrapped_mls_blob(&OLD, "default")
                .await
                .unwrap()
                .is_none(),
            "the MUA credential wrap opens under a secret the seed thief read; \
             left resting it is unrevocable, since the revoke door refuses any \
             target that is not the caller"
        );
        assert!(
            db.get_wrapped_submission_token(&OLD, "default")
                .await
                .unwrap()
                .is_none()
        );
        assert!(db.get_mls_snapshot_blob(&OLD).await.unwrap().is_none());
        assert!(db.get_webdav_keys_blob(&OLD).await.unwrap().is_none());

        // And NOT inherited — this is the assert that reds a `Move`, and no
        // structural gate would: the address moved to the successor at this very
        // ceremony, so a carried wrap makes the THIEF's mail password open the
        // successor's mail, which is the defect one table over.
        assert!(
            db.get_wrapped_mls_blob(&NEW, "default")
                .await
                .unwrap()
                .is_none(),
            "the successor must not inherit a wrap that opens under the \
             predecessor's compromised credential secret"
        );
        assert!(
            db.get_wrapped_submission_token(&NEW, "default")
                .await
                .unwrap()
                .is_none(),
            "nor a submission token, which authorizes SENDING as the account and \
             is honoured in place of an identity"
        );
        assert!(db.get_mls_snapshot_blob(&NEW).await.unwrap().is_none());
        assert!(db.get_webdav_keys_blob(&NEW).await.unwrap().is_none());

        // The two bridge-reported logs STAY: on a succeeding account the
        // interesting rows are the thief's, and re-pointing them would attribute
        // the intrusion to the identity recovering from it.
        assert_eq!(
            db.list_bridge_auth_events_for_actor(&OLD, 5)
                .await
                .unwrap()
                .len(),
            1,
            "a failed AUTH from the thief's address is testimony about the theft; \
             the successor did not make it"
        );
        assert!(
            db.list_bridge_auth_events_for_actor(&NEW, 5)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.list_bridge_session_close_for_actor(&OLD, 5)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// The submission meter is a LIMIT, and a succession is self-service — so
    /// it must not be a reset button for the account's daily relay capacity
    /// (§ Re-key scope's liability blockquote).
    #[tokio::test]
    async fn a_succession_does_not_hand_the_account_a_fresh_submission_day() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let day = 20_000i64;

        // Spend the whole day's allowance on the old identity.
        assert!(matches!(
            db.try_consume_submission_quota(&OLD, day, 100, 100)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::Allowed
        ));
        assert!(matches!(
            db.try_consume_submission_quota(&OLD, day, 1, 100)
                .await
                .unwrap(),
            SubmissionQuotaOutcome::OverQuota { .. }
        ));

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert!(
            matches!(
                db.try_consume_submission_quota(&NEW, day, 1, 100)
                    .await
                    .unwrap(),
                SubmissionQuotaOutcome::OverQuota { remaining: 0 }
            ),
            "the successor must inherit the day already spent — left behind, the \
             meter reads zero and any account holder can mint a fresh relay day \
             with their own recovery kit. The cost of moving is bounded and \
             self-clearing (one day, zeroed at the next bucket rollover); the \
             cost of not moving is unbounded"
        );
    }

    /// Seed the delivery plane's five MOVING tables plus its two `Stay`s on
    /// `actor`, each with the state its own consumer reads back.
    ///
    /// The two `Stay`s are seeded here rather than in a test of their own so
    /// every assert below runs against one ceremony: a plane is ruled as a
    /// whole, and a pin that seeds only the rows it expects to move cannot
    /// notice a leg that moved something it should not have.
    async fn seed_delivery_plane(db: &CacheDb, actor: &[u8; 32]) {
        // MOVES.
        db.insert_notification(
            actor.as_slice(),
            &fauna_protocol::notifications::NotifType::Reply,
            "fauna",
            Some(&[0xC3u8; 32]),
            None,
            None,
            &crate::db::notifications::NotificationText::untranslated("someone replied to you"),
            1,
        )
        .await
        .unwrap();
        db.push_knock(
            actor,
            &[0xC4u8; 32],
            b"https://the-knockers-nest.example",
            "a stranger would like to reach you",
            b"the knock payload",
        )
        .await
        .unwrap();
        db.insert_dedup_key(
            actor,
            "dedup-key-of-a-message-already-held",
            "env:v1:already-held",
            "imap://1",
        )
        .await
        .unwrap();
        db.insert_scan_result(&crate::db::bridge_routing::ScanResultRow {
            message_id: [0xD5u8; 32],
            received_at: 1,
            scanned_at: 1,
            clamav_verdict: "clean".to_string(),
            clamav_signature: None,
            rspamd_score_raw: Some(1),
            rspamd_score_scaled: Some(1),
            rspamd_flagged_rules: None,
            rspamd_score_breakdown: None,
            action_taken: "delivered".to_string(),
            delivered_to_actor: Some(*actor),
        })
        .await
        .unwrap();
        db.put_personalization_model(actor, "topic:the-user-trained-this", b"sealed model", 7, 32)
            .await
            .unwrap();

        // STAYS.
        db.put_actor_index_pubkey(actor, &[0xE6u8; 32])
            .await
            .unwrap();
        db.update_actor_last_ip(actor.as_slice(), "203.0.113.7")
            .await
            .unwrap();
    }

    /// The delivery plane, asserted on **product observables** — one per table,
    /// each read through the consumer that actually honours the row rather than
    /// through a row count. The registry's data-driven sweep already witnesses
    /// that the declared moves execute; what it cannot witness is whether the
    /// declaration is *right*, which is the whole point (a
    /// verdict-to-verdict flip leaves both sweeps green because agreement is all
    /// they check).
    #[tokio::test]
    async fn the_ceremony_moves_the_delivery_plane_and_leaves_its_two_compromise_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_delivery_plane(&db, &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // ── The five that move ───────────────────────────────────────────────
        //
        // The successor's own notification page, and the badge that counts it.
        // Left behind, the ceremony that rescued the account empties it.
        assert_eq!(
            db.list_notifications(&NEW[..], None, 10)
                .await
                .unwrap()
                .len(),
            1,
            "the successor's notification page must not be emptied by the ceremony"
        );
        assert_eq!(db.count_unread_notifications(&NEW[..]).await.unwrap(), 1);
        assert!(
            db.list_notifications(&OLD[..], None, 10)
                .await
                .unwrap()
                .is_empty()
        );

        // The pending-reach queue. `inbox_modes` already moves, so a knock left
        // behind would hand the successor the narrowed mode and none of the
        // people it narrowed — answered by nobody, forever, with no surface
        // saying one ever waited.
        let knocks = db.poll_knocks(&NEW).await.unwrap();
        assert_eq!(knocks.len(), 1, "a pending knock must survive the ceremony");
        assert!(db.poll_knocks(&OLD).await.unwrap().is_empty());

        // The import dedup ledger: the successor's first re-import must still
        // hit, or the ceremony's visible effect is a mailbox of duplicates.
        assert!(
            db.has_dedup_key(&NEW, "dedup-key-of-a-message-already-held")
                .await
                .unwrap(),
            "the dedup ledger must agree with the corpus that moved with it"
        );
        assert!(
            !db.has_dedup_key(&OLD, "dedup-key-of-a-message-already-held")
                .await
                .unwrap()
        );

        // The scan record, read through its real per-actor consumer: the
        // scoring backlog seeds from "the owner's mail item universe", so a
        // record left behind seeds the successor from an empty one.
        assert_eq!(
            db.seed_factor_backlog("topic:anything", "mail", &NEW)
                .await
                .unwrap(),
            1,
            "the successor's scoring backlog must seed from the mail it inherited"
        );

        // The trained factor's sealed model. Its registry lives in `fauna.state.personalization`,
        // which the aftermath re-seals to the successor — so leaving the blob
        // behind splits one object and the seam degrades the orphan to a zero
        // term, silently.
        assert!(
            db.get_personalization_model(&NEW, "topic:the-user-trained-this")
                .await
                .unwrap()
                .is_some(),
            "a factor the successor's own registry still names must keep its model"
        );
        assert!(
            db.get_personalization_model(&OLD, "topic:the-user-trained-this")
                .await
                .unwrap()
                .is_none()
        );

        // ── The two that stay, and WHY, in observables ───────────────────────
        //
        // The perimeter's index-hint seal key is the `actor_mls_pubkeys` class:
        // it seals FUTURE INBOUND mail, so carrying it would seal fresh hints to
        // a key derived from what the thief read.
        assert!(
            db.get_actor_index_pubkey(&NEW).await.unwrap().is_none(),
            "a recipient seal key must never travel with the account"
        );
        assert_eq!(
            db.get_actor_index_pubkey(&OLD).await.unwrap(),
            Some([0xE6u8; 32])
        );

        // `actor_last_ip` has no getter at all; its only consumer compares it to
        // the address in hand and returns *changed?*, which arms the
        // new-location security alert. So the observable IS that comparison: the
        // successor's first sign-in must be a first insert (`false`), not a
        // change measured against the address the thief last used.
        assert!(
            !db.update_actor_last_ip(&NEW[..], "198.51.100.9")
                .await
                .unwrap(),
            "the successor's first honest sign-in must not raise a new-location \
             alert against a baseline the retired identity left behind"
        );
    }

    /// The recovery plane stays behind, and the pin is written for the direction
    /// NOT chosen: every assert below is one a `Move` would break.
    ///
    /// This is the one plane on the axis where moving the rows breaks the very
    /// act that would perform the move — the ceremony is verified against this
    /// chain and three consumers go on reading it under the **retired** id
    /// afterwards. The third assert is the sharpest and answers the question the
    /// guardianship plane taught this axis to ask: *can the successor ever
    /// succeed again?*
    #[tokio::test]
    async fn the_recovery_plane_stays_with_the_identity_whose_chain_it_is() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        // The retired identity's own chain, plus the peer-side watermark this
        // box would hold for a foreign identity of the same id.
        db.append_recovery_registration(&OLD[..], 0, &[0x11u8; 32], b"registration 0")
            .await
            .unwrap();
        db.append_recovery_registration(&OLD[..], 1, &[0x11u8; 32], b"registration 1")
            .await
            .unwrap();
        db.record_foreign_recovery_head(&OLD[..], &[0x22u8; 32], 4, &[0x33u8; 32])
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // 1. The peer push builds its chain from the RETIRED id after the
        //    ceremony (`push_succession_detached`), and bails when that read is
        //    empty — so a moved chain means no peer nest ever learns the
        //    succession happened.
        assert_eq!(
            db.list_recovery_registrations(&OLD[..])
                .await
                .unwrap()
                .len(),
            2,
            "the succession push reads the retired identity's chain AFTER the \
             ceremony; empty here means the statement reaches no peer at all"
        );

        // 2. The successor starts with no chain of its own — it has not run a
        //    kit ceremony yet, and inheriting one would be inheriting a
        //    RecoveryKey the predecessor's thief may hold.
        assert!(
            db.list_recovery_registrations(&NEW[..])
                .await
                .unwrap()
                .is_empty()
        );

        // 3. …and BECAUSE it starts empty, its first kit ceremony works. The
        //    chain appends under a per-actor monotonic seq, so a chain moved
        //    onto the successor would arrive with a head that outranks the seq-0
        //    registration a new identity presents, and this call would be
        //    refused — leaving the successor permanently unable to register a
        //    RecoveryKey, i.e. unable to ever succeed again.
        db.append_recovery_registration(&NEW[..], 0, &[0x44u8; 32], b"the successor's own kit")
            .await
            .expect("a successor must be able to register its first RecoveryKey");

        // 4. The anti-downgrade watermark is still under the identity it was
        //    learned for. Moving it would erase, in the act of using it, the
        //    baseline `verify_succession_against_chain` compares the NEXT
        //    statement against.
        let head = db.foreign_recovery_head(&OLD[..]).await.unwrap();
        assert!(
            head.is_some_and(|h| h.seq == 4),
            "the head learned for the old identity is what a later statement \
             must extend; carrying it to the successor re-opens the downgrade"
        );
        assert!(db.foreign_recovery_head(&NEW[..]).await.unwrap().is_none());
    }

    /// Seed the membership/participation cluster under `actor` — the tables row
    /// 78 ruled `Stay` on 2026-08-14 (its `groups` + `group_members` pair
    /// retired with the group plane, schema 86). Returns the shared channel id
    /// the asserts key on.
    ///
    /// The channel carries a foreign member on a peer nest, because the sharpest
    /// property here is not where the rows land but whether the *push* can still
    /// find that peer once the ceremony has run.
    async fn seed_membership_plane(db: &CacheDb, actor: &[u8; 32]) -> [u8; 32] {
        let channel = [0x1Au8; 32];

        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO actor_channels (actor_id, channel_id, created_at) VALUES (?1, ?2, 1)",
                rusqlite::params![actor.as_slice(), channel.as_slice()],
            )
            .unwrap();
        }
        // The peer this channel is shared with — the push's audience.
        foreign_member(db, &channel, &[0x5Au8; 32]).await;
        // The writer half of the conjunction `resolve_writable_folder` applies.
        db.set_folder_member_access(&channel, actor, "writer", None)
            .await
            .unwrap();

        channel
    }

    /// **The membership/participation cluster (2026-08-14).** All its
    /// tables `Stay`, and this is the pin written for the direction NOT chosen —
    /// the data-driven sweeps cannot grade a ruling, only its execution,
    /// and a `Stay` executes as "nothing happened", which is what every green
    /// sweep already looks like.
    ///
    /// The first assert is the one that matters and it is the
    /// `recovery_registrations` shape one plane over: **the ceremony's own
    /// federation push reads `actor_channels` under the RETIRED id, after the
    /// transaction commits** (`succession_push_targets` ← `push_succession_
    /// detached`). Move the roster and the target set is empty, the push returns
    /// early, and no peer nest ever learns the succession happened — the table
    /// the propagation reads to find its audience cannot be re-pointed by the
    /// transaction it is announcing.
    #[tokio::test]
    async fn the_membership_plane_stays_and_the_ceremony_still_reaches_its_peers() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let channel = seed_membership_plane(&db, &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // 1. ⚠ THE decisive one. The peer holding residue is still discoverable
        //    from the retired id, which is the only id the detached push has.
        assert_eq!(
            db.succession_push_targets(&OLD).await.unwrap(),
            vec!["https://peer.example".to_string()],
            "the succession push resolves its targets through `actor_channels` under the \
             RETIRED id, after the ceremony commits — empty here means the statement \
             reaches no peer at all and the propagation this ruling defers to never runs"
        );

        // 2. The roster itself: participation waits for the sweep's Welcome
        //    (`register_actor_channel_gated`), which is the ratified leg. A
        //    `Move` would seat the successor before the group accepted them.
        assert!(db.is_actor_in_channel(&OLD, &channel).await.unwrap());
        assert!(
            !db.is_actor_in_channel(&NEW, &channel).await.unwrap(),
            "the ceremony must not hand channel-fetch authorization to a credential the \
             group has not accepted — that is the propagation's job"
        );

        // 3. The writer grant travels with its roster row or not at all: the
        //    write gate is a conjunction, so a grant that moved alone would
        //    admit nothing while stripping the row that still can.
        assert!(
            db.get_folder_member_role(&channel, &OLD)
                .await
                .unwrap()
                .is_some_and(|r| r.access == "writer"),
            "the grant must stay paired with the roster row it is conjoined with"
        );
        assert!(
            db.get_folder_member_role(&channel, &NEW)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// The push-ordering pin of ruling (8)(j)(2). The Welcome that seats the
    /// successor retires the roster row `succession_push_targets` finds its
    /// audience through, so the targets exist only for a reader that comes
    /// first — which is why the ceremony's handler reads them before it
    /// replies (`recovery_handlers::succession_push_targets_for_reply`) and
    /// `push_succession_detached` takes them as an argument.
    #[tokio::test]
    async fn the_push_targets_are_gone_once_the_successors_welcome_retires_the_seat() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let channel = seed_membership_plane(&db, &OLD).await;
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let before_the_reply = db.succession_push_targets(&OLD).await.unwrap();
        assert_eq!(before_the_reply, vec!["https://peer.example".to_string()]);

        let carried = db
            .register_successor_carrying_seat(&NEW, &channel)
            .await
            .unwrap();
        assert_eq!(
            carried,
            crate::db::channels::SeatCarried {
                seats_retired: 1,
                grants: 1
            }
        );
        assert!(!db.is_actor_in_channel(&OLD, &channel).await.unwrap());
        assert!(db.is_actor_in_channel(&NEW, &channel).await.unwrap());
        assert!(
            db.get_folder_member_role(&channel, &NEW)
                .await
                .unwrap()
                .is_some_and(|r| r.access == "writer"),
            "the grant followed the seat"
        );
        assert!(
            db.get_folder_member_role(&channel, &OLD)
                .await
                .unwrap()
                .is_none()
        );

        assert!(
            db.succession_push_targets(&OLD).await.unwrap().is_empty(),
            "a read under the retired id that ran after the Welcome finds no peer — \
             the read must precede the reply, never ride the detached task"
        );
    }

    /// The carry walks the whole chain into the successor: a middle identity
    /// succeeded before its own Welcome landed never held a seat, and the
    /// first identity's seat and grant still reach the live successor. A
    /// predecessor's grant on a channel where it holds no seat stays put.
    #[tokio::test]
    async fn the_seat_carry_reaches_past_a_middle_hop_that_was_never_seated() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let channel = seed_membership_plane(&db, &OLD).await;
        let unseated = [0x1Bu8; 32];
        db.set_folder_member_access(&unseated, &OLD, "writer", None)
            .await
            .unwrap();
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        db.record_succession(&NEW, &THIRD, b"s2", 2)
            .await
            .unwrap()
            .unwrap();

        let carried = db
            .register_successor_carrying_seat(&THIRD, &channel)
            .await
            .unwrap();
        assert_eq!(carried.seats_retired, 1);
        assert_eq!(carried.grants, 1);
        assert!(!db.is_actor_in_channel(&OLD, &channel).await.unwrap());
        assert!(
            db.get_folder_member_role(&channel, &THIRD)
                .await
                .unwrap()
                .is_some_and(|r| r.access == "writer")
        );

        let none = db
            .register_successor_carrying_seat(&THIRD, &unseated)
            .await
            .unwrap();
        assert_eq!(none, crate::db::channels::SeatCarried::default());
        assert!(
            db.get_folder_member_role(&unseated, &OLD)
                .await
                .unwrap()
                .is_some(),
            "a grant never moves without its seat"
        );
    }

    /// The other direction, and the reason the cluster needed a pass rather than
    /// a footnote: `actor_channels` is the channel-fetch authorization, so a
    /// wrong `Stay` reads like it would strand the successor out of their own
    /// data. It does not, and the reason is structural — every folder gate
    /// (`folder_authz::{can_read_folder, resolve_readable_folder,
    /// resolve_writable_folder}`) takes an owner fast-path on `folders.actor_id`
    /// BEFORE consulting the roster, and `folders` moves.
    ///
    /// So the ownership half of this table is carried by a column that already
    /// moves, and what `Stay` leaves behind is participation and nothing else.
    /// If a later change makes the roster the owner's path too, this pin is what
    /// reds.
    #[tokio::test]
    async fn a_successor_reaches_its_own_sets_without_inheriting_a_roster_row() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.create_folder("the retired identity's own set", &OLD)
            .await
            .unwrap();
        let channel = seed_membership_plane(&db, &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The gate's own input moved: the successor owns the set outright.
        assert!(
            db.get_folder_for_actor("the retired identity's own set", &NEW)
                .await
                .unwrap()
                .is_some(),
            "the owner fast-path resolves on `folders.actor_id`, which moves — this is \
             what makes the roster's Stay cost the successor nothing"
        );
        assert!(
            db.get_folder_for_actor("the retired identity's own set", &OLD)
                .await
                .unwrap()
                .is_none()
        );
        // …and it got there without a roster row, which is the whole point.
        assert!(!db.is_actor_in_channel(&NEW, &channel).await.unwrap());
    }

    /// `channel_foreign_members` is the `sync_devices` case in its purest form:
    /// already re-pointed, by a leg that has shipped for as long as peer
    /// succession has, and never declared. This pin holds both halves at once —
    /// the LOCAL ceremony leaves it alone (vacuously: every row names an actor
    /// homed elsewhere, which `record_succession`'s locally-registered `?old`
    /// cannot match), and the PEER path is what moves it.
    #[tokio::test]
    async fn foreign_membership_moves_on_the_peer_path_and_never_on_the_local_one() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let channel = [0x2Au8; 32];
        let remote_old = [0x6Au8; 32];
        let remote_new = [0x6Bu8; 32];
        foreign_member(&db, &channel, &remote_old).await;

        // The local ceremony: no row here names a local actor, so its leg is
        // vacuous by construction. Declaring `Move` would assert a re-point that
        // does not happen — and the generic sweep would seed a synthetic row and
        // cheerfully witness the lie.
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            db.foreign_member_home_nest(&channel, &remote_old)
                .await
                .unwrap(),
            Some(PEER_NEST),
            "a local ceremony must not rewrite membership only a verified peer statement \
             may touch"
        );

        // The peer path, which owns this table's re-point and reports it.
        let applied = db
            .record_peer_succession(&remote_old, &remote_new, b"peer-statement", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            applied.foreign_memberships, 1,
            "the peer path is where foreign membership re-points, and it counts it"
        );
        assert!(
            db.foreign_member_home_nest(&channel, &remote_old)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.foreign_member_home_nest(&channel, &remote_new)
                .await
                .unwrap(),
            Some(PEER_NEST)
        );
    }

    /// Arm the two entitlement-authority surfaces a retired identity can leave
    /// standing: a provider config whose `webhook_secret` admits provider
    /// events, and the `ManageSubscribers` delegation the public federation
    /// bootstrap read serves.
    async fn seed_entitlement_authority(db: &CacheDb, actor: &[u8; 32]) {
        db.upsert_payment_provider(actor, "stripe", "whsec_the_thief_chose_this", "gold")
            .await
            .unwrap();
        db.upsert_device_authorization(actor, &[0x66u8; 32], b"the signed delegation bytes")
            .await
            .unwrap();
    }

    /// The entitlement-authority burn, asserted on the two **consumers that
    /// never consult the refusal plane**: the unauthenticated webhook ingress
    /// resolves a provider config straight out of the URL's actor id, and the
    /// delegation read is public federation bootstrap. Neither asks who is
    /// authenticating, so retiring the identity key reaches neither.
    ///
    /// The successor-side asserts are the half that would fail on a `Move`, and
    /// they are the point: the secret is one a seed thief could have chosen at
    /// `fauna.payments.providers.set`, so a moved row would let the thief forge
    /// verified payment events against the successor's own tiers.
    #[tokio::test]
    async fn the_ceremony_burns_the_retired_identitys_entitlement_authority() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_entitlement_authority(&db, &OLD).await;

        // Armed before the ceremony, so the asserts below cannot pass against a
        // plane that was never seeded.
        assert!(
            db.get_payment_provider(&OLD, "stripe")
                .await
                .unwrap()
                .is_some()
        );
        assert!(db.get_device_authorization(&OLD).await.unwrap().is_some());

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // Nothing admits an event on the retired identity any more...
        assert!(
            db.get_payment_provider(&OLD, "stripe")
                .await
                .unwrap()
                .is_none(),
            "a webhook posted to the retired identity's own URL must stop verifying — \
             the ingress reads the actor out of the path and never consults the refusal"
        );
        assert!(db.get_device_authorization(&OLD).await.unwrap().is_none());

        // ...and nothing is handed to the successor either.
        assert!(
            db.get_payment_provider(&NEW, "stripe")
                .await
                .unwrap()
                .is_none(),
            "a provider secret the thief may have chosen must not verify events \
             against the successor's tiers; the creator re-registers at the provider \
             dashboard, which is where the secret comes from and where the webhook URL \
             (which carries the actor id) has to be re-pointed anyway"
        );
        assert!(
            db.get_device_authorization(&NEW).await.unwrap().is_none(),
            "the stored envelope is signed by the predecessor and names it as \
             `actor_id`, so it can never authorize anything of the successor's — \
             moving it would only serve a stale cert under a live name"
        );
    }

    /// A mailing list is MANAGEABLE by the successor — asserted on the owner
    /// doors, because the list's other half moves whatever this table declares.
    ///
    /// A list is two rows: a `kind='list'` alias (the posting address, which
    /// moves as part of the ratified address leg) and the `mail_lists` row that
    /// owns it. Inbound delivery resolves through the alias and never consults
    /// the owner, so a list left behind stays **live** while every management
    /// door — all of them owner-scoped — refuses the only identity that could
    /// still reach it. That is the state this pin exists to make impossible.
    ///
    /// ⚠ `mail_lists` is in the registry gate's `SEED_BLIND` list, so the
    /// generic sweep does NOT witness this ruling; this test is the only thing
    /// standing behind it.
    #[tokio::test]
    async fn a_succession_hands_the_successor_the_mailing_list_it_now_addresses() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let (list_id, _alias_id) = db
            .create_list(
                &OLD,
                "example.com",
                "newsletter",
                Some("News"),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.list_lists_for_actor(&NEW).await.unwrap().len(),
            1,
            "the successor must be able to SEE the list whose address it now owns"
        );
        assert!(
            db.list_lists_for_actor(&OLD).await.unwrap().is_empty(),
            "and it must not still be listed under the identity that was retired"
        );
        assert!(
            db.get_list_for_owner(&list_id, &NEW)
                .await
                .unwrap()
                .is_some(),
            "every management door is owner-scoped, so a list left behind can be \
             edited, sent to and deleted by nobody at all — while it keeps \
             accepting mail, because inbound resolves through the alias and never \
             looks at the owner"
        );
    }

    /// A succession does **not** hand out a fresh day of list-sending quota.
    ///
    /// The ceremony is self-service, so this is the whole liability rule in one
    /// assert: if the meter stayed behind, `try_consume_list_quota` would read
    /// `unwrap_or(0)` for the successor and allow a second full day — and again
    /// on the next recovery, without limit.
    #[tokio::test]
    async fn a_succession_does_not_reset_the_list_senders_daily_quota() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let (list_id, _alias) = db
            .create_list(
                &OLD,
                "example.com",
                "newsletter",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let caps = ListSendCaps {
            per_send: 1_000,
            per_account_per_day: 100,
            per_deployment_per_day: 1_000_000,
        };
        let now_ms = 1_700_000_000_000;

        // The predecessor spends 90 of the day's 100 recipients.
        assert!(matches!(
            db.try_consume_list_quota(&list_id, &OLD, 90, caps, now_ms)
                .await
                .unwrap(),
            ListQuotaOutcome::Allowed { .. }
        ));

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // Same UTC day, same account, new identity: 20 more must NOT fit.
        let outcome = db
            .try_consume_list_quota(&list_id, &NEW, 20, caps, now_ms)
            .await
            .unwrap();
        assert!(
            matches!(
                outcome,
                ListQuotaOutcome::PerAccountDayExceeded { remaining: 10, .. }
            ),
            "the day's consumption must follow the account across the ceremony — a \
             limit a self-service recovery sheds is not a limit. Got {outcome:?}"
        );
    }

    /// A succession does **not** empty the anti-spam fan-out windows.
    ///
    /// The same liability rule one plane over, and the window is what makes it
    /// bite: `get_behavioral_profile` measures DISTINCT DM recipients over
    /// 1 h / 24 h / 7 d, so leaving the rows behind means the successor's very
    /// next burst is measured against no history at all.
    #[tokio::test]
    async fn a_succession_does_not_empty_the_spam_fanout_windows() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let now_us = 1_700_000_000_000_000;
        for i in 0..5u8 {
            let target = [0xD0 + i; 32];
            db.record_sender_event(&OLD, "dm_sent", Some(&target), now_us - 60_000_000)
                .await
                .unwrap();
        }
        let victim = [0xB1u8; 32];

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let profile = db
            .get_behavioral_profile(&NEW, &victim, now_us)
            .await
            .unwrap();
        assert_eq!(
            profile.unique_dm_recipients_1h, 5,
            "the successor's spray history must survive the ceremony that renamed it"
        );
        let stale = db
            .get_behavioral_profile(&OLD, &victim, now_us)
            .await
            .unwrap();
        assert_eq!(
            stale.unique_dm_recipients_1h, 0,
            "and must not be double-counted under the retired identity"
        );
    }

    /// The retired identity's export bearer token, on the **product observable
    /// that matters**: `validate_eviction_token` is the door
    /// `GET /api/v1/export` falls back to when the Authorization header is not a
    /// session token, and it hands back an actor id with no identity check at
    /// all. Whoever holds the string gets that actor's whole account as a zip.
    ///
    /// The successor-side assert is the one that would fail on a `Move`, and it
    /// is the whole ruling: the token's *value* follows the data, and the data
    /// moves. A seed thief can read the string before the ceremony (their app
    /// shows it — `account.eviction_token`), so a moved row would re-aim a
    /// credential the thief already holds at the successor's corpus.
    #[tokio::test]
    async fn the_ceremony_burns_the_retired_identitys_export_token() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.create_eviction_token("the-thief-read-this-string", &OLD, 1 << 40)
            .await
            .unwrap();

        // Armed: the door opens for the token before the ceremony.
        assert_eq!(
            db.validate_eviction_token("the-thief-read-this-string")
                .await
                .unwrap(),
            Some(OLD)
        );

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.validate_eviction_token("the-thief-read-this-string")
                .await
                .unwrap(),
            None,
            "an export token minted before the ceremony must open nothing after it — \
             the export door takes the token INSTEAD of an identity, so the refusal \
             plane never sees the caller"
        );
        assert!(
            db.get_eviction_token_for_actor(&NEW)
                .await
                .unwrap()
                .is_none(),
            "and it must not be handed to the successor: the string exports whatever \
             actor it names, so moving it aims a credential the thief may hold at the \
             successor's whole account"
        );
    }

    /// The share registry's move, asserted on the **kill switch** rather than on
    /// row ownership — because the kill switch is the only thing this table
    /// does, and it is scoped to `author`.
    ///
    /// **Why a burn would be a security regression here, which is the finding
    /// this pin exists to hold.** Registration is deliberately *not* a serving
    /// gate: `GET /share/{token}` serves an unregistered token statelessly, and
    /// consults this table only to refuse a **revoked** one. So deleting the row
    /// of a token the owner had already revoked does not end that token — it
    /// **un-revokes** it, because `is_share_token_revoked` answers `false` for a
    /// row that is not there. The revoked-token assert below is what pins that.
    #[tokio::test]
    async fn a_succession_hands_the_successor_the_share_kill_switch() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let live_token = [0x11u8; 32];
        let killed_token = [0x22u8; 32];
        for id in [live_token, killed_token] {
            db.register_share_token(&id, &OLD, &[0x33u8; 32], b"sealed", 1 << 40, true)
                .await
                .unwrap();
        }
        db.revoke_share_token(&killed_token, &OLD).await.unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The successor can SEE the shares the predecessor left live...
        assert_eq!(
            db.list_share_tokens_for_author(&NEW).await.unwrap().len(),
            2
        );
        assert!(
            db.list_share_tokens_for_author(&OLD)
                .await
                .unwrap()
                .is_empty()
        );

        // ...and can KILL one, which is the only remedy that exists: the token
        // itself is client-minted, stateless and signed by a key the ceremony
        // just retired, so nothing else can stop it serving.
        assert!(
            db.revoke_share_token(&live_token, &NEW).await.unwrap(),
            "revocation is scoped to `author`, so a share left on the retired \
             identity could never be revoked by anyone again"
        );
        assert!(db.is_share_token_revoked(&live_token).await.unwrap());

        // And a token the predecessor had already revoked STAYS revoked. This is
        // the assert that reds on a `Burn`: deleting the row un-revokes the
        // token, because an unregistered token is not a revoked one.
        assert!(
            db.is_share_token_revoked(&killed_token).await.unwrap(),
            "a burn would resurrect every share link the owner had already killed"
        );
    }

    /// A supervised invite code minted **before** the guardian's ceremony must
    /// seat the ward under the **successor** when it is redeemed after it.
    ///
    /// Asserted end to end through the real registration write rather than on
    /// the code row, because the row is only ever an input: `account_core`
    /// reads `validate_invite_code`'s guardian designation and hands it
    /// straight to `create_user_with_handle`, which writes the `guardianships`
    /// row. Left on the retired identity, the code seats a ward under an actor
    /// that can never authenticate again — the ward is supervised on paper and
    /// unmanageable in fact, which is the state `family-safety.md` § Lifecycle
    /// refuses and which the rest of this plane was ruled to prevent.
    #[tokio::test]
    async fn a_supervised_invite_code_seats_its_ward_under_the_successor() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.create_invite_code_with_guardian("code-minted-before", "free", 1, Some(&OLD), None)
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The designation the registration path reads.
        let grant = db
            .validate_invite_code("code-minted-before")
            .await
            .unwrap()
            .expect("the code still admits — a succession is not a revocation");
        let guardian = grant.guardian_actor;
        assert_eq!(
            guardian.as_deref(),
            Some(NEW.as_slice()),
            "the code must designate the guardian who still exists"
        );

        // ...and the link that designation actually writes.
        db.create_user_with_handle(&THIRD, "free", "the-ward", guardian.as_deref())
            .await
            .unwrap();
        let link = db.get_guardian_of(&THIRD).await.unwrap().unwrap();
        assert_eq!(
            link.guardian_actor_id,
            NEW.to_vec(),
            "a ward seated under a retired identity is supervised on paper and \
             unmanageable in fact — nobody can ever authenticate as their guardian"
        );
    }

    /// Two claims on the creator's ledger: one a buyer already redeemed (the
    /// audit trail), one still outstanding (bearer authority).
    async fn seed_claim_ledger(db: &CacheDb, actor: &[u8; 32]) {
        db.insert_payment_claim(
            "code-already-redeemed",
            actor,
            "gold",
            "stripe",
            "evt_the_buyer_paid",
            None,
        )
        .await
        .unwrap();
        db.redeem_payment_claim("code-already-redeemed", &THIRD)
            .await
            .unwrap();
        db.insert_payment_claim(
            "code-still-outstanding",
            actor,
            "gold",
            "manual",
            "manual_code-still-outstanding",
            None,
        )
        .await
        .unwrap();
        // A third: voided by a refund that landed before redemption. Its
        // `voided_at` is stamped to a distinctive constant rather than to `now`,
        // so the assert that the ceremony leaves it alone cannot pass by the two
        // timestamps happening to fall in the same second.
        db.insert_payment_claim(
            "code-refunded-before-redemption",
            actor,
            "gold",
            "stripe",
            "evt_refunded",
            None,
        )
        .await
        .unwrap();
        let conn = db.conn().await;
        conn.execute(
            "UPDATE payment_claim_codes SET voided_at = 5 WHERE code = ?1",
            rusqlite::params!["code-refunded-before-redemption"],
        )
        .unwrap();
    }

    /// The creator's claim ledger moves **whole** and is voided **in part**, and
    /// both halves are asserted because each answers a different failure.
    ///
    /// The move: left behind, the ledger is invisible to the successor
    /// (`claims.list` is keyed on `author_id`) and every code on it is dead
    /// anyway — the tier plane moved, so `redeem_claim`'s tier pre-check fails.
    /// The buyer's receipt has to travel with the tiers it names.
    ///
    /// The void: a claim code is a **bearer** credential. `redeem_claim` finds
    /// it by code alone and binds it to whoever presents it, so it consults the
    /// retired identity nowhere — and a seed thief could have minted an
    /// unbounded supply at `fauna.payments.claims.mint` before the ceremony.
    /// Nothing in any app can revoke one (there is no `claims.void` gesture;
    /// `void_payment_claim` is reachable only from the refund path), so carrying
    /// them live would hand the successor exactly the unrevocable standing
    /// authority `nest_pairings` burns for.
    #[tokio::test]
    async fn a_succession_carries_the_claim_ledger_and_voids_what_was_never_redeemed() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_claim_ledger(&db, &OLD).await;

        assert_eq!(db.list_payment_claims(&OLD).await.unwrap().len(), 3);

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The whole ledger is the successor's now — nothing is deleted, which
        // the schema requires of a table whose rows are the payment audit trail
        // ("rows are kept for audit, never deleted").
        assert!(db.list_payment_claims(&OLD).await.unwrap().is_empty());
        assert_eq!(
            db.list_payment_claims(&NEW).await.unwrap().len(),
            3,
            "the receipt has to travel with the tiers it names, or the successor \
             cannot honour a buyer who paid"
        );

        // The redeemed row keeps its audit shape untouched: it is history of a
        // sale that really happened, and `apply_refund` still resolves through
        // it.
        let redeemed = db
            .get_payment_claim("code-already-redeemed")
            .await
            .unwrap()
            .unwrap();
        assert!(redeemed.redeemed_at.is_some());
        assert!(
            redeemed.voided_at.is_none(),
            "a redeemed claim is a completed sale, not standing authority — \
             voiding it would rewrite history and break the refund path"
        );

        // The outstanding one is dead on arrival, which is the direction that
        // costs the thief and not the buyer: the row survives with its
        // `external_ref` intact, so the successor can verify the payment in the
        // provider dashboard and mint a fresh code.
        let outstanding = db
            .get_payment_claim("code-still-outstanding")
            .await
            .unwrap()
            .unwrap();
        assert!(
            outstanding.voided_at.is_some(),
            "an unredeemed claim is a bearer credential the thief could have minted, \
             and no app can revoke one — the ceremony is the only place it can die"
        );
        assert!(outstanding.redeemed_at.is_none());
        assert_eq!(outstanding.external_ref, "manual_code-still-outstanding");

        // A claim a refund already voided keeps that refund's timestamp: the
        // stamp is history of the refund, and the ceremony has nothing to say
        // about it. This is what `COALESCE` in the leg buys.
        let refunded = db
            .get_payment_claim("code-refunded-before-redemption")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            refunded.voided_at,
            Some(5),
            "the ceremony must not restamp a void a refund already recorded"
        );
    }

    // ── The content plane ────────────
    //
    // `content.author` is the nest-side OWNERSHIP pointer, never the
    // attribution: "posts stay attributed" binds the signed post bytes, which
    // no leg touches. Every production writer derives the column from the
    // decoded payload's own author field, so `author = ?old` matches exactly
    // the rows the retired identity signed — bridge-ingested rows carry the
    // remote author's id and the undecodable-raw arm writes a zero author,
    // both structurally unreachable by the plain leg.

    /// Seed a legacy `content` post row the way `put_post` writes one.
    async fn seed_legacy_post(db: &CacheDb, id: &[u8; 32], author: &[u8; 32]) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO content (id, schema, author, created_at, payload, source)
             VALUES (?1, 'post/text', ?2, 1000, x'', 'fauna')",
            rusqlite::params![id.as_slice(), author.as_slice()],
        )
        .unwrap();
    }

    /// Seed the post's segment record — the post-cutover authoritative twin
    /// (`segments::post::lookup_scope_by_post_id` resolves through this row).
    async fn seed_post_segment_record(db: &CacheDb, id: &[u8; 32], scope: &[u8; 32]) {
        let cid = fauna_cbor::Cid::from_digest_dag_cbor(*id);
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO segment_records
                (scope_id, kind, segment_id, record_cid, bucket)
             VALUES (?1, 'post', 1, ?2, 'b0')",
            rusqlite::params![scope.as_slice(), &cid.as_bytes()[..]],
        )
        .unwrap();
    }

    /// The live defect this exists for (`succession-aftermath.md`
    /// § Implementation status today, found 2026-08-14): after a ceremony a
    /// successor could not delete its own legacy posts, because
    /// `check_post_delete_authorization` compares the caller against the STORED
    /// author and `content.author` never moved. The two stored-author checks
    /// are conjunctive, so the defect bit both shapes: a legacy-only row
    /// (`content` alone) and a cutover row whose `segment_records.scope_id`
    /// twin already moved while the `content` row still named the predecessor.
    ///
    /// The fix is deliberately NOT a succession-chain lookup in the delete
    /// path: the ruling moves the ownership column, so the ordinary equality
    /// keeps being the whole rule ("no second authorization rule anywhere" —
    /// `succession-aftermath.md` § Re-key scope; the single-owner read-only
    /// contract on this function is untouched).
    #[tokio::test]
    async fn a_successor_can_retract_its_own_back_catalogue() {
        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        seed_account(&db, &OLD, "alice").await;
        let legacy_only = [0x51u8; 32];
        let cutover = [0x52u8; 32];
        seed_legacy_post(&db, &legacy_only, &OLD).await;
        seed_legacy_post(&db, &cutover, &OLD).await;
        seed_post_segment_record(&db, &cutover, &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let state = std::sync::Arc::new(crate::AppState::for_test(db.clone()));
        for post in [legacy_only, cutover] {
            let tomb = fauna_core::data::Tombstone {
                author: fauna_core::identity::ActorId(NEW),
                post_id: fauna_cbor::Cid::from_digest_dag_cbor(post),
                created_at: fauna_core::data::Timestamp(2_000),
            };
            let verdict =
                crate::routes::check_post_delete_authorization(&state, NEW, &tomb, post).await;
            assert!(
                matches!(verdict, Ok(None)),
                "a post authored before the ceremony must be deletable by the \
                 successor after it — this is the client-reachable state no \
                 client could fix (post {:02x})",
                post[0]
            );
        }

        // The rule itself is unchanged: a different account's tombstone still
        // refuses, in both storage shapes.
        for post in [legacy_only, cutover] {
            let tomb = fauna_core::data::Tombstone {
                author: fauna_core::identity::ActorId(THIRD),
                post_id: fauna_cbor::Cid::from_digest_dag_cbor(post),
                created_at: fauna_core::data::Timestamp(2_000),
            };
            let verdict =
                crate::routes::check_post_delete_authorization(&state, THIRD, &tomb, post).await;
            assert!(
                matches!(verdict, Err(crate::routes::PostDeleteError::NotAuthor)),
                "moving the ownership column must not widen who may delete"
            );
        }
    }

    /// `fauna.posts.list` enumerates by `content.author` — a successor whose
    /// catalogue stayed behind sees an EMPTY own-post list, silently agreeing
    /// with the broken state (the `web_subdomain_enabled` shape).
    #[tokio::test]
    async fn a_succession_keeps_the_successors_own_post_list_whole() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_legacy_post(&db, &[0x53u8; 32], &OLD).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.list_authored_posts_page(&NEW, None, 10)
                .await
                .unwrap()
                .len(),
            1,
            "the successor's own-post enumeration must include the pre-ceremony \
             catalogue it now owns"
        );
        assert!(
            db.list_authored_posts_page(&OLD, None, 10)
                .await
                .unwrap()
                .is_empty(),
            "and the retired identity's enumeration must be empty"
        );
    }

    /// `content_scores.actor_id` is the trust boundary `content_score_owner`
    /// enforces on `submit_scores` (a holder's claimed owner must match the
    /// row). Left behind, every
    /// legitimate re-score of the successor's legacy corpus is refused forever:
    /// the pre-ceremony grants burn and re-mint naming the successor, while the
    /// binding still names the predecessor.
    #[tokio::test]
    async fn a_succession_repoints_the_re_score_owner_binding() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let cid = [0x55u8; 32];
        db.insert_content_scores(
            &cid,
            "mail",
            Some(&OLD),
            1_000,
            &[fauna_core::scoring::ScoreEntry {
                factor: fauna_core::scoring::factor::SPAM.into(),
                score: 500,
                tier: fauna_core::scoring::TIER_USER,
                scorer_version: 1,
            }],
        )
        .await
        .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.content_score_owner(&cid).await.unwrap(),
            Some(NEW),
            "the re-score owner binding must follow the corpus it scopes, or \
             the scanning plane stops updating for the whole legacy mailbox"
        );
    }

    /// `engagement_events.actor_id` feeds the trend export's k-anonymity gate
    /// (`COUNT(DISTINCT actor_id)` — `export_trend_entries`), so an un-moved
    /// log counts one human as two distinct engagers: a below-k engagement
    /// pattern crosses the wire with fewer real people behind it, and the
    /// already-counted constraint is shed by a self-service ceremony.
    ///
    /// Declared residual, deliberate: toggle event ids are content-addressed
    /// to the identity that acted (`compute_toggle_event_id`), so a successor
    /// cannot retract a pre-ceremony like and a re-like double-counts one
    /// counter tick per (post, type). Bounded, decays with trending, and
    /// repairable by a bespoke event-id re-key if it ever matters.
    #[tokio::test]
    async fn a_succession_counts_one_human_once_at_the_k_gate() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let post = [0x56u8; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO content_meta (content_id) VALUES (?1)",
                rusqlite::params![post.as_slice()],
            )
            .unwrap();
        }
        let now_us = 1_700_000_000_000_000i64;
        // Exactly k = TREND_MIN_ENGAGERS distinct engagers, the predecessor
        // among them, so the entry sits exactly at the export threshold.
        for actor in [OLD, [0x91u8; 32], [0x92u8; 32]] {
            let ev = fauna_core::engagement::compute_toggle_event_id(
                &fauna_core::identity::ActorId(actor),
                &fauna_core::data::ContentHash::from_digest_raw(post),
                "like",
            );
            db.insert_engagement_event(&ev.digest(), &post, Some(&actor[..]), "like", None, now_us)
                .await
                .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The successor re-likes a post its predecessor had already liked —
        // the same human, acting again.
        let ev = fauna_core::engagement::compute_toggle_event_id(
            &fauna_core::identity::ActorId(NEW),
            &fauna_core::data::ContentHash::from_digest_raw(post),
            "like",
        );
        db.insert_engagement_event(&ev.digest(), &post, Some(&NEW[..]), "like", None, now_us)
            .await
            .unwrap();

        let entries = db.export_trend_entries(now_us + 1_000_000).await.unwrap();
        let entry = entries
            .iter()
            .find(|e| e.0 == post)
            .expect("the at-threshold entry must still export");
        assert_eq!(
            entry.2,
            fauna_core::scoring::trends::TREND_MIN_ENGAGERS,
            "one human must count once at the k-gate — an un-moved log inflates \
             a below-k pattern over the export threshold"
        );
    }

    /// `feature_usage` is the per-account per-day feature quota — the ratified
    /// "a limit a self-service ceremony sheds is not a limit" class
    /// (`succession-aftermath.md` § Re-key scope, 2026-08-14). The inherited
    /// cost is one UTC day's consumption, which the rollover zeroes.
    #[tokio::test]
    async fn a_succession_does_not_reset_the_feature_days_spend() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let today = 100i64;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO feature_usage (actor_id, feature, dimension, day, amount)
                 VALUES (?1, 'payments', 'operations', ?2, 7)",
                rusqlite::params![OLD.as_slice(), today],
            )
            .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let counters = db
            .feature_usage_counters(
                &NEW,
                fauna_core::feature_gate::GatedFeature::Payments,
                today,
            )
            .await
            .unwrap();
        assert_eq!(
            counters.operations.day, 7,
            "the day's spend must follow the account across the ceremony — the \
             verdict reads these counters, so a left-behind bucket is a fresh \
             quota on demand"
        );
    }

    /// `sync_changes`' move, asserted end to end on the reader that was blind —
    /// the registry gate checks which actor owns the rows, not that the feed
    /// answers, and "the feed answers" is the whole reason this table is ruled.
    #[tokio::test]
    async fn the_successors_actor_keyed_change_feed_answers_after_the_ceremony() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let fs = db.create_folder("photos", &OLD).await.unwrap();
        db.record_sync_change(
            &OLD,
            &[0x11u8; 32],
            Some(&[0x22u8; 32]),
            17,
            "upsert",
            Some(fs),
            None,
            Some("holiday.jpg"),
        )
        .await
        .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The actor-wide journal read — the shape the `fauna.sync.changes.list`
        // no-`folder` arm used to serve before the wire kind refused it (there
        // is no actor-wide feed). It answered "no changes" while the
        // successor owned every byte, so the broken state and the displayed
        // state agreed.
        let successor_feed = db.get_sync_changes(&NEW, 0).await.unwrap();
        assert_eq!(
            successor_feed.len(),
            1,
            "the successor's own change journal must follow the corpus that \
             already moves"
        );
        assert!(
            db.get_sync_changes(&OLD, 0).await.unwrap().is_empty(),
            "nothing may be left answering for the retired identity"
        );
    }

    #[tokio::test]
    async fn the_home_nest_repoints_its_own_contact_residue_like_a_peer() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_account(&db, &THIRD, "bob").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO contacts (actor_id, peer_id, status, created_at) VALUES
                    (?1, ?2, 'accepted', 1),  -- the old user's own edge to bob
                    (?2, ?1, 'accepted', 1),  -- bob's edge naming the old id
                    (?2, ?3, 'accepted', 1),  -- bob already knows the successor
                    (?1, ?3, 'accepted', 1)   -- old's edge to its own successor
                 ",
                rusqlite::params![OLD.as_slice(), THIRD.as_slice(), NEW.as_slice()],
            )
            .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        let edges: Vec<(Vec<u8>, Vec<u8>)> = conn
            .prepare("SELECT actor_id, peer_id FROM contacts ORDER BY actor_id, peer_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        // The home nest is peer zero: its own residue re-points exactly as
        // `record_peer_succession` re-points a peer's. Bob's pre-existing edge
        // to the successor absorbs the superseded one (no duplicate), and the
        // old id's edge to its own successor dies rather than becoming a
        // self-edge.
        assert_eq!(
            edges,
            vec![
                (NEW.to_vec(), THIRD.to_vec()),
                (THIRD.to_vec(), NEW.to_vec()),
            ]
        );
    }

    /// The boot heal's work list: every local predecessor at the **terminal**
    /// successor of its chain, newest hop first — and nothing on a nest with
    /// no successions. The rows are the transaction's, so the list is all the
    /// heal reads: a crash between commit and dir rename must still find its
    /// pair, even though the corpus already sits on the successor.
    #[tokio::test]
    async fn the_heal_pairs_name_every_local_predecessor_at_its_terminal() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(db.succession_heal_pairs().await.unwrap().is_empty());

        seed_account(&db, &OLD, "alice").await;
        seed_corpus(&db, &OLD).await;
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(db.succession_heal_pairs().await.unwrap(), vec![(OLD, NEW)]);
        assert_eq!(
            corpus_rows_owned_by(&db, &OLD).await,
            [0; 5],
            "the transaction moved every row — the heal owes the rows nothing"
        );

        db.record_succession(&NEW, &THIRD, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            db.succession_heal_pairs().await.unwrap(),
            vec![(NEW, THIRD), (OLD, THIRD)],
            "both predecessors heal straight to the terminal, newest hop first"
        );
        assert_eq!(quota_counters_of(&db, &THIRD).await, (7, 11));
    }

    /// Stand up the `nostr` schema and give `actor` an account, a live bunker
    /// connection and a designated zap signer — the three shapes
    /// `record_succession`'s Nostr legs treat differently (move, revoke,
    /// delete). `tag` keeps the `UNIQUE` pubkey columns distinct between two
    /// actors in one test.
    #[cfg(feature = "nostr")]
    async fn seed_nostr_identity(db: &CacheDb, actor: &[u8; 32], tag: &str) {
        let conn = db.conn.lock().await;
        crate::nostr::apply_schema(&conn).unwrap();
        let hex = hex::encode(actor);
        crate::nostr::db::link_account(
            &conn,
            &hex,
            &format!("npub-{tag}"),
            "nsec_deposited",
            Some(b"sealed-nsec-bytes"),
            None,
            None,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nostr_bunker_apps (actor_id, app_pubkey, status, created_at, expires_at)
             VALUES (?1, ?2, 'active', 1, 9999999999)",
            rusqlite::params![hex, format!("app-{tag}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nostr_zap_signers (actor_id, signer_pubkey, created_at)
             VALUES (?1, ?2, 1)",
            rusqlite::params![hex, format!("zapper-{tag}")],
        )
        .unwrap();
        // A PK-keyed satellite (`actor_id TEXT PRIMARY KEY`), so the fixtures
        // exercise the collision-guarded arm and not only the plain ones.
        conn.execute(
            "INSERT INTO nostr_bunker_signers
                (actor_id, signer_pubkey, encrypted_privkey, created_at)
             VALUES (?1, ?2, x'00', 1)",
            rusqlite::params![hex, format!("signer-{tag}")],
        )
        .unwrap();
    }

    #[cfg(feature = "nostr")]
    async fn nostr_counts(db: &CacheDb, actor: &[u8; 32]) -> (i64, i64, i64) {
        let conn = db.conn.lock().await;
        let hex = hex::encode(actor);
        let q = |sql: &str| -> i64 {
            conn.query_row(sql, rusqlite::params![hex], |r| r.get(0))
                .unwrap()
        };
        (
            q("SELECT COUNT(*) FROM nostr_accounts WHERE actor_id = ?1"),
            q("SELECT COUNT(*) FROM nostr_bunker_apps WHERE actor_id = ?1 AND status = 'active'"),
            q("SELECT COUNT(*) FROM nostr_zap_signers WHERE actor_id = ?1"),
        )
    }

    /// The ceremony's own Nostr legs: the account moves, every bunker
    /// connection is revoked, every zap-signer designation is deleted.
    #[cfg(feature = "nostr")]
    #[tokio::test]
    async fn the_ceremony_moves_the_npub_and_revokes_standing_nostr_authority() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_nostr_identity(&db, &OLD, "old").await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(nostr_counts(&db, &OLD).await, (0, 0, 0));
        let (accounts, active_apps, zappers) = nostr_counts(&db, &NEW).await;
        assert_eq!(accounts, 1, "the npub must follow the account");
        assert_eq!(active_apps, 0, "every bunker connection is revoked");
        assert_eq!(zappers, 0, "every zap-signer designation is deleted");
    }

    // ── The counterparty columns (2026-08-15) ───────────────────────
    //
    // Pinned on the aggregates themselves rather than on row counts, because the
    // defect these rulings close is not "a stale id" but "the nest believes there
    // are two humans where there is one" — and only the counting doors can say
    // that. ⚠ Pinned by hand: a `SUCCESSION_REFERENCES` leg is witnessed by no
    // data-driven gate at all (the measurement).

    /// **A DM recipient who succeeds must not become two recipients.** The
    /// sender's spam profile counts DISTINCT `target_actor` over its windows, so
    /// an un-moved reference inflates an uninvolved sender's fan-out toward the
    /// thresholds.
    #[tokio::test]
    async fn a_dm_recipient_who_succeeds_is_still_one_recipient() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let sender = [0xdd; 32];
        let now = 1_000_000_000i64;

        db.record_sender_event(&sender, "dm_sent", Some(&OLD), now)
            .await
            .unwrap();
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        db.record_sender_event(&sender, "dm_sent", Some(&NEW), now + 1)
            .await
            .unwrap();

        let profile = db
            .get_behavioral_profile(&sender, &NEW, now + 2)
            .await
            .unwrap();
        assert_eq!(
            profile.unique_dm_recipients_24h, 1,
            "one human is one recipient: the fan-out windows count DISTINCT \
             `target_actor`, so a recipient left behind by their own ceremony \
             makes an uninvolved sender look like a fan-out spammer"
        );
    }

    const LABELLED_POST: &str = "7777777777777777777777777777777777777777777777777777777777777777";

    /// A label `who` hand-attaches to a post, as `fauna.labels.attach` writes
    /// it: the caller in BOTH writer columns.
    async fn self_label(db: &CacheDb, who: &[u8; 32], confidence: f64) {
        db.upsert_content_label(
            "post",
            LABELLED_POST,
            "nsfw",
            confidence,
            0,
            who,
            1,
            0,
            None,
            None,
            1,
            who,
            &[],
        )
        .await
        .unwrap();
    }

    /// Every `nsfw` row on the labelled post: confidence, classifier, scanner.
    async fn self_labels(db: &CacheDb) -> Vec<(f64, Vec<u8>, Vec<u8>)> {
        let conn = db.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT confidence, classifier_id, scanner_id FROM content_labels
                 WHERE content_id = ?1 AND category = 'nsfw' ORDER BY id",
            )
            .unwrap();
        stmt.query_map([LABELLED_POST], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// **An author who succeeds can still revise their own label.** A label has
    /// no detach door: its writer revises it by attaching again, and the upsert
    /// keys on `classifier_id`. Left naming the retired identity, the old row
    /// is one the successor can never reach — their revision lands BESIDE it,
    /// and every reader takes the highest confidence of the two, so a
    /// self-label (or one a seed thief attached in their name) could be raised
    /// for ever and never lowered.
    #[tokio::test]
    async fn an_author_who_succeeds_still_revises_their_own_label() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        self_label(&db, &OLD, 0.9).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        self_label(&db, &NEW, 0.1).await;

        assert_eq!(
            self_labels(&db).await,
            vec![(0.1, NEW.to_vec(), NEW.to_vec())],
            "ONE row, the successor's in both writer columns, carrying the \
             revision — not the retired identity's 0.9 standing beside it"
        );
    }

    /// Found a ceremony-born — floor-authoritative — room with `owner` in the
    /// owner seat and `seats` beside it, every seat homed on this nest and
    /// carrying a wrap target.
    async fn found_floor_room(
        db: &CacheDb,
        room: [u8; 32],
        owner: [u8; 32],
        seats: &[([u8; 32], &str)],
    ) {
        let seat = |id: [u8; 32], role: &str| crate::db::rooms::ReportedMember {
            principal_id: id,
            principal_kind: "user".into(),
            role: Some(role.into()),
            home_node_url: String::new(),
            reception_pubkey: vec![0x7e; 8],
        };
        let mut members = vec![seat(owner, "owner")];
        members.extend(seats.iter().map(|(id, role)| seat(*id, role)));
        assert!(
            db.found_room(
                &room,
                "community",
                &owner,
                1,
                b"policy",
                &[0x5a; 32],
                &members
            )
            .await
            .unwrap()
        );
    }

    /// A principal's floor row — live or `Removed`-absorbed — as
    /// `(role, removed_at, entry_id, reception_pubkey)`.
    async fn floor_seat_of(
        db: &CacheDb,
        room: &[u8; 32],
        who: &[u8; 32],
    ) -> Option<(
        Option<String>,
        Option<i64>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
    )> {
        db.list_floor_roster_including_removed(room)
            .await
            .unwrap()
            .into_iter()
            .find(|m| &m.principal_id == who)
            .map(|m| (m.role, m.removed_at, m.entry_id, m.reception_pubkey))
    }

    /// **A floor seat passes to the successor; a mirror seat stays** — the
    /// row-level rule `room_members`' `Partial` ruling names this test for.
    ///
    /// A ceremony-born room's floor is its membership authority and no member
    /// report will ever arrive for it, so a seat left on the retired identity is
    /// a seat nobody can ever use again — for the owner, the unremovable one
    /// every owner-only door keys on. An end-to-end room's rows only mirror its
    /// MLS group, whose add-successor commit is reported through the table's
    /// own mechanism, so the ceremony must not seat anybody there on its own
    /// say-so. The successor lands at a FRESH roster entry with NO wrap target:
    /// the predecessor's reception key is derived from the seed the ceremony
    /// exists to retire, so carrying it would keep wrapping every new generation
    /// to the thief.
    #[tokio::test]
    async fn a_succession_hands_each_floor_seat_to_the_successor_and_leaves_mirror_seats() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let other = [0x0c; 32];
        let owned = [0x11; 32];
        let administered = [0x22; 32];
        let already_left = [0x33; 32];
        let mirrored = [0x44; 32];
        found_floor_room(&db, owned, OLD, &[(other, "member")]).await;
        found_floor_room(&db, administered, other, &[(OLD, "admin")]).await;
        found_floor_room(&db, already_left, other, &[(OLD, "member")]).await;
        assert!(db.unseat_room_member(&already_left, &OLD).await.unwrap());
        let reported = |id: [u8; 32]| crate::db::rooms::ReportedMember {
            principal_id: id,
            principal_kind: "user".into(),
            role: None,
            home_node_url: String::new(),
            reception_pubkey: Vec::new(),
        };
        db.replace_floor_roster(
            &mirrored,
            "end_to_end",
            "",
            None,
            None,
            &[reported(OLD), reported(other)],
        )
        .await
        .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let (role, removed, entry, key) = floor_seat_of(&db, &owned, &NEW)
            .await
            .expect("the owner's successor is seated");
        assert_eq!(role.as_deref(), Some("owner"));
        assert_eq!(removed, None);
        assert_eq!(key, None, "never the predecessor's wrap target");
        let (old_role, old_removed, old_entry, _) = floor_seat_of(&db, &owned, &OLD)
            .await
            .expect("the predecessor's row stays as history");
        assert_eq!(old_role.as_deref(), Some("owner"));
        assert!(old_removed.is_some(), "and is Removed-absorbed");
        assert!(
            entry.is_some() && entry != old_entry,
            "a fresh roster entry"
        );

        assert_eq!(
            db.get_room_member_role(&administered, &NEW)
                .await
                .unwrap()
                .as_deref(),
            Some("admin"),
            "an admin's successor holds the admin seat"
        );
        assert_eq!(
            db.get_room_member_role(&administered, &OLD).await.unwrap(),
            None
        );

        assert_eq!(
            floor_seat_of(&db, &already_left, &NEW).await,
            None,
            "a seat the predecessor had already left is not resurrected"
        );
        assert!(
            db.is_room_member(&mirrored, &OLD).await.unwrap(),
            "a mirror room's row is its MLS group's to move"
        );
        assert_eq!(floor_seat_of(&db, &mirrored, &NEW).await, None);
    }

    /// **A standing invitation follows the account, because its envelope
    /// already does** — the witness `room_invites`' `Move` ruling rests on.
    ///
    /// An invitation is three things written as one act
    /// (`record_room_invite_and_deliver`): the row the accept door reads, the
    /// un-acked inbox envelope that is the invitee's only way to learn the room
    /// id, and the quota charge for that envelope. The ceremony already carries
    /// two of them — undelivered `content_links` move, and `inbox_bytes_used`
    /// is summed onto the successor. A row left on the retired identity would
    /// leave the successor holding a charged knock the accept door refuses, and
    /// nobody can clear such a row: there is no revoke door, and
    /// `unseat_room_member` deletes an invitation only beside a live seat.
    #[tokio::test]
    async fn a_pending_room_invitation_follows_the_account_with_its_envelope() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute("UPDATE tiers SET max_inbox_bytes = 1000", [])
                .unwrap();
        }
        let inviter = [0x0c; 32];
        let room = [0x11; 32];
        found_floor_room(&db, room, inviter, &[]).await;
        assert!(
            db.record_room_invite_and_deliver(
                &room,
                &OLD,
                &inviter,
                "member",
                "",
                b"signed",
                b"envelope",
                true,
            )
            .await
            .unwrap()
        );

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert!(
            db.get_pending_room_invite(&room, &OLD)
                .await
                .unwrap()
                .is_none(),
            "nothing is left standing for the retired identity"
        );
        let pending = db
            .get_pending_room_invite(&room, &NEW)
            .await
            .unwrap()
            .expect("the invitation stands for the successor");
        assert_eq!(pending.inviter_id, inviter, "and still names who issued it");

        assert!(
            db.accept_room_invite(&room, &NEW, &[], None)
                .await
                .unwrap()
                .seated()
        );
        assert!(db.is_room_member(&room, &NEW).await.unwrap());
        let conn = db.conn.lock().await;
        let (charged, standing): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT inbox_bytes_used FROM users WHERE actor_id = ?1),
                        (SELECT COUNT(*) FROM content_links WHERE status = 'undelivered')",
                rusqlite::params![NEW.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (charged, standing),
            (0, 0),
            "accepting consumed the envelope the ceremony carried over and refunded \
             the charge it carried with it"
        );
    }

    /// **A content reporter who succeeds must not become two flags.** Same
    /// counterparty class as the reputation reporter above and graded beside it,
    /// but this one is an `ACTOR_TABLES` entry, so it rides the registry loop
    /// with no hand-written leg — the pin is what says the *ruling* is right,
    /// since the data-driven sweep only ever says the declaration executed.
    ///
    /// The door is the real one: `gated_report_score` counts rows for a
    /// (content_hash, factor) pair, one-per-reporter being enforced by the
    /// primary key rather than by a DISTINCT clause. Two rows means two accusers
    /// against a third party's content.
    #[tokio::test]
    async fn a_content_reporter_who_succeeds_is_still_one_flag() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let content = [0x77; 32];

        let key = crate::db::reports::ReportKey {
            content_hash: content,
            factor: "spam".to_string(),
            content_kind: "post".to_string(),
        };
        // insert_content_report is gated on the reporter's own opt-in
        // — seed OLD's only: the succession's plain-move loop re-points
        // OLD's own spam_preferences row onto NEW (ceremony path, a hard
        // UPDATE), so NEW inherits the opt-in from the same row — seeding
        // NEW's row too would collide with that re-point.
        db.set_share_reports(&OLD, true).await.unwrap();
        db.insert_content_report(&key, &OLD, crate::db::reports::REPORT_PREF_COLUMN)
            .await
            .unwrap();
        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        db.insert_content_report(&key, &NEW, crate::db::reports::REPORT_PREF_COLUMN)
            .await
            .unwrap();

        let conn = db.conn.lock().await;
        let flags: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM content_reports WHERE content_hash = ?1 AND factor = ?2",
                rusqlite::params![&content[..], "spam"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            flags, 1,
            "the moderation consensus counts rows for a (content_hash, factor) \
             pair with one-per-reporter enforced by the primary key — so a \
             reporter left behind by their own ceremony flags the same content \
             twice and moves a third party's content toward exposure"
        );
    }

    /// **A feed contributor who succeeds keeps reaching the feeds that collect
    /// them.** The row belongs to somebody *else's* feed, and the discovery loop
    /// reads this column as "where does this author live" — so a `Stay` silently
    /// stops that person's posts arriving, without the feed's owner doing
    /// anything or being able to see why.
    ///
    /// Asserted through the production door's own query (a private method on the
    /// discovery worker, so the statement is reproduced rather than called).
    #[tokio::test]
    async fn a_feed_contributor_who_succeeds_still_resolves_to_their_nest() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO feed_contributors
                    (feed_id, nest_url, author_id, poll_priority, created_at)
                 VALUES ('someone-elses-feed', 'https://home.example', ?1, 'warm', 1)",
                rusqlite::params![OLD.to_vec()],
            )
            .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        let home: Option<String> = conn
            .query_row(
                "SELECT nest_url FROM feed_contributors WHERE author_id = ?1 LIMIT 1",
                rusqlite::params![NEW.to_vec()],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(
            home.as_deref(),
            Some("https://home.example"),
            "the discovery loop resolves an author's home through this column, so \
             a contributor left behind by their own ceremony drops out of every \
             feed that collects them — a third party's feed quietly going empty"
        );
    }

    // ── The two feature-gated bridge planes (2026-08-15) ────────────
    //
    // ⚠ Both groups sit in `FEATURE_GATED_ELSEWHERE`, so **neither data-driven
    // sweep witnesses a single ruling below** — the generic seeder cannot even
    // stand these tables up. Every pin here therefore asserts through a real
    // product door (the name an arrival resolves, the cross-post setting the
    // write-through reads) rather than through a row count, per the
    // standing instruction and the measurement that the sweep grades
    // execution and never correctness.

    /// Stand up the ActivityPub schema and give `actor` the three shapes the
    /// plane's ruling covers: the account whose `username` every arrival
    /// resolves through, an accepted inbound follower, and a pushed-note row
    /// (the Delete witness). `name` is the frozen username — in production it is
    /// minted from the Fauna handle, which is exactly why this plane moves.
    #[cfg(feature = "activitypub")]
    async fn seed_ap_identity(db: &CacheDb, actor: &[u8; 32], name: &str) {
        crate::activitypub::init_db(db).await.unwrap();
        let conn = db.conn.lock().await;
        let hex = hex::encode(actor);
        crate::activitypub::db_helpers::create_account(
            &conn,
            &hex,
            name,
            &format!("https://nest.example/ap/users/{name}"),
            b"sealed-rsa-der",
            "-----BEGIN PUBLIC KEY-----",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ap_follows
                (local_actor_id, remote_actor_uri, direction, state, created_at)
             VALUES (?1, ?2, 'inbound', 'accepted', 1)",
            rusqlite::params![hex, format!("https://remote.example/users/fan-of-{name}")],
        )
        .unwrap();
        crate::activitypub::db_helpers::insert_post_map(
            &conn,
            &format!("post-of-{name}"),
            &format!("https://nest.example/ap/users/{name}/notes/1"),
            &hex,
            None,
        )
        .unwrap();
    }

    /// Read the plane through its own doors: who the username resolves to, how
    /// many accepted followers the actor has, and whether its pushed note is
    /// still findable as *its* note.
    #[cfg(feature = "activitypub")]
    async fn ap_view(db: &CacheDb, actor: &[u8; 32], name: &str) -> (Option<String>, usize, bool) {
        let conn = db.conn.lock().await;
        let hex = hex::encode(actor);
        let resolved = crate::activitypub::db_helpers::get_account_by_username(&conn, name)
            .unwrap()
            .map(|a| a.actor_id);
        let followers = crate::activitypub::db_helpers::list_followers(&conn, &hex)
            .unwrap()
            .len();
        let note = crate::activitypub::db_helpers::get_local_note_url(
            &conn,
            &format!("post-of-{name}"),
            &hex,
        )
        .unwrap()
        .is_some();
        (resolved, followers, note)
    }

    /// **The ceremony carries the fediverse identity, and the door that proves
    /// it is the one every arrival uses.** `alice` is the frozen AP username,
    /// minted from the Fauna handle that the ceremony moves — so after the
    /// succession the name must resolve to the SUCCESSOR. Left un-ruled it
    /// resolves to the retired actor forever: WebFinger hands remotes an actor
    /// URL nobody can authenticate as, the per-user inbox files every arriving
    /// Create and Follow under a dead identity, and the successor enabling
    /// federation is pushed onto `alice-2` by the taken-name suffix loop, with
    /// no way to ever reclaim its own name (the unlink door is caller-scoped).
    #[cfg(feature = "activitypub")]
    #[tokio::test]
    async fn a_succession_carries_the_fediverse_identity_to_the_successor() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_ap_identity(&db, &OLD, "alice").await;

        assert_eq!(
            ap_view(&db, &OLD, "alice").await,
            (Some(hex::encode(OLD)), 1, true),
            "baseline: the predecessor owns the name, the follower and the note"
        );

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let (resolved, followers, note) = ap_view(&db, &NEW, "alice").await;
        assert_eq!(
            resolved,
            Some(hex::encode(NEW)),
            "the username every AP arrival resolves through must name the successor — \
             otherwise the handle moved and the fediverse identity built on it did not"
        );
        assert_eq!(
            followers, 1,
            "the follower graph belongs to the AP account: split from it the successor \
             federates to nobody while the remote still believes it follows"
        );
        assert!(
            note,
            "the pushed-note witness must follow its author, or the successor's delete \
             of a legacy post finds no URL and pushes no AP Delete"
        );
        assert_eq!(
            ap_view(&db, &OLD, "alice").await.1,
            0,
            "and nothing is left behind on the retired identity"
        );
    }

    /// Stand up the Bluesky schema and link `actor` to `did`, with cross-posting
    /// ON and one row in each satellite. `did` is the attacker-choosable half:
    /// in production it is whatever account completed the OAuth flow, and
    /// linking is an ordinary User-class gesture.
    #[cfg(feature = "bluesky")]
    async fn seed_bluesky_link(db: &CacheDb, actor: &[u8; 32], did: &str) {
        let conn = db.conn.lock().await;
        // Idempotent on a second call in one test (two actors, one schema).
        crate::bluesky::apply_schema(&conn).unwrap();
        let hex = hex::encode(actor);
        crate::bluesky::db_helpers::upsert_linked_account(&conn, &hex, did, "handle.bsky.social")
            .unwrap();
        crate::bluesky::db_helpers::set_write_through(&conn, &hex, 1).unwrap();
        crate::bluesky::db_helpers::store_interaction(
            &conn,
            &hex,
            "post-1",
            "like",
            &format!("at://{did}/app.bsky.feed.like/1"),
        )
        .unwrap();
        crate::bluesky::db_helpers::save_feed(&conn, &hex, "at://feed/1", "A feed", None, None)
            .unwrap();
    }

    /// Read the link through its own doors: which external account the actor
    /// resolves to, and what the cross-post leg would do for it.
    #[cfg(feature = "bluesky")]
    async fn bluesky_view(db: &CacheDb, actor: &[u8; 32]) -> (Option<String>, i64, usize) {
        let conn = db.conn.lock().await;
        let hex = hex::encode(actor);
        let linked = crate::bluesky::db_helpers::get_linked_account(&conn, &hex)
            .unwrap()
            .map(|a| a.bluesky_did);
        let write_through = crate::bluesky::db_helpers::get_write_through(&conn, &hex).unwrap();
        let feeds = crate::bluesky::db_helpers::get_saved_feeds(&conn, &hex)
            .unwrap()
            .len();
        (linked, write_through, feeds)
    }

    /// **The ceremony burns the link, and the door that proves it is the one the
    /// cross-post leg reads.** The linked-account row is the actor→authority
    /// edge: a lookup on it resolves straight into an authenticated agent for
    /// whatever DID it names, and the cross-post setting rides the same row. A
    /// thief links their own external account with an ordinary User-class
    /// gesture, so carrying the row forward would publish the SUCCESSOR's every
    /// public post into the repo the thief chose, with no end date.
    ///
    /// The successor's `write_through` reading 0 is the whole property: it is
    /// what the write-through leg consults before it posts anything.
    #[cfg(feature = "bluesky")]
    #[tokio::test]
    async fn a_succession_burns_the_thiefs_bluesky_link() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        seed_bluesky_link(&db, &OLD, "did:plc:thief").await;

        assert_eq!(
            bluesky_view(&db, &OLD).await,
            (Some("did:plc:thief".into()), 1, 1),
            "baseline: the link resolves and cross-posting is on"
        );

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            bluesky_view(&db, &NEW).await,
            (None, 0, 0),
            "the successor must inherit NO external link and NO cross-post setting — \
             a carried-forward link mirrors their posts into a repo the thief chose"
        );
        assert_eq!(
            bluesky_view(&db, &OLD).await,
            (None, 0, 0),
            "and it is burned rather than left behind: the unlink door is caller-scoped, \
             so a row left on the retired identity is unrevocable by everyone"
        );
        let conn = db.conn.lock().await;
        let satellites: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM bluesky_interactions WHERE actor_id = ?1)
                      + (SELECT COUNT(*) FROM bluesky_saved_feeds WHERE actor_id = ?1)",
                rusqlite::params![hex::encode(OLD)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            satellites, 0,
            "the satellites burn with the binding — each names an external account the \
             row's owner no longer has"
        );
    }

    async fn catch_all_of(db: &CacheDb, domain: &str) -> Option<[u8; 32]> {
        db.lookup_active_mail_domain(domain)
            .await
            .unwrap()
            .and_then(|d| d.catch_all_actor_id)
    }

    async fn catch_all_cleared_by_succession_stamp_of(db: &CacheDb, domain: &str) -> Option<i64> {
        db.lookup_active_mail_domain(domain)
            .await
            .unwrap()
            .and_then(|d| d.catch_all_cleared_by_succession_at)
    }

    /// **A catch-all naming the retired identity keeps SEALING to a burned
    /// key, so it is cleared — it is not the clean black-hole it was first
    /// ruled to be.** The first ruling said *stay*, reasoning that the
    /// catch-all is admin-plane routing and that its cost was a loss (mail
    /// nobody can read) rather than a leak. That conflated retrieval with
    /// sealing, and only retrieval is closed by the alias re-point:
    ///
    /// 1. `catch_all_actor_id` feeds `resolve_recipient` as the fallback, and
    ///    succession does not move it — unmatched mail resolves to the
    ///    **retired** actor.
    /// 2. The retired actor's `actor_mls_pubkeys` row deliberately survives
    ///    (it is what arms the `succession_pending` tempfail for the account's
    ///    *own* address, whose alias did move).
    /// 3. So `get_recipient_mail_seal_key(retired)` returns `Some(k)`, and that
    ///    branch reports `succession_pending = false` unconditionally — the
    ///    tempfail never fires for this path, because it only fires on `None`.
    ///
    /// Every future catch-all delivery is therefore freshly sealed to a key
    /// derived from the MSEK the thief read, with **no end date**, which is
    /// strictly worse than the residuals the ceremony accepts elsewhere: for
    /// the actor's own address, sealing *stops* at the ceremony, so a thief's
    /// surviving session decays. Here it never stops.
    ///
    /// **Clearing beats following.** Following would let one user's private
    /// ceremony silently re-aim a deployment-wide routing decision an admin
    /// made — and on a multi-admin nest, re-aim it at an actor who does not own
    /// the domain. Clearing removes a designation that has become unsafe
    /// without inventing a new target, and it makes the failure **loud**: with
    /// no catch-all, unmatched mail is rejected where the admin can see it,
    /// instead of vanishing into an identity nobody can read. Nothing
    /// irrecoverable dies — a catch-all designation is admin-recreatable in one
    /// gesture.
    #[tokio::test]
    async fn a_succession_clears_a_catch_all_that_named_the_retired_identity() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.add_mail_domain("fauna.test", false, "testing", "none", Some(&OLD), None)
            .await
            .unwrap();
        // A second domain whose catch-all names someone else entirely — it must
        // NOT be touched, or the clear is an over-broad sweep of admin config.
        let other = [0x77u8; 32];
        db.add_mail_domain("other.test", false, "testing", "none", Some(&other), None)
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            catch_all_of(&db, "fauna.test").await,
            None,
            "a catch-all naming the retired identity must be cleared — leaving it \
             keeps sealing every future unmatched delivery to a key the thief can \
             derive, forever"
        );
        assert_eq!(
            catch_all_of(&db, "other.test").await,
            Some(other),
            "an unrelated domain's catch-all is another admin's routing decision"
        );
        assert!(
            catch_all_cleared_by_succession_stamp_of(&db, "fauna.test")
                .await
                .is_some(),
            "the row must record that a SUCCESSION cleared this catch-all \
             — an admin-visible surface needs to tell that apart from a catch-all \
             that was simply never set"
        );
        assert_eq!(
            catch_all_cleared_by_succession_stamp_of(&db, "other.test").await,
            None,
            "an untouched domain's catch-all was never cleared by anything"
        );
    }

    /// Seed a real supervised link through the **admission** path, which is the
    /// only writer of `guardianships` (`insert_guardianship_tx`'s single caller).
    async fn seed_supervised(db: &CacheDb, ward: &[u8; 32], guardian: &[u8; 32], handle: &str) {
        db.create_user_with_handle(ward, "free", handle, Some(guardian.as_slice()))
            .await
            .unwrap();
    }

    /// **A ward's own succession must not emancipate them, and before this leg
    /// it did — permanently.**
    ///
    /// `family-safety.md` § Lifecycle gates refuses a supervised account's own
    /// `fauna.account.delete` precisely because it "would unilaterally sever the
    /// link". The recovery ceremony is the same gesture with no gate on it: the
    /// kind is **pre-identity**, authorized by the recovery chain alone, so a
    /// ward holding their own kit reached it with no tier or supervision check.
    /// With `guardianships` un-ruled the link stayed on the retired actor, and
    /// every enforcement gate is a `WHERE EXISTS (… supervised_actor_id = ?1)`,
    /// so the successor was simply not supervised.
    ///
    /// And it could never be undone: `insert_guardianship_tx`'s only caller is
    /// admission, and § The guardianship link rules out converting an existing
    /// full account into a supervised one.
    #[tokio::test]
    async fn a_succession_carries_supervision_across_with_the_ward() {
        let db = CacheDb::open_in_memory().unwrap();
        let guardian = [0x61u8; 32];
        seed_account(&db, &guardian, "parent").await;
        seed_supervised(&db, &OLD, &guardian, "kid").await;

        // A tightened control, so the assertion below is about the guardian's
        // actual decision rather than about a row that happens to exist: every
        // `guardian_policies` default is the unsupervised-equivalent.
        db.update_guardian_policy(
            &OLD, true, "hold", false, "block", None, None, None, None, None,
        )
        .await
        .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let link = db.get_guardian_of(&NEW).await.unwrap();
        assert!(
            link.is_some(),
            "the successor must still be supervised — the link row IS the supervised \
             designation, so leaving it behind lets a ward emancipate themselves by \
             running the recovery ceremony, which is the exact gesture \
             fauna.account.delete refuses them"
        );
        assert_eq!(
            link.unwrap().guardian_actor_id,
            guardian.to_vec(),
            "and it must still name the same guardian"
        );
        assert!(
            db.get_guardian_of(&OLD).await.unwrap().is_none(),
            "the retired identity keeps no link — a ward supervised twice over is \
             not a state any reader expects"
        );

        let policy = db
            .get_guardian_policy(&NEW)
            .await
            .unwrap()
            .expect("the policy document must move with the link");
        assert_eq!(
            (policy.contact_approval, policy.unknown_sender_mail.as_str()),
            (true, "hold"),
            "the guardian's tightening must survive the ward's ceremony: every \
             default here is the unsupervised-equivalent, so a link that moved \
             without its policy would leave the ward nominally supervised with \
             enforcement silently off"
        );
    }

    /// **A guardian's own succession must carry their wards, or § Lifecycle
    /// gates' "a stranded ward is unrepresentable" stops being true.**
    ///
    /// That invariant is enforced by refusing evict/delete while links exist.
    /// Succession is neither: it retires the guardian's identity — refused
    /// everywhere — while `guardian_actor_id` goes on naming it, so the ward
    /// stays supervised under a guardian who can never act again and the
    /// successor's own app lists no wards at all.
    #[tokio::test]
    async fn a_succession_carries_the_guardians_wards_across() {
        let db = CacheDb::open_in_memory().unwrap();
        let ward = [0x62u8; 32];
        let other_guardian = [0x63u8; 32];
        let other_ward = [0x64u8; 32];
        seed_account(&db, &OLD, "parent").await;
        seed_account(&db, &other_guardian, "neighbour").await;
        seed_supervised(&db, &ward, &OLD, "kid").await;
        // Another family entirely — it must not be swept up, or the leg is
        // re-aiming someone else's guardianship.
        seed_supervised(&db, &other_ward, &other_guardian, "kid2").await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let wards = db.list_wards(&NEW).await.unwrap();
        assert_eq!(
            wards.len(),
            1,
            "the successor guardian must still see their ward — list_wards selects \
             WHERE guardian_actor_id = ?1, so an un-moved column leaves the ward \
             supervised by an identity refused everywhere"
        );
        assert_eq!(wards[0].supervised_actor_id, ward.to_vec());
        assert!(
            db.list_wards(&OLD).await.unwrap().is_empty(),
            "and the retired identity guards nobody"
        );
        assert_eq!(
            db.get_guardian_of(&ward)
                .await
                .unwrap()
                .unwrap()
                .guardian_actor_id,
            NEW.to_vec(),
            "read from the ward's side too: the link the ward's own client renders \
             must name the guardian who can actually answer it"
        );
        assert_eq!(
            db.get_guardian_of(&other_ward)
                .await
                .unwrap()
                .unwrap()
                .guardian_actor_id,
            other_guardian.to_vec(),
            "an unrelated family is another person's guardianship and is untouched"
        );
    }

    /// Seed a real paid relationship: an author with a tier, a reader holding
    /// it with a published ML-KEM ek, and a pending request for a second tier.
    ///
    /// `add_subscriber` is the production writer (`requests.approve` and the
    /// auto-approve cascade both land here), and the ek is a stand-in for the
    /// 1184-byte value `subscriber_mlkem_encaps_key` derives — its *length* is
    /// irrelevant to the leg, only its presence and provenance are.
    async fn seed_paid_reader(db: &CacheDb, author: &[u8; 32], reader: &[u8; 32]) {
        db.create_subscription_tier(author, "gold", 1, None, None, None, true, None, None, false)
            .await
            .unwrap();
        db.create_subscription_tier(
            author, "silver", 2, None, None, None, true, None, None, false,
        )
        .await
        .unwrap();
        db.add_subscriber(author, reader, "gold", Some(&[0x77u8; 8]))
            .await
            .unwrap();
        db.insert_subscribe_request(author, reader, "silver", "subscribe", Some(&[0x77u8; 8]))
            .await
            .unwrap();
    }

    /// The `subscribers` row's stored ek, `None` when the row is gone.
    async fn stored_ek(
        db: &CacheDb,
        author: &[u8; 32],
        reader: &[u8; 32],
    ) -> Option<Option<Vec<u8>>> {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT mlkem_encaps_key FROM subscribers \
             WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = 'gold'",
            rusqlite::params![author.as_slice(), reader.as_slice()],
            |r| r.get::<_, Option<Vec<u8>>>(0),
        )
        .ok()
    }

    /// **A reader who rotates their identity must keep the subscriptions they
    /// paid for — and before this leg they lost every one of them, silently.**
    ///
    /// The author's side of this plane moves as a declared coupled family; the
    /// reader's side is `subscribers.subscriber_id`, which was accounted for by
    /// nothing at all because the completeness walk matched `actor`/`author`/
    /// `owner` and this plane names its second person *subscriber*. The loss is
    /// not cosmetic: `is_subscriber` and `get_subscribed_tiers` gate
    /// `fauna.subscriptions.key_blob.get`, so the successor's paid content
    /// goes dark with no in-product way back.
    ///
    /// The ek assertion is the other half of the ruling: carried across, the
    /// stored ML-KEM key is the *predecessor's* while the X25519 half the author
    /// reconstructs is the *successor's*, so every wrap under the pair is
    /// unopenable by anybody. Cleared, the author wraps classically until the
    /// successor republishes.
    #[tokio::test]
    async fn a_succeeding_reader_keeps_the_subscriptions_they_paid_for() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = [0x71u8; 32];
        let other_reader = [0x72u8; 32];
        seed_account(&db, &author, "creator").await;
        seed_account(&db, &OLD, "reader").await;
        seed_account(&db, &other_reader, "someone-else").await;
        seed_paid_reader(&db, &author, &OLD).await;
        // A control: another reader of the same author must not be swept along.
        db.add_subscriber(&author, &other_reader, "gold", Some(&[0x88u8; 8]))
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert!(
            db.is_subscriber(&author, &NEW, "gold").await.unwrap(),
            "the successor must still hold the tier they paid for — is_subscriber \
             gates the key-material fetches, so a row left behind means the content \
             is dark, not merely unlisted"
        );
        assert!(
            !db.is_subscriber(&author, &OLD, "gold").await.unwrap(),
            "and the retired identity holds nothing: a doubled roster row would keep \
             serving an identity the ceremony just retired"
        );
        assert_eq!(
            db.get_subscribed_tiers(&author, &NEW).await.unwrap(),
            vec!["gold".to_string()],
            "the entitlement gate reads the same column and must agree"
        );

        let mine = db.list_my_subscriptions(&NEW).await.unwrap();
        assert_eq!(
            mine.iter()
                .map(|r| (r.tier_name.as_str(), r.status.as_str()))
                .collect::<Vec<_>>(),
            vec![("gold", "active"), ("silver", "pending")],
            "the reader's own list must carry BOTH halves across — the pending \
             request as much as the approved row, or the author's next drain grants \
             the tier to an identity refused everywhere"
        );
        assert!(
            db.list_my_subscriptions(&OLD).await.unwrap().is_empty(),
            "and nothing is left listed under the retired identity"
        );

        assert_eq!(
            stored_ek(&db, &author, &NEW).await,
            Some(None),
            "the moved row must arrive with its post-quantum ek CLEARED: it derives \
             from the predecessor's seed, while the X25519 half of the same X-Wing \
             public is derived from the successor's ActorId — carried, the author \
             wraps to a hybrid key nobody holds the secret for, which is worse than \
             the classical fallback a NULL selects"
        );
        assert_eq!(
            stored_ek(&db, &author, &other_reader).await,
            Some(Some(vec![0x88u8; 8])),
            "an unrelated reader's published ek is untouched — the clear is scoped \
             to the rows that moved"
        );
    }

    /// **The refund path resolves through `payment_claim_codes.redeemed_by`, so
    /// that column must move with the roster row it points at.**
    ///
    /// `payment_core::apply_refund` is the production caller: for a refund on an
    /// already-redeemed claim `void_payment_claim` returns false, the handler
    /// reads `redeemed_by` as the buyer, and voids *that* actor's window with
    /// `set_subscriber_valid_until`. This test drives exactly that two-step join
    /// at the db level (the handler itself needs an `AppState` this module has
    /// no business standing up).
    ///
    /// Ruling this column `Stay` — which is what "rows are kept for audit"
    /// suggests, and what the row that asked for the ruling predicted — would
    /// leave the void aimed at the retired identity, matching nothing: money
    /// refunded, content still served. There is no second handle to fall back
    /// on, because a claim-code purchase is `Buyer::Unbound` by construction.
    #[tokio::test]
    async fn a_refund_after_the_readers_succession_still_takes_the_entitlement_back() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = [0x73u8; 32];
        seed_account(&db, &author, "creator").await;
        seed_account(&db, &OLD, "reader").await;
        seed_paid_reader(&db, &author, &OLD).await;
        db.insert_payment_claim("CODE-1", &author, "gold", "stripe", "evt_1", None)
            .await
            .unwrap();
        assert!(db.redeem_payment_claim("CODE-1", &OLD).await.unwrap());

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // Step 1 of `apply_refund`: the claim cannot be voided (already
        // redeemed), so the redeemer IS the buyer whose window must go.
        assert!(!db.void_payment_claim("CODE-1").await.unwrap());
        let claim = db.get_payment_claim("CODE-1").await.unwrap().unwrap();
        let buyer: [u8; 32] = claim
            .redeemed_by
            .as_deref()
            .expect("a redeemed claim names its redeemer")
            .try_into()
            .unwrap();
        assert_eq!(
            buyer, NEW,
            "the redeemer pointer must name the identity that now holds the \
             entitlement it bought — left on the retired identity it names nobody \
             the roster still knows"
        );

        // Step 2: void that buyer's window, and the entitlement is gone.
        assert!(
            db.set_subscriber_valid_until(&author, &buyer, "gold", Some(now_epoch_secs()))
                .await
                .unwrap(),
            "the void must land on a real row — this returning false is exactly how \
             a split ruling would fail in production, silently"
        );
        assert!(
            !db.is_subscriber(&author, &NEW, "gold").await.unwrap(),
            "and the refunded reader is un-entitled at the gates"
        );
    }

    // ── The reference columns the vocabulary widening surfaced ───────────────
    //
    // A reference ruling gets **no** data-driven witness: the registry sweep
    // iterates `ACTOR_TABLES`, so each of these owes a hand-written pin or it
    // is asserted by nothing. Two stay (a third, `admin_actor_ids.added_by`,
    // left with schema 100's dead columns; a fourth, `group_members.invited_by`,
    // retired with the group plane), one moves, one burns.

    /// The two `Stay`s, pinned together because the property is one property —
    /// *the ceremony leaves an attribution pointer exactly where it was* — and a
    /// per-column test would repeat the same lines twice. The
    /// assertion messages name the column, so a failure still says which ruling
    /// broke.
    ///
    /// What makes this a pin rather than a tautology: each of these columns sits
    /// beside an `actor_id` on the same table that a later pass may well rule
    /// `Move`, and the plain loop moves *one* declared column per table. A leg
    /// written for the neighbour that swept the whole table would red here.
    #[tokio::test]
    async fn the_attribution_pointers_stay_with_the_retired_identity() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let member = [0x92u8; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO invite_requests (actor_id, handle, status, created_at, decided_at, decided_by)
                 VALUES (?1, 'bob', 'denied', 1, 2, ?2)",
                rusqlite::params![member.as_slice(), OLD.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO pending_actions
                     (action_type, actor_id, status, created_at, execute_after, cancelled_by, cancelled_at)
                 VALUES ('HandleChange', ?1, 'cancelled', 1, 2, ?2, 3)",
                rusqlite::params![member.as_slice(), OLD.as_slice()],
            )
            .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        for (table, column) in [
            ("invite_requests", "decided_by"),
            ("pending_actions", "cancelled_by"),
        ] {
            let on_old: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                    rusqlite::params![OLD.as_slice()],
                    |r| r.get(0),
                )
                .unwrap();
            let on_new: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                    rusqlite::params![NEW.as_slice()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                (on_old, on_new),
                (1, 0),
                "{table}.{column} is ruled Stay: it records what the retired \
                 identity DID, nothing resolves through it, and re-pointing it \
                 would re-attribute an act a seed thief may have performed to \
                 the identity recovering from them"
            );
        }
    }

    /// **The curation pass's product-observable pin: a successor OWNS the
    /// feeds and curation settings the retired identity created.**
    ///
    /// Written the way the agreement-only lesson demands — on what a user can see, and for
    /// the direction NOT chosen. Both data-driven sweeps assert only that the
    /// executor agrees with the declaration, so flipping any of these three
    /// tables back to `Stay`/`Unruled` leaves them green; what reds is this. The
    /// three observables are the three doors a stranded row closes:
    ///
    /// - `delete_feed` carries `AND owner = ?`, so a feed left behind can be
    ///   removed by **nobody** — the successor is refused for owner mismatch and
    ///   the retired key is refused everywhere — while `fauna.feed.list` keeps
    ///   showing it to the whole nest. The `web_domains` stranding on a
    ///   published object (`nest/common.md` § Client-state recoverability).
    /// - `get_global_factors` is owner-scoped, and its entries fold into every
    ///   one of the successor's feeds' orderings.
    /// - a third party's feed must not be swept up by a leg keyed on `old`.
    #[tokio::test]
    async fn a_succession_carries_the_retired_identitys_feeds_and_curation() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let other = [0x95u8; 32];

        let mine_id = db
            .create_feed(&OLD, "mine", b"[]", "all", "local", "[]", None)
            .await
            .unwrap();
        db.create_feed(&other, "theirs", b"[]", "all", "local", "[]", None)
            .await
            .unwrap();
        db.set_global_factors(
            &OLD,
            &[fauna_core::scoring::CompositionEntry {
                factor: "cats".into(),
                weight_permille: 700,
            }],
        )
        .await
        .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The successor's own list answers — the app's "my feeds" surface.
        let mine = db.list_feeds_by_owner(&NEW).await.unwrap();
        assert_eq!(
            mine.iter().map(|f| f.feed_id.as_str()).collect::<Vec<_>>(),
            vec![mine_id.as_str()],
            "the successor's feed list must answer with the feed they now own — \
             left behind, `fauna.feed.list` shows it to the whole nest under a \
             retired identity while the successor's own list is empty"
        );

        // The global factor set the composition folds in.
        let factors = db.get_global_factors(&NEW).await.unwrap();
        assert_eq!(
            factors
                .iter()
                .map(|e| (e.factor.as_str(), e.weight_permille))
                .collect::<Vec<_>>(),
            vec![("cats", 700)],
            "the successor's feeds must compose from the same global factors — \
             split from `feeds.composition`, which moves, the ordering changes \
             and the app renders the set as empty rather than disagreeing"
        );

        // A third party's feed is untouched by a leg keyed on `old`.
        assert_eq!(
            db.list_feeds_by_owner(&other).await.unwrap().len(),
            1,
            "a leg keyed on the retired identity must not move another owner's feed"
        );

        // ⚠ The direction NOT chosen, asserted directly: with `feeds` left
        // behind this delete is refused and no other caller can ever issue it.
        assert!(
            db.delete_feed(&mine_id, &NEW).await.unwrap(),
            "the successor must be able to delete the feed they now own — this \
             is the assertion a `Stay` ruling fails, and the reason the stranding \
             is a recoverability breach rather than a preference loss"
        );
    }

    /// **A retired subscriber must not pin a labeler's nest-wide scoring on
    /// forever.**
    ///
    /// `unsubscribe_labeler_core` withdraws a List labeler's materialized
    /// `content_scores` rows only when `count_subscriptions_for_labeler` reaches
    /// zero, and `delete_subscription` is `(owner_actor, labeler_id)`-scoped. So
    /// a subscription left on an identity refused everywhere holds that count
    /// above zero with no surface anywhere to clear it — the presence-denies
    /// shape of `share_tokens`, inverted: here the row's presence keeps an
    /// effect **alive**. The observable is that the successor's unsubscribe can
    /// still take the count to zero.
    #[tokio::test]
    async fn a_succession_carries_the_retired_identitys_labeler_subscriptions() {
        use crate::db::labelers::PutLabeler;

        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let labeler_id = [0x77u8; 32];
        db.put_labeler(PutLabeler {
            labeler_id: &labeler_id,
            version: 1,
            // The artifact's own signing key — `publish_labeler_core` writes
            // `publisher_actor = labeler_id`, never an account id.
            publisher_actor: &labeler_id,
            caller_actor: &OLD,
            content_kind: "post",
            factor: "labeler:x",
            wasm_hash: &[0u8; 32],
            wasm_size: 1,
            metadata_blob: &[0u8],
            wasm_bytes: &[0u8],
            artifact_kind: "list",
            artifact_version: 0,
            list_entries: &[],
        })
        .await
        .unwrap();
        db.put_subscription(&OLD, &labeler_id, None, 1)
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert!(
            db.list_subscribed_labeler_ids(&NEW)
                .await
                .unwrap()
                .contains(labeler_id.as_slice()),
            "the successor's subscribed set must carry the subscription — the \
             delivery-time `seed_new_mail_labeler_obligations` reads exactly \
             this, so left behind the successor's arrivals seed no re-score work \
             at all"
        );
        assert!(
            db.delete_subscription(&NEW, &labeler_id).await.unwrap(),
            "the successor must be able to unsubscribe — left behind, the row is \
             unreachable from every app and `count_subscriptions_for_labeler` \
             never reaches zero, so the labeler's materialized scores can never \
             be withdrawn for anyone"
        );
        assert_eq!(
            db.count_subscriptions_for_labeler(&labeler_id)
                .await
                .unwrap(),
            0,
            "and the withdraw gate must actually be reachable afterwards"
        );
    }

    /// **The publication quota the security review moved onto the un-rotatable
    /// identity must not be reset by rotating that identity.**
    ///
    /// `MAX_LABELERS_PER_CALLER` exists because `MAX_LABELERS_PER_PUBLISHER`
    /// bounds nothing — the signing keypair is free and off-box-rotatable.
    /// A succession is self-service, so a left-behind
    /// `caller_actor` hands the successor a fresh full allowance while the old
    /// rows stay in a catalog with **no delete verb**. The observable is the
    /// refusal: at the cap before the ceremony, still at the cap after it.
    ///
    /// The same test pins the ruling's other half — `publisher_actor` **stays**,
    /// because it names the key the stored bytes were signed with.
    #[tokio::test]
    async fn a_succession_carries_the_labeler_publication_quota() {
        use crate::db::labelers::{
            LabelerCallerQuotaExceeded, MAX_LABELERS_PER_CALLER, PutLabeler,
        };

        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;

        // Fill the retired identity's allowance. Each row carries its own
        // signing key, which is exactly the evasion the per-caller cap answers.
        {
            let conn = db.conn.lock().await;
            for i in 0..MAX_LABELERS_PER_CALLER {
                let mut id = [0u8; 32];
                id[0] = (i % 251) as u8;
                id[1] = (i / 251) as u8;
                conn.execute(
                    "INSERT INTO labelers
                        (labeler_id, version, publisher_actor, caller_actor, content_kind,
                         factor, wasm_hash, wasm_size, metadata_blob, wasm_bytes,
                         updated_at, artifact_kind)
                     VALUES (?1, 1, ?1, ?2, 'post', 'labeler:x', x'00', 1, x'00', x'00', 1, 'list')",
                    rusqlite::params![id.as_slice(), OLD.as_slice()],
                )
                .unwrap();
            }
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // ⚠ The direction NOT chosen: leave `caller_actor` behind and this
        // publish SUCCEEDS, because the successor's count reads zero.
        let fresh = [0xEEu8; 32];
        let err = db
            .put_labeler(PutLabeler {
                labeler_id: &fresh,
                version: 1,
                publisher_actor: &fresh,
                caller_actor: &NEW,
                content_kind: "post",
                factor: "labeler:y",
                wasm_hash: &[0u8; 32],
                wasm_size: 1,
                metadata_blob: &[0u8],
                wasm_bytes: &[0u8],
                artifact_kind: "list",
                artifact_version: 0,
                list_entries: &[],
            })
            .await
            .expect_err(
                "the successor must still be at the cap — a self-service ceremony \
                 must not reset a DoS bound whose whole point was keying on the \
                 un-rotatable identity",
            );
        assert!(
            err.downcast_ref::<LabelerCallerQuotaExceeded>().is_some(),
            "and it must be the per-CALLER cap that refuses, not the per-publisher \
             one (each row above carries its own signing key): {err}"
        );

        // The other half of the ruling: the signing identity stays put.
        let conn = db.conn.lock().await;
        let on_old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM labelers WHERE publisher_actor = ?1",
                rusqlite::params![[0u8; 32].as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            on_old, 1,
            "`publisher_actor` names the key the artifact was SIGNED with, so it \
             must not be re-pointed — a moved one would advertise the successor \
             as the signer of bytes they never signed"
        );
    }

    #[tokio::test]
    async fn a_handle_less_account_succeeds_without_inventing_one() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "").await;

        let applied = db
            .record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(applied.handle, None);
        assert_eq!(handle_of(&db, &NEW).await.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn an_identity_is_succeeded_at_most_once() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.record_succession(&OLD, &NEW, b"first", 1)
            .await
            .unwrap()
            .unwrap();

        // A second statement for the same old identity — even at a higher seq,
        // even naming a different successor — is refused. Without this the old
        // RecoveryKey (which stays verifiable forever) could re-point the
        // account away from the successor at any later time.
        let refusal = db
            .record_succession(&OLD, &THIRD, b"second", 9)
            .await
            .unwrap()
            .expect_err("a second succession is refused");
        assert_eq!(refusal, SuccessionRefusal::AlreadySucceeded);

        // Nothing moved.
        assert_eq!(handle_of(&db, &NEW).await.as_deref(), Some("alice"));
        assert!(handle_of(&db, &THIRD).await.is_none());
    }

    #[tokio::test]
    async fn an_unhomed_identity_and_an_occupied_successor_are_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        // No account for OLD at all.
        assert_eq!(
            db.record_succession(&OLD, &NEW, b"s", 1)
                .await
                .unwrap()
                .unwrap_err(),
            SuccessionRefusal::OldNotRegistered
        );

        // Successor already has an account — re-pointing onto it would merge
        // two users into one.
        seed_account(&db, &OLD, "alice").await;
        seed_account(&db, &NEW, "bob").await;
        assert_eq!(
            db.record_succession(&OLD, &NEW, b"s", 1)
                .await
                .unwrap()
                .unwrap_err(),
            SuccessionRefusal::NewAlreadyRegistered
        );
        // Both accounts are untouched.
        assert_eq!(handle_of(&db, &OLD).await.as_deref(), Some("alice"));
        assert_eq!(handle_of(&db, &NEW).await.as_deref(), Some("bob"));
        assert!(db.succession_for(&OLD).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn malformed_input_is_refused_before_any_write() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;

        assert!(
            db.record_succession(&OLD[..31], &NEW, b"s", 1)
                .await
                .is_err()
        );
        assert!(
            db.record_succession(&OLD, &NEW[..31], b"s", 1)
                .await
                .is_err()
        );
        assert!(db.record_succession(&OLD, &OLD, b"s", 1).await.is_err());
        assert!(db.record_succession(&OLD, &NEW, b"", 1).await.is_err());
        assert!(db.succession_for(&OLD).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_lockout_does_not_ride_to_the_successor_but_the_suspension_does() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE users SET locked_until = 9999999999, suspended = 1 WHERE actor_id = ?1",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        let (locked, suspended): (Option<i64>, i64) = conn
            .query_row(
                "SELECT locked_until, suspended FROM users WHERE actor_id = ?1",
                rusqlite::params![NEW.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        // The thief's emergency lockout must not survive the ceremony that
        // undoes them — otherwise the recovered account stays frozen.
        assert_eq!(locked, None);
        // An admin's suspension is about the account, not the key: rotating
        // keys is not an appeal.
        assert_eq!(suspended, 1);
    }

    #[tokio::test]
    async fn admin_role_moves_and_granted_authority_is_revoked() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO admin_actor_ids (actor_id, added_at) VALUES (?1, 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO capability_grants
                    (owner_actor_id, grant_id, holder_pubkey, blob, epoch_end, created_at)
                 VALUES (?1, x'01', x'02', x'03', 1, 1), (?1, x'04', x'05', x'06', 1, 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO nest_backup_keys (owner_actor_id, backup_key, granted_at)
                 VALUES (?1, x'07', 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO recovery_pending_replacements
                    (actor_id, record, record_digest, new_recovery_pubkey, seq, requested_at)
                 VALUES (?1, x'08', x'09', x'0a', 2, 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO recovery_escrow (actor_id, blob, updated_at)
                 VALUES (?1, x'0b', 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
        }

        let applied = db
            .record_succession(&OLD, &NEW, b"s", 3)
            .await
            .unwrap()
            .unwrap();
        assert!(applied.was_admin);
        assert_eq!(applied.capability_grants_revoked, 2);
        assert!(applied.cancelled_pending_replacement);

        let conn = db.conn.lock().await;
        let admin_new: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![NEW.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        let admin_old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![OLD.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!((admin_new, admin_old), (1, 0));

        // Granted authority is revoked outright, never migrated: every one of
        // these is sealed to (or names) a key the successor does not hold.
        for (table, col) in [
            ("capability_grants", "owner_actor_id"),
            ("nest_backup_keys", "owner_actor_id"),
            ("recovery_pending_replacements", "actor_id"),
            ("recovery_escrow", "actor_id"),
        ] {
            let n: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {col} = ?1"),
                    rusqlite::params![OLD.as_slice()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "{table} should be empty for the old identity");
            let n_new: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {col} = ?1"),
                    rusqlite::params![NEW.as_slice()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n_new, 0, "{table} must not be migrated to the successor");
        }
    }

    /// An admin who has approved a bridge registration must still be able to
    /// succeed their identity.
    ///
    /// `bridge_service_users.approved_by_actor_id` is a **foreign key** to
    /// `admin_actor_ids(actor_id)` (`migrations.rs`), and the admin leg above is
    /// a delete-then-insert — so the `DELETE` runs while a child row still names
    /// the retired admin. With `PRAGMA foreign_keys = ON` (`db/mod.rs`) that is
    /// not a no-op and not a stranded row: it **aborts the whole succession
    /// transaction**, so the one ceremony that ends a key compromise is denied
    /// to exactly the admin most likely to have approved a bridge.
    ///
    /// The column is correctly *excluded* from the deletion registry (it names
    /// the approving admin, not an owner), which is why no per-table ruling ever
    /// looked at it — but the succession axis asks a different question of a
    /// third-party reference, and here the answer is load-bearing.
    ///
    /// The ruling is `Stay` (`actor_tables::SUCCESSION_REFERENCES`): the retired
    /// identity genuinely did approve that bridge, nothing resolves through the
    /// pointer, and `actor_successions` links it forward — so the value is left
    /// exactly as written and only the FK goes.
    #[tokio::test]
    async fn an_admin_who_approved_a_bridge_can_still_succeed() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO admin_actor_ids (actor_id, added_at) VALUES (?1, 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
            // Exactly what `fauna.bridges.approve_pending_bridge` writes:
            // `approve_bridge_service_user(&pk, Some(&actor_id))`.
            conn.execute(
                "INSERT INTO bridge_service_users
                    (ed25519_pubkey, role, bridge_id, status, created_at, approved_at,
                     approved_by_actor_id)
                 VALUES (x'11', 'mta', 'b1', 'approved', 1, 1, ?1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
        }

        let applied = db
            .record_succession(&OLD, &NEW, b"s", 3)
            .await
            .expect("the ceremony must not fail for an admin who approved a bridge")
            .unwrap();
        assert!(applied.was_admin);

        let conn = db.conn.lock().await;
        // The approval survives with its provenance intact. It STAYS on the
        // retired identity: attribution of an act the old key genuinely
        // performed, which re-pointing would silently re-attribute to a
        // successor who may be recovering from a thief that made it.
        let approver: Vec<u8> = conn
            .query_row(
                "SELECT approved_by_actor_id FROM bridge_service_users WHERE ed25519_pubkey = x'11'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            approver,
            OLD.to_vec(),
            "the approval's provenance is history and stays on the retired identity"
        );
        // And the admin role itself still moved — the two are independent.
        let admin_new: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM admin_actor_ids WHERE actor_id = ?1",
                rusqlite::params![NEW.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(admin_new, 1);
    }

    /// **The floor roster's authorship pointer stays on the retired identity**
    /// (`actor_tables::SUCCESSION_REFERENCES`,
    /// `channel_commit_watermark.last_commit_sender`; ruled 2026-09-11).
    ///
    /// The ruling is `Stay` for a reason none of its siblings share: the
    /// pointer IS resolved through — the roster-report door refuses a report
    /// at the newest commit's position from anyone but the stored sender — and
    /// it stays because that resolution fails *closed*: equality with a retired
    /// identity matches nobody the nest still admits, so the position is
    /// claimable by no one until the next commit rewrites the pair. `Clear`
    /// would reopen it to every live member (the guard's fail-open residue for
    /// an unknown sender), `Move` would hand the successor a position it did
    /// not commit. A `Stay` has no leg by construction, so this test is the
    /// only thing that reds if a later session adds one: it holds the
    /// ceremony to the untouched (seq, sender) pair.
    #[tokio::test]
    async fn the_newest_commits_sender_stays_on_the_retired_identity() {
        const CHANNEL: [u8; 32] = [0xC3; 32];

        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.set_channel_commit_watermark(&CHANNEL, 7, Some(&OLD))
            .await
            .unwrap();
        db.record_succession(&OLD, &NEW, b"s", 3)
            .await
            .expect("the ceremony must not fail over a commit mark")
            .unwrap();
        assert_eq!(
            db.channel_commit_watermark_with_sender(&CHANNEL)
                .await
                .unwrap(),
            (7, Some(OLD)),
            "the ceremony leaves the newest commit's observed sender on the \
             retired identity: neither cleared (which would reopen the position \
             to every live member) nor moved (the successor did not send it)"
        );
    }

    /// The same FK broke the *ordinary* admin gesture too: de-admining someone
    /// who had approved a bridge failed outright. Pinned beside the succession
    /// case because they share one cause and a future rebuild could restore it.
    #[tokio::test]
    async fn de_admining_an_approver_does_not_abort() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO admin_actor_ids (actor_id, added_at) VALUES (?1, 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
            // A peer superadmin keeps the de-admin floor-legal — the roster
            // floor (admin.md § 2) would otherwise refuse the removal
            // outright; this test's subject is the FK, not the floor.
            conn.execute(
                "INSERT INTO admin_actor_ids (actor_id, added_at) VALUES (?1, 1)",
                rusqlite::params![NEW.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO bridge_service_users
                    (ed25519_pubkey, role, bridge_id, status, created_at, approved_at,
                     approved_by_actor_id)
                 VALUES (x'12', 'mda', 'b2', 'approved', 1, 1, ?1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
        }

        let removed = db
            .remove_admin_actor(OLD.as_slice())
            .await
            .expect("de-admin must not fail for an admin who approved a bridge");
        assert_eq!(removed, crate::db::admin::RosterWrite::Applied);
    }

    #[tokio::test]
    async fn a_non_admin_succession_does_not_mint_an_admin() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let applied = db
            .record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        assert!(!applied.was_admin);

        let conn = db.conn.lock().await;
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM admin_actor_ids", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn the_path_walks_every_hop_oldest_first() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.record_succession(&OLD, &NEW, b"first", 1)
            .await
            .unwrap()
            .unwrap();
        db.record_succession(&NEW, &THIRD, b"second", 2)
            .await
            .unwrap()
            .unwrap();

        // A peer that only ever saw OLD gets both hops in one call.
        let path = db.succession_path(&OLD).await.unwrap();
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].statement, b"first".to_vec());
        assert_eq!(path[1].statement, b"second".to_vec());
        assert_eq!(path[1].new_actor_id, THIRD.to_vec());

        // A peer already at NEW gets only the remaining hop.
        let path = db.succession_path(&NEW).await.unwrap();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].statement, b"second".to_vec());

        // The terminal identity has nothing to report.
        assert!(db.succession_path(&THIRD).await.unwrap().is_empty());

        // The handle chased the whole chain.
        assert_eq!(handle_of(&db, &THIRD).await.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn the_path_walk_is_bounded() {
        let db = CacheDb::open_in_memory().unwrap();
        // Build a longer chain than the bound, then check the reply truncates
        // instead of running to the end. Written as a straight loop of real
        // successions so the bound is exercised through the production path.
        let actors: Vec<[u8; 32]> = (0..(MAX_SUCCESSION_PATH as u8 + 3))
            .map(|i| [i.wrapping_add(1); 32])
            .collect();
        seed_account(&db, &actors[0], "alice").await;
        for pair in actors.windows(2) {
            db.record_succession(&pair[0], &pair[1], b"s", 1)
                .await
                .unwrap()
                .unwrap();
        }

        let path = db.succession_path(&actors[0]).await.unwrap();
        assert_eq!(path.len(), MAX_SUCCESSION_PATH);
    }

    /// The backward walk a roster row's carriage is filled from: oldest first,
    /// ending at the member, and cut at its OLD end when the chain is longer
    /// than a reader will verify — the links kept are the ones nearest the
    /// member.
    #[tokio::test]
    async fn the_walk_into_an_identity_is_oldest_first_and_keeps_the_nearest_links() {
        const CAP: usize = fauna_core::recovery::MAX_VERIFIED_CHAIN_LEN;
        let db = CacheDb::open_in_memory().unwrap();
        let actors: Vec<[u8; 32]> = (0..(CAP as u8 + 4))
            .map(|i| [i.wrapping_add(1); 32])
            .collect();
        seed_account(&db, &actors[0], "alice").await;
        for (i, pair) in actors.windows(2).enumerate() {
            db.record_succession(&pair[0], &pair[1], &[i as u8], 1)
                .await
                .unwrap()
                .unwrap();
        }

        // Two hops in: both links, oldest first.
        assert_eq!(
            db.succession_statements_into(&actors[2]).await.unwrap(),
            vec![vec![0u8], vec![1u8]]
        );
        // Nobody succeeded into the first identity.
        assert!(
            db.succession_statements_into(&actors[0])
                .await
                .unwrap()
                .is_empty()
        );
        // The whole chain is CAP + 3 links: the CAP nearest the last identity
        // survive, still oldest first and still ending at it.
        let last = actors.len() - 1;
        let chain = db.succession_statements_into(&actors[last]).await.unwrap();
        let nearest: Vec<Vec<u8>> = ((last - CAP)..last).map(|i| vec![i as u8]).collect();
        assert_eq!(chain, nearest);
    }

    /// A nest holds one predecessor per identity (ruling (8)(j)(1)): a second
    /// peer-learned link into one successor is refused and the first-landed
    /// link stands, so the walk carries one linear chain.
    #[tokio::test]
    async fn a_second_link_into_an_identity_is_refused_and_the_first_stands() {
        let db = CacheDb::open_in_memory().unwrap();
        let (x, y, p, q) = ([0x51u8; 32], [0x52u8; 32], [0x53u8; 32], [0x54u8; 32]);
        assert!(
            db.record_peer_succession(&x, &p, b"x-p", 1)
                .await
                .unwrap()
                .is_ok()
        );
        assert_eq!(
            db.record_peer_succession(&y, &p, b"y-p", 1)
                .await
                .unwrap()
                .unwrap_err(),
            SuccessionRefusal::NewAlreadySucceeded
        );
        assert_eq!(
            db.succession_statements_into(&p).await.unwrap(),
            vec![b"x-p".to_vec()]
        );

        assert!(
            db.record_peer_succession(&p, &q, b"p-q", 1)
                .await
                .unwrap()
                .is_ok()
        );
        assert_eq!(
            db.succession_statements_into(&q).await.unwrap(),
            vec![b"x-p".to_vec(), b"p-q".to_vec()]
        );
    }

    // ---- The peer half (slice 4 propagation) ----

    const PEER_NEST: [u8; 32] = [0xD4; 32];

    async fn foreign_member(db: &CacheDb, channel: &[u8; 32], actor: &[u8; 32]) {
        db.register_foreign_channel_member(
            channel,
            actor,
            &PEER_NEST,
            Some("https://peer.example"),
            RebindPower::Standing,
        )
        .await
        .unwrap();
    }

    async fn contact_edge(db: &CacheDb, owner: &[u8; 32], peer: &[u8; 32], status: &str) {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO contacts (actor_id, peer_id, status, created_at) VALUES (?1, ?2, ?3, 1)",
            rusqlite::params![owner.as_slice(), peer.as_slice(), status],
        )
        .unwrap();
    }

    async fn contact_status(db: &CacheDb, owner: &[u8; 32], peer: &[u8; 32]) -> Option<String> {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT status FROM contacts WHERE actor_id = ?1 AND peer_id = ?2",
            rusqlite::params![owner.as_slice(), peer.as_slice()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
    }

    async fn members_of(db: &CacheDb, channel: &[u8; 32]) -> Vec<[u8; 32]> {
        db.list_foreign_channel_members(channel).await.unwrap()
    }

    #[tokio::test]
    async fn a_peer_succession_records_the_link_and_re_points_residue() {
        let db = CacheDb::open_in_memory().unwrap();
        let channel = [0x10; 32];
        let local = [0x20; 32];
        seed_account(&db, &local, "bob").await;
        foreign_member(&db, &channel, &OLD).await;
        contact_edge(&db, &local, &OLD, "accepted").await;

        let applied = db
            .record_peer_succession(&OLD, &NEW, b"stmt", 3)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(applied.foreign_memberships, 1);
        assert_eq!(applied.contact_edges, 1);

        // The link is the enforcement row: this is precisely what makes the
        // inbox consult refuse the superseded key's next relayed post.
        let row = db.succession_for(&OLD).await.unwrap().expect("link stored");
        assert_eq!(row.new_actor_id, NEW.to_vec());
        assert_eq!(row.seq, 3);
        assert_eq!(row.statement, b"stmt".to_vec());

        // Residue names the successor, and the contact keeps its trust flag
        // rather than being re-created as a stranger.
        assert_eq!(members_of(&db, &channel).await, vec![NEW]);
        assert_eq!(
            contact_status(&db, &local, &NEW).await.as_deref(),
            Some("accepted")
        );
        assert!(contact_status(&db, &local, &OLD).await.is_none());

        // A peer serves what it learned, so propagation is transitive.
        assert_eq!(db.succession_path(&OLD).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_peer_succession_collision_keeps_the_stricter_contact_status() {
        // Where a holder has edges to BOTH identities, the collapse keeps the
        // higher-trust-impact status: blocked absorbs everything, else
        // confirmed > accepted > pending (`succession-aftermath.md`
        // § Propagation → Contacts). A succession may re-point an identity;
        // it may never widen what its holder reaches — in particular it can
        // never silently dissolve a block.
        let db = CacheDb::open_in_memory().unwrap();
        let blocked_old = [0x21; 32];
        let blocked_new = [0x22; 32];
        let confirmed_old = [0x23; 32];
        let pending_old = [0x24; 32];
        for (holder, handle) in [
            (&blocked_old, "b-old"),
            (&blocked_new, "b-new"),
            (&confirmed_old, "c-old"),
            (&pending_old, "p-old"),
        ] {
            seed_account(&db, holder, handle).await;
        }
        contact_edge(&db, &blocked_old, &OLD, "blocked").await;
        contact_edge(&db, &blocked_old, &NEW, "accepted").await;
        contact_edge(&db, &blocked_new, &OLD, "accepted").await;
        contact_edge(&db, &blocked_new, &NEW, "blocked").await;
        contact_edge(&db, &confirmed_old, &OLD, "confirmed").await;
        contact_edge(&db, &confirmed_old, &NEW, "accepted").await;
        contact_edge(&db, &pending_old, &OLD, "pending").await;
        contact_edge(&db, &pending_old, &NEW, "accepted").await;

        db.record_peer_succession(&OLD, &NEW, b"stmt", 3)
            .await
            .unwrap()
            .unwrap();

        for (holder, survives) in [
            (&blocked_old, "blocked"),
            (&blocked_new, "blocked"),
            (&confirmed_old, "confirmed"),
            (&pending_old, "accepted"),
        ] {
            assert_eq!(
                contact_status(&db, holder, &NEW).await.as_deref(),
                Some(survives),
                "merged status for holder {:02x}",
                holder[0]
            );
            assert!(contact_status(&db, holder, &OLD).await.is_none());
        }
    }

    #[tokio::test]
    async fn the_ceremony_contact_collision_keeps_the_stricter_status() {
        // Peer zero: another LOCAL account blocked the predecessor and had
        // accepted the successor identity before the local ceremony ran. Same
        // merge rule as the peer arm — the block survives the collapse.
        let db = CacheDb::open_in_memory().unwrap();
        let holder = [0x25; 32];
        seed_account(&db, &OLD, "alice").await;
        seed_account(&db, &holder, "carol").await;
        contact_edge(&db, &holder, &OLD, "blocked").await;
        contact_edge(&db, &holder, &NEW, "accepted").await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            contact_status(&db, &holder, &NEW).await.as_deref(),
            Some("blocked")
        );
        assert!(contact_status(&db, &holder, &OLD).await.is_none());
    }

    #[tokio::test]
    async fn a_peer_succession_naming_a_local_identity_is_refused() {
        // The stranding case: writing the refusal row without the account
        // transaction would leave a local user superseded with no successor
        // account to move to.
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;

        assert_eq!(
            db.record_peer_succession(&OLD, &NEW, b"stmt", 1)
                .await
                .unwrap(),
            Err(SuccessionRefusal::OldIsLocal)
        );
        assert!(db.succession_for(&OLD).await.unwrap().is_none());
        assert_eq!(handle_of(&db, &OLD).await.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn a_peer_learns_a_succession_at_most_once() {
        // First-succession-wins holds on peers for the same structural reason as
        // on the home nest: the retired kit signs valid bytes forever.
        let db = CacheDb::open_in_memory().unwrap();
        db.record_peer_succession(&OLD, &NEW, b"first", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.record_peer_succession(&OLD, &THIRD, b"second", 2)
                .await
                .unwrap(),
            Err(SuccessionRefusal::AlreadySucceeded)
        );
        assert_eq!(
            db.succession_for(&OLD).await.unwrap().unwrap().new_actor_id,
            NEW.to_vec()
        );
    }

    #[tokio::test]
    async fn re_pointing_onto_residue_the_successor_already_holds_drops_the_old_row() {
        // Both tables are keyed on the actor id, so a blind UPDATE would hit the
        // PK. Keeping the old row instead would leave the superseded key holding
        // live membership — the exact access this propagation removes.
        let db = CacheDb::open_in_memory().unwrap();
        let channel = [0x10; 32];
        let local = [0x20; 32];
        seed_account(&db, &local, "bob").await;
        foreign_member(&db, &channel, &OLD).await;
        foreign_member(&db, &channel, &NEW).await;
        contact_edge(&db, &local, &OLD, "accepted").await;
        contact_edge(&db, &local, &NEW, "pending").await;

        db.record_peer_succession(&OLD, &NEW, b"stmt", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(members_of(&db, &channel).await, vec![NEW]);
        assert!(contact_status(&db, &local, &OLD).await.is_none());
        assert!(contact_status(&db, &local, &NEW).await.is_some());
    }

    #[tokio::test]
    async fn peer_succession_input_is_refused_before_any_write() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.record_peer_succession(&OLD[..8], &NEW, b"s", 1)
                .await
                .is_err()
        );
        assert!(
            db.record_peer_succession(&OLD, &OLD, b"s", 1)
                .await
                .is_err()
        );
        assert!(db.record_peer_succession(&OLD, &NEW, b"", 1).await.is_err());
        assert!(db.succession_for(&OLD).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn push_targets_come_from_shared_channels_and_skip_addressless_peers() {
        let db = CacheDb::open_in_memory().unwrap();
        let shared = [0x10; 32];
        let unrelated = [0x11; 32];
        let local = [0x20; 32];
        let remote_a = [0x30; 32];
        let stranger = [0x40; 32];

        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO actor_channels (actor_id, channel_id, created_at) VALUES (?1, ?2, 1)",
                rusqlite::params![local.as_slice(), shared.as_slice()],
            )
            .unwrap();
        }
        foreign_member(&db, &shared, &remote_a).await;
        // A peer on a channel the succeeded actor has nothing to do with. It is
        // a DIFFERENT nest identity, which is what "unrelated peer" means now
        // that targets resolve through the nest-wide address directory: the
        // boundary this asserts is over peer *identities*, and a second address
        // of an identity already being told is not a new recipient.
        db.register_foreign_channel_member(
            &unrelated,
            &stranger,
            &[0xE5; 32],
            Some("https://elsewhere.example"),
            RebindPower::Standing,
        )
        .await
        .unwrap();
        // Recorded before the address column existed: skipped, not an error.
        db.register_foreign_channel_member(
            &shared,
            &[0x50; 32],
            &PEER_NEST,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();

        assert_eq!(
            db.succession_push_targets(&local).await.unwrap(),
            vec!["https://peer.example".to_string()]
        );
        // The succeeded actor may itself be the foreign member — their own home
        // nest is then a target.
        assert_eq!(
            db.succession_push_targets(&remote_a).await.unwrap(),
            vec!["https://peer.example".to_string()]
        );
        assert!(
            db.succession_push_targets(&[0x99; 32])
                .await
                .unwrap()
                .is_empty()
        );

        // **push side.** A second address for a target identity JOINS
        // the target set instead of replacing it. Before the directory, this
        // leg read the membership row's own column, so the address written
        // *last* was the only target — no walk, no fallback, the sharpest form
        // of the same defect. A plant here silently redirected the whole push.
        db.register_foreign_channel_member(
            &shared,
            &remote_a,
            &PEER_NEST,
            Some("https://peer-2.example"),
            RebindPower::Standing,
        )
        .await
        .unwrap();
        let targets = db.succession_push_targets(&local).await.unwrap();
        assert!(
            targets.contains(&"https://peer.example".to_string())
                && targets.contains(&"https://peer-2.example".to_string()),
            "both addresses of the target identity must be pushed to; got {targets:?}"
        );
    }

    /// Stamp an explicit `first_seen` on a directory row. Ordering pins seed
    /// explicit timestamps because `now_epoch_secs()` is second-granular —
    /// every row a fast test writes shares one stamp, and the rowid tiebreaker
    /// then carries the ordering invisibly (convention 14).
    async fn set_addr_first_seen(db: &CacheDb, nest_id: &[u8; 32], url: &str, t: i64) {
        let conn = db.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE nest_addresses SET first_seen = ?3 WHERE nest_id = ?1 AND nest_url = ?2",
                rusqlite::params![nest_id.as_slice(), url, t],
            )
            .unwrap();
        assert_eq!(n, 1, "expected exactly one directory row for {url}");
    }

    /// The push target set is attacker-extendable (any User-class caller can
    /// mint an address binding), so it must be bounded — the same reasoning
    /// [`crate::succession_pull::MAX_ANCHOR_CANDIDATES`] applies to the pull
    /// walk. Unbounded, a crowd of identities would turn a single succession
    /// into an arbitrarily large outbound fan-out. (This is the GLOBAL-bound
    /// pin only — it deliberately floods many identities, one address each,
    /// because the per-identity directory cap makes a single-identity flood
    /// unable to reach the global limit at all. Honest-peer survival under a
    /// flood is `a_flooded_identity_cannot_evict_a_later_seen_peer`; a count
    /// alone is not that property.)
    #[tokio::test]
    async fn the_push_target_set_is_bounded() {
        let db = CacheDb::open_in_memory().unwrap();
        let shared = [0x10; 32];
        let local = [0x20; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO actor_channels (actor_id, channel_id, created_at) VALUES (?1, ?2, 1)",
                rusqlite::params![local.as_slice(), shared.as_slice()],
            )
            .unwrap();
        }
        for i in 0..(MAX_PUSH_TARGETS + 25) {
            // Distinct identity AND distinct member actor per row — the
            // membership row is keyed `(channel_id, actor_id)`, so one shared
            // actor would upsert its `home_nest_id` in place and leave only
            // the LAST identity joined to the directory.
            let mut nest = [0xE0u8; 32];
            nest[30] = (i / 256) as u8;
            nest[31] = (i % 256) as u8;
            let mut actor = [0xC0u8; 32];
            actor[30] = (i / 256) as u8;
            actor[31] = (i % 256) as u8;
            db.register_foreign_channel_member(
                &shared,
                &actor,
                &nest,
                Some(&format!("https://flood-{i}.example")),
                RebindPower::Standing,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            db.succession_push_targets(&local).await.unwrap().len(),
            MAX_PUSH_TARGETS,
            "the push fan-out must be capped"
        );
    }

    /// the cap must be FAIR, not merely present. One attacker
    /// identity holding many early-seen addresses (each costs only a dial —
    /// 32 URLs behind one wildcard cert) must not evict a later-seen honest
    /// peer from the push set: the count is not the property, honest-peer
    /// SURVIVAL is. Five attacker identities × the per-identity directory cap
    /// of early addresses would fill the old globally-sorted LIMIT before the
    /// honest peer's single later row; per-identity interleaving (every
    /// identity's best address outranks any identity's second) keeps the
    /// honest rank-1 row in the set.
    #[tokio::test]
    async fn a_flooded_identity_cannot_evict_a_later_seen_peer() {
        let db = CacheDb::open_in_memory().unwrap();
        let shared = [0x10; 32];
        let local = [0x20; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO actor_channels (actor_id, channel_id, created_at) VALUES (?1, ?2, 1)",
                rusqlite::params![local.as_slice(), shared.as_slice()],
            )
            .unwrap();
        }

        // Five attacker identities, each flooding the directory to the
        // per-identity cap with EARLY explicit timestamps (t = 100..).
        let mut t = 100i64;
        for a in 0..5u8 {
            let mut att_nest = [0xE0u8; 32];
            att_nest[31] = a;
            let mut att_actor = [0xF0u8; 32];
            att_actor[31] = a;
            for i in 0..crate::db::channels::MAX_DIRECTORY_ADDRESSES_PER_IDENTITY {
                let url = format!("https://att-{a}-{i}.example");
                db.register_foreign_channel_member(
                    &shared,
                    &att_actor,
                    &att_nest,
                    Some(&url),
                    RebindPower::Standing,
                )
                .await
                .unwrap();
                set_addr_first_seen(&db, &att_nest, &url, t).await;
                t += 1;
            }
        }

        // One honest peer, seen strictly LATER.
        let honest_nest = [0xD9u8; 32];
        let honest_actor = [0xDAu8; 32];
        db.register_foreign_channel_member(
            &shared,
            &honest_actor,
            &honest_nest,
            Some("https://honest.example"),
            RebindPower::Standing,
        )
        .await
        .unwrap();
        set_addr_first_seen(&db, &honest_nest, "https://honest.example", 500).await;

        let targets = db.succession_push_targets(&local).await.unwrap();
        assert!(
            targets.len() <= MAX_PUSH_TARGETS,
            "the global bound still holds"
        );
        assert!(
            targets.iter().any(|u| u == "https://honest.example"),
            "a later-seen honest peer must survive an early flood; got {targets:?}"
        );
    }

    /// the directory retains the FIRST
    /// `MAX_DIRECTORY_ADDRESSES_PER_IDENTITY` addresses per identity —
    /// first-N-wins, so the mint is capped at the source. A re-sighting of a
    /// retained address still updates in place at the cap (proof stays
    /// monotonic), and other identities are unaffected.
    #[tokio::test]
    async fn the_directory_retains_first_n_addresses_per_identity() {
        let db = CacheDb::open_in_memory().unwrap();
        let cap = crate::db::channels::MAX_DIRECTORY_ADDRESSES_PER_IDENTITY;

        for i in 0..(cap + 2) {
            db.record_nest_address(&PEER_NEST, &format!("https://addr-{i}.example"), false)
                .await
                .unwrap();
        }
        let count: i64 = {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT COUNT(*) FROM nest_addresses WHERE nest_id = ?1",
                rusqlite::params![PEER_NEST.as_slice()],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            count, cap as i64,
            "beyond the cap, new addresses are not retained (first-N-wins)"
        );

        // A retained address still updates in place at the cap: a proven
        // re-sighting stamps proof (monotonic set-once).
        db.record_nest_address(&PEER_NEST, "https://addr-0.example", true)
            .await
            .unwrap();
        let proven: Option<i64> = {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT proven_at FROM nest_addresses WHERE nest_id = ?1 AND nest_url = ?2",
                rusqlite::params![PEER_NEST.as_slice(), "https://addr-0.example"],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(
            proven.is_some(),
            "a retained address stays updatable at the cap"
        );

        // Another identity's first address is unaffected by this one's cap.
        let other = [0xD5u8; 32];
        db.record_nest_address(&other, "https://other.example", false)
            .await
            .unwrap();
        let other_count: i64 = {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT COUNT(*) FROM nest_addresses WHERE nest_id = ?1",
                rusqlite::params![other.as_slice()],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(other_count, 1, "the cap is per-identity, never global");
    }

    /// under a `first_seen` tie the order is pinned by
    /// rowid — the same tiebreaker `resolve_foreign_nest_urls` already
    /// carries. Second-granular stamps make ties the COMMON case for rows
    /// written in one burst, and an unpinned tie leaves the surviving-set
    /// choice to the query plan. (Exact-order assertion; seeded with an
    /// explicit shared timestamp per convention 14.)
    #[tokio::test]
    async fn push_target_order_is_deterministic_under_first_seen_ties() {
        let db = CacheDb::open_in_memory().unwrap();
        let shared = [0x10; 32];
        let local = [0x20; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO actor_channels (actor_id, channel_id, created_at) VALUES (?1, ?2, 1)",
                rusqlite::params![local.as_slice(), shared.as_slice()],
            )
            .unwrap();
        }
        let urls = [
            "https://tie-a.example",
            "https://tie-b.example",
            "https://tie-c.example",
        ];
        for url in urls {
            db.register_foreign_channel_member(
                &shared,
                &OLD,
                &PEER_NEST,
                Some(url),
                RebindPower::Standing,
            )
            .await
            .unwrap();
            set_addr_first_seen(&db, &PEER_NEST, url, 100).await;
        }
        let targets = db.succession_push_targets(&local).await.unwrap();
        assert_eq!(
            targets, urls,
            "a first_seen tie must resolve by rowid (insertion order), never by query plan"
        );
    }

    #[tokio::test]
    async fn the_pull_work_list_is_the_distinct_set_of_foreign_actors() {
        let db = CacheDb::open_in_memory().unwrap();
        foreign_member(&db, &[0x10; 32], &OLD).await;
        db.register_foreign_channel_member(
            &[0x11; 32],
            &NEW,
            &PEER_NEST,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();

        let mut list = db.distinct_foreign_member_actors().await.unwrap();
        list.sort();
        let mut want = vec![OLD, NEW];
        want.sort();
        assert_eq!(
            list, want,
            "every distinct foreign actor is a work-list entry"
        );
    }

    #[tokio::test]
    async fn a_re_invite_without_an_address_does_not_blank_a_known_one() {
        let db = CacheDb::open_in_memory().unwrap();
        let channel = [0x10; 32];
        foreign_member(&db, &channel, &OLD).await;
        db.register_foreign_channel_member(&channel, &OLD, &PEER_NEST, None, RebindPower::Standing)
            .await
            .unwrap();

        assert_eq!(
            db.resolve_foreign_nest_urls(&PEER_NEST, 8).await.unwrap(),
            vec!["https://peer.example".to_string()],
            "losing the address would silently drop this peer from propagation"
        );
    }

    #[tokio::test]
    async fn a_recovery_head_round_trips_advances_and_pins_the_anchor_write_once() {
        let db = CacheDb::open_in_memory().unwrap();
        const HOME: [u8; 32] = [0x77; 32];
        const OTHER: [u8; 32] = [0x88; 32];
        assert!(db.foreign_recovery_head(&OLD[..]).await.unwrap().is_none());

        db.record_foreign_recovery_head(&OLD[..], &[0x22; 32], 3, &HOME)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_recovery_head(&OLD[..]).await.unwrap().unwrap(),
            ForeignRecoveryHead {
                recovery_pubkey: [0x22; 32],
                seq: 3,
                anchor_nest_id: HOME,
            }
        );

        // A later, higher head advances…
        db.record_foreign_recovery_head(&OLD[..], &[0x33; 32], 5, &HOME)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_recovery_head(&OLD[..]).await.unwrap().unwrap(),
            ForeignRecoveryHead {
                recovery_pubkey: [0x33; 32],
                seq: 5,
                anchor_nest_id: HOME,
            }
        );

        // …a regression — the thief's re-mint shape — is a silent no-op, AND an
        // attempt to re-point the anchor to another identity is refused: the
        // anchor is write-once, so a rewrite cannot move a proven pin.
        db.record_foreign_recovery_head(&OLD[..], &[0x99; 32], 5, &OTHER)
            .await
            .unwrap();
        db.record_foreign_recovery_head(&OLD[..], &[0x99; 32], 2, &OTHER)
            .await
            .unwrap();
        assert_eq!(
            db.foreign_recovery_head(&OLD[..]).await.unwrap().unwrap(),
            ForeignRecoveryHead {
                recovery_pubkey: [0x33; 32],
                seq: 5,
                anchor_nest_id: HOME,
            },
            "a non-advancing write must never regress the head or move the pinned anchor"
        );
    }

    #[tokio::test]
    async fn the_anchor_identity_is_the_oldest_row_overall_not_the_oldest_addressable() {
        let db = CacheDb::open_in_memory().unwrap();
        // Oldest row: the honest home, recorded WITHOUT a URL (the NULL
        // `nest_url` a room accept with an empty invitee URL writes). Later, addressable binding at a nest the attacker
        // controls — the plant.
        db.register_foreign_channel_member(
            &[0x10; 32],
            &OLD,
            &PEER_NEST,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
        db.register_foreign_channel_member(
            &[0x11; 32],
            &OLD,
            &[0x66; 32],
            Some("https://thief.example"),
            RebindPower::Standing,
        )
        .await
        .unwrap();

        // The anchor IDENTITY is the honest URL-less row's home, never the
        // attacker's addressable one.
        assert_eq!(
            db.oldest_foreign_member_nest_id(&OLD).await.unwrap(),
            Some(PEER_NEST),
            "the oldest row overall is the anchor identity, even with a NULL URL"
        );
        // …and it has no addressable binding, so it is un-resolvable — the pull
        // refuses rather than dialing the attacker.
        assert!(
            db.resolve_foreign_nest_urls(&PEER_NEST, 8)
                .await
                .unwrap()
                .is_empty(),
            "a URL-less anchor identity with no sibling address is un-resolvable"
        );
    }

    #[tokio::test]
    async fn a_url_less_anchor_resolves_its_url_from_a_sibling_of_the_same_identity() {
        let db = CacheDb::open_in_memory().unwrap();
        // The oldest row is URL-less (NULL); a later addressable binding for the
        // SAME identity supplies the URL (the member's current address).
        db.register_foreign_channel_member(
            &[0x10; 32],
            &OLD,
            &PEER_NEST,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
        db.register_foreign_channel_member(
            &[0x11; 32],
            &OLD,
            &PEER_NEST,
            Some("https://later.example"),
            RebindPower::Standing,
        )
        .await
        .unwrap();

        assert_eq!(
            db.oldest_foreign_member_nest_id(&OLD).await.unwrap(),
            Some(PEER_NEST)
        );
        assert_eq!(
            db.resolve_foreign_nest_urls(&PEER_NEST, 8).await.unwrap(),
            vec!["https://later.example".to_string()],
            "the URL resolves from a sibling binding sharing the anchor identity"
        );
        // And an actor with no residue at all anchors nowhere.
        db.register_foreign_channel_member(
            &[0x12; 32],
            &NEW,
            &[0xEE; 32],
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
        assert!(
            db.resolve_foreign_nest_urls(&[0xEE; 32], 8)
                .await
                .unwrap()
                .is_empty(),
            "an identity with no addressable binding is un-resolvable"
        );
        assert_eq!(
            db.oldest_foreign_member_nest_id(&THIRD).await.unwrap(),
            None
        );
    }

    /// The candidate resolver's three properties, which together are what makes
    /// a planted binding survivable rather than terminal:
    /// **every** address naming the identity is returned, **oldest first**, and
    /// **deduplicated** so one address registered many times cannot spend the
    /// caller's whole dial budget on itself.
    #[tokio::test]
    async fn the_url_candidates_are_every_address_oldest_first_and_deduplicated() {
        let db = CacheDb::open_in_memory().unwrap();
        // Registration order IS the candidate order: the honest address lands
        // first, the address an attacker adds later can only ever be later.
        for (channel, url) in [
            ([0x10; 32], "https://honest.example"),
            ([0x11; 32], "https://honest.example"), // the same box, another binding
            ([0x12; 32], "https://planted.example"),
            ([0x13; 32], "https://alsoplanted.example"),
        ] {
            db.register_foreign_channel_member(
                &channel,
                &OLD,
                &PEER_NEST,
                Some(url),
                RebindPower::Standing,
            )
            .await
            .unwrap();
        }
        // A binding for an UNRELATED actor at the same identity is a candidate
        // too — this directory is nest-wide, which is exactly why one actor's
        // planted row could deny another actor's succession.
        db.register_foreign_channel_member(
            &[0x14; 32],
            &NEW,
            &PEER_NEST,
            Some("https://elsewhere.example"),
            RebindPower::Standing,
        )
        .await
        .unwrap();

        assert_eq!(
            db.resolve_foreign_nest_urls(&PEER_NEST, 8).await.unwrap(),
            vec![
                "https://honest.example".to_string(),
                "https://planted.example".to_string(),
                "https://alsoplanted.example".to_string(),
                "https://elsewhere.example".to_string(),
            ],
            "every distinct address naming the identity, oldest sighting first"
        );

        // Re-registering the oldest address does not demote it: the order is by
        // FIRST sighting, so an attacker cannot reorder the list by touching it.
        db.register_foreign_channel_member(
            &[0x15; 32],
            &NEW,
            &PEER_NEST,
            Some("https://honest.example"),
            RebindPower::Standing,
        )
        .await
        .unwrap();
        assert_eq!(
            db.resolve_foreign_nest_urls(&PEER_NEST, 1).await.unwrap(),
            vec!["https://honest.example".to_string()],
            "re-registering an address must not move it later in the order"
        );

        // And the cap is a cap.
        assert_eq!(
            db.resolve_foreign_nest_urls(&PEER_NEST, 2)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    /// **a plant must JOIN the directory, never erase it.**
    ///
    /// The sibling test above plants only through *fresh channel ids* — the
    /// INSERT path — which is why it stayed green while this hole was open. The
    /// membership write is an **upsert** on `(channel_id, actor_id)`, so a
    /// second call for the SAME pair rewrites the row in place: `created_at` and
    /// rowid are untouched (the plant inherits the honest row's sighting, so it
    /// leads an oldest-first walk) and the address column is overwritten (so the
    /// honest address is gone, not merely demoted). For an identity known
    /// through one cross-nest row — the ordinary 1:1 DM or folder share — the
    /// candidate list afterwards holds *only* the plant, the walk has nothing to
    /// fail over to, and the permanent silent denial this fix closed is restored.
    ///
    /// The conflict path IS the finding: a pin that plants via a fresh channel
    /// id exercises the half already closed.
    #[tokio::test]
    async fn a_planted_address_joins_the_directory_instead_of_erasing_it() {
        let db = CacheDb::open_in_memory().unwrap();
        db.register_foreign_channel_member(
            &[0x10; 32],
            &OLD,
            &PEER_NEST,
            Some("https://honest"),
            RebindPower::Standing,
        )
        .await
        .unwrap();

        // THE PLANT: the same (channel, actor) pair — the CONFLICT path — with a
        // different address. One User-class `welcome.deliver` call reaches this.
        db.register_foreign_channel_member(
            &[0x10; 32],
            &OLD,
            &PEER_NEST,
            Some("https://attacker"),
            RebindPower::Standing,
        )
        .await
        .unwrap();

        let urls = db.resolve_foreign_nest_urls(&PEER_NEST, 8).await.unwrap();
        assert!(
            urls.contains(&"https://honest".to_string()),
            "the honest address was ERASED by the plant, leaving {urls:?} — the walk has nothing \
             to fail over to and the identity is silently denied"
        );
        assert!(
            urls.contains(&"https://attacker".to_string()),
            "the planted address should still be a candidate (it fails its own identity proof); \
             got {urls:?}"
        );
    }

    /// **A re-sighting must not move an address later in the walk order.**
    ///
    /// The directory's `first_seen = MIN(stored, excluded)` guard is what holds
    /// this, and it is deliberately pinned against **explicit stored timestamps**
    /// rather than by registering twice and hoping the clock moved:
    /// `now_epoch_secs()` is second-granular, so every row a fast test writes
    /// shares one timestamp and the `rowid` tiebreaker silently carries the
    /// ordering — a re-sighting could overwrite `first_seen` and no wall-clock
    /// test would notice (the 60th-pass mutation matrix caught exactly that).
    /// Seeding distinct `first_seen` values directly makes the assertion
    /// latency-independent, per testing.md convention 14.
    #[tokio::test]
    async fn a_re_sighting_does_not_move_an_address_later_in_the_order() {
        let db = CacheDb::open_in_memory().unwrap();
        {
            let conn = db.conn.lock().await;
            for (url, seen) in [("https://old", 100i64), ("https://mid", 200i64)] {
                conn.execute(
                    "INSERT INTO nest_addresses (nest_id, nest_url, first_seen, proven_at)
                     VALUES (?1, ?2, ?3, NULL)",
                    rusqlite::params![PEER_NEST.as_slice(), url, seen],
                )
                .unwrap();
            }
        }
        // Re-sight the OLDER address now (a far larger timestamp).
        db.register_foreign_channel_member(
            &[0x10; 32],
            &OLD,
            &PEER_NEST,
            Some("https://old"),
            RebindPower::Standing,
        )
        .await
        .unwrap();

        assert_eq!(
            db.resolve_foreign_nest_urls(&PEER_NEST, 8).await.unwrap(),
            vec!["https://old".to_string(), "https://mid".to_string()],
            "a re-sighting moved the older address later in the walk — an attacker who can \
             cause a re-registration could demote the honest address without erasing it"
        );
    }

    /// A plant must not inherit the honest row's position: ordering is per
    /// **address**, by its own first sighting, not per membership row.
    #[tokio::test]
    async fn a_replanted_address_does_not_inherit_the_honest_rows_sighting() {
        let db = CacheDb::open_in_memory().unwrap();
        db.register_foreign_channel_member(
            &[0x10; 32],
            &OLD,
            &PEER_NEST,
            Some("https://honest"),
            RebindPower::Standing,
        )
        .await
        .unwrap();
        db.register_foreign_channel_member(
            &[0x10; 32],
            &OLD,
            &PEER_NEST,
            Some("https://attacker"),
            RebindPower::Standing,
        )
        .await
        .unwrap();

        assert_eq!(
            db.resolve_foreign_nest_urls(&PEER_NEST, 1).await.unwrap(),
            vec!["https://honest".to_string()],
            "the honest address must still lead the walk after the plant"
        );
    }

    // ── the long tail's scheduled-action ruling ──────────────────

    /// Queue a destructive action that is already due, exactly as the
    /// production door does: `status='pending'` with `execute_after` in the
    /// past, so the executor's own selector reports it ready.
    async fn queue_due_action(
        db: &CacheDb,
        actor: &[u8; 32],
        action_type: &str,
        target: Option<&str>,
    ) -> i64 {
        let conn = db.conn.lock().await;
        conn.execute(
            "INSERT INTO pending_actions
                (action_type, actor_id, target, status, created_at, execute_after)
             VALUES (?1, ?2, ?3, 'pending', 1, 1)",
            rusqlite::params![action_type, actor.as_slice(), target],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// **A queued destructive action must not fire after its author's
    /// ceremony — the product observable, asserted on the executor's own
    /// input.**
    ///
    /// `execute_ready_actions` authenticates nobody: the row *is* the
    /// authority, and `list_ready_pending_actions` is the whole of its
    /// selection. So the question "can the thief's queued account deletion
    /// still fire?" is exactly "is the row still in that list?", which is what
    /// this asserts — never a status column read for its own sake.
    ///
    /// The delays make this the common case rather than a corner: 6 h for a
    /// handle change, 48 h for a snapshot delete, 14 d for an account
    /// deletion. A succession racing a thief happens *inside* those windows by
    /// construction — that is what the window is for.
    #[tokio::test]
    async fn a_succession_disarms_the_retired_identitys_queued_destructive_actions() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let armed = queue_due_action(&db, &OLD, "account.delete", None).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let ready = db.list_ready_pending_actions().await.unwrap();
        assert!(
            !ready.iter().any(|a| a.id == armed),
            "the ceremony left a due `account.delete` armed: the 60 s executor \
             tick authenticates nobody, so a seed thief's queued deletion fires \
             against the account the ceremony just rescued"
        );
    }

    /// **A row the executor has already claimed is disarmed too, and a run
    /// that then fails never re-arms it.** The disarm cannot recall a run
    /// under way, but if that run fails the executor's release must find the
    /// row cancelled, not hand the thief's action back to the queue.
    #[tokio::test]
    async fn a_succession_disarms_a_claimed_action_so_a_failed_run_never_rearms_it() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let claimed = queue_due_action(&db, &OLD, "account.delete", None).await;
        assert!(db.claim_pending_action(claimed).await.unwrap().is_some());

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        // The run fails; the executor hands its claim back.
        db.release_pending_action_claim(claimed).await.unwrap();
        let row = db.get_pending_action(claimed).await.unwrap().unwrap();
        assert_eq!(row.status, "cancelled", "the disarm holds over the claim");
        assert!(
            !db.mark_pending_action_executed(claimed).await.unwrap(),
            "a completed run must not overwrite the disarm's cancel"
        );
    }

    /// **The direction NOT chosen: disarming must not become a
    /// move, and must not touch anything it does not own.**
    ///
    /// A `Move` here would be the *worst* available verdict — it re-aims the
    /// thief's queued deletion at the successor's live corpus — so the row
    /// stays on the retired identity, which is also what keeps the ledger's
    /// creation order intact. Terminal rows and other actors' rows are the
    /// blast-radius half: a leg keyed on status alone would re-cancel history,
    /// and one keyed on nothing would disarm the whole box.
    #[tokio::test]
    async fn disarming_a_queued_action_is_not_a_move_and_spares_everything_else() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        let other = [0x77u8; 32];
        seed_account(&db, &other, "bob").await;

        let mine = queue_due_action(&db, &OLD, "account.delete", None).await;
        let theirs = queue_due_action(&db, &other, "account.delete", None).await;
        let executed = queue_due_action(&db, &OLD, "handle.change", None).await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE pending_actions SET status = 'executed', executed_at = 5 WHERE id = ?1",
                rusqlite::params![executed],
            )
            .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        let owner_of = |id: i64| -> Vec<u8> {
            conn.query_row(
                "SELECT actor_id FROM pending_actions WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            owner_of(mine),
            OLD.to_vec(),
            "a disarmed action must STAY on the retired identity — moving it \
             re-aims a thief's queued deletion at the successor's live corpus, \
             the one verdict worse than leaving it armed"
        );
        assert_eq!(
            owner_of(theirs),
            other.to_vec(),
            "another actor's queued action is not this ceremony's business"
        );
        let (their_status, executed_status): (String, String) = (
            conn.query_row(
                "SELECT status FROM pending_actions WHERE id = ?1",
                rusqlite::params![theirs],
                |r| r.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT status FROM pending_actions WHERE id = ?1",
                rusqlite::params![executed],
                |r| r.get(0),
            )
            .unwrap(),
        );
        assert_eq!(their_status, "pending", "a bystander's action stays armed");
        assert_eq!(
            executed_status, "executed",
            "a terminal row is history and must not be re-written — the leg's \
             predicate is (this actor AND still pending), never status alone"
        );
    }

    /// **Cancelling a snapshot deletion has to take the mark off the snapshot,
    /// and the ceremony's disarm is a cancel.**
    ///
    /// `cancel_pending_action` clears `snapshots.deletion_pending` for exactly
    /// this reason: nothing else ever clears it, so a snapshot left marked is
    /// silently excluded from `count_active_snapshots` — its own folder's hard
    /// floor and the auto-pruner's candidate population — **forever**, on the
    /// strength of a deletion that was called off. A disarm that skips the
    /// un-mark hands the successor exactly that, on their own snapshots.
    #[tokio::test]
    async fn disarming_a_snapshot_deletion_unmarks_the_successors_snapshot() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO folders (id, name, actor_id, created_at) VALUES (1, 'docs', ?1, 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
            // `folders.actor_id` moves, so this snapshot is the SUCCESSOR's the
            // moment the ceremony commits — which is what makes a left-behind
            // `deletion_pending` mark their problem rather than the retired
            // identity's.
            conn.execute(
                "INSERT INTO snapshots
                     (id, folder_id, created_at, file_count, total_bytes, deletion_pending)
                 VALUES (42, 1, 1, 0, 0, 1)",
                [],
            )
            .unwrap();
        }
        queue_due_action(&db, &OLD, "snapshot.delete", Some("42")).await;

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        let pending: i64 = conn
            .query_row(
                "SELECT deletion_pending FROM snapshots WHERE id = 42",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            pending, 0,
            "the disarm must clear `deletion_pending` like every other cancel \
             — left set, the successor's snapshot is invisible to its own \
             folder's retention floor for good"
        );
    }

    /// **The un-mark reaches exactly what the disarm disarmed — the pin a
    /// surviving mutation asked for.**
    ///
    /// Widening the leg's `SELECT` to the actor alone (dropping `AND status =
    /// 'pending'`) left all 145 tests green, because the `UPDATE` carries its
    /// own status guard so no terminal row is re-written. The un-mark has no
    /// such guard: it would clear `deletion_pending` for the snapshot targets
    /// of rows the leg is not disarming at all, silently reviving a snapshot
    /// that some *other* live action has legitimately marked. A mutation that
    /// survives a green suite is a hole in the suite, not a harmless one — so
    /// this is the observable that separates the two predicates.
    #[tokio::test]
    async fn the_disarm_unmarks_only_the_snapshots_it_actually_disarmed() {
        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO folders (id, name, actor_id, created_at) VALUES (1, 'docs', ?1, 1)",
                rusqlite::params![OLD.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO snapshots
                     (id, folder_id, created_at, file_count, total_bytes, deletion_pending)
                 VALUES (7, 1, 1, 0, 0, 1)",
                [],
            )
            .unwrap();
        }
        // A TERMINAL action naming snapshot 7 — its deletion already ran. The
        // live mark on 7 belongs to something else entirely, and this leg has
        // no business touching it.
        let spent = queue_due_action(&db, &OLD, "snapshot.delete", Some("7")).await;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE pending_actions SET status = 'executed', executed_at = 5 WHERE id = ?1",
                rusqlite::params![spent],
            )
            .unwrap();
        }

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        let still_marked: i64 = conn
            .query_row(
                "SELECT deletion_pending FROM snapshots WHERE id = 7",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            still_marked, 1,
            "the disarm cleared a mark it did not set: its un-mark must be \
             driven by the rows it actually cancelled, never by every row the \
             retired identity ever queued"
        );
    }

    /// **The ceremony leaves the admin transparency chain VERIFYING — the
    /// direction not chosen, and the pin M7 proved was missing.**
    ///
    /// Flipping `audit_log` to `Move(Plain)` left all 146 tests green. Unlike
    /// `invite_requests` one entry over, this table is fully reachable — every
    /// actor accumulates rows — so that was a real hole rather than an
    /// unfalsifiable ruling: the generic sweep seeds a row, the executor moves
    /// it, declaration and observable agree, and the fact that the move has
    /// **invalidated a cryptographic commitment** is invisible to all of it.
    ///
    /// So this asserts the commitment itself rather than the column, by
    /// recomputing `audit_on_conn`'s hash over the stored bytes. Nothing else
    /// in the tree does — `audit_integrity` returns head metadata and never
    /// walks the links — so this is also the chain's first actual verifier, and
    /// it reds on any future mutation of these rows, not just this one.
    #[tokio::test]
    async fn the_ceremony_leaves_the_audit_chain_verifying() {
        use sha2::Digest;

        let db = CacheDb::open_in_memory().unwrap();
        seed_account(&db, &OLD, "alice").await;
        db.audit(Some(&OLD), "a.one", Some("t1"), Some("d1"))
            .await
            .unwrap();
        db.audit(Some(&OLD), "a.two", None, None).await.unwrap();
        db.audit(Some(&[0x55u8; 32]), "a.three", Some("t3"), None)
            .await
            .unwrap();

        db.record_succession(&OLD, &NEW, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        let conn = db.conn.lock().await;
        let rows: Vec<(
            i64,
            i64,
            Option<Vec<u8>>,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            i64,
        )> = conn
            .prepare(
                "SELECT id, ts, actor_id, action, target, detail, prev_hash, entry_hash,
                        entry_hash_version
                   FROM audit_log ORDER BY id ASC",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            rows.len() >= 3,
            "the ceremony's own audit rows are here too"
        );

        let mut expected_prev = format!("{:x}", sha2::Sha256::digest(b"fauna-audit-genesis-v1"));
        for (id, ts, actor_id, action, target, detail, prev_hash, entry_hash, version) in &rows {
            assert_eq!(
                prev_hash, &expected_prev,
                "audit row {id}'s prev_hash no longer matches the previous row's \
                 entry_hash — the chain is broken, which is what re-pointing any \
                 column inside it does"
            );
            // The preimage comes from the version the row RECORDS — one format
            // per row, no trying until one matches (`db::chain_version`).
            let version = crate::db::chain_version::ChainVersion::from_recorded(*version)
                .unwrap_or_else(|| {
                    panic!("audit row {id} records a preimage version this binary cannot verify")
                });
            let recomputed = crate::db::chain_version::audit_entry_hash(
                version,
                &crate::db::chain_version::AuditPreimage {
                    id: *id,
                    ts: *ts,
                    actor_id: actor_id.as_deref(),
                    action,
                    target: target.as_deref(),
                    detail: detail.as_deref(),
                    prev_hash,
                },
            );
            assert_eq!(
                &recomputed, entry_hash,
                "audit row {id}'s stored entry_hash does not match its own bytes \
                 — `actor_id` is INSIDE this hash, so a succession that re-points \
                 it forges the transparency record the admin surface serves"
            );
            expected_prev = entry_hash.clone();
        }

        let on_old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE actor_id = ?1",
                rusqlite::params![OLD.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            on_old >= 2,
            "the retired identity's audit rows must still name the retired \
             identity: they answer who an entry is ABOUT, and the successor was \
             not the subject of those actions"
        );
    }

    /// The PRODUCER half of the threaded commit stamp: `record_succession`
    /// reads the clock exactly once, INSERTs that value as
    /// `actor_successions.succeeded_at`, and returns the same binding as
    /// `SuccessionApplied::succeeded_at`. The consumer half is pinned in
    /// `recovery_handlers.rs` (`the_submit_reply_never_reads_the_clock_directly`),
    /// but that guard reads only its own file: a second clock read HERE — say
    /// `succeeded_at: now_epoch_secs()` in the `SuccessionApplied`
    /// construction — would drift the reply from the row across a wall-clock
    /// second boundary with both existing pins green (the behavioural
    /// equality assert lands both reads in the same second on any fast run,
    /// and there is no seam on the DB clock to make them straddle one). Same
    /// source-guard class, bounded to this fn so it cannot match itself.
    #[test]
    fn record_succession_returns_the_stamp_it_committed_not_a_second_clock_read() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/db/successions.rs");
        let src = std::fs::read_to_string(&path).expect("read own source");

        let start = src
            .find("pub async fn record_succession(")
            .expect("record_succession must exist");
        let after_start = &src[start..];
        let next_fn = after_start[1..]
            .find("\n    pub async fn ")
            .map(|i| i + 1)
            .expect("another method must follow record_succession");
        let body = &after_start[..next_fn];

        assert_eq!(
            body.matches("now_epoch_secs()").count(),
            1,
            "record_succession must read the clock exactly once — the one \
             `let now = now_epoch_secs();` that is INSERTed as \
             `actor_successions.succeeded_at`; a second read anywhere in the \
             transaction is a stamp that can disagree with the row"
        );
        assert!(
            body.contains("succeeded_at: now,"),
            "SuccessionApplied::succeeded_at must be the bound `now` the INSERT \
             used — the reply's value is this binding, not a fresh clock read"
        );
        assert!(
            body.contains("rusqlite::params![old, new, statement, seq as i64, now]"),
            "the actor_successions INSERT must bind that same `now` as \
             succeeded_at — the row and the reply are one value by construction"
        );
    }
}
