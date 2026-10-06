//! The **offline co-present share ceremony**, shared (`docs/goal/behavior/p2p.md`
//! § Offline share initiation).
//!
//! Every app's folders page drives the same ceremony: two people in the same
//! room read a compare code to each other, and one hands the other a
//! recipient set without either device consulting a nest. What follows is the
//! whole of that act except the one part that genuinely differs per app — the
//! transport assembly — so no app owns a private copy of an ordering rule.
//!
//! # What lives here, and what stays in the app
//!
//! This module owns the ceremony's **orchestration and its records**:
//! [`initiate`], [`consent`], [`decline`], the write-through that lands the
//! rows, the record flush, the input parsing, and the folders page's own
//! read ([`load_group_shares`] and its view types).
//!
//! The **bind doors** live here too ([`bind_seat`] for the panel,
//! [`bind_share_plane_seat`] for the transfer plane), lifted 2026-08-23 once
//! the second app leg had landed a byte-identical copy of both. Both go
//! through the session's one [`SessionSeat`], so whichever asks first binds
//! and the other is handed that seat back — the adoption rule lives in that
//! slot, never in an app's fold. What the app
//! keeps of them is exactly two things: its own **device label** (the string
//! the person across the table reads) and a
//! [`CeremonyTransportFactory`] — the closure that assembles the actor-keyed
//! endpoint. That second one is not taste: this crate names no concrete
//! transport substrate (the same iroh-cleanliness bargain
//! [`crate::peer_leg`] documents, where the caller passes the transport
//! factory), so the `fauna_iroh` call belongs on the app side of the seam.
//! Everything else about a bind — the ordering, the brake verdict, the record
//! load, the error wording — is one implementation for all seven apps.
//!
//! Wording is likewise the app's: this module decides *facts* and never
//! sentences, the same posture [`crate::group_scope_view`] states for the
//! listing projection.
//!
//! # The ceremony's own record, and where it rests
//!
//! The record is the account plane's `fauna.state.group-share-ceremony` rows
//! (one per ceremony side-record, folded into a [`GroupShareConfig`]),
//! reached through the [`CeremonyRecord`] seam — the account-store handle in
//! production. That store is local and durable, so the record rests and reads
//! with no nest at all, which is the co-present ceremony's whole case; its
//! fleet siblings receive it by the ordinary account-plane walk. The store
//! exists only once the account runtime is assembled, seconds after sign-in,
//! so the record is **lent to the session seat late**
//! ([`SessionSeat::lend_record`], `p2p.md` § Offline share initiation → *The
//! seat's record is lent late*): a seat binds over an empty replica and the
//! lend replays it. The ceremony's other product — the held machinery root
//! and the scope's plane rows — is not config at all: it goes through the
//! account runtime's group doors onto the group plane
//! (`docs/goal/architecture/account-data-plane.md` § Implementation status
//! today). Three orderings here are load-bearing rather than stylistic, and
//! [`write_through`] is one function so they cannot drift apart:
//!
//! - **The reception keypair rests BEFORE the accept is recorded.** Its public
//!   half rides the accept and the initiator seals the admission bundle to it;
//!   a crash between the two must leave an unused key, never an unopenable
//!   delivery.
//! - **The root row lands BEFORE the machinery rows.** The rows seal under
//!   that root, so adopting first would risk entries this device could not
//!   re-open.
//! - **Every monotone marker is set AFTER its write returned.** A marker that
//!   outran its row would tell a resuming driver the scope is readable when it
//!   is not — the record-then-act law's whole point.
//!
//! # What the folders page reads, and from where
//!
//! Two different sources, deliberately: consent cards come from the ceremony
//! record ([`load_group_shares`] reads the durable account store joined with
//! the bound seat's replica — an invitation must survive a restart, and one
//! ingested before the lend must still paint), and the shared-set listing
//! comes from the **store's own group rows**. A set is listed because its
//! machinery landed, never because a ceremony was recorded — painting the
//! latter would name a set this device cannot read a byte of, which is exactly
//! the stub the standing tui-parity rule forbids.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock, Weak};

use fauna_client_capabilities::group_ceremony_node::{
    CeremonyNode, GroupShareInitiator, ceremony_bind_verdict,
};
use fauna_client_capabilities::group_ceremony_view::{
    CeremonyStatus, OfflineSharePanel, OfflineShareView, PeerCode, PeerCodeError, parse_peer_code,
};
use fauna_core::data::Timestamp;
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_generation::GroupReceptionKeyRecord;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_peer_share::admission::{GroupRosterState, SetMembership};

use crate::account_runtime::AccountStoreHandle;

/// The page state the Folders surface paints from.
#[derive(Default)]
pub struct OfflineShareState {
    /// Which panel is open.
    pub panel: OfflineSharePanel,
    /// The `offline-share-peer-code-input` buffer — a local draft committed
    /// only by Begin/Expect (the `muted-word-input` shape).
    pub peer_code_input: String,
    /// This side's progress, painted in `offline-share-status`.
    pub status: CeremonyStatus,
    /// This sign-in's seat slot — the one both bind doors go through. The
    /// panel reads its seat from here ([`Self::seat`]) instead of keeping a
    /// copy, so a seat the share plane bound is the panel's by construction.
    pub session_seat: SessionSeat,
    /// This actor's own id — set at the post-auth hook, and what makes the
    /// affordance available at all.
    pub own: Option<ActorId>,
    /// This actor's secret, for the actor-keyed transport and for signing the
    /// ceremony's own frames.
    pub secret_hex: Option<String>,
}

/// One bound ceremony seat: the listener, the in-memory replica of the
/// ceremony record it records into, and the session's lend slot for where
/// that record rests (empty until the account runtime lends it).
pub struct CeremonySeat {
    pub node: CeremonyNode,
    pub config: Arc<Mutex<GroupShareConfig>>,
    pub record: Arc<SessionRecord>,
}

/// Hand-written so anything that carries a seat can derive `Debug` without a
/// live listener (or a ceremony record holding a machinery root) ever
/// reaching a log line. What a reader needs is that a seat exists and whose it is — the
/// compare code is public by construction.
impl std::fmt::Debug for CeremonySeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CeremonySeat")
            .field("own_code", &self.node.own_code())
            .finish_non_exhaustive()
    }
}

/// Build the page state at the post-auth hook. Binds nothing: it mints this
/// sign-in's empty [`SessionSeat`], which whichever door asks first binds.
pub fn init(secret_hex: &str) -> OfflineShareState {
    let own = ActorKeypair::from_secret_hex(secret_hex)
        .ok()
        .map(|k| k.actor_id());
    OfflineShareState {
        own,
        secret_hex: own.is_some().then(|| secret_hex.to_string()),
        ..OfflineShareState::default()
    }
}

impl OfflineShareState {
    /// The shared paint projection — every element decision comes from here,
    /// never from this struct's fields directly.
    pub fn view(&self) -> OfflineShareView {
        // The endpoints come from the BOUND seat, not from the machine: the
        // code must publish where this listener is actually reachable, and
        // before either door binds there is no listener and nothing to
        // publish.
        let own_endpoints = self
            .seat()
            .map(|s| s.node.local_endpoints())
            .unwrap_or_default();
        OfflineShareView::new(
            self.panel,
            self.own.as_ref(),
            &own_endpoints,
            &self.peer_code_input,
            self.status,
        )
    }

    /// The session's bound seat, once either door bound it.
    pub fn seat(&self) -> Option<Arc<CeremonySeat>> {
        self.session_seat.seat()
    }

    /// The keypair this seat signs with, rebuilt per act (never held — the
    /// secret rests in one place, and a rebuild is cheap).
    pub fn keypair(&self) -> Option<ActorKeypair> {
        ActorKeypair::from_secret_hex(self.secret_hex.as_deref()?).ok()
    }
}

/// The clock a bound seat mints and judges receive-act expectations against,
/// as the shared glue's injected `NowFn`.
///
/// [`crate::ceremony_clock::now_secs`], not [`now_secs`]: the admission window
/// is a 15-minute Rust constant, so a journey can only witness a LAPSED
/// expectation by moving this clock (convention 14's fake clock, never a
/// sleep, and never a configurable TTL). The offset it adds is compiled out of
/// release artifacts and zero in every real run, and the closure reads it per
/// call rather than capturing it, so a test can move the window of a seat that
/// is already listening. The ceremony's durable record timestamps keep
/// [`now_secs`]' real clock — an offset must never forward-date a stored row.
pub fn now_fn() -> fauna_client_capabilities::group_ceremony_peer::NowFn {
    Arc::new(|| Timestamp(crate::ceremony_clock::now_secs()))
}

pub fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero() as u64
}

/// Where the ceremony record rests: the account plane's
/// `fauna.state.group-share-ceremony` row, through the account store's typed
/// door ([`AccountStoreHandle::group_shares`] /
/// [`AccountStoreHandle::merge_group_shares`]). A seam rather than the handle
/// itself only so the seat's ordering rules are testable without an
/// assembled runtime — the same reason the retired store seam it
/// replaced was one.
pub trait CeremonyRecord: Send + Sync {
    /// The record as it rests — empty when none does yet.
    fn load(&self) -> futures_util::future::BoxFuture<'_, Result<GroupShareConfig, String>>;

    /// Join `replica` into the resting record, write it when it moved, and
    /// answer the record as it now stands.
    fn merge(
        &self,
        replica: GroupShareConfig,
    ) -> futures_util::future::BoxFuture<'_, Result<GroupShareConfig, String>>;
}

impl CeremonyRecord for AccountStoreHandle {
    fn load(&self) -> futures_util::future::BoxFuture<'_, Result<GroupShareConfig, String>> {
        Box::pin(async move { self.group_shares().await.map_err(|e| format!("{e:#}")) })
    }

    fn merge(
        &self,
        replica: GroupShareConfig,
    ) -> futures_util::future::BoxFuture<'_, Result<GroupShareConfig, String>> {
        Box::pin(async move {
            self.merge_group_shares(replica)
                .await
                .map_err(|e| format!("{e:#}"))
        })
    }
}

