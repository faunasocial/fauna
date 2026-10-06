//! The share plane's app **driver** — the loop, the join and the readings
//! every app's share glue used to own privately (`p2p-shared-set-build.md` § Cross-user
//! shared-set transfer → *Built — the tui app leg*).
//!
//! [`crate::share_pump`] is the plane's mechanism: one pull pass, one serve
//! refresh, one advertisement decision. This module is the **composition
//! above it** — bind the seat under rule 7's brake, join the two halves of a
//! spec, hold the last-known specs across a nest-down window, advertise what
//! is due, pull, and publish the pass's result to whatever paints. tui wrote
//! all of that once, in app glue; this is that same code, lifted, so the six
//! apps still owed the plane inherit it rather than re-deriving it.
//!
//! # Why the lift, and why here
//!
//! [`compose_specs`] is the fix. Its arms decide which folder's
//! bytes are served under which peer group's keys, and getting one wrong is
//! a live cross-user disclosure — the flaw the security review found in
//! tui's private composer. A guard re-derived by six app legs
//! is a guard six chances to miss; the same reasoning already moved the
//! duplicate-`set_id` refusal into [`crate::share_pump::refresh_serve_sources`]
//! ("defence in depth, not the primary guard" — this module is the primary
//! guard, and now there is one of it). Priorities #1 and #2 say the same
//! thing from the other side: an app leg should own its shell, never the
//! plane's decisions.
//!
//! # What stays app-shaped
//!
//! Four things genuinely differ per app, and they are exactly
//! [`SharePlaneHost`]'s methods: how the app **binds** its ceremony seat
//! (the device label differs, and each app holds its own seat type for the
//! offline-share panel), how it reads the **live** capability list and the
//! transfer **policy** (its nest client), how it loads this identity's
//! **key bindings**, how it **publishes** an advertisement on a set's own
//! channel (its conversations rail), and the two **nudges** its UI needs.
//! Everything else — the cadence, the brake composition, the join, the
//! last-known-spec hold, the serve refresh, the publish decision, the pull,
//! the readings — is here.
//!
//! # Why the loop starts at store-ready
//!
//! Rule 7's cached brake evidence, the discovery cache's dial targets, the
//! sink's durable write and the transfer ledger all live behind
//! [`AccountStoreHandle`], which an app assembles asynchronously after
//! post-auth. Binding earlier could read only the live brake and still
//! could not pump, so the ready edge IS the earliest honest start; a failed
//! assembly leaves the plane down for the session, the same degraded
//! posture every other store consumer takes. [`run`] exits when the
//! runtime's `data_version` read errs (sign-out's deterministic shutdown),
//! dropping its seat clone with it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fauna_client_capabilities::group_ceremony_node::{
    CeremonyBindVerdict, CeremonyNode, ceremony_bind_verdict,
};
use fauna_core::feature_gate::{EffectivePolicy, GatedFeature, effective_policy};
use fauna_core::folder_keys::FolderEngineKeys;
use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;

use crate::account_runtime::AccountStoreHandle;
use crate::share_body::{BodySource, OnDemandBodySource, TreeBodySource};
use crate::share_landing::Landing;
use crate::share_pump::{
    AdvertiseState, SetPullOutcome, ShareIngestDoor, ShareIngestSummary, SharedSetSpec, pull_pass,
    refresh_serve_sources,
};

/// How often the pump refreshes serve sources, advertises due endpoints and
/// pulls from cached dial targets. Latency here is a courtesy: the
/// nest-mediated path stays the always-on source, and a missed pass is
/// retried on the next tick.
pub const DEFAULT_PUMP_SECS: u64 = 60;

/// The pump cadence in seconds: `raw` when it parses to a positive integer,
/// else [`DEFAULT_PUMP_SECS`] — the `FAUNA_CONV_POLL_SECS` pattern
/// (`fauna_conversations::session`), so a tier_3 journey waits on the
/// mechanism, never on a production timer (e2e convention 14).
pub fn resolve_pump_secs(raw: Option<String>) -> u64 {
    raw.and_then(|v| v.parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .unwrap_or(DEFAULT_PUMP_SECS)
}

/// [`resolve_pump_secs`] over this process's `FAUNA_SHARE_PUMP_SECS` — what
/// an app passes to [`SharePlane::pump_interval`] unless it is a test
/// injecting its own.
pub fn pump_interval_from_env() -> std::time::Duration {
    std::time::Duration::from_secs(resolve_pump_secs(
        std::env::var("FAUNA_SHARE_PUMP_SECS").ok(),
    ))
}

/// One bound folder's replica as the plane uses it — the serve half of a
/// spec: where its state DB lives, where its bodies are read from, and how
/// pulled bodies land. The desktops read it from the agent
/// (`GetShareServeInfo`, a bound tree); an on-demand host answers with its
/// two roots.
#[derive(Debug, Clone)]
pub struct ServeFolderInfo {
    pub folder: String,
    /// The set's `FolderRef` wire form — the join key (names are unique only
    /// per owner). Every binding carries one since the 2026-09-24
    /// compat-remnant sweep retired the name-keyed binding.
    pub folder_id: String,
    /// Where the serve half reads this replica's plaintext.
    pub body: Arc<dyn BodySource>,
    /// How this replica lands pulled bodies.
    pub landing: Landing,
    pub db_path: PathBuf,
}

/// The serve-info seam (the replica host's adapter in production; stubbed in
/// tests).
#[async_trait::async_trait]
pub trait ShareServeInfoSource: Send + Sync {
    async fn serve_info(&self) -> Result<Vec<ServeFolderInfo>, String>;
}

/// The **replica access** — the two seams the plane rides, whoever hosts the
/// replica: serve info + the state writer's ingest door
/// (`p2p-shared-set-build.md` § *Phone peers — design*, decision 1). Two
/// constructions: the sync agent's ([`agent_share_access`], every desktop)
/// and the on-demand host's ([`on_demand_share_access`], a phone's provider).
#[derive(Clone)]
pub struct ReplicaAccess {
    pub info: Arc<dyn ShareServeInfoSource>,
    pub door: Arc<dyn ShareIngestDoor>,
}

/// The convergence loop's **bearer hook** over an app's shared
/// [`fauna_nest_http::BearerSource`] (the WS-handshake bearer cache —
/// auto-refreshing, so each tick pushes a currently-valid token).
///
/// Same rule as [`AgentShareInfo`] below: every app that drives the external
/// `fauna-sync-agent` shares this adapter rather than writing its own. tui and
/// linux each had a byte-identical private copy, and the four apps still owed
/// sync-agent provisioning would each have written a fifth.
///
/// The pushed expiry is the source's own
/// [`fauna_nest_http::BearerSource::bearer_with_expiry`] — the deadline on this
/// machine's clock, anchored at receipt (`login.md` § Token lifetime on the
/// client's clock), which the agent on the same machine plans its renewal on.
/// A bearer the source refuses to produce reads as *not authenticated*, and the
/// tick skips — the same fold both app copies made. So does a bearer whose
/// source **cannot say** its expiry: the agent is never handed a deadline this
/// seam would have to guess.
pub struct AgentBearerSource(pub Arc<dyn fauna_nest_http::BearerSource>);

impl fauna_client_sync::agent::ProvisioningBearerSource for AgentBearerSource {
    fn current_bearer(
        &self,
    ) -> impl std::future::Future<Output = Option<fauna_client_sync::agent::ProvisioningBearerToken>>
    + Send {
        let source = Arc::clone(&self.0);
        async move {
            let (token, expires_at) = source.bearer_with_expiry().await.ok()?;
            let Some(expires_at) = expires_at else {
                // A wiring defect, not a runtime state: every app's bearer
                // source mints its own bearer and publishes its expiry. Loud,
                // because the skipped tick is otherwise invisible.
                tracing::warn!(
                    "sync agent: bearer source publishes no expiry — tick skipped, agent not provisioned"
                );
                return None;
            };
            Some(fauna_client_sync::agent::ProvisioningBearerToken::new(
                token, expires_at,
            ))
        }
    }
}

/// The serve-info seam over the shared agent control client
/// (`GetShareServeInfo`) — every app that drives the external
/// `fauna-sync-agent` (tui, linux, windows) shares this adapter rather than
/// writing its own; path resolution stays the agent's one authority.
pub struct AgentShareInfo<R, B>(pub Arc<fauna_client_sync::agent::SyncAgentProvisioner<R, B>>)
where
    R: fauna_protocol::RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: fauna_client_sync::agent::ProvisioningBearerSource + Send + Sync + 'static;

#[async_trait::async_trait]
impl<R, B> ShareServeInfoSource for AgentShareInfo<R, B>
where
    R: fauna_protocol::RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: fauna_client_sync::agent::ProvisioningBearerSource + Send + Sync + 'static,
{
    async fn serve_info(&self) -> Result<Vec<ServeFolderInfo>, String> {
        Ok(self
            .0
            .share_serve_info()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|i| ServeFolderInfo {
                folder: i.folder,
                folder_id: i.folder_id,
                // The agent's replica is a bound tree: its reply names the
                // watch dir, and every accepted body lands there.
                body: TreeBodySource::shared(i.watch_dir),
                landing: Landing::Resident,
                db_path: i.db_path.into(),
            })
            .collect())
    }
}

