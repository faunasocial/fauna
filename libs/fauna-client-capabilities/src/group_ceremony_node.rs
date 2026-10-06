//! The contact-plane **ceremony node** — the offline share-initiation
//! ceremony's listener composition and its initiator drive
//! (`p2p.md` § Offline share initiation, contract point 1; the listener
//! shape is `p2p-shared-set-build.md` § *Build contract*, "The
//! listener is the contact-plane node, in the app process").
//!
//! This module is the **bind door** `fauna_peer_share::server` deliberately
//! ships without: it composes [`ShareServer`] + [`GroupCeremonyPeer`] onto a
//! [`PeerNode`], and it is the place wormability rule 7's version brake is
//! consulted. It sits here rather than in `fauna-peer-share` because the door
//! needs *both* halves and only this crate can see both — the carriage crate
//! carries no ceremony record, and it is this crate that depends on it, never the
//! other way round.
//!
//! # The brake is structural, not a caller's duty
//!
//! [`CeremonyNode::bind`] takes a [`CeremonyBindVerdict`], not a bare
//! transport, so a caller cannot reach the listener without having consulted
//! the `p2p-share` capability advertisement first. The posture is the peer
//! leg's, verbatim (`fauna_sync_engine::peer_leg` — `NoBrakeEvidence` /
//! `BrakeOn`): **no evidence at all refuses**, because a door that opens on
//! optimism is the brake-less listener rule 7 names. Callers pass the
//! last-known cached advertisement when the nest is unreachable — an offline
//! co-present ceremony is exactly the case the cache exists for.
//!
//! # Two endpoints, two identities
//!
//! The ceremony node is keyed by the **actor** key (PT-1b — the iroh NodeId
//! *is* the actor key, which is what makes the in-person code compare
//! meaningful), and is a different endpoint from the same-account peer-sync
//! listener, which is keyed by the machine's **device principal** (R5 (account-data-plane.md § The ratified decisions)). The
//! two never share a NodeId, a kind family, or a gate. The app supplies the
//! transport, already bound to the actor secret — this crate names no
//! concrete substrate.
//!
//! # Pacing lives here, not in each app
//!
//! [`GroupShareInitiator`] carries the whole initiator side: `begin` pushes
//! the offer, `poll_consent` asks once, `deliver` closes the ceremony. The
//! composed [`GroupShareInitiator::drive`] walks all three under a **named
//! generous budget with a deadline poll** (e2e convention 14 — the consent
//! gap is a human deciding, never a fixed sleep), so all seven apps get one
//! implementation of the pacing rather than seven.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_core::data::Timestamp;
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_generation::GroupReceptionKeyRecord;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_peer_channel::{PeerChannel, PeerNode};
use fauna_peer_share::server::{ShareServer, ShareServerConfig, ShareStore};
use fauna_peer_share::{
    CeremonyState, SetMembership, poll_ceremony_accept, send_ceremony_deliver, send_ceremony_offer,
};
use fauna_peer_sync::quota::QuotaConfig;
use fauna_transport::{EndpointKey, PathCandidates, PeerTransport};

use crate::group_ceremony::{
    AdmittedGroupShare, BegunGroupShare, BuiltGroupShareDeliver, GroupCeremonyError,
    GroupIngestOutcome, admit_group_share, begin_group_share, build_group_deliver,
    ingest_group_frame, mark_group_delivered, mark_group_offer_posted,
};
use crate::group_ceremony_peer::{GroupCeremonyPeer, NowFn, OnConfigChange};
use crate::group_ceremony_view::{CeremonyStatus, PeerCode, format_peer_code};

/// The `p2p-share` capability token, as advertised on `fauna.nest.info` — a
/// RE-EXPORT of the protocol constant, never a second literal.
///
/// The brake (`ceremony_bind_verdict`) and the feature-limits screen
/// (`fauna_client_features::view_model::capability_token`) must read ONE
/// string by construction, because they answer the same question — *does this
/// nest carry the plane* — and a drift between them is silent both ways: the
/// door would refuse to bind against a nest that does carry it, while the
/// screen went on showing the feature available.
///
/// ⚠ This was a local literal until 2026-08-18, justified as sparing the brake
/// "a protocol dep on the wasm-clean default build". That was already untrue
/// when written: this crate depends on `fauna-protocol` unconditionally
/// (`Cargo.toml`, `default-features = false` for wasm-cleanliness, with `js`
/// forwarded on wasm), and `rpc.rs` uses its wire types. There was no dep to
/// avoid — only two literals with nothing pinning them equal. Do not re-fork it.
pub use fauna_protocol::discovery::capability::P2P_SHARE as P2P_SHARE_CAPABILITY;

