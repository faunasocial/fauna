//! **The publish diff** — the row half of the bind leg
//! (`account-sync-plane.md` § The bind leg, ruling 1): after a plane's
//! full-state reconcile, push to the bound nest, verbatim, every relay row the
//! nest's listing lacks or holds at a lower `writer_seq`.
//!
//! The journal-driven publish (`AccountStatePlane::publish_pending`) sends
//! this replica's own rows above its frontier slot, a slot that names no nest
//! — so a row once published anywhere never reached a second nest, a rebuilt
//! one, or any nest at all when a sibling wrote it. The reconcile already
//! answers with every live `(item, writer)` row the nest holds
//! (`AccountStatePlane::listing`); this step compares it with the relay plane
//! by `(writer, item)` and sends the difference, so the nest leg is as
//! symmetric as the peer leg, whose serve is the same source.
//!
//! **What may be pushed** (ruling 1(a)): this device's own rows — those at or
//! below its published high-water; the rows above it, and the rows it holds
//! parked, are `publish_pending`'s, which ran earlier in the same pass — and a sibling's row only when this
//! device opens it (the AEAD and the in-seal writer signature, which is
//! what "the walk verified it" means), its kind rides this scope, and its
//! writer is a verified member of this device's fleet view. A row this device
//! cannot open or verify is never pushed: the put door checks neither
//! membership nor content, so relaying it would let one hostile nest use
//! honest devices to plant rows on another under a sibling's writer id.
//!
//! **The one exception to the member test** (ruling 7(b)): a departing
//! device's own removal row — a row this device opened, resting in its
//! writer's own device-set cell and reading `Removed` by that writer, for a
//! device this replica's fleet view reads removed — is pushed although its
//! writer is no longer a member: the row is the evidence of exactly that, and
//! a device bound to a linked nest reads the removal only from a row in that
//! nest's feed. It is its writer's last word, so there is no later put of
//! that writer for it to wedge. Nothing else of a non-member goes — a
//! `Removed` row it wrote for another device included.
//!
//! **A covered row is skipped** (ruling 1(b)): a sibling's row the
//! reclamation pass calls covered is one its writer has retired or will, and
//! pushing it would put a superseded generation back in use on a nest with no
//! memory of the retirement (`crate::generation_reclaim::covered_at_tip`).
//!
//! **So is a dead row, whoever wrote it** (ruling 1(b)): a row the
//! reclamation pass forgets as dead from this replica's merged state alone —
//! sealed under a `Shredded` generation, or a copy of a dead gen-0 item
//! (`account-data-taxonomy.md` clause (3)(h)) — asked of that pass's own
//! predicate, [`ReclaimState::relay_row_dead`], and on the one scope it
//! reclaims. Its writer retires it under the rule this replica has just
//! applied, so no nest is owed it, and the pass that runs behind this step
//! forgets the copy. A `Shredded` generation's own mint row is not dead: no
//! pass forgets a sibling's copy of it, so it goes, and the refused put is
//! how this replica learns its retirement (ruling 1(c)).
//!
//! **So is a row below a listed cover, on the delegable scope, whoever wrote
//! it** (ruling 1(b); `delegable-scope-reclamation.md` § Delegable-scope
//! reclamation, part (4)): asked of the cover step's own predicate
//! (`crate::delegable_reclaim::listed_cover_above`), and the relay copy is
//! forgotten — a cover this nest lists carries its item. A row that is pushed
//! there names every listed row of its item it covers
//! (`crate::delegable_reclaim::rows_a_push_names`, part (2)), so it needs no
//! room for them.
//!
//! **A refused push is final for that copy** (ruling 1(c),
//! `AccountStatePlane::push_verbatim`). `scope_full` is reported, never
//! retried in a loop (ruling 1(d)). On the fleet scope it ends the push for
//! this pass — reclamation's retires, which run after, are what free
//! headroom. On the delegable scope the diff goes on, withholding only the
//! pushes that need room ([`needs_room`]) and still sending the rest, so a
//! row the nest has room for is never held behind one it has not.

use anyhow::Result;
use ed25519_dalek::SigningKey;
use fauna_account_store::types::{RelayRow, WriterId};
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::account_entry_crypto::EntryPlaintext;
use fauna_core::generation::DeviceSetRecord;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::merge_policy::{KIND_DEVICE_SET, home_scope_for_kind};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::account_state_plane::{AccountStatePlane, Listing, Pushed};
use crate::generation_reclaim::ReclaimState;
use crate::generation_tip::{self, GenerationTrust};

/// What one diff did (the pump's `publish_diff` / `fleet_publish_diff`
/// slots).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffPush {
    /// There was a completed listing to diff against. `false`: the reconcile
    /// failed this pass, and nothing was compared or sent.
    pub listed: bool,
    /// Rows the nest stored.
    pub pushed: usize,
    /// Rows the nest refused for good (`stale_writer_seq`) — their local
    /// copies retired.
    pub refused: usize,
    /// Sibling rows withheld: not opened or verified here, off this scope, or
    /// by a writer that is not a verified member.
    pub unvouched: usize,
    /// Sibling rows withheld as covered.
    pub covered: usize,
    /// Delegable scope only: rows withheld, and forgotten, as below a cover
    /// the listing holds — the cover step's own predicate, whoever wrote them.
    pub below_cover: usize,
    /// Rows withheld as dead — the reclamation pass's own predicate, whoever
    /// wrote them ([`crate::generation_reclaim::ReclaimState::relay_row_dead`]).
    pub dead: usize,
    /// The nest refused a push `scope_full`. On the fleet scope the diff
    /// stopped there; on the delegable scope it went on without the pushes
    /// that need room ([`Self::withheld_for_room`]).
    pub scope_full: bool,
    /// Delegable scope only: pushes withheld after the pass's first
    /// `scope_full`, each a row whose pair the listing lacks and that names
    /// no listed row ([`needs_room`]; `account-sync-plane.md` § The bind leg,
    /// ruling 1(d)).
    pub withheld_for_room: usize,
    /// Delegable scope only: rows withheld as a departed scope's items, whoever
    /// wrote them (`delegable-scope-reclamation.md` § Delegable-scope
    /// reclamation, part (6); [`crate::departure::departed_scopes`]). The
    /// relay copy stays.
    pub departed: usize,
}

