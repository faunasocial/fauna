//! The peer-leg assembly seam (`account-data-plane.md` § The peer leg): the account runtime
//! brings the same-account peer listener up **itself**, when and only when
//! every gate the charter names is open.
//!
//! # The gates, in check order
//!
//! 1. **Engine-singleton role** — "whichever co-located process holds the
//!    engine-singleton role speaks for its store" (§ The peer leg). Enforced
//!    by *placement*: [`ensure_bound`] runs as a pump step, and pump passes
//!    are holder-only (W5.1 (account-data-plane.md § Workstreams)). A non-holder therefore never constructs a
//!    transport at all — see the factory note below for why that matters.
//! 2. **A transport factory was supplied** ([`PeerTransportFactory`]) — web
//!    and not-yet-wired apps pass `None` and the leg stays structurally off.
//! 3. **The enrollment witness exists** — the T10 slot's root-signed
//!    `DeviceAuthorization` over the writer key (W5.4's ceremony;
//!    `principal_bundle`). An unenrolled machine skips quietly: only a
//!    signed-in ceremony can change it.
//! 4. **The nest advertises the `peer-sync` brake** (wormability rule 7,
//!    `p2p.md` § Wormability posture). Evidence is `fauna.nest.info` fetched
//!    over the pump's own requester, **cached in the store's meta table** so
//!    an offline start still binds from the last-known advertisement — the
//!    outage matrix ("same LAN, WAN down") is the leg's whole point, and a
//!    brake that only worked online would keep the leg off exactly when it
//!    is needed. No evidence at all (fresh store, nest never reached) reads
//!    as brake-on: the door refuses by default, never by optimism.
//!
//! # Why a factory, not `Option<Arc<dyn PeerTransport>>`
//!
//! The ruled seam sentence predates two facts the W5 build
//! itself created, and the factory is the recorded refinement of both
//! (charter § Implementation status today → *Built — W5.7*):
//!
//! - **One NodeId per machine.** The peer-plane identity is the *machine's*
//!   device principal (R5 (account-data-plane.md § The ratified decisions)) — the writer key every co-located process shares
//!   since W5.4. A transport constructed at params time would exist (socket
//!   bound, endpoint live) in **every** instance of every app, all carrying
//!   the same NodeId; the moment a relay is configured they would fight over
//!   the relay's routing for that id. A factory invoked only by the elected
//!   holder makes "at most one live endpoint per machine" structural.
//! - **The writer key is resolved inside the assembly** (the W5.3 migration
//!   section). A caller cannot hand over a transport bound to the writer
//!   identity before `start()` without re-resolving the key itself; the
//!   factory receives the assembly's own resolved key instead — one source
//!   of truth, and the first launch (key minted during assembly) binds on
//!   its very first holder pass.
//!
//! The witness parameter of the ruled sentence is likewise refined away: the
//! slot the W5.4a carriage built *is* the witness source
//! (`PrincipalSlot::device_authorization` — its doc names W5.7 as the
//! consumer), and a second hand-fed witness could only agree with it or
//! silently diverge from it.
//!
//! # What the bind feeds back
//!
//! [`EndpointFacts`] — the bound listener's LAN candidates (interface
//! addresses × bound ports) and the nest-advertised relay URL — flow into the
//! same slot `AccountStoreHandle::set_endpoint_facts` fills, and the
//! device-endpoints step publishes them on the **same pass** (dial candidates
//! join the `node_id`-only floor row). An app-fed `set_endpoint_facts` value
//! deliberately wins over the self-observed one: the Cmd is the explicit
//! override (and what conformance V5 drives). `public_addrs` stays empty,
//! for good: the substrate exchanges an observed address inside the
//! connection it makes, to the one admitted peer, and keeps it nowhere; a
//! fauna-side advert would make a device's public-address history durable
//! merged state on every sibling (`p2p.md` § The relay → *Address discovery,
//! and what rides a relayed path*, ruling 3).
//!
//! Dropping the node (worker shutdown) closes the listener and every inbound
//! channel — wormability rule 5 holds by construction. The *dial* half of the
//! leg (sibling_dial_targets → dial → admit → pull-only walk, with the
//! retained-key custody attached) is deliberately not here — it is captured
//! as its own track.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_peer_channel::PeerNode;
use fauna_peer_sync::quota::QuotaConfig;
use fauna_peer_sync::server::{
    NowFn, PeerSyncServer, PeerSyncServerConfig, ServeStoreHandle, start_peer_sync_node,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};
use fauna_protocol::sync::{
    KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET, SyncDeviceP2pParticipationSetReply,
    SyncDevicesListReply, SyncDevicesListRequest,
};
use fauna_transport::PeerTransport;
use futures_util::future::BoxFuture;

use crate::device_endpoints_writer::EndpointFacts;
use crate::principal_bundle::PrincipalCustody;
/// The ensure and dial passes' report shapes — the driver's vocabulary
/// (`fauna_account_plane::host_legs`), re-exported at their old paths.
pub use fauna_account_plane::host_legs::{DialPass, PeerLegPass};

/// What an app's transport factory hands back: the seam-typed transport plus
/// the one fact only the concrete constructor can observe — where the
/// endpoint actually bound. (The trait deliberately has no `bound_addrs`;
/// "transport truth only the assembler can observe" is the
/// `device_endpoints_writer` contract this feeds.)
pub struct PeerLegBinding {
    pub transport: Arc<dyn PeerTransport>,
    /// The bound socket addresses, unspecified-IP (`0.0.0.0:port`) entries
    /// welcome — the facts composition crosses their ports with this
    /// device's interface addresses.
    pub bound_addrs: Vec<SocketAddr>,
    /// The host's file-sync engines on the peer leg ([`PeerFileSync`]) —
    /// `None` on a host that runs no file-sync engine beside this runtime: the
    /// leg then serves no file body and fills no sibling registry.
    pub file_sync: Option<PeerFileSync>,
}

/// What a host running file-sync engines beside the runtime hands the peer
/// leg: the same-account peer data plane's two halves (`p2p.md` § Goal;
/// `file-sync.md` § Content residency — the seat↔seat chunk pull).
///
/// Carried on the factory's [`PeerLegBinding`] rather than as a runtime
/// parameter because it is the factory's own statement about this host — the
/// process that builds the endpoint is the process whose engines it serves.
#[derive(Clone)]
pub struct PeerFileSync {
    /// The serve half: one stored chunk of a folder this host runs an engine
    /// for, through the one serve core (the agent passes its relay seat's
    /// `RelaySeat::serve_peer`).
    pub file_chunks: fauna_peer_sync::FileChunkFn,
    /// The pull half: the registry the dial pass fills with the siblings it
    /// admitted, which the host's engines ask before the nest
    /// (`SyncEngine::with_sibling_chunks`).
    pub siblings: Arc<crate::sibling_chunks::SiblingChannels>,
}

/// What the runtime hands the factory: the assembly's own resolved writer
/// key (module docs own why it is never caller-supplied), plus the transport
/// facts only the runtime observes.
pub struct PeerLegFactoryInputs {
    /// The machine's device principal — the endpoint's identity (R5).
    pub writer_key: SigningKey,
    /// The relay URL this device's nest advertises
    /// (`NestInfoReply.iroh_relay_url`, live-or-cached — own-nest provenance
    /// by construction, never a peer advert), for
    /// `IrohTransportBuilder::relay_url`. `None` = direct-only. The relay
    /// protocol runs over ordinary HTTPS; the builder's default trust roots
    /// are correct for a production `relay.<domain>` certificate, and
    /// `custom_roots` stays the test-relay injection point.
    pub relay_url: Option<String>,
}

/// Constructs the peer transport bound to this machine's device principal.
///
/// Invoked by the **elected holder only**, with [`PeerLegFactoryInputs`]
/// (module docs own why the key is the runtime's, never the caller's). tui
/// and the agent pass a closure building `fauna_iroh::IrohTransport` from the
/// key's secret bytes; tests pass an in-memory transport. `None` in
/// `AccountRuntimeParams::peer_transport` keeps the leg structurally off.
pub type PeerTransportFactory =
    Arc<dyn Fn(PeerLegFactoryInputs) -> BoxFuture<'static, Result<PeerLegBinding>> + Send + Sync>;

/// The last `fauna.nest.info` facts the peer leg cares about, cached in the
/// store's meta table so an offline start binds from the last-known
/// advertisement. The leg fetches and writes them; the driver's handle serves
/// the read (`Cmd::CachedNestCapabilities`), which is why the row lives in
/// `fauna_account_plane::host_legs`.
pub use fauna_account_plane::host_legs::{META_NEST_FACTS, PeerLegNestFacts, cached_nest_facts};
const NODE_INFO_KIND: &str = "fauna.nest.info";