/// How long the initiator waits for the recipient's consent before giving up
/// — a named generous budget, not a timing assertion (convention 14). A
/// co-present ceremony's consent gap is one person reading a card and
/// tapping accept; five minutes is far above any non-pathological version of
/// that, and the ceremony record survives a give-up (the initiator simply
/// redials).
pub const CONSENT_BUDGET: Duration = Duration::from_secs(300);

/// How often the initiator asks, inside [`CONSENT_BUDGET`]. The wait is
/// deadline-bounded; this is only how finely it samples.
pub const CONSENT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long the initiator pauses before each attempt to dial the counterpart
/// again after the connection dropped. Paced well inside the serving side's
/// per-peer connection quota (`fauna_peer_sync::quota::QuotaConfig`, 32 per
/// minute by default), so a redial loop can never talk itself into a refusal.
pub const REDIAL_PAUSE: Duration = Duration::from_secs(2);

/// Opens a fresh channel to the same counterpart. [`GroupShareInitiator`]
/// calls it when the connection drops part-way through a ceremony. The app
/// builds it from the compare code the user already typed, so the ceremony
/// picks up again without anyone entering a code a second time.
pub type Redial = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<Arc<PeerChannel>>> + Send>,
        > + Send
        + Sync,
>;

/// Whether a carriage error means the connection is gone, as opposed to the
/// counterpart answering with a refusal.
///
/// Only a lost connection is worth dialing again. A refusal (a declined
/// invitation, or an admission the recipient no longer grants) is the other
/// side's answer, and asking again over a new connection would just repeat it
/// until the budget ran out.
fn is_connection_loss(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<fauna_peer_channel::ChannelError>(),
            Some(fauna_peer_channel::ChannelError::Dispatch(_))
        ) || matches!(
            cause.downcast_ref::<fauna_peer_channel::ChannelError>(),
            Some(fauna_peer_channel::ChannelError::Rpc(rpc))
                if rpc.code == fauna_protocol::DISCONNECTED_CODE
        )
    })
}

/// A carriage error as the drive reports it: a lost connection gets its own
/// variant so the drive knows it can dial again.
fn carriage_error(error: anyhow::Error) -> CeremonyDriveError {
    if is_connection_loss(&error) {
        CeremonyDriveError::ConnectionLost(format!("{error:#}"))
    } else {
        CeremonyDriveError::Transport(format!("{error:#}"))
    }
}

/// What the brake evidence says about binding the ceremony listener — the
/// peer leg's posture (`fauna_sync_engine::peer_leg::PeerLegPass`) applied to
/// the share plane's own token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeremonyBindVerdict {
    /// The best evidence advertises `p2p-share` — the door may open.
    Bind,
    /// The best evidence says this nest does not advertise it: the fleet
    /// brake is on.
    BrakeOn,
    /// No evidence at all — the nest was never reached and nothing is
    /// cached. Refuses by default, never by optimism.
    NoBrakeEvidence,
}

/// Read the brake from an advertisement — live, or the last-known cached one.
///
/// `capabilities` is `None` when there is no evidence at all (fresh install,
/// nest never reached, cache unreadable — a corrupt cache is *no* evidence,
/// never optimistic evidence).
pub fn ceremony_bind_verdict(capabilities: Option<&[String]>) -> CeremonyBindVerdict {
    match capabilities {
        None => CeremonyBindVerdict::NoBrakeEvidence,
        Some(caps) if caps.iter().any(|c| c == P2P_SHARE_CAPABILITY) => CeremonyBindVerdict::Bind,
        Some(_) => CeremonyBindVerdict::BrakeOn,
    }
}

/// Why the ceremony listener refused to bind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CeremonyBindRefusal {
    /// The nest does not advertise `p2p-share`.
    #[error("the nest does not advertise the p2p-share capability — the fleet brake is on")]
    BrakeOn,
    /// Nothing is known about the nest's advertisement.
    #[error("no p2p-share brake evidence — refusing to bind a listener on optimism")]
    NoBrakeEvidence,
}

/// A roster that vouches for nobody — the ceremony precedes any set
/// membership, so a [`CeremonyNode::bind`] (ceremony-only) node has no M2
/// admission to grant. The transfer half binds with the real roster consult
/// ([`CeremonyNode::bind_with_share_plane`]).
struct NoSharedSets;

impl SetMembership for NoSharedSets {
    fn is_member(&self, _channel_id: &[u8; 32], _actor: &ActorId) -> bool {
        false
    }
}

