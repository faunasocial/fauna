//! The group pump leg — the share plane's pull of the group content plane,
//! and a bound group set's lift and fold (`docs/goal/behavior/p2p-shared-set-build.md`
//! § Build design — the group set, decisions 6, 7 and 8).
//!
//! Every pass of [`crate::share_glue::run`], for every group scope this
//! device holds and is a live member of: write this seat's own
//! `fauna.group.endpoints` cell where it is missing or stale; **lift** a bound
//! set's pending own rows onto the plane; then dial each other member the
//! scope's cells name, present this member's own `Enrolled` entry, walk the
//! scope's feed over the admitted channel, and **fold** the merged
//! `fauna.group.file` rows into the set's engine with their bodies pulled over
//! the same channel. The cursor is the plane's own per-writer frontier — no
//! pull-cursor row of the share leg's own — and the seat is the session's one
//! actor-keyed seat, so the bind door and rule 7's brake are untouched. The
//! store half (what to pull, the cell, the walk and the lift's write on the
//! store's own thread) is `fauna_account_plane::group_pump` and
//! `fauna_account_plane::group_set`.
//!
//! # The lift and the fold cross the replica's process boundary through the one door
//!
//! The set's engine — the agent's on a desktop — is the one writer of its
//! state DB. The lift reads the pending own rows over a read connection, as
//! the serve does, puts each through the store's door (the device key and its
//! authorization are this process's), and hands the writer seq back through
//! the ingest door as a sequenced row of this account: the engine's
//! `Ok(seq)` upgrade by path. The fold hands the merged rows through the same
//! door unsequenced, converged by the engine's own family. Neither needs an
//! IPC method of its own.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fauna_account_plane::group_pump::{GroupFeedChannel, GroupPumpScope};
use fauna_account_plane::group_set::{GroupContentKeys, GroupFileDraft, fold_group_files};
use fauna_account_store::types::StateEntry;
use fauna_client_capabilities::group_ceremony_node::CeremonyNode;
use fauna_core::blob_seal::SealStamp;
use fauna_core::data::ContentHash;
use fauna_core::device_endpoints::{DeviceEndpoints, DeviceEndpointsEntry};
use fauna_core::feature_gate::EffectivePolicy;
use fauna_core::group_file::{GroupFileChange, open_group_file_path};
use fauna_core::identity::ActorId;
use fauna_peer_channel::PeerChannel;
use fauna_peer_share::client::PeerShareBlobFetcher;
use fauna_peer_sync::discovery::{PeerDialTarget, dial_target_from, share_relay_attaches};
use fauna_protocol::Value;
use fauna_protocol::peer_row::PeerChangeRow;
use fauna_protocol::sync::SyncChange;

use crate::account_runtime::AccountStoreHandle;
use crate::account_state_plane::WalkReport;
use crate::db::{OwnChangeRow, SyncDb, SyncState};
use crate::share_pump::{
    PlannedFetch, PricedPlan, ShareIngestDoor, ShareIngestSummary, SharedSetSpec,
    TransferUsageLedger, mark_spool_capped, price_plan, spool_planned,
};

/// The budget on each request a group walk makes of a peer. The walk runs on
/// the account runtime's thread, which serves nothing else while it waits
/// (`AccountStoreHandle`'s local-command contract), so a peer that stops
/// answering must cost a bounded wait, never a wedged runtime.
pub const GROUP_FEED_REQUEST_BUDGET: Duration = Duration::from_secs(15);

/// The budget on reaching one member: the dial and the admit exchange.
pub const GROUP_CONNECT_BUDGET: Duration = Duration::from_secs(20);

/// The most pending rows one pass lifts for one set; the rest wait for the
/// next pass, oldest first.
pub const LIFT_BATCH: u32 = 256;

/// A [`PeerChannel`] as the group walk's erased channel, every request
/// bounded by [`GROUP_FEED_REQUEST_BUDGET`].
pub struct PeerFeed(pub Arc<PeerChannel>);

