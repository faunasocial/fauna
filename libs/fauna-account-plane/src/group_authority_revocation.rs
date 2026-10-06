//! The group plane's **authority-device severance** — the publisher half of
//! the authority-device revocation ruling, and the first production re-mint
//! trigger this plane has ever had.
//!
//! Authority: `docs/goal/architecture/account-data-taxonomy.md` § The
//! recipient-set scheme → *Severance, per axis* (*An authority device's
//! removal* — all three rules), → *The roster kind* → *The per-writer roster
//! cell* (the cell shape, who may write a `Removed`, and what a severance
//! re-publishes) and → *Mint triggers* (trigger (1), the severance mint, and
//! its line-committed arm); § Implementation status today (the 2026-09-19
//! entry, whose "What is NOT code: any publisher" this pass closes, and the
//! 2026-09-27 per-writer-cell entry this pass builds).
//!
//! # What is owed
//!
//! Every authority check on this plane accepts any device whose cert chains to
//! the authority root — and a device removed from the authority's own account
//! keeps its device secret, its root-signed cert and the never-rotated
//! machinery root, so it could go on enrolling actors it controls, binding
//! members' cells to its own reception key, and minting. The account plane
//! closes the same shape with `FleetView`, but that plane is sealed to the
//! authority's own fleet: a cross-account member can never read it, and the
//! group plane has no nest to ask.
//!
//! The reader half landed 2026-09-19 — `fauna.group.authority-revocation`,
//! [`GroupAuthority`] and its one `verify_device_cert` behind every roster
//! view, membership witness, mint resolver and authored shred. **Nothing wrote
//! one.** This pass is that writer, the group plane's pump leg.
//!
//! # The three writes, and why they land in this order
//!
//! For every scope this device holds the machinery root of and is the
//! authority of:
//!
//! 1. **Re-admissions.** Every entry id the authority line — *as this pass
//!    will leave it* — leaves without an honored `Enrolled` cell and without
//!    an honored `Removed` cell is re-published at the **same entry id, under
//!    this device's own cell** (`<entry>/<this device>`), from the ceremony
//!    snapshot's member-signed key. Nothing is written into the revoked
//!    device's cell: under the per-writer roster cell it is dead on its own
//!    at every reader that has the line, and no `Removed` marks a severance
//!    (an authored `Removed` is a member removal, which this pass never
//!    writes). Re-signing *into* the revoked device's cell cannot work and is
//!    unrepresentable besides — first-contact strictness refuses a row not
//!    sitting at its own author's cell.
//! 2. **The severance mint.** Trigger (1) read as MERGED STATE
//!    ([`owed_mint`]), never as what this pass wrote: a scope whose merged
//!    roster holds an honored `Removed` or whose line revokes a device, that
//!    lists at least one verified member and resolves **no admissible mint**
//!    — where admissibility includes the line-committed arm, the tip's
//!    `GroupMintCore::revoked_past` covering the line as this pass will leave
//!    it — is minted over its post-severance roster, past that line. So a tip
//!    minted before the revocation is inadmissible at every reader with the
//!    line, and the mint re-keys the group past the revoked device, as an
//!    authority-device compromise deserves.
//! 3. **The revocations**, last.
//!
//! The order is the ruling's fail-safe direction read forward. Until a
//! revocation merges at a reader, that reader still accepts the revoked device
//! (the staleness window every frontier-merged fact carries); once it merges,
//! the revoked device's cells are refused and **the scope resolves no tip
//! there until the re-admissions and the mint arrive**. Rows leave one writer
//! log in write order, so putting the revocation last means no reader has to
//! sit in that gap. A crash between any two writes leaves a state the next
//! pass finishes, and never one where a member is severed with nothing owed to
//! re-admit it. That holds for the mint only because its trigger is state: a
//! pass that died after its re-admissions, or whose mint errored, comes back
//! to entries that already stand under its own cell — nothing left to
//! re-admit — and the mint is still owed, by a scope entered on that debt
//! alone. A failed mint does not hold the revocations back: they are the
//! security-critical write, and the no-tip gap is the fail-safe direction, now
//! transient.
//!
//! # A device the line refuses
//!
//! All three writes are a **live** device's. A device the authority line *as
//! this pass will leave it* revokes — it merged its own fleet `Removed`, or a
//! sibling's revocation of it — publishes the revocations it owes (rule (2):
//! its own standing is not consulted, and its own revocation is among them)
//! and nothing else. A re-admission under its carriage
//! never verifies, so the entry is stale again next pass and re-admitted
//! again, for ever, a dead cell per pass; a mint it authors is inadmissible by
//! construction, so the debt it paid still stands and is paid again. Both are
//! a live sibling's to write. Such a scope also never asks the ceremony record
//! for anything — an ended device's nest connection dies with its writer-key
//! tombstone, before the fleet `Removed` exists, and that (not this guard) is
//! the primary control.
//!
//! # Why the re-published row is NOT the merged one
//!
//! A forged cell carries the authority's `authorization` and `authority_sig`
//! verbatim (both cover only the content-derived entry id, never the reception
//! key), so an authority that re-published *those* bytes would hand an
//! attacker an entry it could never forge. The source is this account's own
//! retained attestation of what it delivered — the signed deliver envelope
//! kept on the initiated side of the `fauna.state.group-share-ceremony` record
//! (`InitiatedGroupShare::deliver`, read off this replica's own store),
//! verified under the authority actor's signature before a byte of it is read.
//!
//! # The source at every severance — the ceremony snapshot alone
//!
//! Under the per-writer cell a re-admission keeps the entry id, so the
//! ceremony's deliver snapshot names every entry at every severance: the first
//! removal re-publishes `E` under device B, the second re-publishes `E` under
//! device C, each from the same attested row — its identity core (which IS the
//! entry id) and the member-signed reception key. No retained record of
//! written cells is needed, and none exists (the `group_readmissions` cell
//! retired 2026-09-27 with the shared cell that needed it). An entry the
//! snapshot does not name is *unsourced*: refused at every reader once the
//! revocation merges, and — with member adds unbuilt — only a forgery can
//! leave one (the trigger-(2) build lands a member's rotated key in this
//! snapshot before any re-admission may use it; the member-add writer will
//! land its entries here too).
//!
//! The two shortcuts stay refused by the ruling. Matching a stale cell's
//! *member actor* against the snapshot reads the link off the merged cell,
//! which a revoked device can still write (it keeps the machinery root): it
//! could make this pass re-admit anybody the snapshot ever named. Trusting a
//! *bound merged entry* is rule (3) read backwards — at the second removal
//! every stale cell's author IS the device just revoked. So the entry id is
//! the only thing a merged row contributes, and the snapshot answers for it or
//! the pass leaves it alone.
//!
//! **The snapshot row's original author is irrelevant here.** A binding is
//! only ever checked against the key the entry's own carriage names, and the
//! re-admission is a **new row under this device's own carriage, in its own
//! cell** — as it must be, since the whole point is that a live sibling
//! re-publishes what the *revoked* device vouched for.
//!
//! # Cost
//!
//! One local read on an account that holds no group scope. The device-set read
//! and the ceremony record — the one read that can reach the nest — are loaded
//! only once a scope with work is actually found, so a healed account never
//! pays for either again.

use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::Timestamp;
use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode};
use fauna_core::group_ceremony::{GroupPlaneRow, GroupShareConfig, verify_group_share_deliver};
use fauna_core::group_generation::resolve_admissible_group_tip;
use fauna_core::group_scope::{
    GroupAuthority, GroupRosterRecord, RosterEntryCore, RosterMember, RosterView,
    parse_roster_cell_key, roster_cell_key, roster_entry_id, sign_authority_revocation,
    sign_roster_enrollment,
};
use fauna_core::identity::ActorId;
use fauna_mls::wrapped_blob::group_generation_wraps::build_group_mint;
use fauna_protocol::RpcRequester;
use fauna_protocol::group_state::{
    KIND_GROUP_AUTHORITY_REVOCATION, KIND_GROUP_GENERATION_MINT, KIND_GROUP_GENERATION_WRAP,
    KIND_GROUP_ROSTER,
};

use crate::account_state_plane::{ItemId, heal_parents};
use crate::generation_tip::GenerationTrust;
use crate::group_state_plane::{
    GroupStatePlane, HeldAuthorityScope, NoFeed, held_authority_scopes,
};

/// What one severance pass did (the pump's `group_authority_revocation`
/// report slot).
///
/// Every counter is a *fact about this pass*, so "nothing to do" is
/// distinguishable from "something was left": a non-zero `unsourced` means a
/// revoked device's entry could not be re-published because this account's
/// ceremony snapshot names no such entry — loud, because a genuine member
/// would stay severed until one is found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RevocationPass {
    /// Scopes this device holds a machinery root for **and** is the birth
    /// record's authority of — the only scopes whose plane it may write.
    pub authority_scopes: usize,
    /// `fauna.group.authority-revocation` rows published this pass.
    pub revoked: usize,
    /// Entries re-published under this device's own cell this pass.
    pub readmitted: usize,
    /// Severance mints published this pass — at most one per scope.
    pub minted: usize,
    /// Stale entry ids this account's ceremony snapshot names nothing at —
    /// which, with member adds unbuilt, is exactly what a revoked device's own
    /// forgery looks like. The cell is refused at every reader once the
    /// revocation merges, so a genuine one would be an *availability* loss,
    /// never an authority one — but it would be a real one, hence the counter.
    pub unsourced: usize,
}

/// One scope this pass has work in, with the merged state distilled to the
/// three questions it asks: which devices are owed a revocation, what the
/// authority line will read once they land, and which entries that line
/// leaves standing nowhere.
struct SeveranceScope {
    held: HeldAuthorityScope,
    /// The revocation rows this pass will publish — built before anything is
    /// written, because the authority line below must already contain them.
    owed: Vec<(String, Vec<u8>)>,
    /// The authority line **as it will be** once `owed` lands: the scope's
    /// merged revocation rows plus this pass's own. Everything downstream —
    /// the candidate test, the roster view the mint wraps to, and the set the
    /// mint is minted past — reads this one, or the mint would seal to
    /// entries the readers are about to refuse and so be inadmissible the
    /// moment the revocations arrive.
    authority: GroupAuthority,
    /// Entry ids some merged `Enrolled` cell names under a device `authority`
    /// refuses, that `authority` leaves without an honored `Enrolled` and
    /// without an honored `Removed`, ascending — the ruling's re-admission
    /// candidates.
    stale: Vec<[u8; 32]>,
    /// `authority` refuses THIS device, so `owed` is all the scope gets from
    /// it (module header, *A device the line refuses*): `stale` is empty, no
    /// mint is paid, and the ceremony record is never asked.
    revocations_only: bool,
}