/// Persist the seat's ceremony record into the lent store — a no-op while
/// nothing is lent (the lend replays the replica, [`SessionSeat::lend_record`]).
///
/// The seat's record is a **replica**: loaded at bind (or at the lend), mutated in memory
/// by the serve side (under a lock it cannot await in) and by the driver's own
/// acts, and persisted here. Two writes race it, and a flush loses neither,
/// because both directions are the record's own join
/// (`GroupShareConfig::merge`):
///
/// - **What the store gained since the seat loaded** — a record another
///   device's walk merged in, or another surface of this app wrote. The store
///   joins the replica INTO what it holds, never puts it over it.
/// - **What the replica gained while the write was in flight** — a frame the
///   serve side ingested meanwhile. The stored result is joined back into the
///   replica, never assigned over it. An assignment erased exactly the
///   co-present ceremony's deliver: the initiator's poll is answered from the
///   accept the moment it is recorded, so its deliver lands while the
///   recipient is still persisting that accept, and the next flush then
///   persisted the loss (`p2p.md` § Offline share initiation).
pub async fn flush(
    record: &SessionRecord,
    config: &Arc<Mutex<GroupShareConfig>>,
) -> Result<(), String> {
    let Some(record) = record.lent() else {
        return Ok(());
    };
    let local = config.lock().unwrap().clone();
    let stored = record
        .merge(local)
        .await
        .map_err(|e| format!("ceremony record: {e}"))?;
    let mut replica = config.lock().unwrap();
    *replica = replica.merge(&stored);
    Ok(())
}

/// The persistence signal both bind doors share: `on_config_change` fires
/// inside the state machine's lock, so it cannot await — it nudges a task
/// that can.
pub fn spawn_record_persist(
    record: &Arc<SessionRecord>,
    config: &Arc<Mutex<GroupShareConfig>>,
) -> fauna_client_capabilities::group_ceremony_peer::OnConfigChange {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    {
        let record = Arc::clone(record);
        let config = Arc::clone(config);
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                if let Err(e) = flush(&record, &config).await {
                    tracing::warn!("[offline-share] ceremony record not persisted: {e}");
                }
            }
        });
    }
    Arc::new(move || {
        let _ = tx.send(());
    })
}

/// What an app's ceremony transport factory hands back: the seam-typed
/// transport plus the one fact only the concrete constructor can observe —
/// where the endpoint actually bound. That address list is the compare code's
/// **addressing half**, and the share plane's publish pump crosses it with
/// this device's interface list.
///
/// Mirrors [`crate::peer_leg::PeerLegBinding`] deliberately: same seam, same
/// reason the transport trait itself carries no `bound_addrs`.
pub struct CeremonyBinding {
    pub transport: Arc<dyn fauna_transport::PeerTransport>,
    pub bound_addrs: Vec<SocketAddr>,
}

/// Constructs the ceremony's **actor-keyed** transport (PT-1b — the NodeId is
/// the actor key, which is what makes the in-person code compare mean
/// anything; a different endpoint from the same-account peer-sync leg's
/// device-principal one).
///
/// This callback is the app's whole remaining share of a bind door, and it is
/// a callback rather than a call because this crate names no concrete
/// transport substrate (module docs): the `fauna_iroh` assembly belongs on the
/// app side of the seam. Every app passes a closure over
/// `fauna_iroh::peer_leg_transport(secret, None)`; a test passes an in-memory
/// transport. Same shape, and the same reason, as
/// [`crate::peer_leg::PeerTransportFactory`].
///
/// ⚠ The relay URL stays `None` in every production closure today — by ruling,
/// until the nest's relay serves address discovery (`p2p.md` § The relay →
/// *The cross-user seat and the relay*). A co-present pair is on the same
/// network by construction and its dials never attach a relay (their
/// candidates come from the spoken code alone), so the seat takes the relay
/// for the share plane's dials only; whether an unreachable relay slows a
/// direct dial is the measurement that build lands first.
pub type CeremonyTransportFactory = Arc<
    dyn Fn([u8; 32]) -> futures_util::future::BoxFuture<'static, Result<CeremonyBinding, String>>
        + Send
        + Sync,
>;

/// This sign-in's ONE actor-keyed ceremony seat — the slot both bind doors go
/// through (`p2p.md` § Offline share initiation → *One seat per session*).
///
/// A session has two doors that want the same endpoint: the panel's
/// ([`bind_seat`], when the user opens a panel) and the share plane's
/// ([`bind_share_plane_seat`], on its driver's first pass with a set to
/// serve). Whichever asks first binds; the other is handed that seat back.
/// The bind runs INSIDE the slot's once-cell, not behind a check each door
/// makes against its own copy, so a second listener on this NodeId cannot
/// come up in either order or in a race. (The two per-app folds this replaced
/// ran in the apps' seat-bound handlers — after the driver's own bind had
/// already opened its listener.)
///
/// Every seat binds share-plane-capable, over the session's
/// [`SessionRoster`], which admits nobody until the plane's driver lends it
/// the M2 roster consult. A seat the panel bound first therefore serves no set
/// until the moment the plane would otherwise have bound its own, and a
/// ceremony in flight on it is never displaced by a second seat.
///
/// One per sign-in ([`init`] mints it empty). A cancelled ceremony keeps the
/// seat, since the listener is the session's; the one thing that takes a
/// bound seat down within a sign-in is the device's own participation
/// switch ([`Self::unbind`], driven by `crate::share_glue::run` —
/// `p2p.md` § Per-device participation), and the same verdict refuses
/// either door's bind while it is off ([`Self::set_participation`]).
#[derive(Clone, Default)]
pub struct SessionSeat(Arc<SessionSeatSlot>);

#[derive(Default)]
struct SessionSeatSlot {
    /// The bound seat. Written only under [`Self::bind_lock`]; read freely.
    bound: RwLock<Option<(Arc<CeremonySeat>, Vec<SocketAddr>)>>,
    /// Serializes the two doors' binds (and an unbind against them), so a
    /// second listener on this NodeId cannot come up in either order or in
    /// a race — the guarantee the once-cell used to give.
    bind_lock: tokio::sync::Mutex<()>,
    /// This device's participation verdict as last read from its own row
    /// (`Some(false)` refuses both doors; `Some(true)` admits). `None` until
    /// the share driver's first pass has read the store — which admits, as
    /// every bind did before the control existed: the row lives on the
    /// account store, assembled seconds after sign-in, and the driver's
    /// first pass at that edge unbinds whatever a door bound under no
    /// verdict if the row says off. So the one window a switched-off device
    /// can hold a listener is sign-in → store-ready, and only through the
    /// panel's own door (`p2p.md` § Implementation status today).
    participation: RwLock<Option<bool>>,
    roster: Arc<SessionRoster>,
    group_roster: Arc<SessionGroupRoster>,
    record: Arc<SessionRecord>,
}

/// The bind refusal while the device's own switch is off — the string the
/// offline-share panel paints through its error prefix.
pub const PARTICIPATION_OFF: &str = "peer transfers are off on this device (Settings → Devices)";

impl SessionSeat {
    /// The bound seat, once either door bound it.
    pub fn seat(&self) -> Option<Arc<CeremonySeat>> {
        self.0
            .bound
            .read()
            .unwrap()
            .as_ref()
            .map(|(seat, _)| Arc::clone(seat))
    }

    /// Record the device's participation verdict
    /// (`crate::p2p_participation::P2pParticipation::effective`), read by
    /// the share driver from the store at every pass and on every toggle.
    /// `Some(false)` makes both doors refuse; it does NOT drop a bound seat —
    /// [`Self::unbind`] does, and the driver calls the two together.
    pub fn set_participation(&self, verdict: Option<bool>) {
        *self.0.participation.write().unwrap() = verdict;
    }

    /// The verdict as last recorded (`None` = not read yet).
    pub fn participation(&self) -> Option<bool> {
        *self.0.participation.read().unwrap()
    }

    /// Drop the bound seat, if any: the listener and every inbound channel
    /// end with the last handle (the driver drops its own clone beside this
    /// call; a panel mid-ceremony keeps only a dead node until it re-reads
    /// [`Self::seat`]). Returns whether a seat was bound.
    pub async fn unbind(&self) -> bool {
        let _serialized = self.0.bind_lock.lock().await;
        self.0.bound.write().unwrap().take().is_some()
    }

    /// Lend the session seat its ceremony record — the account runtime's host
    /// calls this once, at the account-store-ready edge (`p2p.md` § Offline
    /// share initiation → *The seat's record is lent late*). Held `Weak`, like
    /// the roster lends: the caller keeps the strong `Arc` for the runtime's
    /// life, and a panel's hold on the seat never keeps the runtime open.
    ///
    /// When a seat is already bound, one [`flush`] runs at once — both
    /// directions of the record's join: what the replica ingested before the
    /// lend lands in the store, and the store's rows reach the replica.
    /// Serialized against a bind, so a lend and a bind racing each other
    /// cannot both miss: either the bind loads the lent record, or the lend
    /// finds the bound seat.
    ///
    /// # Errors
    /// The lend-time flush's refusal (the lend itself always takes).
    pub async fn lend_record(&self, record: &Arc<dyn CeremonyRecord>) -> Result<(), String> {
        let _serialized = self.0.bind_lock.lock().await;
        self.0.record.lend(record);
        match self.seat() {
            Some(seat) => flush(&seat.record, &seat.config).await,
            None => Ok(()),
        }
    }

