//! Gathers the client-held facts a devices-page removal resolves its target
//! from — the store half of `fauna_core::fleet_removal`, which owns the rule.
//!
//! Authority: `docs/goal/architecture/fleet-scope-reclamation.md`
//! § *Fleet-scope reclamation*, clause (4) (*The removal
//! target*). Everything read here is merged plane state this replica holds:
//! the verified fleet view over `fauna.state.device-set`, and each member's
//! own row statement on its `fauna.state.device-endpoints` entry
//! (`device_endpoints_writer` § The row statement). Nothing comes off the
//! nest's roster.

//!
//! # The completion rule
//!
//! The removal is two legs on two systems — `fauna.sync.devices.delete` at
//! the nest, the absorbing `Removed` row on the plane — and the nest deletion
//! is the **single decision point**. The intent
//! ([`PendingFleetRemoval`], in the credential slot) is staged before it and
//! settled by [`settle`]: `Gone` writes every `Removed` row then clears,
//! `Kept` clears without writing, `Unknown` leaves it for
//! [`complete_pending`] — the pump's once-per-full-pass reconcile, which asks
//! the roster: row absent → the deletion happened → `Gone`. A row still
//! present is **not** yet "it never happened": the page's deletion may be in
//! flight while a pass runs (this runtime serves the page's commands inside
//! its passes; a co-located agent passes on its own clock), so the pass only records its
//! sighting, and the intent is dropped unwritten — the user's row still there
//! to retry from — only once a later pass still finds the row
//! `fauna_core::fleet_removal::DELETION_IN_FLIGHT_BOUND_MS` after that
//! sighting ([`fauna_core::fleet_removal::reconcile_verdict`]). Every
//! intermediate crash lands in a state that reconcile finishes with no user
//! gesture (`nest/common.md` § Client-state recoverability).
//!
//! # Served inside a pass
//!
//! Every leg here is a local command of the account runtime
//! (`Cmd::is_local`, the fleet-removal verdict): the slot's staged intents
//! are persisted on every update, never held by a pass from its start to its
//! end, and the `Removed` row is the **local write only**
//! ([`AccountStatePlane::put_local`] — a `Gen0` machinery kind, so its door
//! never mints) that the runtime's publish step ships. With the network gone
//! from [`settle`], [`complete_pending`] has no yield point between its read
//! of the slot and its last write to it, so a page command lands wholly
//! before or after it; the pump publishes the fleet plane right after it
//! finishes an intent.
//!
//! So the page reports a removal done at its local write, before the
//! `Removed` row is on the plane — ruled acceptable. The
//! window is one publish step: the write arms it at once, and on the
//! quartet's own path [`settle`] writes the row only after the nest deletion
//! already refused the device. Within that window this replica's own
//! healer and mint no longer wrap to the device either — both re-check
//! removal at their write (`generation_topup::no_longer_wrap_targets`).
//! Awaiting the publish inside the command would put the network back into a
//! local command, the stall the Commands-and-passes ruling exists to end.

use anyhow::Result;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::device_endpoints::DeviceEndpointsEntry;
use fauna_core::fleet_removal::{FleetMembersView, RemovalFacts, UnaccountedMember};
use fauna_core::generation::{FleetView, GenerationClosedRecord, closed_cell_key, closure_set};
use fauna_protocol::merge_policy::{
    KIND_DEVICE_ENDPOINTS, KIND_DEVICE_SET, KIND_GENERATION_CLOSED, KIND_GENERATION_MINT,
};

use crate::generation_store::{live_rows, row_ref};
use crate::generation_tip::GenerationTrust;

// The completion leg (everything below `removal_facts`) drives the credential
// slot through the plane crate's slot seam (`principal_custody`), so it
// compiles for every host; the native slot behind it exists only under the
// engine's `account-runtime`, which is where its one caller — the account
// driver's pump — is wired.
use crate::account_state_plane::{AccountStatePlane, ItemId};
use crate::principal_custody::PrincipalCustody;
use anyhow::Context;
use ed25519_dalek::SigningKey;
use fauna_core::fleet_removal::{
    NestDeletion, PendingFleetRemoval, ReconcileVerdict, clear_pending, drop_if_unchanged,
    note_row_present, reconcile_verdict, stage_pending,
};
use fauna_core::generation::DeviceSetRecord;
use fauna_protocol::{RpcErrorClass, RpcRequester};

