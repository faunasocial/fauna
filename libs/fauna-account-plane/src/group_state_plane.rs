//! The group plane's class-2 client leg — the storage-group sibling of
//! [`crate::account_state_plane`] (the group plane's own W2.4 (account-data-plane.md § Workstreams)
//! equivalent).
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § Implementation
//! status today (the group-scope paragraph — what this module is the store
//! for), `key-material-hierarchy.md` § Audience: a storage group (the
//! machinery-root sealing ruling this module's seal path implements), and
//! `fauna_protocol::group_state` (the kind registry whose two columns gate
//! every write door here).
//!
//! # The account plane's shape, at one structural difference
//!
//! Everything here is [`crate::account_state_plane`]'s mechanics on the group
//! plane's own tables and keys: local-first writes journaled under the scope,
//! the sealed T14 entry envelope with the blinded item key, the relay plane
//! for onward serving, the frontier vector, and the walk's trial-open →
//! `apply_class2` → ingest chain. The one structural difference is the whole
//! design question the sealing ruling answered: entries seal under
//! [`GroupSealing::MachineryRoot`] — possession of the scope's birth-minted,
//! never-rotated root, which a joiner unwraps from its admission bundle —
//! rather than under the owner `BackupKey` the account applier derives its
//! schedule from. [`GroupSealing::GenerationTip`] is the second stratum and
//! arrives with group content-kind sealing: the seam is registered (every
//! door here dispatches on the sealing column) and its arm deliberately
//! unbuilt — a `GenerationTip` write refuses, a v2 envelope on the feed
//! counts [`WalkReport::unopened`].
//!
//! # Pull-only by construction
//!
//! The account plane's nest leg publishes through `fauna.account.state.put`;
//! **no such write kind exists for a group scope** — the group plane's feed
//! is the share serve set's relay pull (`p2p.md` § Cross-user shared-set
//! transfer; the transport wiring is that workstream's), and convergence is
//! pull-both-ways exactly like the account peer leg. So every plane here is
//! the peer-leg posture: a write lands locally (journal + entry + sealed
//! relay row) and our own frontier slot — the published high-water —
//! deliberately never advances. [`NoFeed`] is the standing requester for a
//! plane constructed only to write.
//!
//! # Why the store rests plaintext and the relay row rests sealed
//!
//! The same R3 (account-data-plane.md § The ratified decisions)/R7 split as the account plane: the reading replica's own
//! tables hold opened values (`group_entries`), while the wire/at-rest form —
//! what a peer pulls, what a custodian holds — is the sealed envelope in the
//! relay plane. Sealing happens at write, opening at ingest; a custodian
//! without the machinery root sees nothing past the custody floor.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use fauna_account_store::{
    backend::StoreBackend,
    store::{AccountStore, WriterRelation},
    types::{ItemRef, JournalOp, JournalRow, RelayRow, StateEntry},
};
use fauna_core::account_entry_crypto::{
    EntryCoordinates, EntryPlaintext, open_entry, peek_generation_id, seal_entry,
};
use fauna_core::crypto::{GroupMachineryRoot, GroupMachinerySchedule};
use fauna_core::group_ceremony::GroupPlaneRow;
use fauna_core::group_generation::{GroupHeldRootRecord, GroupReceptionKeyRecord};
use fauna_core::group_scope::decode_birth_for_scope;
use fauna_protocol::RpcRequester;
use fauna_protocol::account_state::{ItemClass, OP_STATE_PUT, OP_TOMBSTONE};
use fauna_protocol::group_state::{
    GroupSealing, KIND_GROUP_BIRTH, group_kinds, group_merge_policy, group_sealing,
};
use fauna_protocol::merge_policy::{
    KIND_GROUP_MACHINERY_ROOT, KIND_GROUP_RECEPTION_KEY, MergeOutcome,
};
use fauna_protocol::scope::GroupScope;
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};

use crate::account_state_plane::{
    AccountStatePlane, ItemId, NEST_SLOT_UNUSED, WalkReport, entry_to_plaintext, item_key_of,
    row_coordinates, seed_past_own_held, stored_frontier,
};

/// What one [`GroupStatePlane::adopt_rows`] pass did. Every row lands in
/// exactly one counter, so a driver can tell "already converged" from
/// "half the snapshot was refused".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdoptReport {
    /// Rows adopted verbatim (first contact, or replacing the local value).
    pub adopted: usize,
    /// Rows merged into a new local value.
    pub merged: usize,
    /// Rows whose value lost to the local one.
    pub kept: usize,
    /// Rows of a kind this build's group registry does not know — left alone,
    /// the plane's standing compat answer (a later build re-adopts them from
    /// the feed).
    pub unknown: usize,
    /// Rows `apply_class2`'s first-contact strictness refused. Never benign —
    /// the ceremony verifier refuses a snapshot that does not admit, so one
    /// here means a forged or corrupted row — but skip-not-abort, the walk's
    /// own discipline: one bad row must not strand the rest of the scope.
    pub refused: usize,
}

/// The class-2 leg of one group scope: write, adopt, walk, reconcile.
///
/// Constructed per use over borrowed parts, like the account plane; the
/// machinery-root schedule is derived once at construction. `rpc` is the
/// scope's pull feed — the share serve set once its transport is wired,
/// [`NoFeed`] for a plane constructed only to write locally.
pub struct GroupStatePlane<'a, B: StoreBackend, R: RpcRequester> {
    store: &'a AccountStore<B>,
    rpc: &'a R,
    schedule: GroupMachinerySchedule,
    /// The authoring device's signing key — the R13 in-seal writer signature,
    /// exactly the account plane's. Its public half must be the store's
    /// writer, refused at construction otherwise.
    writer_key: &'a SigningKey,
    /// The scope string every row here lands under —
    /// [`GroupScope`]'s `group:<scope-id-hex>` form.
    scope: String,
    /// The scope's content-derived id — what a birth row landing here must
    /// re-derive to ([`Self::unbound_birth`]).
    scope_id: [u8; 32],
}

