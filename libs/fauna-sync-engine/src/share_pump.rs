//! The share leg's app-side PUMP (slice E — `p2p.md` § Cross-user shared-set
//! transfer): the cross-user twin of [`crate::peer_leg`]'s same-account pump,
//! written once for all seven apps (priority #2).
//!
//! One pass does, per bound shared set: resolve dial targets from the
//! discovery cache ([`ShareStoreDoors::share_dial_targets`] — advertised
//! endpoints re-bound through the shared hygiene), dial each admitted
//! member's ACTOR-keyed node over the app's one contact-plane endpoint
//! ([`CeremonyNode::dial_with_candidates`]), run the mutual M2 admission
//! ([`fauna_peer_share::client::admit_share_over`]), page the peer's
//! own-authored change rows, pre-fetch the bodies materialization will need
//! into a spool, and hand rows + spool to the state-writing half through the
//! [`ShareIngestDoor`].
//!
//! ## The door is a seam because the state writer is per-deployment
//!
//! The engine that owns the set's state lives wherever the replica is hosted:
//! **in the per-user sync agent** (the desktops — the
//! `RequestMethod::ShareIngest` verb + spool handoff), or **in an on-demand
//! host** (a phone's provider, in this process on android and in the File
//! Provider extension on iOS — `crate::share_glue::on_demand_share_access`).
//! The deployment shape is genuinely not the pump's business: it fetches over
//! the channel it owns and hands the page to whoever owns the writes. The
//! door's contract mirrors the engine's ingest:
//! idempotent per page, spool misses skip rows never pages, and the returned
//! cursor is authoritative (forward-only, owned by the state writer).
//!
//! ## Reads are cross-process WAL reads, deliberately
//!
//! The pull cursor and the cached writer fact live in the set's state DB
//! (single owner: the engine's B2 tables); the pump reads them through a
//! second connection (`SyncDb::open` — the sanctioned pattern
//! `fauna_account_store::set_unrecorded_rels` documents), never through a
//! second bookkeeping copy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_client_capabilities::group_ceremony_node::CeremonyNode;
use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderContentKeys;
use fauna_core::identity::ActorId;
use fauna_peer_share::client::{PeerShareBlobFetcher, admit_share_over, fetch_share_changes};
use fauna_peer_share::server::ShareStore;
use fauna_peer_sync::discovery::PeerDialTarget;
use fauna_transport::{NestPath, bytes_may_ride};
// Re-served for app glue: the pump's callers need the same interface-list
// producer the same-account leg publishes with, without growing their own
// `fauna-peer-sync` dependency edge.
pub use fauna_peer_sync::discovery::discover_lan_candidates;

use crate::account_runtime::AccountStoreHandle;

/// The transfer gate's device-local ledger blob — this store's meta table,
/// one owner: this pump (`AccountStoreHandle::meta_get`'s one-owner-per-key
/// contract; the `META_NEST_FACTS` pattern).
const META_SHARE_TRANSFER_LEDGER: &str = "share_pump/transfer_ledger";