/// The worker-owned peer-leg state: the app-supplied factory, the live node
/// (its Drop is the listener's lifetime — rule 5), and the facts observed at
/// bind.
pub(crate) struct PeerLegState {
    factory: Option<PeerTransportFactory>,
    /// The per-actor store dir — the serve side opens its own connection to
    /// it (the store's multi-connection posture, the W2.6 serve shape).
    store_dir: PathBuf,
    actor_id_hex: String,
    node: Option<PeerNode>,
    /// The bound transport, kept beside the node for the DIAL half:
    /// one endpoint serves and dials — the machine's single NodeId. Crate-
    /// visible so the pump's custodian dial pass rides the same endpoint.
    pub(crate) transport: Option<Arc<dyn PeerTransport>>,
    /// Where the listener bound — kept so the per-pass facts refresh can
    /// recompose LAN candidates when this device's interfaces change.
    bound_addrs: Vec<SocketAddr>,
    /// The host's file-sync hooks from the bind ([`PeerFileSync`]) — the dial
    /// pass fills its sibling registry.
    pub(crate) file_sync: Option<PeerFileSync>,
    /// The last nest facts (live fetch preferred, cache as fallback) — the
    /// relay half of the composed facts, refreshed opportunistically while
    /// bound.
    nest_facts: Option<PeerLegNestFacts>,
    /// Self-observed facts (bound LAN candidates + nest relay URL). An
    /// app-fed `set_endpoint_facts` value wins over these — the Cmd is the
    /// explicit override.
    pub(crate) facts: Option<EndpointFacts>,
    /// The live serve side, kept beside the node so the pump can refresh
    /// its custodied-accounts registry per pass (W8.5 P1 —
    /// `PeerSyncServer::set_custodied`).
    pub(crate) server: Option<Arc<PeerSyncServer>>,
    /// The custody-revocation snapshot — revoked custody-grant ids, derived
    /// per pump pass from the account's grant-event log, the succession
    /// ledger's rows (`custody_leg::CustodyLegState::refresh_revoked`; W8.5 P2, T13's
    /// honest bound IS this refresh cadence). Read by the serve side's
    /// custody predicate at the admit door and on every request, and by the
    /// dialer over a custodian's reply witness.
    pub(crate) revoked: WithdrawalSnapshot<Vec<u8>>,
    /// The removed-device snapshot — this account's own fleet ids carrying a
    /// `Removed` row, derived per pump pass from merged
    /// `fauna.state.device-set` state ([`refresh_removed_devices`]). The
    /// device twin of `revoked`, read by the same two halves.
    ///
    /// Own-account only, by construction rather than by omission: a
    /// custodied account's device-set rows rest sealed to its owner's fleet,
    /// so this machine cannot read them. For those accounts the view answers
    /// from `custodied_exclusions` instead
    /// ([`WithdrawalSnapshot::device_removed_view`]).
    pub(crate) removed_devices: WithdrawalSnapshot<[u8; 32]>,
    /// The custodied accounts' removed-device map — the owner-signed
    /// exclusion lists on the custody grants this machine holds, unioned per
    /// account, re-derived each pass from the `custodies-held` rows where the
    /// serve registry is fed (`custody_leg::CustodyLegState::serve_refresh`).
    /// Read live by both halves beside `removed_devices`; absent admits.
    pub(crate) custodied_exclusions: fauna_peer_sync::admission::CustodiedExclusions,
}

impl PeerLegState {
    pub(crate) fn new(
        factory: Option<PeerTransportFactory>,
        store_dir: PathBuf,
        actor_id_hex: String,
    ) -> Self {
        PeerLegState {
            factory,
            store_dir,
            actor_id_hex,
            node: None,
            transport: None,
            bound_addrs: Vec::new(),
            file_sync: None,
            nest_facts: None,
            facts: None,
            server: None,
            revoked: WithdrawalSnapshot::underived(),
            removed_devices: WithdrawalSnapshot::underived(),
            custodied_exclusions: Default::default(),
        }
    }
}

/// One pump-refreshed **withdrawal snapshot** — the evaluating side's second
/// store, beside the witness verifier: "has the account since withdrawn this
/// self-contained witness?" (`account-sync-plane.md` § The admission seam →
/// *Validity and severance*). [`PeerLegState`] holds two, one per witness
/// arm: revoked custody-grant ids and removed fleet device ids.
///
/// **Not yet derived is not "nothing withdrawn".** A snapshot starts
/// underived and stays so until its first successful refresh, and a view
/// over an underived snapshot answers *withdrawn* for every id of its arm —
/// fail-closed, the posture a serve side with no custody view already has.
/// So a leg that binds before its first refresh, or while every refresh so
/// far has failed, refuses that arm rather than answering from an empty set
/// that would admit every withdrawn witness. The other arm is untouched: one broken
/// read never takes the whole leg down.
///
/// **A refresh that fails after one succeeded keeps the last derived set.**
/// A withdrawal merged since then waits for the next success — the window a
/// persistent local read failure holds open, surfaced as that pass's step
/// error, and stated as part of the bound in that same bullet.
///
/// Clones share one snapshot (an `Arc`): every view built over it reads it
/// live, so a refresh reaches an already-bound server's admit door, its live
/// connections' next requests and the next dial — no rebind.
pub struct WithdrawalSnapshot<T>(Arc<std::sync::RwLock<Option<std::collections::HashSet<T>>>>);

impl<T> Clone for WithdrawalSnapshot<T> {
    fn clone(&self) -> Self {
        WithdrawalSnapshot(Arc::clone(&self.0))
    }
}

impl<T: Eq + std::hash::Hash> WithdrawalSnapshot<T> {
    /// A snapshot no refresh has derived yet — every view over it refuses.
    pub fn underived() -> Self {
        WithdrawalSnapshot(Arc::new(std::sync::RwLock::new(None)))
    }

    /// Install a freshly derived set. An empty set is a real answer ("the
    /// merged state withdraws nothing") and admits; only never having
    /// derived refuses.
    pub fn replace(&self, next: std::collections::HashSet<T>) {
        *self.0.write().unwrap() = Some(next);
    }

    /// Whether any refresh has succeeded yet.
    pub fn is_derived(&self) -> bool {
        self.0.read().unwrap().is_some()
    }

    /// Whether a view must refuse `id`: listed in the derived set, or no set
    /// derived yet.
    pub fn withdraws<Q>(&self, id: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
        Q: Eq + std::hash::Hash + ?Sized,
    {
        self.0
            .read()
            .unwrap()
            .as_ref()
            .is_none_or(|set| set.contains(id))
    }
}

impl WithdrawalSnapshot<Vec<u8>> {
    /// The custody-revocation view, in the shape both halves take — the
    /// serve config's `custody_revoked` and the dialer's
    /// `AdmissionViews::custody_revoked`.
    pub fn custody_revoked_view(&self) -> impl Fn(&[u8]) -> bool + Send + Sync + 'static {
        let snapshot = self.clone();
        move |grant_id: &[u8]| snapshot.withdraws(grant_id)
    }
}

impl WithdrawalSnapshot<[u8; 32]> {
    /// The removed-device view for `account` (this machine's own), in the
    /// shape both halves take — the serve config's `device_removed` and the
    /// dialer's `AdmissionViews::device_removed`. Two sources, split by
    /// account: this machine holds device-set rows for its OWN account only,
    /// so that account answers from this snapshot (underived refuses), and
    /// any other — a custodied account — answers from `custodied`, the
    /// owner-signed lists on the grants held for it (absent admits; the
    /// bound `DeviceRemovedFn`'s doc states). No refresh here could ever
    /// read a custodied account's rows, so the snapshot never answers for
    /// one, derived or not.
    pub fn device_removed_view(
        &self,
        account: [u8; 32],
        custodied: &fauna_peer_sync::admission::CustodiedExclusions,
    ) -> impl Fn(&[u8; 32], &[u8; 32]) -> bool + Send + Sync + 'static {
        let snapshot = self.clone();
        let custodied = custodied.clone();
        move |acct: &[u8; 32], device_key: &[u8; 32]| {
            if *acct == account {
                snapshot.withdraws(device_key)
            } else {
                custodied.excludes(acct, device_key)
            }
        }
    }
}

/// Re-derive the removed-device snapshot from merged plane state — the
/// device twin of the custody leg's revocation refresh, run beside it right
/// after the fleet walk so this pass's rows are already in.
///
/// The derivation is the plane's own: [`FleetView`] over the live
/// `fauna.state.device-set` rows, whose `Removed` exclusion is unconditional
/// (`fauna_core::generation`'s module ruling — attribution is advisory, so a
/// "stop trusting" signal is never refused on verification grounds). Runs
/// whether or not the leg is bound, so a bind that happens later starts
/// derived; an unbound pass costs one `states_of_kind` read. **On failure
/// the snapshot is left as it was** — underived before a first success
/// (every view over it refuses), the last derived set after one
/// ([`WithdrawalSnapshot`] owns both halves of that posture).
pub(crate) async fn refresh_removed_devices<B: StoreBackend>(
    state: &PeerLegState,
    store: &AccountStore<B>,
    trust: &crate::generation_tip::GenerationTrust,
) -> Result<usize> {
    let next = crate::fleet_removal::removed_device_ids(store, trust).await?;
    let n = next.len();
    state.removed_devices.replace(next);
    Ok(n)
}