/// The engine's provisional-ingest door over the shared agent control client
/// (`ShareIngest`) — the agent arm of [`ShareIngestDoor`], which every
/// desktop takes. A replica hosted on demand takes [`OnDemandIngestDoor`]
/// instead: a phone keeps no agent, and its replica's one writer is its
/// provider's host.
pub struct AgentIngestDoor<R, B>(pub Arc<fauna_client_sync::agent::SyncAgentProvisioner<R, B>>)
where
    R: fauna_protocol::RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: fauna_client_sync::agent::ProvisioningBearerSource + Send + Sync + 'static;

#[async_trait::async_trait]
impl<R, B> ShareIngestDoor for AgentIngestDoor<R, B>
where
    R: fauna_protocol::RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: fauna_client_sync::agent::ProvisioningBearerSource + Send + Sync + 'static,
{
    async fn ingest(
        &self,
        folder: &str,
        folder_id: &str,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &std::path::Path,
    ) -> anyhow::Result<crate::share_pump::ShareIngestSummary> {
        let outcome = self
            .0
            .share_ingest(
                folder.to_string(),
                folder_id.to_string(),
                proven_actor_hex.to_string(),
                rows,
                spool_dir.to_string_lossy().into_owned(),
            )
            .await?;
        for (path, reason) in &outcome.skipped {
            tracing::debug!(
                path = %fauna_core::log_redact::log_path(path),
                %reason,
                "share ingest: materialization skipped this page"
            );
        }
        Ok(crate::share_pump::ShareIngestSummary {
            refused: outcome.refused,
            overlaid: outcome.overlaid,
            materialized: outcome.materialized,
            already_current: outcome.already_current,
            cursor: outcome.cursor,
            // A bound tree lands every body: the agent has no floor to report.
            storage_limited: false,
        })
    }
}

/// The replica access over one sync-agent provisioner — what an agent-driving
/// app hands [`SharePlane::replica`].
pub fn agent_share_access<R, B>(
    provisioner: Arc<fauna_client_sync::agent::SyncAgentProvisioner<R, B>>,
) -> ReplicaAccess
where
    R: fauna_protocol::RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: fauna_client_sync::agent::ProvisioningBearerSource + Send + Sync + 'static,
{
    ReplicaAccess {
        info: Arc::new(AgentShareInfo(Arc::clone(&provisioner))),
        door: Arc::new(AgentIngestDoor(provisioner)),
    }
}

/// One on-demand replica, as its host describes it to the plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDemandReplica {
    /// The nest folder name — the label the transfer surface shows.
    pub folder: String,
    /// The set's `FolderRef` wire form — the join and routing key.
    pub folder_id: String,
    /// The set's state DB (`fsid-<ref>.db`).
    pub db_path: PathBuf,
    /// Bodies the nest may not hold yet (the engine's root).
    pub kept_root: PathBuf,
    /// Recorded bodies, which the OS may reclaim.
    pub cache_root: PathBuf,
}

/// One on-demand host — the one writer of one set's replica on this device —
/// as the plane reaches it. Implemented by the platform's replica host (the
/// FFI's file-provider host: in this process on android; on iOS the app
/// reaches its File Provider extension through the same two operations).
#[async_trait::async_trait]
pub trait OnDemandReplicaHost: Send + Sync {
    /// The replica this host serves, or `None` while it has none to offer
    /// (its engine is not built — a refused binding — or it owns no tree).
    async fn replica(&self) -> Option<OnDemandReplica>;

    /// The host's ingest door: land one page of accepted peer rows
    /// ([`crate::provider_face::owned_tree::OwnedTree::share_ingest`] on the
    /// host's own worker — the twin of the agent's `ShareIngest` command arm).
    async fn share_ingest(
        &self,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &std::path::Path,
    ) -> anyhow::Result<ShareIngestSummary>;
}

/// The on-demand hosts of one account in this process — the registry the
/// platform's provider and the plane both reach (one host per set, one writer
/// per set). Read afresh at every pass: hosts come and go with the provider.
pub trait OnDemandHosts: Send + Sync {
    fn hosts(&self) -> Vec<Arc<dyn OnDemandReplicaHost>>;
}

/// The serve-info seam over the on-demand hosts: each replica's state DB, a
/// body source over its two roots, and the by-policy landing.
pub struct OnDemandShareInfo(pub Arc<dyn OnDemandHosts>);

#[async_trait::async_trait]
impl ShareServeInfoSource for OnDemandShareInfo {
    async fn serve_info(&self) -> Result<Vec<ServeFolderInfo>, String> {
        let mut out = Vec::new();
        for host in self.0.hosts() {
            let Some(replica) = host.replica().await else {
                continue;
            };
            // The body source's own read connection (a cross-process WAL read
            // on iOS, a second connection in-process on android).
            let db = match crate::db::SyncDb::open(&replica.db_path) {
                Ok(db) => db,
                Err(e) => {
                    tracing::warn!(
                        folder = %fauna_core::log_redact::log_folder_name(&replica.folder),
                        error = %e,
                        "share plane: an on-demand replica's state DB will not open; \
                         set unserved this pass"
                    );
                    continue;
                }
            };
            out.push(ServeFolderInfo {
                folder: replica.folder,
                folder_id: replica.folder_id,
                body: Arc::new(OnDemandBodySource::new(
                    replica.kept_root.clone(),
                    replica.cache_root,
                    db,
                )),
                landing: Landing::OnDemand {
                    kept_root: replica.kept_root,
                },
                db_path: replica.db_path,
            });
        }
        Ok(out)
    }
}

/// The ingest door over the on-demand hosts — the on-demand arm of
/// [`ShareIngestDoor`]: the page goes to the host that holds the set, routed
/// by `FolderRef`, never by name.
pub struct OnDemandIngestDoor(pub Arc<dyn OnDemandHosts>);

#[async_trait::async_trait]
impl ShareIngestDoor for OnDemandIngestDoor {
    async fn ingest(
        &self,
        folder: &str,
        folder_id: &str,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &std::path::Path,
    ) -> anyhow::Result<ShareIngestSummary> {
        for host in self.0.hosts() {
            if host
                .replica()
                .await
                .is_some_and(|r| r.folder_id == folder_id)
            {
                return host.share_ingest(proven_actor_hex, rows, spool_dir).await;
            }
        }
        anyhow::bail!(
            "no on-demand host holds folder {}",
            fauna_core::log_redact::log_folder_name(folder)
        )
    }
}