/// The share leg's three typed reads and writes on the account runtime's
/// handle — an extension rather than handle methods because the leg is
/// native-only (web's declared absence) while the handle is the driver's,
/// hosted on every target. Each rides a generic door the handle already
/// serves as a local command: the meta table for the ledger, the plain
/// `states_of_kind` read for the dial targets.
pub trait ShareStoreDoors {
    /// The transfer gate's device-local ledger, or `None` when nothing was
    /// ever recorded (or the blob is unreadable — absent, never a crash:
    /// losing counters under-counts, and the gate's fail direction for a
    /// device-local bound is the ledger's own stated posture). A local read.
    fn share_transfer_ledger(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<TransferUsageLedger>>>;
    /// Persist the transfer gate's ledger — the pump's end-of-pass write.
    fn put_share_transfer_ledger(
        &self,
        ledger: TransferUsageLedger,
    ) -> impl std::future::Future<Output = anyhow::Result<()>>;
    /// Dial targets for one shared set's admitted members — the discovery
    /// cache's advertised endpoints re-bound through the shared hygiene
    /// (`fauna_peer_sync::discovery::share_dial_targets_from_rows`; `node_id`
    /// is the member's ACTOR key). The pump's per-set read. A local read —
    /// never a network call.
    fn share_dial_targets(
        &self,
        channel_id: [u8; 32],
        lan_ips: Vec<std::net::Ipv4Addr>,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<fauna_peer_sync::discovery::PeerDialTarget>>>;
}

impl ShareStoreDoors for AccountStoreHandle {
    async fn share_transfer_ledger(&self) -> anyhow::Result<Option<TransferUsageLedger>> {
        Ok(self
            .meta_get(META_SHARE_TRANSFER_LEDGER)
            .await?
            .and_then(|bytes| fauna_core::encoding::canonical_decode(&bytes).ok()))
    }

    async fn put_share_transfer_ledger(&self, ledger: TransferUsageLedger) -> anyhow::Result<()> {
        let bytes = fauna_core::encoding::canonical_encode(&ledger)
            .map_err(|e| anyhow::anyhow!("encode transfer ledger: {e}"))?;
        self.meta_put(META_SHARE_TRANSFER_LEDGER, bytes.to_vec())
            .await
    }

    async fn share_dial_targets(
        &self,
        channel_id: [u8; 32],
        lan_ips: Vec<std::net::Ipv4Addr>,
    ) -> anyhow::Result<Vec<fauna_peer_sync::discovery::PeerDialTarget>> {
        let rows = self
            .states_of_kind(fauna_protocol::merge_policy::KIND_SHARE_ENDPOINTS)
            .await?;
        fauna_peer_sync::discovery::share_dial_targets_from_rows(&rows, &channel_id, &lan_ips)
    }
}
use crate::db::SyncDb;
use crate::peer_share_store::{ShareServeSource, spool_chunk_path, spool_manifest_path};
use crate::share_body::BodySource;
use crate::share_landing::{Landing, body_is_wanted, free_space, landing_fits};

/// One bound, cross-user shared set the pump serves and pulls: where its
/// state DB lives, where its bodies are read from and how pulled bodies land
/// (the replica access's serve-info read — `crate::share_glue::ReplicaAccess`)
/// plus the app-held M2 content keys.
///
/// `Clone` so the app glue can hold the last-known set across passes: the
/// spec's key-bindings half rides a nest read, and the OFFLINE pass — the
/// scenario the plane exists for — is exactly when that read fails.
#[derive(Clone)]
pub struct SharedSetSpec {
    /// The nest folder name — the human-readable label. Names are unique
    /// only per owner, so the name alone can pair one folder's bytes with
    /// another's keys — [`folder_id`](Self::folder_id) is the
    /// key and rides every hand-off that routes state writes.
    pub folder: String,
    /// The set's [`fauna_core::folder_keys::FolderRef`] in wire form.
    pub folder_id: String,
    /// The set's derived 32-byte MLS `ChannelId`.
    pub set_id: [u8; 32],
    /// The set's per-folder state DB.
    pub db_path: PathBuf,
    /// Where the byte half reads this replica's plaintext: the bound tree,
    /// or an on-demand replica's two roots.
    pub body: Arc<dyn BodySource>,
    /// How this replica lands pulled bodies — the pump plans its fetches by
    /// it, so a body the state writer would not land is never pulled.
    pub landing: Landing,
    /// The set's M2 content-key generations — app custody.
    pub content_keys: FolderContentKeys,
}

/// What one ingest hand-off did — the door's reply, mirroring the engine's
/// own report plus the authoritative cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShareIngestSummary {
    pub refused: u32,
    pub overlaid: u32,
    pub materialized: u32,
    pub already_current: u32,
    /// The per-peer pull cursor after the page — the pump's next `since`.
    pub cursor: i64,
    /// The state writer left a wanted body un-landed at the storage floor
    /// ([`SetPullOutcome::storage_limited`]).
    pub storage_limited: bool,
}

/// The state-writing half's door (module doc). Two implementations, one per
/// kind of replica host, both shared rather than per-app: the agent's
/// (`crate::share_glue::AgentIngestDoor` over
/// `SyncAgentProvisioner::share_ingest`, every desktop) and the on-demand
/// host's (`crate::share_glue::OnDemandIngestDoor` over the host's own
/// `share_ingest` operation — a phone's replica, which records every accepted
/// row and lands bodies by `crate::share_landing`'s policy;
/// `p2p-shared-set-build.md` § *Phone peers — design*, decision 1).
#[async_trait::async_trait]
pub trait ShareIngestDoor: Send + Sync {
    /// Ingest one page of accepted rows (canonical dag-cbor
    /// `PeerShareChange` encodings) with bodies pre-fetched into
    /// `spool_dir` (`spool/manifests/<hex>` + `spool/chunks/<hex>`).
    ///
    /// `folder_id` is the set's `FolderRef` wire form — the door routes by
    /// it, never by the name, because two sets can share a name and a
    /// misrouted ingest writes one set's rows into another's state.
    async fn ingest(
        &self,
        folder: &str,
        folder_id: &str,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &Path,
    ) -> Result<ShareIngestSummary>;
}

/// The specs that may be routed: those whose `set_id` no other spec claims.
///
/// A `set_id` appearing more than once yields **none** of its specs — never
/// "the last one", which is what a plain map insert would pick. See
/// [`refresh_serve_sources`] for why this rule lives in the shared crate and
/// not only in the app-side composer.
fn unambiguous_set_specs(specs: &[SharedSetSpec]) -> impl Iterator<Item = &SharedSetSpec> {
    let mut counts: HashMap<[u8; 32], usize> = HashMap::new();
    for spec in specs {
        *counts.entry(spec.set_id).or_default() += 1;
    }
    specs.iter().filter(move |spec| {
        if counts[&spec.set_id] > 1 {
            tracing::error!(folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
                "share serve source: several specs claim this set; routing NONE of them \
                 rather than whichever composed last — the composer should have refused \
                 this pair");
            return false;
        }
        true
    })
}

/// Build the serve snapshot for `specs` and feed it to the node — sources
/// AND claimed-sets in one write ([`CeremonyNode::set_shared_sets`]).
/// Returns how many sets are now routed; a set whose DB will not open is
/// skipped with a warning (it re-enters at the next refresh).
///
/// ## Two specs sharing a `set_id` route NEITHER — the invariant lives here
///
/// This map is where a mispairing becomes a **disclosure**: the value carries
/// the set's DB, watch dir and that peer group's content keys, so inserting
/// twice under one `set_id` would hand a group whichever folder happened to be
/// composed last. The composer that produced `specs` is
/// expected to have refused the collision already — but the composer is *app
/// glue*, and this is **shared Rust every app's share plane calls**. An
/// invariant enforced only in one app's private composer has to be re-derived
/// correctly by each of the other six as the plane trickles down, and the cost
/// of one miss is the defect above. So the refusal is duplicated here
/// deliberately, at the operation rather than at one of its callers: a
/// `set_id` appearing more than once in `specs` routes **no** source, loudly.
///
/// This is defence in depth, not the primary guard — with a correct composer
/// it never fires, which is why it is cheap to keep. The rule itself is
/// [`unambiguous_set_specs`].
pub fn refresh_serve_sources(node: &CeremonyNode, specs: &[SharedSetSpec]) -> usize {
    let mut sources: HashMap<[u8; 32], Arc<dyn ShareStore>> = HashMap::new();
    for spec in unambiguous_set_specs(specs) {
        match SyncDb::open(&spec.db_path) {
            Ok(db) => {
                sources.insert(
                    spec.set_id,
                    Arc::new(ShareServeSource::new(
                        spec.set_id,
                        db,
                        Arc::clone(&spec.body),
                        spec.content_keys.clone(),
                    )),
                );
            }
            Err(e) => {
                tracing::warn!(folder = %fauna_core::log_redact::log_folder_name(&spec.folder), error = %e,
                    "share serve source: state DB will not open; set unrouted this pass");
            }
        }
    }
    let routed = sources.len();
    node.set_shared_sets(sources);
    routed
}

/// What one `(set, peer)` pull did — the transfer surface's row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetPullOutcome {
    pub folder: String,
    pub peer_hex: String,
    /// The peer's admission verdict let this set through.
    pub admitted: bool,
    pub pages: u32,
    pub rows_accepted: u32,
    pub rows_refused: u32,
    pub materialized: u32,
    pub cursor: i64,
    /// `Some` when the transfer gate refused a page — the surface's honest
    /// "limited by ..." source (Dim-3), naming the binding tier and bound.
    pub refusal: Option<fauna_core::feature_gate::FeatureVerdict>,
    /// A wanted body was not pulled because landing it would take this
    /// device's free space under the storage floor
    /// (`crate::share_landing::STORAGE_FLOOR_BYTES`). Its row stays a
    /// placeholder and a later pass retries; the surface says "limited by
    /// free space on this device".
    pub storage_limited: bool,
    /// Bodies the page planned but left to the nest path because the
    /// connection ran over a relay while the set's nest answered this pass
    /// (`fauna_transport::bytes_may_ride`; `p2p.md` § The relay, ruling 4).
    /// Their rows are walked and stay placeholders; the nest path, or a later
    /// pass on a direct path, lands the bytes — deferred, never failed.
    pub relay_deferred: u32,
}

/// One full pull pass: every spec × every cached dial target. Per-target
/// failures are warned and skipped — an unreachable member is the offline
/// scenario's ordinary weather, and the next pass retries. `nest` is whether
/// the nest answered this pass — the byte gate's one input beside the
/// connection's path ([`pull_set_from_peer`]).
#[allow(clippy::too_many_arguments)] // the pass takes every seam once; a builder would only rename them
pub async fn pull_pass(
    node: &CeremonyNode,
    account: &AccountStoreHandle,
    membership: &Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync>,
    door: &dyn ShareIngestDoor,
    specs: &[SharedSetSpec],
    spool_root: &Path,
    lan_ips: &[std::net::Ipv4Addr],
    policy: &fauna_core::feature_gate::EffectivePolicy,
    today: i64,
    nest: NestPath,
) -> Vec<SetPullOutcome> {
    let own_claimed: Vec<[u8; 32]> = specs.iter().map(|s| s.set_id).collect();
    // The transfer gate's device-local ledger: loaded once per pass, spent
    // per page in memory, persisted once at the end (bounded under-count on
    // a crash - the module's stated posture).
    let mut ledger = account
        .share_transfer_ledger()
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut out = Vec::new();
    for spec in specs {
        let targets = match account
            .share_dial_targets(spec.set_id, lan_ips.to_vec())
            .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(folder = %fauna_core::log_redact::log_folder_name(&spec.folder), error = %e, "share pump: dial-target read failed");
                continue;
            }
        };
        if targets.is_empty() {
            // The FIRST link of the pull chain, and until now the quietest:
            // no cached `fauna.state.share-endpoints` row names a peer for
            // this set, so the pass has nothing to dial and does nothing at
            // all. Silence here reads exactly like a successful no-op pass —
            // which is how the two-actor journey's offline half burned three
            // ~35-minute runs without naming a link (conventions point 6:
            // a failure must diagnose itself).
            tracing::debug!(
                folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
                set = %&hex::encode(spec.set_id)[..16],
                "share pump: no cached dial target for this set; nothing to pull this pass"
            );
            continue;
        }
        for target in &targets {
            match pull_set_from_peer(
                node,
                membership,
                door,
                &own_claimed,
                spec,
                target,
                spool_root,
                policy,
                &mut ledger,
                today,
                nest,
            )
            .await
            {
                Ok(outcome) => out.push(outcome),
                Err(e) => {
                    tracing::debug!(
                        folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
                        peer = %hex::encode(target.node_id),
                        error = %e,
                        "share pump: pull from peer failed; next pass retries"
                    );
                }
            }
        }
    }
    if let Err(e) = account.put_share_transfer_ledger(ledger).await {
        tracing::warn!(error = %e, "share pump: transfer ledger persist failed; this pass's spend is in-memory only");
    }
    out
}