impl GroupFeedChannel for PeerFeed {
    fn request_value<'a>(
        &'a self,
        kind: &'static str,
        payload: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::timeout(GROUP_FEED_REQUEST_BUDGET, self.0.request(kind, payload))
                .await
                .map_err(|_| anyhow::anyhow!("{kind}: no answer within the request budget"))?
                .map_err(|e| anyhow::anyhow!("{kind}: {e}"))
        })
    }
}

/// The store side of the group leg: the account runtime in production; a
/// test's own store in the tier_1.
pub trait GroupWalkStore {
    /// Walk one held scope's feed over an admitted channel.
    fn walk_group_scope(
        &self,
        scope_id: [u8; 32],
        channel: Arc<dyn GroupFeedChannel>,
    ) -> impl Future<Output = Result<WalkReport>>;

    /// The scope's merged, opened rows — what the fold reads.
    fn group_scope_states(
        &self,
        scope_id: [u8; 32],
    ) -> impl Future<Output = Result<Vec<StateEntry>>>;

    /// The scope's content-key ring (`fauna_account_plane::group_set::group_set_keys`).
    fn group_set_keys(&self, scope_id: [u8; 32]) -> impl Future<Output = Result<GroupContentKeys>>;

    /// The lift's write (`fauna_account_plane::group_set::put_group_file`):
    /// answers the writer seq.
    fn put_group_file(
        &self,
        scope_id: [u8; 32],
        draft: GroupFileDraft,
    ) -> impl Future<Output = Result<u64>>;
}

impl GroupWalkStore for AccountStoreHandle {
    async fn walk_group_scope(
        &self,
        scope_id: [u8; 32],
        channel: Arc<dyn GroupFeedChannel>,
    ) -> Result<WalkReport> {
        AccountStoreHandle::walk_group_scope(self, scope_id, channel).await
    }

    async fn group_scope_states(&self, scope_id: [u8; 32]) -> Result<Vec<StateEntry>> {
        AccountStoreHandle::group_scope_states(self, scope_id).await
    }

    async fn group_set_keys(&self, scope_id: [u8; 32]) -> Result<GroupContentKeys> {
        AccountStoreHandle::group_set_keys(self, scope_id).await
    }

    async fn put_group_file(&self, scope_id: [u8; 32], draft: GroupFileDraft) -> Result<u64> {
        AccountStoreHandle::put_group_file(self, scope_id, draft).await
    }
}

/// The dial targets a scope's endpoints cells name, through the shared PT-4
/// hygiene ([`dial_target_from`]); a member's relay is attached only when it
/// is the one this seat was bound with ([`share_relay_attaches`]), as on the
/// folder leg.
pub fn group_dial_targets(
    scope: &GroupPumpScope,
    our_lan_ips: &[std::net::Ipv4Addr],
    own_relay_url: Option<&str>,
) -> Vec<PeerDialTarget> {
    scope
        .peers
        .iter()
        .map(|entry| {
            let mut target = dial_target_from(
                DeviceEndpoints {
                    node_id: entry.node_id,
                    lan_addrs: entry.lan_addrs.clone(),
                    public_addrs: entry.public_addrs.clone(),
                    relay_url: entry.relay_url.clone(),
                },
                our_lan_ips,
            );
            target.candidates.relay_available =
                share_relay_attaches(target.relay_url.as_deref(), own_relay_url);
            target
        })
        .collect()
}