/// The pump's peer-leg ensure step. Holder-only by placement (module docs,
/// gate 1); runs before the device-endpoints step so a first bind's facts
/// publish on the same pass. While bound it doubles as the **facts refresh**:
/// re-fetch the nest facts opportunistically and recompose the LAN
/// candidates from `lan_ips` — the injectable interface list, production =
/// `fauna_peer_sync::discovery::discover_lan_candidates()` — so an interface
/// change republishes through the device-endpoints step's write-if-changed.
pub(crate) async fn ensure_bound<B, R>(
    state: &mut PeerLegState,
    store: &AccountStore<B>,
    writer_key: &SigningKey,
    slot: &dyn PrincipalCustody,
    rpc: &R,
    lan_ips: &[Ipv4Addr],
) -> Result<PeerLegPass>
where
    B: StoreBackend,
    R: RpcRequester,
{
    // The device's own participation, read BEFORE the brake (`p2p.md`
    // § Per-device participation): first fold a pending nest-side brake and
    // settle any report this device owes, then let the verdict decide
    // whether a listener may exist at all. Off drops a live node here — the
    // one door in, the one door out (rule 5: p2p disabled ⇒ no socket).
    let participation = reconcile_participation(state, store, writer_key, slot, rpc).await;
    if !participation.effective() {
        if state.node.is_some() {
            drop_listener(state);
            tracing::info!("peer leg: participation is off on this device — listener dropped");
        }
        return Ok(PeerLegPass::ParticipationOff);
    }
    if state.node.is_some() {
        // The refresh half: best-effort nest-facts update (a nest that stops
        // advertising stops the leg at the next start, never mid-run — the
        // ruled rule-7 semantic), then recompose.
        //
        // ⚠ **This is an RPC per pump pass, and it is deliberate — re-sighted
        // 2026-08-30 and left as-is.** `fauna.nest.info` rides the nest's
        // throttled discovery surface (`anonymous_rate_limit.rs`, 60 events /
        // 60 s, bucketed per `actor_id` for an authenticated connection since
        // 2026-08-24), so an unbounded per-pass fetch here would be a standing
        // draw on a security budget. It was raised as exactly that alarm and
        // measured down:
        //
        // * **Passes are NOT nudge-driven.** The nudge arm drains its burst and
        //   runs `walk_one_scope` only — it never re-enters the pump body, and
        //   says so ("Walk-only: a nudge never runs the departure step",
        //   `account_runtime.rs`). Full passes come from the prologue, reconnect
        //   wakes, an automation `reconcile-now`, and the 300 s backstop
        //   (`DEFAULT_BACKSTOP_INTERVAL`). The steady-state draw is therefore
        //   ~0.2 RPC/min per actor against a 60/min bucket — two orders of
        //   magnitude of headroom, and reaching the bound would take ~60
        //   reconnects in a minute, which has larger problems than this fetch.
        // * **The fetch is not waste, so a TTL would cost something real.** The
        //   BRAKE half is bind-only (`peer_sync_enabled` is consulted in the
        //   bind arm below, never here) — that is the "never mid-run" semantic.
        //   But `iroh_relay_url` is read straight into `compose_facts` on the
        //   next line, and a changed composition republishes this device's dial
        //   candidates through the device-endpoints step. That per-pass refresh
        //   is the ruled behaviour, not an accident (`account-data-plane.md`
        //   § the dial-pass entry: *"Facts refresh rides the ensure step
        //   while bound"*). Debouncing the fetch would trade relay-URL
        //   propagation latency for headroom already measured in orders of
        //   magnitude.
        //
        // Keep the recompose below per-pass regardless: `lan_ips` is the
        // caller's once-per-pass snapshot and costs no RPC at all.
        if let Ok(fresh) = fetch_nest_facts(rpc).await {
            cache_nest_facts(store, &fresh, "refreshed").await;
            state.nest_facts = Some(fresh);
        }
        let relay_url = state
            .nest_facts
            .as_ref()
            .and_then(|f| f.iroh_relay_url.clone());
        let recomposed = compose_facts(&state.bound_addrs, lan_ips, relay_url);
        if state.facts.as_ref() != Some(&recomposed) {
            tracing::info!("peer leg: transport facts changed — republishing dial candidates");
            state.facts = Some(recomposed);
        }
        return Ok(PeerLegPass::AlreadyBound);
    }
    let Some(factory) = state.factory.clone() else {
        return Ok(PeerLegPass::NoTransport);
    };
    let Some(witness) = slot.device_authorization() else {
        tracing::debug!(
            "peer leg: no enrollment witness in the credential slot — not enrolled yet; \
             the leg stays down until a signed-in ceremony runs"
        );
        return Ok(PeerLegPass::NotEnrolled);
    };

    // Brake evidence: live fetch first (and cache it), last-known second.
    let nest_facts = match fetch_nest_facts(rpc).await {
        Ok(fresh) => {
            cache_nest_facts(store, &fresh, "written").await;
            Some(fresh)
        }
        Err(e) => {
            tracing::debug!("peer leg: nest.info unreachable ({e:#}) — trying the cached brake");
            cached_nest_facts(store).await
        }
    };
    let Some(nest_facts) = nest_facts else {
        return Ok(PeerLegPass::NoBrakeEvidence);
    };
    if !fauna_peer_sync::peer_sync_enabled(&nest_facts.capabilities) {
        return Ok(PeerLegPass::BrakeOn);
    }

    let binding = factory(PeerLegFactoryInputs {
        writer_key: writer_key.clone(),
        relay_url: nest_facts.iroh_relay_url.clone(),
    })
    .await
    .context("peer leg: transport factory")?;

    // The serve side reads the relay plane over its own connection to the
    // same WAL store dir (the W2.6 shape; the store's multi-connection
    // posture).
    let account: [u8; 32] = fauna_core::hex32::decode(&state.actor_id_hex)
        .map_err(|e| anyhow::anyhow!("peer leg: actor id: {e}"))?;
    let writer = WriterId(writer_key.verifying_key().to_bytes());
    let serve_backend =
        SqliteBackend::open(&state.store_dir).context("peer leg: open serve backend")?;
    let serve_store = AccountStore::open(serve_backend, &state.actor_id_hex, writer)
        .await
        .context("peer leg: open serve store")?;
    let now: NowFn = Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });
    let server = Arc::new(PeerSyncServer::new(
        ServeStoreHandle::spawn(serve_store),
        account,
        PeerSyncServerConfig {
            // Pre-auth-visible surface: no name by design (a label is a user
            // choice, and none exists — the `PeerNode::start(_, String::new())`
            // precedent).
            display_name: String::new(),
            own_witness: witness.wire,
            own_witness_kind: fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION.to_string(),
            // The W8.5 revocation view over the pump-refreshed snapshot (T13's
            // two-stores rule, fleet half) — wiring it is what ENABLES custody
            // serving; the snapshot refresh cadence IS the ratified honest
            // bound. A revoked grant is refused at its next handshake AND at
            // its next request on a live connection.
            custody_revoked: Some(Arc::new(state.revoked.custody_revoked_view())),
            // The device twin, same shape and same cadence: a sibling the
            // fleet removed is refused at its next handshake AND at its next
            // request on a live connection. Both views read their snapshot
            // live, so one this pass could not derive refuses its arm until a
            // later pass does (`WithdrawalSnapshot`), never answering from an
            // empty set.
            device_removed: Some(Arc::new(
                state
                    .removed_devices
                    .device_removed_view(account, &state.custodied_exclusions),
            )),
            // The host's engines, through the one serve core — a sibling's
            // chunk pull is that core's further consumer (`file-sync.md`
            // § Relay serving).
            file_chunks: binding.file_sync.as_ref().map(|f| f.file_chunks.clone()),
            quotas: QuotaConfig::default(),
            now,
        },
    ));
    let node = start_peer_sync_node(
        Arc::clone(&binding.transport),
        Arc::clone(&server),
        &nest_facts.capabilities,
    )
    .await
    .context("peer leg: bind")?;
    state.node = Some(node);
    state.server = Some(server);
    state.transport = Some(binding.transport);
    state.facts = Some(compose_facts(
        &binding.bound_addrs,
        lan_ips,
        nest_facts.iroh_relay_url.clone(),
    ));
    state.bound_addrs = binding.bound_addrs;
    state.file_sync = binding.file_sync;
    state.nest_facts = Some(nest_facts);
    tracing::info!("peer leg: listener up (engine-singleton, enrolled, brake off)");
    Ok(PeerLegPass::Bound)
}

/// A generous per-sibling ceiling for the whole dial → admit → walk → pull
/// interaction — a budget a green run never pays (convention 14), sized so a
/// black-holed candidate set cannot stall the pump for the pass's whole
/// lifetime times the fleet size.
const PER_SIBLING_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// The dial pass (the pull half of the peer leg): for every sibling
/// the store's device-endpoints entries name, dial over the SAME transport
/// the listener runs on (one endpoint, one NodeId — the machine's device
/// principal), admit mutually with the slot witness, and run the ordinary
/// pull-only walks — delegable + fleet class-2 (custody attached to both,
/// production shape) and every content scope's class-1 feed plus its
/// want-list block pull.
///
/// Runs **only while the leg is bound**: bound implies elected + enrolled +
/// brake-off, so the rule-7 client gate covers dialing for free, and a
/// factory-less caller never dials (structurally off, both directions).
/// Per-target failures are isolated and absorbed — an unreachable sibling is
/// the peer leg's ordinary weather, not an error; the next pass retries.
///
/// `nest` is whether this pass's own nest walk answered: over a relayed
/// connection the rows are walked as ever but the block pull is left to the
/// nest path while it is reachable (`fauna_transport::bytes_may_ride`;
/// `p2p.md` § The relay, ruling 4), counted in
/// [`DialPass::blocks_relay_deferred`].
#[allow(clippy::too_many_arguments)] // the writer-identity bundle, spelled out (mirrors pump)
pub(crate) async fn dial_pass<B>(
    state: &PeerLegState,
    store: &AccountStore<B>,
    schedule: &fauna_core::crypto::AccountStateKeySchedule,
    trust: &crate::generation_tip::GenerationTrust,
    writer_key: &SigningKey,
    slot: &dyn PrincipalCustody,
    content_scopes: &[fauna_protocol::scope::ContentScope],
    lan_ips: &[Ipv4Addr],
    own_endpoints: Option<&DeviceEndpoints>,
    nest: fauna_transport::NestPath,
) -> Result<Option<DialPass>>
where
    B: StoreBackend,
{
    let Some(transport) = state.transport.as_ref() else {
        return Ok(None); // not bound — the leg is off (module docs)
    };
    let Some(witness) = slot.device_authorization() else {
        return Ok(None); // unenrolled can't happen while bound; belt anyway
    };
    let account: [u8; 32] = fauna_core::hex32::decode(&state.actor_id_hex)
        .map_err(|e| anyhow::anyhow!("peer leg: actor id: {e}"))?;
    let own_hex = fauna_core::hex32::encode(&writer_key.verifying_key().to_bytes());
    // Sibling candidates are this replica's verified fleet members only
    // (`account-sync-plane.md` § The peer leg → *Discovery*): a removed,
    // signed-out or predecessor device's merged entry outlives the device,
    // and dialing it would spend a whole failing dial on every pass. That
    // covers this machine's own retired writers after an in-process
    // succession too — their enrollment certs are the retired root's, which
    // the view never verifies. The custodian targets below are not fleet
    // members and are not filtered here.
    let fleet = crate::fleet_removal::fleet_view(store, trust).await?;
    let mut targets = fauna_peer_sync::sibling_dial_targets(
        store,
        &own_hex,
        |id| fleet.is_verified_member(id),
        lan_ips,
    )
    .await?;
    // The fleet members among the targets — the only ones whose channels the
    // file-sync engines may ask for chunk bodies (a custodian holds account
    // planes, not file bodies, and its serve side refuses the kind).
    let siblings: Vec<[u8; 32]> = targets.iter().map(|t| t.node_id).collect();
    let file_siblings = state.file_sync.as_ref().map(|f| Arc::clone(&f.siblings));
    if let Some(registry) = &file_siblings {
        registry.set_nest(nest);
    }
    // The owner-side custodian dials (W8.5 P5): custodians this account
    // granted custody to, from the fleet-only `custodian-endpoints` rows the
    // ceremony wrote — same witness, same pull-only walks (a custodied store
    // serves the same relay plane a sibling does). Deduped by NodeId: a
    // machine that is somehow both is dialed once.
    for target in fauna_peer_sync::custodian_dial_targets(store, lan_ips).await? {
        if target.node_id != writer_key.verifying_key().to_bytes()
            && !targets.iter().any(|t| t.node_id == target.node_id)
        {
            targets.push(target);
        }
    }
    if targets.is_empty() {
        return Ok(Some(DialPass::default()));
    }
    let mut report = DialPass {
        targets: targets.len(),
        ..DialPass::default()
    };
    // The dialer's revocation view over custody-grant REPLY witnesses (a
    // custodian answers with the custody grant): the same pump-refreshed
    // snapshot the serve side reads, so an owner-side revoke severs BOTH
    // directions at the next evaluation.
    let revoked = &state.revoked;
    // The dialer's removed-device view over `DeviceAuthorization` REPLY
    // witnesses (a sibling answers with its fleet cert): the same
    // pump-refreshed snapshot the serve side reads, so a removal severs BOTH
    // directions at the next evaluation — a sibling never walks a removed
    // device's relay plane either.
    let removed_devices = &state.removed_devices;
    let custodied_exclusions = &state.custodied_exclusions;
    for target in targets {
        let attempt = tokio::time::timeout(
            PER_SIBLING_BUDGET,
            dial_one(
                transport,
                &target,
                &witness.wire,
                &account,
                store,
                schedule,
                trust,
                writer_key,
                slot,
                content_scopes,
                revoked,
                removed_devices,
                custodied_exclusions,
                own_endpoints,
                nest,
            ),
        )
        .await;
        match attempt {
            Ok(Ok((applied, pulled, peer_endpoints, channel))) => {
                report.admitted += 1;
                report.applied += applied;
                report.blocks_fetched += pulled.fetched;
                report.blocks_relay_deferred += pulled.relay_deferred;
                if let Some(fresh) = peer_endpoints {
                    report.observed.insert(fresh.node_id, fresh);
                }
                // Mutually admitted this pass: the host's engines may ask it
                // for chunk bodies until the next pass replaces the channel.
                if let Some(registry) = &file_siblings
                    && siblings.contains(&target.node_id)
                {
                    registry.admitted(target.node_id, channel);
                }
            }
            Ok(Err(e)) => {
                report.failed += 1;
                if let Some(registry) = &file_siblings {
                    registry.dropped(&target.node_id);
                }
                tracing::debug!(
                    node = %fauna_core::hex32::encode(&target.node_id),
                    "peer dial: sibling unreachable or refused this pass: {e:#}"
                );
            }
            Err(_) => {
                report.failed += 1;
                if let Some(registry) = &file_siblings {
                    registry.dropped(&target.node_id);
                }
                tracing::debug!(
                    node = %fauna_core::hex32::encode(&target.node_id),
                    "peer dial: sibling interaction blew its budget — abandoned this pass"
                );
            }
        }
    }
    Ok(Some(report))
}