/// The facts as this replica holds them now. `me` is this device's fleet id;
/// `own_row` the registration latch's row half.
pub async fn removal_facts<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
    me: [u8; 32],
    own_row: Option<String>,
) -> Result<RemovalFacts> {
    let view = fleet_view(store, trust).await?;
    let mut facts = RemovalFacts {
        me,
        own_row,
        members: view.wrap_targets().map(|m| m.device_id).collect(),
        removed: view.removed().map(|(id, _)| *id).collect(),
        ..RemovalFacts::default()
    };
    // Every live entry's statement is gathered, a departed device's included
    // (nothing forgets a merged entry): the membership gate is the
    // resolver's, which reads a statement only at a verified member's cell
    // (`fauna_core::fleet_removal::resolve_removal_targets`; pinned by its
    // `a_non_members_statement_binds_nothing`), so a non-member's statement
    // can never aim or block a removal.
    for entry in live_rows(store, KIND_DEVICE_ENDPOINTS).await? {
        // Canonical-or-skip, the `FleetView::build` injectivity rule: a
        // statement counts only at the cell of the device making it.
        let Ok(id) = fauna_core::hex32::decode(&entry.key) else {
            continue;
        };
        if fauna_core::hex32::encode(&id) != entry.key {
            continue;
        }
        let Ok(value) =
            fauna_core::encoding::canonical_decode::<DeviceEndpointsEntry>(&entry.value)
        else {
            continue;
        };
        if let Some(row) = value.enrolled_row {
            facts.bindings.insert(id, row);
        }
    }
    Ok(facts)
}

/// The Devices page's read of the **member-addressed door** (clause (4), *A
/// disagreement is the user's to settle*): this device's own fleet id, and
/// every verified member other than it that no roster row accounts for —
/// `fauna_core::fleet_removal::unaccounted_members` run over the same facts
/// [`removal_facts`] gathers, with each listed member's asserted enrollment
/// instant read off its own verified `Enrolled` record. `roster` is every
/// nest row the page lists, as `(row id, claimed principal)`.
pub async fn unaccounted_members<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
    me: [u8; 32],
    own_row: Option<String>,
    roster: &[(String, Option<[u8; 32]>)],
) -> Result<FleetMembersView> {
    let facts = removal_facts(store, trust, me, own_row).await?;
    let view = fleet_view(store, trust).await?;
    let enrolled_at: std::collections::BTreeMap<[u8; 32], i64> = view
        .wrap_targets()
        .map(|m| (m.device_id, m.enrolled_at_ms))
        .collect();
    let unaccounted = fauna_core::fleet_removal::unaccounted_members(&facts, roster)
        .into_iter()
        .map(|device_id| UnaccountedMember {
            device_id,
            // Every unaccounted member is a wrap target, so the lookup
            // cannot miss; `0` rather than a panic keeps a reader honest.
            enrolled_at_ms: enrolled_at.get(&device_id).copied().unwrap_or(0),
        })
        .collect();
    Ok(FleetMembersView { me, unaccounted })
}

/// The fleet ids this replica's merged device-set state **excludes** — the
/// admission half's whole question, where [`removal_facts`] answers the
/// devices page's. One derivation, one owner: [`FleetView`] over the live
/// `fauna.state.device-set` rows, whose `Removed` exclusion is unconditional
/// (`fauna_core::generation`'s module ruling — attribution is advisory, so a
/// "stop trusting" signal is never refused on verification grounds).
///
/// Consumed by the peer leg's pump-refreshed removed-device snapshot
/// (`peer_leg::refresh_removed_devices`), which is what severs a removed
/// sibling's peer-plane admission — its fleet cert carries no expiry of its
/// own (`account-sync-plane.md` § The peer leg → *Validity and severance*).
/// Public so the peer-leg pins derive the set exactly as the pump does.
pub async fn removed_device_ids<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
) -> Result<std::collections::HashSet<[u8; 32]>> {
    let view = fleet_view(store, trust).await?;
    Ok(view.removed().map(|(id, _)| *id).collect())
}