/// Pull one set from one admitted member: dial → mutual admission → page
/// rows from the peer's own-authored log → spool the bodies materialization
/// will need → hand each page through the door. The cursor is read from the
/// set's state DB before the first page and thereafter taken from the door's
/// reply (the state writer owns it).
///
/// Bytes ride direct paths only (`p2p.md` § The relay, ruling 4): before each
/// page's byte fetch the channel's path is re-read, and while it is relayed
/// and `nest` is [`NestPath::Reachable`] the page's planned bodies are left
/// to the nest path ([`SetPullOutcome::relay_deferred`]) — the rows are
/// handed through the door as ever, and the state writer holds the cursor
/// below a sequenced row whose body did not arrive, so the file-granularity
/// resume is untouched.
#[allow(clippy::too_many_arguments)] // the per-peer leg of the pass above; same seams
pub async fn pull_set_from_peer(
    node: &CeremonyNode,
    membership: &Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync>,
    door: &dyn ShareIngestDoor,
    own_claimed: &[[u8; 32]],
    spec: &SharedSetSpec,
    target: &PeerDialTarget,
    spool_root: &Path,
    policy: &fauna_core::feature_gate::EffectivePolicy,
    ledger: &mut TransferUsageLedger,
    today: i64,
    nest: NestPath,
) -> Result<SetPullOutcome> {
    let peer = ActorId(target.node_id);
    let peer_hex = hex::encode(target.node_id);
    let mut outcome = SetPullOutcome {
        folder: spec.folder.clone(),
        peer_hex: peer_hex.clone(),
        ..Default::default()
    };

    let channel = node
        .dial_with_candidates(peer, target.candidates.clone())
        .await
        .context("dial admitted member")?;
    let admission = admit_share_over(
        Arc::clone(&channel),
        own_claimed,
        membership.as_ref(),
        &target.node_id,
    )
    .await
    .context("share admission")?;
    if !admission.admitted_by_peer.contains(&spec.set_id) {
        // Their side does not (yet) admit us for this set — their roster may
        // lag ours. Ordinary weather; the next pass retries. Ordinary is not
        // the same as invisible: an admission that never flips is also how a
        // permanently-stuck pull looks, and the two are indistinguishable
        // without this line.
        tracing::debug!(
            folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
            peer = %&peer_hex[..16],
            "share pump: the peer does not admit us for this set yet; next pass retries"
        );
        return Ok(outcome);
    }
    outcome.admitted = true;

    // Cross-process WAL reads (module doc): the cursor + the cached writer
    // fact, from the state writer's own tables.
    let (mut cursor, is_writer) = {
        let db = SyncDb::open(&spec.db_path).context("open set state DB")?;
        (
            db.share_pull_cursor(&peer_hex)?,
            db.cached_share_writer(&peer_hex)?,
        )
    };
    outcome.cursor = cursor;

    let fetcher = PeerShareBlobFetcher::new(Arc::clone(&channel), spec.set_id);
    loop {
        let page = fetch_share_changes(
            Arc::clone(&channel),
            &spec.set_id,
            cursor,
            &target.node_id,
            is_writer,
        )
        .await
        .context("fetch peer change page")?;
        // A refused row is the single most diagnostic event on this path and
        // was, until now, recorded ONLY as a count: `screen_peer_row` hands
        // back the reason per row and the pump dropped it on the floor. The
        // fail-closed `NotACachedWriter` arm in particular refuses EVERY row
        // a peer serves — sequenced and own-pending alike — so a receiver
        // whose writer-roster cache never filled looks, from every log the
        // pump writes, exactly like a peer with nothing new to serve.
        // Aggregated by reason: one line per (page, reason), never per row.
        if !page.refused.is_empty() {
            let mut by_reason: HashMap<&'static str, u32> = HashMap::new();
            for (_, refusal) in &page.refused {
                *by_reason.entry(refusal.reason()).or_default() += 1;
            }
            for (reason, count) in by_reason {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
                    peer = %&peer_hex[..16],
                    rows = count,
                    "share pump: peer rows refused — {reason}"
                );
            }
        }
        outcome.rows_refused += page.refused.len() as u32;
        if page.accepted.is_empty() {
            // The pull's terminating condition AND its commonest silent
            // failure, sharing one shape. Distinguish them: a first page
            // (`pages == 0`) that accepts nothing means this pull moved no
            // row at all, which is a different fact from a drained cursor.
            if outcome.pages == 0 {
                tracing::debug!(
                    folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
                    peer = %&peer_hex[..16],
                    cursor,
                    refused = page.refused.len(),
                    "share pump: the peer's first page carried no acceptable row"
                );
            }
            break;
        }
        outcome.pages += 1;
        outcome.rows_accepted += page.accepted.len() as u32;

        // Plan the page's fetches (rows already current locally are skipped
        // - a cheap read of the same state DB; sealed-path rows cannot be
        // pre-checked and plan anyway: a body the judge then skips costs
        // bandwidth, never correctness; a body this replica's landing policy
        // does not want, or has no room for, is not planned at all), then put
        // the plan to the transfer gate BEFORE any byte moves - the
        // client-side `p2p-share.transfer` composition (rule 8; the app is
        // the enforcement point).
        let free = match &spec.landing {
            Landing::Resident => None,
            Landing::OnDemand { kept_root } => free_space(kept_root),
        };
        let PagePlan {
            fetches: plan,
            storage_limited,
        } = plan_page_fetch(&spec.db_path, &page.accepted, &spec.landing, free);
        outcome.storage_limited |= storage_limited;
        // The byte gate, re-read per page: a relayed connection with the nest
        // reachable fetches no body — the rows below still go through the
        // door, and the gate prices a page that moves nothing.
        let path = channel.path();
        let plan = if !plan.is_empty() && !bytes_may_ride(path, nest) {
            tracing::debug!(
                folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
                peer = %&peer_hex[..16],
                path = path.label(),
                bodies = plan.len(),
                "share pump: relayed connection while the nest answers; the page's bodies are left to the nest path"
            );
            outcome.relay_deferred += plan.len() as u32;
            Vec::new()
        } else {
            plan
        };
        let planned_volume: u64 = plan.iter().map(|p| p.size).fold(0, u64::saturating_add);
        let verdict = transfer_verdict(
            policy,
            ledger,
            today,
            &peer_hex,
            plan.len() as u64,
            planned_volume,
        );
        if !verdict.is_allowed() {
            // The surface renders the reason and the next window retries —
            // but only the app's surface does, and a headless run (the e2e
            // journey included) then sees a pull that simply stops. Say it
            // here too, naming the binding tier the way the surface does.
            tracing::warn!(
                folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
                peer = %&peer_hex[..16],
                rows = plan.len(),
                planned_volume,
                tier = ?verdict.binding_tier(),
                verdict = ?verdict,
                "share pump: the transfer gate refused this page"
            );
            outcome.refusal = Some(verdict);
            break;
        }
        let spool = spool_root.join(format!(
            "{}-{}",
            &hex::encode(spec.set_id)[..16],
            &peer_hex[..16]
        ));
        let spent = spool_planned(&fetcher, &plan, &spool).await?;

        let rows_encoded: Vec<Vec<u8>> = page
            .accepted
            .iter()
            .map(|r| {
                fauna_core::encoding::canonical_encode(r)
                    .map(|b| b.to_vec())
                    .context("encode accepted row")
            })
            .collect::<Result<_>>()?;
        let summary = door
            .ingest(
                &spec.folder,
                &spec.folder_id,
                &peer_hex,
                rows_encoded,
                &spool,
            )
            .await
            .context("ingest door")?;
        outcome.rows_refused += summary.refused;
        outcome.materialized += summary.materialized;
        outcome.storage_limited |= summary.storage_limited;
        // Record what the SPOOL moved, not what the peer said it would - the
        // rule. `planned_volume` prices the gate's pre-authorisation
        // above, and that is all it may ever do: it is the sum of the
        // counterparty's own `size_bytes` assertions, so metering it lets a
        // peer stamp `size_bytes: 0` on every row and transfer for free
        // forever. Worse, the old form only recorded at all when the ingest
        // materialized something, and whether it does is equally the peer's
        // choice - a page of rows the path guard skips (`../x`) moves every
        // byte and materializes nothing, so the ledger never advanced and the
        // next page re-priced from a standing-still meter.
        //
        // Both dimensions are therefore taken from this side's own count:
        // ops = bodies that actually arrived (never fewer than were
        // materialized, so a spool hit from an earlier pass still counts its
        // file), volume = bytes that actually arrived. Over-counting against
        // the counterparty is the sanctioned direction; under-counting on the
        // counterparty's say-so is not a direction at all.
        let ops = spent.files.max(summary.materialized as u64);
        if ops > 0 || spent.bytes > 0 {
            ledger.record(today, &peer_hex, ops, spent.bytes);
        }
        let _ = tokio::fs::remove_dir_all(&spool).await; // best-effort scratch cleanup

        let advanced = summary.cursor > cursor;
        cursor = summary.cursor.max(cursor);
        outcome.cursor = cursor;
        if !page.more {
            break;
        }
        if !advanced {
            // `more` promised further sequenced rows but the cursor stood
            // still — refusing to spin beats trusting a misbehaving server.
            tracing::warn!(folder = %fauna_core::log_redact::log_folder_name(&spec.folder), peer = %peer_hex,
                "share pump: page claimed more rows but the cursor did not advance; stopping");
            break;
        }
    }
    Ok(outcome)
}

