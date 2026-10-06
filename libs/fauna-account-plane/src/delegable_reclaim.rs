//! **The delegable scope's retire behind the succession carry**
//! (`succession-aftermath.md` § Re-key scope → *The predecessor's own row is
//! retired behind the carry*): a pass step that retires, at the bound nest,
//! each attested predecessor's delegable row once a row sealed under the
//! successor's schedule carries its item there.
//!
//! The successor's walk re-authors each predecessor row as its own, but
//! nothing is written at the predecessor writer's coordinate, so without this
//! step the predecessor's row stays live beside the carry for good — and the
//! live-entry cap counts both, so a predecessor past half the cap could never
//! be carried whole.
//!
//! **The candidates are the walk's** (clause (1)): every row the bound
//! plane's last completed full-state reconcile opened under a predecessor's
//! delegable schedule and merged ([`AccountStatePlane::inherited_rows`]).
//! This step opens nothing under a retired schedule itself — the retired
//! schedule still opens rows in the bound nest's walk and nowhere else
//! (*Which walks carry*), and [`AccountStatePlane::open_relay_row`], the only
//! open here, tries the plane's own schedule alone.
//!
//! **The licence** (clause (2), [`covering_row`]): a row at the item's key
//! under the successor's schedule, written by this device or by a verified
//! member of its fleet, that opens to this replica's merged entry for the
//! item, and that the bound nest holds as this pass saw it
//! (`account-sync-plane.md` § The bind leg, ruling 1(d):
//! [`AccountStatePlane::listed_at_or_above`]). No such row, no retire — the
//! predecessor's row stays the item's carrier at the nest.
//!
//! **The retire** (clause (3)): the existing kind, naming the row by its own
//! coordinates, withheld against the gate's watermark exactly as the fleet
//! scope's reclamation pass withholds
//! ([`crate::account_state_plane::withheld_by_gate`]); the relay copy is
//! forgotten once the nest answers retired or gone.
//! [`AccountStatePlane::retire`] enters it in the store's retire record with
//! its scope, so the secondary leg re-issues it at the linked nests
//! (ruling 5).
//!
//! **The cover step** ([`reclaim_below_cover`];
//! `delegable-scope-reclamation.md` § Delegable-scope reclamation, parts (3)
//! and (4)): the rest of the scope's reclamation, one live row per item. For
//! each item the listing shows a row of that this device opens, the *cover*
//! is the listed row latest in serve order, written by this device or a
//! verified member, that opens to this replica's merged entry. No cover — or
//! one with an equal, later row no member wrote above it — and no unsent own
//! row for the item: the device re-authors the merged entry as its own row
//! (the hand-over), whose put names every row it covers
//! (`AccountStatePlane::publish`). A cover: every listed row the device
//! opened that is [`below`] it goes, whoever wrote it, behind the gate. The
//! publish diff skips by the same predicate ([`listed_cover_above`]), and the
//! linked leg runs the retire half at each linked nest on that nest's own
//! listing and serve order.
//!
//! **Why no row is lost** (the doc's paragraph of that name): every retire
//! names one row and cites a row above it, and *above* is decided by what the
//! two rows open to and by the nest's serve order, never by a replica's view
//! of the fleet. [`entry_covers`] is strict — a row that would win back
//! (a tied stamp with another value) is never called below — and a row with
//! no known serve position is never called below an equal one.

use std::collections::BTreeSet;

use anyhow::Result;
use ed25519_dalek::SigningKey;
use fauna_account_store::types::{RelayRow, StateEntry, WriterId};
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::account_entry_crypto::EntryPlaintext;
use fauna_core::generation::FleetView;
use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
use fauna_protocol::merge_policy::{
    KIND_READ_MARKER, KIND_SEEN_SET, MergeOutcome, MergePolicy, apply_class2,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::account_state_plane::{
    AccountStatePlane, ItemId, RetireOutcome, entry_to_plaintext, withheld_by_gate,
};
use crate::departure::member_scope_of_item;
use crate::generation_reclaim::ReclaimState;
use crate::generation_tip::GenerationTrust;

/// What one retire-behind-the-carry step did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CarryRetire {
    /// Candidates the walk listed.
    pub candidates: usize,
    /// Retired at the bound nest this pass.
    pub retired: usize,
    /// Already off the feed (retired by another asker, or superseded).
    pub gone: usize,
    /// Kept: no listed member row carries the merged entry (*No cover, no
    /// retire*), or this replica holds no entry for the item.
    pub uncovered: usize,
    /// Kept: the row sits above the gate's watermark — withheld unsent.
    pub withheld: usize,
    /// Kept: the nest deferred the retire (`not_yet_stable`).
    pub deferred: usize,
    /// Uncovered candidates whose merged entry this device re-authored as its
    /// own row (*No cover, no retire*: the hand-over, part (3)); the
    /// candidate goes once that row is listed.
    pub handed_over: usize,
}