/// Every fleet id this replica's verified view names — its members and its
/// removed ids, in id order, without duplicates. What the device running a
/// succession ceremony records as the predecessor's departed devices
/// (`succession-aftermath.md` § Re-key scope → *Predecessor devices are
/// departed devices*), for the successor to attest with
/// [`attest_departed_predecessor_devices`]. A row that verifies as nothing
/// names no id.
pub async fn fleet_device_ids<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
) -> Result<Vec<[u8; 32]>> {
    Ok(fleet_view_device_ids(&fleet_view(store, trust).await?))
}

/// [`fleet_device_ids`]' answer over a view already built — shared with the
/// runtime-free read of a closed store
/// (`departed_fleet::read_local_fleet_device_ids`), so both name the same ids.
pub fn fleet_view_device_ids(view: &FleetView) -> Vec<[u8; 32]> {
    let ids: std::collections::BTreeSet<[u8; 32]> = view
        .wrap_targets()
        .map(|m| m.device_id)
        .chain(view.removed().map(|(id, _)| *id))
        .collect();
    ids.into_iter().collect()
}

/// This replica's verified [`FleetView`] — [`FleetView::build`] over the
/// live `fauna.state.device-set` rows, against the account root alone. The
/// one read every consumer here takes, and the peer leg's dial pass besides:
/// a device-endpoints entry is a dial candidate only while its device is a
/// verified member of this view (`account-sync-plane.md` § The peer leg →
/// *Discovery*).
pub async fn fleet_view<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
) -> Result<FleetView> {
    let rows = live_rows(store, KIND_DEVICE_SET).await?;
    Ok(FleetView::build(&trust.root, rows.iter().map(row_ref)))
}

/// Write the plane's `Removed` row for `device_id`, attributed to this device
/// — locally; the caller owes the publish (module docs, *Served inside a
/// pass*) — behind the honest-writer check: never this device's own id (leaving is
/// sign-out's path), never an id this replica's fleet view does not verify;
/// an id already removed is done. `Removed` is absorbing and excludes
/// unconditionally at every reader, which is why the writer is strict.
///
/// **The closure goes first** (`account-data-taxonomy.md` § The generation
/// machinery → *The mint protocol, trigger (b)*): ahead of the `Removed` row
/// this journals one `fauna.state.generation-closed` row for every
/// generation the removed device may still key as a sealing candidate
/// ([`closure_set`] over this replica's merged mint rows), skipping a
/// generation merged state already holds a closed row for. So a removal on
/// record is never without its closures; a crash between the two costs one
/// mint, and the staged intent still writes the removal. Every remover path
/// reaches this one writer — the row gesture, the member-addressed door, and
/// [`complete_pending`]. A sign-out is not a removal by another device and
/// closes nothing (`generation_reclaim::sever_self` writes its own row).
pub async fn write_removed<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    device_id: [u8; 32],
) -> Result<()>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let me = writer_key.verifying_key().to_bytes();
    if device_id == me {
        anyhow::bail!(
            "refusing to remove this device from its own fleet — leaving is sign-out's path"
        );
    }
    let facts = removal_facts(store, trust, me, None).await?;
    if facts.removed.contains(&device_id) {
        return Ok(());
    }
    if !facts.members.contains(&device_id) {
        anyhow::bail!("refusing to remove an id that is not a verified fleet member");
    }
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    close_generations(store, fleet, trust, me, device_id, now_ms).await?;
    let value = fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
        removed_at_ms: now_ms,
        removed_by: me,
    })
    .context("encoding the fleet removal")?;
    fleet
        .put_local(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: fauna_core::hex32::encode(&device_id),
            },
            value,
            None,
        )
        .await
        .map(|_| ())
}

