//! The share leg's serve side: an admitted **member of a shared set** reads
//! that set's change-log delta and pulls its manifests and chunks — the
//! cross-user twin of `fauna_peer_sync::server` (`p2p-shared-set-build.md` § Cross-user
//! shared-set transfer → *Build contract*; row 59 slice B).
//!
//! # The allowlist (wormability rule 3)
//!
//! A connection's serve set is **exactly** [`allowlisted_kinds`]:
//! `fauna.peer.node_info` (the pre-witness probe), the admission exchange, the
//! set change-log read, manifest fetch, the want-list chunk pull, and the
//! three offline-share-initiation ceremony kinds ([`crate::ceremony`] — gated
//! by their own actor-level pre-parse admission, not the verdict slot).
//! Nothing else is mapped, so `PeerChannel::serve` answers every other kind
//! `fauna.protocol.unknown_kind` — no config, capability-mint, admin or
//! key-material kind exists here, and **content keys never ride this plane**
//! (they stay on the M2 group's own rail; the ceremony deliver's admission
//! bundle is the GROUP scheme's rail — sealed end-to-end to the recipient's
//! reception key, opaque to this layer).
//!
//! # Admission: a verdict, never a witness
//!
//! Per connection there is one verdict slot. The admit exchange fills it
//! through [`crate::admission::evaluate_share_witness`] (a claim + the
//! evaluator's own roster consult); every data request then re-checks the set it
//! names against that verdict with [`crate::admission::verdict_admits_set`].
//! The two checks are deliberately separate: the connection is admitted to *a
//! set of sets*, and a request naming one outside it is refused even on a live,
//! admitted connection — which is also how eviction severs (the roster consult
//! re-runs at the next admission evaluation).
//!
//! # Rows travel as far as their proof (rule 4's sequenced ruling, lifted)
//!
//! The change-log arm serves every row this replica holds — its own, and every
//! writer-signed row it relays — never an unsigned row of someone else's,
//! decided by [`crate::provenance::serves_held_row`]. The filter lives in this crate
//! rather than behind the [`ShareStore`] seam on purpose: it is *policy*, so it
//! is unit-testable without a store, and an impl cannot weaken it by accident.
//!
//! # There is deliberately NO bind door here (rules 5 + 7)
//!
//! Unlike `fauna_peer_sync::server::start_peer_sync_node`, this module exposes
//! only [`ShareServer::handler_factory`]. The share plane must light **behind
//! the `p2p-share` nest capability token** (rule 7's version brake), and that
//! token lands with the nest legs; the contact-plane node's
//! lifecycle lands with the tui lead (slice E). A bind door added here before
//! the brake exists would be a listener with no brake — the exact hole rule 7
//! names. Whoever builds that door owns checking the brake first.

use std::sync::{Arc, Mutex, RwLock};

use fauna_core::identity::ActorId;
use fauna_peer_channel::{HandlerFactory, PeerHandlers, base_peer_handlers, from_value};
use fauna_peer_sync::admission::{AdmissionVerdict, admitted_verdict};
use fauna_peer_sync::quota::{MeteredPlane, QuotaConfig, QuotaLedger, metered_handler_factory};
use fauna_protocol::peer_share::{
    ERR_CEREMONY_NOT_EXPECTED, ERR_CEREMONY_REFUSED, ERR_NOT_ADMITTED, ERR_OVER_QUOTA,
    ERR_UNSUPPORTED, ERR_WITNESS_REFUSED, KIND_PEER_SHARE_ADMIT,
    KIND_PEER_SHARE_CEREMONY_ACCEPT_POLL, KIND_PEER_SHARE_CEREMONY_DELIVER,
    KIND_PEER_SHARE_CEREMONY_OFFER, KIND_PEER_SHARE_CHANGES_LIST, KIND_PEER_SHARE_CHUNKS_PULL,
    KIND_PEER_SHARE_MANIFESTS_GET, PeerShareAdmitReply, PeerShareAdmitRequest,
    PeerShareCeremonyAcceptPollReply, PeerShareCeremonyAcceptPollRequest,
    PeerShareCeremonyFrameAck, PeerShareCeremonyFrameRequest, PeerShareChange,
    PeerShareChangesListReply, PeerShareChangesListRequest, PeerShareChunk,
    PeerShareChunksPullReply, PeerShareChunksPullRequest, PeerShareManifest,
    PeerShareManifestsGetReply, PeerShareManifestsGetRequest, WITNESS_M2_MEMBERSHIP,
};
use fauna_protocol::{RpcError, Value};
use fauna_transport::EndpointKey;
use serde_bytes::ByteBuf;

use crate::admission::{
    GroupRosterState, SetMembership, evaluate_share_witness, verdict_admits_set,
    verdict_for_group_membership,
};
use crate::ceremony::{CeremonyRefusal, CeremonyState};
use crate::provenance::{LocalShareChange, serves_held_row};
use fauna_peer_sync::admission::AdmittedScopes;
use fauna_protocol::peer_share::WITNESS_GROUP_MEMBERSHIP;

/// Injected clock (epoch seconds) — quota windows never read the wall clock
/// directly, so tier-1 tests drive them (e2e convention 14 applied at tier 1).
///
/// The sync plane's alias, re-exported rather than re-declared: both planes
/// meter against the same ledger on the same clock, so a second declaration is
/// two names for one contract waiting to drift apart.
pub use fauna_peer_sync::server::NowFn;

/// The kinds this dispatcher serves — the whole allowlist (rule 3). Public so
/// the compliance pins assert the *set*, not a sample, and so the hardening
/// table can be tied to it (`fauna_peer_channel::hardening`).
///
/// The three `ceremony.*` kinds are the offline share-initiation carriage
/// (`crate::ceremony`): gated by the actor-level [`CeremonyState::admits`]
/// pre-parse check instead of the verdict slot, and fail-closed
/// ([`ERR_CEREMONY_NOT_EXPECTED`]) when no ceremony state is wired.
pub const fn allowlisted_kinds() -> [&'static str; 8] {
    [
        fauna_protocol::peer::KIND_PEER_NODE_INFO,
        KIND_PEER_SHARE_ADMIT,
        KIND_PEER_SHARE_CHANGES_LIST,
        KIND_PEER_SHARE_MANIFESTS_GET,
        KIND_PEER_SHARE_CHUNKS_PULL,
        KIND_PEER_SHARE_CEREMONY_OFFER,
        KIND_PEER_SHARE_CEREMONY_ACCEPT_POLL,
        KIND_PEER_SHARE_CEREMONY_DELIVER,
    ]
}

/// Serve-page bounds, sized under the peer channel's 1 MiB frame
/// (`fauna_peer_channel::MAX_FRAME_LEN`). Rust constants — never a knob
/// (§ Product invariants: no human chooses these).
const MAX_ROWS_PER_PAGE: u32 = 64;
/// Byte budget for one manifests/chunks reply; entries over it are `deferred`
/// rather than dropped, so a puller can always make progress by asking for
/// fewer.
const MAX_BODY_BYTES_PER_REPLY: usize = 700 * 1024;