/// Run the step on the bound nest's delegable plane, right after a pass's
/// reconcile of it. `None` when there is nothing to judge: the plane was
/// handed no predecessor schedule, or no full-state reconcile of it has
/// completed (no listing, no candidates).
pub async fn retire_behind_carry<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
) -> Result<Option<CarryRetire>>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    if !plane.holds_predecessor_schedules() || plane.listing().is_none() {
        return Ok(None);
    }
    let Some(mut candidates) = plane.inherited_rows() else {
        return Ok(None);
    };
    candidates.sort_by_key(|c| (c.writer, c.item_key, c.writer_seq));
    candidates.dedup_by_key(|c| (c.writer, c.item_key, c.writer_seq));
    let mut report = CarryRetire {
        candidates: candidates.len(),
        ..CarryRetire::default()
    };
    if candidates.is_empty() {
        return Ok(Some(report));
    }
    let state = ReclaimState::read(store, trust, writer_key.verifying_key().to_bytes()).await?;
    let me = store.writer();
    let withhold_above = plane.retirable_through_seq();
    let unsent = plane.unsent_own_items().await?;
    for candidate in candidates {
        let Some(entry) = store.state(&candidate.kind, &candidate.key).await? else {
            report.uncovered += 1;
            continue;
        };
        if covering_row(plane, state.view(), &me, &entry)
            .await?
            .is_none()
        {
            report.uncovered += 1;
            if !unsent.contains(&(entry.kind.clone(), entry.key.clone())) {
                hand_over(plane, &entry_to_plaintext(&entry)).await?;
                report.handed_over += 1;
            }
            continue;
        }
        if withheld_by_gate(withhold_above, candidate.feed_seq) {
            report.withheld += 1;
            continue;
        }
        match plane
            .retire(
                &candidate.item_key,
                &candidate.writer,
                candidate.writer_seq,
                None,
                false,
            )
            .await?
        {
            outcome @ (RetireOutcome::Retired | RetireOutcome::Gone) => {
                if outcome == RetireOutcome::Retired {
                    report.retired += 1;
                } else {
                    report.gone += 1;
                }
                plane
                    .relay_forget(&candidate.writer, &candidate.item_key)
                    .await?;
            }
            RetireOutcome::NotYetStable | RetireOutcome::GenerationInUse => report.deferred += 1,
            // The peer leg: no retire door. The bound plane never answers so.
            RetireOutcome::Unsupported => break,
        }
    }
    if report.retired > 0 || report.deferred > 0 || report.withheld > 0 {
        tracing::info!(
            scope = %plane.scope(),
            candidates = report.candidates,
            retired = report.retired,
            gone = report.gone,
            uncovered = report.uncovered,
            withheld = report.withheld,
            deferred = report.deferred,
            "delegable scope: retire behind the succession carry"
        );
    }
    Ok(Some(report))
}

/// **The licence** (`succession-aftermath.md` § Re-key scope → *The
/// predecessor's own row is retired behind the carry*, clause (2)): the
/// relay row at `entry`'s gen-0 item key under this plane's own schedule that
/// carries `entry` whole at the bound nest — written by `me` or by a writer
/// `view` verifies as a member, opening (under the plane's own keys only) to
/// exactly `entry`'s value, `merge_meta` and tombstone marker, and held by the
/// nest at or above its `writer_seq` as this pass saw it. `None`: no cover.
pub async fn covering_row<B, R>(
    plane: &AccountStatePlane<'_, B, R>,
    view: &FleetView,
    me: &WriterId,
    entry: &StateEntry,
) -> Result<Option<RelayRow>>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let Some(item_key) = plane.gen0_item_key(&entry.kind, &entry.key) else {
        return Ok(None);
    };
    let merged = entry_to_plaintext(entry);
    for row in plane.relay_rows_at(&item_key).await? {
        if row.writer != *me && !view.is_verified_member(&row.writer.0) {
            continue;
        }
        if !plane.listed_at_or_above(&row.writer, &item_key, row.writer_seq) {
            continue;
        }
        if plane.open_relay_row(&row).await?.as_ref() == Some(&merged) {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

/// **Does `upper` cover `lower`** (`delegable-scope-reclamation.md`
/// § Delegable-scope reclamation, the definition of *below*): the two are
/// equal, or `lower` lost the merge to `upper` — applied to `upper` it
/// changes nothing, and `upper` applied to `lower` would change it. The
/// second half keeps the relation strict: two values neither of which moves
/// the other (an `Immutable` kind's two values, a tied stamp) never cover each
/// other, so two replicas holding different merged values can never retire
/// each other's row.
pub fn entry_covers(policy: MergePolicy, upper: &EntryPlaintext, lower: &EntryPlaintext) -> bool {
    let keeps = |current: &EntryPlaintext, incoming: &EntryPlaintext| {
        matches!(
            apply_class2(policy, Some(current), incoming),
            Ok(MergeOutcome::KeepCurrent)
        )
    };
    upper == lower || (keeps(upper, lower) && !keeps(lower, upper))
}

/// **Is a row opening to `row` (served at `row_at`) below a cover opening
/// to `cover` (served at `cover_at`)?** Below means its entry lost the merge
/// ([`entry_covers`], strict), or it is the same entry and the nest serves it
/// earlier. A row with no known serve position is never below an equal row.
pub fn below(
    policy: MergePolicy,
    cover: &EntryPlaintext,
    cover_at: Option<u64>,
    row: &EntryPlaintext,
    row_at: Option<u64>,
) -> bool {
    if row != cover {
        return entry_covers(policy, cover, row);
    }
    matches!((row_at, cover_at), (Some(r), Some(c)) if r < c)
}

/// What one cover step did ([`reclaim_below_cover`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverReclaim {
    /// Items judged: a listed row of each opened here, and a merged entry
    /// held.
    pub items: usize,
    /// Items re-authored as this device's own row and taken by the nest
    /// (part (3)).
    pub handed_over: usize,
    /// Hand-overs the nest refused for room: the row is parked and the
    /// publish retries it (refinement 11).
    pub refused_for_room: usize,
    /// Rows below a cover retired this pass.
    pub retired: usize,
    /// Rows below a cover already off the feed.
    pub gone: usize,
    /// Kept: above the gate's watermark — withheld unsent.
    pub withheld: usize,
    /// Kept: the nest deferred the retire.
    pub deferred: usize,
    /// Rows of a departed scope's items retired this pass (part (6)): this
    /// device's own, and those of writers that are not verified members.
    pub departed: usize,
}

/// One listed row of an item, opened.
struct Opened {
    row: RelayRow,
    plaintext: EntryPlaintext,
    at: Option<u64>,
}

/// One item as this pass sees it at a plane's nest: the listed rows of it the
/// plane opens, the merged entry, and the cover among them.
struct Judged {
    item_key: [u8; 32],
    policy: MergePolicy,
    merged: EntryPlaintext,
    rows: Vec<Opened>,
    /// Index into `rows`.
    cover: Option<usize>,
}

impl Judged {
    fn cover(&self) -> Option<&Opened> {
        self.cover.map(|i| &self.rows[i])
    }

    /// Is the merged entry the join of every listed row opened here — does it
    /// cover each? The hand-over's precondition: a replica whose merged entry
    /// trails a row it opened has a walk still to merge it, and its own put
    /// of the trailing entry would collapse its own newer row at the nest.
    fn merged_covers_every_row(&self) -> bool {
        self.rows
            .iter()
            .all(|o| entry_covers(self.policy, &self.merged, &o.plaintext))
    }

    /// Is `row` below this item's cover?
    fn below_cover(&self, row: &Opened) -> bool {
        let Some(cover) = self.cover() else {
            return false;
        };
        (row.row.writer, row.row.writer_seq) != (cover.row.writer, cover.row.writer_seq)
            && below(
                self.policy,
                &cover.plaintext,
                cover.at,
                &row.plaintext,
                row.at,
            )
    }
}

/// Judge the item at `item_key`: open every relay row there that the listing
/// holds as this pass saw it, read the merged entry, and find the cover —
/// among the rows written by `me` or a member `view` verifies that open to
/// the merged entry, the one latest in serve order. `None` when no listed row
/// opens here, or no entry is held.
async fn judge_item<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    view: &FleetView,
    me: &WriterId,
    item_key: [u8; 32],
) -> Result<Option<Judged>>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let mut rows = Vec::new();
    for row in plane.relay_rows_at(&item_key).await? {
        if !plane.listed_at_or_above(&row.writer, &item_key, row.writer_seq) {
            continue;
        }
        if let Some(plaintext) = plane.open_relay_row(&row).await? {
            let at = plane.serve_position(&row);
            rows.push(Opened { row, plaintext, at });
        }
    }
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let (kind, key) = (first.plaintext.kind.clone(), first.plaintext.key.clone());
    if rows
        .iter()
        .any(|o| o.plaintext.kind != kind || o.plaintext.key != key)
    {
        // One blinded key names one item; rows that say otherwise are no
        // evidence about either.
        return Ok(None);
    }
    let (Some(entry), Some(policy)) = (store.state(&kind, &key).await?, plane.merge_policy(&kind))
    else {
        return Ok(None);
    };
    let merged = entry_to_plaintext(&entry);
    let cover = rows
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            (o.row.writer == *me || view.is_verified_member(&o.row.writer.0))
                && o.plaintext == merged
        })
        .max_by_key(|(_, o)| o.at)
        .map(|(i, _)| i);
    Ok(Some(Judged {
        item_key,
        policy,
        merged,
        rows,
        cover,
    }))
}