/// Attest a succession's **departed devices**: one `Removed` row, attributed
/// to this device, for every id in `departed` that this replica's merged
/// device-set state holds no row for — locally; the caller owes the publish,
/// as for [`write_removed`]. Returns how many rows it wrote.
///
/// `departed` is the predecessor's verified fleet view, members and removed
/// ids alike, as the device that ran the ceremony merged it and recorded it in
/// its account registry (`succession-aftermath.md` § Re-key scope →
/// *Predecessor devices are departed devices*). A `Removed` row excludes
/// unconditionally, so `ExtWriters::admits`' removed-device clause then
/// admits a predecessor device's inherited `ext:<kind>` rows — and nothing
/// else changes: a `Removed` id is no member, no wrap target and no tip
/// member, and the enrollment check stays root-only.
///
/// **A sibling of [`write_removed`], never a widening of it.** That writer
/// refuses an id its fleet view does not verify, and must stay strict; a
/// predecessor id is by construction not a member of the successor's view.
/// And no generation is closed: a predecessor device keys no successor
/// generation, so its closure set is empty by definition. An id merged state
/// already holds ANY row for is skipped — already removed, or (never for an
/// honest predecessor id, since a keypair is per (machine, account)) enrolled
/// in this fleet, which an attestation must not unseat. This device's own id
/// is skipped for the same reason. Idempotent: a second call writes nothing.
pub async fn attest_departed_predecessor_devices<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    writer_key: &SigningKey,
    departed: &[[u8; 32]],
) -> Result<usize>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let me = writer_key.verifying_key().to_bytes();
    let mut written = 0;
    for device_id in departed {
        if *device_id == me {
            continue;
        }
        let key = fauna_core::hex32::encode(device_id);
        if store
            .state(KIND_DEVICE_SET, &key)
            .await?
            .is_some_and(|held| !held.tombstone)
        {
            continue;
        }
        let value = fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
            removed_at_ms: fauna_core::data::Timestamp::now_millis_or_zero() as i64,
            removed_by: me,
        })
        .context("encoding a departed predecessor device")?;
        fleet
            .put_local(
                &ItemId {
                    kind: KIND_DEVICE_SET.into(),
                    key,
                },
                value,
                None,
            )
            .await
            .context("attesting a departed predecessor device")?;
        written += 1;
    }
    Ok(written)
}

/// [`write_removed`]'s closure half: journal a closed row for every generation
/// in the removal's closure set that merged state holds none for yet. Local
/// writes only, like the `Removed` row behind them — the kind is `Gen0`, so
/// its door never mints.
async fn close_generations<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    me: [u8; 32],
    removed: [u8; 32],
    now_ms: i64,
) -> Result<()>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let view = fleet_view(store, trust).await?;
    let mint_rows = live_rows(store, KIND_GENERATION_MINT).await?;
    let value = fauna_core::encoding::canonical_encode(&GenerationClosedRecord {
        closed_by: me,
        answers: removed,
        closed_at_ms: now_ms,
    })
    .context("encoding the generation closure")?;
    for generation in closure_set(&view, mint_rows.iter().map(row_ref), &removed) {
        let key = closed_cell_key(&generation);
        if store
            .state(KIND_GENERATION_CLOSED, &key)
            .await?
            .is_some_and(|held| !held.tombstone)
        {
            continue;
        }
        fleet
            .put_local(
                &ItemId {
                    kind: KIND_GENERATION_CLOSED.into(),
                    key,
                },
                value.clone(),
                None,
            )
            .await
            .context("closing a generation the removed device may key")?;
    }
    Ok(())
}

/// Stage the intent to remove `targets` with nest row `row` — BEFORE the nest
/// deletion. An error means nothing persisted and the gesture must not go on.
pub fn stage(slot: &dyn PrincipalCustody, row: &str, targets: &[[u8; 32]]) -> Result<()> {
    slot.update_pending_fleet_removals(&mut |pending| {
        stage_pending(
            pending,
            PendingFleetRemoval {
                row: row.to_string(),
                targets: targets.to_vec(),
            },
        );
        true
    })
    .map(|_| ())
}

/// Clear `row`'s intent, whatever it carries.
fn clear(slot: &dyn PrincipalCustody, row: &str) -> Result<()> {
    slot.update_pending_fleet_removals(&mut |pending| clear_pending(pending, row))
        .map(|_| ())
}