/// The replica access over this account's on-demand hosts — what a phone's
/// shell hands [`SharePlane::replica`].
pub fn on_demand_share_access(hosts: Arc<dyn OnDemandHosts>) -> ReplicaAccess {
    ReplicaAccess {
        info: Arc::new(OnDemandShareInfo(Arc::clone(&hosts))),
        door: Arc::new(OnDemandIngestDoor(hosts)),
    }
}

/// The one-line `share-serve-status` reading — rule-5 transparency: is this
/// device serving shared sets to peers, and why not when it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServeStatus {
    /// No bound cross-user set — nothing to serve, no listener bound for
    /// serving (rule 5's no-unconditional-bind). Also the honest reading
    /// before the first pass has answered.
    #[default]
    NoSets,
    /// The brake refused: no `p2p-share` token live or cached.
    BrakeRefused,
    /// The device's own switch is off (`p2p.md` § Per-device
    /// participation): no seat, nothing served, nothing pulled.
    ParticipationOff,
    /// The seat is bound and this many sets are routed on the serve router.
    Serving(usize),
}

/// What the transfer surface paints. The pump writes it once per pass; a
/// paint locks it only for a synchronous clone (e2e convention 11 — paint
/// does no I/O).
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct SharePlaneState {
    pub status: ServeStatus,
    /// The latest pass's per-(set × peer) outcomes — the
    /// `share-transfer-item` rows.
    pub outcomes: Vec<SetPullOutcome>,
}

/// The cell an app's paint reads and the pump writes.
pub type SharePlaneCell = Arc<Mutex<SharePlaneState>>;

/// The app-shaped seams the shared driver composes over. Everything a leg
/// implements, and nothing it decides.
#[async_trait::async_trait]
pub trait SharePlaneHost: Send + Sync + 'static {
    /// The app's own ceremony-seat type — it holds the panel's seat too, so
    /// the driver never imposes a shape on it.
    type Seat: Send + Sync + 'static;

    /// The M2 roster consult (`fauna_client_folders::MlsSetMembership` over
    /// the app's live MLS engine): who this set admits, asked per request.
    fn membership(&self) -> Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync>;

    /// This session's ONE actor-keyed seat, through the slot the panel's own
    /// bind shares (`offline_share::bind_share_plane_seat` over the app's
    /// `SessionSeat`): bound under `evidence`'s verdict when no door has bound
    /// it yet, handed back when the panel already did — and lent
    /// `membership`, the roster this driver holds for its whole loop, and
    /// `group_roster`, the peer witness door's evaluator this driver refreshes
    /// from the account store every pass. Returns
    /// the seat and its bound socket addresses (the publish half crosses them
    /// with the interface list). ⚠ The transport's relay stays `None` today —
    /// by ruling, until the nest's relay serves address discovery; the
    /// ceremony's own dials carry `relay_available: false` by construction,
    /// so a relay on the seat hands it no dial path (`p2p.md` § The relay →
    /// *The cross-user seat and the relay*).
    async fn bind_seat(
        &self,
        evidence: Option<Vec<String>>,
        membership: &Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync>,
        group_roster: &Arc<dyn fauna_peer_share::admission::GroupRosterState + Send + Sync>,
    ) -> Result<(Arc<Self::Seat>, Vec<std::net::SocketAddr>), String>;

    /// The listener inside the app's seat.
    fn seat_node<'a>(&self, seat: &'a Self::Seat) -> &'a CeremonyNode;

    /// This sign-in's seat slot — the same one [`Self::bind_seat`] binds
    /// through. The driver records the device's participation verdict on it
    /// every pass and unbinds it when the switch is off (`p2p.md` § Per-device
    /// participation), so no host gates a listener itself.
    fn session_seat(&self) -> &crate::offline_share::SessionSeat;

    /// The seat is serving the plane now — nudge the panel's repaint. Nothing
    /// is handed over: the panel reads its seat from the same `SessionSeat`
    /// this bind went through, so there is no fold for an app to get wrong.
    fn seat_bound(&self);

    /// The cell changed — nudge a repaint. Called only when the pass
    /// actually moved the state.
    fn state_changed(&self);

    /// Rule 7's LIVE half: `fauna.nest.info`'s capability list when the nest
    /// answers. An `Err` is *no evidence*, never optimism.
    async fn live_capabilities(&self) -> Result<Vec<String>, String>;

    /// The `p2p-share.transfer` policy when the nest answers; `None` falls
    /// back to the tier-1 constants (`dynamic-features.md` § Evaluation
    /// points — this verdict is client-side by design).
    async fn transfer_policy(&self) -> Option<EffectivePolicy>;

    /// This identity's engine key bindings (the folder-keys custody) — the join's other
    /// half, and the read that fails in exactly the offline window the plane
    /// exists for (which is why [`run`] holds the last-known specs).
    async fn key_bindings(&self) -> Result<Vec<FolderEngineKeys>, String>;

    /// Publish one advertisement on the set's OWN channel.
    async fn send_share_endpoints(&self, channel_hex: &str, bytes: Vec<u8>) -> Result<(), String>;
}

/// Rule 7's LIVE half, read through the app's nest connection — the body
/// every leg's [`SharePlaneHost::live_capabilities`] is, so no leg decides
/// how a `fauna.nest.info` failure reads. An `Err` here is *no evidence*
/// (the caller's `.ok()`), never optimism.
///
/// A free function rather than a default method: the trait cannot reach a
/// leg's nest handle, and a leg's own field is the only thing that differs.
pub async fn live_capabilities(nest: Arc<fauna_client::NestClient>) -> Result<Vec<String>, String> {
    fauna_client_features::FeaturesClient::new(nest)
        .node_capabilities()
        .await
        .map_err(|e| e.to_string())
}

/// The `p2p-share` transfer policy, read through the app's nest connection —
/// the body every leg's [`SharePlaneHost::transfer_policy`] is. `None`
/// (unreachable nest, or the feature simply absent from the status) falls
/// back to the tier-1 constants, `dynamic-features.md` § Evaluation points.
pub async fn transfer_policy(nest: Arc<fauna_client::NestClient>) -> Option<EffectivePolicy> {
    fauna_client_features::FeaturesClient::new(nest)
        .status()
        .await
        .ok()?
        .features
        .into_iter()
        .find(|i| i.feature == GatedFeature::P2pShare)
        .map(|i| i.policy)
}

/// A pass's opening decision, taken before the seat exists and before any
/// network I/O: is there anything to serve, and does rule 7's brake admit?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassGate {
    /// No bound cross-user set — nothing to serve, and no listener bound for
    /// serving (rule 5's no-unconditional-bind).
    NoSets,
    /// The brake refused: no `p2p-share` token, live or cached.
    BrakeRefused,
    /// Bind (if not bound yet) and pump.
    Proceed,
}

/// [`PassGate`] for one pass. Ordered deliberately: **no sets short-circuits
/// before the brake is even consulted**, because a device with nothing to
/// serve must not bind a listener whatever the nest says, and the surface
/// should read "no shared folders" rather than "peer transfers are off" —
/// two different facts, and only one of them is about this device.
pub fn pass_gate(has_specs: bool, evidence: Option<&[String]>) -> PassGate {
    if !has_specs {
        return PassGate::NoSets;
    }
    match ceremony_bind_verdict(evidence) {
        CeremonyBindVerdict::Bind => PassGate::Proceed,
        _ => PassGate::BrakeRefused,
    }
}