    /// Whether `other` is this very slot (a clone of it), not merely another
    /// empty one — what a host holding one slot per sign-in checks to prove it
    /// hands the same slot to both bind doors.
    pub fn is_same_slot(&self, other: &SessionSeat) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// This session's seat and its bound addresses, binding it first when no
    /// door has. `evidence` is awaited only by the door that binds: a seat
    /// already bound is handed back without reading the brake again.
    async fn get_or_bind(
        &self,
        secret_hex: &str,
        device_label: &str,
        evidence: impl std::future::Future<Output = Option<Vec<String>>>,
        transport: &CeremonyTransportFactory,
    ) -> Result<(Arc<CeremonySeat>, Vec<SocketAddr>), String> {
        let _serialized = self.0.bind_lock.lock().await;
        if let Some((seat, bound)) = self.0.bound.read().unwrap().as_ref() {
            return Ok((Arc::clone(seat), bound.clone()));
        }
        // The device's own switch, before the brake and before any I/O: a
        // listener never comes up while it is off (`p2p.md` § Per-device
        // participation). An unread verdict admits — the slot's doc owns why.
        if self.participation() == Some(false) {
            return Err(PARTICIPATION_OFF.to_string());
        }
        let roster = Arc::clone(&self.0.roster) as Arc<dyn SetMembership + Send + Sync>;
        let group_roster =
            Arc::clone(&self.0.group_roster) as Arc<dyn GroupRosterState + Send + Sync>;
        let evidence = evidence.await;
        let (seat, bound) = bind(
            secret_hex,
            device_label,
            Arc::clone(&self.0.record),
            evidence.as_deref(),
            roster,
            group_roster,
            transport,
        )
        .await?;
        *self.0.bound.write().unwrap() = Some((Arc::clone(&seat), bound.clone()));
        Ok((seat, bound))
    }
}

/// The session seat's M2 roster consult, lent late: it admits nobody until
/// the share plane's driver lends it the real roster, and nobody again once
/// that driver lets go.
///
/// Held as a `Weak` on purpose. The driver's roster holds the conversations
/// rail's MLS engine, whose one-engine-per-store lock the next account
/// session's open needs (`crate::share_glue::run`'s docs); a strong clone
/// here would keep that engine for as long as anything holds the seat — the
/// panel's state, for the rest of the sign-in.
#[derive(Default)]
struct SessionRoster(RwLock<Option<Weak<dyn SetMembership + Send + Sync>>>);

impl SessionRoster {
    fn lend(&self, roster: &Arc<dyn SetMembership + Send + Sync>) {
        *self.0.write().unwrap() = Some(Arc::downgrade(roster));
    }
}

impl SetMembership for SessionRoster {
    fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
        // Upgraded under the lock, consulted after it: the consult reads an
        // MLS engine, and a lend must never wait behind that.
        let lent = self.0.read().unwrap().as_ref().and_then(Weak::upgrade);
        lent.is_some_and(|roster| roster.is_member(channel_id, actor))
    }
}

/// The session seat's peer witness door evaluator, lent late — the
/// [`SessionRoster`] twin for the group family: it holds no group scope until
/// the share plane's driver lends it the pump-fed evaluator
/// (`crate::group_roster_door::LiveGroupRoster`, rebuilt each pass from this
/// replica's own store), and none again once that driver lets go. Held as a
/// `Weak` for the same reason as its twin: the lend's lifetime is the
/// driver's loop, never the panel's hold on the seat.
#[derive(Default)]
struct SessionGroupRoster(RwLock<Option<Weak<dyn GroupRosterState + Send + Sync>>>);

impl SessionGroupRoster {
    fn lend(&self, roster: &Arc<dyn GroupRosterState + Send + Sync>) {
        *self.0.write().unwrap() = Some(Arc::downgrade(roster));
    }

    fn lent(&self) -> Option<Arc<dyn GroupRosterState + Send + Sync>> {
        self.0.read().unwrap().as_ref().and_then(Weak::upgrade)
    }
}

impl GroupRosterState for SessionGroupRoster {
    fn authority(&self, scope_id: &[u8; 32]) -> Option<fauna_core::group_scope::GroupAuthority> {
        self.lent().and_then(|roster| roster.authority(scope_id))
    }

    fn is_entry_removed(&self, scope_id: &[u8; 32], entry_id: &[u8; 32]) -> bool {
        // Nothing lent means no scope is held, which reads as removed: the
        // lend can drop between the door's `authority` read and this one.
        self.lent()
            .is_none_or(|roster| roster.is_entry_removed(scope_id, entry_id))
    }
}

/// The session seat's ceremony record, lent late
/// ([`SessionSeat::lend_record`]): empty until the account runtime's host
/// lends the store's handle at the store-ready edge, and empty again once that
/// host lets go. Held as a `Weak` for its twins' reason — the account driver's
/// command channel closes when its last handle drops, so a strong hold here
/// would keep the runtime open for as long as a panel holds the seat.
#[derive(Default)]
pub struct SessionRecord(RwLock<Option<Weak<dyn CeremonyRecord>>>);

impl SessionRecord {
    fn lend(&self, record: &Arc<dyn CeremonyRecord>) {
        *self.0.write().unwrap() = Some(Arc::downgrade(record));
    }

    /// The lent record, while its lender still holds it.
    pub fn lent(&self) -> Option<Arc<dyn CeremonyRecord>> {
        self.0.read().unwrap().as_ref().and_then(Weak::upgrade)
    }
}

/// The account-store-ready edge's one ceremony call, on every host that runs
/// an account runtime (tui and linux beside the share plane's start,
/// `fauna-ffi` at its runtime's start): lend `account` to `seat` as its
/// ceremony record ([`SessionSeat::lend_record`]) and hold that lease for
/// exactly the runtime's life — a spawned task keeps the strong `Arc` until
/// [`AccountStoreHandle::closed`] resolves, so no host keeps a field for it,
/// and a lease can neither outlive the runtime nor end before it. The seat
/// slot itself is released as soon as the lend returns: the task must never
/// be what keeps a signed-out session's listener up.
pub fn lend_account_record(seat: &SessionSeat, account: AccountStoreHandle) {
    let seat = seat.clone();
    tokio::spawn(async move {
        let record: Arc<dyn CeremonyRecord> = Arc::new(account.clone());
        if let Err(e) = seat.lend_record(&record).await {
            tracing::warn!("[offline-share] ceremony record not joined at the lend: {e}");
        }
        drop(seat);
        account.closed().await;
        drop(record);
    });
}

/// The **panel** bind door: this session's seat, handed back if the share
/// plane already bound it, else bound — brake read live, ceremony record
/// loaded (if lent — [`SessionSeat::lend_record`]; else the seat starts on an
/// empty replica), actor-keyed transport built, listener opened. The bind never
/// waits for the account runtime: the panel opens whenever the user opens it.
///
/// `device_label` is the one string that genuinely differs per app (it names
/// this device to the person across the table) — everything else here is one
/// implementation for all seven apps, lifted 2026-08-23 once linux's leg had
/// landed a second byte-identical copy of it.
pub async fn bind_seat(
    session_seat: &SessionSeat,
    nest: Arc<fauna_client::NestClient>,
    secret_hex: &str,
    device_label: &str,
    transport: &CeremonyTransportFactory,
) -> Result<Arc<CeremonySeat>, String> {
    // Rule 7's brake, read live. `Err` is NO evidence, never optimism — the
    // shared verdict function turns `None` into a refusal.
    let capabilities = async move {
        fauna_client_features::FeaturesClient::new(nest)
            .node_capabilities()
            .await
            .ok()
    };
    let (seat, _bound) = session_seat
        .get_or_bind(secret_hex, device_label, capabilities, transport)
        .await?;
    Ok(seat)
}

/// The **share plane's** bind door ([`crate::share_glue`]): the SAME session
/// seat as [`bind_seat`] — bound here only when the panel has not bound it —
/// then lent `membership`, the M2 roster consult the plane's driver holds for
/// its whole loop ([`SessionSeat`], [`SessionRoster`]), and `group_roster`,
/// the peer witness door's evaluator the same driver feeds from this
/// replica's store ([`SessionGroupRoster`]). The per-set serve
/// router the pump feeds rides the same seat (`CeremonyNode::set_shared_sets`).
///
/// `evidence` is the shared driver's composed brake reading — live
/// `fauna.nest.info` when reachable, else the peer leg's cached last-known
/// capabilities (rule 7's offline half); the verdict typing inside the bind
/// still refuses no-evidence.
///
/// Returns the bound socket addresses too: the publish pump crosses them with
/// the interface list and advertises the result on each set's own channel.
#[allow(clippy::too_many_arguments)] // the bind door's own shape: one lend per witness family
pub async fn bind_share_plane_seat(
    session_seat: &SessionSeat,
    secret_hex: &str,
    device_label: &str,
    evidence: Option<Vec<String>>,
    membership: &Arc<dyn SetMembership + Send + Sync>,
    group_roster: &Arc<dyn GroupRosterState + Send + Sync>,
    transport: &CeremonyTransportFactory,
) -> Result<(Arc<CeremonySeat>, Vec<SocketAddr>), String> {
    let bound = session_seat
        .get_or_bind(secret_hex, device_label, async move { evidence }, transport)
        .await?;
    // Lent only once a seat is up: a refused bind lends nothing, so a seat the
    // panel binds after it admits nobody, exactly as it would have alone.
    session_seat.0.roster.lend(membership);
    session_seat.0.group_roster.lend(group_roster);
    Ok(bound)
}