/// Mint trigger (1) read as **state**: the wrap set and the parents of the
/// severance mint `states` owes under `authority`, or `None` when it owes
/// none. Owed exactly while the merged roster holds an honored `Removed` or
/// the line revokes a device, the roster lists a verified member, and the
/// scope resolves no admissible mint.
///
/// - The `Removed`-or-revocation conjunct is trigger (1) itself, and what
///   keeps "adds never mint" true of a scope that never severed: it is not
///   even resolved (which also spares every such scope the per-pass signature
///   checks). In a once-severed scope an add's entry that outruns its top-up
///   IS a debt — one harmless generation, ratified by the owner doc, never
///   refined away here.
/// - "No admissible mint" is the resolver's own predicate with
///   observer-keyability taken out — hence the always-`true` closure — and
///   the line-committed arm inside it: a tip whose `revoked_past` does not
///   cover `authority`'s revoked set is nobody's tip. Nothing is claimed about
///   this device's key reach: no group generation custody exists here
///   (`GroupSealing::GenerationTip` is registered and deliberately unbuilt),
///   and admissibility is the observer-independent half.
/// - Stable by construction: the mint this returns the inputs of wraps every
///   verified member and is minted past `authority`'s whole revoked set, so
///   once written it IS the admissible mint, and the next read owes nothing.
///
/// Parents are the mint DAG's current leaves — the account-plane
/// heal-mint's rule transplanted, capped by the caller
/// (`fauna_core::generation::MAX_MINT_PARENTS`); `leaf_ids` is a fact about
/// the DAG's edges alone, so no verdict above reaches it.
fn owed_mint(
    scope_id: &[u8; 32],
    authority: &GroupAuthority,
    states: &[StateEntry],
) -> Option<(Vec<RosterMember>, Vec<[u8; 32]>)> {
    let view = RosterView::build(scope_id, authority, rows_of(states, KIND_GROUP_ROSTER));
    let severed = view.excluded_entries().next().is_some() || authority.revoked().next().is_some();
    if !severed {
        return None;
    }
    let members: Vec<RosterMember> = view.wrap_targets().cloned().collect();
    if members.is_empty() {
        return None;
    }
    let resolution = resolve_admissible_group_tip(
        &view,
        authority,
        rows_of(states, KIND_GROUP_GENERATION_MINT),
        rows_of(states, KIND_GROUP_GENERATION_WRAP),
        |_, _, _| true,
    );
    resolution
        .tip
        .is_none()
        .then_some((members, resolution.leaf_ids))
}

/// The identity one severance pass writes as, and the instant every row it
/// stamps carries.
///
/// The key and the carriage travel as ONE value for
/// `AccountStoreHandle::group_ceremony_authority`'s reason: the carriage names
/// the very key it is paired with, and splitting them across two reads is what
/// would let a rotation slip a stale witness under a fresh key — here, onto
/// every roster entry and mint this pass publishes.
struct Severer<'a> {
    /// This device's signing key — the store's writer, and the revoker.
    writer_key: &'a SigningKey,
    /// Its root-signed `DeviceAuthorization` in the `EmbedAsBytes` carriage.
    carriage: &'a [u8],
    /// The authority account: the scopes' birth authority, and the actor the
    /// retained deliver verifies under.
    actor: &'a ActorId,
    /// One instant for the whole pass, so a scope's revocation, re-admissions
    /// and mint carry the same advisory stamp.
    now_ms: i64,
}

impl Severer<'_> {
    fn device_id(&self) -> [u8; 32] {
        self.writer_key.verifying_key().to_bytes()
    }
}

/// Rows of one kind in a scope's merged state, as the `(key, value)` pairs
/// every `fauna_core::group_scope` reader takes.
fn rows_of<'a>(states: &'a [StateEntry], kind: &str) -> Vec<(&'a str, &'a [u8])> {
    states
        .iter()
        .filter(|r| r.kind == kind)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect()
}

/// Distil one held authority scope into the work this pass owes it, or `None`
/// when it owes none. Pure over the scope's merged rows plus the fleet's
/// removed set — no writes, no config, no clock but the revocation stamp.
fn severance_work(
    held: HeldAuthorityScope,
    me: &Severer<'_>,
    prior: &[ActorId],
    removed_devices: &BTreeSet<[u8; 32]>,
) -> Option<SeveranceScope> {
    let merged = GroupAuthority::build(
        &held.scope_id,
        me.actor,
        prior,
        rows_of(&held.states, KIND_GROUP_AUTHORITY_REVOCATION),
    );

    // Rule (1), authored never unconditional: this device signs each row, and
    // its own cert carriage is what makes the row count at a reader. Rule (2):
    // the revoker's own standing is not consulted, here or at the reader — a
    // mutual race revokes both, the fail-safe direction.
    let owed: Vec<(String, Vec<u8>)> = removed_devices
        .iter()
        .filter(|device| !merged.is_revoked(device))
        .filter_map(|device| {
            let (key, record) = sign_authority_revocation(
                me.writer_key,
                me.carriage.to_vec(),
                held.scope_id,
                *device,
                me.now_ms,
            );
            match canonical_encode(&record) {
                Ok(value) => Some((key, value.to_vec())),
                Err(e) => {
                    tracing::warn!(
                        device = %fauna_core::hex32::encode(device),
                        "authority severance: revocation will not encode: {e} — skipped"
                    );
                    None
                }
            }
        })
        .collect();

    // The line as it WILL be. Re-built rather than mutated: `GroupAuthority`
    // has one constructor on purpose ("no reader can be built without a
    // revocation input"), and a second way to reach the revoked set would be a
    // second answer to the question every verdict below rests on.
    let authority = GroupAuthority::build(
        &held.scope_id,
        me.actor,
        prior,
        rows_of(&held.states, KIND_GROUP_AUTHORITY_REVOCATION)
            .into_iter()
            .chain(owed.iter().map(|(k, v)| (k.as_str(), v.as_slice()))),
    );

    // A device the line refuses writes its revocations and nothing else. Read
    // off the line as it WILL be, so the pass that revokes this device is
    // already bound by it.
    if authority.is_revoked(&me.device_id()) {
        return (!owed.is_empty()).then_some(SeveranceScope {
            held,
            owed,
            authority,
            stale: Vec::new(),
            revocations_only: true,
        });
    }

    // Rule (3)'s re-admission candidates, read off the roster as the line
    // will leave it: an entry some `Enrolled` cell names under a device the
    // line refuses, standing nowhere the line honors — and not excluded by an
    // honored `Removed` (a removed member is never re-admitted; the strike is
    // structural).
    let roster_rows = rows_of(&held.states, KIND_GROUP_ROSTER);
    let standing = RosterView::build(&held.scope_id, &authority, roster_rows.iter().copied());
    let stale: BTreeSet<[u8; 32]> = roster_rows
        .iter()
        .filter_map(|(_, value)| canonical_decode::<GroupRosterRecord>(value).ok())
        .filter(|record| matches!(record, GroupRosterRecord::Enrolled { .. }))
        .filter(|record| {
            record
                .authority_device_key()
                .is_some_and(|device| authority.is_revoked(&device))
        })
        .filter_map(|record| record.entry_id())
        .filter(|entry| !standing.is_enrolled_entry(entry) && !standing.is_excluded_entry(entry))
        .collect();
    let stale: Vec<[u8; 32]> = stale.into_iter().collect();

    // The third kind of work: a mint an earlier pass left owed (it died before
    // it, or it errored). Such a scope has no revocation owed and no stale
    // entry, so without this test it would never be entered again.
    if owed.is_empty()
        && stale.is_empty()
        && owed_mint(&held.scope_id, &authority, &held.states).is_none()
    {
        return None;
    }
    Some(SeveranceScope {
        held,
        owed,
        authority,
        stale,
        revocations_only: false,
    })
}

/// One enrollment the retained deliver attests: the entry (its identity core,
/// which IS its id) and the reception key that member signed for.
struct Attested {
    entry_id: [u8; 32],
    core: RosterEntryCore,
    reception_pubkey: Vec<u8>,
}

/// Every enrollment of `scope_id` the (already actor-verified) snapshot
/// attests. A row that does not sit at its own content-derived entry id is no
/// attestation of that entry at all, and is dropped.
fn snapshot_enrollments(snapshot: &[GroupPlaneRow], scope_id: &[u8; 32]) -> Vec<Attested> {
    snapshot
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER)
        .filter_map(|row| {
            let GroupRosterRecord::Enrolled {
                core,
                reception_pubkey,
                ..
            } = canonical_decode(&row.value).ok()?
            else {
                return None;
            };
            let entry_id = roster_entry_id(&core).ok()?;
            let at_own_entry =
                parse_roster_cell_key(&row.key).is_some_and(|(entry, _)| entry == entry_id);
            (core.scope_id == *scope_id && at_own_entry).then_some(Attested {
                entry_id,
                core,
                reception_pubkey,
            })
        })
        .collect()
}

/// This account's ceremony record as one severance pass uses it: read once,
/// lazily — only once work is found. A seam so a test can stand one shared
/// record in for every replica's synced row.
trait RetainedRecord {
    /// The ceremony records.
    async fn load(&self) -> Result<GroupShareConfig>;
}

/// The production record: the `fauna.state.group-share-ceremony` row on this
/// replica's own store — a local read, so a device with no nest left (the
/// removed-device case) reads it all the same.
struct StoreRecord<'a, B: StoreBackend>(&'a AccountStore<B>);

impl<B: StoreBackend> RetainedRecord for StoreRecord<'_, B> {
    async fn load(&self) -> Result<GroupShareConfig> {
        crate::group_share_rows::read_group_shares(self.0).await
    }
}

/// This account's retained attestation of what it delivered into one scope,
/// verified under the authority actor's own signature before a byte is read.
fn retained_snapshot(
    cfg: &GroupShareConfig,
    scope_id: &[u8; 32],
    actor: &ActorId,
) -> Result<Vec<GroupPlaneRow>> {
    let Some(record) = cfg.initiated.iter().find(|r| r.scope_id == *scope_id) else {
        bail!("no initiated-share record for a scope this actor is the authority of");
    };
    if record.deliver.is_empty() {
        bail!("the initiated-share record carries no deliver — nothing was ever written here");
    }
    let envelope: EmbedAsBytes =
        canonical_decode(&record.deliver).context("the retained deliver envelope")?;
    let deliver = verify_group_share_deliver(&envelope, actor)
        .map_err(|e| anyhow!("the retained deliver does not verify under this actor: {e}"))?;
    if deliver.scope_id != *scope_id {
        bail!("the retained deliver names a different scope than the record it sits in");
    }
    Ok(deliver.machinery_snapshot)
}