/// Which specs this pass pumps, and what it remembers.
///
/// **The pump must not need the nest to pump** — the spec's key-bindings half
/// loads the folder-keys custody, and the OFFLINE pass is exactly the
/// scenario the plane exists for (found by the two-actor journey's first
/// offline phase: the pump sat out the whole nest-down window). So a fresh
/// read wins and refreshes the memory; a failed read pumps the LAST-KNOWN
/// specs; a failed read with nothing remembered skips the pass.
///
/// Serving stale specs is safe by construction: admission still consults the
/// live M2 roster per request, the transfer gate prices every page, and a
/// stale content-key generation fails closed at the seal/open.
fn specs_for_pass(
    fresh: Result<Vec<SharedSetSpec>, String>,
    last_known: &mut Vec<SharedSetSpec>,
) -> Option<Vec<SharedSetSpec>> {
    match fresh {
        Ok(specs) => {
            *last_known = specs.clone();
            Some(specs)
        }
        Err(e) if !last_known.is_empty() => {
            tracing::debug!("share plane: spec read failed; pumping the last-known specs: {e}");
            Some(last_known.clone())
        }
        Err(e) => {
            tracing::debug!("share plane: spec read failed; next pass retries: {e}");
            None
        }
    }
}

/// Everything the driver composes over, gathered by the app at its
/// store-ready edge.
pub struct SharePlane<H: SharePlaneHost> {
    pub host: Arc<H>,
    pub account: AccountStoreHandle,
    /// The replica host's two seams — the agent's or the on-demand hosts'.
    pub replica: ReplicaAccess,
    /// This actor's id — the advertisement's `node_id` and the publish
    /// decision's key.
    pub own_actor: ActorId,
    pub cell: SharePlaneCell,
    /// Spool ground for pulled bodies (`spool/manifests/<hex>` +
    /// `spool/chunks/<hex>` under here). Transient by contract: a lost body
    /// skips its row and the next pass re-pulls.
    pub spool_root: PathBuf,
    /// The pass cadence — [`pump_interval_from_env`] in production.
    pub pump_interval: std::time::Duration,
}

/// Stop a running plane: end `driver` (the task running [`run`]) and unbind
/// the session's seat — the `unbind` the participation switch uses. **A
/// stopped plane holds no socket** (`p2p-shared-set-build.md` § *Phone peers —
/// design*, decision 2: a foreground peer stops, it does not pause): the
/// driver's own seat clone drops with its task, which is awaited here, and
/// the slot's was the other handle. Starting again binds afresh; the pull
/// cursor and the overlay are at rest in the set's state DB, so a transfer
/// the stop cut resumes with nothing re-sent.
pub async fn stop_plane(
    driver: tokio::task::JoinHandle<()>,
    session_seat: &crate::offline_share::SessionSeat,
) {
    driver.abort();
    // Cancelled is the expected answer; either way the task's locals are gone.
    let _ = driver.await;
    session_seat.unbind().await;
}

/// The whole plane, as one task. Every pass is best-effort and the next tick
/// retries; the loop ends when the account runtime does — **at** the runtime's
/// shutdown, as a `select!` arm over [`AccountStoreHandle::closed`], not at the
/// next tick: `membership` below holds the conversations rail's MLS engine,
/// whose one-engine-per-store lock the incoming session of an account switch
/// (or the post-succession sweep retry) opens next, and the production 60 s
/// cadence is exactly the window in which that open was refused
/// (`account-scoping.md` § Implementation status → the `tui (in-memory)`
/// ledger row, 2026-08-27). The seat holds that roster only as the `Weak`
/// [`SharePlaneHost::bind_seat`] lends it, so ending this loop releases the
/// engine even while the panel keeps the session's seat.
pub async fn run<H: SharePlaneHost>(plane: SharePlane<H>) {
    let mut ticker = tokio::time::interval(plane.pump_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seat: Option<(Arc<H::Seat>, Vec<std::net::SocketAddr>)> = None;
    let mut advertise = AdvertiseState::default();
    let membership = plane.host.membership();
    // The peer witness door's evaluator: empty (refusing every group witness)
    // until a pass has read the store, then replaced wholesale each pass from
    // this replica's own held group scopes (`crate::group_roster_door`). The
    // seat holds it only as the `Weak` the bind lends, like `membership`.
    let live_group_roster = Arc::new(crate::group_roster_door::LiveGroupRoster::default());
    let group_roster = Arc::clone(&live_group_roster)
        as Arc<dyn fauna_peer_share::admission::GroupRosterState + Send + Sync>;
    // The last successful spec read. The spec's key-bindings half loads
    // the folder-keys custody, and the OFFLINE pass — the scenario the
    // plane exists for — is exactly when that read fails; found by the
    // two-actor journey's first offline phase (the pump sat out the whole
    // nest-down window). Serving the last-known specs is safe: admission
    // still consults the live M2 roster per request, the transfer gate
    // prices every page, and a stale content-key generation fails closed at
    // the seal/open. Refreshed whenever the nest answers again.
    let mut last_specs: Vec<SharedSetSpec> = Vec::new();
    // The device's own switch: a toggle wakes the loop at once, so the seat
    // drops (or the next bind is admitted) within the pass it triggers
    // rather than a whole cadence later.
    let mut participation_changed = plane.account.p2p_participation_watch();

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = participation_changed.changed() => {}
            // The runtime's shutdown is an event, not a tick: end now, and
            // with it let go of the engine `membership` holds (the doc above).
            _ = plane.account.closed() => break,
        }

        // Runtime gone (sign-out's deterministic shutdown, or teardown): the
        // driver dies with it, and dropping our seat clone releases the
        // listener as soon as the app's own panel state lets go of its own.
        // Kept beside the `select!` arm for the tick that races the shutdown.
        if plane.account.data_version().await.is_err() {
            break;
        }

        // The device's own participation, before anything else this pass
        // (`p2p.md` § Per-device participation): the verdict goes onto the
        // session's seat slot, where BOTH bind doors read it, and off takes
        // a bound seat down — ours and the slot's, the last handles that
        // hold the listener. A row the runtime cannot answer keeps the last
        // verdict rather than flipping a listener on a read failure.
        if let Ok(row) = plane.account.p2p_participation().await {
            let on = row.effective();
            plane.host.session_seat().set_participation(Some(on));
            if !on {
                let held = seat.take().is_some();
                let slot = plane.host.session_seat().unbind().await;
                if held || slot {
                    tracing::info!(
                        "share plane: participation is off on this device — seat dropped"
                    );
                }
                update_cell(&plane, ServeStatus::ParticipationOff, Vec::new());
                continue;
            }
        }

        let Some(specs) = specs_for_pass(load_specs(&plane).await, &mut last_specs) else {
            continue;
        };

        if seat.is_none() {
            // The brake is read only when there is something to serve — the
            // `NoSets` arm never touches the nest (`pass_gate`'s own rule).
            let evidence = if specs.is_empty() {
                None
            } else {
                composed_evidence(
                    plane.host.live_capabilities().await,
                    plane
                        .account
                        .cached_nest_capabilities()
                        .await
                        .ok()
                        .flatten(),
                )
            };
            // A refusal shapes only the cell: rule 7 says no evidence refuses,
            // and the status line says so rather than hiding the plane.
            match pass_gate(!specs.is_empty(), evidence.as_deref()) {
                PassGate::NoSets => {
                    update_cell(&plane, ServeStatus::NoSets, Vec::new());
                    continue;
                }
                PassGate::BrakeRefused => {
                    update_cell(&plane, ServeStatus::BrakeRefused, Vec::new());
                    continue;
                }
                PassGate::Proceed => {}
            }
            match plane
                .host
                .bind_seat(evidence, &membership, &group_roster)
                .await
            {
                Ok((bound_seat, bound_addrs)) => {
                    plane.host.seat_bound();
                    seat = Some((bound_seat, bound_addrs));
                }
                Err(e) => {
                    tracing::debug!("share plane: seat bind failed; next pass retries: {e}");
                    continue;
                }
            }
        }
        let Some((bound_seat, bound_addrs)) = seat.as_ref() else {
            continue;
        };
        let node = plane.host.seat_node(bound_seat);

        refresh_group_roster(&plane.account, &live_group_roster).await;
        let routed = refresh_serve_sources(node, &specs);

        // Everything got unshared while the seat stayed bound. The refresh
        // above already unrouted the serve map (it is rebuilt from `specs`
        // every pass, so an empty read serves nothing) — what is left is to
        // say so: `Serving 0 shared folders` is a reading no user should ever
        // be shown, because "none" is `NoSets`'s whole sentence. Nothing to
        // advertise and nothing to pull either, so the pass ends here.
        if specs.is_empty() {
            update_cell(&plane, ServeStatus::NoSets, Vec::new());
            continue;
        }

        // The publish half. The composition mirrors the same-account leg's
        // published facts, except relay: this transport is bound with NO
        // relay until the nest's relay serves address discovery (`p2p.md`
        // § The relay → *The cross-user seat and the relay*), so advertising
        // one would name a rendezvous nothing answers.
        let lan_ips = crate::share_pump::discover_lan_candidates();
        let endpoints = fauna_core::device_endpoints::DeviceEndpoints {
            node_id: plane.own_actor.0,
            lan_addrs: fauna_core::device_endpoints::lan_socket_addrs(bound_addrs, &lan_ips),
            public_addrs: Vec::new(),
            relay_url: None,
        };
        let now = std::time::Instant::now();
        for due in advertise.due(&specs, &plane.own_actor, &endpoints, now) {
            match plane
                .host
                .send_share_endpoints(&due.channel_hex, due.bytes)
                .await
            {
                // Mark only a send that REACHED the channel — a failed one
                // stays due and the next pass retries immediately
                // (`AdvertiseState::mark_sent`'s own doc owns why).
                Ok(()) => advertise.mark_sent(due.set_id, &endpoints, now),
                // Every failure retries on the next pass, terminal and
                // retryable alike, with no backoff. The classification IS
                // available — a `fauna.conversations.rate_limited` refusal from
                // the nest's per-(actor, channel) commit cap arrives as
                // `Transient` — but the gate below collapses every non-stale
                // refusal into one arm before it reaches here
                // (`fauna-client-mls-sync/src/commit_gate.rs`). Harmless while
                // that limiter does not extend its own lockout under retry;
                // build the backoff here if it ever does.
                Err(e) => {
                    tracing::debug!(
                        "share plane: advertisement send failed; next pass retries: {e}"
                    )
                }
            }
        }

        // The pull half, priced through the one shared feature verdict —
        // live policy when the nest answers, tier-1 constants otherwise.
        let live_policy = plane.host.transfer_policy().await;
        // The byte gate's nest fact (`p2p.md` § The relay, ruling 4): the
        // nest answered this pass's policy read, so its path carries the
        // bodies a relayed connection leaves. An answer without the feature
        // reads as unavailable — the side that keeps today's delivery.
        let nest = if live_policy.is_some() {
            fauna_transport::NestPath::Reachable
        } else {
            fauna_transport::NestPath::Unavailable
        };
        let policy =
            live_policy.unwrap_or_else(|| effective_policy(GatedFeature::P2pShare, &[], &[]));
        let today = fauna_core::day_bucket::local_day_bucket(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            0,
        );
        let outcomes = pull_pass(
            node,
            &plane.account,
            &membership,
            plane.replica.door.as_ref(),
            &specs,
            &plane.spool_root,
            &lan_ips,
            &policy,
            today,
            nest,
        )
        .await;

        update_cell(&plane, ServeStatus::Serving(routed), outcomes);
    }
}

