//! The enumerable per-actor data boundary — every SQLite table whose rows
//! belong to one local actor, keyed by that table's own actor-identifying
//! column (`actor_id`, `author`, `owner_actor_id`, `recipient_id`, `scope_id`,
//! … ~15 spellings, none of them a foreign key).
//!
//! **Why this exists.** `nest.db` has zero foreign keys to `users`
//! (`db/migrations.rs`), so "everything belonging to actor X" was answerable
//! only by three hand-maintained, mutually-inconsistent partial inventories —
//! succession re-point (~16 tables, `db/successions.rs`), per-actor export
//! (~8 domains, `export_routes.rs`), and the nest-wide logical dump (28
//! hardcoded names, `export/logical.rs`) — while `delete_user` touched exactly
//! 7 tables and left the other ~140 orphaned.
//! `docs/goal/architecture/account-data-plane.md` § Nest-side requirements
//! item 1 names this as the same missing piece: **build the registry
//! once**, not a fourth hand list.
//!
//! **Scope of this registry.** It answers "which tables does actor X's own
//! data live in", not "does deleting X ever need to touch a row some *other*
//! actor owns". A row where the actor-identifying column names X only as a
//! third party — an admin who approved someone else's request
//! (`approved_by_actor_id`), a domain's configured catch-all
//! (`mail_domains.catch_all_actor_id`), a report's *content* rather than its
//! filer — is deliberately absent: purging on that column would delete a row
//! X does not own. Those tables were reviewed and excluded by hand while
//! building this list (see the account-deletion-orphans recon), not missed.
//!
//! **A succession ruling is only as reachable as the ceremony that executes
//! it — ask, per table, whether its rows can belong to an identity THIS nest
//! succeeds (2026-08-13, the backup/custody plane).** Both succession writers
//! refuse the case a cross-nest table lives in: `record_succession` needs the
//! old identity to hold a locally-registered RecoveryKey chain, and
//! `record_peer_succession` refuses `OldIsLocal` outright. A held-for-friends
//! guest is admitted on the *destination* nest as a storage-only local user
//! (`behavior/backup-destinations.md` § Held-for-friends enrollment) whose
//! chain is registered on their **home** nest — so on the box holding their
//! `backup_writer_grants` / `backup_custody*` rows they are local (peer path
//! refuses) and chain-less (home path refuses), and no ceremony ever runs
//! there. Such a table's ruling is a **declaration of direction**, not a fix:
//! the data-driven gates still witness it (they seed synthetically and run the
//! real ceremony), which is exactly why a reader can mistake it for a closed
//! exposure. Rule it as if it were reachable — the safe direction — and say so
//! in the reason, because the live gap in those cases is cross-nest and no
//! per-table verdict can reach it.
//!
//! **`Policy::Purge` vs `Policy::Retain`.** Every table below was reviewed
//! individually. `Retain` tables genuinely are per-actor, but a raw
//! `DELETE … WHERE <col> = X` on them is *wrong*, not merely deferred: a post
//! needs the existing federation-aware retraction (Nostr kind-5, AP
//! tombstone) instead of a silent row drop; a financial/entitlement record
//! needs its own retention ruling; an identity-succession or handle-cooldown
//! record must outlive the actor by design. Each carries its reason so a
//! future session does not "fix" it into a silent purge — that is exactly
//! the failure mode `docs/goal/principles.md` § No user-data loss's alpha
//! carve-out gate (explicit surfaced approval before any deletion) exists to
//! prevent. A third verdict, [`Policy::Partial`], is for the table where
//! neither is right — some rows must go and some must stay — and the
//! registry has no row filter to say which: the purge walk carries a
//! hand-written predicate and a dedicated test owns the row rule (`outbox`,
//! the first; `abuse_reports`, whose leg withdraws open rows rather than
//! deleting them).
//!
//! **Who reads this list today.** [`CacheDb::purge_orphaned_actor_rows`] and —
//! since 2026-08-12 — **the succession ceremony**, which executes every
//! [`MoveShape::Plain`] leg from [`plain_move_legs`], so a table joins the
//! re-point by declaring it and by nothing else. Since 2026-08-15 the **per-actor export** reads it too:
//! [`CacheDb::gather_actor_export`] walks [`export_emit_legs`] and
//! `export_routes.rs` emits one `tables/<name>.ndjson` per emitting verdict,
//! beside the 11 hand-written shaped domains the walk deliberately skips
//! ([`Export::Shaped`]). The nest-wide logical dump still carries its own hand
//! list, and must — see the TRAP note below.
//!
//! **⚠ But the logical dump is a TRAP — do not converge it on this registry
//!.** `export/logical.rs`'s `DUMP_TABLES`
//! is narrow *deliberately*: the S9 path-sealing self-backup ruling
//! (`docs/goal/architecture/encryption-at-rest.md` § Carve-outs;
//! `docs/goal/behavior/path-sealing.md`) bounds how long pre-scrub plaintext
//! survives by resting on the daily dump carrying **no name plane at all**, and
//! `folders`, `sync_changes`, `snapshots` and `snapshot_files` are all
//! actor-scoped — so pointing that list here would pull them in and regress a
//! ratified confidentiality bound. `export::logical::tests::the_dump_carries_no_sealed_plane`
//! reds if it is attempted. The drift is real, but the remedy needs a
//! **per-table confidentiality disposition**, a fourth axis this registry does
//! not carry: an over-broad *export* leaks where an over-broad *move* is a
//! security regression. The same caution applies to the per-actor export.

use super::{CacheDb, table_exists};
use anyhow::{Context, Result};

/// What happens to a table's rows for the deleted actor when the registry is
/// walked. See the module doc for why `Retain` is not "purge later".
#[derive(Debug, Clone, Copy)]
pub enum Policy {
    /// `DELETE FROM <table> WHERE <column> = <actor>` is correct and complete
    /// on its own — the row is wholly this actor's private/operational state.
    Purge,
    /// Genuinely per-actor, but a raw delete is wrong for the stated reason.
    /// Never purged by [`CacheDb::purge_orphaned_actor_rows`].
    Retain(&'static str),
    /// Only some rows are purged, by a predicate the purge walk owns — the
    /// deletion axis's twin of [`Succession::Partial`], and for its reason: an
    /// `ActorTable` is one table and one column, so a row filter is not
    /// expressible here. The generic loop skips the table (it is not `Purge`);
    /// a hand-written leg in [`CacheDb::purge_orphaned_actor_rows`] runs the
    /// predicate; and the table's own dedicated test is the authority on the
    /// row-level rule — both registry-driven sweeps deliberately assert nothing
    /// about it, since "every row goes" and "every row stays" are each wrong.
    /// The reason says what goes, what stays, and which test pins it. The
    /// predicate may take part of a ROW rather than whole rows (`abuse_reports`:
    /// an open report's words and open status go, the row stays).
    Partial(&'static str),
}

/// How a table spells the actor id in its own actor-identifying column — the
/// third piece of registry data, and the one whose absence made the registry
/// walk a **silent no-op** on a whole family of tables.
///
/// Most of `nest.db` binds the raw 32 bytes as a `BLOB`. The three bridge
/// families (`nostr_*`, `bluesky_*`, `ap_*`) each declare their actor column
/// `TEXT` and store lowercase hex, because they were built as bridge-side
/// schemas where the id travels as a string. **SQLite applies no affinity
/// conversion between a blob operand and a TEXT column**, so
/// `WHERE actor_id = ?` with a blob parameter against those tables matches
/// nothing at all — it does not error, it returns rowcount 0, which reads
/// exactly like "this account had no rows there". That is why the encoding is
/// *data* here rather than a convention: a future bridge table joins the
/// registry by stating its encoding, and cannot silently join the broken half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKey {
    /// The raw 32 bytes, bound as a `BLOB` (the `nest.db` default).
    Blob,
    /// Lowercase hex of the 32 bytes, bound as `TEXT` — the `nostr_*` /
    /// `bluesky_*` / `ap_*` bridge tables. Same spelling `db/successions.rs`'s
    /// Nostr re-point legs use (`hex::encode`).
    Hex,
}

/// What an identity **succession** does with a table's rows — the third axis,
/// and deliberately not derivable from [`Policy`].
///
/// **Why the deletion axis cannot be reused.** `Policy` answers *deletion*:
/// may these rows be dropped with a raw `DELETE`? Succession asks a different
/// question — the account continues under a new key, so each table must say
/// whether its rows follow the account, stay with the retired identity, or die.
/// The two axes are independent in both directions: `folders` is `Purge` on
/// deletion and `Move` here, while `actor_mls_pubkeys` is `Purge` on deletion
/// and must emphatically **not** move.
///
/// **`Stay` covers "left behind because compromised", and that is on purpose.**
/// The natural reading groups the seal keys with the burns — both are refusals
/// to carry — but the ceremony *does different things*: it deletes a capability
/// grant and it leaves `actor_mls_pubkeys` in place. Filing the seal keys as a
/// burn would invite a future session to "finish the job" by deleting them, and
/// leaving them behind is **load-bearing**: an address that resolves to an actor
/// with no registered pubkey is exactly what arms the `succession_pending`
/// tempfail. So this axis splits on *what the ceremony does*, which is what a
/// test can observe, and each `Stay` reason says whether it is history or
/// compromise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Succession {
    /// The rows are the account's ownership of resting data, its routing, or
    /// its reach — they must name the successor after the ceremony, or the
    /// user loses their own corpus. The payload says whether the plain
    /// statement is the whole leg — see [`MoveShape`].
    Move(MoveShape),
    /// The rows remain on the retired identity. Either signed history and
    /// attribution the old key genuinely produced, participation in *other*
    /// owners' sets and groups (which moves by propagation, never by the
    /// succession transaction), or key material deliberately left behind
    /// because it derives from what the thief read. The reason says which.
    Stay(&'static str),
    /// The ceremony **deletes** the rows: standing authority a seed thief could
    /// have minted, which re-granting makes bounded and visible.
    Burn(&'static str),
    /// Only some rows move, by a predicate the ceremony owns — the table's own
    /// dedicated test is the authority on the row-level rule, and the
    /// registry-driven sweep deliberately asserts nothing about it.
    Partial(&'static str),
    /// **Reference columns only** ([`SUCCESSION_REFERENCES`]): the row survives
    /// and its *pointer* to the retired identity is nulled. The distinct
    /// verdict earns its place because neither of the other three fits — the
    /// row is not the actor's to move or burn, and leaving the pointer is not
    /// neutral when something downstream still resolves through it.
    Clear(&'static str),
    /// Not yet ruled for succession. **This is the status quo, not a
    /// judgement**: the rows simply stay where they are, exactly as they did
    /// before this axis existed. Every one is a candidate defect of the
    /// `account_aliases` class, so the count is ratcheted down-only and no new
    /// table may join (`tests::the_unruled_backlog_only_shrinks`).
    Unruled,
}

/// Whether a [`Succession::Move`] table's leg **is** the plain statement, or
/// does strictly more than move rows.
///
/// This is the declaration the registry-driven executor reads, and it is
/// deliberately the **smallest** axis that makes the executor possible.
///
/// **Why there is no collision vocabulary here, though a reader will expect
/// one.** The ceremony is *collision-free by construction* — `record_succession`
/// refuses `NewAlreadyRegistered`, so its successor owns nothing a move could
/// land on (`succession-aftermath.md` § Re-key scope). A collision verdict
/// would be unobservable (nothing can collide), so a wrong entry would be a
/// silent no-op in the nest's most security-critical transaction; a real
/// collision is a bug, and the bare `UPDATE` aborts the ceremony on it.
///
/// So this axis answers one question a test can always see: *is `UPDATE {table}
/// SET {column} = ?new WHERE {column} = ?old` the entire leg?*
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveShape {
    /// The plain statement is the whole leg. These execute from one
    /// registry-driven loop in the ceremony, as a bare `UPDATE`.
    Plain,
    /// The leg does strictly more than move the rows — it rewrites other columns
    /// as it moves them, absorbs superseded third-party rows, or is a
    /// delete-then-insert. Hand-written — and still **asserted
    /// present** rather than merely trusted, because
    /// [`a_succession_moves_exactly_the_tables_the_registry_declares`] judges a
    /// `Move` on its *observable* and does not care how the leg is written: a
    /// deleted bespoke leg fails there exactly as a deleted plain one does. The
    /// reason says what the extra work is.
    Bespoke(&'static str),
}

/// What the **per-actor export** (`export_routes.rs`, "export my data") does
/// with a table's rows — the fourth axis, ratified 2026-08-15.
///
/// **Why this is an axis and not a walk.** The export's completeness gap (11
/// shaped domains vs ~150 registered tables) cannot be closed by pointing the
/// export at this registry: the three existing axes answer *deletion*,
/// *binding* and *succession*, and none of them answers **disclosure**. An
/// over-broad *move* is a security regression; an over-broad *export* is a
/// leak that has already left the box. So every table carries an
/// individually-reasoned verdict, and the field is **required** — a new table
/// cannot join the registry without declaring what the export does with it,
/// which makes the omission-by-forgetting failure a compile error.
///
/// **Never derive a verdict** from [`Policy`]/[`Succession`]/the schema.
/// Registry-*driven* emission is fine once verdicts exist — the walk executes
/// declared verdicts, it never infers them. A wrong `Withheld*` is the status
/// quo (the table simply stays out, as every table was before this axis); a
/// wrong `Verbatim`/`Redacted`/`Shaped` is a leak. Bias accordingly, and when
/// unsure leave [`Export::Unreviewed`].
///
/// **The weakest-credential rule.** The export endpoint accepts an *eviction
/// export token* as a fallback credential (`export_routes.rs::handle_export`),
/// so everything a verdict admits must be safe to serve under that weaker
/// credential — and an export must never mint a new resting place for a
/// secret (the zip outlives every session that produced it).
///
/// **Sealed columns ride in at-rest form.** The nest never unseals for
/// export; sealed ciphertext in a `Verbatim`/`Redacted` table is the owner's
/// own data in the form the nest holds it (their client unseals what its keys
/// reach). The S9 dump bound (`export/logical.rs::DUMP_TABLES`) does **not**
/// transfer to this axis — the dump rests unencrypted in the self-backup
/// store, while this export goes to the data's owner over their own channel.
///
/// ⚠ **That clause and [`Export::WithheldSecret`] meet on every wrapped key
/// blob, and the discriminator is WHAT the ciphertext is, never WHETHER it is
/// ciphertext** (ruled while draining the secret plane, 2026-08-15). Sealed
/// *content* rides: it is the user's own data, and the only key that opens it
/// is one their client already holds. Sealed *key material* — a wrapped
/// content/sealing key, an escrow wrap, a wrapped credential — is
/// `WithheldSecret` even though it is ciphertext, because the two halves of
/// the secret do not travel together only by accident: the wrapping key of
/// such a blob is routinely something an eviction-token holder can obtain by
/// another route (a MUA app password, a bridge credential, a device seed), so
/// admitting the ciphertext into a file that outlives every session is exactly
/// the new resting place the weakest-credential rule forbids. The shipped
/// precedent is `recovery_escrow`: a sealed blob, and `WithheldSecret`. The
/// test is not "can the export reader open this?" but "does the export carry a
/// key, under any wrapping, that opens something else?"
#[derive(Debug, Clone, Copy)]
pub enum Export {
    /// All columns join the per-actor export, in at-rest form.
    Verbatim,
    /// Rows join minus the named columns (the dump's `content`-sans-`payload`
    /// precedent). The reason says why each omitted column must not ride.
    Redacted {
        omit: &'static [&'static str],
        reason: &'static str,
    },
    /// The rows already reach the export through the named shaped domain of
    /// `export_routes.rs::gather_export_data` (e.g. `contacts.json`), so the
    /// registry-driven emission deliberately skips the table rather than
    /// exporting the same data twice.
    ///
    /// ⚠ **This verdict asserts TOTAL coverage, and `covers` is what makes the
    /// assertion checkable** (ruled draining the shaped-domain plane,
    /// 2026-08-16). The rule was always *"the reason must account for every
    /// column and row-filter the shaped domain drops — if a dropped piece is
    /// user-meaningful data, this verdict is wrong: extend the shaped domain
    /// or go `Verbatim`"*, but it lived only in prose, so a `Shaped` verdict
    /// was a claim about the columns **of the day it was written** and the
    /// next `ALTER TABLE` silently falsified it with nothing watching. That is
    /// not hypothetical: `feeds` grew `scope`, `contributor_seeds` and
    /// `composition` by ALTER long after its shaped domain was written, and a
    /// reader checking the `CREATE TABLE` alone would never see them.
    ///
    /// So `covers` names every column of the table the domain carries, and
    /// [`tests::every_shaped_verdict_covers_every_column`] walks the *real*
    /// schema and requires `covers` ∪ {the actor column} to be exactly the
    /// table's columns. **A partial shaped domain is therefore no longer
    /// expressible**: a new column reds the guard, and the author chooses
    /// deliberately between extending the domain and flipping the verdict to
    /// `Verbatim`/`Redacted` — which is the choice the prose asked for and
    /// could not enforce.
    Shaped {
        domain: &'static str,
        /// Every column the named domain carries. The actor column is
        /// implicit (it is the exporter themselves) and must not be listed.
        covers: &'static [&'static str],
        reason: &'static str,
    },
    /// Never exported: credential, key, or escrow material. Exporting would
    /// mint a new resting place for a secret and would serve it to the
    /// weakest credential the endpoint accepts.
    WithheldSecret(&'static str),
    /// Never exported: a projection or cache re-derivable from data the
    /// export already carries — exporting adds bytes, not data the user
    /// lacks.
    WithheldDerived(&'static str),
    /// Never exported: nest-internal serving/bookkeeping state *about* the
    /// actor (rate counters, delivery bookkeeping, locks), not the actor's
    /// own data. Meaningless off-box; the reason argues why nothing here is
    /// user-meaningful.
    WithheldOperational(&'static str),
    /// Not yet reviewed for export. **This is the status quo, not a
    /// judgement**: the table stays out of the export exactly as every table
    /// did before this axis existed. The backlog is exact-counted and
    /// down-only (`tests::the_unreviewed_export_backlog_only_shrinks`), and
    /// no new table may join it — a migration's author rules the export
    /// disposition of the table they are creating.
    Unreviewed,
}

/// One table's actor-identifying column, how that column spells the actor, and
/// what deletion, succession, and the per-actor export each do with it.
#[derive(Debug, Clone, Copy)]
pub struct ActorTable {
    pub table: &'static str,
    pub column: &'static str,
    /// How `column` spells the actor id — see [`ActorKey`]. Getting this wrong
    /// is not a type error; it is a delete that quietly matches no rows.
    pub key: ActorKey,
    pub policy: Policy,
    /// What a succession does with these rows — see [`Succession`].
    pub succession: Succession,
    /// What the per-actor export does with these rows — see [`Export`].
    pub export: Export,
}

/// The registry. Alphabetical within each policy group; append, don't
/// reorder, when a migration adds the next per-actor table —
/// `tests::every_actor_shaped_column_is_registered_or_excluded` fails loudly
/// (a table with an `actor`/`author`/`owner`-shaped column absent from this
/// list and not in that test's own `EXCLUDED`) if a new one is added and
/// forgotten here.
pub const ACTOR_TABLES: &[ActorTable] = &[
    // === Purge: this actor's own operational/private-state rows ===
    ActorTable {
        table: "account_aliases",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The account's mail addresses — the canonical `<handle>@<domain>` row
        // mail-enable writes, plus every alias the user made themselves.
        //
        // **This is the entry the whole axis was built for, and it is the one
        // whose absence was a live exposure** (found + closed 2026-08-11):
        // `validate_recipient` resolves bridge AUTH through `account_aliases`
        // and *only* that row on any nest that has claimed a mail domain — the
        // handle→actor fallback beside it is gated on the domain being
        // unregistered, so it never fires where mail is served. A stale row
        // therefore kept pointing the user's own address at the retired actor,
        // and the successor could not clean up behind it:
        // `ensure_canonical_handle_alias` declines to take a localpart another
        // actor owns, and `revoke_wrapped_mls_blob` refuses any target that is
        // not the caller, so the burn deleted the successor's (nonexistent)
        // blobs while the retired actor's survived. The predecessor's password
        // went on authenticating through the ceremony meant to end the theft
        // (`succession-aftermath.md` § Re-key scope → the address paragraph).
        //
        // Plain, and provably so: uniqueness is on `(local_domain, pattern,
        // kind)`, which excludes `actor_id`, so neither path can collide here.
        //
        // ⚠ The recipient seal keys (`actor_mls_pubkeys`,
        // `actor_epoch_seal_keys`) are `Stay` and deliberately do NOT travel
        // with the address: they derive from material the thief read, so
        // carrying them would seal fresh mail to a compromised key. Leaving them
        // behind is what arms the `succession_pending` tempfail — an address
        // resolving to an actor with no registered pubkey — which is reachable
        // only once *this* move has taken the address away from the retired
        // identity in the first place. The two rulings are one mechanism.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the account's mail
        // ADDRESSES: the canonical `<handle>@<domain>` row plus every alias the
        // user made, with the label they gave it, the per-alias controls they
        // set (spam threshold, rate caps, use budget, expiry) and its hit
        // counters. An address is the most portable thing a mail account has,
        // and the succession axis already ruled in as many words that an
        // address IS ownership.
        //
        // `forward_target` is a destination the OWNER chose and their own app
        // displays; unlike `mail_account_settings.forward_all_to` — which the
        // succession axis clears because a moved tap keeps copying a recovered
        // account's mail to a thief — an export hands it back to the person who
        // set it. The axes diverge for the same reason they do at the
        // tombstone logs below: one governs a live account's future, the other a snapshot
        // handed to its owner.
        export: Export::Verbatim,
    },
    // === The membership/participation cluster (ruled 2026-08-14) ===
    //
    // The ratified class rule decides four of the five: *ownership moves in the
    // succession transaction; participation moves by propagation*
    // (`succession-aftermath.md` § Re-key scope, the ownership blockquote —
    // "membership in OTHER owners' sets and groups moves by the ratified
    // propagation paths, never by this transaction"). What made the cluster
    // worth a pass rather than a footnote is that `actor_channels` looks like a
    // counterexample and is not: it is the channel-fetch authorization, so a
    // wrong `Stay` reads like it would strand the successor out of their own
    // conversations. It does not, and the reason is structural — see below.
    ActorTable {
        table: "actor_channels",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-14). Participation, and BOTH error
        // directions were real here, which is unusual on this axis — the
        // asymmetry could not break the tie, so the mechanism did. Three
        // independent reasons, in ascending order of how badly a `Move` ends:
        //
        // 1. The propagation leg that writes this row for the successor already
        //    exists and is the ratified one: the sweep's add-successor Welcome
        //    lands on `conversations_handlers::welcome_deliver`, which calls
        //    `register_actor_channel_gated` for the recipient. Participation
        //    genuinely moves by propagation here rather than in principle.
        // 2. A `Stay` strands nothing the ceremony owns, because this row is
        //    NOT how an owner reaches their own set. Every folder gate —
        //    `folder_authz::{can_read_folder, resolve_readable_folder,
        //    resolve_writable_folder}` — takes an owner fast-path on
        //    `folders.actor_id` BEFORE consulting the roster, and `folders`
        //    moves. The roster check governs exactly one thing: membership in
        //    SOMEONE ELSE'S set. So the ownership half of this table is carried
        //    by a column that already moves, and what is left is participation.
        // 3. ⚠ The decisive one, and it is the `recovery_registrations` shape
        //    one plane over: **a `Move` would break the ceremony's own
        //    federation push.** `succession_push_targets` (db/channels.rs)
        //    finds the peer nests holding residue by reading `actor_channels
        //    WHERE actor_id = ?old` — and `push_succession_detached` runs it
        //    AFTER the transaction commits (recovery_handlers.rs). Move the
        //    rows and that read returns nothing, the target set is empty, the
        //    push returns early, and NO PEER NEST EVER LEARNS THE SUCCESSION
        //    HAPPENED. The table the propagation reads to find its audience
        //    cannot be re-pointed by the transaction it is announcing.
        //
        // What the same leg does to the RETIRED seat (`writer-signed-change-
        // records.md` ruling (8)(j)(2), 2026-10-02): the claimant's Welcome
        // that seats the successor on a folder channel deletes the
        // predecessor's row here and carries its `folder_member_access` grant,
        // in one write (`register_successor_carrying_seat`). Reason 3 survives
        // it because the handler reads the push targets before it replies
        // (`succession_push_targets_for_reply`), so the read precedes every
        // such Welcome. `Stay` is unchanged: it governs the transaction.
        //
        // The security direction agrees: a `Move` hands channel-fetch
        // authorization to a credential the group has not accepted, front-
        // running the very propagation the ratified rule assigns the job to.
        succession: Succession::Stay(
            "PARTICIPATION, not ownership: this is the channel-fetch roster for OTHER \
             owners' groups and sets — the owner's own reach is the `folders.actor_id` \
             fast-path in `folder_authz`, which moves. The successor is registered here \
             by the ratified propagation leg (the sweep's add-successor Welcome calls \
             `register_actor_channel_gated`), never by this transaction, which would \
             front-run the group's acceptance; on a claimed folder channel that same \
             Welcome retires the predecessor's seat and carries its grant \
             (`register_successor_carrying_seat`, ruling (8)(j)(2)). ⚠ AND a move would \
             disable the propagation itself: `succession_push_targets` reads this table \
             under the RETIRED id, after the transaction commits and before the \
             ceremony replies, to find the peers to push the statement to — moved rows \
             mean an empty target set and no peer ever learns the succession happened",
        ),
        // EXPORT RULING (the sync plane). The exporter's
        // own channel-membership roster, and the plane's easiest call for the
        // `restore_history` reason: the nest **already answers this question to
        // this actor** over an ordinary door —
        // `fauna.conversations.channel.list_for_actor` reads exactly
        // `SELECT channel_id FROM actor_channels WHERE actor_id = ?` and hands
        // back the hex ids (`conversations_handlers.rs`). An archive quieter
        // than a door the owner can already call is the wrong direction.
        //
        // Three columns and no third party in any of them: `actor_id` is the
        // exporter, `channel_id` is a channel they are on, `created_at` is when
        // they joined it. The inverse read — *which actors sit on one channel* —
        // is a different door (`fauna.conversations.channel.actors`) and a
        // different key; an actor-scoped read here can never return it.
        //
        // ⚠ Do NOT read the succession `Stay` above across to this axis. That
        // ruling says the row's *authority* must not front-run the group's
        // acceptance, which is a question about who may act. Disclosure asks
        // whose data it is, and participation in someone else's set is still
        // the exporter's own record of where they participate.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "actor_epoch_seal_keys",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "COMPROMISE, not history: the per-epoch seal keys derive from material the \
             thief read, so carrying them would seal fresh mail to a compromised key. \
             Leaving them behind is load-bearing — it is what arms the \
             `succession_pending` tempfail",
        ),
        // The name is the trap and the schema is the answer — the exact
        // inverse of the sealed-key-material case above. The table holds
        // `mls_pubkey` and `mlkem_ek`: the PUBLIC halves of a horizon of
        // future weekly sealing epochs, which the owner's own client
        // pre-publishes so the MTA can epoch-seal inbound mail with no client
        // online. The migration says it in as many words — *"Public keys only
        // — floor-safe; nothing here opens content"* (`migrations.rs:3096`) —
        // and an ML-KEM `ek` is by construction the encapsulation half.
        //
        // The `Succession::Stay` beside this is about the PRIVATE halves the
        // owner derives from the seed, which this table has never held; it is
        // not evidence for withholding what the table does hold. Nothing here
        // opens anything, so nothing here is a secret, and it is the owner's
        // own published schedule.
        export: Export::Verbatim,
    },
    // The perimeter's index-hint sealing key. Its own migration comment names it
    // "sibling of actor_mls_pubkeys", and the sibling relationship is the whole
    // ruling: same schema block, same bridge-class fetch at the perimeter
    // (`fauna.bridges.fetch_recipient_index_key`), same role — a PUBLIC half the
    // MTA seals *future inbound* mail to. That is exactly the class whose Stay is
    // load-bearing rather than incidental, so it takes its sibling's verdict.
    //
    // ⚠ Ruled CONDITIONAL on the table being DARK today, the
    // `admin_actor_ids.added_by` precedent: provisioning is a Phase E concern
    // with no production RPC, so nothing writes this row outside tests and the
    // private half's provenance cannot be read off any producer. `Stay` is
    // therefore the safe direction rather than a measured one. When the Phase E
    // provisioning kind lands, re-check whether the private half derives from the
    // MSEK a seed thief holds: if it does, this Stay is load-bearing exactly as
    // `actor_mls_pubkeys`' is; if it does not, this entry must be re-ruled.
    ActorTable {
        table: "actor_index_pubkeys",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "COMPROMISE, not history, on its sibling's reasoning: the index-hint \
             seal key is the recipient-key class that seals FUTURE INBOUND mail at \
             the perimeter, so carrying it would seal fresh hints to a key derived \
             from what the thief read. Conditional on the table being dark today — \
             re-check the private half's derivation when its provisioning lands",
        ),
        // EXPORT RULING (the identity/lifecycle plane).
        // The PUBLIC half, on `actor_mls_pubkeys`' reasoning below and the
        // `actor_epoch_seal_keys.mls_pubkey` clearance beside it: this is the
        // key the MTA bridge FETCHES (`fauna.bridges.fetch_recipient_index_
        // key`) in order to seal to this recipient, so it is published
        // material by function. The private half is the client's and has never
        // rested here.
        //
        // ⚠ Do NOT read the succession `Stay` across, and this plane is where
        // that mistake is most inviting: `Stay` is there because the key seals
        // FUTURE INBOUND mail and a seed thief can derive the private half —
        // a question about who should keep RECEIVING under it. Disclosure asks
        // whether the stored bytes are secret, and they are the published half.
        // The two axes disagree here on purpose.
        export: Export::Verbatim,
    },
    // Derived sign-in telemetry, and its ONE consumer is what rules it: the row
    // is never read back by anything (no getter exists) — `update_actor_last_ip`
    // compares it to the address of the sign-in in hand and returns *changed?*,
    // which arms the new-location security notification.
    //
    // So moving it would make that alert lie in BOTH directions at once: the
    // successor's first honest sign-in, compared against the address the THIEF
    // last used, reads as an intrusion — and a thief signing in from that same
    // address would have read as routine. A fresh identity legitimately starts
    // with no baseline: the first insert returns `false` by construction, so
    // leaving the row behind is what makes the successor's first sign-in silent.
    // The classification rule reaches the same answer from the other end — this
    // records where the RETIRED identity was last seen, which is history.
    ActorTable {
        table: "actor_last_ip",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "HISTORY, and moving it would make the new-location alert lie both \
             ways: the row's only consumer compares it to fire that alert, so \
             carrying the thief's last address makes the successor's first honest \
             sign-in look like an intrusion, and would have made the thief's own \
             re-use look routine. A new identity starts with no baseline on purpose",
        ),
        // EXPORT RULING (the account-record plane). Three
        // columns, and every one is about the exporter and nobody else:
        // `ip_address` is the address THIS actor last signed in from, with
        // `updated_at`. It is personal data in the strict sense, which cuts
        // toward exporting rather than away — the archive goes to the person
        // it describes, and "what does my nest know about me?" is exactly the
        // question `principles.md` § The user always controls their data says
        // an export must answer.
        //
        // Not operational despite its single-row, single-consumer shape: the
        // succession axis calls it HISTORY in as many words, and it is the
        // baseline behind the new-location sign-in alert — a security record
        // whose owner is the only person who can judge whether a listed
        // address was really theirs.
        export: Export::Verbatim,
    },
    // Whether this account serves mail at all — the enablement the address
    // rows in `account_aliases` route *into*. Left behind, the successor's
    // mailbox is switched off by the ceremony that was meant to rescue it,
    // and the app renders the toggle off, so nothing contradicts it.
    ActorTable {
        table: "actor_mail_serving",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — one user-set boolean:
        // whether this actor's mail is served to MUAs from this nest, set
        // through `fauna.bridges.set_mail_serving_enabled` (User-class,
        // caller-scoped). A switch the user flipped is the user's data. Its
        // absence means ON, so the archive carrying only the rows they actually
        // set is the honest shape — the same argument as `bridge_search_policy`.
        export: Export::Verbatim,
    },
    // The mail-import dedup ledger: one row per (actor, dedup key) the account
    // has already ingested, consulted by `mail_import` before persisting so a
    // re-import of mail the delivery path already landed is a no-op. It moves
    // with the corpus it describes — the mailbox rows, their expunge state and
    // their scan records all move — because the ledger's whole job is to agree
    // with that corpus. Left behind it silently disagrees: the successor's first
    // re-import finds no dedup hit and re-inserts mail they already hold, so the
    // ceremony's visible effect is a mailbox full of duplicates.
    //
    // Collides per-row on `PRIMARY KEY (actor_id, dedup_key)`, which the plain
    // leg's `UPDATE OR IGNORE` parks rather than aborting — the correct arm here,
    // since a key both identities already ingested is a key the terminal has its
    // own row for.
    ActorTable {
        table: "actor_message_dedup",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the account-record plane).
        // Delivery de-duplication memory: `(dedup_key, message_uri)`, the
        // nest's record of what it has already delivered so a redelivery is
        // suppressed. Nest-internal bookkeeping ABOUT the actor rather than the
        // actor's own data — the archetype `WithheldOperational` names — and
        // meaningless off the box that does the delivering: `message_uri`
        // points at content the archive carries on its own terms (`content`,
        // already ruled `Verbatim`), so nothing the owner has is missing.
        export: Export::WithheldOperational(
            "delivery de-duplication memory -- the nest's own record of what it has already \
             delivered so a redelivery is suppressed. Bookkeeping about the actor, not the \
             actor's data, and inert off the box that delivers; the content the `message_uri` \
             points at reaches the archive through `content` on its own verdict",
        ),
    },
    ActorTable {
        table: "actor_mls_pubkeys",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "COMPROMISE, not history: the recipient seal keys derive from the MSEK the \
             thief holds, and the successor re-registers its own. Do NOT 'finish the \
             job' by deleting these — the tempfail depends on the address resolving to \
             an actor with no registered pubkey",
        ),
        // EXPORT RULING (the identity/lifecycle plane).
        // Two published halves and a timestamp. `mls_pubkey` is the X25519
        // public encryption key the MTA bridge fetches
        // (`fauna.bridges.fetch_recipient_mls_pubkey`) to seal inbound mail at
        // the perimeter, and `mlkem_ek` is the 1184-byte ML-KEM-768
        // ENCAPSULATION key — the public half, exactly the
        // `subscribers.mlkem_encaps_key` clearance ruled elsewhere in this file,
        // one plane over, where the decapsulation half is the secret one and the naming
        // is the trap. Both are provisioned by the owner's own client and
        // exist to be handed out; the private halves are MSEK-derived and
        // client-held, and have never rested in this table.
        //
        // ⚠ The succession `Stay` beside this says "COMPROMISE, not history",
        // which reads like a withholding argument and is not one. It answers
        // whether the SUCCESSOR should keep receiving mail sealed to a key a
        // seed thief can derive — a question about future capability, not
        // about whether these bytes are secret. They are not: the bridge asks
        // for them by RPC. The plane's general form of this point recurs
        // elsewhere on this plane — a `Stay` reasoned from compromise says nothing about
        // disclosure, and three of this plane's six entries carry one.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "admin_actor_ids",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Adminship moves — but as DELETE-then-INSERT rather than an UPDATE, so
        // the row identity is not preserved even though the authority is. The
        // observable is what this axis declares, and the observable is a move.
        succession: Succession::Move(MoveShape::Bespoke(
            "delete-then-insert, not an UPDATE: the row carries `added_at`, and an `INSERT OR IGNORE` after the DELETE is what makes the successor's grant date its own rather than inheriting the retired identity's. Also conditional -- it runs only when the predecessor actually held the role.",
        )),
        // EXPORT RULING (the identity/lifecycle plane).
        // The exporter's own adminship: `actor_id` is the admin, `added_at`
        // and `role` are the grant, and this is the "governance record" half
        // of what the prior escalation ruled `audit_log` rides for the same
        // person.
        //
        // ⚠ `added_by` (added by the admin-roles ALTER, so a reader of the
        // CREATE TABLE alone will not see it — the same `feeds` trap noted
        // elsewhere in this file)
        // names ANOTHER admin: the party who granted the role. It rides, on
        // the escalation's own `pending_actions.cancelled_by` precedent — an
        // act performed UPON my row, recorded precisely to be legible, and
        // legible must include the export, since an evicted or succeeded admin
        // may have no live session left to read it from. The counterparty is
        // also already recorded in `audit_log` keyed to themselves, so nothing
        // here is a channel that surface does not already open.
        export: Export::Verbatim,
    },
    // ─── The ActivityPub plane, ruled 2026-08-15 ──────────────────
    //
    // The whole family MOVES, and the reason is one sentence: the AP identity
    // is DERIVED FROM THE HANDLE, and the handle moves with the account. The
    // username is minted from it at enablement and frozen
    // (`activitypub/bridge_provider.rs`), the actor URL is
    // `https://{domain}/ap/users/{username}`, and both the WebFinger door and
    // the per-user inbox resolve an arrival by that username. So this plane's
    // external destination is DEPLOYMENT-FIXED, not attacker-chosen — the
    // `spam_preferences` side of that test, where `bluesky_accounts` below is
    // the `forward_all_to` side. A thief who merely flipped `enabled` hands the
    // successor a presence one owner-scoped toggle away.
    //
    // ⚠ And the key material argument runs the OPPOSITE way to
    // `actor_mls_pubkeys`: the RSA privkey is sealed under the **nest** KEK
    // (`nest_kek::ACTIVITYPUB_RSA_CONTEXT`), never under the user seed, so a
    // seed thief never read it. There is nothing here to leave behind as
    // compromised.
    ActorTable {
        table: "ap_accounts",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the feature-gated bridge plane).
        // The owner's own ActivityPub identity: `username` and `actor_url` are
        // the presence remotes already resolve them by, `enabled`/`backfill`/
        // `auto_accept_follows`/`default_visibility` are their own settings,
        // and every one of them is a thing they set and would want back.
        //
        // ⚠ `encrypted_privkey` is the ONE column that cannot ride, and the
        // discriminator is the enum's, not the wrapping: it is the RSA private
        // key that signs HTTP Signatures AS this actor, so the export would
        // carry a key that opens something else -- the whole federated
        // identity, to anyone holding the eviction token. That it is sealed
        // under the NEST KEK (`nest_kek::ACTIVITYPUB_RSA_CONTEXT`) rather than
        // the user seed does not change the class; *what the ciphertext IS*
        // decides, never whether it is ciphertext.
        //
        // ⚠ The succession axis reasons the OPPOSITE way about that same
        // column and is right to -- the plane MOVES, and the module comment
        // above says a seed thief never read the privkey, so there is nothing
        // to leave behind as compromised. That is a statement about
        // COMPROMISE; this axis asks about DISCLOSURE, and the same sealed key
        // that is safe to carry forward is unsafe to write into a zip. The
        // same identity/lifecycle-plane trap, met head-on inside a single table.
        //
        // `public_key_pem` rides and is cleared: `GET /ap/users/{username}`
        // serves it unauthenticated to the entire fediverse
        // (`activitypub/actor_routes.rs`), because HTTP Signatures cannot be
        // verified without it.
        export: Export::Redacted {
            omit: &["encrypted_privkey"],
            reason: "the owner's own AP identity and settings ride -- username, actor URL, the \
                     enable/backfill/auto-accept/visibility toggles they set. `encrypted_privkey` \
                     is the RSA key that signs HTTP Signatures AS this actor: a key that opens \
                     something else, which the weakest-credential rule forbids however it is \
                     wrapped. Sealed under the nest KEK, not the user seed -- which is why the \
                     succession axis carries it forward untroubled; that reasons about \
                     compromise, this about disclosure. `public_key_pem` rides because the AP \
                     actor endpoint already serves it to the world unauthenticated.",
        },
    },
    ActorTable {
        table: "ap_follows",
        column: "local_actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. The owner's own follow graph,
        // both directions: the column is named `local_actor_id` and the
        // writers honour it -- `insert_follow` takes the local actor for an
        // outbound Follow and for an inbound one alike, so a row exists
        // because THIS actor follows or is followed, never because two
        // strangers do. No wrong-party question survives that.
        //
        // `remote_actor_uri` is the counterparty, the `contacts.peer_id` /
        // `notifications.sender_id` class that has ridden unchanged since it
        // was first ruled -- and here it is weaker still, because an AP followers /
        // following collection is served publicly by protocol. `state` and
        // `follow_activity_id` are the edge's own pending/accepted bookkeeping
        // and the activity URI that carries it; both are the owner's record of
        // a relationship they can see in their app today.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "ap_post_map",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. The owner's own post ->
        // AP-URL witness: which of their posts were pushed to the fediverse and
        // under what URL, plus the `tombstoned` flag recording a retraction.
        //
        // ⚠ This table holds rows for OTHER authors and the wrong-party check
        // still passes, which is worth stating because the reflex goes the
        // other way. Inbound Notes are mapped too, but keyed on
        // `synthetic_actor_id(actor_uri)` (`inbox_routes.rs`) -- a BLAKE3
        // derivation of the remote's URI, an id nobody holds a key for. So a
        // real exporter's `actor_id = ?` match returns exactly the rows they
        // authored, and the synthetic rows belong to actors that can never
        // authenticate to the export endpoint at all. The column is the AUTHOR
        // and the author, for anyone who can export, is themselves.
        //
        // Consequence for the reader: `remote_actor_uri` (the ALTER-added
        // column, so a reader of the CREATE TABLE alone will miss it -- the
        // same `feeds` trap noted elsewhere in this file) is NULL on every row an exporter
        // matches; it is populated only on those synthetic-author inbound rows.
        export: Export::Verbatim,
    },
    // ─── The hosted ATProto (PDS) plane, ruled 2026-08-12 ──────────────────
    //
    // **The plane's own authority is what makes it different from every other
    // family here, and it is why the whole plane needed one pass rather than
    // twelve independent ones.** Every other table this registry rules is
    // reached by authenticating as the actor — so "the old key is refused
    // everywhere" already ends the thief's reach, and the ruling only decides
    // where the rows live. The PDS does **not** authenticate that way: an app
    // password (`atproto_app_credentials`) and an OAuth grant
    // (`atproto_oauth_grants`) are their own credentials, minted at the user's
    // gesture and honoured on their own terms. A seed thief could mint either
    // before the ceremony, and neither the succession's refusal plane nor the
    // MSEK burn touches them. That is the `account_aliases` shape exactly — a
    // second authentication plane keyed on something other than the Fauna
    // identity — which is why the authority half of this plane burns rather
    // than moves.
    //
    // The identity half moves for the mirror-image reason. Burning it would
    // take **nothing** from the thief: the DID lives at the PLC directory, and
    // its senior rotation key is client-held in the account plane's `fauna.state.atproto-identity` rows
    // (`atproto-identity-custody.md` § Key custody), which the thief read with
    // the seed and keeps regardless. All a burn would do is blind the
    // successor — leaving a live DID this box hosts with no app able to see,
    // manage, contest or retire it, the `web_domains` stranding was ruled a
    // `nest/common.md` § Client-state recoverability breach.
    ActorTable {
        table: "atproto_account_settings",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The depth selector's stored intent (`integration_level`) and the
        // external-apps kill switch, one row per account. Moves **with the
        // identity row it governs**: split from it, the successor gets a
        // hosted identity whose settings row is absent, which reads as the
        // `'off'` default — an identity that is active and un-served at once,
        // a half-state neither the bridge nor the app was built to see.
        //
        // A thief could have raised the level, and that is deliberately not a
        // burn: the destination is the **public ATProto network**, not a box
        // the thief chose (the `spam_preferences` test — thief-writable
        // is not sufficient, attacker-chosen destination is what earns a
        // bespoke leg). A raised level is a consent regression the successor
        // sees on their own AT Protocol page and steps down in one gesture, and
        // the step-down is explicitly reversible-by-design.
        succession: Succession::Move(MoveShape::Plain),
        // The depth selector's stored intent and the external-apps kill switch
        // — the owner's own consent posture, rendered on their own AT Protocol page
        // and changed there in one gesture. Four columns and no authority in
        // any of them: `integration_level` and `external_apps_enabled` say what
        // the owner CHOSE, never what anything may do on their behalf (the
        // standing authority is `atproto_oauth_grants`, one plane over).
        //
        // Withholding would invert `principles.md` § The user always controls
        // their data: this row IS the record of a consent decision, and an
        // archive carrying the hosted identity while dropping the level it is
        // hosted at describes an account whose own settings are missing.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "atproto_app_credentials",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "app passwords for the hosted PDS — standing authority on a plane \
             that does NOT authenticate with the Fauna identity key, so the \
             ceremony's refusal never reaches them. A thief-minted credential \
             goes on authenticating to the successor's own repo after the \
             ceremony meant to end the theft: the `account_aliases` shape, one \
             plane over. Burns with `atproto_sessions`, never without it — \
             `revoke_atproto_app_credential` deletes the credential and revokes \
             its sessions in one transaction, so a credential burned alone \
             would leave exactly the live-session-without-a-credential state \
             that function exists to make unrepresentable. Re-minting is one \
             gesture on the connected-apps surface, which is what makes the \
             burn bounded and visible",
        ),
        export: Export::WithheldSecret(
            "app passwords for the hosted PDS (`verifier` is the credential's \
             auth material — its own succession reason above calls the rows \
             standing authority). A credential must never gain a second \
             resting place, least of all one servable to the eviction-token \
             fallback credential",
        ),
    },
    ActorTable {
        table: "atproto_authoring_keys",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the D10 delegated authoring sub-key K and its identity-signed \
             cert. RATIFIED as a burn since the delegation shipped and never \
             implemented until now — `atproto-pds-full.md` § D10's revocation \
             bullet says in as many words that *at identity succession the \
             delegation dies with the old identity* (future-authority class), \
             and it cited a `succession-aftermath.md` row that the 2026-08-12 \
             registry conversion removed, so the claim had no carrier at all. \
             The cert authorizes K against the PREDECESSOR's identity key — the \
             key the thief read — so carrying it would let this box keep \
             authoring ATProto content under authority the successor never \
             granted. Recreatable by construction: re-enabling a hosted level \
             mints a fresh K and provisions a fresh cert, and already-published \
             posts stay verifiable from their embedded cert",
        ),
        export: Export::WithheldSecret(
            "`k_secret_wrapped` is the D10 delegated authoring sub-key -- a \
             signing secret, wrapped, which the sealed-columns clause does not \
             reach (see the discriminator on this enum: the ciphertext IS a \
             key). Its succession reason above already calls the cert standing \
             authority to author ATProto content; a file that outlives the \
             session must not carry the key that authority rests on. \
             `nostr_accounts` is the same ruling on the neighbouring bridge",
        ),
    },
    ActorTable {
        table: "atproto_blobs",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Uploaded media referenced by the repo's records. Ownership of resting
        // data, and it belongs to the DID: it moves with the identity family or
        // parks with it.
        succession: Succession::Move(MoveShape::Plain),
        // The owner's own media ledger: `cid` is the ATProto blob address,
        // `media_ref` the 32-byte blake3 content hash of the same bytes in this
        // box's store (`bridge_atproto_handlers.rs` refuses any other length).
        // A content address is not a capability — the bytes sit behind the same
        // actor-scoped door with or without this row — and the mapping is not
        // re-derivable from the repo, which names blobs by their ATProto CID
        // alone. Without it the archive's records point at media the owner
        // cannot match to anything.
        //
        // ⚠ The sweeper calls an *unreferenced* row "transient upload state"
        // (`atproto_blob_sweeper.rs`), and that does NOT demote the verdict: it
        // licenses the seven-day sweep of rows no record ever named, and says
        // nothing about what a live row is. A referenced row is the durable pin
        // for media the owner's own records embed.
        //
        // ⚠ This rules the ROW, not the BYTES. Exactly like `content`'s
        // offloaded half, ruled elsewhere in this file, the row carries a reference; whether the
        // export reaches the blob/segment store at all is the still-open
        // `segment_records` decision, whose precedent is `include_blobs`.
        // Nothing here pre-empts it.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "atproto_consent_requests",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "pending OAuth consent questions — a request resolved after the \
             ceremony would mint a grant on the plane the ceremony just burned, \
             which is the one thing this pass must not leave reachable. Nothing \
             precious dies: the table's own C6 comment calls the row a \
             minutes-long question, recreatable by retrying the flow. A row \
             whose `actor_id` is NULL names no account (a PAR with no \
             `login_hint`) and is untouched — it is bound to whoever resolves \
             it, and after the ceremony that can only be a live identity",
        ),
        // ⚠ The CEREMONY, not the authority. An earlier ruling left the test —
        // *is the neighbouring reason about the BYTES or about the CEREMONY?* —
        // and this is the first table that IS one. What the owner durably
        // agreed to lives one table over as an `atproto_oauth_grants` row,
        // which is `Verbatim` precisely so the connected-apps audit
        // `principles.md` requires reaches the archive. This row is only the
        // question that minted it.
        //
        // And the question does not survive to be exported:
        // `sweep_expired_atproto_consent_requests` is `DELETE ... WHERE
        // expires_at <= now` with no carve-out for a resolved row, and its own
        // docstring reads "Not user data: a consent request is a minutes-long
        // question". Approved and denied rows alike are gone minutes after the
        // ceremony; an export can only ever catch one mid-flight.
        //
        // ⚠ A DENIAL therefore leaves no durable trace anywhere. That is a
        // property of the sweep, not of this verdict — ruling `Verbatim` would
        // not recover it, since an export cannot carry what the box already
        // deleted. Recorded so a later session reads it as the known shape of
        // the plane rather than a hole this ruling opened.
        //
        // ⚠ And part of the population is unmatchable regardless: `actor_id` is
        // NULLABLE by design (a PAR carrying no `login_hint`) while the walk
        // binds `actor_id = ?`. Resolution stamps the approver's id, so only
        // pending unbound rows are NULL — the sync-plane vacuity check, and the
        // reason the class had to be argued rather than read off the column.
        export: Export::WithheldOperational(
            "pending OAuth consent questions -- the ceremony, whose durable \
             outcome is an `atproto_oauth_grants` row and exports there. Swept \
             by expiry however it was answered (the sweep's own docstring calls \
             the row a minutes-long question, recreatable by retrying the \
             flow), so nothing here is the owner's durable data",
        ),
    },
    ActorTable {
        table: "atproto_identities",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The account's hosted DID — the head of this plane's move family, and
        // the row the rest of the family belongs to. Moves because the DID is
        // the account's public reach and because burning it would take nothing
        // from the thief (see the family comment above): the successor keeps the
        // senior rotation key through the account plane, so it is the successor, not
        // this box, who can still contest or retire the DID at the directory —
        // but only if an app can still see it.
        succession: Succession::Move(MoveShape::Plain),
        // The account's hosted DID and the PUBLISHED halves of its keys.
        // `user_rotation_pub`, `signing_pub` and `bridge_rotation_pub` are
        // exactly what the PLC directory serves to anyone who resolves the DID;
        // the secret halves are `atproto_identity_key_blobs` (WithheldSecret)
        // and the client-held senior rotation key, neither of them here.
        //
        // ⚠ The identity/lifecycle-plane trap, second sighting: the succession ruling directly
        // above reasons from COMPROMISE — it moves because burning would take
        // nothing from a seed thief — and a compromise argument says nothing
        // about DISCLOSURE. Read the columns instead, and they are the
        // account's public reach plus its own settings (`status`,
        // `history_backfill`, `projection_floor_micros`,
        // `tombstone_requested`). Withholding would hide the owner's own DID
        // from their own archive.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "atproto_identity_key_blobs",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The bridge-custodied sealed key blob for the DID above. **Not the
        // `actor_mls_pubkeys` class, and the difference is the sealing key:**
        // these are sealed to the atproto.pds bridge's attested x25519 and
        // minted nest-side, so no part of them derives from the seed the thief
        // read — carrying them re-seals nothing to a compromised key. Meaningless
        // without its identity row and vice versa, so the two move or park
        // together.
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldSecret(
            "the DID's signing key, sealed to the atproto.pds bridge's attested \
             x25519 (the succession comment above). Sealed key material, not \
             sealed content -- the enum's discriminator -- and the wrapping key \
             lives on a bridge this box also hosts, so the two halves are one \
             compromise apart. The repo it signs for exports under \
             `atproto_native_records`; the key that authors into it does not",
        ),
    },
    ActorTable {
        table: "atproto_native_records",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The repo itself — the user's ATProto content, tombstoned via
        // `deleted_at` and never DELETEd, because re-derivability of the repo
        // needs the tombstones. Belongs to the DID exactly as `nostr_follows`
        // belongs to the npub, which is why it parks with the identity rather
        // than moving onto a successor holding a different DID.
        succession: Succession::Move(MoveShape::Plain),
        // The repo itself — the owner's ATProto content, inline in `record` and
        // tombstoned via `deleted_at` rather than deleted. This is the plane's
        // whole point: a PDS repo is the user's published work, and an archive
        // carrying their identity, settings and media ledger but not their
        // posts would be an export of everything except the data.
        //
        // Already PRESUMED by a landed neighbour, and now true rather than
        // presumed: `atproto_identity_key_blobs`' WithheldSecret reason draws
        // its own line as "the repo it signs for exports under
        // `atproto_native_records`; the key that authors into it does not".
        //
        // ⚠ `rkey` is the ATProto record key — a path segment (`3jzfci…`), the
        // "key" in the REST sense — so it trips the secret-shaped column guard
        // on the fragment alone. Cleared in `CLEARED_EXPORTING_COLUMNS`.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "atproto_oauth_grants",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "standing OAuth authority granted to external client apps — the \
             bunker class verbatim (`nostr_zap_signers`' reason applies word for \
             word), on the same non-Fauna-key plane as the app credentials \
             above. Burns together with `atproto_sessions`: `grant_id` IS the \
             session family id, and `record_atproto_oauth_grant` writes both \
             rows in one transaction precisely because neither is meaningful \
             alone — burning one would leave the connected-apps surface showing \
             a capability the user can see and cannot revoke, which \
             `principles.md`'s *audited from the user's app* forbids",
        ),
        // The connected-apps ledger, and the bearer material it describes lives
        // one table over. `client_id`, `client_name`, `scopes`, `sets` and the
        // four timestamps say WHICH external app holds WHAT authority and until
        // when; the access/refresh tokens and the rotating `current_refresh_jti`
        // are columns of `atproto_sessions`, which is `WithheldSecret`.
        // `dpop_jkt` is a JWK thumbprint of the client's DPoP public key — a
        // fingerprint of a public key, not a credential.
        //
        // The succession ruling above cites `principles.md`'s *audited from the
        // user's app* to refuse leaving a capability the user can see and cannot
        // revoke. The same clause reads the same way here: withholding this
        // would make the export silent about every app holding standing
        // authority over the account, which is precisely the audit the
        // invariant names. Nothing in the row opens anything.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "third_party_principals",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the connected-apps roster row one approved client document holds on \
             this account (`third-party.md` § The principal model) — the other half \
             of `atproto_oauth_grants`' row, minted in the same transaction, so it \
             burns with it for the same reason: standing authority granted to an \
             external app is the account's consent, not the successor's, and a \
             principal whose grants burned while its row moved would show the \
             successor a connection nothing backs",
        ),
        // The roster: which external app, attesting which public key, runs in
        // which form, with which scopes, since when. `holder_x25519` and
        // `publisher_key` are PUBLIC keys; `principal_id` is a nest-minted
        // handle. The account's own audit of who holds reach over it — the
        // same *audited from the user's app* reading `atproto_oauth_grants`'
        // export takes, one table over.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "atproto_preferences",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The account's own ATProto preferences blob. User data, one row per
        // account, no authority in it; moves with the identity family.
        succession: Succession::Move(MoveShape::Plain),
        // The same blob, read the same way for this axis: an opaque preferences
        // payload written straight through from the owner's own app
        // (`putPreferences`, D2 — the nest never parses it, so it holds no
        // structure this box could withhold selectively even if it wanted to).
        // Their data, and nothing in it opens anything.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "atproto_retired_identities",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The append-only record of DIDs this account permanently retired.
        //
        // **First written as `Stay` and corrected before landing** — the *stay*
        // reading is that these are acts the old key performed, so they belong
        // with the signed history. What refutes it is that nothing here is an
        // authority or an attribution to a counterparty: it is the account's own
        // provenance, read back per-actor by `list_retired_atproto_identities`
        // and rendered in the user's own app, and its schema comment states it
        // is user-irrecoverable and re-derivable from nothing (a retired DID no
        // longer resolves anywhere). Left behind it is unreachable from any app,
        // which is a loss of user data rather than a preservation of history.
        //
        // Unlike its siblings it is deliberately NOT in the coupled parking
        // family: `actor_id` carries no unique constraint, the rows are
        // independent facts about DIDs that are already dead, and the union of
        // two retirement histories under one account is the correct merge — the
        // account is the same human either way.
        succession: Succession::Move(MoveShape::Plain),
        // Its own schema comment settles this one: never DELETEd, never
        // updated, user-irrecoverable and re-derivable from nothing, because a
        // retired DID no longer resolves anywhere. It is read back per-actor by
        // `list_retired_atproto_identities` and rendered in the owner's own app,
        // so withholding it would drop from the archive the ONLY surviving
        // record of an identity they published — the same loss the succession
        // ruling above refused when it declined to leave the rows behind.
        //
        // Same published-halves reading as `atproto_identities`: the three
        // `*_pub` columns are what the directory served while the DID lived.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "atproto_sessions",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "live refresh-token families for the PDS, one row per session, and \
             the join partner of BOTH burns above (`credential_id` to the app \
             credentials, `session_id = grant_id` to the OAuth grants). Burned \
             rather than revoked-in-place because a `revoked_at` stamp is a \
             state the successor's surfaces would still render and the \
             predecessor's rows are not theirs to render; and burned rather \
             than moved because a session is bearer authority minted before the \
             ceremony. The deletion is what `refresh_atproto_session` already \
             treats as terminal — a family whose row is gone answers `Invalid`, \
             so no in-flight refresh survives the ceremony. The table's own \
             durability class agrees: sessions are re-derivable/expirable, \
             registered so revocation is visible",
        ),
        export: Export::WithheldSecret(
            "live refresh-token families — `session_id`/`current_refresh_jti` \
             are bearer authority (presenting a jti IS the refresh \
             credential, per the rotate-on-use family-kill design). Bearer \
             material never rides an export",
        ),
    },
    // The per-sender "already auto-replied recently" ledger. It follows the
    // mailbox because it is what stops a vacation responder answering the same
    // correspondent twice — left behind, every sender gets a duplicate reply
    // once, which is the account's outbound behaviour, not the predecessor's.
    ActorTable {
        table: "auto_reply_log",
        column: "recipient_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — a dedup memory, not a
        // record: one row per (recipient, sender_hash) saying an auto-reply
        // already went out, so the MTA does not answer the same correspondent
        // twice inside the window. `sender_hash` is a hash, so the row cannot
        // even name who was replied to — which is what settles it: a plane
        // whose only content is "we already did this once" carries nothing the
        // owner could read. The rule that produced the replies rides
        // `email_filters`.
        export: Export::WithheldOperational(
            "the auto-reply dedup memory -- (recipient, sender_hash, last_sent_at) rows whose              only function is stopping the MTA answering one correspondent twice in a              window. The sender is a HASH, so the row names nobody, and nothing about the              owner's mail is reconstructible from it; the rule that sends the replies is              theirs and rides `email_filters`",
        ),
    },
    ActorTable {
        table: "backup_custodian_checkins",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a projection of what a custodian device holds of the PREDECESSOR-sealed \
             corpus, hanging off a `backup_destinations` row that burns beside it. \
             Carrying it would report a custodian caught up on bytes the successor has \
             neither re-sealed nor re-pushed — fail-silent — where its absence renders \
             `never caught up` until the device's next check-in rebuilds it, which is \
             one pull pass away and fail-visible",
        ),
        // EXPORT RULING (the backup/custody plane). Its
        // migration answers the secrecy half outright — "Nothing here is
        // secret: it is the owner's own device reporting its own progress, so
        // the row rests plaintext like `backup_destinations` itself" — and the
        // product answers the meaningfulness half: this is what backs the
        // custodian's *last synced* on the Backups page, with `caught_up_at`
        // deliberately separate from `checked_in_at` so a permanently-lagging
        // custodian cannot render as freshly synced. `audit_state` /
        // `last_audit_passed_at` are that custodian's audit record, which is
        // precisely what an owner wants to keep about a device holding their
        // backup. Same `Burn`-is-not-`Derived` reasoning as
        // `backup_destinations` above: the input is the device's own report,
        // and no device is in the archive.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "backup_custody",
        column: "uploader_actor",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the backup/custody plane). The
        // INDEX of what this actor has backed up, keyed on `uploader_actor`,
        // "the authenticated bearer, never taken from the wire" (its own
        // migration) — so an actor-scoped read returns this actor's own
        // uploads and never the held-for-friends rows a foreign uploader wrote
        // to the same nest. It is genuinely two things at once and both are
        // theirs: the GC/quota bookkeeping half (`size_bytes`, `manifest_hash`)
        // and the user-facing half — `path` is the recorder's PLAINTEXT path
        // and `thumbnail_hash` its thumbnail, both populated so a backup-type
        // set's files surface in `fauna.media.list`. A withheld verdict would
        // take the file list out of the owner's archive while the nest keeps
        // serving it to their own app. ⚠ `path_sealed` rides beside the
        // plaintext `path`: the row is mid expand-phase (the 2026-07-29
        // paths-are-content ruling), and both spellings are this actor's own
        // file path going to this actor — the guard found that column, not I.
        //
        // Not `WithheldDerived`: the test for that class is *is the input in the
        // archive?*, and the manifests and chunks this indexes live in the blob
        // store, not in `nest.db` — the owner could not rebuild the index from
        // anything the export carries.
        export: Export::Verbatim,
    },
    // The retained **superseded** generations of the custody rows above — real
    // held bytes of the owner's own backup, charged to `users.storage_bytes_used`
    // (which moves with the successor row). It moves for exactly the reason its
    // live twin `backup_custody` does, and leaving it behind would charge the
    // successor for generations `list_backup_custody_generations` — owner-scoped
    // — can neither list nor restore.
    ActorTable {
        table: "backup_custody_generations",
        column: "uploader_actor",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the backup/custody plane). The
        // retained superseded generations of the rows above: real held bytes of
        // this actor's own backup, charged to their own quota and, per this
        // table's own migration, "client-restorable — until it ages past T".
        // That is version history the owner is still entitled to restore, so
        // withholding it would hide from the archive a set of file versions the
        // nest is holding, and charging them for, on their behalf. Same
        // `uploader_actor` key and the same two-halves reasoning as
        // `backup_custody`.
        export: Export::Verbatim,
    },
    // Added 2026-08-20 as a FIX-FORWARD of a separate in-flight change
    // ("row 167, WIP"), which added the table + its
    // destination-scoped teardown but not this registry entry —
    // leaving `every_actor_shaped_column_is_registered_or_excluded` RED on
    // `origin/main`, i.e. `nest-lib-test-check` red for every session. Landed
    // rather than reported because the ruling is mechanical by precedent, not a
    // fresh judgement: this table is the CHILD of `backup_destinations`
    // directly below, keyed `(owner_actor_id, destination_id, folder_id)`, and
    // `unregister_destination` already deletes its rows in the same breath as
    // the parent's (`db/backup_destinations.rs:224`). So it takes the parent's
    // verdicts verbatim, for the parent's reasons.
    //
    // ⚠ **Row 167 should confirm this rather than inherit it** — if the coverage
    // set was meant to outlive a re-registration, `Policy` is the line to
    // revisit. Purge is the safe direction meanwhile: the row records *which
    // folders a destination covers*, holds no user content, and the successor's
    // client rebuilds it from `fauna.state.backup` exactly as it rebuilds the
    // parent.
    ActorTable {
        table: "backup_destination_folders",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the folder-coverage set of a `backup_destinations` row, and it burns for \
             exactly the parent's reasons: it is a PROJECTION of \
             the `fauna.state.backup` destination rows, which are the authority and which the \
             successor's client rebuilds at first sign-in (`reconcile_backup_enrollment`), \
             so nothing here is lost by deleting it — and it must not MOVE, because a \
             coverage row is reachable from a thief-writable destination registration \
             and moving it would carry a planted destination's reach into the successor",
        ),
        export: Export::Verbatim,
    },
    ActorTable {
        table: "backup_destinations",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a PROJECTION of the `fauna.state.backup` destination rows, which are the authority \
             and which the successor's client rebuilds at first sign-in \
             (`reconcile_backup_enrollment`), so nothing here is lost by deleting it. \
             It must not MOVE: a row is thief-writable directly over \
             `fauna.backup.destination.register`, so a planted destination need not \
             appear on the account plane at all — and the ratified re-register-then-mark \
             adjudication is keyed on the plane list, so a moved row could replicate \
             the successor's whole corpus to an attacker-chosen box that no mark can \
             ever raise (`succession-aftermath.md` § Adjudicating what the aftermath \
             carries across)",
        ),
        // EXPORT RULING (the backup/custody plane).
        // Settled by its own migration in as many words: "None of it is secret
        // — the user's own chosen backup targets — so the row rests plaintext."
        // The same principle holding here too: the person who built the table already
        // answered the disclosure question, and the columns agree (a URL, a
        // 32-byte expected-peer id, a mode, a kind, an optional custodian
        // device id and capacity cap).
        //
        // ⚠ The interesting half is the one the succession axis raises and this
        // one must NOT inherit. That axis rules the row a `Burn` because it is
        // "a PROJECTION of the `fauna.state.backup` destination rows, which are the
        // authority" — and "projection" is the exact word that invites a
        // `WithheldDerived` reflex here. It does not survive that class's test,
        // *is the input in the archive?*: the authority is the client-sealed
        // `fauna.state.backup` plane entries, whose content the export does not
        // carry at all (the shaped folders domain emits folder metadata; blob
        // bodies ride only for refs scanned out of posts). So the owner
        // cannot re-derive this list from their archive, and withholding it
        // would drop their backup topology entirely. Derived is about what the
        // ARCHIVE'S READER can reconstruct, never about what the SYSTEM can.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "backup_writer_grants",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "standing authority with an attacker-chosen grantee: the row is what lets a \
             SOURCE nest write this owner's custody here, a thief can name their own nest \
             over `fauna.backup.writer_grant.register`, and a rogue writer's supersede \
             power IS delete power (`backup_custody_generations` exists to bound it). \
             Re-granting is one visible enroll gesture. ⚠ These rows belong to an owner \
             who is local to the DESTINATION nest, whose ceremony this nest never runs \
             (see the module doc's succession note) — so this declares the direction \
             rather than closing a live exposure",
        ),
        // Three columns, no secret in any of them, and the migration says why
        // in as many words: **"This row IS the authorization"** — a nest id is
        // self-minted and free, so `writer_nest_id` confers nothing on whoever
        // reads it; the permission is the row's existence *on this nest*.
        // Exporting the id therefore grants no one anything, while withholding
        // it would hide which nests the owner authorized to write their custody.
        // That list is the owner's own grant ledger and the invariant's
        // *audited from the user's app* names it.
        export: Export::Verbatim,
    },
    // ─── The Bluesky link plane, ruled 2026-08-15 ─────────────────
    //
    // The whole link BURNS, and it is the same *shape* as the ActivityPub
    // family above with the opposite verdict, because the discriminator is not
    // the plane but WHO CHOSE THE EXTERNAL DESTINATION. AP's is derived from
    // the account; this one's is supplied by whoever completed the OAuth flow.
    ActorTable {
        table: "bluesky_accounts",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the row IS the actor->authority edge, and its destination is attacker-chosen. The \
             token columns are written empty (the OAuth session is keyed by DID, not by actor), so \
             what this row holds is the binding a linked-account lookup resolves into an \
             authenticated third-party agent with no further identity check -- a credential \
             honoured INSTEAD of an identity, the eviction-token class. Linking is an ordinary \
             User-class gesture, so a seed thief links their OWN external account; carried \
             forward, the cross-post setting on this same row then publishes the SUCCESSOR's every \
             public post into the repo the thief chose, with no end date. Stay is not the safe \
             fallback: the unlink door scopes to the authenticated caller, so a row left on the \
             retired identity is unrevocable by the successor and by everyone else. Nothing \
             irrecoverable dies -- the link is a means of access to an account the user still \
             controls elsewhere, re-made in one authorization gesture.",
        ),
        // EXPORT RULING. ⚠ **This table points the
        // opposite way to its schema, and the recon that measured it is worth
        // re-reading before anyone "fixes" this verdict.** The columns read
        // `access_token`, `refresh_token`, `dpop_key`, all `BLOB NOT NULL`, and
        // the reflex is WithheldSecret. But the only production writer
        // (`bluesky/db_helpers.rs::upsert_linked_account`) stores **empty
        // bytes** in all three -- "tokens managed by session store" -- and the
        // only other writer touches `write_through`. The live credentials rest
        // in `atproto_sessions`, already WithheldSecret and on the belt's
        // roster. The succession axis's own landed reason says the same thing
        // in passing ("the token columns are written empty"), so this is a
        // measured fact, not a reading of intent.
        //
        // So they are **dead carriers**: the `content_key_version` lesson
        // inverted -- there a name said key and was not, here a name AND a type
        // say credential while the content is empty. `Verbatim` would
        // nevertheless be a verdict that is correct only until a writer
        // changes, and the change would be silent. `Redacted` omitting all
        // three is correct today AND correct the day someone fills them.
        //
        // What rides is the link itself: `bluesky_did` and `bluesky_handle` are
        // the owner's own external identity (they typed the handle),
        // `write_through` is their cross-post setting, and the timestamps are
        // when they linked. `token_expires` rides and is cleared -- an expiry
        // integer, written `0` by the same writer that empties the tokens.
        export: Export::Redacted {
            omit: &["access_token", "refresh_token", "dpop_key"],
            reason: "the link rides -- the DID and handle of the account the owner chose to \
                     connect, their write-through setting, and when they linked it. The three \
                     credential-shaped BLOBs are omitted although they are EMPTY in production: \
                     `upsert_linked_account` writes zero bytes into all three (the OAuth session \
                     is keyed by DID and lives in `atproto_sessions`, WithheldSecret), so \
                     Verbatim would be correct only until a writer changed, silently. These are \
                     exactly the columns that must never ride if anything ever fills them. \
                     `token_expires` is an expiry integer, not a token.",
        },
    },
    // The two satellites burn WITH the link rather than on arguments of their
    // own: each is per-actor state *of that link*, and with the binding gone
    // every one of them names an external account the row's owner no longer
    // has. (the three-burns-are-one-object coupling, one bridge over.) None
    // of them holds user content: the Fauna posts and conversations they map
    // TO are `content` / `segment_records` rows, which move with the account.
    ActorTable {
        table: "bluesky_interactions",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "each row is the record URI of a like or repost living inside the repo the retired \
             identity linked -- i.e. a thief-chosen one. Carried forward, the successor's undo \
             would reach into that repo; left behind, it is an unreachable pointer. It burns with \
             the link that is the only way to resolve it.",
        ),
        // EXPORT RULING. The owner's own likes and
        // reposts: which Fauna post they interacted with, how, and the ATProto
        // record URI their action minted in their own repo. Actions they took,
        // recorded against themselves.
        //
        // ⚠ The succession axis BURNS this table and reasons from a thief
        // choosing the destination repo -- that is a compromise argument about
        // where the records live, and it says nothing about whether the owner
        // may read their own interaction history. `record_uri` names a record
        // in the repo of the account they linked; naming it discloses to the
        // owner what the owner already published there.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bluesky_saved_feeds",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "saved feed pointers whose only surface is the linked account's own feed picker; with \
             the link burned there is nothing that reads them. A curation preference, re-made in \
             one gesture after re-linking.",
        ),
        // EXPORT RULING. The owner's saved-feed list --
        // the feed URI plus the display name, description and avatar the
        // picker shows. A curation preference they made, and precisely the
        // shape of thing a user rebuilding elsewhere wants the list of. The
        // succession axis burns it because the picker that reads it dies with
        // the link; that is about reachability, not about disclosure.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_audit_events",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "TESTIMONY ABOUT THE THEFT ITSELF: a row says *this credential \
             authenticated from this IP at this time*, and on a succeeding account \
             the interesting rows are overwhelmingly the THIEF's — re-pointing them \
             would attribute the intrusion to the identity recovering from it, the \
             `bridge_service_users.approved_by_actor_id` argument verbatim. Ruled \
             CONDITIONAL on the read side being dark: `report_auth_event` is a \
             write-only wire kind, `list_bridge_auth_events_for_actor` has test \
             callers only, and no list kind exists — so no reachable row \
             distinguishes this verdict from `Move` today. ⚠ Residual for whoever \
             ships that surface: scope it through the succession chain, NOT by \
             re-pointing rows — the successor needs to READ the predecessor's \
             AUTH history (it is the evidence of their own theft) without the \
             record claiming they made it",
        ),
        // EXPORT RULING (2026-08-15) — the account's own
        // authentication history, and the succession ruling above is the
        // corroborating read (read the `Succession`
        // reason first, per the general practice this file uses throughout). Its residual says in as many words that the successor
        // *needs to READ* the predecessor's auth history because it is the
        // evidence of their own theft — a plane the axis has already argued the
        // account holder is entitled to read is a plane their archive carries.
        //
        // Every column is a fact about a login attempt against THIS account:
        // which bridge reported it, which credential id was presented, whether
        // it succeeded, from which IP, when, and why it failed.
        // ⚠ `credential_id` is an IDENTIFIER, not the credential — the handler
        // (`report_auth_event_handler`) passes through a name the bridge
        // chooses; the secret halves rest in `bridge_wrapped_mls_blobs` and
        // `bridge_wrapped_submission_tokens`, both `WithheldSecret`. A table
        // whose name says credential and whose column holds a label is
        // read by that same principle in its cheaper direction: read the writer.
        //
        // `source_ip` is a third-party datum only in the failure rows — where
        // it is the attacker's, and the owner is exactly who should have it.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_caldav_calendars",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — THE USER'S OWN CALENDAR.
        // The DAV/IMAP collection plane is the account's resting content, so the
        // class rule at the top of § Re-key scope decides it: ownership moves.
        // `encrypted_metadata` is sealed under the owner's MLS read key, which
        // the corpus re-seal's predecessor-key read fallback already opens for
        // the successor; every door here (list, PUT, DELETE,
        // `sync_calendar_since`) keys on `actor_id`. So a `Stay` leaves a
        // calendar the successor can neither list nor delete, while their MUA —
        // which re-AUTHs to the SAME address, `account_aliases` having moved —
        // shows an empty account. Unrecoverable from any client
        // (`nest/common.md` § Client-state recoverability), and the rows are
        // user-irrecoverable data, so no burn was ever available.
        //
        // ⚠ The three CalDAV tables CANNOT take different verdicts: `ctag` /
        // `highestmodseq` / `etag` are one sync clock a MUA compares across all
        // three, so splitting them hands the successor's MUA a collection whose
        // counters disagree with its contents — silent protocol-level
        // corruption, where the whole-plane failure is merely a missing
        // calendar.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the owner's own calendars,
        // and **the axis's first use of the sealed-columns clause**
        // (`account-data-plane.md` § Nest-side requirements item 1: *"Sealed
        // columns ride `Verbatim`/`Redacted` in their at-rest form"*).
        //
        // `encrypted_metadata` is the calendar's name, colour, timezone and
        // visibility, sealed under the owner's own MLS read key; the nest never
        // opens it and does not open it for the export either — the hex of the
        // ciphertext is what rides. This is the OTHER half of the earlier
        // ruling's discriminator, used here for the first time: that ruling established
        // that sealed KEY material never rides because its wrapping key is
        // obtainable by another route; sealed CONTENT rides for the reason the
        // goal doc gives in as many words — the export goes to the data's owner
        // over their own channel, so withholding their own sealed rows would
        // invert the invariant the export serves.
        //
        // ⚠ And the obtainability argument does NOT flip this back. The
        // wrapping key here *is* separately reachable (that is how a MUA reads
        // the calendar at all — the bridge opens it with the wraps in
        // `bridge_webdav_keys_blobs` / `bridge_wrapped_mls_blobs`, both
        // `WithheldSecret`). Under that same reasoning this would condemn a KEY;
        // for CONTENT it is the point: the owner can open what is theirs. What
        // must never ride is the wrap, and it does not.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_caldav_events",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the events inside the
        // calendars above, moving as one unit with them (the one-sync-clock
        // argument on `bridge_caldav_calendars`).
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the events themselves: this
        // is the single largest piece of the owner's own content the archive
        // gains from this cluster. `encrypted_fauna_ext` is the sealed Fauna
        // sidecar (RSVP refinement, per-attendee nest hints) and
        // `encrypted_index_hint` the sealed lookup hint — both CONTENT sealed
        // under the owner's own read key, riding in at-rest form per the
        // sealed-columns clause spelled out on `bridge_caldav_calendars` above.
        // The sealed VEVENT itself rests in the owner's `__calendar` segment,
        // not in this row; `record_cid` names it.
        //
        // The plaintext floor beside them (`uid_hash`, `etag`, `modseq`,
        // `ciphertext_size`, the two dates) is the owner's own metadata about
        // their own events, and `uid_hash` is a blake3 that discloses nothing
        // further. A `Redacted` dropping the sealed columns would be the
        // inverted verdict: it would export the shape of a calendar while
        // withholding the calendar.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_caldav_expunged",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the deletion tombstones
        // `sync_calendar_since` serves (RFC 6578).
        //
        // ⚠ The tombstone logs are the members whose `Stay` fails in the
        // RESURRECTION direction rather than the missing-data one: the events
        // move, the tombstones do not, so the successor's MUA syncs from its
        // stored modseq, is told about no removals, and re-shows every event the
        // account had deleted. A left-behind tombstone is not inert residue — it
        // is a deletion the user performed that the protocol never reports.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — `bridge_imap_expunged`'s
        // twin one protocol over; the full reasoning, including why the
        // succession axis's inseparable-unit ruling does not transfer to this
        // axis, is written at that entry.
        export: Export::WithheldOperational(
            "the RFC 6578 deletion tombstones `sync_calendar_since` serves — this box's own \
             incremental-sync bookkeeping, keyed to its own modseq clock, which an archive \
             has nothing to catch up against. The rows cannot even name what was deleted: \
             `uid_hash` is a blake3 of the plaintext iCalendar UID. The calendars and the \
             events that still exist ride `bridge_caldav_calendars` and \
             `bridge_caldav_events`, both Verbatim",
        ),
    },
    ActorTable {
        table: "bridge_carddav_addressbooks",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the user's own address books.
        // `bridge_caldav_calendars`' structural twin one protocol over: same
        // argument, same three-table unit. `MIGRATIONS_BRIDGE_CARDDAV`'s own
        // comment already names these rows user-irrecoverable at-rest data.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — `bridge_caldav_calendars`'
        // structural twin one protocol over: the owner's own address books,
        // `encrypted_metadata` sealed under their own read key and riding in
        // at-rest form (the sealed-columns clause, spelled out at the CalDAV
        // entry). `MIGRATIONS_BRIDGE_CARDDAV`'s own comment calls these rows
        // user-irrecoverable at-rest data, which is the migration-comment
        // shortcut used elsewhere in this file — and here it points the
        // same way as the schema read.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_carddav_cards",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the vCards inside the address
        // books above; `bridge_caldav_events`' twin, one-sync-clock included.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the vCards: the owner's
        // address book, sealed under their own read key, riding at rest exactly
        // as `bridge_caldav_events` does (same two sealed columns, same
        // segment-resident body, same plaintext floor, same argument).
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_carddav_expunged",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the CardDAV deletion
        // tombstones; `bridge_caldav_expunged`' twin, resurrection argument
        // included.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the third tombstone log;
        // reasoning at `bridge_imap_expunged`.
        export: Export::WithheldOperational(
            "the RFC 6578 deletion tombstones `sync_addressbook_since` serves -- \
             `bridge_caldav_expunged`'s twin, same blake3 `uid_hash` that cannot name the \
             deleted card, same incremental-sync bookkeeping an archive has no clock to \
             use. The address books and cards that still exist ride \
             `bridge_carddav_addressbooks` and `bridge_carddav_cards`, both Verbatim",
        ),
    },
    ActorTable {
        table: "bridge_feed_subscriptions",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the account's own list of
        // feeds it follows through a content bridge. Every door is owner-scoped
        // (`db/bridge.rs`: the list SELECTs `WHERE actor_id = ?1`, the delete is
        // `WHERE id = ?1 AND actor_id = ?2`), so a `Stay` gives the successor an
        // empty list *and* leaves rows nobody can enumerate or remove — the
        // `feeds` stranding shape one plane over, minus the public catalog.
        //
        // Checked and answered NO, because both would have changed the verdict:
        // nothing polls this table across actors (the four statements in
        // `db/bridge.rs` are its only consumers, all actor-scoped), so a
        // stranded row runs on no clock; and the subscription names a feed URI
        // the account chose from a registry-supplied bridge, not an
        // attacker-selected destination — the `spam_preferences` side of the
        // derived-vs-supplied test, not the `forward_all_to` side.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the account's own list of
        // feeds it follows through a content bridge: which bridge, which feed
        // URI, the name the user gave it, when they added it. A subscription
        // list is curation, i.e. the user's own data, and the succession ruling
        // above already established that every door to it is owner-scoped, so
        // nothing here belongs to anyone else. No secret, no bearer material.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_imap_expunged",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the VANISHED log
        // CONDSTORE/QRESYNC serves (RFC 7162); `bridge_caldav_expunged`'s
        // resurrection argument on the mail plane.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the first of the three
        // tombstone logs this cluster withholds, and the place the reasoning is
        // written out in full (the CalDAV and CardDAV twins point here).
        //
        // ⚠ **The succession axis rules these three tables INSEPARABLE from the
        // collections they belong to, and that unit does NOT transfer to this
        // axis.** The argument (above, and on `bridge_caldav_expunged`) is
        // that a MUA holds a stored modseq and syncs FORWARD from it, so a
        // moved collection whose tombstones stayed behind resurrects deleted
        // items on a live account. An archive is a snapshot: nothing reads it
        // incrementally, there is no stored modseq to sync forward from, and no
        // clock for the tombstones to keep consistent. The two axes ask
        // different questions of the same three tables and correctly get
        // different answers — a sibling axis's ruling is not evidence for this
        // one (the same principle, one level up).
        export: Export::WithheldOperational(
            "the VANISHED log RFC 7162 serves a reconnecting MUA -- (mailbox, uid, modseq, \
             expunged_at) rows naming messages that no longer exist. It is one client's \
             incremental catch-up against THIS box's own modseq clock, and an archive is a \
             snapshot with no clock to catch up against, so the log is meaningless off the \
             nest. Nothing of the owner's is withheld by it: a UID cannot name the deleted \
             message, and the mail that still exists rides `bridge_imap_messages`. The \
             succession axis moves these rows for a reason that does not apply here -- see \
             the comment at this entry",
        ),
    },
    ActorTable {
        table: "bridge_imap_mailbox_state",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the per-mailbox UID window.
        //
        // ⚠ This is the member where a `Stay` is worst, and the reason is an
        // IMAP invariant rather than a Fauna one: `uid_next` must never hand out
        // a UID this mailbox has used before under the same `uid_validity`. Left
        // behind, the successor's mailbox state row is absent, so the defaults
        // apply (`uid_validity = 1`, `uid_next = 1`) while the moved
        // `bridge_imap_messages` rows already occupy those UIDs — every MUA that
        // had synced the account then sees a *different message* at a UID it has
        // cached, which RFC 9051 makes no provision for detecting. The three
        // IMAP placement tables therefore move as one unit, exactly like the
        // CalDAV three.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — **this table IS the mailbox
        // list**, which is what makes it the owner's data rather than the
        // protocol bookkeeping its column names suggest. `LIST` is served from
        // `list_bridge_imap_mailbox_state` (`db/bridge_imap.rs`), so a mailbox
        // exists exactly when it has a row here: the folder names the user made
        // — "Receipts", "Family", whatever they called them — rest in this
        // table and in no other, and an empty mailbox has no other trace at all.
        // Withholding it would export a pile of messages with no account
        // structure and silently drop every empty folder.
        //
        // The three counters (`uid_validity`, `uid_next`, `highestmodseq`) ride
        // along at rest. They are this box's clock and mean nothing off it, but
        // `Verbatim` is per TABLE and the alternative — a `Redacted` dropping
        // them — would trade a real column of the row for nothing: they are not
        // secret, not another party's, and reconstructing the account elsewhere
        // wants them.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_imap_messages",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the account's own mailbox
        // placements. The bodies these rows point at are `segment_records`
        // (kind='mail') whose `scope_id` already moves, so a `Stay` splits one
        // mechanism in half in the direction that hides the data: the mail is
        // the successor's and the index saying which mailbox it sits in, whether
        // it was read, and what UID it has, is not — the successor's MUA AUTHs
        // to the same address and finds an empty account whose storage quota is
        // fully consumed.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the index of the owner's own
        // mail. Every column is a fact about their mailbox: which mailbox a
        // message sits in, at which UID, which IMAP flags it carries (read,
        // answered, flagged, draft), when it arrived, and the four case-folded
        // header norms IMAP SEARCH matches on. No key, no credential, no bearer
        // material — and read state and placement rest nowhere else, so
        // withholding this would hand the owner an archive that cannot say
        // which of their own mail they had read.
        //
        // ⚠ **The bodies are not in this table and are not yet ruled.** The
        // RFC822 bytes live in the segment store, mirrored by `segment_records`
        // (kind='mail'), which is still `Unreviewed` — so this verdict ships an
        // index whose targets the archive does not yet carry. That is declared
        // rather than hidden: `segment_records` is named in the manifest's
        // `unreviewed_tables`, which is what rule 4 exists for. Ruling the
        // mail-BODY plane is the named next question, and it is a bigger one
        // (the bytes are not in `nest.db` at all).
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_imap_subscriptions",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the LSUB / LIST (SUBSCRIBED)
        // set. Moves with the mailboxes it names; RFC 9051 §6.3.7 decouples a
        // subscription from mailbox existence, so this is the one member of the
        // IMAP unit whose rows can outlive their target — which is an argument
        // for carrying them, not against: the successor's MUA re-subscribing by
        // hand is the only alternative, and nothing else can ever remove a
        // stranded row.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — a two-column preference the
        // user expressed through their MUA: which of their mailboxes they
        // subscribed to. RFC 9051 §6.3.7 keeps a subscription alive across the
        // mailbox's own lifetime, so this set is not derivable from the mailbox
        // list beside it — the only way to reproduce it elsewhere is to carry
        // it. Nothing here is a secret, and the alternative reading (protocol
        // bookkeeping) fails the class question: the box never decides a
        // subscription, the user does.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_mls_snapshot_blobs",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "MSEK-SEALED KEY MATERIAL WITH NO REVOKE DOOR — the bridge's copy of \
             the actor's MLS state, AEAD-sealed under the MSEK, which derives \
             from the seed the thief read. `Move` is the actively dangerous \
             direction: it hands the successor a snapshot the thief can open, and \
             it is stale anyway once the corpus re-seal re-keys under MSEK'. \
             `Stay` is not the safe fallback either — this table has no delete \
             accessor at ALL (`db/bridge_blobs.rs` offers put/get only), so a \
             left-behind row is unrevocable by construction, one worse than the \
             caller-scoped revoke doors on its two siblings. Nothing \
             user-irrecoverable dies: the blob is a projection the successor's \
             client re-provisions after the re-seal. See the shared burn leg for \
             why this class is a burn where `actor_mls_pubkeys` is a stay",
        ),
        // The neighbour an earlier pass walked past: it sits in the same migration
        // block as `bridge_wrapped_mls_blobs` and `bridge_webdav_keys_blobs`,
        // both ruled `WithheldSecret` there, and was not ruled with them.
        //
        // `blob` is the per-actor MLS state snapshot, AEAD-sealed under the
        // MSEK (`migrations.rs:2990`). MLS state *is* key material — the
        // ratchet tree and the epoch secrets are what opens the group's
        // traffic — so this is the enum's discriminator applied exactly:
        // sealed KEY material, not sealed content. And the wrapping key is
        // reachable by another route rather than by accident: the same actor's
        // MSEK is what `bridge_wrapped_mls_blobs` hands a MUA credential, and
        // what the plaintext-mode custody row holds literally
        // (`migrations.rs:3009`). An eviction-token holder who also holds a MUA
        // app password would open this blob out of the archive.
        //
        // ⚠ The succession ruling above landed in parallel and reaches
        // the same reading from the other axis — "MSEK-SEALED KEY MATERIAL WITH
        // NO REVOKE DOOR", sealed under a key that derives from the seed. Two
        // independent passes classifying this blob as key material rather than
        // content is the corroboration the discriminator wants.
        export: Export::WithheldSecret(
            "the per-actor MLS state snapshot, AEAD-sealed under the MSEK: ratchet tree \
             and epoch secrets, i.e. key material, and the MSEK that opens it is \
             separately obtainable through a MUA credential (`bridge_wrapped_mls_blobs`) \
             or the plaintext custody row. Sealed KEY material never rides -- the \
             enum's discriminator, not the sealed-content clause",
        ),
    },
    ActorTable {
        table: "bridge_restore_divergence",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — and the question is the one
        // that decides it: IS THE COLUMN THE REGISTRY NAMES THE ONE THAT MATTERS?
        // No. The only production reader
        // (`db/snapshots.rs::list_restore_divergence`, behind
        // `fauna.filesync.snapshot.list_restore_divergence`) selects `WHERE
        // snapshot_id = ?1` and takes its authorization from
        // `resolve_snapshot_owner`; the GC deletes by `snapshot_id` too. So
        // `actor_id` is written and never read, and access already follows the
        // account through `snapshots`.
        //
        // It moves anyway, and not as a coin-toss: the row is a notice ABOUT
        // DATA rather than testimony about a person — "your MUA was ahead of
        // what the restore gave back, you may have lost N events" — and every
        // half of what it is about moves (the snapshot, `restore_history` the
        // writer resolves through, the DAV/IMAP collections above). That is
        // `obligation_action_records.author_hex`'s argument verbatim: a record
        // moves with the content it is about. Keeping the two halves of one fact
        // consistent is the whole of it.
        //
        // ⚠ Recorded as an UNFALSIFIABLE ruling in that sense: no production
        // consumer reads this column, so no reachable row distinguishes `Stay`
        // from `Move`. Said here rather than papered over with a pin that seeds a
        // read the schema has no reader for.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — a data-loss notice addressed
        // to the owner: *your MUA was ahead of what the restore gave back, you
        // may have lost N events in this collection*. That is precisely the
        // class of fact an archive exists to carry, and it is unreproducible
        // anywhere else once the snapshot's retention window closes.
        //
        // ⚠ Its `actor_id` has no production reader (the succession ruling
        // above rests on that, and the sweep authorizes through the snapshot's
        // owner instead) — but the EXPORT read is exactly a read of this
        // column, so this verdict gives the column its first live consumer.
        // Nothing in the row is another party's: `mua_id` is the owner's own
        // client, the modseqs are their own collection's.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_search_policy",
        column: "actor_id",
        // Hex TEXT, like the bridge families — `bridges_ui_handlers.rs` writes it
        // through `bridge_search::set_policy(conn, actor_hex, …)`. Found by the
        // affinity test the moment it existed; before that this entry was a
        // silent no-op in the main (non-feature-gated) schema.
        key: ActorKey::Hex,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the account's two per-bridge
        // Search-corpus choices. It moves as the owner's own curation preference:
        // `set_policy` is written only through the caller-scoped settings door,
        // so a `Stay` shows the successor the defaults on a page whose real,
        // still-counted row they cannot reach.
        //
        // ⚠ THE RULING ALONE DOES NOT CLOSE WHAT IT SURFACED, and the residual is
        // the interesting half. `bridge_search::effective_policy` unions every
        // actor's vote and adds an ON default vote whenever `explicit <
        // total_actors`, where `total_actors` is a bare `COUNT(*) FROM users`.
        // The ceremony keeps the predecessor's `users` row (handle cleared) and
        // inserts the successor's, so every succession permanently inflates that
        // denominator by one and the retired identity — which can never express a
        // preference — votes ON forever. On a nest whose users had all opted OUT,
        // one recovery ceremony silently switches bridge indexing back ON. Moving
        // this table cannot fix that (the phantom vote comes from the users row,
        // not from this one); the denominator is fixed in
        // `effective_policy` itself, which now excludes superseded actors.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the account's own two
        // per-bridge Search-corpus choices (`show_in_search`, `post_limit`),
        // written only through the caller-scoped settings door. A setting the
        // user made is the user's data; an absent row means they are on the
        // defaults, so the archive carrying only the rows they actually set is
        // the honest shape.
        //
        // ⚠ This is the cluster's only `ActorKey::Hex` entry — the export read
        // binds the actor in the entry's own declared spelling
        // (`the_export_read_binds_the_declared_actor_key_encoding`), which is
        // the same trap that made sixteen registry entries silent no-ops on the
        // deletion axis before the encoding became registry data.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_session_close_events",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "`bridge_audit_events`' twin — a bridge's report that a session for \
             this credential ended, for this reason, at this time. Same \
             attribution argument (on a succeeding account the rows worth reading \
             are the thief's, and re-pointing them re-attributes the intrusion to \
             its victim), same CONDITIONAL-on-dark-readers footing: \
             `report_session_close` is write-only on the wire and \
             `list_bridge_session_close_for_actor` has test callers only",
        ),
        // EXPORT RULING (2026-08-15) — `bridge_audit_events`' twin
        // on the session side (which credential's session ended, for what
        // reason, when), and the same verdict for the same reason: the owner's
        // own connection history, no secret in any column, `credential_id` an
        // identifier rather than a credential.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_submission_quota",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-15) — the per-account-per-day
        // outbound submission meter, and it is the LIMITS-MOVE rule verbatim
        // (§ Re-key scope's liability blockquote): a succession is self-service,
        // so a cap a ceremony sheds is a cap with a reset button. Left behind,
        // `try_consume_submission_quota` reads zero for the successor and the
        // account gets a fresh full day of relay capacity on demand. The cost of
        // moving is bounded and self-clearing — one day's consumption inherited,
        // zeroed at the next `day_bucket` rollover — where the cost of not
        // moving is unbounded. `mail_list_account_daily_counter`'s twin on the
        // submission side.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-15) — the rate-counter archetype
        // the `WithheldOperational` variant names, and the one member of this
        // cluster whose succession ruling argues the OPPOSITE way for a reason
        // that is specific to that axis: a limit must move so a ceremony cannot
        // reset it (the liability rule). Export asks only whether the row is
        // the owner's data, and a per-day meter of this box's own relay
        // capacity is not — it is what the nest decided to allow, it zeroes
        // itself at the next day boundary, and nothing about the account is
        // reconstructible from it.
        export: Export::WithheldOperational(
            "a per-account-per-day outbound submission meter (`day_bucket`, `used`) -- the \
             rate-counter case this variant names. It is this nest's own metering of its \
             own relay capacity, self-clearing at the next day boundary, and says nothing \
             about the account's data; the mail it metered rides the mail plane. \
             ⚠ The succession axis MOVES this row, for an axis-specific reason (a cap a \
             self-service ceremony sheds is a cap with a reset button) that has no bearing \
             on what an archive should carry",
        ),
    },
    ActorTable {
        table: "bridge_webdav_keys_blobs",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "MSEK-SEALED CONTENT KEYS WITH NO REVOKE DOOR — the bundle of content \
             keys for the sets the account flagged for WebDAV serving, sealed \
             under the MSEK the thief read. `Move` is doubly wrong: it hands the \
             successor a bundle the thief can open, and the corpus re-seal re-keys \
             those sets, so the carried bundle is stale and serving breaks anyway. \
             Like `bridge_mls_snapshot_blobs` there is no delete accessor at all, \
             so `Stay` is unrevocable by construction. The client re-provisions \
             after the re-seal (`fauna-client-folders`'s webdav provisioning is an \
             atomic replace), so nothing user-irrecoverable dies — the sets \
             themselves are `folders` rows that move",
        ),
        export: Export::WithheldSecret(
            "a `WebdavKeysBlob` carrying the CONTENT KEYS of every set the user \
             flagged for WebDAV serving (`webdav-server.md` § Key model; the \
             nest never decodes it). Sealed under the MSEK -- so this is the \
             enum's wrapped-key case exactly: the blob opens the served sets, \
             and `bridge_wrapped_mls_blobs` beside it is where the MSEK itself \
             rests. Neither rides",
        ),
    },
    ActorTable {
        table: "bridge_wrapped_mls_blobs",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "THE MUA CREDENTIAL PLANE, WHICH IS WHERE THE ONE DEMONSTRATED FAILURE \
             ON THIS AXIS LIVED — each row is the account's MSEK wrapped under one \
             MUA credential's secret, and the pre-succession seed holder read \
             both. `Move` would re-create the `account_aliases` defect \
             deliberately: the address moves to the successor, and a moved wrap \
             makes the THIEF's mail password keep opening the successor's mail. \
             `Stay` leaves rows the successor cannot delete (the revoke door \
             refuses any target that is not the caller) on an identity no address \
             routes to any more — useless and unrevocable, which the axis has \
             ruled is not the same as gone. Nothing user-irrecoverable dies: the \
             mail is in the segment store, and the successor's client re-provisions \
             fresh wraps under MSEK' as part of the aftermath's mail burn",
        ),
        export: Export::WithheldSecret(
            "the MSEK wrapped under a MUA credential (the migration comment says \
             so in as many words). The wrapping key is an IMAP/SMTP app password \
             the user holds and reuses -- the archetype the enum's discriminator \
             names: ciphertext whose other half an eviction-token holder can \
             come by separately. The mail this key opens exports as mail, \
             through the shaped domains and the mailbox tables, never as its key",
        ),
    },
    ActorTable {
        table: "bridge_wrapped_submission_tokens",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the outbound half of the same credential plane — a submission token \
             wrapped under one MUA credential's secret, honoured by the relay in \
             place of an identity, so the refusal plane never sees the caller \
             (the earlier pass's question answered no, the `eviction_tokens` class). Same \
             three-way argument as `bridge_wrapped_mls_blobs`, and the direction \
             matters more here because the credential authorizes SENDING as the \
             account: carried forward it lets the thief relay as the successor",
        ),
        export: Export::WithheldSecret(
            "a wrapped SMTP submission token -- bearer authority to send AS this \
             account, one wrapping away, keyed by the same `credential_id` as \
             the wrapped MSEK above. `atproto_sessions`' ruling applies word for \
             word: bearer material never rides an export",
        ),
    },
    ActorTable {
        table: "capability_grants",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a grantee the thief added to the ledger would otherwise be handed live \
             READ capability by the successor's own client — re-minting is bounded \
             and visible, inheriting is neither",
        ),
        // The one row an earlier recon deferred as genuinely ambiguous, ruled
        // here after reading what `blob` actually seals.
        //
        // `blob` is a canonical-dag-cbor `fauna_mls::wrapped_blob::GrantBlob`
        // HPKE-sealed to `holder_pubkey` (`migrations.rs:3453`), and its
        // `wrapped_keys` field is one `WrappedScopeKey` **per key-bearing scope
        // tuple × epoch it opens** (`fauna-mls/src/wrapped_blob/format.rs:1138`).
        // That is a key, under a wrapping, that opens something else — the
        // enum's own test — and the wrapping key is a bridge service-user
        // X25519 key, i.e. exactly the "obtainable via a bridge credential"
        // route. So the blob cannot ride.
        //
        // The REST of the row is the opposite case, and it is why this is not
        // `WithheldSecret`: `grant_id`, `holder_pubkey`, `epoch_end` and
        // `created_at` are the grant ledger itself — who holds standing
        // capability over this owner's scopes, and until when. `principles.md`
        // § The user always controls their data requires exactly that ledger be
        // "audited from the user's own app"; withholding the whole table would
        // make the export silent about the grants it is the invariant's job to
        // surface. Omitting one column keeps the audit and drops the key.
        export: Export::Redacted {
            omit: &["blob"],
            reason: "`blob` is a GrantBlob whose `wrapped_keys` carry one wrapped content key \
                     per granted scope-epoch, HPKE-sealed to a bridge service-user key -- a key \
                     that opens something else, so it never rides. Every other column is the \
                     grant ledger (holder, expiry, mint time) that `principles.md` § The user \
                     always controls their data requires the owner be able to audit, so the \
                     row rides without it",
        },
    },
    ActorTable {
        table: "channel_foreign_members",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-14). The `sync_devices` case in its
        // purest form — this table is ALREADY re-pointed, by a leg that has
        // shipped for as long as peer succession has, and the registry simply
        // never said so. `record_peer_succession` runs a bespoke two-step on it
        // (drop the superseded row where the successor already holds the seat,
        // then update the rest) and counts the result as
        // `PeerSuccessionApplied::foreign_memberships`.
        //
        // ⚠ That leg is the PEER path; this axis governs the LOCAL ceremony, and
        // there the leg is vacuous BY CONSTRUCTION rather than by choice. Rows
        // here name only actors homed on another nest: the sole producer,
        // `register_foreign_channel_member`, is reachable only from
        // `welcome_deliver`'s federation-relay branch, and a local recipient
        // takes the `register_actor_channel` branch instead ("never reaching
        // these sites", conversations_handlers.rs). `record_succession` succeeds
        // a LOCAL account, so its `?old` can never match a row here.
        //
        // Declaring `Move` would therefore be worse than merely useless. It
        // would assert that the local ceremony re-points foreign memberships —
        // false, and the generic sweep would happily seed a synthetic row and
        // witness the lie — and it would let a local transaction rewrite
        // membership rows that only a VERIFIED peer statement is entitled to
        // touch. It also shares `actor_channels`' push-stranding hazard: two of
        // `succession_push_targets`' three branches read this table under the
        // retired id.
        //
        // ⚠ MEASURED (2026-08-14, mutation M5), and it is the sharpest form of
        // the reachability finding rather than a restatement of it: flipping
        // this entry to `Move(Plain)` leaves **every** test green — both
        // data-driven sweeps, the FK gate, the exact-count ratchet and all three
        // of this cluster's hand pins. A vacuous-by-construction leg is not
        // merely "witnessed like a reachable one" (the wording); it is
        // UNFALSIFIABLE, because the only rows that could distinguish the two
        // verdicts are rows no production writer can create. Worse, the sweep
        // does not fail to notice — it MANUFACTURES the observable, seeding a
        // synthetic local row and confirming that it moves. So this ruling rests
        // on the reachability argument above and on nothing else, deliberately
        // and unavoidably. Do not "strengthen" it with a pin that seeds a local
        // actor here: that pin would assert a state the schema's own producer
        // cannot reach, and passing it would mean less than the argument does.
        succession: Succession::Stay(
            "FOREIGN membership, and already re-pointed by the path that owns it: \
             `record_peer_succession` runs a bespoke two-step here and reports \
             `PeerSuccessionApplied::foreign_memberships`. On the LOCAL ceremony this \
             axis governs, the leg is vacuous by construction — the only producer \
             (`register_foreign_channel_member`) fires on the federation-relay branch of \
             welcome delivery, so every row names an actor homed on ANOTHER nest, which \
             `record_succession`'s locally-registered `?old` can never match. A `Move` \
             would declare a re-point that does not happen, let a local transaction \
             rewrite membership only a verified peer statement may touch, and (like \
             `actor_channels`) empty two of the three `succession_push_targets` branches",
        ),
        // EXPORT RULING (the sync plane). The `actor_id`
        // column here names an actor homed on ANOTHER nest, so this is the
        // same `feed_contributors` check, seen elsewhere in this file, firing again — run it FIRST on
        // any membership table, because the column name gives nothing away.
        // The sole producer is `welcome_deliver`'s federation-relay branch
        // (`conversations_handlers.rs`, the `register_foreign_channel_member`
        // call beside `resolve_peer_nest_id`), whose `recipient` is by
        // construction the actor the Welcome was relayed to at `peer_url`; a
        // local recipient takes the `register_actor_channel` branch instead.
        // A local exporter's `?actor` therefore matches no row here, ever.
        //
        // Vacuity is why it cannot leak today; the CLASS is why the verdict must
        // not flip if a producer ever writes a local row. The row is the
        // channel's federation-relay authorization record — which peer nest may
        // pull this channel's application messages
        // (`fauna.federation.channel.fetch`; `direct-messages.md` § Technical
        // Flow — Cross-Nest step 3) — owned by the channel and meaningless off
        // the box that serves it. `home_nest_id` and `nest_url` are that peer's
        // address, not the member's data.
        //
        // The exporter loses nothing: their own membership rides `actor_channels`
        // (`Verbatim`, this cluster).
        //
        // ⚠ Pinned on the BYTES by
        // `a_foreign_channel_membership_never_reaches_the_archive`, because
        // an earlier pass established no registry guard notices a withholding class
        // being flipped. That pin seeds SYNTHETICALLY and says so — which the
        // succession ruling above forbids for its own axis, and rightly, since a
        // `Move` pin on an unreachable row would *manufacture* the observable it
        // claims. The asymmetry is the direction of the claim: this pin asserts
        // a WITHHOLDING, so passing it means only "the walk honours the verdict",
        // never "production can reach this state".
        export: Export::WithheldOperational(
            "FOREIGN membership, and the `feed_contributors` shape one plane over: the \
             `actor_id` column names an actor homed on ANOTHER nest, because the sole \
             producer (`register_foreign_channel_member`) fires only on the \
             federation-relay branch of welcome delivery -- a local recipient takes \
             `register_actor_channel` instead. So a local exporter matches no row here. \
             The class holds independently of that vacuity: the row is the CHANNEL's \
             relay-authorization record naming which peer nest may pull the channel's \
             application messages, not the member's own data, and `home_nest_id` / \
             `nest_url` are that peer's address. The exporter loses nothing -- their own \
             membership rides `actor_channels`",
        ),
    },
    ActorTable {
        table: "contacts",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Bespoke(
            "not a move but an ABSORB: a local user may already hold an edge to the successor, and the retired id's own edge to its successor would become a self-edge (it dies). Both sides collapse through `collapse_contact_edges`, which merges a colliding pair on the stricter-status rule (blocked absorbs; else confirmed > accepted > pending -- `succession-aftermath.md` s. Propagation, Contacts) before dropping the superseded row, so edges are neither duplicated nor trust-widened, and the reconcile's live-terminal collision merges instead of aborting on the (actor_id, peer_id) primary key. `record_peer_succession` runs the identical peer-side collapse for a foreign peer -- the home nest is peer zero.",
        )),
        export: Export::Shaped {
            domain: "contacts.json",
            covers: &["peer_id", "status", "accepted_at", "created_at"],
            reason: "full coverage verified 2026-08-15, and machine-checked \
                     since 2026-08-16: `ContactExport` carries every column \
                     except actor_id, which is the exporter themselves. No row \
                     filter — `list_contacts_full` reads every row",
        },
    },
    ActorTable {
        table: "content_links",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Partial(
            "only UNDELIVERED delivery links move — an undelivered payload is access \
             the successor drains with the key material it holds, while a delivered \
             one is history. The row rule is owned by \
             `successions::tests::pending_deliveries_move_and_delivered_history_stays`",
        ),
        // EXPORT RULING (the shaped-domain plane). The
        // edge table `inbox.json` is actually built from — `list_inbox_all`
        // joins it to `content` — and the domain sees one link_type of many:
        // it filters `link_type = 'delivery'`, so this actor's reposts and
        // every other edge kind they own are absent, and within the delivery
        // slice it collapses `status` to the boolean `delivered` and drops
        // `metadata`, `updated_at` and `modseq`.
        //
        // `metadata` is the one column that needed a read rather than a
        // reflex: it is a JSON side-record on the delivery link, and it was
        // deliberately built to hold routing only — `push_inbox`'s own pin
        // (db/mod.rs:5767) asserts a delivery link's metadata carries
        // `mailbox` and *neither* `subject` *nor* `sender`. So the column is
        // this actor's own mailbox routing, not a plaintext leak of sealed
        // mail, and it rides.
        //
        // The rows are this actor's edges by construction (`actor_id` is the
        // recipient on a delivery, the actor on every other kind); the content
        // they point at is ruled on `content` itself.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "current_key_blobs",
        column: "author_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The wrapped distribution of a period key to the tier's subscribers.
        // Moves with its tier -- a blob whose tier moved and whose row did not
        // is a version subscribers can no longer resolve. Member of the
        // `subscription_tiers` family; see the block at that entry.
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldSecret(
            "`blob_data` is the WRAPPED distribution of the tier's period key -- \
             the paywall secret, one wrapping out, which is the case the enum's \
             discriminator rules withheld rather than the sealed-content case it \
             rules exportable. The key seals the author's whole paid archive, so \
             exporting it would hand a file that archive in openable form under \
             the endpoint's weakest credential. The archive's CONTENT is what the \
             export owes the author, not the keys that gate it for everyone else",
        ),
    },
    ActorTable {
        table: "custody_hosting",
        column: "host_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a PROJECTION of the host's client-sealed ceremony records (`HeldCustody` \
             in `fauna.state.custody-ceremony`), which are the authority and which the successor's client \
             re-deposits by the hosting reconcile (the register arm re-verifies \
             desired-vs-listed at each ceremony drive pass), so nothing here is lost \
             by deleting it. It must not MOVE, for exactly `backup_destinations`' \
             reason: a row is thief-writable directly over \
             `fauna.custody.hosting.register` without ever appearing on the account plane, \
             and a moved planted row would keep the successor's nest running a \
             standing outbound pull + hold for an attacker-chosen owner/URL that no \
             client-side mark can raise",
        ),
        // EXPORT RULING. The registered half is the host's own accepted
        // custody-hosting topology (grant id, owner id, the owner-signed
        // witness — a signed public artifact the ceremony handed this account
        // — the owner's nest URL, an endpoints snapshot the ceremony likewise
        // delivered, a budget); the metering half is this nest's own counters.
        // None of it is secret or another party's confidence beyond what the
        // ceremony deliberately conveyed to this account, and none of it is
        // re-derivable from the archive's other contents (the authority is
        // client-sealed plane ciphertext the export does not open), so
        // withholding it would drop the host's hosting topology entirely —
        // the same shape as `backup_destinations` directly above it in this
        // family.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "custody_receipts_staged",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a transit buffer of custodian-signed receipts for grants the ceremony \
             re-mint sweep retires with the old identity — a staged receipt can \
             never verify against the successor's re-minted accept, and the \
             successor's fresh ceremonies earn fresh receipts within a day (the \
             custodian nest's own cadence). Deleting loses nothing recreatable-by \
             anyone: the durable copy is the folded `custodian-endpoints` row in \
             the owner's own sealed plane",
        ),
        // EXPORT RULING. Transit staging, not resting truth: the fleet fetches
        // each receipt at sync and folds it into the sealed
        // `custodian-endpoints` registry row, which reaches the archive
        // through the owner's own sealed plane on that plane's verdict — so
        // the archive's reader already holds the folded copy of everything
        // durable here, and the buffer itself is superseded on the
        // custodian's daily cadence.
        export: Export::WithheldOperational(
            "transit staging of custody receipts the fleet folds into the sealed \
             `custodian-endpoints` row at sync -- the folded copy reaches the \
             archive through the owner's own sealed plane; this buffer is \
             re-earned within a day by the custodian nest's own attestation \
             cadence",
        ),
    },
    // Despite reading as a device row this is the SUBSCRIPTION plane's:
    // `subscription_handlers::delegate_upload_handler` stores an author's
    // `ManageSubscribers` delegation here. Its ruling was deliberately deferred
    // by the sync/device pass (2026-08-13) until the entitlement plane it
    // belongs to was settled — a delegation whose grantor's tiers stayed behind
    // is coherent, one whose tiers moved is not. Those tiers moved
    // (`subscription_tiers` family, 2026-08-13), so it is ruled here with the
    // rest of that plane's non-FK remainder.
    ActorTable {
        table: "device_authorizations",
        column: "author_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Ruled Burn 2026-08-14, from the two facts the deferral recorded so the
        // recon would not be repeated. A `Move` was already known-wrong: the
        // stored envelope is signed by the PREDECESSOR and names it as
        // `actor_id`, re-verified from the served bytes, so a moved row would
        // serve a cert under a live name that authorizes nothing the successor
        // owns. And the grantee is deployment-fixed rather than attacker-chosen
        // (`delegate_upload_handler` refuses any `device_key` that is not this
        // nest's own public key), which is the `spam_preferences` test and puts
        // the row in the benign direction.
        //
        // **The benign direction settles the severity, not the verdict.** What
        // is left on a `Stay` is a signed capability envelope, PUBLICLY served
        // by `GET /api/v1/subscriptions/delegate/{author_id}` (a federation
        // bootstrap read, no auth), plantable by a seed thief, and unrevocable
        // from every app — the write path is upsert-only, with no list or remove
        // gesture anywhere, so the successor can neither see it nor take it
        // down. That is the `nest_pairings` breach of `nest/common.md`
        // § Client-state recoverability, and the same reasoning that burns a
        // pairing burns this.
        //
        // **And the burn costs nothing, which is what makes it the cheap
        // ruling rather than the bold one.** Nothing resolves through the table
        // to verify history: `verify_authoring_envelope` reads the cert attached
        // to each payload (`signer_auth`), never this row, so burning it cannot
        // invalidate a single signed thing the predecessor authored. The
        // successor is equally without a delegation under BOTH rulings (the row
        // is keyed on the author, so nothing was ever going to sit at the new
        // id), and its client uploads its own on next use. `Policy::Purge`
        // already says this box does not consider the row a record worth
        // keeping.
        succession: Succession::Burn(
            "an identity-signed `ManageSubscribers` delegation, served PUBLICLY and \
             unauthenticated by `GET /subscriptions/delegate/{author_id}` and plantable \
             by a seed thief -- and unrevocable from every app, since the write path is \
             upsert-only with no list or remove gesture, the `nest_pairings` breach of \
             `nest/common.md` § Client-state recoverability. A Move is known-wrong (the \
             envelope names the PREDECESSOR as `actor_id` and is re-verified from the \
             served bytes, so it could authorize nothing of the successor's). The burn \
             destroys nothing: `verify_authoring_envelope` reads the cert attached to \
             each payload, never this row, so no signed history depends on it, and the \
             successor -- which had no row under either ruling -- re-uploads its own",
        ),
        // The export cannot leak what the world can already GET. The succession
        // ruling above establishes the operative fact: `payload` is served
        // PUBLICLY and unauthenticated by
        // `GET /api/v1/subscriptions/delegate/{author_id}`, hex-encoded, as a
        // federation bootstrap read — `get_delegation` in `subscription_routes.rs`
        // takes an actor id off the path and returns the envelope, no auth. An
        // archive carrying it is strictly narrower than the live surface.
        //
        // ⚠ Despite the name this is the SUBSCRIPTION plane's `ManageSubscribers`
        // delegation, not a device credential; `device_key` is this nest's own
        // public key (`delegate_upload_handler` refuses any other), so nothing
        // in the row is private to anyone. And the envelope's authority derives
        // from the identity signature it carries, not from possessing a copy —
        // which is why serving it publicly is sound in the first place.
        export: Export::Verbatim,
    },
    // A second address→actor routing namespace (`local_part@domain → actor`),
    // ruled as the route it is. ⚠ It has **no production writer or reader
    // today** — `validate_recipient` and delivery both resolve through
    // `account_aliases`, and every caller of `assign_domain_user` /
    // `resolve_email_address_by_domain` is a test. It is ruled `Move` rather
    // than left un-ruled precisely because that is the state
    // `account_aliases` was in when it cost a security property: a routing
    // table nobody was watching. If it is ever revived, an address follows the
    // account by declaration instead of by someone remembering.
    ActorTable {
        table: "email_domain_users",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — which local-part this actor
        // holds on which mail domain: the account's own address, in the table
        // that reserves it. Nothing here is secret, and every column is about
        // the exporting actor by construction (`UNIQUE (domain, actor_id)`).
        export: Export::Verbatim,
    },
    ActorTable {
        table: "email_filters",
        column: "owner",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Partial(
            "user-authored rules are the successor's own configuration and move, except \
             where the rule is standing authority to make this nest EMIT \
             attacker-authored content outward under the successor's recovered \
             identity — authority of exactly the bunker class, authorable with the \
             account key a seed thief read. Two actions are in that class and both are \
             DELETED: `Forward` (exfiltration — its already-queued copies are ruled on \
             `forward_queue` and `outbound_mail_queue`) and \
             `AutoReply`, which is the LIVE one — the MTA composes and sends the reply \
             after local delivery, `From:` the recipient's address and DKIM-signed \
             under their domain, so a moved rule keeps sending the thief's subject+body \
             to everyone trying to reach the recovered person, with no end date. \
             Deleted, never disarmed in place: `action_from_string` falls back to \
             `Discard`, so a rewrite that missed would turn a forward into silent mail \
             destruction. `Reject` is the third disposition — its reason is \
             thief-authorable text on a 550, but burning the rule would WIDEN who may \
             reach the successor (the `inbox_modes` asymmetry inverted), so it moves \
             with the reason cleared, which the perimeter already renders as a generic \
             message. `Discard` deliberately MOVES though a thief-armed one destroys \
             future mail: burning it re-admits mail the user chose to drop, its harm is \
             prospective-only, and the successor's own filter list is its remedy. \
             ⚠ The split is ruled by CLASS in `email_handlers::filter_succession`, which \
             matches the action enum EXHAUSTIVELY, so a new action variant cannot ship \
             un-ruled; do not re-introduce an `action LIKE` list here. The row rule is \
             owned by \
             `successions::tests::outward_emitting_filters_die_at_the_ceremony_and_the_rest_move`",
        ),
        // EXPORT RULING (2026-08-16) — user-AUTHORED mail rules:
        // the name, the match rules, the action, the priority. The succession
        // reason above opens by calling them "the successor's own
        // configuration", and configuration a user wrote is the archetype of
        // what an export exists to carry — it is also the only copy, since the
        // rules are composed in the app and rest only here.
        //
        // ⚠ Note the axes diverge again, and in the direction that surprises:
        // succession DELETES the outward-emitting rules (a moved `AutoReply`
        // keeps answering everyone with the thief's text). Export carries all
        // of them, because a rule in an archive emits nothing — it is a
        // description of what the user configured, not standing authority. The
        // distinction to keep: succession asks *what will this row DO next*,
        // export asks *whose data is it*.
        export: Export::Verbatim,
    },
    // SUCCESSION RULING (2026-08-14): the explicit-act log is consulted
    // about what counts NOW, not only about what happened — the trend export's
    // k-anonymity gate counts DISTINCT actor_id (`export_trend_entries`), so an
    // un-moved log counts one human as two engagers and a below-k pattern
    // crosses the wire with fewer real people behind it; and the like-toggle
    // dedup is an already-counted constraint a self-service ceremony must not
    // shed (the second-question rule). Declared residual, deliberate: toggle
    // event ids are content-addressed to the identity that acted
    // (`compute_toggle_event_id`), so a successor cannot retract a
    // pre-ceremony like and a re-like double-counts one counter tick per
    // (post, type) — bounded, decays with trending, repairable by a bespoke
    // event-id re-key if it ever matters.
    ActorTable {
        table: "engagement_events",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the owner's own engagement:
        // which content they liked, replied to, reposted or viewed, and when.
        // These rows are the SOURCE the aggregate counters on `content_meta`
        // are derived from, not the other way round, so withholding them would
        // withhold the record and keep the summary.
        //
        // ⚠ `actor_id` is nullable — anonymous/unattributed events carry NULL
        // and therefore match no actor's export read at all, which is the
        // shape already met on `foreign_recovery_heads` and on
        // `outbound_mail_queue`: what rides is only the attributed subset, and
        // it is attributed to the exporting actor by definition.
        export: Export::Verbatim,
    },
    // ⚠ The row's old comment said this table was "already covered by
    // `delete_eviction_tokens`". That is true of the DELETION axis only — that
    // function runs on eviction-cancel and account-deletion, and no succession
    // path has ever called it. On this axis the table was un-ruled and the rows
    // survived the ceremony.
    ActorTable {
        table: "eviction_tokens",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // A BEARER credential that exports a whole account, and the one table in
        // this cohort where `Move` is the actively dangerous direction rather
        // than `Stay`.
        //
        // `GET /api/v1/export` takes this string **instead of** an identity:
        // when the Authorization header is not a session token it falls back to
        // `validate_eviction_token`, which hands back an actor id with no
        // identity check at all, and the handler then zips that actor's entire
        // account. So the refusal plane never sees the caller — the
        // *"does anything that honours these rows read an actor?"* question,
        // answered no.
        //
        // The token's value therefore **follows the data**, and the data moves.
        // A seed thief can read the string before the ceremony (their app shows
        // it — `fauna.account.status`'s eviction fields), so a moved row aims a
        // credential the thief already holds at the successor's whole corpus.
        // Left behind it is merely useless (it would export an account whose
        // corpus just re-pointed away), but "useless" is not the same as "gone",
        // and the successor can neither see nor revoke it.
        //
        // Nothing irrecoverable dies: the token is a means of access, not data.
        // Its one legitimate holder is an account under eviction, who after a
        // ceremony exports the ordinary way as the successor — and if the
        // eviction window still needs one, the admin re-issues, which is the
        // bounded, visible re-grant this axis prefers everywhere.
        succession: Succession::Burn(
            "a BEARER credential that exports the whole account: `GET /api/v1/export` \
             falls back to `validate_eviction_token`, which returns an actor id with no \
             identity check, so the refusal plane never sees the caller. Move is the \
             dangerous direction here -- the token names whatever actor it is bound to, \
             and a seed thief can read the string from their own app before the \
             ceremony, so carrying it aims a credential the thief holds at the \
             SUCCESSOR's corpus. Nothing irrecoverable dies (it is access, not data); \
             an account still under eviction exports as the successor, or the admin \
             re-issues. ⚠ `delete_eviction_tokens` covers the DELETION axis only -- no \
             succession path calls it",
        ),
        export: Export::WithheldSecret(
            "the rows ARE live bearer credentials for this very endpoint (the \
             succession reason above: a token exports the whole account with \
             no identity check). Exporting them would re-mint the credential \
             into a file — an export must never mint a new resting place for \
             a secret",
        ),
    },
    // SUCCESSION RULING (2026-08-14): the per-account per-day feature
    // quota buckets (`try_spend_feature_usage` reads them for the verdict) —
    // the ratified "a limit a self-service ceremony sheds is not a limit"
    // class, verbatim. The inherited cost is bounded and self-clearing: one
    // UTC day's consumption, zeroed by the rollover and pruned by day.
    ActorTable {
        table: "feature_usage",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the account-record plane).
        // Per-day metering counters, `(feature, dimension, day, amount)` — the
        // rate/quota accounting the nest does to decide what it will serve
        // next. `mail_list_account_daily_counter` was withheld on exactly
        // this reading and this is the same class: a counter is the ENFORCEMENT
        // side of a limit, not a record of what the user did. ⚠ Worth stating
        // because the name invites the opposite reading: this is not a usage
        // HISTORY the owner would recognise as theirs — the underlying acts
        // (posts, messages, uploads) each reach the archive through their own
        // tables, and what is left here is the tally the limiter reads.
        export: Export::WithheldOperational(
            "per-day metering counters (feature, dimension, day, amount) -- the rate/quota \
             accounting the nest consults before serving, the `mail_list_account_daily_counter` \
             class. The acts being counted reach the archive through their own \
             tables; a limiter's tally is enforcement state, not the owner's record",
        ),
    }, // deletion axis already covered by delete_user; kept for enumeration completeness
    // ⚠ **DELIBERATELY NOT RULED by the curation pass, and the
    // reason is that this column is in the wrong list.** `author_id` does not
    // name the row's owner: a `feed_contributors` row belongs to a **feed**,
    // whose owner is `feeds.owner` one entry below, and `author_id` names the
    // *other* person — a contributor whose posts that feed collects. Every
    // production writer confirms it (`discovery.rs`'s poll and referral arms,
    // `feed_routes::add_contributor_core`'s manual grant): each pairs the id
    // with a peer `nest_url` the discovery loop then polls, and
    // `discovery.rs`'s `SELECT nest_url FROM feed_contributors WHERE author_id`
    // reads it back as "where does this author live". So the succession
    // question here is not *what happens to my rows* but *what happens to a
    // reference when the COUNTERPARTY succeeds* — the class, which
    // `content_reports.reporter`, `knocks.sender_id` and `notifications.sender_id`
    // are already queued under, to be graded once and coherently rather than a
    // sixth time in a pass whose subject was the curation owner's own rows.
    //
    // ⚠ Stated because it is a finding and not this axis's to fix: the
    // *deletion* axis reads the same column as ownership (`Policy::Purge` — a
    // purge on `author_id` deletes rows out of a **different** owner's feed),
    // which is the exact shape `succession-aftermath.md` § Re-key scope calls
    // out as correctly excluded there ("purging on it would delete a row X does
    // not own"). ⚠ **Row 91 closed 2026-08-15 WITHOUT settling it, deliberately:
    // it is the deletion axis's question, not this one's, and it is still owed
    // to that axis's owner.** The entry stays put here for the reason the
    // paragraph above gives — re-filing it out of `ACTOR_TABLES` would change
    // the deletion behaviour as a side effect of a succession ruling.
    //
    // SUCCESSION RULING (2026-08-15) — **`Move(Plain)`**, on the
    // counterparty rule: the discovery read named above resolves *where this
    // author lives*, so a `Stay` silently stops that person's posts reaching a
    // feed whose owner never asked for the change and cannot see why, while the
    // handle the feed owner knows them by moved with the succession. No
    // authority is conferred and the polled content is public, so the move is
    // also the low-risk direction.
    ActorTable {
        table: "feed_contributors",
        column: "author_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the subscription plane). ⚠ The
        // one table in this cluster that is NOT the exporting actor's data,
        // and it reads exactly like the five that are. The name and the
        // `author_id` column both suggest "the author's feed contributions";
        // `upsert_contributor` (db/feeds.rs:436) says otherwise — a row is a
        // contributor a DISCOVERY feed found, keyed `(feed_id, nest_url,
        // author_id)`, and the feed's own owner is `feeds.owner`, a different
        // column in a different table. So an actor-scoped read here returns
        // rows in which this actor is the DISCOVERED party inside somebody
        // else's feed: which local feeds picked them up, how often
        // (`hit_count`), how recently (`last_seen`), by what route
        // (`discovered_via`) and at what `poll_priority`.
        //
        // The same shape as `foreign_recovery_heads`, one plane over — except
        // that one was unreachable by construction and this one matches real
        // rows, so a `Verbatim` here would actually emit. It would disclose one
        // user's discovery-feed composition and behaviour to a different user,
        // and it would answer a question the exporter has no standing to ask.
        // The same rule applies: a verdict is a claim about the
        // CLASS of a plane, not only about whether bytes escape.
        //
        // `WithheldOperational` rather than a two-party carve-out because that
        // is honestly what the row is — the discovery engine's own crawl
        // bookkeeping, owned by the feed, meaningless off the box that polls.
        // Pinned on the BYTES by `a_foreign_feeds_contributor_rows_never_reach_
        // the_archive` (the `the_mail_spools_never_reach_the_archive` shape),
        // because nothing in the registry guards
        // catches a withholding class being flipped.
        export: Export::WithheldOperational(
            "the discovery engine's crawl bookkeeping, owned by the FEED and not by this \
             actor: `upsert_contributor` keys a row `(feed_id, nest_url, author_id)` where \
             `author_id` is a contributor the feed DISCOVERED, while the feed's owner is \
             `feeds.owner` -- so an actor-scoped read returns rows about the exporter sitting \
             inside other users' feeds (which feed found them, `hit_count`, `last_seen`, \
             `discovered_via`, `poll_priority`). Exporting it would hand one user another \
             user's feed composition and crawl behaviour. The exporter loses nothing they \
             authored: their own feeds ride `feeds` (Verbatim) and their posts ride \
             `content`",
        ),
    },
    // SUCCESSION RULING (2026-08-15) — the `web_domains` stranding
    // shape, and the FIRST table to carry it on a **nest-wide published**
    // object rather than a DNS name.
    //
    // A feed is browsable by everyone: `fauna.feed.list` → `list_feeds()` is
    // unfiltered and returns each row's `owner` hex to any caller. Management is
    // owner-scoped in every door — `update_feed` and `delete_feed` both carry
    // `AND owner = ?`, `list_feeds_by_owner` is the app's "my feeds". Left
    // behind, the two halves part company exactly as `web_domains` did: the feed
    // keeps appearing in the public catalog under a retired identity, the
    // successor cannot see it, edit it, or delete it, and the retired key is
    // refused everywhere — so **nobody can ever remove it again**. That is a
    // `nest/common.md` § Client-state recoverability breach, not a preference
    // loss, and it is reached by an ordinary self-service ceremony.
    //
    // The *limits* half points the same way (§ Re-key scope's limits-move rule):
    // `feed_routes` gates creation on `count_feeds_by_owner(owner) >=
    // get_user_tier_max_feeds(owner)`, so a left-behind set gives the successor
    // a fresh `max_feeds` allowance while the old feeds still exist and still
    // cost the nest their discovery polling. Moving is also what keeps a
    // *discovery* feed's outbound work attributable to somebody who can stop it.
    //
    // Nothing is stranded on the other side: `feed_contributors` hangs off
    // `feed_id`, which no leg touches, so the contributor set travels with the
    // feed without either table naming the other's actor.
    ActorTable {
        table: "feeds",
        column: "owner",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the shaped-domain plane). ⚠ This
        // table is the reason the new `Shaped { covers }` guard exists, and it
        // very nearly got the wrong verdict. `feeds.json` carries exactly the
        // six columns of the ORIGINAL `CREATE TABLE`, so a reader who checks
        // the migration block — the obvious way to verify a coverage claim —
        // sees a perfect match and rules `Shaped` in one line. The table has
        // since grown `scope`, `contributor_seeds` and `composition` by
        // `ALTER`, none of which the domain carries: a feed's local-vs-
        // discovery scope, the peer nest URLs it exchanges with, and its
        // composition are the substance of what the user built, and
        // `rules_hex` without them describes a different feed. The lesson
        // generalises past this row — a coverage claim
        // verified against `CREATE TABLE` is verified against the schema of
        // the day the table was born.
        //
        // Everything here is the owner's own curation and nothing addresses a
        // third party (`owner` IS the exporter), so the row rides whole rather
        // than the domain being widened three times and left to rot again.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "folder_member_access",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING (2026-08-14). A grant, so the question is
        // the right one to ask — but it is a grant IN SOMEONE ELSE'S set, minted
        // only by the set owner (`set_folder_member_access`, behind the
        // owner+claimant-gated `fauna.folders.members.set_access` and the
        // share-time `access` param). Nothing the successor does can mint this
        // row, which is what makes it participation rather than reach.
        //
        // The propagation leg is the one that moves the SEAT (`writer-signed-
        // change-records.md` ruling (8)(j)(2)–(4), 2026-10-02, replacing "the
        // owner re-shares"): for a local member, the claimant's Welcome that
        // seats the successor (`register_successor_carrying_seat`); for a
        // cross-nest member, `record_peer_succession`, alongside the
        // `channel_foreign_members` row. On a collision the successor's own
        // grant stands. The owner's `set_access`/`evict` override either.
        //
        // ⚠ The load-bearing detail is that it must AGREE with its roster twin,
        // because the write gate is a CONJUNCTION: `resolve_writable_folder`
        // admits only a caller who is both on `actor_channels` AND holds an
        // `access = 'writer'` row here. Moving this table alone splits the pair
        // into two useless halves — the successor holds a writer grant with no
        // roster row (admits nothing) while the predecessor keeps the roster row
        // stripped of its grant (silently downgraded to reader). A verdict here
        // is not independently choosable; it is `actor_channels`' verdict.
        //
        // On the limits rule (a limit a self-service ceremony sheds is not a
        // limit): `bytes_used` is a consumed quota, but it is charged against
        // ANOTHER owner's `byte_cap`, and shedding it requires that owner to
        // re-share. Gated on a third party, so the inversion does not reach it.
        succession: Succession::Stay(
            "the writer-grant half of a CONJUNCTION with the `actor_channels` roster \
             (`resolve_writable_folder` requires both), so it takes that table's verdict \
             rather than its own: moving it alone yields a grant that admits nothing and \
             a roster row stripped of its grant. Participation in someone else's set — \
             the row is minted only by that set's owner via \
             `fauna.folders.members.set_access`, and it is carried to the successor by \
             the leg that moves the seat (the claimant's add-successor Welcome for a \
             local member, `record_peer_succession` for a cross-nest one; ruling \
             (8)(j)(2)-(4)), never by this transaction. The limits-invert rule does not reach `bytes_used`: it is consumed \
             against another owner's cap, so shedding it is not self-service",
        ),
        // EXPORT RULING (the sync plane). The same
        // membership check applies here, and it points the RIGHT way: `actor_id` is the
        // GRANTEE, so an actor-scoped read returns the grants this actor HOLDS
        // and never the other members' rows in the same set. (The set's own
        // owner is not a column in this table at all — that is
        // `folder_channel_claims.claimed_by`, one table over.)
        //
        // A grant ledger the holder is entitled to audit, on the
        // `capability_grants` precedent in this same file: `principles.md` § The
        // user always controls their data puts audit of grants in the user's own
        // app, and every column is the holder's own side of one — `access`
        // (reader or writer), `byte_cap` (the ceiling this set's owner set for
        // them), `bytes_used` (what they have spent under it), `updated_at`.
        // Withholding it would leave the exporter unable to answer "what may I
        // do in the sets I was invited to, and how much of my allowance is
        // left".
        //
        // ⚠ The succession `Stay` above is INHERITED from `actor_channels`
        // because the write gate is a conjunction. Disclosure is not inherited —
        // it is asked and answered per table — and it happens to land the same
        // way here, which is worth stating so the next reader does not take the
        // agreement for a derivation.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "folders",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the shaped-domain plane). The
        // widest gap the shaped half has, and the narrowing happens in a place
        // worth naming: `get_folders_for_actor_full` reads the table's columns
        // through `folder_columns!()` and hands back a full `FolderRow` — it
        // is `FolderExport`, the JSON struct, that keeps four of them (`id`,
        // `name`, `created_at`, `node_cache`) plus the two
        // nested lists. So the data reaches the export code and is dropped on
        // the way out, which is why reading the DB layer would have cleared
        // this table wrongly. Absent from the archive: `mode`,
        // `conflict_policy`, `webdav_enabled`, `web_paywall_tier`,
        // `retention_policy`, `nest_snapshots`, `nest_snapshot_quiet_secs`,
        // `high_cadence`, `include_paths`,
        // `exclude_paths`, `mls_group_id`, the cached-stat trio and the four
        // sealed labels. Most are knobs the user set in their own app.
        //
        // The sealed labels are the sharpest of them and the enum's
        // sealed-columns clause is what admits them: `name_sealed`,
        // `include_paths_sealed`, `exclude_paths_sealed` and
        // `retention_policy_sealed` seal this actor's own filesystem layout
        // under this actor's own root, so they ride in at-rest form and the
        // archive's reader is exactly who holds the key. `mls_group_id` is a
        // group IDENTIFIER, not the group's key material (which rests in
        // `bridge_wrapped_mls_blobs`, `WithheldSecret`) —
        // the governing test asks what the bytes ARE, and these open
        // nothing.
        export: Export::Verbatim,
    }, // already covered by delete_all_folders_for_actor; kept for enumeration completeness
    // The peer-side twin of `recovery_registrations`, and it stays for a
    // stronger reason than its sibling: this row is not a record of what
    // happened, it is the **baseline the next verification is measured
    // against**. `verify_succession_against_chain` is handed the head learned
    // for the OLD identity as `known`, so a later statement must EXTEND what
    // this box already saw; the monotonic-seq guard and the write-once
    // `anchor_nest_id` beside it exist for exactly that. Moving the head onto
    // the successor erases the baseline in the act of using it, which re-opens
    // the downgrade those two guards were written to close. What a peer needs to
    // know about the successor is `actor_successions`, which the same pull path
    // writes; this table is deliberately the *old* identity's watermark.
    //
    // ⚠ Also the REACHABILITY shape, stated rather than assumed away: these
    // rows name identities this nest does NOT host (the succession-pull path
    // writes them for foreign actors), while the ceremony that would execute a
    // ruling only ever names a locally registered old actor — so in practice no
    // ceremony reaches one. Ruled as if reachable, which is the safe direction
    // and what that finding prescribes.
    ActorTable {
        table: "foreign_recovery_heads",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "an ANTI-DOWNGRADE watermark, not a record: the head learned for the \
             old identity is what the next statement must extend, so moving it \
             erases the baseline in the act of using it and re-opens the \
             downgrade the monotonic seq and write-once anchor exist to close. A \
             peer learns of the successor through the succession row instead. \
             Also unreachable by construction — these rows name foreign \
             identities, and the ceremony only ever names a local one",
        ),
        // ⚠ **The actor column here does not name the exporting actor.** As the
        // succession reason beside it says, these rows name FOREIGN identities:
        // one per remote actor, the `(recovery_pubkey, seq)` head this nest last
        // verified from that identity's home nest. So a row keyed by a local
        // actor is unreachable by construction, and the honest verdict is about
        // the CLASS, not the (nil) leak — a `Verbatim` here would declare a
        // plane exportable that is not the owner's data at all, and would say so
        // in the manifest's coverage object.
        //
        // What it is instead is an anti-downgrade watermark this box keeps in
        // order to serve: nest-internal bookkeeping, meaningless off-box, and
        // authoritative nowhere but the subject's own home nest, which serves
        // the chain itself. Nothing is withheld from the owner by saying so —
        // their own chain rides `recovery_registrations`, `Verbatim`.
        export: Export::WithheldOperational(
            "not the exporting actor's data at all: one row per FOREIGN identity, holding \
             the (recovery_pubkey, seq) head this nest last verified from that identity's \
             home nest as an anti-downgrade watermark. A row keyed by a local actor is \
             unreachable by construction (the same fact the succession ruling turns on), \
             and the watermark is nest-internal serving state -- the authoritative chain \
             lives on the subject's own nest. The owner's OWN chain rides \
             `recovery_registrations`",
        ),
    },
    ActorTable {
        table: "forward_queue",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Partial(
            "forwards parked by the rate cap — exfiltration already in flight. Leaving \
             them is NOT the status quo the rest of the backlog enjoys: \
             `promote_due_forwards` walks `distinct_forward_queue_actors()` and is \
             succession-blind, so a row parked on the retired identity is still \
             dispatched to the thief. The split is by what a burn would destroy, keyed \
             on the row's persisted `copy_mode`, never its class: a `copy` row BURNS, \
             whatever rule armed it — its destination is thief-chosen and the original \
             rests sealed in the mailbox, so only the copy dies — while a `redirect` \
             row, or one with no persisted mode, MOVES, because a redirect keeps no \
             local copy and its row is then the only copy of a message this nest \
             already answered 250 for. No class stands in for the mode: forward-all \
             follows the message and is sent as `redirect` whenever a redirect rule \
             fired. The row rule is owned by \
             `successions::tests::a_parked_forward_copy_burns_at_the_ceremony_and_a_redirect_moves`",
        ),
        // EXPORT RULING (2026-08-16) — a TRANSIENT SPOOL, which is
        // the "delivery bookkeeping" case this variant names. A row exists only
        // between the rate cap parking a forward and `promote_due_forwards`
        // dispatching it; the message it carries is not stored here in any
        // durable sense.
        //
        // ⚠ **And the second argument is the one that would decide it alone:
        // `raw_message` is a full RFC822 message in PLAINTEXT at rest.** The
        // owner's durable copy of that same mail rests sealed in their mailbox
        // (the succession ruling above turns on exactly that: "the original
        // rests sealed in the mailbox, so only the copy dies"). Exporting the
        // spool row would therefore mint an UNSEALED copy of mail whose real
        // resting place is sealed, into an archive retrievable with an eviction
        // token — the weakest credential the endpoint takes. A withheld
        // transient copy costs the owner nothing; an exported one weakens the
        // resting posture of mail they already have.
        export: Export::WithheldOperational(
            "the rate-cap parking lot for outbound forwards -- a transient spool row that              exists only until the promoter dispatches it, i.e. delivery bookkeeping. Its              `raw_message` is a full RFC822 message in PLAINTEXT, while the owner's durable              copy of that same mail rests SEALED in their mailbox, so exporting the spool              would mint an unsealed copy of already-held mail into an archive an eviction              token can fetch",
        ),
    },
    ActorTable {
        table: "generation_escrow_wraps",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "R14 (account-data-plane.md § The ratified decisions) escrow wraps sealed to the PREDECESSOR's identity-derived escrow \
             target — the KEPT WRAP (owner-key-material.md § Rotation, the succession \
             rider → 'The kept wrap', re-ruled 2026-10-01; the first ruling burned \
             them). The ceremony runs with no session, so in the canonical theft no \
             device has keyed a generation first and this row is its last copy: a \
             burn here was a certain, total loss of every pre-succession generation. \
             The row stays under the retired id — the actor id IS the identity the \
             wrap seals to, which a move would erase — and is served to the successor \
             through the chain walk (`generation_escrow.rs`), opened by the successor's \
             seed-holding device under the retired seed, and swept by the successor's \
             first deposit of the same generation, by `escrow.delete` and the belted \
             receipt retire across the chain, and by account deletion's chain Purge.",
        ),
        export: Export::WithheldSecret(
            "R14 escrow wraps -- `recovery_escrow`'s ruling generalised to the \
             per-generation plane: escrow leaves the nest through the recovery \
             ceremony and no other channel, least of all a bulk one a weaker \
             eviction-token credential can pull. Its succession reason above \
             already establishes the wrap is openable from the identity seed, \
             which is exactly the second half an export must not sit beside",
        ),
    },
    // === The supervised side of the family plane (ruled 2026-08-13) ===
    //
    // All twelve move, and they move for ONE reason that is worth stating once
    // here rather than twelve times below: **supervision is an oversight state
    // about the ACCOUNT, not about the key** — precisely the class
    // `record_succession`'s own step 3 already reasons about when it carries
    // `suspended` across ("an admin's moderation verdict is about the account,
    // not the key, so rotating keys must not launder it away"). `suspended` is
    // carried because it is a column on `users`; supervision laundered away
    // because it is a row in another table, and that table was un-ruled.
    //
    // Leaving them behind is not the status quo this axis usually gets to
    // assume. It is the `inbox_modes` asymmetry in its sharpest form, twice
    // over:
    //
    //   * `guardianships` IS the supervised designation — `family-safety.md`
    //     § The guardianship link: *"the supervised designation IS the link
    //     row's existence"*, and every enforcement gate is a
    //     `WHERE EXISTS (SELECT 1 FROM guardianships WHERE
    //     supervised_actor_id = ?1)`. A ward whose link stays behind is simply
    //     not supervised any more.
    //   * and it is **irreversible**: `insert_guardianship_tx` is the only
    //     writer, its only caller is `create_user_with_handle` (admission), and
    //     `family-safety.md` § The guardianship link rules out converting an
    //     existing full account into a supervised one. Nothing can put it back.
    //
    // So an un-ruled family plane made the succession ceremony an unguarded
    // version of the exact gesture § Lifecycle gates *refuses*: a supervised
    // account's own `fauna.account.delete` returns a typed error because it
    // "would unilaterally sever the link", while the recovery ceremony — which
    // is **pre-identity**, authorized by the recovery chain alone with no
    // session, tier or supervision check (`recovery_handlers.rs`) — severed it
    // silently and permanently. A ward holding their own kit could emancipate
    // themselves; a thief who read a ward's seed got an unsupervised account.
    // Moving is what makes § Lifecycle gates true rather than aspirational.
    //
    // The side tables move with the link for a second, independent reason:
    // every one of them defaults to the *unsupervised-equivalent* when absent
    // (the migration's own words), so a partial move silently WIDENS. See each
    // entry for the specific widening it prevents.
    ActorTable {
        table: "guardian_contact_requests",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The ward's pending "may I talk to this person" asks. They move so the
        // guardian's queue survives the ward's ceremony; left behind, the ward
        // must re-ask and the guardian's doorbell has already rung for an ask
        // that now names nobody.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the ward's own asks: which
        // peer they requested contact with, and when. An explicit ward action
        // (the doc refuses to auto-mint one from a refused attempt), so the row
        // is a record of something the ward did. Reaches the ward's archive
        // only.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "guardian_content_notices",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Per-day enforcement counts the guardian's status page renders. Loss
        // only, but it is the ward's own oversight record and re-derivable from
        // nothing.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — (day, category, count)
        // aggregates of what the ward's own client collapsed or blocked. The
        // disclosure is fixed at category + count by
        // `family-safety.md:365` — no content, no content id, deliberately —
        // so what an archive can carry is bounded by the schema itself rather
        // than by this verdict. Derived from the ward's own client decisions
        // and reaching the ward's archive only.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "guardian_dm_peers",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Carries `verdict = 'block'` rows, so leaving it behind is a WIDENING,
        // not a loss: a peer the guardian explicitly blocked reverts to the
        // `unknown_peer_dm` default. The `email_filters::Reject` lesson exactly
        // — the one place in a family where dropping a row re-admits someone
        // the user refused.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the per-(bridge, peer)
        // allow/block verdicts the DM gate reads, and who set each one (the
        // ward's own outbound send, or the guardian). Policy about the ward's
        // own correspondents, which the transparency invariant
        // (`family-safety.md:86`) puts on the ward's side of the line; the peer
        // ids are external handles the ward already sees in their own app.
        // Reaches the ward's archive only.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "guardian_feed_requests",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Pending asks AND the single-use grants they become. A grant confers
        // future access, which is the axis's move-side test — and it passes the
        // `spam_preferences` question too: only the GUARDIAN can approve one,
        // and the target is the one object they saw, so no destination here is
        // attacker-chosen. Burning would re-close access a guardian
        // deliberately opened.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the ward's own feed-source
        // asks and the single-use grants they become (`approved_at` set).
        //
        // ⚠ An approved row IS the grant, which invites the question whether
        // exporting it hands over authority. It does not, and the reason is
        // the same as `backup_writer_grants`'s precedent exactly: **the row's
        // EXISTENCE here is the authority, never knowledge of its id.** The
        // grant is redeemed by the ward's client retrying the operation as the
        // ward, which an archive reader cannot do without the ward's key.
        // Reaches the ward's archive only.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "guardian_mail_allowlist",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The ward's known-sender set. Moves as the account's own routing
        // state; `added_by` keeps the guardian's approvals distinguishable from
        // the ward's own correspondents on the far side.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the ward's known-sender set:
        // seeded from the ward's OWN outbound mail on both submission paths,
        // plus whatever the guardian approved. That makes it the ward's mail
        // correspondence graph — the same class as their contacts — rather than
        // gate machinery, which is why it parts company here with its two
        // neighbours below.
        //
        // ⚠ The line between this table and `guardian_mail_correlated_origins`
        // / `guardian_mail_sent_msgids` is worth keeping: **a set the user
        // would recognise as their own rides; the gate's internal memory does
        // not.** All three are guardianship-guarded side tables written by the
        // same mail gate, so a name- or provenance-based sweep would put them
        // in one bucket and be wrong twice.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "guardian_mail_correlated_origins",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // A NEGATIVE list — the addresses the outbound auto-seed must DECLINE
        // to allowlist. Leaving it behind therefore widens in the quiet
        // direction the axis keeps finding: the successor's auto-seed would
        // permanently allowlist an address the ward merely replied to, which is
        // the precise bootstrap this table exists to refuse.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the gate's NEGATIVE memory,
        // and `guardian_mail_sent_msgids`' companion: the address set of each
        // delivered correlated report, whose only function is making the
        // outbound auto-seed DECLINE those addresses, so the ward's reply to a
        // correlated report cannot allowlist its author. Read through the same
        // 30-day window, refreshed on repeat delivery, pruned opportunistically.
        //
        // A suppression list the ward never sees and could not act on is the
        // variant's own case: nest-internal serving state about the actor. The
        // addresses the ward WOULD recognise — the ones they may hear from —
        // ride `guardian_mail_allowlist`, `Verbatim`.
        export: Export::WithheldOperational(
            "the mail gate's negative memory -- the addresses of delivered correlated              delivery-status reports, kept only so the outbound auto-seed declines to              allowlist them, inside the same 30-day window and pruned opportunistically.              Nest-internal gate state the ward never sees and could not act on; the sender              set they WOULD recognise rides `guardian_mail_allowlist`",
        ),
    },
    ActorTable {
        table: "guardian_mail_holds",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The envelope sidecar for a held message. The held message itself
        // moves with the mail plane, so leaving the sidecar strands it: the
        // guardian's queue cannot render the sender and the approve path has no
        // address to allowlist, making the mail unreleasable by any app.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the envelope sidecar for a
        // message held for guardian approval: who it was from and when. The
        // held message itself is in the ward's own mailbox (the doc is explicit
        // that a hold is the placement, not a quarantine store), so this row is
        // metadata about the ward's own mail; `family-safety.md:365` fixes the
        // disclosure at envelope metadata, so there is no content here by
        // ratified design.
        //
        // ⚠ **`message_id` here is NOT the capability its neighbour's is.** The
        // adjacent `guardian_mail_sent_msgids.message_id` is an unguessable
        // token a delivery-status report must NAME AND SPEND to be delivered
        // rather than held; this one is the id of a message already sitting in
        // the ward's mailbox, and naming it buys nothing. Two adjacent tables,
        // one column name, opposite classes — read the consumer, not the name.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "guardian_mail_sent_msgids",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The correlation a null-reverse-path delivery report must match to be
        // delivered rather than held. Left behind, every bounce for mail the
        // ward sent before the ceremony is held instead of delivered — and the
        // budget column keeps the carried set bounded, so moving it grants no
        // new reach.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the mail gate's correlation
        // memory, and the one member of this family whose export would weaken a
        // guardian control.
        //
        // ⚠ **Two grounds, and the second alone would decide it.** (1) Class:
        // this is the gate's own 30-day bookkeeping — seeded at the two
        // submission chokepoints, pruned on seed, enforced on the probe — not a
        // set the ward would recognise as their data (their sent mail is their
        // mail; this is the index the gate spends against it). (2) Capability:
        // `family-safety.md:106` states that a Fauna-minted Message-ID is
        // **unguessable**, that a null-reverse-path delivery-status report must
        // NAME it to be *delivered rather than held*, and that each correlated
        // delivery atomically **consumes** a budget unit. So the column is a
        // spendable token that buys a message past the guardian's mail gate —
        // and this archive is retrievable with an eviction export token, the
        // weakest credential the endpoint accepts.
        //
        // ⚠ Deliberately NOT `WithheldSecret`, and the distinction matters
        // because the manifest shows the class to the ward: the same goal-doc
        // sentence says threading DISCLOSES the id to correspondents, so this
        // is not credential material the nest is guarding — it is a
        // self-disclosing token with a budget, and the honest class is the
        // gate's own state. The capability is why a `Redacted` keeping the
        // ledger is not worth reaching for either: what would remain is a
        // 30-day count of timestamps and budgets, which is gate machinery
        // wearing a ledger's clothes.
        export: Export::WithheldOperational(
            "the mail gate's 30-day correlation memory: the ward's sent Message-IDs with the              budget each has left. It is gate state rather than the ward's data -- their sent              mail is their mail, this is the index the gate spends against it -- and the              values are SPENDABLE: a null-reverse-path delivery report that names one is              delivered rather than held for guardian approval, consuming a budget unit. An              archive an eviction token can fetch must not carry a token that buys delivery              past a guardian control",
        ),
    },
    ActorTable {
        table: "guardian_policies",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The whole policy document. **Every default is the
        // unsupervised-equivalent** by design, so a link that moved without its
        // policy row would leave the ward nominally supervised with every
        // control silently reset to permissive — worse than either mis-ruling,
        // because both roles' apps would render a supervised account whose
        // enforcement is off.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the policy document, and
        // `family-safety.md:86` rules it the ward's in as many words: the
        // supervised user always sees *what the policy is*; there is no
        // silent-surveillance mode. Every default is the unsupervised-equivalent
        // value, so a row is informative exactly where the guardian tightened
        // something — which is the part the ward is entitled to read.
        // Reaches the ward's archive only (see `guardianships`).
        export: Export::Verbatim,
    }, // also covered by delete_family_rows_for_supervised; kept for enumeration completeness
    ActorTable {
        table: "guardian_transfers",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // A pending transfer proposal is keyed on the ward, so it follows the
        // ward. Its two guardian-side columns are ruled separately in
        // `SUCCESSION_REFERENCES`.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the pending proposal to move
        // this ward to another guardian: who is proposed, who initiated it,
        // when. The transfer is a consent handshake the ward is party to and
        // their app renders, so the transparency invariant
        // (`family-safety.md:86`) covers it; the two guardian-side columns name
        // accounts on the ward's own nest that the proposal already shows them.
        // Reaches the ward's archive only — a proposal never appears in the
        // proposed guardian's export, because their column is a
        // `SUCCESSION_REFERENCES` entry (see `guardianships`).
        export: Export::Verbatim,
    },
    ActorTable {
        table: "guardian_usage",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Screen-time minutes already spent today. Left behind it reads as a
        // fresh day: the ward clears their own daily budget by rotating
        // identity, which is a widening of an enforcement control rather than
        // the loss it looks like.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — screen-time minutes per the
        // ward's local day, reported by the ward's own client. An aggregate of
        // the ward's own activity, carrying no content id by ratified design
        // (`family-safety.md:365` forbids one here, and none exists in the
        // schema). Reaches the ward's archive only.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "guardianships",
        column: "supervised_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The link row — the supervised designation itself (see the block
        // comment above). Its `guardian_actor_id` is the OTHER actor this row
        // names and is ruled separately in `SUCCESSION_REFERENCES`: the two
        // columns answer two different ceremonies.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — **the family plane's
        // structural fact is written here, and the other eleven entries point
        // at it.**
        //
        // ⚠ **Every table in this family keys on `supervised_actor_id`, so the
        // whole family reaches the WARD's archive and never the guardian's.**
        // The guardian-side columns — this row's `guardian_actor_id`, and
        // `guardian_transfers`' `proposed_guardian_actor_id` / `initiated_by` —
        // are `SUCCESSION_REFERENCES` entries, which the export never reads on
        // (`gather_export_set` walks `ACTOR_TABLES` alone). That answers
        // the governing question — *is this the exporting actor's
        // data?* — for twelve tables with one structural sentence instead of
        // twelve judgment calls, and it is what makes the family safe to rule
        // as a plane. `the_guardian_family_exports_only_on_the_wards_own_column`
        // pins it, because a future entry naming a guardian-side column would
        // silently turn this axis into a second disclosure channel beside the
        // guardian-scoped RPCs — one that `family-safety.md:365` bounds and
        // this registry does not.
        //
        // Corollary, so nobody reads the above as a denial: the guardian is not
        // being withheld anything here. Their app reads this family through
        // guardian-scoped RPCs. An "export my family data" surface for the
        // guardian would be a NEW mechanism with its own disclosure bound, not
        // a verdict change on these rows.
        //
        // The row itself: who supervises this account, since when, and the
        // ward's last-reported clamped UTC offset. `family-safety.md:86` makes
        // it the ward's by ratified invariant — *"the supervised user always
        // sees that they are supervised, by whom"* — and it is the single most
        // consequential fact about the account, so an archive silent about it
        // would be actively misleading.
        export: Export::Verbatim,
    }, // also covered by delete_family_rows_for_supervised; kept for enumeration completeness
    ActorTable {
        table: "account_age_bands",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The band follows the identity: a succession replaces the ACCOUNT's
        // key, not the person, so the successor keeps the band exactly as
        // they keep the guardianship link (a succession is not a graduation —
        // graduation is the one ceremony that deletes this row,
        // `family-safety.md` § The account age band).
        succession: Succession::Move(MoveShape::Plain),
        // The account's own band + how it was established — admission
        // metadata about the exporting actor themselves, no other party's
        // column on the row and no content id (the same plane discipline as
        // the guardianship family above).
        export: Export::Verbatim,
    }, // also covered by delete_family_rows_for_supervised; kept for enumeration completeness
    // The successor's own record of their own in-flight mail import — the
    // `notifications` / `actor_message_dedup` class the delivery plane ruled,
    // and its dedup ledger already moves, so a `Stay` splits one mechanism in
    // half: the successor inherits the ledger that says "already imported"
    // with no session that explains it, and no cursor to resume from.
    //
    // Nothing here grants: every door is owner-scoped `WHERE actor_id = ?1`
    // behind `require_class`, so a left-behind row is unreachable rather than
    // dangerous. What decides it is that a left-behind row is also
    // **permanent** — the 30-day expiry sweep is itself actor-scoped
    // (`mail_import.rs`'s two `DELETE … WHERE actor_id = ?1 AND expires_at <`
    // statements, fired only on that actor's own create/list), and the retired
    // identity never calls again, so the one verdict that makes this ephemeral
    // bookkeeping immortal is `Stay`.
    ActorTable {
        table: "import_sessions",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the residual set). The actor's
        // own mailbox-import history: which source, when it ran, how far it
        // got, what errored. Nothing names a third party and nothing is
        // credential material — `source_descriptor` is a source LABEL
        // (`gmail:imap.gmail.com:alice`, the shape the wire fixtures use), the
        // multi-device lock key, never a password; the bearer the import
        // actually uses lives on the bridge side and never rests here.
        //
        // `source_sealed` is a sealed label over that descriptor, convergent
        // under `source_hash`, from the same 2026-07-29 paths-are-content
        // expand phase as `sync_changes.path_sealed` — sealed CONTENT under
        // the exporting actor's own root, riding in at-rest form and cleared by
        // name in `CLEARED_EXPORTING_COLUMNS`. The plaintext sibling is
        // scrubbed where the seal rests (v32) and is the exporter's own label
        // wherever it still does.
        export: Export::Verbatim,
    },
    // The mailbox-export twin of `import_sessions` directly above — and ruled
    // AGAINST that entry on two of its three axes, deliberately. ⚠ Do not
    // "restore the symmetry": `mail-export.md` § Architectural rules makes the
    // two tables share a column shape and a state machine, and that is exactly
    // what makes copying the triple look right. The asymmetry that decides it
    // is that an import only *consumes* (its output is mail, which lives and
    // moves in `bridge_imap_messages`), while an export *produces* an artifact:
    // a sealed whole-mailbox snapshot on nest disk, under a per-session key
    // wrapped for the actor who started it (`mail-export.md` § Key material).
    //
    // **Why `Burn`, argued against the twin's `Move`
    // (`succession-repoint-axis.md`, the "other five" blockquote).**
    // - `Move` hands the successor rows whose blob opens only under a key
    //   wrapped for the RETIRED identity. The successor cannot unwrap it, so
    //   they inherit up to three concurrency slots and tens of GiB of archive
    //   that exactly one party can read — whoever holds the old key, which is
    //   the thief the ceremony exists to lock out. The twin has no such column;
    //   a moved import session is a cursor the successor can resume.
    // - `Stay` dies on the twin's own deciding fact, softened here only in
    //   duration: the retired identity never calls again, so nothing its
    //   owner does ever discards a left-behind row. The periodic expiry tick
    //   (`mail_export_blobs::run_export_expiry_tick`) bounds it at 30 days —
    //   and 30 days of a compromised identity's whole mailbox resting in one
    //   exfiltratable file is still the wrong answer.
    // - So it burns, and it fits the verdict's own definition: a session is
    //   something a seed thief could have minted (the wrap is under the key
    //   they stole), and re-exporting under the new identity is the bounded,
    //   visible redo.
    //
    // ⚠ **A burn deletes ROWS, and this table's rows name FILES.** The burn
    // executor is `DELETE … WHERE actor_id = ?old` inside the ceremony
    // transaction and can unlink nothing. The file half is
    // `mail_export_blobs::reclaim_orphaned_export_blobs` — a blob no row names
    // is garbage — run right after the ceremony commits and again at boot.
    //
    // `Purge` is right for the row and, for the same reason, insufficient for
    // the feature: `finalize_user_deletion` unlinks the actor's blobs
    // (`mail_export_blobs::unlink_export_blobs_for_actor`) BEFORE this
    // registry's sweep deletes the rows that name them, so a failed unlink
    // leaves its row standing and the retried deletion finds it again.
    ActorTable {
        table: "export_sessions",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a mailbox-export session's blob is sealed under a per-session key WRAPPED FOR THE \
             RETIRED ACTOR (`mail-export.md` § Key material): the successor cannot open it, so a \
             move would hand them archives only the old key's holder can read, and leaving it \
             behind rests the old key holder's archive on disk for the whole 30-day expiry \
             window, since an identity that never calls never discards it. The rows burn; the \
             blob files they named are reclaimed \
             as orphans (`mail_export_blobs::reclaim_orphaned_export_blobs`). Re-exporting under \
             the new identity is the bounded, visible redo",
        ),
        // EXPORT RULING. The actor's own mailbox-export history — which format,
        // which mailboxes (`scope_descriptor`, the user's own selection, at
        // rest as the opaque DAG-CBOR the client wrote), when it ran, how far
        // it got — rides as the import twin's does. Two columns do not:
        //
        // - `blob_decryption_key_wrapped_for_actor` is wrapped KEY material, so
        //   the enum's own discriminator settles it: not "can the reader open
        //   this?" but "does the export carry a key, under any wrapping, that
        //   opens something else?" It opens the whole-mailbox blob. The user
        //   loses nothing — their clients fetch the wrap over the session RPC
        //   while the session lives.
        // - `blob_path` is the nest-internal file handle `mail-export.md`
        //   § Don't do these forbids surfacing on any user-facing surface, and
        //   an archive of the user's data is one.
        //
        // `blob_bytes` rides: a size, cleared by name in
        // `CLEARED_EXPORTING_COLUMNS`.
        export: Export::Redacted {
            omit: &["blob_decryption_key_wrapped_for_actor", "blob_path"],
            reason: "`blob_decryption_key_wrapped_for_actor` is the wrapped per-session key that \
                     opens the sealed whole-mailbox export blob -- key material under a wrapping \
                     is still a key, and an archive retrievable with an eviction token must not \
                     become its second resting place; `blob_path` is the nest-internal file \
                     handle `mail-export.md` forbids surfacing to the user. The rest is the \
                     actor's own export history (format, their own mailbox selection, counters, \
                     timestamps) and rides",
        },
    },
    // Who may reach this account unsolicited. Ruled `Move` on the asymmetry
    // rather than on tidiness: the column defaults to `'allow_knock'`, so a
    // user who narrowed it and is then succeeded silently **widens** back to
    // the permissive default — the one direction where leaving a row behind is
    // not the status quo but a loosening the user never asked for.
    ActorTable {
        table: "inbox_modes",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        export: Export::Shaped {
            domain: "profile.json",
            covers: &["mode"],
            reason: "full coverage verified 2026-08-15, and machine-checked \
                     since 2026-08-16: the row is one value per actor (`mode`, \
                     actor_id PK) and `profile.json` carries it as \
                     `inbox_mode` via `get_inbox_mode`",
        },
    },
    // This table's actor column is the **guardian designation**, not the code's
    // owner — so it belongs to the ruled family plane, not to the invite
    // plane its name suggests. It moves for exactly that reason: the eleven
    // supervised-side tables were ruled by asking *what the row means when the
    // guardian is ABSENT*, and here the answer is a ward seated under an actor
    // that can never authenticate again — supervised on paper, unmanageable in
    // fact, which `family-safety.md` § Lifecycle refuses when asked directly.
    // `account_core` reads the designation out of `validate_invite_code` and
    // hands it straight to `create_user_with_handle`, which writes the
    // `guardianships` row, so a stale designation is not a cosmetic pointer —
    // it is the guardianship a post-ceremony registration will create.
    // Plain, so it rides the registry loop with `guardianships` itself.
    ActorTable {
        table: "invite_codes",
        column: "guardian_actor",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // `code` is redeemed by ITSELF: `SELECT tier, uses_left, guardian_actor
        // FROM invite_codes WHERE code = ?1` is the whole lookup in
        // `CacheDb::validate_invite_code` (and its `peek_invite_code`
        // pre-check), which then atomically decrements `uses_left`. It
        // authenticates nobody — presenting the string is the
        // entire credential — so an unspent code in the archive is a live
        // capability to enrol on this nest at the named tier, under the
        // guardian's designation. The weakest-credential rule settles it: the
        // export is retrievable with an eviction export token, and a zip that
        // outlives every session is exactly the new resting place a bearer
        // credential must not get.
        //
        // The rest of the row is the guardian's own ledger — which tier they
        // minted for, how many uses are left, when — so the row rides without
        // the string. A guardian who needs a usable code mints a fresh one;
        // that is the bounded, visible re-grant, and it costs nothing an
        // export was ever entitled to give them.
        export: Export::Redacted {
            omit: &["code"],
            reason: "`code` is a bearer credential -- `validate_invite_code` finds the row by \
                     the code alone and decrements `uses_left`, authenticating nobody -- so an \
                     unspent code in an archive retrievable with an eviction token is a live \
                     enrolment capability that outlives every session. The tier / uses-left / \
                     mint-time ledger is the guardian's own data and rides. COUPLED to \
                     `audit_log`: the minting admin's `invite.create`/`invite.delete` rows \
                     ride their export too, so they store `admin::invite_code_audit_fingerprint` \
                     in `target`, never the code -- else this omission is routed around",
        },
    },
    ActorTable {
        table: "invite_requests",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "UNREACHABLE BY CONSTRUCTION, and it is the producer that says so \
             rather than an argument about the rows: `submit_invite_request_core` \
             refuses an already-registered actor by name (`ActorAlreadyRegistered`) \
             and an approve consumes its row (`delete_invite_request`), so every \
             row that can exist names an actor with no `users` row — while both \
             succession writers require one (`record_succession` needs a locally \
             registered RecoveryKey chain, `record_peer_succession` refuses \
             `OldIsLocal`). No row this table can hold can belong to an identity \
             any nest succeeds, so no verdict here is witnessable; recorded as \
             `Stay` because it is what already happens, and said plainly rather \
             than pinned by seeding a row the schema's own producer would reject. \
             The DECIDER's column is ruled separately in `SUCCESSION_REFERENCES`",
        ),
        // EXPORT RULING (the residual set). Vacuous, and
        // by a mechanism no other table in this registry uses: **the export
        // endpoint's own precondition excludes exactly the population this
        // table can hold.** `handle_export` refuses before the walk unless
        // `get_user(actor)` is `Some`, while the succession reason above
        // establishes that every row here names an actor with NO `users` row —
        // `submit_invite_request_core` refuses an already-registered actor and
        // an approve CONSUMES the row (`delete_invite_request`). So a caller
        // who could reach the walk has no row here, and a subject of a row
        // cannot reach the walk. The two facts meet exactly.
        //
        // The class holds independently, which is what the verdict rests on:
        // the row is the admissions queue's own record about a would-be user —
        // their proposed handle, their message to the admin, the decision,
        // `decided_by` and `denial_reason`. It belongs to the nest's admission
        // process, and nothing in it is data an account holder is missing.
        //
        // ⚠ Do not "reactivate" this by keeping the row at approve so the new
        // user can export their own application: that changes the table's
        // population, and this verdict with it.
        export: Export::WithheldOperational(
            "the ADMISSIONS QUEUE's own record about a would-be user, and vacuous for the \
             export by construction: every row names an actor with no `users` row (submit \
             refuses an already-registered actor -- approve DELETES the row), while \
             `handle_export` refuses any caller `get_user` does not resolve. A caller who \
             can reach the walk has no row here and a subject of a row cannot reach the \
             walk. The class is what the verdict rests on: proposed handle, message to the \
             admin, decision, decider and denial reason are the nest's admission process, \
             not an account holder's own data",
        ),
    },
    ActorTable {
        table: "key_packages",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the published pool is this identity's ADDRESSABILITY — a KP is the \
             standing offer *add me to a group*, and neither consumer consults \
             the refusal plane (`take_key_package` is called unconditionally by \
             `conversations_handlers::keypackage_fetch_handler` and by \
             `federation_handlers::keypackage_fetch_handler`, the latter reachable \
             by any unauthenticated peer nest). Leaving it is not the neutral \
             status quo the backlog usually enjoys: a contact whose client has not \
             yet propagated the succession keeps resolving the OLD id, consumes a \
             KP, and seats a leaf that can never read the group — and the \
             last-resort KP is served WITHOUT being consumed, so that trap is \
             permanent rather than pool-limited, and silent on both sides (the \
             inviter believes the person joined; the successor never appears). A \
             `Move` is the actively wrong answer and is why this is ruled rather \
             than left: the KP embeds the PREDECESSOR's credential and its private \
             half is seed-derived, i.e. thief-held, so re-pointing the row would \
             advertise a thief-openable leaf under the successor's own name. \
             Burning makes the stale add fail loudly, which is what routes the \
             contact back to a fresh resolve (§ Propagation), and nothing \
             irrecoverable dies — the client republishes its pool, one-time and \
             last-resort alike, at its next sign-in.",
        ),
        // EXPORT RULING (the shaped-domain plane). A
        // table whose every column has "key" in its name or meaning, and it is
        // not `WithheldSecret` — the clearest case in the cluster of the enum's
        // own test: *does the export carry a key, under any wrapping, that
        // opens something else?* An MLS KeyPackage is a PUBLISHED offer, and
        // the nest serves it to anyone: `federation_handlers::keypackage_fetch_
        // handler` is reachable by any unauthenticated peer nest, as this
        // row's own succession reason records. Its private half is
        // seed-derived and client-held and has never been in this table. So
        // `key_package_data` is world-readable material the archive's owner
        // published themselves — the `actor_epoch_seal_keys` precedent,
        // and the inverse of that same test:
        // there a benign-looking blob was key material, here key-looking
        // material is public.
        //
        // Coverage the shaped domain misses: `list_key_packages_for_actor`
        // filters expired rows, so the pool the user actually published is
        // narrower in the archive than on the box, and `last_resort` is
        // dropped — the flag distinguishing the reusable KP from the one-time
        // ones, i.e. which row is which.
        export: Export::Verbatim,
    },
    // The pending-reach queue `inbox_modes` routes into — a stranger's arrival
    // held for the recipient's (or their guardian's) decision. It moves for the
    // same reason its gate does, and the pair must agree: `inbox_modes` is
    // already `Move` because leaving the mode behind silently WIDENS who may
    // reach the successor, and leaving the queue behind is the same asymmetry
    // read from the other side — the successor inherits the narrowed mode and
    // none of the people it narrowed, so every pending knock is answered by
    // nobody, forever, with no surface that says one was ever waiting.
    //
    // `actor_id` here is the RECIPIENT. The knocker is `sender_id`, a counterparty
    // column ruled separately in `SUCCESSION_REFERENCES`.
    ActorTable {
        table: "knocks",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the shaped-domain plane). The
        // hand-written `knocks.json` domain is PARTIAL in both directions and
        // neither drop survives inspection, so the shaped verdict its
        // existence invites is the wrong one. It reads `poll_knocks`, which
        // filters `delivered = 0` — so every knock the user has already
        // received is absent from their archive, which is most of them — and
        // it drops `payload`, the knock's actual contents. A knock is inbound
        // content addressed to this actor; the summary without the payload is
        // the envelope without the letter. Sealed content rides in at-rest
        // form (the enum's sealed-columns clause), `sender_id`/`sender_node`
        // name the person who chose to knock at this actor's door, and
        // `delivered` is this actor's own receipt state.
        export: Export::Verbatim,
    },
    // SUCCESSION RULING (2026-08-15) — the subscription is the
    // user's own curation, and leaving it behind strands a **nest-wide** effect
    // no one can then withdraw.
    //
    // Three consumers, and each fails a different way if the rows stay:
    //  1. **Arrival-time** (§ Re-key scope's arrival question): mail
    //     delivery calls `seed_new_mail_labeler_obligations(content_id, owner)`,
    //     which reads `labeler_subscriptions WHERE owner_actor = ?owner` to seed
    //     the per-item re-score obligations. Left behind, the successor's
    //     arrivals seed **none** — their community mail labelers silently stop
    //     scoring — while a peer that has not yet propagated the statement goes
    //     on landing mail on the retired actor, whose obligations nobody drains.
    //     The `message_scan_results` shape ("seeds the successor from an empty
    //     universe"), one plane over.
    //  2. **The unsubscribe gate**: `unsubscribe_labeler_core` withdraws a List
    //     labeler's materialized `content_scores` rows only when
    //     `count_subscriptions_for_labeler(labeler_id) == 0`. A stranded row
    //     keeps that count above zero **forever** — `delete_subscription` is
    //     `(owner_actor, labeler_id)`-scoped and the retired identity is refused
    //     everywhere — so one retired subscriber pins a labeler's nest-wide
    //     scoring on for every other user, with no surface anywhere to clear it.
    //     `is_share_token_revoked`'s mirror image: here it is the row's
    //     **presence** that keeps an effect alive.
    //  3. The successor's own browse (`list_subscribed_labeler_ids`) shows
    //     nothing subscribed while the scores keep arriving — recreatable by
    //     re-subscribing, and the least of the three.
    //
    // ⚠ `grant_id` travels with the row and points at a `capability_grants` row
    // the ceremony **burns**. That is not a reason to hold the subscription
    // back: the column has no production reader at all today (`get_subscription`
    // is the test read; `subscribe_labeler_core` stores it and nothing resolves
    // through it), and the aftermath's grant re-mint is where a live one would
    // be re-issued — the bounded, visible re-grant this axis prefers.
    ActorTable {
        table: "labeler_subscriptions",
        column: "owner_actor",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the residual set). The user's
        // own choices, and the migration says so in those words: "a
        // subscription row *is* the user's choice". `owner_actor` is the
        // subscriber, `labeler_id` names a labeler that is published and
        // fetchable by anyone, `subscribed_ver` and `created_at` are their own
        // history. `grant_id` is an opaque handle and nothing resolves through
        // it today (the comment above this entry establishes that) — a name
        // for a grant, never the grant.
        //
        // ⚠ Its neighbour `labelers` goes the other way, and the reason is the
        // column rather than the plane: see that entry.
        export: Export::Verbatim,
    },
    // SUCCESSION RULING (2026-08-15) — `publisher_actor` is a
    // **signing key**, not an account pointer, so the classification rule's
    // "embedded signatures are immutable and genuinely name the old key" arm
    // applies literally rather than by analogy.
    //
    // `publish_labeler_core` writes `publisher_actor: &labeler_id`, and
    // `labeler_id` is the metadata's `algorithm_id` — the self-signed,
    // off-box-rotatable keypair the artifact's Ed25519 signature is verified
    // against, at publish and again at every FFI instantiation. The account's
    // own identity is the *other* column, `caller_actor`, ruled `Move` in
    // `SUCCESSION_REFERENCES`. Re-pointing this one would make the stored
    // publisher disagree with the bytes it signed, and would advertise the
    // successor as the signer of an artifact they never signed.
    //
    // It confers nothing that a move could rescue. The registry has no delete
    // or retire verb at all (`fauna.labelers.*` is publish/list/inspect/
    // subscribe/unsubscribe), and a new version is admitted by signing with the
    // *algorithm* key — which the successor still holds, the keypair being
    // client-side material a succession does not touch. So `Stay` costs the
    // successor no reachable authority.
    //
    // ⚠ **This ruling is UNFALSIFIABLE by the generic sweep, and the reason is
    // structural — the M5 finding, second instance and sharper.** No
    // production writer can ever put an enrolled account id in this column
    // (`publish_labeler_core` writes the artifact key unconditionally), so no
    // reachable row distinguishes `Stay` from `Move`; the sweep does not merely
    // miss that, it seeds a synthetic row keyed on the acting actor and confirms
    // whichever verdict is declared. Recorded rather than papered over with a
    // pin that seeds a state the schema cannot reach. It is also the
    // `sync_devices` case — the ceremony already left these rows alone, so the
    // ruling changes no behaviour and exists to be attackable.
    ActorTable {
        table: "labelers",
        column: "publisher_actor",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "a signing-key identity embedded in signed bytes, not an account pointer: \
             `publish_labeler_core` writes `publisher_actor = labeler_id = \
             metadata.algorithm_id`, the self-signed rotatable keypair the artifact's \
             signature verifies against at publish AND at every FFI instantiation. \
             Moving it would make the stored publisher disagree with the bytes it \
             signed. It confers nothing a move could rescue — the registry has no \
             delete/retire verb, and re-publishing is gated on the algorithm key the \
             successor still holds. The account's own identity on this row is \
             `caller_actor`, ruled Move in SUCCESSION_REFERENCES. ⚠ Unfalsifiable by \
             the generic sweep (the M5 class): no production writer can put an enrolled \
             actor id here, so no reachable row distinguishes this from Move",
        ),
        // EXPORT RULING (the residual set). The
        // wrong-column check in its THIRD distinct form, and the sharpest yet:
        // one earlier finding was a column naming the wrong PARTY, another a column
        // naming a party a local export can never match, and this one names a
        // different KIND OF IDENTITY altogether. `publisher_actor` is
        // `metadata.algorithm_id` — a self-signed, freely rotatable artifact
        // keypair, established by the succession reason directly above — never
        // an enrolled account. "No production writer can put an enrolled actor
        // id here" is the succession ruling's own measured claim, and it means
        // an actor-scoped export read matches nothing for the same reason.
        //
        // The class holds without the vacuity: the row is the published-
        // artifact store, addressed by artifact. `wasm_bytes` is up to a MiB of
        // module the nest serves to anyone who asks for it, so the archive
        // withholds nothing the exporter could not fetch — and the exporter's
        // own authorship link is `caller_actor`, which lives in
        // `SUCCESSION_REFERENCES` and this walk never touches. That is the
        // `actor_successions.new_actor_id` shape, one plane
        // over: the same structural gap, and the same warning — a second
        // `ACTOR_TABLES` entry on `caller_actor` would emit this table's
        // `.ndjson` twice into one zip.
        export: Export::WithheldOperational(
            "the PUBLISHED-ARTIFACT store, addressed by artifact: `publisher_actor` is \
             `metadata.algorithm_id`, a self-signed rotatable artifact keypair and never an \
             enrolled account (the succession ruling beside this measured that no \
             production writer can put an enrolled actor id here), so an actor-scoped read \
             matches nothing. Nor is anything withheld from the exporter -- `wasm_bytes` is \
             served to any caller that asks, and their own authorship link is \
             `caller_actor`, which lives in SUCCESSION_REFERENCES and this walk does not \
             touch",
        ),
    },
    ActorTable {
        table: "mail_account_settings",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Bespoke(
            "the row is the account's own mail settings and follows it, but \
             `forward_all_to` is CLEARED as it moves. That column is a live \
             exfiltration tap: `fauna.bridges.set_forward_all_to` is User-class, so a \
             seed thief arms it with the key they read, and the MTA reads it at the \
             perimeter for every inbound message \
             (`fetch_recipient_forward_config`) — a plain move would hand the successor \
             a mailbox that silently copies every future message to the thief, with no \
             end date. Re-point *and* revoke, the `nostr_bunker_apps` shape, uniform \
             for theft and loss like the MSEK burn. `forward_per_hour` beside it is the \
             user's own choice and rides along",
        )),
        // EXPORT RULING (2026-08-16) — the account's own mail
        // settings, both columns. `forward_all_to` is an address the OWNER set
        // through their app, which displays it back to them; an export is that
        // same disclosure through the owner's own channel. The succession axis
        // clears this column, and the divergence is the `account_aliases`
        // reasoning exactly: a moved tap keeps copying a recovered account's
        // future mail to whoever armed it, while an archived copy of the
        // setting forwards nothing.
        //
        // ⚠ Not a case where the app is the narrower surface — that test
        // (`payment_providers`' `webhook_secret`) turns on whether
        // the owner's own read path declines to echo the value.
        // `get_forward_config` returns it; the export must not be WIDER than
        // the app, and here it is not wider.
        export: Export::Verbatim,
    },
    // ─── The mailing-list plane — RULED 2026-08-14 ───
    //
    // Three tables, one verdict, and the first two are ruled by a question this
    // axis had not had to ask before: **what happens to a row that LIMITS the
    // account?** Every ruling up to here asked what authority a row confers,
    // because carrying authority forward was the dangerous direction. A rate
    // meter is the mirror image — it carries *liability* — and there the
    // dangerous direction is leaving it behind, because **a succession is
    // self-service**: any account holder can perform one with their own
    // recovery kit, so a limit that a ceremony sheds is not a limit. See
    // `succession-aftermath.md` § Re-key scope, the 2026-08-14 liability
    // blockquote, which states the rule for the whole axis.
    ActorTable {
        table: "mail_list_account_daily_counter",
        column: "actor_id",
        key: ActorKey::Blob,
        // The per-account-per-day list-recipient meter `try_consume_list_quota`
        // reads before every fan-out, enforcing `mail-mass-mailing.md` § The
        // per-day per-account cap ("across all lists owned by one user").
        //
        // Left behind, the successor's first send reads `unwrap_or(0)` — a
        // fresh 20 000-recipient day, and a full one every time the account is
        // recovered again. The deployment-wide valve beside it is not keyed on
        // an actor and so is unaffected, which is precisely why this row has to
        // move: it is the only per-account half of the pair.
        //
        // What the move costs is bounded and self-clearing: the successor
        // inherits the day's consumption and the lazy UTC-day rollover
        // (`counters_day`) zeroes it at the next send after midnight. A thief
        // who burned the day's quota throttles the real owner for the remainder
        // of one day; the alternative is handing every account an unlimited
        // supply of fresh days.
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — `bridge_submission_quota`'s
        // exact twin on the list-send side (its own succession ruling above
        // calls it that), and the same verdict for the same reason: a per-day
        // meter of what this nest allowed, self-clearing at the UTC rollover,
        // with nothing about the account's data in it.
        export: Export::WithheldOperational(
            "the per-account-per-day list-recipient meter `try_consume_list_quota` reads              before every fan-out -- a rate counter, the archetype this variant names. It              self-resets at the next UTC day and says nothing about the account's data; the              sends it metered ride `mail_list_sends`",
        ),
    },
    ActorTable {
        table: "mail_list_sends",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        // The per-send audit row. ⚠ **This column has no reader today** —
        // `list_send_history` is scoped by `list_id`, and nothing else selects
        // on it — so it is a denormalized copy of the owning list's
        // `owner_actor_id`, and both verdicts are unobservable at this moment.
        // It is ruled on consistency rather than on an observable, which the
        // `…_by` pass established as the right treatment: the row's
        // list moves, so a stamp left behind puts the two in disagreement and
        // hands the first reader to arrive a false owner.
        //
        // Not "history stays", though the table is an audit trail: what stays
        // is the RECORD, and a `Move(Plain)` deletes nothing — it rewrites one
        // pointer. The send happened, its count is intact, and the owner field
        // says who owns it *now*, which is the only thing a denormalized owner
        // can honestly mean.
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the owner's own send
        // history: when they sent to a list, to how many, how many were
        // delivered, how many unsubscribed during the send. An audit trail of
        // the owner's own actions, with no third-party identity in any column
        // (the counts are aggregates; the addresses live in
        // `mail_list_members`, which is not a registry entry — see
        // `mail_lists`).
        export: Export::Verbatim,
    },
    ActorTable {
        table: "mail_lists",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        // The list itself, and a `Stay` here is the `nest_pairings` breach in
        // its clearest form yet: it strands a **live, publicly addressable**
        // surface that no client can reach.
        //
        // The two halves of a list part company at the ceremony. Its ADDRESS is
        // a `kind='list'` row in `account_aliases`, which moves — that is the
        // ratified "an address is ownership" leg — and inbound delivery
        // resolves through `lookup_list_id_for_address`, which joins alias to
        // list and never consults the owner. So the list keeps accepting and
        // keeps serving one-click unsubscribes no matter who owns it. Its
        // MANAGEMENT, by contrast, is owner-scoped in every single door:
        // `list_lists_for_actor`, `get_list_for_owner`, `update_list_metadata`,
        // `delete_list` and the ownership check `send_list_message` runs first
        // (`mail-mass-mailing.md` § Wire shapes: "all per-user / owner-scoped").
        //
        // Left behind, then: the successor owns the address the list sends
        // from, and cannot see the list, edit it, send to it, or delete it —
        // while the list stays live. Nobody else can either; the retired
        // identity is refused everywhere. That is a client-reachable state no
        // client can fix (`nest/common.md` § Client-state recoverability). The
        // only lever left would be deleting the ALIAS, whose `ON DELETE
        // CASCADE` takes the list and every subscriber row with it — data loss
        // as the remedy for a mis-ruling.
        //
        // ⚠ And that cascade is why a `Burn` was never a candidate: deleting
        // these rows would cascade into `mail_list_members`, destroying a
        // subscriber list the owner cannot reconstruct. `Move` re-points; it
        // deletes nothing.
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the mailing lists the owner
        // created: friendly name, description, the List-Help / List-Archive
        // URLs they published, per-send recipient cap, and the send/member
        // counters. Owner-scoped in every management door (the succession
        // ruling above enumerates them), so a row keyed by the exporting actor
        // is theirs by construction.
        //
        // ⚠ The SUBSCRIBERS are not in this table and are not ruled here.
        // `mail_list_members` is keyed by `list_id`, not by an actor column, so
        // it is not an `ACTOR_TABLES` entry at all and this verdict says
        // nothing about it — which matters, because that table holds each
        // member's address and their cached one-click-unsubscribe token. An
        // export of the member list is a separate question (a bearer token per
        // row, and third parties' addresses); do not read this Verbatim as
        // having settled it.
        export: Export::Verbatim,
    },
    // The per-message scan record for mail this account was DELIVERED. Its own
    // per-actor consumer is what rules it: `seed_factor_backlog` selects on
    // `delivered_to_actor` to enumerate "the owner's mail item universe" and seed
    // the scoring backlog from it, so the record is the index of the very corpus
    // that moves. Left behind, the successor's backlog seeds from an empty
    // universe and every inherited message scores unpersonalized — the
    // `spam_training_history` class, the account's own operational record of its
    // own mail, not a counterparty-facing attestation.
    //
    // The perimeter's forensic rows carry `delivered_to_actor = NULL` (rejected
    // before any delivery) and are untouched by an actor-bound leg by
    // construction — they belong to no account and are pruned on their own cutoff.
    ActorTable {
        table: "message_scan_results",
        column: "delivered_to_actor",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the record of what this nest
        // DID to the owner's incoming mail and why: the antivirus verdict, the
        // spam score and the rules that produced it, and `action_taken`. This
        // is the same class as `bridge_restore_divergence` — a
        // notice about the owner's own data — and it is the only place a user
        // can learn why a message was quarantined or rejected.
        //
        // ⚠ The perimeter-reject forensic rows carry `delivered_to_actor =
        // NULL` (the migration says so) and therefore match no actor's export
        // read: what rides is exactly the subset that was delivered to, or
        // decided about, the exporting actor. Nothing about another recipient
        // is reachable through this verdict.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "nest_backup_keys",
        column: "owner_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "derived from the seed the thief read; the successor re-grants its own \
             from the new seed",
        ),
        export: Export::WithheldSecret(
            "`backup_key` is the user-granted `NestBackupKey` at rest, \
             unwrapped -- the key this box opens the owner's segment backups \
             with. Its succession reason above burns it for being \
             seed-derived; the export axis withholds it for the simpler reason \
             that a raw key in a zip is the resting place the \
             weakest-credential rule exists to prevent. The client re-derives \
             it from the identity seed, so nothing is lost by omission",
        ),
    },
    ActorTable {
        table: "nest_pairings",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "standing authority whose destination is ATTACKER-CHOSEN, which is the \
             `forward_all_to` test rather than the `spam_preferences` one: \
             `fauna.pair.add` is an ordinary User-class gesture taking a \
             caller-supplied `nest_url`, so a seed thief pairs a nest they control \
             with exactly what they read. Two of the three consumers never look at \
             the actor at all — `any_pairing_with_capability` and \
             `list_pairings_with_capability` scan EVERY non-expired row box-wide — \
             so a row parked on a retired identity still holds \
             `nostr_serving_available` true and is still iterated by the head's \
             NIP-46 proxy, which dials that row's `nest_url`. The identity-key \
             refusal plane cannot reach any of that: it gates who may \
             authenticate, and these two read no actor to refuse. Worse, the \
             successor cannot clean it up — `fauna.pair.list` and \
             `fauna.pair.revoke` are both owner-implicit on the connection actor, \
             so the stale row is invisible AND unrevocable from every app, a \
             `nest/common.md` § Client-state recoverability breach of the \
             `web_domains` shape with live authority in it instead of dead \
             routing. Uniform for theft and loss on the MSEK's reasoning: the \
             statement carries no reason, and the costs are asymmetric — the loss \
             case re-pairs its own nest in one visible gesture, while the theft \
             case otherwise proxies through the thief's box silently and forever.",
        ),
        // EXPORT RULING (the residual set). The owner's
        // own pairing ledger: which private nest, under what `capabilities`,
        // until when, under the `label` they chose. `principles.md` § The user
        // always controls their data puts audit of standing authority in the
        // user's own hands, and `fauna.pair.list` already answers exactly this
        // question owner-implicitly on the connection actor — so withholding
        // would make the archive quieter than a door the owner can call, on
        // the `restore_history` reading.
        //
        // ⚠ The `Burn` above is the FOURTH succession verdict this plane pair
        // has that says nothing about disclosure, and the loudest: it burns
        // because the row is standing authority with an ATTACKER-CHOSEN
        // destination that the successor could neither see nor revoke. That is
        // an argument about who may keep ACTING, and it cuts the opposite way
        // on disclosure — a standing authority the owner cannot enumerate is
        // precisely what an export should hand them. `nest_url` is the
        // destination they paired; no bearer or key material rests in this
        // table at all.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "nostr_accounts",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldSecret(
            "`encrypted_privkey` is the deposited nsec — the signing key for \
             the account's whole nostr identity, and the very column whose \
             silent survival an earlier encoding fix existed to purge. Key \
             material never rides an export; the identity-export QR ceremony \
             is the sanctioned way a user carries a signing secret",
        ),
    },
    ActorTable {
        table: "nostr_bunker_apps",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Bespoke(
            "moves AND revokes in one statement (`status = 'revoked'`, `secret_hash = NULL`). The rows re-point so the successor's Connected-apps page shows exactly which apps held standing authority, and are revoked because a thief-minted connection signs as the npub silently and forever (`ui/nostr.md` § Key succession and rotation). A plain move would carry the thief's authority to the successor.",
        )),
        // The roster IS the user-meaningful half, and the succession ruling
        // beside this says why: the rows exist so the owner's Connected-apps
        // page shows exactly which apps hold standing authority to sign as
        // their npub. An export that withheld the whole table would be silent
        // about precisely the standing authority `principles.md` § The user
        // always controls their data asks the user be able to audit.
        //
        // `secret_hash` is the only column that is not that. It is
        // `blake3(secret)` over a 16-byte `random_hex` one-time pairing secret
        // (`nostr/bunker.rs::create_invite`), held only while the row is
        // `pending` and NULLed the moment the app claims or the owner revokes.
        // A 128-bit preimage is not recoverable from the digest, so this is not
        // a leak so much as a verifier that means nothing to its owner and
        // sits one preimage away from a live pairing: it buys the user nothing
        // and the ruling's bias says drop it.
        export: Export::Redacted {
            omit: &["secret_hash"],
            reason: "the roster (label, app pubkey, status, use count, timestamps) is the \
                     Connected-apps ledger the owner audits, and rides. `secret_hash` is the \
                     blake3 verifier for a pending one-time pairing secret, NULLed on claim or \
                     revoke -- meaningless to its owner, adjacent to a live pairing, so it does \
                     not ride",
        },
    },
    ActorTable {
        table: "nostr_bunker_signers",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldSecret(
            "`encrypted_privkey` is the bunker signer's secret key -- the same \
             column, in the same encoding, on the same bridge as \
             `nostr_accounts`' deposited nsec, and it gets that entry's ruling \
             verbatim: key material never rides an export, and the \
             identity-export QR ceremony is the sanctioned way a user carries a \
             signing secret",
        ),
    },
    // The oracle's bound NIP-46 clients (TP11, `nostr/oracle.rs`): one row per
    // (account, third-party principal) naming the client key the principal
    // bound, plus the audit counters and the rate ceiling's window. Burned at
    // succession for the `nostr_bunker_apps` reason, sharper: a principal
    // minted under the thief's authority must not keep signing as the npub,
    // and unlike the invite roster there is nothing for the successor to
    // audit here that `third_party_principals` and the grant log do not
    // already show. The export rides it: the client key is PUBLIC, and the
    // counters are the owner's own ledger of what the principal did.
    ActorTable {
        table: "nostr_oracle_clients",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a bound oracle client is standing third-party authority to sign as the \
             npub, of exactly the bunker class — a principal minted under the thief's \
             authority keeps signing for the user after the ceremony",
        ),
        export: Export::Verbatim,
    },
    // The oracle's per-operation record — the audit half the custodian keeps
    // (`key-material-hierarchy.md` § *The oracle*, the audit split): which
    // class, when, and the event kind or method. Burned with its client rows;
    // the owner's own audit trail, so it rides the export.
    ActorTable {
        table: "nostr_oracle_ops",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the operation record of a burned oracle client — rows naming a client \
             that no longer exists",
        ),
        export: Export::Verbatim,
    },
    ActorTable {
        table: "nostr_federation_cursors",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. Phase-2 proxy-delegation sync
        // watermarks: the compound `(stored_at, id)` push and pull cursors the
        // head advances per (actor, peer public box) after each successful leg.
        // The table's own migration comment rules itself -- "Ephemeral-
        // recoverable: a lost cursor re-pulls from the start and dedups" -- and
        // a value whose loss costs one redundant re-pull is not data a user can
        // be missing.
        //
        // `peer_nest_id` is the paired public box the deployment bridges
        // through, not a peer the user picked, so no federation choice of
        // theirs is hidden by withholding it. Zeroed columns mean nothing has
        // bridged yet.
        export: Export::WithheldOperational(
            "push/pull sync cursors for the Nostr proxy-delegation bridge, one pair per (actor, \
             peer public box). Delivery bookkeeping in its purest form: the migration itself \
             says a lost cursor simply re-pulls from the start and dedups, so the value is \
             meaningless off-box and costs the owner nothing to be without. The events the \
             cursors walk are `nostr_events` rows and the Nostr leg's bridged-conversation \
             rows, and are ruled on their own.",
        ),
    },
    ActorTable {
        table: "nostr_follows",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. The owner's Nostr follow list,
        // and `petname` is the column that settles it: a name THEY chose for a
        // contact, stored nowhere else, recreatable by nobody. `relay_hints`
        // is where to find that contact, `nostr_pubkey` is the contact's public
        // identity (the `contacts.peer_id` class, and public by protocol --
        // it is an npub). Withholding this would drop a hand-curated list on
        // the floor.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "nostr_zap_signers",
        column: "actor_id",
        key: ActorKey::Hex,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a designated zap signer is standing third-party authority of exactly the \
             bunker class — a thief-planted one keeps minting receipts this nest \
             believes for the user's pubkey after the ceremony",
        ),
        // Four columns: the actor, a designated signer's PUBLIC key, a label the
        // owner chose, and a timestamp. Nothing here signs anything — the signer
        // holds its own private half off this box entirely, which is the whole
        // point of designating one — so the row confers no authority on a reader.
        // It is a roster of who the owner designated, i.e. the same
        // standing-third-party-authority ledger the succession ruling above burns
        // for being invisible, and the export is where the owner reads it back.
        export: Export::Verbatim,
    },
    // The account's own notification record — what others did toward it, rendered
    // on its own page and counted into its own unread badge. Nothing here is
    // authority and nothing is attribution owed to a counterparty, so the
    // `restore_history` / `sync_changes` reading applies: it is the user's record
    // of their own account, and leaving it behind empties the successor's
    // notifications page rather than protecting anything.
    //
    // RETENTION (`behavior/notifications.md` § Retention, 2026-09-24) follows
    // from the same reading: nothing sweeps these rows by age or count. This
    // purge, the user's own `fauna.notifications.{dismiss,clear}`, and a
    // `knock` doorbell going with its knock are the only deletes.
    //
    // `actor_id` is the RECIPIENT; `sender_id` names the counterparty and is ruled
    // separately in `SUCCESSION_REFERENCES`.
    ActorTable {
        table: "notifications",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the residual set). The class was
        // already named in the comment above this entry and simply never
        // carried to this axis: "the `restore_history` / `sync_changes`
        // reading applies — it is the user's record of their own account."
        // These rows are their notifications page and their unread badge.
        //
        // `sender_id` names a counterparty, and it rides on the earlier
        // resolution of that question in the permissive direction: the person
        // it names ACTED TOWARD this user, and the product already renders
        // them on the page the rows back. Withholding would take the
        // interaction the user is looking at out of their own records. It is
        // also the `actor_id` = RECIPIENT direction, so the exporter is the
        // party the row is FOR, not a bystander to someone else's.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "oauth_client_blocks",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The account's "never show requests from this app" list
        // (`authorization-server.md` § Consent rule (c)) — the user's own
        // setting, and purely RESTRICTIVE: a row can only keep a client's quiet
        // push from opening a card. A seed thief who wrote one could only have
        // hidden an app's requests, which the successor sees and lifts from
        // their own app, so nothing here earns a burn; moved, the successor
        // keeps the blocks they set.
        succession: Succession::Move(MoveShape::Plain),
        // Their own setting, one row per blocked client, naming nothing but
        // the client's public `client_id` URL. Nothing in it opens anything.
        export: Export::Verbatim,
    },
    // The first table on this axis whose hazard is a **timer** rather than a
    // standing row, and the second (after `eviction_tokens`) where `Move` is
    // the actively dangerous direction. A row is a delayed destructive
    // operation; `pending_actions::start_executor` ticks every 60 s and
    // `execute_ready_actions` fires everything `status='pending' AND
    // execute_after <= now` while **authenticating nobody**, so the row is the
    // authority — `push_subscriptions`' standing, one plane over. The delays
    // run 6 h to 30 d (14 d for `account.delete`), which is exactly the window
    // a succession racing a seed thief lands inside.
    //
    // `Move` would re-aim it: `snapshot.delete`, `snapshot.bulk_prune` and
    // `admin.backup_purge_override` resolve their targets from the row's own
    // `target`/`payload` with no actor check at all, so a thief's queued prune
    // would run against the corpus the ceremony just handed the successor.
    // `Burn` would delete the record that it was ever queued — the act most
    // worth leaving legible, which is `cancelled_by`'s own argument in
    // `SUCCESSION_REFERENCES`. So the rows STAY and the ceremony writes the
    // table's existing terminal state: see
    // `successions::disarm_the_retired_identitys_scheduled_actions`, which also
    // owns the `snapshots.deletion_pending` un-mark every cancel must do.
    ActorTable {
        table: "pending_actions",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Partial(
            "a delayed destructive operation, fired by a 60 s executor tick that \
             authenticates nobody. Terminal rows (executed/cancelled/expired) are \
             history and are untouched; rows still `pending` are disarmed in place \
             by the ceremony, because a `Move` would \
             re-aim a thief's queued snapshot prune at the successor's corpus (three \
             arms take their target from the row's own payload, never from an actor) \
             and a `Burn` would erase the record that it was ever scheduled. Nothing \
             moves: the predicate is (this actor AND still pending). The row rule is owned by \
             `successions::tests::\
             a_succession_disarms_the_retired_identitys_queued_destructive_actions`, \
             named because the sweep asserts nothing here",
        ),
        // EXPORT RULING (2026-08-16) -- ruled with
        // `audit_log`; the authorship invariant and its consequences are that
        // entry's comment, and the same e2e pin covers both tables. Held here
        // too: every `create_pending_action` site passes the CALLER --
        // self-service (handle change, account delete, snapshot ops; the
        // retention sweep keys its batch to the file set's owner) and admin
        // schedules alike -- so the slice is actions I initiated, with my own
        // `target`/`payload`. The three columns the recon flagged, each ruled
        // toward riding: `ip_address` is my own request's address (the
        // `actor_last_ip` precedent -- personal data cuts TOWARD exporting);
        // `approvals` (other admins approving MY action) is already served to
        // me by `fauna.pending_actions.{list,get}` on my own rows;
        // `cancelled_by`/`cancelled_at` are NOT on that surface today, but a
        // cancellation is an act UPON my row recorded precisely to be legible
        // -- this table's own `SUCCESSION_REFERENCES` entry calls a thief's
        // cancellation "the act worth leaving legible to the identity it was
        // performed against", and legible must include the export, since an
        // evicted or succeeded owner may have no live session; the canceller
        // (creator, target, or admin -- always a party to the row) is also
        // already recorded in `audit_log` keyed to themselves.
        export: Export::Redacted {
            omit: &["chain_hash"],
            reason: "the actor's own scheduled-action record rides, lifecycle columns \
                     included; the chain column is `audit_log`'s `prev_hash`/`entry_hash` \
                     class -- about the whole log, verifying nothing in a slice, \
                     confirming neighbours' entries -- see `audit_log`'s reason",
        },
    },
    // The at-rest half of a registry that ALREADY moves. Each row is the sealed
    // statistical model for one trained topic factor; the registry naming those
    // factors is `PersonalizationConfig.trained_factors` in `fauna.state.personalization`, which the
    // aftermath re-seals to the successor. Leaving the blobs behind therefore
    // splits one object: the successor's factor list still names every topic they
    // trained, and each one's model is gone — and the sealed-factor seam
    // degrades a model it cannot load to a ZERO TERM rather than an error, so the
    // page reads "trained" while the factor does nothing. The
    // `web_subdomain_enabled` shape, where the broken state and the displayed
    // state agree.
    //
    // A thief-trained factor riding along is the `spam_preferences` /
    // `atproto_account_settings` case, not the `forward_all_to` one: it appears in
    // the successor's own trained-topics list and is deleted in one reversible
    // gesture, where the alternative destroys models the user trained themselves.
    // Declared degradation, stated so it is not re-discovered as a defect: a blob
    // the successor cannot open is inert (zero term) and retraining rebuilds it —
    // the `spam_models` precedent, client-recoverable rather than a
    // `nest/common.md` breach.
    ActorTable {
        table: "personalization_models",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the owner's personalization
        // models, sealed CLIENT-side under their BackupKey and stored
        // verbatim-opaque; the nest has no train path and never holds a
        // plaintext model at any point. So this is the sealed-columns clause in
        // its purest form — the nest cannot unseal for the export even in
        // principle, and the ciphertext rides at rest.
        //
        // The migration settles the class in one sentence, the same fact (2)
        // again: **"User-irrecoverable data: rows are removed only by the
        // owner's own `fauna.personalization.model.delete`, never by a
        // migration."** A plane the codebase protects from its own migrations
        // is not a plane to withhold from its owner. ⚠ Note the contrast with
        // `spam_model_holder_copies` beside it — same shape, sealed blob per
        // actor — which was ruled `WithheldDerived` on its own
        // migration's word ("Recreatable derived data"). Two sealed-blob
        // tables, opposite verdicts, each decided by what its author wrote
        // about recoverability.
        export: Export::Verbatim,
    },
    // The REACH plane's third member, and the one that is live rather than
    // latent. A row is a destination the account named — a caller-supplied
    // `endpoint` URL plus the web-push encryption keys the caller also supplied —
    // registered by an ordinary User-class gesture, i.e. by exactly what a seed
    // thief holds. Three facts make leaving it behind an exposure rather than the
    // status quo the rest of the backlog enjoys:
    //
    //   1. A retired identity KEEPS BEING DELIVERED TO. The inbox path consults
    //      the succession table for the *sender* and never for the recipient, so
    //      a peer that has not propagated the statement goes on landing posts on
    //      the retired actor, and each delivery calls `maybe_send_push` on it.
    //      The same population as the key-package burn beside this one, one
    //      mechanism over.
    //   2. That push is dispatched by a consumer that AUTHENTICATES NOBODY. The
    //      nest POSTs to whatever URL the row holds; the identity-key refusal
    //      decides who may authenticate and has nothing to say here. The reach
    //      plane's own test, answered the same way as its two siblings.
    //   3. It is unrevocable from every app. There is no list kind at all — only
    //      subscribe and an unsubscribe that must NAME a device id — so no
    //      surface anywhere enumerates these rows, and after the ceremony they
    //      belong to an actor nobody can authenticate as. The `nest_pairings`
    //      stranding shape with a live feed in it.
    //
    // No `Partial` is available and the reason generalizes: a legitimate
    // pre-ceremony registration is indistinguishable from a thief's — a
    // subscription carries no attestation of who armed it. So the whole plane
    // burns, and what dies is one re-registration.
    //
    // ⚠ Declared residual, PRE-EXISTING and not created by this ruling: the
    // successor's own device does not re-register. Clients subscribe once behind
    // a permission gesture and remember that they did, while the successor is a
    // NEW actor with no row — so push is silently off for them under any ruling
    // (a `Move` would have carried the thief's endpoint along with the fix).
    // Tracked as a client leg, not answerable per-table here.
    ActorTable {
        table: "push_subscriptions",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "REACH the refusal plane cannot reach: the row is a caller-supplied \
             destination URL armed by a User-class gesture, a retired identity is \
             still delivered to (the inbox path refuses a superseded SENDER, never \
             a superseded recipient), and the dispatcher authenticates nobody — so \
             a thief-registered endpoint keeps receiving the account's arrivals \
             after the ceremony, with no kind anywhere that can list or revoke it",
        ),
        // EXPORT RULING (the residual set). ⚠ The
        // succession reason directly above is ALSO the export reason here,
        // which is the exception to this plane's own "never read the adjacent
        // axis's conclusion" rule and is worth stating as such: it happens to
        // transfer because it is not an argument about succession at all, but
        // about what the triple `(endpoint, key_p256dh, key_auth)` IS. It is a
        // sendable Web Push credential, "the dispatcher authenticates nobody",
        // and there is "no kind anywhere that can list or revoke it". Put that
        // in an archive an eviction token can fetch and you have minted an
        // UNREVOKABLE delivery capability that outlives every session — the
        // `invite_codes` unspent-code precedent a few entries over, where an
        // enrolment capability was dropped for the identical reason.
        //
        // `key_p256dh` is the harmless public half on its own and is omitted
        // anyway: alone it is worth nothing to the exporter, and beside the
        // other two it completes the triple. The row still rides — device_id,
        // transport and created_at are the owner's own "which of my devices
        // have push registered, on what transport, since when", which is a
        // real question and one no other table answers.
        export: Export::Redacted {
            omit: &["endpoint", "key_p256dh", "key_auth"],
            reason: "the three together ARE a sendable Web Push credential, and the \
                     dispatcher authenticates nobody -- so an archive copy is a live \
                     delivery capability for anyone holding the zip, reachable under the \
                     eviction token and revocable by NO kind this nest has (the \
                     succession entry beside this one says the same of a thief-registered \
                     endpoint). `key_p256dh` is the public half and worth nothing alone; \
                     it goes because it completes the triple. The registration itself -- \
                     which device, what transport, since when -- rides",
        },
    },
    ActorTable {
        table: "recovery_escrow",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the retired kit must not stay a live authorization root — the old row is \
             deleted in the transaction and the successor's kit ceremony writes a new \
             blob carrying the predecessor seed alongside the new one",
        ),
        export: Export::WithheldSecret(
            "the escrow blob is recovery-kit key material (its succession \
             reason above calls the row a live authorization root). Escrow \
             leaves the nest only through the recovery ceremony — never a \
             bulk channel a weaker eviction-token credential can pull",
        ),
    },
    ActorTable {
        table: "recovery_pending_replacements",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "succession outranks a pending seed-initiated replacement, unconditionally \
             and inside the transaction, so the landing sweep can never race a \
             committed succession",
        ),
        // The same class as `recovery_registrations`, one step earlier in the
        // chain: `record` is a `SignedRecoveryKeyRegistration` parked for the
        // grace window, plus its digest and the proposed new PUBLIC recovery
        // key. Nothing secret, by the same reasoning that migration states.
        //
        // And here the invariant argues actively FOR exporting it rather than
        // merely permitting it. A pending replacement is seed-initiated and
        // **vetoable instantly by the current RecoveryKey** — so a row here can
        // be an attacker mid-ceremony, and it is exactly the thing an owner
        // reviewing their own data most needs to see while the window is still
        // open. Withholding it would hide a live security event from the person
        // holding the veto.
        export: Export::Verbatim,
    },
    // The chain the ceremony ITSELF runs on, and the one table on this axis
    // where a `Move` breaks the very act that would perform it. Three consumers
    // read it *after* the succession has landed, all of them keyed on the
    // retired identity:
    //
    //   1. `push_succession_detached` reads the OLD chain to send alongside the
    //      statement, so a peer can verify without calling back — and returns
    //      early if it is empty. Moved, that read finds nothing and **no peer
    //      nest ever learns the succession happened**; the function's own
    //      comment calls the empty case "unreachable in practice", which a Move
    //      would make routine.
    //   2. `fauna.recovery.registration.chain` serves it to anyone verifying the
    //      hop, which is what makes the statement checkable at all.
    //   3. The successor's OWN fresh kit ceremony would break: the chain appends
    //      under a per-actor monotonic seq, so a chain moved onto the successor
    //      arrives with a head that outranks the seq-0 registration a new
    //      identity's first kit presents, and `append_recovery_registration`
    //      refuses it — the successor could then never register a RecoveryKey,
    //      i.e. **never succeed again**. The guardianship shape, one plane over.
    //
    // Nothing is stranded by staying: the successor mints its own chain from
    // seq 0 at the kit ceremony the aftermath already runs, and the predecessor's
    // chain is the immutable record its own signatures attest to — history in the
    // strict sense the classification rule means.
    ActorTable {
        table: "recovery_registrations",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "HISTORY, and load-bearing rather than incidental: this is the chain \
             the ceremony was verified against, still read under the RETIRED id \
             afterwards — by the peer push (moved, no peer ever learns of the \
             succession), by the public chain read, and against the successor's \
             own first registration, whose seq 0 a moved chain's head would \
             outrank, leaving the successor unable to ever register a kit again",
        ),
        // Its own migration rules the disclosure question outright: **"Nothing
        // here is secret — the same `recovery_pubkey` rides the actor's public
        // signed `Profile` — so the rows rest plaintext"**. `record` is the
        // verbatim canonical DAG-CBOR the owner's client SIGNED and submitted,
        // stored unmodified so the public serve path
        // (`fauna.recovery.registration.chain`) can replay exactly what was
        // signed. A signed public registration, already served on request.
        //
        // ⚠ Do not read `recovery_escrow`'s `WithheldSecret` across to this
        // table: the migration draws the line between the two itself, and the
        // escrow blob is the sealed *key* half while this is the published
        // *pubkey* half. That is the enum's discriminator, one table apart.
        export: Export::Verbatim,
    },
    // The account's own operational log of restores it performed on its own
    // corpus, rendered owner-scoped on its own Backups page (`ui/backups.md`
    // § Restore history) — the `sync_changes` / `spam_training_history` class,
    // not counterparty-facing attestation, so it moves with the corpus it
    // describes. Absent, the successor's page reports that the account has never
    // restored anything.
    ActorTable {
        table: "restore_history",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the backup/custody plane). The
        // record of restores performed for this actor, and the plane's easiest
        // call because the product already answers it: `ui/backups.md`
        // § Restore history renders these rows to the owner —
        // `restore-history-item[i]` shows completed-at, kinds restored, and the
        // source destination. An archive quieter than the page the user is
        // looking at is the wrong direction.
        //
        // ⚠ `source_member_id` is the one column that could name someone else
        // and does not: it is backup-destination PROVENANCE (which destination
        // member served the restore, NULL = "local snapshot"), rendered to this
        // same owner as a short hex label via `fauna_core::format::hex_short`.
        // A destination is a target this actor enrolled themselves.
        export: Export::Verbatim,
    },
    ActorTable {
        // The durable idempotency tier (db/rpc_idempotency.rs) — a replay
        // cache of the actor's own recorded replies; nothing to keep once the
        // actor is gone, and the rows expire on their own 7-day sweep anyway.
        table: "rpc_idempotency",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "a replay cache keyed to the retired identity's own envelope keys: the successor's \
             client mints fresh keys, so no replay of its ever matches these rows; the retired \
             identity can no longer authenticate to be served them; and the 7-day retention \
             sweep collects them regardless — moving them would serve one identity replies \
             recorded for another, for zero benefit",
        ),
        // EXPORT RULING (the account-record plane). A
        // replay cache: the actor's own recorded WS-RPC replies, keyed by
        // idempotency key, swept at 7 days. Its own entry comment already says
        // "nothing to keep once the actor is gone", and that is the export
        // answer too — a reply is a copy of an answer the client already
        // received and acted on, held only so a retried request is not executed
        // twice. ⚠ A second, independent reason to withhold rather than a
        // tiebreak: `reply` holds whatever a served reply contained, so a
        // `Verbatim` here would export an unbounded, unaudited slice of every
        // other verdict's decisions through a table nobody would think to
        // check. The class is operational, and the belt is that the cache
        // never becomes a back door around the axis.
        export: Export::WithheldOperational(
            "a 7-day replay cache of the actor's own recorded RPC replies, kept so a retried \
             request is not executed twice -- its own entry comment says nothing here is worth \
             keeping once the actor is gone. Withheld also because `reply` mirrors whatever a \
             served reply carried, so exporting it would route around every other table's \
             verdict through a cache nobody would think to audit",
        ),
    },
    ActorTable {
        table: "segment_records",
        column: "scope_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (**the last verdict in the
        // registry**; the backlog goes 1 -> 0 with this entry).
        //
        // **This table is the segment store's MIRROR, never its bodies.** A row
        // is a placement: `(kind, segment_id, record_cid, bucket)` plus the
        // mail-kind sparse floor. `record_cid` is a content ADDRESS -- the
        // 36-byte CARv2 index key the `.dat` is probed by -- exactly the
        // `content.blob_hash` / `web_files.blob_hash` class cleared in earlier
        // clusters: naming a record does not open one. The RFC822 bytes live in
        // the segment store and this verdict does not move them; whether the
        // export should REACH that store is a separate BUILD question (the
        // `include_blobs` analogue), not something a
        // verdict on the mirror can decide either way.
        //
        // ⚠ **`scope_id` holds TWO KINDS OF IDENTITY, and that is what makes
        // this verdict safe rather than a wrong-party leak.** Plan 6 T4 renamed
        // `actor_id` -> `scope_id` for the audience-scope generalization, so the
        // column is no longer an actor by construction. Measured, per kind:
        // `mail` scopes to the recipient actor, `post` to the AUTHOR actor
        // (`insert_post`'s own doc), `calendar` and `card` to the owner actor
        // (`insert_calendar`/`insert_card`), and `conv` to the **CHANNEL**
        // (`insert_conv` + `next_conv_seq`, "per-channel"). So the walk's
        // `scope_id = ?actor` match returns the owner's own records and can
        // never return a conversation scope, whose id is not an actor id at all.
        // This is the different-KIND-of-identity check reaching the one
        // table that holds both kinds in one column.
        //
        // The consequence was an INCOMPLETENESS, not a disclosure: the owner's
        // `conv` records were structurally absent from the export because they
        // are keyed by the shared channel. **CLOSED 2026-08-17 —
        // and NOT by widening this verdict**, which stays `Verbatim`: the rows
        // reach the archive through [`gather_conv_records`], a shaped domain
        // that resolves `actor_channels` membership first and then reads those
        // channels' `kind = 'conv'` rows into `export/conversations/records.ndjson`.
        // A second registry entry on a channel column would have been the
        // family plane's forbidden shape (a shared scope walked as if it were
        // the actor's); a `Shaped` verdict here would have been a lie, because
        // that domain row-filters to one kind and a partial `Shaped` is not
        // expressible. So the table keeps ONE actor-keyed verdict and the
        // channel-keyed kind gets its own door -- see that function for the
        // disclosure argument (a member already receives every record of their
        // channels over the ordinary serving path, and conv rows persist no
        // sender for the export to disclose).
        //
        // The mail floor rides as the owner's own mail metadata: `sender_dom` is
        // the correspondent domain (the `contacts.peer_id` class), `spam_disp`
        // the classification their own app already shows them, `received_at` /
        // `seq` / `changed_seq` / `tombstoned` the record's own coordinates.
        // `legal_takedown_ref` rides for a POSITIVE reason rather than by
        // default: `moderation.md` § Categories & enforcement makes a legal
        // takedown a visible tombstone plus appeal, never a silent removal, so
        // transparency to the author is the mechanism's point -- the
        // defer-to-the-owner-doc rule running permissive for the second time.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "sender_behavior",
        column: "actor_id",
        key: ActorKey::Blob,
        // The anti-spam behavioural profile: `get_behavioral_profile` counts
        // this actor's DISTINCT DM recipients over 1 h / 24 h / 7 d windows and
        // its DM response rate, and the fan-out figures are what mark a sender
        // as spraying.
        //
        // Same liability rule as the list meter above, and here the window is
        // what makes it bite: leaving the rows behind empties every window at
        // once, so the successor's very next burst is measured against zero
        // history — and the ceremony that emptied them is self-service. The
        // profile describes an ACCOUNT that continues; the world it sprays sees
        // the same handle either way, because the handle moves with the
        // account. A reset would make the nest's view wrong, not merciful.
        //
        // ⚠ `target_actor` beside it is the counterparty's side and is a
        // separate question, still `Unruled` in `SUCCESSION_REFERENCES` — do
        // not read this entry as having answered it.
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the anti-spam behavioural
        // profile: the raw event log `get_behavioral_profile` counts distinct
        // DM recipients and response rates over 1 h / 24 h / 7 d windows from.
        // The succession ruling above names its function exactly — a live
        // constraint on what this sender may do NEXT, not a record — which is
        // the `WithheldOperational` case: the nest's own abuse-control state
        // about the actor, meaningless off the box that enforces it.
        //
        // Nothing of the owner's is lost by withholding it: the sends it counts
        // are their own mail and messages, which ride their own planes, and the
        // windows it feeds are recomputed from scratch as the log ages.
        export: Export::WithheldOperational(
            "the anti-spam behavioural profile -- the raw DM/send event log the fan-out              windows are counted from, whose function (per the succession ruling on this              entry) is constraining what this sender may do NEXT rather than recording what              they did. Nest-internal abuse-control state, meaningless off the box that              enforces it; the messages it counts ride their own planes",
        ),
    },
    // ⚠ **The one table in this pass where a `Burn` is an active security
    // REGRESSION, and the reason is worth carrying to any future revocation
    // registry.** A ShareToken is client-minted, stateless and self-verifying;
    // registration here is deliberately NOT a serving gate, so
    // `GET /share/{token}` serves an unregistered token perfectly well and
    // consults this table for one thing only — to refuse a **revoked** one with
    // 410. `is_share_token_revoked` answers `false` for a row that is not there.
    // So deleting the row of a token the owner already revoked does not end that
    // token: it **un-revokes** it. A burn would resurrect every share link the
    // account had ever killed.
    //
    // Moving is what the successor actually needs, and it is the only remedy
    // that exists. `revoke_share_token` is scoped to `author`, and `fauna.share
    // .list` reads by `author`, so a registry left on the retired identity is
    // both invisible and unrevocable to the successor while the links keep
    // serving — the `nest_pairings` breach of `nest/common.md`
    // § Client-state recoverability. Carried across, the successor sees every
    // live share and holds the kill switch, and rows already revoked stay
    // revoked because the flag travels with the row.
    //
    // ⚠ **Declared residual, and no per-table verdict can reach it:** a token
    // the thief minted but never registered has no row to move, and the serve
    // path's own author check (`is_actor_registered`) still passes, because the
    // ceremony releases the retired identity's handle but keeps its `users` row.
    // So an unregistered pre-succession link keeps serving until its own
    // `expires`. Closing that needs a serve-side gate on the author's succession
    // state, which is a behaviour ruling rather than a table ruling.
    ActorTable {
        table: "share_tokens",
        column: "author",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // Reads as a bearer-token table and is not one. `token_id` is the
        // BLAKE3 of the canonical signed wire bytes, derived by the nest from
        // the base64url string the client registered
        // (`share_routes.rs:35-41`); the artifact that actually serves a file
        // is that **signed** ShareToken presented in the URL path, which
        // `handle_share` verifies by Ed25519 over the CID before this table is
        // consulted at all. The row is the revocation handle, and a BLAKE3
        // digest does not reconstruct its preimage: the nest never stores the
        // bearer artifact, so the export cannot leak it.
        //
        // What the row does carry is the owner's own share list -- filename,
        // manifest hash, expiry, public flag, revoked flag -- which is the
        // sort of thing the export exists to hand back. `filename_sealed` is a
        // SEALED LABEL over the owner's own filename: sealed CONTENT, which
        // rides in at-rest form per the enum's sealed-columns clause.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "spam_baseline_inclusions",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // The row follows the model and the opt-in it records (`spam_models`
        // and `spam_preferences` both Move): a succession is not a departure,
        // so the successor's unchanged model must still read as the one that
        // was summed, not as a join (`mail-spam.md` § Cold start Path 2 → *The
        // floor applies to every published DELTA*).
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldOperational(
            "the spam baseline's inclusion record -- which contributors the last served \
             deployment publish summed, and the model write time it summed -- is nest-internal \
             bookkeeping the delta floor counts changes against. The goal doc rules it never \
             served to any client, the admin included; it is ABOUT the actor's contribution, \
             not data they made, and the model and the opt-in it points at already ride the \
             export verbatim",
        ),
    },
    ActorTable {
        table: "spam_model_holder_copies",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Burn(
            "the at-rest artifact of a CONSENT given by an identity that no longer \
             exists — one copy of the actor's model sealed to the deployment's baseline \
             holder, readable by neither the retired identity nor the successor. The \
             product already treats deleting these as the right answer to a withdrawn \
             consent: opting out of `contribute_baseline` calls \
             `delete_spam_model_holder_copies`, and a succession is a consent boundary. \
             Nothing irrecoverable dies — the model itself moves, and re-contributing is \
             one toggle. Carrying them would instead attribute a retired identity's \
             contribution to a live one",
        ),
        // ⚠ **Not the sealed-content ride it looks like.** `sealed_copy` is
        // HPKE-sealed to the aggregation holder's pubkey, so it is content
        // rather than key material and the enum's discriminator would let it
        // ride — but the migration answers a different question first: this is
        // *"Recreatable derived data (the contributor's agent re-seals a fresh
        // copy on its next write)"*. The plaintext source is the contributor's
        // own model, held by their own client/agent, which is what produced and
        // sealed this copy in the first place.
        //
        // So the export would hand the owner an opaque blob they cannot open
        // (it is sealed to the holder, not to them), derived from a model they
        // already hold, and regenerated on their next write. That is
        // `WithheldDerived`'s definition exactly — bytes, not data the user
        // lacks — and the succession ruling above reaches the same place from
        // the other side: it is readable by "neither the retired identity nor
        // the successor".
        export: Export::WithheldDerived(
            "recreatable derived data, per its own migration: an HPKE copy of the \
             contributor's own spam model sealed to the aggregation holder, re-sealed by \
             their agent on the next write. The owner cannot open it -- it is sealed to \
             the holder -- and its plaintext source is the model their own client already \
             holds, so exporting it adds bytes, not data the user lacks",
        ),
    },
    ActorTable {
        table: "spam_models",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the account's own trained
        // spam model: the n-gram weights plus the ham/spam counts behind them.
        // Trained from the owner's own labelling of their own mail, held for
        // them, and reconstructible from nothing else — the mail it learned
        // from ages out while the weights persist (`spam_training_history`'s
        // own migration says so). Their model, their archive.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "spam_preferences",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the user's own anti-spam
        // settings: their thresholds and each
        // sharing opt-in (`contribute_baseline`, `share_reports`,
        // the Layer-B signal flag). Settings a user chose, every one of them
        // set through their own app, and the opt-in columns are exactly what
        // `principles.md` § The user always controls their data means by an
        // auditable choice — an archive that omitted them could not tell the
        // owner what they had agreed to.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "spam_training_history",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the audit trail of the
        // owner's own training actions: which message, in which mailbox, its
        // subject, the label they gave it, and the delta that applied. Their
        // own decisions about their own mail. The subject and the delta ride
        // sealed under the actor's own recipient key (the only form they
        // rest in), which is the sealed-columns clause again.
        //
        // ⚠ **Its migration calls this "derived/re-creatable audit metadata",
        // and that does NOT make it `WithheldDerived`.** The two questions are
        // different and this is the entry where the distinction gets written
        // down: a *retention* comment asks "does ageing this out destroy
        // something user-irrecoverable?" (no — the weights persist in
        // `spam_models`), while `WithheldDerived` asks "can the reader
        // re-derive it from what the archive already carries?" (no — no set of
        // weights tells you which messages were trained, or when). A table can
        // be safely age-out-able and still be irreplaceable to its owner.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "subscribe_requests",
        column: "author_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Pending subscribe/unsubscribe requests addressed to the author.
        // Undrained requests are inbound work the successor inherits along with
        // the tiers they name. ⚠ `author_id` half only -- the requesting
        // reader's `subscriber_id` is ruled in `SUCCESSION_REFERENCES`, and its
        // DELETION by a hand-written leg of the purge walk
        // (`subscribe_requests_purge_for_deleted_subscriber`: their pending
        // `subscribe` rows go, their `unsubscribe` rows stay).
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the subscription plane). The
        // inbound queue of subscribe/unsubscribe requests ADDRESSED TO this
        // author — the same two-party shape as `subscribers` above and the same
        // answer: the author works this queue by hand in their own app
        // (`monetization.md` § Pillar 1 → §2, the approve/reject buttons), so
        // the requester is already named to them. `kind` distinguishes the
        // subscribe from the unsubscribe leg, `payment_entitled` records
        // whether a provider webhook has already vouched, and `payload` is the
        // request's opaque envelope. `mlkem_encaps_key` is the requester's
        // PUBLIC encapsulation half, as on `subscribers`.
        export: Export::Verbatim,
    },
    // The change journal for the corpus that already moves. `folders` is
    // `Move(Plain)` and § Re-key scope re-points corpus ownership in the
    // ceremony, so leaving the journal behind is the `web_subdomain_enabled`
    // shape: the successor owns every byte, and a journal left behind would
    // attribute the successor's own versions to an actor that no longer
    // exists. Every reader keys on `folder_id`, so none of them breaks either
    // way — which is precisely why this table could sit un-ruled unnoticed.
    //
    // `Plain` is the whole leg: `seq INTEGER PRIMARY KEY AUTOINCREMENT` and no
    // unique constraint names `actor_id`, so nothing can collide on the
    // terminal and `UPDATE OR IGNORE` moves every row.
    //
    // ⚠ Declared consequence, so it is not rediscovered as a defect: in a
    // SHARED set the predecessor's own rows carry the version authorship other
    // members render, so those versions re-attribute to the successor. That is
    // deliberate and distinct from § Re-key scope's signed-history row (posts,
    // messages, profile versions stay attributed) — this is the account's own
    // operational journal rather than counterparty-facing attestation, a
    // succession is identity rotation rather than a change of person, and the
    // predicate touches only rows the predecessor wrote.
    ActorTable {
        table: "sync_changes",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the sync plane). The account's own
        // file-change journal, and the table the `restore_history` entry already
        // names as its class ("the `sync_changes` / `spam_training_history`
        // class, not counterparty-facing attestation"). Every version the owner
        // can restore, every conflict resolution they recorded and every
        // manifest their devices uploaded is a row here; an archive without it
        // is an archive of the *current* corpus with none of its history.
        //
        // No column is key material, which is the only reading that could have
        // gone the other way on a table this wide. `content_key_version` is a
        // GENERATION NUMBER selecting a key the reader already holds
        // (`key_for(version)`), never a key. The two sealed columns ride in
        // at-rest form under the axis's own rule, and both are sealed
        // *content* rather than a wrapped key: `path_sealed` is the owner's own
        // path under `SealedLabel`, and `entry_sealed` is the class-2 state
        // entry the nest stores and echoes byte-for-byte holding no key that
        // opens one. The plaintext `path` sibling is scrubbed where its seal
        // rests (schema v32, the S9 path-sealing flip); where it still rests it
        // is the owner's own path either way.
        //
        // ⚠ The one fact worth stating because it looks like a defect on a first
        // read: `actor_id` is the RECORDING connection actor, nest-stamped at
        // insert, so with writer members in a shared set it is no longer always
        // the set owner (the NB on the migration, and file-sync.md § Multi-writer
        // shared sets). That reads the RIGHT way for a per-actor export — the cut
        // returns exactly the rows this actor recorded, wherever they recorded
        // them — and the consequence in the other direction is an
        // incompleteness, not a leak: a set owner's archive does not carry the
        // rows their members recorded in their set. That is the per-actor
        // boundary this whole axis is cut on, not a gap in this verdict.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "upload_leases",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "the exclusive-editing lease is a 300-second claim by the account that took \
             it (`file-sync.md` § Exclusive editing), bound to the actor so a writer \
             member naming the holder's client-asserted device id can neither release, \
             renew nor take it. Re-pointing it at the successor would hand the retired \
             identity's claim to the live one for the rest of its TTL, and burning it \
             would free a folder mid-upload; leaving it names the retired actor, which \
             nobody can act as, so the row lapses on its TTL and the successor's seats \
             re-acquire — the same outcome as a crashed holder",
        ),
        export: Export::WithheldOperational(
            "a lock row with a 300-second life -- who is editing a folder right now. \
             Nothing in it is the owner's durable data: the lease is re-taken by the \
             next upload pass, and the folder's `exclusive_editing` choice, which IS \
             theirs, rides the folder row",
        ),
    },
    ActorTable {
        table: "sync_devices",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "compromise, and the one ruling on this plane that was already RATIFIED \
             rather than reasoned here: `succession-aftermath.md` § Re-key scope \
             names device registrations and renewal grants *die with the old rows; \
             re-created at fleet re-import*, and § Propagation says the same in \
             prose. It is ruled anyway, because `Unruled` and a ratified `Stay` are \
             indistinguishable in the registry and that gap is exactly what let \
             `atproto_authoring_keys` sit un-carried under a ruling that had \
             shipped. Staying is also fail-closed rather than merely inert: the \
             row's `auth_grant` is re-verified at every mint and \
             `auth_core::device_auth_core` consults `refuse_if_superseded` first, \
             so a retired identity's renewal grant cannot mint — which is what \
             makes a `Move` both unnecessary and wrong, since the stored \
             authorization names the predecessor and would fail its own \
             `auth.actor_id` check on the successor's row anyway. ⚠ One declared \
             consequence: `guardian_marked` (family-safety.md § Full visibility) \
             does not survive a ward's own ceremony, so a supervised ward's \
             re-enrolled devices come back unmarked and the guardian must re-mark \
             them — degraded, not stranded, and the guardian is already in the \
             loop because the enrolled device re-enrols under the new identity.",
        ),
        // EXPORT RULING (the shaped-domain plane). The
        // `devices.json` domain carries four of ten columns; the six it drops
        // are `guardian_marked`, `auth_device_key`, `auth_grant`,
        // `label_sealed` and `capabilities`. Two of those needed a read rather
        // than a reflex, and both came back benign:
        //
        // ⚠ `auth_grant` LOOKS like the bearer credential this axis exists to
        // withhold and is not one. `device_auth_core` (auth_core.rs:456)
        // verifies a **fresh signature by the renewal device's private key**
        // over a domain-tagged handshake message; only then is the stored
        // grant re-decoded and its root envelope re-verified. Its own doc says
        // the row is *"just a cache of what the identity client signed, never
        // itself an authority"*, and `auth_device_key` is by name the PUBLIC
        // half. So the archive carries no key that opens anything — the
        // same `actor_epoch_seal_keys` shape, where the secret-sounding name
        // was the trap and the schema was the answer.
        //
        // ⚠ `guardian_marked` is a supervision flag on a supervised ward's
        // row, so the disclosure question is real and `family-safety.md`
        // § Full visibility answers it directly: *"The child's device list
        // renders the mark (transparency — an additive field on the
        // `fauna.sync.devices.list` reply)"*, and the enrolled-device pattern
        // is *"transparent by construction"*. Exporting it to the ward
        // discloses nothing their own device list does not already show, and
        // withholding it would make the archive quieter than the live UI.
        // The same fact (3) applied in the permissive direction: the owner
        // doc rules the plane's disclosure, so defer to it either way.
        //
        // `label_sealed` is a sealed label over this actor's own device name
        // under their own root (sealed-columns clause); `capabilities` and
        // `registered_at`/`last_seen` are their own fleet's state.
        export: Export::Verbatim,
    }, // already covered by delete_sync_devices_for_actor; kept for enumeration completeness
    ActorTable {
        table: "revoked_device_grants",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Stay(
            "the revocation memory for deleted devices' renewal grants \
             (sync-agent.md § Credential model — device revocation) stays with \
             the retired identity, mirroring the `sync_devices` Stay it \
             tombstones for: a tombstone names the OLD actor's (actor, renewal \
             key) pair, and the successor's grants are minted over fresh keys \
             under the new actor id, so a moved tombstone could never match \
             anything and a stayed one is fail-closed — the retired identity's \
             revoked keys remain refused if anything ever consults them again. \
             Purge on account deletion is safe for the same reason the grant \
             plane allows it: a deleted account has no sessions and \
             device_grant.register requires an authenticated caller, so no \
             grant of a purged account can re-attach regardless of memory.",
        ),
        // A revocation memory, and every column of it is public or a clock:
        // `auth_device_key` is the PUBLIC renewal key of a device the owner
        // deleted, and `revoked_at` is when. A tombstone's whole function is to
        // be matched against and refused, so knowing one grants nothing — it is
        // the inverse of a credential.
        //
        // It is also the owner's own security history: which of their devices
        // were revoked, and when. `principles.md` § The user always controls
        // their data puts that on the export side of the line, and the paired
        // `sync_devices` plane it tombstones for is a shaped domain, so
        // withholding this would leave the export showing devices without
        // showing that any were ever revoked.
        export: Export::Verbatim,
    },
    // The `DeviceAuthorization` each delegated change-record signer verified
    // under at ingest (`mls-group-key-material.md` § M2 → *Writer-signed
    // change records*) — read ONLY by the list replies' `signer_certs` side
    // table (`sync_handlers::signer_certs_for_signers`), never at ingest,
    // which resolves a signer by reference over `sync_devices` grants and the
    // `revoked_device_grants` tombstone (`change_signature::CertCarriage`). So
    // the table is verification material for rows already accepted, and holds
    // no authority of its own. Rows keyed on a FOREIGN actor (a federated
    // writer's inline cert) are never an account of this nest's, so neither
    // leg below ever reaches them.
    ActorTable {
        table: "sync_signer_certs",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // Move, not the `sync_devices` Stay, because the certs FOLLOW THE ROWS
        // THEY VERIFY: `sync_changes` is `Move(Plain)`, and the side table is
        // looked up by `(row.actor_id, signer_key)`, so a stayed cert would
        // leave every moved signed row `CertMissing` on its readers. Moving
        // one authorizes nothing (ingest never reads this table), and the
        // bytes are not rewritten — the cert still names and is root-signed by
        // the PREDECESSOR, which is the chain's succession-crossing case
        // (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
        // ruling (8): the reader recovers the signed actor from the signature
        // and admits it as a predecessor of a writer). `Plain` is the whole
        // leg because no move can collide under the table's key
        // `(actor_id, device_key, cert_actor_id)`
        // (`writer-signed-change-records.md` ruling (8)(i)): ingest stores a
        // cert only under the recorder it names, so a row is written with
        // `cert_actor_id` equal to its `actor_id`, and a move changes
        // `actor_id` alone — the rows moved onto the successor name a
        // predecessor in `cert_actor_id`, and the successor's own name it.
        // `cert_actor_id` itself never moves: it mirrors the signed bytes.
        succession: Succession::Move(MoveShape::Plain),
        // Every column is public or a clock: `device_key` is a device
        // principal's PUBLIC key and `cert` is the root-signed authorization
        // every `changes.list` reply already serves to each reader of the set.
        // It is the owner's own device-certification history beside the
        // `sync_changes` rows it verifies (exported Verbatim), and an archive
        // without it would carry signatures its reader cannot check.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "web_apex_actor",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the nest-wide designation of
        // one actor as the apex site, set by the ADMIN rather than by the actor.
        // The same designation class settles it (`nostr_zap_signers`,
        // `backup_writer_grants`): the row's existence IS the authority, and
        // knowing it grants nothing — so it rides to the actor it designates,
        // who is the one party the fact is about.
        //
        // ⚠ A singleton (`id = 1`), so at most ONE actor's archive on a
        // deployment ever contains this table, and every other actor's export
        // omits it by the ordinary zero-rows rule rather than by a verdict.
        // That is worth saying because a reader comparing two archives from the
        // same nest would otherwise read the difference as drift.
        export: Export::Verbatim,
    },
    // The three rows below plus `web_subdomain_enabled` are the **routing half**
    // of the web-serving plane, and until 2026-08-12 they sat un-ruled while
    // `web_apex_actor` and `web_files` — the half the pre-registry hand list
    // happened to name — moved. The result was the `account_aliases` shape
    // again: the successor owned the files and nothing could reach them.
    //
    // `web_domains` is the sharpest of the four, because the stranding is
    // **unrecoverable from any app** and therefore a
    // `nest/common.md` § Client-state recoverability breach, not merely a
    // regression. Left on the retired identity: the boot resolver still maps
    // the custom host to the retired actor (`lib.rs`'s `list_all_web_domains`
    // seed), so the site 404s against a successor that holds every file;
    // `get_web_domains_for_actor(successor)` returns nothing, so the app cannot
    // even show the domain; `fauna.web.domain.delete` refuses the successor
    // (`web_handlers.rs`'s "domain registered by another actor"); and
    // re-registering hits the `UNIQUE(domain)` bail. The old key is refused
    // everywhere by construction, so *nobody* can ever free that domain again.
    ActorTable {
        table: "web_domains",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the custom domains the owner
        // registered for their site, with the verification state and clock.
        //
        // ⚠ `verify_token` reads like a credential and is not one: it is the
        // DNS challenge value the owner must PUBLISH in a TXT record to prove
        // control, so its whole lifecycle is public disclosure by the owner,
        // and their app has to show it to them for the flow to work at all. An
        // archive that dropped it would withhold the one string a half-finished
        // verification needs to resume.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "web_files",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the SOURCE of the owner's
        // published site: every path they uploaded, its content type, the
        // manifest hash the serve path walks to the blob store, the `web`-mode
        // folder it synced from, and the content-key generation its chunks are
        // sealed under. The authored artifact, held for them.
        //
        // The bodies are content-addressed and ride under `include_blobs`
        // exactly as the shaped domains' blobs do, so this is the same
        // index-here/bytes-there shape as `bridge_imap_messages` — but without
        // that entry's honesty problem, because the blob half of THIS one is
        // already a built export leg rather than an unruled table.
        export: Export::Verbatim,
    },
    // Derived output of the files that already move — the rendered HTML twins
    // `serve.rs` reaches by `(actor_id, path)`. Left behind they are orphaned
    // ciphertext/bytes nothing can serve, and the successor's site loses exactly
    // the pages the renderer produced.
    ActorTable {
        table: "web_rendered",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — **`WithheldDerived`, and
        // this pair is what makes that variant's test precise.**
        //
        // The prior ruling narrowed `Derived` to *re-derivable from what the archive
        // carries* and had to rule the trending cache `Operational` because its
        // input is every other actor's engagement — data no archive of this
        // owner will ever hold. The render cache is the other side of exactly
        // that line: it is a post-processing projection of `web_files`, which
        // rides `Verbatim` right above, with the bodies under `include_blobs`.
        // The owner lacks no *data* by its absence; they lack a byte-identical
        // artifact the nest rebuilds on every re-render.
        //
        // ⚠ So the usable test for the remaining backlog is one question:
        // **is the input in the archive?** Yes → `Derived`. No → it is either
        // the owner's own plane (`Verbatim`) or the nest's (`Operational`), and
        // "the system can rebuild it" decides neither.
        export: Export::WithheldDerived(
            "post-processed render output, cleared and rebuilt on every re-render. It is a              projection of `web_files`, which the archive carries `Verbatim` with its bodies              under `include_blobs` -- so the owner lacks no data by its absence, only a              byte-identical artifact the nest regenerates. The input being IN the archive is              what makes this Derived rather than Operational (cf. `content_scores`, whose              input is every other actor's engagement)",
        ),
    },
    // The owed-render marker beside the rows it promises to revoke. It MOVES
    // with them: `web_rendered` follows the account to its successor, so a
    // render still owed at the ceremony is owed by the successor's site — left
    // behind, the boot drain would render the retired identity (nothing to
    // clear) and the moved stale page would serve on.
    ActorTable {
        table: "web_render_owed",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldOperational(
            "the nest's own bookkeeping that a render of this actor's site is still owed \
             (a nonce and a timestamp) -- it says nothing the site's own source, which rides \
             `web_files` Verbatim, does not, and is meaningless off-box",
        ),
    },
    // Same, for the paywalled slice. Moving cannot leak: the bytes are sealed
    // under a tier period key `serve.rs` resolves from the **live** grant
    // registry, and `capability_grants` is a `Burn` — so a sealed page stays
    // dark until the successor re-mints its own grants (the aftermath's grant
    // re-mint leg), which is the bounded-and-visible shape the burns exist for.
    // ⚠ The tier plane those keys come from (`current_key_blobs`,
    // `subscription_tiers`, `subscribers`, …) is still `Unruled`: this ruling
    // restores parity with `web_files` and deliberately settles nothing about
    // monetization.
    ActorTable {
        table: "web_rendered_sealed",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — `web_rendered`'s paywalled
        // twin, same verdict on its own migration's word: *"derived render
        // output — cleared and rebuilt on every re-render (recreatable, like
        // `web_rendered`)"*.
        //
        // ⚠ Worth noting what is NOT the reason, because the columns invite it:
        // `blob_hash` points at ciphertext sealed under a key derived from a
        // tier period key, so a "sealed ⇒ withhold" reflex would land on the
        // right verdict for the wrong reason and teach the next reader
        // backwards. Sealed CONTENT rides on this axis; what keeps
        // this table out is that it is a REGENERATED projection, and `tier` +
        // `post_id` beside it are floor metadata already carried plaintext on
        // the gated post record.
        export: Export::WithheldDerived(
            "the paywalled twin of `web_rendered` and withheld for the same reason its own              migration gives -- derived render output, cleared and rebuilt on every              re-render. Not withheld for being sealed: sealed content rides this axis, and              `tier`/`post_id` are floor metadata the gated post record already carries              plaintext",
        ),
    },
    // The owed restore of a site a fail-closed clear took dark. It MOVES for
    // the owed-render marker's reason turned around: the successor inherits
    // the site's inputs, and a dark site it inherits is still owed its render.
    ActorTable {
        table: "web_restore_owed",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldOperational(
            "the nest's own bookkeeping that this actor's rendered site was cleared by a \
             failed render and is owed another attempt (a nonce and a timestamp) -- it says \
             nothing the site's own source, which rides `web_files` Verbatim, does not, and \
             is meaningless off-box",
        ),
    },
    // The render generation that keeps two overlapping renders of one actor
    // from writing over each other. It MOVES with the rendered rows it fences:
    // a successor that inherited the site but not the counter would restart it
    // at 1, and a render the succession caught in flight — holding a claim from
    // the retired account's sequence — could match that fresh 1 and write its
    // pre-succession pages onto the successor's site.
    ActorTable {
        table: "web_render_generation",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldOperational(
            "a counter the nest bumps once per render of this actor's site, so that two \
             overlapping renders cannot write over each other -- it says nothing about the \
             site's content, which rides `web_files` Verbatim, and is meaningless off-box",
        ),
    },
    // The bodies a render still holding this actor's site has stored and not
    // yet committed — the blob sweep's reference for them. It MOVES with the
    // generation it belongs to, for the same reason and to the same harmless
    // end: the succession caught that render in flight, its claim names the
    // retired account and can no longer write, so the moved rows only keep its
    // bodies until the successor's first render begins and deletes them.
    ActorTable {
        table: "web_render_staged",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldOperational(
            "the content addresses of page bodies an in-flight render of this actor's site \
             has stored and not yet committed, kept only so the blob sweep does not take them \
             mid-render -- it says nothing about the site's content, which rides `web_files` \
             Verbatim, and is meaningless off-box",
        ),
    },
    // The subdomain opt-in. Left behind it fails **silently and asymmetrically**:
    // boot reads the retired actor out of this table and asks for its handle,
    // which the ceremony moved, so `lib.rs`'s `Ok(_) => {}` arm skips the
    // registration and `<handle>.<nest>` goes dark — while the successor's app
    // reads its own (absent) row and renders the toggle as *off*, so the state
    // the user sees and the state that stranded them never disagree out loud.
    ActorTable {
        table: "web_subdomain_enabled",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — a per-actor opt-in the user
        // set themselves (`fauna.web.set_subdomain_enabled`), with
        // presence-as-flag semantics: the row exists only because they chose
        // subdomain hosting, which its own migration ties to
        // *"privacy / user-controls-their-data"*. A row that exists only as a
        // record of the owner's choice is the plainest `Verbatim` on this axis.
        export: Export::Verbatim,
    },
    // Appended by the completeness walk — found
    // by the walk itself, not missed by hand review; append, don't reorder.
    // SUCCESSION RULING (2026-08-15) — the other half of a
    // composition whose first half moves, so a split is the only wrong answer.
    //
    // These rows are the user's own global factor set, folded into **every** one
    // of their feeds' composed orderings (`resolve_global_factors`;
    // `get_global_factors`/`set_global_factors` are owner-scoped, the latter a
    // whole-set overwrite). `feeds.composition` moves with the feed row one
    // ruling above; leave these behind and the successor's feeds compose from
    // half the input they had the day before — the ordering changes and nothing
    // says so. The app reads its own absent row and renders the set as *empty*,
    // so the state the user sees and the stale rows never disagree out loud:
    // the `web_subdomain_enabled` silent-asymmetry shape.
    //
    // Recreatable in one gesture, so nothing user-irrecoverable rides on it —
    // the ruling turns on the *split*, not on loss.
    ActorTable {
        table: "feed_global_factors",
        column: "owner",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the owner's global feed
        // factor weights, folded into every one of their scored feeds. Its
        // migration draws the line for us: **"Transparent factor preferences
        // only — sealed tier-1 factor data never lands here"**, so the whole
        // table is by construction the user-authored, user-readable half of
        // personalization. Replaced wholesale on `fauna.feed.factors.set`,
        // i.e. exactly what the user last chose.
        export: Export::Verbatim,
    },
    // SUCCESSION RULING (2026-08-14): NOT merely the derived cache the
    // deletion note below describes — `actor_id` is the trust boundary
    // `content_score_owner` enforces on `submit_scores` (a capability holder's
    // claimed owner must match the row). Left behind, every legitimate re-score of the successor's
    // legacy corpus is refused forever: the pre-ceremony grants burn and
    // re-mint naming the successor, while the binding still names the
    // predecessor — the scanning plane silently stops updating for the whole
    // legacy mailbox. `actor_id` is not in the PRIMARY KEY, so the plain leg
    // cannot collide.
    ActorTable {
        table: "content_scores",
        column: "actor_id",
        key: ActorKey::Blob,
        // Derived trending-score cache (`db/trends.rs::recompute_trend_score`,
        // INSERT OR REPLACE / DELETE) -- never a source of truth, safe to purge.
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the trending-score cache the
        // nest recomputes and serves rankings from; its own registry comment
        // above calls it never a source of truth.
        //
        // ⚠ **`WithheldOperational`, NOT `WithheldDerived`, and the reason
        // sharpens the vocabulary for every remaining table.** The `Derived`
        // variant's own words are *"re-derivable from data the export already
        // carries"* — a claim about what the ARCHIVE'S READER can reconstruct.
        // A trend score is computed over every actor's engagement with the
        // item, so its owner cannot recompute it from their archive no matter
        // what the archive holds; what is true is that the NEST recreates it at
        // will and it decides nothing for the owner. That is serving state, and
        // the honest class is `Operational`.
        //
        // The distinction is not pedantry: `WithheldDerived` is the one class
        // that tells the owner *"you already have this"*, and it must not be
        // used to mean *"the system can rebuild it"* — those diverge exactly
        // when the input is other people's data.
        export: Export::WithheldOperational(
            "the trending-score cache the nest recomputes and serves rankings from -- never a              source of truth (its registry comment says so). Deliberately NOT              `WithheldDerived`: that variant means re-derivable from what the ARCHIVE              carries, and a trend score is computed over every actor's engagement with the              item, so its owner could never recompute it. What is true is that the nest              rebuilds it at will and it decides nothing for the owner -- serving state",
        ),
    },
    ActorTable {
        table: "folder_channel_claims",
        column: "claimed_by",
        key: ActorKey::Blob,
        // DELETION RULING (closing the succession axis's
        // open question above): a claimed channel exists ONLY to serve the ONE
        // folder its claimant bound to it (a set never
        // re-binds to a different group), so once that owner's `folders` row
        // is gone, `content_key.put` / `members.evict` / an MLS Commit can never
        // again find a caller == claimant — the channel is permanently dead, not
        // merely orphaned. The real fix is NOT this registry flip alone: it lives
        // in `sync_storage::delete_folder_rows_in_tx`, which — for a BOUND set,
        // on both `delete_folder_for_user` and `delete_all_folders_for_actor`
        // — tears down `folder_channel_claims` / `actor_channels` /
        // `folder_member_access` / `folder_content_keys` for the WHOLE
        // channel, not just this owner's rows, so every OTHER member's roster
        // membership doesn't survive as an un-rotatable, un-leavable stub with
        // no owner left to evict them. `delete_all_folders_for_actor` runs
        // before this registry's own sweep in `finalize_user_deletion`, so by
        // the time `Policy::Purge` fires here it is normally a no-op; Purge (not
        // Retain) is still the honest policy — a claim can never validate again
        // once its owning set is gone, so purging a stray one is always safe,
        // and this is the registry's defense-in-depth backstop, not the primary
        // mechanism.
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the sync plane). Three columns,
        // `claimed_by` is the exporter, and the row IS an authority the owner
        // holds: it records them as the single authorized folder owner of the
        // channel, which `share` / `content_key.put` / `members.evict` each
        // check before admitting a caller (`claim_folder_channel`). Telling an
        // owner which channels they claimed, and when, is squarely the audit
        // `principles.md` § The user always controls their data puts in their
        // own hands.
        //
        // Not `WithheldDerived`: nothing else in the archive carries this. The
        // `folders` rows (`Verbatim`) say which set binds which channel; they do
        // not say who holds the claim on it or when it was taken, and those two
        // facts are exactly what makes an un-rebindable channel legible after a
        // restore.
        export: Export::Verbatim,
    },
    // SUCCESSION RULING (2026-08-14): `author` is the nest-side
    // OWNERSHIP pointer, never the attribution — "posts stay attributed" binds
    // the SIGNED post bytes, which no leg touches (which is why moving
    // `segment_records.scope_id`, this table's post-cutover twin, was never
    // controversial). Everything that consults the column resolves FORWARD
    // through it: the delete rule (`routes::check_post_delete_authorization` /
    // `resolve_post_author` — the live defect this ruling closes: a successor
    // could not delete its own legacy posts, and the two stored-author checks
    // are conjunctive, so a cutover post whose scope had moved was refused by
    // its un-moved `content` twin), the own-post enumeration
    // (`list_authored_posts_page` — fauna.posts.list read empty), notification
    // routing (`get_content_author`), quarantine author-visibility
    // (`db/moderation.rs::is_author`), and the deletion-axis federation
    // retraction (`retract_actor_posts` walks this column, so a post-succession
    // account deletion would have skipped the legacy catalogue). The AP-writer
    // question the row asked is ANSWERED, not waved off: every production
    // writer (`put_post` family, bridge ingest via `put_post_with_source*`,
    // `insert_post_index_entry` feed stubs, the nostr inbound path, the atproto
    // projection) derives `author` from the decoded post payload's own author
    // field, so `author = ?old` matches exactly the rows the retired identity
    // signed — a remote-authored row carries the remote author's id and the
    // undecodable-raw arm writes a zero author, both structurally unreachable
    // by the plain leg. No `Partial` is needed.
    ActorTable {
        table: "content",
        column: "author",
        key: ActorKey::Blob,
        policy: Policy::Retain(
            "posts; deletion must route through the federation-aware per-post retraction (routes::delete_post_core -- nostr kind-5, Bluesky write-through, paired replica, AP Delete), never a raw purge. BUILT: pending_actions::retract_actor_posts runs that path for every post the actor authored, and finalize_user_deletion takes Arc<AppState> precisely so it can, BEFORE this sweep reaches the tables the propagation legs need (nostr_accounts holds the signing nsec, ap_post_map is the AP push witness -- both Purge). Rows are therefore normally gone before this sweep runs; Retain stays the honest policy because a raw purge of a post that retraction skipped would destroy it without ever telling the federated copies.",
        ),
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the shaped-domain plane). The
        // completeness gap with the sharpest user-facing edge in the whole
        // backlog, and the one the shaped half hides best. `posts.json` looks
        // like the domain that carries this table, and it row-filters
        // `schema LIKE 'post/%'` (`list_posts_by_author`, posts.rs:608) — so
        // everything this actor AUTHORED that is not a post is absent from
        // their archive, silently, under a domain named after the table.
        // Delivered content reaches them from the other side (`content_links`
        // → `inbox.json`), but nothing carries the authored non-post rows
        // themselves.
        //
        // `author` IS the exporter, so an actor-scoped read returns their own
        // authorship and nobody else's, and `payload` is sealed content, which
        // rides in at-rest form by the enum's sealed-columns clause.
        //
        // ⚠ DECLARED PARTIALITY, and the honest edge of this verdict: where a
        // payload is offloaded to the PayloadStore the at-rest row holds a
        // REFERENCE plus `blob_hash`, not the bytes — so `Verbatim` here
        // carries the complete row and not always the complete content. That
        // is the same seam `include_blobs` already answers for posts, and it
        // is the near half of the mail-BODY question this row names as its own
        // open cluster (`segment_records`). Ruling that seam is deliberately
        // NOT attempted here: the bytes are not in `nest.db` at all, so it is
        // a decision about whether the export reaches the segment store, not a
        // per-table verdict.
        export: Export::Verbatim,
    },
    // Succession deliberately UNRULED here, deferred to the anti-abuse
    // counterparty columns: `reporter` is
    // a Sybil-distinct-reporter column — the score counts one row per
    // reporter, so a reporter who succeeds counts twice — and both error directions are
    // conservative, so the class deserves one coherent grading pass. Unlike
    // the two reference columns in that row, this entry's eventual `Move`
    // executes from the registry loop.
    //
    // SUCCESSION RULING (2026-08-15) — **`Move(Plain)`**, the
    // counterparty class of `SUCCESSION_REFERENCES` and graded with it. The score reads `COUNT(*)` rows for a (content_hash, factor) pair,
    // one-per-reporter being enforced by the PRIMARY KEY rather than by a
    // DISTINCT clause — so a reporter who succeeds and re-flags the same content
    // adds a SECOND row and counts as two independent accusers against a third
    // party's content. Executes from the registry loop. ⚠ The deletion axis is untouched:
    // `Policy::Retain` stays exactly as it was, for its own string's reasons.
    ActorTable {
        table: "content_reports",
        column: "reporter",
        key: ActorKey::Blob,
        policy: Policy::Retain(
            "anti-abuse moderation record; the reporter's identity should not silently vanish from an active investigation trail -- needs a moderation-owned retention ruling.",
        ),
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (2026-08-16) — the k-anonymity choke point,
        // withheld because **its owner doc forbids a second read surface**, not
        // because the rows would disclose anything alarming.
        //
        // The registry keys this one on `reporter`, so an actor-keyed read
        // returns the exporter's OWN flags — no third party in any column (`content_hash` is a hash).
        // That makes this the entry worth reading carefully: the verdict is
        // withheld anyway. `report-sharing.md` § One gate function says every
        // read routes through `exposed_report_count` and that **"There is no
        // other read surface at all … New surfaces must consume the gate
        // function, never the table"**; this table's own migration repeats it
        // in capitals. A registry-driven export read would be precisely a new
        // direct read surface, and the fact that today's output looks harmless
        // is what would make it durable — the next column added to this table
        // inherits the surface without anyone re-deciding.
        //
        // ⚠ The transferable rule: **when another owner doc rules a plane's
        // disclosure, this axis defers to it rather than re-deriving a verdict
        // from the columns.** A per-table read of the schema would have said
        // `Verbatim` here in one line.
        export: Export::WithheldOperational(
            "the k-anonymity choke point for content reports. Not withheld for what the rows              would show the exporter -- keyed on `reporter`, they are the exporter's own              flags, with no third party in any column -- but because `report-sharing.md`              (§ One gate function) rules that every read of this table routes through              `exposed_report_count` and that no other read surface exists. An actor-keyed              export read would be exactly such a surface, and it would silently inherit              whatever columns this table grows next",
        ),
    },
    ActorTable {
        // Ruled 2026-09-21, and late: the table arrived with schema 55 and sat
        // in NO registry, because it names its people `invitee_id` /
        // `inviter_id` and the census had no root for either word (both added
        // to `ACTOR_SHAPED_ROOTS` with this entry). The declared-type walk
        // surfaced them as unexamined `BLOB`s; nothing else ever would have.
        //
        // The row's own actor is the INVITEE: the invitation is addressed to
        // them, they are half the primary key, and the accept door keys on the
        // caller being this column. The inviter is a counterparty reference,
        // ruled in `SUCCESSION_REFERENCES`.
        table: "room_invites",
        column: "invitee_id",
        key: ActorKey::Blob,
        // DELETION. `room_members.principal_id`'s verdict, for its reason: the
        // row is wholly the invitee's standing with one room. Pending or
        // accepted, nothing survives the account that it could serve — an
        // accepted row's only reader is the re-invite door's "already a
        // member" answer, about a principal who no longer exists. The
        // envelope `inbox_link_id` tracks is the invitee's own inbox row
        // (`content_links`, Purge) and its charge sits on the `users` row
        // deletion removes, so all three leave together and nothing dangles.
        policy: Policy::Purge,
        // SUCCESSION RULING. `Move`, and the reason is the envelope, not the
        // row: an invitation is three things written as ONE act
        // (`db/rooms.rs::record_room_invite_and_deliver`) — this row, the
        // un-acked inbox envelope that is the invitee's only way to learn the
        // room id, and the quota charge for it — and the ceremony ALREADY
        // carries the other two (undelivered `content_links` move;
        // `users.inbox_bytes_used` is summed onto the successor). So the
        // question is only whether the row keeps agreeing with them.
        //
        // - `Stay` strands it for good. There is no revoke or decline door:
        //   `room.remove` refuses a non-member, `unseat_room_member` deletes an
        //   invitation only beside a live seat, and a decline merely acks the
        //   envelope. The one thing that ever replaces a pending row is a
        //   re-invitation of the same (room, invitee) pair, and the invitee
        //   that pair names would no longer exist. Meanwhile the successor
        //   holds a charged knock the accept door answers "no invitation is
        //   pending" — "a delivered one with no row behind it", the state the
        //   writer's own doc says no ceremony produces.
        // - `Burn` leaves the same orphaned envelope and adds a silent loss:
        //   the inviter believes the invitation stands.
        //
        // Nothing here is authority a seed thief could have minted, which is
        // what `Burn` exists for — an invitation is the INVITER's act, judged
        // at the invite door against the inviter's role, and accepting it is a
        // fresh, deliberate act of the successor's own key.
        //
        // The signed bytes beside the row keep naming the predecessor and are
        // never rewritten (`opaque_carriers.rs`, `signed_invite`) — the
        // `rooms.owner_id` / `policy_blob` split exactly: the row moves, the
        // signed act stays as signed. No nest-side statement reads them back.
        //
        // Bounds, declared: invitations exist only for floor-authoritative
        // rooms this nest homes (both doors gate on it), so there is no mirror
        // half to split on as `room_members` must; and an invitee homed on
        // ANOTHER nest (`invitee_node_url` set) succeeds there — this ceremony
        // names only local identities, `room_members`' third bullet.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. The invitee's own record — which rooms asked for
        // them, in what role, who asked, and whether they said yes — by
        // `room_members`' reasoning one step earlier in the same story.
        // Nothing here is a secret or another member's: the signed act was
        // addressed and already delivered to the exporter (it is the payload
        // of an envelope `inbox.json` carries), and the inviter's id inside it
        // is the one the exporter's own app rendered the invitation under.
        // Actor-scoped on `invitee_id`, so an invitation this actor ISSUED is
        // never in their export — that row is its invitee's.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "room_members",
        column: "principal_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        // SUCCESSION RULING — re-ruled 2026-09-13. The 2026-09-09 ruling was `Stay` for every
        // row, on the premise that "the successor's real entry arrives the only
        // way it can: the succession's add-successor commit inside the group,
        // reported here by the committing device". True of an end-to-end room,
        // whose floor is a MIRROR of its MLS group. False of a ceremony-born
        // room, whose floor IS the membership authority: the report door
        // refuses such a room by construction, so nothing ever seated the
        // successor, and the retired identity kept its seat — for an owner the
        // unremovable one every owner-only door keys on. The re-rule trigger the
        // `rooms.owner_id` ruling below declared ("if the community class ever
        // makes this column the *authority* … re-rule the pair with the
        // roster") had tripped the day it was written.
        //
        // So the table splits on the room's provenance
        // (`RoomRecord::is_floor_authoritative`), which is why it is `Partial`:
        //
        // - FLOOR seats pass to the successor, from one hand-written leg
        //   (`db/successions.rs::rule_the_floor_roster_seats`).
        //   The successor is seated with the predecessor's role at a FRESH
        //   roster entry and with NO wrap target — the predecessor's reception
        //   key derives from the seed the ceremony retires — and the
        //   predecessor's row is `Removed`-absorbed as history: exactly "the
        //   recipient-set roster's fresh entry with the predecessor `Removed`"
        //   (`conversation-rooms.md` § The home nest → *Transfer by succession*).
        // - MIRROR seats stay, for the 2026-09-09 reason, which still holds for
        //   them: a move would seat the successor in an MLS group that knows
        //   nothing of them.
        // - Seats homed on ANOTHER nest are that nest's floor to hand over. The
        //   ceremony names only a local identity, and a room homed here seats
        //   its local members at `home_node_url = ''`; a succession this nest
        //   only learns from a peer seats nobody. Declared bound.
        succession: Succession::Partial(
            "a FLOOR seat passes to the successor and a MIRROR seat stays. A \
             ceremony-born room's floor is its membership authority and no member report \
             ever arrives for it, so the ceremony seats the successor with the \
             predecessor's role at a fresh roster entry with no wrap target and \
             Removed-absorbs the predecessor's row as history; an end-to-end room's rows \
             mirror its MLS group, whose add-successor commit is reported here, so they \
             stay. Seats homed on another nest are that nest's to hand over. Row-level \
             rule witnessed by \
             successions::tests::a_succession_hands_each_floor_seat_to_the_successor_and_leaves_mirror_seats",
        ),
        // EXPORT RULING. The row is this principal's own membership record —
        // which rooms they are in, with what role, since when — and it is
        // theirs by the export question (*whose data is it*) even though the
        // succession axis leaves it behind (*what will it DO next*): the same
        // two-axis split the retired `groups.owner_id` recorded. Nothing here is another
        // member's secret: the roster a member holds is the roster their own
        // app renders, and the room's content is not in this table.
        export: Export::Verbatim,
    },
    // The bridged-conversation family (`apps/bridges.md` § Bridge-kind
    // catalogue → Phase G): the account's rooms on a conversation bridge, the
    // sealed rows in both directions, and the items queued for the bridge.
    // Registered 2026-10-03 with the Nostr DM leg's move onto the family
    // (schema 118), which put the account's whole Nostr DM history here: the
    // three verdicts are `nostr_dms`' own, carried — Purge, Move, Verbatim.
    //
    // SUCCESSION. Every row moves: the successor keeps their conversations and
    // their history with them, as they kept `nostr_dms`. A room's `room_id`
    // was derived from the PREDECESSOR's id at birth and is kept — every read
    // and write finds a room by `(actor_id, bridge_id, far_room_id)`, never by
    // re-deriving the id (`bridged_conversations::CacheDb::upsert_bridged_room`),
    // so a moved room is the one the next deposit lands in. The user's floor
    // seat in `room_members` is that table's own ruling.
    ActorTable {
        table: "bridge_conversation_rooms",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. The account's own rooms: the far room id and
        // participants are the correspondents, the `contacts.peer_id` class;
        // `capabilities` and `bridge_x25519` are the bridge's declared shape
        // and public key, snapshotted.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_conversation_messages",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. The message BODIES — `sealed_content` is sealed to
        // the account's own recipient key in both directions (the bridge
        // seals inbound, the user's app the Sent copy), so it rides in
        // at-rest form, the sealed-columns clause exactly, as `nostr_dms`'
        // did. Withholding it would carry the fact of a DM plane and none of
        // the messages.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "bridge_conversation_outbox",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Purge,
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. The account's own undelivered messages, sealed to the
        // bridge that will carry them — the user's sends, still in flight.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "rooms",
        column: "owner_id",
        key: ActorKey::Blob,
        policy: Policy::Retain(
            "the actor may own a room with other members, and delete-vs-transfer is decided (conversation-rooms.md § Roles and authorization, the owner rule): a user's OWN deletion is refused while a live user member homed here could take the room over, so the owner transfers first; with nobody who could, or when an admin deletes the account, the room survives owner-less with its members and everything they hold — so the record is kept, never purged.",
        ),
        // SUCCESSION RULING (2026-09-09). ⚠ The verdict is the OPPOSITE of its
        // retired ancestor `groups.owner_id`'s, and deliberately so — the flip
        // is not drift.
        //
        // `groups.owner_id` was `Stay` because it was a DARK column: written by
        // `create_group`, read by nothing, its live twin being the
        // `role = 'owner'` row in `group_members` (both tables retired with the
        // group plane, schema 86). A dark column takes its live twin's verdict,
        // and moving it alone would split one fact in two.
        //
        // `rooms.owner_id` is not dark and has no twin to disagree with: it is
        // derived from the roster's own owner entry on every report and read as
        // the room's ownership. And the goal doc rules it directly — "a
        // room-ownership row **moves** with the owner (the ownership re-point
        // already carries every other owned plane)"
        // (`conversation-rooms.md` § The home nest → *The succession axis*).
        // That is the ordinary ownership class: the successor owns what the
        // predecessor owned.
        //
        // The residual this ruling declared is RETIRED (2026-09-13). It said moving this column does not by itself make the
        // successor the owner where it counts, and named its own re-rule
        // trigger: "if the community class ever makes this column the
        // *authority* … re-rule the pair with the roster". The community class
        // enforces roles at the floor, so the pair is re-ruled. In an end-to-end
        // room the owner is still the owner-signed policy the succession chain
        // carries in the MLS group, and this move keeps the nest's record
        // agreeing with it. In a ceremony-born room the ceremony's floor leg
        // seats the successor in the owner seat beside this move
        // (`room_members`, above), so the record and the seat every owner-only
        // door reads name the same identity. The signed policy still names the
        // predecessor — the nest cannot author one — and the floor resolves its
        // names through the succession chain (`conversations_handlers.rs`,
        // `floor_designee`).
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. The actor-scoped read returns only rooms this actor
        // owns — never a room they merely joined, whose row belongs to its own
        // owner (the retired `groups.owner_id`'s scoping, carried over). No column here is
        // another member's: the policy blob is the owner's own signed act.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "outbound_mail_queue",
        column: "forward_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Retain(
            "forward_actor_id is a routing DESTINATION, not the row's owner; the queued message may still need delivery/bounce handling independent of that actor's account status.",
        ),
        succession: Succession::Partial(
            "the queue where a thief's in-flight forward copies actually sit — \
             `forward_queue` is only the rate-cap OVERFLOW, and a forward under the cap \
             is enqueued straight here, where the dispatcher selects on \
             `status = 'pending'` alone with no idea the forwarding identity was \
             retired. An UNDISPATCHED row whose persisted `forward_copy_mode` is `copy` \
             BURNS, whatever rule armed it (its destination is thief-chosen and the \
             original rests sealed in the mailbox); everything else re-attributes to \
             the successor, so the SRS \
             rewrite at queue-out and the NDR route name an identity that still exists, \
             and the account's recent forward-rate window travels with it. A `sent` or \
             `bounced` row is history of a forward the retired identity really made and \
             is never deleted, and a redirect (or a row with no persisted mode) may be \
             the only copy of accepted mail, so it is never burned. \
             The row rule is owned by `successions::tests::\
             an_undispatched_forward_copy_burns_at_the_ceremony_and_the_rest_reattribute`",
        ),
        // EXPORT RULING (2026-08-16) — `forward_queue`'s durable
        // sibling and the same verdict, with one extra reason that is specific
        // to this table and worth stating because it is the
        // `foreign_recovery_heads` shape in a milder form: **the registry
        // column is not this table's owner.** `forward_actor_id` is NULL on
        // every ordinary submission and set only on a forwarded row, so the
        // export read matches a narrow SLICE of a nest-wide spool — the
        // forwarded subset — and a `Verbatim` here would hand the owner a
        // partial spool labelled as their outbound mail. The rest of the queue
        // is other actors' rows, correctly unreachable.
        //
        // The plaintext argument on `forward_queue` applies verbatim
        // (`raw_message` again), and the durable copy is again elsewhere:
        // `mail-forwarding.md` § Two forwarding shapes commits the local
        // mailbox write before the forward is dispatched.
        export: Export::WithheldOperational(
            "the outbound delivery spool -- per-(message, recipient) rows carrying retry              state, the last SMTP error and a PLAINTEXT `raw_message`, i.e. delivery              bookkeeping in the variant's own words. Two further reasons: the durable copy              of the same mail rests sealed in the owner's mailbox (committed before              dispatch), so an export would mint an unsealed second copy; and              `forward_actor_id` names the FORWARDING actor rather than the row's owner, so              the actor-scoped read reaches only the forwarded slice of a nest-wide queue",
        ),
    },
    // ─── The entitlement plane's three non-FK tables — RULED 2026-08-14 ───
    //
    // The eight-table `subscription_tiers` family below is bound by a foreign
    // key and moved as one unit (2026-08-13); these three carry no FK, so they
    // rode nothing and stayed `Unruled` a plane behind the tiers they serve.
    // Each is ruled on its own consumer, and two of the three answer the same
    // question the reach plane does: *is this authority honoured by something
    // that never looks at an actor to refuse?*
    ActorTable {
        table: "payment_claim_codes",
        column: "author_id",
        key: ActorKey::Blob,
        policy: Policy::Retain("financial record -- see subscription_tiers."),
        // The ledger MOVES whole and is VOIDED in part, which is two rulings in
        // one statement and needs both halves said.
        //
        // **Why it moves.** A claim names the tier it buys, and the tier plane
        // moved. Left behind, `redeem_claim`'s tier pre-check fails for every
        // code on it (`get_subscription_tier(old, tier)` is now empty), so the
        // buyer who paid gets nothing — and the successor cannot even see the
        // receipt to make it right, because `claims.list` is keyed on
        // `author_id`. The audit trail has to travel with the tiers it names.
        //
        // **Why the unredeemed half is voided as it travels.** A claim code is
        // a BEARER credential: `redeem_claim` finds it by code alone and binds
        // it to whoever presents it, consulting the retired identity nowhere.
        // A seed thief could mint an unbounded supply at
        // `fauna.payments.claims.mint` (a plain User-class gesture) and hold
        // the codes through the ceremony, and NO app can revoke one — there is
        // no `claims.void` gesture; `void_payment_claim` is reachable only from
        // the refund path. Carried live, that is `nest_pairings`' shape exactly:
        // standing authority the thief could have minted, invisible to the
        // refusal plane and unrevocable from every app. Voiding costs the
        // honest buyer nothing a `Stay` would not cost them anyway (their code
        // was dead either way) and leaves the row — `external_ref` intact — so
        // the successor can verify the payment in the provider dashboard and
        // mint a fresh code. That is the bounded, visible re-grant a burn's
        // reasoning asks for, without deleting an audit row the schema says is
        // never deleted.
        //
        // A REDEEMED row is not touched beyond the move: it is a completed sale,
        // and `payment_core::apply_refund` still resolves through it (its
        // `redeemed_by` half is the reader's own ruling, one entry down in
        // `SUCCESSION_REFERENCES`).
        succession: Succession::Move(MoveShape::Bespoke(
            "the ledger moves whole -- a receipt must travel with the tiers it names, \
             or the buyer's code is dead and the successor cannot see it to make good -- \
             but every UNREDEEMED, un-voided claim is stamped `voided_at` as it moves: \
             a claim code is a bearer credential `redeem_claim` binds to whoever \
             presents it, a seed thief could have minted a supply at `claims.mint`, \
             and no app can revoke one (there is no `claims.void` gesture), so the \
             ceremony is the only place it can die. The row survives with its \
             `external_ref`, which is what lets the successor honour a real buyer. \
             Redeemed rows move untouched -- completed sales, and the refund path \
             resolves through them. The row rule is owned by `successions::tests::\
             a_succession_carries_the_claim_ledger_and_voids_what_was_never_redeemed`",
        )),
        // The succession ruling above already did this verdict's hard thinking
        // and said it in as many words: **a claim code is a BEARER credential**
        // — `redeem_claim` finds it by code alone and binds it to whoever
        // presents it — and no app can revoke one, because there is no
        // `claims.void` gesture. That is why the ceremony voids the unredeemed
        // half rather than carrying it.
        //
        // An export is the same exposure through a different door, and a
        // weaker one: the archive is retrievable with an eviction export token
        // and outlives every session that produced it, while the ceremony at
        // least ends in a row the successor can see. So `code` does not ride,
        // and unlike the ceremony there is no voiding step to soften it.
        //
        // Everything else is the author's sales ledger and rides — including
        // `external_ref`, which is the audit link to the provider dashboard
        // the successor uses to honour a real buyer (the ruling above turns on
        // that column surviving), and `redeemed_by`, which the author's own
        // `claims.list` already returns (`payment_handlers.rs`), so the export
        // is no wider than the app.
        export: Export::Redacted {
            omit: &["code"],
            reason: "`code` is a bearer credential -- `redeem_claim` binds it to whoever \
                     presents it and no app can revoke one -- so it must not rest in an \
                     archive retrievable with an eviction token, which is the same exposure \
                     the succession ruling above voids unredeemed codes to prevent, minus the \
                     voiding. The sales ledger rides: `external_ref` is the dashboard audit \
                     link, and `redeemed_by` is already in the owner's own `claims.list`",
        },
    },
    // ⚠ Ruled `Burn` against a `Policy::Retain`, and the pairing is deliberate
    // rather than an oversight: the two axes answer different questions. The
    // deletion axis retains this row as part of the monetization plane's
    // financial record; this row carries no money and no sale — it is a
    // verification SECRET plus a tier mapping, and the financial record proper
    // is `payment_claim_codes` (which moves) and the subscriber rows (which
    // moved with the family).
    ActorTable {
        table: "payment_providers",
        column: "author_id",
        key: ActorKey::Blob,
        policy: Policy::Retain("financial record -- see subscription_tiers."),
        // The `backup_destinations` shape, on the payments plane: thief-writable
        // standing authority whose consumer never consults the refusal.
        //
        // `webhook_secret` is what `payment_routes::payment_webhook` verifies
        // inbound provider events against, and that ingress is PUBLIC HTTP that
        // resolves the config straight out of the URL's actor id — it
        // authenticates nobody, so retiring the identity key reaches it not at
        // all. A thief holding the seed sets the row (and therefore the secret)
        // over `fauna.payments.providers.set`, a plain User-class gesture.
        //
        // **A `Move` is the actively wrong answer, and not only for the secret.**
        // The actor id is INSIDE the webhook URL the provider posts to
        // (`/api/v1/payments/webhook/{author_id}/{provider}`), so a moved config
        // is unreachable at the URL the provider actually calls: the move buys
        // exactly zero continuity while carrying a thief-known secret forward to
        // verify forged events against the successor's tiers. The creator must
        // visit the provider dashboard to re-point the URL either way — and the
        // secret is re-obtainable there, so the burn destroys nothing
        // irrecoverable.
        //
        // **A `Stay` leaves it invisible and unrevocable**, the `nest_pairings`
        // breach of `nest/common.md` § Client-state recoverability:
        // `providers.list` / `providers.remove` are both keyed on the caller's
        // own `author_id`, so the successor can neither see the row nor delete
        // it from any app. (`providers.list` never echoes the secret in any
        // case, so no adjudication mark could raise a planted one.)
        //
        // ⚠ The reason string below names the write kind BARE (`providers.set`),
        // never `fauna.payments.providers.set` — and the same for its siblings.
        // ACTOR_TABLES is ungated (the schema carries these tables in every
        // flavor), so a reason string is a RUNTIME `&'static str` that lands in
        // the binary, where `just nest-store-safe-check`'s `strings` witness
        // reads it: a full kind spelling here fails excision item 2 exactly as a
        // sender would ("absent means absent, prose included" —
        // `dynamic-features.md` § What "completely compiled away" means). It did,
        // on 2026-08-14. Comments like the ones above are compiled away and may
        // spell kinds in full; the quoted strings may not.
        // `no_reason_prose_spells_an_excised_kind` (named
        // `no_succession_reason_spells_an_excised_kind` until 2026-08-15, when
        // it grew to walk the export axis too) now pins the rule for every
        // ruling on every axis, so the next author meets it locally rather than
        // on the asynchronous gate. The export reason below is bound by it
        // identically.
        succession: Succession::Burn(
            "the `webhook_secret` an unauthenticated PUBLIC HTTP ingress verifies provider \
             events against, and a seed thief sets it over `providers.set`. \
             A Move buys no continuity -- the actor id is inside the webhook URL the \
             provider posts to, so a moved config is unreachable at the URL actually \
             called, while the thief-known secret would verify forged events against the \
             successor's tiers; a Stay leaves the row invisible and unrevocable \
             (`providers.list`/`.remove` are owner-keyed), the `nest_pairings` breach. \
             The creator re-registers at the provider dashboard either way, which is \
             where the secret comes from -- so nothing irrecoverable dies here",
        ),
        // `webhook_secret` is a SHARED VERIFICATION secret, and the succession
        // ruling above already establishes what holding it buys: it is what
        // `payment_routes::payment_webhook` verifies inbound provider events
        // against, on a PUBLIC HTTP ingress that authenticates nobody. An
        // eviction-token holder with the secret can forge provider events
        // against this author's tiers — granting entitlements, or refunding
        // them away — from off the box entirely. It never rides.
        //
        // `providers.list` already refuses to echo it to the owner's own app
        // (noted in the ruling above), which settles the shape too: the
        // export must not be a wider surface than the app. The remaining
        // columns — which provider kind is wired to which tier, and when —
        // are exactly what `providers.list` does return, so the row rides
        // minus the one column no surface ever shows.
        export: Export::Redacted {
            omit: &["webhook_secret"],
            reason: "`webhook_secret` verifies inbound provider events on a PUBLIC ingress that \
                     authenticates nobody, so holding it is standing power to forge \
                     entitlement events against this author's tiers -- and the owner's own \
                     `providers.list` already declines to echo it, so an export carrying it \
                     would be a wider surface than any app. The provider-kind/tier/mint-time \
                     config it wraps is the author's own and rides",
        },
    },
    // ─── The `subscription_tiers(author_id, name)` family — RULED 2026-08-13 ───
    //
    // Four tables, one verdict, because a foreign key gives them no choice:
    // three children reference `subscription_tiers(author_id, name)`, so they
    // move together under `PRAGMA defer_foreign_keys` or not at all. Declared in
    // `COUPLED_MOVE_FAMILIES` as `subscription_tiers`; the executor and
    // `no_foreign_key_crosses_a_succession_leg` both read that declaration.
    //
    // **Why the whole plane MOVES.** It is the author's ownership of their own
    // monetization surface, and § Re-key scope's class rule ("rows that confer
    // future authority or access re-point") reaches it twice over. Left behind,
    // the successor has no tiers, no roster and no keys, while every live
    // entitlement still resolves `subscribers(author_id, tier_name)` ->
    // `subscription_tiers` -> `current_key_blobs` under the RETIRED id — so the only
    // party who could serve, rotate or revoke any of it is the identity the
    // ceremony just retired, and the successor cannot even delete it. That is
    // not the status quo a wrong `Stay` usually buys; it is a plane frozen under
    // a dead owner, with the paying readers' access frozen with it.
    //
    // **Why the key-material reflex does NOT rule these `Stay`.**
    // `current_key_blobs` is key material, and the plane a thief read is
    // normally left behind (`actor_mls_pubkeys`). It is the wrong analogy here:
    // those keys seal the author's EXISTING back catalogue, which moves with the
    // account, so burning or stranding them destroys the successor's access to
    // their own published content — user-irrecoverable, which the no-data-loss
    // invariant forbids outright. The thief's retained copy is answered the way
    // § Re-key scope answers every read-key exposure: "key material re-mints
    // through each plane's existing rotation machinery". Here that machinery is
    // the author's client, the only custodian of a tier's period key: the
    // successor's client re-keys every inherited tier as an aftermath leg
    // (`SubscriptionsAuthor::rotate_period_keys_after_succession`, uploading
    // through `fauna.subscriptions.key_blob.rotate`). So the ROWS move and the
    // ceremony's aftermath ROTATES, which is the only combination that keeps
    // the back catalogue readable while making post-ceremony broadcasts dark to
    // the thief. (The nest mints no period key; it holds only the wrapped
    // distributions the author's client uploads.)
    ActorTable {
        table: "subscribers",
        column: "author_id",
        key: ActorKey::Blob,
        policy: Policy::Retain("financial/entitlement record -- see subscription_tiers."),
        // The author's own roster of who has paid them. ⚠ This entry rules the
        // `author_id` half ONLY. The row names a SECOND person —
        // `subscriber_id`, the paying reader — whose verdict is separate and
        // lives in `SUCCESSION_REFERENCES`.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the subscription plane). The
        // plane's genuine two-party table, and the
        // question to ask: *does the registry key this on the subject or on
        // the author?* Here it keys on `author_id`, the creator, and the row
        // names `subscriber_id` — so an actor-scoped read hands the creator a
        // list of the people paying them. That is a real disclosure question
        // and the product has already answered it: `monetization.md` § Pillar 1
        // → the profile Tiers tab renders "§3 Subscribers … the selected
        // tier's roster, with per-row remove". The creator manages this roster
        // by name in their own app, so the archive discloses nothing the live
        // UI does not, and withholding it would take the commercial
        // relationship the creator OWNS (this doc's own framing: "the payee,
        // not Fauna, owns the commercial relationship") out of their records.
        //
        // `mlkem_encaps_key` is the subscriber's ML-KEM **encapsulation** key —
        // the public half, published so the creator's client can wrap the
        // period key to them; the decapsulation half is the subscriber's own
        // and has never been here. `admitted_tier`/`admitted_lapse_tier` are
        // the frozen admission stamp (`monetization.md` § Pillar 4), which is
        // the terms this membership was bought under.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "subscription_tiers",
        column: "author_id",
        key: ActorKey::Blob,
        policy: Policy::Retain(
            "financial/entitlement record -- retention policy needs its own ruling, not a blanket purge default.",
        ),
        // The family parent: the author's tier definitions. See the block above.
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the subscription plane). The
        // author's own tier definitions, and the plane's easiest call because
        // the nest already serves them to STRANGERS: `GET /api/v1/subscriptions
        // /tiers/{author_id}` is one of the three deliberately-unauthenticated
        // HTTP reads (`monetization.md` § Pillar 1 → Wire). Name, rank,
        // description, price hint and payment URL are the shopfront. Nothing
        // here can be more sensitive in the owner's own archive than it already
        // is on the open internet (a `hidden` tier is withheld from that read
        // but is the owner's own row — the export target is the owner, so the
        // verdict stands).
        export: Export::Verbatim,
    },
    ActorTable {
        table: "actor_successions",
        column: "old_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Retain(
            "identity-succession audit trail -- must outlive both the old and new actor rows to record what happened.",
        ),
        // The link row IS the record that this identity was retired, keyed on
        // `old_actor_id`. Moving it would erase the very fact it exists to
        // state, and would make a chain unwalkable.
        succession: Succession::Stay(
            "the ceremony's own audit trail: the row records that THIS identity was \
             succeeded, so `old_actor_id` naming the retired actor is the content, \
             not stale data. `succession_path` walks these to find a chain's terminal",
        ),
        // EXPORT RULING (the identity/lifecycle plane).
        // Nothing here is secret and the migration says so at the table: a
        // succession statement is PUBLIC by construction — peers, MLS members
        // and federation all consume it — so `statement`, `new_actor_id` and
        // `seq` disclose nothing the network was not already told.
        //
        // ⚠ Two facts about WHO this slice reaches, because the answer is not
        // the obvious one and a later reader will re-derive it wrongly.
        //
        // (1) The slice keys on `old_actor_id`, the RETIRED identity — so it
        // is the retired side that reads this row back, not the successor.
        // That is reachable, narrowly: `auth_core::refuse_if_superseded`
        // guards token MINTING, while `export_routes::handle_export` accepts
        // an already-minted bearer or an eviction export token and re-checks
        // neither. So this verdict is a real disclosure decision under the
        // weakest credential, not a vacuous one — and it comes out permissive
        // because the content is published material.
        //
        // (2) The SUCCESSOR's archive carries no succession history at all,
        // and that is structural rather than an oversight: their side is
        // `actor_successions.new_actor_id`, which lives in
        // `SUCCESSION_REFERENCES` — a list `gather_export_set` never walks.
        // It is the same actions-I-performed / actions-about-me split
        // mirrored onto identity. ⚠ Do NOT "fix" it by adding a second
        // `ACTOR_TABLES` entry keyed on `new_actor_id`: the walk iterates
        // entries, so one table with two entries emits
        // `export/tables/actor_successions.ndjson` TWICE into the same zip.
        // If the successor's view is wanted, it belongs in a shaped domain.
        export: Export::Verbatim,
    },
    // A succession's owed nests: the destinations of the `nest_pairings` rows
    // the ceremony burned, kept so the successor's devices can carry the
    // statement to each (`identity-succession.md` § Enforcement on the home
    // nest → *Every nest the identity is linked to*). Keyed on the RETIRED
    // identity, as `actor_successions` is — and unlike that table the rows are
    // a to-do list, not an audit trail, which is what separates the two
    // deletion verdicts.
    ActorTable {
        table: "succession_owed_nests",
        column: "old_actor_id",
        key: ActorKey::Blob,
        // An account deletion walks the deleted actor's local predecessors
        // (`purge_orphaned_actor_rows`), so deleting the successor reaches the
        // rows kept under every identity on its path. Nobody is left to
        // deliver, and the list names the person's other nests: it goes with
        // the `nest_pairings` rows it was copied from, which are `Purge` too.
        policy: Policy::Purge,
        succession: Succession::Stay(
            "the entry records what THIS identity's succession burned, so the retired \
             id is the content and half of the settle key. A later succession leaves \
             it where it is and adds its own under the next retired id; the status \
             read walks the caller's predecessor path, which is how an entry a first \
             successor never settled is still served to the second",
        ),
        // ⚠ The export slice keys on `old_actor_id`, so it is the RETIRED
        // identity that would read these rows back — reachable with a bearer or
        // an eviction export token minted before the ceremony (see
        // `actor_successions` above). The goal doc serves the list to the
        // successor alone, and the retired key is the one a thief holds.
        export: Export::WithheldOperational(
            "delivery bookkeeping for the successor's devices: which nests have not \
             been told of the succession yet. The successor reads it on \
             `fauna.recovery.succession.status`; this slice would serve it to the \
             retired identity instead, the one caller the list is withheld from. \
             Nothing here is data the user lacks -- an entry is a copy of a pairing \
             destination they chose, and it is deleted when settled",
        ),
    },
    // ⚠ `old_actor_id` names the actor who RELEASED the handle. The collision
    // with this axis's own vocabulary is a coincidence of naming and not a
    // succession pointer — read it as `released_by`.
    //
    // The row is a 72-hour EXCLUSIVE RECLAIM RIGHT, not a bare denial:
    // `check_handle_cooldown` answers `old_actor_id == caller`, so inside the
    // window the named actor is the only one who may take the handle back and
    // everyone else is refused. That is future access, which re-points under
    // the classification rule — and the theft case settles the direction rather
    // than merely agreeing with it. A thief who releases the victim's handle
    // arms a 72 h clock; left behind, the successor is the one actor who cannot
    // reclaim their own handle, and when the window lapses the row is deleted
    // and the handle is open to anyone. Moving it hands the thief nothing (the
    // retired identity is refused everywhere) and restores exactly the position
    // the same human held before the ceremony.
    //
    // ⚠ CONDITIONAL ON THE TABLE BEING DARK TODAY, the `admin_actor_ids.added_by`
    // precedent: the readers are live (`discovery_core`, `account_core`,
    // `account_handlers`) but `release_handle_with_cooldown` has NO production
    // caller — only `tests/registration_db.rs` — so nothing writes a row today,
    // and the ceremony deliberately writes none of its own (`successions.rs`:
    // the handle never becomes claimable by anyone else). Re-check when a
    // release path ships; the ruling is the one that will be right then.
    ActorTable {
        table: "handle_cooldowns",
        column: "old_actor_id",
        key: ActorKey::Blob,
        policy: Policy::Retain(
            "handle-squatting cooldown record keyed by the OLD actor id -- must survive the old actor to enforce the cooldown window.",
        ),
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the identity/lifecycle plane).
        // Read the succession comment above first: `old_actor_id` here is
        // `released_by`, NOT a succession pointer, so the actor column names
        // the exporter and the subscription-plane check comes out clean. Three
        // columns, all theirs — a handle they released, the moment it was
        // released, and the fact that until `released_at + 72h` they are the
        // only actor who may take it back. The row is a RIGHT the exporter
        // holds (`check_handle_cooldown` answers `old_actor_id == caller`),
        // and a right nobody can read is one they cannot exercise before it
        // lapses.
        //
        // ⚠ Written for a REVIVAL:
        // `release_handle_with_cooldown` has no production caller
        // today (only `tests/registration_db.rs`), so the emission is empty
        // and this is the verdict for when a release path ships. Nothing about
        // it depends on the darkness — a released handle is the exporter's own
        // either way.
        export: Export::Verbatim,
    },
    // Appended by the completeness walk — found
    // by the walk itself, not missed by hand review; append, don't reorder.
    ActorTable {
        table: "audit_log",
        column: "actor_id",
        key: ActorKey::Blob,
        policy: Policy::Retain(
            "hash-chained transparency log (db/admin.rs::audit_on_conn -- each entry's \
             entry_hash covers the previous entry's, so deleting any row breaks every \
             later row's chain); actor_id records WHO PERFORMED the entry's action -- \
             every call site passes the acting party (an admin/guardian; for \
             `moderation:appeal` the appellant User, db/moderation.rs::record_appeal) and \
             puts the subject in `target` (admin_ws_handlers.rs:551, family_handlers.rs:744) \
             -- not the actor the row belongs to for deletion purposes. ⚠ CORRECTED 2026-08-16: this reason \
             and the succession reason below both used to say \
             actor_id was who an entry is ABOUT, i.e. the party acted upon. It is the \
             opposite, and the error mattered enough to fix rather than leave -- an export \
             verdict read off the old wording would have been ruled on the wrong party.",
        ),
        succession: Succession::Stay(
            "attribution under a CRYPTOGRAPHIC COMMITMENT, which is stronger than the \
             usual history argument and is what decides it: `audit_on_conn` computes \
             `entry_hash = SHA256(id || ts || actor_id || action || target || detail || \
             prev_hash)`, so this column is INSIDE the hash, and each row's `prev_hash` \
             is the previous row's `entry_hash` -- re-pointing one row invalidates its \
             own hash and every later row's chain alike. Nor is the chain private \
             bookkeeping: both hashes are served to the admin transparency surface \
             (`admin_ws_handlers`), so a moved row is a break an admin's own \
             verification finds. A move would also be a FORGERY in content, restating \
             the successor as the AUTHOR of admin actions the retired identity took \
             (⚠ direction corrected 2026-08-16 -- this clause used \
             to say `subject ... taken against`; every call site passes the acting \
             admin/guardian in this column and the subject in `target`. The Stay \
             conclusion is unaffected: a move forges authorship instead of \
             subjecthood, and the hash-chain argument above never depended on which \
             party the column names), and nothing authorizes through the column -- it \
             answers who ACTED, never who may do anything next",
        ),
        // EXPORT RULING (2026-08-16) -- ruled with
        // `pending_actions` below; the shared reasoning lives here, the chain
        // columns' half on this reason string, and the e2e pin is
        // `export_api.rs::the_actors_own_conduct_rides_while_the_hash_chain_stays_home`
        // (hand-written because the guard-set asymmetry means no belt reds a
        // wrong Withheld demotion).
        //
        // **What decides it is an AUTHORSHIP invariant every writer holds**:
        // every production `audit_on_conn`/`.audit(` site keys the row to the
        // party who PERFORMED the act and fills `action`/`target`/`detail`
        // from that party's own request (admin_ws_handlers, family_handlers,
        // node_policy_handlers, pair_handlers, claim_core, feature_gate,
        // region_tier, host_maintenance, storage/nat_mode_core, the
        // appellant's own `moderation:appeal` (db/moderation.rs::record_appeal
        // -- their reason, the content id they named), and the
        // pending-action lifecycle writers -- system writes pass `None` and
        // are keyed to nobody, so the slice never sees them). So the
        // `WHERE actor_id = me` slice is by construction records of MY OWN
        // conduct with content I supplied: exporting it discloses nothing to
        // me that acting did not already require me to know, under ANY
        // credential -- which is what makes it safe under the eviction token,
        // the axis's weakest-credential rule. **The one carve-out that argument
        // needs:** a BEARER CREDENTIAL the actor handled is not made safe by
        // their having known it -- knowing an invite code at mint is not
        // holding a live one after removal -- so no writer stores one
        // (`audit_on_conn`'s rule; `invite.create`/`invite.delete` store
        // `admin::invite_code_audit_fingerprint`, pinned by
        // `export_api.rs::an_admins_minted_invite_codes_never_ride_their_audit_rows`). Withholding would instead deny
        // an evicted or succeeded actor the record of their own acts -- for an
        // ordinary user their pairing, self-service scheduling and succession
        // history; for an admin their governance record -- which is the
        // "what does my nest know about me" question `principles.md` says the
        // export answers, while the admin ROLE surface serves the whole log
        // (everyone's rows, both hashes) to any current admin: the export
        // serving strictly less (own rows only) to the weaker credential is
        // the right asymmetry.
        //
        // ⚠ Scope caveat a reader WILL trip on: this is the
        // actions-I-PERFORMED view, never the actions-about-me view -- rows
        // where I am the `target` are keyed to the acting admin and do NOT
        // ride (the column is the KEY, per the correction above). An about-me
        // affordance would be a fresh shaped-domain question over `target`
        // (TEXT, not always an actor), not an extension of this verdict.
        export: Export::Redacted {
            omit: &["prev_hash", "entry_hash"],
            // The chain columns are the one part of the row that is about the
            // REST OF THE LOG rather than about the actor: computed by the
            // nest over all entries, they verify nothing inside a slice (the
            // unbroken whole lives on the admin transparency surface), and
            // their only marginal information is a confirmation oracle on
            // third parties' ADJACENT entries -- `prev_hash` IS the
            // neighbour's `entry_hash`, so a holder who guesses a neighbour's
            // full content (id and ts are bracketed by their own rows) can
            // confirm the guess offline. Thin value vs thin leak; the axis's
            // bias resolves that toward not leaking.
            reason: "the actor's own conduct record rides; the hash-chain columns are \
                     about the whole log, verify nothing in a slice, and confirm \
                     neighbours' entries -- see the comment above. Riding is safe only \
                     because no writer stores a bearer credential in `target`/`detail` \
                     (`audit_on_conn`'s rule): knowing a credential once is not holding it \
                     after removal, so `invite.create`/`invite.delete` store a keyed \
                     fingerprint, coupled to `invite_codes`' omission of `code`",
        },
    },
    // === The legal-takedown tombstone (schema 65, ruled 2026-09-11) ===
    //
    // A post its author deleted WHILE under a legal takedown: the post id, the
    // citation and the blob digests the record named, captured by
    // `delete_post_core` in the one moment all three are still readable
    // (`moderation.md` § Legal takedown → *Posts*, the 2026-09-11 delete-door
    // ruling). Landed the same day without a ruling here. The row is keyed by the
    // post, and `author_id` is whose post it was — the row's own actor, so
    // this is an `ACTOR_TABLES` entry and not a reference.
    //
    // **Two readers, and only one of them is actor-keyed — that asymmetry
    // decides the succession verdict below.** The export's segment-pair
    // withhold set is seeded from `taken_down_deleted_post_ids_for_author(
    // exporting actor)` (`export_routes.rs`), so the row is found by whoever
    // is EXPORTING; the blob door's rebuild reads every row's digests with no
    // actor filter at all (`taken_down_deleted_post_blob_digests`), so that
    // door is blind to this column under every verdict.
    ActorTable {
        table: "legal_takedown_deleted_posts",
        column: "author_id",
        key: ActorKey::Blob,
        // DELETION RULING. The owner doc calls the row a FLOOR — "after the
        // flag row is deleted the box holds nothing left to recompute from" —
        // and says the same delete core runs under account deletion. That is
        // exactly the sequence `execute_account_delete` performs:
        // `retract_actor_posts` runs the delete door over every post FIRST,
        // which writes this row for each taken-down one, and
        // `purge_orphaned_actor_rows` runs AFTER. A `Purge` would therefore
        // destroy the floor in the same tick the retraction laid it, while
        // the compelled attachments stay on the box until the GC sweep — and
        // the blob door's rebuild reads only this table for them, so they
        // would serve again to any digest holder for exactly that window. The
        // `obligation_action_records` class besides: a compelled act's record
        // does not vanish because its subject left. Tiny by construction, no
        // DELETE anywhere in the tree, inert once the GC has reclaimed the
        // bytes.
        policy: Policy::Retain(
            "the legal-takedown tombstone, a FLOOR per moderation.md § Legal \
             takedown -> Posts: account deletion runs the same delete door \
             (retract_actor_posts) that WRITES this row, before the purge walk, \
             and the blob door's rebuild reads only this table for a deleted \
             post's compelled digests -- a purge would re-serve them to any \
             digest holder until the GC sweep. Same class as \
             obligation_action_records: a compliance record outlives its subject",
        ),
        // SUCCESSION RULING — Move, and the reason is the actor-keyed reader.
        // The corpus this row withholds FROM moves with the account:
        // `content.author` and `segment_records.scope_id` are `Move(Plain)`
        // (the content-plane ruling, 2026-08-14), so after the ceremony the
        // successor's `include_blobs` export walks the legacy segments — and
        // seeds its withhold set from THIS table by the successor's id. A
        // `Stay` would leave the set empty for exactly the pairs the retired
        // identity deleted while taken down, so the compelled bytes ride the
        // successor's next export until compaction — the leak the table was
        // built to close, reopened by the ceremony. Safe in the direction
        // that matters: nothing AUTHORIZES through this column (the export
        // door authenticates its caller itself; the column only says whose
        // withhold set a post belongs to), and the mark-what-you-carry
        // objection does not apply — the row is not an act the retired key
        // performed being credited to the successor, it is a compelled fact
        // ABOUT a post that is now the successor's. The blob door is
        // actor-blind, so it is unchanged either way. Witnessed by both
        // data-driven sweep (the plain leg is the whole leg; no FK, PK is
        // the post id).
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING. Every column is the exporter's own compelled fact:
        // the id of THEIR post, the citation the tombstone already names to
        // them (the `posts` domain rides `legal_takedown_ref` for the same
        // positive reason, and `obligation_action_records` rides whole because
        // transparency to the author is the mechanism's stated point), the
        // digests of their own attachments — names, never bytes; the bytes
        // themselves are withheld by the pair leg and the door — and when
        // they deleted it. Under the eviction token, the weakest credential,
        // that discloses nothing the author was not shown at takedown time.
        export: Export::Verbatim,
    },
    // The author-facing half of the moderation transparency record, and it
    // moves with the content it is about — `content.author` was ruled
    // `Move(Plain)` on 2026-08-14, so the taken-down posts are the successor's
    // while this column decides whose *queue* the notice appears in.
    // `fauna.moderation.actions` reads `get_obligation_actions_for_author(hex(
    // connection actor))`, so a `Stay` empties the successor's transparency
    // page of the actions taken against posts they now own, and leaves the
    // record readable by exactly one actor — the retired one, refused
    // everywhere. That is the F4 transparency gap `post_legal_takedown_txn`'s
    // all-or-nothing transaction exists to prevent, opened by the ceremony
    // rather than by a crash.
    //
    // Safe in the direction that matters: nothing AUTHORIZES through this
    // column. The one live consult that gates behaviour — the OVERTURN
    // exclusion in `nostr::store`'s materialization query — matches on
    // `content_type` + `content_id` + `action_taken` and never on the author,
    // so a post that was legally taken down stays un-materialized under either
    // verdict.
    //
    // ⚠ The `Retain` reason below said "signed" until 2026-08-15 and no
    // production row is: `post_legal_takedown_txn`, the only production writer,
    // stores an EMPTY signature, and `insert_obligation_action` — the arm that
    // takes a real one — has no caller outside tests. Corrected rather than
    // relied on; had it been true it would have been an argument for `Stay`.
    ActorTable {
        table: "obligation_action_records",
        column: "author_hex",
        key: ActorKey::Hex,
        policy: Policy::Retain(
            "anti-abuse moderation obligation-action audit record -- same class \
             as content_reports: the content author's identity should not silently \
             vanish from a compliance action record. (The `signature` column is \
             written EMPTY by the only production writer, post_legal_takedown_txn; \
             the signing arm insert_obligation_action is test-only today.)",
        ),
        succession: Succession::Move(MoveShape::Plain),
        // EXPORT RULING (the residual set). Ruled by
        // deferring to the owner doc, which is the same transferable rule
        // working in the PERMISSIVE direction for the first time (it withheld
        // `content_reports` because `report-sharing.md` § One gate function
        // rules every read of that table through `exposed_report_count`).
        // Here the owner doc rules the opposite: `moderation.md` § Queue
        // defines `fauna.moderation.actions` as returning these rows **scoped
        // to the connection actor's own content**, and the whole legal-takedown
        // design is "a visible tombstone + `fauna.moderation.appeal` + audit
        // log, never a silent removal"
        // (`content-moderation-and-ranking.md` § Resolved design decisions Q5).
        // Transparency to the author is the mechanism's stated point, so an
        // archive quieter than it would be arguing with its own goal doc.
        //
        // ⚠ Read the neighbouring `content_reports` verdict and do NOT
        // generalize it here: these two tables look adjacent and are ruled
        // opposite ways, both times by asking the owning doc rather than the
        // columns. A report names a REPORTER whose k-anonymity is the gate; an
        // obligation action names no complainant at all — every column is the
        // action taken, the rule it came from, and the content it hit.
        //
        // The wire projection is narrower than the row (`ObligationAction`
        // carries id / content_type / content_id / category / confidence /
        // action / timestamp), and the extra at-rest columns still ride:
        // `obligation_id` and `rule_index` ARE the "[reference]" the design's
        // tombstone promises to name, `label_id` points at the label row, and
        // `signature` is written EMPTY by the only production writer.
        export: Export::Verbatim,
    },
    // === Ruled 2026-09-21 off the declared-type walk's held carriers ===
    //
    // Three tables the census had never opened, because no column on them is
    // named like a person: the walk over declared column TYPE read their
    // writers and found an actor id in each (`opaque_carriers.rs`;
    // `succession-repoint-axis.md` § The declared re-point axis, the type
    // walk's held-carriers blockquote). Until these entries nothing deleted or
    // re-pointed those ids.
    ActorTable {
        table: "feature_policies",
        column: "subject_id",
        key: ActorKey::Blob,
        // The account a SELF-imposed feature policy binds
        // (`db/feature_gate.rs::put_feature_policy` derives it from the
        // authenticated caller and never trusts the wire). The nest-wide tiers
        // (region, admin) rest an EMPTY subject, so every statement keyed on a
        // 32-byte id is tier-selective by construction -- the registry has no
        // row filter, and needs none here for the reason `segment_records`
        // needs none: a zero-length blob can never equal an actor id.
        // `delete_user` also deletes these rows by hand in its own
        // transaction; the entry is what lets the WALKS see the table.
        policy: Policy::Purge,
        // The user's own choice about themselves -- "turn this feature off for
        // me" is the self-exclusion case the tier exists for
        // (`dynamic-features.md` § Authoring surfaces). A key rotation that
        // silently erased it would hand a self-excluded person their feature
        // back for having recovered their account. And `Move` is safe in the
        // COMPROMISE case too, which is what usually argues against one: the
        // self tier can only TIGHTEN (the meet composition makes relaxing
        // unrepresentable), so a thief-authored row widens nothing, and the
        // owner removes any limit with no cooling-off (the de-escalation
        // ruling, same §) -- a tier that cannot trap its author cannot trap
        // their successor either.
        succession: Succession::Move(MoveShape::Plain),
        // The user's own authored limits, in the same canonical document the
        // `self_limits` read returns to them; numeric bounds and an allow/deny,
        // no key material, nothing about anyone else. Keyed on `subject_id`, so
        // the nest-wide documents never ride in a user's export.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "membership_tiers",
        column: "admin_id",
        key: ActorKey::Blob,
        // The designation `(admin, subscription tier) -> {admin_tier,
        // lapse_tier}` that makes one of the admin's OWN subscription tiers
        // mean membership of this nest (`db/membership.rs`). It addresses
        // `subscription_tiers (author_id, name)` by value -- no foreign key, by
        // that migration's own choice -- so it takes its PARENT's verdicts or
        // it dangles: a purged designation over a retained tier would silently
        // stop admitting the members who paid for it.
        policy: Policy::Retain(
            "follows `subscription_tiers`, the tier it designates: that row is retained \
             as a financial/entitlement record, and a designation purged from under it \
             would turn paid nest membership back into a plain subscription -- every \
             later payment or renewal for the tier would stop running the membership \
             grant. Members already admitted keep their frozen `admitted_tier` / \
             `admitted_lapse_tier` stamp on the `subscribers` row (`db/membership.rs`), \
             so what dies is the rule, not the record of whom it admitted",
        ),
        // `subscription_tiers` is the family parent and moves `Plain`; every
        // read of this table scopes on the admin's actor id
        // (`list_membership_tiers`, `get_membership_tier`), so a row left
        // behind names a tier that no longer exists under that id.
        //
        // ⚠ **The Move carries a designation a THIEF may have written, and that
        // is ruled acceptable (2026-09-22).** A designation
        // is standing authority: a verified payment for (admin, tier) runs
        // `grant_membership`, and the set door checks only that the caller is
        // an admin and both quota tiers exist. So a seed thief can point any of
        // the admin's tiers at any quota tier, and the ceremony hands that to
        // the successor. The payment-claim ledger's answer, voiding at the
        // ceremony (`db/successions.rs`), was weighed and declined. A claim code
        // is a bearer credential that no app gesture can revoke. A designation
        // is listed and cleared by the successor from the app
        // (`fauna.admin.membership_tiers.list` / `.clear`), and every set leaves
        // a `membership_tier.set` audit entry naming its author and terms. A
        // void would also un-designate every legitimately paid membership tier
        // at each admin's routine device-loss recovery, which is a succession
        // too. Declared residual: nothing flags the carried designations at
        // the ceremony, so a thief-made one stands until the successor reviews
        // the list. It admits only payers of the admin's own tiers.
        succession: Succession::Move(MoveShape::Plain),
        // The admin's own configuration: two quota-tier NAMES and a timestamp
        // beside the tier name the public tiers route already serves.
        export: Export::Verbatim,
    },
    ActorTable {
        table: "worker_replication",
        column: "payload_key",
        key: ActorKey::Blob,
        // Bookkeeping that a payload reached the paired worker. `payload_key`
        // is TWO id spaces under one name: the inbox RECIPIENT's actor id on
        // `payload_type = 'inbox'` rows (`routes.rs::spawn_replicate_inbox`), a
        // post's content id on `'post'` rows. ⚠ The registry has no row filter
        // (`ActorTable` is one table and one column), so the `'inbox'`
        // narrowing is NOT expressible here and the safety of a statement keyed
        // on an actor id rests entirely on id-space disjointness -- a BLAKE3
        // content id can never equal an Ed25519 actor id, `segment_records
        // .scope_id`'s argument exactly. The one production reader is
        // `replication_count`, an admin-dashboard total, which a deleted
        // account's markers would over-count for ever -- the same truthfulness
        // argument `unmark_replicated` makes for a deleted post. What this does
        // NOT reach is the replica itself, which rests on the worker under the
        // same id and is that nest's to delete.
        policy: Policy::Purge,
        succession: Succession::Stay(
            "history, and true as written: the marker records that a payload was \
             replicated to the worker UNDER THE RETIRED ID (`spawn_replicate_inbox` sends \
             the recipient's id as the store key), so moving it would claim the worker \
             holds the successor's copy of something it holds the predecessor's copy of. \
             Nothing resolves a row through it -- the only reader counts rows",
        ),
        export: Export::WithheldOperational(
            "replication bookkeeping between this nest and its paired worker: a payload \
             kind, an id the user already has, a row number and a timestamp. Nothing here \
             is the user's data, and the count it feeds is an admin dashboard's",
        ),
    },
    // === Ruled 2026-09-21: the last two of the held carriers' rows ===
    ActorTable {
        table: "namespace_entries",
        column: "namespace",
        key: ActorKey::Blob,
        // The PUBLISHING actor's id, by the publish door's convention: the one
        // originating producer is `fauna.tls.publish_cert`
        // (`tls_handlers.rs`), which writes the caller's own id, and the sync
        // worker names it *the actor's self-namespace (= the actor pubkey)*.
        // The row is a MAILBOX, not what the box serves: a LAN TLS bundle
        // sealed to one nest, parked so a paired nest can pull it.
        // `lan_cert::install_client_issued_cert` copies an opened bundle into
        // `acme_dir`, and the listener serves from there -- so purging the row
        // takes no working certificate with the account. What it does take is
        // a blob nothing can reach any more: a pull is gated on the actor's
        // `nest_pairings` row, which is `Purge` too.
        // Since 2026-09-21 this is enforced, not a convention: `sync.pull` and
        // `sync.push` refuse a `namespace` other than the paired `actor_id`
        // (`federation_handlers.rs::paired_actors_namespace`), so no peer can
        // rest a row under another account's id. ⚠ What the door still does
        // NOT check is `actor_sig`, and by ruling it never will: the signature
        // convention is per entry kind, so each consumer verifies it
        // (`private-mode.md` § Namespace Sync). A pushed row under the actor's
        // own namespace may therefore carry a signature nothing has checked,
        // and `entry_id` is whatever string the pushing peer chose. Neither
        // changes this ruling, which keys only on `namespace`.
        policy: Policy::Purge,
        succession: Succession::Burn(
            "a signed INSTRUCTION TO INSTALL A TLS KEY, which is standing authority in the \
             most literal sense: `actor_sig` verifies only under the retired key, so the \
             row cannot move (it would rest under an id that never signed it), and it must \
             not stay -- the sync worker's pull watermarks are in memory, so every restart \
             of a paired nest re-pulls the namespace from zero and re-runs the install for \
             whatever still verifies there, over a certificate the successor has since \
             published. It is reachable only through the actor's pairing, which burns for \
             the same reason. Nothing is lost: an installed bundle is already in \
             `acme_dir`, and the successor's client re-issues under its own id",
        ),
        export: Export::WithheldSecret(
            "`ciphertext` is a TLS PRIVATE KEY under an HPKE seal to a nest's identity key \
             -- sealed key material, the `recovery_escrow` shape, and a key that opens the \
             deployment's traffic rather than anything of the exporter's. The admin's own \
             client issued it and holds the original",
        ),
    },
    ActorTable {
        table: "outbox",
        column: "author_id",
        key: ActorKey::Blob,
        // The author of a post or tombstone queued for forward to the paired
        // public nest. The column exists FOR this entry (schema v74): the table
        // had no person column at all, only the signed `payload`, so neither
        // registry could find an account's rows. Stamped by the three
        // producers, each with the verified author in hand
        // (`routes.rs::maybe_enqueue_outbox` / `maybe_enqueue_delete_outbox`).
        // The create path's hand is the CALLER, which is the author only
        // because post ingest is own-write (`storage/sealed.rs::
        // classify_encrypted_post` refuses any other uploader); before that bind a re-upload was stamped with its
        // uploader and escaped its author's deletion.
        policy: Policy::Partial(
            "the two entry types want OPPOSITE things and the registry has no row filter: a \
             queued POST must not be published after its author is gone, while a queued \
             DELETION is the message that removes a copy already published on the paired \
             nest -- purging it would make the account's deletion the reason a post \
             outlives it. So the purge walk runs a predicated leg \
             (`outbox_purge_for_deleted_author`): everything goes except a tombstone that \
             can still be sent, which leaves when the worker sends it or, refused, at the \
             retry ceiling, since an entry whose author has no account keeps only the retries \
             it has left -- pinned by \
             `db::tests::a_deleted_authors_queue_keeps_only_a_tombstone_that_can_still_be_sent` \
             and `db::tests::a_tombstone_kept_past_its_authors_deletion_leaves_at_the_ceiling`",
        ),
        // Follows `content.author`, the post it forwards, which moves `Plain`.
        // The stamp is a FINDER, not an attribution: its one reader is the
        // deletion leg above, and the account continues under the successor --
        // a stamp left on the retired id would hide the queue from the
        // successor's own deletion. Attribution lives in `payload`, which is
        // signed under the retired key, names it, and STAYS as written (the
        // peer verifies the envelope against the author the bytes name); so
        // after a ceremony the stamp and the bytes deliberately disagree, and
        // `opaque_carriers.rs` says so for the carrier.
        succession: Succession::Move(MoveShape::Plain),
        export: Export::WithheldOperational(
            "delivery bookkeeping: a verbatim copy of a post or deletion the author already \
             holds (the post rides the export's own domain), a retry counter and a \
             timestamp. Exporting it adds bytes, not data",
        ),
    },
    ActorTable {
        // User-initiated reporting (`moderation.md` § User-initiated
        // reporting). The row's own actor is the REPORTER of a local report;
        // a copy forwarded from a peer nest carries no reporter at all
        // (NULL — the reporter's identity never crosses a nest boundary), so
        // no local account owns it. The reported author (`subject_actor`) and
        // the resolving admin (`resolved_by`) are counterparty references,
        // ruled in `SUCCESSION_REFERENCES`.
        table: "abuse_reports",
        column: "reporter_actor",
        key: ActorKey::Blob,
        policy: Policy::Partial(
            "an account deletion is the reporter's withdrawal of every OPEN report they \
             filed, and nothing more (moderation.md § Where it lands, the deletion ruling): \
             `moderation::withdraw_abuse_reports_for_deleted_reporter` withdraws each open \
             row as the living door does -- status withdrawn, note and excerpt deleted, a \
             queued delivery dropped, a landed one followed by a `withdraw` queued in the \
             same step, so the home nest's copy loses the words too -- and leaves resolved \
             rows whole, words and retired reporter id included: the admin's audit of what \
             was decided and on what evidence, surviving its author as an appeal's reason \
             does, and a row the living reporter could not strip either. Withdrawn rows \
             keep the living withdrawal's skeleton under the retired id, the audit_log \
             precedent. Pinned by \
             `moderation::abuse_report_tests::a_deleted_reporters_open_reports_are_withdrawn_and_the_rest_stay` \
             and, across two nests, \
             `conformance_abuse_report_federation::a_deleted_reporters_forwarded_report_is_withdrawn_on_the_home_nest`",
        ),
        // A report is the reporter's own act and compels nothing (bound 1), so
        // nothing here is authority a seed thief could have minted over anyone
        // else. What follows the reporter is their ledger (`abuse_report.mine`),
        // their power to withdraw an open report, and the outcome notification
        // keyed on the reporter — left on the retired id, the successor could
        // neither see nor withdraw reports filed in their name, including any
        // a thief filed. The primary key is `id`, so the UPDATE cannot collide.
        succession: Succession::Move(MoveShape::Plain),
        export: Export::Redacted {
            omit: &["resolved_by"],
            reason: "the admin who recorded the outcome: the reporter is told the outcome \
                     only, never who decided it (moderation.md § What the reporter is \
                     told). Every other column is the reporter's own act or what they were \
                     told -- the subject they named, their reason, note and excerpt, where \
                     it went, its status and outcome",
        },
    },
];

/// The registry's plain-`Move` legs, in declaration order — **the executor's
/// work list, and the whole point of the axis.**
///
/// `record_succession` drives its bulk re-point from this iterator rather than
/// from a hand-typed list: the drift the axis was built to catch
/// (`account_aliases` missing from the ceremony for weeks, keeping a thief's
/// mail password authenticating) is now unrepresentable rather than merely
/// tested for. A table joins the re-point by declaring
/// [`MoveShape::Plain`] here and by nothing else.
///
/// Order is registry order, which is stable and alphabetical within each policy
/// group. Nothing depends on it — the plain legs touch disjoint tables and no
/// foreign key runs between them — but a deterministic order keeps the
/// row-count reporting and the logs reproducible.
pub fn plain_move_legs() -> impl Iterator<Item = &'static ActorTable> {
    ACTOR_TABLES
        .iter()
        .filter(|e| matches!(e.succession, Succession::Move(MoveShape::Plain)))
}

/// The registry's [`Succession::Burn`] legs, in declaration order — the burn
/// executor's work list, and the `Burn` sibling of [`plain_move_legs`].
///
/// **Why this needs no `BurnShape`.** [`MoveShape`] exists because a move can do
/// strictly more than move rows (rewrite other columns, absorb a superseded
/// third-party row, be a delete-then-insert). A burn cannot: every declared burn
/// leg is `DELETE FROM {table} WHERE {column} = ?old` and nothing else. A
/// *predicated* deletion — only some rows go — is [`Succession::Partial`] by
/// definition, and stays hand-written by construction. So the smallest axis that
/// makes the burn executor possible is the verdict that already exists.
///
/// **Unconditional, and that is the axis's rule rather than each leg's local
/// choice.** Standing authority on a retired identity dies whether or not
/// anything moves: a retired identity must not keep a live app password, a
/// live pairing, or a live bunker connection.
pub fn burn_legs() -> impl Iterator<Item = &'static ActorTable> {
    ACTOR_TABLES
        .iter()
        .filter(|e| matches!(e.succession, Succession::Burn(_)))
}

/// The reason a table's rows burn, straight from its declaration.
///
/// Reading it off the registry rather than restating it at the log site is the
/// point: the six hand-written legs this replaced each carried their own prose,
/// and prose beside a declaration is prose that can drift from it.
pub fn burn_reason(table: &str) -> Option<&'static str> {
    ACTOR_TABLES.iter().find_map(|e| match e.succession {
        Succession::Burn(reason) if e.table == table => Some(reason),
        _ => None,
    })
}

/// A set of tables a foreign key binds together, which therefore **move as one
/// or not at all** — the declaration `no_foreign_key_crosses_a_succession_leg`
/// accepts in place of a hazard, and the unit the executor moves under
/// `PRAGMA defer_foreign_keys`.
///
/// **Why a declaration exists at all, when deferral alone makes order
/// irrelevant.** Deferring the constraint moves the check from each statement
/// to the `COMMIT`, so *sequencing* stops mattering — but *completeness* starts
/// mattering absolutely. A family whose parent is ruled `Move` and whose
/// children are left `Unruled` still aborts, now at the commit instead of at the
/// statement, and now for the whole ceremony instead of one leg. Deferral does
/// not weaken the constraint; it only widens the window. So the thing that has
/// to be written down is not the order — it is **which tables are the family**,
/// so a gate can check that all of them are ruled the same way before any of
/// them moves.
///
/// **What a declaration asserts,** each pinned by a test below rather than left
/// to the author: every named table exists in the real schema
/// (`a_declared_family_is_really_coupled`), every one declares
/// [`MoveShape::Plain`] (`a_declared_family_moves_as_one`), the tables really
/// are foreign-key coupled (same test — a family that stops being coupled is a
/// stale suppression of the gate that guards it), and no table is claimed by two
/// families (`no_table_belongs_to_two_families`).
#[derive(Debug, Clone, Copy)]
pub struct CoupledFamily {
    /// What the family is, for logs and failure messages.
    pub name: &'static str,
    /// Every table the foreign keys bind together, the parent included.
    pub tables: &'static [&'static str],
    /// Why these rows are one unit — the sentence a future session reads
    /// before touching the grouping.
    pub reason: &'static str,
}

/// The declared coupled families.
///
/// The executor and the gate both work off this list. It was built (2026-08-13)
/// *before* any family joined it, deliberately: the one live instance — the
/// `subscription_tiers(author_id, name)` family — could not be declared without
/// also **ruling** it, and shipping the mechanism and the ruling together would
/// have graded a mistake in either as a mistake in both. The mechanism is
/// therefore proven against synthetic families in tests, independently of the
/// real ruling below.
///
/// The tier family joined it the same day, when that plane was ruled: every
/// member `Move(Plain)`, the reasoning at the `subscription_tiers` entry in
/// [`ACTOR_TABLES`]. Both questions the emptiness was waiting on are answered
/// there and in [`SUCCESSION_REFERENCES`] — the two people named by one
/// `subscribers` row get *different* verdicts on *different* axes (the author
/// owns the roster and moves with it; the paying reader's entitlement is ruled
/// as a reference column), and the period-key distributions move **and** rotate,
/// because moving alone strands the thief's copy over every future broadcast
/// while burning destroys the author's own back catalogue.
pub const COUPLED_MOVE_FAMILIES: &[CoupledFamily] = &[CoupledFamily {
    name: "subscription_tiers",
    // The parent first, then its three children, in the order
    // `no_foreign_key_crosses_a_succession_leg`'s doc comment names them.
    tables: &[
        "subscription_tiers",
        "subscribers",
        "subscribe_requests",
        "current_key_blobs",
    ],
    reason: "three tables carry a composite foreign key to \
             `subscription_tiers(author_id, name)`, so the author's tier plane has no \
             per-table choice at all: moving the parent alone orphans every child and \
             aborts the ceremony, and no ordering of the legs repairs it. They move as \
             one deferred unit or the plane stays `Unruled`. The verdict itself -- why \
             the whole plane moves rather than staying with a compromised author, and \
             why the key material inside it moves and is then rotated rather than being \
             left behind -- is argued at the `subscription_tiers` entry in ACTOR_TABLES.",
}];

/// The family that claims `table`, if any — the executor's grouping lookup and
/// the plain loop's skip test.
pub fn coupled_family_of(table: &str) -> Option<&'static CoupledFamily> {
    COUPLED_MOVE_FAMILIES
        .iter()
        .find(|f| f.tables.contains(&table))
}

/// One table's registry entry — the actor column and its encoding, which a
/// family member's leg needs exactly as the plain loop's does.
pub fn table_entry(table: &str) -> Option<&'static ActorTable> {
    ACTOR_TABLES.iter().find(|e| e.table == table)
}

/// The registry's **export-emitting legs** — every entry whose verdict admits
/// its rows into the per-actor export, in declaration order.
///
/// The export axis's counterpart to [`plain_move_legs`], and licensed by the
/// same sentence of the ruling that licenses that one: *registry-driven
/// emission is fine once verdicts exist, because the walk executes declared
/// verdicts rather than inferring them* (`account-data-plane.md` § Nest-side
/// requirements item 1, rule 2). Nothing here derives a disposition — a table
/// joins the export by carrying [`Export::Verbatim`] or [`Export::Redacted`]
/// and by nothing else.
///
/// [`Export::Shaped`] is deliberately absent: those rows already reach the
/// archive through a named shaped domain of `export_routes.rs`, and emitting
/// them here too would export the same data twice.
pub fn export_emit_legs() -> impl Iterator<Item = &'static ActorTable> {
    ACTOR_TABLES
        .iter()
        .filter(|e| matches!(e.export, Export::Verbatim | Export::Redacted { .. }))
}

impl Export {
    /// The manifest's **reason class** for a withheld verdict — the second half
    /// of rule 4's *"withheld tables are likewise declared by name and reason
    /// class"*. `None` for a verdict that is not a withholding.
    ///
    /// The class, not the prose reason: the reason strings argue the ruling to
    /// the next session that reads the registry, and shipping them in an
    /// archive the owner downloads would be publishing internal review notes,
    /// not telling them what the nest holds.
    pub fn withheld_reason_class(&self) -> Option<&'static str> {
        match self {
            Export::WithheldSecret(_) => Some("secret"),
            Export::WithheldDerived(_) => Some("derived"),
            Export::WithheldOperational(_) => Some("operational"),
            Export::Verbatim
            | Export::Redacted { .. }
            | Export::Shaped { .. }
            | Export::Unreviewed => None,
        }
    }
}

/// One table's contribution to the per-actor export.
#[derive(Debug, Clone)]
pub struct TableExport {
    /// The registry table name — the archive entry is `tables/<table>.ndjson`.
    pub table: &'static str,
    /// One compact JSON object per line, one line per row.
    pub ndjson: Vec<u8>,
    /// Rows emitted, so a caller can skip an empty table without re-parsing.
    pub rows: usize,
}

/// A table this nest holds and deliberately does not export — rule 4's
/// declaration, by name and reason class.
#[derive(Debug, Clone, Copy)]
pub struct WithheldDeclaration {
    pub table: &'static str,
    /// `"secret"` / `"derived"` / `"operational"` — see
    /// [`Export::withheld_reason_class`].
    pub reason_class: &'static str,
}

/// Everything the per-actor export needs from this registry: the emitted rows,
/// and the coverage declaration that says what was left out.
#[derive(Debug, Clone, Default)]
pub struct ActorExportSet {
    /// Tables that produced at least one row, in registry order.
    pub tables: Vec<TableExport>,
    /// Tables still awaiting a verdict — rule 3's honest interim, rule 4's
    /// partiality flag.
    pub unreviewed: Vec<&'static str>,
    /// Tables ruled out of the export, by name and reason class.
    pub withheld: Vec<WithheldDeclaration>,
}

impl ActorExportSet {
    /// Rule 4's partiality flag: true while any table this nest holds is still
    /// [`Export::Unreviewed`].
    pub fn partial(&self) -> bool {
        !self.unreviewed.is_empty()
    }
}

/// Reads one entry's rows for `actor`, as NDJSON.
///
/// `Ok(None)` means **this connection's schema has no such table** — the
/// registry includes tables that exist only under the `nostr`/`bluesky`/
/// `activitypub` features, none of which is in `bins/fauna-nest`'s default
/// feature set, so an unconditional `SELECT` would abort every export on a
/// nest built without them (the same reason
/// [`CacheDb::purge_orphaned_actor_rows`] skips rather than errors).
///
/// **Column encoding.** A SQLite `BLOB` becomes a lowercase-hex JSON string —
/// the convention every hand-written domain of `export_routes.rs` already
/// uses (`payload_hex`, `data_hex`, actor ids). `NULL`/integer/real/text ride
/// as their JSON counterparts; a non-UTF-8 `TEXT` value is lossily converted
/// rather than failing the whole export.
///
/// ⚠ **The `ActorKey` binding is load-bearing in exactly the way it is for the
/// purge.** A blob bound against a hex-`TEXT` column is not an error — it
/// matches nothing. As a delete that left `nostr_accounts.encrypted_privkey`
/// behind; as a read it would emit an empty file and look green.
fn read_actor_rows(
    conn: &rusqlite::Connection,
    entry: &ActorTable,
    actor: &[u8; 32],
    omit: &[&str],
) -> Result<Option<TableExport>> {
    if !table_exists(conn, entry.table)? {
        return Ok(None);
    }

    // Table and column names are compile-time constants from this module, never
    // caller input, so the interpolated SQL carries no injection surface.
    let sql = format!("SELECT * FROM {} WHERE {} = ?1", entry.table, entry.column);
    let mut stmt = conn
        .prepare(&sql)
        .with_context(|| format!("prepare export read for {}", entry.table))?;
    let names: Vec<String> = stmt.column_names().iter().map(|n| n.to_string()).collect();
    let keep: Vec<usize> = (0..names.len())
        .filter(|i| !omit.contains(&names[*i].as_str()))
        .collect();

    let actor_blob = actor.to_vec();
    let actor_hex = hex::encode(actor);
    let mut rows = match entry.key {
        ActorKey::Blob => stmt.query(rusqlite::params![actor_blob]),
        ActorKey::Hex => stmt.query(rusqlite::params![actor_hex]),
    }
    .with_context(|| format!("export read from {}", entry.table))?;

    let mut ndjson: Vec<u8> = Vec::new();
    let mut count = 0usize;
    while let Some(row) = rows
        .next()
        .with_context(|| format!("export row from {}", entry.table))?
    {
        append_row_as_ndjson(&mut ndjson, row, &names, &keep, entry.table)?;
        count += 1;
    }

    Ok(Some(TableExport {
        table: entry.table,
        ndjson,
        rows: count,
    }))
}

/// Encode one SQLite row as a compact JSON object and push it, newline
/// terminated, onto `ndjson`.
///
/// Extracted so the registry walk and the membership-resolved conversations
/// domain below cannot drift apart in how they encode a column: both doors
/// read the same table (`segment_records`), so a divergence here would make
/// one kind's rows unreadable beside their four siblings' in the same archive.
fn append_row_as_ndjson(
    ndjson: &mut Vec<u8>,
    row: &rusqlite::Row<'_>,
    names: &[String],
    keep: &[usize],
    table: &str,
) -> Result<()> {
    let mut obj = serde_json::Map::with_capacity(keep.len());
    for &i in keep {
        let value = match row
            .get_ref(i)
            .with_context(|| format!("read {}.{}", table, names[i]))?
        {
            rusqlite::types::ValueRef::Null => serde_json::Value::Null,
            rusqlite::types::ValueRef::Integer(v) => serde_json::Value::from(v),
            rusqlite::types::ValueRef::Real(v) => serde_json::Number::from_f64(v)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            rusqlite::types::ValueRef::Text(t) => {
                serde_json::Value::String(String::from_utf8_lossy(t).into_owned())
            }
            rusqlite::types::ValueRef::Blob(b) => serde_json::Value::String(hex::encode(b)),
        };
        obj.insert(names[i].clone(), value);
    }
    serde_json::to_writer(&mut *ndjson, &serde_json::Value::Object(obj))
        .with_context(|| format!("serialize {table} row"))?;
    ndjson.push(b'\n');
    Ok(())
}

/// The registry-driven walk, over an arbitrary entry list.
///
/// Taking the entries rather than reading [`ACTOR_TABLES`] directly is what
/// lets the *mechanism* be graded independently of the *ruling* — the
/// [`COUPLED_MOVE_FAMILIES`] precedent, and load-bearing here because the
/// skeleton pass left zero [`Export::Verbatim`]/[`Export::Redacted`] verdicts:
/// without a synthetic slice the emission would ship with no test that ever
/// saw it emit anything.
fn gather_export_set(
    conn: &rusqlite::Connection,
    entries: &[ActorTable],
    actor: &[u8; 32],
) -> Result<ActorExportSet> {
    let mut set = ActorExportSet::default();
    for entry in entries {
        match entry.export {
            // A table the actor has no rows in produces no archive entry at
            // all: an empty `.ndjson` would say "you have nothing here", which
            // the archive already says by omission, at the cost of one zip
            // entry per registry table once the backlog drains.
            Export::Verbatim => {
                if let Some(t) = read_actor_rows(conn, entry, actor, &[])?.filter(|t| t.rows > 0) {
                    set.tables.push(t);
                }
            }
            Export::Redacted { omit, .. } => {
                if let Some(t) = read_actor_rows(conn, entry, actor, omit)?.filter(|t| t.rows > 0) {
                    set.tables.push(t);
                }
            }
            // Already in the archive under a shaped domain — emitting here
            // would export the same data twice.
            Export::Shaped { .. } => {}
            Export::WithheldSecret(_)
            | Export::WithheldDerived(_)
            | Export::WithheldOperational(_) => {
                if table_exists(conn, entry.table)? {
                    set.withheld.push(WithheldDeclaration {
                        table: entry.table,
                        reason_class: entry
                            .export
                            .withheld_reason_class()
                            .expect("a withheld verdict has a reason class"),
                    });
                }
            }
            Export::Unreviewed => {
                if table_exists(conn, entry.table)? {
                    set.unreviewed.push(entry.table);
                }
            }
        }
    }
    set.unreviewed.sort_unstable();
    set.unreviewed.dedup();
    set.withheld.sort_unstable_by_key(|w| w.table);
    Ok(set)
}

/// The exporter's **conversation records**, reached by resolving their channel
/// membership first — the one kind the registry walk above structurally cannot
/// return.
///
/// ⚠ **Why this is a separate door and not a registry entry.** `segment_records`
/// is one table holding two kinds of identity in `scope_id`: `mail` scopes to
/// the recipient, `post` to the author, `calendar`/`card` to the owner — all
/// actors — while `conv` scopes to the **channel** (`records_db::insert_conv`,
/// `next_conv_seq`, "per-channel"). The walk's `WHERE scope_id = ?actor` is
/// therefore correct and complete for four kinds and structurally blind to the
/// fifth. Widening it is not the fix: a second registry entry on a channel
/// column is the family plane's forbidden shape — a shared scope walked as
/// though it were the actor's, which the registry's own rules would then bless
/// because *"the rows are the actor's"* reads true
/// ([`tests::the_guardian_family_exports_only_on_the_wards_own_column`] is the
/// same error one plane over). `segment_records` stays [`Export::Verbatim`]:
/// this domain row-filters to one kind, and a partial [`Export::Shaped`] is
/// deliberately not expressible.
///
/// **The shape is the one `account-data-plane.md` § Nest-side requirements
/// item 1 already ruled** (the fourth axis's *Universe* paragraph): *content
/// reachable via membership is served by the shaped domains under their own
/// access rules*. `export_routes.rs`'s `conversations` domain is the built
/// instance — it resolves the exporter's channel membership, then reads the
/// shared scope's records, other authors included.
///
/// **Disclosure.** The access rule is membership, and it is bounded twice over.
/// A member already receives every record of their channels through the
/// ordinary serving door (`segments::conv::read_after_seq`, which does not
/// filter by author), so this adds no reach. And the rows carry no third party
/// to begin with: `insert_conv` writes `sender_dom` / `spam_disp` /
/// `is_own_submission` as `NULL` — *"conv records persist no sender, so the
/// nest cannot attribute a stored sealed message to an author post-hoc"*
/// (`moderation_handlers.rs`, the legal-takedown doc comment). What rides is a
/// placement — channel, segment, `record_cid`, bucket, coordinates — under the
/// same reasoning that already cleared the mirror: naming a record does not
/// open one.
///
/// **Not the bodies.** Like its four siblings' rows, this emits the mirror and
/// not the segment `.dat` bytes the `record_cid`s address. Reaching the segment
/// store is one build for all five kinds.
///
/// `Ok(None)` when the actor is on no channel or has no conv records — an empty
/// entry would say *"you have nothing here"*, which the archive already says by
/// omission (the walk's own rule).
fn gather_conv_records(
    conn: &rusqlite::Connection,
    actor: &[u8; 32],
) -> Result<Option<TableExport>> {
    // Same skip-rather-than-error rule as the walk: a nest whose schema lacks
    // either table holds no conversations to export.
    if !table_exists(conn, "actor_channels")? || !table_exists(conn, "segment_records")? {
        return Ok(None);
    }

    // Step 1 — resolve membership EXPLICITLY. This read is the access rule:
    // the same statement `fauna.conversations.channel.list_for_actor` answers
    // to this actor over an ordinary door, and the same one
    // `federation_handlers`' MLS pull uses to bound a peer's reach
    // (`CacheDb::list_actor_channels`).
    let channels: Vec<Vec<u8>> = {
        let mut stmt = conn
            .prepare("SELECT channel_id FROM actor_channels WHERE actor_id = ?1")
            .context("prepare conv export membership read")?;
        let rows = stmt
            .query_map(rusqlite::params![actor.as_slice()], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .context("conv export membership read")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("conv export membership row")?
    };
    if channels.is_empty() {
        return Ok(None);
    }

    // Step 2 — read those channels' conv records. Chunked at SQLite's practical
    // bound-parameter limit, the same 900 `records_db`'s own multi-scope
    // statements use.
    const CHUNK: usize = 900;
    let mut ndjson: Vec<u8> = Vec::new();
    let mut count = 0usize;
    for chunk in channels.chunks(CHUNK) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        // `kind` is a literal and the scope ids are bound — no injection
        // surface, and the shape mirrors `read_actor_rows`' `SELECT *` so the
        // emitted column set tracks the schema rather than a hand list.
        let sql = format!(
            "SELECT * FROM segment_records WHERE kind = 'conv' AND scope_id IN ({placeholders})"
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare conv export record read")?;
        let names: Vec<String> = stmt.column_names().iter().map(|n| n.to_string()).collect();
        let keep: Vec<usize> = (0..names.len()).collect();
        let params = rusqlite::params_from_iter(chunk.iter().map(|c| c.as_slice()));
        let mut rows = stmt.query(params).context("conv export record read")?;
        while let Some(row) = rows.next().context("conv export record row")? {
            append_row_as_ndjson(&mut ndjson, row, &names, &keep, "segment_records")?;
            count += 1;
        }
    }

    if count == 0 {
        return Ok(None);
    }
    Ok(Some(TableExport {
        table: CONV_RECORDS_DOMAIN,
        ndjson,
        rows: count,
    }))
}

/// The archive path stem of the conversations domain — deliberately **not**
/// `segment_records`, so the two doors onto that one table never collide in the
/// zip and a reader can tell which key produced which rows.
pub const CONV_RECORDS_DOMAIN: &str = "conversations/records";

/// Columns that **name** an actor without the row belonging to it — outside
/// [`ACTOR_TABLES`] by design, and squarely inside succession's scope.
///
/// **The two axes do not share an in-scope set, and this list is where that
/// stops being an abstract point.** Deletion asks *"does this row belong to
/// actor X?"*, so a column naming X as a third party is correctly excluded —
/// purging on it would delete a row X does not own. Succession asks a different
/// question: *"does this row still name an identity that no longer exists?"* A
/// third-party reference is exactly as stale after a ceremony as an ownership
/// row, and nothing about the deletion exclusion says so.
///
/// Entries are declared here rather than folded into `ACTOR_TABLES` precisely
/// so that no future reader can mistake one for a deletion target.
pub const SUCCESSION_REFERENCES: &[(&str, &str, Succession)] = &[
    (
        "contacts",
        "peer_id",
        // The COUNTERPARTY of a contact edge -- the knock's verified sender,
        // or the peer the holder accepted or blocked. The row is the holder's
        // (`contacts.actor_id`, `Move(Bespoke)`), so deletion rightly leaves
        // it: an edge TO a deleted account rides in the holder's address book
        // until the holder removes it. Declared here rather than in the
        // opaque-carrier walk since 2026-09-21, when the `peer` root let the
        // census see it.
        Succession::Move(MoveShape::Bespoke(
            "the peer side of the same ABSORB the row's own entry declares: \
             `collapse_contact_edges` rewrites a succeeded peer's id in every local \
             holder's edge, merging a colliding pair on the stricter-status rule, and \
             `record_peer_succession` runs the identical collapse for a foreign peer. \
             Hand-written because it is a merge on the (actor_id, peer_id) primary key, \
             not an UPDATE",
        )),
    ),
    (
        "custody_hosting",
        "owner_actor_id",
        // The custodied OWNER, named by a row that belongs to the HOST (the
        // depositing user). The row is the host's projection of a ceremony
        // whose witness the OLD owner identity signed — and witnesses signed
        // by a retired key die with that key's trust (the custody grants join
        // the succession re-mint sweep: the successor re-offers, the host
        // re-accepts, and the host's client deposits a FRESH row under the
        // successor's id and new grant_id). Rewriting this pointer would forge
        // a hosting row for a ceremony that never happened, wrapping a witness
        // the successor never minted. The stale row is inert by refusal — the
        // owner-side custody handshake refuses the dead witness at its
        // capability row, so the pump's dial fails closed every pass — until
        // the host's reconcile replaces or stops it.
        Succession::Stay(
            "the host's projection of a ceremony the RETIRED owner identity signed; \
             the witness inside dies with that key and the re-mint sweep replaces \
             the ceremony (and this row) wholesale under the successor, so following \
             the pointer would forge a hosting row for a ceremony that never \
             happened — the stale row fails closed at the owner-side handshake",
        ),
    ),
    (
        "custody_hosting",
        "owner_devices",
        // Payload of the same dead ceremony snapshot as `owner_actor_id`
        // directly above — an opaque endpoints capture the ceremony delivered,
        // never separately followed or rewritten.
        Succession::Stay(
            "opaque payload of the same retired-owner ceremony snapshot as \
             `custody_hosting.owner_actor_id`; replaced wholesale by the re-mint \
             sweep's fresh deposit, never rewritten in place",
        ),
    ),
    (
        "custody_hosting",
        "owner_nest_url",
        // Same snapshot, same ruling: the dial anchor of a dead ceremony. The
        // pump's per-pass URL policy check plus the handshake refusal bound
        // what a stale anchor can do (fixed-shape connect attempts, the
        // accepted residual), and the fresh ceremony deposits its own.
        Succession::Stay(
            "the dial anchor of the same retired-owner ceremony snapshot as \
             `custody_hosting.owner_actor_id`; the handshake refuses the dead \
             witness so a stale anchor decays to bounded failed dials, and the \
             re-mint sweep's fresh deposit carries the successor's own",
        ),
    ),
    (
        "mail_domains",
        "catch_all_actor_id",
        // The ruling row 76 asked for. ⚠ **It was first written as `Stay` and that
        // was WRONG — the correction is recorded here rather than quietly applied,
        // because the mistake is instructive and a future session may be tempted
        // back into it.**
        //
        // The `Stay` reasoning went: the catch-all is admin-plane config, an admin
        // may aim it at an actor who does not own the domain, so a user's own
        // ceremony must not re-aim a deployment-wide decision — and its only cost
        // is unmatched mail black-holed to an unreadable identity, a LOSS and not a
        // leak. **The first half stands; the cost was an overclaim, and it
        // conflated retrieval with sealing.** Only *retrieval* is closed by the
        // alias re-point:
        //
        //   1. `catch_all_actor_id` feeds `resolve_recipient` as the fallback, so
        //      unmatched mail resolves to the **retired** actor.
        //   2. That actor's `actor_mls_pubkeys` row deliberately survives — its
        //      survival is what arms the `succession_pending` tempfail for the
        //      address that *did* move.
        //   3. So `get_recipient_mail_seal_key(retired)` returns `Some(k)`, and
        //      that branch reports `succession_pending = false` unconditionally.
        //      The tempfail fires only on `None`, so it never fires here.
        //
        // Every future catch-all delivery would therefore be freshly sealed to a
        // key derived from the MSEK the thief read, **with no end date** — strictly
        // worse than the residuals the ceremony knowingly accepts elsewhere, which
        // decay precisely because sealing STOPS at the ceremony. Not a black-hole:
        // an open sealing channel to a burned key.
        //
        // **Clear, therefore — and clearing is not the same concession as
        // following.** Following would re-aim an admin's routing decision, which is
        // the objection the `Stay` reasoning got right. Clearing merely removes a
        // designation that has become unsafe, without inventing a new target, and
        // it makes the failure LOUD: unmatched mail is rejected where an admin can
        // see it rather than vanishing. Nothing irrecoverable dies — the
        // designation is admin-recreatable in one gesture, so the alpha
        // deletion carve-out is not even engaged.
        //
        // **Row 242 (2026-08-19): the clear now stamps
        // `mail_domains.catch_all_cleared_by_succession_at`** (in
        // `record_succession`) — this classification comment is
        // documentation only, the `UPDATE` in `successions.rs` is the
        // actual fire point. The stamp is what lets the admin-visible
        // surface (`admin-dns`, tui-first) tell "cleared by a succession"
        // apart from "never set" — the gap this row exists to close.
        // `update_mail_domain_config` clears the stamp back to `NULL` on any
        // subsequent admin-driven set/clear, so a fresh admin decision always
        // supersedes it.
        Succession::Clear(
            "admin-plane routing, so it is CLEARED and never followed — re-aiming it \
         would let one user's private ceremony move a deployment-wide decision an \
         admin made, possibly at an actor who does not own the domain. Leaving it \
         is not the harmless black-hole it looks like: unmatched mail keeps \
         resolving to the retired actor, whose seal key deliberately survives, and \
         that branch reports succession_pending = false — so every future delivery \
         is freshly sealed to a key the thief can derive, with no end date. \
         Clearing closes the channel and makes the failure visible to the admin \
         who must re-designate",
        ),
    ),
    (
        "bridge_service_users",
        "approved_by_actor_id",
        // Ruled 2026-08-12, and the ruling is the *smaller* half of what this
        // column cost. It was a FOREIGN KEY to `admin_actor_ids(actor_id)` —
        // an AUDIT pointer bound to a LIVE AUTHORITY row — while the admin leg
        // of `record_succession` is a delete-then-insert. Under
        // `PRAGMA foreign_keys = ON` that DELETE did not strand a row or no-op:
        // it **aborted the entire succession transaction**, so an admin who had
        // approved any bridge could not rotate away from a compromised key at
        // all. The same FK also broke `remove_admin_actor`. The FK is gone
        // (the genesis declares the column without one); what remains is the
        // succession question, and the answer is that nothing needs to move.
        //
        // Found by walking the foreign-key graph against this registry rather
        // than by a report — the generalized form of the check is in
        // `no_foreign_key_names_a_table_the_ceremony_deletes_from`, because the
        // hazard is structural: any FK whose parent is a table the ceremony
        // deletes from turns a leg into a total ceremony failure.
        Succession::Stay(
            "history/attribution: the retired identity genuinely did approve that \
         bridge registration, and nothing RESOLVES through this pointer — a \
         bridge's authority is its `status` plus `bridge_method_allowlist`, \
         never its approver — so leaving it is neutral, which is exactly the \
         test `Clear` exists for and this column fails. Re-pointing it to the \
         successor would be worse than useless: it would silently re-attribute \
         an approval a SEED THIEF may have made to the very identity recovering \
         from them, which is the opposite of the aftermath's mark-what-you-carry \
         rule. Nothing is lost by staying — `actor_successions` links the old \
         actor forward, so an admin surface can always resolve who that approver \
         is today",
        ),
    ),
    // === The guardian side of the family plane (ruled 2026-08-13) ===
    //
    // The supervised side is twelve `ACTOR_TABLES` entries keyed on
    // `supervised_actor_id`; these three columns name the OTHER party, and they
    // answer a **different ceremony** — the guardian's own, or a third party's.
    // They were invisible until this pass for a structural reason worth naming:
    // the completeness walk skipped any table already in `ACTOR_TABLES`, so a
    // second actor column on a registered table was accounted for by nothing.
    // That is now `every_actor_shaped_column_is_registered_or_excluded`'s job.
    (
        "guardianships",
        "guardian_actor_id",
        // `family-safety.md` § Lifecycle gates: *"A stranded ward is
        // unrepresentable: no path removes a guardian account while a link
        // references it"* — enforced by a gate on evict/delete. **Succession is
        // neither.** It retires the guardian's identity (refused everywhere)
        // while this column goes on naming it, so the invariant's words
        // survived and its guarantee did not: `guardian_accounts_for(G')`
        // selects `WHERE guardian_actor_id = ?1` and returns nothing, so the
        // successor guardian sees no wards, while the ward stays supervised
        // with policies enforced and an approvals queue nobody can attend.
        //
        // Moving is not the `catch_all_actor_id` concession it superficially
        // resembles. That objection was that a *user's* private ceremony must
        // not re-aim a decision an admin made about a third party. Here the
        // guardian succeeding IS the guardian: a succession is identity
        // rotation, not a change of person, so the duty follows the identity.
        // The consent the transfer handshake exists to protect is therefore
        // already given — `fauna.family.transfer` needs the proposed guardian's
        // consent because re-pointing onto an actor who never agreed conscripts
        // them, and no new person is being conscripted by a rotation.
        //
        // Degraded rather than stranded without this leg (an admin can still
        // graduate or transfer the ward), so it is not a
        // `nest/common.md` § Client-state recoverability breach — but it is
        // exactly the half-state § Lifecycle gates declares unrepresentable,
        // reached by the one path that gate never grew a leg for.
        Succession::Move(MoveShape::Bespoke(
            "the guardian's own ceremony must carry their wards with them, or \
             the link names an identity refused everywhere and the ward is \
             supervised by nobody. Hand-written because the \
             registry-driven loop moves each table's ONE declared column and \
             this is the second column of a table whose first one moves for a \
             different ceremony",
        )),
    ),
    (
        "guardian_transfers",
        "proposed_guardian_actor_id",
        // Same ceremony as the link's guardian column, one table over: a
        // pending proposal names the actor who must accept it, and
        // `fauna.family.transfer.accept` re-validates against the caller. If
        // the proposed guardian succeeds mid-handshake the row names an
        // identity that can never call accept, so the proposal is dead while
        // still occupying the one-pending-per-ward slot until it lapses.
        // Self-healing in 7 days, so this is the mildest of the three — ruled
        // with them because it is the same question and the same leg.
        Succession::Move(MoveShape::Bespoke(
            "a pending proposal must name an identity that can still accept it; \
             hand-written with the rest of the guardian family",
        )),
    ),
    (
        "guardian_contact_requests",
        "peer_actor_id",
        // A THIRD party's ceremony, not either family member's: the peer the
        // ward asked to contact. Left behind, the guardian's approval would
        // mint a contact edge to a retired identity — the ask survives its
        // subject and resolves to nobody.
        Succession::Move(MoveShape::Bespoke(
            "the ward's pending ask must keep naming the person it is about \
             after that person's own succession; hand-written with the rest of \
             the guardian family",
        )),
    ),
    (
        "guardian_transfers",
        "initiated_by",
        // Ruled although the actor-shaped walk **cannot see it** — no
        // `actor`/`author`/`owner` root — which is `succession-aftermath.md`'s
        // "complete only against its own vocabulary" finding one level down. It
        // is recorded here rather than left to the heuristic that missed it.
        //
        // Read at source before ruling: `initiated_by` has no consumer outside
        // the row's own read struct. Cancel authority is "current guardian or
        // admin", checked against `guardianships`, never against this column —
        // so nothing resolves through it and leaving it is neutral, which is
        // the test `Clear` exists for and this column fails.
        Succession::Stay(
            "history/attribution: who opened this proposal. Nothing resolves \
             through it — cancel authority is checked against the guardianship \
             link, not against the initiator — so leaving it is neutral, and \
             re-pointing it would re-attribute the act to an identity that did \
             not perform it",
        ),
    ),
    (
        "actor_successions",
        "new_actor_id",
        // The successor half of the chain's own audit row. Surfaced by the same
        // second-column walk as the guardian family; ruled here because the
        // answer is load-bearing and easy to get catastrophically wrong.
        Succession::Stay(
            "the succession chain itself, and the ONE reference column where \
             moving would be actively destructive: `succession_heal_pairs` \
             walks these rows old -> new to find a chain's terminal, so \
             re-pointing `new_actor_id` at a later successor would collapse a \
             hop and make the chain unwalkable. Each ceremony writes its own \
             row; this one records a hop that genuinely happened, which is \
             history by the same rule as `old_actor_id`'s Policy::Retain beside it",
        ),
    ),
    // === The paying READER's side of the entitlement plane (ruled 2026-08-13) ===
    //
    // The author's side is eight `ACTOR_TABLES` entries moving as one declared
    // family (the `subscription_tiers` block there). These three columns name
    // the OTHER person in the same plane — the reader who paid — and until this
    // pass they were accounted for by **nothing**: no entry, no reference
    // declaration, no reasoned exclusion. Not a judgement anybody made:
    // `every_actor_shaped_column_is_registered_or_excluded` matched column names
    // against `actor`/`author`/`owner`, and this plane names its second person
    // *subscriber* and spells its third as an act (`redeemed_by`). The walk's
    // own vocabulary is widened separately, in its test — ruling a plane and
    // rebuilding the gate that should have found it are different acts, and
    // shipping them as one commit would grade a mistake in either as a mistake
    // in both.
    //
    // ⚠ **The three move as ONE leg, and their coupling is a join in CODE, not a
    // foreign key** — so `COUPLED_MOVE_FAMILIES` cannot express it and
    // `no_foreign_key_crosses_a_succession_leg` cannot catch a split ruling.
    // `payment_core::apply_refund` reads `payment_claim_codes.redeemed_by` to
    // learn who holds the entitlement a refunded payment bought, then calls
    // `set_subscriber_valid_until(author, that_buyer, tier)`. Move the roster row
    // and leave the redeemer behind and the refund un-entitles an identity that
    // no longer holds the row: **the successor keeps access that was refunded**,
    // and on the claim-code path there is no second handle to fall back on
    // (`Buyer::Unbound` is precisely why claim codes exist). A half-ruling here
    // is worse than either whole one, which is why the leg is one function.
    //
    // ⚠ **Reachability bounds all three** (the finding): a subscriber's rows
    // live on the AUTHOR's nest, and a ceremony runs only where the succeeding
    // identity is locally registered — `record_succession` needs a local chain,
    // and `record_peer_succession` refuses `OldIsLocal` and runs no move legs at
    // all. So this closes the LOCAL reader (subscribed to an author hosted on
    // the same nest) and leaves the cross-nest reader as the same residual the
    // `bridge_*` plane inherits. Ruled as if reachable, and said here so a later
    // session does not read the ruling wider than it is.
    (
        "subscribers",
        "subscriber_id",
        // The reader's own entitlement — the row that says they paid. Three
        // readers key on this column and every one of them stops answering for
        // the person who paid if it is left behind: `list_my_subscriptions`
        // (their own subscription list), `get_subscribed_tiers` and
        // `is_subscriber` — and the last two gate
        // `fauna.subscriptions.key_blob.get`, the key material that opens the
        // content. So a succeeding reader does not
        // merely lose a list: every tier they bought goes dark. That is the
        // `account_aliases` class exactly — an entitlement column nobody was
        // watching — and § Re-key scope's class rule reaches it in one step,
        // since a paid entitlement is future access.
        //
        // ⚠ **Not a plain move: the ek is CLEARED as the row moves.**
        // `mlkem_encaps_key` is the subscriber's published ML-KEM-768
        // encapsulation key, derived from their identity SEED
        // (`fauna_core::subscription::crypto::derive_subscriber_xwing_keypair`),
        // while the X25519 half of the same X-Wing public is reconstructed by
        // the author from the subscriber's ActorId. Carrying the column would
        // hand the author a hybrid public made of the *predecessor's* ML-KEM
        // half and the *successor's* X25519 half — a key nobody holds the
        // secret for, so every future wrap under it is unopenable. That is the
        // worst available outcome: an entitlement that looks preserved and
        // serves nothing. NULL is a first-class state (classical-only
        // subscribers leave it NULL and `create_key_blob_entry_auto` wraps them
        // classically), and the successor re-publishes on its next
        // `fauna.subscriptions.subscribe`, whose already-subscribed arm exists
        // to upgrade a stored ek in place (`update_subscriber_mlkem_ek`). The
        // clear therefore costs one suite for one rotation and self-heals
        // through machinery that already ships.
        Succession::Move(MoveShape::Bespoke(
            "the reader's paid entitlement follows the reader, or every tier they \
             bought goes dark -- `is_subscriber` and `get_subscribed_tiers` gate the \
             key-material fetches, not just the list. Hand-written for \
             two reasons: it is the SECOND actor column of a table whose first one \
             moves for the author's ceremony, and the leg NULLs `mlkem_encaps_key` as \
             it moves, because that key derives from the predecessor's seed and pairs \
             with an X25519 half derived from the successor's ActorId -- carried, it \
             wraps to a hybrid public nobody holds the secret for",
        )),
    ),
    (
        "subscribe_requests",
        "subscriber_id",
        // The same person one step earlier in the same flow: a subscribe or
        // unsubscribe request the author has not drained yet. Ruled with the
        // roster row rather than after it, because the pending half is where the
        // loss is *silent* — `list_my_subscriptions` reports these as
        // `pending`, and an approval drained after the ceremony would mint the
        // entitlement onto the retired identity, which is a fresh grant to an
        // actor refused everywhere rather than a stale row.
        //
        // Carries its own `mlkem_encaps_key` for the enqueue → approve gap and
        // clears it for the same reason the roster row does; the approval path
        // copies whatever is there onto the `subscribers` row, so a stale ek
        // here would survive the ceremony by being copied *forward*.
        //
        // **Deletion of this half (ruled 2026-09-22):** the
        // registry keys the row on its author, so no registry verdict reaches
        // the reader, and a deleted reader's request used to wait in the
        // author's queue, with their id and ek, until the author drained it.
        // The purge walk runs a hand-written leg instead
        // (`subscribe_requests_purge_for_deleted_subscriber`, pinned by
        // `a_deleted_readers_pending_subscribe_requests_leave_the_authors_queue`):
        // pending `subscribe` rows go, paid or not, because each is only an
        // instruction to grant a tier to an account that no longer exists;
        // `unsubscribe` rows stay, because draining one removes the retained
        // `subscribers` row. Two stated residuals: it reaches only a reader
        // this nest hosts (the reachability note above), and the boot-time
        // unlock fan-out reconcile can re-enqueue a `subscribe` row from that
        // reader's retained, unexpired roster row. That row names only the id
        // the roster still holds, and it carries no ek.
        Succession::Move(MoveShape::Bespoke(
            "a pending request must keep naming the reader who made it, or the \
             author's drain grants the tier to a retired identity. Same leg, same \
             ek clear, and the same second-column reason as `subscribers`",
        )),
    ),
    (
        "payment_claim_codes",
        "redeemed_by",
        // **Ruled `Move`, against the prediction the row that asked for it
        // carried (`Stay` — "attribution, rows are kept for audit"), and the
        // correction is recorded rather than quietly applied.** The audit half
        // of that reading is right: the schema keeps claim rows forever and
        // never deletes them. It is the *nothing resolves through it* half that
        // is false, and it is the half the verdict turns on — the same test the
        // `mail_domains.catch_all_actor_id` ruling was corrected by.
        //
        // `apply_refund` resolves through it. On a refund or dispute for a claim
        // that was already redeemed, `void_payment_claim` returns false and the
        // handler reads `redeemed_by` as the buyer, then voids *that* actor's
        // paid window. With the roster row moved and this column left behind,
        // the void names the retired identity, matches nothing, and the
        // successor keeps a refunded entitlement — money returned, content still
        // served. There is no fallback handle on this path: a claim-code
        // purchase is `Buyer::Unbound` by construction, which is the whole point
        // of a claim code.
        //
        // The thief objection that keeps other audit columns `Stay` does not
        // reach here, because it points the other way: a redemption a thief made
        // *granted the account an entitlement*, and that entitlement moves with
        // `subscribers`. Following it with the redeemer pointer keeps the refund
        // able to take back exactly what the payment gave, which is the
        // conservative direction. Re-redemption is unaffected either way — the
        // `prev != redeemer` guard turns a successor's replay into the same
        // idempotent re-grant it is for any other repeat caller.
        //
        // ⚠ This rules the READER's column only. `payment_claim_codes.author_id`
        // is a separate question (the seller's side) and is still `Unruled` in
        // `ACTOR_TABLES` — see the backlog row for the non-FK entitlement
        // tables, which must not read this entry as having answered it.
        Succession::Move(MoveShape::Bespoke(
            "the refund path resolves through it: `apply_refund` reads `redeemed_by` \
             to find whose paid window to void, and a claim-code purchase has no \
             other buyer handle (`Buyer::Unbound`). Left behind while the roster row \
             moves, a refund voids an identity that holds nothing and the successor \
             keeps access that was refunded. Hand-written with the two `subscriber_id` \
             columns because the three are joined in CODE rather than by a foreign \
             key, so no family declaration can hold them together",
        )),
    ),
    // The three below were surfaced by the second-column walk and are genuine
    // third-party references, but their tables are themselves `Unruled`, so
    // they join the backlog **visibly** rather than being ruled in a pass whose
    // subject was the family plane. `the_unruled_reference_backlog_only_shrinks`
    // is what keeps them from hiding here the way they hid before the walk saw
    // them. (The first of the three, `labelers.caller_actor`, is
    // RULED — its table came up in the curation pass, which is
    // precisely the condition this comment was waiting on. Two remain.)
    // RULED 2026-08-15, with its table (`labelers.publisher_actor`, `Stay`).
    // Of the three deferred above this is the one whose table came up, and the
    // two columns answer opposite questions — which is exactly why the block
    // above declined to rule it while `labelers` was `Unruled`.
    //
    // **`caller_actor` is a DoS bound, and a succession resets it.** It exists
    // because `publisher_actor` could not bound anything: the security review
    // observed that the signing keypair is free and off-box-rotatable, so
    // `MAX_LABELERS_PER_PUBLISHER` is evaded by a fresh keypair per publish, and
    // installed `MAX_LABELERS_PER_CALLER` keyed on "the un-rotatable enrolled-user
    // identity" instead. `put_labeler` enforces it with `SELECT COUNT(*) FROM
    // labelers WHERE caller_actor = ?`. Left behind, the successor's count reads
    // zero and the cap re-opens in full — and a succession is **self-service**
    // (§ Re-key scope's limits-move rule: any holder performs one with their own
    // recovery kit), so the bound designed to survive keypair rotation is
    // defeated by identity rotation instead. 64 rows per ceremony, unbounded
    // ceremonies. That the catalog has **no delete verb at all** is what makes
    // the leak permanent rather than merely wide: every row the reset admits is
    // in the DB forever, under the deployment-wide `MAX_TOTAL_LABELER_BYTES`
    // backstop and nothing else.
    //
    // Moving is also the *accurate* reading, not a punishment: the storage
    // really was consumed and really does still exist, so the successor
    // genuinely holds it. The standing thief objection (a thief who published 64
    // leaves the victim no headroom) is the one § Re-key scope already answers —
    // rare, not self-service, admin-remediable — and here it is bounded by the
    // same 64 either way.
    //
    // Hand-written (`rule_the_labeler_publication_quota`) rather than
    // registry-driven, for the mechanical reason every reference leg is: the
    // loop moves each table's **one** `ACTOR_TABLES` column, and that column on
    // this table is `publisher_actor`, which stays.
    (
        "labelers",
        "caller_actor",
        Succession::Move(MoveShape::Bespoke(
            "the per-caller publication cap resolves through it: `put_labeler` counts \
             `WHERE caller_actor = ?` against MAX_LABELERS_PER_CALLER, the bound \
             the security review installed *because* the per-publisher cap keys on a \
             freely-rotatable signing keypair. Left behind, a self-service succession \
             resets it to zero — the un-rotatable identity the cap was moved onto turns \
             out to be rotatable after all — and the catalog has no delete verb, so \
             every row the reset admits is permanent. Hand-written because it is the \
             SECOND actor column on a table whose registry column (`publisher_actor`) \
             stays",
        )),
    ),
    // === The counterparty columns, RULED 2026-08-15 ===
    //
    // All four move, and one sentence covers them: **a counterparty reference
    // names a HUMAN, and the human is continuous across their own ceremony.**
    // Every consumer below is counting or matching people — distinct reporters,
    // distinct DM recipients, a correspondent pair, a dedup key — so a
    // left-behind reference does not merely go stale, it makes the nest believe
    // there are TWO people where there is one. That is a different failure from
    // every ruling before this pass, which asked what a row grants or limits
    // *its own* actor: these rows are about somebody else, and the damage lands
    // on somebody else too.
    //
    // ⚠ Their error direction is CONSERVATIVE (more spam-suspicious, a higher
    // penalty), which is exactly why they waited while the grant-shaped columns
    // went first — and it is not a reason to leave them: a conservative error is
    // still wrong data, and here it is wrong data about an uninvolved third
    // party who cannot see or appeal it.
    (
        "sender_behavior",
        "target_actor",
        Succession::Move(MoveShape::Bespoke(
            "the DM recipient in a fan-out event, and the sender's whole profile is              built by counting DISTINCT values of this column across 1 h / 24 h / 7 d              windows -- so one human who succeeds between two DMs is counted twice and              inflates a THIRD party's fan-out toward the spam thresholds. The              correspondent-pair reply guard keys on it too, and that half fails the              other way: the reply-recording statement requires a matching `dm_sent` row              for the same pair, so after the recipient's ceremony their replies stop              recording at all and the sender's response rate reads colder than it is.              Both errors are conservative and both are wrong. Hand-written with the              other two counterparty columns",
        )),
    ),
    // === Surfaced 2026-08-14 by the `sender` widening (the delivery plane) ===
    //
    // Two columns that have always named an actor and were accounted for by
    // NOTHING — not by an entry, not by a reference, not by an exclusion —
    // because the vocabulary had no word for the commonest role noun in a
    // delivery schema. Both name the counterparty who reached the account: the
    // author of a notification's triggering event, and the stranger behind a
    // held knock. Their recipient columns are ruled `Move` in `ACTOR_TABLES` by
    // the same pass that surfaced these.
    //
    // Declared `Unruled` here rather than ruled, following the `…_by` widening
    // above: fixing the gate that could not see a column is a separate act from
    // ruling what it surfaced, and these are the question (what happens to
    // a COUNTERPARTY reference when that counterparty succeeds) rather than this
    // plane's. `the_unruled_reference_backlog_only_shrinks` is what keeps them
    // visible in the meantime.
    (
        "knocks",
        "sender_id",
        Succession::Move(MoveShape::Bespoke(
            "the stranger behind a held knock. The recipient half of this table already              moves, so leaving the sender half hands the successor a queue of requests              whose askers cannot be resolved, accepted or blocked as the people they              are -- and a knock accepted against a retired id mints reach for an              identity that is refused everywhere. Hand-written with the other two              counterparty columns",
        )),
    ),
    (
        "notifications",
        "sender_id",
        Succession::Move(MoveShape::Bespoke(
            "the author of the triggering event, and it is part of the insert's own              dedup key `(actor_id, notif_type, sender_id, content_id)` -- so a sender              who succeeds re-notifies the recipient for an event they were already told              about, the ceremony's visible effect on somebody else's notification page              being duplicates. The recipient half of this table already moves.              Hand-written with the other two counterparty columns",
        )),
    ),
    // === Surfaced 2026-08-13 by the VOCABULARY widening, RULED 2026-08-13 ===
    //
    // Six columns that have always named an actor and were seen by no gate,
    // because the completeness walk searched for `actor`/`author`/`owner` and
    // these say *who did it* instead (`…_by`). They were declared `Unruled`
    // first and ruled in a second pass, deliberately: fixing the gate that
    // should have found a column and *ruling* that column are different acts,
    // and shipping both together would grade a mistake in either as a mistake
    // in both. (One of the six, `admin_actor_ids.added_by`, was dark and left
    // with schema 100's dead-column step; five remain.)
    //
    // **The three `Stay`s are not one ruling repeated.** Each was checked for
    // what *resolves* through the pointer before being read as attribution —
    // the habit `payment_claim_codes.redeemed_by` paid for, where the obvious
    // "attribution, kept for audit" reading was wrong because `apply_refund`
    // resolved through it. Two of the five turned out not to be attribution at
    // all, and they are the two that move.
    (
        "invite_requests",
        "decided_by",
        // ⚠ Rules the DECIDER's column only; `invite_requests.actor_id`, the
        // requester, is still `Unruled` in `ACTOR_TABLES`.
        Succession::Stay(
            "history/attribution, and only on rows that were DENIED — an \
             approve consumes its row (`delete_invite_request`), so this column \
             exists only where the answer was no. It is surfaced read-only to \
             admin surfaces (`admin_ws_handlers`'s summary, `invite_handlers`'s \
             hex rendering) and nothing resolves through it: re-deciding is \
             gated on `status = 'pending'` and never on who decided last, and \
             denied rows are age-purged anyway. Re-pointing would re-attribute \
             a denial a seed thief may have issued",
        ),
    ),
    (
        "hosted_plugins",
        "installed_by",
        // ⚠ Rules the INSTALLER's column only; the row belongs to no account
        // -- an install is the nest's act (`third-party.md` § The principal
        // model → *Hosted principals*), which is why `hosted_plugins` is in no
        // `ACTOR_TABLES` entry.
        Succession::Stay(
            "history/attribution, `invite_requests.decided_by`'s shape: the admin \
             the install card was assigned to, surfaced read-only by \
             `fauna.plugins.list` and read by nothing else. Nothing resolves \
             through it -- uninstall and list are gated on the caller's permission \
             at the door, never on who installed, and the runner starts a plugin \
             from its row whoever that was. Re-pointing would credit an install a \
             seed thief may have approved to the identity recovering from them. A \
             DELETED installer's id likewise stays: the plugin is the nest's and \
             outlives the admin who approved it, and the row leaves only with \
             uninstall",
        ),
    ),
    (
        "room_generations",
        "minted_by",
        // Added 2026-09-10, catching up the sealing plane (schema 57, landed
        // 2026-09-09 without a ruling here). ⚠ Rules the MINTER's column
        // only; the row itself belongs to the ROOM, not to any actor, which
        // is why `room_generations` is in no `ACTOR_TABLES` entry.
        Succession::Stay(
            "history/attribution, and here the pointer is not merely unread \
             but CONTRADICTABLE. The row's `mint_blob` is the minter's own \
             signed `GroupGenerationMintRecord`, and the nest verified that \
             signature against this very actor before storing it \
             (`conversations_handlers`'s mint admission: `core.minter` is \
             bound to the authenticated caller and \
             `verify_group_mint_minter_sig` checks it). Re-pointing the column \
             would leave the stored attribution naming one actor while the \
             signature inside the blob names another -- a row that reads as \
             forged to anyone who checks it, which is worse than a stale \
             pointer. Nothing resolves through it either: key authority is \
             read live off the floor roster's `role` at every mint, never off \
             who minted last. And a generation a SEED THIEF minted is exactly \
             the act worth leaving legible to the identity recovering from \
             them -- the same reasoning as `pending_actions.cancelled_by` \
             below",
        ),
    ),
    (
        "room_invites",
        "inviter_id",
        // Added 2026-09-21 with the table's `ACTOR_TABLES` entry. ⚠ Rules the
        // INVITER's column only; the row belongs to `invitee_id`, which moves.
        //
        // Declared HERE rather than in the census walk's `EXCLUDED`, on
        // purpose. `room_members.invited_by` is an exclusion, and this list's
        // own charter settles the difference: the column names an actor the
        // row does not belong to, so a succession must rule it, and `EXCLUDED`
        // carries no verdict at all. (The retired plane's
        // `group_members.invited_by` was the `Stay` precedent followed.)
        Succession::Stay(
            "history/attribution, and CONTRADICTABLE if re-pointed, \
             `room_generations.minted_by`'s reason: the row's `signed_invite` is \
             this actor's own signed act, and the invite door bound its signer to \
             the authenticated caller before storing either, so a re-pointed \
             column would name one inviter while the signature beside it names \
             another. It HAS a reader — \
             `accept_room_invite` copies it into the new seat's \
             `room_members.invited_by` — but that copy is attribution too, and \
             nothing resolves through it: the inviter's right to invite was \
             judged once, at the invite door, against its floor role then, and \
             the accept door re-checks nothing about it. Re-pointing would \
             credit an invitation a seed thief may have issued to the identity \
             recovering from them. A DELETED inviter's id likewise stays, on the \
             row and under its own signature: the invitation is the invitee's \
             record of who asked, the `room_members.invited_by` residue one step \
             earlier",
        ),
    ),
    (
        "content_labels",
        "classifier_id",
        // Added 2026-09-21. ⚠ `content_labels` has NO `ACTOR_TABLES` entry and
        // that is the ruling, not an omission: the row belongs to the CONTENT
        // it labels (`room_generations`' shape — a row that is the room's, with
        // a person only in a reference column). It is addressed by content,
        // deleted by content (`db/posts.rs::delete_post_projection`,
        // `db/room_labels.rs`), and the only door that puts a PERSON in it
        // admits the content's own author or a grant holder that is never a
        // `users` row (`label_handlers::authorize_attach`).
        //
        // DELETION therefore needs no verdict of its own, and a `Purge` here
        // would be wrong as well as redundant. An author's labels leave with
        // their posts, through the federation-aware retraction `content.author`
        // is `Retain` for. Where that retraction leaves a post standing, the
        // post itself still names its author — the label beside it discloses
        // nothing more — and a self-label (`nsfw`) is a safety property of
        // content that is still being served: it must live exactly as long as
        // the content does, never a moment less. Residue, declared: a row written with a ZERO
        // `scanner_id` (the channel behavioral-anomaly label writes zeros;
        // `db::feeds::ATTRIBUTED_LABEL` is the standing guard against such
        // unattributed rows), and whatever `classifier_id` it holds leaves
        // only with its content.
        //
        // The four writers, since the verdict turns on them: the attach door
        // (the caller in BOTH columns); a community room's labeler pass
        // (`db/room_labels.rs::record_bus` — the labeler's artifact key here,
        // the NEST's own key in `scanner_id`); and the channel behavioral-
        // anomaly label (`conversations_handlers.rs` — zeros in both). Only
        // the first names an account, so only its rows match a ceremony.
        Succession::Move(MoveShape::Bespoke(
            "the upsert key a label's writer revises it through, and a label has no \
             detach door: left on the retired identity the row is one the successor can \
             never reach, their revision lands BESIDE it, and every reader takes the \
             highest confidence of the two — so a self-label, or one a seed thief \
             attached in the author's name, could be raised for ever and never lowered. \
             The post it sits on has already moved (`content.author`), so after the move \
             the label again names its content's author. Nothing is signed (`signature` \
             is empty on every writer), so there is nothing to contradict. Hand-written \
             with `scanner_id` in ONE statement keyed on this column \
             (`db/successions.rs::rule_the_label_writer_columns`), and a collision \
             SUPERSEDES rather than parks: a successor who re-labelled holds the same \
             writer's later verdict. Witnessed by \
             successions::tests::an_author_who_succeeds_still_revises_their_own_label",
        )),
    ),
    (
        "content_labels",
        "scanner_id",
        // ⚠ A person on ONE writer of three — see `classifier_id` directly
        // above. Claimed by the census by name all the same: the word is
        // specific to this plane, and a second `scanner_*` column arriving
        // unruled is exactly what the walk is for.
        Succession::Move(MoveShape::Bespoke(
            "the writing POSITION, which on a hand-attached label is the same caller as \
             `classifier_id` and moves beside it in that column's one statement — never \
             on its own key, so a room labeler's row (the nest's own Ed25519 key here, and \
             a nest identity is never a person) and the anomaly label's zeros are matched \
             by nothing. The read side needs only that it is non-zero \
             (`db::feeds::ATTRIBUTED_LABEL`), which a move preserves",
        )),
    ),
    (
        "channel_commit_watermark",
        "last_commit_sender",
        // Added 2026-09-11, catching up the floor roster's authorship column
        // (schema 64, landed the same day without a ruling here). ⚠ Rules the SENDER's column only; the row
        // itself belongs to the CHANNEL (`channel_id` is the primary key),
        // which is why the table is in no `ACTOR_TABLES` entry.
        //
        // **Not the neutral `Stay` its siblings are — this pointer IS resolved
        // through, and it stays anyway because the resolution fails CLOSED.**
        // The one reader is the roster-report body
        // (`conversations_handlers.rs`, the commit-order guard's authorship
        // half, `conversation-rooms.md` § The floor roster): a report naming
        // the newest commit's position is refused unless the reporter EQUALS
        // the stored sender. Equality with an identity the nest no longer
        // admits — the retired key is answered `superseded` at every
        // handshake and the ceremony tears down its sockets
        // (`identity-succession.md` § Implementation status today, slice 3)
        // — matches nobody, so a stale pointer narrows the newest position to
        // NO ONE until the next commit rewrites the pair. That is the guard's
        // closed state, and the window is bounded by the succession itself:
        // propagation into every group the owner holds is two membership
        // commits, add-successor and remove-old (`succession-propagation.md`
        // § Propagation), each of which lands above the mark and stamps its
        // own observed sender.
        //
        // `Clear` is the wrong verdict precisely BECAUSE the column admits a
        // null: an unknown sender is the guard's declared fail-OPEN residue
        // (a row written before schema 64), so nulling the pointer would not
        // land the row in a handled state — it would reopen the newest
        // position to every live member, the defect the column
        // was landed to close, on exactly the commit a succession is most
        // sensitive about (in the theft case, plausibly the thief's own).
        // `Move` is wrong twice over: the successor did not send that commit
        // and could not have — an MLS credential IS the actor id, so the
        // successor joins as a new leaf — and until its add-successor commit
        // lands it is not a live member of the stored floor either, so the
        // door's membership gate refuses it before authorship is consulted;
        // the move would buy nothing and re-attribute a possibly thief-made
        // commit to the identity recovering from it. Underneath both: the
        // column is an OBSERVATION the nest made at the append, under the
        // conv seq lock (`set_channel_commit_watermark`), and a re-pointed
        // observation is a state no append ever produced.
        //
        // No leg, by construction — and pinned anyway: `successions.rs`'s
        // `the_newest_commits_sender_stays_on_the_retired_identity` holds
        // the ceremony to the untouched pair, so a later `Clear`/`Move` leg
        // reds a test rather
        // than silently widening the guard.
        Succession::Stay(
            "history/attribution that the guard RESOLVES THROUGH, and stays \
             because the resolution fails closed: the roster-report door admits \
             a report at the newest commit's position only from a reporter EQUAL \
             to the stored sender, and a retired identity — refused `superseded` \
             at every handshake — equals nobody, so the position is claimable by \
             no one until the next commit rewrites the pair, which the \
             succession's own two propagation commits per group do. Clearing \
             would reopen that position to every live member (the guard's \
             declared fail-open residue for an unknown sender — the defect the \
             column closes, on the commit a thief most plausibly made); moving \
             would attribute to the successor a commit it did not send and, as \
             a new leaf, could not have. The column records what the nest \
             OBSERVED at the append, and an observation is never re-pointed",
        ),
    ),
    (
        "conv_record_authors",
        "author",
        // **The fourth conv-plane attestation column to arrive without a
        // ruling** (schema 70), after `rooms.owner_id` /
        // `room_members.principal_id`, `room_generations.minted_by` and
        // `channel_commit_watermark.last_commit_sender` — and it lands on the
        // same verdict as the last of those by the same stated discriminator,
        // not by analogy to the table name.
        //
        // The row belongs to a RECORD, keyed `(channel_id, seq)`; this column
        // is the actor the nest authenticated as that record's poster
        // (`insert_conv_record_author`, written in the mirror row's own
        // transaction). Its one reader binds an inbound scheduling mutation to
        // the event's organizer by EQUALITY with this stored author
        // (`caldav-server.md` § Scheduling & invitations → *Who may mutate an
        // existing event over the inbound rail*; the doc's 2026-09-20 amendment
        // is explicit that the iMIP `ORGANIZER` line authenticates nothing and
        // that this home-nest attestation is what does).
        //
        // `Stay` on the succession-repoint axis' own transferable rule
        // (`succession-repoint-axis.md` § The declared re-point axis, the floor
        // roster's authorship entry): *resolves through* is not the `Clear`
        // trigger — the discriminator is whether leaving the pointer is
        // UNSAFE, and a comparand that can only ever match a dead identity is
        // the safe direction. A retired identity is answered `superseded` at
        // every handshake, so a stale author equals nobody and the mutation is
        // refused — which is exactly what the client already reads a record
        // with NO author as (the schema-70 note: "mutation refused, never
        // applied"), so the fail-closed state is one the rail was built to
        // handle rather than a new one. `Move` would credit the successor with
        // posting a record it did not post, and would hand it mutation rights
        // over events the predecessor organized on the strength of an
        // attestation no append ever made. Underneath both, as for the
        // watermark: this is an OBSERVATION the nest made at the append, and a
        // re-pointed observation is a state no append ever produced.
        Succession::Stay(
            "the home-nest attestation of who posted a conv record, on a row that \
             belongs to the RECORD and not to any actor. Its only reader compares \
             it for EQUALITY with the caller claiming to be the event's organizer, \
             and a retired identity equals nobody — so a stale pointer refuses the \
             mutation, the same fail-closed state a record with no author already \
             produces. Moving it would credit the successor with an append it never \
             made and grant mutation rights off an attestation no append produced; \
             the column records what the nest OBSERVED at the append, and an \
             observation is never re-pointed",
        ),
    ),
    (
        "pending_actions",
        "cancelled_by",
        // ⚠ Rules the CANCELLER's column only; `pending_actions.actor_id`, the
        // creator, is still `Unruled` in `ACTOR_TABLES`.
        Succession::Stay(
            "history/attribution on a TERMINAL row. `cancel_pending_action` \
             computes authorization entirely from the CALLER — creator, target, \
             or `is_admin(caller)` — and writes this column only as the record \
             of who cancelled, beside `status = 'cancelled'`; no statement ever \
             reads it back to authorize anything, and a cancelled action never \
             executes, so no future authority flows from the row at all. \
             Re-pointing would re-attribute a cancellation a seed thief may \
             have performed, and a thief's cancellation of the owner's own \
             scheduled action is precisely the act worth leaving legible to the \
             identity it was performed against",
        ),
    ),
    (
        "outbound_mail_queue",
        "submit_actor_id",
        // The AUTHENTICATED SUBMITTER of a queued outbound message — added
        // 2026-08-23 and stamped only by `fauna.email.send`
        // (`email_handlers.rs`), NULL on every system-generated insert (bounce,
        // retry, TLSRPT report, bridge submission). It exists for exactly one
        // job: the per-hour outbound cap counts rows by it, because the cap had
        // been counting `original_sender` — the caller's own `From:` header,
        // which an off-domain sender may set freely, so a fresh string was a
        // fresh counter and any authenticated actor was an unmetered relay.
        //
        // It is a REFERENCE rather than an ownership column, and that is
        // structural rather than a judgement call: `ACTOR_TABLES` is
        // one-entry-per-table by construction (161 entries, 161 distinct
        // tables) because the deletion and export executors read it, and this
        // table's ownership slot is already `forward_actor_id`. A second
        // `ACTOR_TABLES` entry would run two legs over one table.
        //
        // ⚠ **`Stay` is the deliberately conservative verdict, and the open
        // question is named here rather than answered.** Two verdicts are
        // defensible and they pull opposite ways. `Move` has a real precedent —
        // `bridge_submission_quota` is `Move(Plain)`, "the limits-move rule
        // verbatim", and the sibling column on this very table rules that "the
        // account's recent forward-rate window travels with it" — so a
        // successor would inherit the consumed budget, which is also the
        // conservative direction against using a ceremony to reset a cap.
        // `Stay` is what this entry declares because the rows are true
        // attribution: the retired identity really did submit those messages,
        // nothing user-facing reads or exports the column, and
        // `the_unruled_backlog_only_shrinks` states the tie-breaker for this
        // whole axis in as many words — "a wrong `Move` is a security
        // regression while a wrong `Stay` is the status quo". `Stay` is also
        // byte-for-byte today's behaviour, so this entry accounts for the
        // column without moving anything.
        //
        // The residual it leaves is bounded and worth stating plainly: the cap
        // counts a ONE-HOUR window over a transient spool, so a succession
        // hands the successor at most one hour of un-charged outbound budget.
        // Whether that should instead travel is the outbound-metering plane's
        // ruling to make, and `succession-aftermath.md` § Re-key scope does not
        // rule it today — which is why it is not ruled here either.
        Succession::Stay(
            "the authenticated submitter of a queued outbound message, stamped only by              `fauna.email.send` and read only as the per-hour outbound cap's counter key.              True attribution — the retired identity really did submit these rows — and              nothing user-facing reads or exports it, so the pointer is not stale in the              way a routing or authority pointer would be. Deliberately NOT `Move`, though              `bridge_submission_quota`'s limits-move rule and this table's own              `forward_actor_id` (whose forward-rate window travels) both argue for it: the              outbound-metering plane is unruled in succession-aftermath.md § Re-key scope,              and a wrong Move is a security regression where a wrong Stay is the status              quo. The bounded residual is that a successor starts with at most one hour of              un-charged outbound budget on a transient spool",
        ),
    ),
    (
        "abuse_reports",
        "subject_actor",
        // The REPORTED author (hex TEXT) — the report is about them, never
        // theirs: the row belongs to the reporter (`reporter_actor`), or on a
        // forwarded copy to no local actor at all.
        Succession::Stay(
            "evidence and attribution: the report names the identity whose content \
             or conduct it describes, and a report compels nothing (moderation.md \
             § Anti-abuse posture bound 1), so nothing resolves authority through \
             the column — the admin queue displays it and the triad routes by it at \
             submit time only. Re-pointing would attribute to the successor what \
             the retired identity (perhaps a seed thief) was reported for",
        ),
    ),
    (
        "abuse_reports",
        "resolved_by",
        // The ADMIN who recorded the outcome — the `invite_requests.decided_by`
        // shape: an attribution of a decision, on someone else's row.
        Succession::Stay(
            "history/attribution: the admin who recorded a resolution, written once \
             by `resolve_abuse_report` and read by nothing — the queue and the \
             ledger never show it (the reporter is told the outcome only), and \
             resolving is gated on `status = 'open'`, never on who resolved. \
             Re-pointing would re-attribute a decision a seed thief may have made",
        ),
    ),
    (
        "sync_signer_certs",
        "cert_actor_id",
        // The identity the stored `DeviceAuthorization` NAMES — a mirror of
        // the actor id inside the root-signed `cert` bytes, written at ingest
        // from the cert that verified. The row belongs to `actor_id`
        // (registered: Purge, `Move(Plain)`, exported), which follows the
        // `sync_changes` rows the cert verifies; this column is the third
        // part of the key that keeps a predecessor's moved cert beside a
        // successor's for the same device key
        // (`writer-signed-change-records.md` ruling (8)(i)).
        Succession::Stay(
            "signed attribution: it mirrors the actor id inside the root-signed cert, \
             whose bytes no ceremony rewrites, and it is the key part that keeps a \
             predecessor's moved cert from colliding with the successor's own cert for \
             the same device key (`writer-signed-change-records.md` ruling (8)(i)). \
             Re-pointing it would collapse the two onto one key and drop the cert every \
             row the key signed as the predecessor verifies under",
        ),
    ),
];

impl CacheDb {
    /// Deletes every row in every [`Policy::Purge`] table of [`ACTOR_TABLES`]
    /// for `actor_id`. Table and column names are compile-time constants from
    /// this module, never caller input, so the interpolated SQL carries no
    /// injection surface.
    ///
    /// Idempotent and safe to call after the existing dedicated helpers
    /// (`delete_inbox_for_actor`, `delete_sync_devices_for_actor`, …) — the
    /// handful of tables they already cover are included here too (see their
    /// per-row comments) so the registry stays the complete, single source of
    /// truth `account-data-plane.md` asks for; re-deleting an already-empty
    /// set is a zero-row no-op.
    ///
    /// Skips a table this connection's schema does not have, rather than
    /// erroring the whole account deletion out from under the caller: the
    /// registry includes tables that exist only under the `nostr`/`bluesky`/
    /// `activitypub` features (none in `bins/fauna-nest`'s default
    /// `["store-safe", "payments", "zaps"]` — e.g. the Windows `FaunaNest`
    /// service builds `default-features = false`), so an unconditional
    /// `DELETE FROM ap_accounts` would abort every account deletion on a
    /// nest built without those features, before `delete_user` ever runs.
    ///
    /// Before any of that it withdraws the deployment spam baseline when the
    /// actor stands as a contributor to it — the one registry-less table an
    /// account deletion answers for (see the comment at the call) — and sweeps
    /// the actor's forward queue by the predicate `outbox`'s
    /// [`Policy::Partial`] entry states, and the actor's pending subscribe
    /// requests as a READER, a second person the author-keyed
    /// `subscribe_requests` entry cannot reach — and withdraws the actor's
    /// open abuse reports by `abuse_reports`' [`Policy::Partial`] leg, a
    /// deletion of words plus a queued propagation rather than of rows: the
    /// three legs here a plain `Purge` cannot express.
    ///
    /// All of that runs for `actor_id` AND for every local predecessor this
    /// nest's successions retired into it, back to the chain's first identity
    /// ([`super::successions::local_predecessors`]; `account-data-plane.md`
    /// § Nest-side requirements item 1, *Deletion reaches the account's
    /// predecessors*). A succession leaves every `Stay` row under the retired
    /// id, and a predecessor is the same person under an earlier key, so the
    /// same verdicts apply to it: its `Purge` rows go, its `Partial` rows take
    /// their legs, its `Retain` rows keep their reasons (`actor_successions`
    /// among them, which is why a retried deletion walks the same chain). The
    /// walk lives here, not in `pending_actions::finalize_user_deletion`, so
    /// every caller of the purge gets it; the post retraction that runs there
    /// first needs no walk, because `content.author` is `Move` at succession
    /// and a retired id authors nothing afterwards. The predecessors' `users`
    /// rows are `delete_user`'s, not this walk's — it deletes the whole
    /// chain's rows in one transaction, and it must run AFTER this walk,
    /// whose locality test those rows are (`local_predecessors`).
    ///
    /// Returns the total row count deleted, for the caller's own audit log.
    pub async fn purge_orphaned_actor_rows(&self, actor_id: &[u8; 32]) -> Result<u64> {
        let conn = self.conn.lock().await;
        let mut total = 0u64;
        for id in std::iter::once(*actor_id)
            .chain(super::successions::local_predecessors(&conn, actor_id)?)
        {
            total += Self::purge_one_identitys_rows(&conn, &id)?;
        }
        Ok(total)
    }

    /// [`Self::purge_orphaned_actor_rows`]'s walk for ONE id, under the
    /// caller's lock.
    fn purge_one_identitys_rows(conn: &rusqlite::Connection, actor_id: &[u8; 32]) -> Result<u64> {
        let actor = actor_id.to_vec();
        // The bridge families spell the same id as lowercase hex TEXT — one
        // encoding per registry entry, not per call site (`ActorKey`).
        let actor_hex = hex::encode(actor_id);
        // The one thing this walk does to a table that is NOT in the registry.
        // `spam_baseline` is a singleton with no actor column, so no entry can
        // reach it — yet it may hold this actor's summed counts, and the
        // witnesses that it does — the inclusion record, and for a baseline
        // older than it `spam_models` + `spam_preferences` — are rows the loop
        // below is about to delete. So the withdrawal comes first,
        // under the same lock (`mail-spam.md` § Cold start Path 2 → *A
        // contributor's departure withdraws the baseline*). A crash between the
        // two leaves a withdrawn baseline and a retried deletion.
        // The same call counts the purged inclusion row toward the next
        // publish's delta floor before the registry loop deletes it.
        super::moderation::withdraw_spam_baseline_if_contributor(
            conn,
            actor_id,
            super::spam_baseline::BaselineDeparture::AccountDeleted,
        )
        .context("withdraw the spam baseline a departing contributor is summed into")?;
        // The PREDICATED deletions, which the registry cannot express
        // (`ActorTable` has no row filter). The forward queue is
        // `Policy::Partial` because a queued tombstone must still be sent, and
        // everything else the author has queued goes here (the `outbox`
        // entry's reason).
        let mut total: u64 = CacheDb::outbox_purge_for_deleted_author(conn, actor_id)
            .context("purge the deleted author's queued forwards")?
            as u64;
        // The subscribe queue's second person: the registry reaches the row by
        // its AUTHOR, and a deleted READER's pending `subscribe` requests go
        // here (the `subscribe_requests.subscriber_id` reference's comment).
        total += CacheDb::subscribe_requests_purge_for_deleted_subscriber(conn, actor_id)
            .context("purge the deleted reader's pending subscribe requests")?
            as u64;
        // The reporter's open abuse reports: withdrawn as the reporter would
        // have withdrawn them, the home nest's copy included through the
        // `withdraw` queued here (`abuse_reports`' `Policy::Partial` reason).
        // Not counted: the rows stay, their words go.
        CacheDb::withdraw_abuse_reports_for_deleted_reporter(
            conn,
            actor_id,
            super::now_epoch_secs(),
        )
        .context("withdraw the deleted reporter's open abuse reports")?;
        for entry in ACTOR_TABLES {
            if !matches!(entry.policy, Policy::Purge) {
                continue;
            }
            if !table_exists(conn, entry.table)? {
                continue;
            }
            let sql = format!("DELETE FROM {} WHERE {} = ?1", entry.table, entry.column);
            // Bind the spelling THIS table uses ([`ActorKey`]). A blob bound
            // against a hex-TEXT column is not an error — it matches nothing,
            // and the row survives a deletion that reported success.
            let deleted = match entry.key {
                ActorKey::Blob => conn.execute(&sql, rusqlite::params![actor]),
                ActorKey::Hex => conn.execute(&sql, rusqlite::params![actor_hex]),
            }
            .with_context(|| format!("purge orphaned rows from {}", entry.table))?;
            total += deleted as u64;
        }
        Ok(total)
    }

    /// The per-actor export's registry-driven half: every
    /// [`Export::Verbatim`]/[`Export::Redacted`] table's rows for `actor_id`,
    /// plus the coverage declaration rule 4 requires
    /// (`account-data-plane.md` § Nest-side requirements item 1).
    ///
    /// The counterpart of [`CacheDb::purge_orphaned_actor_rows`] on the fourth
    /// axis, and driven the same way — [`export_emit_legs`] is the work list, so
    /// a table joins the export by declaring a verdict and by nothing else.
    /// Nothing here infers a disposition from the schema or the other axes;
    /// that inference is what the ruling forbids.
    ///
    /// One connection lock for the whole walk, like the purge: the export is a
    /// point-in-time archive, and a walk interleaved with writes could emit a
    /// row set no instant ever held.
    pub async fn gather_actor_export(&self, actor_id: &[u8; 32]) -> Result<ActorExportSet> {
        let conn = self.conn.lock().await;
        gather_export_set(&conn, ACTOR_TABLES, actor_id)
    }

    /// The per-actor export's **conversations** domain: the exporter's conv
    /// records, resolved through their channel membership. See
    /// [`gather_conv_records`] for why this is a door of its own rather than a
    /// registry entry, and what it does and does not carry.
    ///
    /// One connection lock over both the membership read and the record read,
    /// for the same reason [`CacheDb::gather_actor_export`] holds one for the
    /// whole walk: an archive interleaved with writes could name a channel set
    /// no instant ever held.
    pub async fn gather_actor_conv_records(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<TableExport>> {
        let conn = self.conn.lock().await;
        gather_conv_records(&conn, actor_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The registry's own no-duplicates, no-`users` invariant. `users` is the
    /// account row `delete_user` deletes directly with its own statement and
    /// boolean return value — listing it here too would read as if this
    /// module were responsible for it.
    #[test]
    fn no_duplicate_tables_and_users_is_absent() {
        let mut seen = HashSet::new();
        for entry in ACTOR_TABLES {
            assert!(
                seen.insert(entry.table),
                "table {} listed more than once in ACTOR_TABLES",
                entry.table
            );
            assert_ne!(
                entry.table, "users",
                "users is delete_user's own row, not a purge-registry entry"
            );
        }
    }

    /// Tables that exist only under a non-default bridge feature (nostr,
    /// bluesky, activitypub — none in `bins/fauna-nest`'s default
    /// `["store-safe", "payments", "zaps"]`). Their `CREATE TABLE` was read
    /// straight from source (`nostr/db.rs`, `fauna-bridge-atproto`,
    /// `fauna-bridge-activitypub`), same as every other entry — this test
    /// just cannot stand them up without those features compiled in. Verify
    /// this list against `db/actor_tables.rs`'s own source if it ever grows.
    const FEATURE_GATED_ELSEWHERE: &[&str] = &[
        "ap_accounts",
        "ap_follows",
        "ap_post_map",
        "bluesky_accounts",
        "bluesky_interactions",
        "bluesky_saved_feeds",
        "nostr_accounts",
        "nostr_bunker_apps",
        "nostr_bunker_signers",
        "nostr_federation_cursors",
        "nostr_follows",
        "nostr_oracle_clients",
        "nostr_oracle_ops",
        "nostr_zap_signers",
    ];

    /// Every table+column in [`ACTOR_TABLES`] must exist in the real schema —
    /// this is what caught `actor_plaintext_msek` during authoring: its
    /// `CREATE TABLE` is real, but a later migration (`I1 (a)`, Phase-3 S4b)
    /// drops it again, so the mechanically-extracted candidate was a stale
    /// legacy table, not a current one. A registry entry naming a table or
    /// column that does not exist would make `purge_orphaned_actor_rows` fail
    /// every deletion outright (rusqlite errors on an unknown table/column),
    /// so this is load-bearing, not a nice-to-have.
    #[test]
    fn every_table_and_column_exists_in_the_real_schema() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();
        let mut missing_table = Vec::new();
        let mut missing_col = Vec::new();
        for entry in ACTOR_TABLES {
            if FEATURE_GATED_ELSEWHERE.contains(&entry.table) {
                continue;
            }
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![entry.table],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if !exists {
                missing_table.push(entry.table);
                continue;
            }
            let pragma = format!("PRAGMA table_info({})", entry.table);
            let mut stmt = conn.prepare(&pragma).unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            if !cols.iter().any(|c| c == entry.column) {
                missing_col.push((entry.table, entry.column));
            }
        }
        assert!(
            missing_table.is_empty() && missing_col.is_empty(),
            "missing tables: {missing_table:?}\nmissing columns: {missing_col:?}"
        );
    }

    /// Every registry entry's declared [`ActorKey`] must match the actor
    /// column's own **declared type affinity** — the check whose absence let
    /// `purge_orphaned_actor_rows` bind a blob against fifteen hex-`TEXT`
    /// columns and delete nothing for years of schema growth.
    ///
    /// This is the encoding half of `every_table_and_column_exists_in_the_real_schema`,
    /// and it fails in the same direction: an entry that *names* a real table
    /// and column can still be a silent no-op, and SQLite will never say so —
    /// a blob operand simply never equals a `TEXT` value. Same
    /// `FEATURE_GATED_ELSEWHERE` limit applies (this connection cannot stand
    /// those tables up), which is exactly why the companion test below asserts
    /// something about *them* rather than passing over them in silence.
    #[test]
    fn every_declared_key_encoding_matches_the_columns_affinity() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();
        let mut wrong = Vec::new();
        for entry in ACTOR_TABLES {
            if FEATURE_GATED_ELSEWHERE.contains(&entry.table) {
                continue;
            }
            let pragma = format!("PRAGMA table_info({})", entry.table);
            let Ok(mut stmt) = conn.prepare(&pragma) else {
                continue; // absence is `every_table_and_column_exists`'s failure
            };
            let declared: Option<String> = stmt
                .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .find(|(name, _)| name == entry.column)
                .map(|(_, ty)| ty.to_ascii_uppercase());
            let Some(declared) = declared else {
                continue; // ditto
            };
            // SQLite affinity rules, narrowed to the two we bind: a declared
            // type containing "BLOB" (or empty) is BLOB affinity; one
            // containing "CHAR"/"CLOB"/"TEXT" is TEXT affinity.
            let is_text =
                declared.contains("CHAR") || declared.contains("CLOB") || declared.contains("TEXT");
            let is_blob = declared.contains("BLOB") || declared.is_empty();
            let ok = match entry.key {
                ActorKey::Hex => is_text,
                ActorKey::Blob => is_blob,
            };
            if !ok {
                wrong.push((entry.table, entry.column, entry.key, declared));
            }
        }
        assert!(
            wrong.is_empty(),
            "these entries declare an ActorKey the column's own type cannot match — \
             the delete would bind the wrong spelling and silently match no rows: {wrong:?}"
        );
    }

    /// The roots a column name is matched against to decide whether it *looks
    /// like* it names a person — the vocabulary the completeness walk below
    /// searches with.
    ///
    /// **Widened 2026-08-13, and the widening is the fix for a self-fulfilling
    /// justification rather than a bigger guess.** The list was
    /// `actor`/`author`/`owner`, defended by the observation that "every real
    /// entry in ACTOR_TABLES already contains one of these three roots" — which
    /// proves nothing, because the registry had been built *by* that heuristic,
    /// and the observation was in any case **false**: six registered columns
    /// matched none of the three, one of them the `recipient_id` the old comment
    /// offered as an example. [`the_walks_vocabulary_can_name_every_registered_column`]
    /// now asserts that claim instead of asserting itself, so the vocabulary can
    /// never again be justified by the registry it filtered.
    ///
    /// Three additions come from that assertion (`recipient`, `reporter`, and
    /// the agentive `_by` suffix below), and one from the plane that exposed the
    /// blindness: `subscriber`, the reader half of the entitlement plane, whose
    /// three columns were seen by nothing at all until they were ruled.
    ///
    /// **`sender` joined 2026-08-14 and is the same lesson a third time.** The
    /// delivery plane's two counterparty columns — `knocks.sender_id` and
    /// `notifications.sender_id` — have always named an actor and were accounted
    /// for by nothing at all: no entry, no reference, no exclusion. `recipient`
    /// had been added the pass before while its own antonym had not, which is the
    /// tell — a vocabulary assembled one finding at a time will keep having the
    /// word for the half someone happened to look at. Note the asymmetry it
    /// creates and that [`VOCABULARY_BLIND_SPOTS`] absorbs: `sender` is the one
    /// root here that is also an ordinary MAIL noun, so it surfaces a handful of
    /// domain/address/hash columns that name no person, each excluded by name
    /// rather than by narrowing the root back down — narrowing is precisely the
    /// move that fails silently.
    const ACTOR_SHAPED_ROOTS: &[&str] = &[
        "actor",
        // Added 2026-09-21 with `membership_tiers.admin_id` -- a raw actor id
        // that no root could see and no walk had opened, found only when the
        // declared-TYPE walk read the column's writer. `admin` is a role noun
        // exactly like `owner`; its false positive (a quota-tier NAME) is
        // excluded by name below, which is
        // the move this list prefers to narrowing.
        "admin",
        "author",
        // Added 2026-09-21 with `content_labels`' two writer columns — the
        // `room_invites` finding's twin from the same walk: person ids (the
        // attach door's caller) under a word no root reached, on a table the
        // census therefore never opened. Pulls in one non-person,
        // `content_labels.classifier_version`, excluded by name below.
        // (`scanner` is further down, in its alphabetical place.)
        "classifier",
        // Added 2026-09-21 with `room_invites`, as a PAIR — the `recipient` /
        // `sender` lesson applied on the day rather than a pass late. The
        // table named its two people by these words and nothing else, so it
        // sat in no registry and account deletion never reached it; the
        // declared-TYPE walk found the ids only because they happen to be
        // `BLOB`s. Checked across the schema before widening: the substring
        // pulls in exactly one non-person, `room_invites.invitee_node_url`,
        // excluded by name below.
        "invitee",
        "inviter",
        // Added 2026-09-21 with `namespace_entries.namespace` -- the publishing
        // actor's raw id, which the census had excused as *no actor-identifying
        // column at all* on the strength of the name alone. Checked across the
        // schema before widening: it is the ONLY column containing the word,
        // so the root has no false positive to exclude.
        "namespace",
        "owner",
        // Added 2026-09-21 with `contacts.peer_id` -- a plain counterparty
        // actor id that sat DECLARED in the opaque-carrier walk because no root
        // reached it. The word is the federation plane's too, so the root
        // surfaces four non-persons, each excluded by name below (two
        // `peer_nest_id`s, a guardian policy knob, and an external-network
        // identity); `guardian_contact_requests.peer_actor_id` was already
        // seen through `actor`.
        "peer",
        // Added 2026-09-09 with the room plane's floor roster. `room_members`
        // names its person column `principal_id` — the room model's own word
        // for "a member with a key", which may be a user, the room's home nest
        // or a bridge (`conversation-rooms.md` § The room → *Principals*). No
        // existing root could see it: this is the `segment_records.scope_id`
        // blind spot's shape — a person column carried out of sight by a name
        // that does not read as a person — caught this time by registering the
        // column, which is what makes the vocabulary check red.
        "principal",
        "recipient",
        "reporter",
        // Added 2026-09-21 — see `classifier` above. No false positive today.
        "scanner",
        "sender",
        "subscriber",
    ];

    /// Does this column name read as naming a person?
    ///
    /// Two rules, because this schema names people two ways. A **role noun**
    /// ([`ACTOR_SHAPED_ROOTS`], matched as a substring so `sender_actor` and
    /// `recipient_id` both hit), or an **agentive `…_by`** — the suffix a
    /// column takes when it records *who did this*. The second rule is not
    /// speculative: `folder_channel_claims.claimed_by` cost the succession
    /// axis a hand-registration precisely because no root could see it
    /// (`succession-aftermath.md` § Re-key scope, the vocabulary blockquote),
    /// and `payment_claim_codes.redeemed_by` repeated it one plane over. A rule
    /// that has been paid for twice is not a guess.
    ///
    /// Deliberately NOT a bare `_id` suffix: `nest_id`, `blob_id`, `content_id`
    /// and their kin would drown the signal. What that leaves uncovered is
    /// stated and bounded in `VOCABULARY_BLIND_SPOTS` rather than left implicit.
    fn actor_shaped(col: &str) -> bool {
        ACTOR_SHAPED_ROOTS.iter().any(|root| col.contains(root)) || col.ends_with("_by")
    }

    /// **The vocabulary must be able to NAME every column the registry already
    /// holds — otherwise its completeness claim is circular.**
    ///
    /// The walk below finds unaccounted columns by searching for words. That
    /// makes it exactly as complete as its word list, and the old word list
    /// defended itself with "every entry already contains one of these roots" —
    /// a claim about a registry that had been assembled with the same list. This
    /// test turns that sentence into an assertion, which is the only form in
    /// which it means anything: a hand-registered column the vocabulary cannot
    /// see is proof that a *future* column of the same shape will be invisible,
    /// and the failure names it in the commit that adds it.
    ///
    /// It reds today on nothing, and it red on six columns when it was written:
    /// three `recipient_id`s, `content_reports.reporter`,
    /// `folder_channel_claims.claimed_by` (the one
    /// `succession-aftermath.md`'s vocabulary blockquote already names as the
    /// counterexample the axis surfaced) and `segment_records.scope_id`. Five
    /// were fixed by widening the vocabulary; the sixth is a blind spot no
    /// widening can close, and it is declared rather than hidden.
    #[test]
    fn the_walks_vocabulary_can_name_every_registered_column() {
        /// Registered columns no plausible vocabulary can find, each with the
        /// reason it is unfindable rather than merely unfound. Adding to this
        /// list is a deliberate act that says *"a new column shaped like this
        /// one will be invisible, and we accept that"* — which is a very
        /// different statement from silence.
        const VOCABULARY_BLIND_SPOTS: &[(&str, &str, &str)] = &[
            (
                "segment_records",
                "scope_id",
                "a RENAME erased the root: this column was `actor_id` until the \
             audience-scope generalization renamed it (migrations.rs, plan 6 T4), \
             and nothing about `scope_id` reads as a person. No widening reaches \
             it — `scope` matches capability scopes, key scopes and feed scopes \
             across the schema — so the residual is that a rename can carry a \
             registered column out of the vocabulary's sight without any gate \
             noticing. What catches THAT is `every_table_and_column_exists_in_the_real_schema`, \
             which reds when a registry entry names a column the schema no longer has",
            ),
            (
                "feature_policies",
                "subject_id",
                "a GENERIC noun: `subject` is the mail plane's word for a message's subject \
             line (`subject`, `subject_norm`, `sealed_subject`, `subject_uri`), so the \
             root would surface five non-persons to reach one. Here the subject is the \
             account a self-imposed policy binds, and only its writer says so. Found by \
             the declared-TYPE walk, which is what catches the next column of this \
             shape: a BLOB cannot arrive without saying what it holds",
            ),
            (
                "worker_replication",
                "payload_key",
                "a POLYMORPHIC key: an actor id on `inbox` rows and a content id on `post` \
             rows, under a name that describes neither. No vocabulary reaches a column \
             whose meaning is chosen by its sibling; the declared-TYPE walk is what \
             found it and what guards the next one",
            ),
        ];

        let mut unnameable: Vec<String> = ACTOR_TABLES
            .iter()
            .map(|e| (e.table, e.column))
            .chain(SUCCESSION_REFERENCES.iter().map(|(t, c, _)| (*t, *c)))
            .filter(|(_, col)| !actor_shaped(col))
            .filter(|(table, col)| {
                !VOCABULARY_BLIND_SPOTS
                    .iter()
                    .any(|(t, c, _)| t == table && c == col)
            })
            .map(|(table, col)| format!("{table}.{col}"))
            .collect();
        unnameable.sort();
        unnameable.dedup();

        assert!(
            unnameable.is_empty(),
            "these columns are REGISTERED as naming an actor, yet the completeness \
             walk's own vocabulary cannot see them — so a future column of the same \
             shape will be missed in silence. Widen ACTOR_SHAPED_ROOTS (or the \
             agentive rule) to cover them, or declare each in VOCABULARY_BLIND_SPOTS \
             with why no vocabulary can: {unnameable:?}"
        );
    }

    /// The completeness gate the module doc has claimed since authoring but
    /// that never existed: walks every table this connection can see for an
    /// actor-shaped column and fails loudly if one is absent from
    /// [`ACTOR_TABLES`] and not a reasoned exclusion. Without this, the other
    /// tests only ever check entries someone already remembered to add — a
    /// brand-new `CREATE TABLE` with its own `actor_id` column passes every
    /// one of them silently, which is exactly how the ~140-orphan state this
    /// registry fixed got built one table at a time.
    ///
    /// **Same blind spot as `every_table_and_column_exists_in_the_real_schema`,
    /// inherited rather than solved here:** `CacheDb::open_in_memory()` never
    /// stands up the three bridge families (`nostr_*`, `bluesky_*`, `ap_*`) —
    /// their `CREATE TABLE`s live in separate crates and need each bridge's own
    /// migration call (see `purge_removes_rows_from_a_hex_keyed_bridge_table`'s
    /// manual `crate::nostr::db::CREATE_TABLES_SQL`), which no single connection
    /// runs for all three at once. A new bridge table is invisible to this walk
    /// exactly as it is to its siblings; `FEATURE_GATED_ELSEWHERE`'s own comment
    /// — "verify this list against source if it ever grows" — is still the only
    /// coverage there. This walk closes the gap for the ~135 tables the main
    /// schema actually creates, which is where the ~140-orphan state lived.
    ///
    /// **Widened 2026-08-13 to see SECOND actor columns, and it had been blind
    /// to them by construction.** The walk used to `continue` on any table
    /// already in [`ACTOR_TABLES`], so a registered table's *other*
    /// actor-shaped columns were accounted for by nothing at all — not by the
    /// entry (which declares exactly one column), not by
    /// [`SUCCESSION_REFERENCES`], not by `EXCLUDED`. Fourteen columns sat in
    /// that blind spot, and three of them were the guardian side of the family
    /// plane: `guardianships.guardian_actor_id`,
    /// `guardian_transfers.proposed_guardian_actor_id` and
    /// `guardian_contact_requests.peer_actor_id`.
    ///
    /// That is the finding `succession-aftermath.md` records about the
    /// actor/author/owner vocabulary — *"a registry that can only see what it
    /// can name is complete only against its own vocabulary"* — one level down:
    /// this walk could see only what it could name **and only one name per
    /// table**. It bites harder on the succession axis than on the deletion
    /// axis, and the asymmetry is the reason it went unnoticed: deletion asks
    /// *"does this row belong to X"*, for which one column per table is the
    /// right shape, while succession asks *"does this row still name an
    /// identity that no longer exists"*, which every actor column answers
    /// separately.
    ///
    /// A column is now accounted for by its own `ACTOR_TABLES` entry, by a
    /// `SUCCESSION_REFERENCES` declaration, or by `EXCLUDED` — and by nothing
    /// else.
    #[test]
    fn every_actor_shaped_column_is_registered_or_excluded() {
        // Reviewed by hand, same standard as a `Policy::Retain` reason: each
        // names why the column is NOT "this row belongs to that actor".
        const EXCLUDED: &[(&str, &str, &str)] = &[
            (
                "third_party_principals",
                "principal_id",
                "a nest-minted 16-byte handle for a third-party principal -- an \
                 external app's roster row, not an account. `principal` is the \
                 OAuth/trust-model word for that app (`third-party.md` § The \
                 principal model); the row's own actor is `actor_id`, registered",
            ),
            // The same roster handle on the three tables that hang off a
            // principal and belong to no account. `hosted_plugins` and
            // `plugin_state` are the hosted half of an INSTALL, the nest's act
            // and no account's (`third-party.md` § The principal model →
            // *Hosted principals*): uninstall deletes them, and the one person
            // on them, `hosted_plugins.installed_by`, is ruled in
            // `SUCCESSION_REFERENCES`.
            (
                "hosted_plugins",
                "principal_id",
                "the hosted plugin's install-row handle \
                 (`third_party_principals.principal_id`, excluded above) -- an app, \
                 never an account; the row is the nest's install and goes with \
                 uninstall, not with any account",
            ),
            (
                "plugin_state",
                "principal_id",
                "the hosted plugin whose state scope the row is -- the same install-row \
                 handle as `hosted_plugins.principal_id`, deleted with the plugin",
            ),
            (
                "folder_deposit_inbox",
                "principal_id",
                "the DEPOSITOR, a third-party principal's roster handle \
                 (`third_party_principals.principal_id`) -- an app, never an account. \
                 The table has no actor column: an item is its folder's \
                 (`folder_id`, ON DELETE CASCADE) and leaves with it",
            ),
            // Added 2026-10-03 with the bridged-conversation family's
            // registration (the row's own actor is `actor_id`, registered).
            (
                "bridge_conversation_rooms",
                "bridge_principal_id",
                "the bridge the room is born on -- a third-party principal's 16-byte \
                 roster handle (`third_party_principals.principal_id`, excluded above) or \
                 the in-process Nostr leg's constant; an app, never an account",
            ),
            (
                "bridge_conversation_outbox",
                "bridge_principal_id",
                "the bridge that drains the item -- the room's own bridge principal, \
                 never an account (see `bridge_conversation_rooms.bridge_principal_id`)",
            ),
            (
                "bridge_conversation_messages",
                "sender",
                "the FAR network's address of who sent the row, as the authenticated bridge \
                 asserts it (a Matrix id, a Nostr pubkey) -- or, on a Sent copy, the \
                 account's own far address. Never a Fauna actor id, so no deletion or \
                 succession on this nest can name it; the row's own actor is `actor_id`",
            ),
            (
                "bridge_authors",
                "actor_id",
                "the SYNTHETIC id a bridged post rests under, a domain-separated BLAKE3 \
                 derivation over an external identity (`synthetic_actor_id` over the AP \
                 actor URI, the nostr pubkey, the Bluesky account's DID) -- an id no Fauna account \
                 can hold, so no deletion or succession on this nest can name it. The row \
                 is a derived, recreatable face of a REMOTE author (`bridges.md` § Unified \
                 feed ingestion), re-projected by the next transit that sees them",
            ),
            // Added 2026-09-21 with the `peer` root: its four false positives.
            // The root exists for `contacts.peer_id`, a raw actor id declared
            // in `SUCCESSION_REFERENCES`.
            (
                "peer_content_reports",
                "peer_nest_id",
                "the exporting peer NEST's channel-verified key, not an account -- \
                 report-exchange state keyed by content and nest; no account's \
                 deletion or succession can name it",
            ),
            (
                "peer_content_trends",
                "peer_nest_id",
                "the exporting peer NEST's channel-verified key -- same table family, \
                 same reason as peer_content_reports.peer_nest_id above",
            ),
            (
                "guardian_policies",
                "unknown_peer_dm",
                "a policy KNOB (a closed enum: what the bridge-DM gate does with a peer \
                 it has no row for), not an identifier; the row's person is \
                 `supervised_actor_id`, registered",
            ),
            (
                "guardian_dm_peers",
                "peer_id",
                "an EXTERNAL network identity as TEXT (a nostr pubkey hex, a bluesky DID), \
                 scoped by `bridge_id` and opaque to the gate, which only compares it -- \
                 never a Fauna actor id, so no deletion or succession on this nest can \
                 name it. The row is the WARD's oversight verdict and goes with \
                 `supervised_actor_id`, registered",
            ),
            // Added 2026-09-21 with the `admin` root, which exists for
            // `membership_tiers.admin_id`, a raw actor id registered in
            // `ACTOR_TABLES`: its one false positive.
            (
                "membership_tiers",
                "admin_tier",
                "a quota-tier NAME (`tiers.name`) the designation grants, not a person -- \
                 the row's person is `admin_id`, registered",
            ),
            // Added 2026-09-20 with `export_sessions`. A vocabulary FALSE
            // POSITIVE of the `authority_name` class: the column holds a
            // wrapped session KEY and no actor id at all — it matched because
            // "actor" sits inside "…_wrapped_for_**actor**". The row's person
            // column is `actor_id`, which carries the table's `ACTOR_TABLES`
            // entry (Purge / Burn / Redacted) — and that entry is where this
            // column's own ruling lives: it is the `omit` of the `Redacted`
            // verdict, and it dies with the row on both the deletion and the
            // succession axis. ⚠ This line excuses the COLUMN from the
            // who-is-this-row's-actor question only. It is matched per
            // `(table, column)`, so `actor_id` and every column a later
            // `ALTER TABLE` adds stay watched — never widen it to the table.
            (
                "export_sessions",
                "blob_decryption_key_wrapped_for_actor",
                "a wrapped per-session KEY, not an actor id — matched only because \
                 the root list matches substrings and \"actor\" is inside \
                 \"wrapped_for_actor\"; the row's person column is `actor_id`, \
                 which IS registered, and this column's disposition is ruled on \
                 that entry (omitted from the export, gone with the row)",
            ),
            // Added 2026-09-09 with the `principal` root. A vocabulary FALSE
            // POSITIVE of the `authority_name` class: `principal_kind` is not
            // a person but the KIND of one — the closed vocabulary
            // `user | nest | bridge` that decides a room's confidentiality
            // class (`conversation-rooms.md` § The three classes). It matches
            // only because the root list matches substrings. The row's actual
            // person column is `principal_id`, registered above.
            (
                "room_members",
                "principal_kind",
                "the KIND of principal (user/nest/bridge), a closed vocabulary \
                 naming no account — matched only because the root list matches \
                 substrings; the row's person column is `principal_id`, which IS \
                 registered",
            ),
            // Added 2026-09-21 with the `classifier` root. A vocabulary FALSE
            // POSITIVE of the `principal_kind` class: an INTEGER, the version
            // of the classifier that produced the verdict.
            (
                "content_labels",
                "classifier_version",
                "the VERSION of the producing classifier, an integer naming nobody — \
                 matched only because the root list matches substrings; the column \
                 that can name a person is `classifier_id`, declared in \
                 SUCCESSION_REFERENCES",
            ),
            // Added 2026-09-21 with the `invitee` root. A vocabulary FALSE
            // POSITIVE, `knocks.sender_node`'s shape: the invitee's home NEST
            // url — empty for a same-nest invitee — copied onto the seat's
            // `home_node_url` at acceptance. A box, not a person; the person
            // is `invitee_id`, which IS registered.
            (
                "room_invites",
                "invitee_node_url",
                "the invitee's home NEST url (TEXT, empty when same-nest), read back \
                 as the new seat's `home_node_url` — a box, not a person, matched \
                 only because the root list matches substrings; the row's person \
                 column is `invitee_id`, which IS registered",
            ),
            // The inviter on a room's floor roster — an agentive `…_by`: it
            // records WHO seated this member, not whose row it is. The row
            // belongs to `principal_id`.
            (
                "room_members",
                "invited_by",
                "the member who seated this one, not the row's own principal — \
                 the row belongs to `principal_id`, which IS registered",
            ),
            // Added 2026-08-20 with `region_sequence_floor` (the anti-replay floor). ⚠ **A vocabulary FALSE POSITIVE, and the
            // first one on record** — every other entry in this list is a real
            // person-naming column that simply is not this row's own actor.
            // `authority_name` is not a person at all: it is the curated
            // registry's name for the body administering a region (a
            // government regulator), stored at acceptance time so the
            // transparency read shows who was verified when the document
            // started binding. It matches only because `ACTOR_SHAPED_ROOTS`
            // matches SUBSTRINGS and "author" sits inside "**author**ity".
            // Nothing here belongs to any account, and deleting a user must
            // never touch it.
            //
            // Left as an exclusion rather than "fixed" in the vocabulary: the
            // substring rule is deliberate (it is what lets `sender_actor` and
            // `recipient_id` both hit), and narrowing it to word boundaries to
            // dodge one collision would risk blinding the walk to a real
            // column — the failure mode this whole test exists to prevent. An
            // over-eager match costs one reasoned line here; an under-eager
            // one costs an unruled actor column nobody sees.
            (
                "region_sequence_floor",
                "authority_name",
                "the curated registry's name for the AUTHORITY administering a \
                 region (a regulator, not a person) — matched only because the \
                 root list matches substrings and \"author\" is inside \
                 \"authority\"; region_sequence_floor is keyed by (region, \
                 payload kind, authority) and belongs to no account",
            ),
            (
                "bridge_service_users",
                "approved_by_actor_id",
                "the ADMIN who approved the bridge registration, not the service \
                 user's own actor — this table has no per-user actor column at all",
            ),
            (
                "mail_domains",
                "catch_all_actor_id",
                "a domain's configured catch-all DESTINATION; mail_domains is keyed \
                 by domain, not by actor, and this actor may not even be the domain's owner",
            ),
            (
                "mail_outbound_diagnostic_runs",
                "ran_by_actor_id",
                "the ADMIN who triggered a deployment-wide diagnostic run, not a \
                 per-actor owner — this table has no per-user actor column at all",
            ),
            (
                "mail_domain_renames",
                "initiated_by_actor_id",
                "the ADMIN who started a domain-rename ceremony; mail_domain_renames \
                 is keyed by rename_id, not by actor",
            ),
            // Added 2026-08-12 for `nest_rotation_log` (the
            // deployment-seed rotation ceremony). Both columns name the **nest's
            // own deployment identity**, not any account — the same
            // "names an actor without being that actor's row" shape as the
            // entries around them, one level up: this is the box rotating, not a
            // user. Deleting a user must never touch it, and in fact *nothing*
            // may: the migration's own comment says rows are never deleted or
            // rewritten, because a missing hop is not a shorter chain but an
            // unwalkable one, which re-opens the downgrade the ceremony closes.
            // Deliberately absent from `SUCCESSION_REFERENCES` too — a *user's*
            // succession has nothing to say about the box's identity chain.
            (
                "nest_rotation_log",
                "old_actor_id",
                "the NEST's own superseded deployment identity, not an account — \
                 nest_rotation_log is the box's rotation chain, keyed by seq, and its \
                 rows are never deleted or rewritten by design (a missing hop is an \
                 unwalkable chain, not a smaller one)",
            ),
            (
                "nest_rotation_log",
                "new_actor_id",
                "the NEST's own established deployment identity — same table, same \
                 reason as old_actor_id above",
            ),
            (
                "namespace_entries",
                "actor_sig",
                "a cryptographic SIGNATURE over the row's ciphertext, not an actor id -- \
                 it matches the `actor` root by name only. The row's person column is \
                 `namespace`, the publishing actor's id, which IS registered (this note \
                 said the table had no actor-identifying column at all until 2026-09-21; \
                 its writers refute that)",
            ),
            (
                "region_artifacts",
                "authority_name",
                "the region policy AUTHORITY's name (an external governance body) — \
                 \"authority\" matches the `author` root by substring only; \
                 region_artifacts is deployment-wide config keyed by region, not by actor",
            ),
            (
                "region_relay_cache",
                "authority_name",
                "the relayed region policy's AUTHORITY name, the third table to carry \
                 it after region_sequence_floor and region_artifacts — the same \
                 `author`-inside-`authority` substring false positive; region_relay_cache \
                 is the nest's cache of a region's published policy fetched on an app's \
                 behalf, keyed by (region, payload kind), and belongs to no account",
            ),
            (
                "content_fts",
                "author_name",
                "a free-text DISPLAY NAME for FTS5 matching, not an actor id. \
                 content_fts is a derived index, but NOT only a mirror of `content`: \
                 its `profile` rows mirror no content row — each is keyed on \
                 blake3(\"profile:\" + hex(actor)), an actor id this column walk \
                 cannot see, and names that actor's HANDLE. So no content purge \
                 reaches them; they are derived from `users.handle` instead, by \
                 `fts::sync_profile_row` inside every statement that writes or clears \
                 a handle or deletes a `users` row (the account going removes it; \
                 a succession moves it to the successor), and re-derived at every \
                 boot by `fts::reconcile_profile_rows`. Post rows mirror `content` \
                 (Policy::Retain) and bridge rows follow their bridge's search \
                 policy — no separate raw purge applies to either",
            ),
            (
                "feed_global_factors",
                "factor",
                "a TEXT factor-name label (e.g. \"recency\"), not an actor id — \
                 \"factor\" matches the `actor` root by substring only; this table's \
                 real actor column is `owner`, already registered",
            ),
            (
                "peer_content_reports",
                "factor",
                "same substring false positive as feed_global_factors.factor — a \
                 report-category label, not an actor id; this table has no \
                 actor-identifying column at all (federated aggregate counts)",
            ),
            (
                "content_scores",
                "factor",
                "same substring false positive as feed_global_factors.factor — a \
                 scoring-dimension label (e.g. \"trending\"), not an actor id; this \
                 table's real actor column is `actor_id`, already registered",
            ),
            // Surfaced 2026-08-13 by the second-column widening above. The
            // `factor` pair repeats the substring false positive its three
            // siblings already carry; it was invisible before only because both
            // tables are registered on a different column.
            (
                "content_reports",
                "factor",
                "same substring false positive as feed_global_factors.factor — a \
                 report-category label, not an actor id; this table's real actor \
                 column is already registered",
            ),
            (
                "labelers",
                "factor",
                "same substring false positive as feed_global_factors.factor — a \
                 scoring/classification label, not an actor id; this table's real \
                 actor column is `publisher_actor`, already registered",
            ),
            (
                "personalization_models",
                "factor",
                "same substring false positive as feed_global_factors.factor — a \
                 model feature-dimension label, not an actor id; this table's real \
                 actor column is already registered",
            ),
            // The two bridge columns name a BRIDGE SERVICE USER, which is the
            // `nest_rotation_log` shape one role over: an actor id that belongs
            // to no human account. Deliberately outside SUCCESSION_REFERENCES
            // as well as ACTOR_TABLES — a bridge is enrolled and revoked
            // through its service-user record and has no recovery chain, so no
            // succession ceremony can ever name one and there is nothing for
            // this axis to rule.
            (
                "bridge_audit_events",
                "bridge_actor_id",
                "the BRIDGE service user that performed the audited action, not the \
                 subject account — this table's per-user column is `actor_id`, already \
                 registered. A bridge holds no recovery chain, so it can never be the \
                 old or new actor of a succession",
            ),
            (
                "bridge_session_close_events",
                "bridge_actor_id",
                "the BRIDGE service user whose session closed — same shape and same \
                 reason as bridge_audit_events.bridge_actor_id; this table's per-user \
                 column is `actor_id`, already registered",
            ),
            // Surfaced 2026-08-13 by the VOCABULARY widening (`recipient`,
            // `reporter`, `subscriber` and the agentive `…_by`). The six real
            // actor columns it found are declared in `SUCCESSION_REFERENCES`;
            // these twelve are the widening's false positives, and they cluster
            // into three families worth naming rather than reasoning about
            // twelve times.
            //
            // FAMILY 1 — `recipient` means an EMAIL ADDRESS in the mail plane,
            // never an actor id. That is the price of a root that earns its
            // place elsewhere (`auto_reply_log.recipient_id`
            // is a genuine registered actor column).
            (
                "greylist_tuples",
                "recipient",
                "the envelope RCPT address a greylist tuple keys on — \
                 greylist_tuples is (sender_domain, recipient, subnet) triple state \
                 for an inbound attempt, and the address may belong to no local \
                 account at all",
            ),
            (
                "outbound_mail_queue",
                "recipient",
                "the remote envelope RCPT address a queued message is bound for; \
                 this table's own actor column is `actor_id`, already registered",
            ),
            (
                "mail_list_members",
                "recipient_address",
                "a subscribed EMAIL ADDRESS on a mailing list — members are \
                 addresses, not accounts, which is the whole point of a list",
            ),
            (
                "tlsrpt_outbound_reports",
                "recipient_domain",
                "the remote DOMAIN a TLS-RPT report covers; the table is \
                 deployment-wide reporting state keyed by (domain, date), with no \
                 per-actor column at all",
            ),
            // FAMILY 2 — `recipient` inside a COUNT or a LIMIT: integers, not
            // identifiers. The substring match cannot tell a noun from a
            // quantity of that noun.
            (
                "mail_list_sends",
                "recipient_count",
                "an INTEGER fan-out size for one send, not an identifier",
            ),
            (
                "mail_lists",
                "recipients_per_send",
                "an INTEGER rate limit on a list's fan-out, not an identifier",
            ),
            (
                "mail_lists",
                "recipients_today",
                "an INTEGER daily counter, not an identifier",
            ),
            (
                "mail_list_account_daily_counter",
                "recipients_sent",
                "an INTEGER per-account daily counter, not an identifier",
            ),
            (
                "mail_list_deployment_daily_counter",
                "recipients_sent",
                "an INTEGER deployment-wide daily counter, not an identifier",
            ),
            // FAMILY 3 — `…_by` that records a ROLE or a non-actor id rather
            // than an actor. The agentive rule is right about the question
            // ("who did this?") and these three answer it with something other
            // than an account.
            (
                "guardian_dm_peers",
                "added_by",
                "a two-value ROLE enum ('ward' | 'guardian') recording which side \
                 seeded the row, not who — it is what lets the ward's outbound seed \
                 refuse to overwrite a guardian's block; this table's actor column is \
                 `supervised_actor_id`, already registered",
            ),
            (
                "guardian_mail_allowlist",
                "added_by",
                "the same 'ward' | 'guardian' role enum as guardian_dm_peers.added_by, \
                 one table over; the actor column here is `supervised_actor_id` too",
            ),
            // Surfaced 2026-08-14 by the `sender` widening (the delivery plane).
            // The two real actor columns it found — `knocks.sender_id` and
            // `notifications.sender_id` — are declared in `SUCCESSION_REFERENCES`;
            // these ten are its false positives, and they are ONE family with one
            // cause: `sender` is the only root in the vocabulary that is also an
            // everyday MAIL noun, so it matches the SMTP envelope's vocabulary
            // wholesale. Every one of them names an address, a domain, a hash of
            // an address, a peer nest or a policy knob — never a person.
            //
            // Excluded by name rather than by narrowing the root back to
            // something mail-safe, and that choice is the point: a narrower
            // search finds fewer problems, which is indistinguishable from having
            // none (`the_walks_vocabulary_can_name_every_registered_column`'s
            // whole argument). Ten one-line exclusions are the cheap half of a
            // trade whose expensive half was two columns no gate could see.
            (
                "bounce_history",
                "original_sender",
                "the SMTP envelope sender ADDRESS a bounce is owed to (TEXT), not an \
                 actor id — bounce_history is keyed by (original_sender, \
                 original_msgid, sent_at) and holds no account column at all",
            ),
            (
                "outbound_mail_queue",
                "original_sender",
                "the same envelope-sender ADDRESS one queue over (TEXT), carried so \
                 the SRS rewrite and the NDR route can name it; this table's actor \
                 column is `forward_actor_id`, already registered",
            ),
            (
                "forward_queue",
                "original_sender",
                "the same envelope-sender ADDRESS again (TEXT) — the third table in \
                 the mail path to carry it; this table's actor column is already \
                 registered",
            ),
            (
                "alias_hits",
                "sender_domain",
                "the sending DOMAIN of a message that hit an alias (TEXT), kept for \
                 per-alias traffic stats — a domain is not a person",
            ),
            (
                "greylist_tuples",
                "sender_domain",
                "the sending DOMAIN half of the greylist's (domain, recipient, \
                 subnet) key (TEXT); greylist state is per-tuple, not per-account",
            ),
            (
                "segment_records",
                "sender_dom",
                "the sending DOMAIN of an indexed message (TEXT) — an abbreviation \
                 of the same false positive; this table's actor column is `scope_id`, \
                 the declared VOCABULARY_BLIND_SPOTS entry above",
            ),
            (
                "guardian_mail_holds",
                "sender_address",
                "the ADDRESS a held message came from (TEXT), shown to the guardian \
                 deciding on it; the account is `supervised_actor_id`, already \
                 registered",
            ),
            (
                "auto_reply_log",
                "sender_hash",
                "a HASH of the sender's address, the half of the (recipient_id, \
                 sender_hash) key that keeps an auto-reply from looping — it \
                 identifies an address the box never stores in clear, and no actor",
            ),
            (
                "guardian_policies",
                "unknown_sender_mail",
                "a closed-enum POLICY knob ('allow' | …) deciding what happens to \
                 mail from an unrecognised sender, not an identifier of any kind; \
                 this table's actor column is `supervised_actor_id`",
            ),
            (
                "knocks",
                "sender_node",
                "the knocking party's home NEST url (BLOB holding UTF-8), read back \
                 as a node address to authorize the cross-nest fetch — a box, not a \
                 person; the person is `sender_id`, declared in SUCCESSION_REFERENCES \
                 by the same pass that surfaced this",
            ),
            (
                "room_invites",
                "invitee_nest_id",
                "the invitee's home NEST id (BLOB) a cross-nest invitation is pushed \
                 to — a box, not a person; the person is `invitee_id`, the row's own \
                 registered actor",
            ),
            (
                "abuse_reports",
                "block_author",
                "a BOOLEAN — whether the reporter also blocked the reported author in \
                 the same gesture — not an identifier; the author is `subject_actor`, \
                 declared in SUCCESSION_REFERENCES",
            ),
            (
                "abuse_report_outbox",
                "peer_url",
                "the peer NEST's URL a queued triad call is sent to — a box, not a \
                 person; the queue has no actor column (its rows follow the report \
                 they name by `report_id`)",
            ),
            (
                "abuse_report_outbox",
                "peer_nest_id",
                "the peer NEST's verified id (hex) a queued triad call is pinned to — \
                 a box, not a person",
            ),
        ];

        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();

        let mut tables_stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap();
        let all_tables: Vec<String> = tables_stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        drop(tables_stmt);

        // The table's OWN declared column, when it has one. A registered table
        // is not skipped wholesale — see the second-column paragraph in this
        // test's doc comment — only the one column its entry already accounts
        // for.
        let registered: std::collections::HashMap<&str, &str> =
            ACTOR_TABLES.iter().map(|e| (e.table, e.column)).collect();
        let declared_references: HashSet<(&str, &str)> = SUCCESSION_REFERENCES
            .iter()
            .map(|(t, c, _)| (*t, *c))
            .collect();

        let mut unaccounted = Vec::new();
        for table in &all_tables {
            // `users` is `delete_user`'s own row, not a registry entry — see
            // `no_duplicate_tables_and_users_is_absent`.
            if table == "users" {
                continue;
            }
            let owned_column = registered.get(table.as_str()).copied();
            let pragma = format!("PRAGMA table_info({table})");
            let mut stmt = conn.prepare(&pragma).unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            for col in &cols {
                if !actor_shaped(col) {
                    continue;
                }
                // Accounted for by this table's own ACTOR_TABLES entry…
                if owned_column == Some(col.as_str()) {
                    continue;
                }
                // …or by a SUCCESSION_REFERENCES declaration (the list for
                // columns that name an actor without the row belonging to it)…
                if declared_references.contains(&(table.as_str(), col.as_str())) {
                    continue;
                }
                // …or by a reasoned exclusion here.
                if EXCLUDED.iter().any(|(t, c, _)| t == table && c == col) {
                    continue;
                }
                unaccounted.push(format!("{table}.{col}"));
            }
        }
        unaccounted.sort();
        assert!(
            unaccounted.is_empty(),
            "actor-shaped column(s) accounted for by nothing — add each to ACTOR_TABLES \
             (this row's own actor), to SUCCESSION_REFERENCES (it names an actor the row \
             does NOT belong to, and a succession must still rule it), or to this test's \
             EXCLUDED list with why it is neither: {unaccounted:?}"
        );
    }

    /// The feature-gated tables this module cannot stand up must still assert
    /// *something*, and the something is their encoding: all fifteen belong to
    /// the three bridge families, every one of which declares its actor column
    /// `TEXT` and stores lowercase hex (`nostr/db.rs`,
    /// `fauna-bridge-atproto/src/db.rs`, `fauna-bridge-activitypub/src/db.rs`,
    /// read at source).
    ///
    /// This exists because `FEATURE_GATED_ELSEWHERE` had silently widened from
    /// *"this test cannot create the table"* into *"nothing checks these tables
    /// at all"* — and the one family it excused was the one that was broken.
    /// An exemption that excuses every check is the structural costume of
    /// "cannot be tested here"; this narrows it back to the one thing the
    /// connection genuinely cannot do.
    #[test]
    fn every_feature_gated_table_declares_the_bridge_hex_encoding() {
        for entry in ACTOR_TABLES {
            if !FEATURE_GATED_ELSEWHERE.contains(&entry.table) {
                continue;
            }
            assert_eq!(
                entry.key,
                ActorKey::Hex,
                "{} is a bridge-family table: its actor column is TEXT hex, so a \
                 Blob binding would delete nothing",
                entry.table
            );
        }
        // No converse assertion here on purpose. `Hex` is NOT exclusive to the
        // gated set — `bridge_search_policy` lives in the main schema and is
        // hex-keyed too (`bridges_ui_handlers.rs` writes it with `actor_hex`),
        // which is what the affinity test found the moment it existed. Every
        // non-gated entry is already checked directly against its column, so a
        // "Hex ⟹ gated" rule here would only be a second, weaker claim that a
        // legitimate table breaks.
    }

    /// Column-name fragments that read as "opaque bytes this schema does not
    /// interpret". Deliberately dumb, like `SECRET_SHAPED_COLUMN_FRAGMENTS`:
    /// the job is to NOTICE, never to judge. Module-level because two walks
    /// read it: the blob walk below claims these columns, and the declared-type
    /// walk in [`opaque_carriers`] subtracts exactly the same set, so one
    /// column can never be owed to both or to neither.
    const BLOB_SHAPED_COLUMN_FRAGMENTS: &[&str] = &["blob"];

    /// The declared-TYPE walk over every `BLOB` column the two name walks do
    /// not claim. A child module so it still sees this module's helpers, and
    /// a file of its own because its lists are the registry's longest.
    mod opaque_carriers;

    /// **A walk that reads column NAMES is blind to what a column CONTAINS —
    /// so a blob-held actor id is declared, never searched for.**
    ///
    /// [`every_actor_shaped_column_is_registered_or_excluded`] finds
    /// unaccounted columns with [`actor_shaped`], a word list over column
    /// names. That makes it exactly as complete as its vocabulary, which
    /// `the_walks_vocabulary_can_name_every_registered_column` turns from a
    /// circular claim into an assertion — one level down. One level **out**,
    /// the same circularity was still live in a form no widening of the word
    /// list can reach: a column whose *contents* name actors reads as nothing
    /// at all, and a table with no actor-shaped column is never even opened by
    /// that walk.
    ///
    /// `room_policy_versions` (schema 68) is exactly that: its owner id, admin
    /// set and signer id all sit inside `policy_blob`, and it landed with no
    /// ruling on either axis without reddening anything, because nothing could
    /// notice it had none. `rooms.policy_blob` and `rooms.labelers_blob` are
    /// the same shape one table over — visible to the census only through
    /// `rooms.owner_id`, which says nothing about what the blobs hold.
    ///
    /// So every blob-shaped column in the live schema must SAY which it is:
    /// [`ACTOR_BEARING_BLOB_COLUMNS`] (its bytes name actors — with the ruling
    /// spelled out) or [`NON_ACTOR_BLOB_COLUMNS`] (they do not — with why).
    /// The same declare-the-exceptions-by-name shape
    /// `VOCABULARY_BLIND_SPOTS` and `CLEARED_EXPORTING_COLUMNS` already use.
    ///
    /// ⚠ **What this does NOT claim.** That the fragment list finds every
    /// blob-held actor id — a column named `payload` or `record` could hold
    /// one and this walk would not ask. It claims the narrower, checkable
    /// thing: a column the schema itself calls a blob cannot arrive without
    /// somebody writing down whether it names people. A `_blob`-free carrier
    /// is the declared residual, and it is the reason the fragment list is
    /// here in code rather than in prose — widening it is one line.
    ///
    /// **Re-affirmed 2026-09-21, with the measurement that decided it.** The
    /// pass that drained this walk's backlog weighed widening the list to
    /// `payload` / `record` / `body` and declined: the schema's opaque
    /// carriers outside the word `blob` are on the order of a hundred columns
    /// (`record`, `statement`, `envelope`, `witness`, `receipt`, `evidence`,
    /// the `*_sealed` and `encrypted_*` families, …) with no shared
    /// vocabulary, so three more fragments would catch a handful and read as
    /// coverage of the rest — the exact circularity this walk exists to break,
    /// re-created inside it. The complete notion is not a longer word list but
    /// the column's declared TYPE — and that walk now exists:
    /// [`opaque_carriers`] takes every `BLOB`-typed column this walk and the
    /// census do not claim, so a carrier named `payload` or `record` no longer
    /// arrives silently. What remains outside BOTH walks is a carrier that is
    /// not `BLOB`-typed at all — structured data in a `TEXT` column
    /// (`results_json`, `origins_json`, the `overrides_json` family) — and
    /// that is the residual now declared, here and in
    /// `succession-repoint-axis.md` § The declared re-point axis.
    ///
    /// Authority for the room-plane rulings below:
    /// `conversation-rooms.md` § Roles and authorization → *Members verify
    /// what they paint*; the walk itself is
    /// `succession-repoint-axis.md` § The declared re-point axis.
    #[test]
    fn every_blob_shaped_column_declares_whether_it_holds_actor_ids() {
        /// Blob columns whose CONTENTS name actors, each with what deletion
        /// and succession do to those ids. An entry here is a ruling, not a
        /// note — it is the only place the census can carry one, since no
        /// column name reaches inside.
        const ACTOR_BEARING_BLOB_COLUMNS: &[(&str, &str, &str)] = &[
            (
                "rooms",
                "policy_blob",
                "the owner-signed room policy: it names the OWNER and every ADMIN. \
                 STAY, and rewriting is the floor's own compare-and-set, never a \
                 succession's: the bytes carry the author's signature over them, so \
                 re-pointing an id inside would break the very signature every member \
                 verifies. A succession resolves a named id through the chain at \
                 judgment time (`floor_designee`) instead, and a deleted admin's id \
                 leaves when the owner signs the next version. The row's own person \
                 column, `owner_id`, is registered in ACTOR_TABLES and rules the ROW; \
                 this rules what is inside the column",
            ),
            (
                "rooms",
                "labelers_blob",
                "the owner-signed labeler set: it names the SIGNER (owner or an admin) \
                 and the labeler actors chosen. STAY, for `policy_blob`'s reason and \
                 by the same mechanism — a signed act, replaced wholesale by the next \
                 signed act, never edited in place",
            ),
            (
                "room_policy_versions",
                "policy_blob",
                "every signed policy version the room has held (schema 68), each naming \
                 that version's owner, admins and signer. STAY, and it is the one entry \
                 here where staying is load-bearing rather than merely safe: a member \
                 judges a floor delete record against the policy OF THE VERSION THE \
                 RECORD NAMES, so a retained version naming a since-deleted admin is \
                 the verification history that keeps that delete a delete. Dropping it \
                 would un-tombstone every delete made under it on the next fresh \
                 session -- silently, since a re-walking member re-judges from scratch. \
                 The bytes are a signed PUBLIC act every member already received, so \
                 retaining one exposes nothing the room did not publish. Rewriting is \
                 refused outright (`db/rooms.rs::retain_policy_version`): history that \
                 can be rewritten answers a different question from the one members ask \
                 of it. ⚠ This SUPERSEDES the answer recorded earlier for these same \
                 bytes -- \"the owner re-signs\" -- which was true \
                 only while each re-sign \
                 OVERWROTE the blob; since schema 68 the re-sign mints a new version and \
                 keeps the old one. Ruling: `conversation-rooms.md` § Roles and \
                 authorization -> *Members verify what they paint*",
            ),
            (
                "room_generations",
                "mint_blob",
                "the owner-or-admin-signed generation mint: it names the MINTER. STAY, \
                 same shape and same reason as `rooms.policy_blob` -- a signed act \
                 whose signature covers the id inside it. Unlike the three above, this \
                 table's minter is ALSO carried as a plain column (`minted_by`), which \
                 is registered and rules the row; this entry exists so the blob is not \
                 read as un-ruled merely because its twin is the visible one",
            ),
            // ── The second pass (2026-09-21): the nine bearers outside the room
            // plane. One rule ran through all of them and is the transferable
            // half — **a blob ruling follows the row ruling its table already
            // has in ACTOR_TABLES; it never re-derives one.** What the pass had
            // to establish per carrier was only (a) which ids the bytes name
            // and (b) whether the row's verdict leaves those ids in a state
            // anything can observe. Rulings: `succession-repoint-axis.md`
            // § The declared re-point axis, the blob-held entry's second half.
            //
            // The four MUA-plane blobs share one shape, so it is said once: the
            // container carries a PLAINTEXT index (`"ix"`) naming the row's own
            // actor, which the client writes and the AEAD binds as associated
            // data (`fauna_mls::wrapped_blob::AadBinding`). The nest never
            // decodes these — the row's `actor_id` comes from the authenticated
            // caller, the index from the client — so the two agree by
            // convention, and the bridge that opens one passes the actor it
            // resolved, which is what makes a disagreement fail closed.
            (
                "bridge_wrapped_mls_blobs",
                "blob",
                "the MSEK wrapped under one MUA credential: its plaintext index names \
                 the row's OWN actor (and the credential id), AEAD-bound. FOLLOWS THE \
                 ROW -- Purge on deletion, Burn on succession -- so the id never \
                 outlives the row that carries it. `Move` was already ruled dangerous \
                 for the row; for the bytes it is also impossible: the bridge opens \
                 with the actor it resolved, so a carried blob indexed to the \
                 predecessor cannot be opened as the successor's, and the nest holds \
                 no key to re-seal it",
            ),
            (
                "bridge_wrapped_submission_tokens",
                "blob",
                "the outbound half of the same credential plane: the plaintext index \
                 names the row's own actor and credential, and the sealed token inside \
                 names them again under the user's signature. FOLLOWS THE ROW (Purge / \
                 Burn), for `bridge_wrapped_mls_blobs.blob`'s reason",
            ),
            (
                "bridge_mls_snapshot_blobs",
                "blob",
                "the actor's MLS state sealed under the MSEK: the plaintext index names \
                 the row's own actor, AEAD-bound; what the ciphertext names (group \
                 members, as any MLS state does) is the client's and the bridge's to \
                 read, never the nest's. FOLLOWS THE ROW (Purge / Burn) -- the \
                 successor's client re-provisions under its own MSEK",
            ),
            (
                "bridge_webdav_keys_blobs",
                "blob",
                "the served-set content keys sealed under the MSEK: the plaintext index \
                 names the row's own actor, AEAD-bound. FOLLOWS THE ROW (Purge / Burn)",
            ),
            (
                "atproto_identity_key_blobs",
                "blob",
                "the DID's bridge-custodied signing + rotation keys, minted NEST-side and \
                 sealed to the atproto.pds bridge: the actor id the keys were minted for \
                 appears TWICE, in the AEAD-bound plaintext index and again inside the \
                 sealed bundle. Deletion purges the row. Succession is the one case on \
                 this list where the row MOVES and the bytes cannot follow: the nest \
                 discarded the plaintext at mint and holds only ciphertext, so after the \
                 ceremony the row is the successor's and both ids inside still name the \
                 FIRST holder, for the life of the DID. The bytes STAY inside a row that \
                 moves, and the ids inside are PROVENANCE, never compared: the bridge \
                 binds an opened blob to the identity's PUBLISHED keys \
                 (`unseal_atproto_identity`'s expectation, served from the \
                 `atproto_identities` row that moves or parks all-or-nothing with this \
                 one). That refuses another identity's WHOLE blob -- which the index \
                 binding, detecting only an EDITED index, never saw -- and holds across \
                 any number of hops with no chain lookup. The fix NOT to reach for is an \
                 actor-id comparison: it strands every succeeded account's DID. Closes a \
                 misrouted or substituted row, not a hostile nest, which sources the \
                 expectation and minted the keys (`atproto-pds-bridge.md` § State & data \
                 shape)",
            ),
            (
                "capability_grants",
                "blob",
                "a user-minted grant: `GrantIndex` names the OWNER (the nest checks it \
                 equals the authenticated caller at deposit -- the one carrier here \
                 whose inner id the nest verifies). `holder` is a bridge service-user's \
                 X25519 key, documented *not* an actor identity, and the scope tuples \
                 name classes/kinds/tiers/sets, never people. FOLLOWS THE ROW: Purge on \
                 deletion; Burn on succession, where the successor's client re-mints \
                 each adjudicated grant under its own id (`succession-aftermath.md` \
                 § Implementation status today, the capability-grant re-mint) -- so no \
                 grant naming the predecessor survives to be honoured",
            ),
            (
                "recovery_escrow",
                "blob",
                "the identity seed sealed to the RecoveryKey: the plaintext index names \
                 the row's own actor; on a successor's blob the optional predecessor \
                 section names every PREDECESSOR id beside its seed, inside a seal only \
                 the phrase holder opens -- the nest is a bit-store on this path and can \
                 read neither. FOLLOWS THE ROW: Purge on deletion; Burn on succession, \
                 deleted in the ceremony's transaction, the successor's kit ceremony \
                 writing a fresh blob under its own id",
            ),
            (
                "current_key_blobs",
                "blob_data",
                "a tier's signed `KeyBlob`: it names the AUTHOR, the signing device, and \
                 every SUBSCRIBER the period key is wrapped to -- the only bearer here \
                 that names people other than the row's owner. The ROW moves with the \
                 `subscription_tiers` family; the BYTES STAY as signed, for the room \
                 plane's reason -- the author id sits under a signature verified against \
                 the author's own delegation (`verify_key_blob_signature`), so a \
                 re-pointed id is a blob nobody can verify. A moved historical version \
                 therefore names the predecessor as author for ever, which is true: the \
                 predecessor signed it. It is superseded, never edited -- every approval, \
                 revocation and rotation writes the next version wholesale. A deleted or \
                 succeeded SUBSCRIBER's id likewise rides until the next version: a wrap \
                 to a key nobody holds, conferring nothing (readability is gated on the \
                 `subscribers` row, which the purge walk and the paying-reader leg do \
                 rule), and it is a pseudonymous public key the author already held",
            ),
            (
                "labelers",
                "metadata_blob",
                "the canonical signed `AlgorithmLabeler`: it names `algorithm_id`, the \
                 artifact's self-signed rotatable keypair -- the same value the row \
                 carries as `publisher_actor`, and by that column's own ruling never an \
                 enrolled account. STAY, with the row and for its reason: the signature \
                 is verified over these bytes at publish and at every instantiation. \
                 Deletion is `publisher_actor`'s verdict too, with its caveat: no \
                 production writer can put an enrolled actor id there, so no account's \
                 deletion reaches the row through it. The account's own identity on the \
                 row is `caller_actor`, a plain column ruled elsewhere",
            ),
        ];

        /// Blob columns that name no actor, with the reason. A vocabulary
        /// false positive here is the common case — `blob` is in the name of
        /// plenty of columns that hold bytes, sizes, paths and digests.
        const NON_ACTOR_BLOB_COLUMNS: &[(&str, &str, &str)] = &[
            ("export_sessions", "blob_bytes", "an INTEGER byte count"),
            (
                "export_sessions",
                "blob_decryption_key_wrapped_for_actor",
                "a wrapped per-session KEY (already cleared for the export walk in \
                 `every_actor_shaped_column_is_registered_or_excluded`'s EXCLUDED)",
            ),
            (
                "export_sessions",
                "blob_path",
                "a filesystem path to the produced artifact",
            ),
            // The deployment's own key material: rows keyed by bridge, domain or
            // selector with no actor column at all, each an HPKE seal to a
            // bridge's attested X25519 whose plaintext index and sealed bundle
            // were both read for this entry (`fauna_mls::wrapped_blob::format`).
            (
                "bridge_tls_cert_blobs",
                "blob",
                "a TLS certificate + key sealed to one bridge: indexed `(bridge_role, \
                 bridge_id, domain)`, bundle = chain, key, expiry, issue time. A \
                 certificate names hosts",
            ),
            (
                "atproto_session_secret_blobs",
                "blob",
                "the bridge-wide HS256 session-token secret: indexed `(bridge_role, \
                 bridge_id)`, bundle = 32 secret bytes + issue time. One per bridge, \
                 shared by every account it serves, naming none",
            ),
            (
                "personalization_models",
                "sealed_blob",
                "a per-factor vocabulary model sealed CLIENT-side under the owner's \
                 delegable key for `fauna.personalization.model` \
                 (`fauna_client_personalization::seal_model_bytes`): the container is \
                 zstd + AEAD with NO index and only the fixed kind as associated data, \
                 so nothing in the bytes says whose they are -- ownership is the row's \
                 `actor_id` alone, which is registered and moves. The plaintext is word \
                 statistics over text the owner trained on; the nest never holds it",
            ),
        ];

        /// Blob-shaped columns that hold a digest, a size, a path or an id —
        /// a property OF a blob rather than the blob's contents, so there is
        /// nothing inside them to name anybody. Kept as a list rather than a
        /// name rule (`blob_hash` and friends) on purpose: the moment the
        /// matcher starts excusing shapes, the walk's completeness claim goes
        /// back to being a claim about the matcher.
        const BLOB_PROPERTY_COLUMNS: &[(&str, &str)] = &[
            ("backup_snapshots", "blob_hash"),
            ("blob_legal_withhold", "blob_hash"),
            ("content", "blob_hash"),
            ("conv_attachment_refs", "blob_hash"),
            ("current_key_blobs", "blob_hash"),
            ("legal_takedown_deleted_posts", "blob_digests"),
            ("tiers", "max_blob_size"),
            ("web_files", "blob_hash"),
            ("web_render_staged", "blob_hash"),
            ("web_rendered", "blob_hash"),
            ("web_rendered_sealed", "blob_hash"),
        ];

        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();
        let mut tables_stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap();
        let all_tables: Vec<String> = tables_stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        drop(tables_stmt);

        let declared: HashSet<(&str, &str)> = ACTOR_BEARING_BLOB_COLUMNS
            .iter()
            .chain(NON_ACTOR_BLOB_COLUMNS.iter())
            .map(|(t, c, _)| (*t, *c))
            .chain(BLOB_PROPERTY_COLUMNS.iter().copied())
            .collect();

        let mut undeclared = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for table in &all_tables {
            let pragma = format!("PRAGMA table_info({table})");
            let mut stmt = conn.prepare(&pragma).unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            for col in cols {
                if !BLOB_SHAPED_COLUMN_FRAGMENTS.iter().any(|f| col.contains(f)) {
                    continue;
                }
                seen.insert((table.clone(), col.clone()));
                if !declared.contains(&(table.as_str(), col.as_str())) {
                    undeclared.push(format!("{table}.{col}"));
                }
            }
        }
        undeclared.sort();
        assert!(
            undeclared.is_empty(),
            "blob-shaped column(s) that declare nothing about what they hold — add each \
             to ACTOR_BEARING_BLOB_COLUMNS (its bytes name actors: say what deletion and \
             succession do to them), to NON_ACTOR_BLOB_COLUMNS (they do not: say why), or \
             to BLOB_PROPERTY_COLUMNS (it is a digest/size/path/id, not the bytes). \
             ⚠ There is no fourth list: the backlog this walk landed with was drained \
             to zero on 2026-09-21 and its constant deleted, so that \"owed\" is not a \
             verdict a new column can take — read the writer and rule it. A \
             column-name walk cannot read inside a blob, so \
             this declaration is the only thing standing between a new blob-held actor \
             id and silence: {undeclared:?}"
        );

        // …and the converse, for the reason the vocabulary test exists at all:
        // a declaration naming a column the schema no longer has is a rule
        // that has quietly stopped applying. Feature-gated tables this
        // connection cannot create are exempt, as everywhere else in this
        // module.
        let mut vanished: Vec<String> = declared
            .iter()
            .filter(|(t, _)| !FEATURE_GATED_ELSEWHERE.contains(t))
            .filter(|(t, c)| !seen.contains(&((*t).to_string(), (*c).to_string())))
            .map(|(t, c)| format!("{t}.{c}"))
            .collect();
        vanished.sort();
        assert!(
            vanished.is_empty(),
            "blob declaration(s) naming a column the live schema does not have — a \
             ruling nothing applies is worse than no ruling, because it reads as \
             coverage: {vanished:?}"
        );
    }

    /// **The succession axis' own success bar, and the gate the
    /// `account_aliases` defect needed.** That row was missing from
    /// `record_succession` for weeks while every test stayed green, because the
    /// tests asserted a *hand-listed* set of tables — the same hand-listing the
    /// production code was wrong about. Several lists claimed to be the same
    /// set (the ceremony and the `seed_corpus` fixture whose doc comment says
    /// so outright among them) and none of them were equal.
    ///
    /// So this asserts nothing hand-listed. It seeds one row keyed to the
    /// retired actor into **every** registry table the in-memory schema
    /// supports, runs the real ceremony, and checks the observable against what
    /// that table *declares*:
    ///
    /// - [`Succession::Move`] — no rows left on the retired identity, rows on
    ///   the successor. A leg that is missing (`account_aliases`) or whose
    ///   predicate silently matches nothing (the hex/blob affinity class) fails
    ///   here.
    /// - [`Succession::Stay`] — rows still on the retired identity. This
    ///   direction matters as much: an over-broad `UPDATE` that swept
    ///   `actor_mls_pubkeys` along would seal fresh mail to a key the thief
    ///   holds, and it would fail here rather than in production.
    /// - [`Succession::Burn`] — gone from both.
    /// - [`Succession::Unruled`] — rows stay, because that is the status quo
    ///   this axis has not yet ruled on. It is asserted rather than skipped so
    ///   the backlog cannot quietly start moving rows nobody decided to move.
    /// - [`Succession::Partial`] — skipped, and the reason names the test that
    ///   owns the row-level rule.
    ///
    /// Tables whose synthetic single-row insert is refused by a CHECK/FK the
    /// seed cannot satisfy are reported, not silently dropped, and the sweep
    /// must still reach the large majority — the same anti-degradation bar
    /// [`deleting_an_actor_purges_every_seeded_table`] carries.
    #[tokio::test]
    async fn a_succession_moves_exactly_the_tables_the_registry_declares() {
        let db = CacheDb::open_in_memory().unwrap();
        let old: [u8; 32] = [0x41; 32];
        let new: [u8; 32] = [0x51; 32];

        let mut seeded: Vec<&ActorTable> = Vec::new();
        let mut skipped: Vec<&str> = Vec::new();
        let mut why_skipped: Vec<String> = Vec::new();
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT OR IGNORE INTO tiers (name, max_inbox_bytes, max_storage_bytes, \
                 max_devices, max_blob_size) VALUES ('free', 1, 1, 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO users (actor_id, tier, label, created_at, handle) \
                 VALUES (?1, 'free', 'the label', 1, 'alice')",
                rusqlite::params![old.as_slice()],
            )
            .unwrap();

            for entry in ACTOR_TABLES {
                if FEATURE_GATED_ELSEWHERE.contains(&entry.table) {
                    continue;
                }
                // The ceremony's own control surface: a seeded link row makes
                // the actor look already-succeeded and `record_succession`
                // refuses before writing anything. Skipped as an *input*, not
                // exempted from the axis — it is ruled `Stay` and its
                // behaviour is pinned by the succession tests directly.
                if entry.table == "actor_successions" {
                    continue;
                }
                match seed_one_row(&conn, entry, &old) {
                    Ok(()) => seeded.push(entry),
                    Err(why) => {
                        skipped.push(entry.table);
                        why_skipped.push(why);
                    }
                }
            }
        }

        // Named, not merely counted. A percentage floor cannot say *which*
        // tables it failed to reach, so a bounced insert used to sit below its
        // notice — declared and never observed. `SEED_BLIND` is the down-only
        // ratchet that fixes it: a new blind table trips here by name, in the
        // commit that blinded it, and a session ruling one of the listed tables
        // is told up front that its ruling has no witness.
        let mut skipped_sorted = skipped.clone();
        skipped_sorted.sort_unstable();
        assert_eq!(
            skipped_sorted,
            SEED_BLIND,
            "the generic seeder's blind spot changed. If a table joined it, the sweep \
             stopped observing a succession ruling and the ruling became a claim no gate \
             checks — fix the seed rather than widening the list. If one left it, delete \
             it from `SEED_BLIND`.\n\nWhy each skipped table bounced (read this before \
             theorising — the reason used to be a guess written into a comment, and it \
             was wrong about two of the three tables for months):\n  {}",
            why_skipped.join("\n  ")
        );

        db.record_succession(&old, &new, b"statement", 1)
            .await
            .unwrap()
            .expect("succession applies");

        let conn = db.conn.lock().await;
        let mut wrong: Vec<String> = Vec::new();
        for entry in &seeded {
            let (on_old, on_new) = (
                count_rows(&conn, entry, &old),
                count_rows(&conn, entry, &new),
            );
            let ok = match entry.succession {
                Succession::Move(_) => on_old == 0 && on_new >= 1,
                Succession::Stay(_) => on_old >= 1 && on_new == 0,
                Succession::Burn(_) => on_old == 0 && on_new == 0,
                Succession::Unruled => on_old >= 1,
                Succession::Partial(_) => true,
                // Reference-only verdict: nulling a pointer is not something a
                // row in this registry can declare. Fail loudly rather than
                // silently passing a mis-filed entry.
                Succession::Clear(_) => false,
            };
            if !ok {
                wrong.push(format!(
                    "{} ({:?}): {on_old} row(s) still on the retired identity, \
                     {on_new} on the successor",
                    entry.table,
                    match entry.succession {
                        Succession::Move(_) => "Move",
                        Succession::Stay(_) => "Stay",
                        Succession::Burn(_) => "Burn",
                        Succession::Partial(_) => "Partial",
                        Succession::Unruled => "Unruled",
                        Succession::Clear(_) => "Clear (MISFILED — reference-only)",
                    }
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "the succession did not do what these tables declare — either the \
             ceremony is missing a leg (the `account_aliases` defect: a Move that \
             did not move) or a declaration is wrong: {wrong:#?}"
        );
    }

    /// The backlog is a list of candidate defects of the `account_aliases`
    /// class, so it may shrink and never grow. A **new** per-actor table cannot
    /// join it at all: adding one without ruling it trips this immediately,
    /// which is the property that makes the registry succession's source of
    /// truth rather than a fourth hand list.
    ///
    /// **The ceiling is EXACT, and that tightening is load-bearing rather than
    /// tidiness — it is the one property the registry-driven executor took
    /// away.** While the two paths hand-wrote their legs and the registry merely
    /// *checked* them, a wrong declaration was caught by the data-driven gate:
    /// the leg still moved the rows, so an entry demoted to `Unruled` disagreed
    /// with the observable and failed loudly. Now that the legs are **generated
    /// from** the declaration, the two move together — demote `account_aliases`
    /// to `Unruled` and the executor simply stops moving it, which is exactly
    /// what `Unruled` asserts, so the gate passes. Mutation-verified 2026-08-12:
    /// that demotion reds only the two *hand-written* address tests, and every
    /// data-driven gate stays green. Most tables have no hand-written test.
    ///
    /// A ceiling with slack would therefore silently absorb a demotion. An exact
    /// count cannot: any table leaving the ruled set trips this test by name, so
    /// a ruling can only ever be *changed* deliberately. Lower it as tables are
    /// ruled (that is the whole job); never raise it.
    ///
    /// ⚠ **The assertion is `==`, and it was `<=` until 2026-08-12** — the
    /// paragraph above was written for an exactness the code did not have. The
    /// gap was not academic in either direction. A ruling landed without
    /// lowering the constant left real slack behind it, and the *next* session's
    /// demotion then passed silently — the precise failure this test names. And
    /// `<=` cannot enforce its own premise: it is exact only while someone
    /// remembers to keep it exact, which is the property being tested. `==`
    /// makes the bookkeeping compulsory rather than conscientious: a ruling that
    /// does not lower the constant reds here, in the same commit that made it
    /// wrong.
    #[test]
    fn the_unruled_backlog_only_shrinks() {
        // Row 76 deliberately did NOT bulk-sweep all ~150 — a wrong `Move` is a
        // security regression while a wrong `Stay` is the status quo — so the
        // remainder is worked outward from the mail/routing planes.
        // 40 → 36 on 2026-08-15: the curation cluster's four —
        // `feeds`, `feed_global_factors`, `labeler_subscriptions` Move(Plain),
        // `labelers` Stay. Its fifth member, `feed_contributors`, is
        // deliberately NOT ruled: its `author_id` names the *counterparty* a
        // feed collects rather than the row's owner, so it belongs to the
        // counterparty pass — see its entry.
        // 36 → 30 on 2026-08-15: the long tail's un-clustered remainder,
        // minus `content_reports`, which is ruled with the
        // `content_reports.reporter` rather than split across two passes.
        // `pending_actions` Partial (the first hazard on this axis that is a
        // TIMER — see its entry), `import_sessions`,
        // `obligation_action_records`, `handle_cooldowns` Move(Plain),
        // `audit_log` + `invite_requests` Stay.
        // 30 → 22 on 2026-08-15: the two FEATURE-GATED bridge planes —
        // `ap_*` (3) Move(Plain), `bluesky_*` (5) Burn. Same shape, opposite
        // verdicts, and the discriminator is who chose the external
        // destination: AP's is derived from the handle (deployment-fixed), the
        // Bluesky link's is supplied by whoever completed the OAuth flow. ⚠
        // Neither data-driven sweep can witness either ruling — both groups sit
        // in `FEATURE_GATED_ELSEWHERE`, so every verdict here stands on a
        // hand-written pin on its own product observable.
        // 22 → 20 on 2026-08-15: the counterparty columns'
        // two table entries, `content_reports` and `feed_contributors`.
        // 20 → 0 on 2026-08-15: the `bridge_*` twenty — THE BACKLOG IS
        // EMPTY. Fourteen `Move(Plain)` (the IMAP/CalDAV/CardDAV collection
        // plane's ten, which are the account's own resting content and move as
        // three per-protocol units because their sync clocks cannot disagree;
        // `bridge_submission_quota`, the limits-move rule verbatim;
        // `bridge_feed_subscriptions` and `bridge_search_policy`, the owner's own
        // curation; `bridge_restore_divergence`, a notice about data that moves).
        // Four `Burn` from one shared leg — the MSEK-derived credential and
        // key-material blobs, the class where a `Move` re-creates the
        // `account_aliases` defect on purpose. Two `Stay` — the bridge-reported
        // audit and session-close logs, testimony about the theft itself.
        //
        // ⚠ The map called this group feature-gated and therefore
        // unwitnessable. THAT WAS FALSE and it is the pass's cheapest finding:
        // `FEATURE_GATED_ELSEWHERE` names only the `ap_*` / `bluesky_*` /
        // `nostr_*` families, and every `bridge_*` table is created by the
        // ordinary `MIGRATIONS` block, so both data-driven sweeps walk them and
        // witness all fourteen moves for free. Only the burns needed hand pins —
        // for the standing reason that a burn leg's own table list, not the
        // declaration, is the executor.
        //
        // ⚠ **This constant reaching 0 does NOT retire this test.** Its live job
        // is the other direction: `==` reds the moment a NEW per-actor table
        // joins the registry without a succession verdict, which is the hole the
        // ratchet was built for and the only one still open.
        const UNRULED_EXACT: usize = 0;

        let unruled = ACTOR_TABLES
            .iter()
            .filter(|e| matches!(e.succession, Succession::Unruled))
            .count();
        assert_eq!(
            unruled, UNRULED_EXACT,
            "{unruled} tables are Succession::Unruled, the declared count is \
             {UNRULED_EXACT}. Above it: either a new per-actor table must declare what a \
             succession does with its rows, or a table that WAS ruled has been demoted \
             back to the backlog — the dangerous one, since the executor is driven from \
             these declarations, so a demotion silently stops moving the rows and no \
             other test will see it. Below it: a table was ruled without lowering this \
             constant in the same commit; lower it (never raise it) and the ruling is \
             recorded."
        );
    }

    /// The export axis's ratchet — the [`the_unruled_backlog_only_shrinks`]
    /// precedent applied to [`Export::Unreviewed`] (`account-data-plane.md` § Nest-side requirements item 1).
    ///
    /// `==` for the same reason as its precedent: a ceiling cannot enforce
    /// its own premise. Above the constant, either a migration added a table
    /// without ruling its export disposition, or a ruled table was demoted
    /// back to the backlog. Below it, a verdict landed without lowering the
    /// constant. Either way the count and the registry must move together in
    /// one commit.
    ///
    /// ⚠ The demotion direction *was* the less dangerous one only while the
    /// emission was unbuilt. Row 138 landed the registry-driven leg
    /// (2026-08-15), so a verdict now takes effect on the next export: a
    /// demotion to `Unreviewed` silently withdraws a table from the archive,
    /// and a promotion past this constant starts emitting rows. `Unreviewed`
    /// remains the safe *resting* state — absent from the export, as every
    /// table was before this axis — but "nothing emits from these verdicts"
    /// is no longer true and must not be re-derived from this comment.
    #[test]
    fn the_unreviewed_export_backlog_only_shrinks() {
        // The skeleton pass (2026-08-15) ruled only the evidence-backed
        // tranche: the secret-bearing planes row 130 itself named or that one
        // schema read confirms (nostr_accounts, recovery_escrow,
        // eviction_tokens, atproto_app_credentials, atproto_sessions —
        // WithheldSecret), and the two shaped domains with verified full
        // column coverage (contacts, inbox_modes — Shaped). Everything else
        // is deliberately Unreviewed: a wrong Withheld is the status quo,
        // a wrong Verbatim/Redacted/Shaped is a leak, so the tail drains by
        // individually-reasoned judgment passes, secret/credential planes
        // first.
        //
        // The KEY/CREDENTIAL/ESCROW plane (2026-08-15) went 151 → 140: the
        // two atproto key tables, the three bridge wrapped-blob tables, the
        // three subscription-tier key tables, plus
        // `nest_backup_keys`, `generation_escrow_wraps` and
        // `nostr_bunker_signers`. All eleven are `WithheldSecret` and all
        // eleven joined `SECRET_BEARING_TABLES` in the same commit, so the
        // runtime belt in `tests/export_api.rs` seeds and audits each one
        // rather than trusting this declaration.
        //
        // The BEARER/VERIFICATION-SECRET plane (2026-08-15) went 140 → 132 —
        // the eight tables the earlier key-blob sweep could
        // not see, because their secret is a redeemable *code*, a shared
        // *verification* secret, or a key sealed inside an otherwise
        // user-meaningful ledger row: `bridge_mls_snapshot_blobs`
        // (WithheldSecret, the neighbour that earlier sweep named and left);
        // `capability_grants`, `payment_claim_codes`, `payment_providers`,
        // `invite_codes`, `nostr_bunker_apps` (Redacted — the registry's FIRST
        // five `Redacted` verdicts, each dropping exactly the bearer/key column
        // and keeping the ledger `principles.md` § The user always controls
        // their data requires the owner be able to audit); and
        // `actor_epoch_seal_keys` + `share_tokens` (Verbatim — the two
        // look-alikes whose NAMES say secret and whose schemas say published
        // public keys and a revocation digest).
        //
        // Two guards landed with it because `Redacted` had never been used
        // before and both its failure modes are silent:
        // `every_redacted_omit_names_a_real_column` (a typo in `omit` matches
        // nothing and omits nothing) and the export axis joining
        // `no_reason_prose_spells_an_excised_kind`, whose walk had covered only
        // the deletion and succession reasons while the KEY/CREDENTIAL/ESCROW
        // plane's eleven export reasons were already shipping in the binary.
        //
        // The RECOVERY / PUBLIC-MATERIAL plane (2026-08-15) went 132 → 123
        // — the tables whose contents are published,
        // world-served, or a public-key tombstone, plus the two the plane's
        // recon had misclassified. Six `Verbatim`: `recovery_registrations` and
        // `recovery_pending_replacements` (signed public registrations — the
        // former's migration says outright that nothing in it is secret),
        // `revoked_device_grants` (public renewal keys the box refuses),
        // `device_authorizations` (already served unauthenticated to the world),
        // `atproto_oauth_grants` (the connected-apps ledger; its tokens live in
        // `atproto_sessions`, `WithheldSecret`) and `nostr_zap_signers` and
        // `backup_writer_grants` (designations whose authority is the row's
        // existence here, never knowledge of the id). Two are NOT `Verbatim`
        // and both were recon'd as such — see their entries:
        // `foreign_recovery_heads` names foreign identities rather than the
        // exporting actor (`WithheldOperational`), and `spam_model_holder_copies`
        // is recreatable derived data by its own migration (`WithheldDerived`),
        // which is also the axis's first use of each of those two variants.
        //
        // The MUA-FACING BRIDGE plane (2026-08-15) went
        // 123 → 107 — the sixteen `bridge_*` tables the DAV/IMAP collection
        // stack rests on, and the first cluster whose bulk is the owner's own
        // CONTENT rather than material to withhold. Twelve `Verbatim`: the
        // three IMAP placement tables (`bridge_imap_messages` — the index of
        // their mail, `bridge_imap_mailbox_state` — which IS the mailbox list,
        // so the folder names live there and nowhere else, and
        // `bridge_imap_subscriptions`), the four DAV collection tables
        // (`bridge_caldav_calendars`, `bridge_caldav_events`,
        // `bridge_carddav_addressbooks`, `bridge_carddav_cards` — the axis's
        // FIRST use of the sealed-columns clause: their sealed bodies ride as
        // ciphertext, because the export goes to the owner), the two
        // owner-expressed preferences (`bridge_feed_subscriptions`,
        // `bridge_search_policy`), the two auth-history logs
        // (`bridge_audit_events`, `bridge_session_close_events` — whose
        // `credential_id` is an identifier, not a credential) and
        // `bridge_restore_divergence` (a data-loss notice addressed to the
        // owner). Four `WithheldOperational`: the three RFC 7162 / RFC 6578
        // tombstone logs, whose only function is one MUA's incremental catch-up
        // against this box's modseq clock, and `bridge_submission_quota`, the
        // rate-counter archetype.
        //
        // ⚠ The cluster's transferable finding: **the succession axis rules the
        // IMAP three and the CalDAV three inseparable units, and that unit does
        // not transfer here** — a live sync clock can be corrupted by a partial
        // move, an archive is a snapshot with no clock. Written out at
        // `bridge_imap_expunged`.
        //
        // The mail CONTROL plane (2026-08-16) went
        // 107 → 93 — the settings, rules and addresses beside the
        // MUA-FACING BRIDGE plane's collections. Nine `Verbatim`: `account_aliases` (the account's
        // addresses), `actor_mail_serving`, `email_domain_users`,
        // `email_filters` (user-authored rules), `mail_account_settings`,
        // `mail_lists`, `mail_list_sends`, `message_scan_results` (what this
        // nest did to the owner's mail and why) and `imap_mailboxes` (table and registry entry dropped at schema 99).
        // Five `WithheldOperational`: `mail_list_account_daily_counter` (the
        // rate-counter twin of `bridge_submission_quota`), `auto_reply_log` (a
        // dedup memory whose sender is a hash), the two mail SPOOLS
        // (`forward_queue`, `outbound_mail_queue`) and
        // `expunged_uids` (table and registry entry dropped at schema 99).
        //
        // ⚠ Two rules this cluster added, both stated at their entries:
        // **(a) a superseded table inherits its successor's verdict** — the two
        // tables since dropped at schema 99 took the verdicts their live replacements got
        // in the MUA-FACING BRIDGE plane's ruling, so the archive cannot treat the same fact differently
        // by era. **(b) A transient spool is withheld even though it carries
        // the owner's own mail**: `raw_message` is PLAINTEXT while the durable
        // copy of that same message rests sealed in the mailbox, so exporting
        // the spool would mint an unsealed copy into an archive an eviction
        // token can fetch. `the_mail_spools_never_reach_the_archive` in
        // `tests/export_api.rs` is the belt, and it asserts the BYTES are
        // absent rather than the verdict — so a later promotion to `Verbatim`
        // reds regardless of how the declaration reads.
        //
        // The FAMILY plane (2026-08-16) went 93 → 81 — the
        // eleven `guardian_*` tables plus `guardianships`. Ten `Verbatim`, two
        // `WithheldOperational` (`guardian_mail_sent_msgids`,
        // `guardian_mail_correlated_origins` — the mail gate's own correlation
        // memory). ⚠ The cluster's decisive finding is structural rather than
        // per-table: **every entry in the family keys on `supervised_actor_id`,
        // so the whole plane reaches the WARD's archive and never the
        // guardian's** — the guardian-side columns are `SUCCESSION_REFERENCES`
        // entries, which the export never walks. That answers the
        // MUA-FACING BRIDGE plane's fact (4) for twelve tables at once, and
        // `the_guardian_family_exports_only_on_the_wards_own_column` keeps it
        // from decaying. Written out at `guardianships`.
        //
        // ⚠ The second finding is the one a name-sweep would miss: three tables
        // written by the same mail gate, guardianship-guarded and cascading
        // together, split two ways — `guardian_mail_allowlist` is the ward's
        // own correspondence set and rides, while the other two are the gate's
        // internal memory. And one of those carries a SPENDABLE token: a
        // delivery report naming a `guardian_mail_sent_msgids` row is delivered
        // rather than held (`family-safety.md:106`), so exporting it would
        // hand an eviction-token holder a key past a guardian control.
        //
        // The SCORING / REPORTING plane (2026-08-16) went
        // 81 → 71. Six `Verbatim` — `spam_preferences`, `spam_models`,
        // `spam_training_history`, `personalization_models` (sealed
        // client-side, and its migration calls the rows user-irrecoverable),
        // `feed_global_factors`, `engagement_events`. Four
        // `WithheldOperational` — `content_scores`, `sender_behavior`, and the
        // two reporting tables.
        //
        // ⚠ Three vocabulary rulings this cluster pins, all of them narrowing
        // a variant that was drifting wider: **(a)** `WithheldDerived` means
        // re-derivable from what the ARCHIVE carries (a claim about the
        // reader), not "the nest can rebuild it" — so the trending cache is
        // `Operational`; the two diverge exactly when the input is other
        // people's data. **(b)** A migration calling a table
        // "derived/re-creatable" is answering a RETENTION question ("does
        // ageing this out lose something irrecoverable?"), which is a different
        // question with a different answer — `spam_training_history` is
        // age-out-safe and still irreplaceable to its owner, so it rides.
        // **(c)** When another owner doc rules a plane's disclosure, this axis
        // DEFERS rather than re-deriving from the columns: both reporting
        // tables are withheld on `report-sharing.md`'s gate, and one of them
        // (`content_reports`) would have read as a clean `Verbatim` from its
        // schema alone.
        //
        // ⚠ And the plane's landmine: `sender_reputation` was keyed on the
        // person REPORTED, so an actor-keyed read returned other people's
        // testimony about the exporter — the first case in this axis where a
        // `Verbatim` would have disclosed a third party TO the exporting actor.
        // (The table left the registry with the federation reputation leg,
        // schema 109; the lesson stays.)
        //
        // The WEB-PUBLISHING plane (2026-08-16) went
        // 71 → 65. Four `Verbatim` — `web_files` (the source the owner
        // uploaded), `web_domains`, `web_subdomain_enabled`, `web_apex_actor`.
        // Two `WithheldDerived` — the two render caches, which are the axis's
        // SECOND and THIRD uses of that variant and the ones that make its test
        // precise: **is the input in the archive?** A render is a projection of
        // `web_files`, which rides `Verbatim` beside it with bodies under
        // `include_blobs`, so the owner lacks no data — where `content_scores`
        // (the SCORING / REPORTING plane) is computed from every other actor's engagement and can
        // never be re-derived from this owner's archive, which is why that one
        // is `Operational`. "The system can rebuild it" decides neither.
        // The SHAPED-DOMAIN plane (2026-08-16) went
        // 65 → 56 — the nine tables the eleven hand-written domains of
        // `export_routes.rs::gather_export_data` read, which the axis had left
        // for last because a shaped domain LOOKS like coverage. All nine are
        // `Verbatim`: `knocks`, `folders`, `sync_devices`, `groups`,
        // `group_members`, `key_packages`, `feeds`, `content` and
        // `content_links`. Not one of the eleven domains was total — every
        // single one drops columns, row-filters, or both — so `Shaped` was the
        // wrong verdict for all of them, and the plane's real finding is that
        // the shaped half of the export has been quietly partial the whole
        // time: `posts.json` omits every non-post row this actor authored,
        // `knocks.json` omits every knock already delivered, `folders.json`
        // omits twelve of eighteen columns.
        //
        // `Shaped` therefore keeps exactly its two proven-total entries
        // (`contacts`, `inbox_modes`) and gains `covers` + the walking guard
        // `every_shaped_verdict_covers_every_column`, so a partial shaped
        // domain is no longer expressible rather than merely discouraged.
        // The BACKUP/CUSTODY plane (2026-08-16) went 56 -> 51
        // — `backup_custody`, `backup_custody_generations`,
        // `backup_destinations`, `backup_custodian_checkins`, `restore_history`.
        // All five `Verbatim`, and four of them were decided by a sentence
        // whoever built the table had already written ("None of it is secret --
        // the user's own chosen backup targets"; "Nothing here is secret: it is
        // the owner's own device reporting its own progress"), which is the
        // RECOVERY / PUBLIC-MATERIAL plane's fact (2) paying out again.
        //
        // ⚠ The plane's one real trap is a word: the succession axis rules two
        // of these a `Burn` because each is "a PROJECTION" of an authority that
        // lives elsewhere, and `WithheldDerived` reads like the matching export
        // verdict. It is not. The WEB-PUBLISHING plane's test is *is the input in the
        // archive?* — and neither input is: `backup_destinations`' authority is
        // the client-sealed `fauna.state.backup` plane entries, whose content the
        // export never carries, and `backup_custodian_checkins`' input is a device's own
        // report. `Derived` is a claim about what the ARCHIVE'S READER can
        // reconstruct; "the system can rebuild it" is a different sentence.
        // The SUBSCRIPTION plane (2026-08-16) went 51 -> 45.
        // Five `Verbatim` — `subscribers`, `subscription_tiers`,
        // `tier_mls_groups`, `tier_transitions`, `subscribe_requests` — and one
        // `WithheldOperational`, `feed_contributors`.
        //
        // ⚠ The plane is where the SCORING / REPORTING plane's fact (1) finally pays out in the
        // PERMISSIVE direction, twice. `subscribers` and `subscribe_requests`
        // each name a second person, which is the shape that withheld the
        // reported-party plane (`sender_reputation`, since removed); here the
        // product already discloses them — the creator works the roster and the request queue by hand in their own
        // Tiers tab (`monetization.md` § Pillar 1) — so withholding would take
        // the commercial relationship the creator OWNS out of their own
        // records. Two-party is a question, never an answer.
        //
        // ⚠ And the plane's real catch is a table whose NAME and actor COLUMN
        // both lie: `feed_contributors.author_id` is not the feed's author but
        // a contributor the feed DISCOVERED, so an actor-scoped read returns
        // rows about the exporter sitting inside other users' discovery feeds.
        // The same `foreign_recovery_heads` shape the RECOVERY / PUBLIC-MATERIAL plane found, except this one matches
        // real rows and would really emit.
        // The ACCOUNT-RECORD plane (2026-08-16) went 45 -> 41 —
        // `actor_last_ip` `Verbatim`; `actor_message_dedup`, `feature_usage` and
        // `rpc_idempotency` `WithheldOperational`. The split is the plane's
        // point: a record of what the OWNER did rides, and the machinery the
        // nest runs about them does not.
        //
        // ⚠ Two tables of this plane are deliberately LEFT `Unreviewed`, which
        // is the ruling's own bias working rather than the cluster running out
        // of road. `audit_log` and `pending_actions` are both nest-wide
        // hash-chained records that NAME THIRD PARTIES in a `target` column and
        // are reachable under the export's weakest credential; ruling them is a
        // disclosure judgment about a transparency log, not a table read. The
        // recon is banked so the escalation costs one
        // sitting, not a re-derivation.
        //
        // ⚠ The cluster's durable find is a CORRECTION, not a verdict:
        // `audit_log`'s own policy and succession reasons both said `actor_id`
        // records "who an entry is about" / "the subject of admin actions taken
        // against" — and every call site passes the acting admin/guardian,
        // putting the subject in `target`. An export verdict read off that
        // wording would have been ruled on the wrong party. Both reasons are
        // fixed in place; the succession CONCLUSION is unaffected (a move
        // forges authorship rather than subjecthood).
        // The SYNC plane (2026-08-16) — five
        // `Verbatim` (`sync_changes`, `actor_channels`, `folder_member_access`,
        // `folder_channel_claims`, `content_uid_map`) and one
        // `WithheldOperational` (`channel_foreign_members`).
        //
        // ⚠ The plane's catch is the one the SUBSCRIPTION plane taught the registry to look
        // for, in its second and sharper form. `channel_foreign_members.actor_id`
        // names an actor homed on ANOTHER nest — the sole producer is the
        // federation-relay branch of welcome delivery — so unlike
        // `feed_contributors` it does not merely point at the wrong party, it
        // points at a party a local export can never match at all. Both halves
        // had to be ruled: the vacuity (why nothing leaks today) and the class
        // (why the verdict must not flip if a local row ever appears). Its
        // membership TWIN, `folder_member_access`, runs the same check and comes
        // out the other way — `actor_id` there is the grantee — which is the
        // point of running the check per table instead of per family.
        //
        // ⚠ And the plane's near-miss: `sync_changes.actor_id` is the RECORDING
        // writer, not always the set owner (multi-writer shared sets). It reads
        // the right way for a per-actor cut — the exporter gets the rows they
        // recorded — but a reader who takes it for "the set owner" would rule
        // the whole journal on a party the column does not name.
        //
        // (It landed beside the `audit_log` / `pending_actions` escalation
        // rather than after it — 41 -> 39 there, 39 -> 33 here — so neither
        // block's own arithmetic is this constant's history; the count is.)
        // The IDENTITY/LIFECYCLE plane (2026-08-16) went
        // 33 -> 27 — all six `Verbatim`: `actor_successions`,
        // `handle_cooldowns`, `admin_actor_ids`, `actor_mls_pubkeys`,
        // `actor_index_pubkeys`, `access_grants`. The first plane in four
        // clusters where the wrong-party check comes out clean on every table.
        //
        // ⚠ Its own trap replaces that one, and three of the six carry it: a
        // succession `Stay` reasoned from COMPROMISE says nothing about
        // disclosure. `actor_mls_pubkeys` / `actor_index_pubkeys` stay because
        // their keys seal FUTURE INBOUND mail and a seed thief can derive the
        // private halves — a question about who keeps receiving. The stored
        // bytes are the PUBLISHED halves, fetched by the MTA bridge over RPC.
        // Reading "compromise" as "withhold" would have withheld the public
        // key the nest hands to anyone who asks.
        //
        // ⚠ And the plane's structural find, recorded because a later session
        // will want to "fix" it: the SUCCESSOR's archive carries no succession
        // history. Their side of `actor_successions` is `new_actor_id`, which
        // lives in `SUCCESSION_REFERENCES` and the walk never touches — so the
        // slice that exists is the RETIRED identity's. Adding a second
        // `ACTOR_TABLES` entry on that column would emit the table's
        // `.ndjson` twice into one zip; the successor's view belongs in a
        // shaped domain if it is wanted.
        // The RESIDUAL set (2026-08-16) went 27 -> 19 —
        // `notifications`, `labeler_subscriptions`, `nest_pairings`,
        // `import_sessions`, `obligation_action_records` `Verbatim`;
        // `push_subscriptions` `Redacted`; `invite_requests` and `labelers`
        // `WithheldOperational`.
        //
        // ⚠ The wrong-column check reached its THIRD distinct form on
        // `labelers`: the SUBSCRIPTION plane found a column naming the wrong PARTY, the SYNC
        // plane one naming a party a local export can never match, and this residual sweep one naming a different
        // KIND OF IDENTITY — `publisher_actor` is a self-signed artifact
        // keypair, never an enrolled account. Do not read the family as
        // covered; read the column.
        //
        // ⚠ `invite_requests` is vacuous by a mechanism no other entry uses:
        // the EXPORT ENDPOINT'S OWN PRECONDITION excludes the population the
        // table can hold (`handle_export` requires `get_user` to resolve; every
        // row here names an actor with no `users` row). Worth remembering as a
        // reachability tool — some tables are answered by the door, not the
        // walk.
        //
        // ⚠ And `push_subscriptions` is the one place a succession reason DID
        // transfer, which is the exception that sharpens the IDENTITY/LIFECYCLE plane's rule: it
        // transferred because it was never an argument about succession, but
        // about what the `(endpoint, key_p256dh, key_auth)` triple IS — a
        // sendable push credential no kind can revoke. The test is whether the
        // neighbouring reason is about the BYTES or about the CEREMONY.
        //
        // The ATPROTO PDS plane (2026-08-17) went 19 -> 12
        // — six `Verbatim` (`atproto_account_settings`, `atproto_blobs`,
        // `atproto_identities`, `atproto_native_records`, `atproto_preferences`,
        // `atproto_retired_identities`) and `atproto_consent_requests`
        // `WithheldOperational`.
        //
        // ⚠ These seven are NOT feature-gated, correcting the ranking this
        // backlog carried for two clusters: `FEATURE_GATED_ELSEWHERE` names
        // fifteen tables and no `atproto_*` among them, so the plane was
        // visible to a bare `cargo test` all along and was the CHEAPEST
        // remaining work, not part of the expensive gated tail. The eleven
        // `ap_*` / `bluesky_*` / `nostr_*` tables are the gated set.
        //
        // ⚠ `atproto_consent_requests` is the first table that IS a ceremony,
        // which is the sharpest use yet of the residual sweep's test: the durable thing
        // the owner agreed to is an `atproto_oauth_grants` row (Verbatim), and
        // this table is only the minutes-long question that minted it — swept
        // by expiry however it was answered, so a DENIAL leaves no durable
        // trace anywhere. That last fact is the sweep's doing, not this
        // verdict's; `Verbatim` could not have recovered it.
        //
        // ⚠ And the plane's own presumption was already load-bearing before it
        // was ruled: `atproto_identity_key_blobs`' landed WithheldSecret reason
        // draws its line as "the repo it signs for exports under
        // `atproto_native_records`" — a neighbour's reason quietly depending on
        // an Unreviewed verdict going a particular way. Worth looking for when
        // draining the rest.
        //
        // The ELEVEN feature-gated bridge
        // tables (2026-08-17) went 12 -> 1 — seven `Verbatim` (`ap_follows`, `ap_post_map`,
        // `bluesky_dm_map`, `bluesky_interactions`, `bluesky_saved_feeds`,
        // `nostr_dms`, `nostr_follows`), two `Redacted` (`ap_accounts` minus
        // the RSA privkey, `bluesky_accounts` minus the three token BLOBs) and
        // two `WithheldOperational` (`bluesky_convos`,
        // `nostr_federation_cursors`).
        //
        // ⚠ **The plane's lesson is that three of its tables lie in their
        // column names, and only a writer read tells you which way.**
        // `bluesky_accounts`' three `BLOB NOT NULL` credential columns are
        // written EMPTY by their only writer; `bluesky_convos`' `fauna_convo`
        // mapping is written as the empty string and its `last_poll` /
        // `poll_interval` by nothing at all, which is what turned the ATPROTO PDS plane's
        // ceremony-or-thing question into a third answer (*neither* — a poll
        // watermark); and `bluesky_dm_map` has **no production writer at all**.
        // A verdict ruled off `CREATE TABLE` alone would have gone wrong on all
        // three, in a different direction each time.
        //
        // ⚠ And `ap_accounts` carries the same trap the IDENTITY/LIFECYCLE plane hit, inside ONE table: the
        // succession axis carries `encrypted_privkey` forward untroubled
        // (nest-KEK-sealed, so no seed thief read it — a COMPROMISE argument),
        // while this axis must drop it (an RSA key that signs as the actor — a
        // DISCLOSURE one). Two axes, one column, opposite answers, both right.
        //
        // ⚠ ELEVEN of these verdicts are invisible to a bare `cargo test`:
        // grade this plane with `--features nostr,bluesky` or the guards look
        // at nothing and say so only in `skipped_gated`.
        //
        // The final pass (2026-08-17) ruled `segment_records` `Verbatim`
        // and took the backlog to **ZERO**: every table in this registry now
        // carries an individually-reasoned export verdict, 151 of them ruled
        // across nineteen clusters. The ratchet stays — it is what stops a new
        // table joining a backlog that no longer exists, and `Export` is a
        // required field precisely so the omission is a compile error.
        //
        // ⚠ **Zero `Unreviewed` is NOT "the export is complete", and the
        // manifest must not be read that way.** `ActorExportSet::partial()` is
        // defined as "any table still Unreviewed", so it now reports false —
        // meaning *no open judgments remain*, while `withheld_tables` goes on
        // naming, by reason class, everything the nest holds and does not
        // export. Two real gaps survive this milestone and are captured rather than hidden by the flag: the mail/segment
        // BODIES (the export reaches no segment store; `include_blobs` is the
        // precedent for the mechanism that would), and the owner's `conv`
        // records (keyed by channel, so structurally outside a per-actor walk).
        const UNREVIEWED_EXACT: usize = 0;

        let unreviewed = ACTOR_TABLES
            .iter()
            .filter(|e| matches!(e.export, Export::Unreviewed))
            .count();
        assert_eq!(
            unreviewed, UNREVIEWED_EXACT,
            "{unreviewed} tables are Export::Unreviewed, the declared count is \
             {UNREVIEWED_EXACT}. Above it: either a new per-actor table must declare \
             what the per-actor export does with its rows (the migration's author \
             rules it — no new table joins the backlog), or a ruled table was demoted \
             back. Below it: a table was ruled without lowering this constant in the \
             same commit; lower it (never raise it) and the verdict is recorded."
        );
    }

    /// Every column an [`Export::Redacted`] verdict omits must be a column that
    /// table actually has.
    ///
    /// **The failure this exists for is silent in the worst direction.**
    /// `read_actor_rows` drops a column by `omit.contains(&name)` against the
    /// live `stmt.column_names()`, so a misspelled entry — `webhook_secrets`,
    /// `token`, a column renamed by a later migration — matches nothing,
    /// omits nothing, and emits the secret it was written to withhold. Nothing
    /// else notices: the archive gains a file that looks exactly like a correct
    /// `Verbatim` emission, and the reason string beside it still says the
    /// column was dropped. The BEARER/VERIFICATION-SECRET plane's ruling introduced the registry's first
    /// `Redacted` verdicts, so the guard lands with them.
    ///
    /// It **walks** rather than listing (the lesson): every `Redacted`
    /// entry is checked against the real schema, so a sixth verdict is covered
    /// by existing, and a migration that renames a column out from under an
    /// `omit` list reds here rather than in an export.
    ///
    /// ⚠ The skipped set is declared BY NAME, not counted — the shape cluster
    /// 1's belt had to learn (`export_api.rs`'s `UNSEEDABLE_IN_DEFAULT_BUILD`).
    /// `CacheDb::open_in_memory` builds the default flavor's schema, so a
    /// feature-gated table is absent and unverifiable here; letting that be
    /// implicit would mean a `Redacted` verdict on any gated table silently
    /// buying coverage it never had.
    #[test]
    fn every_redacted_omit_names_a_real_column() {
        // Absent from `bins/fauna-nest`'s default features, so this build's
        // schema cannot answer for them. `nostr_bunker_apps` lives in
        // `nostr/db.rs` behind the `nostr` feature — the same reason its
        // secret-bearing sibling sits in the belt's own skip list.
        //
        // `ap_accounts` and `bluesky_accounts` joined 2026-08-17 with the
        // eleven feature-gated bridge tables ruling, which put the plane's first two `Redacted` verdicts on
        // gated tables: all three bridges are opt-in (`default = ["store-safe",
        // "payments", "zaps"]`), so a default build cannot verify their `omit`
        // names — `--features nostr,bluesky,activitypub` can, and does. ⚠ Their
        // `omit` lists are the two most consequential in the registry (an RSA
        // signing key and three credential-shaped BLOBs), so a typo here is
        // exactly the failure this list exists to keep visible rather than
        // implicit.
        const ABSENT_FROM_DEFAULT_BUILD: &[&str] =
            &["ap_accounts", "bluesky_accounts", "nostr_bunker_apps"];

        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();
        apply_available_bridge_schemas(&conn);

        let mut checked = 0usize;
        let mut skipped: Vec<&str> = Vec::new();
        for entry in ACTOR_TABLES {
            let Export::Redacted { omit, .. } = entry.export else {
                continue;
            };
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({})", entry.table))
                .unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            if cols.is_empty() {
                skipped.push(entry.table);
                continue;
            }
            assert!(
                !omit.is_empty(),
                "{} is Export::Redacted with an empty `omit` — that is Verbatim \
                 wearing a redaction's reason string",
                entry.table
            );
            for name in omit {
                assert!(
                    cols.iter().any(|c| c == name),
                    "{}'s Redacted verdict omits `{}`, which is not one of its columns \
                     ({:?}). The omission is matched by name against the live schema, so \
                     this drops nothing and the column rides out in the export.",
                    entry.table,
                    name,
                    cols
                );
            }
            checked += 1;
        }

        skipped.sort_unstable();
        // ⚠ Under a bridge-feature build the gated tables ARE present, so
        // `skipped` is legitimately empty and the assertion below must not
        // read that as drift (the same
        // skip-on-absence-not-on-name correction the secret-shaped-column belt
        // took). An empty skip set means every Redacted verdict was checked,
        // which is strictly the stronger outcome.
        if !skipped.is_empty() {
            assert_eq!(
                skipped,
                ABSENT_FROM_DEFAULT_BUILD,
                "the set of Redacted tables this build's schema cannot see has changed. \
             Nothing verifies their `omit` names, so a typo in one would emit the \
             column it claims to drop: either a Redacted verdict landed on a \
             feature-gated table (name it above, deliberately) or a gated table \
             became unconditional (drop it from the list). Checked {checked} of {} \
             Redacted verdicts",
                checked + skipped.len(),
            );
        }
    }

    /// An [`Export::Shaped`] verdict asserts its named domain carries the
    /// WHOLE table, and this walk is what holds it to that.
    ///
    /// **The hole it closes is the one every other guard on this axis was
    /// built for, arriving one variant later.** `Shaped` says *these rows
    /// already reach the archive through `contacts.json`*, and the enum has
    /// always required the reason to account for every column and row-filter
    /// the domain drops. But the accounting was PROSE, so the claim was true
    /// of the columns on the day it was written and nothing re-checked it: an
    /// `ALTER TABLE` adds a column, the domain does not carry it, the verdict
    /// still reads correctly, and the archive silently stops being complete.
    /// The failure is quieter than the `Redacted` typo the BEARER/VERIFICATION-SECRET plane found —
    /// nothing leaks, so no belt fires; the user simply never receives data
    /// the nest holds, which is the invariant on the other side of
    /// `principles.md` § The user always controls their data.
    ///
    /// The SHAPED-DOMAIN plane found this live rather than hypothetically. `feeds`
    /// grew `scope`, `contributor_seeds` and `composition` by `ALTER` after
    /// `feeds.json` was written; the shaped domain matches the original
    /// `CREATE TABLE` exactly, so verifying coverage the obvious way — read
    /// the migration block — CONFIRMS a claim that is false. Nine tables were
    /// ruled `Verbatim` in that cluster for want of total domains; the two
    /// that remain are proven total, and this keeps them that way.
    ///
    /// It **walks** rather than listing (the lesson): the real schema
    /// decides, so the guard covers shaped verdicts that do not exist yet.
    #[test]
    fn every_shaped_verdict_covers_every_column() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();

        let mut checked = 0usize;
        for entry in ACTOR_TABLES {
            let Export::Shaped { domain, covers, .. } = entry.export else {
                continue;
            };
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({})", entry.table))
                .unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            assert!(
                !cols.is_empty(),
                "{}'s schema is invisible to this build, so its Shaped coverage \
                 claim cannot be checked at all. A Shaped verdict on a \
                 feature-gated table needs a deliberate skip entry here, the \
                 UNSEEDABLE_IN_DEFAULT_BUILD shape — there is no such case today.",
                entry.table
            );

            // The actor column is the exporter themselves; a domain never
            // carries it, and listing it would let a `covers` entry satisfy
            // the walk without the domain carrying anything.
            for name in covers {
                assert_ne!(
                    *name, entry.column,
                    "{}'s Shaped verdict lists its own actor column `{}` in \
                     `covers`. That column is the exporter and is implicit.",
                    entry.table, entry.column
                );
                assert!(
                    cols.iter().any(|c| c == name),
                    "{}'s Shaped verdict claims `{}` is carried by {}, but the \
                     table has no such column ({:?}) — a renamed or misspelt \
                     entry silently excuses a real column from the check.",
                    entry.table,
                    name,
                    domain,
                    cols
                );
            }

            let uncovered: Vec<&String> = cols
                .iter()
                .filter(|c| c.as_str() != entry.column && !covers.contains(&c.as_str()))
                .collect();
            assert!(
                uncovered.is_empty(),
                "{} is Export::Shaped on {}, which asserts that domain carries the \
                 whole table — but {:?} {} carried and unaccounted for. A shaped \
                 domain that drops user-meaningful data is the WRONG verdict (see \
                 the Export::Shaped docs): either extend {} to carry {:?} and add \
                 {} to `covers`, or rule the table Verbatim/Redacted so the \
                 registry-driven leg emits the row whole. Do NOT widen `covers` \
                 without widening the domain — `covers` is the claim, not the fix.",
                entry.table,
                domain,
                uncovered,
                if uncovered.len() == 1 {
                    "is not"
                } else {
                    "are not"
                },
                domain,
                uncovered,
                if uncovered.len() == 1 { "it" } else { "them" },
            );
            checked += 1;
        }

        assert!(
            checked >= 2,
            "expected at least the two proven-total shaped verdicts (contacts, \
             inbox_modes) to be checked; saw {checked}. A Shaped verdict \
             disappearing is fine, but this walk silently checking NOTHING is how \
             a guard rots into decoration."
        );
    }

    /// Column-name fragments that read as credential, key or bearer material.
    ///
    /// Deliberately broad and deliberately dumb: this list's job is to
    /// **notice**, never to judge. Every hit on an exporting table is either
    /// dropped by a `Redacted` verdict or individually cleared in
    /// [`CLEARED_EXPORTING_COLUMNS`] with the reason it is safe — the same
    /// declare-the-exceptions-by-name shape the KEY/CREDENTIAL/ESCROW plane gave
    /// `UNSEEDABLE_IN_DEFAULT_BUILD` and the BEARER/VERIFICATION-SECRET plane gave the `Redacted` walk.
    const SECRET_SHAPED_COLUMN_FRAGMENTS: &[&str] = &[
        "key",
        "secret",
        "token",
        "credential",
        "password",
        "passwd",
        "privkey",
        "private",
        "seed",
        "wrapped",
        "escrow",
        "nsec",
        "blob",
        // Ciphertext columns are here on purpose, and they are the fragments
        // that make this guard more than a name filter: the KEY/CREDENTIAL/ESCROW plane's
        // discriminator is *what the ciphertext IS, never whether it is
        // ciphertext*, so a new sealed column on an exporting table is exactly
        // the case that needs a human sentence — sealed CONTENT rides, a sealed
        // KEY never does, and the two are indistinguishable from the schema.
        "encrypted",
        "sealed",
        // Matches nothing today, and that is the point: the mail CONTROL plane withheld two
        // mail spools whose `raw_message` is a full RFC822 message in
        // plaintext, so a `raw_*` column arriving on a table that EXPORTS is
        // the shape to stop before it ships.
        "raw_",
    ];

    /// Every secret-shaped column name that a `Verbatim`/`Redacted` verdict
    /// nevertheless exports, with the reason it is not what its name suggests.
    ///
    /// A column arrives here by being read, not by being assumed: the entries
    /// are public keys, revocation digests, opaque identifiers, and sealed
    /// **content** (which rides by the sealed-columns clause —
    /// `account-data-plane.md` § Nest-side requirements item 1).
    const CLEARED_EXPORTING_COLUMNS: &[(&str, &str, &str)] = &[
        (
            "bridge_conversation_messages",
            "sealed_content",
            "the message BODY, sealed to the exporting actor's OWN recipient key in both \
             directions (the bridge seals inbound, the user's app the Sent copy; the Nostr \
             leg's ingest seals through the D2 resolver), so the owner reads their whole \
             history back with a key their client already holds. The nest cannot unseal it \
             and does not try. Withholding it would carry the fact of a DM plane and none \
             of the messages -- the ruling `nostr_dms.sealed_content` carried before schema 118",
        ),
        (
            "third_party_principals",
            "publisher_key",
            "the PUBLIC signing key a hosted plugin's publisher signs its manifest \
             with (`third-party.md` § The manifest) -- published in the document \
             itself, so exporting it reveals nothing; NULL on every row today",
        ),
        // Added 2026-09-21 with `notifications.body_key` (schema 71), which
        // landed red on `origin/main` -- the guard did exactly its job.
        (
            "notifications",
            "body_key",
            "an i18n CATALOG KEY -- the string-table identifier of the notification's \
             localized body, which the reader's app turns into a sentence in its own \
             language (`notifications.md` § Localized body; the column feeds \
             `LocalizedText::new` in `db/notifications.rs`). It matches only because \
             the fragment list matches substrings and `key` is a whole word here in \
             the wrong sense: the value is a name like a message id, never key \
             material, and its companion `body_args` holds the substitution strings. \
             Exporting it hands the owner the same text their own app already showed \
             them",
        ),
        (
            "rooms",
            "policy_blob",
            "the OWNER-SIGNED ROOM POLICY -- the room's name, its join rule, its \
             history policy and its admin set \
             (`conversation-rooms.md` § Roles and authorization). Not key \
             material and not sealed content: every member verifies this \
             signature and renders these fields, and in an end-to-end room the \
             same bytes ride the MLS group context, where a joiner receives \
             them in the Welcome before any commit. `blob` in the name means \
             `the signed bytes, opaque to SQLite`, not `wrapped secret` -- the \
             room's key material is the generation key of the recipient-set \
             scheme, which lives nowhere in this table. Exporting it hands the \
             owner back their own signed act",
        ),
        (
            "rooms",
            "labelers_blob",
            "the OWNER-SIGNED LABELER SET -- which labelers the room's owner or \
             admins chose to apply, as the signed \
             `fauna_mls::room_policy::SignedRoomLabelers` \
             (`conversation-rooms.md` § The three classes -> *What the home \
             nest does with its read*). The same shape as `policy_blob` \
             directly above and stored the same way -- a compare-and-set on a \
             version, never authored by the nest -- so `blob` here likewise \
             means `the signed bytes, opaque to SQLite`, not `wrapped secret`. \
             It is the room's own public choice rather than anything the nest \
             read: a revoke deliberately leaves it standing, so a nest the \
             members rotate back in resumes labelling under the set the room \
             already chose. The derived planes it feeds (`content_labels`, \
             `content_scores`) are rebuildable views, and the room's key \
             material is the recipient-set generation key, which lives nowhere \
             in this table. Exporting it hands the owner back their own signed \
             act",
        ),
        (
            "actor_epoch_seal_keys",
            "mls_pubkey",
            "a PUBLIC key -- the migration says 'Public keys only -- floor-safe; nothing here \
             opens content'",
        ),
        (
            "room_members",
            "reception_pubkey",
            "a PUBLIC key -- the public half of the member's X-Wing \
             group-reception keypair, the WRAP TARGET a room generation is \
             sealed TO (`account-data-taxonomy.md` § The recipient-set \
             scheme). Sealing to it is what every honest minter does with \
             it; the half that OPENS a wrap is the member's own reception \
             secret, which is client-held and appears in no nest table at \
             all. The nest's own entry in this column is its room-read \
             public key, published on `fauna.nest.info` for exactly this \
             purpose. Exporting it hands a member back the address other \
             members already wrap to",
        ),
        (
            "legal_takedown_deleted_posts",
            "blob_digests",
            "CONTENT-ADDRESS NAMES, not keys -- the concatenated 32-byte digests the \
             deleted post's record named (`Post::blob_refs`), captured by the delete \
             so the blob door can keep WITHHOLDING those blobs after the record is \
             unreadable. A digest names bytes and opens nothing; the bytes it names \
             are exactly what the pair leg and the door refuse to serve. Matched on \
             the word `digest` alone. The exporter already holds every one of them: \
             they are the attachment hashes of a post they authored",
        ),
        (
            "atproto_native_records",
            "rkey",
            "the ATProto RECORD KEY -- the last path segment of an at:// URI \
             (`3jzfci...`), the 'key' in the REST sense and half this table's \
             PRIMARY KEY. It names a record, it does not open one; the repo's \
             signing key is `atproto_authoring_keys.k_secret_wrapped` and the \
             DID's is `atproto_identity_key_blobs`, both WithheldSecret",
        ),
        (
            "bridge_audit_events",
            "credential_id",
            "an IDENTIFIER the reporting bridge chooses and `report_auth_event_handler` passes \
             through verbatim (`default`, a MUA's name) -- never the credential; the secret \
             halves rest in `bridge_wrapped_mls_blobs` / `bridge_wrapped_submission_tokens`, \
             both WithheldSecret",
        ),
        (
            "bridge_session_close_events",
            "credential_id",
            "the same bridge-chosen identifier as on `bridge_audit_events`, same writer",
        ),
        (
            "device_authorizations",
            "device_key",
            "the PUBLIC device key inside a signed delegation -- and the whole signed payload \
             carrying it is already served to the world unauthenticated at \
             `GET /api/v1/subscriptions/delegate/{author_id}`, which is why the \
             RECOVERY / PUBLIC-MATERIAL plane ruled this table Verbatim",
        ),
        (
            "bridge_caldav_calendars",
            "encrypted_metadata",
            "sealed CONTENT (the calendar's name/colour/timezone) under the owner's own read \
             key -- the sealed-columns clause, not key material",
        ),
        (
            "bridge_caldav_events",
            "encrypted_index_hint",
            "the sealed lookup hint over the same VEVENT -- derived FROM the content, sealed \
             under the same read key, opens nothing else",
        ),
        (
            "bridge_caldav_events",
            "encrypted_fauna_ext",
            "the sealed Fauna sidecar (RSVP refinement, per-attendee nest hints) -- content a \
             Fauna app wrote, never served to a CalDAV MUA and never a key",
        ),
        (
            "bridge_carddav_addressbooks",
            "encrypted_metadata",
            "sealed CONTENT (the address book's name/colour) under the owner's own read key",
        ),
        (
            "bridge_carddav_cards",
            "encrypted_index_hint",
            "the sealed lookup hint over the same vCard -- `bridge_caldav_events`' twin",
        ),
        (
            "bridge_carddav_cards",
            "encrypted_fauna_ext",
            "the sealed Fauna sidecar (e.g. the linkage to a social contact) -- content, not a key",
        ),
        (
            "web_domains",
            "verify_token",
            "the DNS challenge value the owner must PUBLISH in a TXT record to prove control of \
             the domain -- public disclosure by the owner is its whole purpose, and their app \
             must show it for the flow to complete",
        ),
        (
            "web_files",
            "blob_hash",
            "a content ADDRESS (the sync change's manifest hash the serve path walks), not a \
             key -- naming it opens nothing the blob store would not already serve to a \
             reader holding the manifest",
        ),
        (
            "web_files",
            "content_key_version",
            "a content-key GENERATION NUMBER (NULL = plaintext chunks), never key material -- \
             it records which generation a paywalled file's chunks were sealed under, and the \
             key itself lives in the tier's own plane",
        ),
        (
            "personalization_models",
            "sealed_blob",
            "a personalization MODEL sealed client-side under the owner's BackupKey -- sealed \
             content, and the nest has no train path and never holds a plaintext model at any \
             point, so it could not unseal for the export even in principle",
        ),
        (
            "spam_training_history",
            "sealed_subject",
            "the trained message's own subject, sealed to the actor's own recipient key on a \
             client-written row -- sealed content about the owner's own mail; the nest holds \
             only their public half and stores this verbatim-opaque",
        ),
        (
            "share_tokens",
            "filename_sealed",
            "a SEALED LABEL over the file's own name, for the author's own share list (the \
             migration says so): sealed content about the owner's file, and the recipient \
             path never reads this column",
        ),
        (
            "share_tokens",
            "key_envelope",
            "SEALED CONTENT, not a key: a private link's KeyEnvelope, AEAD-sealed by the \
             author's client under a link key that exists only in the URL the author copied \
             and rests nowhere on the nest -- the per-chunk keys inside open nothing without \
             it, and the nest serves these same bytes to anyone holding the link anyway",
        ),
        (
            "capability_grants",
            "holder_pubkey",
            "the PUBLIC key a grant is sealed to -- the ledger half the Redacted verdict \
             deliberately keeps so the owner can audit who holds what",
        ),
        (
            "recovery_registrations",
            "recovery_pubkey",
            "a PUBLIC key that already rides the actor's signed Profile (the migration says \
             so in as many words)",
        ),
        (
            "recovery_pending_replacements",
            "new_recovery_pubkey",
            "the PUBLIC half of a proposed replacement, published by the same signed route",
        ),
        (
            "revoked_device_grants",
            "auth_device_key",
            "the PUBLIC renewal key of a device the owner deleted -- a tombstone exists to be \
             matched and refused, which is the inverse of a credential",
        ),
        (
            "share_tokens",
            "token_id",
            "the BLAKE3 *of* the signed token, a revocation handle from which the bearer \
             artifact cannot be reconstructed",
        ),
        // --- The shaped-domain plane (2026-08-16). Five of
        // these are SEALED LABELS, which is the fragment list earning its keep:
        // each is sealed under the exporting actor's OWN root, so it is sealed
        // content riding in at-rest form, not a key. ---
        (
            "folders",
            "name_sealed",
            "a SEALED LABEL over this actor's own folder name, under this actor's own root -- \
             sealed CONTENT, which rides in at-rest form (the enum's sealed-columns clause); \
             the archive's reader is exactly who holds the key",
        ),
        (
            "folders",
            "include_paths_sealed",
            "a SEALED LABEL over this actor's own include list -- their local filesystem \
             layout, sealed under their own root. The sharpest item in the set and still \
             theirs; withholding an owner's own sealed rows would invert the invariant the \
             export serves",
        ),
        (
            "folders",
            "exclude_paths_sealed",
            "the exclude half of the same sealed label pair, same reason",
        ),
        (
            "folders",
            "retention_policy_sealed",
            "a SEALED LABEL over this actor's own retention policy, salted on name_hash -- \
             sealed content, not key material",
        ),
        (
            "sync_devices",
            "label_sealed",
            "a SEALED LABEL over this actor's own device name, under their own root -- the \
             `folders.name_sealed` shape one table over",
        ),
        (
            "sync_devices",
            "auth_device_key",
            "the PUBLIC renewal device key -- the same column `revoked_device_grants` clears \
             above, on the live row rather than the tombstone. Its private half is device-held \
             and `device_auth_core` verifies a fresh signature by it before the stored grant \
             is consulted at all",
        ),
        (
            "key_packages",
            "key_package_data",
            "a PUBLISHED MLS KeyPackage -- public by design and served to anyone: \
             `federation_handlers::keypackage_fetch_handler` answers any unauthenticated peer \
             nest. The private half is seed-derived and client-held, never in this table",
        ),
        (
            "content",
            "blob_hash",
            "a content-addressed digest of an offloaded payload, not key material -- the blob \
             itself rides only under `include_blobs`, behind the blob store's own rules",
        ),
        // --- The backup/custody plane (2026-08-16). Both
        // found BY the guard rather than by me: they are expand-phase sealed
        // companions the migration ALTERed on, which is the same shape that
        // made `feeds` misread one cluster earlier. ---
        // --- The subscription plane (2026-08-16). The
        // ML-KEM naming is the trap: an ENCAPSULATION key is the public half
        // (the decapsulation key is the secret one), and the fragment list
        // cannot tell them apart -- which is the guard working, not failing. ---
        (
            "subscribers",
            "mlkem_encaps_key",
            "the subscriber's ML-KEM ENCAPSULATION key -- the PUBLIC half, published so the \
             creator's client can wrap the tier period key TO them. The decapsulation half is \
             the subscriber's own and has never rested here; the period key itself is \
             client-custodied and never minted nest-side (`monetization.md` § Pillar 1). The \
             `actor_epoch_seal_keys.mls_pubkey` class",
        ),
        (
            "subscribe_requests",
            "mlkem_encaps_key",
            "the same public encapsulation half, carried on the request that precedes the \
             roster row",
        ),
        (
            "backup_custody",
            "path_sealed",
            "a SEALED LABEL over this row's own `path`, convergent under `path_hash` -- the \
             2026-07-29 paths-are-content expand phase (`file-sync.md` § Sealed names & paths), \
             the same class as `folders.name_sealed`. Sealed CONTENT under the exporting \
             actor's own root, so it rides in at-rest form; the plaintext `path` beside it \
             rides for the same reason, both being this actor's own file path",
        ),
        (
            "feeds",
            "contributor_seeds",
            "SEED in the peering sense, not the key sense: a JSON array of peer nest URLs a \
             discovery-scope feed exchanges with, validated as http(s) at the door \
             (feed_routes.rs:239) and fanned into `upsert_contributor`. The user's own \
             curation; no key material shares anything but the word",
        ),
        // --- The sync plane (2026-08-16). All three found
        // BY the guard, and the first of them is the one worth the friction:
        // `content_key_version` is the only column in the whole registry that
        // names a content key and is not one. ---
        (
            "sync_changes",
            "content_key_version",
            "a GENERATION NUMBER, not a key: the M2 content-key generation these chunks were \
             sealed under, stored opaque and echoed back on list so the reader selects \
             `key_for(version)` from keys it already holds (the migration says so at the \
             column). An integer that names a key is not one -- the `bridge_audit_events. \
             credential_id` class, one plane over",
        ),
        (
            "sync_changes",
            "path_sealed",
            "a SEALED LABEL over this row's own `path`, convergent under `path_hash` -- the \
             identical column `backup_custody` clears above, from the same 2026-07-29 \
             paths-are-content expand phase. Sealed CONTENT under the exporting actor's own \
             root, riding in at-rest form",
        ),
        // --- The skip-on-absence fix (2026-08-16). ⚠ These two
        // are the FIRST columns of a feature-gated exporting table this belt
        // has ever seen. Both verdicts shipped in earlier clusters with no
        // column guard able to look at them, because the skip was by NAME and
        // the schema was never seeded; the moment it became a skip on ABSENCE
        // with the bridge schemas applied, these surfaced. Both read clean --
        // which is the outcome to want and not the one to assume. ---
        (
            "nostr_bunker_apps",
            "app_pubkey",
            "the connected app's own PUBLIC key -- the npub-shaped identity a NIP-46 client \
             presents, which is what makes the Connected-apps roster legible (`which app is \
             this?`). It authorises nothing on its own: the pairing verifier is `secret_hash`, \
             which this table's Redacted verdict drops, and the signing half is the bunker \
             signer's `encrypted_privkey` in `nostr_bunker_signers` (WithheldSecret)",
        ),
        (
            "nostr_zap_signers",
            "signer_pubkey",
            "the designated zap signer's PUBLIC key, and the entry's own reason already says \
             so in as many words: `nothing here signs anything -- the signer holds its own \
             private half off this box entirely, which is the whole point of designating one`. \
             The `actor_epoch_seal_keys.mls_pubkey` class",
        ),
        (
            "nostr_oracle_clients",
            "client_pubkey",
            "a third-party principal's NIP-46 client PUBLIC key -- the `nostr_bunker_apps. \
             app_pubkey` class one table over. It authorises nothing on its own: every request \
             it signs is re-resolved against the principal row and the owner's live \
             `identity.op` grant, and the key the signer uses is the deposited nsec in \
             `nostr_accounts` (WithheldSecret)",
        ),
        // --- The residual set (2026-08-16). The second
        // pure name false positive in two clusters, after
        // `period_keys_rotated_at` -- and the belt is still right to ask. ---
        (
            "nest_pairings",
            "private_nest_id",
            "NOT A PRIVATE KEY -- `private` is the PAIRING ROLE. A pairing links a public nest \
             to a PRIVATE nest (the head/leaf topology), and this column is that nest's \
             identifier: the exporter's own paired destination, sitting beside `nest_url` which \
             names the same box in the clear and does not match any fragment at all. No key \
             material rests in this table",
        ),
        (
            "import_sessions",
            "source_sealed",
            "a SEALED LABEL over this row's own `source_descriptor` -- convergent under \
             `source_hash`, the same 2026-07-29 paths-are-content expand phase that produced \
             `sync_changes.path_sealed` and `backup_custody.path_sealed` cleared above. Sealed \
             CONTENT under the exporting actor's own root, so it rides in at-rest form; the \
             descriptor itself is a source LABEL (`gmail:imap.gmail.com:alice`), never a \
             credential -- the import's bearer lives bridge-side and has never rested here",
        ),
        (
            "export_sessions",
            "blob_bytes",
            "a SIZE -- the running byte count of the session's export blob, an integer the \
             wizard's summary card already shows the user (`mail-export.md` § UX shape step 5). \
             `blob` in the name is the artifact it measures. The two columns on this table that \
             ARE what the fragment list fears -- the wrapped session key and the internal file \
             handle -- are the `omit` of the table's `Redacted` verdict",
        ),
        // --- The identity/lifecycle plane (2026-08-16).
        // Two published halves and one column that is not a key at all —
        // the clearest demonstration yet that the fragment list is meant to
        // notice, never to judge. ---
        (
            "actor_mls_pubkeys",
            "mls_pubkey",
            "the recipient's X25519 PUBLIC encryption key, provisioned by their own client and \
             FETCHED BY THE MTA BRIDGE over `fauna.bridges.fetch_recipient_mls_pubkey` so it can \
             seal inbound mail at the perimeter -- published material by function. The private \
             half is MSEK-derived and client-held and has never rested here. The \
             `actor_epoch_seal_keys.mls_pubkey` class, whose migration says it in the same words",
        ),
        (
            "actor_index_pubkeys",
            "index_pubkey",
            "the same shape one table over: the index-hint PUBLIC key the bridge fetches via \
             `fauna.bridges.fetch_recipient_index_key`, held separately from the MLS pubkey so a \
             future deployment can scope index-builder access without granting body reads",
        ),
        (
            "sync_changes",
            "entry_sealed",
            "the SEALED class-2 state entry served inline on the feed row (W2.3 (account-data-plane.md § Workstreams)): the nest \
             stores and echoes it byte-for-byte and holds no key that opens one, and its AAD \
             binds it to its own writer/scope/item coordinates so no relay can splice it \
             elsewhere. Sealed CONTENT belonging to the exporting actor -- the sealed-columns \
             clause, and the archive's reader is exactly who holds the key",
        ),
        // --- Writer-signed change records (schema 92), found by the guard. ---
        (
            "sync_changes",
            "signer_key",
            "the 32-byte PUBLIC key the row's writer signature verifies under -- the writer's \
             store device principal key, or the actor id itself for a direct signature \
             (`change_signature.rs`; the migration says so at the column). Every \
             `changes.list` reply serves it to each reader of the set, and its private half \
             never reaches the nest",
        ),
        (
            "sync_signer_certs",
            "device_key",
            "the delegated signer's device principal PUBLIC key -- the same key \
             `sync_changes.signer_key` names and `sync_devices.auth_device_key` clears above; \
             its companion `cert` is the root-signed authorization the list replies already \
             publish",
        ),
        // --- The eleven feature-gated bridge tables
        // (2026-08-17). ⚠ **Every entry below is invisible to a bare
        // `cargo test`**, and that includes the `ap_*` ones: all THREE bridges
        // are opt-in in `bins/fauna-nest/Cargo.toml` (`default = ["store-safe",
        // "payments", "zaps"]`), so the grading command for this plane is
        // `cargo test -p fauna-nest --lib --features nostr,bluesky,activitypub
        // actor_tables`. With anything less the guards report green having
        // looked at nothing — which is what the skip-on-absence fix bought the
        // `skipped_gated` assertion to make loud. ---
        (
            "ap_accounts",
            "public_key_pem",
            "the PUBLIC half of the actor's HTTP-Signatures keypair, and public by function \
             rather than by convention: `GET /ap/users/{username}` serves it to any \
             unauthenticated caller in the fediverse (`activitypub/actor_routes.rs`, via \
             `minimal_person`/`fauna_profile_to_ap_person`) because no remote can verify a \
             signed delivery without it. The private half is `encrypted_privkey`, which this \
             table's Redacted verdict drops",
        ),
        (
            "bluesky_accounts",
            "token_expires",
            "an EXPIRY TIMESTAMP, not a token -- it matched on the word inside a column that \
             holds an integer. Written `0` by the same writer that writes the three token \
             BLOBs empty (`upsert_linked_account`), because the real OAuth session is keyed by \
             DID in `atproto_sessions` (WithheldSecret). A time, wearing a secret's name",
        ),
        (
            "nostr_follows",
            "nostr_pubkey",
            "the followed contact's npub: the same public identity one table over, here as the \
             list the owner curated with their own `petname` beside it. Nostr identities are \
             published by design -- the secret half of a Nostr keypair rests in \
             `nostr_accounts` / `nostr_bunker_signers`, both WithheldSecret",
        ),
    ];

    /// The family plane exports on the WARD's own column and on no other.
    ///
    /// **What this protects.** Guardianship rows name two people, and the
    /// bounded disclosure between them is ratified prose, not a schema
    /// property: `family-safety.md` § Don't do these fixes what a guardian may
    /// see at envelope metadata and category counts, and the guardian reads it
    /// through guardian-scoped RPCs. Today the export cannot cross that line by
    /// construction — every entry in the family keys on `supervised_actor_id`,
    /// and the guardian-side columns (`guardianships.guardian_actor_id`,
    /// `guardian_transfers.proposed_guardian_actor_id` / `initiated_by`) live
    /// in [`SUCCESSION_REFERENCES`], which `gather_export_set` never walks.
    ///
    /// That is exactly the kind of by-construction property that dies quietly.
    /// [`every_actor_shaped_column_is_registered_or_excluded`] pushes every
    /// actor-shaped column toward *some* registry, so a future session
    /// resolving that pressure by adding an `ACTOR_TABLES` entry on a
    /// guardian-side column would open a second disclosure channel — one this
    /// registry's own rules would then quietly bless, since a `Verbatim`
    /// verdict on a table whose rows are "the actor's" is the obvious reading.
    /// The failure is silent at every step, which is why it gets a test rather
    /// than a comment.
    ///
    /// ⚠ **Mutation-graded, and the grade corrected the claim above — read this
    /// before deciding the test is redundant.** Three shapes were run:
    /// swapping this entry's column to `guardian_actor_id` also reds
    /// [`every_actor_shaped_column_is_registered_or_excluded`] and
    /// [`succession_references_are_real_columns_outside_the_deletion_registry`];
    /// swapping it *and* deleting the reference entry still reds the first.
    /// Only the **self-consistent** refactor — swap the column, re-home the
    /// ward's column into [`SUCCESSION_REFERENCES`], drop the now-colliding
    /// guardian reference — satisfies every registry-hygiene guard, and there
    /// this test is the sole witness (37 others green). So the honest statement
    /// of its value: the hygiene guards catch a *half-done* move, and only this
    /// one catches the move a session would actually land. The transferable
    /// lesson is about grading, not about families: **a mutation that trips
    /// other guards has not shown your test is redundant — it has shown your
    /// mutation is incoherent. Mutate all the way to a state a real session
    /// would commit.**
    #[test]
    fn the_guardian_family_exports_only_on_the_wards_own_column() {
        const WARD_COLUMN: &str = "supervised_actor_id";
        let mut wrong: Vec<String> = Vec::new();
        let mut checked = 0usize;
        for entry in ACTOR_TABLES {
            if !entry.table.starts_with("guardian") {
                continue;
            }
            // A withheld verdict reads no rows at all, so only the emitting
            // verdicts can disclose. The count below still covers the whole
            // family, so a table leaving it is visible too.
            checked += 1;
            let emits = matches!(
                entry.export,
                Export::Verbatim | Export::Redacted { .. } | Export::Shaped { .. }
            );
            if emits && entry.column != WARD_COLUMN {
                wrong.push(format!("{} keys on {}", entry.table, entry.column));
            }
        }
        assert!(
            wrong.is_empty(),
            "these family tables would be exported on a column that is not the ward's own: \
             {wrong:?}. The guardian reads this family through guardian-scoped RPCs, whose \
             disclosure is bounded by `family-safety.md` § Don't do these (envelope metadata \
             and category counts, never content); an actor-keyed export on a guardian-side \
             column is a second channel with no such bound. If a guardian-facing export is \
             genuinely wanted, it is a new mechanism with its own ruling — not a column swap \
             here."
        );
        assert_eq!(
            checked, 12,
            "the guardian family is 12 registry entries; it is now {checked}. A table joining \
             or leaving it must be ruled against the ward-column invariant deliberately."
        );
    }

    /// A table the export **emits** must not carry an uncleared secret-shaped
    /// column.
    ///
    /// **The hole this closes was opened by the MUA-FACING BRIDGE plane and is structural, not
    /// hypothetical.** `SECRET_BEARING_TABLES` (the runtime belt's roster in
    /// `tests/export_api.rs`) is per *table*: it catches demoting a withheld
    /// table to `Verbatim`. Nothing watched the other direction — a later
    /// migration ALTERing a wrapped key, a bearer token or a deposited secret
    /// onto a table that already exports. Every such column would ride out on
    /// the next export, silently, with the table's verdict and reason still
    /// reading correctly, because the verdict was ruled against the columns of
    /// the day. Twenty-six tables emit today (the MUA-FACING BRIDGE plane alone added twelve, four
    /// of them in the same migration blocks as the wrapped-key blobs), so the
    /// surface is now wide enough that "the author will notice" is not a plan.
    ///
    /// This is a *noticing* guard, not a judging one: a hit is cleared by
    /// reading the column and saying why, or by omitting it in a `Redacted`
    /// verdict. The reverse check below keeps the clearance list from rotting
    /// into a silent widening — the `UNSEEDABLE_IN_DEFAULT_BUILD` lesson.
    #[test]
    fn an_exporting_verdict_never_carries_an_uncleared_secret_shaped_column() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();
        apply_available_bridge_schemas(&conn);

        let mut uncleared: Vec<String> = Vec::new();
        let mut seen_cleared: HashSet<(&str, &str)> = HashSet::new();
        let mut skipped_gated: Vec<&str> = Vec::new();

        for entry in ACTOR_TABLES {
            let omit: &[&str] = match entry.export {
                Export::Verbatim => &[],
                Export::Redacted { omit, .. } => omit,
                _ => continue,
            };
            // ⚠ Skip on ABSENCE, never on the name (the skip-on-absence fix). This
            // used to `continue` for anything in `FEATURE_GATED_ELSEWHERE`
            // unconditionally, which meant a gated table was unchecked even in
            // a build that had its schema — so `--features nostr,bluesky` bought
            // no coverage, and the plane where the guard is most needed (a
            // bridge table holding OAuth tokens and deposited keys) was the one
            // plane it never looked at. The list stays, as the statement of
            // WHICH absences are expected; presence is what decides.
            if FEATURE_GATED_ELSEWHERE.contains(&entry.table) && !table_exists(&conn, entry.table) {
                skipped_gated.push(entry.table);
                continue;
            }
            let stmt = conn
                .prepare(&format!("SELECT * FROM {} LIMIT 0", entry.table))
                .unwrap_or_else(|e| panic!("prepare column read for {}: {e}", entry.table));
            let cols: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
            for col in cols {
                if omit.contains(&col.as_str()) {
                    continue;
                }
                let lower = col.to_ascii_lowercase();
                if !SECRET_SHAPED_COLUMN_FRAGMENTS
                    .iter()
                    .any(|f| lower.contains(f))
                {
                    continue;
                }
                match CLEARED_EXPORTING_COLUMNS
                    .iter()
                    .find(|(t, c, _)| *t == entry.table && *c == col)
                {
                    Some((t, c, _)) => {
                        seen_cleared.insert((*t, *c));
                    }
                    None => uncleared.push(format!("{}.{}", entry.table, col)),
                }
            }
        }

        assert!(
            uncleared.is_empty(),
            "these columns are exported by a Verbatim/Redacted verdict and their names read \
             as credential or key material: {uncleared:?}. Read each one. If it is a public \
             key, an opaque handle or sealed CONTENT, clear it in \
             CLEARED_EXPORTING_COLUMNS with that reason; if it is actually a key, a bearer \
             token or a wrapped secret, it must be omitted by a Redacted verdict (or the \
             table withheld) — an export is retrievable with an eviction token, the weakest \
             credential the endpoint takes. Feature-gated tables this build cannot see are \
             not checked: {skipped_gated:?}"
        );

        // ⚠ The coverage itself is now a checked property, not a courtesy —
        // and it is here because the obvious version was MUTATION-SURVIVED
        // (the skip-on-absence fix, M13): deleting the `apply_available_bridge_
        // schemas` call above left every test green, because a guard that
        // silently stops looking looks exactly like a guard with nothing to
        // find. That is the withholding-class asymmetry one level up —
        // coverage that can be removed without a red — so the feature build
        // asserts it CAN see what the feature compiled in.
        //
        // ⚠ **Per-bridge, not whole-build** (tightened by the eleven feature-gated
        // bridge tables ruling, which put the first exporting verdicts on all three families at
        // once). The check used to be one `#[cfg(any(...))]` assertion that
        // `skipped_gated` was empty, which was right only while at most one
        // family had an exporting verdict: with all three ruled, a build of
        // `--features nostr` alone legitimately cannot see the `ap_*` and
        // `bluesky_*` tables, and the whole-build form would have blamed the
        // seeding for a bridge that was simply not compiled in. Asking the
        // question per bridge keeps the M13 property exactly — delete the
        // seeding call and every family this build DID compile goes invisible,
        // which reds — while staying quiet about the ones it did not.
        let expected_visible: Vec<&str> = skipped_gated
            .iter()
            .copied()
            .filter(|t| {
                if t.starts_with("ap_") {
                    cfg!(feature = "activitypub")
                } else if t.starts_with("bluesky_") {
                    cfg!(feature = "bluesky")
                } else if t.starts_with("nostr_") {
                    cfg!(feature = "nostr")
                } else {
                    // Not one of the three bridge families, so nothing here
                    // explains its absence — surface it rather than excuse it.
                    true
                }
            })
            .collect();
        assert!(
            expected_visible.is_empty(),
            "these tables' own bridge feature IS compiled into this build, so they must be \
             VISIBLE to this guard — {expected_visible:?} were skipped as absent instead. \
             Either the schema seeding stopped running (see `apply_available_bridge_schemas`) \
             or a bridge moved its `CREATE_TABLES_SQL`; until it is fixed, the export \
             verdicts on those tables have no column guard at all, which is the exact \
             blindness this pass removed. (Tables whose bridge is switched off are not \
             checked and are not listed here — this build skipped {skipped_gated:?} in all.)"
        );

        // The reverse direction: a clearance that no longer names a live
        // exported column is a hole that widens in silence — the column may
        // have been renamed by a migration (so its new spelling is unchecked)
        // or the table demoted out of the export (so the clearance now
        // pre-approves a name nothing walks).
        let stale: Vec<&str> = CLEARED_EXPORTING_COLUMNS
            .iter()
            .filter(|(t, c, _)| !seen_cleared.contains(&(*t, *c)) && !skipped_gated.contains(t))
            .map(|(t, _, _)| *t)
            .collect();
        assert!(
            stale.is_empty(),
            "these clearances no longer match a secret-shaped column on an exporting table, \
             so they approve nothing and hide a rename: {stale:?}"
        );
    }

    /// The same ratchet for [`SUCCESSION_REFERENCES`], which
    /// [`the_unruled_backlog_only_shrinks`] does not count.
    ///
    /// It earns a constant of its own rather than being folded into that one:
    /// the two lists answer different questions (*does this row belong to the
    /// actor* versus *does this row still name an identity that no longer
    /// exists*), and a combined count would let a table ruling silently pay for
    /// a reference column's demotion. Both are `assert_eq!` for the reason the
    /// table ratchet is — a ceiling cannot enforce its own premise.
    ///
    /// This list only became ratchetable once
    /// [`every_actor_shaped_column_is_registered_or_excluded`] could see second
    /// actor columns at all; before that a reference column did not have to be
    /// declared anywhere, so counting them would have counted only the ones
    /// somebody had already remembered.
    #[test]
    fn the_unruled_reference_backlog_only_shrinks() {
        // 3 → 9 on 2026-08-13: the vocabulary widening surfaced six columns that
        // had always named an actor and had never been visible to any gate. They
        // joined the backlog rather than being ruled in the pass that fixed the
        // walk — see their block in `SUCCESSION_REFERENCES`.
        //
        // 9 → 3 the same day: those six are ruled (four `Stay`, one `Move`, one
        // `Burn`). What is left is the three whose *tables* are themselves
        // `Unruled`, so ruling the reference alone would answer half a question.
        //
        // 3 → 5 on 2026-08-14, and it is the SAME event a second time: the
        // `sender` widening surfaced `knocks.sender_id` and
        // `notifications.sender_id`, two columns that had always named an actor
        // and were accounted for by nothing at all. A raise here is legitimate
        // for exactly one cause — the walk learning to SEE a column it could not
        // name before — and it is legitimate only because the raise is what makes
        // the column visible instead of absent. Any other raise is a demotion
        // wearing this comment's clothes. Both are the question (a
        // counterparty who succeeds), deliberately not answered by a pass whose
        // subject was the delivery plane's own rows.
        //
        // 5 → 4 on 2026-08-15: `labelers.caller_actor` is ruled `Move`
        // with its own table, which is the condition its deferral named. The
        // remaining four are all the counterparty question.
        const UNRULED_REFERENCES_EXACT: usize = 0;

        let unruled = SUCCESSION_REFERENCES
            .iter()
            .filter(|(_, _, s)| matches!(s, Succession::Unruled))
            .count();
        assert_eq!(
            unruled, UNRULED_REFERENCES_EXACT,
            "{unruled} reference columns are Succession::Unruled, the declared count \
             is {UNRULED_REFERENCES_EXACT}. Above it: a newly-surfaced reference column \
             joined the backlog without a ruling, or a ruled one was demoted. Below it: \
             a column was ruled without lowering this constant in the same commit."
        );
    }

    /// Every disposition that is a *judgement* names a reason, the same bar
    /// [`every_retain_carries_a_reason`] holds `Policy::Retain` to: the point of
    /// a declared axis is that a future reader can judge whether the ruling
    /// still holds, and "it stays" without a why is not a ruling.
    ///
    /// A plain `Move` is the only silent one, and deliberately: it says the leg
    /// is the same statement every other plain leg is, which the executor
    /// carries out uniformly and [`a_succession_moves_exactly_the_tables_the_registry_declares`]
    /// observes directly. A [`MoveShape::Bespoke`] is a judgement — it claims
    /// the plain statement is *not enough* — so it is held to the same bar as a
    /// `Stay` or a `Burn`.
    #[test]
    fn every_succession_ruling_carries_a_reason() {
        let reasons = ACTOR_TABLES
            .iter()
            .map(|e| (e.table, e.succession))
            .chain(SUCCESSION_REFERENCES.iter().map(|(t, _, s)| (*t, *s)));
        for (table, succession) in reasons {
            let reason = match succession {
                Succession::Stay(r)
                | Succession::Burn(r)
                | Succession::Partial(r)
                | Succession::Clear(r) => r,
                Succession::Move(MoveShape::Bespoke(r)) => r,
                Succession::Move(MoveShape::Plain) | Succession::Unruled => continue,
            };
            assert!(
                !reason.trim().is_empty(),
                "table {table} carries a succession ruling with an empty reason"
            );
        }
    }

    /// **`Partial` is the one disposition that switches the data-driven sweep
    /// OFF, so its promised witness must be named.**
    ///
    /// [`a_succession_moves_exactly_the_tables_the_registry_declares`] asserts
    /// `true` for a `Partial` table — deliberately, because a row-level
    /// predicate is not something the registry sweep can express. The variant's
    /// own doc pays for that by promising the table has "its own dedicated
    /// test"; until now nothing collected on the promise, and
    /// [`every_succession_ruling_carries_a_reason`] — the only other gate over
    /// these strings — checks solely that the prose is non-empty.
    ///
    /// That left `Partial` the sole escape hatch in this registry with no
    /// ratchet behind it. Its two siblings both have one: the unruled backlog is
    /// pinned at exactly zero (`the_unruled_backlog_only_shrinks`), and the
    /// seeder's blind spot is pinned by name (`SEED_BLIND`). A table declared
    /// `Partial` with no dedicated test would be **counted as observed** by the
    /// sweep, pass, and carry a row-level authority rule nothing checks.
    ///
    /// This gate is the missing ratchet. It was RED on landed code:
    /// `pending_actions` — a delayed destructive operation fired by an executor
    /// tick that authenticates nobody — named no test, while four of its five
    /// siblings did. Its rule *was* witnessed (tests in `successions::tests`), so
    /// the defect was the unpaid promise, not an uncovered table; naming them
    /// is what makes the next `Partial` unable to ship silent.
    ///
    /// The deletion axis's [`Policy::Partial`] is held to the same rule (since
    /// 2026-09-26, when `abuse_reports` became its second table): its walk
    /// skips the table exactly as the succession sweep does, so its reason
    /// names the owning test by a `…tests::` path — the legs' tests live in
    /// more than one module, so the module is part of the name.
    #[test]
    fn every_partial_ruling_names_its_dedicated_test() {
        let mut unwitnessed: Vec<&str> = Vec::new();
        let mut unwitnessed_deletion: Vec<&str> = Vec::new();
        for entry in ACTOR_TABLES {
            if let Succession::Partial(reason) = entry.succession
                && !reason.contains("successions::tests::")
            {
                unwitnessed.push(entry.table);
            }
            if let Policy::Partial(reason) = entry.policy
                && !reason.contains("tests::")
            {
                unwitnessed_deletion.push(entry.table);
            }
        }
        assert!(
            unwitnessed_deletion.is_empty(),
            "these tables declare `Policy::Partial` — which makes the purge walk's \
             registry loop skip them — without naming, by a `<module>::tests::<fn>` \
             path, the dedicated test that owns their row-level rule: \
             {unwitnessed_deletion:?}."
        );
        assert!(
            unwitnessed.is_empty(),
            "these tables declare `Succession::Partial` — which makes the \
             registry-driven sweep assert NOTHING about them — without naming \
             the dedicated test that owns their row-level rule: {unwitnessed:?}.\n\n\
             Name it as `successions::tests::<fn>` in the ruling's own reason \
             string, or change the disposition. A `Partial` whose witness is \
             unnamed is a rule no gate checks and no reader can find."
        );
    }

    /// A ruling's prose ships **in the binary**, so it may not spell a kind
    /// string the store-safe flavor is checked for having excised.
    ///
    /// `ACTOR_TABLES` is ungated — every flavor compiles it, including the
    /// store-safe nest, whose entire claim is that the payments/tips/zap planes
    /// are *completely compiled away* (`dynamic-features.md` § What "completely
    /// compiled away" means). `nest-store-safe-check` makes that claim
    /// falsifiable by `strings`-grepping the built binary for those planes' kind
    /// prefixes — and to `strings` a reason string is indistinguishable from a
    /// wire sender. The `payment_providers` ruling spelled the provider-set
    /// kind verbatim and reddened that gate.
    ///
    /// It reddened it *asynchronously*: the store-safe check is a heavy gate, so
    /// the break landed on `origin/main` and was found by a later session rather
    /// than by the author who wrote the sentence. This test is the cheap local
    /// half — it fails in the same `cargo test` run that writes the ruling.
    /// Naming the kind is still fine in a `//` comment; comments never reach the
    /// binary.
    ///
    /// ⚠ **It walks every reason-bearing axis, and that is why it is not named
    /// for one of them** (widened 2026-08-15). It covered
    /// only `Policy` and `Succession` while the export axis had been shipping
    /// eleven `WithheldSecret` reason strings since the KEY/CREDENTIAL/ESCROW plane's ruling — prose in the
    /// same binary, invisible to the same gate, caught by nothing. A fourth
    /// axis is exactly the thing a hand-listed walk forgets, so the match below
    /// is written to fail to compile rather than to skip silently when a fifth
    /// arrives: add the axis to the tuple, do not add a second test.
    #[test]
    fn no_reason_prose_spells_an_excised_kind() {
        // Verbatim from `nest-store-safe-check` (justfile), minus the regex
        // escaping — keep the two lists in step if that recipe grows a pattern.
        const EXCISED: &[&str] = &[
            "fauna.payments.",
            "fauna.tips.",
            "fauna.nostr.zap_signers.",
            "nostr.zaps.total",
        ];

        let prose = ACTOR_TABLES
            .iter()
            .flat_map(|e| {
                [
                    match e.policy {
                        Policy::Retain(r) | Policy::Partial(r) => Some((e.table, "deletion", r)),
                        Policy::Purge => None,
                    },
                    match e.succession {
                        Succession::Stay(r)
                        | Succession::Burn(r)
                        | Succession::Partial(r)
                        | Succession::Clear(r)
                        | Succession::Move(MoveShape::Bespoke(r)) => {
                            Some((e.table, "succession", r))
                        }
                        Succession::Move(MoveShape::Plain) | Succession::Unruled => None,
                    },
                    // The fourth axis. Exhaustive on purpose: a new `Export`
                    // variant carrying prose must fail to compile here rather
                    // than fall through a `_ => None` into the binary unwatched.
                    match e.export {
                        Export::Redacted { reason, .. } | Export::Shaped { reason, .. } => {
                            Some((e.table, "export", reason))
                        }
                        Export::WithheldSecret(r)
                        | Export::WithheldDerived(r)
                        | Export::WithheldOperational(r) => Some((e.table, "export", r)),
                        Export::Verbatim | Export::Unreviewed => None,
                    },
                ]
            })
            .flatten()
            .chain(
                SUCCESSION_REFERENCES
                    .iter()
                    .filter_map(|(t, _, s)| match s {
                        Succession::Stay(r)
                        | Succession::Burn(r)
                        | Succession::Partial(r)
                        | Succession::Clear(r)
                        | Succession::Move(MoveShape::Bespoke(r)) => Some((*t, "succession", *r)),
                        Succession::Move(MoveShape::Plain) | Succession::Unruled => None,
                    }),
            );

        for (table, axis, reason) in prose {
            for kind in EXCISED {
                assert!(
                    !reason.contains(kind),
                    "{table}'s {axis} reason spells `{kind}` — that literal is compiled \
                     into the STORE-SAFE nest, whose whole claim is that this plane is \
                     excised, so `nest-store-safe-check` will red on it. Move the kind \
                     name into a `//` comment above the entry; comments do not ship"
                );
            }
        }
    }

    /// A [`SUCCESSION_REFERENCES`] entry must be a real column that is
    /// deliberately **not** in the deletion registry — the two axes have
    /// different in-scope sets, and an entry drifting into both would mean one
    /// of the two lists is wrong.
    #[test]
    fn succession_references_are_real_columns_outside_the_deletion_registry() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();
        for (table, column, _) in SUCCESSION_REFERENCES {
            let pragma = format!("PRAGMA table_info({table})");
            let mut stmt = conn.prepare(&pragma).unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            assert!(
                cols.iter().any(|c| c == column),
                "{table}.{column} is declared in SUCCESSION_REFERENCES but the real \
                 schema has no such column"
            );
            assert!(
                !ACTOR_TABLES
                    .iter()
                    .any(|e| e.table == *table && e.column == *column),
                "{table}.{column} is in both ACTOR_TABLES and SUCCESSION_REFERENCES — \
                 the reference list is for columns that name an actor WITHOUT the row \
                 belonging to it, so one of the two is wrong"
            );
        }
    }

    /// The defect itself, end to end: a deleted account's row in a **hex-keyed**
    /// bridge table must actually be gone. Needs the `nostr` feature compiled,
    /// so it runs in `nest-lib-test-check`'s union-feature arm — the arm that
    /// exists because a feature-gated plane is otherwise never executed at all.
    ///
    /// Red against the pre-fix code: `purge_orphaned_actor_rows` bound the raw
    /// 32 bytes, SQLite compared a blob to a `TEXT` column, matched nothing,
    /// and returned success — leaving `nostr_accounts.encrypted_privkey`, the
    /// **deposited nsec**, at rest for a user who had asked to be deleted.
    #[cfg(feature = "nostr")]
    #[tokio::test]
    async fn purge_removes_rows_from_a_hex_keyed_bridge_table() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];
        let actor_hex = hex::encode(actor);
        {
            let conn = db.conn.lock().await;
            // The bridge's schema, applied the way `init_db` applies it
            // under the feature.
            crate::nostr::apply_schema(&conn).unwrap();
            crate::nostr::db::link_account(
                &conn,
                &actor_hex,
                "npub-test-pubkey",
                "nsec_deposited",
                Some(b"sealed-nsec-bytes"),
                None,
                None,
            )
            .unwrap();
            let seeded: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM nostr_accounts WHERE actor_id = ?1",
                    rusqlite::params![actor_hex],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(seeded, 1, "fixture failed to seed the hex-keyed row");
        }

        let deleted = db.purge_orphaned_actor_rows(&actor).await.unwrap();
        assert!(
            deleted >= 1,
            "the purge reported {deleted} rows — a hex-keyed table it walked was a no-op"
        );

        let conn = db.conn.lock().await;
        let left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nostr_accounts WHERE actor_id = ?1",
                rusqlite::params![actor_hex],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            left, 0,
            "the deleted account's nostr_accounts row survived — its encrypted_privkey \
             is the deposited nsec, so this is user key material left at rest"
        );
    }

    /// Every retained table names a non-empty reason — the whole point of
    /// `Policy::Retain` is that a future reader can judge whether the reason
    /// still holds, so an empty one is a lint failure, not a valid entry.
    #[test]
    fn every_retain_carries_a_reason() {
        for entry in ACTOR_TABLES {
            if let Policy::Retain(reason) | Policy::Partial(reason) = entry.policy {
                assert!(
                    !reason.trim().is_empty(),
                    "table {} is Policy::Retain or Policy::Partial with an empty reason",
                    entry.table
                );
            }
        }
    }

    /// The tables the generic seeder still cannot reach, and therefore the
    /// tables whose succession ruling **no gate observes** — a ruling on one of
    /// these is trusted, not tested, which is the state row 78 exists to get
    /// out of. Sorted; keep it that way.
    ///
    /// **Shrink this list; never grow it.**
    ///
    /// ⚠ **The `mail_lists` pair left this list 2026-08-15, and the mechanism
    /// this comment used to name was wrong.** It said all three bounce on a
    /// parent whose own **CHECK** swallows the `INSERT OR IGNORE`. For the two
    /// mail-list tables it was a **UNIQUE**: `account_aliases` carries
    /// `UNIQUE (local_domain, pattern, kind)` — three `TEXT` columns — and is
    /// itself seeded at depth 0, while [`dummy_value_sql`] salted only its
    /// `BLOB` literals by depth. So `mail_lists`' depth-1 plant of that parent
    /// regenerated the depth-0 row's three text values byte for byte, collided,
    /// was swallowed, and left the child to bounce against a parent that was
    /// never inserted. `OR IGNORE` skips *any* constraint violation, not just a
    /// CHECK — which is what made one mechanism look like another.
    ///
    /// `restore_history` is a different shape and stays: its chain is
    /// `restore_history → snapshots(id) → folders(id)`, and the reason it still
    /// bounces is now **reported by the gate** (see [`seed_one_row`]) rather
    /// than guessed at here. Read the assertion message before theorising.
    const SEED_BLIND: &[&str] = &[];

    /// **A parent plant that gets swallowed is REPORTED, not assumed.**
    ///
    /// `INSERT OR IGNORE` skips *any* constraint violation, so a plant that
    /// collided with an unrelated row is indistinguishable by rowcount from a
    /// parent that legitimately already existed — the case `OR IGNORE` is for.
    /// [`seed_row`] used to treat both as success, which is the whole reason
    /// [`SEED_BLIND`]'s comment could name the wrong mechanism (a `CHECK`) for
    /// two of its three tables and go unchallenged for months: the child bounced,
    /// the table was recorded as blind, and *why* was left to a guess.
    ///
    /// The synthetic schema makes the swallow unconditional rather than
    /// depending on the values [`dummy_value_sql`] happens to generate: `only_one`
    /// carries a default, so the seeder never binds it, so SQLite fills the same
    /// `1` that the pre-planted row already holds — and the `UNIQUE` refuses.
    #[test]
    fn a_swallowed_parent_plant_is_reported_not_assumed() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE p (id INTEGER PRIMARY KEY, only_one INTEGER NOT NULL DEFAULT 1 UNIQUE);
             CREATE TABLE c (id INTEGER PRIMARY KEY, p_id INTEGER NOT NULL REFERENCES p(id));
             INSERT INTO p (id, only_one) VALUES (999, 1);",
        )
        .unwrap();

        let err = seed_row(&conn, "c", &[("id", "7")], 0)
            .expect_err("the child must bounce: its parent plant cannot be inserted");
        assert!(
            err.contains("p:") && err.contains("swallowed by a constraint"),
            "the failure must name the PARENT that could not be planted and say the \
             plant was swallowed — a bare child-side error is what sent earlier \
             sessions guessing at mechanisms. Got: {err}"
        );
        assert!(
            err.starts_with("c:"),
            "and it must still lead with the table the sweep actually asked for. \
             Got: {err}"
        );
    }

    /// Seed one synthetic row keyed to `actor` into `entry`'s table, filling
    /// every other NOT NULL column with a dummy the schema will accept.
    ///
    /// `Err` carries the reason, and the callers print it. A refusal is
    /// **reported**, never silently swallowed — a constraint the synthetic row
    /// cannot satisfy shrinks the sweep, and both gates hold a floor on how much
    /// of the registry they actually exercised. Before 2026-08-15 the refusal
    /// was a bare `false` and the *reason* lived only in whatever comment a
    /// session had guessed at; the comment on [`SEED_BLIND`] had been wrong
    /// about two of its three tables for that entire time.
    ///
    /// One seeding routine on purpose: several is how the inventories that
    /// "claimed to be the same set" got to disagree in the first place.
    fn seed_one_row(
        conn: &rusqlite::Connection,
        entry: &ActorTable,
        actor: &[u8; 32],
    ) -> std::result::Result<(), String> {
        // The encoding the registry declares, not a guess: a blob bound against
        // a TEXT column matches nothing in SQLite, so seeding the wrong
        // spelling would make the gate pass vacuously on exactly the bridge
        // tables the affinity trap bites.
        let keyed = match entry.key {
            ActorKey::Blob => format!("x'{}'", hex::encode(actor)),
            ActorKey::Hex => format!("'{}'", hex::encode(actor)),
        };
        seed_row(conn, entry.table, &[(entry.column, keyed.as_str())], 0)
    }

    /// Insert one synthetic row into `table`, pinning `keyed` if given, and
    /// **satisfying whatever the schema demands of it first** — the parents its
    /// foreign keys name, and the literals its CHECK enumerations allow.
    ///
    /// **Why the generic seeder grew this, 2026-08-12.** Its refusals were
    /// reported rather than swallowed (that part was right), but nothing read
    /// the report: both gates only hold an 80% floor, so a table whose insert
    /// bounced sat *below* the floor's notice — declared, never observed. The
    /// probe that found it counted **14** such tables, and the class is exactly
    /// the wrong one to leave blind: `email_domain_users` is a mail *route*,
    /// `current_key_blobs` is key material, `subscribers` is
    /// an entitlement. A ruling on any of them was a claim no gate could check,
    /// which is the "declared but unobserved" gap the axis exists to close, one
    /// level up. Nine bounced on a FOREIGN KEY and five on a CHECK
    /// enumeration — both mechanical, so neither is a reason to trust a ruling
    /// instead of testing it.
    ///
    /// `depth` bounds the parent walk; a self-referencing or cyclic FK gives up
    /// rather than recursing, and the caller records the table as skipped
    /// exactly as before.
    fn seed_row(
        conn: &rusqlite::Connection,
        table: &str,
        keyed: &[(&str, &str)],
        depth: usize,
    ) -> std::result::Result<(), String> {
        if depth > 4 {
            return Err(format!(
                "{table}: foreign-key chain deeper than the depth-4 bound"
            ));
        }
        let cols = table_columns(conn, table);
        if cols.is_empty() {
            return Err(format!("{table}: no such table in this schema"));
        }
        let ddl = table_ddl(conn, table);

        // What distinguishes THIS plant of `table` from another plant of the
        // same table for a different child (see `dummy_value_sql`). Zero at
        // depth 0, where the literals must stay byte-identical to their
        // pre-2026-08-15 values.
        let pin_salt: u16 = if depth == 0 {
            0
        } else {
            keyed
                .iter()
                .flat_map(|(col, literal)| col.bytes().chain(literal.bytes()))
                .fold(0u16, |a, b| a.wrapping_mul(31).wrapping_add(b as u16))
        };

        // Decide every literal first: the FK parents below must be seeded with
        // the same values the child is about to bind, or the child still
        // bounces.
        let mut bound: Vec<(String, String)> = Vec::new();
        for (i, (name, decl_type, notnull, has_default, pk)) in cols.iter().enumerate() {
            if let Some((_, literal)) = keyed.iter().find(|(k, _)| k == name) {
                bound.push((name.clone(), (*literal).to_string()));
                continue;
            }
            // An INTEGER PRIMARY KEY is nullable-by-omission (a rowid alias),
            // but a TEXT one is not, and it carries no NOT NULL flag.
            // A nullable column a `CHECK` requires (`folders.name`: NULL only
            // beside its hash and seal) is bound like a `NOT NULL` one, or the
            // row bounces on the check.
            let checked = ddl.contains(&format!("{name} IS NOT NULL"));
            if !(*notnull && !*has_default) && !*pk && !checked {
                continue;
            }
            let value = allowed_literal(&ddl, name)
                .unwrap_or_else(|| dummy_value_sql(decl_type, table, i, depth, pin_salt));
            bound.push((name.clone(), value));
        }

        // Grouped by FK id, because a **composite** foreign key is one
        // constraint reported as several rows: pinning its columns into
        // separate parent rows satisfies neither half, which is what kept the
        // whole `subscription_tiers(author_id, name)` family — `subscribers`,
        // `subscribe_requests`, `current_key_blobs` — out of the
        // sweep.
        let mut parent_failures: Vec<String> = Vec::new();
        for (parent, pairs) in foreign_keys(conn, table) {
            if parent.eq_ignore_ascii_case(table) {
                continue;
            }
            let mut pins: Vec<(String, String)> = Vec::new();
            for (from_col, to_col) in pairs {
                let Some((_, literal)) = bound.iter().find(|(n, _)| *n == from_col) else {
                    continue;
                };
                let Some(to_col) = to_col.or_else(|| primary_key_column(conn, &parent)) else {
                    continue;
                };
                pins.push((to_col, literal.clone()));
            }
            if pins.is_empty() {
                continue;
            }
            let pins: Vec<(&str, &str)> =
                pins.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            // A parent we cannot satisfy leaves the child to bounce. That
            // outcome is unchanged — what changed (2026-08-15) is that
            // `seed_row` now tells the truth about whether the parent is really
            // there (see its tail), and that the truth is **carried up** instead
            // of dropped on the floor. The child's own insert still decides;
            // this only means the eventual failure can name the parent that
            // caused it rather than leaving a session to guess, which is how
            // `SEED_BLIND`'s comment came to be wrong about two of its three
            // tables for months.
            if let Err(why) = seed_row(conn, &parent, &pins, depth + 1) {
                parent_failures.push(why);
            }
        }

        let names: Vec<&str> = bound.iter().map(|(n, _)| n.as_str()).collect();
        let values: Vec<&str> = bound.iter().map(|(_, v)| v.as_str()).collect();
        // `OR IGNORE` only for the parents: a parent may legitimately already
        // exist (two children naming the same one), while the sweep's own row
        // must never be silently skipped — that would report a table as seeded
        // when nothing was inserted, the exact vacuity this gate exists to
        // prevent.
        let verb = if depth == 0 {
            "INSERT"
        } else {
            "INSERT OR IGNORE"
        };
        let sql = format!(
            "{verb} INTO {} ({}) VALUES ({})",
            table,
            names.join(", "),
            values.join(", ")
        );
        let blame = |what: String| {
            if parent_failures.is_empty() {
                format!("{table}: {what}")
            } else {
                format!(
                    "{table}: {what} (after failing to plant its parent(s): {})",
                    parent_failures.join("; ")
                )
            }
        };
        let inserted = match conn.execute(&sql, []) {
            Ok(n) => n,
            Err(e) => return Err(blame(e.to_string())),
        };
        if inserted > 0 {
            return Ok(());
        }
        // Nothing was inserted. At depth 0 that cannot arise — the sweep's own
        // row uses a bare `INSERT`, whose only nothing-inserted outcome is the
        // error handled above.
        //
        // At depth >= 1 the `OR IGNORE` swallowed something, and the two cases
        // look identical from the rowcount alone: the parent may legitimately
        // already exist (two children naming the same one — the case `OR IGNORE`
        // is *for*), or the plant may have collided with an unrelated row on a
        // constraint it cannot satisfy, leaving no parent carrying the key the
        // child is about to bind. Reporting success for both is what made the
        // second case invisible: the child bounced, the table joined
        // `SEED_BLIND`, and the reason was recorded as a guess that stood for
        // months. So ask the question the caller actually has — *is there now a
        // row carrying the pinned key?* — instead of inferring it from a
        // rowcount that cannot tell the two apart.
        if keyed.is_empty() {
            return Err(blame(
                "INSERT OR IGNORE matched nothing and the row carries no pinned key \
                 to verify against"
                    .to_string(),
            ));
        }
        let predicate = keyed
            .iter()
            .map(|(col, literal)| format!("{col} = {literal}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        match conn.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE {predicate})"),
            [],
            |r| r.get::<_, bool>(0),
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(blame(format!(
                "INSERT OR IGNORE was swallowed by a constraint and no row carries \
                 the pinned key ({predicate}) — the child that needs this parent \
                 will bounce"
            ))),
            Err(e) => Err(blame(e.to_string())),
        }
    }

    /// `(name, declared type, NOT NULL, has default, is primary key)`.
    #[allow(clippy::type_complexity)]
    fn table_columns(
        conn: &rusqlite::Connection,
        table: &str,
    ) -> Vec<(String, String, bool, bool, bool)> {
        let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
                r.get::<_, Option<String>>(4)?.is_some(),
                r.get::<_, i64>(5)? != 0,
            ))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// One entry per foreign key — `(parent table, [(child column, parent
    /// column)])`, the parent column `None` when the FK names the parent's
    /// primary key implicitly. `PRAGMA foreign_key_list` reports a composite key
    /// as one row per column sharing an `id`; they are grouped back here so a
    /// caller cannot mistake half a constraint for a whole one.
    /// **A foreign key can turn any succession leg into a total ceremony
    /// failure, and nothing else in this file could see it.** Every gate beside
    /// this one judges a ruling by its *observable* — did these rows land on the
    /// successor — which presumes the transaction commits. A foreign key breaks
    /// that presumption from outside the registry entirely: `PRAGMA
    /// foreign_keys = ON` (`db/mod.rs`) makes an `UPDATE` of a referenced column,
    /// or a `DELETE` of a referenced row, **abort the whole transaction** while a
    /// child still points at the old value.
    ///
    /// That is strictly worse than any mis-ruling. A wrong `Stay` is the status
    /// quo and a wrong `Move` is a security regression, but an abort denies the
    /// ceremony *itself* — the user cannot rotate away from a compromised key at
    /// all — and it does so for a table whose own ruling may be perfectly
    /// correct. It shipped exactly once, on
    /// `bridge_service_users.approved_by_actor_id → admin_actor_ids(actor_id)`:
    /// any admin who had approved a bridge was locked out of succession, and out
    /// of ordinary de-admin too.
    ///
    /// **Both repair orders fail, so this cannot be fixed by sequencing the
    /// legs** — updating the parent orphans the children, updating the children
    /// first points them at a parent that does not exist yet, and `UPDATE OR
    /// IGNORE` does *not* swallow a foreign-key violation the way it swallows a
    /// uniqueness one. A coupled family therefore needs
    /// `PRAGMA defer_foreign_keys` inside the transaction, or no FK at all.
    ///
    /// ⚠ **The live warning this test exists to carry forward:** the whole
    /// `subscription_tiers(author_id, name)` family — `subscribers`,
    /// `subscribe_requests`, `current_key_blobs` — is coupled
    /// this way and sits `Unruled` today, which is the only reason it is quiet.
    /// Ruling any one of them `Move` without deferral does not move rows; it
    /// **breaks the ceremony for every author who ever created a tier**. That
    /// plane is the one the backlog row recommends taking next, so this test
    /// fails the moment a future session rules it, rather than after.
    #[test]
    fn no_foreign_key_crosses_a_succession_leg() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();

        let hazards = succession_fk_hazards(
            &conn,
            &registry_touched(std::iter::empty()),
            &registry_actor_columns(),
            COUPLED_MOVE_FAMILIES,
        );
        assert!(
            hazards.is_empty(),
            "a foreign key crosses a succession leg — the ceremony will ABORT, not \
             merely mis-sort rows. Three ways out, in order of preference: drop the \
             FK (if it binds an audit column to a live authority row, as \
             `approved_by_actor_id` did); rule the whole coupled family together and \
             declare it in `COUPLED_MOVE_FAMILIES`, which is what makes the executor \
             move it under `PRAGMA defer_foreign_keys`; or leave the \
             tables `Unruled`. Declaring a family WITHOUT ruling every member `Move` \
             does not help — the commit then aborts instead of the statement. \
             Hazards:\n  {}",
            hazards.join("\n  ")
        );
    }

    /// The registry's succession verdicts as the hazard walk reads them, with
    /// `overrides` applied on top — which is how a test states *"suppose this
    /// plane were ruled `Move`"* without editing the registry.
    ///
    /// **Every declaration for a table is kept, not just the last one.** A table
    /// may be ruled on two different columns — once in [`ACTOR_TABLES`] for the
    /// column it owns, once in [`SUCCESSION_REFERENCES`] for a column that names
    /// somebody else — and the two verdicts are independent. Collecting them
    /// into a `HashMap<&str, &Succession>` keyed on the table **name** silently
    /// kept whichever landed second, so one ruling shadowed the other; the walk's
    /// own comment below already claimed the opposite (*"a table in BOTH lists
    /// keeps the conservative reading"*), which is the semantics this shape makes
    /// true rather than merely asserted.
    ///
    /// ⚠ **Latent rather than live when this was fixed (2026-08-15), and worth
    /// stating so a future reader does not go looking for the outage.** Thirteen
    /// tables sit in both lists, but `Stay`/`Unruled` are filtered out *before*
    /// the collect and so can never shadow anything; of the rest only
    /// `payment_claim_codes` disagreed with itself — `Move(Bespoke)` in
    /// [`ACTOR_TABLES`] (the claim ledger moves and is voided in part) against a
    /// plain `Move` in [`SUCCESSION_REFERENCES`] (`redeemed_by`) — which flipped
    /// `parent_rows_die` from true to false. No child table carries a foreign key
    /// to `payment_claim_codes` today, so the flipped verdict hid no real
    /// crossing. It would have hidden the first one added.
    ///
    /// An override **replaces** a table's declarations rather than joining them:
    /// its whole purpose is to state *"suppose this table were ruled X"*, which a
    /// union would turn into *"suppose it were ruled X as well as what it already
    /// says"* — a different and much weaker claim.
    fn registry_touched<'a>(
        overrides: impl Iterator<Item = (&'a str, &'a Succession)>,
    ) -> std::collections::HashMap<&'a str, Vec<&'a Succession>> {
        registry_touched_from(
            ACTOR_TABLES.iter().map(|e| (e.table, &e.succession)),
            SUCCESSION_REFERENCES.iter().map(|(t, _, s)| (*t, s)),
            overrides,
        )
    }

    /// [`registry_touched`] with both registry lists injected, for the same
    /// reason `execute_plain_moves_with` and `execute_coupled_family_moves_with`
    /// have their seams: the shadowing this shape prevents is invisible in the
    /// real data (it is latent, see above), so the only way to pin it is to hand
    /// the fold a synthetic pair that disagrees — and to hand it in **both
    /// orders**, since order is exactly what the defect was sensitive to.
    fn registry_touched_from<'a>(
        owned: impl Iterator<Item = (&'a str, &'a Succession)>,
        references: impl Iterator<Item = (&'a str, &'a Succession)>,
        overrides: impl Iterator<Item = (&'a str, &'a Succession)>,
    ) -> std::collections::HashMap<&'a str, Vec<&'a Succession>> {
        // `Unruled` and `Stay` touch nothing, so they cannot abort anything.
        let touches = |s: &Succession| {
            matches!(
                s,
                Succession::Move(_)
                    | Succession::Burn(_)
                    | Succession::Partial(_)
                    | Succession::Clear(_)
            )
        };
        let mut out: std::collections::HashMap<&'a str, Vec<&'a Succession>> =
            std::collections::HashMap::new();
        for (table, verdict) in owned.chain(references).filter(|(_, s)| touches(s)) {
            out.entry(table).or_default().push(verdict);
        }
        for (table, verdict) in overrides {
            if touches(verdict) {
                out.insert(table, vec![verdict]);
            } else {
                out.remove(table);
            }
        }
        out
    }

    fn registry_actor_columns() -> std::collections::HashMap<&'static str, &'static str> {
        ACTOR_TABLES.iter().map(|e| (e.table, e.column)).collect()
    }

    /// Walk the real schema's foreign keys against a set of succession verdicts
    /// and return every crossing that is not covered by a declared family.
    ///
    /// Split out of the gate so that the two things it decides can be tested
    /// against the **real** schema rather than asserted about in prose: that
    /// ruling a coupled plane `Move` is caught, and that declaring the family is
    /// what clears it.
    fn succession_fk_hazards(
        conn: &rusqlite::Connection,
        touched: &std::collections::HashMap<&str, Vec<&Succession>>,
        actor_col: &std::collections::HashMap<&str, &str>,
        families: &[CoupledFamily],
    ) -> Vec<String> {
        let mut tables_stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap();
        let all_tables: Vec<String> = tables_stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        drop(tables_stmt);

        let mut hazards = Vec::new();
        for child in &all_tables {
            for (parent, pairs) in foreign_keys(conn, child) {
                let child_cols: Vec<&str> = pairs.iter().map(|(c, _)| c.as_str()).collect();
                let parent_cols: Vec<String> = pairs
                    .iter()
                    .filter_map(|(_, p)| p.clone().or_else(|| primary_key_column(conn, &parent)))
                    .collect();

                // (a) The parent's referenced column IS the column a leg
                //     rewrites — the update aborts while any child row survives.
                let parent_actor_moved = touched.contains_key(parent.as_str())
                    && actor_col
                        .get(parent.as_str())
                        .is_some_and(|c| parent_cols.iter().any(|p| p == c));
                // (b) The parent's rows can be DELETED while a child still names
                //     one. `Burn` and `Partial` delete by definition; `Bespoke`
                //     may, and `admin_actor_ids` is exactly that case (a
                //     delete-then-insert, so the successor's grant date is its
                //     own). Keyed on the declaration rather than on the table
                //     name — the name is a proxy, and this file has paid for
                //     proxies before.
                //
                //     ⚠ **`Bespoke` is qualified by whether the rows are the
                //     actor's at all** (refined 2026-08-13, when
                //     `snapshots.tags_sealed_by` — since retired — became the first reference-only
                //     `Move(Bespoke)` on an FK parent). The delete-then-insert
                //     hazard is about a leg replacing *this actor's own rows*,
                //     which is what an `ACTOR_TABLES` entry declares. A table
                //     reached only through `SUCCESSION_REFERENCES` is, by that
                //     list's defining property, one whose rows do **not** belong
                //     to the actor — the leg rewrites a pointer on somebody
                //     else's row and deletes nothing, so the parent's referenced
                //     key is untouched and no child can be orphaned. A leg on
                //     such a table that genuinely deletes is a `Burn` or a
                //     `Partial`, and both stay in the set above regardless of
                //     which list they came from; declaring a deleting leg
                //     `Move` would be the mis-declaration, and it is the one
                //     assumption this refinement rests on.
                //
                //     A table in BOTH lists keeps the conservative reading —
                //     `actor_col` answers for the `ACTOR_TABLES` half. That is a
                //     fold over **every** verdict declared for the table, not a
                //     lookup of one: any declaration that deletes makes the
                //     parent's rows able to die, and a second, non-deleting
                //     ruling on another column of the same table does not make
                //     the first one stop deleting. Before 2026-08-15 this read a
                //     single shadowed verdict — see `registry_touched`.
                let parent_rows_die = touched.get(parent.as_str()).is_some_and(|verdicts| {
                    verdicts.iter().any(|s| match s {
                        Succession::Burn(_) | Succession::Partial(_) => true,
                        Succession::Move(MoveShape::Bespoke(_)) => {
                            actor_col.contains_key(parent.as_str())
                        }
                        _ => false,
                    })
                });
                // (c) The child's own actor column is inside the FK, so moving
                //     the CHILD orphans it against an unmoved parent.
                let child_actor_in_fk = touched.contains_key(child.as_str())
                    && actor_col
                        .get(child.as_str())
                        .is_some_and(|c| child_cols.contains(c));

                if !(parent_actor_moved || parent_rows_die || child_actor_in_fk) {
                    continue;
                }

                // Declared family: both ends are ruled together and the executor
                // moves them under `PRAGMA defer_foreign_keys`, so the crossing
                // is the reviewed one rather than a new one. **Both ends must be
                // in the SAME family** — two families that reference each other
                // are still a hazard, because each is deferred and committed as
                // its own unit.
                let covered = families.iter().any(|f| {
                    f.tables.contains(&child.as_str()) && f.tables.contains(&parent.as_str())
                });
                if covered {
                    continue;
                }

                hazards.push(format!(
                    "{child}({}) -> {parent}({})",
                    child_cols.join(","),
                    parent_cols.join(",")
                ));
            }
        }

        hazards.sort();
        hazards
    }

    /// The `subscription_tiers` family, as a test states it — the plane the
    /// backlog row must rule next, and the one instance the schema still
    /// carries.
    const TIER_FAMILY_TABLES: &[&str] = &[
        "subscription_tiers",
        "subscribers",
        "subscribe_requests",
        "current_key_blobs",
    ];

    /// **The mutation, kept as a standing test instead of a hand-run one.**
    ///
    /// Ruling the coupled tier plane `Move` — the thing the backlog row wants to
    /// do next — is caught while the family is undeclared, and is cleared by
    /// declaring it. Both halves matter: the first is the gate doing its job,
    /// the second is this row's whole deliverable, and testing only the first
    /// would let the acceptance path rot unnoticed (there is no live family in
    /// `COUPLED_MOVE_FAMILIES` to notice it for us).
    #[test]
    fn ruling_a_coupled_plane_needs_its_family_declared() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();

        let ruled: Vec<(&str, Succession)> = TIER_FAMILY_TABLES
            .iter()
            .map(|t| (*t, Succession::Move(MoveShape::Plain)))
            .collect();
        let touched = registry_touched(ruled.iter().map(|(t, s)| (*t, s)));
        // The tier tables are not all in `ACTOR_TABLES` yet (that is what
        // ruling them means), so the walk needs their actor column too.
        let mut actor_col = registry_actor_columns();
        for t in TIER_FAMILY_TABLES {
            actor_col.entry(t).or_insert("author_id");
        }

        // Deliberately `&[]` rather than `COUPLED_MOVE_FAMILIES`: this test
        // states what declaring a family *does*, so it must keep meaning the
        // same on the day the tier plane is actually ruled and declared.
        let undeclared = succession_fk_hazards(&conn, &touched, &actor_col, &[]);
        assert!(
            !undeclared.is_empty(),
            "ruling the tier plane `Move` with no family declared must be a hazard — \
             the ceremony would abort for every author who ever created a tier"
        );
        for child in ["subscribers", "subscribe_requests", "current_key_blobs"] {
            assert!(
                undeclared
                    .iter()
                    .any(|h| h.starts_with(&format!("{child}("))),
                "hazard list must name {child}; got {undeclared:?}"
            );
        }

        let declared = [CoupledFamily {
            name: "subscription tiers",
            tables: TIER_FAMILY_TABLES,
            reason: "test-local declaration",
        }];
        let cleared = succession_fk_hazards(&conn, &touched, &actor_col, &declared);
        assert!(
            cleared.is_empty(),
            "declaring the family must clear exactly these hazards; left: {cleared:?}"
        );
    }

    /// **The `Bespoke` delete-proxy's boundary, pinned against the real schema
    /// rather than argued about in the comment that draws it.**
    ///
    /// `snapshots` is the subject because it is a genuine FK parent — four
    /// children reference `snapshots(id)` — and because it has no
    /// [`ACTOR_TABLES`] entry, so a declaration on it is reference-only. That
    /// combination is exactly what the refinement rules on, and ruling
    /// `snapshots.tags_sealed_by` (retired at schema 101 with the tags scrub it
    /// licensed) produced a hazard the day it landed.
    ///
    /// Both directions are asserted, because a one-sided version of this test
    /// would pass just as well against a gate that had stopped checking (b)
    /// altogether — the failure mode a refinement of a security gate has to
    /// exclude by construction.
    #[test]
    fn a_reference_only_bespoke_move_is_not_a_delete_but_a_burn_still_is() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();
        let actor_col = registry_actor_columns();
        assert!(
            !actor_col.contains_key("snapshots"),
            "the premise of this test: `snapshots` carries no ownership column, it \
             follows its folder. If it gains an ACTOR_TABLES entry, the \
             conservative reading applies again and this test should be re-thought \
             rather than re-pointed"
        );

        // A pointer rewrite on a row that is not the actor's: nothing is
        // deleted, the referenced key (`snapshots.id`) is untouched, so no
        // child can be orphaned and the ceremony cannot abort.
        let moved = [(
            "snapshots",
            Succession::Move(MoveShape::Bespoke("test-local: a pointer rewrite")),
        )];
        let touched = registry_touched(moved.iter().map(|(t, s)| (*t, s)));
        let hazards = succession_fk_hazards(&conn, &touched, &actor_col, COUPLED_MOVE_FAMILIES);
        assert!(
            hazards.iter().all(|h| !h.contains("snapshots(")),
            "a reference-only Bespoke move rewrites a pointer and deletes nothing, \
             so it must raise no crossing; got {hazards:?}"
        );

        // The same table ruled `Burn` deletes rows by definition, reference-only
        // or not — and then every child naming one is a real abort.
        let burned = [("snapshots", Succession::Burn("test-local: deletes rows"))];
        let touched = registry_touched(burned.iter().map(|(t, s)| (*t, s)));
        let hazards = succession_fk_hazards(&conn, &touched, &actor_col, COUPLED_MOVE_FAMILIES);
        assert!(
            hazards.iter().any(|h| h.contains("-> snapshots(id)")),
            "a Burn on the same reference-only table MUST still raise the crossing \
             — the refinement narrows which verdicts count as deleting, never \
             whether a deleting verdict is checked; got {hazards:?}"
        );
    }

    /// **Two rulings on one table cannot shadow each other, in either order.**
    ///
    /// A table may be ruled twice — once in [`ACTOR_TABLES`] for the column it
    /// owns, once in [`SUCCESSION_REFERENCES`] for a column naming somebody else
    /// — and the hazard walk asks a per-*table* question of them. Keying the
    /// verdicts on the table name kept whichever landed second, so a `Burn` could
    /// be silently replaced by a `Move` and the walk would stop reporting the
    /// crossings that `Burn` raises.
    ///
    /// `snapshots` is the subject for the same reason
    /// [`a_reference_only_bespoke_move_is_not_a_delete_but_a_burn_still_is`] uses
    /// it: four real children reference `snapshots(id)`, so a dropped `Burn` is
    /// visible as a missing crossing against the real schema rather than a
    /// synthetic one.
    ///
    /// **Both orders are asserted, and that is the whole point.** The defect was
    /// order-sensitive by construction, so a one-order test passes against the
    /// broken code half the time — it would have to be the *unlucky* order to
    /// red, and which order that is depends on which list the walk chains first.
    #[test]
    fn a_second_ruling_on_one_table_cannot_shadow_the_first() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();
        let actor_col = registry_actor_columns();

        let burn = Succession::Burn("test-local: deletes rows");
        let mv = Succession::Move(MoveShape::Plain);

        for (label, owned, references) in [
            ("Burn owned, Move referenced", &burn, &mv),
            ("Move owned, Burn referenced", &mv, &burn),
        ] {
            let touched = registry_touched_from(
                std::iter::once(("snapshots", owned)),
                std::iter::once(("snapshots", references)),
                std::iter::empty(),
            );
            let hazards = succession_fk_hazards(&conn, &touched, &actor_col, COUPLED_MOVE_FAMILIES);
            assert!(
                hazards.iter().any(|h| h.contains("-> snapshots(id)")),
                "{label}: a Burn declared on `snapshots` must raise its children's \
                 crossings no matter which list it came from or which order the two \
                 rulings are read in — a second, non-deleting ruling on another \
                 column does not make the first one stop deleting; got {hazards:?}"
            );
        }
    }

    /// An override states *"suppose this table were ruled X"*, so it must
    /// **replace** the table's real declarations rather than join them —
    /// otherwise every override silently becomes "X in addition to whatever it
    /// already says", and the two tests that lean on it
    /// ([`ruling_a_coupled_plane_needs_its_family_declared`] and
    /// [`a_reference_only_bespoke_move_is_not_a_delete_but_a_burn_still_is`])
    /// would be asserting something weaker than they read as.
    #[test]
    fn an_override_replaces_a_tables_real_verdicts() {
        let burn = Succession::Burn("test-local: deletes rows");
        let stay = Succession::Stay("test-local: touches nothing");
        let mv = Succession::Move(MoveShape::Plain);

        let touched = registry_touched_from(
            std::iter::once(("snapshots", &burn)),
            std::iter::empty(),
            std::iter::once(("snapshots", &mv)),
        );
        assert_eq!(
            touched.get("snapshots").map(Vec::len),
            Some(1),
            "an override must replace, not append"
        );
        assert!(matches!(
            touched.get("snapshots").and_then(|v| v.first()),
            Some(Succession::Move(MoveShape::Plain))
        ));

        // Overriding to a verdict that touches nothing removes the table, the
        // same way a real `Stay`/`Unruled` never enters the map at all.
        let touched = registry_touched_from(
            std::iter::once(("snapshots", &burn)),
            std::iter::empty(),
            std::iter::once(("snapshots", &stay)),
        );
        assert!(
            !touched.contains_key("snapshots"),
            "overriding to a non-touching verdict must clear the table, not leave \
             the real one standing underneath"
        );
    }

    /// A declaration cannot be a blanket suppression: naming only *part* of a
    /// coupled family still leaves the crossings that reach outside it.
    #[test]
    fn a_partial_family_declaration_does_not_suppress_the_rest() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();

        let ruled: Vec<(&str, Succession)> = TIER_FAMILY_TABLES
            .iter()
            .map(|t| (*t, Succession::Move(MoveShape::Plain)))
            .collect();
        let touched = registry_touched(ruled.iter().map(|(t, s)| (*t, s)));
        let mut actor_col = registry_actor_columns();
        for t in TIER_FAMILY_TABLES {
            actor_col.entry(t).or_insert("author_id");
        }

        let half = [CoupledFamily {
            name: "half the tier family",
            tables: &["subscription_tiers", "subscribers"],
            reason: "test-local declaration",
        }];
        let left = succession_fk_hazards(&conn, &touched, &actor_col, &half);
        assert!(
            !left.is_empty() && !left.iter().any(|h| h.starts_with("subscribers(")),
            "a partial declaration must clear only its own members; got {left:?}"
        );

        // Two families, each holding one end of the same foreign key, is NOT a
        // declaration of that coupling: each is deferred and committed as its
        // own unit, so the crossing between them is exactly as fatal as an
        // undeclared one. This is what makes the check "same family" rather
        // than "both declared somewhere".
        let split = [
            CoupledFamily {
                name: "parents",
                tables: &["subscription_tiers", "subscribe_requests"],
                reason: "test-local declaration",
            },
            CoupledFamily {
                name: "children",
                tables: &["subscribers", "current_key_blobs"],
                reason: "test-local declaration",
            },
        ];
        let across = succession_fk_hazards(&conn, &touched, &actor_col, &split);
        assert!(
            across.iter().any(|h| h.starts_with("subscribers(")),
            "a foreign key running BETWEEN two declared families is still a \
             hazard — neither family's deferred unit covers the other; got \
             {across:?}"
        );
    }

    /// Every declared family is real: its tables exist, they are genuinely
    /// foreign-key coupled, and none of them is a stale name. A family that
    /// stops being coupled is a suppression of the gate that guards it, so it
    /// must be deleted rather than left standing.
    #[test]
    fn a_declared_family_is_really_coupled() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.blocking_lock();

        for family in COUPLED_MOVE_FAMILIES {
            assert!(
                family.tables.len() >= 2,
                "family {} names {} table(s) — a family is a coupling, not a label",
                family.name,
                family.tables.len()
            );
            assert!(
                !family.reason.trim().is_empty(),
                "family {} carries no reason",
                family.name
            );
            for table in family.tables {
                assert!(
                    table_exists(&conn, table),
                    "family {} names {table}, which is not in the schema",
                    family.name
                );
            }
            // At least one foreign key must actually run between two of its
            // members, or the declaration is suppressing nothing and hiding
            // that fact.
            let coupled = family.tables.iter().any(|child| {
                foreign_keys(&conn, child)
                    .into_iter()
                    .any(|(parent, _)| family.tables.contains(&parent.as_str()))
            });
            assert!(
                coupled,
                "family {} declares no foreign key between its own members — \
                 delete the declaration rather than leaving it to suppress a \
                 future crossing",
                family.name
            );
        }
    }

    /// Every member of a declared family moves, and moves plainly. A family
    /// with an unruled or `Stay` member is the abort this row exists to
    /// prevent, merely relocated from the statement to the commit.
    #[test]
    fn a_declared_family_moves_as_one() {
        for family in COUPLED_MOVE_FAMILIES {
            for table in family.tables {
                let entry = ACTOR_TABLES.iter().find(|e| e.table == *table);
                let Some(entry) = entry else {
                    panic!(
                        "family {} names {table}, which is not in ACTOR_TABLES — \
                         a family member must be ruled, and ruling happens there",
                        family.name
                    );
                };
                assert!(
                    matches!(entry.succession, Succession::Move(MoveShape::Plain)),
                    "family {}'s member {table} declares {:?}; every member must be \
                     Move(Plain) or the deferred commit aborts",
                    family.name,
                    entry.succession
                );
            }
        }
    }

    #[test]
    fn no_table_belongs_to_two_families() {
        let mut seen: Vec<&str> = Vec::new();
        for family in COUPLED_MOVE_FAMILIES {
            for table in family.tables {
                assert!(
                    !seen.contains(table),
                    "{table} is claimed by two families; the executor moves each \
                     family as its own unit, so a shared table would be moved twice"
                );
                seen.push(table);
            }
        }
    }

    fn table_exists(conn: &rusqlite::Connection, table: &str) -> bool {
        super::table_exists(conn, table).unwrap()
    }

    /// Apply each bridge's own schema when this build actually has it.
    ///
    /// **Why this exists, measured by the skip-on-absence fix.** `run_migrations`
    /// does **not** create the `nostr_*` / `bluesky_*` / `ap_*` tables — every
    /// bridge ships its own `CREATE_TABLES_SQL` and applies it at `init_db` —
    /// so a guard that only opened `CacheDb::open_in_memory()` could never see
    /// one, **and turning the feature on changed nothing**: the flag compiled
    /// the bridge code and left the guard exactly as blind. That is why
    /// `FEATURE_GATED_ELSEWHERE` had grown into an unconditional by-name skip,
    /// and why the one plane holding OAuth tokens and deposited signing keys
    /// was the one plane no column guard ever inspected.
    ///
    /// So the guards call this and then skip on **absence**, never on the name:
    /// a default build still skips (the tables genuinely are not there), while
    /// `--features nostr,bluesky` now buys real coverage instead of the
    /// appearance of it.
    ///
    /// ⚠ **Faithfulness to `init_db` is the whole property here.** Each bridge
    /// exposes one `apply_schema` — its genesis block plus the shared additive
    /// column reconciler (`bridge_schema::apply_genesis`) — and `init_db` and
    /// this seeding both call it, so a guard reading this connection rules on
    /// exactly the schema production has. Re-spelling a bridge's setup here is
    /// the move that once left the guards blind to every `ALTER`-added column; with every column now in the genesis block
    /// that class of drift has no hand-written step left to miss.
    fn apply_available_bridge_schemas(conn: &rusqlite::Connection) {
        #[cfg(feature = "nostr")]
        crate::nostr::apply_schema(conn).unwrap();
        #[cfg(feature = "bluesky")]
        crate::bluesky::apply_schema(conn).unwrap();
        #[cfg(feature = "activitypub")]
        crate::activitypub::apply_schema(conn).unwrap();
        let _ = conn;
    }

    #[allow(clippy::type_complexity)]
    fn foreign_keys(
        conn: &rusqlite::Connection,
        table: &str,
    ) -> Vec<(String, Vec<(String, Option<String>)>)> {
        let Ok(mut stmt) = conn.prepare(&format!("PRAGMA foreign_key_list({table})")) else {
            return Vec::new();
        };
        let rows: Vec<(i64, String, String, Option<String>)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        let mut grouped: Vec<(i64, String, Vec<(String, Option<String>)>)> = Vec::new();
        for (id, parent, from_col, to_col) in rows {
            match grouped.iter_mut().find(|(gid, _, _)| *gid == id) {
                Some((_, _, pairs)) => pairs.push((from_col, to_col)),
                None => grouped.push((id, parent, vec![(from_col, to_col)])),
            }
        }
        grouped
            .into_iter()
            .map(|(_, parent, pairs)| (parent, pairs))
            .collect()
    }

    fn primary_key_column(conn: &rusqlite::Connection, table: &str) -> Option<String> {
        table_columns(conn, table)
            .into_iter()
            .find(|(_, _, _, _, pk)| *pk)
            .map(|(name, ..)| name)
    }

    fn table_ddl(conn: &rusqlite::Connection, table: &str) -> String {
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            rusqlite::params![table],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .unwrap_or_default()
    }

    /// A literal the DDL's own `CHECK` on `column` accepts — the first of a
    /// `CHECK (col IN (…))` enumeration, or the right-hand side of a
    /// `CHECK (col = <literal>)`.
    ///
    /// Scanned rather than parsed, but **whitespace-tolerantly**, because the
    /// naive version was wrong twice for the same reason: `import_sessions`
    /// wraps its enumeration across a newline (`state IN\n  ('running',…)`) and
    /// `web_apex_actor` is a singleton table pinned with `CHECK (id = 1)`. A
    /// miss falls back to the generic dummy, exactly as before.
    fn allowed_literal(ddl: &str, column: &str) -> Option<String> {
        let hay = ddl.to_ascii_lowercase();
        let col = column.to_ascii_lowercase();
        let mut from = 0usize;
        while let Some(rel) = hay[from..].find(&col) {
            let at = from + rel;
            from = at + col.len();
            // Must be a whole-word match, else `id` fires inside `list_id`.
            let before_ok = at == 0 || !is_ident_byte(hay.as_bytes()[at - 1]);
            let after = &hay[from..];
            if !before_ok || after.starts_with(|c: char| is_ident_byte(c as u8)) {
                continue;
            }
            let rest = after.trim_start();
            let consumed = after.len() - rest.len();
            let tail_at = from + consumed;
            if let Some(inner) = rest.strip_prefix("in") {
                let inner = inner.trim_start();
                if let Some(list) = inner.strip_prefix('(') {
                    let start = tail_at + (rest.len() - list.len());
                    let end = ddl[start..].find(')')?;
                    let first = ddl[start..start + end].split(',').next()?.trim();
                    if !first.is_empty() {
                        return Some(first.to_string());
                    }
                }
            } else if let Some(eq) = rest.strip_prefix('=') {
                let start = tail_at + (rest.len() - eq.len()) + 1;
                let value = ddl[start..]
                    .trim_start()
                    .split([')', ',', '\n'])
                    .next()?
                    .trim();
                // Only a literal: `CHECK (a = b)` compares two columns, and
                // binding the *name* `b` would build invalid SQL.
                let literal = value.starts_with(|c: char| c.is_ascii_digit() || c == '\'')
                    || value.starts_with("x'");
                if literal {
                    return Some(value.to_string());
                }
            }
        }
        None
    }

    fn is_ident_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    /// How many of `entry`'s rows name `actor`, bound per the declared encoding.
    fn count_rows(conn: &rusqlite::Connection, entry: &ActorTable, actor: &[u8; 32]) -> i64 {
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {} = ?1",
            entry.table, entry.column
        );
        let bound: Box<dyn rusqlite::ToSql> = match entry.key {
            ActorKey::Blob => Box::new(actor.to_vec()),
            ActorKey::Hex => Box::new(hex::encode(actor)),
        };
        conn.query_row(&sql, rusqlite::params![bound], |r| r.get(0))
            .unwrap()
    }

    /// `depth` salts the literal so a row seeded to satisfy someone else's
    /// foreign key never occupies the key the sweep's *own* row wants. Without
    /// it the two collide on a deterministic dummy PK, the sweep's insert is
    /// ignored, and a table that had been observed for months (`folders`,
    /// `groups`, `subscription_tiers`) silently drops out of the gate — which is
    /// how the FK-parent walk first arrived: it fixed nine tables and quietly
    /// broke three.
    ///
    /// **`depth` salts EVERY affinity, not just `BLOB` (2026-08-15).** It used to
    /// salt the blob literal alone, which left the whole collision class open on
    /// any `UNIQUE` over text or integer columns — and that is exactly what kept
    /// `mail_lists` (and `mail_list_sends` behind it) out of the sweep.
    /// `account_aliases` carries `UNIQUE (local_domain, pattern, kind)`, all
    /// three `TEXT`; it is itself a registry table seeded at depth 0, so when
    /// `mail_lists` planted its FK parent at depth 1 the plant regenerated those
    /// three literals **byte for byte**, collided on the UNIQUE, was swallowed by
    /// `INSERT OR IGNORE`, and left the child to bounce against a parent that was
    /// never inserted. (The `SEED_BLIND` comment used to name a `CHECK` as the
    /// mechanism; it is a `UNIQUE`.)
    ///
    /// ⚠ **Depth 0 is byte-identical to the pre-change literal, deliberately.**
    /// Every currently-seeding table must keep seeding exactly as it did, or this
    /// becomes a change to what the whole gate observes rather than a change to
    /// what it can reach. Only planted parents (depth ≥ 1) move.
    fn dummy_value_sql(
        decl_type: &str,
        table: &str,
        counter: usize,
        depth: usize,
        pin_salt: u16,
    ) -> String {
        // Fold the table name in so a row planted to satisfy someone else's
        // foreign key cannot occupy the key the sweep's own row wants: the
        // child's FK literal is derived from the CHILD's name, the parent's own
        // seed from the parent's.
        let salt = table
            .bytes()
            .fold(0u16, |a, b| a.wrapping_mul(31).wrapping_add(b as u16));
        let t = decl_type.to_uppercase();
        if t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB") {
            return "1.0".to_string();
        }
        // ⚠ Depth 0 keeps the pre-2026-08-15 literals BYTE FOR BYTE. Every
        // currently-seeding table must go on seeding exactly as it did, or this
        // stops being a change to what the sweep can *reach* and becomes a
        // change to what it *observes*.
        if depth == 0 {
            return if t.contains("INT") {
                format!("{}", 1 + (salt as usize % 4096) * 8 + counter)
            } else if t.contains("BLOB") {
                format!(
                    "x'{}'",
                    "ab".repeat(13) + &format!("{salt:04x}{depth:02x}{counter:02x}")
                )
            } else {
                format!("'seed_{table}_{counter}'")
            };
        }
        // A planted parent (depth >= 1) is salted by BOTH `depth` and the key it
        // is being planted for, because there are two distinct collision classes
        // and each needs its own axis:
        //
        //   `depth` separates a plant from the sweep's own depth-0 row of the
        //   same table. This is what kept `mail_lists` out: `account_aliases` is
        //   seeded at depth 0 and carries `UNIQUE (local_domain, pattern, kind)`
        //   over three TEXT columns, and the literals were salted only by table
        //   and column index — so the plant reproduced the depth-0 row exactly.
        //
        //   `pin_salt` separates two plants of the SAME parent made for
        //   DIFFERENT children, which sit at the same depth and so are identical
        //   on every axis above. This is what kept `restore_history` out:
        //   `snapshots` has four FK children, each planting its own `snapshots`
        //   row with a distinct pinned `id` but with `folder_id`/`created_at`
        //   computed from the parent's name alone — so all four collided on
        //   `UNIQUE (folder_id, created_at)` and only the first survived.
        //
        // The pin is the right discriminator because it is exactly what makes
        // one plant different from another: the child derives it from its own
        // name, so two children never pin the same value.
        if t.contains("INT") {
            // `(salt % 4096) * 8 + counter` occupies [0, 32768) and `depth` is
            // bounded at 4, so both ride below 2^18; the pin's multiplier starts
            // past that and cannot fold back onto either.
            format!(
                "{}",
                1 + (salt as usize % 4096) * 8
                    + counter
                    + depth * 32_768
                    + (pin_salt as usize) * 4_194_304
            )
        } else if t.contains("BLOB") {
            // Same total width as the depth-0 literal (34 hex chars): the pin
            // takes four of the padding's characters rather than extending it,
            // so a fixed-width blob column keeps accepting the value.
            format!(
                "x'{}'",
                "ab".repeat(11) + &format!("{salt:04x}{depth:02x}{counter:02x}{pin_salt:04x}")
            )
        } else {
            format!("'seed_{table}_{counter}_d{depth}p{pin_salt:04x}'")
        }
    }

    /// The row's own success bar, driven end-to-end and data-driven rather
    /// than hand-picking a handful of tables: seed one row keyed to a single
    /// actor into every `Policy::Purge` table this in-memory schema supports
    /// (skipping the feature-gated bridge tables `every_table_and_column_
    /// exists_in_the_real_schema` already excludes, and any table whose
    /// generic dummy-value insert is refused by a CHECK/FK constraint this
    /// synthetic row cannot satisfy — those are reported, not silently
    /// dropped), delete the actor, and assert every successfully-seeded
    /// table is empty. A registry entry that regresses to a no-op delete (a
    /// typo'd column that always matches zero rows, say) would leave a
    /// seeded row behind and fail this test.
    #[tokio::test]
    async fn deleting_an_actor_purges_every_seeded_table() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [0x42; 32];

        let mut seeded: Vec<&str> = Vec::new();
        let mut skipped: Vec<&str> = Vec::new();
        let mut why_skipped: Vec<String> = Vec::new();
        {
            let conn = db.conn.lock().await;
            for entry in ACTOR_TABLES {
                if !matches!(entry.policy, Policy::Purge) {
                    continue;
                }
                if FEATURE_GATED_ELSEWHERE.contains(&entry.table) {
                    continue;
                }
                // One seeding routine, shared with the succession gates — the
                // deletion tests each carried their own copy until 2026-08-12,
                // and the copies had silently diverged into *relying on a
                // collision*: every table's dummy literals were keyed on the
                // column index alone, so a child's `tier_name` matched whatever
                // `subscription_tiers.name` some unrelated table had planted,
                // and four Retain tables seeded only by that accident. That is
                // the same "two lists claiming to be the same set" shape the
                // registry itself exists to end.
                match seed_one_row(&conn, entry, &actor) {
                    Ok(()) => seeded.push(entry.table),
                    Err(why) => {
                        skipped.push(entry.table);
                        why_skipped.push(why);
                    }
                }
            }
        }

        // The sweep must actually exercise the large majority of the
        // registry, not silently degrade to testing a handful of tables —
        // constraints this synthetic single-row seed can't satisfy (a
        // self-referential FK, a CHECK spanning two columns) are expected
        // for a few tables, not most of them.
        assert!(
            seeded.len() * 10 >= (seeded.len() + skipped.len()) * 8,
            "only seeded {}/{} purge tables (skipped: {skipped:?}) — the sweep is too weak to trust",
            seeded.len(),
            seeded.len() + skipped.len()
        );

        db.purge_orphaned_actor_rows(&actor).await.unwrap();

        let conn = db.conn.lock().await;
        let mut still_present = Vec::new();
        for table in &seeded {
            let entry = ACTOR_TABLES.iter().find(|e| &e.table == table).unwrap();
            let column = entry.column;
            // Read back in the same spelling the seed used, or the check would
            // be as blind as the seed was.
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
            let count: i64 = match entry.key {
                ActorKey::Blob => {
                    conn.query_row(&sql, rusqlite::params![actor.to_vec()], |r| r.get(0))
                }
                ActorKey::Hex => {
                    conn.query_row(&sql, rusqlite::params![hex::encode(actor)], |r| r.get(0))
                }
            }
            .unwrap();
            if count > 0 {
                still_present.push(*table);
            }
        }
        assert!(
            still_present.is_empty(),
            "rows survived deletion in: {still_present:?}"
        );
    }

    /// The other half of the success bar this module doc promises but
    /// only `deleting_an_actor_purges_every_seeded_table` pinned: every
    /// `Policy::Retain` table must survive `purge_orphaned_actor_rows`, not
    /// just be absent from the sweep by construction. Same data-driven seed
    /// as the purge test, opposite assertion — a future edit that moved a
    /// `Retain` table's rows into the `Purge` iteration by mistake (or that
    /// added a second, wrongly-unconditional delete for one of these tables)
    /// would silently destroy the deposited nsec's `content` twin, financial
    /// records, or an identity-succession audit trail with no test to catch it.
    #[tokio::test]
    async fn deleting_an_actor_does_not_purge_a_retained_table() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [0x77; 32];
        // A local predecessor, retired into `actor` — the purge walks to it
        // (`purge_orphaned_actor_rows`'s predecessor paragraph), and its
        // Retain rows must keep their reasons exactly as the actor's do.
        let predecessor: [u8; 32] = [0x78; 32];

        let mut seeded: Vec<&str> = Vec::new();
        let mut skipped: Vec<&str> = Vec::new();
        let mut why_skipped: Vec<String> = Vec::new();
        {
            let conn = db.conn.lock().await;
            for entry in ACTOR_TABLES {
                if !matches!(entry.policy, Policy::Retain(_)) {
                    continue;
                }
                if FEATURE_GATED_ELSEWHERE.contains(&entry.table) {
                    continue;
                }
                // The shared seeder — see the sibling test for why the local
                // copies had to go.
                match seed_one_row(&conn, entry, &actor) {
                    Ok(()) => seeded.push(entry.table),
                    Err(why) => {
                        skipped.push(entry.table);
                        why_skipped.push(why);
                    }
                }
                // Tolerated like the actor's own seed: a UNIQUE the synthetic
                // literals collide on is expected for a few tables, and the
                // assertion below counts only what landed.
                let _ = seed_one_row(&conn, entry, &predecessor);
            }
            // The chain: a handle-less `users` row for the predecessor, as
            // `record_succession` leaves it, and the link into `actor`.
            conn.execute(
                "INSERT OR IGNORE INTO users (actor_id, tier, label, created_at)
                 VALUES (?1, 'free', '', 0)",
                rusqlite::params![predecessor.to_vec()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO actor_successions
                    (old_actor_id, new_actor_id, statement, seq, succeeded_at)
                 VALUES (?1, ?2, x'00', 1, 0)
                 ON CONFLICT(old_actor_id) DO UPDATE SET new_actor_id = excluded.new_actor_id",
                rusqlite::params![predecessor.to_vec(), actor.to_vec()],
            )
            .unwrap();
        }
        let predecessor_seeded: Vec<&str> = {
            let conn = db.conn.lock().await;
            seeded
                .iter()
                .copied()
                .filter(|table| {
                    let entry = ACTOR_TABLES.iter().find(|e| &e.table == table).unwrap();
                    count_rows_for(&conn, entry, &predecessor) > 0
                })
                .collect()
        };
        assert!(
            predecessor_seeded.contains(&"actor_successions"),
            "the chain row itself must be among the predecessor's Retain rows under test"
        );

        // Same tolerance as `deleting_an_actor_purges_every_seeded_table`, for
        // the same reason: a CHECK or FK this synthetic single-row seed cannot
        // satisfy is expected for a handful of tables, not most of them.
        assert!(
            seeded.len() * 10 >= (seeded.len() + skipped.len()) * 8,
            "only seeded {}/{} Retain tables (skipped: {skipped:?}) — the sweep is too weak to trust",
            seeded.len(),
            seeded.len() + skipped.len()
        );

        db.purge_orphaned_actor_rows(&actor).await.unwrap();

        let conn = db.conn.lock().await;
        let mut lost = Vec::new();
        for table in &seeded {
            let entry = ACTOR_TABLES.iter().find(|e| &e.table == table).unwrap();
            if count_rows_for(&conn, entry, &actor) == 0 {
                lost.push(*table);
            }
        }
        for table in &predecessor_seeded {
            let entry = ACTOR_TABLES.iter().find(|e| &e.table == table).unwrap();
            if count_rows_for(&conn, entry, &predecessor) == 0 {
                lost.push(*table);
            }
        }
        assert!(
            lost.is_empty(),
            "Policy::Retain row(s) were purged anyway -- this table's policy \
             regressed to Purge, or a second delete path bypasses the registry: {lost:?}"
        );
    }

    /// `SELECT COUNT(*)` of `entry`'s rows for `actor`, bound in the spelling
    /// the entry declares ([`ActorKey`]).
    fn count_rows_for(conn: &rusqlite::Connection, entry: &ActorTable, actor: &[u8; 32]) -> i64 {
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {} = ?1",
            entry.table, entry.column
        );
        match entry.key {
            ActorKey::Blob => conn.query_row(&sql, rusqlite::params![actor.to_vec()], |r| r.get(0)),
            ActorKey::Hex => {
                conn.query_row(&sql, rusqlite::params![hex::encode(actor)], |r| r.get(0))
            }
        }
        .unwrap()
    }

    /// Deletion reaches the account's local predecessors
    /// (`account-data-plane.md` § Nest-side requirements item 1): a `Stay`
    /// row a key rotation left under the retired id goes with the person —
    /// across two ceremonies — while a bystander's row and a peer-recorded
    /// predecessor's row (someone this nest knew only as a remote actor) stay.
    #[tokio::test]
    async fn deleting_an_account_purges_its_local_predecessors_stay_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let (first, middle, current) = ([0xA1u8; 32], [0xA2u8; 32], [0xA3u8; 32]);
        let (bystander, remote) = ([0xB1u8; 32], [0xC1u8; 32]);
        db.create_user(&first, "free", "").await.unwrap();
        db.create_user(&bystander, "free", "").await.unwrap();
        // `worker_replication` is Purge + Stay: a marker written under a key
        // stays under it through every later ceremony.
        db.mark_replicated("inbox", &first, None).await.unwrap();
        // `generation_escrow_wraps` is Purge + Stay too (the kept wrap): a
        // wrap the first identity deposited rests under it through both
        // ceremonies, and the deletion takes it with the chain.
        let generation = [0x6Eu8; 32];
        db.put_generation_escrow_wrap(&first, &generation, &[1; 32], b"kept wrap")
            .await
            .unwrap();
        db.put_generation_escrow_wrap(&bystander, &generation, &[2; 32], b"bystander's")
            .await
            .unwrap();
        // A peer ceremony at the head of the chain: its old id is not a local
        // account, so its rows are not this person's local data. It names
        // `first` as successor because an identity holds one predecessor
        // (ruling (8)(j)(1)) — `middle` and `current` each get theirs from the
        // local ceremonies below.
        db.record_peer_succession(&remote, &first, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        db.mark_replicated("inbox", &remote, None).await.unwrap();
        db.record_succession(&first, &middle, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        db.mark_replicated("inbox", &middle, None).await.unwrap();
        db.record_succession(&middle, &current, b"s", 2)
            .await
            .unwrap()
            .unwrap();
        db.mark_replicated("inbox", &current, None).await.unwrap();
        db.mark_replicated("inbox", &bystander, None).await.unwrap();

        db.purge_orphaned_actor_rows(&current).await.unwrap();

        for (who, id, survives) in [
            ("the deleted account", current, false),
            ("its predecessor", middle, false),
            ("its first identity, two hops back", first, false),
            ("a bystander", bystander, true),
            ("a peer-recorded predecessor", remote, true),
        ] {
            assert_eq!(
                db.is_replicated("inbox", &id, None).await.unwrap(),
                survives,
                "{who}'s replication marker"
            );
        }
        assert_eq!(db.replication_count().await.unwrap(), 2);
        assert!(
            db.get_generation_escrow_wraps(&first, None)
                .await
                .unwrap()
                .is_empty(),
            "the first identity's kept escrow wrap goes with the account"
        );
        assert_eq!(
            db.get_generation_escrow_wraps(&bystander, None)
                .await
                .unwrap()
                .len(),
            1,
            "a bystander's escrow wrap survives"
        );
        // The chain itself is Retain, so a retried deletion walks it again.
        assert!(db.succession_for(&first).await.unwrap().is_some());
        assert!(db.succession_for(&middle).await.unwrap().is_some());
    }

    /// The predecessors' `users` rows go with the account (item 1's 2026-09-24
    /// ruling): `delete_user`, run after the purge walk exactly as
    /// `finalize_user_deletion` orders it, deletes the whole local chain's
    /// rows — while a bystander keeps its row and the `Retain` chain survives,
    /// so the retired keys stay refusable at every door.
    #[tokio::test]
    async fn deleting_an_account_deletes_its_local_predecessors_users_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let (first, middle, current) = ([0xA1u8; 32], [0xA2u8; 32], [0xA3u8; 32]);
        let bystander = [0xB1u8; 32];
        db.create_user(&first, "free", "").await.unwrap();
        db.create_user(&bystander, "free", "").await.unwrap();
        db.record_succession(&first, &middle, b"s", 1)
            .await
            .unwrap()
            .unwrap();
        db.record_succession(&middle, &current, b"s", 2)
            .await
            .unwrap()
            .unwrap();
        for id in [first, middle, current, bystander] {
            assert!(db.is_actor_registered(&id).await.unwrap());
        }

        db.purge_orphaned_actor_rows(&current).await.unwrap();
        assert!(db.delete_user(&current).await.unwrap());

        for (who, id, survives) in [
            ("the deleted account", current, false),
            ("its predecessor", middle, false),
            ("its first identity, two hops back", first, false),
            ("a bystander", bystander, true),
        ] {
            assert_eq!(
                db.is_actor_registered(&id).await.unwrap(),
                survives,
                "{who}'s users row"
            );
        }
        // The chain is Retain: the retired keys are still on record as
        // succeeded, which is what the registration doors refuse on.
        assert!(db.succession_for(&first).await.unwrap().is_some());
        assert!(db.succession_for(&middle).await.unwrap().is_some());
        // A second deletion of the same id is a clean no-op, not an error.
        assert!(!db.delete_user(&current).await.unwrap());
    }

    // -----------------------------------------------------------------------
    // The export axis's emission, proven on a SYNTHETIC registry slice.
    //
    // The mechanism is graded independently of the ruling — the
    // COUPLED_MOVE_FAMILIES precedent above, and load-bearing here because the
    // skeleton pass (2026-08-15) left zero real Verbatim/Redacted verdicts.
    // Against the real registry these tests would all pass vacuously; against
    // the synthetic slice they fail the moment the walk stops working, and they
    // keep working unchanged as each pass drains the backlog.
    // -----------------------------------------------------------------------

    /// Two synthetic tables, one per [`ActorKey`] spelling, so a walk that
    /// ignores the encoding cannot pass.
    fn synthetic_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t_blob (
                 actor_id BLOB NOT NULL,
                 note TEXT,
                 secret TEXT,
                 payload BLOB,
                 n INTEGER
             );
             CREATE TABLE t_hex (
                 owner TEXT NOT NULL,
                 note TEXT
             );",
        )
        .unwrap();
        conn
    }

    fn synth(
        table: &'static str,
        column: &'static str,
        key: ActorKey,
        export: Export,
    ) -> ActorTable {
        ActorTable {
            table,
            column,
            key,
            policy: Policy::Purge,
            succession: Succession::Unruled,
            export,
        }
    }

    fn ndjson_of(set: &ActorExportSet, table: &str) -> Vec<serde_json::Value> {
        let t = set
            .tables
            .iter()
            .find(|t| t.table == table)
            .unwrap_or_else(|| panic!("{table} not emitted; emitted: {:?}", set.tables));
        String::from_utf8(t.ndjson.clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Seed one actor with a `mail` record scoped to themselves and a `conv`
    /// record scoped to a channel they are a member of. Returns
    /// `(actor, channel)`.
    ///
    /// Both inserts go through the production writers' exact column sets —
    /// `records_db::insert_mail` / `insert_conv` — so a schema change that
    /// moved either kind's scoping would break this fixture rather than let it
    /// keep asserting about a shape production no longer writes.
    fn seed_actor_with_mail_and_conv(db: &CacheDb) -> ([u8; 32], [u8; 32]) {
        let actor = [7u8; 32];
        let channel = [9u8; 32];
        let conn = db.conn.blocking_lock();
        conn.execute(
            "INSERT INTO actor_channels (actor_id, channel_id, created_at) VALUES (?1, ?2, 100)",
            rusqlite::params![actor.as_slice(), channel.as_slice()],
        )
        .unwrap();
        // `mail` scopes to the recipient ACTOR; `conv` scopes to the CHANNEL.
        conn.execute(
            "INSERT INTO segment_records
                (scope_id, kind, segment_id, record_cid, bucket, tombstoned,
                 changed_seq, received_at, sender_dom, spam_disp,
                 is_own_submission, seq)
             VALUES (?1, 'mail', 1, x'aa', 'inbox', 0, 1, 100, NULL, NULL, NULL, 1)",
            rusqlite::params![actor.as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO segment_records
                (scope_id, kind, segment_id, record_cid, bucket, tombstoned,
                 changed_seq, received_at, sender_dom, spam_disp,
                 is_own_submission, seq)
             VALUES (?1, 'conv', 2, x'bb', 'live', 0, 1, 200, NULL, NULL, NULL, 1)",
            rusqlite::params![channel.as_slice()],
        )
        .unwrap();
        // ⚠ A third row: a NON-conv kind sharing the CHANNEL scope. No kind does
        // this in production today — only `conv` is channel-scoped — and that is
        // exactly why it is seeded here. Without it, the domain's
        // `kind = 'conv'` filter is load-bearing only by *data*: dropping it
        // changes no result, because the other kinds' scopes are actor ids the
        // channel-keyed read never matches, so the test would agree with itself
        // (mutation-proven: dropping the filter survived the first draft of these
        // tests). With it, the filter is pinned to the domain's declared
        // universe, and a future kind that becomes channel-scoped has to be
        // ruled into this export deliberately instead of being swept in by a
        // read that was never asked about it.
        conn.execute(
            "INSERT INTO segment_records
                (scope_id, kind, segment_id, record_cid, bucket, tombstoned,
                 changed_seq, received_at, sender_dom, spam_disp,
                 is_own_submission, seq)
             VALUES (?1, 'not-a-ruled-kind', 3, x'cc', 'live', 0, 1, 300, NULL, NULL, NULL, 1)",
            rusqlite::params![channel.as_slice()],
        )
        .unwrap();
        drop(conn);
        (actor, channel)
    }

    /// **Row 157.** The owner's conversation records must reach their export,
    /// and the mechanism must resolve channel membership explicitly.
    ///
    /// The gap this pins was measured closing the final pass: the registry
    /// walk is `WHERE scope_id = ?actor`, and `conv` rows key on the channel,
    /// so the owner's conversation records were structurally unreachable by it.
    /// An INCOMPLETENESS, never a leak — the walk cannot return another party's
    /// rows, it returns nothing for this kind.
    #[test]
    fn the_owners_conv_records_reach_the_export_through_membership() {
        let db = CacheDb::open_in_memory().unwrap();
        let (actor, channel) = seed_actor_with_mail_and_conv(&db);

        // The registry walk's own reach, unchanged: it finds the `mail` row and
        // — correctly, on its own key — not the `conv` one. This half is the
        // control, and it must keep holding after the fix.
        let conn = db.conn.blocking_lock();
        let set = gather_export_set(&conn, ACTOR_TABLES, &actor).unwrap();
        let walked = ndjson_of(&set, "segment_records");
        assert_eq!(
            walked.len(),
            1,
            "the actor-keyed walk returns exactly the actor-scoped record: {walked:?}"
        );
        assert_eq!(walked[0]["kind"], "mail");

        // The membership-resolved door: the conv record the walk cannot see.
        let conv = gather_conv_records(&conn, &actor)
            .unwrap()
            .expect("the actor is on a channel that holds a conv record");
        let rows: Vec<serde_json::Value> = String::from_utf8(conv.ndjson.clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            conv.rows, 1,
            "one conv record -- and NOT the other channel-scoped row the fixture \
             seeds beside it: the domain reads the kind it was ruled for, never \
             everything that happens to share a channel scope: {rows:?}"
        );
        assert_eq!(rows[0]["kind"], "conv");
        assert_eq!(
            rows[0]["scope_id"],
            serde_json::Value::String(hex::encode(channel)),
            "the row names the CHANNEL it is scoped to, so the archive is \
             self-describing about which door produced it"
        );
        // Same encoder as the walk: blobs as lowercase hex, NULLs as null.
        assert_eq!(rows[0]["record_cid"], "bb");
        assert_eq!(rows[0]["is_own_submission"], serde_json::Value::Null);
    }

    /// The two doors onto `segment_records` are keyed differently and must stay
    /// **disjoint**: every row reaches the archive through exactly one of them.
    ///
    /// This is what makes the pair safe without an `Export::Shaped` verdict —
    /// the verdict's job is preventing a double emission, and here the keys do
    /// it structurally instead (an actor id is never a channel id).
    #[test]
    fn the_conv_domain_and_the_registry_walk_never_double_emit() {
        let db = CacheDb::open_in_memory().unwrap();
        let (actor, _channel) = seed_actor_with_mail_and_conv(&db);

        let conn = db.conn.blocking_lock();
        let walked = ndjson_of(
            &gather_export_set(&conn, ACTOR_TABLES, &actor).unwrap(),
            "segment_records",
        );
        let conv = gather_conv_records(&conn, &actor).unwrap().unwrap();
        let conv_rows: Vec<serde_json::Value> = String::from_utf8(conv.ndjson.clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let walked_kinds: Vec<&str> = walked.iter().map(|r| r["kind"].as_str().unwrap()).collect();
        let conv_kinds: Vec<&str> = conv_rows
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect();
        assert!(
            !walked_kinds.contains(&"conv"),
            "the actor-keyed walk must never return a channel-scoped row -- if it \
             does, someone widened the verdict or added a channel-column entry, \
             which is the family plane's forbidden shape: {walked_kinds:?}"
        );
        assert_eq!(
            conv_kinds,
            vec!["conv"],
            "the membership door reads exactly the kind the walk cannot -- and \
             nothing else, including the non-conv row the fixture seeds on the \
             same channel scope: {conv_kinds:?}"
        );
        assert_ne!(
            conv.table, "segment_records",
            "the domain writes its own archive path, so the two doors' rows never \
             land in one file where a reader could not tell which key produced them"
        );
    }

    /// **The forbidden shape, guarded.** `segment_records` must keep exactly ONE
    /// registry entry, keyed on the actor-scoped column, with an emitting
    /// verdict that is not `Shaped`.
    ///
    /// Three ways a future session could break the ruling, all caught
    /// here: adding a second entry on a channel column (a shared scope walked as
    /// though it were the actor's — the family plane's error one plane over);
    /// re-keying the one entry off `scope_id`; or flipping it to `Shaped` to
    /// "declare" the conversations domain, which would assert total coverage
    /// that a one-kind row-filter cannot honour.
    #[test]
    fn segment_records_keeps_one_actor_keyed_export_verdict() {
        let entries: Vec<&ActorTable> = ACTOR_TABLES
            .iter()
            .filter(|e| e.table == "segment_records")
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "one entry only. A second entry keyed on a channel column is the \
             shape row 157 ruled out: the registry's own rules would bless it \
             because \"the rows are the actor's\" reads true, and it would hand \
             every member of a channel the whole shared scope on an actor key."
        );
        assert_eq!(
            entries[0].column, "scope_id",
            "the entry keys on the scope column, whose actor-scoped kinds \
             (mail/post/calendar/card) are what the walk is complete for"
        );
        assert!(
            matches!(entries[0].export, Export::Verbatim),
            "`Verbatim`, not `Shaped`: the conversations domain row-filters to \
             `kind = 'conv'`, and `Export::Shaped` asserts the domain carries the \
             table WHOLE -- a partial `Shaped` is deliberately inexpressible \
             (`every_shaped_verdict_covers_every_column`). A table may carry an \
             emitting verdict AND be read by a shaped domain on a different key; \
             that combination is this ruling's, and flipping the verdict to \
             announce the domain would be a lie about coverage."
        );
    }

    /// The membership resolution is the ACCESS RULE, so a non-member's export
    /// must not reach the channel — the property that makes this door a shaped
    /// domain rather than the forbidden channel-column registry entry.
    #[test]
    fn a_non_member_reaches_no_conv_record() {
        let db = CacheDb::open_in_memory().unwrap();
        let (_actor, _channel) = seed_actor_with_mail_and_conv(&db);
        let stranger = [42u8; 32];

        let conn = db.conn.blocking_lock();
        let conv = gather_conv_records(&conn, &stranger).unwrap();
        assert!(
            conv.is_none(),
            "an actor on no channel resolves to no channel set, so the record \
             read has nothing to read: {conv:?}. A registry entry on a channel \
             column would have handed them the whole table instead."
        );
    }

    /// `Verbatim` emits every column, with blobs as lowercase hex.
    #[test]
    fn a_verbatim_verdict_emits_every_column() {
        let conn = synthetic_conn();
        let actor = [7u8; 32];
        conn.execute(
            "INSERT INTO t_blob (actor_id, note, secret, payload, n) VALUES (?1, 'hi', 's', ?2, 5)",
            rusqlite::params![actor.to_vec(), vec![0xde_u8, 0xad]],
        )
        .unwrap();

        let entries = [synth(
            "t_blob",
            "actor_id",
            ActorKey::Blob,
            Export::Verbatim,
        )];
        let set = gather_export_set(&conn, &entries, &actor).unwrap();

        let rows = ndjson_of(&set, "t_blob");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["actor_id"], hex::encode(actor));
        assert_eq!(rows[0]["note"], "hi");
        assert_eq!(rows[0]["secret"], "s");
        assert_eq!(rows[0]["payload"], "dead", "blobs ride as lowercase hex");
        assert_eq!(rows[0]["n"], 5);
    }

    /// `Redacted` omits exactly its named columns and nothing else.
    #[test]
    fn a_redacted_verdict_omits_exactly_its_named_columns() {
        let conn = synthetic_conn();
        let actor = [9u8; 32];
        conn.execute(
            "INSERT INTO t_blob (actor_id, note, secret, payload, n) VALUES (?1, 'keep', 'drop', ?2, 1)",
            rusqlite::params![actor.to_vec(), vec![0x01_u8]],
        )
        .unwrap();

        let entries = [synth(
            "t_blob",
            "actor_id",
            ActorKey::Blob,
            Export::Redacted {
                omit: &["secret", "payload"],
                reason: "synthetic",
            },
        )];
        let set = gather_export_set(&conn, &entries, &actor).unwrap();

        let rows = ndjson_of(&set, "t_blob");
        assert_eq!(rows.len(), 1);
        let obj = rows[0].as_object().unwrap();
        assert!(obj.contains_key("note"), "unnamed columns still ride");
        assert!(obj.contains_key("actor_id"));
        assert!(obj.contains_key("n"));
        assert!(!obj.contains_key("secret"), "named column must be omitted");
        assert!(!obj.contains_key("payload"), "named column must be omitted");
    }

    /// The read binds the entry's own [`ActorKey`] spelling — the trap
    /// in read form. Each half is asserted against the *other* encoding's
    /// storage, so a walk that hard-codes either one reds here.
    #[test]
    fn the_export_read_binds_the_declared_actor_key_encoding() {
        let conn = synthetic_conn();
        let actor = [3u8; 32];
        // Blob table stores raw bytes; hex table stores lowercase hex.
        conn.execute(
            "INSERT INTO t_blob (actor_id, note) VALUES (?1, 'blob-row')",
            rusqlite::params![actor.to_vec()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO t_hex (owner, note) VALUES (?1, 'hex-row')",
            rusqlite::params![hex::encode(actor)],
        )
        .unwrap();

        let entries = [
            synth("t_blob", "actor_id", ActorKey::Blob, Export::Verbatim),
            synth("t_hex", "owner", ActorKey::Hex, Export::Verbatim),
        ];
        let set = gather_export_set(&conn, &entries, &actor).unwrap();
        assert_eq!(ndjson_of(&set, "t_blob")[0]["note"], "blob-row");
        assert_eq!(ndjson_of(&set, "t_hex")[0]["note"], "hex-row");

        // The inverse: declaring the wrong encoding matches nothing and emits
        // nothing — silently, which is exactly why the encoding is registry
        // data and not a convention.
        let wrong = [
            synth("t_blob", "actor_id", ActorKey::Hex, Export::Verbatim),
            synth("t_hex", "owner", ActorKey::Blob, Export::Verbatim),
        ];
        let empty = gather_export_set(&conn, &wrong, &actor).unwrap();
        assert!(
            empty.tables.is_empty(),
            "a mis-declared encoding matches no rows -- it does not error, it \
             emits an empty export: {:?}",
            empty.tables
        );
    }

    /// A table absent from this build's schema is skipped, not an error —
    /// otherwise a nest built without the bridge features could never export.
    #[test]
    fn a_table_absent_from_the_schema_is_skipped() {
        let conn = synthetic_conn();
        let entries = [
            synth(
                "t_no_such_table",
                "actor_id",
                ActorKey::Blob,
                Export::Verbatim,
            ),
            synth(
                "t_also_absent",
                "actor_id",
                ActorKey::Blob,
                Export::WithheldSecret("synthetic"),
            ),
            synth(
                "t_absent_too",
                "actor_id",
                ActorKey::Blob,
                Export::Unreviewed,
            ),
        ];
        let set = gather_export_set(&conn, &entries, &[0u8; 32]).unwrap();
        assert!(set.tables.is_empty());
        assert!(
            set.withheld.is_empty() && set.unreviewed.is_empty(),
            "a table this nest does not have is not something it holds and \
             withholds: {set:?}"
        );
    }

    /// The coverage declaration: withheld tables by name + reason class,
    /// unreviewed tables by name, and the partiality flag that follows.
    #[test]
    fn the_walk_declares_what_it_withheld_and_what_it_has_not_reviewed() {
        let conn = synthetic_conn();
        let entries = [
            synth(
                "t_blob",
                "actor_id",
                ActorKey::Blob,
                Export::WithheldSecret("synthetic"),
            ),
            synth("t_hex", "owner", ActorKey::Hex, Export::Unreviewed),
        ];
        let set = gather_export_set(&conn, &entries, &[1u8; 32]).unwrap();

        assert_eq!(set.withheld.len(), 1);
        assert_eq!(set.withheld[0].table, "t_blob");
        assert_eq!(set.withheld[0].reason_class, "secret");
        assert_eq!(set.unreviewed, vec!["t_hex"]);
        assert!(set.partial());

        // …and with every table ruled, the export stops calling itself partial.
        let ruled = [synth(
            "t_hex",
            "owner",
            ActorKey::Hex,
            Export::WithheldDerived("synthetic"),
        )];
        let full = gather_export_set(&conn, &ruled, &[1u8; 32]).unwrap();
        assert!(!full.partial());
        assert_eq!(full.withheld[0].reason_class, "derived");
    }

    /// A `Shaped` verdict emits nothing here — its rows already reach the
    /// archive through a named shaped domain, and a second copy would be the
    /// duplicate the verdict exists to prevent.
    #[test]
    fn a_shaped_verdict_emits_nothing_from_the_registry_walk() {
        let conn = synthetic_conn();
        let actor = [4u8; 32];
        conn.execute(
            "INSERT INTO t_blob (actor_id, note) VALUES (?1, 'served by a shaped domain')",
            rusqlite::params![actor.to_vec()],
        )
        .unwrap();

        let entries = [synth(
            "t_blob",
            "actor_id",
            ActorKey::Blob,
            Export::Shaped {
                domain: "synthetic.json",
                covers: &["note"],
                reason: "synthetic",
            },
        )];
        let set = gather_export_set(&conn, &entries, &actor).unwrap();
        assert!(set.tables.is_empty());
        assert!(set.withheld.is_empty() && set.unreviewed.is_empty());
    }

    /// Every reason class the manifest can print, and the non-withholding
    /// verdicts that must print none.
    #[test]
    fn every_withheld_verdict_has_a_reason_class() {
        assert_eq!(
            Export::WithheldSecret("r").withheld_reason_class(),
            Some("secret")
        );
        assert_eq!(
            Export::WithheldDerived("r").withheld_reason_class(),
            Some("derived")
        );
        assert_eq!(
            Export::WithheldOperational("r").withheld_reason_class(),
            Some("operational")
        );
        assert_eq!(Export::Verbatim.withheld_reason_class(), None);
        assert_eq!(Export::Unreviewed.withheld_reason_class(), None);
        assert_eq!(
            Export::Redacted {
                omit: &[],
                reason: "r"
            }
            .withheld_reason_class(),
            None
        );
        assert_eq!(
            Export::Shaped {
                domain: "d",
                covers: &[],
                reason: "r"
            }
            .withheld_reason_class(),
            None
        );
    }

    /// [`export_emit_legs`] selects on the verdict and nothing else.
    #[test]
    fn export_emit_legs_selects_exactly_the_emitting_verdicts() {
        for entry in export_emit_legs() {
            assert!(
                matches!(entry.export, Export::Verbatim | Export::Redacted { .. }),
                "{} is not an emitting verdict",
                entry.table
            );
        }
        let emitting = ACTOR_TABLES
            .iter()
            .filter(|e| matches!(e.export, Export::Verbatim | Export::Redacted { .. }))
            .count();
        assert_eq!(export_emit_legs().count(), emitting);
    }
}