/// One planned body fetch: the manifest a page row names, with the size the
/// transfer gate prices it at (the row's own `size_bytes`).
pub struct PlannedFetch {
    pub manifest_hash: ContentHash,
    pub size: u64,
    /// Error-message context only - never a lookup key.
    pub context: String,
}

/// One page's fetch plan, and whether the storage floor kept a wanted body
/// out of it.
#[derive(Default)]
pub struct PagePlan {
    pub fetches: Vec<PlannedFetch>,
    /// A wanted body was left unplanned because landing it would cross the
    /// storage floor ([`SetPullOutcome::storage_limited`]).
    pub storage_limited: bool,
}

/// Decide which of a page's rows need bodies: create/modify rows with a
/// well-formed manifest whose body this replica's `landing` wants
/// ([`body_is_wanted`]) and that are not already current locally (a cheap
/// read of the state writer's own table), deduplicated by manifest within the
/// page. The plan is what the transfer gate prices BEFORE any byte moves.
///
/// On an on-demand replica a wanted body is planned only while the page's
/// bodies fit above the storage floor: `free` is the device's free space
/// where they land, and each body is counted twice — once in the spool, once
/// landed. The state writer re-checks at the landing; this pre-check is what
/// keeps a body that cannot land from being pulled at all.
fn plan_page_fetch(
    db_path: &Path,
    accepted: &[fauna_protocol::peer_share::PeerShareChange],
    landing: &Landing,
    free: Option<u64>,
) -> PagePlan {
    // One short-lived read connection for the current-manifest pre-check.
    let db = SyncDb::open(db_path).ok();
    let on_demand = matches!(landing, Landing::OnDemand { .. });
    let mut seen = std::collections::HashSet::new();
    let mut plan = PagePlan::default();
    let mut planned_bytes: u64 = 0;
    for row in accepted {
        if row.change.change_type == "delete" {
            continue;
        }
        if !body_is_wanted(landing, row.sequenced) {
            continue; // the row lands; its body is the nest's to serve on open
        }
        let Some(manifest_hex) = row.change.manifest_hash.as_deref() else {
            continue;
        };
        let Some(manifest_hash) = parse_hash(manifest_hex) else {
            continue; // the ingest will refuse the malformed row; nothing to spool
        };
        if !seen.insert(manifest_hash.digest()) {
            continue; // an earlier row in this page named the same content
        }
        if let Some(path) = row.change.path.as_deref()
            && !crate::path_guard::is_safe_relative_path(path)
        {
            // The ingest refuses this row on the same predicate
            // (`engine.rs`, "unsafe path refused"), so fetching its body
            // would move every byte for a row that can never materialize.
            // Cheap to ask here, and asking here is what keeps a peer from
            // using the guard as a free-bandwidth valve. Rows
            // carrying no plaintext path are sealed-path rows: they are
            // opened at the ingest, cannot be pre-checked, and plan as
            // before.
            continue;
        }
        if let (Some(db), Some(path)) = (&db, row.change.path.as_deref())
            && let Ok(Some(entry)) = db.get_entry(path)
            && entry.manifest_hash == Some(manifest_hash)
            // An on-demand placeholder at this head still owes its wanted
            // body (an earlier pass recorded the row and could not land it).
            && !(on_demand && entry.state == crate::db::SyncState::Placeholder)
        {
            continue; // already current - the ingest stamps, fetches nothing
        }
        let size = row.change.size_bytes.max(0) as u64;
        if on_demand {
            let with_this = planned_bytes.saturating_add(size.saturating_mul(2));
            if !landing_fits(free, with_this) {
                plan.storage_limited = true;
                continue; // the row still lands as a placeholder; a later pass retries
            }
            planned_bytes = with_this;
        }
        plan.fetches.push(PlannedFetch {
            manifest_hash,
            size,
            context: row
                .change
                .path
                .clone()
                .unwrap_or_else(|| row.change.path_hash.clone()),
        });
    }
    plan
}

/// What one page's spool ACTUALLY moved, as opposed to what the plan was
/// *priced* at.
///
/// The two are different numbers and the difference is security-bearing: the
/// plan's `size` is [`PlannedFetch::size`], which comes from the counterparty's
/// own `size_bytes` on the change row, while these are bytes this side counted
/// arriving. The transfer gate may *pre-authorise* against the asserted number
/// (a cheap refusal before any byte moves), but the ledger must **record** this
/// one - see [`pull_set_from_peer`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpoolSpend {
    /// Bodies whose bytes actually arrived (manifest fetched and its chunks
    /// served) - the plane's "one operation = one file transfer" unit,
    /// counted at the transfer, not at the materialization.
    pub files: u64,
    /// Bytes that arrived: manifest bodies plus chunk bodies. A chunk named
    /// twice by one manifest is counted per reference - over-counting against
    /// the counterparty is the sanctioned direction.
    pub bytes: u64,
}