/// The pending own row as the lift drafts it, or why it cannot be lifted.
fn lift_draft(row: &OwnChangeRow, ring: &GroupContentKeys) -> Result<GroupFileDraft, String> {
    let stamped = match row.content_stamp {
        Some(SealStamp::Generation(g)) => Some(g),
        Some(SealStamp::Version(_)) => {
            return Err("a group set's row carries a folder-family version".into());
        }
        None => None,
    };
    let (change, generation_id) = match row.change_type.as_str() {
        "delete" => (
            GroupFileChange::Delete,
            // A delete seals no body: it names the tip it is written under.
            stamped
                .or_else(|| ring.tip().map(|(id, _)| id))
                .ok_or("no tip to stamp a delete with")?,
        ),
        _ => {
            let manifest = row
                .manifest_hash
                .as_deref()
                .and_then(|h| fauna_core::hex32::decode(h).ok())
                .ok_or("a write names no well-formed manifest")?;
            (
                GroupFileChange::Put {
                    manifest_hash: manifest,
                    size_bytes: u64::try_from(row.size_bytes).unwrap_or(0),
                },
                stamped.ok_or("a write carries no generation stamp")?,
            )
        }
    };
    Ok(GroupFileDraft {
        path: row.path.clone(),
        change,
        generation_id,
        stamped_at_ms: row.created_at,
        // Decision 10: the head the edit wrote over, signed into the core.
        base_manifest: match row.base_manifest.as_deref() {
            None => None,
            Some(h) => Some(
                fauna_core::hex32::decode(h)
                    .map_err(|_| "a malformed base manifest".to_string())?,
            ),
        },
    })
}

/// The row the lift hands back through the door: the pending row as it now
/// stands on the plane, sequenced at the writer seq.
fn lifted_row(row: &OwnChangeRow, seq: u64) -> PeerChangeRow {
    PeerChangeRow {
        change: SyncChange {
            seq: i64::try_from(seq).unwrap_or(i64::MAX),
            path: Some(row.path.clone()),
            path_hash: row.path_hash.clone(),
            manifest_hash: row.manifest_hash.clone(),
            size_bytes: row.size_bytes,
            change_type: row.change_type.clone(),
            created_at: row.created_at,
            content_generation: match row.content_stamp {
                Some(SealStamp::Generation(g)) => Some(g),
                _ => None,
            },
            author_actor_id: Some(row.author_actor_id.clone()),
            device_id: Some(row.device_id.clone()),
            ..Default::default()
        },
        sequenced: true,
        ..Default::default()
    }
}

fn encode_rows(rows: &[PeerChangeRow]) -> Result<Vec<Vec<u8>>> {
    rows.iter()
        .map(|r| {
            fauna_core::encoding::canonical_encode(r)
                .map(|b| b.to_vec())
                .context("encode a group row for the door")
        })
        .collect()
}

/// **The lift** (decision 7): put each of `spec`'s pending own rows on the
/// plane as this device's `fauna.group.file` row, then hand the writer seqs
/// back through `door` so the engine upgrades each pending row in place.
/// Answers how many rows were put. Per-row failures are logged and left
/// pending for the next pass.
pub async fn lift_group_set<S: GroupWalkStore>(
    store: &S,
    door: &dyn ShareIngestDoor,
    spec: &SharedSetSpec,
    spool_root: &Path,
) -> Result<usize> {
    let scope_id = spec.set_id;
    let pending = SyncDb::open(&spec.db_path)
        .context("open the group set's state DB")?
        .own_pending_changes(LIFT_BATCH)?;
    if pending.is_empty() {
        return Ok(0);
    }
    let ring = store.group_set_keys(scope_id).await?;
    let mut lifted = Vec::new();
    for row in &pending {
        let draft = match lift_draft(row, &ring) {
            Ok(draft) => draft,
            Err(why) => {
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(&row.path),
                    "group lift: pending row not liftable: {why}"
                );
                continue;
            }
        };
        match store.put_group_file(scope_id, draft).await {
            Ok(seq) => lifted.push(lifted_row(row, seq)),
            Err(e) => tracing::warn!(
                path = %fauna_core::log_redact::log_path(&row.path),
                "group lift: the plane refused the row; the next pass retries: {e:#}"
            ),
        }
    }
    if lifted.is_empty() {
        return Ok(0);
    }
    // No body moves on an upgrade: an empty scratch spool.
    let spool = spool_root.join(format!("lift-{}", &hex::encode(scope_id)[..16]));
    let summary = door
        .ingest(
            &spec.folder,
            &spec.folder_id,
            &lifted[0].change.author_actor_id.clone().unwrap_or_default(),
            encode_rows(&lifted)?,
            &spool,
        )
        .await
        .context("hand the lift's seqs to the set's engine")?;
    tracing::debug!(
        lifted = lifted.len(),
        upgraded = summary.already_current,
        "group lift: pending rows put on the plane"
    );
    Ok(lifted.len())
}