/// Dial a target's transport candidates and open the peer channel on the
/// resulting connection — shared by the sibling dial pass ([`dial_one`])
/// and the custody dial pass (`custody_leg::dial_one_owner`), which
/// otherwise re-derived this identically (scouted 2026-08-19). Mutual
/// admission differs per caller (a sibling admits over the fleet witness, a
/// custodian over the custody grant) and stays local to each.
pub(crate) async fn dial_and_open_channel(
    transport: &Arc<dyn PeerTransport>,
    target: &fauna_peer_sync::PeerDialTarget,
) -> Result<Arc<fauna_peer_channel::PeerChannel>> {
    let conn = transport
        .dial(
            fauna_transport::EndpointKey::from_bytes(target.node_id),
            target.candidates.clone(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("dial: {e}"))?;
    Ok(Arc::new(
        fauna_peer_channel::PeerChannel::open(conn)
            .await
            .context("peer channel")?,
    ))
}

/// One content scope's walk + missing-block pull over an open channel —
/// shared by [`dial_one`] and `custody_leg::dial_one_owner`, which
/// otherwise re-derived this identically apart from log-context wording
/// (scouted 2026-08-19). `walk_context`/`pull_context` reproduce each
/// caller's own wording verbatim (they aren't a common prefix + suffix —
/// e.g. `"content walk"` / `"blocks pull"` vs. `"custody content walk"` /
/// `"custody blocks pull"`) so error messages stay exactly as before. The
/// walk always runs; the pull moves bytes only as `nest` and the channel's
/// live path allow (`fauna_peer_sync::pull_missing_blocks`).
pub(crate) async fn walk_and_pull_content_scope<B: StoreBackend>(
    store: &AccountStore<B>,
    requester: &fauna_peer_sync::PeerRequester,
    channel: &fauna_peer_channel::PeerChannel,
    cs: &fauna_protocol::scope::ContentScope,
    walk_context: &str,
    pull_context: &str,
    nest: fauna_transport::NestPath,
) -> Result<fauna_peer_sync::PullReport> {
    let scope = cs.to_string();
    crate::content_scope_plane::ContentScopePlane::new(store, requester, cs.clone())
        .walk()
        .await
        .with_context(|| format!("{walk_context} {scope}"))?;
    fauna_peer_sync::pull_missing_blocks(store, channel, &scope, nest)
        .await
        .with_context(|| format!("{pull_context} {scope}"))
}

/// One sibling: dial → channel → mutual admission → the walks.
#[allow(clippy::too_many_arguments)] // see dial_pass
async fn dial_one<B>(
    transport: &Arc<dyn PeerTransport>,
    target: &fauna_peer_sync::PeerDialTarget,
    own_witness: &fauna_core::encoding::EmbedAsBytes,
    account: &[u8; 32],
    store: &AccountStore<B>,
    schedule: &fauna_core::crypto::AccountStateKeySchedule,
    trust: &crate::generation_tip::GenerationTrust,
    writer_key: &SigningKey,
    slot: &dyn PrincipalCustody,
    content_scopes: &[fauna_protocol::scope::ContentScope],
    revoked: &WithdrawalSnapshot<Vec<u8>>,
    removed_devices: &WithdrawalSnapshot<[u8; 32]>,
    custodied_exclusions: &fauna_peer_sync::admission::CustodiedExclusions,
    own_endpoints: Option<&DeviceEndpoints>,
    nest: fauna_transport::NestPath,
) -> Result<(
    usize,
    fauna_peer_sync::PullReport,
    Option<DeviceEndpoints>,
    Arc<fauna_peer_channel::PeerChannel>,
)>
where
    B: StoreBackend,
{
    use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};

    let channel = dial_and_open_channel(transport, target).await?;
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Full-form admission: a SIBLING answers with a `DeviceAuthorization`,
    // a CUSTODIAN with the custody grant — the revocation view (this side's
    // synced grant-event log, pump-refreshed) is what lets the custody reply
    // be evaluated at all (fail-closed without one, and while it has not
    // derived).
    let revoked_view = revoked.custody_revoked_view();
    let removed_view = removed_devices.device_removed_view(*account, custodied_exclusions);
    // T13 step 4: a CUSTODIAN among these targets cannot read our fleet-only
    // `device-endpoints` kind, so this exchange is the only way it learns
    // where we moved — and its reply is the only way we learn where it did.
    let outcome = fauna_peer_sync::admit_over_as(
        &channel,
        fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION,
        own_witness.clone(),
        account,
        now_secs,
        fauna_peer_sync::AdmissionViews {
            custody_revoked: Some(&revoked_view),
            device_removed: Some(&removed_view),
        },
        own_endpoints.cloned(),
    )
    .await
    .context("admission")?;

    let requester = fauna_peer_sync::PeerRequester::new(Arc::clone(&channel));
    let mut applied = 0usize;
    let mut pulled = fauna_peer_sync::PullReport::default();
    // The two class-2 planes, custody attached to both — the production
    // shape (`account_runtime`'s own planes), and the seam the crypto-shred
    // contract binds: an authored `Shredded` mint merged over THIS leg must
    // drop the retained key exactly as one merged over the nest leg does, and
    // an unauthored one drops it on neither (the plane's walk hook judges it).
    //
    // Neither is handed a predecessor schedule, by rule
    // (`succession-aftermath.md` § Re-key scope → *Which walks carry*): the
    // bound nest's walk alone carries a predecessor identity's delegable
    // rows. Admission says who this peer is, not which rows it relays, so a
    // retired key is never tried on what a peer serves; the carried values
    // arrive here as a sibling's own rows, under this account's schedule.
    for scope in [ACCOUNT_STATE_SCOPE, ACCOUNT_STATE_FLEET_SCOPE] {
        let plane = crate::account_state_plane::AccountStatePlane::new_pull_only(
            store, &requester, schedule, writer_key, trust, scope,
        )?
        .with_generation_custody(slot);
        let report = plane
            .walk()
            .await
            .with_context(|| format!("walk {scope}"))?;
        applied += report.applied;
    }
    // The content scopes: the class-1 feed walk plus the want-list block
    // pull (class-3 bytes) — the charter's peer-wise scope set.
    for cs in content_scopes {
        let pull = walk_and_pull_content_scope(
            store,
            &requester,
            &channel,
            cs,
            "content walk",
            "blocks pull",
            nest,
        )
        .await?;
        pulled.fetched += pull.fetched;
        pulled.missing += pull.missing;
        pulled.relay_deferred += pull.relay_deferred;
    }
    Ok((applied, pulled, outcome.peer_endpoints, channel))
}

/// Take the listener down: the node (its accept loop and every inbound
/// channel end with it), the serve side, and the transport — the dial
/// endpoint is the same iroh endpoint, so dropping every handle is what
/// frees the socket — plus the facts composed at bind, so nothing keeps
/// advertising an address that no longer answers.
fn drop_listener(state: &mut PeerLegState) {
    state.node = None;
    state.server = None;
    state.transport = None;
    state.facts = None;
    state.bound_addrs.clear();
    // The held sibling channels ride the endpoint going down: no engine may
    // ask through them, and none may keep it alive.
    if let Some(file_sync) = state.file_sync.take() {
        file_sync.siblings.clear();
    }
}