/// Rebuild the peer witness door's evaluator from this replica's store — the
/// pump side of the door (`account-data-plane.md` § The admission seam: a
/// pump-fed `GroupRosterState`). A failed read CLEARS the evaluator rather
/// than keeping the last snapshot: a snapshot that can no longer be refreshed
/// can no longer learn a removal or a revoked authority device, so it stops
/// vouching until the next pass reads the store again.
async fn refresh_group_roster(
    account: &AccountStoreHandle,
    live: &crate::group_roster_door::LiveGroupRoster,
) {
    match account.group_roster_snapshot().await {
        Ok(snapshot) => live.replace(snapshot),
        Err(e) => {
            tracing::debug!("share plane: group roster read failed; refusing group witnesses: {e}");
            live.clear();
        }
    }
}

/// Write the pass's result into the paint cell, nudging a repaint only when
/// something actually changed (the `SyncAgentChanged` posture).
fn update_cell<H: SharePlaneHost>(
    plane: &SharePlane<H>,
    status: ServeStatus,
    outcomes: Vec<SetPullOutcome>,
) {
    let changed = {
        let mut cell = plane.cell.lock().unwrap();
        let changed = cell.status != status || cell.outcomes != outcomes;
        cell.status = status;
        cell.outcomes = outcomes;
        changed
    };
    if changed {
        plane.host.state_changed();
    }
}

/// The bound cross-user sets, joined from the two seams that each hold half:
/// the serve side knows where a folder's state DB and tree live, the
/// identity holder's folder-keys custody knows which folders are MLS-bound sets and
/// holds their content keys.
async fn load_specs<H: SharePlaneHost>(
    plane: &SharePlane<H>,
) -> Result<Vec<SharedSetSpec>, String> {
    let serve = plane.replica.info.serve_info().await?;
    if serve.is_empty() {
        return Ok(Vec::new());
    }
    Ok(compose_specs(serve, &plane.host.key_bindings().await?))
}

/// Join the serve info with the identity holder's key bindings: only an
/// MLS-bound set with resolvable content keys is a cross-user set the plane
/// serves (an unbound folder has no roster to admit against, and
/// bound-but-keyless is the resolver's fail-closed arm).
///
/// The join is by `FolderRef` ONLY: a serve row matches the binding
/// wearing the same ref, never one wearing the same name — names are unique
/// only per owner (`fauna_core::folder_keys::FolderEngineKeys::folder`'s own
/// warning). The name-only pairing a pre-identity binding took was retired
/// 2026-09-24 (the compat-remnant sweep): both sides are required to carry the
/// ref. A serve list can still report two rows for one set (two locations
/// bound to it), which makes the SERVE side the duplicating one. Any two rows resolving to one
/// `set_id` therefore serve NEITHER: the silent last-write-wins downstream
/// ([`refresh_serve_sources`]' map) is what turned a mispair into serving one
/// folder's bytes under another's keys.
pub fn compose_specs(
    serve: Vec<ServeFolderInfo>,
    bindings: &[FolderEngineKeys],
) -> Vec<SharedSetSpec> {
    let mut specs: Vec<SharedSetSpec> = Vec::new();
    for info in serve {
        let binding = bindings
            .iter()
            .find(|b| b.folder_id == info.folder_id && b.mls_group_id.is_some());
        let Some(binding) = binding else {
            continue; // not a shared set — nothing to serve
        };
        let (Some(group_id), Some(content_keys)) = (&binding.mls_group_id, &binding.content_keys)
        else {
            continue; // bound-but-keyless: the resolver's fail-closed arm
        };
        specs.push(SharedSetSpec {
            folder: info.folder,
            folder_id: binding.folder_id.clone(),
            set_id: fauna_mls::types::ChannelId::from_group_id(group_id).0,
            db_path: info.db_path,
            body: info.body,
            landing: info.landing,
            content_keys: content_keys.clone(),
        });
    }
    // The serve-side duplicate guard: two rows resolving to one set pair one
    // set's keys with (at least one) wrong folder's bytes, and which row wins
    // downstream is insertion order — so neither may serve.
    let mut counts: std::collections::HashMap<[u8; 32], usize> = std::collections::HashMap::new();
    for spec in &specs {
        *counts.entry(spec.set_id).or_default() += 1;
    }
    specs.retain(|spec| {
        let unique = counts[&spec.set_id] == 1;
        if !unique {
            tracing::warn!(folder = %fauna_core::log_redact::log_folder_name(&spec.folder),
                "share plane: several serve rows resolve to this set; serving none of \
                 them rather than whichever registered last");
        }
        unique
    });
    specs
}