/// Settle `row`'s removal on what the nest deletion came to (module docs).
/// `targets` rides along so a settle still completes after a racing pass has
/// already dropped the staged copy (a deletion slower than the in-flight
/// bound): `Gone` re-stages before
/// it writes. On `Gone` the intent is cleared only once every `Removed` row is
/// journaled; a failure leaves it for [`complete_pending`] and is returned for
/// the page to show.
pub async fn settle<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    slot: &dyn PrincipalCustody,
    removal: &PendingFleetRemoval,
    outcome: NestDeletion,
) -> Result<()>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    match outcome {
        NestDeletion::Unknown => return Ok(()),
        NestDeletion::Kept => {}
        NestDeletion::Gone => {
            // Re-stage first: from here the rows MUST land, and the staged
            // copy is what finishes them if this call does not.
            stage(slot, &removal.row, &removal.targets)?;
            for target in &removal.targets {
                write_removed(store, fleet, trust, writer_key, *target).await?;
            }
        }
    }
    clear(slot, &removal.row)
}

/// What one reconcile pass did (the pump's `fleet_removals` report slot).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FleetRemovalPass {
    /// Intents finished: every `Removed` row journaled, the intent cleared.
    pub completed: usize,
    /// Intents dropped unwritten: the nest still held the row a whole
    /// in-flight bound after a pass first found it there, so no deletion was
    /// in flight and none happened.
    pub abandoned: usize,
    /// Intents left staged: the roster could not be read, a write failed, or
    /// the row is still present inside the in-flight bound (its deletion may
    /// yet land).
    pub waiting: usize,
}