/// The bound contact-plane node — the ceremony listener AND (when bound with
/// a share plane) the transfer plane's serve side, on ONE actor-keyed
/// endpoint. Dropping it closes the listener and every inbound channel
/// (wormability rule 5, by construction — the [`PeerNode`] drop semantics).
pub struct CeremonyNode {
    peer: Arc<GroupCeremonyPeer>,
    transport: Arc<dyn PeerTransport>,
    own_actor: ActorId,
    server: Arc<ShareServer>,
    /// The per-set serve router; empty on a ceremony-only bind. Fed by
    /// [`Self::set_shared_sets`] together with the server's claimed-sets
    /// snapshot — one write, so the two cannot drift.
    router: Arc<fauna_peer_share::server::MultiSetShareStore>,
    /// Where this listener actually bound, as the assembler observed it —
    /// empty until [`Self::with_bound_addrs`] records it. Feeds the compare
    /// code's addressing half and nothing else.
    bound_addrs: Vec<SocketAddr>,
    /// Held for its Drop: the listener's lifetime.
    _node: PeerNode,
}

impl CeremonyNode {
    /// Bring the ceremony listener up — only on a [`CeremonyBindVerdict::Bind`].
    ///
    /// Ceremony-only: no set membership, no serve sources — the first-share
    /// door, honest before any set exists. The transfer half uses
    /// [`Self::bind_with_share_plane`]; both are ONE endpoint shape, so an
    /// app never binds a second actor-keyed node.
    ///
    /// `transport` must already be bound to this actor's own key (PT-1b: the
    /// NodeId *is* the actor key, which is what the in-person code compare
    /// compares). `config` is shared with the driver, which owns persistence;
    /// `on_config_change` fires after every mutation the serve side makes.
    pub async fn bind(
        verdict: CeremonyBindVerdict,
        transport: Arc<dyn PeerTransport>,
        own_actor: ActorId,
        display_name: String,
        config: Arc<Mutex<GroupShareConfig>>,
        now: NowFn,
        on_config_change: OnConfigChange,
    ) -> Result<Self, CeremonyBindRefusal> {
        Self::bind_with_share_plane(
            verdict,
            transport,
            own_actor,
            display_name,
            config,
            now,
            on_config_change,
            Arc::new(NoSharedSets),
        )
        .await
    }

    /// [`Self::bind`] with the transfer plane composed: the real M2 roster
    /// consult (`membership` — the set's own MLS roster, the evaluator's own
    /// store) and an initially-empty per-set serve router the pump feeds via
    /// [`Self::set_shared_sets`]. Rule 7 is identical — the verdict types the
    /// brake, no evidence refuses.
    #[allow(clippy::too_many_arguments)] // the bind door's own shape, one arg wider
    pub async fn bind_with_share_plane(
        verdict: CeremonyBindVerdict,
        transport: Arc<dyn PeerTransport>,
        own_actor: ActorId,
        display_name: String,
        config: Arc<Mutex<GroupShareConfig>>,
        now: NowFn,
        on_config_change: OnConfigChange,
        membership: Arc<dyn SetMembership + Send + Sync>,
    ) -> Result<Self, CeremonyBindRefusal> {
        match verdict {
            CeremonyBindVerdict::BrakeOn => return Err(CeremonyBindRefusal::BrakeOn),
            CeremonyBindVerdict::NoBrakeEvidence => {
                return Err(CeremonyBindRefusal::NoBrakeEvidence);
            }
            CeremonyBindVerdict::Bind => {}
        }
        let peer = Arc::new(GroupCeremonyPeer::new(
            own_actor,
            config,
            Arc::clone(&now),
            on_config_change,
        ));
        let router = Arc::new(fauna_peer_share::server::MultiSetShareStore::new());
        let server = Arc::new(ShareServer::new(
            ShareServerConfig {
                display_name,
                own_actor: own_actor.0,
                quotas: QuotaConfig::default(),
                now: Arc::new(move || now().0),
            },
            membership,
            Arc::clone(&router) as Arc<dyn ShareStore>,
        ));
        server.set_ceremony(Arc::clone(&peer) as Arc<dyn CeremonyState>);
        let node = PeerNode::start_with(Arc::clone(&transport), server.handler_factory()).await;
        Ok(Self {
            peer,
            transport,
            own_actor,
            server,
            router,
            bound_addrs: Vec::new(),
            _node: node,
        })
    }

    /// Feed the serve side one bound-set snapshot: the per-set sources into
    /// the router AND the claimed-sets list onto the server, in one call —
    /// the pump's write side, refreshed at its own cadence.
    pub fn set_shared_sets(
        &self,
        sources: std::collections::HashMap<[u8; 32], Arc<dyn ShareStore>>,
    ) {
        self.server
            .set_own_claimed_sets(sources.keys().copied().collect());
        self.router.set_sources(sources);
    }