/// Publish one scope's severance: re-admissions, the mint, then the
/// revocations. Errors are the scope's, never the pass's.
async fn sever_scope<B: StoreBackend>(
    store: &AccountStore<B>,
    me: &Severer<'_>,
    cfg: &GroupShareConfig,
    scope: &SeveranceScope,
    pass: &mut RevocationPass,
) -> Result<()> {
    let attested = snapshot_enrollments(
        &retained_snapshot(cfg, &scope.held.scope_id, me.actor)?,
        &scope.held.scope_id,
    );
    let plane = scope_plane(store, me, scope)?;

    // ── 1. Re-admissions: the same entry id, this device's own cell, the
    // attested key ──
    let mut readmitted_here = 0usize;
    for entry_id in &scope.stale {
        let Some(attested) = attested.iter().find(|a| a.entry_id == *entry_id) else {
            tracing::warn!(
                entry = %fauna_core::hex32::encode(entry_id),
                "authority severance: this account's ceremony snapshot names no such entry — \
                 left alone; its revoked author's cell is refused at every reader"
            );
            pass.unsourced += 1;
            continue;
        };
        let (derived, record) = sign_roster_enrollment(
            me.writer_key,
            attested.core.clone(),
            attested.reception_pubkey.clone(),
            me.carriage.to_vec(),
            me.now_ms,
        )
        .context("signing the re-admitted roster entry")?;
        debug_assert_eq!(
            derived, *entry_id,
            "the same core derives the same entry id"
        );
        plane
            .put(
                &ItemId {
                    kind: KIND_GROUP_ROSTER.into(),
                    key: roster_cell_key(&derived, &me.device_id()),
                },
                canonical_encode(&record)
                    .context("encoding the re-admitted roster entry")?
                    .to_vec(),
                None,
            )
            .await
            .context("publishing the re-admitted roster entry")?;
        pass.readmitted += 1;
        readmitted_here += 1;
    }

    // ── 2. The severance mint — trigger (1), read off the state just written ──
    // Never off `readmitted_here`: a pass that comes back after dying here, or
    // after this mint errored, re-admits nothing and owes the mint all the
    // same. An error is logged and the revocations still go out (module
    // header).
    match mint_severance(store, &plane, me, scope).await {
        Ok(true) => pass.minted += 1,
        Ok(false) if readmitted_here > 0 => tracing::warn!(
            "authority severance: no verified member survives the severance — no mint, and the \
             scope resolves no tip until a re-admission lands"
        ),
        Ok(false) => {}
        Err(e) => tracing::warn!("authority severance: the severance mint: {e:#}"),
    }

    // ── 3. The revocations, last (module header owns the order) ──
    publish_revocations(&plane, scope, pass).await
}

/// The scope's plane as this pass writes it.
fn scope_plane<'a, B: StoreBackend>(
    store: &'a AccountStore<B>,
    me: &'a Severer<'_>,
    scope: &'a SeveranceScope,
) -> Result<GroupStatePlane<'a, B, NoFeed>> {
    GroupStatePlane::new(
        store,
        &NoFeed,
        &scope.held.root,
        me.writer_key,
        &scope.held.scope_id,
    )
}

/// Publish the scope's owed revocations — a full severance's last write, and
/// the only one a device the line refuses makes.
async fn publish_revocations<B: StoreBackend, R: RpcRequester>(
    plane: &GroupStatePlane<'_, B, R>,
    scope: &SeveranceScope,
    pass: &mut RevocationPass,
) -> Result<()> {
    for (key, value) in &scope.owed {
        plane
            .put(
                &ItemId {
                    kind: KIND_GROUP_AUTHORITY_REVOCATION.into(),
                    key: key.clone(),
                },
                value.clone(),
                None,
            )
            .await
            .context("publishing the authority-device revocation")?;
        pass.revoked += 1;
    }
    Ok(())
}

/// Pay the scope's mint debt, if it has one ([`owed_mint`]), over the roster
/// read back from the store — so the wrap set is exactly what a reader will
/// compute — and PAST the line as this pass will leave it (the mint's
/// `revoked_past` is that line's whole revoked set). `Ok(false)` when nothing
/// is owed: an admissible mint already stands, the scope never severed, or no
/// verified member survives (a mint with an empty member set is refused by
/// the builder and is a forged shape at every reader).
async fn mint_severance<B: StoreBackend, R: RpcRequester>(
    store: &AccountStore<B>,
    plane: &GroupStatePlane<'_, B, R>,
    me: &Severer<'_>,
    scope: &SeveranceScope,
) -> Result<bool> {
    let states: Vec<StateEntry> = store
        .group_scope_states(plane.scope())
        .await?
        .into_iter()
        .filter(|r| !r.tombstone)
        .collect();
    let Some((members, leaves)) = owed_mint(&scope.held.scope_id, &scope.authority, &states) else {
        return Ok(false);
    };
    let built = build_group_mint(
        &members,
        heal_parents(&leaves),
        scope.authority.revoked().copied().collect(),
        me.writer_key,
        me.carriage.to_vec(),
        me.now_ms,
    )
    .map_err(|e| anyhow!("mint assembly failed: {e}"))?;
    plane
        .put(
            &ItemId {
                kind: KIND_GROUP_GENERATION_MINT.into(),
                key: fauna_core::hex32::encode(&built.generation_id),
            },
            canonical_encode(&built.record)
                .context("encoding the severance mint")?
                .to_vec(),
            None,
        )
        .await
        .context("publishing the severance mint")?;
    Ok(true)
}

/// One severance pass. The ceremony record's load is deliberately lazy — an
/// account with no severance owed never makes it; so is the device-set read,
/// for the same reason.
async fn severance_pass<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    carriage: &[u8],
    retained: &impl RetainedRecord,
) -> Result<RevocationPass> {
    let scopes = held_authority_scopes(store, &trust.root).await?;
    let mut pass = RevocationPass {
        authority_scopes: scopes.len(),
        ..RevocationPass::default()
    };
    if scopes.is_empty() {
        return Ok(pass);
    }
    // The ids this replica's merged device-set state excludes — one
    // derivation, one owner (`fleet_removal::removed_device_ids`), the same
    // set the peer leg severs a removed sibling's admission on.
    let removed: BTreeSet<[u8; 32]> = crate::fleet_removal::removed_device_ids(store, trust)
        .await?
        .into_iter()
        .collect();
    let me = Severer {
        writer_key,
        carriage,
        actor: &trust.root,
        now_ms: Timestamp::now_millis_or_zero() as i64,
    };
    let work: Vec<SeveranceScope> = scopes
        .into_iter()
        .filter_map(|held| severance_work(held, &me, &trust.prior, &removed))
        .collect();
    // Revocations-only scopes first, and before the record's load: they need
    // nothing from it, and a device the line refuses owes its revocations
    // whatever else fails.
    let (revocations_only, full): (Vec<SeveranceScope>, Vec<SeveranceScope>) =
        work.into_iter().partition(|scope| scope.revocations_only);
    for scope in &revocations_only {
        let published = match scope_plane(store, &me, scope) {
            Ok(plane) => publish_revocations(&plane, scope, &mut pass).await,
            Err(e) => Err(e),
        };
        scope_outcome(scope, published)?;
    }
    if full.is_empty() {
        return Ok(pass);
    }
    let cfg = retained
        .load()
        .await
        .context("authority severance: the ceremony record")?;
    for scope in &full {
        let severed = sever_scope(store, &me, &cfg, scope, &mut pass).await;
        scope_outcome(scope, severed)?;
    }
    Ok(pass)
}

/// One scope's result, as the pass takes it: a rotated writer is the whole
/// runtime's business (the pump reassembles on it) and ends the pass; anything
/// else is that scope's failure, logged, and the next pass retries it.
fn scope_outcome(scope: &SeveranceScope, result: Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(e) if fauna_account_store::store::is_stale_writer(&e) => Err(e),
        Err(e) => {
            tracing::warn!(
                scope = %fauna_core::hex32::encode(&scope.held.scope_id),
                "authority severance: {e:#}"
            );
            Ok(())
        }
    }
}

/// The pump step: publish every revocation this device's fleet view calls for,
/// re-admit what the revoked devices vouched for, and fire the severance mint.
/// Module docs own the decision table.
///
/// `carriage` is this device's root-signed `DeviceAuthorization` in the
/// `EmbedAsBytes` carriage every group-plane row embeds. `None` — the machine
/// has run no enrollment ceremony — is a quiet, self-healing absence, never an
/// error: without it this device can author nothing a reader would verify.
pub async fn ensure_revoked<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    carriage: Option<&[u8]>,
) -> Result<RevocationPass> {
    let Some(carriage) = carriage else {
        return Ok(RevocationPass::default());
    };
    severance_pass(store, trust, writer_key, carriage, &StoreRecord(store)).await
}

// `account-runtime` rather than this module's own `preference-store`: the
// fleet-removal writer these tests drive the pass from
// (`fleet_removal::write_removed` — what the devices page's removal gesture
// ultimately lands) lives behind it. `account-runtime` forwards
// `preference-store`, so the module under test is compiled whenever this is,
// and every gate that RUNS this crate's tests resolves both.
#[cfg(test)]
mod tests {
    use super::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::WriterId;
    use fauna_client_capabilities::group_ceremony::{
        begin_group_share, build_group_accept, build_group_deliver, ingest_group_frame,
        mark_group_accept_posted, mark_group_delivered, mark_group_offer_posted,
    };
    use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
    use fauna_core::data::{Capability, DeviceAuthorization};
    use fauna_core::encoding::sign_envelope;
    use fauna_core::group_generation::{
        GroupGenerationMintRecord, GroupHeldRootRecord, GroupReceptionKeyRecord,
    };
    use fauna_core::group_scope::verify_enrolled_entry;
    use fauna_core::identity::ActorKeypair;
    use fauna_mls::wrapped_blob::group_generation_wraps::open_group_generation_key_as_entry;
    use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
    use fauna_protocol::merge_policy::{
        KIND_DEVICE_SET, KIND_GROUP_MACHINERY_ROOT, home_scope_for_kind,
    };
    use fauna_protocol::scope::GroupScope;

    use crate::account_state_plane::AccountStatePlane;

    /// The authority account — the one that mints the scope, and whose fleet
    /// device A is removed from.
    fn authority() -> ActorKeypair {
        ActorKeypair::from_secret([0x21; 32])
    }
    /// The member on the other side of the two-party scope: a DIFFERENT
    /// account, which is the whole point of the group plane.
    fn member() -> ActorKeypair {
        ActorKeypair::from_secret([0x31; 32])
    }
    /// The authority's device that runs the ceremony and is then removed.
    fn device_a() -> SigningKey {
        SigningKey::from_bytes(&[0xA1; 32])
    }
    /// Its live sibling — the device whose pump pass severs A, and which is
    /// then removed in turn.
    fn device_b() -> SigningKey {
        SigningKey::from_bytes(&[0xB2; 32])
    }
    /// The third authority device, whose pass severs B. A second severance
    /// needs it: `fleet_removal::write_removed` refuses the writer's own id, so
    /// B's removal has to be written — and then severed — by somebody else.
    fn device_c() -> SigningKey {
        SigningKey::from_bytes(&[0xC3; 32])
    }

    fn now() -> Timestamp {
        Timestamp(1_700_000_000)
    }
    fn at_ms() -> i64 {
        1_700_000_000_000
    }