/// The body every bind shares, run inside [`SessionSeat`]'s once-cell: the
/// ordering below (parse → verdict → record load if lent → transport → bind → record
/// the bound addresses on the seat) is a contract, not a style, and one function
/// is what keeps it from drifting between the doors. `membership` is always
/// the session's [`SessionRoster`] and `group_roster` its
/// [`SessionGroupRoster`] — `CeremonyNode::bind`'s ceremony-only form would
/// fix a seat the share plane could never ride.
async fn bind(
    secret_hex: &str,
    device_label: &str,
    record: Arc<SessionRecord>,
    evidence: Option<&[String]>,
    membership: Arc<dyn SetMembership + Send + Sync>,
    group_roster: Arc<dyn GroupRosterState + Send + Sync>,
    transport: &CeremonyTransportFactory,
) -> Result<(Arc<CeremonySeat>, Vec<SocketAddr>), String> {
    let keypair =
        ActorKeypair::from_secret_hex(secret_hex).map_err(|e| format!("invalid secret: {e}"))?;
    let verdict = ceremony_bind_verdict(evidence);

    // Lent → the resting record; not yet → an empty replica the lend replays.
    let loaded = match record.lent() {
        Some(lent) => lent
            .load()
            .await
            .map_err(|e| format!("ceremony record load: {e}"))?,
        None => GroupShareConfig::default(),
    };
    let config = Arc::new(Mutex::new(loaded));

    // `bound_addrs` is the assembler-observed truth the seam deliberately does
    // not expose (`crate::peer_leg` module docs).
    let CeremonyBinding {
        transport,
        bound_addrs,
    } = transport(*keypair.secret_bytes()).await?;

    let node = CeremonyNode::bind_with_share_plane(
        verdict,
        transport,
        keypair.actor_id(),
        device_label.to_string(),
        Arc::clone(&config),
        now_fn(),
        spawn_record_persist(&record, &config),
        membership,
    )
    .await
    .map_err(|e| e.to_string())?
    // The seat carries its bound addresses too (the compare code's addressing
    // half); the returned copy feeds the publish pump.
    .with_bound_addrs(bound_addrs.clone());
    // Every seat carries the door's evaluator, lent or not: unlent it holds no
    // scope and refuses every group witness, exactly as an unwired server did.
    node.set_group_roster(group_roster);

    Ok((
        Arc::new(CeremonySeat {
            node,
            config,
            record,
        }),
        bound_addrs,
    ))
}

/// initiator's ceremony frames for the expectation's TTL.
pub fn expect_from(seat: &CeremonySeat, initiator: ActorId) {
    seat.node.expect_share_from(initiator);
}

/// Withdraw the receive act (rule 6).
pub fn cancel_expectation(seat: &CeremonySeat, initiator: &ActorId) {
    seat.node.cancel_expectation(initiator);
}

/// Run the initiator's whole side against the co-present counterpart, and
/// persist what it records.
///
/// `authority` is this machine's device principal + its `DeviceAuthorization`
/// carriage (`AccountStoreHandle::group_ceremony_authority`). Its absence is
/// not an error to hide: an unenrolled machine genuinely cannot mint a roster
/// entry anything else will verify, so the act refuses with a plain reason.
pub async fn initiate(
    seat: Arc<CeremonySeat>,
    account: Option<AccountStoreHandle>,
    keypair: ActorKeypair,
    peer: PeerCode,
) -> Result<CeremonyStatus, String> {
    let account = account.ok_or_else(|| "this device has no account runtime yet".to_string())?;
    let authority = account
        .group_ceremony_authority()
        .await
        .map_err(|e| format!("authority read: {e}"))?
        .ok_or_else(|| "this device is not enrolled yet, so it cannot start a share".to_string())?;

    // The candidates the counterpart read out ARE the addressing — a
    // co-present pair has no nest to advertise through, so without them the
    // dial has an `EndpointId` and no path and iroh answers "No addressing
    // information available" (the measured RED this resolves). They stay
    // peer-supplied and therefore attacker-influenceable: the transport's own
    // PT-4 `is_safe_candidate` filter is what makes them safe to hand over.
    let channel = seat
        .node
        .dial_code(&peer)
        .await
        .map_err(|e| format!("could not reach them: {e:#}"))?;
    // If the link drops once the offer has crossed, dial the same code again:
    // the recipient's ceremony record admits us from then on, so neither
    // person re-enters a code (`p2p.md` § Offline share initiation).
    let redial: fauna_client_capabilities::group_ceremony_node::Redial = {
        let seat = Arc::clone(&seat);
        let code = peer.clone();
        Arc::new(move || {
            let seat = Arc::clone(&seat);
            let code = code.clone();
            Box::pin(async move { seat.node.dial_code(&code).await })
        })
    };
    let peer = peer.actor;

    let now = Timestamp(now_secs());
    let mut initiator =
        GroupShareInitiator::new(channel, Arc::clone(&seat.config), peer).with_redial(redial);
    let own_reception = GroupReceptionKeyRecord::mint(now.0 as i64 * 1_000);

    // The initiator is a member too, so its own reception keypair rests
    // durably like a joiner's — a later generation minted to this scope wraps
    // to it, and a key held only in this task's stack would be gone by then.
    account
        .put_group_reception_key(own_reception.clone())
        .await
        .map_err(|e| format!("reception key row: {e}"))?;

    let result = initiator
        .drive(
            &keypair,
            &authority.device_key,
            authority.device_authorization,
            &own_reception,
            now,
            &|_status| {},
        )
        .await;

    // Record-then-act: the record is durable state whether or not the walk
    // finished, so flush before reporting either way.
    if let Err(e) = flush(&seat.record, &seat.config).await {
        tracing::warn!("[offline-share] ceremony record not persisted: {e}");
    }
    let driven = result.map_err(|e| e.to_string())?;

    // The two write-throughs the ceremony owes this side's own store. Marking
    // follows the write, never precedes it: a marker set first would tell a
    // resuming driver the scope is readable when its root is not there.
    write_through(
        &account,
        &seat,
        driven.held_root_row,
        driven.built.plane_rows,
        WriteThroughSide::Initiator {
            scope_id: driven.scope_id,
            recipient: peer,
        },
    )
    .await?;

    Ok(CeremonyStatus::Delivered)
}

/// Which side's monotone markers a [`write_through`] pass sets. The two rows
/// are identical; only the bookkeeping differs, and keeping it in one function
/// is what stops the two sides drifting into two orderings.
pub enum WriteThroughSide {
    Initiator {
        scope_id: [u8; 32],
        recipient: ActorId,
    },
    Joiner {
        scope_id: [u8; 32],
    },
}

/// Write the held root row and adopt the machinery rows, marking each step
/// only once its write returned, and flushing the record after both.
///
/// Order matters and is not arbitrary: the **root row first**, because the
/// machinery rows seal under that root — adopting first would produce entries
/// this device could not re-open after a restart if the root write then
/// failed. A failure at either step leaves the step's marker unset, which is
/// exactly what a resuming driver reads to re-drive it.
pub async fn write_through(
    account: &AccountStoreHandle,
    seat: &CeremonySeat,
    held_root_row: fauna_core::group_generation::GroupHeldRootRecord,
    plane_rows: Vec<fauna_core::group_ceremony::GroupPlaneRow>,
    side: WriteThroughSide,
) -> Result<(), String> {
    use fauna_client_capabilities::group_ceremony::{
        mark_group_invited_root_written, mark_group_plane_rows_written,
        mark_group_root_row_written, mark_group_rows_adopted,
    };

    account
        .put_group_held_root(held_root_row.clone())
        .await
        .map_err(|e| format!("held root row: {e}"))?;
    {
        let mut cfg = seat.config.lock().unwrap();
        match &side {
            WriteThroughSide::Initiator {
                scope_id,
                recipient,
            } => mark_group_root_row_written(&mut cfg, scope_id, recipient),
            WriteThroughSide::Joiner { scope_id } => {
                mark_group_invited_root_written(&mut cfg, scope_id)
            }
        }
    }

    let report = account
        .adopt_group_rows(held_root_row, plane_rows)
        .await
        .map_err(|e| format!("machinery rows: {e}"))?;
    if report.refused > 0 {
        // Never benign: the ceremony verifier already refused a snapshot that
        // does not admit us, so a row refused HERE is forged or corrupted.
        // Surfaced rather than swallowed, and the marker stays unset.
        return Err(format!(
            "{} of the shared set's machinery rows were refused",
            report.refused
        ));
    }
    {
        let mut cfg = seat.config.lock().unwrap();
        match &side {
            WriteThroughSide::Initiator {
                scope_id,
                recipient,
            } => mark_group_plane_rows_written(&mut cfg, scope_id, recipient),
            WriteThroughSide::Joiner { scope_id } => mark_group_rows_adopted(&mut cfg, scope_id),
        }
    }

    if let Err(e) = flush(&seat.record, &seat.config).await {
        tracing::warn!("[offline-share] ceremony progress not persisted: {e}");
    }
    Ok(())
}

/// The recipient's **consent**, and everything it owes downstream: mint the
/// reception keypair, rest it durably, record the accept, wait for the
/// delivery the initiator's poll then triggers, admit it, and write the
/// machinery through.
///
/// One act rather than four, because a user pressing Accept is making ONE
/// decision and every step after it is owed unconditionally. The waiting
/// happens inside the op (off the UI thread) under the shared crate's named
/// budget with a deadline poll — never a sleep, and never a spinner the user
/// has to babysit.
///
/// The reception keypair rests BEFORE the accept is recorded, and that
/// ordering is load-bearing: its public half rides the accept, and the
/// initiator seals the admission bundle to it. A crash between the two leaves
/// an unopenable delivery; this way it leaves an unused key.
pub async fn consent(
    seat: Arc<CeremonySeat>,
    account: Option<AccountStoreHandle>,
    keypair: ActorKeypair,
    scope_id: [u8; 32],
) -> Result<CeremonyStatus, String> {
    let account = account.ok_or_else(|| "this device has no account runtime yet".to_string())?;

    let now = Timestamp(now_secs());
    let reception = GroupReceptionKeyRecord::mint(now.0 as i64 * 1_000);
    account
        .put_group_reception_key(reception.clone())
        .await
        .map_err(|e| format!("reception key row: {e}"))?;

    fauna_client_capabilities::group_ceremony_node::consent_to_group_share(
        &seat.config,
        &keypair,
        &reception,
        &scope_id,
        now,
    )
    .map_err(|e| e.to_string())?;
    if let Err(e) = flush(&seat.record, &seat.config).await {
        tracing::warn!("[offline-share] consent not persisted: {e}");
    }

    fauna_client_capabilities::group_ceremony_node::await_delivery(&seat.config, &scope_id)
        .await
        .map_err(|e| e.to_string())?;

    let admitted = fauna_client_capabilities::group_ceremony_node::admit_delivered_share(
        &seat.config,
        &keypair,
        &reception,
        &scope_id,
        Timestamp(now_secs()),
    )
    .map_err(|e| e.to_string())?;

    write_through(
        &account,
        &seat,
        admitted.held_root_row,
        admitted.rows,
        WriteThroughSide::Joiner { scope_id },
    )
    .await?;

    Ok(CeremonyStatus::Admitted)
}