    /// Wire the peer witness door's evaluator — what lets this seat's admit
    /// exchange verify a carried group-membership certificate
    /// (`ShareServer::set_group_roster`). Without it the group witness kind
    /// is refused. The seat's binder hands it the session's lent roster
    /// (`fauna_sync_engine::offline_share`), which the share plane's pump
    /// feeds from the evaluator's own store.
    pub fn set_group_roster(
        &self,
        state: Arc<dyn fauna_peer_share::admission::GroupRosterState + Send + Sync>,
    ) {
        self.server.set_group_roster(state);
    }

    /// Record where this seat's listener actually bound — the one fact only
    /// the **assembler** can observe.
    ///
    /// Deliberately not read off the [`PeerTransport`] seam: that trait has no
    /// `bound_addrs` on purpose (`fauna_sync_engine::peer_leg` module docs —
    /// "transport truth only the assembler can observe"), and the concrete
    /// constructor already hands it back —
    /// `fauna_iroh::peer_leg_transport` returns
    /// `(Arc<dyn PeerTransport>, Vec<SocketAddr>)`. An app that skips this
    /// call gets a bare-key code, which is honest: it does not know where it
    /// is reachable.
    pub fn with_bound_addrs(mut self, bound: Vec<SocketAddr>) -> Self {
        self.bound_addrs = bound;
        self
    }

    /// This device's compare code — the actor key **and this listener's LAN
    /// endpoints**, the text form the other side types in (QR stays a later
    /// per-app rendering of the same value).
    ///
    /// The addressing half is what makes the ceremony dialable with no nest
    /// in reach: there is no advertisement channel for a co-present pair, so
    /// the code IS the channel (`p2p.md` § Offline share initiation).
    pub fn own_code(&self) -> String {
        format_peer_code(&self.own_actor, &self.local_endpoints())
    }

    /// The actor this seat is bound as — what a typed counterpart code is
    /// parsed against (a code naming this very actor is refused as one's own).
    pub fn own_actor(&self) -> ActorId {
        self.own_actor
    }

    /// The dialable endpoints this seat's listener bound, crossed with the
    /// machine's interface addresses — what [`Self::own_code`] publishes.
    pub fn local_endpoints(&self) -> Vec<SocketAddr> {
        fauna_core::device_endpoints::lan_socket_addr_candidates(
            &self.bound_addrs,
            &fauna_peer_sync::discovery::discover_lan_candidates(),
        )
    }

    /// The **receive act**: after the in-person compare, admit exactly this
    /// initiator's ceremony frames for the expectation's TTL
    /// (`group_ceremony_peer::GROUP_CEREMONY_EXPECTATION_TTL_SECS`).
    pub fn expect_share_from(&self, initiator: ActorId) {
        self.peer.expect_share_from(initiator);
    }

    /// Withdraw a receive-act expectation (rule 6 — the user changed their
    /// mind before any offer arrived).
    pub fn cancel_expectation(&self, initiator: &ActorId) {
        self.peer.cancel_expectation(initiator);
    }

    /// Drop every connection a counterpart has open to this seat, keeping the
    /// listener up (see [`PeerNode::close_inbound`]), and return how many were
    /// dropped. An initiator in the middle of a ceremony sees its connection
    /// fail and dials again ([`GroupShareInitiator::with_redial`]).
    pub fn close_inbound(&self) -> usize {
        self._node.close_inbound()
    }

    /// Dial the co-present counterpart by the code they read out — the
    /// actor key IS the NodeId, so no registry lookup is involved (PT-1b).
    pub async fn dial(&self, peer: ActorId) -> anyhow::Result<Arc<PeerChannel>> {
        self.dial_with_candidates(peer, PathCandidates::default())
            .await
    }

    /// Dial the counterpart a **compare code** named — the co-present
    /// ceremony's shape, and the only door an app needs.
    ///
    /// The code's endpoints are the ceremony's whole addressing story: there
    /// is no nest to advertise through, so what the other person read out is
    /// what the dial gets (`p2p.md` § Offline share initiation). Keeping the
    /// `PathCandidates` construction here rather than in seven apps is the
    /// same reason the paint decision is shared — and it keeps the transport
    /// seam out of app code entirely.
    pub async fn dial_code(&self, code: &PeerCode) -> anyhow::Result<Arc<PeerChannel>> {
        self.dial_with_candidates(
            code.actor,
            PathCandidates {
                lan_endpoints: code.lan_endpoints.clone(),
                ..PathCandidates::default()
            },
        )
        .await
    }