impl DiffPush {
    /// Nothing was sent — the steady state of a replica the nest already
    /// holds whole.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.pushed == 0 && self.refused == 0 && !self.scope_full
    }
}

/// A row the nest refused for good, by its coordinates: writer, item key,
/// `writer_seq`.
pub type RefusedRow = (WriterId, [u8; 32], u64);

/// Run the diff for `plane` against the listing its last reconcile banked.
/// Module docs own the rules.
pub async fn publish_diff<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
) -> Result<DiffPush>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    Ok(
        publish_diff_naming_refusals(store, plane, trust, writer_key)
            .await?
            .0,
    )
}

/// [`publish_diff`], also naming each row the nest refused for good
/// ([`DiffPush::refused`] counts them). A refusal says that nest held the row
/// and has retired it, or holds a newer word in its cell — what the secondary
/// leg's removed-device arm reads as *carried*
/// (`crate::linked_leg::retire_carried_evidence`).
pub async fn publish_diff_naming_refusals<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
) -> Result<(DiffPush, Vec<RefusedRow>)>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let mut report = DiffPush::default();
    let mut refused: Vec<RefusedRow> = Vec::new();
    let Some(listing) = plane.listing() else {
        return Ok((report, refused));
    };
    report.listed = true;
    let me = store.writer();
    // Our published high-water: an own row above it is `publish_pending`'s,
    // and so is an own row it holds parked, which it owes by name
    // (`account-replica-posture.md` § The store device principal, refinement
    // 11 → *A row refused for room is parked*).
    let published = store
        .frontier(plane.scope())
        .await?
        .into_iter()
        .find(|(w, _)| *w == me)
        .map(|(_, seq)| seq);
    let parked = store.parked(plane.scope(), &me).await?;
    // Ruling 1(d): on the delegable scope a refusal for room withholds only
    // the pushes that need room; the fleet scope stops at the first one.
    let skips_for_room = plane.scope() == ACCOUNT_STATE_SCOPE;
    let mut missing: Vec<RelayRow> = store
        .relay_rows(
            plane.scope(),
            ItemClass::StateEntry.as_wire(),
            &[],
            u32::MAX,
        )
        .await?
        .into_iter()
        .filter(|row| {
            let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
                return false;
            };
            row.entry.is_some()
                && listing
                    .get(&(row.writer, item_key))
                    .is_none_or(|held| *held < row.writer_seq)
        })
        .collect();
    if missing.is_empty() {
        return Ok((report, refused));
    }
    // In each writer's own order, so a nest's per-item seq check meets them
    // as their writer sent them.
    missing.sort_by_key(|row| (row.writer, row.writer_seq));
    let departed = plane.departed_scopes().await?;

    // Built once, and only when there is something to judge.
    let mut vouch: Option<Vouch> = None;
    for row in missing {
        crate::pass_breath::pass_breath().await;
        if row.writer == me
            && (published.is_none_or(|p| row.writer_seq > p) || parked.contains(&row.writer_seq))
        {
            continue;
        }
        if !departed.is_empty()
            && let Some(opened) = plane.open_relay_row(&row).await?
            && crate::departure::of_departed_scope(&departed, &opened.kind, &opened.key)
        {
            report.departed += 1;
            continue;
        }
        let vouch = match &mut vouch {
            Some(v) => v,
            None => vouch.insert(Vouch::read(store, plane, trust, writer_key).await?),
        };
        if skips_for_room
            && crate::delegable_reclaim::listed_cover_above(store, plane, vouch.state.view(), &row)
                .await?
        {
            report.below_cover += 1;
            // A linked nest's cover says nothing about the bound nest's: the
            // copy stays for the bound plane's own judgement.
            if !plane.is_linked() {
                plane.relay_forget(&row.writer, &row.item_key).await?;
            }
            continue;
        }
        let names = crate::delegable_reclaim::rows_a_push_names(plane, &row).await?;
        if report.scope_full {
            // Only reached on the delegable scope: the fleet scope broke out
            // at the refusal.
            let named: Vec<RefusedRow> = names
                .iter()
                .filter_map(|n| {
                    <[u8; 32]>::try_from(n.item_key.as_slice())
                        .ok()
                        .map(|item| (n.writer, item, n.writer_seq))
                })
                .collect();
            if needs_room(&listing, &row, &named) {
                report.withheld_for_room += 1;
                continue;
            }
        }
        match vouch.judge(store, plane, &row).await? {
            Verdict::Push => {}
            Verdict::Unvouched => {
                report.unvouched += 1;
                continue;
            }
            Verdict::Dead => {
                report.dead += 1;
                continue;
            }
            Verdict::Covered => {
                report.covered += 1;
                continue;
            }
        }
        match plane.push_verbatim(&row, &names).await? {
            Pushed::Acked => report.pushed += 1,
            Pushed::RefusedForGood => {
                report.refused += 1;
                if let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) {
                    refused.push((row.writer, item_key, row.writer_seq));
                }
            }
            Pushed::ScopeFull => {
                report.scope_full = true;
                if !skips_for_room {
                    break;
                }
            }
        }
    }
    Ok((report, refused))
}