    /// `device`'s root-signed `DeviceAuthorization` in the `EmbedAsBytes`
    /// carriage every group-plane row embeds — production's exact shape.
    fn cert_for(device: &SigningKey) -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: authority().actor_id(),
            device_key: device.verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(&authority(), &cert).expect("sign cert");
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
            .expect("carriage")
            .to_vec()
    }

    /// No nest anywhere in this file; a request reaching the wire is a defect.
    #[derive(Debug, Clone)]
    struct NoNest;

    impl std::fmt::Display for NoNest {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "no nest in this test")
        }
    }
    impl std::error::Error for NoNest {}
    impl fauna_protocol::RpcErrorClass for NoNest {
        fn is_rejection(&self) -> bool {
            false
        }
    }
    impl RpcRequester for NoNest {
        type Error = NoNest;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> std::result::Result<Reply, NoNest>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err(NoNest)
        }
    }

    /// The account's ceremony record as every one of its devices sees it —
    /// one value shared by the replicas of a test, standing in for the
    /// fleet-synced `fauna.state.group-share-ceremony` row. What a pass
    /// records here is all a later device's pass has.
    #[derive(Clone)]
    struct AccountRecord(std::sync::Arc<std::sync::Mutex<GroupShareConfig>>);

    impl AccountRecord {
        fn new(cfg: GroupShareConfig) -> Self {
            Self(std::sync::Arc::new(std::sync::Mutex::new(cfg)))
        }
        fn snapshot(&self) -> GroupShareConfig {
            self.0.lock().unwrap().clone()
        }
    }

    impl RetainedRecord for AccountRecord {
        async fn load(&self) -> Result<GroupShareConfig> {
            Ok(self.snapshot())
        }
    }

    fn trust() -> GenerationTrust {
        GenerationTrust {
            root: authority().actor_id(),
            prior: Vec::new(),
            trusted_holders: Default::default(),
        }
    }

    /// Everything the ceremony left behind, driven through the production
    /// entry points on device A.
    #[derive(Clone)]
    struct Ceremony {
        scope_id: [u8; 32],
        held_root_row: GroupHeldRootRecord,
        plane_rows: Vec<GroupPlaneRow>,
        member_reception: GroupReceptionKeyRecord,
        member_entry_id: [u8; 32],
        first_generation: [u8; 32],
        cfg: GroupShareConfig,
    }

    /// Device A enrols the member and mints the first generation — every row
    /// through `fauna_client_capabilities::group_ceremony`, none hand-built.
    fn run_ceremony() -> Ceremony {
        let mut auth_cfg = GroupShareConfig::default();
        let mut ceremony = run_ceremony_on(&device_a(), &mut auth_cfg);
        ceremony.cfg = auth_cfg;
        ceremony
    }

    /// One ceremony driven on `device`, recorded into the account's
    /// `auth_cfg` beside whatever it already holds. The returned `cfg` is a
    /// copy as of this ceremony.
    fn run_ceremony_on(device: &SigningKey, auth_cfg: &mut GroupShareConfig) -> Ceremony {
        let mut member_cfg = GroupShareConfig::default();

        let begun =
            begin_group_share(auth_cfg, &authority(), member().actor_id(), now()).expect("begin");
        mark_group_offer_posted(auth_cfg, &begun.scope_id, &member().actor_id());
        ingest_group_frame(
            &mut member_cfg,
            &member().actor_id(),
            &authority().actor_id(),
            &begun.frame,
            now(),
        )
        .expect("member ingests the offer");

        let member_reception = GroupReceptionKeyRecord::mint(at_ms());
        let accept = build_group_accept(
            &mut member_cfg,
            &member(),
            &begun.scope_id,
            &member_reception,
            now(),
        )
        .expect("accept");
        mark_group_accept_posted(&mut member_cfg, &begun.scope_id);
        ingest_group_frame(
            auth_cfg,
            &authority().actor_id(),
            &member().actor_id(),
            &accept,
            now(),
        )
        .expect("authority ingests the accept");

        let own_reception = GroupReceptionKeyRecord::mint(at_ms());
        let delivered = build_group_deliver(
            auth_cfg,
            &authority(),
            device,
            cert_for(device),
            &own_reception,
            &begun.scope_id,
            &member().actor_id(),
            now(),
        )
        .expect("deliver");
        mark_group_delivered(auth_cfg, &begun.scope_id, &member().actor_id());

        // The member's own cell, as the deliver names it.
        let recorded = auth_cfg
            .initiated
            .iter()
            .find(|r| r.scope_id == begun.scope_id)
            .expect("the ceremony's record");
        let deliver_envelope: EmbedAsBytes =
            canonical_decode(&recorded.deliver).expect("deliver bytes");
        let deliver = verify_group_share_deliver(&deliver_envelope, &authority().actor_id())
            .expect("the deliver verifies");

        Ceremony {
            scope_id: begun.scope_id,
            held_root_row: begun.held_root_row,
            plane_rows: delivered.plane_rows,
            member_reception,
            member_entry_id: deliver.roster_entry_id,
            first_generation: delivered.generation_id,
            cfg: auth_cfg.clone(),
        }
    }

    /// One authority device's replica: the scope's machinery rows adopted, its
    /// held root in custody, and a fleet holding all three devices.
    struct Fx {
        _dir: tempfile::TempDir,
        store: AccountStore<SqliteBackend>,
        schedule: AccountStateKeySchedule,
        /// This replica's writer — the live device whose pass is under test.
        key: SigningKey,
        /// The account's retained record, shared with every sibling replica.
        record: AccountRecord,
        root: fauna_core::crypto::GroupMachineryRoot,
        ceremony: Ceremony,
        scope: String,
    }

    impl Fx {
        /// Device B's replica over a fresh ceremony.
        async fn open() -> Fx {
            let ceremony = run_ceremony();
            let record = AccountRecord::new(ceremony.cfg.clone());
            Fx::open_as(device_b(), ceremony, record).await
        }

        /// `writer`'s replica of the account that ran `ceremony`.
        async fn open_as(writer: SigningKey, ceremony: Ceremony, record: AccountRecord) -> Fx {
            let dir = tempfile::tempdir().expect("tempdir");
            let store = AccountStore::open(
                SqliteBackend::open(dir.path()).unwrap(),
                &authority().actor_id_hex(),
                WriterId(writer.verifying_key().to_bytes()),
            )
            .await
            .unwrap();
            let fx = Fx {
                _dir: dir,
                store,
                schedule: AccountStateKeySchedule::derive(&BackupKey::derive(&[0x07; 32])),
                key: writer,
                record,
                root: ceremony
                    .held_root_row
                    .machinery_root()
                    .expect("machinery root"),
                scope: GroupScope::new(ceremony.scope_id).to_string(),
                ceremony,
            };

            // Every device enrolled, through the production enrollment shape.
            for device in [device_a(), device_b(), device_c()] {
                let record = fauna_core::generation::sign_device_enrollment(
                    &device,
                    cert_for(&device),
                    5_000,
                );
                fx.store
                    .put_state(StateEntry {
                        kind: KIND_DEVICE_SET.into(),
                        key: fauna_core::hex32::encode(&device.verifying_key().to_bytes()),
                        scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
                        value: canonical_encode(&record).unwrap().to_vec(),
                        merge_meta: None,
                        entry_version: 0,
                        tombstone: false,
                    })
                    .await
                    .unwrap();
            }

            fx.install(&fx.ceremony).await;
            fx
        }

        /// Take one ceremony's scope into this replica: its machinery root
        /// into fleet-scope custody — which is what makes this device able to
        /// write a readable byte into the plane — and its machinery rows,
        /// adopted exactly as a replica converges on them, never hand-written.
        async fn install(&self, ceremony: &Ceremony) {
            self.store
                .put_state(StateEntry {
                    kind: KIND_GROUP_MACHINERY_ROOT.into(),
                    key: GroupHeldRootRecord::logical_key_for(&ceremony.scope_id),
                    scope: home_scope_for_kind(KIND_GROUP_MACHINERY_ROOT)
                        .unwrap()
                        .into(),
                    value: canonical_encode(&ceremony.held_root_row).unwrap().to_vec(),
                    merge_meta: None,
                    entry_version: 0,
                    tombstone: false,
                })
                .await
                .unwrap();
            let root = ceremony
                .held_root_row
                .machinery_root()
                .expect("machinery root");
            let report =
                GroupStatePlane::new(&self.store, &NoFeed, &root, &self.key, &ceremony.scope_id)
                    .expect("group plane")
                    .adopt_rows(&ceremony.plane_rows)
                    .await
                    .expect("adopt the ceremony rows");
            assert_eq!(
                report.refused, 0,
                "the ceremony snapshot adopts: {report:?}"
            );
        }

        fn plane(&self) -> GroupStatePlane<'_, SqliteBackend, NoFeed> {
            GroupStatePlane::new(
                &self.store,
                &NoFeed,
                &self.root,
                &self.key,
                &self.ceremony.scope_id,
            )
            .expect("group plane")
        }

        /// The devices page's removal, through its production writer.
        async fn remove_device(&self, device: &SigningKey) {
            let rpc = NoNest;
            let trust = trust();
            let fleet = AccountStatePlane::new_pull_only(
                &self.store,
                &rpc,
                &self.schedule,
                &self.key,
                &trust,
                ACCOUNT_STATE_FLEET_SCOPE,
            )
            .expect("fleet plane");
            crate::fleet_removal::write_removed(
                &self.store,
                &fleet,
                &trust,
                &self.key,
                device.verifying_key().to_bytes(),
            )
            .await
            .expect("remove the device from the fleet");
        }

        /// This replica merging its OWN fleet removal — a sibling's devices-page
        /// gesture, as the account-plane walk would pull it. Planted the way
        /// `open_as` plants the enrollments, because the production writer
        /// refuses the writer's own id; `Removed` excludes unconditionally at
        /// every reader, so the planted row reads exactly as a pulled one.
        async fn merge_own_removal(&self, removed_by: &SigningKey) {
            let removed = fauna_core::generation::DeviceSetRecord::Removed {
                removed_at_ms: at_ms(),
                removed_by: removed_by.verifying_key().to_bytes(),
            };
            self.store
                .put_state(StateEntry {
                    kind: KIND_DEVICE_SET.into(),
                    key: fauna_core::hex32::encode(&self.key.verifying_key().to_bytes()),
                    scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
                    value: canonical_encode(&removed).unwrap().to_vec(),
                    merge_meta: None,
                    entry_version: 0,
                    tombstone: false,
                })
                .await
                .unwrap();
        }

        /// Converge on a sibling replica's group-plane rows, exactly as the
        /// ceremony's own rows arrive — never hand-written here.
        async fn adopt_group_rows(&self, rows: &[StateEntry]) {
            let rows: Vec<GroupPlaneRow> = rows
                .iter()
                .map(|r| GroupPlaneRow {
                    kind: r.kind.clone(),
                    key: r.key.clone(),
                    value: r.value.clone(),
                })
                .collect();
            self.adopt_plane_rows(&rows).await;
        }

        async fn adopt_plane_rows(&self, rows: &[GroupPlaneRow]) {
            let report = self.plane().adopt_rows(rows).await.expect("adopt");
            assert_eq!(report.refused, 0, "the rows adopt: {report:?}");
        }

        async fn run_pass(&self) -> RevocationPass {
            severance_pass(
                &self.store,
                &trust(),
                &self.key,
                &cert_for(&self.key),
                &self.record,
            )
            .await
            .expect("severance pass")
        }

        async fn group_rows(&self) -> Vec<StateEntry> {
            self.store
                .group_scope_states(&self.scope)
                .await
                .unwrap()
                .into_iter()
                .filter(|r| !r.tombstone)
                .collect()
        }
    }

    /// A member-side reading of the merged rows: the authority line, the
    /// roster it verifies, and the tip the member can key — the same three
    /// calls every reader of this plane makes.
    fn member_view(
        rows: &[StateEntry],
        scope_id: &[u8; 32],
        reception: &GroupReceptionKeyRecord,
    ) -> (GroupAuthority, RosterView, Option<[u8; 32]>) {
        let authority = GroupAuthority::build(
            scope_id,
            &authority().actor_id(),
            &[],
            rows_of(rows, KIND_GROUP_AUTHORITY_REVOCATION),
        );
        let roster = RosterView::build(scope_id, &authority, rows_of(rows, KIND_GROUP_ROSTER));
        let secret = reception.keypair().expect("reception keypair").secret;
        let mine: Vec<[u8; 32]> = roster
            .wrap_targets()
            .filter(|m| m.member_actor == member().actor_id())
            .map(|m| m.entry_id)
            .collect();
        let resolution = resolve_admissible_group_tip(
            &roster,
            &authority,
            rows_of(rows, KIND_GROUP_GENERATION_MINT),
            rows_of(rows, KIND_GROUP_GENERATION_WRAP),
            |id, core, wraps| {
                wraps.iter().any(|w| {
                    mine.contains(&w.entry_id)
                        && open_group_generation_key_as_entry(
                            &w.wrap,
                            &secret,
                            id,
                            &w.entry_id,
                            &core.key_commitment,
                        )
                        .is_ok()
                })
            },
        );
        let tip = resolution.tip.map(|t| t.generation_id);
        (authority, roster, tip)
    }

    /// The ruling's publisher half, end to end: an authority device removed
    /// from its own fleet is revoked ON THE GROUP PLANE by a live sibling,
    /// everything it vouched for is re-published under the sibling's own cell
    /// at the SAME entry ids (no `Removed` — the revoked device's cells are
    /// dead on their own), the severance mint re-keys the group past the
    /// revoked device, and the member reads exactly one of itself under the
    /// sibling's mint — while a row the revoked device authors afterwards is
    /// refused at every reader.
    #[tokio::test]
    async fn a_removed_authority_device_is_revoked_readmitted_and_reminted() {
        let fx = Fx::open().await;
        let scope_id = fx.ceremony.scope_id;
        let a_id = device_a().verifying_key().to_bytes();

        // Before the removal there is nothing to sever: the pass sees its one
        // authority scope and writes not a byte.
        assert_eq!(
            fx.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                ..RevocationPass::default()
            },
            "a fleet with no removal owes no severance"
        );

        fx.remove_device(&device_a()).await;
        let pass = fx.run_pass().await;
        assert_eq!(
            pass,
            RevocationPass {
                authority_scopes: 1,
                revoked: 1,
                // Both ceremony entries — the member's and the authority's own
                // — were authored by device A.
                readmitted: 2,
                minted: 1,
                unsourced: 0,
            },
            "device B's pass revokes A, re-admits both members and re-mints"
        );

        let rows = fx.group_rows().await;
        let (authority, roster, tip) = member_view(&rows, &scope_id, &fx.ceremony.member_reception);

        assert!(
            authority.is_revoked(&a_id),
            "the member learns A's revocation from the plane alone"
        );
        assert!(
            authority.invalid().is_empty(),
            "every revocation row verifies: {:?}",
            authority.invalid()
        );
        assert!(
            !roster.is_excluded_entry(&fx.ceremony.member_entry_id),
            "no Removed marks a severance — the revoked device's cell is dead on its own"
        );
        assert!(
            rows.iter().all(|r| {
                r.kind != KIND_GROUP_ROSTER
                    || !matches!(
                        canonical_decode::<GroupRosterRecord>(&r.value),
                        Ok(GroupRosterRecord::Removed { .. })
                    )
            }),
            "the pass wrote no Removed at all"
        );
        assert!(
            rows.iter().any(|r| {
                r.kind == KIND_GROUP_ROSTER
                    && r.key
                        == fauna_core::group_scope::roster_cell_key(
                            &fx.ceremony.member_entry_id,
                            &device_b().verifying_key().to_bytes(),
                        )
            }),
            "the re-admission sits in B's own cell at the member's entry id"
        );
        let mine: Vec<_> = roster
            .wrap_targets()
            .filter(|m| m.member_actor == member().actor_id())
            .collect();
        assert_eq!(mine.len(), 1, "the member is listed exactly once: {mine:?}");
        assert_eq!(
            mine[0].entry_id, fx.ceremony.member_entry_id,
            "under the SAME entry id, in the live device's own cell"
        );
        assert_eq!(
            mine[0].reception_pubkey,
            fx.ceremony
                .member_reception
                .keypair()
                .unwrap()
                .public
                .to_bytes()
                .to_vec(),
            "carrying the reception key the retained deliver attests to"
        );

        let tip = tip.expect("the member resolves a keyable tip");
        assert_ne!(
            tip, fx.ceremony.first_generation,
            "the severance mint supersedes device A's"
        );
        let minted = rows
            .iter()
            .find(|r| r.kind == KIND_GROUP_GENERATION_MINT && r.key == hex(&tip))
            .expect("the tip's row");
        let record: GroupGenerationMintRecord = canonical_decode(&minted.value).unwrap();
        let GroupGenerationMintRecord::Minted { core, .. } = &record else {
            panic!("the tip is a Minted row");
        };
        assert_eq!(
            core.minter,
            device_b().verifying_key().to_bytes(),
            "and device B minted it"
        );
        assert!(
            core.parents.contains(&fx.ceremony.first_generation),
            "naming the DAG's leaf as its parent: {:?}",
            core.parents
        );
        assert_eq!(
            core.revoked_past,
            vec![a_id],
            "minted PAST the line as the pass left it — the line-committed arm"
        );

        // The whole point: A can still sign, and nothing it signs counts.
        let (_, forged) = sign_roster_enrollment(
            &device_a(),
            RosterEntryCore {
                scope_id,
                member_actor: ActorKeypair::from_secret([0x99; 32]).actor_id(),
                admission_salt: [0x7E; 32],
            },
            vec![0xAA; 32],
            cert_for(&device_a()),
            at_ms(),
        )
        .expect("A can still build a well-formed entry");
        assert!(
            verify_enrolled_entry(&forged, &scope_id, &authority).is_err(),
            "a roster entry A authors after its revocation enrols nobody"
        );
        let a_mint = build_group_mint(
            &roster.wrap_targets().cloned().collect::<Vec<_>>(),
            vec![tip],
            vec![a_id], // a revoked device may list itself — authorship refuses it
            &device_a(),
            cert_for(&device_a()),
            at_ms() + 1,
        )
        .expect("A can still build a well-formed mint");
        let with_a_mint: Vec<StateEntry> = rows
            .iter()
            .cloned()
            .chain(std::iter::once(StateEntry {
                kind: KIND_GROUP_GENERATION_MINT.into(),
                key: hex(&a_mint.generation_id),
                scope: fx.scope.clone(),
                value: canonical_encode(&a_mint.record).unwrap().to_vec(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            }))
            .collect();
        let (_, _, after) = member_view(&with_a_mint, &scope_id, &fx.ceremony.member_reception);
        assert_eq!(
            after,
            Some(tip),
            "and a mint A authors after its revocation never becomes the tip"
        );

        // Idempotent over its own result: the revocation is published, every
        // entry stands under B's own cell, the tip is minted past A, so a
        // second pass owes nothing.
        assert_eq!(
            fx.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                ..RevocationPass::default()
            },
            "the pass is idempotent over its own result"
        );
    }

    /// Device B's severance of A, then device C's replica converged on the
    /// result with B removed from the fleet in turn — the state every second
    /// authority-device removal starts from. `before_c` is what else reached
    /// C's replica first (a revoked device's forgeries), given the scope id.
    async fn second_removal(before_c: impl Fn(&[u8; 32]) -> Vec<GroupPlaneRow>) -> (Fx, Fx) {
        let b = Fx::open().await;
        b.remove_device(&device_a()).await;
        let first = b.run_pass().await;
        assert_eq!((first.readmitted, first.unsourced), (2, 0), "{first:?}");

        let c = Fx::open_as(device_c(), b.ceremony.clone(), b.record.clone()).await;
        c.adopt_group_rows(&b.group_rows().await).await;
        c.adopt_plane_rows(&before_c(&c.ceremony.scope_id)).await;
        c.remove_device(&device_a()).await;
        c.remove_device(&device_b()).await;
        (b, c)
    }

    /// The ruling promises re-admission at EVERY authority-device removal, not
    /// the first one. Remove A and let B sever it; then
    /// remove B. Every standing entry now sits in B's cell, which C's line
    /// refuses — and C's pass re-publishes each one under ITS own cell, at the
    /// same entry id, sourced from the ceremony snapshot alone (no retained
    /// record of written cells exists any more, and none is needed).
    #[tokio::test]
    async fn a_second_authority_device_removal_readmits_again() {
        let (b, c) = second_removal(|_| Vec::new()).await;
        let scope_id = c.ceremony.scope_id;
        let b_rows = b.group_rows().await;
        let (_, after_first, _) = member_view(&b_rows, &scope_id, &c.ceremony.member_reception);
        let first_entry = after_first
            .wrap_targets()
            .find(|m| m.member_actor == member().actor_id())
            .expect("B's severance listed the member")
            .entry_id;
        assert_eq!(
            first_entry, c.ceremony.member_entry_id,
            "B kept the entry id"
        );

        assert_eq!(
            c.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                // A's revocation arrived with B's rows; only B's is owed.
                revoked: 1,
                readmitted: 2,
                minted: 1,
                unsourced: 0,
            },
            "device C's pass revokes B, re-admits both members again and re-mints"
        );

        let rows = c.group_rows().await;
        let (authority, roster, tip) = member_view(&rows, &scope_id, &c.ceremony.member_reception);
        assert!(authority.is_revoked(&device_a().verifying_key().to_bytes()));
        assert!(authority.is_revoked(&device_b().verifying_key().to_bytes()));
        assert!(
            !roster.is_excluded_entry(&first_entry),
            "no Removed — B's cell is dead on its own at every reader with the line"
        );
        assert!(
            rows.iter().any(|r| {
                r.kind == KIND_GROUP_ROSTER
                    && r.key
                        == fauna_core::group_scope::roster_cell_key(
                            &first_entry,
                            &device_c().verifying_key().to_bytes(),
                        )
            }),
            "C's re-admission sits in C's own cell"
        );
        let mine: Vec<_> = roster
            .wrap_targets()
            .filter(|m| m.member_actor == member().actor_id())
            .collect();
        assert_eq!(mine.len(), 1, "the member is listed exactly once: {mine:?}");
        assert_eq!(mine[0].entry_id, first_entry, "at the SAME entry id again");
        assert_eq!(
            mine[0].reception_pubkey,
            c.ceremony
                .member_reception
                .keypair()
                .unwrap()
                .public
                .to_bytes()
                .to_vec(),
            "at the SAME attested key the first severance used — the member-signed \
             ceremony key, never one read off a revoked device's row"
        );

        let tip = tip.expect("the member resolves a keyable tip");
        let minted = rows
            .iter()
            .find(|r| r.kind == KIND_GROUP_GENERATION_MINT && r.key == hex(&tip))
            .expect("the tip's row");
        let record: GroupGenerationMintRecord = canonical_decode(&minted.value).unwrap();
        let GroupGenerationMintRecord::Minted { core, .. } = &record else {
            panic!("the tip is a Minted row");
        };
        assert_eq!(
            core.minter,
            device_c().verifying_key().to_bytes(),
            "and device C minted it"
        );
        let mut past = vec![
            device_a().verifying_key().to_bytes(),
            device_b().verifying_key().to_bytes(),
        ];
        past.sort_unstable();
        assert_eq!(core.revoked_past, past, "minted past BOTH revoked devices");

        assert_eq!(
            c.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                ..RevocationPass::default()
            },
            "the pass is idempotent over its own result"
        );
    }

    /// The severance mint is THIS scope's trigger (1), read per scope — and
    /// under line-committed admissibility a scope the removed device authored
    /// NOTHING in still owes one: its line now revokes
    /// A, its tip was minted past nothing, so the tip is inadmissible at every
    /// reader with the line and A — who held the machinery root — is keyed
    /// out by a mint past it. (Under the shared cell, where only a `Removed`
    /// merge minted, this scope minted nothing.) Each scope's mint is its
    /// own: one per scope, each past A, none re-admitting in the scope with
    /// no stale entry. Driven through `sever_scope` in a fixed order, A's
    /// scope first, because that is the order a pass-wide counter once got
    /// wrong.
    #[tokio::test]
    async fn a_scope_the_removed_device_authored_nothing_in_still_mints_past_it() {
        let mut cfg = GroupShareConfig::default();
        let by_a = run_ceremony_on(&device_a(), &mut cfg);
        let by_b = run_ceremony_on(&device_b(), &mut cfg);
        let record = AccountRecord::new(cfg.clone());
        let fx = Fx::open_as(device_b(), by_a.clone(), record).await;
        fx.install(&by_b).await;
        fx.remove_device(&device_a()).await;

        let carriage = cert_for(&fx.key);
        let root = trust().root;
        let me = Severer {
            writer_key: &fx.key,
            carriage: &carriage,
            actor: &root,
            now_ms: at_ms(),
        };
        let removed = BTreeSet::from([device_a().verifying_key().to_bytes()]);
        let mut work: Vec<SeveranceScope> = held_authority_scopes(&fx.store, &root)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|held| severance_work(held, &me, &[], &removed))
            .collect();
        assert_eq!(work.len(), 2, "both scopes owe A's revocation");
        // The scope A authored — the one with stale cells — first.
        work.sort_by_key(|w| w.held.scope_id != by_a.scope_id);
        assert_eq!((work[0].stale.len(), work[1].stale.len()), (2, 0));

        let mut pass = RevocationPass::default();
        for scope in &work {
            sever_scope(&fx.store, &me, &cfg, scope, &mut pass)
                .await
                .expect("sever");
        }
        assert_eq!(
            pass,
            RevocationPass {
                authority_scopes: 0,
                revoked: 2,
                readmitted: 2,
                minted: 2,
                unsourced: 0,
            },
            "one mint per scope the line moved under; re-admissions only where A authored"
        );
        for scope in [&by_a, &by_b] {
            let rows: Vec<StateEntry> = fx
                .store
                .group_scope_states(&GroupScope::new(scope.scope_id).to_string())
                .await
                .unwrap()
                .into_iter()
                .filter(|r| !r.tombstone)
                .collect();
            let (_, _, tip) = member_view(&rows, &scope.scope_id, &scope.member_reception);
            let tip = tip.expect("each scope resolves a keyable tip past A");
            let minted = rows
                .iter()
                .find(|r| r.kind == KIND_GROUP_GENERATION_MINT && r.key == hex(&tip))
                .unwrap();
            let GroupGenerationMintRecord::Minted { core, .. } =
                canonical_decode(&minted.value).unwrap()
            else {
                panic!("a Minted row");
            };
            assert_eq!(
                core.revoked_past,
                vec![device_a().verifying_key().to_bytes()]
            );
        }
    }

    /// Was this mint row authored by `device`?
    fn minted_by(row: &StateEntry, device: &SigningKey) -> bool {
        row.kind == KIND_GROUP_GENERATION_MINT
            && matches!(
                canonical_decode::<GroupGenerationMintRecord>(&row.value),
                Ok(GroupGenerationMintRecord::Minted { core, .. })
                    if core.minter == device.verifying_key().to_bytes()
            )
    }

    /// `fx`'s device again, as it comes back after a pass that did not finish:
    /// a fresh replica of the same writer holding every group row `fx` wrote
    /// except those `lost` names, with the same fleet removals.
    async fn resumed_without(
        fx: &Fx,
        removed: &[SigningKey],
        lost: impl Fn(&StateEntry) -> bool,
    ) -> Fx {
        let again = Fx::open_as(fx.key.clone(), fx.ceremony.clone(), fx.record.clone()).await;
        let kept: Vec<StateEntry> = fx
            .group_rows()
            .await
            .into_iter()
            .filter(|r| !lost(r))
            .collect();
        again.adopt_group_rows(&kept).await;
        for device in removed {
            again.remove_device(device).await;
        }
        again
    }

    /// The member's keyable tip over `fx`'s merged rows, and who minted it.
    async fn member_tip_minter(fx: &Fx) -> Option<[u8; 32]> {
        let rows = fx.group_rows().await;
        let (_, _, tip) = member_view(&rows, &fx.ceremony.scope_id, &fx.ceremony.member_reception);
        let tip = tip?;
        let minted = rows
            .iter()
            .find(|r| r.kind == KIND_GROUP_GENERATION_MINT && r.key == hex(&tip))?;
        match canonical_decode::<GroupGenerationMintRecord>(&minted.value).ok()? {
            GroupGenerationMintRecord::Minted { core, .. } => Some(core.minter),
            _ => None,
        }
    }

    /// Trigger (1) is a fact about MERGED STATE, never about what this pass
    /// happened to write. A device that crashed after the last `Removed` and
    /// before the mint comes back to stale cells that are already `Removed`:
    /// nothing is left to sever, and the mint is still owed — or the
    /// revocation it publishes next strands every member with no tip, and the
    /// compromised device is never keyed out.
    #[tokio::test]
    async fn a_pass_that_died_before_its_mint_still_owes_it() {
        let b = Fx::open().await;
        b.remove_device(&device_a()).await;
        b.run_pass().await;

        let resumed = resumed_without(&b, &[device_a()], |r| {
            minted_by(r, &device_b()) || r.kind == KIND_GROUP_AUTHORITY_REVOCATION
        })
        .await;
        assert_eq!(
            resumed.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                revoked: 1,
                readmitted: 0,
                minted: 1,
                unsourced: 0,
            },
            "nothing left to re-admit, the mint and the revocation still owed"
        );
        assert_eq!(
            member_tip_minter(&resumed).await,
            Some(device_b().verifying_key().to_bytes()),
            "the member resolves a keyable tip, minted by the live device"
        );
        assert_eq!(
            resumed.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                ..RevocationPass::default()
            },
            "and a healed scope never mints again"
        );
    }

    /// The same debt when the mint merely ERRORED: the pass logs it and still
    /// publishes the revocations (they are the security-critical write, and
    /// the no-tip gap is the fail-safe direction). So the next pass finds no
    /// revocation owed and no stale cell — the scope must be entered on the
    /// owed mint alone.
    #[tokio::test]
    async fn a_mint_that_failed_is_owed_by_the_next_pass() {
        let b = Fx::open().await;
        b.remove_device(&device_a()).await;
        b.run_pass().await;

        let resumed = resumed_without(&b, &[device_a()], |r| minted_by(r, &device_b())).await;
        assert_eq!(member_tip_minter(&resumed).await, None, "the gap");
        assert_eq!(
            resumed.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                minted: 1,
                ..RevocationPass::default()
            },
            "the mint alone is owed"
        );
        assert_eq!(
            member_tip_minter(&resumed).await,
            Some(device_b().verifying_key().to_bytes())
        );
        assert_eq!(
            resumed.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                ..RevocationPass::default()
            },
            "exactly once"
        );
    }

    /// And over RECORDED cells: the second severance's mint is owed the same
    /// way when device C's pass dies before it.
    #[tokio::test]
    async fn a_second_severance_that_died_before_its_mint_still_owes_it() {
        let (_b, c) = second_removal(|_| Vec::new()).await;
        c.run_pass().await;

        let resumed =
            resumed_without(&c, &[device_a(), device_b()], |r| minted_by(r, &device_c())).await;
        assert_eq!(
            resumed.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                minted: 1,
                ..RevocationPass::default()
            }
        );
        assert_eq!(
            member_tip_minter(&resumed).await,
            Some(device_c().verifying_key().to_bytes())
        );
        assert_eq!(
            resumed.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                ..RevocationPass::default()
            },
            "exactly once"
        );
    }

    /// An account record no request reaches — what a removed device's nest
    /// connection is: removal tombstones the writer key its bearer is signed
    /// with before the fleet `Removed` exists.
    struct NestGone;

    impl RetainedRecord for NestGone {
        async fn load(&self) -> Result<GroupShareConfig> {
            bail!("the nest refuses a removed device's bearer")
        }
    }

    /// A device the authority line refuses writes its revocations and nothing
    /// else. Device B severed A, so every standing entry is
    /// under B's carriage; then B merges its OWN fleet removal. Rule (2) still
    /// owes B's revocation of itself — but a re-admission under B's carriage
    /// can never verify, so it would be stale again on the next pass and
    /// re-admitted again, for ever, each one a fresh record in the account's
    /// synced config; and a mint B authors is inadmissible by construction.
    /// The revocation goes out with no nest to ask, because a removed device
    /// has none.
    #[tokio::test]
    async fn a_device_that_merged_its_own_removal_publishes_only_its_revocations() {
        let b = Fx::open().await;
        b.remove_device(&device_a()).await;
        b.run_pass().await;
        let rows_before = b.group_rows().await.len();

        b.merge_own_removal(&device_c()).await;
        let carriage = cert_for(&b.key);
        let first = severance_pass(&b.store, &trust(), &b.key, &carriage, &NestGone)
            .await
            .expect("a revocations-only pass asks the nest for nothing");
        assert_eq!(
            first,
            RevocationPass {
                authority_scopes: 1,
                revoked: 1,
                ..RevocationPass::default()
            },
            "B revokes itself — rule (2) — and re-admits and mints nothing"
        );
        for _ in 0..3 {
            assert_eq!(
                b.run_pass().await,
                RevocationPass {
                    authority_scopes: 1,
                    ..RevocationPass::default()
                },
                "and every later pass owes nothing"
            );
        }

        let rows = b.group_rows().await;
        assert_eq!(
            rows.len(),
            rows_before + 1,
            "the plane gained B's revocation of itself and not one row more"
        );
        assert_eq!(
            rows.iter().filter(|r| minted_by(r, &device_b())).count(),
            1,
            "B's one mint is the one it wrote while it stood"
        );
        let (authority, _, _) =
            member_view(&rows, &b.ceremony.scope_id, &b.ceremony.member_reception);
        assert!(authority.is_revoked(&device_b().verifying_key().to_bytes()));
    }

    /// The same guard when the refusal arrives on the GROUP line instead: C
    /// severed B, and B's replica converges on C's rows with C's mint missing
    /// (the errored-mint shape above). The scope owes a mint — but not from B,
    /// whose mint every reader refuses and which would therefore stay owed,
    /// and be minted again, on every pass until C's arrives.
    #[tokio::test]
    async fn a_device_the_group_line_revokes_never_mints() {
        let (b, c) = second_removal(|_| Vec::new()).await;
        c.run_pass().await;
        let from_c: Vec<StateEntry> = c
            .group_rows()
            .await
            .into_iter()
            .filter(|r| !minted_by(r, &device_c()))
            .collect();
        b.adopt_group_rows(&from_c).await;

        for _ in 0..4 {
            assert_eq!(
                b.run_pass().await,
                RevocationPass {
                    authority_scopes: 1,
                    ..RevocationPass::default()
                },
                "a revoked device pays no mint debt"
            );
        }
        assert_eq!(
            b.group_rows()
                .await
                .iter()
                .filter(|r| minted_by(r, &device_b()))
                .count(),
            1,
            "B's one mint is the one it wrote while it stood"
        );
    }

    /// "Adds never mint" holds only of a scope that never severed. The line's revoked set only grows, so a once-severed scope is
    /// resolved at every pass after, and an add whose entry reaches a replica ahead of
    /// its top-up reads as a coverage failure — a debt. The owner rules it
    /// harmless and bounded: at most one mint per authority device that sees
    /// the gap, per add, and each wraps the new member. B severed A; C
    /// converged; C admits a new member, and both replicas see the entry
    /// before any top-up. Each mints once, and once they converge neither
    /// mints again.
    #[tokio::test]
    async fn an_add_in_a_once_severed_scope_costs_at_most_one_mint_per_device() {
        let b = Fx::open().await;
        b.remove_device(&device_a()).await;
        assert_eq!(b.run_pass().await.minted, 1, "B's severance of A");
        let c = Fx::open_as(device_c(), b.ceremony.clone(), b.record.clone()).await;
        c.adopt_group_rows(&b.group_rows().await).await;
        c.remove_device(&device_a()).await;
        assert_eq!(
            c.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                ..RevocationPass::default()
            },
            "C converged on a healed scope"
        );

        let scope_id = c.ceremony.scope_id;
        let newcomer = ActorKeypair::from_secret([0x77; 32]).actor_id();
        let newcomer_reception = GroupReceptionKeyRecord::mint(at_ms());
        let (entry_id, record) = sign_roster_enrollment(
            &device_c(),
            RosterEntryCore {
                scope_id,
                member_actor: newcomer,
                admission_salt: [0x7A; 32],
            },
            newcomer_reception
                .keypair()
                .unwrap()
                .public
                .to_bytes()
                .to_vec(),
            cert_for(&device_c()),
            at_ms(),
        )
        .expect("C admits a member");
        let add = GroupPlaneRow {
            kind: KIND_GROUP_ROSTER.into(),
            key: roster_cell_key(&entry_id, &device_c().verifying_key().to_bytes()),
            value: canonical_encode(&record).unwrap().to_vec(),
        };
        b.adopt_plane_rows(std::slice::from_ref(&add)).await;
        c.adopt_plane_rows(std::slice::from_ref(&add)).await;

        let one_mint = RevocationPass {
            authority_scopes: 1,
            minted: 1,
            ..RevocationPass::default()
        };
        assert_eq!(b.run_pass().await, one_mint, "B sees the gap: one mint");
        assert_eq!(c.run_pass().await, one_mint, "C sees it too: one mint");
        let (from_b, from_c) = (b.group_rows().await, c.group_rows().await);
        b.adopt_group_rows(&from_c).await;
        c.adopt_group_rows(&from_b).await;
        for fx in [&b, &c] {
            for _ in 0..2 {
                assert_eq!(
                    fx.run_pass().await,
                    RevocationPass {
                        authority_scopes: 1,
                        ..RevocationPass::default()
                    },
                    "converged, the add costs nothing more"
                );
            }
        }

        let rows = c.group_rows().await;
        let (authority, roster, tip) = member_view(&rows, &scope_id, &c.ceremony.member_reception);
        assert!(tip.is_some(), "the original member still keys a tip");
        assert!(
            roster
                .wrap_targets()
                .any(|m| m.member_actor == newcomer && m.entry_id == entry_id),
            "the newcomer is a member"
        );
        let post_add: Vec<GroupGenerationMintRecord> = rows
            .iter()
            .filter(|r| minted_by(r, &device_b()) || minted_by(r, &device_c()))
            .map(|r| canonical_decode(&r.value).unwrap())
            .filter(|m| {
                matches!(m, GroupGenerationMintRecord::Minted { core, .. }
                    if core.member_entries.contains(&entry_id))
            })
            .collect();
        assert_eq!(
            post_add.len(),
            2,
            "one mint per device that saw the gap, each listing the newcomer"
        );
        assert!(!authority.is_revoked(&device_c().verifying_key().to_bytes()));
    }

    /// The source is never the plane. Device B is stolen and then removed; it
    /// keeps its key, its cert and the machinery root, so before C's pass it
    /// writes two self-consistent bound entries in its own (not yet revoked)
    /// cell: the real member's actor bound to a reception key the thief
    /// holds, and an actor the authority never admitted — both at entry ids
    /// the ceremony snapshot never named. Both are stale the moment B is
    /// revoked — and both must be left alone: C re-admits exactly what the
    /// snapshot attests, at the member-signed key, and neither forgery buys a
    /// re-admission or a wrap.
    #[tokio::test]
    async fn a_revoked_devices_own_rows_are_never_a_readmission_source() {
        let thief_key = vec![0xEE; 32];
        let outsider = ActorKeypair::from_secret([0x99; 32]).actor_id();
        let forge = |scope_id: &[u8; 32], member_actor: ActorId, salt: u8| {
            let (entry_id, record) = sign_roster_enrollment(
                &device_b(),
                RosterEntryCore {
                    scope_id: *scope_id,
                    member_actor,
                    admission_salt: [salt; 32],
                },
                vec![0xEE; 32],
                cert_for(&device_b()),
                at_ms(),
            )
            .expect("B can still build a well-formed bound entry");
            GroupPlaneRow {
                kind: KIND_GROUP_ROSTER.into(),
                key: roster_cell_key(&entry_id, &device_b().verifying_key().to_bytes()),
                value: canonical_encode(&record).unwrap().to_vec(),
            }
        };
        let (_b, c) = second_removal(|scope_id| {
            vec![
                forge(scope_id, member().actor_id(), 0x5A),
                forge(scope_id, outsider, 0x5B),
            ]
        })
        .await;
        let scope_id = c.ceremony.scope_id;

        let pass = c.run_pass().await;
        assert_eq!(
            pass,
            RevocationPass {
                authority_scopes: 1,
                revoked: 1,
                readmitted: 2,
                minted: 1,
                // The two forgeries: the ceremony snapshot names no such
                // entries.
                unsourced: 2,
            },
            "only what the snapshot attests is re-admitted"
        );

        let rows = c.group_rows().await;
        let (_, roster, tip) = member_view(&rows, &scope_id, &c.ceremony.member_reception);
        let targets: Vec<_> = roster.wrap_targets().cloned().collect();
        assert!(
            targets.iter().all(|m| m.reception_pubkey != thief_key),
            "no wrap target carries the thief's key: {targets:?}"
        );
        assert!(
            targets.iter().all(|m| m.member_actor != outsider),
            "an actor the authority never admitted is not a member"
        );
        assert_eq!(
            targets
                .iter()
                .filter(|m| m.member_actor == member().actor_id())
                .count(),
            1,
            "the member is listed exactly once: {targets:?}"
        );
        assert!(tip.is_some(), "and still resolves a keyable tip");
    }

    /// "Adds never mint" in a scope that never severed: `owed_mint`'s `Removed` conjunct is trigger (1)
    /// itself. A scope with a verified member and NO admissible mint — the
    /// shape an add whose top-up has not yet arrived leaves — owes this pass
    /// nothing while no removal ever merged there.
    #[tokio::test]
    async fn a_never_severed_scope_with_no_admissible_mint_is_not_minted() {
        let mut ceremony = run_ceremony();
        ceremony.plane_rows.retain(|r| {
            r.kind != KIND_GROUP_GENERATION_MINT && r.kind != KIND_GROUP_GENERATION_WRAP
        });
        let record = AccountRecord::new(ceremony.cfg.clone());
        let fx = Fx::open_as(device_b(), ceremony, record).await;
        let rows = fx.group_rows().await;
        let (_, roster, tip) =
            member_view(&rows, &fx.ceremony.scope_id, &fx.ceremony.member_reception);
        assert!(
            roster.wrap_targets().next().is_some(),
            "precondition: members stand"
        );
        assert_eq!(tip, None, "precondition: no mint resolves");
        for _ in 0..2 {
            assert_eq!(
                fx.run_pass().await,
                RevocationPass {
                    authority_scopes: 1,
                    ..RevocationPass::default()
                },
                "no removal ever merged, so no mint is owed"
            );
        }
        assert_eq!(
            fx.group_rows().await.len(),
            rows.len(),
            "not a byte written"
        );
    }

    /// The merged row at `cell` on `fx`'s replica, decoded.
    async fn merged_enrolled(fx: &Fx, cell: &str) -> GroupRosterRecord {
        let row = fx
            .group_rows()
            .await
            .into_iter()
            .find(|r| r.kind == KIND_GROUP_ROSTER && r.key == cell)
            .expect("the cell is on the plane");
        canonical_decode(&row.value).expect("a roster record")
    }

    /// **The re-squat, made unrepresentable — the
    /// inverse of the shared cell's
    /// `a_revoked_device_rebinding_its_recorded_cell_never_moves_the_key`,
    /// whose precondition WAS the residual.** B severed A; C severed B, so the
    /// member stands under C's cell. Revoked B keeps its key and re-signs the
    /// member's entry — same core, so the same entry id — bound to a key the
    /// thief holds, at a stamp that would have outranked the genuine row under
    /// the shared cell. Now: filed into C's cell it is refused at first
    /// contact (not C's row); filed into B's own cell it lands in a cell every
    /// reader with the line refuses. The genuine row stands, at the attested
    /// key, and C's next pass re-admits nothing, severs nothing, mints
    /// nothing — no round is paid.
    #[tokio::test]
    async fn a_revoked_device_rebinding_its_recorded_cell_displaces_nothing() {
        let thief_key = vec![0xFF; 32];
        let (_b, c) = second_removal(|_| Vec::new()).await;
        c.run_pass().await;
        let entry_id = c.ceremony.member_entry_id;
        let core = {
            let cell = roster_cell_key(&entry_id, &device_c().verifying_key().to_bytes());
            let GroupRosterRecord::Enrolled { core, .. } = merged_enrolled(&c, &cell).await else {
                panic!("C's re-admission is Enrolled");
            };
            core
        };
        let rows_before = c.group_rows().await;
        let genuine_cell = roster_cell_key(&entry_id, &device_c().verifying_key().to_bytes());
        let genuine = rows_before
            .iter()
            .find(|r| r.kind == KIND_GROUP_ROSTER && r.key == genuine_cell)
            .expect("C's re-admission is on the plane")
            .value
            .clone();

        // B's own re-admission row, standing in B's (dead) cell — the row the
        // thief's must outrank there for (ii) to test anything.
        let dead_cell = roster_cell_key(&entry_id, &device_b().verifying_key().to_bytes());
        let b_standing = rows_before
            .iter()
            .find(|r| r.kind == KIND_GROUP_ROSTER && r.key == dead_cell)
            .expect("B's re-admission stands in B's cell")
            .value
            .clone();

        // The thief re-signs at stamps until its bytes outrank the genuine
        // row — the shared cell's winning move — and B's own standing row, so
        // (ii) lands it whatever the fixture's randomly minted reception keys
        // encode to (the byte tie-break is `join_group_roster`'s).
        //
        // The search has NO small provable bound, so it must not carry a small
        // one: the canonical encoding puts `binding_sig` (a pseudo-random
        // Ed25519 signature) BEFORE `enrolled_at_ms`, so the stamp moves the
        // comparison only through the signature hash, and a standing row whose
        // signature starts near 0xFF.. leaves each stamp a ~1/256 (or worse)
        // chance — 64 stamps missed in ~3% of draws. A window of 2^20 stamps
        // misses with probability ~2/2^20 over the draw, and costs only the
        // draws that need it.
        let thief_cert = cert_for(&device_b());
        let rebound = (0..1i64 << 20)
            .map(|k| {
                let (_, record) = sign_roster_enrollment(
                    &device_b(),
                    core.clone(),
                    thief_key.clone(),
                    thief_cert.clone(),
                    at_ms() + k,
                )
                .expect("B can still bind the entry");
                canonical_encode(&record).unwrap().to_vec()
            })
            .find(|forged| *forged > genuine && *forged > b_standing)
            .expect("a stamp in 2^20 outranks both standing rows by bytes");

        // (i) Into C's standing cell: the key-aware join keeps C's row (the
        // rebound row does not verify at C's cell, whatever its bytes).
        let report = c
            .plane()
            .adopt_rows(&[GroupPlaneRow {
                kind: KIND_GROUP_ROSTER.into(),
                key: genuine_cell.clone(),
                value: rebound.clone(),
            }])
            .await
            .expect("adopt");
        assert_eq!(
            (report.kept, report.adopted, report.merged),
            (1, 0, 0),
            "a row not at its own author's cell displaces nothing: {report:?}"
        );
        // …and at a replica where C's cell does not stand yet, it is refused
        // at first contact rather than adopted-then-outranked.
        let fresh = Fx::open_as(device_c(), c.ceremony.clone(), c.record.clone()).await;
        let report = fresh
            .plane()
            .adopt_rows(&[GroupPlaneRow {
                kind: KIND_GROUP_ROSTER.into(),
                key: genuine_cell.clone(),
                value: rebound.clone(),
            }])
            .await
            .expect("adopt");
        assert_eq!(report.refused, 1, "first contact refuses it: {report:?}");
        let GroupRosterRecord::Enrolled {
            reception_pubkey, ..
        } = merged_enrolled(&c, &genuine_cell).await
        else {
            panic!("C's cell is still Enrolled");
        };
        assert_ne!(reception_pubkey, thief_key, "C's row is untouched");

        // (ii) Into B's own cell: adopted — into a cell the line refuses.
        c.adopt_plane_rows(&[GroupPlaneRow {
            kind: KIND_GROUP_ROSTER.into(),
            key: dead_cell.clone(),
            value: rebound,
        }])
        .await;
        let GroupRosterRecord::Enrolled {
            reception_pubkey, ..
        } = merged_enrolled(&c, &dead_cell).await
        else {
            panic!("B's cell is Enrolled");
        };
        assert_eq!(
            reception_pubkey, thief_key,
            "the thief's row sits in B's dead cell"
        );

        // Nothing moved: no re-admission, no removal, no mint — no round.
        for _ in 0..3 {
            assert_eq!(
                c.run_pass().await,
                RevocationPass {
                    authority_scopes: 1,
                    ..RevocationPass::default()
                },
                "the genuine row stands, nothing is re-admitted"
            );
        }
        let rows = c.group_rows().await;
        assert_eq!(
            rows.len(),
            rows_before.len(),
            "not a row added: the re-bind replaced B's own earlier row inside B's dead cell"
        );
        let (_, roster, tip) =
            member_view(&rows, &c.ceremony.scope_id, &c.ceremony.member_reception);
        let targets: Vec<_> = roster.wrap_targets().cloned().collect();
        assert!(
            targets.iter().all(|m| m.reception_pubkey != thief_key),
            "no wrap target carries the thief's key: {targets:?}"
        );
        let mine: Vec<_> = targets
            .iter()
            .filter(|m| m.member_actor == member().actor_id())
            .collect();
        assert_eq!(mine.len(), 1, "the member is listed exactly once: {mine:?}");
        assert_eq!(mine[0].entry_id, entry_id);
        assert_eq!(
            mine[0].reception_pubkey,
            c.ceremony
                .member_reception
                .keypair()
                .unwrap()
                .public
                .to_bytes()
                .to_vec(),
            "at the member-signed key the deliver snapshot attests"
        );
        assert!(tip.is_some(), "and the member resolves a keyable tip");
        assert!(
            roster
                .invalid()
                .iter()
                .any(|(key, why)| *key == dead_cell && why.contains("revoked authority device")),
            "the dead cell is counted, never a member: {:?}",
            roster.invalid()
        );
    }

    /// The cross-account arm: every member
    /// the group ever admitted holds the machinery root, so a DIFFERENT
    /// account can write roster rows too — but it holds no device key of this
    /// authority, so it can make no row that self-verifies. Its two best
    /// shapes — (i) the member's entry re-filed beside its own reception key
    /// with the carried `authorization`/`authority_sig` verbatim and a binding
    /// it cannot make, into the genuine author's standing cell; (ii) a fresh
    /// entry for the member carrying revoked B's public carriage, into B's
    /// cell — lose at the key-aware join and are refused at first contact
    /// respectively, so neither changes the plane, and C's severance of B
    /// sources exactly the snapshot's two entries.
    #[tokio::test]
    async fn a_cross_account_forgery_sources_nothing() {
        let forger_key = vec![0xFF; 32];
        let b = Fx::open().await;
        b.remove_device(&device_a()).await;
        b.run_pass().await;
        let entry_id = b.ceremony.member_entry_id;
        let cell = roster_cell_key(&entry_id, &device_b().verifying_key().to_bytes());

        let c = Fx::open_as(device_c(), b.ceremony.clone(), b.record.clone()).await;
        c.adopt_group_rows(&b.group_rows().await).await;
        let GroupRosterRecord::Enrolled {
            core,
            authorization,
            authority_sig,
            enrolled_at_ms,
            ..
        } = merged_enrolled(&c, &cell).await
        else {
            panic!("B's re-admission is Enrolled");
        };
        let refiled = GroupRosterRecord::Enrolled {
            core: core.clone(),
            reception_pubkey: forger_key.clone(),
            authorization: authorization.clone(),
            authority_sig: authority_sig.clone(),
            enrolled_at_ms,
            binding_sig: vec![0xFF; 64],
        };
        let fresh_core = RosterEntryCore {
            admission_salt: [0x6A; 32],
            ..core
        };
        let fresh_cell = roster_cell_key(
            &roster_entry_id(&fresh_core).unwrap(),
            &device_b().verifying_key().to_bytes(),
        );
        let squat = GroupRosterRecord::Enrolled {
            core: fresh_core,
            reception_pubkey: forger_key.clone(),
            authorization,
            authority_sig,
            enrolled_at_ms,
            binding_sig: vec![0xFF; 64],
        };
        let report = c
            .plane()
            .adopt_rows(&[
                GroupPlaneRow {
                    kind: KIND_GROUP_ROSTER.into(),
                    key: cell.clone(),
                    value: canonical_encode(&refiled).unwrap().to_vec(),
                },
                GroupPlaneRow {
                    kind: KIND_GROUP_ROSTER.into(),
                    key: fresh_cell.clone(),
                    value: canonical_encode(&squat).unwrap().to_vec(),
                },
            ])
            .await
            .expect("adopt");
        assert_eq!(
            (report.kept, report.refused, report.adopted, report.merged),
            (1, 1, 0, 0),
            "neither forgery verifies at its cell: the re-file loses to B's standing row at \
             the join, the squat is refused at first contact: {report:?}"
        );
        let GroupRosterRecord::Enrolled {
            reception_pubkey, ..
        } = merged_enrolled(&c, &cell).await
        else {
            panic!("the cell is still Enrolled");
        };
        assert_ne!(reception_pubkey, forger_key, "B's row is untouched");
        assert!(
            c.group_rows().await.iter().all(|r| r.key != fresh_cell),
            "the squat never reached the plane"
        );
        c.remove_device(&device_a()).await;
        c.remove_device(&device_b()).await;

        assert_eq!(
            c.run_pass().await,
            RevocationPass {
                authority_scopes: 1,
                revoked: 1,
                readmitted: 2,
                minted: 1,
                unsourced: 0,
            },
        );
        let rows = c.group_rows().await;
        let (_, roster, tip) =
            member_view(&rows, &c.ceremony.scope_id, &c.ceremony.member_reception);
        let targets: Vec<_> = roster.wrap_targets().cloned().collect();
        assert!(
            targets.iter().all(|m| m.reception_pubkey != forger_key),
            "no wrap target carries the forger's key: {targets:?}"
        );
        assert_eq!(
            targets
                .iter()
                .filter(|m| m.member_actor == member().actor_id())
                .count(),
            1
        );
        assert!(tip.is_some());
    }

    fn hex(id: &[u8; 32]) -> String {
        fauna_core::hex32::encode(id)
    }
}