/// Rule 7's evidence composition: the live `fauna.nest.info` read when the
/// nest answers, else the peer leg's cached last-known capabilities. `None`
/// (no cache either) reaches the shared verdict, which refuses — no evidence
/// never binds.
pub fn composed_evidence(
    live: Result<Vec<String>, String>,
    cached: Option<Vec<String>>,
) -> Option<Vec<String>> {
    live.ok().or(cached)
}

/// The `fauna.state.share-endpoints` sink body — the discovery carriage's
/// durable half (`p2p-shared-set-build.md` § Built — the discovery carriage): decode, bind the
/// claim to the MLS-authenticated sender (refuse-not-repair), land the row
/// through the account store. `false` on refusal OR write failure — both mean
/// "no candidate this replica may dial", and neither ever stalls the walk.
///
/// Each app's `ShareEndpointsSink` impl is the two lines that call this: the
/// trait lives in the conversations rail, the decision does not.
pub async fn accept_share_advertisement(
    account: &AccountStoreHandle,
    channel_hex: &str,
    sender: ActorId,
    bytes: &[u8],
) -> bool {
    let Ok(advertised) = fauna_core::encoding::canonical_decode::<
        fauna_core::share_endpoints::ShareEndpoints,
    >(bytes) else {
        return false;
    };
    let Ok(channel) = fauna_mls::types::ChannelId::from_hex(channel_hex) else {
        return false;
    };
    match fauna_peer_share::bind_share_advertisement(&advertised, sender, &channel.0) {
        Ok((_key, row)) => match account.put_share_endpoints(row).await {
            Ok(_) => true,
            Err(e) => {
                // `:#` — the whole context chain. The plain `{e}` printed only
                // the outermost context, which for a refused `GenerationTip`
                // write is the standing R14 (account-data-plane.md § The ratified decisions) refusal text with the actual
                // failure (mint refused: no escrow target / holder / door)
                // invisible — measured 2026-08-24 diagnosing the share
                // journey's linux red.
                tracing::warn!("share sink: dial row not persisted: {e:#}");
                false
            }
        },
        Err(refusal) => {
            tracing::debug!(?refusal, "share sink: advertisement refused");
            false
        }
    }
}

/// The `share-serve-status` reading for a state — resolved by each app's own
/// i18n pipeline (the shared-label pattern).
pub fn serve_status_label(status: ServeStatus) -> LocalizedText {
    match status {
        ServeStatus::NoSets => LocalizedText::key("folders.share_serve_status_no_sets"),
        ServeStatus::BrakeRefused => LocalizedText::key("folders.share_serve_status_off"),
        ServeStatus::ParticipationOff => {
            LocalizedText::key("folders.share_serve_status_participation_off")
        }
        ServeStatus::Serving(n) => {
            LocalizedText::key_arg("folders.share_serve_status_serving", "count", n.to_string())
        }
    }
}

/// The `share-transfer-name` reading for one outcome: the set's folder name
/// plus the peer's short id.
pub fn transfer_name_label(outcome: &SetPullOutcome) -> LocalizedText {
    LocalizedText::key_args(
        "folders.share_transfer_peer_row",
        [
            ("folder", outcome.folder.clone()),
            ("who", fauna_core::format::short_id(&outcome.peer_hex)),
        ],
    )
}

/// The `share-transfer-progress` reading for one outcome: what the latest
/// pass moved.
pub fn transfer_progress_label(outcome: &SetPullOutcome) -> LocalizedText {
    LocalizedText::key_args(
        "folders.share_transfer_progress",
        [
            ("files", outcome.materialized.to_string()),
            ("rows", outcome.rows_accepted.to_string()),
        ],
    )
}

/// The `{source}` of "Limited by {source}" when the storage floor held a
/// body back: *free space on this device* (the user's ruling of 2026-09-30 —
/// the existing reading with a new source, no new element or id).
pub const TRANSFER_SOURCE_FREE_SPACE: &str = "folders.share_transfer_source_free_space";