/// One fold's rows for a set: the scope's honoured file rows its projection
/// does not reflect yet.
#[derive(Debug, Default)]
pub struct GroupFoldPage {
    pub rows: Vec<PeerChangeRow>,
    /// Rows the writer check refused, or whose path did not open to its cell.
    pub refused: usize,
    /// Honoured rows naming a generation this member holds no key for — they
    /// fold once its wrap arrives.
    pub unopened: usize,
}

/// **The fold** (decision 7): the scope's merged `fauna.group.file` rows
/// under the writer check ([`fold_group_files`]), each path opened under its
/// generation's content key and checked against its cell, screened against
/// the projection — a write whose manifest the path's entry already names,
/// or a delete of a path the projection does not track, is not news — and
/// shaped as the converged rows the engine's ingest door takes — plus a
/// delete, authored by `leaver`, for each live path whose cell folds as no
/// row ([`leaving_deletes`]).
pub fn fold_group_rows(
    scope_id: &[u8; 32],
    rows: &[StateEntry],
    ring: &GroupContentKeys,
    projection: Option<&SyncDb>,
    leaver: &[u8; 32],
) -> GroupFoldPage {
    let mut page = GroupFoldPage::default();
    let Some(fold) = fold_group_files(scope_id, rows) else {
        return page;
    };
    page.refused = fold.refused;
    for honoured in fold.files.values() {
        let core = &honoured.record.core;
        let Some(key) = ring.key(&core.generation_id) else {
            page.unopened += 1;
            continue;
        };
        let path = match open_group_file_path(
            key,
            scope_id,
            &core.generation_id,
            &core.path_hash,
            &core.sealed_path,
        ) {
            Ok(path) => path,
            Err(why) => {
                tracing::warn!("group fold: refusing a file row: {why}");
                page.refused += 1;
                continue;
            }
        };
        let entry = projection.and_then(|db| db.get_entry(&path).ok().flatten());
        let live = entry.as_ref().filter(|e| e.state != SyncState::Deleted);
        let (change_type, manifest_hash, size_bytes) = match &core.change {
            GroupFileChange::Put {
                manifest_hash,
                size_bytes,
            } => {
                let manifest = ContentHash::from_digest_raw(*manifest_hash);
                if live.is_some_and(|e| e.manifest_hash == Some(manifest)) {
                    continue;
                }
                (
                    "create",
                    Some(hex::encode(manifest_hash)),
                    i64::try_from(*size_bytes).unwrap_or(i64::MAX),
                )
            }
            GroupFileChange::Delete => {
                if live.is_none() {
                    continue;
                }
                ("delete", None, 0)
            }
        };
        page.rows.push(PeerChangeRow {
            change: SyncChange {
                path_hash: hex::encode(fauna_core::sync::path_hash(&path)),
                path: Some(path),
                manifest_hash,
                size_bytes,
                change_type: change_type.into(),
                created_at: core.stamped_at_ms,
                content_generation: Some(core.generation_id),
                author_actor_id: Some(hex::encode(honoured.member.0)),
                device_id: Some(hex::encode(honoured.device)),
                ..Default::default()
            },
            sequenced: false,
            group_base: core.base_manifest,
            ..Default::default()
        });
    }
    page.rows.extend(leaving_deletes(
        scope_id,
        rows,
        ring,
        projection,
        &fold.files,
        leaver,
    ));
    page
}