impl<'a, B: StoreBackend, R: RpcRequester> GroupStatePlane<'a, B, R> {
    /// # Errors
    /// `writer_key`'s public half is not the store's writer id — every entry
    /// this plane sealed would then fail on every reader, so it is refused
    /// here rather than one write at a time.
    pub fn new(
        store: &'a AccountStore<B>,
        rpc: &'a R,
        root: &GroupMachineryRoot,
        writer_key: &'a SigningKey,
        scope_id: &[u8; 32],
    ) -> Result<Self> {
        if writer_key.verifying_key().to_bytes() != store.writer().0 {
            bail!(
                "group-state plane: the signing key is not this store's writer — every entry \
                 it sealed would fail the R13 writer-signature check on every reading replica"
            );
        }
        Ok(Self {
            store,
            rpc,
            schedule: GroupMachinerySchedule::derive(root),
            writer_key,
            scope: GroupScope::new(*scope_id).to_string(),
            scope_id: *scope_id,
        })
    }

    /// The scope string this plane serves (`group:<scope-id-hex>`).
    pub fn scope(&self) -> &str {
        &self.scope
    }

    // ── Write ───────────────────────────────────────────────────────────────

    /// Write one group-plane class-2 value: local first (journal + entry in
    /// one transaction), then the sealed relay row for onward serving.
    /// Returns the writer seq the entry landed on.
    ///
    /// No tombstone door exists here on purpose: every registered group kind
    /// merges under `Immutable` or `CrdtPerField`, neither of which admits
    /// one — deletion on this plane is an absorbing lattice phase (`Removed`,
    /// `Shredded`), written through this same door. The door lands with the
    /// first kind whose policy admits a tombstone, not before.
    pub async fn put(
        &self,
        item: &ItemId,
        value: Vec<u8>,
        merge_meta: Option<Vec<u8>>,
    ) -> Result<u64> {
        let Some(policy) = group_merge_policy(&item.kind) else {
            bail!(
                "kind {:?} is not on the group plane in this build — register it in \
                 fauna_protocol::group_state before writing entries under it",
                item.kind
            );
        };
        // A stamped policy's value is ordered only by its stamp — the account
        // door's preflight, kept though no registered group kind requires one
        // yet, so the first that does cannot ship stampless writes.
        if policy.requires_stamp() && merge_meta.is_none() {
            bail!(
                "kind {:?} merges under {:?}, which orders values by their LwwStamp — this \
                 write carries no merge_meta, so no reading replica could rank it",
                item.kind,
                policy
            );
        }
        // The sealing-stratum door lives in `write_own` — the one funnel every
        // write path reaches (a `put`-only door let
        // `adopt_rows` seal any kind under the machinery root).
        self.write_own(&EntryPlaintext {
            kind: item.kind.clone(),
            key: item.key.clone(),
            merge_meta: merge_meta.map(Into::into),
            value: value.into(),
            tombstone: false,
        })
        .await
    }

    /// Land one own-authored plaintext: the local write (journal + entry in
    /// one transaction), the seal under the machinery-root schedule, and the
    /// relay row. Our own frontier slot deliberately never advances — it is
    /// the published-to-a-feed high-water, and this plane has no publish leg
    /// (pull-only posture, module docs).
    ///
    /// **The sealing-stratum door lives HERE — the one funnel every write
    /// path reaches** (it used to live in [`Self::put`]
    /// alone, so [`Self::adopt_rows`] could drive any kind into the
    /// machinery-root seal below — latent while all registered group kinds
    /// are `MachineryRoot`, and exactly the silent stratum forfeit the first
    /// `GenerationTip` kind would have shipped). A refused row aborts the
    /// whole write — for `adopt_rows` that drops the snapshot, which is the
    /// deliberately conservative arm while no `GenerationTip` kind is
    /// registerable (nothing can stage a counted per-row refusal today);
    /// whoever lands the first `GenerationTip` kind owns adding the counted
    /// `report.refused` pre-check in `adopt_rows` beside its seal path.
    async fn write_own(&self, plaintext: &EntryPlaintext) -> Result<u64> {
        if let Some(why) = self.unbound_birth(plaintext) {
            bail!("group-state put: {why}");
        }
        // The registry answers the column for every registered kind
        // (law-tested), and only the machinery stratum has a built seal path.
        match group_sealing(&plaintext.kind) {
            Some(GroupSealing::MachineryRoot) => {}
            Some(GroupSealing::GenerationTip) => bail!(
                "kind {:?} is registered under the GenerationTip stratum, whose seal path \
                 arrives with group content-kind sealing (the `p2p-share` data plane) — \
                 nothing can seal it yet (key-material-hierarchy.md § Audience: a storage \
                 group)",
                plaintext.kind
            ),
            None => bail!(
                "kind {:?} has no MachineryRoot stratum registration — only machinery \
                 kinds can seal on this plane today, and every write path funnels \
                 through this door",
                plaintext.kind
            ),
        }
        let (_, seq) = self
            .store
            .put_group_state(self.entry_of(plaintext))
            .await
            .context("group-state put: local write")?;

        let writer = self.store.writer();
        let coords = EntryCoordinates {
            writer_id: writer.0,
            writer_seq: seq,
            scope: &self.scope,
        };
        let keys = self.schedule.for_kind(&plaintext.kind);
        let sealed = seal_entry(&keys, &coords, plaintext, self.writer_key)
            .context("group-state put: seal")?;
        self.store
            .record_relay_row(&RelayRow {
                scope: self.scope.clone(),
                item_class: ItemClass::StateEntry.as_wire().to_string(),
                writer,
                writer_seq: seq,
                item_key: sealed.item_key.to_vec(),
                op: if plaintext.tombstone {
                    OP_TOMBSTONE.to_string()
                } else {
                    OP_STATE_PUT.to_string()
                },
                entry: Some(sealed.envelope),
                feed_seq: None,
            })
            .await
            .context("group-state put: relay plane")?;
        Ok(seq)
    }