/// The reconcile: finish every staged intent the roster can decide, by
/// `fauna_core::fleet_removal::reconcile_verdict`. `roster` is the nest's
/// current `sync_devices` row ids, `None` when it could not be read this pass
/// (offline — everything waits). The roster is the nest's word, and that is
/// sound here: the *targets* are client-verified and the user asked for the
/// removal; the nest only answers whether its own half happened. A sighting
/// is recorded and a drop carried out only on the intent the verdict judged —
/// a removal re-staged since this pass read the slot is a new flight.
pub async fn complete_pending<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    slot: &dyn PrincipalCustody,
    roster: Option<&[String]>,
) -> FleetRemovalPass
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let mut pass = FleetRemovalPass::default();
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero();
    for staged in slot.pending_fleet_removals() {
        let Some(roster) = roster else {
            pass.waiting += 1;
            continue;
        };
        let removal = &staged.removal;
        let held = roster.iter().any(|r| r.eq_ignore_ascii_case(&removal.row));
        match reconcile_verdict(held, staged.present_since_ms, now_ms) {
            ReconcileVerdict::Complete => {
                match settle(
                    store,
                    fleet,
                    trust,
                    writer_key,
                    slot,
                    removal,
                    NestDeletion::Gone,
                )
                .await
                {
                    Ok(()) => pass.completed += 1,
                    Err(e) => {
                        tracing::warn!(
                            "fleet removal: the staged removal of row {} is not finished \
                             ({e:#}) — retried next pass",
                            removal.row
                        );
                        pass.waiting += 1;
                    }
                }
            }
            ReconcileVerdict::Wait { stamp } => {
                if let Some(at_ms) = stamp {
                    let judged = staged.present_since_ms;
                    if let Err(e) = slot.update_pending_fleet_removals(&mut |pending| {
                        note_row_present(pending, &removal.row, judged, at_ms)
                    }) {
                        // Unrecorded, the next pass sights it afresh: a later
                        // drop, never an earlier one.
                        tracing::warn!(
                            "fleet removal: could not record the sighting of row {} ({e:#})",
                            removal.row
                        );
                    }
                }
                pass.waiting += 1;
            }
            ReconcileVerdict::Drop { since } => {
                match slot.update_pending_fleet_removals(&mut |pending| {
                    drop_if_unchanged(pending, &removal.row, since)
                }) {
                    Ok(true) => pass.abandoned += 1,
                    // Re-staged since this pass read the slot: a new flight.
                    Ok(false) => pass.waiting += 1,
                    Err(e) => {
                        tracing::warn!(
                            "fleet removal: could not drop the never-deleted row {} ({e:#})",
                            removal.row
                        );
                        pass.waiting += 1;
                    }
                }
            }
        }
    }
    pass
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::*;
    use fauna_account_store::types::ItemRef;
    use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;

    /// A third device, enrolled beside US and THEM.
    const THIRD: [u8; 32] = [0xC3u8; 32];
    /// A device that never enrolls.
    const STRANGER: [u8; 32] = [0xD4u8; 32];

    /// This device's journal in the fleet scope as `(kind, key)`, in journal
    /// order — what the publish step sends, in the order it sends it.
    async fn journal(f: &Fixture) -> Vec<(String, String)> {
        f.store
            .scope_rows(ACCOUNT_STATE_FLEET_SCOPE, &f.store.writer(), 0, 1_000)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|row| match row.item {
                ItemRef::StateKey { kind, key, .. } => Some((kind, key)),
                _ => None,
            })
            .collect()
    }

    /// No nest anywhere in this file: the writer is local-only, so a request
    /// reaching the wire is a defect. (The shared fixture's `NoNest` answers
    /// with an error type the removal writer's bound does not admit.)
    #[derive(Debug, Clone)]
    struct Offline;

    impl std::fmt::Display for Offline {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "no nest in this test")
        }
    }
    impl std::error::Error for Offline {}
    impl RpcErrorClass for Offline {
        fn is_rejection(&self) -> bool {
            false
        }
    }
    impl RpcRequester for Offline {
        type Error = Offline;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> std::result::Result<Reply, Offline>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err(Offline)
        }
    }

    async fn remove(f: &Fixture, seed: [u8; 32]) {
        let fleet = AccountStatePlane::new_pull_only(
            &f.store,
            &Offline,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .expect("fleet plane");
        write_removed(
            &f.store,
            &fleet,
            &f.trust,
            &f.writer_key,
            device_id_of(seed),
        )
        .await
        .expect("removed");
    }

    /// **Trigger (b), the writer.** The tip names US alone; THEM enrolled
    /// after it. Removing THEM journals a closed row for that generation
    /// *ahead of* the `Removed` row, so a removal on record is never without
    /// its closure. A generation merged state already holds a closed row for
    /// draws no second one.
    #[tokio::test]
    async fn a_removal_journals_its_closures_ahead_of_the_removed_row() {
        let f = fixture().await;
        for seed in [US, THEM, THIRD] {
            f.put(enrollment_row(seed)).await;
        }
        let (generation, _, _) = f.mint_over(&[member_of(US)]).await;
        let closed_key = closed_cell_key(&generation);
        let removed_key = fauna_core::hex32::encode(&device_id_of(THEM));
        let before = journal(&f).await.len();

        remove(&f, THEM).await;

        let written = journal(&f).await.split_off(before);
        assert_eq!(
            written,
            vec![
                (KIND_GENERATION_CLOSED.to_string(), closed_key.clone()),
                (KIND_DEVICE_SET.to_string(), removed_key),
            ],
            "the closure, then the removal"
        );
        let held = f
            .store
            .state(KIND_GENERATION_CLOSED, &closed_key)
            .await
            .unwrap()
            .expect("the closed row");
        let record: GenerationClosedRecord =
            fauna_core::encoding::canonical_decode(&held.value).unwrap();
        assert_eq!(
            (record.closed_by, record.answers),
            (device_id_of(US), device_id_of(THEM))
        );

        // A second removal finds the generation already closed.
        let before = journal(&f).await.len();
        remove(&f, THIRD).await;
        let written = journal(&f).await.split_off(before);
        assert!(
            written.len() == 1 && written[0].0 == KIND_DEVICE_SET,
            "an already-closed generation draws no second closed row: {written:?}"
        );
        // And removing an id already removed writes nothing at all.
        let before = journal(&f).await.len();
        remove(&f, THEM).await;
        assert_eq!(journal(&f).await.len(), before);
    }

    /// **The departed-device attestation** (`succession-aftermath.md`
    /// § Re-key scope → *Predecessor devices are departed devices*): one
    /// `Removed` row per attested id merged state holds nothing for,
    /// attributed to this device, and no closed row — a predecessor id keys
    /// no generation here. An id this fleet enrolls (THEM) and this device's
    /// own id are never unseated; a second call writes nothing. The
    /// attested id is then excluded, never a member and never a wrap target,
    /// and the honest-writer [`write_removed`] still refuses such an id.
    #[tokio::test]
    async fn attesting_departed_predecessor_devices_writes_one_removed_row_each_and_closes_nothing()
    {
        const PRED_A: [u8; 32] = [0xE5u8; 32];
        const PRED_B: [u8; 32] = [0xE6u8; 32];
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        f.mint_over(&[member_of(US), member_of(THEM)]).await;
        let fleet = AccountStatePlane::new_pull_only(
            &f.store,
            &Offline,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .expect("fleet plane");
        let (a, b, them, us) = (
            device_id_of(PRED_A),
            device_id_of(PRED_B),
            device_id_of(THEM),
            device_id_of(US),
        );
        let before = journal(&f).await.len();

        let written =
            attest_departed_predecessor_devices(&f.store, &fleet, &f.writer_key, &[a, them, us])
                .await
                .expect("attest");
        assert_eq!(written, 1, "only the predecessor id draws a row");
        let written = attest_departed_predecessor_devices(&f.store, &fleet, &f.writer_key, &[a, b])
            .await
            .expect("attest again");
        assert_eq!(written, 1, "the id merged state already holds is skipped");
        assert_eq!(
            journal(&f).await.split_off(before),
            vec![
                (KIND_DEVICE_SET.to_string(), fauna_core::hex32::encode(&a)),
                (KIND_DEVICE_SET.to_string(), fauna_core::hex32::encode(&b)),
            ],
            "two Removed rows, no generation-closed row"
        );
        assert!(
            live_rows(&f.store, KIND_GENERATION_CLOSED)
                .await
                .unwrap()
                .is_empty()
        );
        let held = f
            .store
            .state(KIND_DEVICE_SET, &fauna_core::hex32::encode(&a))
            .await
            .unwrap()
            .expect("the attestation row");
        let DeviceSetRecord::Removed { removed_by, .. } =
            fauna_core::encoding::canonical_decode(&held.value).unwrap()
        else {
            panic!("a Removed row");
        };
        assert_eq!(removed_by, us, "attributed to this device");
        assert_eq!(
            attest_departed_predecessor_devices(&f.store, &fleet, &f.writer_key, &[a, b])
                .await
                .expect("a third call"),
            0
        );

        let view = fleet_view(&f.store, &f.trust).await.unwrap();
        for id in [a, b] {
            assert!(view.is_excluded(&id));
            assert!(!view.is_verified_member(&id));
            assert!(view.wrap_targets().all(|m| m.device_id != id));
        }
        assert!(
            view.is_verified_member(&them),
            "an enrolled id stays seated"
        );
        let mut named = vec![us, them, a, b];
        named.sort_unstable();
        assert_eq!(
            fleet_device_ids(&f.store, &f.trust).await.unwrap(),
            named,
            "what a ceremony on this replica records: members and removed alike"
        );
        assert!(
            write_removed(
                &f.store,
                &fleet,
                &f.trust,
                &f.writer_key,
                device_id_of([0xE7u8; 32])
            )
            .await
            .is_err(),
            "the honest-writer check still refuses a non-member"
        );
    }

    /// The closure set at the writer: a mint naming a member this view does
    /// not verify — an invented mint — draws no closed row, and neither does
    /// one naming the removed device (the member rule already unseats it).
    #[tokio::test]
    async fn an_invented_mint_draws_no_closed_row() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        f.mint_over(&[member_of(US), member_of(STRANGER)]).await;
        f.mint_over(&[member_of(US), member_of(THEM)]).await;

        remove(&f, THEM).await;

        assert!(
            live_rows(&f.store, KIND_GENERATION_CLOSED)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