/// **Is `row` below a cover this pass lists at `plane`'s nest?** — the
/// publish diff's skip (`account-sync-plane.md` § The bind leg, ruling 1(b)),
/// by the cover step's own predicate. A row the nest does not list at its own
/// coordinate has no place in that nest's serve order: it is below a listed
/// cover whose entry covers it, an equal one included (the definition of
/// *below*) — pushed, it would carry nothing the cover does not and, landing
/// last, displace the cover it duplicates. `false` off the delegable scope,
/// with no listing, and for a row this plane cannot open.
pub async fn listed_cover_above<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    view: &FleetView,
    row: &RelayRow,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if plane.scope() != ACCOUNT_STATE_SCOPE || plane.listing().is_none() {
        return Ok(false);
    }
    let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
        return Ok(false);
    };
    let Some(plaintext) = plane.open_relay_row(row).await? else {
        return Ok(false);
    };
    let me = store.writer();
    let Some(judged) = judge_item(store, plane, view, &me, item_key).await? else {
        return Ok(false);
    };
    if plaintext.kind != judged.merged.kind || plaintext.key != judged.merged.key {
        return Ok(false);
    }
    if !plane.listed_at_or_above(&row.writer, &item_key, row.writer_seq) {
        return Ok(judged
            .cover()
            .is_some_and(|cover| entry_covers(judged.policy, &cover.plaintext, &plaintext)));
    }
    let at = plane.serve_position(row);
    Ok(judged.below_cover(&Opened {
        row: row.clone(),
        plaintext,
        at,
    }))
}

/// **The rows a push of `row` names at `plane`'s nest** (part (2), at the
/// diff): every listed row of the same item under another writer that this
/// plane opens to an entry `row`'s covers. The pushed row is the item's latest
/// once it lands there, so an equal row is below it. Empty off the delegable
/// scope.
pub async fn rows_a_push_names<B, R>(
    plane: &AccountStatePlane<'_, B, R>,
    row: &RelayRow,
) -> Result<Vec<RelayRow>>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if plane.scope() != ACCOUNT_STATE_SCOPE {
        return Ok(Vec::new());
    }
    let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
        return Ok(Vec::new());
    };
    let Some(pushed) = plane.open_relay_row(row).await? else {
        return Ok(Vec::new());
    };
    let Some(policy) = plane.merge_policy(&pushed.kind) else {
        return Ok(Vec::new());
    };
    let mut named = Vec::new();
    for listed in plane.relay_rows_at(&item_key).await? {
        if listed.writer == row.writer
            || !plane.listed_at_or_above(&listed.writer, &item_key, listed.writer_seq)
        {
            continue;
        }
        if let Some(opened) = plane.open_relay_row(&listed).await?
            && entry_covers(policy, &pushed, &opened)
        {
            named.push(listed);
        }
    }
    named.truncate(fauna_protocol::account_state::MAX_REPLACED_ROWS_PER_PUT);
    Ok(named)
}

/// Re-author `merged` as this device's own row through the ordinary writer
/// door — value, `merge_meta` and tombstone marker verbatim — published in
/// journal order; its put names every row it covers
/// (`AccountStatePlane::publish`). A refusal for room parks the row, which is
/// no error on this scope.
async fn hand_over<B, R>(
    plane: &AccountStatePlane<'_, B, R>,
    merged: &EntryPlaintext,
) -> Result<u64>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let item = ItemId {
        kind: merged.kind.clone(),
        key: merged.key.clone(),
    };
    let meta = merged.merge_meta.clone().map(|m| m.to_vec());
    if merged.tombstone {
        plane.tombstone(&item, meta).await
    } else {
        plane.put(&item, merged.value.to_vec(), meta).await
    }
}