    /// [`Self::dial`] with cached path candidates — the share pump's shape:
    /// the discovery cache's advertised endpoints ride as path hints
    /// (`fauna_peer_sync::discovery::share_dial_targets` produces them
    /// through the shared PT-4 hygiene), while the NodeId stays the actor
    /// key. A stale hint costs a slower path, never a wrong peer.
    pub async fn dial_with_candidates(
        &self,
        peer: ActorId,
        candidates: PathCandidates,
    ) -> anyhow::Result<Arc<PeerChannel>> {
        let conn = self
            .transport
            .dial(EndpointKey::from_bytes(peer.0), candidates)
            .await?;
        Ok(Arc::new(PeerChannel::open(conn).await?))
    }
}

/// Why the initiator's drive stopped short.
#[derive(Debug, thiserror::Error)]
pub enum CeremonyDriveError {
    /// A ceremony state transition refused.
    #[error(transparent)]
    Ceremony(#[from] GroupCeremonyError),
    /// The carriage refused, or the channel failed.
    #[error("ceremony transport: {0}")]
    Transport(String),
    /// The connection to the counterpart dropped, and no redial got it back
    /// before the step's budget ran out (or no redial was wired). The
    /// ceremony record survives, as for a spent consent budget.
    #[error("the connection to them dropped and could not be re-established: {0}")]
    ConnectionLost(String),
    /// The recipient did not consent within [`CONSENT_BUDGET`]. The ceremony
    /// record survives — redialing resumes it without a second receive act.
    #[error("the recipient did not consent within the ceremony's consent budget")]
    ConsentBudgetSpent,
    /// The accept frame crossed but was not the accept this ceremony owed.
    #[error("unexpected ceremony frame from the recipient: {0}")]
    UnexpectedFrame(String),
    /// The initiator's deliver never arrived within [`DELIVERY_BUDGET`]. Like
    /// a spent consent budget this is recoverable, not terminal: the recorded
    /// accept still admits the initiator, so a redial finishes the ceremony
    /// without a second consent.
    #[error("the delivery did not arrive within the ceremony's delivery budget")]
    DeliveryBudgetSpent,
    /// The invitation this consent was given for is gone from the record —
    /// a decline on another device, or a config that never merged.
    #[error("no invitation for this scope is on record any more")]
    InvitationGone,
}

/// How long a joiner waits for the initiator's deliver after consenting.
///
/// The same shape as [`CONSENT_BUDGET`] and for the same reason (convention
/// 14): the wait is on a co-present machine building and pushing one frame,
/// which is sub-second in every non-pathological case — so the budget is
/// generous by two orders of magnitude and the assertion is on the recorded
/// deliver, never on elapsed time.
pub const DELIVERY_BUDGET: Duration = Duration::from_secs(120);

/// How finely [`await_delivery`] samples inside [`DELIVERY_BUDGET`].
pub const DELIVERY_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Wait for the initiator's deliver to reach this account's ceremony record.
///
/// The joiner is **purely reactive** by the carriage's design — the deliver
/// is an initiator-originated push, ingested by the serving side into the
/// shared `GroupShareConfig` — so there is nothing to poll on the wire. What this
/// polls is the record itself, which is also what makes it correct across a
/// dropped and redialed connection: the deliver may have landed while the UI
/// was elsewhere, and this returns immediately in that case.
///
/// Shared rather than per-app so seven apps do not each invent a pacing for
/// the same wait.
pub async fn await_delivery(
    config: &Arc<Mutex<GroupShareConfig>>,
    scope_id: &[u8; 32],
) -> Result<(), CeremonyDriveError> {
    tracing::debug!(
        record = ?Arc::as_ptr(config),
        "[offline-share] consent recorded; awaiting the initiator's deliver"
    );
    let deadline = tokio::time::Instant::now() + DELIVERY_BUDGET;
    loop {
        {
            let cfg = config.lock().unwrap();
            let record = cfg
                .invited
                .iter()
                .find(|r| r.scope_id == *scope_id)
                .ok_or(CeremonyDriveError::InvitationGone)?;
            if record.declined {
                // Another device dismissed it while we waited. Not an error to
                // hide: the fleet-wide decline is the answer.
                return Err(CeremonyDriveError::InvitationGone);
            }
            if !record.deliver.is_empty() {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(CeremonyDriveError::DeliveryBudgetSpent);
        }
        tokio::time::sleep(DELIVERY_POLL_INTERVAL).await;
    }
}

/// The initiator side of one co-present ceremony, over one dialed channel.
///
/// Steps are separate so a UI can paint between them ("offer sent" /
/// "awaiting consent" / "delivering"); [`Self::drive`] composes all three
/// under the shared budget for callers that just want the ceremony run.
pub struct GroupShareInitiator {
    channel: Arc<PeerChannel>,
    config: Arc<Mutex<GroupShareConfig>>,
    recipient: ActorId,
    scope_id: Option<[u8; 32]>,
    redial: Option<Redial>,
}

impl GroupShareInitiator {
    pub fn new(
        channel: Arc<PeerChannel>,
        config: Arc<Mutex<GroupShareConfig>>,
        recipient: ActorId,
    ) -> Self {
        Self {
            channel,
            config,
            recipient,
            scope_id: None,
            redial: None,
        }
    }

    /// Let [`Self::drive`] dial the counterpart again when the connection
    /// drops after the offer has crossed.
    ///
    /// `p2p.md` § Offline share initiation: from offer-recorded on, the
    /// recipient's durable ceremony record admits this initiator, so a dropped
    /// connection redials without a second receive act. The drive then carries
    /// on with the SAME ceremony: it keeps asking about the same scope, and a
    /// deliver it already built is sent again rather than minted again.
    /// Without a redial, a lost connection ends the drive with
    /// [`CeremonyDriveError::ConnectionLost`].
    pub fn with_redial(mut self, redial: Redial) -> Self {
        self.redial = Some(redial);
        self
    }

    /// Dial the counterpart again after `lost`, pausing [`REDIAL_PAUSE`]
    /// before each attempt, until one succeeds or `deadline` passes.
    async fn redial_until(
        &mut self,
        deadline: tokio::time::Instant,
        lost: String,
    ) -> Result<(), CeremonyDriveError> {
        let Some(redial) = self.redial.clone() else {
            return Err(CeremonyDriveError::ConnectionLost(lost));
        };
        tracing::info!("[offline-share] the connection dropped part-way; dialing again: {lost}");
        let mut last = lost;
        loop {
            if tokio::time::Instant::now() + REDIAL_PAUSE > deadline {
                return Err(CeremonyDriveError::ConnectionLost(last));
            }
            tokio::time::sleep(REDIAL_PAUSE).await;
            match redial().await {
                Ok(channel) => {
                    tracing::info!("[offline-share] redialed; picking the ceremony up again");
                    self.channel = channel;
                    return Ok(());
                }
                Err(e) => last = format!("{e:#}"),
            }
        }
    }

    /// The scope this ceremony minted, once [`Self::begin`] has run.
    pub fn scope_id(&self) -> Option<[u8; 32]> {
        self.scope_id
    }

    /// Mint the scope, record the ceremony, and push the offer. Returns the
    /// initiator's own held-root row, which the driver writes through.
    pub async fn begin(
        &mut self,
        initiator: &ActorKeypair,
        now: Timestamp,
    ) -> Result<BegunGroupShare, CeremonyDriveError> {
        let begun = {
            let mut cfg = self.config.lock().unwrap();
            begin_group_share(&mut cfg, initiator, self.recipient, now)?
        };
        send_ceremony_offer(Arc::clone(&self.channel), begun.frame.clone())
            .await
            .map_err(carriage_error)?;
        {
            let mut cfg = self.config.lock().unwrap();
            mark_group_offer_posted(&mut cfg, &begun.scope_id, &self.recipient);
        }
        self.scope_id = Some(begun.scope_id);
        Ok(begun)
    }

    /// Ask once whether the recipient has consented. `Ok(false)` is the
    /// consent gap — the recipient's user is still deciding — and is never
    /// an error. A declined invitation surfaces as
    /// [`CeremonyDriveError::Transport`] carrying the carriage's terminal
    /// refusal, which is what stops the poll.
    pub async fn poll_consent(
        &self,
        initiator: &ActorKeypair,
        now: Timestamp,
    ) -> Result<bool, CeremonyDriveError> {
        let scope = self.scope_id.ok_or_else(|| {
            CeremonyDriveError::UnexpectedFrame("poll before the offer was sent".into())
        })?;
        let Some(frame) = poll_ceremony_accept(Arc::clone(&self.channel), &scope)
            .await
            .map_err(carriage_error)?
        else {
            return Ok(false);
        };
        let sender = ActorId(*self.channel.peer_identity().as_bytes());
        let outcome = {
            let mut cfg = self.config.lock().unwrap();
            ingest_group_frame(&mut cfg, &initiator.actor_id(), &sender, &frame, now)?
        };
        match outcome {
            GroupIngestOutcome::AcceptRecorded { scope_id, .. } if scope_id == scope => Ok(true),
            other => Err(CeremonyDriveError::UnexpectedFrame(format!("{other:?}"))),
        }
    }

    /// Mint the roster + first generation, push the deliver, and mark it
    /// posted. The returned writes are the initiator's own plane rows.
    pub async fn deliver(
        &self,
        initiator: &ActorKeypair,
        authority_device: &SigningKey,
        device_authorization: Vec<u8>,
        own_reception: &GroupReceptionKeyRecord,
        now: Timestamp,
    ) -> Result<BuiltGroupShareDeliver, CeremonyDriveError> {
        let built = self.build_deliver(
            initiator,
            authority_device,
            device_authorization,
            own_reception,
            now,
        )?;
        self.push_deliver(&built).await?;
        Ok(built)
    }

    /// Mint the roster + first generation — once per ceremony. A deliver that
    /// has to be sent again after a dropped connection re-sends THIS frame;
    /// building again would mint a second generation for the same scope.
    fn build_deliver(
        &self,
        initiator: &ActorKeypair,
        authority_device: &SigningKey,
        device_authorization: Vec<u8>,
        own_reception: &GroupReceptionKeyRecord,
        now: Timestamp,
    ) -> Result<BuiltGroupShareDeliver, CeremonyDriveError> {
        let scope = self.scope_id.ok_or_else(|| {
            CeremonyDriveError::UnexpectedFrame("deliver before the offer was sent".into())
        })?;
        let mut cfg = self.config.lock().unwrap();
        Ok(build_group_deliver(
            &mut cfg,
            initiator,
            authority_device,
            device_authorization,
            own_reception,
            &scope,
            &self.recipient,
            now,
        )?)
    }

    /// Push a built deliver and mark it posted. Safe to repeat: the recipient
    /// keeps the first deliver it records for a scope.
    async fn push_deliver(&self, built: &BuiltGroupShareDeliver) -> Result<(), CeremonyDriveError> {
        let scope = self.scope_id.ok_or_else(|| {
            CeremonyDriveError::UnexpectedFrame("deliver before the offer was sent".into())
        })?;
        send_ceremony_deliver(Arc::clone(&self.channel), built.frame.clone())
            .await
            .map_err(carriage_error)?;
        let mut cfg = self.config.lock().unwrap();
        mark_group_delivered(&mut cfg, &scope, &self.recipient);
        Ok(())
    }
}

/// Everything one driven initiator ceremony owes its own store — the whole
/// point of returning a struct rather than the deliver alone.
///
/// The held-root row is minted at `begin` and the machinery rows at
/// `deliver`, and the initiator must write BOTH: without the root row its own
/// scope becomes unreadable at the next restart, and without the machinery
/// rows its own folders page cannot list the set it just shared. An earlier
/// shape returned only the deliver, and the tui pilot duly dropped the root
/// row on the floor — so both now leave by the same door.
pub struct DrivenGroupShare {
    /// The initiator's `fauna.state.group-machinery-root` row (from `begin`).
    pub held_root_row: fauna_core::group_generation::GroupHeldRootRecord,
    /// The scope this ceremony minted.
    pub scope_id: [u8; 32],
    /// The deliver's own writes (from `deliver`).
    pub built: BuiltGroupShareDeliver,
}

impl GroupShareInitiator {
    /// Run the whole initiator side: offer → consent → deliver, under
    /// [`CONSENT_BUDGET`] with a deadline poll (convention 14 — the budget
    /// is generous and the assertion is on state, never on elapsed time).
    /// `on_progress` fires at each transition so the UI can paint without
    /// re-implementing the walk.
    ///
    /// The caller writes [`DrivenGroupShare`]'s two halves through its account
    /// runtime and marks the ceremony's monotone booleans only after each
    /// write returns — record-then-act, whose whole point is that a marker
    /// must never outrun the row it claims.
    ///
    /// With a [`Self::with_redial`] wired, a connection that drops once the
    /// offer has crossed is dialed again inside the same budget, and the walk
    /// resumes where it was: the same scope is asked about, and the deliver
    /// already built is sent again. The offer itself is never re-sent after a
    /// drop. Before it crossed, the recipient holds no record that could
    /// admit a redial.
    #[allow(clippy::too_many_arguments)]
    pub async fn drive(
        &mut self,
        initiator: &ActorKeypair,
        authority_device: &SigningKey,
        device_authorization: Vec<u8>,
        own_reception: &GroupReceptionKeyRecord,
        now: Timestamp,
        on_progress: &(dyn Fn(CeremonyStatus) + Send + Sync),
    ) -> Result<DrivenGroupShare, CeremonyDriveError> {
        let begun = self.begin(initiator, now).await?;
        on_progress(CeremonyStatus::OfferSent);
        on_progress(CeremonyStatus::AwaitingConsent);

        let deadline = tokio::time::Instant::now() + CONSENT_BUDGET;
        loop {
            match self.poll_consent(initiator, now).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(CeremonyDriveError::ConnectionLost(lost)) => {
                    self.redial_until(deadline, lost).await?;
                    continue;
                }
                Err(e) => return Err(e),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CeremonyDriveError::ConsentBudgetSpent);
            }
            tokio::time::sleep(CONSENT_POLL_INTERVAL).await;
        }

        on_progress(CeremonyStatus::Delivering);
        let built = self.build_deliver(
            initiator,
            authority_device,
            device_authorization,
            own_reception,
            now,
        )?;
        let deadline = tokio::time::Instant::now() + DELIVERY_BUDGET;
        loop {
            match self.push_deliver(&built).await {
                Ok(()) => break,
                Err(CeremonyDriveError::ConnectionLost(lost)) => {
                    self.redial_until(deadline, lost).await?;
                }
                Err(e) => return Err(e),
            }
        }
        on_progress(CeremonyStatus::Delivered);
        Ok(DrivenGroupShare {
            held_root_row: begun.held_root_row,
            scope_id: begun.scope_id,
            built,
        })
    }
}