/// What the serve side reads from local sync state, as one seam.
///
/// Async and `dyn`-shaped so the impl owns its I/O strategy — a native impl
/// proxies to the store-owner thread its sqlite backend requires, exactly as
/// `fauna_peer_sync::server::ServeStoreHandle` does. Keeping it a seam is what
/// holds this crate free of `rusqlite`/engine deps, the same discipline
/// [`SetMembership`] applies to the MLS engine.
///
/// **Byte answers are re-derivable, deterministic by contract.** An impl
/// answers [`Self::manifest_bytes`] and [`Self::chunk_body`] by re-sealing
/// the local plaintext under the set's content key
/// (`fauna_sync_engine::seal::seal_blob`, deterministic by contract), which
/// yields byte-identical artifacts to the ones any nest holds. That is why a
/// miss is `Ok(None)` — "this replica cannot produce it" is an ordinary,
/// expected answer (the file is not local, or is at another generation), not an
/// error. An impl MAY memoize a derived answer (content-addressed by
/// `store_key`/`manifest_hash`, so a memoized hit is never staler than a fresh
/// re-derivation would be) — [`fauna_sync_engine::peer_share_store::ShareServeMemo`]
/// does, bounded, for exactly this reason (`chunk_body`'s read +
/// reseal is the expensive half of serving a chunk, and this crate's own
/// per-reply byte budget forces a puller to re-ask for the same large chunk
/// across several rounds).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ShareStore: fauna_core::MaybeSendSync {
    /// The set's change rows with `seq` strictly greater than `since`, in seq
    /// order, at most `max_rows`. May include rows this side must not serve —
    /// the crate filters (see the module doc), so an impl never has to encode
    /// the provenance ruling.
    async fn changes_since(
        &self,
        set: &[u8; 32],
        since: i64,
        max_rows: u32,
    ) -> anyhow::Result<Vec<LocalShareChange>>;

    /// Canonical-encoded `ChunkManifest` bytes for `manifest_hash`, or `None`
    /// when this replica cannot produce them.
    async fn manifest_bytes(
        &self,
        set: &[u8; 32],
        manifest_hash: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>>;

    /// The stored chunk body for `store_key` (still sealed, still
    /// compression-framed), or `None` when this replica cannot produce it.
    async fn chunk_body(
        &self,
        set: &[u8; 32],
        store_key: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>>;

    /// Cheap existence check: true iff [`Self::chunk_body`] would answer
    /// `Some` for `store_key`, without necessarily paying that call's
    /// read+reseal cost. Only ever consulted when the reply budget is
    /// already spent (`handle_chunks_pull`) — the answer still separates a
    /// `missing` want (this replica will never produce it) from a merely
    /// `deferred` one (it will, once there is room), so `Ok(false)` here MUST
    /// agree with what `chunk_body` would return, not just guess "probably".
    /// Default: fall back to the expensive path — correct always, fast only
    /// for an impl that overrides this.
    async fn chunk_exists(&self, set: &[u8; 32], store_key: &[u8; 32]) -> anyhow::Result<bool> {
        Ok(self.chunk_body(set, store_key).await?.is_some())
    }
}

/// A [`ShareStore`] over MANY sets: one per-set source, hot-swappable as the
/// bound-set list changes (the pump feeds [`Self::set_sources`] alongside
/// [`ShareServer::set_own_claimed_sets`] — same snapshot, same cadence).
///
/// Routing posture mirrors the per-set stores': the server consults a store
/// only after its admission verdict named the set, so a *row* read for a set
/// with no source is a mis-wired assembly and fails loud, while a *byte* read
/// answers the ordinary multi-source `None` (bytes are self-verifying
/// regardless of source; rows are the provenance-bearing half).
#[derive(Default)]
pub struct MultiSetShareStore {
    sources: std::sync::RwLock<std::collections::HashMap<[u8; 32], Arc<dyn ShareStore>>>,
}

impl MultiSetShareStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the routed set wholesale — the bound-set snapshot's write side.
    pub fn set_sources(&self, sources: std::collections::HashMap<[u8; 32], Arc<dyn ShareStore>>) {
        *self.sources.write().expect("share router poisoned") = sources;
    }

    /// The currently routed set ids — what feeds
    /// [`ShareServer::set_own_claimed_sets`].
    pub fn sets(&self) -> Vec<[u8; 32]> {
        self.sources
            .read()
            .expect("share router poisoned")
            .keys()
            .copied()
            .collect()
    }

    fn source(&self, set: &[u8; 32]) -> Option<Arc<dyn ShareStore>> {
        self.sources
            .read()
            .expect("share router poisoned")
            .get(set)
            .cloned()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ShareStore for MultiSetShareStore {
    async fn changes_since(
        &self,
        set: &[u8; 32],
        since: i64,
        max_rows: u32,
    ) -> anyhow::Result<Vec<LocalShareChange>> {
        match self.source(set) {
            Some(s) => s.changes_since(set, since, max_rows).await,
            None => anyhow::bail!(
                "mis-wired share router: no source for admitted set {}",
                hex::encode(set)
            ),
        }
    }

    async fn manifest_bytes(
        &self,
        set: &[u8; 32],
        manifest_hash: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        match self.source(set) {
            Some(s) => s.manifest_bytes(set, manifest_hash).await,
            None => Ok(None),
        }
    }

    async fn chunk_body(
        &self,
        set: &[u8; 32],
        store_key: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        match self.source(set) {
            Some(s) => s.chunk_body(set, store_key).await,
            None => Ok(None),
        }
    }

    // Explicit delegation, not the trait default: the default would call
    // THIS router's own `chunk_body` and never reach the routed source's
    // override, silently losing whatever cheap existence check it offers
    // (this is exactly the loss that made the first cut of that
    // fix a no-op end to end).
    async fn chunk_exists(&self, set: &[u8; 32], store_key: &[u8; 32]) -> anyhow::Result<bool> {
        match self.source(set) {
            Some(s) => s.chunk_exists(set, store_key).await,
            None => Ok(false),
        }
    }
}

/// Everything the serve side needs beyond its two seams.
pub struct ShareServerConfig {
    /// The human label answered on `fauna.peer.node_info`.
    pub display_name: String,
    /// This node's own actor id — the identity a dialer proves against, and the
    /// author every row this side serves must carry (lowercase hex is derived
    /// once at construction).
    pub own_actor: [u8; 32],
    pub quotas: QuotaConfig,
    pub now: NowFn,
}

/// The share leg's serve side for one actor's app process.
pub struct ShareServer {
    config: ShareServerConfig,
    /// `own_actor` in the lowercase-hex spelling `SyncChange::author_actor_id`
    /// uses — derived once, so the per-row filter costs no allocation.
    own_actor_hex: String,
    /// The evaluator's roster consult (the M2 witness's whole verification).
    membership: Arc<dyn SetMembership + Send + Sync>,
    store: Arc<dyn ShareStore>,
    ledger: QuotaLedger,
    /// The sets this side claims in its own half of the admit exchange, so one
    /// round trip yields mutual (independently evaluated) admission. Pump-fed
    /// truth like `PeerSyncServer::set_custodied`: a set joined this pass is
    /// claimed on the next, and the admit path only ever *reads* the snapshot
    /// (no I/O on the ack path).
    own_claimed_sets: RwLock<Vec<[u8; 32]>>,
    /// The offline share-initiation ceremony's state seam
    /// ([`crate::ceremony`]), wired by the driver via [`Self::set_ceremony`].
    /// `None` (the default) fail-closed-refuses the ceremony kinds — a node
    /// with no receive affordance never parses a ceremony payload.
    ceremony: RwLock<Option<Arc<dyn CeremonyState>>>,
    /// The group-membership witness's evaluator seam
    /// ([`crate::admission::GroupRosterState`]), wired via
    /// [`Self::set_group_roster`]. `None` (the default) refuses the group
    /// witness kind — an evaluator that holds no group state can vouch for
    /// nothing, exactly as the M2 roster answers `false` for a set it does
    /// not hold.
    group_roster: RwLock<Option<Arc<dyn GroupRosterState + Send + Sync>>>,
}

impl ShareServer {
    pub fn new(
        config: ShareServerConfig,
        membership: Arc<dyn SetMembership + Send + Sync>,
        store: Arc<dyn ShareStore>,
    ) -> Self {
        let ledger = QuotaLedger::new(config.quotas.clone());
        Self {
            own_actor_hex: hex::encode(config.own_actor),
            config,
            membership,
            store,
            ledger,
            own_claimed_sets: RwLock::new(Vec::new()),
            ceremony: RwLock::new(None),
            group_roster: RwLock::new(None),
        }
    }

    /// Publish the sets this side will claim in the mutual half of the admit
    /// exchange. Pump-fed; replacing the snapshot is how a left set stops being
    /// claimed.
    pub fn set_own_claimed_sets(&self, sets: Vec<[u8; 32]>) {
        *self.own_claimed_sets.write().unwrap() = sets;
    }

    /// Wire the ceremony state seam ([`crate::ceremony`]) — the receive side
    /// of the offline share initiation. Without it the three `ceremony.*`
    /// kinds fail closed; with it they are gated per request by the seam's
    /// actor-level pre-parse admission.
    pub fn set_ceremony(&self, state: Arc<dyn CeremonyState>) {
        *self.ceremony.write().unwrap() = Some(state);
    }

    /// Wire the group-roster evaluator seam — what lets this node's admit
    /// exchange verify carried group-membership certificates
    /// ([`WITNESS_GROUP_MEMBERSHIP`]). Without it the group witness kind is
    /// refused.
    pub fn set_group_roster(&self, state: Arc<dyn GroupRosterState + Send + Sync>) {
        *self.group_roster.write().unwrap() = Some(state);
    }

    /// The per-connection handler factory for `PeerNode::start_with` — the
    /// shared rule-8 door ([`metered_handler_factory`]) over this plane's
    /// serve set. Sides admit independently: the verdict slot lives in
    /// [`Self::connection_handlers`], minted fresh per connection. This is
    /// the crate's ONLY listener-facing door (rules 5 + 7: no bind door
    /// exists until the `p2p-share` capability brake does).
    pub fn handler_factory(self: &Arc<Self>) -> HandlerFactory {
        metered_handler_factory(self, Self::connection_handlers)
    }

    /// The serve set for one connection — exactly [`allowlisted_kinds`].
    fn connection_handlers(self: &Arc<Self>, peer: EndpointKey) -> PeerHandlers {
        let verdict: Arc<Mutex<Option<AdmissionVerdict>>> = Arc::new(Mutex::new(None));

        let admit_server = Arc::clone(self);
        let admit_slot = Arc::clone(&verdict);
        let changes_server = Arc::clone(self);
        let changes_slot = Arc::clone(&verdict);
        let manifests_server = Arc::clone(self);
        let manifests_slot = Arc::clone(&verdict);
        let chunks_server = Arc::clone(self);
        let chunks_slot = Arc::clone(&verdict);
        let offer_server = Arc::clone(self);
        let poll_server = Arc::clone(self);
        let deliver_server = Arc::clone(self);

        base_peer_handlers(self.config.display_name.clone())
            .on(KIND_PEER_SHARE_ADMIT, move |req| {
                let server = Arc::clone(&admit_server);
                let slot = Arc::clone(&admit_slot);
                async move { server.handle_admit(&peer, &slot, req.payload).await }
            })
            .on(KIND_PEER_SHARE_CHANGES_LIST, move |req| {
                let server = Arc::clone(&changes_server);
                let slot = Arc::clone(&changes_slot);
                async move { server.handle_changes_list(&peer, &slot, req.payload).await }
            })
            .on(KIND_PEER_SHARE_MANIFESTS_GET, move |req| {
                let server = Arc::clone(&manifests_server);
                let slot = Arc::clone(&manifests_slot);
                async move { server.handle_manifests_get(&peer, &slot, req.payload).await }
            })
            .on(KIND_PEER_SHARE_CHUNKS_PULL, move |req| {
                let server = Arc::clone(&chunks_server);
                let slot = Arc::clone(&chunks_slot);
                async move { server.handle_chunks_pull(&peer, &slot, req.payload).await }
            })
            .on(KIND_PEER_SHARE_CEREMONY_OFFER, move |req| {
                let server = Arc::clone(&offer_server);
                async move { server.handle_ceremony_frame(&peer, req.payload).await }
            })
            .on(KIND_PEER_SHARE_CEREMONY_ACCEPT_POLL, move |req| {
                let server = Arc::clone(&poll_server);
                async move { server.handle_ceremony_accept_poll(&peer, req.payload).await }
            })
            .on(KIND_PEER_SHARE_CEREMONY_DELIVER, move |req| {
                let server = Arc::clone(&deliver_server);
                async move { server.handle_ceremony_frame(&peer, req.payload).await }
            })
    }

    /// The ceremony kinds' preflight — meter, then the actor-level pre-parse
    /// gate (wormability rule 1): a wired [`CeremonyState`] whose
    /// [`admits`](CeremonyState::admits) names the channel-proven actor.
    /// Deliberately BEFORE any payload decode, and fail-closed on both counts.
    fn ceremony_admitted(&self, peer: &EndpointKey) -> Result<Arc<dyn CeremonyState>, RpcError> {
        let now = (self.config.now)();
        // Rule 8: refused ceremony attempts spend budget like admitted ones.
        if !self.ledger.try_request(peer.as_bytes(), now) {
            return Err(over_quota());
        }
        let Some(state) = self.ceremony.read().unwrap().clone() else {
            return Err(ceremony_not_expected("no ceremony state on this node"));
        };
        if !state.admits(&ActorId(*peer.as_bytes())) {
            return Err(ceremony_not_expected(
                "no live expectation or ceremony names this actor",
            ));
        }
        Ok(state)
    }

    /// `fauna.peer.share.ceremony.{offer,deliver}` — one pushed frame, handed
    /// verbatim to the seam under the channel-proven sender. One handler for
    /// both kinds: the frame self-describes and the state machine owns every
    /// content/order verification.
    async fn handle_ceremony_frame(
        &self,
        peer: &EndpointKey,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let state = self.ceremony_admitted(peer)?;
        let req: PeerShareCeremonyFrameRequest = from_value(&payload)?;
        state
            .ingest_frame(&ActorId(*peer.as_bytes()), &req.frame)
            .await
            .map_err(refusal_to_rpc)?;
        to_value(&PeerShareCeremonyFrameAck::default())
    }

    /// `fauna.peer.share.ceremony.accept.poll` — the owed accept frame, or
    /// `None` while the recipient's user is deciding.
    async fn handle_ceremony_accept_poll(
        &self,
        peer: &EndpointKey,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let state = self.ceremony_admitted(peer)?;
        let req: PeerShareCeremonyAcceptPollRequest = from_value(&payload)?;
        let scope: [u8; 32] = req
            .scope_id
            .as_ref()
            .try_into()
            .map_err(|_| unsupported("a scope id must be exactly 32 bytes"))?;
        let frame = state
            .accept_frame(&ActorId(*peer.as_bytes()), &scope)
            .await
            .map_err(refusal_to_rpc)?;
        to_value(&PeerShareCeremonyAcceptPollReply {
            frame: frame.map(ByteBuf::from),
            extra: Default::default(),
        })
    }

    /// Meter one request, then read the connection's verdict and check it
    /// admits the **named set**. Every data handler's preflight: the set is
    /// named per request precisely so one admitted connection cannot reach a
    /// set outside its verdict.
    fn admitted_for_set(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmissionVerdict>>,
        set: &ByteBuf,
    ) -> Result<[u8; 32], RpcError> {
        let now = (self.config.now)();
        let verdict = admitted_verdict(
            peer,
            slot,
            &self.ledger,
            now,
            over_quota,
            || not_admitted("no admission verdict on this connection"),
            || not_admitted("the admission verdict has expired — re-present the claim"),
        )?;
        let set: [u8; 32] = set
            .as_ref()
            .try_into()
            .map_err(|_| unsupported("a set id must be exactly 32 bytes"))?;
        // The verdict is held for the channel-proven actor; check the set
        // through the share door (never `admits_scope`, whose account-equality
        // semantics are the account plane's — see `verdict_admits_set`).
        if !verdict_admits_set(&verdict, peer.as_bytes(), &set) {
            return Err(not_admitted("the verdict does not admit this set"));
        }
        // The LIVE roster re-consult — finding: the verdict is a
        // cache, and nothing else ever closes an open connection (both
        // witness families mint `expires_at: None` by design; the slot's only
        // write is the admit merge; the party who would re-present is the one
        // being evicted). The twin already carries this line —
        // `fauna_peer_sync::server::admitted_connection` re-consults its
        // served registry so "a custody dropped from the registry severs
        // here, at the next request, even on a live connection" — and this
        // plane is the cross-user one, where eviction means a user stopped
        // sharing a set with a person. A future group-family data arm
        // inherits the same duty against `GroupRosterState::is_entry_removed`
        // (its seam doc says so); today every data arm is M2/folder-family
        // and this consult is the whole answer.
        if !self.membership.is_member(&set, &ActorId(*peer.as_bytes())) {
            return Err(not_admitted(
                "the roster no longer holds this membership for the named set",
            ));
        }
        Ok(set)
    }

    async fn handle_admit(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmissionVerdict>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let now = (self.config.now)();
        // Rule 8: the admission exchange itself is metered — a refused claim
        // spends budget exactly like an admitted one (the pre-auth DoS bound).
        if !self.ledger.try_request(peer.as_bytes(), now) {
            return Err(over_quota());
        }
        let req: PeerShareAdmitRequest = from_value(&payload)?;
        // One by-name dispatch for every witness kind — a kind this build does
        // not implement is refused by name, never guessed at from shape. The
        // group kind is a carried CERTIFICATE, not a claim, so it takes its
        // own arm rather than the claim-list signature.
        let (verdict, admitted) = if req.witness_kind == WITNESS_GROUP_MEMBERSHIP {
            self.evaluate_group_witnesses(&req.group_witnesses, peer)?
        } else {
            evaluate_share_witness(
                &req.witness_kind,
                &req.claimed_sets,
                self.membership.as_ref(),
                peer.as_bytes(),
            )
            .map_err(|e| witness_refused(&e.to_string()))?
        };
        // MERGE into the slot — one connection may hold both families (an M2
        // admit then a group admit widens the verdict, never clobbers it).
        {
            let mut held = slot.lock().unwrap();
            let merged = merge_verdicts(held.take(), verdict);
            *held = Some(merged);
        }
        let reply = PeerShareAdmitReply {
            admitted_sets: admitted.iter().map(|s| ByteBuf::from(s.to_vec())).collect(),
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: self
                .own_claimed_sets
                .read()
                .unwrap()
                .iter()
                .map(|s| ByteBuf::from(s.to_vec()))
                .collect(),
            extra: Default::default(),
        };
        to_value(&reply)
    }

    /// The group-membership arm of the admit exchange: verify each carried
    /// certificate through [`verdict_for_group_membership`] (PT-1b key
    /// binding, evaluator-rooted chain, frontier supersession — the verifier
    /// owns the order) and admit the verifying subset, degrading like the M2
    /// arm. Fail-closed when no [`GroupRosterState`] is wired, and refused
    /// outright when nothing verifies — an all-refused exchange must not
    /// read as an empty admission.
    fn evaluate_group_witnesses(
        &self,
        witnesses: &[fauna_protocol::peer_share::PeerShareGroupWitness],
        peer: &EndpointKey,
    ) -> Result<(AdmissionVerdict, Vec<[u8; 32]>), RpcError> {
        let Some(state) = self.group_roster.read().unwrap().clone() else {
            return Err(witness_refused(
                "this node holds no group roster state to evaluate against",
            ));
        };
        let mut scopes: Vec<String> = Vec::new();
        let mut admitted: Vec<[u8; 32]> = Vec::new();
        let mut last_refusal: Option<String> = None;
        for witness in witnesses {
            let scope: [u8; 32] = witness
                .scope_id
                .as_ref()
                .try_into()
                .map_err(|_| unsupported("a group scope id must be exactly 32 bytes"))?;
            match verdict_for_group_membership(
                &witness.entry,
                &scope,
                state.as_ref(),
                peer.as_bytes(),
            ) {
                Ok((verdict, _entry_id)) => {
                    if let AdmittedScopes::Named(named) = verdict.scopes {
                        for s in named {
                            if !scopes.contains(&s) {
                                scopes.push(s);
                            }
                        }
                    }
                    if !admitted.contains(&scope) {
                        admitted.push(scope);
                    }
                }
                Err(e) => last_refusal = Some(e.to_string()),
            }
        }
        if admitted.is_empty() {
            return Err(witness_refused(
                &last_refusal.unwrap_or_else(|| "no group witness was carried".into()),
            ));
        }
        Ok((
            AdmissionVerdict {
                account: *peer.as_bytes(),
                scopes: AdmittedScopes::Named(scopes),
                expires_at: None,
            },
            admitted,
        ))
    }

    async fn handle_changes_list(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmissionVerdict>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let req: PeerShareChangesListRequest = from_value(&payload)?;
        let set = self.admitted_for_set(peer, slot, &req.set)?;
        // Ask for one more than the page bound: if it comes back, there is
        // genuinely more, and `more` is a fact rather than a guess.
        let rows = self
            .store
            .changes_since(&set, req.since, MAX_ROWS_PER_PAGE + 1)
            .await
            .map_err(internal)?;
        let more = rows.len() as u32 > MAX_ROWS_PER_PAGE;
        let changes = rows
            .into_iter()
            .take(MAX_ROWS_PER_PAGE as usize)
            // The provenance ruling, applied as crate policy: own rows, and
            // relayed rows only where the writer's signature travels with them.
            .filter(|row| serves_held_row(row, &self.own_actor_hex))
            .map(|row| PeerShareChange {
                change: row.change,
                sequenced: row.sequenced,
                signer_cert: row.signer_cert,
                extra: Default::default(),
            })
            .collect();
        to_value(&PeerShareChangesListReply {
            changes,
            more,
            extra: Default::default(),
        })
    }

    async fn handle_manifests_get(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmissionVerdict>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let req: PeerShareManifestsGetRequest = from_value(&payload)?;
        let set = self.admitted_for_set(peer, slot, &req.set)?;
        let mut reply = PeerShareManifestsGetReply::default();
        let mut budget = MAX_BODY_BYTES_PER_REPLY;
        for wanted in &req.manifest_hashes {
            let Ok(hash) = <[u8; 32]>::try_from(wanted.as_ref()) else {
                return Err(unsupported("a manifest hash must be exactly 32 bytes"));
            };
            match self
                .store
                .manifest_bytes(&set, &hash)
                .await
                .map_err(internal)?
            {
                None => reply.missing.push(wanted.clone()),
                Some(bytes) => {
                    if bytes.len() > budget {
                        // Held, but over this reply's budget. Distinct from
                        // `missing` so a puller re-asks instead of concluding
                        // no member holds it.
                        reply.deferred.push(wanted.clone());
                        continue;
                    }
                    budget -= bytes.len();
                    reply.manifests.push(PeerShareManifest {
                        hash: wanted.clone(),
                        bytes: ByteBuf::from(bytes),
                        extra: Default::default(),
                    });
                }
            }
        }
        to_value(&reply)
    }

    async fn handle_chunks_pull(
        &self,
        peer: &EndpointKey,
        slot: &Mutex<Option<AdmissionVerdict>>,
        payload: Value,
    ) -> Result<Value, RpcError> {
        let req: PeerShareChunksPullRequest = from_value(&payload)?;
        let set = self.admitted_for_set(peer, slot, &req.set)?;
        let mut reply = PeerShareChunksPullReply::default();
        let mut budget = MAX_BODY_BYTES_PER_REPLY;
        for want in &req.wants {
            let Ok(key) = <[u8; 32]>::try_from(want.store_key.as_ref()) else {
                return Err(unsupported("a store key must be exactly 32 bytes"));
            };
            if budget == 0 {
                // No room left this reply for ANY want from here on — never
                // pay `chunk_body`'s read+reseal just to discard the bytes.
                // The cheap existence check still tells `missing` (never
                // arriving) from `deferred` (will, once there is room); the
                // one thing it cannot do is validate `want.offset` without
                // the body's `total_len`, so a bad offset on a want that
                // lands here is deferred rather than refused immediately —
                // it re-validates, and errors then, once budget allows the
                // real fetch.
                if self
                    .store
                    .chunk_exists(&set, &key)
                    .await
                    .map_err(internal)?
                {
                    reply.deferred.push(want.store_key.clone());
                } else {
                    reply.missing.push(want.store_key.clone());
                }
                continue;
            }
            match self.store.chunk_body(&set, &key).await.map_err(internal)? {
                None => reply.missing.push(want.store_key.clone()),
                Some(bytes) => {
                    let total_len = bytes.len() as u64;
                    // budget > 0 here — a want reaching this arm already
                    // passed the loop-top zero-budget check above.
                    // A chunk body can be 8 MiB against a 1 MiB frame, so the
                    // reply carries a SLICE bounded by what is left of this
                    // reply's budget. The puller re-asks from the new offset,
                    // and every slice of an incomplete body is non-empty
                    // (budget > 0 and offset < total_len), so a pull always
                    // advances. An `offset == total_len` want yields an empty
                    // slice, which the puller reads as complete — the same
                    // arithmetic that ends an ordinary body.
                    let Some(slice) =
                        fauna_peer_sync::ranged::slice_for(&bytes, want.offset, budget)
                    else {
                        // Loud, not an empty slice: an empty slice would read as
                        // "converged" to a puller assembling the body.
                        return Err(unsupported(
                            "the want's offset is past the end of the stored body",
                        ));
                    };
                    budget -= slice.len();
                    reply.chunks.push(PeerShareChunk {
                        store_key: want.store_key.clone(),
                        offset: want.offset,
                        bytes: ByteBuf::from(slice.to_vec()),
                        total_len,
                        extra: Default::default(),
                    });
                }
            }
        }
        to_value(&reply)
    }
}

/// The plane-scoped wrapper over the shared round-trip: only the encode-failure
/// error is this plane's (`fauna.peer.share.internal`).
fn to_value<T: serde::Serialize>(reply: &T) -> Result<Value, RpcError> {
    fauna_peer_channel::to_value(reply)
        .ok_or_else(|| internal(anyhow::anyhow!("encoding the reply")))
}

fn not_admitted(detail: &str) -> RpcError {
    RpcError::new(ERR_NOT_ADMITTED, "error.peer_share.not_admitted").with_details_text(detail)
}

fn over_quota() -> RpcError {
    RpcError::new(ERR_OVER_QUOTA, "error.peer_share.over_quota")
}

fn unsupported(detail: &str) -> RpcError {
    RpcError::new(ERR_UNSUPPORTED, "error.peer_share.unsupported").with_details_text(detail)
}

fn witness_refused(detail: &str) -> RpcError {
    RpcError::new(ERR_WITNESS_REFUSED, "error.peer_share.witness_refused").with_details_text(detail)
}

/// Merge a fresh admission verdict into a connection's held one: same channel
/// identity, union of `Named` scopes, the sooner of any expiry bounds (the
/// conservative bound — a narrower verdict is re-presentable, a wider one is a
/// hole). Both share arms produce `Named`/`None` today; a non-`Named` form is
/// replaced outright (defensive — widening by merge is exactly what this
/// function must never do).
fn merge_verdicts(held: Option<AdmissionVerdict>, fresh: AdmissionVerdict) -> AdmissionVerdict {
    let Some(held) = held else { return fresh };
    if held.account != fresh.account {
        return fresh;
    }
    match (held.scopes, fresh.scopes) {
        (AdmittedScopes::Named(mut union), AdmittedScopes::Named(new)) => {
            for scope in new {
                if !union.contains(&scope) {
                    union.push(scope);
                }
            }
            AdmissionVerdict {
                account: fresh.account,
                scopes: AdmittedScopes::Named(union),
                expires_at: match (held.expires_at, fresh.expires_at) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                },
            }
        }
        (_, scopes) => AdmissionVerdict { scopes, ..fresh },
    }
}

fn ceremony_not_expected(detail: &str) -> RpcError {
    RpcError::new(
        ERR_CEREMONY_NOT_EXPECTED,
        "error.peer_share.ceremony_not_expected",
    )
    .with_details_text(detail)
}

fn refusal_to_rpc(refusal: CeremonyRefusal) -> RpcError {
    match refusal {
        CeremonyRefusal::NotExpected(detail) => ceremony_not_expected(&detail),
        CeremonyRefusal::Refused(detail) => {
            RpcError::new(ERR_CEREMONY_REFUSED, "error.peer_share.ceremony_refused")
                .with_details_text(&detail)
        }
    }
}

fn internal(e: anyhow::Error) -> RpcError {
    RpcError::new("fauna.peer.share.internal", "error.peer_share.internal")
        .with_details_text(e.to_string())
}

// The serve side driven directly at its handlers: the verdict slot is
// per-connection state a wire test cannot reach without a live transport, and
// these are the refusals that matter most (a scope escape, a relayed row, a
// quota bypass). The wire-reachable end-to-end path is slice E's two-seat tier_3
// proof — this suite is what makes that proof about *convergence* rather than
// about whether the door works at all.
impl MeteredPlane for ShareServer {
    const PLANE: &'static str = "peer-share";

    fn now_secs(&self) -> u64 {
        (self.config.now)()
    }

    fn ledger(&self) -> &QuotaLedger {
        &self.ledger
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorId;
    // Production code round-trips through the shared helpers; these fixtures
    // build raw wire values by hand, so they still reach for the codec directly.
    use fauna_protocol::peer_share::PeerShareChunkWant;
    use fauna_protocol::sync::SyncChange;
    use fauna_protocol::{decode_strict, encode_canonical};
    use std::collections::{HashMap, HashSet};

    const US: [u8; 32] = [0xA1; 32];
    const SPOUSE: [u8; 32] = [0xB2; 32];
    const STRANGER: [u8; 32] = [0xB3; 32];
    const SET_A: [u8; 32] = [0x4F; 32];
    const SET_B: [u8; 32] = [0x50; 32];

    struct FakeRoster(HashSet<([u8; 32], [u8; 32])>);

    impl SetMembership for FakeRoster {
        fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
            self.0.contains(&(*channel_id, actor.0))
        }
    }

    #[derive(Default)]
    struct FakeStore {
        rows: Vec<LocalShareChange>,
        manifests: HashMap<[u8; 32], Vec<u8>>,
        chunks: HashMap<[u8; 32], Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl ShareStore for FakeStore {
        async fn changes_since(
            &self,
            _set: &[u8; 32],
            since: i64,
            max_rows: u32,
        ) -> anyhow::Result<Vec<LocalShareChange>> {
            Ok(self
                .rows
                .iter()
                .filter(|r| r.change.seq > since)
                .take(max_rows as usize)
                .cloned()
                .collect())
        }

        async fn manifest_bytes(
            &self,
            _set: &[u8; 32],
            manifest_hash: &[u8; 32],
        ) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self.manifests.get(manifest_hash).cloned())
        }

        async fn chunk_body(
            &self,
            _set: &[u8; 32],
            store_key: &[u8; 32],
        ) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self.chunks.get(store_key).cloned())
        }
    }

    fn row(seq: i64, author: Option<[u8; 32]>, sequenced: bool) -> LocalShareChange {
        LocalShareChange {
            change: SyncChange {
                seq,
                path_hash: "cc".repeat(32),
                manifest_hash: Some("dd".repeat(32)),
                size_bytes: 10,
                change_type: "create".to_string(),
                created_at: 1_760_000_000,
                author_actor_id: author.map(hex::encode),
                ..Default::default()
            },
            sequenced,
            locally_authored: author == Some(US) || author.is_none(),
            signer_cert: None,
        }
    }

    fn server(store: FakeStore) -> Arc<ShareServer> {
        let roster = FakeRoster(HashSet::from([(SET_A, SPOUSE)]));
        Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name: "test".into(),
                own_actor: US,
                quotas: QuotaConfig::default(),
                now: Arc::new(|| 1_000),
            },
            Arc::new(roster),
            Arc::new(store),
        ))
    }

    fn value_of<T: serde::Serialize>(v: &T) -> Value {
        decode_strict(&encode_canonical(v).expect("encode")).expect("as Value")
    }

    fn reply_as<T: serde::de::DeserializeOwned>(v: Value) -> T {
        decode_strict(&encode_canonical(&v).expect("encode")).expect("decode reply")
    }

    fn slot() -> Mutex<Option<AdmissionVerdict>> {
        Mutex::new(None)
    }

    /// Admit `peer` for its claimed sets, returning the filled slot.
    async fn admitted_slot(
        server: &Arc<ShareServer>,
        peer: &EndpointKey,
        claimed: &[[u8; 32]],
    ) -> Mutex<Option<AdmissionVerdict>> {
        let s = slot();
        let req = PeerShareAdmitRequest {
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: claimed.iter().map(|c| ByteBuf::from(c.to_vec())).collect(),
            group_witnesses: Vec::new(),
            extra: Default::default(),
        };
        server
            .handle_admit(peer, &s, value_of(&req))
            .await
            .expect("the roster admits this peer");
        s
    }

    /// Rule 3, asserted as the whole SET rather than a sample: the dispatcher
    /// serves the probe, the admit exchange, the three data kinds and the
    /// three ceremony kinds — and nothing whose name could carry config,
    /// capability-mint, admin or key material.
    #[test]
    fn the_allowlist_is_exactly_the_probe_the_admit_the_data_and_the_ceremony_kinds() {
        let kinds = allowlisted_kinds();
        assert_eq!(kinds.len(), 8);
        assert_eq!(kinds[0], fauna_protocol::peer::KIND_PEER_NODE_INFO);
        let share: Vec<&str> = kinds[1..].to_vec();
        assert_eq!(
            share,
            vec![
                KIND_PEER_SHARE_ADMIT,
                KIND_PEER_SHARE_CHANGES_LIST,
                KIND_PEER_SHARE_MANIFESTS_GET,
                KIND_PEER_SHARE_CHUNKS_PULL,
                KIND_PEER_SHARE_CEREMONY_OFFER,
                KIND_PEER_SHARE_CEREMONY_ACCEPT_POLL,
                KIND_PEER_SHARE_CEREMONY_DELIVER,
            ]
        );
        for kind in share {
            assert!(kind.starts_with("fauna.peer.share."), "{kind}");
        }
    }

    #[tokio::test]
    async fn a_data_request_on_an_unadmitted_connection_is_refused() {
        let server = server(FakeStore::default());
        let peer = EndpointKey::from_bytes(SPOUSE);
        let err = server
            .handle_changes_list(
                &peer,
                &slot(),
                value_of(&PeerShareChangesListRequest {
                    set: ByteBuf::from(SET_A.to_vec()),
                    since: 0,
                    extra: Default::default(),
                }),
            )
            .await
            .expect_err("no verdict on this connection");
        assert_eq!(err.code, ERR_NOT_ADMITTED);
    }

    #[tokio::test]
    async fn a_stranger_the_roster_does_not_hold_is_refused_at_admit() {
        let server = server(FakeStore::default());
        let peer = EndpointKey::from_bytes(STRANGER);
        let req = PeerShareAdmitRequest {
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: vec![ByteBuf::from(SET_A.to_vec())],
            group_witnesses: Vec::new(),
            extra: Default::default(),
        };
        let err = server
            .handle_admit(&peer, &slot(), value_of(&req))
            .await
            .expect_err("the roster holds no membership for a stranger");
        assert_eq!(err.code, ERR_WITNESS_REFUSED);
    }

    /// The admit reply reports what was admitted, and the mutual half carries
    /// this side's own claimed sets — one round trip, two independent verdicts.
    #[tokio::test]
    async fn the_admit_reply_reports_the_admitted_subset_and_carries_our_own_claim() {
        let server = server(FakeStore::default());
        server.set_own_claimed_sets(vec![SET_A]);
        let peer = EndpointKey::from_bytes(SPOUSE);
        let req = PeerShareAdmitRequest {
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            // SET_B is claimed but not on the roster: it degrades away rather
            // than failing the whole exchange.
            claimed_sets: vec![ByteBuf::from(SET_A.to_vec()), ByteBuf::from(SET_B.to_vec())],
            group_witnesses: Vec::new(),
            extra: Default::default(),
        };
        let reply: PeerShareAdmitReply = reply_as(
            server
                .handle_admit(&peer, &slot(), value_of(&req))
                .await
                .expect("admits SET_A"),
        );
        assert_eq!(reply.admitted_sets, vec![ByteBuf::from(SET_A.to_vec())]);
        assert_eq!(reply.witness_kind, WITNESS_M2_MEMBERSHIP);
        assert_eq!(reply.claimed_sets, vec![ByteBuf::from(SET_A.to_vec())]);
    }

    /// The scope check is **per request**, not per connection: an admitted
    /// connection reaching for a set outside its verdict is refused. This is the
    /// escape a single connection-level check would allow.
    #[tokio::test]
    async fn an_admitted_connection_cannot_reach_a_set_outside_its_verdict() {
        let server = server(FakeStore::default());
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;
        let err = server
            .handle_chunks_pull(
                &peer,
                &s,
                value_of(&PeerShareChunksPullRequest {
                    set: ByteBuf::from(SET_B.to_vec()),
                    wants: vec![],
                    extra: Default::default(),
                }),
            )
            .await
            .expect_err("SET_B was never admitted");
        assert_eq!(err.code, ERR_NOT_ADMITTED);
    }

    /// Finding: **eviction must reach an OPEN connection.** The
    /// verdict slot is a cache — the roster is the live truth, re-consulted
    /// on every data request exactly as the peer-sync twin re-consults its
    /// served registry ("a custody dropped from the registry severs here, at
    /// the next request, even on a live connection"). Nothing else can close
    /// this: both witness families mint `expires_at: None` by design, the
    /// slot's only write is the admit merge, and the party who would
    /// re-present is the one being evicted.
    #[tokio::test]
    async fn evicting_a_member_severs_an_already_admitted_connection() {
        struct LiveRoster(std::sync::RwLock<HashSet<([u8; 32], [u8; 32])>>);
        impl SetMembership for LiveRoster {
            fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
                self.0.read().unwrap().contains(&(*channel_id, actor.0))
            }
        }
        let roster = Arc::new(LiveRoster(std::sync::RwLock::new(HashSet::from([(
            SET_A, SPOUSE,
        )]))));
        let server = Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name: "test".into(),
                own_actor: US,
                quotas: QuotaConfig::default(),
                now: Arc::new(|| 1_000),
            },
            Arc::clone(&roster) as Arc<dyn SetMembership + Send + Sync>,
            Arc::new(FakeStore::default()),
        ));
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;

        let req = PeerShareChangesListRequest {
            set: ByteBuf::from(SET_A.to_vec()),
            since: 0,
            extra: Default::default(),
        };
        server
            .handle_changes_list(&peer, &s, value_of(&req))
            .await
            .expect("an admitted member reads the set before the eviction");

        // The eviction: roster truth changes; the connection and its verdict
        // slot are deliberately untouched.
        roster.0.write().unwrap().remove(&(SET_A, SPOUSE));

        let err = server
            .handle_changes_list(&peer, &s, value_of(&req))
            .await
            .expect_err("the evicted member's very next request refuses on the live roster");
        assert_eq!(err.code, ERR_NOT_ADMITTED);
    }

    #[tokio::test]
    async fn a_malformed_set_id_is_refused_as_unsupported() {
        let server = server(FakeStore::default());
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;
        let err = server
            .handle_changes_list(
                &peer,
                &s,
                value_of(&PeerShareChangesListRequest {
                    set: ByteBuf::from(vec![0x4F; 31]),
                    since: 0,
                    extra: Default::default(),
                }),
            )
            .await
            .expect_err("31 bytes is not a set id");
        assert_eq!(err.code, ERR_UNSUPPORTED);
    }

    /// The provenance ruling at the serve boundary, lifted: of the rows the
    /// store holds, this replica's own leave the door, and so does another
    /// writer's SIGNED row (with its inline cert) — never an unsigned row of
    /// someone else's.
    #[tokio::test]
    async fn own_rows_and_signed_relays_leave_the_change_door() {
        let writer = fauna_core::identity::ActorKeypair::from_secret([0x33; 32]);
        let mut relayed = row(4, Some(writer.actor_id().0), true);
        relayed.change.device_id = Some("ee".repeat(32));
        fauna_protocol::sync_writer_sig::ChangeSigner::direct(&writer)
            .sign_row(&mut relayed.change, [7; 32])
            .expect("sign");
        let cert = fauna_core::encoding::EmbedAsBytes {
            envelope: vec![1; 100],
            bytes: vec![2; 8],
            signer_auth: None,
        };
        relayed.signer_cert = Some(cert.clone());
        let store = FakeStore {
            rows: vec![
                row(1, Some(US), true),
                row(2, Some(SPOUSE), true),
                row(3, None, false),
                relayed,
            ],
            ..Default::default()
        };
        let server = server(store);
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;
        let reply: PeerShareChangesListReply = reply_as(
            server
                .handle_changes_list(
                    &peer,
                    &s,
                    value_of(&PeerShareChangesListRequest {
                        set: ByteBuf::from(SET_A.to_vec()),
                        since: 0,
                        extra: Default::default(),
                    }),
                )
                .await
                .expect("serves"),
        );
        let seqs: Vec<i64> = reply.changes.iter().map(|c| c.change.seq).collect();
        assert_eq!(
            seqs,
            vec![1, 3, 4],
            "the spouse's unsigned row (seq 2) is never relayed; the signed one is"
        );
        assert!(reply.changes[0].sequenced);
        assert!(
            !reply.changes[1].sequenced,
            "the own-pending row keeps its marking on the wire"
        );
        assert_eq!(
            reply.changes[2].signer_cert,
            Some(cert),
            "a relayed row carries its cert inline — self-contained"
        );
        assert!(reply.changes[2].change.signature.is_some());
        assert!(!reply.more);
    }

    /// `more` is a fact, not a guess: the handler asks the store for one row
    /// beyond the page bound to learn it.
    #[tokio::test]
    async fn a_full_page_reports_more() {
        let store = FakeStore {
            rows: (1..=(MAX_ROWS_PER_PAGE as i64 + 5))
                .map(|seq| row(seq, Some(US), true))
                .collect(),
            ..Default::default()
        };
        let server = server(store);
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;
        let reply: PeerShareChangesListReply = reply_as(
            server
                .handle_changes_list(
                    &peer,
                    &s,
                    value_of(&PeerShareChangesListRequest {
                        set: ByteBuf::from(SET_A.to_vec()),
                        since: 0,
                        extra: Default::default(),
                    }),
                )
                .await
                .expect("serves"),
        );
        assert_eq!(reply.changes.len(), MAX_ROWS_PER_PAGE as usize);
        assert!(reply.more);
    }

    #[tokio::test]
    async fn the_byte_doors_split_served_missing_and_deferred() {
        let held = [0x11; 32];
        let absent = [0x22; 32];
        let oversized = [0x33; 32];
        let store = FakeStore {
            manifests: HashMap::from([
                (held, vec![0xAA; 64]),
                (oversized, vec![0xBB; MAX_BODY_BYTES_PER_REPLY + 1]),
            ]),
            chunks: HashMap::from([
                (held, vec![0xCC; 128]),
                (oversized, vec![0xDD; MAX_BODY_BYTES_PER_REPLY + 1]),
            ]),
            ..Default::default()
        };
        let server = server(store);
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;
        let want: Vec<ByteBuf> = [held, absent, oversized]
            .iter()
            .map(|k| ByteBuf::from(k.to_vec()))
            .collect();

        let manifests: PeerShareManifestsGetReply = reply_as(
            server
                .handle_manifests_get(
                    &peer,
                    &s,
                    value_of(&PeerShareManifestsGetRequest {
                        set: ByteBuf::from(SET_A.to_vec()),
                        manifest_hashes: want.clone(),
                        extra: Default::default(),
                    }),
                )
                .await
                .expect("serves"),
        );
        assert_eq!(manifests.manifests.len(), 1);
        assert_eq!(manifests.manifests[0].hash, want[0]);
        assert_eq!(manifests.missing, vec![want[1].clone()]);
        assert_eq!(
            manifests.deferred,
            vec![want[2].clone()],
            "over-budget is DEFERRED, never missing — a puller must re-ask, not \
             conclude no member holds it"
        );
    }

    /// The chunk door **slices**: a body over one reply's budget comes back as a
    /// bounded prefix with its true `total_len`, never as `deferred` (deferring
    /// it would make every ordinary multi-MB file untransferable — the defect
    /// this shape was built to fix). `deferred` is reserved for a want that got
    /// no room at all because earlier wants spent the budget, and `missing` for
    /// a body this peer cannot produce.
    #[tokio::test]
    async fn the_chunk_door_slices_an_oversized_body_and_defers_only_what_got_no_room() {
        let big = [0x33; 32];
        let small = [0x11; 32];
        let absent = [0x22; 32];
        let big_len = MAX_BODY_BYTES_PER_REPLY + 4096;
        let store = FakeStore {
            chunks: HashMap::from([(big, vec![0xDD; big_len]), (small, vec![0xCC; 128])]),
            ..Default::default()
        };
        let server = server(store);
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;

        async fn pull(
            server: &Arc<ShareServer>,
            slot: &Mutex<Option<AdmissionVerdict>>,
            wants: Vec<PeerShareChunkWant>,
        ) -> PeerShareChunksPullReply {
            reply_as(
                server
                    .handle_chunks_pull(
                        &EndpointKey::from_bytes(SPOUSE),
                        slot,
                        value_of(&PeerShareChunksPullRequest {
                            set: ByteBuf::from(SET_A.to_vec()),
                            wants,
                            extra: Default::default(),
                        }),
                    )
                    .await
                    .expect("serves"),
            )
        }

        // The big body first: it takes the whole budget as a prefix, so the
        // small one behind it gets no room and is deferred.
        let reply = pull(
            &server,
            &s,
            vec![
                PeerShareChunkWant {
                    store_key: ByteBuf::from(big.to_vec()),
                    offset: 0,
                    extra: Default::default(),
                },
                PeerShareChunkWant {
                    store_key: ByteBuf::from(absent.to_vec()),
                    offset: 0,
                    extra: Default::default(),
                },
                PeerShareChunkWant {
                    store_key: ByteBuf::from(small.to_vec()),
                    offset: 0,
                    extra: Default::default(),
                },
            ],
        )
        .await;
        assert_eq!(reply.chunks.len(), 1);
        assert_eq!(reply.chunks[0].offset, 0);
        assert_eq!(
            reply.chunks[0].bytes.len(),
            MAX_BODY_BYTES_PER_REPLY,
            "a prefix bounded by the reply budget"
        );
        assert_eq!(
            reply.chunks[0].total_len, big_len as u64,
            "and the honest whole-body length, so the puller knows how far it has to go"
        );
        assert_eq!(reply.missing, vec![ByteBuf::from(absent.to_vec())]);
        assert_eq!(
            reply.deferred,
            vec![ByteBuf::from(small.to_vec())],
            "no room left this reply — re-ask, do not conclude it is gone"
        );

        // Resuming from the served offset returns exactly the remainder.
        let reply = pull(
            &server,
            &s,
            vec![PeerShareChunkWant {
                store_key: ByteBuf::from(big.to_vec()),
                offset: MAX_BODY_BYTES_PER_REPLY as u64,
                extra: Default::default(),
            }],
        )
        .await;
        assert_eq!(reply.chunks[0].offset, MAX_BODY_BYTES_PER_REPLY as u64);
        assert_eq!(reply.chunks[0].bytes.len(), 4096);
        assert_eq!(
            reply.chunks[0].offset as usize + reply.chunks[0].bytes.len(),
            big_len,
            "two rounds cover the body exactly — no gap, no overlap"
        );
    }

    /// An offset past the end is a protocol error, not an empty slice: an empty
    /// slice would read as "complete" to a puller assembling the body.
    #[tokio::test]
    async fn an_offset_past_the_end_of_the_body_is_refused() {
        let key = [0x11; 32];
        let store = FakeStore {
            chunks: HashMap::from([(key, vec![0xCC; 128])]),
            ..Default::default()
        };
        let server = server(store);
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;
        let err = server
            .handle_chunks_pull(
                &peer,
                &s,
                value_of(&PeerShareChunksPullRequest {
                    set: ByteBuf::from(SET_A.to_vec()),
                    wants: vec![PeerShareChunkWant {
                        store_key: ByteBuf::from(key.to_vec()),
                        offset: 129,
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                }),
            )
            .await
            .expect_err("an offset past the end is a protocol error");
        assert_eq!(err.code, ERR_UNSUPPORTED);
    }

    /// Rule 8, including its load-bearing clause: an **admission-refused**
    /// attempt spends the same budget as an admitted one, so a hostile peer
    /// cannot probe for free.
    #[tokio::test]
    async fn the_request_quota_meters_refused_attempts_too() {
        let roster = FakeRoster(HashSet::from([(SET_A, SPOUSE)]));
        let server = Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name: "test".into(),
                own_actor: US,
                quotas: QuotaConfig {
                    requests_per_window: 2,
                    ..Default::default()
                },
                now: Arc::new(|| 1_000),
            },
            Arc::new(roster),
            Arc::new(FakeStore::default()),
        ));
        let peer = EndpointKey::from_bytes(STRANGER);
        let refused = PeerShareAdmitRequest {
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: vec![ByteBuf::from(SET_A.to_vec())],
            group_witnesses: Vec::new(),
            extra: Default::default(),
        };
        for _ in 0..2 {
            let err = server
                .handle_admit(&peer, &slot(), value_of(&refused))
                .await
                .expect_err("the roster refuses a stranger");
            assert_eq!(err.code, ERR_WITNESS_REFUSED, "budget still spent");
        }
        let err = server
            .handle_admit(&peer, &slot(), value_of(&refused))
            .await
            .expect_err("the window's budget is gone");
        assert_eq!(err.code, ERR_OVER_QUOTA);
    }

    /// A store error surfaces as an internal error, never as an empty page — an
    /// empty page would read as "converged" to a puller (the same reasoning
    /// `ERR_UNSUPPORTED`'s doc records).
    #[tokio::test]
    async fn a_store_failure_is_never_an_empty_page() {
        struct Failing;
        #[async_trait::async_trait]
        impl ShareStore for Failing {
            async fn changes_since(
                &self,
                _set: &[u8; 32],
                _since: i64,
                _max_rows: u32,
            ) -> anyhow::Result<Vec<LocalShareChange>> {
                anyhow::bail!("the store is unavailable")
            }
            async fn manifest_bytes(
                &self,
                _set: &[u8; 32],
                _h: &[u8; 32],
            ) -> anyhow::Result<Option<Vec<u8>>> {
                Ok(None)
            }
            async fn chunk_body(
                &self,
                _set: &[u8; 32],
                _k: &[u8; 32],
            ) -> anyhow::Result<Option<Vec<u8>>> {
                Ok(None)
            }
        }
        let roster = FakeRoster(HashSet::from([(SET_A, SPOUSE)]));
        let server = Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name: "test".into(),
                own_actor: US,
                quotas: QuotaConfig::default(),
                now: Arc::new(|| 1_000),
            },
            Arc::new(roster),
            Arc::new(Failing),
        ));
        let peer = EndpointKey::from_bytes(SPOUSE);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;
        let err = server
            .handle_changes_list(
                &peer,
                &s,
                value_of(&PeerShareChangesListRequest {
                    set: ByteBuf::from(SET_A.to_vec()),
                    since: 0,
                    extra: Default::default(),
                }),
            )
            .await
            .expect_err("a store failure is loud");
        assert_eq!(err.code, "fauna.peer.share.internal");
    }

    // ── The group-membership arm of the admit exchange (
    // carriage-slot remainder) — the certificate kind, wired through the same
    // handler and the same per-connection verdict slot ────────────────────────

    use crate::admission::group_fixtures as gf;
    use crate::admission::verdict_admits_group;
    use fauna_protocol::peer_share::{PeerShareGroupWitness, WITNESS_GROUP_MEMBERSHIP};

    /// A server whose M2 roster vouches for the group MEMBER on SET_A (so the
    /// merge test can hold both families at once), with the group evaluator
    /// wired.
    fn group_server() -> Arc<ShareServer> {
        let member_key = gf::proven(&gf::member());
        let roster = FakeRoster(HashSet::from([(SET_A, member_key)]));
        let server = Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name: "test".into(),
                own_actor: US,
                quotas: QuotaConfig::default(),
                now: Arc::new(|| 1_000),
            },
            Arc::new(roster),
            Arc::new(FakeStore::default()),
        ));
        server.set_group_roster(Arc::new(gf::Evaluator::holding()));
        server
    }

    /// The share plane serves through the shared rule-8 door
    /// ([`metered_handler_factory`]) rather than a local copy of it: with a
    /// one-connection window, the same peer's second dial is refused outright.
    /// This is the plane's ONLY listener-facing door (rules 5 + 7), so the
    /// meter being on it is the whole connection-side bound.
    #[test]
    fn the_share_plane_meters_connections_through_the_shared_door() {
        let server = Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name: "meter-door".into(),
                own_actor: US,
                quotas: QuotaConfig {
                    conns_per_window: 1,
                    ..QuotaConfig::default()
                },
                now: Arc::new(|| 1_000),
            },
            Arc::new(FakeRoster(HashSet::new())),
            Arc::new(FakeStore::default()),
        ));

        let factory = server.handler_factory();
        let peer = fauna_transport::EndpointKey::from_bytes([0xEE; 32]);
        assert!(factory(peer, fauna_transport::PathKind::Lan).is_some());
        assert!(
            factory(peer, fauna_transport::PathKind::Lan).is_none(),
            "the second connection in-window is refused by the shared meter"
        );
    }

    fn group_admit_req(entry: Vec<u8>) -> PeerShareAdmitRequest {
        PeerShareAdmitRequest {
            witness_kind: WITNESS_GROUP_MEMBERSHIP.to_string(),
            claimed_sets: Vec::new(),
            group_witnesses: vec![PeerShareGroupWitness {
                scope_id: ByteBuf::from(gf::scope_id().to_vec()),
                entry: ByteBuf::from(entry),
                extra: Default::default(),
            }],
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_carried_group_witness_admits_its_scope_on_the_exchange() {
        let server = group_server();
        let member_key = gf::proven(&gf::member());
        let peer = EndpointKey::from_bytes(member_key);
        let s = slot();
        let reply: PeerShareAdmitReply = reply_as(
            server
                .handle_admit(
                    &peer,
                    &s,
                    value_of(&group_admit_req(gf::carried(
                        &gf::authority(),
                        &gf::member(),
                    ))),
                )
                .await
                .expect("the certificate admits"),
        );
        assert_eq!(
            reply.admitted_sets,
            vec![ByteBuf::from(gf::scope_id().to_vec())]
        );
        let verdict = s.lock().unwrap().clone().expect("slot filled");
        assert!(verdict_admits_group(&verdict, &member_key, &gf::scope_id()));
        // Family separation: a group verdict never opens the FOLDER door for
        // the same 32 bytes — the scope-string families are disjoint.
        assert!(!verdict_admits_set(&verdict, &member_key, &gf::scope_id()));
    }

    /// One connection, both families: an M2 admit then a group admit MERGE
    /// into the slot — the second exchange widens, never clobbers.
    #[tokio::test]
    async fn a_group_admit_merges_with_an_existing_m2_verdict() {
        let server = group_server();
        let member_key = gf::proven(&gf::member());
        let peer = EndpointKey::from_bytes(member_key);
        let s = admitted_slot(&server, &peer, &[SET_A]).await;
        server
            .handle_admit(
                &peer,
                &s,
                value_of(&group_admit_req(gf::carried(
                    &gf::authority(),
                    &gf::member(),
                ))),
            )
            .await
            .expect("the certificate admits");
        let verdict = s.lock().unwrap().clone().expect("slot filled");
        assert!(
            verdict_admits_set(&verdict, &member_key, &SET_A),
            "the earlier M2 admission survives the group exchange"
        );
        assert!(
            verdict_admits_group(&verdict, &member_key, &gf::scope_id()),
            "and the group admission is added"
        );
    }

    #[tokio::test]
    async fn a_group_admit_with_no_group_state_wired_fails_closed() {
        let server = server(FakeStore::default()); // no set_group_roster
        let member_key = gf::proven(&gf::member());
        let peer = EndpointKey::from_bytes(member_key);
        let err = server
            .handle_admit(
                &peer,
                &slot(),
                value_of(&group_admit_req(gf::carried(
                    &gf::authority(),
                    &gf::member(),
                ))),
            )
            .await
            .expect_err("no group state → refused, never admitted");
        assert_eq!(err.code, ERR_WITNESS_REFUSED);
    }

    /// PT-1b at the exchange: a valid certificate for somebody else, presented
    /// on this channel, admits nothing (the verifier's stolen-witness rule,
    /// re-asserted where the wire reaches it).
    #[tokio::test]
    async fn a_stolen_group_witness_is_refused_at_the_exchange() {
        let server = group_server();
        let member_key = gf::proven(&gf::member());
        let peer = EndpointKey::from_bytes(member_key);
        let err = server
            .handle_admit(
                &peer,
                &slot(),
                value_of(&group_admit_req(gf::carried(
                    &gf::authority(),
                    &gf::other_member(),
                ))),
            )
            .await
            .expect_err("the entry names a different actor");
        assert_eq!(err.code, ERR_WITNESS_REFUSED);
    }
}