/// Retire one listed row `o` of the item at `item_key` behind the gate, and
/// count it: on the bound plane through the retire record, forgetting the
/// relay copy on retired or gone; on a linked plane unrecorded. `false` when
/// the nest has no retire door and the step stops.
async fn retire_listed<B, R>(
    plane: &AccountStatePlane<'_, B, R>,
    item_key: &[u8; 32],
    o: &Opened,
    withhold_above: Option<u64>,
    report: &mut CoverReclaim,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let linked = plane.is_linked();
    if withheld_by_gate(withhold_above, o.at) {
        report.withheld += 1;
        return Ok(true);
    }
    let outcome = if linked {
        plane
            .retire_unrecorded(item_key, &o.row.writer, o.row.writer_seq)
            .await?
    } else {
        plane
            .retire(item_key, &o.row.writer, o.row.writer_seq, None, false)
            .await?
    };
    match outcome {
        RetireOutcome::Retired | RetireOutcome::Gone => {
            if outcome == RetireOutcome::Retired {
                report.retired += 1;
            } else {
                report.gone += 1;
            }
            if !linked {
                plane.relay_forget(&o.row.writer, item_key).await?;
            }
        }
        RetireOutcome::NotYetStable | RetireOutcome::GenerationInUse => {
            report.deferred += 1;
        }
        RetireOutcome::Unsupported => return Ok(false),
    }
    Ok(true)
}

/// **The cover step** (module docs): on the delegable scope, after a pass's
/// reconcile, publish diff and fleet walk. On the bound nest's plane it hands
/// over and retires; on a linked nest's (`AccountStatePlane::is_linked`) it
/// retires only, by that nest's listing and serve order, enters no retire in
/// the record and forgets no relay copy — the bound plane's own step does
/// that. `None` off the delegable scope or with no listing banked.
///
/// **A member scope's items** (`delegable-scope-reclamation.md` § Delegable-
/// scope reclamation, parts (6) and (7); [`crate::departure`]). An item of a
/// scope in the departed list is never handed over: the step retires each
/// listed row of it that this device wrote or that no verified member wrote,
/// and never a member sibling's. Any other member scope's item is handed over
/// only when `membership` — this pass's affirmative answer, as the scope
/// strings it names; `None` when the pass has none — names its scope, and
/// then also when the nest lists no row of it at all: that is how a re-joined
/// conversation's entry, and an entry a departed sibling's retire left with
/// no row, is written back. Preference records and the own-actor seen-set
/// entries are handed over as before, whatever the answer.
pub async fn reclaim_below_cover<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    membership: Option<&BTreeSet<String>>,
) -> Result<Option<CoverReclaim>>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    if plane.scope() != ACCOUNT_STATE_SCOPE {
        return Ok(None);
    }
    let Some(listing) = plane.listing() else {
        return Ok(None);
    };
    let linked = plane.is_linked();
    let state = ReclaimState::read(store, trust, writer_key.verifying_key().to_bytes()).await?;
    let me = store.writer();
    let unsent = if linked {
        Default::default()
    } else {
        plane.unsent_own_items().await?
    };
    let departed = crate::departure::departed_scopes(store).await?;
    let withhold_above = plane.retirable_through_seq();
    let item_keys: BTreeSet<[u8; 32]> = listing.keys().map(|(_, item_key)| *item_key).collect();
    let mut report = CoverReclaim::default();
    'items: for &item_key in &item_keys {
        crate::pass_breath::pass_breath().await;
        let Some(judged) = judge_item(store, plane, state.view(), &me, item_key).await? else {
            continue;
        };
        report.items += 1;
        let member = |w: &WriterId| *w == me || state.view().is_verified_member(&w.0);
        let member_scope = member_scope_of_item(&judged.merged.kind, &judged.merged.key);
        if let Some(scope) = &member_scope
            && departed.contains(scope)
        {
            // Part (6): this device's own rows and rows no member wrote. A
            // member sibling's row stays until that sibling concludes the
            // departure itself.
            for o in &judged.rows {
                if o.row.writer != me && state.view().is_verified_member(&o.row.writer.0) {
                    continue;
                }
                let before = report.retired + report.gone;
                if !retire_listed(plane, &judged.item_key, o, withhold_above, &mut report).await? {
                    break 'items;
                }
                report.departed += report.retired + report.gone - before;
            }
            continue;
        }
        // Part (7): a member scope's item is handed over only by a device
        // this pass's answer says is in the scope.
        let may_hand_over =
            member_scope.is_none_or(|scope| membership.is_some_and(|m| m.contains(&scope)));
        // Part (3)'s second case: an equal row no member wrote, served after
        // the cover — neither the cover nor below it.
        let orphan_above = judged.cover().is_some_and(|cover| {
            judged.rows.iter().any(|o| {
                !member(&o.row.writer)
                    && o.plaintext == judged.merged
                    && matches!((o.at, cover.at), (Some(r), Some(c)) if r > c)
            })
        });
        if !linked
            && may_hand_over
            && (judged.cover.is_none() || orphan_above)
            && judged.merged_covers_every_row()
            && !unsent.contains(&(judged.merged.kind.clone(), judged.merged.key.clone()))
        {
            let seq = hand_over(plane, &judged.merged).await?;
            if store.parked(plane.scope(), &me).await?.contains(&seq) {
                report.refused_for_room += 1;
            } else {
                report.handed_over += 1;
            }
            // The next pass lists the new row and retires what it covers.
            continue;
        }
        if judged.cover.is_none() {
            continue;
        }
        for o in &judged.rows {
            if judged.below_cover(o)
                && !retire_listed(plane, &judged.item_key, o, withhold_above, &mut report).await?
            {
                break 'items;
            }
        }
    }
    if !linked && let Some(membership) = membership {
        hand_over_unlisted(
            store,
            plane,
            membership,
            &departed,
            &item_keys,
            &unsent,
            &mut report,
        )
        .await?;
    }
    if report.handed_over + report.refused_for_room + report.retired + report.withheld > 0 {
        tracing::info!(
            scope = %plane.scope(),
            linked,
            items = report.items,
            handed_over = report.handed_over,
            refused_for_room = report.refused_for_room,
            retired = report.retired,
            gone = report.gone,
            withheld = report.withheld,
            deferred = report.deferred,
            departed = report.departed,
            "delegable scope: one live row per item"
        );
    }
    Ok(Some(report))
}