/// The `share-transfer-state` reading for one outcome — including the honest
/// "limited by …" (Dim-3: a bound names what set it, never a silent stall):
/// the transfer gate's binding tier, or this device's free space when the
/// storage floor kept a wanted body from landing
/// (`crate::share_landing::STORAGE_FLOOR_BYTES`).
///
/// ⚠ The limited arm's `{source}` argument is itself an i18n **key**
/// (`fauna_client_features::tier_label`, or [`TRANSFER_SOURCE_FREE_SPACE`]),
/// so this one resolves through `LocalizedText::resolve_nested`, not
/// `resolve` — a plain resolve would paint the raw key at the user.
pub fn transfer_state_label(outcome: &SetPullOutcome) -> LocalizedText {
    if let Some(refusal) = &outcome.refusal {
        let tier = refusal
            .binding_tier()
            .map(|tier| fauna_client_features::tier_label(tier).key)
            .unwrap_or_default();
        return LocalizedText::key_arg("folders.share_transfer_state_limited", "source", tier);
    }
    if outcome.storage_limited {
        return LocalizedText::key_arg(
            "folders.share_transfer_state_limited",
            "source",
            TRANSFER_SOURCE_FREE_SPACE,
        );
    }
    if !outcome.admitted {
        return LocalizedText::key("folders.share_transfer_state_admission_pending");
    }
    if outcome.rows_accepted > 0 || outcome.materialized > 0 {
        return LocalizedText::key("folders.share_transfer_state_pulling");
    }
    LocalizedText::key("folders.share_transfer_state_up_to_date")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixtures' per-name ref: one set per name unless a test overrides it.
    fn ref_for(folder: &str) -> String {
        format!("ref-{folder}")
    }

    fn binding(folder: &str, group: Option<&[u8]>, keys: bool) -> FolderEngineKeys {
        FolderEngineKeys {
            folder: folder.to_string(),
            folder_id: ref_for(folder),
            mls_group_id: group.map(|g| g.to_vec()),
            content_keys: keys
                .then(|| fauna_core::folder_keys::FolderContentKeys::genesis([9u8; 32], 1)),
            ..FolderEngineKeys::default()
        }
    }

    fn info(folder: &str) -> ServeFolderInfo {
        ServeFolderInfo {
            folder: folder.to_string(),
            folder_id: ref_for(folder),
            body: TreeBodySource::shared("/w"),
            landing: Landing::Resident,
            db_path: PathBuf::from("/d"),
        }
    }

    /// The cadence override parses positively or falls back — the
    /// `resolve_poll_secs` contract, mirrored.
    #[test]
    fn pump_cadence_overrides_only_on_a_positive_integer() {
        assert_eq!(resolve_pump_secs(None), DEFAULT_PUMP_SECS);
        assert_eq!(resolve_pump_secs(Some("2".into())), 2);
        assert_eq!(resolve_pump_secs(Some("0".into())), DEFAULT_PUMP_SECS);
        assert_eq!(resolve_pump_secs(Some("nope".into())), DEFAULT_PUMP_SECS);
    }

    /// Rule 7's composition, end to end through the shared verdict: a live
    /// read wins, a failed live read falls back to the cached token, and no
    /// evidence at all refuses.
    #[test]
    fn cold_start_binds_on_the_cached_token_and_refuses_with_no_cache() {
        let token = || vec!["p2p-share".to_string()];

        let live = composed_evidence(Ok(token()), None);
        assert!(matches!(
            ceremony_bind_verdict(live.as_deref()),
            CeremonyBindVerdict::Bind
        ));

        let cached = composed_evidence(Err("nest down".into()), Some(token()));
        assert!(matches!(
            ceremony_bind_verdict(cached.as_deref()),
            CeremonyBindVerdict::Bind
        ));

        let nothing = composed_evidence(Err("nest down".into()), None);
        assert!(
            !matches!(
                ceremony_bind_verdict(nothing.as_deref()),
                CeremonyBindVerdict::Bind
            ),
            "no evidence at all must refuse the bind"
        );
    }

    fn spec(folder: &str, set: u8) -> SharedSetSpec {
        SharedSetSpec {
            folder: folder.to_string(),
            folder_id: format!("ref-{folder}"),
            set_id: [set; 32],
            db_path: PathBuf::from("/d"),
            body: TreeBodySource::shared("/w"),
            landing: Landing::Resident,
            content_keys: fauna_core::folder_keys::FolderContentKeys::genesis([9u8; 32], 1),
        }
    }

    /// The pass gate: nothing to serve short-circuits BEFORE the brake, a
    /// live token proceeds, and no evidence at all refuses. The ordering is
    /// the assertion that matters — a device with no sets must read "no
    /// shared folders", never "peer transfers are off", and must not bind a
    /// listener to find out (rule 5).
    #[test]
    fn the_pass_gate_asks_about_sets_before_it_asks_about_the_brake() {
        let token = ["p2p-share".to_string()];
        assert_eq!(pass_gate(false, None), PassGate::NoSets);
        assert_eq!(
            pass_gate(false, Some(&token)),
            PassGate::NoSets,
            "a live token does not conjure a set to serve"
        );
        assert_eq!(pass_gate(true, Some(&token)), PassGate::Proceed);
        assert_eq!(
            pass_gate(true, None),
            PassGate::BrakeRefused,
            "no evidence at all must refuse"
        );
    }

    /// The offline hold: a fresh read wins and is remembered, a failed read
    /// pumps what was last known, and a failed read with nothing remembered
    /// skips the pass. The middle arm IS the plane's whole point — the
    /// key-bindings half rides a nest read, so a nest-down pass would
    /// otherwise serve nothing in exactly the window peer transfer exists
    /// for (the two-actor journey's first offline phase found this).
    #[test]
    fn a_failed_spec_read_pumps_the_last_known_specs() {
        let mut last: Vec<SharedSetSpec> = Vec::new();

        assert!(
            specs_for_pass(Err("nest down".into()), &mut last).is_none(),
            "nothing known yet: the pass is skipped, not pumped empty"
        );

        let fresh = specs_for_pass(Ok(vec![spec("photos", 1)]), &mut last).expect("fresh specs");
        assert_eq!(fresh.len(), 1);
        assert_eq!(last.len(), 1, "a successful read is remembered");

        let held = specs_for_pass(Err("nest down".into()), &mut last).expect("last-known specs");
        assert_eq!(held[0].folder, "photos", "the nest-down pass serves on");

        let refreshed =
            specs_for_pass(Ok(vec![spec("docs", 2)]), &mut last).expect("refreshed specs");
        assert_eq!(refreshed[0].folder, "docs");
        assert_eq!(
            last[0].folder, "docs",
            "the memory follows the nest's newest answer, never accumulates"
        );

        let empty =
            specs_for_pass(Ok(Vec::new()), &mut last).expect("an empty answer is an answer");
        assert!(
            empty.is_empty() && last.is_empty(),
            "un-sharing every set must not leave the old ones being served"
        );
    }

    /// The spec join: only an MLS-bound set with keys becomes a spec; an
    /// unbound folder and a bound-but-keyless one never serve; and two bound
    /// sets sharing a NAME are told apart by their refs, never guessed between.
    #[test]
    fn compose_specs_serves_only_keyed_bound_sets_joined_by_ref() {
        let mut twin_a = binding("twin", Some(&[5]), true);
        twin_a.folder_id = "ref-twin-a".to_string();
        let mut twin_b = binding("twin", Some(&[6]), true);
        twin_b.folder_id = "ref-twin-b".to_string();
        let bindings = vec![
            binding("plain", None, false),
            binding("shared", Some(&[1, 2, 3]), true),
            binding("keyless", Some(&[4]), false),
            twin_a,
            twin_b,
        ];
        let mut twin_row = info("twin");
        twin_row.folder_id = "ref-twin-b".to_string();
        let specs = compose_specs(
            vec![info("plain"), info("shared"), info("keyless"), twin_row],
            &bindings,
        );
        assert_eq!(
            specs.len(),
            2,
            "the keyed bound set and the ref-matched twin"
        );
        assert_eq!(specs[0].folder, "shared");
        assert_eq!(
            specs[0].set_id,
            fauna_mls::types::ChannelId::from_group_id(&[1, 2, 3]).0
        );
        assert_eq!(
            specs[1].set_id,
            fauna_mls::types::ChannelId::from_group_id(&[6]).0,
            "the twin row pairs with the binding wearing ITS ref"
        );
    }

    /// the SERVE side is the duplicating side — a serve list
    /// reports every bound location with a DB, and two locations can be bound
    /// to one set. Two serve rows resolving to ONE binding must serve NEITHER:
    /// composing both hands `refresh_serve_sources`' last-write-wins map one
    /// set's keys over whichever folder's bytes registered last.
    #[test]
    fn a_serve_side_twin_serves_neither_row() {
        let bindings = vec![binding("docs", Some(&[1, 2, 3]), true)];
        let mut first = info("docs");
        first.db_path = PathBuf::from("/a");
        let mut second = info("docs");
        second.db_path = PathBuf::from("/b");
        let specs = compose_specs(vec![first, second], &bindings);
        assert_eq!(
            specs.len(),
            0,
            "a serve pair resolving to one set must serve neither side"
        );
    }

    /// The ref is the resolving arm: the same twins compose exactly one spec
    /// when the rows and the binding carry `FolderRef`s, pairing the
    /// REF-matched row's bytes with the binding's keys — and the spec carries
    /// the ref onward so the ingest door routes by it too.
    #[test]
    fn a_folder_ref_resolves_the_name_twin_to_the_bound_row() {
        let mut bound = binding("docs", Some(&[1, 2, 3]), true);
        bound.folder_id = "ref-a".to_string();
        let bindings = vec![bound];
        let mut ours = info("docs");
        ours.folder_id = "ref-a".to_string();
        ours.db_path = PathBuf::from("/a");
        let mut theirs = info("docs");
        theirs.folder_id = "ref-b".to_string();
        theirs.db_path = PathBuf::from("/b");
        let specs = compose_specs(vec![ours, theirs], &bindings);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].db_path, PathBuf::from("/a"));
        assert_eq!(specs[0].folder_id, "ref-a");
    }

    /// A name never stands in for a ref: a serve row and a binding wearing the
    /// same NAME but different refs are different sets, and pair nothing.
    #[test]
    fn a_name_match_without_a_ref_match_pairs_nothing() {
        let mut other_docs = binding("docs", Some(&[1, 2, 3]), true);
        other_docs.folder_id = "ref-someone-elses-docs".to_string();
        assert_eq!(compose_specs(vec![info("docs")], &[other_docs]).len(), 0);
    }

    /// Every serve status and every outcome state has its own reading, and a
    /// refusal names its tier (Dim-3 honesty). The keys are asserted rather
    /// than English text: each app resolves them through its own pipeline.
    #[test]
    fn participation_off_has_its_own_reading() {
        let off = serve_status_label(ServeStatus::ParticipationOff);
        assert_ne!(off, serve_status_label(ServeStatus::BrakeRefused));
        assert_ne!(off, serve_status_label(ServeStatus::NoSets));
    }

    #[test]
    fn surface_readings_are_distinct_and_refusals_name_their_tier() {
        let keys = [
            serve_status_label(ServeStatus::NoSets).key,
            serve_status_label(ServeStatus::BrakeRefused).key,
            serve_status_label(ServeStatus::Serving(2)).key,
        ];
        let unique: std::collections::BTreeSet<&String> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
        assert_eq!(
            serve_status_label(ServeStatus::Serving(2))
                .args
                .get("count"),
            Some(&"2".to_string()),
            "the serving reading names how many sets are routed"
        );

        let mut outcome = SetPullOutcome {
            admitted: false,
            ..SetPullOutcome::default()
        };
        let pending = transfer_state_label(&outcome);
        outcome.admitted = true;
        let quiet = transfer_state_label(&outcome);
        outcome.rows_accepted = 3;
        let pulling = transfer_state_label(&outcome);
        outcome.refusal = Some(fauna_core::feature_gate::FeatureVerdict::Deny {
            tier: fauna_core::feature_gate::RuleTier::Admin,
        });
        let limited = transfer_state_label(&outcome);
        let states: std::collections::BTreeSet<&String> =
            [&pending.key, &quiet.key, &pulling.key, &limited.key]
                .into_iter()
                .collect();
        assert_eq!(states.len(), 4, "four states, four readings");
        assert_eq!(
            limited.args.get("source"),
            Some(&fauna_client_features::tier_label(fauna_core::feature_gate::RuleTier::Admin).key),
            "the refusal names the binding tier, as a key the app resolves"
        );
    }

    /// The storage floor's reading (the user's ruling of 2026-09-30): the
    /// existing "Limited by {source}" with the source *free space on this
    /// device* — a key the app resolves nested, like a tier's. A gate refusal
    /// on the same outcome keeps naming its tier.
    #[test]
    fn a_body_held_back_by_the_storage_floor_reads_limited_by_free_space() {
        let mut outcome = SetPullOutcome {
            admitted: true,
            rows_accepted: 1,
            storage_limited: true,
            ..SetPullOutcome::default()
        };
        let limited = transfer_state_label(&outcome);
        assert_eq!(limited.key, "folders.share_transfer_state_limited");
        assert_eq!(
            limited.args.get("source"),
            Some(&TRANSFER_SOURCE_FREE_SPACE.to_string())
        );

        outcome.refusal = Some(fauna_core::feature_gate::FeatureVerdict::Deny {
            tier: fauna_core::feature_gate::RuleTier::Admin,
        });
        assert_eq!(
            transfer_state_label(&outcome).args.get("source"),
            Some(&fauna_client_features::tier_label(fauna_core::feature_gate::RuleTier::Admin).key)
        );
    }

    struct ScriptedHost {
        replica: Option<OnDemandReplica>,
        ingested: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl OnDemandReplicaHost for ScriptedHost {
        async fn replica(&self) -> Option<OnDemandReplica> {
            self.replica.clone()
        }

        async fn share_ingest(
            &self,
            proven_actor_hex: &str,
            _rows: Vec<Vec<u8>>,
            _spool_dir: &std::path::Path,
        ) -> anyhow::Result<ShareIngestSummary> {
            self.ingested
                .lock()
                .unwrap()
                .push(proven_actor_hex.to_string());
            Ok(ShareIngestSummary {
                cursor: 7,
                ..Default::default()
            })
        }
    }

    struct ScriptedHosts(Vec<Arc<ScriptedHost>>);

    impl OnDemandHosts for ScriptedHosts {
        fn hosts(&self) -> Vec<Arc<dyn OnDemandReplicaHost>> {
            self.0
                .iter()
                .map(|h| Arc::clone(h) as Arc<dyn OnDemandReplicaHost>)
                .collect()
        }
    }

    fn scripted_host(dir: &std::path::Path, name: &str, built: bool) -> Arc<ScriptedHost> {
        let db_path = dir.join(format!("{name}.db"));
        drop(crate::db::SyncDb::open(&db_path).unwrap());
        Arc::new(ScriptedHost {
            replica: built.then(|| OnDemandReplica {
                folder: name.to_string(),
                folder_id: ref_for(name),
                db_path,
                kept_root: dir.join(name).join("kept"),
                cache_root: dir.join(name).join("cache"),
            }),
            ingested: Default::default(),
        })
    }

    /// The on-demand construction: every built host's replica is a serve row
    /// that lands by policy into its kept root, a host with no replica to
    /// offer is skipped, and the door routes a page to the host holding the
    /// set by `FolderRef` — an unknown set is an error, never a guess.
    #[tokio::test]
    async fn the_on_demand_access_lists_built_replicas_and_routes_by_folder_ref() {
        let dir = tempfile::tempdir().unwrap();
        let photos = scripted_host(dir.path(), "photos", true);
        let docs = scripted_host(dir.path(), "docs", true);
        let unbuilt = scripted_host(dir.path(), "refused", false);
        let access = on_demand_share_access(Arc::new(ScriptedHosts(vec![
            Arc::clone(&photos),
            unbuilt,
            Arc::clone(&docs),
        ])));

        let serve = access.info.serve_info().await.unwrap();
        assert_eq!(
            serve.iter().map(|i| i.folder.as_str()).collect::<Vec<_>>(),
            ["photos", "docs"]
        );
        assert_eq!(
            serve[0].landing,
            Landing::OnDemand {
                kept_root: dir.path().join("photos").join("kept")
            }
        );

        let spool = dir.path().join("spool");
        let summary = access
            .door
            .ingest("docs", &ref_for("docs"), "aa", Vec::new(), &spool)
            .await
            .unwrap();
        assert_eq!(summary.cursor, 7);
        assert_eq!(*docs.ingested.lock().unwrap(), ["aa"]);
        assert!(photos.ingested.lock().unwrap().is_empty());

        assert!(
            access
                .door
                .ingest("docs", "ref-elsewhere", "aa", Vec::new(), &spool)
                .await
                .is_err(),
            "a set no host holds is refused, never routed by its name"
        );
    }

    /// A source that mints its own bearer and so can say its deadline.
    struct ExpiringBearer;

    #[async_trait::async_trait]
    impl fauna_nest_http::BearerSource for ExpiringBearer {
        async fn bearer(&self) -> Result<String, fauna_nest_http::ApiError> {
            Ok("tok".into())
        }
        async fn bearer_with_expiry(
            &self,
        ) -> Result<(String, Option<u64>), fauna_nest_http::ApiError> {
            Ok(("tok".into(), Some(1_900_000_000)))
        }
    }

    /// The agent is handed the source's own client-clock deadline, so it
    /// plans its renewal on a real expiry rather than a blind cadence.
    #[tokio::test]
    async fn agent_bearer_source_pushes_the_sources_own_expiry() {
        use fauna_client_sync::agent::ProvisioningBearerSource;
        let b = AgentBearerSource(Arc::new(ExpiringBearer))
            .current_bearer()
            .await
            .expect("a minting source yields a bearer");
        assert_eq!(b.token, "tok");
        assert_eq!(b.expires_at, 1_900_000_000);
    }

    /// A source that cannot say its expiry (an opaque `StaticBearer`) skips the
    /// tick — never a guessed deadline handed to the agent.
    #[tokio::test]
    async fn agent_bearer_source_skips_a_bearer_with_no_expiry() {
        use fauna_client_sync::agent::ProvisioningBearerSource;
        let b = AgentBearerSource(Arc::new(fauna_nest_http::StaticBearer("tok".into())))
            .current_bearer()
            .await;
        assert!(
            b.is_none(),
            "no published expiry ⇒ not authenticated, tick skipped"
        );
    }
}