/// Does a push of `row` need room at the nest — the pushes ruling 1(d)
/// withholds on the delegable scope after the pass's first refusal for room?
/// A put needs room only when it adds a live pair: `listing` lacks the row's
/// `(writer, item)` pair (the nest counts its cap only then), and the put
/// names no row `listing` holds through `replaces` (delegable-scope
/// reclamation, part (2): each named row the put supersedes frees one).
/// `replaces` is the row's named rows as `(writer, item key, writer_seq)`.
pub fn needs_room(listing: &Listing, row: &RelayRow, replaces: &[RefusedRow]) -> bool {
    let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
        return false;
    };
    !listing.contains_key(&(row.writer, item_key))
        && !replaces
            .iter()
            .any(|(writer, item, seq)| listing.get(&(*writer, *item)) == Some(seq))
}

/// Ruling 7(b)'s exception: is `row`, opened to `plaintext`, its writer's own
/// removal row — in that writer's own device-set cell, reading `Removed` by
/// that writer?
fn own_removal_row(row: &RelayRow, plaintext: &EntryPlaintext) -> bool {
    plaintext.kind == KIND_DEVICE_SET
        && plaintext.key == fauna_core::hex32::encode(&row.writer.0)
        && matches!(
            fauna_core::encoding::canonical_decode::<DeviceSetRecord>(&plaintext.value),
            Ok(DeviceSetRecord::Removed { removed_by, .. }) if removed_by == row.writer.0
        )
}

enum Verdict {
    Push,
    Unvouched,
    Dead,
    Covered,
}

/// What a row is judged against: the reclamation pass's own reading of
/// merged state (the verified fleet view, and the dead predicate), and the
/// tip key the covered predicate names rows by — resolved on first need, as
/// it can cost a KEM.
struct Vouch<'a> {
    me: WriterId,
    trust: &'a GenerationTrust,
    writer_key: &'a SigningKey,
    state: ReclaimState,
    /// Is this the scope the reclamation pass forgets rows of — the only one
    /// whose rows the dead predicate may skip, so the skipped set stays the
    /// forgotten set?
    reclaims: bool,
    /// `None` until first asked; then the tip's id and key, when a tip
    /// resolves here and this device keys it.
    tip: Option<Option<([u8; 32], fauna_core::crypto::GenerationKey)>>,
}

impl<'a> Vouch<'a> {
    async fn read<B, R>(
        store: &AccountStore<B>,
        plane: &AccountStatePlane<'_, B, R>,
        trust: &'a GenerationTrust,
        writer_key: &'a SigningKey,
    ) -> Result<Self>
    where
        B: StoreBackend,
        R: RpcRequester + Clone,
    {
        Ok(Self {
            me: store.writer(),
            trust,
            writer_key,
            state: ReclaimState::read(store, trust, writer_key.verifying_key().to_bytes()).await?,
            reclaims: plane.scope() == ACCOUNT_STATE_FLEET_SCOPE,
            tip: None,
        })
    }

    async fn judge<B, R>(
        &mut self,
        store: &AccountStore<B>,
        plane: &AccountStatePlane<'_, B, R>,
        row: &RelayRow,
    ) -> Result<Verdict>
    where
        B: StoreBackend,
        R: RpcRequester + Clone,
        R::Error: RpcErrorClass,
    {
        let own = row.writer == self.me;
        let member = own || self.state.view().is_verified_member(&row.writer.0);
        let plaintext = plane.open_relay_row(row).await?;
        // A writer that is not a verified member has one row that goes: its
        // own removal row (ruling 7(b), module docs).
        if !member
            && !(self.state.view().is_excluded(&row.writer.0)
                && plaintext.as_ref().is_some_and(|p| own_removal_row(row, p)))
        {
            return Ok(Verdict::Unvouched);
        }
        // A dead row is never pushed, whoever wrote it (ruling 1(b)) — and one
        // sealed under a shredded generation is dead unopened.
        if self.reclaims
            && self
                .state
                .relay_row_dead(store, row, plaintext.as_ref())
                .await?
        {
            return Ok(Verdict::Dead);
        }
        if own {
            // Our own row otherwise always goes (ruling 1(a)).
            return Ok(Verdict::Push);
        }
        let Some(plaintext) = plaintext else {
            return Ok(Verdict::Unvouched);
        };
        if home_scope_for_kind(&plaintext.kind).is_some_and(|home| home != plane.scope()) {
            return Ok(Verdict::Unvouched);
        }
        let Some(sealed_under) = row
            .entry
            .as_deref()
            .and_then(fauna_core::account_entry_crypto::peek_generation_id)
        else {
            return Ok(Verdict::Push);
        };
        self.resolve_tip(store, plane).await?;
        if let Some(Some((tip_id, tip_key))) = &self.tip
            && sealed_under != *tip_id
            && crate::generation_reclaim::covered_at_tip(
                store,
                plane,
                self.state.view(),
                tip_key,
                &plaintext,
            )
            .await?
        {
            return Ok(Verdict::Covered);
        }
        Ok(Verdict::Push)
    }