/// Decline an offered share — monotone, fleet-wide, and terminal (rule 6).
pub async fn decline(seat: Arc<CeremonySeat>, scope_id: [u8; 32]) -> Result<(), String> {
    fauna_client_capabilities::group_ceremony_node::decline_group_share(
        &seat.config,
        &scope_id,
        Timestamp(now_secs()),
    );
    flush(&seat.record, &seat.config).await
}

/// Parse the typed counterpart code against this seat's own identity.
pub fn peer_from_input(state: &OfflineShareState) -> Result<ActorId, PeerCodeError> {
    code_from_input(state).map(|c| c.actor)
}

/// The typed code in full — the counterpart AND where to dial them.
///
/// The initiator needs both halves: the actor key is the `NodeId`, and the
/// endpoints are the only addressing a nest-free ceremony ever gets
/// (`p2p.md` § Offline share initiation).
pub fn code_from_input(state: &OfflineShareState) -> Result<PeerCode, PeerCodeError> {
    let own = state.own.ok_or(PeerCodeError::Malformed)?;
    parse_peer_code(&state.peer_code_input, &own)
}

/// The shared sets this device can actually read — the folders page's group
/// listing (`account-data-plane.md` § Implementation status today: the
/// listing read is `group_scope_states`).
///
/// Two steps on purpose. The ceremony record names the CANDIDATE scopes; the
/// store says which of them landed. A scope whose machinery rows never
/// arrived is silently absent rather than listed as an unreadable set — the
/// shared projection's own rule, and the one that keeps a half-finished
/// ceremony from painting a stub.
///
/// Fail-safe empty, the `load_own_tiers` posture: this rides the page's
/// background hydrate, where a failed read must not blank the whole page.
///
/// The ceremony record is the **durable account-store fold** — the seat's
/// lent record ([`SessionSeat::lend_record`]), else `account`'s own handle —
/// joined with the bound seat's replica: local and durable, so a set shared
/// with you last week lists on a cold start and an offer that arrived while
/// the nest was unreachable still paints its consent card (the co-present
/// ceremony's whole case, `p2p.md` § Offline share initiation). The replica
/// is the fallback with the reason the late lend gives it: before the lend,
/// a frame the seat ingested rests only there, and its card must paint; after
/// it, the join means the seat's view never masks what another device
/// recorded. No runtime and no seat → nothing to paint.
///
/// `seat` is the bare [`CeremonySeat`] (tui and linux pass
/// `session_seat.seat()`, the FFI the seat its `FfiCeremonySeat` wraps) and
/// `own` this actor — what tells a scope it minted from one shared with it.
/// The same function serves all seven apps: tui and linux call it directly,
/// the FFI passes it through.
pub async fn load_group_shares(
    account: Option<AccountStoreHandle>,
    seat: Option<&CeremonySeat>,
    own: &ActorId,
) -> GroupShareViews {
    let replica = seat.map(|s| s.config.lock().unwrap().clone());
    let durable = seat.and_then(|s| s.record.lent()).or_else(|| {
        account
            .clone()
            .map(|a| Arc::new(a) as Arc<dyn CeremonyRecord>)
    });
    let cfg = match durable {
        Some(durable) => match durable.load().await {
            Ok(stored) => match &replica {
                Some(replica) => stored.merge(replica),
                None => stored,
            },
            Err(e) => {
                tracing::warn!("[offline-share] ceremony record unreadable: {e}");
                replica.unwrap_or_default()
            }
        },
        None => replica.unwrap_or_default(),
    };
    let invitations =
        fauna_client_capabilities::group_ceremony_view::pending_group_invitations(&cfg)
            .into_iter()
            .map(|i| PendingGroupShareView {
                scope_id: i.scope_id,
                initiator: fauna_core::format::short_id(&i.initiator.to_hex()),
                short_id: i.short_id,
            })
            .collect();

    let Some(account) = account else {
        // No runtime, so nothing can have landed — the invitations still
        // paint, since consenting is exactly what a user with no shared sets
        // yet is here to do.
        return GroupShareViews {
            invitations,
            scopes: Vec::new(),
        };
    };
    let mut scopes = Vec::new();
    for scope_id in fauna_client_capabilities::group_ceremony_view::known_group_scopes(&cfg) {
        match account.group_scope_states(scope_id).await {
            Ok(rows) => {
                if let Some(summary) =
                    crate::group_scope_view::summarize_group_scope(&scope_id, &rows)
                {
                    scopes.push(GroupScopeView {
                        short_id: summary.short_id.clone(),
                        member_count: summary.members.len(),
                        shared_by: (!summary.is_authority(own))
                            .then(|| fauna_core::format::short_id(&summary.authority.to_hex())),
                    });
                }
            }
            Err(e) => tracing::warn!("[offline-share] group scope rows unreadable: {e}"),
        }
    }
    GroupShareViews {
        invitations,
        scopes,
    }
}

/// Both halves of the folders page's group surface, read in one pass because
/// they come from one record and must never disagree about a scope (a set
/// cannot be both awaiting consent and listed).
#[derive(Debug, Clone, Default)]
pub struct GroupShareViews {
    /// Consent cards — the `folder-pending-share` knock list's group arm.
    pub invitations: Vec<PendingGroupShareView>,
    /// Sets this device can actually read.
    pub scopes: Vec<GroupScopeView>,
}

/// One offered set awaiting consent, display-ready.
#[derive(Debug, Clone)]
pub struct PendingGroupShareView {
    /// The accept/decline target — by id, never by row position.
    pub scope_id: [u8; 32],
    /// Who offered it, in the canonical short-id form every other surface
    /// uses for an actor with no handle to hand.
    pub initiator: String,
    /// The nameless set's short scope id.
    pub short_id: String,
}