/// Consume one delivered invitation on the joiner's side: run the full
/// verification chain and hand back everything the driver writes through.
///
/// A thin re-export shape rather than new logic — the door apps reach for,
/// so no app re-derives which of `group_ceremony`'s functions closes the
/// recipient's side.
pub fn admit_delivered_share(
    config: &Arc<Mutex<GroupShareConfig>>,
    own: &ActorKeypair,
    reception: &GroupReceptionKeyRecord,
    scope_id: &[u8; 32],
    now: Timestamp,
) -> Result<AdmittedGroupShare, GroupCeremonyError> {
    let cfg = config.lock().unwrap();
    admit_group_share(&cfg, own, reception, scope_id, now)
}

/// Consent to an offered share: record the signed accept against the
/// invitation, published to `reception`'s public half.
///
/// **No frame is posted here**, and that is the carriage's shape rather than
/// an omission: the joiner never originates: the initiator *polls* for the
/// accept, and the serving side answers that poll straight out of this
/// record. So consent is complete the moment it is recorded, whether or not
/// the counterpart is connected at that instant.
///
/// The caller must have persisted `reception` BEFORE calling this
/// (`AccountStoreHandle::put_group_reception_key`): its public half goes out
/// in the accept and the initiator seals the admission bundle to it, so a
/// keypair that never reached durable storage would make the delivery
/// unopenable forever.
pub fn consent_to_group_share(
    config: &Arc<Mutex<GroupShareConfig>>,
    recipient: &ActorKeypair,
    reception: &GroupReceptionKeyRecord,
    scope_id: &[u8; 32],
    now: Timestamp,
) -> Result<(), GroupCeremonyError> {
    let mut cfg = config.lock().unwrap();
    crate::group_ceremony::build_group_accept(&mut cfg, recipient, scope_id, reception, now)
        .map(|_frame| ())
}