    /// Fill [`Self::tip`] on first need.
    async fn resolve_tip<B, R>(
        &mut self,
        store: &AccountStore<B>,
        plane: &AccountStatePlane<'_, B, R>,
    ) -> Result<()>
    where
        B: StoreBackend,
        R: RpcRequester + Clone,
    {
        if self.tip.is_none() {
            let custody = plane.generation_custody();
            let resolution =
                generation_tip::resolve_tip(store, self.trust, self.writer_key, custody).await?;
            self.tip = Some(match resolution.tip.as_ref() {
                Some(t) => {
                    match generation_tip::key_for_tip(store, t, self.writer_key, custody).await {
                        Ok(key) => Some((t.generation_id, key)),
                        // No tip key: nothing is judged covered, so the row
                        // goes — the conservative side, since a nest missing
                        // a live row is the failure this step exists for.
                        Err(_) => None,
                    }
                }
                None => None,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_state_plane::{ItemId, Listing};
    use crate::generation_fixture_test_support::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::{StateEntry, WriterId};
    use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
    use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
    use fauna_core::generation::{reach_cell_key, sign_device_reach, wrap_cell_key_per_healer};
    use fauna_protocol::account_state::{
        ACCOUNT_STATE_FLEET_SCOPE, AccountStatePutReply, AccountStatePutRequest, KIND_STATE_PUT,
    };
    use fauna_protocol::error::RpcError;
    use fauna_protocol::merge_policy::{
        KIND_DEVICE_REACH, KIND_GENERATION_MINT, KIND_GENERATION_WRAP, LwwStamp, kind_keys,
    };
    use std::sync::{Arc, Mutex};

    /// A nest that records every put and answers each with `verdict`: stored
    /// (`None`) or refused under that code. Its feed is empty — the tests
    /// state the listing directly.
    #[derive(Clone, Default)]
    struct PutNest {
        puts: Arc<Mutex<Vec<AccountStatePutRequest>>>,
        verdict: Arc<Mutex<Option<&'static str>>>,
        /// Every put that arrived, stored or refused.
        attempts: Arc<Mutex<usize>>,
        /// When set, a full scope with room only for these item keys: a put
        /// of any other item is refused `scope_full`.
        room_for: Arc<Mutex<Option<Vec<Vec<u8>>>>>,
    }

    impl PutNest {
        fn puts(&self) -> Vec<AccountStatePutRequest> {
            self.puts.lock().unwrap().clone()
        }
        fn refuse(&self, code: &'static str) {
            *self.verdict.lock().unwrap() = Some(code);
        }
        fn attempts(&self) -> usize {
            *self.attempts.lock().unwrap()
        }
    }

    #[derive(Debug)]
    struct PutNestErr(RpcError);
    impl std::fmt::Display for PutNestErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0.code)
        }
    }
    impl RpcErrorClass for PutNestErr {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    impl RpcRequester for PutNest {
        type Error = PutNestErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, PutNestErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_core::encoding::canonical_encode(&payload).unwrap();
            let reply = match kind {
                KIND_STATE_PUT => {
                    *self.attempts.lock().unwrap() += 1;
                    if let Some(code) = *self.verdict.lock().unwrap() {
                        return Err(PutNestErr(RpcError::new(code, "refused")));
                    }
                    let put: AccountStatePutRequest =
                        fauna_core::encoding::canonical_decode(&bytes).unwrap();
                    if self
                        .room_for
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|room| !room.contains(&put.item_key.to_vec()))
                    {
                        return Err(PutNestErr(RpcError::new(
                            "fauna.account.state.scope_full",
                            "scope_full",
                        )));
                    }
                    self.puts
                        .lock()
                        .unwrap()
                        .push(fauna_core::encoding::canonical_decode(&bytes).unwrap());
                    fauna_core::encoding::canonical_encode(&AccountStatePutReply {
                        seq: 1,
                        ..Default::default()
                    })
                    .unwrap()
                }
                "fauna.sync.changes.list" => fauna_core::encoding::canonical_encode(
                    &fauna_protocol::sync::SyncChangesListReply::default(),
                )
                .unwrap(),
                other => return Err(PutNestErr(RpcError::new("kind_not_served", other))),
            };
            Ok(fauna_core::encoding::canonical_decode(&reply).unwrap())
        }
    }