/// **The leaving delete** (`group-set.md` decision 5, the 2026-11-02 ruling):
/// a path whose writer-checked cell folds as no row leaves the set at the
/// engine like a converged delete. The plane caches such a cell as a
/// tombstoned entry carrying its latest writer's value
/// (`AccountStore::cache_group_state`), so its sealed path still opens here;
/// a path this replica's projection tracks live is handed a delete, which
/// the engine applies by its converged-delete arm — decision 9's retention
/// keeping a lifted own edit as on any winning delete.
///
/// Only a cell can name a leaving path: a path with no cell (an own write
/// the lift has not put yet) is never deleted. The delete is this replica's
/// own fold verdict, not any member's act, so it is authored by `leaver`
/// (this seat's actor) and names no deleting device; the engine ranks no
/// converged delete by its stamp, so the refused row's own instant — however
/// far ahead — rides it unranked.
fn leaving_deletes(
    scope_id: &[u8; 32],
    rows: &[StateEntry],
    ring: &GroupContentKeys,
    projection: Option<&SyncDb>,
    honoured: &std::collections::BTreeMap<String, fauna_core::group_file::HonouredGroupFile>,
    leaver: &[u8; 32],
) -> Vec<PeerChangeRow> {
    use fauna_core::group_file::GroupFileRecord;
    let Some(db) = projection else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in rows.iter().filter(|r| {
        r.kind == fauna_protocol::group_state::KIND_GROUP_FILE
            && r.tombstone
            && !honoured.contains_key(&r.key)
    }) {
        let Ok(record) = fauna_core::encoding::canonical_decode::<GroupFileRecord>(&row.value)
        else {
            continue;
        };
        let core = &record.core;
        if core.scope_id != *scope_id || record.logical_key() != row.key {
            continue;
        }
        let Some(key) = ring.key(&core.generation_id) else {
            continue;
        };
        let Ok(path) = open_group_file_path(
            key,
            scope_id,
            &core.generation_id,
            &core.path_hash,
            &core.sealed_path,
        ) else {
            continue;
        };
        let live = db
            .get_entry(&path)
            .ok()
            .flatten()
            .is_some_and(|e| e.state != SyncState::Deleted);
        if !live {
            continue;
        }
        out.push(PeerChangeRow {
            change: SyncChange {
                path_hash: hex::encode(fauna_core::sync::path_hash(&path)),
                path: Some(path),
                change_type: "delete".into(),
                created_at: core.stamped_at_ms,
                content_generation: Some(core.generation_id),
                author_actor_id: Some(hex::encode(leaver)),
                device_id: None,
                ..Default::default()
            },
            sequenced: false,
            ..Default::default()
        });
    }
    out
}

/// What one fold over one member's channel did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GroupFoldOutcome {
    /// Rows handed to the engine.
    pub rows: u32,
    pub materialized: u32,
    pub refused: u32,
    /// `Some` when the transfer gate refused the fold's bodies.
    pub refusal: Option<fauna_core::feature_gate::FeatureVerdict>,
}

/// A bound group set's fold leg for one pass: where its rows go and what
/// prices its bodies.
pub struct GroupFoldLeg<'a> {
    pub spec: &'a SharedSetSpec,
    pub door: &'a dyn ShareIngestDoor,
    pub spool_root: &'a Path,
    pub policy: &'a EffectivePolicy,
    pub ledger: &'a mut TransferUsageLedger,
    pub today: i64,
    /// This seat's actor — the author of the fold's own leaving deletes.
    pub own_actor: [u8; 32],
}