    // ── Adopt (the ceremony applier) ────────────────────────────────────────

    /// Adopt a machinery snapshot's verbatim rows — the ceremony bootstrap's
    /// applier, and the initiator's own plane write
    /// (`fauna_core::group_ceremony::GroupPlaneRow` carries no writer
    /// coordinates, so every adopted value lands as an own-authored write on
    /// this replica's log, sealed and relay-recorded like any other; the
    /// lattice joins make that convergent when the origin's coordinates later
    /// arrive off the feed).
    ///
    /// Each row goes through `apply_class2`'s ordinary strictness against the
    /// stored current value — first-contact for a fresh scope — exactly as
    /// the deliver's own verifier promises: the snapshot carriage adds no
    /// trust of its own.
    pub async fn adopt_rows(&self, rows: &[GroupPlaneRow]) -> Result<AdoptReport> {
        let mut report = AdoptReport::default();
        for row in rows {
            let Some(policy) = group_merge_policy(&row.kind) else {
                report.unknown += 1;
                continue;
            };
            let incoming = EntryPlaintext {
                kind: row.kind.clone(),
                key: row.key.clone(),
                merge_meta: None,
                value: row.value.clone().into(),
                tombstone: false,
            };
            if let Some(why) = self.unbound_birth(&incoming) {
                tracing::warn!(scope = %self.scope, "refusing a group snapshot row: {why}");
                report.refused += 1;
                continue;
            }
            let current = self
                .store
                .group_state(&self.scope, &row.kind, &row.key)
                .await?
                .map(|e| entry_to_plaintext(&e));
            let outcome = match fauna_protocol::merge_policy::apply_class2(
                policy,
                current.as_ref(),
                &incoming,
            ) {
                Ok(outcome) => outcome,
                Err(err) if err.is_row_content() => {
                    tracing::warn!(
                        kind = %row.kind,
                        key = %row.key,
                        scope = %self.scope,
                        "refusing an unmergeable group snapshot row: {err}"
                    );
                    report.refused += 1;
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            match outcome {
                MergeOutcome::KeepCurrent => report.kept += 1,
                MergeOutcome::Replace => {
                    self.write_own(&incoming).await?;
                    report.adopted += 1;
                }
                MergeOutcome::Merged(merged) => {
                    self.write_own(&merged).await?;
                    report.merged += 1;
                }
                MergeOutcome::NeedsThreeWay => bail!(
                    "kind {:?} needs three-way resolution, which no kind on the group plane \
                     has wired",
                    row.kind
                ),
            }
        }
        Ok(report)
    }

    // ── Walk ────────────────────────────────────────────────────────────────

    /// The accounted catch-up walk from this replica's stored frontier — the
    /// account peer leg's walk on the group scope's feed. The paging cursor
    /// seeds past our own held rows (we hold everything we authored by
    /// definition, and our stored slot deliberately lags — pull-only
    /// posture).
    pub async fn walk(&self) -> Result<WalkReport> {
        let mut start = stored_frontier(self.store, &self.scope).await?;
        seed_past_own_held(self.store, &self.scope, &mut start).await?;
        self.run(start).await
    }

    /// The per-entry full-state reconcile: the same walk from a zero
    /// frontier — re-presents rows an earlier walk left
    /// [`WalkReport::unopened`].
    pub async fn reconcile(&self) -> Result<WalkReport> {
        self.run(BTreeMap::new()).await
    }

    /// The account plane's walk on this scope's feed, down to the paging law
    /// itself — which is `crate::page_walk`'s, shared with every other walk in
    /// the crate. What stays here is the request and the per-row apply.
    async fn run(&self, cursor: BTreeMap<String, i64>) -> Result<WalkReport> {
        crate::page_walk::drive(
            cursor,
            WalkReport::default(),
            "group-state walk",
            async |cursor: &BTreeMap<String, i64>| {
                let reply: SyncChangesListReply = self
                    .rpc
                    .request(
                        "fauna.sync.changes.list",
                        SyncChangesListRequest {
                            since: NEST_SLOT_UNUSED,
                            item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                            scope: Some(self.scope.clone()),
                            frontier: Some(cursor.clone()),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("fauna.sync.changes.list (group-state): {e}"))?;
                Ok(reply)
            },
            async |change: &SyncChange,
                   report: &mut WalkReport,
                   cursor: &mut BTreeMap<String, i64>| {
                self.apply(change, report, cursor).await
            },
        )
        .await
    }

    async fn apply(
        &self,
        change: &SyncChange,
        report: &mut WalkReport,
        cursor: &mut BTreeMap<String, i64>,
    ) -> Result<()> {
        report.rows += 1;
        let (writer, origin_seq) = row_coordinates(change)?;
        let slot = cursor.entry(writer.to_hex()).or_insert(0);
        *slot = (*slot).max(origin_seq as i64);

        // Our own (or a retired identity's) row coming back: we hold it, so
        // only the accounting is owed — and, pull-only, our own CURRENT slot
        // never advances (it is the published high-water; a peer echoing our
        // row proves nothing about any feed holding it).
        //
        // ⚠ Live, never the `open`-time `store.writer()` /
        // `store.retired_writers()` snapshots — the account plane's own note
        // on this line states why (). The harm is LATENT
        // here and only here: this plane has no publish leg, so nothing reads
        // the slot a misclassification would move. It is fixed in the same
        // change anyway, because the latency is a property of today's callers,
        // not of this decision — whoever lands the group plane's publish leg
        // would otherwise inherit a watermark a stale handle can already move.
        let relation = self.store.writer_relation(&writer).await?;
        let own = relation == WriterRelation::Current;
        if relation.is_own()
            && self
                .store
                .max_held_seq(&self.scope, &writer)
                .await?
                .is_some_and(|held| held >= origin_seq)
        {
            if !own {
                self.store
                    .advance_frontier(&self.scope, &writer, origin_seq)
                    .await?;
            }
            report.self_echo += 1;
            return Ok(());
        }

        let envelope = change
            .entry
            .as_ref()
            .with_context(|| format!("group-state row at seq {} carries no entry", change.seq))?;
        let item_key = item_key_of(change)?;

        // The relay plane: keep the verbatim wire row so this replica can
        // serve it onward — unconditional for every coordinate-valid row, the
        // account walk's no-editorializing rule.
        self.store
            .record_relay_row(&RelayRow {
                scope: self.scope.clone(),
                item_class: ItemClass::StateEntry.as_wire().to_string(),
                writer,
                writer_seq: origin_seq,
                item_key: item_key.to_vec(),
                op: change.change_type.clone(),
                entry: Some(envelope.to_vec()),
                feed_seq: u64::try_from(change.seq).ok(),
            })
            .await
            .context("group-state walk: relay plane")?;

        let coords = EntryCoordinates {
            writer_id: writer.0,
            writer_seq: origin_seq,
            scope: &self.scope,
        };
        let Some(plaintext) = self.trial_open(&coords, &item_key, envelope) else {
            report.unopened += 1;
            return Ok(());
        };

        // Cleartext op vs sealed tombstone marker — the seal is authoritative.
        let claimed_tombstone = change.change_type == OP_TOMBSTONE;
        if claimed_tombstone != plaintext.tombstone {
            bail!(
                "group-state row at seq {}: cleartext op {:?} disagrees with the sealed \
                 tombstone marker ({})",
                change.seq,
                change.change_type,
                plaintext.tombstone
            );
        }

        let policy = group_merge_policy(&plaintext.kind).with_context(|| {
            format!(
                "opened a {:?} entry under this build's own machinery schedule but the kind \
                 has no group merge policy — the trial-open set and the registry have \
                 drifted apart",
                plaintext.kind
            )
        })?;
        // A birth row that is another scope's: skip-never-abort, like an
        // unmergeable row — and never merged, because `Immutable` would keep a
        // first-contact forgery for good and refuse the real record after it.
        if let Some(why) = self.unbound_birth(&plaintext) {
            tracing::warn!(
                writer = %hex::encode(writer.0),
                seq = origin_seq,
                scope = %self.scope,
                "skipping a group-state row: {why}"
            );
            report.unmergeable += 1;
            return Ok(());
        }
        let current = self
            .store
            .group_state(&self.scope, &plaintext.kind, &plaintext.key)
            .await?
            .map(|e| entry_to_plaintext(&e));

        let row = JournalRow {
            writer,
            seq: origin_seq,
            scope: self.scope.clone(),
            op: if plaintext.tombstone {
                JournalOp::Tombstone
            } else {
                JournalOp::StatePut
            },
            item: ItemRef::StateKey {
                kind: plaintext.kind.clone(),
                key: plaintext.key.clone(),
                entry_version: origin_seq,
            },
        };

        let outcome = match fauna_protocol::merge_policy::apply_class2(
            policy,
            current.as_ref(),
            &plaintext,
        ) {
            Ok(outcome) => outcome,
            // Skip-never-abort: the row is permanent on its writer's log and
            // aborting would starve this replica of every other writer's rows
            // — the account walk's client-recoverability reasoning, verbatim.
            Err(err) if err.is_row_content() => {
                tracing::warn!(
                    writer = %hex::encode(writer.0),
                    seq = origin_seq,
                    kind = %plaintext.kind,
                    "skipping an unmergeable group-state row: {err}"
                );
                report.unmergeable += 1;
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };
        match outcome {
            MergeOutcome::KeepCurrent => {
                self.store.ingest_row(&row).await?;
                report.kept += 1;
            }
            MergeOutcome::Replace => {
                self.store
                    .ingest_group_state(&row, self.entry_of(&plaintext))
                    .await?;
                report.applied += 1;
            }
            MergeOutcome::Merged(merged) => {
                // The incoming row is consumed as itself; the merged value is
                // a new value this replica authored — onto our own log and
                // relay plane, for the peer to pull back.
                self.store.ingest_row(&row).await?;
                self.store
                    .advance_frontier(&self.scope, &writer, origin_seq)
                    .await?;
                self.write_own(&merged).await?;
                report.merged += 1;
                return Ok(());
            }
            MergeOutcome::NeedsThreeWay => bail!(
                "kind {:?} needs three-way resolution, which no kind on the group plane has \
                 wired",
                plaintext.kind
            ),
        }
        self.store
            .advance_frontier(&self.scope, &writer, origin_seq)
            .await?;
        Ok(())
    }

    /// Open one feed row under this scope's machinery schedule, or `None` for
    /// the [`WalkReport::unopened`] skip. The trial set is the registry's
    /// machinery kinds under the one derived schedule — a v2 (generation-
    /// sealed) envelope is the `GenerationTip` stratum, whose read arm
    /// arrives with group content-kind sealing: unopened, never guessed at.
    fn trial_open(
        &self,
        coords: &EntryCoordinates<'_>,
        item_key: &[u8; 32],
        envelope: &[u8],
    ) -> Option<EntryPlaintext> {
        if peek_generation_id(envelope).is_some() {
            return None;
        }
        group_kinds()
            .filter(|kind| group_sealing(kind) == Some(GroupSealing::MachineryRoot))
            .find_map(|kind| {
                open_entry(&self.schedule.for_kind(kind), coords, item_key, envelope).ok()
            })
    }

    /// Why `plaintext` may not land here, when it is a live birth row that is
    /// not this scope's — `None` for every other row.
    ///
    /// Every write path checks it (adopt, the walk's apply, the own-write
    /// funnel): the birth kind merges `Immutable`, so the first birth row a
    /// replica lands is the one it keeps, and any root holder can seal one. A
    /// record filed here that hashes to another scope's id would otherwise
    /// root this scope's authority in whoever it names, for good
    /// (`fauna_core::group_scope::decode_birth_for_scope`, the one door).
    fn unbound_birth(&self, plaintext: &EntryPlaintext) -> Option<String> {
        if plaintext.kind != KIND_GROUP_BIRTH || plaintext.tombstone {
            return None;
        }
        decode_birth_for_scope(&plaintext.value, &self.scope_id)
            .err()
            .map(|e| e.to_string())
    }

    fn entry_of(&self, plaintext: &EntryPlaintext) -> StateEntry {
        StateEntry {
            kind: plaintext.kind.clone(),
            key: plaintext.key.clone(),
            scope: self.scope.clone(),
            value: plaintext.value.to_vec(),
            merge_meta: plaintext.merge_meta.as_ref().map(|m| m.to_vec()),
            entry_version: 0, // reassigned by the store
            tombstone: plaintext.tombstone,
        }
    }
}

/// The standing requester for a plane constructed only to write locally —
/// honest until the share serve set's transport is wired (`p2p.md` § Cross-
/// user shared-set transfer owns that workstream): the group plane has no
/// feed anywhere yet, so a walk reaching the wire is a build defect, refused
/// loudly rather than answered emptily.
pub struct NoFeed;

impl RpcRequester for NoFeed {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, _payload: Req) -> Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        bail!(
            "the group plane has no feed transport yet ({kind} reached the wire) — walks \
             arrive with the share serve set's wiring"
        )
    }
}

// ── The ceremony driver's account-plane write-throughs ──────────────────────

/// Write one held machinery root through the member's own **fleet-scope**
/// account plane (`fauna.state.group-machinery-root` — Immutable, fleet-only,
/// tip-sealed). The ceremony driver's seam for `BegunGroupShare::held_root_row`
/// and `AdmittedGroupShare::held_root_row`; mark the ceremony's monotone
/// boolean only after this returns.
///
/// The plane's own doors enforce the routing: a non-fleet plane refuses the
/// kind at the A5 partition, and sealing waits on the member's resolved
/// generation tip (R14 — the record must fall out of a stolen device's reach
/// at the next fleet mint).
pub async fn write_held_root_row<B: StoreBackend, R: RpcRequester>(
    fleet_plane: &AccountStatePlane<'_, B, R>,
    record: &GroupHeldRootRecord,
) -> Result<u64> {
    let value = fauna_core::encoding::canonical_encode(record)
        .context("encoding the held machinery root record")?;
    // `put_local`, not `put` — see [`write_reception_key_row`] for the whole
    // argument: a ceremony runs with no nest involved at all, and the publish
    // leg is the pump's.
    fleet_plane
        .put_local(
            &ItemId {
                kind: KIND_GROUP_MACHINERY_ROOT.into(),
                key: GroupHeldRootRecord::logical_key_for(&record.scope_id),
            },
            value.to_vec(),
            None,
        )
        .await
}

/// Write one group-reception keypair record through the member's own
/// **fleet-scope** account plane (`fauna.state.group-reception-key` —
/// Immutable, fleet-only, tip-sealed; old rows retained because old
/// generations' wraps still target old keys). The ceremony driver's seam for
/// the keypair minted at accept.
///
/// **Local write, pump publish — and that is what makes a co-present ceremony
/// possible at all.** `AccountStatePlane::put` writes the durable row and then
/// publishes this replica's unsent rows in journal order, failing the call
/// when the nest cannot be reached — while its own docs say "the local row is
/// durable either way, and the next pass retries". The ceremony has no nest by
/// construction (`p2p.md` § Offline share initiation: the door that binds
/// reads the brake at that moment, so *the ceremony itself then runs with no
/// nest involved at all*), and both seats' ceremonies died on that publish leg
/// with every row already on disk — the measured red behind this door
/// (`test_a_co_present_ceremony_completes_while_the_nest_is_unreachable`). So
/// both rows take `put_local`: the row is durable and stamped when this
/// answers, an `Err` is a LOCAL refusal, and the two network legs are the
/// runtime's publish step, armed by the caller exactly as
/// `AccountStoreHandle::put_preference` arms it. Nothing in the ceremony reads
/// either row back off the fleet — the initiator seals to the reception key it
/// is handed, not to a published row — so no step of it is waiting on the
/// publish.
pub async fn write_reception_key_row<B: StoreBackend, R: RpcRequester>(
    fleet_plane: &AccountStatePlane<'_, B, R>,
    record: &GroupReceptionKeyRecord,
) -> Result<u64> {
    let key = record
        .logical_key()
        .context("deriving the reception key record's logical key")?;
    let value = fauna_core::encoding::canonical_encode(record)
        .context("encoding the reception key record")?;
    fleet_plane
        .put_local(
            &ItemId {
                kind: KIND_GROUP_RECEPTION_KEY.into(),
                key,
            },
            value.to_vec(),
            None,
        )
        .await
}

/// Every group-reception keypair this account holds, **newest first** — the
/// read half of [`write_reception_key_row`].
///
/// The kind retains old rows on purpose ("old generations' wraps still target
/// old keys"), so this is deliberately a *list* and not a "current key" getter
/// that would throw the rest away. A reader opening a room's generation wraps
/// needs whichever key the wrap was addressed to, which for any generation
/// minted before the account's last rotation is not the newest one; a caller
/// handing the room a wrap target at seating wants `first()`.
///
/// A row that fails to decode is **skipped, not fatal**. The alternative is a
/// single corrupted row making every other reception key unreachable — which
/// would lock the account out of rooms it can otherwise still read, the exact
/// unrecoverable-state shape the client-recoverability rule forbids.
pub fn reception_keys_from_rows(entries: Vec<StateEntry>) -> Vec<GroupReceptionKeyRecord> {
    let mut records: Vec<GroupReceptionKeyRecord> = entries
        .into_iter()
        .filter(|e| !e.tombstone)
        .filter_map(|e| match fauna_core::encoding::canonical_decode(&e.value) {
            Ok(record) => Some(record),
            Err(err) => {
                tracing::warn!(
                    key = %e.key,
                    "group-reception key row does not decode; skipping it: {err}"
                );
                None
            }
        })
        .collect();
    // Newest first. The stamp is advisory (`GroupReceptionKeyRecord.minted_at_ms`),
    // so ties are broken by the derived logical key to keep the order total and
    // reproducible across replicas rather than dependent on store iteration.
    records.sort_by(|a, b| {
        b.minted_at_ms.cmp(&a.minted_at_ms).then_with(|| {
            a.logical_key()
                .unwrap_or_default()
                .cmp(&b.logical_key().unwrap_or_default())
        })
    });
    records
}

// ── The group-plane pump legs' scope enumeration ────────────────────────────

/// One group scope a pump leg on this device may **write into**: this device
/// holds the scope's machinery root, and the birth record names this account
/// as the scope's authority.
///
/// The two conditions are separate and both load-bearing. The held root is
/// what makes a write *readable* — a scope whose root never landed here is one
/// this device could not put a readable byte into — and it is why the
/// held-root rows, not the group scopes, are the enumeration's source. The
/// birth record's authority is what makes a write *legitimate*: only the
/// authority account's devices sign roster entries, revocations and mints.
#[cfg(feature = "preference-store")]
pub struct HeldAuthorityScope {
    /// The scope's content-derived id.
    pub scope_id: [u8; 32],
    /// The scope's machinery root — the sealing schedule every group-plane
    /// write derives from, and [`GroupStatePlane::new`]'s third argument.
    pub root: GroupMachineryRoot,
    /// The scope's live (non-tombstone) group-plane rows, read once. No
    /// tombstone door exists on this plane, so the filter is a belt-and-braces
    /// echo of [`GroupStatePlane::put`]'s own contract.
    pub states: Vec<StateEntry>,
}

/// Every group scope this device holds a machinery root for **and** is the
/// authority of, each with its merged rows. All local reads — no config, no
/// nest — and nothing here decides what to write.
///
/// The one enumeration the group plane's pump legs open their scopes through
/// (today the authority-device severance,
/// `fauna_sync_engine::group_authority_revocation`): a second one would be a second
/// answer to "may this device write here" — the one question whose two
/// answers must never differ.
#[cfg(feature = "preference-store")]
pub async fn held_authority_scopes<B: StoreBackend>(
    store: &AccountStore<B>,
    actor: &fauna_core::identity::ActorId,
) -> Result<Vec<HeldAuthorityScope>> {
    let held = store.states_of_kind(KIND_GROUP_MACHINERY_ROOT).await?;
    let mut out = Vec::new();
    for (scope_id, root) in held_group_roots(&held) {
        let scope = GroupScope::new(scope_id).to_string();
        let states: Vec<StateEntry> = store
            .group_scope_states(&scope)
            .await?
            .into_iter()
            .filter(|r| !r.tombstone)
            .collect();
        // No birth row, no authority answer — and the honest reading is that
        // this scope is not listable here at all (`group_scope_view`).
        let Some(summary) = crate::group_scope_view::summarize_group_scope(&scope_id, &states)
        else {
            continue;
        };
        if !summary.is_authority(actor) {
            continue;
        }
        out.push(HeldAuthorityScope {
            scope_id,
            root,
            states,
        });
    }
    Ok(out)
}

/// The group scopes this device HOLDS — one `(scope id, machinery root)` per
/// live, decodable `fauna.group.machinery-root` row. The held-root rows, not
/// the group scopes, are the source: a scope whose root never landed here is
/// one this device can neither read nor vouch for.
///
/// One decode, every enumeration: the pump legs' [`held_authority_scopes`]
/// (which then narrows to the scopes this account is the authority of) and
/// the peer witness door's evaluator
/// ([`crate::group_scope_view::GroupRosterSnapshot::load_held`], every held
/// scope). "Which groups does this device hold" has one answer.
pub fn held_group_roots(rows: &[StateEntry]) -> Vec<([u8; 32], GroupMachineryRoot)> {
    let mut out = Vec::new();
    for row in rows.iter().filter(|r| !r.tombstone) {
        let record: GroupHeldRootRecord = match fauna_core::encoding::canonical_decode(&row.value) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(key = %row.key, "group scopes: held-root row: {e} — skipped");
                continue;
            }
        };
        match record.machinery_root() {
            Ok(root) => out.push((record.scope_id, root)),
            Err(e) => {
                tracing::warn!(key = %row.key, "group scopes: held root: {e} — skipped");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reception_row(record: &GroupReceptionKeyRecord) -> StateEntry {
        StateEntry {
            kind: KIND_GROUP_RECEPTION_KEY.into(),
            key: record.logical_key().unwrap(),
            scope: fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE.into(),
            value: fauna_core::encoding::canonical_encode(record)
                .unwrap()
                .to_vec(),
            merge_meta: None,
            entry_version: 1,
            tombstone: false,
        }
    }

    /// **The reception keys read back newest-first, and one bad row does not
    /// take the others with it.**
    ///
    /// Ordering is the contract a caller depends on twice over, in opposite
    /// directions: seating a member hands the room `first()` (its *current*
    /// wrap target), while opening an old generation's wrap needs whichever
    /// key that mint addressed — which is why the older rows are returned at
    /// all rather than pruned to a single "current" one.
    #[test]
    fn the_reception_keys_read_newest_first_and_survive_a_corrupt_row() {
        let old = GroupReceptionKeyRecord::mint(1_700_000_000_000);
        let newer = GroupReceptionKeyRecord::mint(1_700_000_009_000);
        assert_ne!(
            old.logical_key().unwrap(),
            newer.logical_key().unwrap(),
            "two mints must be distinct rows, else this asserts nothing about order"
        );

        let mut corrupt = reception_row(&old);
        corrupt.key = "corrupt".into();
        corrupt.value = b"not canonical cbor at all".to_vec();

        let mut tombstoned = reception_row(&GroupReceptionKeyRecord::mint(1_700_000_005_000));
        tombstoned.tombstone = true;

        // Deliberately handed in oldest-first order, so a pass-through would
        // fail rather than accidentally agree with the expected answer.
        let got = reception_keys_from_rows(vec![
            reception_row(&old),
            corrupt,
            tombstoned,
            reception_row(&newer),
        ]);

        assert_eq!(
            got.len(),
            2,
            "the corrupt row is skipped and the tombstoned row is not a key this \
             account holds — but neither may cost us the two good ones"
        );
        assert_eq!(
            got[0].minted_at_ms, newer.minted_at_ms,
            "newest first: this is the wrap target a seating hands the room"
        );
        assert_eq!(
            got[1].minted_at_ms, old.minted_at_ms,
            "the older key is retained, because a generation minted before the \
             rotation addressed its wrap to it"
        );
    }
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_core::group_scope::{GroupBirthRecord, group_scope_id};
    use fauna_core::identity::ActorKeypair;

    fn device() -> SigningKey {
        SigningKey::from_bytes(&[0xD1; 32])
    }

    async fn open_store(dir: &tempfile::TempDir) -> AccountStore<SqliteBackend> {
        AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            "unit-test-actor",
            fauna_account_store::types::WriterId(device().verifying_key().to_bytes()),
        )
        .await
        .unwrap()
    }

    /// A real birth record + its content-derived scope id, per test salt.
    fn birth(salt: u8, root: &GroupMachineryRoot) -> (GroupBirthRecord, [u8; 32]) {
        let record = GroupBirthRecord {
            authority_actor: ActorKeypair::from_secret([0x77; 32]).actor_id(),
            salt: [salt; 32],
            machinery_root_commit: root.commitment(),
            created_at_ms: 1_700_000_000_000,
        };
        let scope_id = group_scope_id(&record).unwrap();
        (record, scope_id)
    }

    fn birth_row(record: &GroupBirthRecord) -> GroupPlaneRow {
        GroupPlaneRow {
            kind: KIND_GROUP_BIRTH.into(),
            key: fauna_protocol::group_state::GROUP_BIRTH_KEY.into(),
            value: fauna_core::encoding::canonical_encode(record)
                .unwrap()
                .to_vec(),
        }
    }

    #[tokio::test]
    async fn put_refuses_an_unregistered_kind() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir).await;
        let root = GroupMachineryRoot::mint();
        let sk = device();
        let plane = GroupStatePlane::new(&store, &NoFeed, &root, &sk, &[0xAB; 32]).unwrap();
        let err = plane
            .put(
                &ItemId {
                    kind: "fauna.group.unheard-of".into(),
                    key: "k".into(),
                },
                b"v".to_vec(),
                None,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not on the group plane"), "{err}");
    }

    /// Finding: the sealing-stratum door holds at `write_own` — the
    /// one funnel EVERY write path reaches — never only at `put`. Driven
    /// directly at the funnel because no `GenerationTip` kind is registerable
    /// today (the registry is a frozen const; its own reminder test tells the
    /// first content kind's lander to carry that stratum): an unregistered
    /// kind pushed straight into `write_own` must refuse at the stratum door
    /// and land nothing — a door living only in `put` answers this with a
    /// silent machinery-root seal, which is exactly the forfeit the finding
    /// names for `adopt_rows`.
    #[tokio::test]
    async fn the_stratum_door_holds_at_the_write_funnel_not_only_at_put() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir).await;
        let root = GroupMachineryRoot::mint();
        let sk = device();
        let plane = GroupStatePlane::new(&store, &NoFeed, &root, &sk, &[0xAC; 32]).unwrap();
        let err = plane
            .write_own(&EntryPlaintext {
                kind: "fauna.group.no-stratum".into(),
                key: "k".into(),
                merge_meta: None,
                value: b"v".to_vec().into(),
                tombstone: false,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stratum"), "{err}");
        assert!(
            store
                .group_state(&plane.scope, "fauna.group.no-stratum", "k")
                .await
                .unwrap()
                .is_none(),
            "the refused write landed nothing"
        );
    }

    #[tokio::test]
    async fn adopt_is_first_contact_then_idempotent_and_skips_unknown_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir).await;
        let root = GroupMachineryRoot::mint();
        let (record, scope_id) = birth(0x01, &root);
        let sk = device();
        let plane = GroupStatePlane::new(&store, &NoFeed, &root, &sk, &scope_id).unwrap();