    fn plane<'a>(
        f: &'a Fixture,
        nest: &'a PutNest,
    ) -> AccountStatePlane<'a, SqliteBackend, PutNest> {
        AccountStatePlane::new(
            &f.store,
            nest,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap()
    }

    /// `seed`'s reach row as a gen-0 fleet-scope entry.
    fn reach(seed: [u8; 32], at_ms: i64) -> StateEntry {
        let key = device_key(seed);
        machinery_row(
            KIND_DEVICE_REACH,
            reach_cell_key(&key.verifying_key().to_bytes()),
            &sign_device_reach(&key, at_ms, Vec::new()),
        )
    }

    /// Stage `row` as `seed`'s verbatim relay row at `seq`, sealed under
    /// `schedule` — as a walk (or a peer) would have recorded it.
    async fn relayed(
        f: &Fixture,
        schedule: &AccountStateKeySchedule,
        seed: [u8; 32],
        seq: u64,
        row: &StateEntry,
        tombstone: bool,
    ) -> RelayRow {
        let writer_key = device_key(seed);
        let sealed = seal_entry(
            &kind_keys(schedule, &row.kind).unwrap(),
            &EntryCoordinates {
                writer_id: writer_key.verifying_key().to_bytes(),
                writer_seq: seq,
                scope: ACCOUNT_STATE_FLEET_SCOPE,
            },
            &EntryPlaintext {
                kind: row.kind.clone(),
                key: row.key.clone(),
                merge_meta: row.merge_meta.clone().map(Into::into),
                value: row.value.clone().into(),
                tombstone,
            },
            &writer_key,
        )
        .unwrap();
        let relay = RelayRow {
            scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
            item_class: "state-entry".into(),
            writer: WriterId(writer_key.verifying_key().to_bytes()),
            writer_seq: seq,
            item_key: sealed.item_key.to_vec(),
            op: if tombstone { "tombstone" } else { "state-put" }.into(),
            entry: Some(sealed.envelope),
            feed_seq: None,
        };
        f.store.record_relay_row(&relay).await.unwrap();
        relay
    }

    fn key_of(row: &RelayRow) -> [u8; 32] {
        row.item_key.as_slice().try_into().unwrap()
    }

    fn pushed_as(put: &AccountStatePutRequest, row: &RelayRow) -> bool {
        put.writer_id == row.writer.to_hex()
            && put.writer_seq == row.writer_seq as i64
            && put.item_key.as_ref() == row.item_key.as_slice()
            && put.op == row.op
            && Some(put.entry.as_ref()) == row.entry.as_deref()
    }

    async fn relay_copy_held(f: &Fixture, row: &RelayRow) -> bool {
        f.store
            .relay_rows_at(ACCOUNT_STATE_FLEET_SCOPE, "state-entry", &key_of(row))
            .await
            .unwrap()
            .iter()
            .any(|r| r.writer == row.writer)
    }

    /// Ruling 1: this device's own published row and a verified sibling's row
    /// that the nest's listing lacks go to the nest verbatim — stored bytes,
    /// original writer and seq — and a row the listing already holds at its
    /// seq is left alone. No listing, no diff.
    #[tokio::test]
    async fn own_and_verified_sibling_rows_the_listing_lacks_are_pushed_verbatim() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let nest = PutNest::default();
        let p = plane(&f, &nest);
        // Our own row, published through the writer door: in the relay plane,
        // at our published high-water.
        let me = device_id_of(US);
        let at_ms = 9_000;
        p.put(
            &ItemId {
                kind: KIND_DEVICE_REACH.into(),
                key: reach_cell_key(&me),
            },
            fauna_core::encoding::canonical_encode(&sign_device_reach(
                &f.writer_key,
                at_ms,
                Vec::new(),
            ))
            .unwrap(),
            Some(LwwStamp { at_ms, writer: me }.encode().unwrap()),
        )
        .await
        .unwrap();
        // Every row of ours the door published — the fixture's merged rows
        // were journaled as ours too, and the ordered publish sent them.
        let own = f
            .store
            .relay_rows_of_writer(ACCOUNT_STATE_FLEET_SCOPE, "state-entry", &WriterId(me))
            .await
            .unwrap();
        assert!(!own.is_empty());
        let theirs = relayed(&f, &f.schedule, THEM, 4, &reach(THEM, 8_000), false).await;
        let held = relayed(&f, &f.schedule, THEM, 2, &enrollment_row(THEM), false).await;
        nest.puts.lock().unwrap().clear();

        assert_eq!(
            publish_diff(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            DiffPush::default(),
            "no completed reconcile: nothing to diff against"
        );
        assert!(nest.puts().is_empty());

        p.set_listing(Some(Listing::from([(
            (held.writer, key_of(&held)),
            held.writer_seq,
        )])));
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(report.pushed, own.len() + 1, "{report:?}");
        let puts = nest.puts();
        assert!(
            own.iter()
                .all(|row| puts.iter().any(|put| pushed_as(put, row))),
            "our own rows, verbatim"
        );
        assert!(
            puts.iter().any(|put| pushed_as(put, &theirs)),
            "the sibling's row, verbatim"
        );
        assert!(
            !puts.iter().any(|put| pushed_as(put, &held)),
            "the listed row stays"
        );
        assert!(
            p.listed_at_or_above(&theirs.writer, &key_of(&theirs), theirs.writer_seq),
            "an acked push is published as this pass saw it"
        );

        // Steady state: the nest now holds everything, and nothing moves.
        nest.puts.lock().unwrap().clear();
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(report.is_quiet() && nest.puts().is_empty(), "{report:?}");
    }

    /// A newer tombstone follows an older live row an out-of-date replica
    /// pushed first: the listing's lower seq for the pair is a miss.
    #[tokio::test]
    async fn a_newer_tombstone_is_pushed_over_an_older_listed_row() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let nest = PutNest::default();
        let p = plane(&f, &nest);
        let gone = relayed(&f, &f.schedule, THEM, 5, &reach(THEM, 8_000), true).await;
        p.set_listing(Some(Listing::from([((gone.writer, key_of(&gone)), 3)])));
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(report.pushed, 1, "{report:?}");
        assert!(pushed_as(&nest.puts()[0], &gone));
        assert_eq!(nest.puts()[0].op, "tombstone");
    }

    /// Ruling 1(a): a row this device cannot open, and a row whose writer is
    /// not a verified member, are never pushed — a hostile nest must not be
    /// able to use this device to plant rows on another.
    #[tokio::test]
    async fn an_unopenable_row_and_a_non_members_row_are_never_pushed() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let nest = PutNest::default();
        let p = plane(&f, &nest);
        let foreign = AccountStateKeySchedule::derive(&BackupKey::derive(&[0x13; 32]));
        relayed(&f, &foreign, THEM, 1, &reach(THEM, 8_000), false).await;
        let stranger = [0xC4; 32];
        relayed(&f, &f.schedule, stranger, 1, &reach(stranger, 8_000), false).await;
        p.set_listing(Some(Listing::new()));
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(report.pushed, 0, "{report:?}");
        assert_eq!(report.unvouched, 2, "{report:?}");
        assert!(nest.puts().is_empty());
    }

    /// The carry's leg rule (`succession-aftermath.md` § Re-key scope → *Which
    /// walks carry*): a predecessor-sealed relay row is never vouched, **also
    /// on a plane that holds the predecessor schedules** — the bound nest's
    /// walk opens such rows, the publish diff never does, so a device never
    /// pushes one onward. The plane is the delegable scope's (the row's home
    /// scope), so the home-scope test cannot be what keeps the row back; only
    /// `open_relay_row`'s own-keys-only open can. A fallback to
    /// `trial_open_inherited` there turns this red.
    #[tokio::test]
    async fn a_predecessor_sealed_relay_row_is_never_vouched_on_a_plane_that_holds_its_schedule() {
        use fauna_core::crypto::DelegableSchedule;
        use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
        use fauna_protocol::merge_policy::{KIND_MODERATION, MODERATION_KEY};

        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let nest = PutNest::default();
        let predecessor = BackupKey::derive(&[0x27; 32]);
        let predecessors = [DelegableSchedule::derive(&predecessor)];
        let p = AccountStatePlane::new(
            &f.store,
            &nest,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_SCOPE,
        )
        .unwrap()
        .with_predecessor_schedules(&predecessors);

        // THEM (a verified member) wrote a moderation row sealed under the
        // predecessor's delegable schedule.
        let writer_key = device_key(THEM);
        let sealed = seal_entry(
            &predecessors[0].for_kind(KIND_MODERATION).into_keys(),
            &EntryCoordinates {
                writer_id: writer_key.verifying_key().to_bytes(),
                writer_seq: 1,
                scope: ACCOUNT_STATE_SCOPE,
            },
            &EntryPlaintext {
                kind: KIND_MODERATION.into(),
                key: MODERATION_KEY.into(),
                merge_meta: None,
                value: vec![0xA5].into(),
                tombstone: false,
            },
            &writer_key,
        )
        .unwrap();
        f.store
            .record_relay_row(&RelayRow {
                scope: ACCOUNT_STATE_SCOPE.into(),
                item_class: "state-entry".into(),
                writer: WriterId(writer_key.verifying_key().to_bytes()),
                writer_seq: 1,
                item_key: sealed.item_key.to_vec(),
                op: "state-put".into(),
                entry: Some(sealed.envelope),
                feed_seq: None,
            })
            .await
            .unwrap();

        p.set_listing(Some(Listing::new()));
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(report.unvouched, 1, "{report:?}");
        assert_eq!(report.pushed, 0, "{report:?}");
        assert!(nest.puts().is_empty());
    }

    /// Ruling 7(b), the one exception to the member test: a departed device's
    /// own removal row — its own device-set cell, `Removed` by itself — is
    /// pushed although its writer is no member. Nothing else of it goes: not
    /// a `Removed` row it wrote for another device, not its reach. And a push
    /// the nest refuses for good is named by its coordinates.
    #[tokio::test]
    async fn a_departed_devices_own_removal_row_is_pushed_and_nothing_else_of_it() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        let them = device_id_of(THEM);
        let removed_by_them = |device: [u8; 32]| {
            machinery_row(
                KIND_DEVICE_SET,
                fauna_core::hex32::encode(&device),
                &DeviceSetRecord::Removed {
                    removed_at_ms: 9_000,
                    removed_by: them,
                },
            )
        };
        // THEM signed out: merged state reads it removed.
        f.put(removed_by_them(them)).await;
        let nest = PutNest::default();
        let p = plane(&f, &nest);
        let own_removal = relayed(&f, &f.schedule, THEM, 5, &removed_by_them(them), false).await;
        relayed(&f, &f.schedule, THEM, 4, &reach(THEM, 8_000), false).await;
        relayed(
            &f,
            &f.schedule,
            THEM,
            6,
            &removed_by_them(device_id_of([0xC4; 32])),
            false,
        )
        .await;

        p.set_listing(Some(Listing::new()));
        let (report, refused) = publish_diff_naming_refusals(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!((report.pushed, report.unvouched), (1, 2), "{report:?}");
        assert!(refused.is_empty());
        let puts = nest.puts();
        assert!(
            puts.len() == 1 && pushed_as(&puts[0], &own_removal),
            "the departed device's own removal row, verbatim, and nothing else of it"
        );

        // A nest that held the row and has retired it refuses the push for
        // good, and the diff names the row.
        p.set_listing(Some(Listing::new()));
        nest.refuse(RpcError::CODE_ACCOUNT_STATE_STALE_WRITER_SEQ);
        let (report, refused) = publish_diff_naming_refusals(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(report.refused, 1, "{report:?}");
        assert_eq!(
            refused,
            vec![(own_removal.writer, key_of(&own_removal), 5)],
            "the refused row, by its coordinates"
        );
        assert!(!relay_copy_held(&f, &own_removal).await);
    }

    /// `seed`'s reach row listing `holds`, as a gen-0 fleet-scope entry.
    fn reach_holding(seed: [u8; 32], at_ms: i64, holds: Vec<[u8; 32]>) -> StateEntry {
        let key = device_key(seed);
        machinery_row(
            KIND_DEVICE_REACH,
            reach_cell_key(&key.verifying_key().to_bytes()),
            &sign_device_reach(&key, at_ms, holds),
        )
    }

    /// Ruling 1(b): a row the reclamation pass forgets as dead from this
    /// replica's merged state alone is never pushed, whoever wrote it — here
    /// a wrap cell whose target's reach already lists the generation
    /// (`account-data-taxonomy.md` clause (3)(h), a dead gen-0 item's copies),
    /// a verified sibling's and this device's own. Both are counted `dead`;
    /// the pass's own forget takes them later in the same pass.
    #[tokio::test]
    async fn a_dead_wrap_cell_is_skipped_whoever_wrote_it() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (g, _, _) = f.mint_over(&[member_of(US)]).await;
        // Both targets say they hold the generation.
        f.put(reach_holding(US, 9_000, vec![g])).await;
        f.put(reach_holding(THEM, 9_000, vec![g])).await;
        let nest = PutNest::default();
        let p = plane(&f, &nest);
        // THEM's cell for US, walked in.
        let cell = machinery_row(
            KIND_GENERATION_WRAP,
            wrap_cell_key_per_healer(&g, &device_id_of(US), &device_id_of(THEM)),
            &"a wrap for US",
        );
        f.put(cell.clone()).await;
        let theirs = relayed(&f, &f.schedule, THEM, 3, &cell, false).await;
        // Our own cell for THEM, published through the door.
        let own_key = wrap_cell_key_per_healer(&g, &device_id_of(THEM), &device_id_of(US));
        p.put(
            &ItemId {
                kind: KIND_GENERATION_WRAP.into(),
                key: own_key.clone(),
            },
            fauna_core::encoding::canonical_encode(&"a wrap for THEM").unwrap(),
            None,
        )
        .await
        .unwrap();
        let ours = f
            .store
            .relay_rows_at(
                ACCOUNT_STATE_FLEET_SCOPE,
                "state-entry",
                &p.gen0_item_key(KIND_GENERATION_WRAP, &own_key).unwrap(),
            )
            .await
            .unwrap()
            .remove(0);
        nest.puts.lock().unwrap().clear();

        p.set_listing(Some(Listing::new()));
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        let puts = nest.puts();
        assert!(
            !puts.iter().any(|put| pushed_as(put, &theirs)),
            "the sibling's dead cell is not pushed: {report:?}"
        );
        assert!(
            !puts.iter().any(|put| pushed_as(put, &ours)),
            "nor our own: {report:?}"
        );
        assert_eq!(report.dead, 2, "{report:?}");
    }

    /// Ruling 1(c): a `Shredded` generation's own mint row is in neither arm
    /// of the dead set (`account-data-taxonomy.md` clause (3)(h)) — no pass
    /// forgets a sibling's copy of it, so the diff must keep sending it and
    /// learn the retirement from the refused put. Pinned so the dead
    /// predicate is never widened to it.
    #[tokio::test]
    async fn a_siblings_shredded_mint_row_is_still_pushed() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (g, _, core) = f.mint_over(&[member_of(US)]).await;
        let shred = machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&g),
            &fauna_core::generation::GenerationMintRecord::Shredded {
                core,
                shredded_at_ms: 9_000,
                shredded_by: device_id_of(THEM),
            },
        );
        f.put(shred.clone()).await;
        let nest = PutNest::default();
        let p = plane(&f, &nest);
        let theirs = relayed(&f, &f.schedule, THEM, 5, &shred, false).await;
        p.set_listing(Some(Listing::new()));
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(
            nest.puts().iter().any(|put| pushed_as(put, &theirs)),
            "{report:?}"
        );
        assert_eq!(report.dead, 0, "{report:?}");
    }

    /// Ruling 1(c): a refused push is final for that copy — the relay row is
    /// retired, so the next diff does not send it again. Ruling 1(d), the
    /// fleet scope: `scope_full` stops the diff for this pass — the row after
    /// it is not even asked — is reported, and leaves the copy.
    #[tokio::test]
    async fn a_refused_push_retires_its_copy_and_scope_full_stops_the_fleet_diff() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let nest = PutNest::default();
        let p = plane(&f, &nest);
        let theirs = relayed(&f, &f.schedule, THEM, 4, &reach(THEM, 8_000), false).await;
        let later = relayed(&f, &f.schedule, THEM, 6, &enrollment_row(THEM), false).await;
        p.set_listing(Some(Listing::new()));

        nest.refuse("fauna.account.state.scope_full");
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(report.scope_full && report.pushed == 0, "{report:?}");
        assert_eq!(
            (nest.attempts(), report.withheld_for_room),
            (1, 0),
            "the fleet scope stops at the first refusal for room: {report:?}"
        );
        assert!(
            relay_copy_held(&f, &theirs).await && relay_copy_held(&f, &later).await,
            "scope_full is no verdict on the row: the copy stays"
        );

        nest.refuse(RpcError::CODE_ACCOUNT_STATE_STALE_WRITER_SEQ);
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(report.refused, 2, "{report:?}");
        assert!(
            !relay_copy_held(&f, &theirs).await,
            "the refused copy is retired"
        );
        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(
            report,
            DiffPush {
                listed: true,
                ..Default::default()
            }
        );
    }

    /// Ruling 1(d), the delegable scope: after the pass's first refusal for
    /// room the diff withholds the pushes whose pair the listing lacks and
    /// still sends a row whose pair the listing holds at a lower seq — one
    /// refused put a pass, and nothing the nest has room for held behind it.
    /// An own row the publish holds parked is the publish's, never the
    /// diff's.
    #[tokio::test]
    async fn on_the_delegable_scope_scope_full_withholds_only_the_pushes_that_need_room() {
        use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
        use fauna_protocol::merge_policy::{
            KIND_DELEGATION, KIND_MODERATION, KIND_PERSONALIZATION, KIND_SYNC_PREFS, PREFERENCE_KEY,
        };
        let f = fixture().await;
        let nest = PutNest::default();
        let p = AccountStatePlane::new(
            &f.store,
            &nest,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_SCOPE,
        )
        .unwrap();
        let me = WriterId(device_id_of(US));
        let put = |kind: &'static str, at_ms: i64| {
            let p = &p;
            async move {
                p.put(
                    &ItemId {
                        kind: kind.into(),
                        key: PREFERENCE_KEY.into(),
                    },
                    vec![at_ms as u8],
                    Some(
                        LwwStamp {
                            at_ms,
                            writer: me.0,
                        }
                        .encode()
                        .unwrap(),
                    ),
                )
                .await
                .unwrap()
            }
        };
        let first_moderation = put(KIND_MODERATION, 1).await;
        let parked = put(KIND_PERSONALIZATION, 2).await;
        let refused = put(KIND_SYNC_PREFS, 3).await;
        let moderation = put(KIND_MODERATION, 4).await;
        let withheld = put(KIND_DELEGATION, 5).await;
        let own = f
            .store
            .relay_rows_of_writer(ACCOUNT_STATE_SCOPE, "state-entry", &me)
            .await
            .unwrap();
        let at = |seq: u64| own.iter().find(|r| r.writer_seq == seq).unwrap().clone();
        f.store
            .park(ACCOUNT_STATE_SCOPE, &me, parked)
            .await
            .unwrap();
        // The nest holds the first moderation row and nothing else of ours,
        // and has room only for the moderation pair it already counts.
        p.set_listing(Some(Listing::from([(
            (me, key_of(&at(moderation))),
            first_moderation,
        )])));
        *nest.room_for.lock().unwrap() = Some(vec![at(moderation).item_key.clone()]);
        *nest.attempts.lock().unwrap() = 0;
        nest.puts.lock().unwrap().clear();

        let report = publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(report.scope_full, "{report:?}");
        assert_eq!(
            (report.pushed, report.withheld_for_room, nest.attempts()),
            (1, 1, 2),
            "one refused put, the listed pair's newer row stored, the other new pair \
             withheld unasked: {report:?}"
        );
        let puts = nest.puts();
        assert!(pushed_as(&puts[0], &at(moderation)), "{puts:?}");
        assert!(
            relay_copy_held_in(&f, ACCOUNT_STATE_SCOPE, &at(refused)).await
                && relay_copy_held_in(&f, ACCOUNT_STATE_SCOPE, &at(withheld)).await,
            "a refusal for room is no verdict on the row: both copies stay"
        );
        assert_eq!(
            nest.attempts(),
            2,
            "the parked row was never sent by the diff (seq {parked})"
        );
    }

    /// Ruling 1(d)'s predicate, worded as the ruling words it: a push needs
    /// room when the listing lacks its pair AND it names no listed row. The
    /// names come from the put's `replaces` field (delegable-scope
    /// reclamation, part (2)), which no caller fills yet.
    #[test]
    fn a_push_needs_room_only_for_a_new_pair_that_names_no_listed_row() {
        let w = WriterId([7; 32]);
        let other = WriterId([8; 32]);
        let row = RelayRow {
            scope: "state".into(),
            item_class: "state-entry".into(),
            writer: w,
            writer_seq: 9,
            item_key: vec![1; 32],
            op: "state-put".into(),
            entry: Some(vec![0]),
            feed_seq: None,
        };
        let empty = Listing::new();
        assert!(needs_room(&empty, &row, &[]), "a new pair naming nothing");
        let held = Listing::from([((w, [1; 32]), 3)]);
        assert!(
            !needs_room(&held, &row, &[]),
            "the pair is held: the nest counts no room for it"
        );
        let listed_elsewhere = Listing::from([((other, [2; 32]), 5)]);
        assert!(
            !needs_room(&listed_elsewhere, &row, &[(other, [2; 32], 5)]),
            "a put that names a listed row frees that row's room"
        );
        assert!(
            needs_room(&listed_elsewhere, &row, &[(other, [2; 32], 4)]),
            "a name the listing holds at another seq frees nothing"
        );
        assert!(
            needs_room(&listed_elsewhere, &row, &[(other, [3; 32], 5)]),
            "a name the listing lacks frees nothing"
        );
    }

    /// The journal-driven publish beside the diff (`account-replica-posture.md`
    /// § The store device principal, refinement 11 → *A row refused for room
    /// is parked*): at a full scope the fleet plane still stops at its first
    /// `scope_full` and parks nothing, while the delegable plane parks each
    /// refused row and asks for the next. Red-verified by dropping the scope
    /// test from `parks_refused_for_room`.
    #[tokio::test]
    async fn the_fleet_plane_still_stops_at_scope_full_and_the_delegable_plane_parks() {
        use crate::account_state_plane::is_scope_full;
        use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
        use fauna_protocol::merge_policy::{KIND_DELEGATION, KIND_MODERATION, PREFERENCE_KEY};
        let f = fixture().await;
        let nest = PutNest::default();
        *nest.room_for.lock().unwrap() = Some(Vec::new());
        let me = device_id_of(US);
        let stamp = |at_ms: i64| Some(LwwStamp { at_ms, writer: me }.encode().unwrap());

        let fleet = plane(&f, &nest);
        for (cell, at_ms) in [(device_id_of(US), 9_000), (device_id_of(THEM), 9_001)] {
            fleet
                .put_local(
                    &ItemId {
                        kind: KIND_DEVICE_REACH.into(),
                        key: reach_cell_key(&cell),
                    },
                    fauna_core::encoding::canonical_encode(&sign_device_reach(
                        &f.writer_key,
                        at_ms,
                        Vec::new(),
                    ))
                    .unwrap(),
                    stamp(at_ms),
                )
                .await
                .unwrap();
        }
        let err = fleet.publish_pending().await.unwrap_err();
        assert!(is_scope_full(&err), "{err:#}");
        assert_eq!(nest.attempts(), 1, "the fleet publish stops at the refusal");
        assert_eq!(fleet.parked_count().await.unwrap(), 0);

        let delegable = AccountStatePlane::new(
            &f.store,
            &nest,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_SCOPE,
        )
        .unwrap();
        for (kind, at_ms) in [(KIND_MODERATION, 1), (KIND_DELEGATION, 2)] {
            delegable
                .put_local(
                    &ItemId {
                        kind: kind.into(),
                        key: PREFERENCE_KEY.into(),
                    },
                    vec![1],
                    stamp(at_ms),
                )
                .await
                .unwrap();
        }
        assert_eq!(delegable.publish_pending().await.unwrap(), 0);
        assert_eq!(
            (nest.attempts(), delegable.parked_count().await.unwrap()),
            (3, 2),
            "the delegable publish parks each refused row and asks for the next"
        );
        assert_eq!(
            delegable.publish_pending().await.unwrap(),
            0,
            "the next publish retries the parked rows"
        );
        assert_eq!(
            nest.attempts(),
            4,
            "and stops retrying at the first refused for room again"
        );
    }

    async fn relay_copy_held_in(f: &Fixture, scope: &str, row: &RelayRow) -> bool {
        f.store
            .relay_rows_at(scope, "state-entry", &key_of(row))
            .await
            .unwrap()
            .iter()
            .any(|r| r.writer == row.writer && r.writer_seq == row.writer_seq)
    }
}