/// Pre-fetch the planned manifest + chunk bodies into `spool`, returning what
/// actually moved. Bodies arrive through a [`BlobFetcher`] - for the pump,
/// [`PeerShareBlobFetcher`], which re-hashes every one against the address it
/// asked for, so the spool holds only verified bytes. A body the peer cannot
/// produce skips THAT entry (the ingest's spool miss skips that row), never
/// the page.
///
/// Bytes are counted the moment they arrive, including on the two paths that
/// then discard them (an undecodable manifest, an unservable chunk set): they
/// crossed the wire, so they are spend. A chunk pull that fails mid-transfer
/// reports its arrived bytes through the error
/// ([`fauna_core::file_download::TransferredBeforeFailure`]), and those are
/// charged too. Taking `&dyn BlobFetcher` rather than
/// the concrete peer fetcher is what lets the accounting be pinned without a
/// live channel.
///
/// [`BlobFetcher`]: fauna_core::file_download::BlobFetcher
async fn spool_planned(
    fetcher: &dyn fauna_core::file_download::BlobFetcher,
    plan: &[PlannedFetch],
    spool: &Path,
) -> Result<SpoolSpend> {
    tokio::fs::create_dir_all(spool.join("manifests")).await?;
    tokio::fs::create_dir_all(spool.join("chunks")).await?;

    let mut spent = SpoolSpend::default();
    for entry in plan {
        let manifest_path = spool_manifest_path(spool, &entry.manifest_hash);
        if manifest_path.exists() {
            continue; // spooled by an earlier pass against the same scratch
        }
        let manifest_bytes = match fetcher.fetch_manifest(&entry.manifest_hash).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(path = %fauna_core::log_redact::log_path(&entry.context), error = %e, "spool: manifest unavailable");
                continue;
            }
        };
        spent.bytes = spent.bytes.saturating_add(manifest_bytes.len() as u64);
        // Address-vs-bytes, as `file_download::fetch_manifest` does. This call
        // site reaches the trait method directly rather than through the walk,
        // so it does not inherit that check: without this, a nest could fill
        // the scratch with a manifest that is not the one planned. The peer
        // would refuse it at its own transfer boundary, so the harm is a
        // poisoned spool and wasted transfer rather than a bad read — but it is
        // the same root, and it is cheaper to refuse here.
        if fauna_core::data::ContentHash::of_raw(&manifest_bytes) != entry.manifest_hash {
            tracing::debug!(
                path = %fauna_core::log_redact::log_path(&entry.context),
                "spool: manifest bytes do not hash to the planned address"
            );
            continue;
        }
        let Ok(manifest) = fauna_core::encoding::canonical_decode::<fauna_core::chunk::ChunkManifest>(
            &manifest_bytes,
        ) else {
            continue;
        };
        let store_keys = manifest.store_keys();
        let bodies = match fetcher.fetch_chunks(&store_keys, &entry.context).await {
            Ok(b) => b,
            Err(e) => {
                // A failed pull still moved bytes, and a peer that fails every
                // pull on purpose must not transfer for free — so what the
                // fetcher reports as arrived is spend.
                spent.bytes = spent.bytes.saturating_add(
                    fauna_core::file_download::bytes_transferred_before_failure(&e),
                );
                tracing::debug!(path = %fauna_core::log_redact::log_path(&entry.context), error = %e, "spool: chunk bodies unavailable");
                continue;
            }
        };
        spent.bytes = bodies
            .iter()
            .fold(spent.bytes, |acc, b| acc.saturating_add(b.len() as u64));
        spent.files = spent.files.saturating_add(1);
        tokio::fs::write(&manifest_path, &manifest_bytes).await?;
        for (key, body) in store_keys.iter().zip(bodies) {
            tokio::fs::write(spool_chunk_path(spool, key), body).await?;
        }
    }
    Ok(spent)
}

pub(crate) fn parse_hash(hex_str: &str) -> Option<ContentHash> {
    let bytes = hex::decode(hex_str).ok()?;
    let digest: [u8; 32] = bytes.try_into().ok()?;
    Some(ContentHash::from_digest_raw(digest))
}

// ── The transfer gate (rule 8's client-side composition) ─────────────────────

/// Device-local usage ledger for `p2p-share.transfer` — the counters behind
/// the client-side gate (`dynamic-features.md` § Evaluation points: for a
/// plane the nest never sees, **the app is the enforcement point**, and the
/// tier-1 constants ship in the artifact). Day-bucketed like the nest's
/// `guardian_usage` shape, summed per window at evaluation; counterparty
/// increments are the operation's newness delta (a peer not transferred with
/// inside the largest window), never re-derived from mutable records.
///
/// Device-local by construction: the nest cannot arbitrate what it cannot
/// see, so the enforceable bound is per device — a multi-device account
/// spends each device's budget separately (stated, not hidden; the
/// fleet-summed refinement is named forward work). Persisted in the account
/// store's meta table ([`ShareStoreDoors::share_transfer_ledger`] /
/// [`ShareStoreDoors::put_share_transfer_ledger`]); the pump records per
/// page in memory and persists per pass, so a crash forgets at most one
/// pass's spend — bounded under-counting, the client-side analog of
/// spend-on-commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransferUsageLedger {
    /// day bucket ([`fauna_core::day_bucket::local_day_bucket`]) → that day's
    /// spend. Pruned past the largest window at each record.
    #[serde(default)]
    pub days: std::collections::BTreeMap<i64, TransferDayUsage>,
    /// peer actor hex → the last day bucket a transfer with them recorded —
    /// the newness-delta source.
    #[serde(default)]
    pub seen_peers: std::collections::BTreeMap<String, i64>,
}

/// One day bucket's spend. Grows additively; construct with
/// `..Default::default()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransferDayUsage {
    #[serde(default)]
    pub ops: u64,
    #[serde(default)]
    pub volume: u64,
    #[serde(default)]
    pub new_counterparties: u64,
}

impl TransferUsageLedger {
    /// The windowed sums [`fauna_core::feature_gate::feature_verdict`] takes.
    pub fn usage_counters(&self, today: i64) -> fauna_core::feature_gate::UsageCounters {
        use fauna_core::day_bucket::window_start_bucket;
        use fauna_core::feature_gate::{Window, WindowCounts};
        let sum = |from: i64, pick: fn(&TransferDayUsage) -> u64| -> u64 {
            self.days
                .range(from..=today)
                .map(|(_, d)| pick(d))
                .fold(0u64, u64::saturating_add)
        };
        let counts = |pick: fn(&TransferDayUsage) -> u64| WindowCounts {
            day: sum(window_start_bucket(today, Window::Day.days()), pick),
            week: sum(window_start_bucket(today, Window::Week.days()), pick),
            month: sum(window_start_bucket(today, Window::Month.days()), pick),
        };
        fauna_core::feature_gate::UsageCounters {
            operations: counts(|d| d.ops),
            counterparties: counts(|d| d.new_counterparties),
            volume: counts(|d| d.volume),
        }
    }

    /// The newness delta for one peer: `1` iff no transfer with them recorded
    /// inside the largest window.
    pub fn newness_delta(&self, peer_hex: &str, today: i64) -> u64 {
        let month_start = fauna_core::day_bucket::window_start_bucket(
            today,
            fauna_core::feature_gate::Window::Month.days(),
        );
        match self.seen_peers.get(&peer_hex.to_lowercase()) {
            Some(last) if *last >= month_start => 0,
            _ => 1,
        }
    }

    /// Record one page's actual spend and prune buckets past the largest
    /// window. The counterparty increment is resolved HERE against the seen
    /// map (the newness rule), then the peer's seen mark advances.
    pub fn record(&mut self, today: i64, peer_hex: &str, ops: u64, volume: u64) {
        let newness = if ops > 0 {
            self.newness_delta(peer_hex, today)
        } else {
            0
        };
        let day = self.days.entry(today).or_default();
        day.ops = day.ops.saturating_add(ops);
        day.volume = day.volume.saturating_add(volume);
        day.new_counterparties = day.new_counterparties.saturating_add(newness);
        if ops > 0 {
            self.seen_peers.insert(peer_hex.to_lowercase(), today);
        }
        let month_start = fauna_core::day_bucket::window_start_bucket(
            today,
            fauna_core::feature_gate::Window::Month.days(),
        );
        self.days.retain(|bucket, _| *bucket >= month_start);
        self.seen_peers.retain(|_, last| *last >= month_start);
    }
}

/// The transfer gate's verdict for one prospective page: the ONE shared
/// verdict function over the effective policy (nest-served when reachable;
/// the caller falls back to the artifact's tier-1 constants offline —
/// `effective_policy(feature, &[], &[])`) and this device's ledger.
pub fn transfer_verdict(
    policy: &fauna_core::feature_gate::EffectivePolicy,
    ledger: &TransferUsageLedger,
    today: i64,
    peer_hex: &str,
    files: u64,
    volume: u64,
) -> fauna_core::feature_gate::FeatureVerdict {
    use fauna_core::feature_gate::{GateOp, GatedFeature, entry, feature_verdict};
    // One operation = one file transfer (the ruling this composition makes:
    // "operations" must count something a user can reason about, and files
    // are the plane's unit of user intent). `feature_verdict` evaluates one
    // op; a page of N files folds the other N-1 into the observed counters,
    // so the whole page either fits or names the bound it trips. A page
    // moving nothing (all-deletes) still consults availability — a Deny
    // stops the plane, not just its bytes.
    let mut scratch = ledger.usage_counters(today);
    let extra_ops = files.saturating_sub(1);
    scratch.operations.day = scratch.operations.day.saturating_add(extra_ops);
    scratch.operations.week = scratch.operations.week.saturating_add(extra_ops);
    scratch.operations.month = scratch.operations.month.saturating_add(extra_ops);
    let op = GateOp {
        feature: GatedFeature::P2pShare,
        surface: fauna_core::feature_gate::SURFACE_P2P_SHARE_TRANSFER,
        new_counterparties: if files > 0 {
            ledger.newness_delta(peer_hex, today)
        } else {
            0
        },
        magnitude: volume,
    };
    feature_verdict(entry(GatedFeature::P2pShare), policy, &scratch, &op)
}