/// The participation half of the ensure step (`p2p.md` § Per-device
/// participation): read this device's own row; if the nest lists a pending
/// brake on this device's enrolled roster row, fold it (local off — the
/// nest can only ever bring a listener down); then send whatever report the
/// nest has not heard yet, proven by this machine's device principal (the
/// self arm). Every nest leg is best-effort: an unreachable nest changes
/// nothing about the local verdict, and the next pass retries.
///
/// The roster read is the only place a brake is ever consulted — never
/// live at a bind door — which is what makes "an unreachable nest
/// changes nothing" hold: the folded row is the fact both doors read.
async fn reconcile_participation<B, R>(
    state: &PeerLegState,
    store: &AccountStore<B>,
    writer_key: &SigningKey,
    slot: &dyn PrincipalCustody,
    rpc: &R,
) -> crate::p2p_participation::P2pParticipation
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut row = crate::p2p_participation::load(store).await;
    // No enrolled roster row yet (the ceremony's nest legs have not landed):
    // nothing to fold from and nothing to report on. The local choice
    // stands on its own.
    let Some(row_hex) = slot.grant_registration_row() else {
        return row;
    };
    let (Ok(device_id), Ok(actor_id)) = (
        fauna_core::hex32::decode(&row_hex),
        fauna_core::hex32::decode(&state.actor_id_hex),
    ) else {
        return row;
    };

    // The brake: this device's own roster row, as the nest lists it.
    let listed: Result<SyncDevicesListReply, _> = rpc
        .request(
            "fauna.sync.devices.list",
            SyncDevicesListRequest {
                extra: Default::default(),
            },
        )
        .await;
    match listed {
        Ok(reply) => {
            let off_requested = reply
                .devices
                .iter()
                .find(|d| d.device_id == row_hex)
                .is_some_and(|d| d.p2p_off_requested);
            if row.fold_brake(off_requested) {
                tracing::info!(
                    "peer leg: another of this account's devices asked this one to stop \
                     peer transfers — participation folded to off"
                );
                if let Err(e) = crate::p2p_participation::save(store, &row).await {
                    tracing::warn!("peer leg: folded brake not persisted: {e:#}");
                }
            }
        }
        Err(e) => tracing::debug!("peer leg: roster unreachable ({e}) — brake not consulted"),
    }

    // The report: the nest hears this device's state exactly when it differs
    // from what it heard last.
    if let Some(on) = row.report_owed() {
        let req = fauna_client_sync::build_p2p_participation_report(
            &actor_id, writer_key, &device_id, on,
        );
        let sent: Result<SyncDeviceP2pParticipationSetReply, _> = rpc
            .request(KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET, req)
            .await;
        match sent {
            Ok(_) => {
                row.mark_reported(on);
                if let Err(e) = crate::p2p_participation::save(store, &row).await {
                    tracing::debug!("peer leg: report mark not persisted: {e:#}");
                }
            }
            Err(e) => {
                tracing::debug!("peer leg: participation report not sent ({e}) — next pass retries")
            }
        }
    }
    row
}

async fn fetch_nest_facts<R: RpcRequester>(rpc: &R) -> Result<PeerLegNestFacts> {
    let reply: NestInfoReply = rpc
        .request(NODE_INFO_KIND, NestInfoRequest::default())
        .await
        .map_err(|e| anyhow::anyhow!("fauna.nest.info: {e}"))?;
    Ok(PeerLegNestFacts {
        capabilities: reply.capabilities,
        iroh_relay_url: reply.iroh_relay_url,
    })
}

/// Persist freshly-fetched nest facts to the local brake cache, best-effort:
/// an encode/write failure is logged and swallowed, never propagated — the
/// caller already has the fresh facts in hand, the cache is only for the
/// next cold start. `verb` distinguishes the two call sites' log wording
/// ("refreshed" while already bound vs "written" on first bind).
async fn cache_nest_facts<B: StoreBackend>(
    store: &AccountStore<B>,
    facts: &PeerLegNestFacts,
    verb: &str,
) {
    if let Ok(bytes) = fauna_core::encoding::canonical_encode(facts)
        && let Err(e) = store.backend().meta_put(META_NEST_FACTS, &bytes).await
    {
        tracing::debug!("peer leg: brake cache not {verb}: {e:#}");
    }
}