/// Fold `leg`'s set over an admitted member's `channel`: the fold's rows,
/// their bodies priced at the transfer gate and pulled over the share doors
/// with the scope as the set id, then handed through the ingest door.
///
/// Bodies always ride the member's connection: a group set has no nest to
/// leave them to (decision 7), so the folder leg's relay deferral does not
/// apply.
pub async fn fold_group_set<S: GroupWalkStore>(
    store: &S,
    channel: Arc<PeerChannel>,
    peer: &[u8; 32],
    leg: &mut GroupFoldLeg<'_>,
) -> Result<GroupFoldOutcome> {
    let scope_id = leg.spec.set_id;
    let rows = store.group_scope_states(scope_id).await?;
    let ring = store.group_set_keys(scope_id).await?;
    let projection = SyncDb::open(&leg.spec.db_path).ok();
    let page = fold_group_rows(&scope_id, &rows, &ring, projection.as_ref(), &leg.own_actor);
    drop(projection);
    let mut outcome = GroupFoldOutcome {
        refused: page.refused as u32,
        ..Default::default()
    };
    if page.rows.is_empty() {
        return Ok(outcome);
    }
    let peer_hex = hex::encode(peer);
    let mut seen = std::collections::HashSet::new();
    let plan: Vec<PlannedFetch> = page
        .rows
        .iter()
        .filter_map(|r| {
            let manifest = r.change.manifest_hash.as_deref()?;
            let digest = fauna_core::hex32::decode(manifest).ok()?;
            seen.insert(digest).then(|| PlannedFetch {
                manifest_hash: ContentHash::from_digest_raw(digest),
                size: u64::try_from(r.change.size_bytes).unwrap_or(0),
                context: r.change.path.clone().unwrap_or_default(),
            })
        })
        .collect();
    let PricedPlan {
        fetches,
        verdict,
        capped,
        capped_out,
    } = price_plan(leg.policy, leg.ledger, leg.today, &peer_hex, plan);
    if !verdict.is_allowed() {
        tracing::warn!(
            peer = %&peer_hex[..16],
            verdict = ?verdict,
            "group fold: the transfer gate refused this fold's bodies"
        );
        outcome.refusal = Some(verdict);
        return Ok(outcome);
    }
    if let Some(capped) = capped {
        tracing::warn!(verdict = ?capped, "group fold: a file is over the per-operation cap; its row waits");
    }
    let spool = leg.spool_root.join(format!(
        "group-{}-{}",
        &hex::encode(scope_id)[..16],
        &peer_hex[..16]
    ));
    let fetcher = PeerShareBlobFetcher::new(channel, scope_id);
    // No nest to defer to: the gate is always open (see above).
    let spent = spool_planned(
        &fetcher,
        &fetches,
        &spool,
        leg.policy.per_operation_max,
        &|| true,
    )
    .await?;
    for manifest in &capped_out {
        mark_spool_capped(&spool, manifest).await?;
    }
    outcome.rows = page.rows.len() as u32;
    let summary: ShareIngestSummary = leg
        .door
        .ingest(
            &leg.spec.folder,
            &leg.spec.folder_id,
            &peer_hex,
            encode_rows(&page.rows)?,
            &spool,
        )
        .await
        .context("hand the fold to the set's engine")?;
    outcome.refused += summary.refused;
    outcome.materialized = summary.materialized;
    let ops = spent.files.max(u64::from(summary.materialized));
    if ops > 0 || spent.bytes > 0 {
        leg.ledger.record(leg.today, &peer_hex, ops, spent.bytes);
    }
    let _ = tokio::fs::remove_dir_all(&spool).await; // best-effort scratch cleanup
    Ok(outcome)
}

/// One member pulled for one scope.
#[derive(Debug)]
pub struct GroupPullOutcome {
    pub scope_id: [u8; 32],
    /// The member's actor key.
    pub peer: [u8; 32],
    pub result: Result<WalkReport, String>,
    /// The bound set's fold over this member, when the scope has one and the
    /// walk got through.
    pub fold: Option<Result<GroupFoldOutcome, String>>,
}