// ── The publish pump (the discovery carriage's sending half) ─────────────────

/// The publish floor: re-advertise a set's own endpoints at least this often
/// while the node is up, on top of the change-triggered immediate publish.
///
/// The cadence ruling (`p2p-shared-set-build.md` § Built — the discovery carriage names it
/// "genuinely undesigned; pick one and write down why"): a **change** in the
/// endpoint set publishes immediately (the pump pass recomputes candidates
/// anyway — an address change is exactly the event the row exists to carry),
/// and the **floor** is what eventually reaches members the change-trigger
/// structurally cannot: an MLS application message is delivered to the
/// members present at send time, so a LATER joiner has no row for us until
/// the next publish. One hour bounds that joiner's peer-discovery latency at
/// the cost of one tiny group-sealed message per set per hour per device —
/// negligible against the custody cadence precedent. Reactive re-advertise
/// on observing a new member's first advertisement is the named forward
/// refinement, not built here.
pub const ADVERTISE_FLOOR_SECS: u64 = 3600;

/// The floor in force: `FAUNA_SHARE_ADVERTISE_FLOOR_SECS` when it parses to
/// a positive integer (the `FAUNA_CONV_POLL_SECS` pattern — an e2e journey
/// must exercise the floor's HEAL without waiting production's hour), else
/// [`ADVERTISE_FLOOR_SECS`]. The floor is the only re-send an advertisement
/// lost in flight ever gets (best-effort by design), so the e2e knob is what
/// makes single-shot loss a latency, never a test verdict.
pub fn advertise_floor_secs() -> u64 {
    std::env::var("FAUNA_SHARE_ADVERTISE_FLOOR_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .unwrap_or(ADVERTISE_FLOOR_SECS)
}

/// The publish decision + build, per pass: which sets are due (endpoints
/// changed, floor elapsed, or never published), and the encoded
/// advertisement each should send on its own conversation channel
/// (`ConversationsSession::send_share_endpoints` — the app's thin loop owns
/// that call; this state owns everything decidable).
#[derive(Default)]
pub struct AdvertiseState {
    last: HashMap<
        [u8; 32],
        (
            fauna_core::device_endpoints::DeviceEndpoints,
            std::time::Instant,
        ),
    >,
}

/// One due advertisement: the channel in the hex spelling
/// `send_share_endpoints` takes, and the canonical row bytes to send.
pub struct DueAdvertisement {
    /// The set, for [`AdvertiseState::mark_sent`] once the send SUCCEEDED.
    pub set_id: [u8; 32],
    pub channel_hex: String,
    pub bytes: Vec<u8>,
}

impl AdvertiseState {
    /// Decide + build for one pass. `endpoints` is this device's current
    /// composition (LAN candidates + relay — the same producer the
    /// same-account leg publishes with); `now` is injected so the floor is
    /// testable without wall-clock waits.
    pub fn due(
        &mut self,
        specs: &[SharedSetSpec],
        own_actor: &ActorId,
        endpoints: &fauna_core::device_endpoints::DeviceEndpoints,
        now: std::time::Instant,
    ) -> Vec<DueAdvertisement> {
        let floor = advertise_floor_secs();
        let mut out = Vec::new();
        for spec in specs {
            let due = match self.last.get(&spec.set_id) {
                None => true,
                Some((last_endpoints, at)) => {
                    last_endpoints != endpoints || now.duration_since(*at).as_secs() >= floor
                }
            };
            if !due {
                continue;
            }
            let row =
                fauna_peer_share::own_advertisement(&spec.set_id, own_actor, endpoints.clone());
            match fauna_core::encoding::canonical_encode(&row) {
                Ok(bytes) => {
                    out.push(DueAdvertisement {
                        set_id: spec.set_id,
                        channel_hex: hex::encode(spec.set_id),
                        bytes: bytes.to_vec(),
                    });
                }
                Err(e) => {
                    tracing::warn!(folder = %fauna_core::log_redact::log_folder_name(&spec.folder), error = %e, "advertisement encode failed");
                }
            }
        }
        out
    }

    /// Record that a due advertisement actually REACHED the channel — the
    /// caller's duty after a successful `send_share_endpoints`. Deliberately
    /// not done inside [`Self::due`] (it was, originally): marking at build
    /// time meant a FAILED send lost the advertisement until the floor — an
    /// hour of no dial row for every member, for one transient send error.
    /// An unmarked set is simply due again next pass.
    pub fn mark_sent(
        &mut self,
        set_id: [u8; 32],
        endpoints: &fauna_core::device_endpoints::DeviceEndpoints,
        now: std::time::Instant,
    ) {
        self.last.insert(set_id, (endpoints.clone(), now));
    }
}

#[cfg(test)]
mod transfer_gate_tests {
    use super::*;
    use fauna_core::feature_gate::{
        FeatureVerdict, GatedFeature, QuotaDimension, Window, effective_policy, entry,
    };

    const TODAY: i64 = 20_600;
    const PEER: &str = "aa";

    fn tier1() -> fauna_core::feature_gate::EffectivePolicy {
        effective_policy(GatedFeature::P2pShare, &[], &[])
    }

    /// A fresh device transfers freely at tier 1; the ledger's spend then
    /// shows in every window; and the newness delta counts a peer once per
    /// largest window, never per page.
    #[test]
    fn spend_accumulates_and_a_peer_counts_once() {
        let mut ledger = TransferUsageLedger::default();
        assert!(transfer_verdict(&tier1(), &ledger, TODAY, PEER, 3, 9_000).is_allowed());

        ledger.record(TODAY, PEER, 3, 9_000);
        let usage = ledger.usage_counters(TODAY);
        assert_eq!(usage.operations.day, 3);
        assert_eq!(usage.volume.week, 9_000);
        assert_eq!(usage.counterparties.month, 1);

        ledger.record(TODAY + 1, PEER, 2, 100);
        assert_eq!(
            ledger.usage_counters(TODAY + 1).counterparties.month,
            1,
            "a repeat peer inside the window adds no counterparty"
        );
        assert_eq!(ledger.usage_counters(TODAY + 1).operations.week, 5);
        assert_eq!(
            ledger.usage_counters(TODAY + 1).operations.day,
            2,
            "yesterday's ops left the day window"
        );
    }

    /// A spent day quota refuses the NEXT page with the bound named — the
    /// enforcement the client owes for a plane the nest never sees — and the
    /// day rolling over restores headroom (day bound) while the month keeps
    /// counting.
    #[test]
    fn a_spent_quota_refuses_with_the_bound_named() {
        let ops_day = entry(GatedFeature::P2pShare)
            .tier1
            .operations
            .per_day
            .unwrap();
        let mut ledger = TransferUsageLedger::default();
        ledger.record(TODAY, PEER, ops_day, 1);

        match transfer_verdict(&tier1(), &ledger, TODAY, PEER, 1, 1) {
            FeatureVerdict::OverQuota {
                dimension,
                window,
                limit,
                ..
            } => {
                assert_eq!(dimension, QuotaDimension::Operations);
                assert_eq!(window, Some(Window::Day));
                assert_eq!(limit, ops_day);
            }
            other => panic!("a spent day quota must refuse, got {other:?}"),
        }
        assert!(
            transfer_verdict(&tier1(), &ledger, TODAY + 1, PEER, 1, 1).is_allowed(),
            "the next day's bucket has headroom again"
        );
    }

    /// A whole page is priced at once: a page whose file count alone exceeds
    /// the day bound refuses before any byte moves.
    #[test]
    fn a_page_is_priced_as_a_whole() {
        let ops_day = entry(GatedFeature::P2pShare)
            .tier1
            .operations
            .per_day
            .unwrap();
        let ledger = TransferUsageLedger::default();
        assert!(!transfer_verdict(&tier1(), &ledger, TODAY, PEER, ops_day + 1, 1).is_allowed());
        assert!(transfer_verdict(&tier1(), &ledger, TODAY, PEER, ops_day, 1).is_allowed());
    }

    /// Buckets and seen-peers prune past the largest window — the ledger
    /// cannot grow without bound, and a long-gone peer reads as new again.
    #[test]
    fn the_ledger_prunes_past_the_largest_window() {
        let mut ledger = TransferUsageLedger::default();
        ledger.record(TODAY, PEER, 1, 1);
        let later = TODAY + Window::Month.days() as i64 + 1;
        ledger.record(later, "bb", 1, 1);
        assert!(!ledger.days.contains_key(&TODAY), "old bucket pruned");
        assert_eq!(
            ledger.newness_delta(PEER, later),
            1,
            "a peer last seen past the window is new again"
        );
    }
}

#[cfg(test)]
mod spool_spend_tests {
    use super::*;
    use fauna_core::data::ContentHash;
    use fauna_protocol::peer_share::PeerShareChange;
    use fauna_protocol::sync::SyncChange;

    /// A peer that serves whatever it is asked for, at whatever size it
    /// chooses — the adversary's side of the transfer meter.
    struct LyingPeer {
        manifest: Vec<u8>,
        manifest_hash: ContentHash,
        body: Vec<u8>,
    }

    impl LyingPeer {
        /// One file of `body_len` bytes in a single chunk, addressed honestly
        /// (every body still hashes to the key it is served under — the
        /// verification the spool DOES do is not what this pins).
        fn one_file(body_len: usize) -> Self {
            let body = vec![7u8; body_len];
            let chunk_hash = ContentHash::of_raw(&body);
            let manifest = fauna_core::chunk::ChunkManifest {
                file_hash: chunk_hash,
                total_size: body_len as u64,
                chunk_hashes: vec![chunk_hash],
                chunk_sizes: vec![body_len as u64],
                stored_hashes: None,
                sealed_hashes: None,
                min_reader: None,
            };
            let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest)
                .expect("encode manifest")
                .to_vec();
            let manifest_hash = ContentHash::of_raw(&manifest_bytes);
            Self {
                manifest: manifest_bytes,
                manifest_hash,
                body,
            }
        }

        fn wire_bytes(&self) -> u64 {
            (self.manifest.len() + self.body.len()) as u64
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::file_download::BlobFetcher for LyingPeer {
        async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
            assert_eq!(hash, &self.manifest_hash, "the pump asked for our manifest");
            Ok(self.manifest.clone())
        }

        async fn fetch_chunks(
            &self,
            store_keys: &[ContentHash],
            _relative_path: &str,
        ) -> Result<Vec<Vec<u8>>> {
            Ok(store_keys.iter().map(|_| self.body.clone()).collect())
        }
    }

    /// ⚠ — the transfer meter must count bytes THIS SIDE saw arrive,
    /// never the counterparty's `size_bytes` assertion.
    ///
    /// The plan below is priced at zero, the way a hostile peer prices every
    /// row it serves. The spool must still report the real spend, because
    /// that is what the ledger records and what the next page's verdict is
    /// evaluated against.
    #[tokio::test]
    async fn the_spool_counts_what_arrived_not_what_the_peer_claimed() {
        let peer = LyingPeer::one_file(64 * 1024);
        let dir = tempfile::tempdir().expect("tempdir");

        let plan = vec![PlannedFetch {
            manifest_hash: peer.manifest_hash,
            size: 0, // the lie: "this file is zero bytes"
            context: "some/file".to_string(),
        }];

        let spent = spool_planned(&peer, &plan, dir.path())
            .await
            .expect("spool the plan");

        assert_eq!(spent.files, 1, "one body arrived");
        assert_eq!(
            spent.bytes,
            peer.wire_bytes(),
            "the meter must see the bytes that crossed the wire, not the \
             zero the peer stamped on the row"
        );
        assert!(
            spent.bytes > 0,
            "a peer that prices its own transfers at zero must not transfer for free"
        );
    }

    /// A member that serves the manifest, then fails the chunk pull after
    /// `arrived` body bytes crossed — the way the share fetcher reports a pull
    /// it refused mid-transfer.
    struct FailingPeer {
        inner: LyingPeer,
        arrived: u64,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::file_download::BlobFetcher for FailingPeer {
        async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
            self.inner.fetch_manifest(hash).await
        }

        async fn fetch_chunks(
            &self,
            _store_keys: &[ContentHash],
            _relative_path: &str,
        ) -> Result<Vec<Vec<u8>>> {
            Err(anyhow::anyhow!("the peer stalled mid-body").context(
                fauna_core::file_download::TransferredBeforeFailure(self.arrived),
            ))
        }
    }

    /// ⚠ a chunk pull that fails after bytes arrived still
    /// charges them. Otherwise a writer that declares `size_bytes: 0` and
    /// fails every pull moves bytes on every page of every pass, and the
    /// ledger never sees one.
    #[tokio::test]
    async fn a_failed_chunk_pull_charges_the_bytes_that_arrived() {
        let arrived = 512 * 1024;
        let peer = FailingPeer {
            inner: LyingPeer::one_file(64 * 1024),
            arrived,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = vec![PlannedFetch {
            manifest_hash: peer.inner.manifest_hash,
            size: 0,
            context: "some/file".to_string(),
        }];

        let spent = spool_planned(&peer, &plan, dir.path())
            .await
            .expect("a failed entry skips, never fails the page");

        assert!(
            spent.bytes >= arrived,
            "the failed pull's {arrived} arrived bytes must be charged, got {}",
            spent.bytes
        );
        assert_eq!(
            spent.bytes,
            peer.inner.manifest.len() as u64 + arrived,
            "manifest bytes plus the failed pull's arrived bytes"
        );
        assert_eq!(spent.files, 0, "no body completed");
    }

    fn row(path: &str) -> PeerShareChange {
        PeerShareChange {
            change: SyncChange {
                seq: 9,
                path_hash: "cc".repeat(32),
                path: Some(path.to_string()),
                manifest_hash: Some("dd".repeat(32)),
                size_bytes: 4096,
                change_type: "create".to_string(),
                created_at: 1_760_000_000,
                ..Default::default()
            },
            sequenced: true,
            ..Default::default()
        }
    }

    /// ⚠ the other half — a row the ingest's path guard will refuse
    /// must not have its body fetched at all.
    ///
    /// Without this the guard is a free-bandwidth valve: the bytes move, the
    /// row skips, nothing materializes, and (before the fix) nothing was
    /// recorded either.
    #[test]
    fn an_unsafe_path_is_never_planned_for_a_fetch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("state.db");

        let escaping = plan_page_fetch(
            &db_path,
            &[row("../../etc/passwd")],
            &Landing::Resident,
            None,
        );
        assert!(
            escaping.fetches.is_empty(),
            "a row the ingest refuses on `is_safe_relative_path` must cost no bytes"
        );

        let ordinary =
            plan_page_fetch(&db_path, &[row("notes/today.md")], &Landing::Resident, None);
        assert_eq!(
            ordinary.fetches.len(),
            1,
            "an ordinary row still plans — the guard must not swallow real work"
        );
    }

    fn pending_row(path: &str, manifest_byte: &str, size: i64) -> PeerShareChange {
        let mut pending = row(path);
        pending.sequenced = false;
        pending.change.manifest_hash = Some(manifest_byte.repeat(32));
        pending.change.size_bytes = size;
        pending
    }

    fn on_demand() -> Landing {
        Landing::OnDemand {
            kept_root: PathBuf::from("/kept"),
        }
    }

    /// The pump plans by the landing policy's want: on an on-demand replica a
    /// sequenced row's body is the nest's to serve on open, so it is never
    /// pulled, priced or metered; the un-sequenced row's body is.
    #[test]
    fn an_on_demand_replica_plans_only_the_unsequenced_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("state.db");
        let page = [
            row("recorded.txt"),
            pending_row("cabin-draft.txt", "ee", 4096),
        ];

        let plan = plan_page_fetch(&db_path, &page, &on_demand(), None);
        assert_eq!(plan.fetches.len(), 1, "only the un-sequenced body");
        assert_eq!(plan.fetches[0].context, "cabin-draft.txt");
        assert!(!plan.storage_limited);

        let resident = plan_page_fetch(&db_path, &page, &Landing::Resident, None);
        assert_eq!(resident.fetches.len(), 2, "a resident tree takes both");
    }

    /// The storage floor at plan time: a wanted body that would take the
    /// device under the floor is not pulled, and the plan says so. Each body
    /// is counted twice (spool + landed), and the page's bodies accumulate.
    #[test]
    fn a_body_that_would_cross_the_storage_floor_is_not_planned() {
        use crate::share_landing::STORAGE_FLOOR_BYTES as FLOOR;
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("state.db");
        let page = [
            pending_row("a.bin", "a1", 1000),
            pending_row("b.bin", "b2", 1000),
        ];

        let roomy = plan_page_fetch(&db_path, &page, &on_demand(), Some(FLOOR + 4000));
        assert_eq!(roomy.fetches.len(), 2);
        assert!(!roomy.storage_limited);

        let one_fits = plan_page_fetch(&db_path, &page, &on_demand(), Some(FLOOR + 3999));
        assert_eq!(one_fits.fetches.len(), 1, "the second body no longer fits");
        assert_eq!(one_fits.fetches[0].context, "a.bin");
        assert!(one_fits.storage_limited);

        let full = plan_page_fetch(&db_path, &page, &on_demand(), Some(FLOOR));
        assert!(full.fetches.is_empty());
        assert!(full.storage_limited);

        let resident = plan_page_fetch(&db_path, &page, &Landing::Resident, Some(0));
        assert_eq!(resident.fetches.len(), 2, "a resident tree has no floor");
        assert!(!resident.storage_limited);
    }

    /// A placeholder at the row's head still owes its wanted body on an
    /// on-demand replica (an earlier pass recorded the row and could not land
    /// the bytes), so the retry plans it again.
    #[test]
    fn an_on_demand_placeholder_at_the_rows_head_is_planned_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("state.db");
        let pending = pending_row("cabin-draft.txt", "ee", 4096);
        let manifest = parse_hash(pending.change.manifest_hash.as_deref().unwrap()).unwrap();
        let db = SyncDb::open(&db_path).unwrap();
        db.upsert_entry(
            "cabin-draft.txt",
            None,
            None,
            Some(manifest),
            crate::db::SyncState::Placeholder,
            0,
            0,
            4096,
            1,
            Some(1),
        )
        .unwrap();

        let page = [pending];
        let plan = plan_page_fetch(&db_path, &page, &on_demand(), None);
        assert_eq!(plan.fetches.len(), 1);

        db.update_state("cabin-draft.txt", crate::db::SyncState::Synced)
            .unwrap();
        let landed = plan_page_fetch(&db_path, &page, &on_demand(), None);
        assert!(landed.fetches.is_empty(), "a landed body is current");
    }
}