/// LAN candidates from where the listener bound: a specifically-bound
/// non-loopback address rides as-is; an unspecified bind contributes its port,
/// crossed with this device's interface addresses. `public_addrs` stays empty
/// by ruling (module docs); the read side re-filters everything through PT-4 +
/// the LAN arithmetic regardless (`sibling_dial_targets`).
fn compose_facts(
    bound: &[SocketAddr],
    lan_ips: &[Ipv4Addr],
    relay_url: Option<String>,
) -> EndpointFacts {
    EndpointFacts {
        lan_addrs: fauna_core::device_endpoints::lan_socket_addrs(bound, lan_ips),
        public_addrs: Vec::new(),
        relay_url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The peer leg's planes are handed no predecessor schedule or retired
    /// fleet-only key, by rule
    /// (`succession-aftermath.md` § Re-key scope → *Which walks carry*): the
    /// bound nest's walk alone carries, because a retired key is tried only
    /// on what the home nest serves. If this trips, read that paragraph
    /// before wiring `with_predecessor_schedules` onto a peer plane.
    #[test]
    fn the_peer_leg_hands_its_planes_no_predecessor_schedule() {
        let src = include_str!("peer_leg.rs");
        let production = src
            .split_once("#[cfg(test)]\nmod tests")
            .expect("the test module moved")
            .0;
        assert!(
            !production.contains("with_predecessor_schedules("),
            "the peer leg grew a `with_predecessor_schedules` call: the carry \
             is the bound nest's walk alone (`succession-aftermath.md` § Re-key \
             scope → *Which walks carry*)"
        );
        for retired in [
            "with_predecessor_mint_keys(",
            "with_predecessor_machinery_keys(",
        ] {
            assert!(
                !production.contains(retired),
                "the peer leg grew a `{retired}` call: a retired key is tried only \
                 on what the bound nest serves (`succession-aftermath.md` § Re-key \
                 scope → *Which walks carry*)"
            );
        }
    }

    #[test]
    fn unspecified_binds_cross_ports_with_interface_addresses() {
        let bound = vec!["0.0.0.0:4711".parse().unwrap()];
        let ips = vec![Ipv4Addr::new(192, 168, 1, 20), Ipv4Addr::new(10, 0, 0, 5)];
        let facts = compose_facts(&bound, &ips, Some("https://relay.example".into()));
        assert_eq!(
            facts.lan_addrs,
            vec!["192.168.1.20:4711".to_string(), "10.0.0.5:4711".to_string()]
        );
        assert!(facts.public_addrs.is_empty(), "no reflexive guess ever");
        assert_eq!(facts.relay_url.as_deref(), Some("https://relay.example"));
    }

    #[test]
    fn specific_binds_ride_as_is_and_loopback_is_dropped() {
        let bound: Vec<SocketAddr> = vec![
            "192.168.1.20:4711".parse().unwrap(),
            "127.0.0.1:4711".parse().unwrap(),
        ];
        let facts = compose_facts(&bound, &[], None);
        assert_eq!(facts.lan_addrs, vec!["192.168.1.20:4711".to_string()]);
        assert_eq!(facts.relay_url, None);
    }

    #[test]
    fn duplicate_ports_and_addresses_collapse() {
        let bound: Vec<SocketAddr> = vec![
            "0.0.0.0:4711".parse().unwrap(),
            "0.0.0.0:4711".parse().unwrap(),
            "192.168.1.20:4711".parse().unwrap(),
        ];
        let ips = vec![Ipv4Addr::new(192, 168, 1, 20)];
        let facts = compose_facts(&bound, &ips, None);
        assert_eq!(facts.lan_addrs, vec!["192.168.1.20:4711".to_string()]);
    }

    /// The facts refresh: while bound, a pass that sees a different
    /// interface list recomposes the LAN candidates (the device-endpoints
    /// step's write-if-changed then republishes), and a nest that cannot be
    /// reached keeps the last-known relay half rather than dropping it.
    #[tokio::test]
    async fn a_lan_change_recomposes_the_facts_while_bound() {
        struct DeadNest;
        impl fauna_protocol::RpcRequester for DeadNest {
            type Error = anyhow::Error;
            async fn request<Req, Reply>(
                &self,
                kind: &'static str,
                _payload: Req,
            ) -> anyhow::Result<Reply>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                anyhow::bail!("unreachable nest ({kind})")
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[9; 32]);
        let key_pub = key.verifying_key().to_bytes();
        let actor_hex = fauna_core::hex32::encode(&[0x42; 32]);
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &actor_hex,
            WriterId(key_pub),
        )
        .await
        .unwrap();
        let slot = crate::principal_bundle::PrincipalSlot::resolve(
            std::sync::Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                "fauna-account-store",
                dir.path().join("creds"),
            )),
            actor_hex.clone(),
            dir.path().to_path_buf(),
            &key_pub,
            None,
        );
        // A bound-ish state: a live node over the seam double, one
        // unspecified bind, last-known nest facts carrying a relay.
        let net = fauna_transport::testing::listeners();
        let node = fauna_peer_channel::PeerNode::start(
            Arc::new(fauna_transport::testing::MemTransport {
                me: fauna_transport::EndpointKey::from_bytes(key_pub),
                listeners: net,
            }),
            String::new(),
        )
        .await;
        let mut state = PeerLegState::new(None, dir.path().to_path_buf(), actor_hex);
        state.node = Some(node);
        state.bound_addrs = vec!["0.0.0.0:4711".parse().unwrap()];
        state.nest_facts = Some(PeerLegNestFacts {
            capabilities: vec!["peer-sync".into()],
            iroh_relay_url: Some("https://relay.example".into()),
        });
        state.facts = Some(compose_facts(
            &state.bound_addrs,
            &[Ipv4Addr::new(192, 168, 1, 20)],
            Some("https://relay.example".into()),
        ));

        let moved_lan = [Ipv4Addr::new(10, 0, 0, 5)];
        let pass = ensure_bound(&mut state, &store, &key, &slot, &DeadNest, &moved_lan)
            .await
            .unwrap();
        assert_eq!(pass, PeerLegPass::AlreadyBound);
        let facts = state.facts.expect("facts stay composed");
        assert_eq!(
            facts.lan_addrs,
            vec!["10.0.0.5:4711".to_string()],
            "the moved interface replaces the old candidate"
        );
        assert_eq!(
            facts.relay_url.as_deref(),
            Some("https://relay.example"),
            "an unreachable nest keeps the last-known relay half"
        );
    }

    #[test]
    fn nest_facts_cache_round_trips_dag_cbor() {
        let facts = PeerLegNestFacts {
            capabilities: vec!["peer-sync".into(), "relay".into()],
            iroh_relay_url: Some("https://relay.example".into()),
        };
        let bytes = fauna_core::encoding::canonical_encode(&facts).unwrap();
        let back: PeerLegNestFacts = fauna_core::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, facts);

        let none = PeerLegNestFacts {
            capabilities: Vec::new(),
            iroh_relay_url: None,
        };
        let bytes = fauna_core::encoding::canonical_encode(&none).unwrap();
        let back: PeerLegNestFacts = fauna_core::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, none);
    }

    // ──  PER_SIBLING_BUDGET's witness ───────

    /// The sibling-dial twin of `custody_leg`'s
    /// `a_stalled_owner_device_does_not_block_the_next_owner_device`:
    /// `PER_SIBLING_BUDGET`'s elapsed arm must not stop the sequential
    /// `targets` loop — a sibling that blows its budget is counted `failed`,
    /// and the NEXT sibling in the same pass is still dialed.
    ///
    /// `#[tokio::test(start_paused = true)]` turns the 120 s budget into an
    /// instant clock advance (convention 14). The OUTER
    /// `tokio::time::timeout` is the mutation-verification guard: delete the
    /// inner wrapper from `dial_pass` and the slow sibling's dial hangs with
    /// no timer anywhere else in the test for the paused clock to advance
    /// to — the outer guard turns that into a red `.expect()` panic instead
    /// of a hung test.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_sibling_does_not_block_the_next_sibling() {
        struct HangsForOneNode {
            hangs: [u8; 32],
            dialed: Arc<std::sync::Mutex<Vec<[u8; 32]>>>,
        }

        #[async_trait::async_trait]
        impl PeerTransport for HangsForOneNode {
            async fn dial(
                &self,
                peer: fauna_transport::EndpointKey,
                _candidates: fauna_transport::PathCandidates,
            ) -> Result<Box<dyn fauna_transport::PeerConn>, fauna_transport::TransportError>
            {
                self.dialed.lock().unwrap().push(*peer.as_bytes());
                if *peer.as_bytes() == self.hangs {
                    // Never resolves — the slow sibling. No timer of its
                    // own; only the (inner, or absent-under-mutation outer)
                    // budget can ever make this `.await` return.
                    std::future::pending::<()>().await;
                    unreachable!("a pending future never resolves");
                }
                Err(fauna_transport::TransportError::NoPath)
            }

            async fn listen(
                &self,
            ) -> Result<fauna_transport::IncomingConns, fauna_transport::TransportError>
            {
                Err(fauna_transport::TransportError::Unsupported)
            }

            fn local_identity(&self) -> fauna_transport::EndpointKey {
                fauna_transport::EndpointKey::from_bytes([0; 32])
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[9; 32]);
        let key_pub = key.verifying_key().to_bytes();
        // The account whose `device-auth` this device carries — the slot's
        // `actor_id_hex` MUST match, or the store below verifies against one
        // account, and the persist step below (rightly) refuses the other.
        let account = fauna_core::identity::ActorKeypair::generate();
        let actor_hex = fauna_core::hex32::encode(&account.actor_id().0);
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &actor_hex,
            WriterId(key_pub),
        )
        .await
        .unwrap();

        // A slot carrying a real, decodable enrollment witness (precedent:
        // `principal_bundle.rs`'s own `grant_over` fixture) — `dial_pass`
        // gates on `slot.device_authorization()` being `Some` before it ever
        // reaches the target loop.
        let slot = crate::principal_bundle::PrincipalSlot::resolve(
            std::sync::Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                "fauna-account-store",
                dir.path().join("creds"),
            )),
            actor_hex.clone(),
            dir.path().to_path_buf(),
            &key_pub,
            None,
        );
        let auth = fauna_core::data::DeviceAuthorization {
            actor_id: account.actor_id(),
            device_key: key_pub,
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: fauna_core::data::Timestamp::now(),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(&account, &auth).expect("sign");
        slot.store_device_authorization(
            fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
            &key_pub,
        )
        .expect("persist device authorization");

        // Two verified siblings (a sibling is a dial candidate only while it
        // is one). `states_of_kind` orders by key, so `sibling_dial_targets`
        // yields the lower id first regardless of insertion order — that one
        // is the slow sibling.
        let mut siblings = [
            SigningKey::from_bytes(&[0x61; 32]),
            SigningKey::from_bytes(&[0x62; 32]),
        ];
        siblings.sort_by_key(|k| k.verifying_key().to_bytes());
        let [slow_node, fast_node] = siblings.each_ref().map(|k| k.verifying_key().to_bytes());
        for sibling in &siblings {
            stage_enrollment(&store, &account, sibling).await;
            stage_endpoints(&store, sibling.verifying_key().to_bytes()).await;
        }

        let dialed: Arc<std::sync::Mutex<Vec<[u8; 32]>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = PeerLegState::new(None, dir.path().to_path_buf(), actor_hex);
        state.transport = Some(Arc::new(HangsForOneNode {
            hangs: slow_node,
            dialed: Arc::clone(&dialed),
        }));

        let schedule = fauna_core::crypto::AccountStateKeySchedule::derive(
            &fauna_core::crypto::BackupKey::derive(account.secret_bytes()),
        );
        let trust = crate::generation_tip::GenerationTrust {
            root: account.actor_id(),
            prior: Vec::new(),
            trusted_holders: Default::default(),
        };

        let report = tokio::time::timeout(
            PER_SIBLING_BUDGET * 3,
            dial_pass(
                &state,
                &store,
                &schedule,
                &trust,
                &key,
                &slot,
                &[],
                &[],
                None,
                fauna_transport::NestPath::Reachable,
            ),
        )
        .await
        .expect(
            "dial_pass must return within an outer budget — if this fires, the inner \
             PER_SIBLING_BUDGET timeout is gone and the slow sibling hung the whole pass",
        )
        .unwrap()
        .expect("a bound transport with an enrolled slot yields a report");

        assert_eq!(report.targets, 2);
        assert_eq!(
            report.failed, 2,
            "the elapsed slow sibling and the fast refusal both count as failed"
        );
        assert_eq!(report.admitted, 0);
        assert_eq!(
            *dialed.lock().unwrap(),
            vec![slow_node, fast_node],
            "the fast sibling must still be dialed after the slow one blows its budget"
        );
    }

    /// After an in-process succession the successor's plane still carries
    /// the predecessor's device-endpoints row, and that row names THIS
    /// machine's retired writer: nobody answers for it, so dialing it spent
    /// a whole failing dial (~30 s) on the successor's first pass and held a
    /// gesture's parked put past the agent's patience (measured on tui,
    /// 2026-09-30). The retired writer's enrollment is the retired root's,
    /// which the successor's fleet view never verifies, so the membership
    /// filter keeps it off the dial list — and a real sibling is still
    /// dialed.
    #[tokio::test]
    async fn a_successor_never_dials_its_own_predecessor_writer() {
        struct RecordsDials(Arc<std::sync::Mutex<Vec<[u8; 32]>>>);

        #[async_trait::async_trait]
        impl PeerTransport for RecordsDials {
            async fn dial(
                &self,
                peer: fauna_transport::EndpointKey,
                _candidates: fauna_transport::PathCandidates,
            ) -> Result<Box<dyn fauna_transport::PeerConn>, fauna_transport::TransportError>
            {
                self.0.lock().unwrap().push(*peer.as_bytes());
                Err(fauna_transport::TransportError::NoPath)
            }

            async fn listen(
                &self,
            ) -> Result<fauna_transport::IncomingConns, fauna_transport::TransportError>
            {
                Err(fauna_transport::TransportError::Unsupported)
            }

            fn local_identity(&self) -> fauna_transport::EndpointKey {
                fauna_transport::EndpointKey::from_bytes([0; 32])
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let credentials =
            std::sync::Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                "fauna-account-store",
                dir.path().join("creds"),
            ));
        // The predecessor ran on this machine first: its slot holds the
        // writer key it minted, exactly as the predecessor's assembly left it.
        let predecessor = fauna_core::identity::ActorKeypair::generate();
        let (predecessor_writer, _) =
            fauna_account_plane::principal_bundle::mint_or_load_writer_key(
                &*credentials,
                &fauna_core::hex32::encode(&predecessor.actor_id().0),
            )
            .unwrap();
        let retired = predecessor_writer.verifying_key().to_bytes();

        let successor = fauna_core::identity::ActorKeypair::generate();
        let successor_hex = fauna_core::hex32::encode(&successor.actor_id().0);
        let key = SigningKey::from_bytes(&[7; 32]);
        let key_pub = key.verifying_key().to_bytes();
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &successor_hex,
            WriterId(key_pub),
        )
        .await
        .unwrap();
        let slot = crate::principal_bundle::PrincipalSlot::resolve(
            credentials,
            successor_hex.clone(),
            dir.path().to_path_buf(),
            &key_pub,
            None,
        );
        let auth = fauna_core::data::DeviceAuthorization {
            actor_id: successor.actor_id(),
            device_key: key_pub,
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: fauna_core::data::Timestamp::now(),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(&successor, &auth).expect("sign");
        slot.store_device_authorization(
            fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
            &key_pub,
        )
        .expect("persist device authorization");

        // What the successor's plane carries: the predecessor's endpoints row
        // and its enrollment, certified by the retired root — beside a real
        // sibling enrolled afresh under the successor.
        stage_enrollment(&store, &predecessor, &predecessor_writer).await;
        stage_endpoints(&store, retired).await;
        let sibling_key = SigningKey::from_bytes(&[0x62; 32]);
        let sibling = sibling_key.verifying_key().to_bytes();
        stage_enrollment(&store, &successor, &sibling_key).await;
        stage_endpoints(&store, sibling).await;

        let dialed: Arc<std::sync::Mutex<Vec<[u8; 32]>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = PeerLegState::new(None, dir.path().to_path_buf(), successor_hex);
        state.transport = Some(Arc::new(RecordsDials(Arc::clone(&dialed))));

        let schedule = fauna_core::crypto::AccountStateKeySchedule::derive(
            &fauna_core::crypto::BackupKey::derive(successor.secret_bytes()),
        );
        let trust = crate::generation_tip::GenerationTrust {
            root: successor.actor_id(),
            prior: vec![predecessor.actor_id()],
            trusted_holders: Default::default(),
        };
        let report = dial_pass(
            &state,
            &store,
            &schedule,
            &trust,
            &key,
            &slot,
            &[],
            &[],
            None,
            fauna_transport::NestPath::Reachable,
        )
        .await
        .unwrap()
        .expect("a bound transport with an enrolled slot yields a report");

        assert_eq!(
            *dialed.lock().unwrap(),
            vec![sibling],
            "the predecessor's writer is this machine — only the real sibling is dialed"
        );
        assert_eq!(report.targets, 1);
    }

    /// Records every dial and refuses it — the dial list, observed.
    struct RecordsEveryDial(Arc<std::sync::Mutex<Vec<[u8; 32]>>>);

    #[async_trait::async_trait]
    impl PeerTransport for RecordsEveryDial {
        async fn dial(
            &self,
            peer: fauna_transport::EndpointKey,
            _candidates: fauna_transport::PathCandidates,
        ) -> Result<Box<dyn fauna_transport::PeerConn>, fauna_transport::TransportError> {
            self.0.lock().unwrap().push(*peer.as_bytes());
            Err(fauna_transport::TransportError::NoPath)
        }

        async fn listen(
            &self,
        ) -> Result<fauna_transport::IncomingConns, fauna_transport::TransportError> {
            Err(fauna_transport::TransportError::Unsupported)
        }

        fn local_identity(&self) -> fauna_transport::EndpointKey {
            fauna_transport::EndpointKey::from_bytes([0; 32])
        }
    }

    /// Stage a device-endpoints entry for `node` — door-less, the sanctioned
    /// proof pattern (`fauna_peer_sync::discovery` module docs).
    async fn stage_endpoints(store: &AccountStore<SqliteBackend>, node: [u8; 32]) {
        let value = DeviceEndpoints {
            node_id: node,
            lan_addrs: Vec::new(),
            public_addrs: Vec::new(),
            relay_url: None,
        };
        stage_row(
            store,
            fauna_protocol::merge_policy::KIND_DEVICE_ENDPOINTS,
            fauna_core::hex32::encode(&node),
            fauna_core::encoding::canonical_encode(&value).unwrap(),
        )
        .await;
    }

    /// Stage `device`'s enrollment as `root` certified it, in production's
    /// self-signed shape — a verified member exactly when `root` is the
    /// account's root.
    async fn stage_enrollment(
        store: &AccountStore<SqliteBackend>,
        root: &fauna_core::identity::ActorKeypair,
        device: &SigningKey,
    ) {
        let id = device.verifying_key().to_bytes();
        let cert = fauna_core::data::DeviceAuthorization {
            actor_id: root.actor_id(),
            device_key: id,
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: fauna_core::data::Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(root, &cert).unwrap();
        let authorization = fauna_core::encoding::canonical_encode(
            &fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
        )
        .unwrap();
        let record = fauna_core::generation::sign_device_enrollment(device, authorization, 5_000);
        stage_row(
            store,
            fauna_protocol::merge_policy::KIND_DEVICE_SET,
            fauna_core::hex32::encode(&id),
            fauna_core::encoding::canonical_encode(&record).unwrap(),
        )
        .await;
    }

    async fn stage_row(
        store: &AccountStore<SqliteBackend>,
        kind: &str,
        key: String,
        value: Vec<u8>,
    ) {
        store
            .put_state(fauna_account_store::types::StateEntry {
                kind: kind.into(),
                key,
                scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                    .unwrap()
                    .into(),
                value,
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .unwrap();
    }

    /// **The dial list is the verified fleet, plus the custodians**
    /// (`account-sync-plane.md` § The peer leg → *Discovery*). Nothing forgets
    /// a merged device-endpoints entry, so a replica holds entries whose
    /// devices are gone: one a `Removed` row excludes, one no device-set row
    /// enrolls at all, one enrolled only under a retired root (a predecessor
    /// device after a succession). None of them is dialed; the verified
    /// sibling is, and so is a custodian, which is no fleet member and must
    /// not be caught by the filter.
    #[tokio::test]
    async fn the_dial_pass_dials_verified_members_and_custodians_only() {
        let dir = tempfile::tempdir().unwrap();
        let account = fauna_core::identity::ActorKeypair::generate();
        let actor_hex = fauna_core::hex32::encode(&account.actor_id().0);
        let key = SigningKey::from_bytes(&[0x0C; 32]);
        let key_pub = key.verifying_key().to_bytes();
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &actor_hex,
            WriterId(key_pub),
        )
        .await
        .unwrap();
        let slot = crate::principal_bundle::PrincipalSlot::resolve(
            Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                "fauna-account-store",
                dir.path().join("creds"),
            )),
            actor_hex.clone(),
            dir.path().to_path_buf(),
            &key_pub,
            None,
        );
        slot.store_device_authorization(device_witness(&account, key_pub), &key_pub)
            .expect("persist device authorization");
        stage_enrollment(&store, &account, &key).await;

        // The verified sibling.
        let sibling_key = SigningKey::from_bytes(&[0x51; 32]);
        let sibling = sibling_key.verifying_key().to_bytes();
        stage_enrollment(&store, &account, &sibling_key).await;
        stage_endpoints(&store, sibling).await;

        // A removed device: its merged device-set cell is the `Removed` row.
        let removed = SigningKey::from_bytes(&[0x52; 32])
            .verifying_key()
            .to_bytes();
        stage_row(
            &store,
            fauna_protocol::merge_policy::KIND_DEVICE_SET,
            fauna_core::hex32::encode(&removed),
            fauna_core::encoding::canonical_encode(
                &fauna_core::generation::DeviceSetRecord::Removed {
                    removed_at_ms: 9_000,
                    removed_by: sibling,
                },
            )
            .unwrap(),
        )
        .await;
        stage_endpoints(&store, removed).await;

        // A device no device-set row enrolls (a signed-out device's row
        // retired, or one this replica never merged).
        let unenrolled = SigningKey::from_bytes(&[0x53; 32])
            .verifying_key()
            .to_bytes();
        stage_endpoints(&store, unenrolled).await;

        // A predecessor device: enrolled, but by a retired root.
        let retired_root = fauna_core::identity::ActorKeypair::generate();
        let predecessor_key = SigningKey::from_bytes(&[0x54; 32]);
        let predecessor = predecessor_key.verifying_key().to_bytes();
        stage_enrollment(&store, &retired_root, &predecessor_key).await;
        stage_endpoints(&store, predecessor).await;

        // A custodian this account granted custody to — no fleet member.
        let custodian = SigningKey::from_bytes(&[0x55; 32])
            .verifying_key()
            .to_bytes();
        let grant_id = vec![0xC5; 16];
        stage_row(
            &store,
            fauna_protocol::merge_policy::KIND_CUSTODIAN_ENDPOINTS,
            fauna_core::custody_grant::custody_entry_key(&grant_id),
            fauna_core::encoding::canonical_encode(
                &fauna_core::custodian_endpoints::CustodianEndpoints {
                    grant_id,
                    endpoints: DeviceEndpoints {
                        node_id: custodian,
                        lan_addrs: Vec::new(),
                        public_addrs: Vec::new(),
                        relay_url: None,
                    },
                    ..Default::default()
                },
            )
            .unwrap(),
        )
        .await;

        let dialed: Arc<std::sync::Mutex<Vec<[u8; 32]>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = PeerLegState::new(None, dir.path().to_path_buf(), actor_hex);
        state.transport = Some(Arc::new(RecordsEveryDial(Arc::clone(&dialed))));
        let schedule = fauna_core::crypto::AccountStateKeySchedule::derive(
            &fauna_core::crypto::BackupKey::derive(account.secret_bytes()),
        );
        let trust = crate::generation_tip::GenerationTrust {
            root: account.actor_id(),
            prior: Vec::new(),
            trusted_holders: Default::default(),
        };
        let report = dial_pass(
            &state,
            &store,
            &schedule,
            &trust,
            &key,
            &slot,
            &[],
            &[],
            None,
            fauna_transport::NestPath::Reachable,
        )
        .await
        .unwrap()
        .expect("a bound transport with an enrolled slot yields a report");

        assert_eq!(
            *dialed.lock().unwrap(),
            vec![sibling, custodian],
            "only the verified sibling and the custodian are dialed — never a removed, \
             unenrolled or predecessor device's entry"
        );
        assert_eq!(report.targets, 2);
    }

    // ──  a snapshot that has not derived ──

    /// A nest nobody can reach: the brake comes from the store's cache.
    struct UnreachableNest;
    impl fauna_protocol::RpcRequester for UnreachableNest {
        type Error = anyhow::Error;
        async fn request<Req, Reply>(&self, kind: &'static str, _payload: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::bail!("unreachable nest ({kind})")
        }
    }

    /// An enrolled device on a fresh store: the account, the device key, the
    /// store and a credential slot carrying the root-signed witness — every
    /// bind gate but the transport open.
    struct Enrolled {
        account: fauna_core::identity::ActorKeypair,
        key: SigningKey,
        actor_hex: String,
        store: AccountStore<SqliteBackend>,
        slot: crate::principal_bundle::PrincipalSlot,
    }

    impl Enrolled {
        async fn at(dir: &std::path::Path) -> Self {
            let account = fauna_core::identity::ActorKeypair::generate();
            let key = SigningKey::from_bytes(&[0x0D; 32]);
            let key_pub = key.verifying_key().to_bytes();
            let actor_hex = fauna_core::hex32::encode(&account.actor_id().0);
            let store = AccountStore::open(
                SqliteBackend::open(dir).unwrap(),
                &actor_hex,
                WriterId(key_pub),
            )
            .await
            .unwrap();
            let slot = crate::principal_bundle::PrincipalSlot::resolve(
                Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                    "fauna-account-store",
                    dir.join("creds"),
                )),
                actor_hex.clone(),
                dir.to_path_buf(),
                &key_pub,
                None,
            );
            slot.store_device_authorization(device_witness(&account, key_pub), &key_pub)
                .expect("persist device authorization");
            // The brake, from the cache an offline start binds on.
            cache_nest_facts(
                &store,
                &PeerLegNestFacts {
                    capabilities: vec![fauna_protocol::discovery::capability::PEER_SYNC.into()],
                    iroh_relay_url: None,
                },
                "written",
            )
            .await;
            Enrolled {
                account,
                key,
                actor_hex,
                store,
                slot,
            }
        }

        fn trust(&self) -> crate::generation_tip::GenerationTrust {
            crate::generation_tip::GenerationTrust {
                root: self.account.actor_id(),
                prior: Vec::new(),
                trusted_holders: Default::default(),
            }
        }
    }

    /// `account`'s root-signed fleet cert over `device`.
    fn device_witness(
        account: &fauna_core::identity::ActorKeypair,
        device: [u8; 32],
    ) -> fauna_core::encoding::EmbedAsBytes {
        let auth = fauna_core::data::DeviceAuthorization {
            actor_id: account.actor_id(),
            device_key: device,
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: fauna_core::data::Timestamp::now(),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(account, &auth).expect("sign");
        fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env)
    }

    /// The fault the 2026-09-20 grading could not construct: this store's
    /// `states_of_kind` read fails for `fauna.state.device-set` — and ONLY for
    /// it, so every other read a bind makes (the brake cache, the serve
    /// store's own open) still succeeds. The planted row's `entry_version`
    /// holds TEXT, which no plane write can produce (every insert is typed):
    /// local corruption, never a remotely reachable state.
    const UNREADABLE_KEY: &str = "unreadable";

    fn plant_unreadable_device_set_row(store_dir: &std::path::Path) {
        rusqlite::Connection::open(
            store_dir.join(fauna_account_store::sqlite::ACCOUNT_STORE_DB_FILENAME),
        )
        .unwrap()
        .execute(
            "INSERT INTO state_entries
                 (kind, key, scope, value, merge_meta, entry_version, tombstone)
             VALUES (?1, ?2, ?3, x'00', NULL, 'not-an-integer', 0)",
            rusqlite::params![
                fauna_protocol::merge_policy::KIND_DEVICE_SET,
                UNREADABLE_KEY,
                fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE,
            ],
        )
        .unwrap();
    }

    fn lift_unreadable_device_set_row(store_dir: &std::path::Path) {
        rusqlite::Connection::open(
            store_dir.join(fauna_account_store::sqlite::ACCOUNT_STORE_DB_FILENAME),
        )
        .unwrap()
        .execute(
            "DELETE FROM state_entries WHERE key = ?1",
            rusqlite::params![UNREADABLE_KEY],
        )
        .unwrap();
    }

    /// A sibling of `enrolled`'s account dials the bound listener and runs the
    /// admission exchange, presenting its own fleet cert.
    async fn sibling_admits(
        net: &fauna_transport::testing::Listeners,
        enrolled: &Enrolled,
        sibling: [u8; 32],
    ) -> Result<()> {
        let dialer = fauna_transport::testing::MemTransport {
            me: fauna_transport::EndpointKey::from_bytes(sibling),
            listeners: Arc::clone(net),
        };
        let conn = dialer
            .dial(
                fauna_transport::EndpointKey::from_bytes(enrolled.key.verifying_key().to_bytes()),
                fauna_transport::PathCandidates::default(),
            )
            .await
            .expect("dial the bound listener");
        let channel = fauna_peer_channel::PeerChannel::open(conn)
            .await
            .expect("channel");
        fauna_peer_sync::admit_over_as(
            &channel,
            fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION,
            device_witness(&enrolled.account, sibling),
            &enrolled.account.actor_id().0,
            fauna_core::data::Timestamp::now().0,
            fauna_peer_sync::AdmissionViews::default(),
            None,
        )
        .await
        .map(|_| ())
    }

    /// The first-bind failure path, over the PRODUCTION ensure step: the
    /// removed-device refresh fails on the very first pass, the ensure step
    /// binds anyway (nothing else is wrong), and the bound server must NOT
    /// answer from the never-derived snapshot — an empty set there would admit
    /// every device the fleet has removed, at the door and on every request.
    /// It refuses the whole `DeviceAuthorization` arm instead, and the next
    /// successful refresh heals the already-bound server in place, no rebind.
    ///
    /// The sibling here is not removed; that is the point. A snapshot that
    /// has never derived cannot tell a removed device from a live one, so a
    /// refusal of this sibling is the only answer that also refuses every
    /// removed one.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_first_bind_over_a_failed_refresh_refuses_until_one_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let enrolled = Enrolled::at(dir.path()).await;
        let net = fauna_transport::testing::listeners();
        let me = enrolled.key.verifying_key().to_bytes();
        let factory: PeerTransportFactory = {
            let net = Arc::clone(&net);
            Arc::new(move |_inputs: PeerLegFactoryInputs| {
                let net = Arc::clone(&net);
                Box::pin(async move {
                    let transport: Arc<dyn PeerTransport> =
                        Arc::new(fauna_transport::testing::MemTransport {
                            me: fauna_transport::EndpointKey::from_bytes(me),
                            listeners: net,
                        });
                    Ok(PeerLegBinding {
                        transport,
                        bound_addrs: Vec::new(),
                        file_sync: None,
                    })
                })
            })
        };
        let mut state = PeerLegState::new(
            Some(factory),
            dir.path().to_path_buf(),
            enrolled.actor_hex.clone(),
        );
        let sibling = [0x51u8; 32];

        // Pass 1, in the pump's order: the refresh fails, the ensure step binds.
        plant_unreadable_device_set_row(dir.path());
        refresh_removed_devices(&state, &enrolled.store, &enrolled.trust())
            .await
            .expect_err("the planted row fails the device-set read");
        let pass = ensure_bound(
            &mut state,
            &enrolled.store,
            &enrolled.key,
            &enrolled.slot,
            &UnreachableNest,
            &[],
        )
        .await
        .expect("every bind gate is open");
        assert_eq!(pass, PeerLegPass::Bound);
        fauna_transport::testing::await_listening(&net, &me).await;
        // Refused at the admit door (the dialer sees only the wire code; the
        // server's details stay server-side). That it was the removal view
        // and nothing else is pass 2's job: the same server, the same
        // witness, admitted the moment the snapshot derives.
        let refused = sibling_admits(&net, &enrolled, sibling)
            .await
            .expect_err("a server whose removed-device snapshot never derived must refuse");
        assert!(
            format!("{refused:#}").contains("witness_refused"),
            "refused at the admit door: {refused:#}"
        );

        // Pass 2: the refresh succeeds, the ensure step finds the node up, and
        // the SAME server now admits — the view reads the snapshot live.
        lift_unreadable_device_set_row(dir.path());
        assert_eq!(
            refresh_removed_devices(&state, &enrolled.store, &enrolled.trust())
                .await
                .expect("the device-set read works again"),
            0
        );
        let pass = ensure_bound(
            &mut state,
            &enrolled.store,
            &enrolled.key,
            &enrolled.slot,
            &UnreachableNest,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(pass, PeerLegPass::AlreadyBound, "healed without a rebind");
        sibling_admits(&net, &enrolled, sibling)
            .await
            .expect("a derived snapshot that lists no one admits every sibling");
    }

    /// The snapshot's whole answer table, through the two views production
    /// hands the serve config and the dialer. Underived refuses; derived
    /// admits all but what it lists (an empty set is an answer, the
    /// absent-row-admits rule); another account's devices are never this
    /// snapshot's to answer, since no refresh on this machine could ever read
    /// that account's device-set rows — they answer from the custodied
    /// exclusion map (the held grants' owner-signed lists) alone.
    #[test]
    fn a_withdrawal_view_refuses_until_derived_then_only_what_it_lists() {
        let own = [0x0Au8; 32];
        let custodied = [0x0Bu8; 32];
        let (listed, unlisted) = ([0x21u8; 32], [0x22u8; 32]);

        let removed = WithdrawalSnapshot::<[u8; 32]>::underived();
        let exclusions = fauna_peer_sync::admission::CustodiedExclusions::default();
        let device_view = removed.device_removed_view(own, &exclusions);
        assert!(
            device_view(&own, &unlisted),
            "underived refuses every device"
        );
        assert!(
            !device_view(&custodied, &unlisted),
            "another account's devices are never this snapshot's to refuse"
        );
        // A custodied account answers from the held grants' lists, live —
        // and those lists never answer for the own account.
        exclusions.replace([(custodied, [listed].into()), (own, [unlisted].into())].into());
        assert!(
            device_view(&custodied, &listed),
            "a held grant's list refuses"
        );
        assert!(
            !device_view(&custodied, &unlisted),
            "and only what it lists"
        );
        exclusions.replace(Default::default());
        removed.replace(std::collections::HashSet::new());
        assert!(!device_view(&own, &unlisted), "derived and empty admits");
        removed.replace([listed].into());
        assert!(device_view(&own, &listed), "a listed device is refused");
        assert!(!device_view(&own, &unlisted), "an unlisted one is not");

        let revoked = WithdrawalSnapshot::<Vec<u8>>::underived();
        let custody_view = revoked.custody_revoked_view();
        assert!(custody_view(b"any-grant"), "underived refuses every grant");
        revoked.replace([b"revoked".to_vec()].into());
        assert!(custody_view(b"revoked"));
        assert!(!custody_view(b"live"));
    }

    /// The other half of the failure posture: a refresh that fails AFTER one
    /// succeeded leaves the last derived set in force — neither cleared (that
    /// would admit every device it lists) nor reset to underived (that would
    /// sever every sibling over one bad read). A removal merged while the
    /// read keeps failing therefore waits for the next success: the window
    /// `account-sync-plane.md` states as part of the bound.
    #[tokio::test]
    async fn a_failed_refresh_after_a_success_keeps_the_last_derived_set() {
        let dir = tempfile::tempdir().unwrap();
        let enrolled = Enrolled::at(dir.path()).await;
        let state = PeerLegState::new(None, dir.path().to_path_buf(), enrolled.actor_hex.clone());
        let own = enrolled.account.actor_id().0;
        let view = state
            .removed_devices
            .device_removed_view(own, &state.custodied_exclusions);
        let (listed, unlisted) = ([0x21u8; 32], [0x22u8; 32]);
        // An earlier pass derived a set naming one removed device.
        state.removed_devices.replace([listed].into());

        plant_unreadable_device_set_row(dir.path());
        refresh_removed_devices(&state, &enrolled.store, &enrolled.trust())
            .await
            .expect_err("the planted row fails the device-set read");
        assert!(view(&own, &listed), "the last derived removal still severs");
        assert!(
            !view(&own, &unlisted),
            "and the last derived set still admits the rest — not fail-closed"
        );
    }
}