/// Pull one scope from each target: dial, present `witness`, walk the
/// scope's feed over the admitted channel, and — for a scope bound to a set
/// (`fold`) — fold the merged rows into it with their bodies pulled over the
/// same channel. Best-effort per target — one member unreachable or refusing
/// costs that member's pull only.
pub async fn pull_group_scope<S: GroupWalkStore>(
    node: &CeremonyNode,
    store: &S,
    scope_id: [u8; 32],
    witness: &[u8],
    targets: &[PeerDialTarget],
    mut fold: Option<&mut GroupFoldLeg<'_>>,
) -> Vec<GroupPullOutcome> {
    let mut out = Vec::new();
    for target in targets {
        let connected = tokio::time::timeout(GROUP_CONNECT_BUDGET, async {
            let channel = node
                .dial_with_candidates(ActorId(target.node_id), target.candidates.clone())
                .await
                .context("dial the member")?;
            let admitted = fauna_peer_share::admit_group_over(
                Arc::clone(&channel),
                &[(scope_id, witness.to_vec())],
            )
            .await
            .context("group admission")?;
            anyhow::ensure!(
                admitted.contains(&scope_id),
                "the member did not admit this entry for the scope"
            );
            Ok(channel)
        })
        .await
        .map_err(|_| anyhow::anyhow!("no connection within the connect budget"))
        .and_then(|r| r);
        let (result, fold_result) = match connected {
            Err(e) => (Err(format!("{e:#}")), None),
            Ok(channel) => {
                let walked = store
                    .walk_group_scope(scope_id, Arc::new(PeerFeed(Arc::clone(&channel))))
                    .await
                    .map_err(|e| format!("{e:#}"));
                let folded = match (&walked, fold.as_deref_mut()) {
                    (Ok(_), Some(leg)) => Some(
                        fold_group_set(store, channel, &target.node_id, leg)
                            .await
                            .map_err(|e| format!("{e:#}")),
                    ),
                    _ => None,
                };
                (walked, folded)
            }
        };
        if let Err(e) = &result {
            tracing::debug!(
                peer = %&hex::encode(target.node_id)[..16],
                "group pump: pull failed; the next pass retries: {e}"
            );
        }
        if let Some(Err(e)) = &fold_result {
            tracing::debug!(
                peer = %&hex::encode(target.node_id)[..16],
                "group pump: fold failed; the next pass retries: {e}"
            );
        }
        out.push(GroupPullOutcome {
            scope_id,
            peer: target.node_id,
            result,
            fold: fold_result,
        });
    }
    out
}

/// The bound group sets one pass lifts and folds, and what their folds go
/// through.
pub struct GroupSetsLeg<'a> {
    /// The group-family specs ([`crate::share_glue::compose_specs`]), each
    /// keyed by its scope as its `set_id`.
    pub specs: &'a [SharedSetSpec],
    pub door: &'a dyn ShareIngestDoor,
    pub spool_root: &'a Path,
    pub policy: &'a EffectivePolicy,
    pub ledger: &'a mut TransferUsageLedger,
    pub today: i64,
}

/// The whole group leg of one share-plane pass (see the module docs).
pub async fn group_pass(
    node: &CeremonyNode,
    account: &AccountStoreHandle,
    own_actor: ActorId,
    endpoints: &DeviceEndpoints,
    our_lan_ips: &[std::net::Ipv4Addr],
    now_ms: i64,
    sets: GroupSetsLeg<'_>,
) -> Vec<GroupPullOutcome> {
    if let Err(e) = account
        .put_group_endpoints(DeviceEndpointsEntry::new(endpoints.clone(), None), now_ms)
        .await
    {
        tracing::debug!("group pump: endpoints cell write failed; the next pass retries: {e:#}");
    }
    let scopes = match account.group_pump_scopes(own_actor).await {
        Ok(scopes) => scopes,
        Err(e) => {
            tracing::debug!("group pump: scope read failed; the next pass retries: {e:#}");
            return Vec::new();
        }
    };
    let GroupSetsLeg {
        specs,
        door,
        spool_root,
        policy,
        ledger,
        today,
    } = sets;
    let mut out = Vec::new();
    for scope in &scopes {
        let spec = specs.iter().find(|s| s.set_id == scope.scope_id);
        // The lift first: this pass's own writes are on the plane before any
        // member walks it, and the fold below never lands a member's row over
        // local work still waiting for its lift.
        if let Some(spec) = spec
            && let Err(e) = lift_group_set(account, door, spec, spool_root).await
        {
            tracing::debug!("group pump: lift failed; the next pass retries: {e:#}");
        }
        let mut leg = spec.map(|spec| GroupFoldLeg {
            spec,
            door,
            spool_root,
            policy,
            ledger: &mut *ledger,
            today,
            own_actor: own_actor.0,
        });
        let targets = group_dial_targets(scope, our_lan_ips, node.relay_url());
        out.extend(
            pull_group_scope(
                node,
                account,
                scope.scope_id,
                &scope.witness,
                &targets,
                leg.as_mut(),
            )
            .await,
        );
    }
    out
}