#[cfg(test)]
mod advertise_tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;

    fn spec(id: u8) -> SharedSetSpec {
        SharedSetSpec {
            folder: format!("f{id}"),
            folder_id: format!("local:{id}"),
            set_id: [id; 32],
            db_path: PathBuf::from("/nonexistent"),
            body: crate::share_body::TreeBodySource::shared("/nonexistent"),
            landing: Landing::Resident,
            content_keys: FolderContentKeys::genesis([9; 32], 1),
        }
    }

    /// defence in depth, in the SHARED crate: two specs claiming one
    /// `set_id` route NEITHER. The app-side composer is expected to refuse the
    /// pair first, but the composer is per-app glue and this map is where the
    /// mispairing would become a disclosure — so the rule is enforced at the
    /// operation too, for the six apps whose composers do not exist yet.
    #[test]
    fn two_specs_claiming_one_set_route_neither() {
        let mut a = spec(1);
        a.folder = "docs-a".into();
        a.db_path = PathBuf::from("/a");
        let mut b = spec(1); // same set_id
        b.folder = "docs-b".into();
        b.db_path = PathBuf::from("/b");
        let c = spec(2); // distinct set_id, must survive

        let colliding = [a, b, c.clone()];
        let kept: Vec<&SharedSetSpec> = unambiguous_set_specs(&colliding).collect();
        assert_eq!(kept.len(), 1, "the colliding pair must BOTH drop");
        assert_eq!(kept[0].set_id, c.set_id);

        // And the honest baseline: distinct ids all survive.
        let distinct = [spec(1), spec(2), spec(3)];
        let kept: Vec<&SharedSetSpec> = unambiguous_set_specs(&distinct).collect();
        assert_eq!(kept.len(), 3);
    }

    fn eps(addr: &str) -> fauna_core::device_endpoints::DeviceEndpoints {
        fauna_core::device_endpoints::DeviceEndpoints {
            lan_addrs: vec![addr.to_string()],
            ..Default::default()
        }
    }

    fn mark_all(
        state: &mut AdvertiseState,
        due: &[DueAdvertisement],
        endpoints: &fauna_core::device_endpoints::DeviceEndpoints,
        now: std::time::Instant,
    ) {
        for d in due {
            state.mark_sent(d.set_id, endpoints, now);
        }
    }

    /// First pass publishes every set; a same-endpoints pass inside the floor
    /// publishes nothing; a changed endpoint set publishes immediately; and
    /// the floor re-publishes even unchanged endpoints. Marking is the
    /// CALLER'S act, on send success — an unmarked (failed) send stays due.
    #[test]
    fn change_triggered_plus_floor_and_only_a_marked_send_quiets() {
        let own = ActorKeypair::from_secret([5; 32]).actor_id();
        let mut state = AdvertiseState::default();
        let t0 = std::time::Instant::now();
        let specs = [spec(1), spec(2)];

        let first = state.due(&specs, &own, &eps("10.0.0.2:1"), t0);
        assert_eq!(first.len(), 2);
        assert_eq!(
            state.due(&specs, &own, &eps("10.0.0.2:1"), t0).len(),
            2,
            "an UNMARKED (failed) send stays due — marking at build time lost \
             a failed advertisement until the floor"
        );
        mark_all(&mut state, &first, &eps("10.0.0.2:1"), t0);
        assert_eq!(
            state.due(&specs, &own, &eps("10.0.0.2:1"), t0).len(),
            0,
            "marked + unchanged inside the floor: quiet"
        );
        let changed = state.due(&specs, &own, &eps("10.0.0.3:1"), t0);
        assert_eq!(changed.len(), 2, "an address change publishes immediately");
        // The advertisement's node_id is stamped with OUR actor key by the
        // shared builder — a caller cannot advertise a key it does not hold.
        let row: fauna_core::share_endpoints::ShareEndpoints =
            fauna_core::encoding::canonical_decode(&changed[0].bytes).unwrap();
        assert_eq!(row.endpoints.node_id, own.0);
        assert_eq!(row.member_actor, own.0.to_vec());
        mark_all(&mut state, &changed, &eps("10.0.0.3:1"), t0);

        let past_floor = t0 + std::time::Duration::from_secs(ADVERTISE_FLOOR_SECS + 1);
        assert_eq!(
            state
                .due(&specs, &own, &eps("10.0.0.3:1"), past_floor)
                .len(),
            2,
            "the floor re-publishes unchanged endpoints (the late joiner's path)"
        );
    }
}