/// Part (7)'s write-back, on the bound plane: a member scope's item the
/// device holds an entry for, whose scope `membership` names, and of which the
/// nest lists no row at all (no key in `listed`) — no cover, so the device
/// re-authors it, unless an unsent own row of it is already owed. The rows
/// the listing shows were judged in the loop; these are the items a
/// departure's retires took off the nest while this device stayed, or before
/// it re-joined.
async fn hand_over_unlisted<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    membership: &BTreeSet<String>,
    departed: &BTreeSet<String>,
    listed: &BTreeSet<[u8; 32]>,
    unsent: &BTreeSet<(String, String)>,
    report: &mut CoverReclaim,
) -> Result<()>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let me = store.writer();
    for kind in [KIND_READ_MARKER, KIND_SEEN_SET] {
        for entry in store.states_of_kind(kind).await? {
            let Some(scope) = member_scope_of_item(&entry.kind, &entry.key) else {
                continue;
            };
            if !membership.contains(&scope)
                || departed.contains(&scope)
                || unsent.contains(&(entry.kind.clone(), entry.key.clone()))
            {
                continue;
            }
            let Some(item_key) = plane.gen0_item_key(&entry.kind, &entry.key) else {
                continue;
            };
            if listed.contains(&item_key) {
                continue;
            }
            crate::pass_breath::pass_breath().await;
            let seq = hand_over(plane, &entry_to_plaintext(&entry)).await?;
            if store.parked(plane.scope(), &me).await?.contains(&seq) {
                report.refused_for_room += 1;
            } else {
                report.handed_over += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_state_plane::{InheritedRow, Listing};
    use crate::generation_fixture_test_support::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
    use fauna_core::crypto::{BackupKey, DelegableSchedule};
    use fauna_core::data::ModerationConfig;
    use fauna_protocol::account_state::{
        ACCOUNT_STATE_SCOPE, AccountStatePutReply, AccountStatePutRequest, AccountStateRetireReply,
        AccountStateRetireRequest, KIND_STATE_PUT, KIND_STATE_RETIRE,
    };
    use fauna_protocol::error::RpcError;
    use fauna_protocol::merge_policy::{KIND_MODERATION, LwwStamp, PREFERENCE_KEY, kind_keys};
    use std::sync::{Arc, Mutex};

    /// A nest that answers every retire `retired` and every put stored, and
    /// records each.
    #[derive(Clone, Default)]
    struct RetireNest {
        retires: Arc<Mutex<Vec<AccountStateRetireRequest>>>,
        puts: Arc<Mutex<Vec<AccountStatePutRequest>>>,
    }

    #[derive(Debug)]
    struct NestErr(RpcError);
    impl std::fmt::Display for NestErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0.code)
        }
    }
    impl RpcErrorClass for NestErr {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    impl RpcRequester for RetireNest {
        type Error = NestErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, NestErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_core::encoding::canonical_encode(&payload).unwrap();
            if kind == KIND_STATE_PUT {
                self.puts
                    .lock()
                    .unwrap()
                    .push(fauna_core::encoding::canonical_decode(&bytes).unwrap());
                let reply = fauna_core::encoding::canonical_encode(&AccountStatePutReply {
                    seq: 1,
                    ..Default::default()
                })
                .unwrap();
                return Ok(fauna_core::encoding::canonical_decode(&reply).unwrap());
            }
            if kind != KIND_STATE_RETIRE {
                return Err(NestErr(RpcError::new("kind_not_served", kind)));
            }
            self.retires
                .lock()
                .unwrap()
                .push(fauna_core::encoding::canonical_decode(&bytes).unwrap());
            let reply = fauna_core::encoding::canonical_encode(&AccountStateRetireReply {
                retired: true,
                extra: Default::default(),
            })
            .unwrap();
            Ok(fauna_core::encoding::canonical_decode(&reply).unwrap())
        }
    }

    const PREDECESSOR: [u8; 32] = [0x5A; 32];

    fn moderation(words: &[&str], at_ms: i64, writer: [u8; 32]) -> StateEntry {
        let mut entry = machinery_row(
            KIND_MODERATION,
            PREFERENCE_KEY.into(),
            &ModerationConfig {
                muted_keywords: words.iter().map(|w| (*w).into()).collect(),
                ..Default::default()
            },
        );
        entry.merge_meta = Some(LwwStamp { at_ms, writer }.encode().unwrap());
        entry
    }

    /// `seed`'s relay row of `entry` at `seq`, sealed under the fixture's
    /// own (the successor's) schedule — as the walk would have recorded it.
    async fn relayed(f: &Fixture, seed: [u8; 32], seq: u64, entry: &StateEntry) -> RelayRow {
        let writer_key = device_key(seed);
        let sealed = seal_entry(
            &kind_keys(&f.schedule, &entry.kind).unwrap(),
            &EntryCoordinates {
                writer_id: writer_key.verifying_key().to_bytes(),
                writer_seq: seq,
                scope: ACCOUNT_STATE_SCOPE,
            },
            &EntryPlaintext {
                kind: entry.kind.clone(),
                key: entry.key.clone(),
                merge_meta: entry.merge_meta.clone().map(Into::into),
                value: entry.value.clone().into(),
                tombstone: entry.tombstone,
            },
            &writer_key,
        )
        .unwrap();
        let relay = RelayRow {
            scope: ACCOUNT_STATE_SCOPE.into(),
            item_class: "state-entry".into(),
            writer: WriterId(writer_key.verifying_key().to_bytes()),
            writer_seq: seq,
            item_key: sealed.item_key.to_vec(),
            op: "state-put".into(),
            entry: Some(sealed.envelope),
            feed_seq: None,
        };
        f.store.record_relay_row(&relay).await.unwrap();
        relay
    }

    /// The predecessor's row the walk listed: its own coordinates, at a
    /// blinded key no successor row sits at.
    fn candidate(feed_seq: Option<u64>) -> InheritedRow {
        InheritedRow {
            writer: WriterId(device_id_of(PREDECESSOR)),
            writer_seq: 3,
            item_key: [0xE1; 32],
            feed_seq,
            kind: KIND_MODERATION.into(),
            key: PREFERENCE_KEY.into(),
        }
    }

    /// The walk's relay copy of the candidate's row.
    async fn relay_copy(f: &Fixture, pred: &InheritedRow) {
        f.store
            .record_relay_row(&RelayRow {
                scope: ACCOUNT_STATE_SCOPE.into(),
                item_class: "state-entry".into(),
                writer: pred.writer,
                writer_seq: pred.writer_seq,
                item_key: pred.item_key.to_vec(),
                op: "state-put".into(),
                entry: Some(vec![1, 2, 3]),
                feed_seq: pred.feed_seq,
            })
            .await
            .unwrap();
    }

    /// Proof (6), a sibling's cover (`succession-aftermath.md` § Re-key scope
    /// → *The predecessor's own row is retired behind the carry*, *What the
    /// licence covers*): a device whose merged entry came from a verified
    /// sibling's listed row, and that holds no own row for the item, retires
    /// the predecessor's row by the row's own coordinates and forgets its
    /// relay copy. The same device keeps the row when the sibling is not a
    /// verified member, when the sibling's row is not listed, and when the
    /// listed row carries another value.
    #[tokio::test]
    async fn a_verified_siblings_listed_row_covers_and_a_non_members_does_not() {
        for (enrolled, listed, same_value) in [
            (true, true, true),
            (false, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            let f = fixture().await;
            f.put(enrollment_row(US)).await;
            if enrolled {
                f.put(enrollment_row(THEM)).await;
            }
            let merged = moderation(&["witness"], 9_000, device_id_of(PREDECESSOR));
            f.put(merged.clone()).await;
            let sibling_row = if same_value {
                merged.clone()
            } else {
                moderation(&["other"], 9_001, device_id_of(THEM))
            };
            let cover = relayed(&f, THEM, 4, &sibling_row).await;
            let nest = RetireNest::default();
            let inherited = [DelegableSchedule::derive(&BackupKey::derive(&[0x13; 32]))];
            let p: AccountStatePlane<'_, SqliteBackend, RetireNest> = AccountStatePlane::new(
                &f.store,
                &nest,
                &f.schedule,
                &f.writer_key,
                &f.trust,
                ACCOUNT_STATE_SCOPE,
            )
            .unwrap()
            .with_predecessor_schedules(&inherited);
            let cover_key: [u8; 32] = cover.item_key.as_slice().try_into().unwrap();
            p.set_listing(Some(if listed {
                Listing::from([((cover.writer, cover_key), cover.writer_seq)])
            } else {
                Listing::new()
            }));
            let pred = candidate(None);
            p.set_inherited_rows(Some(vec![pred.clone()]));
            relay_copy(&f, &pred).await;

            let report = retire_behind_carry(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap()
                .expect("a plane with a predecessor schedule and a listing judges");
            let case = format!("enrolled={enrolled} listed={listed} same_value={same_value}");
            let retires = nest.retires.lock().unwrap().clone();
            let kept = f
                .store
                .relay_rows_at(ACCOUNT_STATE_SCOPE, "state-entry", &pred.item_key)
                .await
                .unwrap();
            if enrolled && listed && same_value {
                assert_eq!(
                    (report.retired, report.uncovered),
                    (1, 0),
                    "{case}: {report:?}"
                );
                assert_eq!(retires.len(), 1, "{case}");
                assert_eq!(retires[0].writer_id, pred.writer.to_hex());
                assert_eq!(retires[0].writer_seq, pred.writer_seq as i64);
                assert_eq!(retires[0].item_key.as_ref(), pred.item_key.as_slice());
                assert!(kept.is_empty(), "{case}: the relay copy is forgotten");
            } else {
                assert_eq!(
                    (report.retired, report.uncovered),
                    (0, 1),
                    "{case}: {report:?}"
                );
                assert!(retires.is_empty(), "{case}: no cover, no retire");
                assert_eq!(kept.len(), 1, "{case}: the relay copy stays");
            }
        }
    }

    /// A plane handed no predecessor schedule, or holding no listing, judges
    /// nothing; this device's own listed row covers; and the gate's
    /// watermark withholds a covered retire unsent, by the comparison the
    /// fleet scope's reclamation pass shares.
    #[tokio::test]
    async fn the_gate_withholds_and_no_schedule_or_listing_judges_nothing() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        let merged = moderation(&["witness"], 9_000, device_id_of(PREDECESSOR));
        f.put(merged.clone()).await;
        let own = relayed(&f, US, 4, &merged).await;
        let own_key: [u8; 32] = own.item_key.as_slice().try_into().unwrap();
        let nest = RetireNest::default();
        let plain: AccountStatePlane<'_, SqliteBackend, RetireNest> = AccountStatePlane::new(
            &f.store,
            &nest,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_SCOPE,
        )
        .unwrap();
        plain.set_listing(Some(Listing::from([((own.writer, own_key), 4)])));
        plain.set_inherited_rows(Some(vec![candidate(None)]));
        assert_eq!(
            retire_behind_carry(&f.store, &plain, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            None,
            "no predecessor schedule: nothing to judge"
        );

        let inherited = [DelegableSchedule::derive(&BackupKey::derive(&[0x13; 32]))];
        let p = plain.with_predecessor_schedules(&inherited);
        p.set_listing(None);
        assert_eq!(
            retire_behind_carry(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            None,
            "no listing: nothing is published as this pass saw it"
        );

        // This device's own listed row is the cover.
        p.set_listing(Some(Listing::from([((own.writer, own_key), 4)])));
        let pred = candidate(Some(50));
        relay_copy(&f, &pred).await;
        p.set_inherited_rows(Some(vec![pred]));
        let report = retire_behind_carry(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((report.retired, report.withheld), (1, 0), "{report:?}");
        assert_eq!(nest.retires.lock().unwrap().len(), 1);

        // The comparison the step withholds by.
        assert!(withheld_by_gate(Some(49), Some(50)));
        assert!(!withheld_by_gate(Some(50), Some(50)));
        assert!(!withheld_by_gate(None, Some(50)));
        assert!(!withheld_by_gate(Some(49), None));
    }

    /// Proof (6) of the cover step (`delegable-scope-reclamation.md`
    /// § Delegable-scope reclamation, the definition of *below*): a listed row
    /// this device cannot open is never named, judged or retired; and an equal
    /// row whose serve position is unknown is not below the cover, so the step
    /// keeps it. The control: the same equal row at a known, earlier position
    /// is retired behind the cover and its relay copy forgotten.
    #[tokio::test]
    async fn an_unopenable_row_is_kept_and_an_unknown_position_is_never_below_an_equal_cover() {
        for known_position in [false, true] {
            let f = fixture().await;
            f.put(enrollment_row(US)).await;
            f.put(enrollment_row(THEM)).await;
            let merged = moderation(&["witness"], 9_000, device_id_of(THEM));
            f.put(merged.clone()).await;
            let cover = relayed(&f, THEM, 4, &merged).await;
            let own = relayed(&f, US, 2, &merged).await;
            let item_key: [u8; 32] = cover.item_key.as_slice().try_into().unwrap();
            let sealed_elsewhere = RelayRow {
                writer: WriterId(device_id_of(PREDECESSOR)),
                writer_seq: 7,
                entry: Some(vec![1, 2, 3]),
                feed_seq: Some(1),
                ..cover.clone()
            };
            f.store.record_relay_row(&sealed_elsewhere).await.unwrap();
            let nest = RetireNest::default();
            let p: AccountStatePlane<'_, SqliteBackend, RetireNest> = AccountStatePlane::new(
                &f.store,
                &nest,
                &f.schedule,
                &f.writer_key,
                &f.trust,
                ACCOUNT_STATE_SCOPE,
            )
            .unwrap();
            p.set_listing(Some(Listing::from([
                ((cover.writer, item_key), cover.writer_seq),
                ((own.writer, item_key), own.writer_seq),
                (
                    (sealed_elsewhere.writer, item_key),
                    sealed_elsewhere.writer_seq,
                ),
            ])));
            let mut served = crate::account_state_plane::ServeOrder::from([
                ((cover.writer, item_key), 50),
                ((sealed_elsewhere.writer, item_key), 1),
            ]);
            if known_position {
                served.insert((own.writer, item_key), 40);
            }
            p.set_served(Some(served));

            let report = reclaim_below_cover(&f.store, &p, &f.trust, &f.writer_key, None)
                .await
                .unwrap()
                .expect("the delegable plane with a listing runs the step");
            let case = format!("known_position={known_position}");
            assert_eq!(report.handed_over, 0, "{case}: a cover exists: {report:?}");
            let retires = nest.retires.lock().unwrap().clone();
            assert!(
                retires
                    .iter()
                    .all(|r| r.writer_id != sealed_elsewhere.writer.to_hex()),
                "{case}: a row this device cannot open is never retired"
            );
            assert!(
                retires.iter().all(|r| r.writer_id != cover.writer.to_hex()),
                "{case}: the cover is never retired"
            );
            let left: Vec<WriterId> = f
                .store
                .relay_rows_at(ACCOUNT_STATE_SCOPE, "state-entry", &item_key)
                .await
                .unwrap()
                .into_iter()
                .map(|r| r.writer)
                .collect();
            assert!(left.contains(&sealed_elsewhere.writer), "{case}: {left:?}");
            assert!(left.contains(&cover.writer), "{case}: {left:?}");
            if known_position {
                assert_eq!(report.retired, 1, "{case}: {report:?}");
                assert_eq!(retires.len(), 1, "{case}");
                assert_eq!(retires[0].writer_id, own.writer.to_hex());
                assert_eq!(retires[0].writer_seq, own.writer_seq as i64);
                assert!(!left.contains(&own.writer), "{case}: the copy is forgotten");
            } else {
                assert_eq!(report.retired, 0, "{case}: {report:?}");
                assert!(retires.is_empty(), "{case}: {retires:?}");
                assert!(left.contains(&own.writer), "{case}: {left:?}");
            }
        }
    }

    /// Part (3)'s second case (`delegable-scope-reclamation.md`
    /// § Delegable-scope reclamation): a member's listed row covers the item,
    /// and an equal row no member wrote is served after it — neither the cover
    /// nor below it, the residue of a departed device that re-authored after
    /// a sibling's raise. The step re-authors the merged entry as this
    /// device's own row and its put names both listed rows, so the nest keeps
    /// one, a member's; nothing is retired by the cover it does not outrank.
    /// The control: with no such later row the cover stands and nothing is
    /// put.
    #[tokio::test]
    async fn an_equal_later_row_no_member_wrote_is_handed_over_and_named() {
        for orphan_later in [true, false] {
            let f = fixture().await;
            f.put(enrollment_row(US)).await;
            f.put(enrollment_row(THEM)).await;
            let merged = moderation(&["witness"], 9_000, device_id_of(THEM));
            // Merged here and published: no unsent own row holds the item.
            let (_, seq) = f.store.put_state(merged.clone()).await.unwrap();
            f.store
                .advance_frontier(ACCOUNT_STATE_SCOPE, &f.store.writer(), seq)
                .await
                .unwrap();
            let cover = relayed(&f, THEM, 4, &merged).await;
            let orphan = relayed(&f, PREDECESSOR, 6, &merged).await;
            let item_key: [u8; 32] = cover.item_key.as_slice().try_into().unwrap();
            let nest = RetireNest::default();
            let p: AccountStatePlane<'_, SqliteBackend, RetireNest> = AccountStatePlane::new(
                &f.store,
                &nest,
                &f.schedule,
                &f.writer_key,
                &f.trust,
                ACCOUNT_STATE_SCOPE,
            )
            .unwrap();
            p.set_listing(Some(Listing::from([
                ((cover.writer, item_key), cover.writer_seq),
                ((orphan.writer, item_key), orphan.writer_seq),
            ])));
            let (cover_at, orphan_at) = if orphan_later { (10, 20) } else { (20, 10) };
            p.set_served(Some(crate::account_state_plane::ServeOrder::from([
                ((cover.writer, item_key), cover_at),
                ((orphan.writer, item_key), orphan_at),
            ])));

            let report = reclaim_below_cover(&f.store, &p, &f.trust, &f.writer_key, None)
                .await
                .unwrap()
                .expect("the delegable plane with a listing runs the step");
            let case = format!("orphan_later={orphan_later}");
            let puts = nest.puts.lock().unwrap().clone();
            let retires = nest.retires.lock().unwrap().clone();
            assert!(
                retires.iter().all(|r| r.writer_id != cover.writer.to_hex()),
                "{case}: a member's cover is never retired: {retires:?}"
            );
            if orphan_later {
                assert_eq!(report.handed_over, 1, "{case}: {report:?}");
                assert_eq!(puts.len(), 1, "{case}");
                assert_eq!(puts[0].writer_id, WriterId(device_id_of(US)).to_hex());
                assert_eq!(puts[0].item_key.as_ref(), item_key.as_slice());
                let named: std::collections::BTreeSet<(String, i64)> = puts[0]
                    .replaces
                    .iter()
                    .map(|r| (r.writer_id.clone(), r.writer_seq))
                    .collect();
                assert_eq!(
                    named,
                    [
                        (cover.writer.to_hex(), cover.writer_seq as i64),
                        (orphan.writer.to_hex(), orphan.writer_seq as i64),
                    ]
                    .into(),
                    "{case}: the put names every listed row of the item it opened"
                );
            } else {
                assert_eq!(report.handed_over, 0, "{case}: {report:?}");
                assert!(puts.is_empty(), "{case}: {puts:?}");
                assert_eq!(report.retired, 1, "{case}: the earlier equal row is below");
                assert_eq!(retires[0].writer_id, orphan.writer.to_hex());
            }
        }
    }

    fn marker(through: u64) -> EntryPlaintext {
        EntryPlaintext {
            kind: fauna_protocol::merge_policy::KIND_READ_MARKER.into(),
            key: fauna_core::read_marker::channel_key("ab12"),
            merge_meta: None,
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::read_marker::ReadMarker::new(through),
            )
            .unwrap()
            .into(),
            tombstone: false,
        }
    }

    fn seen(refs: &[(u8, u64)]) -> EntryPlaintext {
        let mut s = fauna_core::seen_set::SeenScopeSet::new();
        for (writer, seq) in refs {
            s.insert_ref([*writer; 32], *seq);
        }
        EntryPlaintext {
            kind: fauna_protocol::merge_policy::KIND_SEEN_SET.into(),
            key: "s".into(),
            merge_meta: None,
            value: fauna_core::encoding::canonical_encode(&s).unwrap().into(),
            tombstone: false,
        }
    }

    /// **A row is never called below a row it is above**
    /// (`delegable-scope-reclamation.md` § Delegable-scope reclamation, *Why
    /// no row is lost*), for the scope's three merge shapes: latest-wins, the
    /// seen-set and the read marker. Over every pair of values and serve
    /// positions, `below` never holds both ways; a row that would win the
    /// merge back is never below; and an equal row with no known position is
    /// never below.
    #[test]
    fn a_row_is_never_called_below_a_row_it_is_above() {
        let records = [
            entry_to_plaintext(&moderation(&["a"], 1, [1; 32])),
            entry_to_plaintext(&moderation(&["b"], 2, [1; 32])),
            // A tied stamp with another value: neither covers the other.
            entry_to_plaintext(&moderation(&["c"], 2, [1; 32])),
            entry_to_plaintext(&moderation(&["d"], 2, [2; 32])),
        ];
        let markers = [marker(1), marker(2), marker(9)];
        let seens = [seen(&[(1, 1)]), seen(&[(1, 1), (2, 2)]), seen(&[(3, 3)])];
        let positions = [None, Some(10), Some(20)];
        for (policy, values) in [
            (MergePolicy::LatestWins, &records[..]),
            (MergePolicy::CrdtPerField, &markers[..]),
            (MergePolicy::CrdtPerField, &seens[..]),
        ] {
            for x in values {
                for y in values {
                    for px in positions {
                        for py in positions {
                            let x_over_y = below(policy, x, px, y, py);
                            let y_over_x = below(policy, y, py, x, px);
                            assert!(!(x_over_y && y_over_x), "{x:?}@{px:?} / {y:?}@{py:?}");
                            if x_over_y && x != y {
                                // y lost the merge to x: applying x to y changes y.
                                assert!(!matches!(
                                    apply_class2(policy, Some(y), x),
                                    Ok(MergeOutcome::KeepCurrent)
                                ));
                            }
                            if x == y && (px.is_none() || py.is_none()) {
                                assert!(!x_over_y, "an unknown position is never below");
                            }
                        }
                    }
                }
            }
        }
        // The shapes the rule names.
        assert!(below(
            MergePolicy::CrdtPerField,
            &marker(9),
            None,
            &marker(1),
            None
        ));
        assert!(!below(
            MergePolicy::CrdtPerField,
            &marker(1),
            None,
            &marker(9),
            None
        ));
        assert!(!below(
            MergePolicy::CrdtPerField,
            &seens[0],
            None,
            &seens[2],
            None
        ));
        assert!(below(
            MergePolicy::LatestWins,
            &records[1],
            Some(5),
            &records[1],
            Some(4)
        ));
        assert!(!below(
            MergePolicy::LatestWins,
            &records[1],
            None,
            &records[2],
            None
        ));
        assert!(!below(
            MergePolicy::LatestWins,
            &records[2],
            None,
            &records[1],
            None
        ));
    }

    /// Proof (7): the step opens nothing under a predecessor's schedule — the
    /// retired schedule still opens rows in the bound nest's walk and nowhere
    /// else (*Which walks carry*). Its one open is `open_relay_row`, which
    /// tries the plane's own keys only (pinned by `crate::publish_diff`'s
    /// `a_predecessor_sealed_relay_row_is_never_vouched_on_a_plane_that_holds_its_schedule`).
    #[test]
    fn the_step_opens_nothing_under_a_predecessor_schedule() {
        let source = include_str!("delegable_reclaim.rs");
        let code: String = source
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "trial_open",
            "open_entry",
            "DelegableSchedule",
            "with_predecessor",
            "predecessor_mint",
        ] {
            assert!(!code.contains(forbidden), "the step names {forbidden}");
        }
        // Every open here is `open_relay_row`, the plane's own keys alone.
        assert!(code.contains(".open_relay_row("));
    }
}