/// One shared set this device holds the machinery for.
///
/// No `scope_id` field: nothing on this row is addressable yet — a group set
/// has no per-row gesture in v1 — and a carried-but-unread id is exactly the
/// kind of field that later gets addressed by the wrong reader. It comes back
/// with the first gesture that needs it.
/// `PartialEq` is load-bearing on a real render loop, not decoration:
/// linux's GTK pass skips a `folder_list_box` rebuild when a poll's answer
/// is unchanged, and a real rebuild collapses any expanded row (tui's cheap
/// re-render-every-tick `Vec<Element>` diff does not care either way). It is
/// derived here so no app has to re-add it to its own copy — the drift this
/// module retires.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupScopeView {
    /// The short scope id — the set's only name in v1.
    pub short_id: String,
    /// How many verified members the roster carries.
    pub member_count: usize,
    /// `Some(who)` when someone else minted the scope — the recipient's
    /// "Shared by ‹them›" reading. `None` on the initiator's own set.
    pub shared_by: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The record's in-memory stand-in: a join on merge, like the account
    /// store's door, plus a hook that runs between a flush's snapshot and its
    /// write-back (where the serve side can race it).
    #[derive(Default)]
    struct FakeRecord {
        stored: Mutex<GroupShareConfig>,
        on_merge: Mutex<Option<Box<dyn FnMut() + Send>>>,
    }

    impl FakeRecord {
        fn with(cfg: GroupShareConfig) -> Arc<Self> {
            Arc::new(Self {
                stored: Mutex::new(cfg),
                on_merge: Mutex::default(),
            })
        }

        fn current(&self) -> GroupShareConfig {
            self.stored.lock().unwrap().clone()
        }
    }

    impl CeremonyRecord for FakeRecord {
        fn load(&self) -> futures_util::future::BoxFuture<'_, Result<GroupShareConfig, String>> {
            let cfg = self.current();
            Box::pin(async move { Ok(cfg) })
        }

        fn merge(
            &self,
            replica: GroupShareConfig,
        ) -> futures_util::future::BoxFuture<'_, Result<GroupShareConfig, String>> {
            Box::pin(async move {
                if let Some(hook) = self.on_merge.lock().unwrap().as_mut() {
                    hook();
                }
                let mut stored = self.stored.lock().unwrap();
                *stored = stored.merge(&replica);
                Ok(stored.clone())
            })
        }
    }

    fn secret_hex() -> String {
        ActorKeypair::from_secret([13u8; 32])
            .secret_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn init_makes_the_affordance_available_only_with_a_usable_secret() {
        let good = init(&secret_hex());
        assert!(good.own.is_some());
        assert!(
            good.view().available,
            "a usable secret means a compare code"
        );
        assert!(good.view().shows_entry_buttons());

        // A secret this device cannot parse is an unavailable affordance, not
        // a panic and not a blank code the user might read aloud.
        let bad = init("not-a-secret");
        assert!(bad.own.is_none());
        assert!(!bad.view().available);
        assert!(!bad.view().shows_entry_buttons());
        assert!(bad.keypair().is_none());
    }

    /// `record` lent into a fresh slot — what a seat's persist task and the
    /// acts flush through.
    fn lent_slot(record: &Arc<dyn CeremonyRecord>) -> SessionRecord {
        let slot = SessionRecord::default();
        slot.lend(record);
        slot
    }

    /// A recipient's record with the accept recorded and the deliver still
    /// owed — the state `consent` leaves the seat in while `await_delivery`
    /// polls.
    fn consented(scope_id: [u8; 32]) -> fauna_core::group_ceremony::InvitedGroupShare {
        fauna_core::group_ceremony::InvitedGroupShare {
            scope_id,
            initiator: ActorKeypair::from_secret([21u8; 32]).actor_id(),
            offer: vec![1],
            accept: vec![2],
            ..Default::default()
        }
    }

    /// A roster that vouches for nobody — the ceremony precedes membership,
    /// so the plane door's own trait object is irrelevant to a bind.
    struct NoMembers;

    impl fauna_peer_share::admission::SetMembership for NoMembers {
        fn is_member(&self, _channel_id: &[u8; 32], _actor: &ActorId) -> bool {
            false
        }
    }

    fn no_members() -> Arc<dyn SetMembership + Send + Sync> {
        Arc::new(NoMembers)
    }

    /// A peer-witness evaluator holding no group scope.
    struct NoGroups;

    impl GroupRosterState for NoGroups {
        fn authority(
            &self,
            _scope_id: &[u8; 32],
        ) -> Option<fauna_core::group_scope::GroupAuthority> {
            None
        }
        fn is_entry_removed(&self, _scope_id: &[u8; 32], _entry_id: &[u8; 32]) -> bool {
            true
        }
    }

    fn no_group_roster() -> Arc<dyn GroupRosterState + Send + Sync> {
        Arc::new(NoGroups)
    }

    /// A transport factory that records the secret it was handed and reports
    /// one fixed bound address — the two facts the seam carries across it.
    fn recording_transport(
        seen: Arc<Mutex<Vec<[u8; 32]>>>,
        bound: Vec<SocketAddr>,
    ) -> CeremonyTransportFactory {
        Arc::new(move |secret| {
            let seen = Arc::clone(&seen);
            let bound = bound.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(secret);
                Ok(CeremonyBinding {
                    transport: Arc::new(fauna_transport::testing::MemTransport {
                        me: fauna_transport::EndpointKey::from_bytes(
                            secret_hex_keypair().actor_id().0,
                        ),
                        listeners: fauna_transport::testing::listeners(),
                    }),
                    bound_addrs: bound,
                })
            })
        })
    }

    fn secret_hex_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([13u8; 32])
    }

    fn brake_open() -> Option<Vec<String>> {
        Some(vec![
            fauna_client_capabilities::group_ceremony_node::P2P_SHARE_CAPABILITY.to_string(),
        ])
    }

    /// **The transport is keyed to the ACTOR secret** (PT-1b), never to some
    /// other key the caller happened to hold: the in-person code compare is
    /// only meaningful because the NodeId *is* the actor key. This is the one
    /// fact a per-app bind door could silently get wrong, and the reason the
    /// factory takes the secret from the shared side rather than the app's.
    #[tokio::test]
    async fn the_transport_is_keyed_to_the_actor_secret() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (_seat, _bound) = bind_share_plane_seat(
            &SessionSeat::default(),
            &secret_hex(),
            "fauna-test",
            brake_open(),
            &no_members(),
            &no_group_roster(),
            &recording_transport(Arc::clone(&seen), vec![]),
        )
        .await
        .expect("an open brake binds");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one bind assembles one transport");
        assert_eq!(
            seen[0],
            *secret_hex_keypair().secret_bytes(),
            "the factory is handed the ACTOR secret, not another key"
        );
    }

    /// The bound addresses reach BOTH the caller (the publish pump crosses
    /// them with the interface list) and the seat itself (they are the
    /// compare code's addressing half). Before `with_bound_addrs` landed they
    /// were discarded and the counterpart's dial had nothing to aim at, so
    /// both halves are asserted, not just the return value.
    #[tokio::test]
    async fn the_bound_addresses_reach_the_caller_and_the_seat() {
        // Concrete, non-loopback, non-unspecified: passes
        // `lan_socket_addr_candidates` through unchanged on any machine.
        let addr: SocketAddr = "192.168.44.7:41999".parse().unwrap();
        let (seat, bound) = bind_share_plane_seat(
            &SessionSeat::default(),
            &secret_hex(),
            "fauna-test",
            brake_open(),
            &no_members(),
            &no_group_roster(),
            &recording_transport(Arc::default(), vec![addr]),
        )
        .await
        .expect("an open brake binds");

        assert_eq!(bound, vec![addr], "the caller is told where it bound");
        assert!(
            seat.node.local_endpoints().contains(&addr),
            "the seat publishes where it bound: {:?}",
            seat.node.local_endpoints()
        );
    }

    /// A transport factory on a SHARED network, so a test can watch the
    /// listener's accept side come and go.
    fn shared_transport(net: fauna_transport::testing::Listeners) -> CeremonyTransportFactory {
        Arc::new(move |secret| {
            let net = Arc::clone(&net);
            Box::pin(async move {
                Ok(CeremonyBinding {
                    transport: Arc::new(fauna_transport::testing::MemTransport {
                        me: fauna_transport::EndpointKey::from_bytes(
                            ActorKeypair::from_secret(secret).actor_id().0,
                        ),
                        listeners: net,
                    }),
                    bound_addrs: vec![],
                })
            })
        })
    }

    /// `stop_share_plane`'s body (`crate::share_glue::stop_plane`), asserted
    /// on the socket and never on a flag: the plane's driver holds its own
    /// clone of the seat for its whole loop, so the listener ends only when
    /// the driver is gone AND the slot is unbound. A start after the stop
    /// binds again.
    #[tokio::test]
    async fn stopping_the_plane_frees_the_listener_and_a_restart_binds_again() {
        let net = fauna_transport::testing::listeners();
        let node = secret_hex_keypair().actor_id().0;
        let session = SessionSeat::default();
        let accepting = |net: &fauna_transport::testing::Listeners| {
            net.lock()
                .unwrap()
                .get(&node)
                .is_some_and(|tx| !tx.is_closed())
        };

        let (bound, _) = bind_share_plane_seat(
            &session,
            &secret_hex(),
            "fauna-test",
            brake_open(),
            &no_members(),
            &no_group_roster(),
            &shared_transport(Arc::clone(&net)),
        )
        .await
        .expect("the plane binds");
        fauna_transport::testing::await_listening(&net, &node).await;
        assert!(accepting(&net));

        // The driver, as `share_glue::run` holds the seat: its own clone.
        let driver = tokio::spawn(async move {
            let _seat = bound;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;

        crate::share_glue::stop_plane(driver, &session).await;
        assert!(session.seat().is_none(), "the slot is unbound");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while accepting(&net) {
            assert!(
                std::time::Instant::now() < deadline,
                "a stopped plane still holds its listener"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // Back in front: the same start binds a fresh listener.
        let (_again, _) = bind_share_plane_seat(
            &session,
            &secret_hex(),
            "fauna-test",
            brake_open(),
            &no_members(),
            &no_group_roster(),
            &shared_transport(Arc::clone(&net)),
        )
        .await
        .expect("the plane binds again");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !accepting(&net) {
            assert!(
                std::time::Instant::now() < deadline,
                "the restarted plane never listened"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// The device's own switch (`p2p.md` § Per-device participation) at the
    /// seat slot — the one body under both doors: off refuses a bind with
    /// no I/O; `unbind` takes a bound seat down, accept side and all; an
    /// unread verdict admits (the slot's doc owns why) and the driver's
    /// first pass settles it.
    #[tokio::test]
    async fn a_switched_off_device_refuses_both_doors_and_unbind_ends_the_listener() {
        let net = fauna_transport::testing::listeners();
        let node = secret_hex_keypair().actor_id().0;
        let seat = SessionSeat::default();
        assert_eq!(
            seat.participation(),
            None,
            "unread until the driver's first pass"
        );

        // Off: refused before any transport is assembled.
        seat.set_participation(Some(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let err = bind_share_plane_seat(
            &seat,
            &secret_hex(),
            "fauna-test",
            brake_open(),
            &no_members(),
            &no_group_roster(),
            &recording_transport(Arc::clone(&seen), vec![]),
        )
        .await
        .expect_err("off refuses");
        assert_eq!(err, PARTICIPATION_OFF);
        assert!(
            seen.lock().unwrap().is_empty(),
            "no transport was ever assembled"
        );
        assert!(seat.seat().is_none());
        assert!(!seat.unbind().await, "nothing to unbind");

        // On: binds, and the accept side is live on the network.
        seat.set_participation(Some(true));
        let (bound, _) = bind_share_plane_seat(
            &seat,
            &secret_hex(),
            "fauna-test",
            brake_open(),
            &no_members(),
            &no_group_roster(),
            &shared_transport(Arc::clone(&net)),
        )
        .await
        .expect("on binds");
        fauna_transport::testing::await_listening(&net, &node).await;
        assert!(seat.seat().is_some());

        // Off again: the driver drops its clone and unbinds the slot — the
        // last handles — and the accept side ends with them.
        seat.set_participation(Some(false));
        drop(bound);
        assert!(seat.unbind().await, "a seat was bound");
        assert!(seat.seat().is_none());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let closed = net
                .lock()
                .unwrap()
                .get(&node)
                .is_some_and(|tx| tx.is_closed());
            if closed {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the accept side outlived the seat"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // And a fresh bind while off is refused again.
        let err = bind_share_plane_seat(
            &seat,
            &secret_hex(),
            "fauna-test",
            brake_open(),
            &no_members(),
            &no_group_roster(),
            &shared_transport(Arc::clone(&net)),
        )
        .await
        .expect_err("still off");
        assert_eq!(err, PARTICIPATION_OFF);
    }

    /// No evidence at all refuses — never optimism (rule 7). A fresh install
    /// that has never reached its nest does not open a listener.
    #[tokio::test]
    async fn no_brake_evidence_refuses_to_bind() {
        let err = bind_share_plane_seat(
            &SessionSeat::default(),
            &secret_hex(),
            "fauna-test",
            None,
            &no_members(),
            &no_group_roster(),
            &recording_transport(Arc::default(), vec![]),
        )
        .await
        .expect_err("no evidence must refuse");
        assert!(
            err.contains("optimism"),
            "the refusal names why, for the page's error-message: {err}"
        );
    }

    /// An advertisement that simply lacks the capability is the brake being
    /// ON — a different refusal from having no evidence, and the user is owed
    /// the difference.
    #[tokio::test]
    async fn an_advertisement_without_the_capability_reads_as_the_brake() {
        let err = bind_share_plane_seat(
            &SessionSeat::default(),
            &secret_hex(),
            "fauna-test",
            Some(vec!["some-other-capability".to_string()]),
            &no_members(),
            &no_group_roster(),
            &recording_transport(Arc::default(), vec![]),
        )
        .await
        .expect_err("a closed brake must refuse");
        assert!(
            err.contains("brake"),
            "the refusal names the brake, not a missing cache: {err}"
        );
        assert!(
            !err.contains("optimism"),
            "the brake refusal is NOT the no-evidence one: {err}"
        );
    }

    /// A secret this device cannot parse fails before anything is assembled —
    /// no transport, no listener, no config read.
    #[tokio::test]
    async fn an_unparseable_secret_assembles_nothing() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let err = bind_share_plane_seat(
            &SessionSeat::default(),
            "not-a-secret",
            "fauna-test",
            brake_open(),
            &no_members(),
            &no_group_roster(),
            &recording_transport(Arc::clone(&seen), vec![]),
        )
        .await
        .expect_err("a malformed secret must refuse");
        assert!(err.contains("invalid secret"), "{err}");
        assert!(
            seen.lock().unwrap().is_empty(),
            "no endpoint is assembled for a secret we cannot key it with"
        );
    }

    /// The set a scripted roster speaks for.
    const SET: [u8; 32] = [0x5E; 32];

    /// A roster admitting exactly one actor to [`SET`].
    struct Admits(ActorId);

    impl SetMembership for Admits {
        fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
            channel_id == &SET && *actor == self.0
        }
    }

    fn member() -> ActorId {
        ActorKeypair::from_secret([41u8; 32]).actor_id()
    }

    fn admitting_member() -> Arc<dyn SetMembership + Send + Sync> {
        Arc::new(Admits(member()))
    }

    /// Every endpoint a factory assembled, held weakly: a live one is a
    /// listener some seat still holds.
    type Assembled = Arc<Mutex<Vec<Weak<fauna_transport::testing::MemTransport>>>>;

    /// A transport factory that counts LISTENERS, not calls — [`live`] reads
    /// how many of its endpoints a seat still holds. It yields before it
    /// builds, so two doors racing into a bind genuinely interleave there.
    fn counting_transport(assembled: &Assembled) -> CeremonyTransportFactory {
        let assembled = Arc::clone(assembled);
        Arc::new(move |_secret| {
            let assembled = Arc::clone(&assembled);
            Box::pin(async move {
                tokio::task::yield_now().await;
                let transport = Arc::new(fauna_transport::testing::MemTransport {
                    me: fauna_transport::EndpointKey::from_bytes(secret_hex_keypair().actor_id().0),
                    listeners: fauna_transport::testing::listeners(),
                });
                assembled.lock().unwrap().push(Arc::downgrade(&transport));
                Ok(CeremonyBinding {
                    transport,
                    bound_addrs: vec![],
                })
            })
        })
    }

    fn live(assembled: &Assembled) -> usize {
        assembled
            .lock()
            .unwrap()
            .iter()
            .filter(|endpoint| endpoint.strong_count() > 0)
            .count()
    }

    /// The panel door's bind with its live brake read stood in: that read is
    /// the only thing [`bind_seat`] adds to the slot, and it needs a nest.
    async fn panel_bind(
        session: &SessionSeat,
        assembled: &Assembled,
    ) -> Result<Arc<CeremonySeat>, String> {
        let (seat, _bound) = session
            .get_or_bind(
                &secret_hex(),
                "fauna-test",
                async { brake_open() },
                &counting_transport(assembled),
            )
            .await?;
        Ok(seat)
    }

    async fn plane_bind(
        session: &SessionSeat,
        assembled: &Assembled,
        roster: &Arc<dyn SetMembership + Send + Sync>,
    ) -> Result<Arc<CeremonySeat>, String> {
        let transport = counting_transport(assembled);
        let bound = bind_share_plane_seat(
            session,
            &secret_hex(),
            "fauna-test",
            brake_open(),
            roster,
            &no_group_roster(),
            &transport,
        )
        .await?;
        Ok(bound.0)
    }

    /// Evidence no door may read once the session's seat is bound.
    fn brake_must_not_be_read() -> Option<Vec<String>> {
        panic!("a bound session seat must be handed back without reading the brake again")
    }

    /// **Panel first: the share plane's door is handed the panel's seat, and
    /// the ceremony in flight on it completes.** The recipient's consent is
    /// waiting out the deliver on the seat's record when the plane's driver
    /// asks for its seat; the deliver recorded through the seat the plane got
    /// back is the one that wait sees. One seat, one record, one listener —
    /// under either per-app fold this replaced, the driver's own bind had
    /// already opened a second.
    #[tokio::test(start_paused = true)]
    async fn panel_first_the_plane_reuses_the_seat_a_ceremony_is_in_flight_on() {
        let assembled = Assembled::default();
        let session = SessionSeat::default();
        let panel = panel_bind(&session, &assembled)
            .await
            .expect("the panel binds");

        let scope_id = [0x5C; 32];
        panel
            .config
            .lock()
            .unwrap()
            .invited
            .push(consented(scope_id));

        let plane_asks = async {
            let plane = plane_bind(&session, &assembled, &admitting_member())
                .await
                .expect("the plane's door is handed the session's seat");
            assert!(
                Arc::ptr_eq(&panel, &plane),
                "the plane's door must hand back the seat the panel's ceremony runs on"
            );
            assert_eq!(
                live(&assembled),
                1,
                "one listener on this NodeId, never two"
            );
            plane.config.lock().unwrap().invited[0].deliver = vec![3];
        };
        let (waited, ()) = tokio::join!(
            fauna_client_capabilities::group_ceremony_node::await_delivery(
                &panel.config,
                &scope_id
            ),
            plane_asks,
        );
        waited.expect("the ceremony in flight on the panel's seat sees the deliver");
    }

    /// **Plane first: the panel's door is handed the plane's seat** — without
    /// reading the brake again (what the panel's old `existing` arm did, now
    /// the slot's), and with the plane's roster still lent to it.
    #[tokio::test]
    async fn plane_first_the_panel_reuses_the_plane_seat() {
        let assembled = Assembled::default();
        let session = SessionSeat::default();
        let roster = admitting_member();
        let plane = plane_bind(&session, &assembled, &roster)
            .await
            .expect("the plane binds");

        let (panel, _bound) = session
            .get_or_bind(
                &secret_hex(),
                "fauna-test",
                async { brake_must_not_be_read() },
                &counting_transport(&assembled),
            )
            .await
            .expect("the panel's door is handed the session's seat");

        assert!(Arc::ptr_eq(&plane, &panel), "one seat for both doors");
        assert_eq!(
            live(&assembled),
            1,
            "one listener on this NodeId, never two"
        );
        assert!(
            session.0.roster.is_member(&SET, &member()),
            "the panel reusing the seat leaves the plane's roster lent"
        );
    }

    /// **Doors racing into a bind open one listener.** Both ask at once — the
    /// transport yields mid-assembly, so they genuinely interleave — and both
    /// are handed the same seat: the bind runs inside the slot, not behind a
    /// check each door makes against its own copy.
    #[tokio::test]
    async fn doors_racing_into_a_bind_open_one_listener() {
        let assembled = Assembled::default();
        let session = SessionSeat::default();
        let roster = admitting_member();
        let (panel, plane) = tokio::join!(
            panel_bind(&session, &assembled),
            plane_bind(&session, &assembled, &roster),
        );
        let (panel, plane) = (
            panel.expect("the panel's door binds or is handed the seat"),
            plane.expect("the plane's door binds or is handed the seat"),
        );
        assert!(Arc::ptr_eq(&panel, &plane), "one seat for both doors");
        assert_eq!(
            live(&assembled),
            1,
            "one listener on this NodeId, never two"
        );
    }

    /// **The plane's roster rides the seat only while its driver holds it.** A
    /// seat the panel bound admits nobody; the plane's door lends it the M2
    /// consult; when the driver lets go — its loop ended with the account
    /// runtime — the seat admits nobody again, and never held the roster (or
    /// the MLS engine behind it) alive.
    #[tokio::test]
    async fn the_plane_roster_admits_only_while_the_driver_holds_it() {
        let assembled = Assembled::default();
        let session = SessionSeat::default();
        panel_bind(&session, &assembled)
            .await
            .expect("the panel binds");
        assert!(
            !session.0.roster.is_member(&SET, &member()),
            "a panel-bound seat admits nobody to any set"
        );

        let roster = admitting_member();
        plane_bind(&session, &assembled, &roster)
            .await
            .expect("the plane is handed the seat");
        assert!(
            session.0.roster.is_member(&SET, &member()),
            "the lent roster admits its member"
        );

        let held = Arc::downgrade(&roster);
        drop(roster);
        assert!(
            held.upgrade().is_none(),
            "the seat keeps no strong hold on the driver's roster"
        );
        assert!(
            !session.0.roster.is_member(&SET, &member()),
            "once the driver lets go, the seat admits nobody again"
        );
    }

    /// **The peer witness door's evaluator is lent exactly like the M2
    /// roster.** A panel-bound seat holds no group scope; the plane's bind
    /// lends the driver's pump-fed evaluator, which the seat then answers
    /// from; when the driver lets go the seat holds none again, and never
    /// kept the evaluator alive.
    #[tokio::test]
    async fn the_plane_group_roster_is_held_only_while_the_driver_holds_it() {
        const SCOPE: [u8; 32] = [0x6A; 32];
        struct HoldsScope;
        impl GroupRosterState for HoldsScope {
            fn authority(
                &self,
                scope_id: &[u8; 32],
            ) -> Option<fauna_core::group_scope::GroupAuthority> {
                (*scope_id == SCOPE).then(|| {
                    fauna_core::group_scope::GroupAuthority::build(
                        scope_id,
                        &member(),
                        &[],
                        std::iter::empty(),
                    )
                })
            }
            fn is_entry_removed(&self, scope_id: &[u8; 32], entry_id: &[u8; 32]) -> bool {
                *scope_id != SCOPE || *entry_id == [0xEE; 32]
            }
        }

        let assembled = Assembled::default();
        let session = SessionSeat::default();
        panel_bind(&session, &assembled)
            .await
            .expect("the panel binds");
        assert!(
            session.0.group_roster.authority(&SCOPE).is_none(),
            "a panel-bound seat holds no group scope"
        );

        let group_roster: Arc<dyn GroupRosterState + Send + Sync> = Arc::new(HoldsScope);
        bind_share_plane_seat(
            &session,
            &secret_hex(),
            "fauna-test",
            brake_open(),
            &no_members(),
            &group_roster,
            &counting_transport(&assembled),
        )
        .await
        .expect("the plane is handed the seat");
        assert!(
            session.0.group_roster.authority(&SCOPE).is_some(),
            "the lent evaluator's scope is held"
        );
        assert!(session.0.group_roster.is_entry_removed(&SCOPE, &[0xEE; 32]));

        let held = Arc::downgrade(&group_roster);
        drop(group_roster);
        assert!(
            held.upgrade().is_none(),
            "the seat keeps no strong hold on the driver's evaluator"
        );
        assert!(
            session.0.group_roster.authority(&SCOPE).is_none(),
            "once the driver lets go, the seat holds no group scope again"
        );
        assert!(
            session.0.group_roster.is_entry_removed(&SCOPE, &[0x01; 32]),
            "nothing lent reads as removed, so the serve re-consult severs"
        );
    }

    /// **A refused bind leaves the slot unbound and lends nothing.** The plane
    /// asking with no evidence opens no listener and lends no roster, so the
    /// seat the panel binds next admits nobody — the posture a panel had
    /// before the plane ever asked.
    #[tokio::test]
    async fn a_refused_bind_leaves_the_session_seat_unbound_and_lends_nothing() {
        let assembled = Assembled::default();
        let session = SessionSeat::default();
        let roster = admitting_member();
        let transport = counting_transport(&assembled);
        let refused = bind_share_plane_seat(
            &session,
            &secret_hex(),
            "fauna-test",
            None,
            &roster,
            &no_group_roster(),
            &transport,
        )
        .await;
        let err = refused.expect_err("no evidence refuses");
        assert!(err.contains("optimism"), "{err}");
        assert!(
            session.seat().is_none(),
            "a refusal binds nothing into the slot"
        );
        assert_eq!(live(&assembled), 0, "and leaves nothing listening");

        panel_bind(&session, &assembled)
            .await
            .expect("the panel may still bind");
        assert_eq!(live(&assembled), 1);
        assert!(
            !session.0.roster.is_member(&SET, &member()),
            "a refused plane bind lent no roster to the seat the panel bound later"
        );
    }

    /// **A deliver ingested while a flush is saving survives the flush** — the
    /// co-present journey's red, pinned (`p2p.md` § Offline share initiation).
    ///
    /// The merge hook plays the serve side: it records the deliver exactly as
    /// `ingest_group_frame`'s deliver arm does, between the flush's snapshot
    /// and its write-back. A write-back that replaced the replica erased it,
    /// and the recipient's `await_delivery` then polled a record that would
    /// never carry it again. Paused time bounds that wait if this regresses.
    #[tokio::test(start_paused = true)]
    async fn a_deliver_ingested_while_a_flush_is_saving_survives_the_flush() {
        let scope_id = [0x5C; 32];
        let mut cfg = GroupShareConfig::default();
        cfg.invited.push(consented(scope_id));
        let fake = FakeRecord::with(cfg.clone());
        let replica = Arc::new(Mutex::new(cfg));
        {
            let replica = Arc::clone(&replica);
            *fake.on_merge.lock().unwrap() = Some(Box::new(move || {
                let mut cfg = replica.lock().unwrap();
                let record = &mut cfg.invited[0];
                if record.deliver.is_empty() {
                    record.deliver = vec![3];
                }
            }));
        }
        let record: Arc<dyn CeremonyRecord> = Arc::clone(&fake) as _;
        let record = lent_slot(&record);

        flush(&record, &replica).await.expect("the flush lands");
        assert_eq!(
            replica.lock().unwrap().invited[0].deliver,
            vec![3],
            "the deliver the serve side recorded mid-save must still be on the seat's record"
        );
        fauna_client_capabilities::group_ceremony_node::await_delivery(&replica, &scope_id)
            .await
            .expect("the deliver is on record, so the recipient's wait is over");

        flush(&record, &replica)
            .await
            .expect("the next flush lands");
        assert_eq!(
            fake.current().invited[0].deliver,
            vec![3],
            "the next flush persists the deliver, never its loss"
        );
    }

    /// **A flush never puts the seat's stale copy over what the store gained
    /// since the seat loaded.** The seat loads its record once, at bind; a
    /// record another writer stored after that must survive this seat's next
    /// flush, and reach the seat's own replica too.
    #[tokio::test]
    async fn a_flush_keeps_what_the_store_gained_since_the_seat_loaded() {
        let scope_id = [0x5C; 32];
        let mut loaded = GroupShareConfig::default();
        loaded.invited.push(consented(scope_id));
        let fake = FakeRecord::with(loaded.clone());
        let replica = Arc::new(Mutex::new(loaded));

        let elsewhere = [0x7A; 32];
        fake.stored.lock().unwrap().initiated.push(
            fauna_core::group_ceremony::InitiatedGroupShare {
                scope_id: elsewhere,
                recipient: ActorKeypair::from_secret([31u8; 32]).actor_id(),
                offer: vec![9],
                ..Default::default()
            },
        );
        replica.lock().unwrap().invited[0].accept_posted = true;

        let record: Arc<dyn CeremonyRecord> = Arc::clone(&fake) as _;
        let slot = lent_slot(&record);
        flush(&slot, &replica).await.expect("the flush lands");

        let stored = fake.current();
        assert!(
            stored.initiated.iter().any(|r| r.scope_id == elsewhere),
            "the other writer's record survives this seat's flush"
        );
        assert!(
            stored.invited[0].accept_posted,
            "and this seat's own progress landed with it"
        );
        assert!(
            replica
                .lock()
                .unwrap()
                .initiated
                .iter()
                .any(|r| r.scope_id == elsewhere),
            "the seat's replica learns the other writer's record too"
        );
    }

    /// An offer as the serve side's ingest records it on the recipient's seat.
    fn offered(scope_id: [u8; 32]) -> fauna_core::group_ceremony::InvitedGroupShare {
        fauna_core::group_ceremony::InvitedGroupShare {
            scope_id,
            initiator: ActorKeypair::from_secret([21u8; 32]).actor_id(),
            offer: vec![1],
            ..Default::default()
        }
    }

    /// **(a) Bound before the lend, the seat's replica reaches the store AT
    /// the lend** — the panel opened before store-ready, an offer ingested,
    /// then the runtime lends the record: the lend's own flush lands it.
    #[tokio::test]
    async fn a_seat_bound_before_the_lend_lands_its_replica_at_the_lend() {
        let assembled = Assembled::default();
        let session = SessionSeat::default();
        let seat = panel_bind(&session, &assembled)
            .await
            .expect("the panel binds with nothing lent");
        assert!(seat.record.lent().is_none());
        let scope_id = [0x5A; 32];
        seat.config.lock().unwrap().invited.push(offered(scope_id));
        // Unlent, a flush is a no-op the lend replays — never an error.
        flush(&seat.record, &seat.config)
            .await
            .expect("an unlent flush is a no-op");

        let fake = FakeRecord::with(GroupShareConfig::default());
        let record: Arc<dyn CeremonyRecord> = Arc::clone(&fake) as _;
        session
            .lend_record(&record)
            .await
            .expect("the lend flushes");
        assert_eq!(
            fake.current().invited,
            vec![offered(scope_id)],
            "the offer ingested before the lend rests in the store"
        );
    }

    /// **(b) Lent before the bind, the seat's replica IS the resting record.**
    #[tokio::test]
    async fn a_seat_bound_after_the_lend_starts_on_the_resting_record() {
        let scope_id = [0x5B; 32];
        let mut resting = GroupShareConfig::default();
        resting.invited.push(offered(scope_id));
        let fake = FakeRecord::with(resting.clone());
        let record: Arc<dyn CeremonyRecord> = Arc::clone(&fake) as _;
        let session = SessionSeat::default();
        session
            .lend_record(&record)
            .await
            .expect("nothing to flush");

        let seat = panel_bind(&session, &Assembled::default())
            .await
            .expect("the panel binds");
        assert_eq!(*seat.config.lock().unwrap(), resting);

        // And the lend is Weak: once the host lets go, the seat holds nothing.
        let held = Arc::downgrade(&record);
        drop(record);
        drop(fake);
        assert!(held.upgrade().is_none(), "the seat keeps no strong hold");
        assert!(seat.record.lent().is_none());
    }

    /// **(c) The page read: an unlent seat answers its replica; a lent seat
    /// the durable record joined with the replica.**
    #[tokio::test]
    async fn the_page_read_is_the_replica_until_the_lend_then_the_join() {
        let own = secret_hex_keypair().actor_id();
        let session = SessionSeat::default();
        let seat = panel_bind(&session, &Assembled::default())
            .await
            .expect("the panel binds");
        let on_seat = [0x5D; 32];
        seat.config.lock().unwrap().invited.push(offered(on_seat));

        let unlent = load_group_shares(None, Some(&seat), &own).await;
        assert_eq!(
            unlent
                .invitations
                .iter()
                .map(|i| i.scope_id)
                .collect::<Vec<_>>(),
            vec![on_seat],
            "unlent: the replica's card paints"
        );

        let elsewhere = [0x5E; 32];
        let mut resting = GroupShareConfig::default();
        resting.invited.push(offered(elsewhere));
        let fake = FakeRecord::with(resting);
        let record: Arc<dyn CeremonyRecord> = Arc::clone(&fake) as _;
        // Lent without the lend-time flush, so the join is the read's own.
        session.0.record.lend(&record);
        let lent = load_group_shares(None, Some(&seat), &own).await;
        let mut scopes: Vec<_> = lent.invitations.iter().map(|i| i.scope_id).collect();
        scopes.sort();
        assert_eq!(
            scopes,
            vec![on_seat, elsewhere],
            "lent: the durable record joined with the replica"
        );
    }
}