/// Decline an offered share (rule 6). Monotone and fleet-wide: the record
/// stays, marked, so the invitation never re-knocks on any of this account's
/// devices, and the serving side stops admitting the initiator's frames for
/// it.
///
/// `false` = there was no invitation for this scope to decline, which a
/// caller may treat as already-handled rather than an error.
pub fn decline_group_share(
    config: &Arc<Mutex<GroupShareConfig>>,
    scope_id: &[u8; 32],
    now: Timestamp,
) -> bool {
    let mut cfg = config.lock().unwrap();
    crate::group_ceremony::decline_group_offer(&mut cfg, scope_id, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brake_refuses_on_no_evidence_and_never_on_optimism() {
        // No evidence at all — a fresh store that never reached its nest.
        assert_eq!(
            ceremony_bind_verdict(None),
            CeremonyBindVerdict::NoBrakeEvidence
        );
        // Evidence, but the token is absent: the fleet brake is on.
        assert_eq!(
            ceremony_bind_verdict(Some(&["peer-sync".to_string()])),
            CeremonyBindVerdict::BrakeOn
        );
        // An empty advertisement is still evidence — and says no.
        assert_eq!(
            ceremony_bind_verdict(Some(&[])),
            CeremonyBindVerdict::BrakeOn
        );
        // The token, among others.
        assert_eq!(
            ceremony_bind_verdict(Some(&[
                "peer-sync".to_string(),
                P2P_SHARE_CAPABILITY.to_string(),
            ])),
            CeremonyBindVerdict::Bind
        );
    }
}