        let rows = vec![
            birth_row(&record),
            GroupPlaneRow {
                kind: "fauna.group.from-a-newer-build".into(),
                key: "k".into(),
                value: b"opaque".to_vec(),
            },
        ];
        let first = plane.adopt_rows(&rows).await.unwrap();
        assert_eq!((first.adopted, first.unknown), (1, 1), "{first:?}");

        // Self-merge is KeepCurrent for every policy — a replayed snapshot
        // adopts nothing and refuses nothing.
        let again = plane.adopt_rows(&rows).await.unwrap();
        assert_eq!(
            (again.kept, again.adopted, again.unknown),
            (1, 0, 1),
            "{again:?}"
        );
    }

    /// A snapshot's birth row that is another scope's — the same root
    /// commitment, another authority — is refused rather than adopted, so the
    /// `Immutable` first contact stays open for the real record; the funnel
    /// refuses it too, for any local write path.
    #[tokio::test]
    async fn a_birth_row_that_is_another_scopes_is_refused_and_the_real_one_still_lands() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir).await;
        let root = GroupMachineryRoot::mint();
        let (record, scope_id) = birth(0x0C, &root);
        let sk = device();
        let plane = GroupStatePlane::new(&store, &NoFeed, &root, &sk, &scope_id).unwrap();

        let forged = GroupBirthRecord {
            authority_actor: ActorKeypair::from_secret([0x78; 32]).actor_id(),
            ..record.clone()
        };
        let refused = plane.adopt_rows(&[birth_row(&forged)]).await.unwrap();
        assert_eq!((refused.refused, refused.adopted), (1, 0), "{refused:?}");
        let err = plane
            .write_own(&EntryPlaintext {
                kind: KIND_GROUP_BIRTH.into(),
                key: fauna_protocol::group_state::GROUP_BIRTH_KEY.into(),
                merge_meta: None,
                value: birth_row(&forged).value.into(),
                tombstone: false,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not this scope's"), "{err}");

        let real = plane.adopt_rows(&[birth_row(&record)]).await.unwrap();
        assert_eq!(real.adopted, 1, "{real:?}");
    }

    /// The regression the dedicated `group_entries` table exists for: every
    /// scope holds a birth row at the SAME `(kind, key)`, and neither
    /// clobbers the other — nor leaks into the account-plane table.
    #[tokio::test]
    async fn two_scopes_hold_birth_rows_at_the_same_logical_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir).await;
        let sk = device();
        let root_a = GroupMachineryRoot::mint();
        let root_b = GroupMachineryRoot::mint();
        let (record_a, scope_a) = birth(0x0A, &root_a);
        let (record_b, scope_b) = birth(0x0B, &root_b);
        assert_ne!(scope_a, scope_b);

        let plane_a = GroupStatePlane::new(&store, &NoFeed, &root_a, &sk, &scope_a).unwrap();
        plane_a.adopt_rows(&[birth_row(&record_a)]).await.unwrap();
        let plane_b = GroupStatePlane::new(&store, &NoFeed, &root_b, &sk, &scope_b).unwrap();
        plane_b.adopt_rows(&[birth_row(&record_b)]).await.unwrap();

        for (plane, record) in [(&plane_a, &record_a), (&plane_b, &record_b)] {
            let held = store
                .group_state(
                    plane.scope(),
                    KIND_GROUP_BIRTH,
                    fauna_protocol::group_state::GROUP_BIRTH_KEY,
                )
                .await
                .unwrap()
                .expect("birth row held");
            assert_eq!(
                held.value,
                fauna_core::encoding::canonical_encode(record)
                    .unwrap()
                    .to_vec()
            );
        }
        // The disjointness, physically: nothing reached the account table.
        assert!(
            store
                .state(
                    KIND_GROUP_BIRTH,
                    fauna_protocol::group_state::GROUP_BIRTH_KEY
                )
                .await
                .unwrap()
                .is_none()
        );
    }
}
