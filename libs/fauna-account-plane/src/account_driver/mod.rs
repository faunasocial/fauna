//! The account data plane's **driver** — the platform-generic half of the
//! client-side account-store lifecycle (`account-data-plane.md` § The account
//! store → *The client-side lifecycle*; ruling (2) of *The trigger fired*).
//!
//! # The line this crate draws
//!
//! Ruling (2) splits the pump in two. **This module is the driver**: the
//! command service ([`AccountStoreHandle`] and the store-thread side that
//! serves a local command at a pass's yield point — `handle`), the pass and
//! its report ([`PumpReport`] — `pass`), the enrollment legs and the fleet
//! bootstrap (`enrollment`), the pass driver that runs pump work beside the
//! command channel (`drive`), and the serve loop itself
//! ([`AccountDriver::serve`]): the prologue, the four wake sources, the
//! publish step, the reassembly verdicts, the sign-out cut. Nothing here
//! names a thread, a runtime, a file, a clock or a network stack beyond the
//! `RpcRequester` seam — the crate compiles for wasm32 and a textual test pins
//! `std::thread`, `tokio::time`, `Instant` and `SystemTime` out of it.
//!
//! **The host is everything else**, behind four seams:
//!
//! - [`Assembly`] — what the host resolved before the driver runs: the open
//!   store, the writer key, the backup key, the principal, the credential slot
//!   ([`PrincipalCustody`] — the slot seam) and the two requesters.
//! - [`HostLegs`] — the legs the pass runs between the fleet walk and the
//!   device-endpoints step: natively the peer leg and the custody leg
//!   (`fauna_sync_engine::account_runtime`), on web [`NoLegs`].
//! - [`EngineElection`] — the engine-singleton re-try (`flock` natively, Web
//!   Locks on web), with the initial election ([`elect_at_start`]) run by the
//!   host inside its readiness barrier.
//! - Time — [`fauna_sleep::sleep`] for the backstop ticker, the sign-out grace
//!   and the retirement budget; the wall clock ([`now_ms`]) for the pass
//!   timings. `tokio::sync` is fine (channels build for wasm32); `tokio::time`
//!   is not.
//!
//! The native host (`AccountStoreRuntime::start`) spawns the store thread and
//! its current-thread runtime, resolves the T10 credential slot, runs the
//! succession probe and the lost-slot heal, opens the SQLite store, builds the
//! legs and calls [`AccountDriver::serve`] once per assembly; web hosts the
//! same driver as a `spawn_local` task in the SPA's process over the
//! IndexedDB store (`account-runtime.md` § Multi-instance concurrency). One
//! [`AccountDriver`] outlives every reassembly — the app-fed state, the parked
//! commands and the heal caps live on it — and one [`AccountStoreHandle`] is
//! minted with it, before the first assembly.
//!
//! # The pump
//!
//! A prologue runs to completion before the first wait (the shipped
//! receive-loop discipline): `publish_pending` → walk. Then
//! one biased select over, in order: the command channel (reads, writes,
//! shutdown — FIFO), the coalescing nudge channel (drained and de-duplicated
//! before walking, so a burst of nudges for one scope costs one walk — fed
//! by the pump's own push arm, the session's push stream mapped through
//! [`nudge_scope_for_push`], so no app hand-wires one), the reconnect watch
//! (push `seq` resets on reconnect, so re-publish and re-walk), and the
//! backstop ticker (correctness never depends on the nudge) — plus the
//! local-write wake: a write through the handle is the local row only, and
//! arms a publish step (both planes' `publish_pending`) the
//! loop runs as soon as no pass is in flight. Each pump pass is
//! panic-contained (`futures_util::FutureExt::catch_unwind`) — a poisoned
//! pass is logged and the loop lives on.
//!
//! **A pass never holds a command hostage** (`account-data-plane.md` § The
//! client-side lifecycle, the pump bullet → *Commands and passes*; measured
//! 2026-09-22: a prologue is a full catch-up on a fresh replica and ran past
//! five minutes under a 33-device account, every preference surface empty
//! behind it). Every pass — the prologue, a nudge walk, the publish step,
//! the backstop, reconnect and `reconcile_now` passes — is DRIVEN beside the
//! command channel (`drive::drive_pass`): a **local** command (`Cmd::is_local`
//! — store reads, principal-slot reads, the local half of every write, an
//! intent enqueue, a scope registration, the endpoint facts) is served at the
//! pass's next yield point on this same task and connection, and a
//! **pass-bound** one (`reconcile_now`, the sign-out's retirement, a
//! shutdown, the barrier — and a tip-sealed door put whose door would mint)
//! is parked and served after the pass in arrival order. Every unit of
//! local work inside a pass ends in a yield (`pass_breath`), so a stretch
//! with no network parks a command — and the sign-out cut — by at most one
//! unit. The explicit barrier is [`AccountStoreHandle::settled`].
//!
//! Every pump step is attempted even when an earlier one failed: an offline
//! launch must still serve local reads, and each step's failure is retried by
//! the next tick anyway. [`PumpReport::errors`] carries what failed.
//!
//! # The engine-singleton election
//!
//! Several same-account processes may share one store (`account-runtime.md`
//! § Multi-instance concurrency); exactly one — the election's holder — runs
//! the pump. The host takes the election at assembly ([`elect_at_start`]
//! over its [`EngineElection`]): the holder pumps exactly as above; a
//! non-holder serves every command — reads, a preference put and its publish
//! step, intent enqueue — but runs no pass, dropping nudges and reconnect
//! wakes, and re-tries the election on the backstop tick (and on an explicit
//! `reconcile_now`) so the role transfers when the holder exits.
//! [`PumpReport::skipped_non_holder`] is the in-band role answer. The MLS
//! drain carve-out is unaffected: whichever process holds the role,
//! `IntentDrainer::Mls` intents are never its leg to send
//! (`outbox::drain_outbox` owns that predicate).
//!
//! # The seed-leg role
//!
//! The engine role elects who pumps, and on a desktop the process that wins it
//! in steady state — the sync agent — holds no seed. Two steps of the pass
//! need what only a signed-in app holds: escrow recovery, and the secondary
//! leg with the custody arm riding it. So a second lock on the same seam
//! ([`EngineElection::try_acquire_seed_legs`]) elects one co-located
//! **seed-holding** runtime per store (`account-runtime.md` § Multi-instance
//! concurrency → *The seed-leg role*): [`AccountDriver::serve`] takes it at
//! assembly, right behind the host's engine election, and re-tries it wherever
//! that election is re-tried — degrade open at start, closed on a later try. A
//! seedless runtime never asks. The holder of both roles runs the steps inside
//! its pass; a seed-leg holder that is not the engine holder runs the **seed
//! pass** (`pass::seed_pass`) at every wake on which an engine holder runs a
//! full pass — the first after assembly, the backstop tick, a reconnect,
//! `reconcile_now` — and never per nudge; an engine holder without the role
//! skips them. The seed pass is driven and contained like any pass, moves no
//! pass counter, and reports `skipped_non_holder` with its own slots filled.

mod drive;
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub mod e2e_readers;
mod enrollment;
mod handle;
mod handle_source;
mod pass;
mod stop;

pub use drive::SIGN_OUT_PASS_GRACE;
pub use enrollment::{
    ENROLLMENT_RETIRE_BUDGET, EnrollmentPass, EnrollmentRetirement, FleetBootstrapRows,
    fleet_bootstrap, fleet_bootstrap_rows, r14_trust,
};
pub use handle::{
    AccountStoreHandle, GroupCeremonyAuthority, LedgerIdentity, PumpCycles, PumpCyclesView,
    RUNTIME_GONE,
};
pub use handle_source::{
    ACCOUNT_HANDLE_WAIT, AccountHandleSource, AccountMailStore, AccountStoreAccess,
    FIRST_PASS_WAIT, NotReadyReason, RUNTIME_ABSENT, ScopeNotReady, SeatAccountStore,
    first_listing_gate, not_ready_reason, not_ready_reason_key, read_gate, wait_for_account_handle,
};
pub use pass::{PassTimings, PumpReport, push_step_error};
pub use stop::{ACCOUNT_RUNTIME_STOP_BUDGET, StopReason, stop_one};

pub use crate::host_legs::{ElectionOutcome, EngineElection};
pub use crate::principal_custody::{EnrollmentRefusal, PrincipalCustody};

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ed25519_dalek::SigningKey;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_core::MaybeSendSync;
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};
use fauna_protocol::scope::ContentScope;
use fauna_protocol::{KeyedRpcRequester, PushEvent, RpcErrorClass, RpcRequester};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::account_state_plane::{AccountStatePlane, FirstListings};
use crate::attested_predecessors::AttestedPredecessors;
use crate::device_endpoints_writer::EndpointFacts;
use crate::generation_escrow_recover::EscrowRecoveryMemo;
use crate::generation_tip::GenerationTrust;
use crate::scope_set::derive_content_scopes;

use drive::{Drive, Driven, SignOutWatch, contained_pump, drive_pass};
use enrollment::retire_enrollment;
use handle::{Cmd, LocalCtx, Served, serve_local_cmd};
use pass::{
    FleetWriter, PassInputs, Planes, enrollment_healthy, log_pump, publish_step, pump,
    reassembly_reason, seed_pass, walk_found_burnt, walk_one_scope,
};

/// The wall clock in milliseconds — `fauna_core::data::Timestamp`'s door, the
/// one clock a crate that reaches wasm may read (`build-system.md` § Wall-clock
/// reads in a crate that can reach wasm). The driver's timings and the
/// sign-out grace are stated against it; nothing here needs a monotonic
/// instant.
pub(crate) fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis_or_zero()
}

/// Backstop reconcile cadence — generous by design (the nudge is the latency
/// path; this is correctness). A constant, not a knob: no human chooses it
/// (`docs/goal/principles.md` — the two-bucket configuration rule).
pub const DEFAULT_BACKSTOP_INTERVAL: Duration = Duration::from_secs(300);

/// The first and the ceiling interval at which a **non-holder** re-reads the
/// shared slot's registration latch while its store principal's connect gate
/// is still closed ([`AccountStoreHandle::subscribe_grant_registered`]). A
/// non-holder runs no pass, so the latch the holder writes is its only news,
/// and the backstop's five minutes is far too long to hold a data path shut.
/// A local credential read, never a dial; the same 500 ms-to-30 s shape the
/// ungated connect retry paced its dials on.
const GRANT_LATCH_POLL: (Duration, Duration) =
    (Duration::from_millis(500), Duration::from_secs(30));

/// Open the store principal's connect gate — once; it never closes again.
fn open_grant_gate(gate: &watch::Sender<bool>) {
    gate.send_if_modified(|registered| !std::mem::replace(registered, true));
}

// `allow`, not a `Box`: this is assembled ONCE per runtime, moved into
// `AccountRuntimeConfig`, and never travels a message chain — so the 264-byte
// enum costs one move on a cold path, which is the case the lint is not aimed
// at (the `UiMessage::Data` precedent in `apps/fauna-tui/src/app.rs`). Boxing
// would also put an `ActorKeypair` on the heap: the move leaves the secret's
// stack bytes behind unzeroed, so it is a small secret-hygiene regression paid
// for a lint rather than a hot path. Where boxing IS right here — a snapshot
// riding an `Outcome`/`DataMessage` chain — the tui does it, with the same
// reason stated at the variant.
#[allow(clippy::large_enum_variant)]
pub enum RuntimePrincipal {
    /// A seed-holding surface ("authoring surfaces hold the seed exactly as
    /// today") — every one of the 7 apps. The assembly derives the `BackupKey`,
    /// mints the enrollment grant when the slot has none, and publishes the
    /// fleet bootstrap rows, all of which need the seed.
    SeedHolding(SeedHolder),
    /// **No identity seed in this process** — the sync agent hosting the store
    /// as the always-on engine singleton. Everything the assembly needs
    /// comes from the shared credential slot a signed-in app populated at
    /// enrollment (T10): the writer key it already resolved there, and the
    /// `BackupKey` the slot persists *for this consumer*.
    ///
    /// The three seed-only legs are **skipped, not faked** — grant mint,
    /// fleet bootstrap, and succession re-key each belong to a
    /// signed-in app and heal on its next assembly. Assembly fails outright if
    /// the slot carries no `BackupKey`, because a host that guessed one would
    /// write ciphertext no app in the fleet could open.
    Seedless,
}

impl RuntimePrincipal {
    /// The keypair when this process holds it — `None` for [`Self::Seedless`].
    pub fn keypair(&self) -> Option<&ActorKeypair> {
        match self {
            Self::SeedHolding(holder) => Some(&holder.keypair),
            Self::Seedless => None,
        }
    }

    /// The attested predecessors' keypairs when this process holds the seed
    /// ([`SeedHolder::predecessors`]) — empty for [`Self::Seedless`].
    pub fn predecessor_keypairs(&self) -> &[ActorKeypair] {
        match self {
            Self::SeedHolding(holder) => &holder.predecessors,
            Self::Seedless => &[],
        }
    }
}

/// What a seed-holding principal holds: this identity's keypair and, for the
/// kept wrap's recovery (`owner-key-material.md` § Path A-sibling-2 →
/// *Rotation*, the succession rider → *The kept wrap*), the keypair of each
/// succeeded-from identity whose seed this device holds, nearest hop first.
///
/// The predecessor seeds ride HERE, beside the principal's own seed, and
/// nowhere else: never in [`AttestedPredecessors`] (cloned into hosts, and
/// open-only by design — it holds no root secret), and never to the seedless
/// agent, which by type holds no [`SeedHolder`]. Two consumers read them: the
/// escrow-recovery pass, which opens a predecessor-targeted wrap under the
/// retired identity's escrow secret, and the road's chain replay, which signs
/// in at an owed nest as the retired identity — the host hands its deliverer a
/// copy ([`crate::owed_delivery::PredecessorSeeds::of`]) before the principal
/// moves into the runtime.
pub struct SeedHolder {
    keypair: ActorKeypair,
    predecessors: Vec<ActorKeypair>,
}

impl SeedHolder {
    /// This identity, holding no predecessor's seed — every identity that
    /// never succeeded.
    #[must_use]
    pub fn new(keypair: ActorKeypair) -> Self {
        Self {
            keypair,
            predecessors: Vec::new(),
        }
    }

    /// Hold `predecessors`' seeds beside this identity's.
    #[must_use]
    pub fn with_predecessors(mut self, predecessors: Vec<ActorKeypair>) -> Self {
        self.predecessors = predecessors;
        self
    }

    /// `keypair`, with the seeds of the succeeded-from identities its
    /// registry rows attest — the same walk
    /// [`AttestedPredecessors::from_registry`] resolves
    /// (`AccountRegistry::predecessor_seeds`). The constructor every
    /// registry-holding app host uses.
    #[must_use]
    pub fn from_registry(
        keypair: ActorKeypair,
        registry: &fauna_client_accounts::AccountRegistry,
    ) -> Self {
        let predecessors = registry
            .predecessor_seeds(&keypair.actor_id_hex())
            .into_iter()
            .map(|(_, seed)| ActorKeypair::from_secret(seed))
            .collect();
        Self::new(keypair).with_predecessors(predecessors)
    }

    /// This identity's keypair.
    #[must_use]
    pub fn keypair(&self) -> &ActorKeypair {
        &self.keypair
    }

    /// The predecessors' keypairs, nearest hop first.
    #[must_use]
    pub fn predecessors(&self) -> &[ActorKeypair] {
        &self.predecessors
    }
}

impl From<ActorKeypair> for SeedHolder {
    fn from(keypair: ActorKeypair) -> Self {
        Self::new(keypair)
    }
}

/// How the runtime learns which `__conv` channels this account has joined —
/// the member half of the scope set (`scope_set` owns the derivation).
///
/// Read once per pump pass rather than pushed, so a join or a leave needs no
/// notification path: the next pass simply derives a different set. The source
/// is the app's own MLS engine (`MlsEngine::list_groups`), which is local
/// state — a replica with no nest in sight still knows what it joined.
///
/// **`None` means "cannot tell right now", not "no channels".** An engine that
/// is still loading would otherwise answer with an empty list, which reads as
/// *left every channel* and would silently stop walking them; on `None` the
/// runtime keeps the set it last derived.
///
/// `Send + Sync` natively (the store thread reads it), neither on wasm32 (the
/// SPA's single-threaded process) — a trait object cannot carry the
/// `MaybeSendSync` marker, so this is the marker's two-arm shape spelled out.
#[cfg(not(target_arch = "wasm32"))]
pub type MembershipSource = Arc<dyn Fn() -> Option<Vec<[u8; 32]>> + Send + Sync>;
/// See the native arm.
#[cfg(target_arch = "wasm32")]
pub type MembershipSource = Arc<dyn Fn() -> Option<Vec<[u8; 32]>>>;

/// Where the escrow-holder trust set comes from — the app's pin store for the
/// bound nest (`fauna_anon_client::trust::trusted_escrow_holders` natively,
/// the origin's TOFU pin on web).
///
/// Read at the start of every pump pass, never frozen at assembly
/// (`account-data-taxonomy.md` § The generation machinery → *A holder change
/// re-receipts and never mints*, (1)): a rotation the app has accepted moves
/// the pin, and the next pass trusts the successor with no reassembly. The
/// same two-arm shape as [`MembershipSource`], for the same reason.
#[cfg(not(target_arch = "wasm32"))]
pub type TrustedHolderSource = Arc<dyn Fn() -> Vec<[u8; 32]> + Send + Sync>;
/// See the native arm.
#[cfg(target_arch = "wasm32")]
pub type TrustedHolderSource = Arc<dyn Fn() -> Vec<[u8; 32]>>;

/// How the host opens the secondary leg's connection to a linked nest
/// (`crate::linked_leg`; `account-sync-plane.md` § The bind leg, ruling 4):
/// given the pairing row's target, an **owner-authenticated** requester of
/// the assembly's own type and the identity that connection is bound to —
/// read off the connection (the origin's pin, else a possession proof over it),
/// never the nest's own claim. The leg compares it with the row's nest id
/// before any account data moves. Natively a second `NestClient` under the
/// same identity; on web a second `WsRpcClient` whose bearer is minted on the
/// anonymous socket. The seedless host passes none.
#[cfg(not(target_arch = "wasm32"))]
pub type LinkedNestConnector<R> = Arc<
    dyn Fn(
            crate::linked_leg::LinkedNestTarget,
        ) -> futures_util::future::BoxFuture<
            'static,
            Result<crate::linked_leg::LinkedConnection<R>>,
        > + Send
        + Sync,
>;
/// See the native arm.
#[cfg(target_arch = "wasm32")]
pub type LinkedNestConnector<R> = Arc<
    dyn Fn(
        crate::linked_leg::LinkedNestTarget,
    ) -> futures_util::future::LocalBoxFuture<
        'static,
        Result<crate::linked_leg::LinkedConnection<R>>,
    >,
>;

/// What one keeper's owed list came to, as the host's deliverer answers it:
/// each served entry and what its delivery did, or why the list was unread.
pub type OwedDeliveries = std::result::Result<
    Vec<(
        fauna_protocol::recovery::OwedNest,
        fauna_client_core::succession_delivery::Delivery,
    )>,
    String,
>;

/// How the host delivers the succession statements one nest says this account
/// is owed at (`crate::owed_delivery`; `identity-succession.md` § Enforcement
/// on the home nest → *Every nest the identity is linked to*, **The road**):
/// given a connection to the keeping nest, authenticated as this account — the
/// bound nest's session, or a linked nest's connection the secondary leg
/// opened — run `fauna_client_core::succession_delivery::deliver_owed_nests`
/// over the host's own [`OwedNestReach`] (an anonymous connection type and a
/// signed-in one, which differ per platform, and the retired identities' seeds
/// this device holds — [`crate::owed_delivery::PredecessorSeeds`]). The
/// seedless host passes none.
///
/// [`OwedNestReach`]: fauna_client_core::succession_delivery::OwedNestReach
#[cfg(not(target_arch = "wasm32"))]
pub type OwedNestDeliverer<R> =
    Arc<dyn Fn(R) -> futures_util::future::BoxFuture<'static, OwedDeliveries> + Send + Sync>;
/// See the native arm.
#[cfg(target_arch = "wasm32")]
pub type OwedNestDeliverer<R> =
    Arc<dyn Fn(R) -> futures_util::future::LocalBoxFuture<'static, OwedDeliveries>>;

/// A [`TrustedHolderSource`] that always answers `holders` — for a host with
/// no pin store to read (a test, a seedless host that trusts no holder).
#[must_use]
pub fn fixed_holders(holders: Vec<[u8; 32]>) -> TrustedHolderSource {
    Arc::new(move || holders.clone())
}

/// What the host settled once, for the driver's whole life — the account,
/// the cadence, and the app-supplied trust and membership sources
/// (`AccountRuntimeParams` documents each on the native host).
pub struct DriverConfig {
    /// 64-hex actor id; validated by the host.
    pub actor_id_hex: String,
    /// Backstop reconcile cadence — [`DEFAULT_BACKSTOP_INTERVAL`] in
    /// production; tests shrink it.
    pub backstop_interval: Duration,
    /// Where the member half of the content-scope set comes from
    /// ([`MembershipSource`]). `None` walks the own-actor scopes only.
    pub memberships: Option<MembershipSource>,
    /// Escrow-holder identities whose receipts this account accepts — the
    /// trust half of the generation writer door
    /// ([`GenerationTrust::trusted_holders`]); the app pins it, and the
    /// source is re-read at every pass ([`TrustedHolderSource`]).
    pub trusted_escrow_holders: TrustedHolderSource,
    /// The account's **attested** succeeded-from identities: their ids are
    /// the `prior` half of the generation trust ([`GenerationTrust::prior`]),
    /// their delegable schedules are what the delegable scope's walk carries
    /// a predecessor's rows under
    /// ([`AccountStatePlane::with_predecessor_schedules`]), and their
    /// mint-kind keys are what the fleet scope's walk carries a predecessor's
    /// mint records under ([`AccountStatePlane::with_predecessor_mint_keys`]).
    pub attested_predecessors: AttestedPredecessors,
    /// Which `sync_devices` row this machine is — the machine's **named**
    /// row, the app's own derived device id, on which the enrollment ceremony
    /// registers the principal's grant (`sync-agent-credentials.md`
    /// § Credential model → the RULED 2026-09-28 block, decision 3).
    pub enrollment_target_device_id: String,
}

/// What one assembly resolved, borrowed for one [`AccountDriver::serve`]: the
/// open store and the identity it runs as. A reassembly (the stale-writer
/// heal, a succession rotation) rebuilds every one of these; the driver's own
/// state survives it.
pub struct Assembly<'a, B: StoreBackend, R> {
    pub store: &'a AccountStore<B>,
    /// The store's writer identity — this machine's device principal.
    pub writer_key: &'a SigningKey,
    /// The account's `BackupKey`, derived from the seed or read from the slot.
    pub backup_key: &'a BackupKey,
    /// Who is assembling: a seed-holding app, or the seedless agent.
    pub principal: &'a RuntimePrincipal,
    /// The T10 slot, through the slot seam.
    pub slot: &'a dyn PrincipalCustody,
    /// The data path's requester — the principal's own session when the host
    /// wired one, else the app session.
    pub data_rpc: &'a R,
    /// The app-session requester the enrollment ceremony's registration legs
    /// ride (`FleetWriter::session_rpc`).
    pub session_rpc: &'a R,
    /// The secondary leg's connector ([`LinkedNestConnector`]) — `None` runs
    /// no secondary leg, and a seedless principal runs none whatever it is
    /// handed.
    pub linked_nests: Option<&'a LinkedNestConnector<R>>,
    /// The host's succession deliverer ([`OwedNestDeliverer`]) — `None`
    /// delivers nothing, and a seedless principal delivers nothing whatever
    /// it is handed.
    pub owed_nests: Option<&'a OwedNestDeliverer<R>>,
}

/// What a pass hands the host's legs ([`HostLegs::run`]): the store, the
/// fleet plane, this device's writer identity and trust, the two requesters
/// and this pass's inputs — every leg step's arguments, in one place.
pub struct LegsCtx<'a, B: StoreBackend, R: RpcRequester> {
    pub store: &'a AccountStore<B>,
    /// `state-fleet` — the plane the custody rows are written through.
    pub fleet: &'a AccountStatePlane<'a, B, R>,
    pub trust: &'a GenerationTrust,
    pub writer_key: &'a SigningKey,
    pub slot: &'a dyn PrincipalCustody,
    pub schedule: &'a AccountStateKeySchedule,
    /// The data-path requester (the peer leg's `fauna.nest.info` fetch).
    pub rpc: &'a R,
    /// This pass's walk set.
    pub content_scopes: &'a [ContentScope],
    /// The app-fed transport facts, if any (the explicit override).
    pub endpoint_facts: Option<&'a EndpointFacts>,
}

/// What the legs hand back to the pass.
#[derive(Debug, Default)]
pub struct LegsOutput {
    /// The peer leg's self-observed transport facts (bound LAN candidates +
    /// the nest's relay URL) — `None` while no listener is bound. The
    /// app-fed facts win over these.
    pub bind_facts: Option<EndpointFacts>,
    /// Every peer endpoint this pass observed — carried in by a dialer or
    /// answered to our own dials — keyed by the channel-proven key. The pass
    /// folds them into the custody registry rows (T13 step 4).
    pub observed_endpoints: std::collections::HashMap<[u8; 32], DeviceEndpoints>,
}

/// The host's legs, run once per full pass between the fleet walk and the
/// device-endpoints step. Natively the peer leg (same-account device↔device
/// sync over iroh) and the custody leg (the accounts this machine holds
/// custody for) — `fauna_sync_engine::account_runtime` implements it over its
/// `peer_leg` and `custody_leg` modules, in the order the pump always kept:
/// withdrawal snapshots, ensure-bound, sibling dial, custody serve refresh,
/// custody nest pull, custody dial + budget, receipts. Web has no iroh and
/// hosts no custody, so it runs [`NoLegs`]. Each leg reports into its own
/// `PumpReport` slot and pushes step failures through [`push_step_error`].
pub trait HostLegs<B: StoreBackend, R: RpcRequester> {
    fn run(
        &mut self,
        ctx: LegsCtx<'_, B, R>,
        report: &mut PumpReport,
    ) -> impl std::future::Future<Output = LegsOutput>;
}

/// A host with no legs — web, and any test that wants the pass alone.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLegs;

impl<B: StoreBackend, R: RpcRequester> HostLegs<B, R> for NoLegs {
    async fn run(&mut self, _ctx: LegsCtx<'_, B, R>, _report: &mut PumpReport) -> LegsOutput {
        LegsOutput::default()
    }
}

/// The engine-singleton role this runtime plays (`account-runtime.md`
/// § Multi-instance concurrency): exactly one co-located process runs the
/// sync engine + outbox drain per store; every other same-store process is a
/// plain reader/writer whose convergence arrives through the shared store.
pub enum EngineRole<H> {
    /// Runs the pump. Holds the election's role until drop — or `None` after
    /// a degraded acquire at start (see [`elect_at_start`]'s ruling), where
    /// this runtime pumps unguarded for its lifetime.
    Holder { _lock: Option<H> },
    /// Another holder (usually another process) has the role. Serve reads
    /// and plain writes; re-try the election on the backstop cadence — no
    /// timer of its own — to pick the role up when the holder exits.
    NonHolder,
}

impl<H> EngineRole<H> {
    pub fn is_holder(&self) -> bool {
        matches!(self, EngineRole::Holder { .. })
    }
}

/// Take the engine-singleton election at assembly — in front of the pump,
/// exactly as the charter reserved it ("the runtime's start takes the store
/// dir, so the multi-process `engine.lock` election slots in front of the pump without
/// reshaping it"). The host calls it inside its readiness barrier, after the
/// store proved openable (a runtime that cannot open the store must not squat
/// on the role).
///
/// **Degrade ruling (start): degrade open — pump anyway, unguarded.** The
/// overwhelmingly common deployment is a lone process, where refusing to
/// pump over a lock-file I/O hiccup would silently strand sync — the same
/// works-out-of-the-box posture as `AccountInstanceLock`'s degrade
/// (`account-scoping.md` § Concurrent instances, a semantic the multi-instance contract
/// preserves where it survives). The window this opens — an unguarded pump
/// beside a live holder — needs the lock to fail on a store that is serving
/// both processes fine, and the store contract (WAL + transactions + keyed
/// drains) keeps even that window safe from corruption; duplicate work is the
/// cost, not integrity.
pub async fn elect_at_start<E: EngineElection>(election: &E) -> EngineRole<E::Held> {
    match election.try_acquire().await {
        ElectionOutcome::Held(lock) => EngineRole::Holder { _lock: Some(lock) },
        ElectionOutcome::Refused => {
            tracing::info!(
                "account runtime: engine singleton held by another process — \
                 starting as a plain reader/writer"
            );
            EngineRole::NonHolder
        }
        ElectionOutcome::Degraded(e) => {
            tracing::warn!(
                "account runtime: engine-singleton election degraded ({e}) — \
                 pumping unguarded (degrade-open, see elect_at_start)"
            );
            EngineRole::Holder { _lock: None }
        }
    }
}

/// The seed-leg role this runtime plays (`account-runtime.md`
/// § Multi-instance concurrency → *The seed-leg role*): exactly one co-located
/// seed-holding runtime per store runs the steps only a signed-in app can run.
enum SeedLegRole<H> {
    /// Runs the seed-only steps — inside its pass when it also holds the
    /// engine role, as a seed pass when it does not. Holds the role's lock
    /// until drop — or `None` after a degraded acquire at assembly.
    Holder { _lock: Option<H> },
    /// A seedless runtime (it never asks), or a seed holder beside the one
    /// that took the role first: runs none of them, and a seed holder re-tries
    /// wherever the engine election is re-tried.
    NonHolder,
}

impl<H> SeedLegRole<H> {
    fn is_holder(&self) -> bool {
        matches!(self, SeedLegRole::Holder { .. })
    }
}

/// Take the seed-leg role at assembly, for a seed-holding runtime — right
/// behind the engine election, with its degrade posture ([`elect_at_start`]):
/// **open**. A lone app whose lock file hiccups must still recover its keys
/// and complete its linked nests; what an unguarded holder beside a live one
/// costs is a leg run twice.
async fn elect_seed_legs_at_start<E: EngineElection>(election: &E) -> SeedLegRole<E::SeedLegs> {
    match election.try_acquire_seed_legs().await {
        ElectionOutcome::Held(lock) => SeedLegRole::Holder { _lock: Some(lock) },
        ElectionOutcome::Refused => {
            tracing::info!(
                "account runtime: the seed-leg role is held by another process — \
                 this runtime runs no seed-only leg"
            );
            SeedLegRole::NonHolder
        }
        ElectionOutcome::Degraded(e) => {
            tracing::warn!(
                "account runtime: seed-leg election degraded ({e}) — running the seed-only \
                 legs unguarded (degrade-open, see elect_seed_legs_at_start)"
            );
            SeedLegRole::Holder { _lock: None }
        }
    }
}

/// Re-try the seed-leg role for a seed-holding runtime that does not hold it —
/// at the backstop tick and on `reconcile_now`, where the engine election is
/// re-tried. A degrade here stays a non-holder (**closed**): a refusal already
/// proved arbitration works on this store and a holder was live.
async fn retry_seed_legs<E: EngineElection>(
    election: &E,
    principal: &RuntimePrincipal,
    role: &mut SeedLegRole<E::SeedLegs>,
) {
    if role.is_holder() || principal.keypair().is_none() {
        return;
    }
    match election.try_acquire_seed_legs().await {
        ElectionOutcome::Held(lock) => {
            tracing::info!(
                "account runtime: seed-leg role acquired (previous holder exited) — \
                 running the seed-only legs"
            );
            *role = SeedLegRole::Holder { _lock: Some(lock) };
        }
        ElectionOutcome::Refused => {}
        ElectionOutcome::Degraded(e) => tracing::warn!(
            "account runtime: seed-leg re-election degraded ({e}) — still running no \
             seed-only leg"
        ),
    }
}

/// How one [`AccountDriver::serve`] ended — what the host does next.
pub enum ServeEnd {
    /// A typed `StaleWriter` refusal, a `RemovedFromAccount` enrollment
    /// answer on a seed-holding runtime, or a burnt-journal verdict: the host
    /// re-runs its assembly (re-resolving the successor from the slot) and
    /// serves again on the same driver.
    Reassemble,
    /// Every handle dropped — the plain-quit teardown. The host ends.
    Closed,
    /// [`AccountStoreHandle::shutdown`] — the host ends, and answers `reply`
    /// **after** this serve's locals have dropped, the engine-singleton lock
    /// above all: `shutdown()` documents that the runtime "finishes its
    /// current step, drops the store, and exits" by the time it returns, and
    /// the next holder — a co-located agent taking over the role, or this
    /// same process re-assembling for another account — elects by
    /// *acquiring* that lock, so answering before releasing it hands them a
    /// `Refused` for a runtime that is already gone. Replying then makes the
    /// release a causal barrier rather than a race the caller sleeps against.
    Shutdown(oneshot::Sender<()>),
    /// The plane could not be built (the actor id, the trust, a plane) — on
    /// a first serve the readiness barrier carried the error; on a reassembly
    /// it was logged. The host ends, loudly.
    Failed,
}

fn failed(ready: Option<oneshot::Sender<Result<()>>>, e: anyhow::Error) -> ServeEnd {
    match ready {
        Some(tx) => {
            let _ = tx.send(Err(e));
        }
        None => tracing::error!("account runtime: reassembly failed: {e:#}"),
    }
    ServeEnd::Failed
}

/// The driver: everything the pump keeps across reassemblies, and the serve
/// loop over one assembly ([`Self::serve`]). Minted once per runtime by
/// [`Self::new`], together with the one [`AccountStoreHandle`] the host hands
/// out; the host owns it for the store thread's (or the `spawn_local` task's)
/// life.
pub struct AccountDriver {
    cmd_rx: mpsc::Receiver<Cmd>,
    nudge_rx: mpsc::Receiver<String>,
    sign_out: SignOutWatch,
    cycles: Arc<PumpCycles>,
    /// This process's first listings, shared with the handle (the
    /// first-listing gate's in-process half) and handed to each assembly's
    /// two bound planes.
    first_listings: Arc<FirstListings>,
    /// `NestClient::subscribe_reconnects()` in production; `None` disarms the
    /// reconnect wake.
    reconnects: Option<watch::Receiver<u64>>,
    /// The session's push stream — the pump's own push→nudge arm, mapped by
    /// [`nudge_scope_for_push`]; `None` disarms it.
    pushes: Option<broadcast::Receiver<PushEvent>>,
    // App-fed state that must SURVIVE a reassembly (the stale-writer heal): a
    // writer rotation must not silently drop the scopes an app registered or
    // the transport facts it fed.
    registered: Vec<ContentScope>,
    endpoint_facts: Option<EndpointFacts>,
    channels: Vec<[u8; 32]>,
    /// Pass-bound commands that arrived while a pass was in flight, served
    /// after it in arrival order (`Cmd::is_local`). Across reassemblies for
    /// the same reason as the app-fed state: a command parked by a pass that
    /// a stale-writer reassembly then dropped is still owed its answer.
    parked: VecDeque<Cmd>,
    /// The local-write wake (the pump bullet's wake source (4)): a local
    /// write armed the publish step, run at the top of the serve loop as
    /// soon as no pass is in flight.
    publish_due: bool,
    /// The succession probe's livelock/adversary cap: one rotation per
    /// assembly chain (reset once a HEALTHY enrollment answer is reached) —
    /// a nest that revokes a freshly minted key must not draw an unbounded
    /// mint loop out of one sign-in. Shared with the host, whose succession
    /// probe reads it (`!rotated_since_serve` is the probe's rotation
    /// license) and sets it when a rotation restarts assembly.
    pub rotated_since_serve: bool,
    /// Principal succession's third trigger, carried across the reassembly
    /// it causes: the writer whose own device-set row a pass read `Removed`
    /// ([`PumpReport::own_row_removed`]), recorded only where the cap above
    /// allowed the heal. The ceremony probe runs before the backend opens and
    /// cannot read merged state, so the pump is the reader and this is how
    /// the finding reaches the next assembly ([`Self::own_row_removed`]).
    /// Keyed by the writer, so it says nothing about a successor and needs no
    /// clearing; in memory only, because the row is absorbing and a pass
    /// after a crash reads it again.
    own_row_removed: Option<[u8; 32]>,
    /// The removed-heal's pacing, beside the rotation cap above: a serve
    /// reassembled on a `RemovedFromAccount` answer and no healthy answer or
    /// backstop tick has come since. The cap counts rotations, and a
    /// reassembly whose probe rotated nothing — its register faulted, or its
    /// budget lapsed — leaves it armed; the new assembly's prologue is then
    /// answered removed again, and without this the worker reassembles on
    /// every prologue with no wait in the loop (refinement 7). While it is
    /// set a removed answer stays loud and reassembles nothing; the backstop
    /// tick clears it, so the heal is tried again at that cadence and a probe
    /// that skipped once is not the end of it. In memory only: a relaunch
    /// starts armed.
    removed_heal_taken: bool,
    /// The burnt-journal heal's own cap (refinement 11): at most ONE
    /// reassembly on a walk's burnt verdict per driver. The heal arm then
    /// mints and fences; a verdict that survives that is a heal that could
    /// not land, and reassembling on every pass would be a livelock at the
    /// pump cadence. A relaunch re-arms it.
    burnt_heal_taken: bool,
    /// The escrow-recovery step's answered-set: one `escrow.get` per live
    /// unkeyable generation per driver, across reassemblies (a rotated device
    /// key changes nothing the holder would answer differently).
    escrow_recovery_memo: EscrowRecoveryMemo,
    /// The handle's grant-registered signal (the sending half is shared).
    grant_registered: Arc<watch::Sender<bool>>,
    config: DriverConfig,
}

impl AccountDriver {
    /// Mint the driver and its handle. The host keeps the driver for the
    /// store task and hands the handle out once its first assembly has
    /// signalled ready.
    pub fn new(
        config: DriverConfig,
        reconnects: Option<watch::Receiver<u64>>,
        pushes: Option<broadcast::Receiver<PushEvent>>,
    ) -> (AccountDriver, AccountStoreHandle) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(32);
        let (nudge_tx, nudge_rx) = mpsc::channel::<String>(8);
        let (sign_out_tx, sign_out_rx) = watch::channel(None);
        let cycles = Arc::new(PumpCycles::default());
        let first_listings = Arc::new(FirstListings::default());
        let grant_registered = Arc::new(watch::channel(false).0);
        let ledger_identity = fauna_core::hex32::decode(&config.actor_id_hex)
            .ok()
            .map(|id| {
                Arc::new(handle::LedgerIdentity {
                    self_actor: ActorId(id),
                    attested: config.attested_predecessors.actor_ids().to_vec(),
                })
            });
        let driver = AccountDriver {
            cmd_rx,
            nudge_rx,
            sign_out: SignOutWatch(sign_out_rx),
            cycles: Arc::clone(&cycles),
            first_listings: Arc::clone(&first_listings),
            reconnects,
            pushes,
            registered: Vec::new(),
            endpoint_facts: None,
            channels: Vec::new(),
            parked: VecDeque::new(),
            publish_due: false,
            rotated_since_serve: false,
            own_row_removed: None,
            removed_heal_taken: false,
            burnt_heal_taken: false,
            escrow_recovery_memo: EscrowRecoveryMemo::default(),
            grant_registered: Arc::clone(&grant_registered),
            config,
        };
        let handle = AccountStoreHandle {
            cmd: cmd_tx,
            nudge: nudge_tx,
            cycles,
            first_listings,
            sign_out: Arc::new(sign_out_tx),
            p2p_participation_changed: Arc::new(watch::channel(0u64).0),
            grant_registered,
            ledger_identity,
        };
        (driver, handle)
    }

    /// The writer a pass of this driver found removed on the fleet plane —
    /// its own `fauna.state.device-set` row reading `Removed` — if a
    /// seed-holding serve under the rotation cap reassembled on it
    /// (`account-replica-posture.md` § The store device principal →
    /// *Principal succession after a device delete*, decision 1, the third
    /// trigger). The host hands it to its assembly's succession probe
    /// (`principal_succession::ceremony_probe`), which rotates when it names
    /// the writer the assembly holds, without asking the nest.
    pub fn own_row_removed(&self) -> Option<[u8; 32]> {
        self.own_row_removed
    }

    /// Serve one assembly: build the two planes and the generation trust over it,
    /// publish the fleet bootstrap rows, signal `ready` (the first assembly
    /// only — the host passes `None` on a reassembly), run the prologue, then
    /// the serve loop until the assembly is over ([`ServeEnd`]).
    ///
    /// **The readiness barrier ends at assembly** (store open, writer key,
    /// fleet bootstrap) — deliberately, so a start never blocks on a network
    /// pass and an offline device comes up regardless. The prologue runs
    /// *after* ready is sent, so a caller that returns from the host's start
    /// and immediately mutates shared state **races that pass**; callers
    /// needing the prologue behind them take [`AccountStoreHandle::settled`].
    pub async fn serve<B, R, L, E>(
        &mut self,
        assembly: Assembly<'_, B, R>,
        legs: &mut L,
        election: &E,
        role: EngineRole<E::Held>,
        ready: Option<oneshot::Sender<Result<()>>>,
    ) -> ServeEnd
    where
        B: StoreBackend,
        // Keyed, not plain: the pump's outbox drain replays intents whose
        // envelope must carry the stored intent id (`outbox::drain_outbox`).
        R: KeyedRpcRequester + Clone + MaybeSendSync,
        R::Error: RpcErrorClass,
        L: HostLegs<B, R>,
        E: EngineElection,
    {
        let Assembly {
            store,
            writer_key,
            backup_key,
            principal,
            slot,
            data_rpc,
            session_rpc,
            linked_nests,
            owed_nests,
        } = assembly;
        let AccountDriver {
            cmd_rx,
            nudge_rx,
            sign_out,
            cycles,
            first_listings,
            reconnects,
            pushes,
            registered,
            endpoint_facts,
            channels,
            parked,
            publish_due,
            rotated_since_serve,
            own_row_removed,
            removed_heal_taken,
            burnt_heal_taken,
            escrow_recovery_memo,
            grant_registered,
            config: settings,
        } = self;
        let first_listings: &FirstListings = first_listings;
        let mut role = role;
        // The seed-leg role, right behind the engine election the host just
        // ran, and inside the same readiness barrier. Held as a local of this
        // serve, so it is released with the engine role's lock when the
        // assembly ends (`ServeEnd::Shutdown` owns why that ordering matters).
        let mut seed_legs = match principal {
            RuntimePrincipal::SeedHolding(_) => elect_seed_legs_at_start(election).await,
            RuntimePrincipal::Seedless => SeedLegRole::NonHolder,
        };
        let grant_registered: &watch::Sender<bool> = grant_registered;
        // Seeded before `ready`, so a host that spawns its principal's gated
        // connect as `start` returns never waits on a launch after the
        // machine's first: the slot already records the registration.
        if slot.grant_registration_row().is_some() {
            open_grant_gate(grant_registered);
        }
        // A non-holder's latch re-read (`GRANT_LATCH_POLL`), armed only while
        // the gate is closed and the role is someone else's.
        let mut latch_backoff =
            fauna_protocol::reconnect::Backoff::new(GRANT_LATCH_POLL.0, GRANT_LATCH_POLL.1);
        let mut latch_poll = std::pin::pin!(fauna_sleep::sleep(latch_backoff.ceiling()));
        // Publish the election result the moment assembly settles it, and
        // again at both re-try wins below — the three places `role` can move.
        // A reader holding the handle must be able to tell a frozen
        // non-holder's counters from an elected runtime's ([`PumpCycles`] owns
        // why). A reassembly re-runs the election, so this re-publishes too.
        cycles.set_holder(role.is_holder());
        let schedule = AccountStateKeySchedule::derive(backup_key);
        // The generation writer-door trust (`generation_tip::GenerationTrust`), wired for
        // production in build step 7: the root is this account, `prior` is the
        // caller's ATTESTED predecessor set (`AccountRuntimeParams` owns why
        // that half is a parameter, and why no device-local replica is
        // read for it), and `trusted_holders` is whatever escrow holders the app
        // pinned (v1: its nest's deployment identity). Each half degrades
        // fail-safe on its own: no priors means a successor account's
        // predecessor-signed enrollments stop verifying (those devices drop out
        // of the fleet view, and tips naming them go inadmissible); no holders
        // means no receipt is trusted, so no tip resolves and `GenerationTip`
        // sealing stays refused with the precise no-tip error — exactly the generation
        // gate this door replaced. `Gen0` kinds (every shipped preference kind,
        // and the machinery itself) are unaffected by either.
        let trust = match r14_trust(
            &settings.actor_id_hex,
            settings.attested_predecessors.actor_ids(),
            &(settings.trusted_escrow_holders)(),
        ) {
            Ok(trust) => trust,
            Err(e) => return failed(ready, e),
        };
        let plane = match AccountStatePlane::new(
            store,
            data_rpc,
            &schedule,
            writer_key,
            &trust,
            ACCOUNT_STATE_SCOPE,
        ) {
            // The delegable scope's plane carries a predecessor's delegable
            // rows (`succession-aftermath.md` § Re-key scope). The fleet
            // plane below is handed no predecessor schedule — the schedule
            // type could open no fleet-only row if it were — and carries one
            // kind under its own keys.
            Ok(p) => p
                .with_generation_custody(slot)
                .with_predecessor_schedules(settings.attested_predecessors.delegable_schedules())
                .with_first_listings(first_listings),
            Err(e) => return failed(ready, e),
        };
        // The A5 partition's other half. Without this plane a replica could neither
        // write nor merge a single machinery row, so no device would ever join the
        // fleet, no generation could be minted, and every `GenerationTip` kind
        // would stay refused on every real account regardless of what the door can
        // do (charter § The generation machinery, the partition bullet).
        let fleet_plane = match AccountStatePlane::new(
            store,
            data_rpc,
            &schedule,
            writer_key,
            &trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        ) {
            // The succession rider's carry (`owner-key-material.md` § Path
            // A-sibling-2 → *Rotation*): each attested predecessor's keys for
            // the mint kind alone, so this walk carries the mint record of
            // every generation the slot holds a key for — the row the
            // re-escrow, escrow-recovery and top-up passes iterate. Nothing
            // else of a predecessor's fleet-only branch opens in the walk.
            // The reclamation pass alone is handed the wider retired
            // machinery keys, to open a predecessor's machinery rows and
            // retire them (`account-data-taxonomy.md` § The generation
            // machinery → *Fleet-scope reclamation*, clause (3)(i)).
            Ok(p) => p
                .with_generation_custody(slot)
                .with_predecessor_mint_keys(settings.attested_predecessors.mint_kind_keys())
                .with_predecessor_machinery_keys(
                    settings.attested_predecessors.retired_machinery_keys(),
                )
                .with_first_listings(first_listings),
            Err(e) => return failed(ready, e),
        };
        let planes = Planes {
            delegable: &plane,
            fleet: &fleet_plane,
        };
        // The bind leg's per-assembly memory — fresh here, so the first pass
        // after every assembly runs the holdings check (`crate::bind_leg`).
        let bind_memo = crate::bind_leg::BindMemo::default();
        // The fleet writer identity the device-endpoints step needs beside the
        // planes (the plane holds both privately; the step also resolves tips
        // directly off the store).
        let fleet_writer = FleetWriter {
            trust: &trust,
            key: writer_key,
            slot,
            session_rpc,
            schedule: &schedule,
            enrollment_target: &settings.enrollment_target_device_id,
            // The two seed-only slots are filled for the seed-leg role's
            // holder alone — `writer!()` below.
            escrow_recovery: None,
            grant_registered,
            holder_source: &settings.trusted_escrow_holders,
            bind: &bind_memo,
            linked_nests: None,
            owed_nests: None,
        };
        // What the role's holder runs the seed-only steps with: the identity
        // seed, the attested predecessors' seeds (the kept wrap's recovery —
        // `SeedHolder`) and this driver's answered-set, and the host's connector — a
        // seedless host has neither, whatever it was handed (ruling 4, *the
        // stated bounds*).
        let seed_escrow = principal.keypair().map(|kp| {
            (
                kp.secret_bytes(),
                principal.predecessor_keypairs(),
                &*escrow_recovery_memo,
            )
        });
        let seed_linked = linked_nests.filter(|_| principal.keypair().is_some());
        // The road's deliverer rides the same role: one delivery per pass per
        // machine, by the seed holder that holds it.
        let seed_owed = owed_nests.filter(|_| principal.keypair().is_some());
        // The fleet writer a pass runs with: read at every pass, because the
        // role can arrive between two of them (a re-try's win).
        macro_rules! writer {
            () => {
                if seed_legs.is_holder() {
                    FleetWriter {
                        escrow_recovery: seed_escrow,
                        linked_nests: seed_linked,
                        owed_nests: seed_owed,
                        ..fleet_writer
                    }
                } else {
                    fleet_writer
                }
            };
        }
        // This device joins its own account's fleet, and the account's escrow
        // target gets written — both idempotent, both **inside the readiness
        // barrier**, so the very first `GenerationTip` write already finds a
        // mintable account and no command can interleave with the bootstrap.
        // Written **locally only**: the barrier is the one stretch a sign-out
        // cannot cut, so it holds no nest leg (`fleet_bootstrap` owns why);
        // the prologue's publish step, its first, sends the rows.
        // The fleet bootstrap rows are seed-only: the enrollment cert is
        // root-signed and the escrow target derives from the identity **seed**.
        // Built here from the borrowed principal — never a reconstructed
        // keypair — so the process keeps a single live copy of that secret.
        // `None` for the seedless host: a signed-in app publishes them.
        // A seed-holding host start is a sign-in, or a launch still signed
        // in: it outranks a sign-out whose erase never landed, so the store's
        // sign-out stamp comes off and this machine's own `Removed` row is a
        // removal to heal from again (`EnrollmentPass::SignedOut`). The first
        // assembly only — a reassembly is the same sign-in, and a sibling's
        // sign-out may have stamped the store since.
        if ready.is_some()
            && principal.keypair().is_some()
            && let Err(e) = store.clear_signed_out_writer().await
        {
            tracing::warn!(
                "account runtime: the store's sign-out stamp did not clear ({e:#}) — a \
                 machine signed out and in again stays unenrolled until the next start"
            );
        }
        let bootstrap = principal
            .keypair()
            .and_then(|kp| fleet_bootstrap_rows(kp, writer_key));
        fleet_bootstrap(store, &fleet_plane, bootstrap).await;

        if let Some(tx) = ready {
            let _ = tx.send(Ok(()));
        }
        // (The one-rotation cap `rotated_since_serve` re-arms only on a
        // HEALTHY enrollment answer — see `enrollment_healthy` — never on
        // merely reaching this point.)

        // The content-scope set, derived rather than configured (`scope_set` —
        // charter § Feeds and cursors → *Scope partition*). The actor is decoded
        // once: a malformed id cannot reach here (the state-dir floor validated it
        // during assembly), and a failure is logged rather than fatal — an empty
        // content set is exactly the shipped pre-derivation behavior, and the
        // class-2 legs must keep pumping regardless.
        let actor = match fauna_core::hex32::decode(&settings.actor_id_hex) {
            Ok(actor) => Some(actor),
            Err(e) => {
                tracing::warn!(
                    "account runtime: content scopes not derived ({e}) — the account-state \
                 legs still pump; no content scope is walked"
                );
                None
            }
        };
        // The auto-in-set producer's domain: the own-actor half **alone** — member
        // scopes are browse content, where a watermark would assert observations
        // nobody made (`seen_set_producer` module docs). Static per account, so
        // derived once; same non-fatal posture as the walk set above.
        let own_scopes: Vec<ContentScope> = actor
            .map(|a| {
                crate::scope_set::derive_own_actor_scopes(a).unwrap_or_else(|e| {
                    tracing::warn!("account runtime: own-actor scopes not derived ({e})");
                    Vec::new()
                })
            })
            .unwrap_or_default();
        // (`registered`, `endpoint_facts`, `channels` are declared ABOVE the
        // assembly loop: app-fed state survives a stale-writer reassembly.)
        // Seeded before the prologue so a fresh replica's very first pass already
        // walks its own content.
        // `answered` rides alongside: it is what licenses the departure step to
        // delete (see `derive_scopes`), and it must never be inferred from the set.
        let (mut content_scopes, mut answered) =
            derive_scopes(actor, &settings.memberships, registered, channels);

        // What a local command may touch (`LocalCtx`), and the pass driver's
        // command side around it (`Drive`). Macros, not bindings: both borrow
        // `registered`, `endpoint_facts` and `publish_due` mutably, so each is
        // built for one pass (or one command) and dropped after it — which is
        // why every pass reads a snapshot of the facts, never the cell.
        macro_rules! local {
            ($pass_in_flight:expr) => {
                LocalCtx {
                    store,
                    plane: &plane,
                    fleet_plane: &fleet_plane,
                    trust: &trust,
                    writer_key,
                    principal_slot: slot,
                    seed_holding: principal.keypair().is_some(),
                    bind: &bind_memo,
                    registered: &mut *registered,
                    endpoint_facts: &mut *endpoint_facts,
                    publish_due: &mut *publish_due,
                    pass_in_flight: $pass_in_flight,
                }
            };
        }
        macro_rules! drive {
            () => {
                Drive {
                    cmd_rx: &mut *cmd_rx,
                    parked: &mut *parked,
                    local: local!(true),
                    cycles,
                }
            };
        }

        // The seed pass, through the pump's one funnel: contained, cut by a
        // sign-out and driven beside the command channel like every pass, and
        // counted as no pass cycle (`cycles: None` — "the pump has run" stays
        // the engine holder's statement). Evaluates to the report, or the
        // reassembly a local command demanded mid-pass.
        //
        // Boxed: the serve future lives on the store thread's stack, every
        // expansion below would otherwise inline a whole secondary leg's
        // state into it, and five of them overflowed that stack (measured
        // 2026-10-01 on a debug build). One allocation per seed pass.
        macro_rules! run_seed_pass {
            ($label:expr) => {
                contained_pump(
                    $label,
                    None,
                    sign_out,
                    &mut drive!(),
                    Box::pin(seed_pass(store, planes, writer!())),
                )
                .await
                .map(|mut report| {
                    // A cut or a panicked pass reports through the funnel's
                    // own default: the role answer holds there too.
                    report.skipped_non_holder = true;
                    log_pump($label, &report);
                    report
                })
            };
        }
        // What a report's enrollment verdict licenses, read one way at every
        // site a report is read: a healthy answer re-arms the one-rotation
        // cap and the heal's pacing; a seed-holding runtime under the cap
        // may heal a removal by reassembling, once until a healthy answer or
        // the backstop tick (`removed_heal_taken`); and a removal read off
        // this machine's own device-set row is carried to the next
        // assembly's probe, which rotates on it (decision 1's third trigger
        // — `AccountDriver::own_row_removed`). A removed answer the heal is
        // allowed on always reassembles (`reassembly_reason`), so the pacing
        // is spent here, where the answer is read.
        macro_rules! removed_heal_allowed {
            ($report:expr) => {{
                if enrollment_healthy($report) {
                    *rotated_since_serve = false;
                    *removed_heal_taken = false;
                }
                let allowed =
                    principal.keypair().is_some() && !*rotated_since_serve && !*removed_heal_taken;
                if allowed && $report.enrollment == Some(EnrollmentPass::RemovedFromAccount) {
                    *removed_heal_taken = true;
                    if $report.own_row_removed {
                        *own_row_removed = Some(writer_key.verifying_key().to_bytes());
                    }
                }
                allowed
            }};
        }
        // What a seed pass's report demands of this serve, read exactly as a
        // full pass's is: a stale writer, or — the seed pass carries the
        // enrollment registration — a removed-from-account answer, healed
        // under the same one-rotation cap, which a healthy answer re-arms.
        // (No burnt-journal arm: a seed pass walks no plane.)
        macro_rules! seed_pass_reassembly {
            ($report:expr) => {{
                let allow_removed_heal = removed_heal_allowed!($report);
                reassembly_reason($report, allow_removed_heal, false)
            }};
        }
        // A wake's seed pass for a seed-leg holder that is not the engine
        // holder; nothing for any other runtime.
        macro_rules! seed_pass_if_held {
            ($label:expr) => {
                if seed_legs.is_holder() {
                    let Ok(report) = run_seed_pass!($label) else {
                        return ServeEnd::Reassemble;
                    };
                    if let Some(reason) = seed_pass_reassembly!(&report) {
                        tracing::info!("account runtime: reassembling — {reason} (seed pass)");
                        return ServeEnd::Reassemble;
                    }
                }
            };
        }

        // ── Prologue: run to completion before the first wait (contained like
        // every other pass — a poisoned prologue must not kill the thread —
        // and driven beside the command channel like every other pass: a
        // local command is served at its next yield point, never behind it;
        // `account-data-plane.md` § The client-side lifecycle, the pump
        // bullet → *Commands and passes*).
        // Holder-only, like every pass: a non-holder's prologue is the holder's
        // to run — its first catch-up pass comes with the role, if it ever wins
        // one ("acquired", in the ticker arm).
        if role.is_holder() {
            // A snapshot, as at every pass: a `SetEndpointFacts` served inside
            // this pass lands for the next.
            let facts = endpoint_facts.clone();
            let inputs = PassInputs {
                content_scopes: &content_scopes,
                own_scopes: &own_scopes,
                membership_answered: answered,
                endpoint_facts: facts.as_ref(),
            };
            let outcome = contained_pump(
                "prologue",
                Some(cycles),
                sign_out,
                &mut drive!(),
                pump(store, planes, data_rpc, writer!(), &mut *legs, &inputs),
            )
            .await;
            let Ok(report) = outcome else {
                return ServeEnd::Reassemble;
            };
            log_pump("prologue", &report);
            let allow_removed_heal = removed_heal_allowed!(&report);
            if let Some(reason) = reassembly_reason(&report, allow_removed_heal, !*burnt_heal_taken)
            {
                *burnt_heal_taken |= walk_found_burnt(&report);
                tracing::info!("account runtime: reassembling — {reason}");
                return ServeEnd::Reassemble;
            }
        } else {
            // The first wake after assembly: beside another engine holder, the
            // seed-leg role's holder runs its seed pass where the prologue
            // would have run.
            seed_pass_if_held!("seed prologue");
        }

        // The backstop ticker: one cross-target sleep (`fauna_sleep`), re-armed
        // when its tick is OBSERVED — tokio's `MissedTickBehavior::Delay` by
        // construction: a pass longer than the cadence is followed by a whole
        // cadence, never a burst. Pinned outside the loop so a stream of other
        // wakes cannot starve it: the deadline survives every select.
        let mut backstop = std::pin::pin!(fauna_sleep::sleep(settings.backstop_interval));

        loop {
            // The local-write wake (the pump bullet's wake source (4)): a
            // local write armed the publish step — the ordered own publish of
            // both planes, then the blob mirror — which runs here, as soon as no
            // pass is in flight, on every role (a non-holder's own rows are
            // its own to publish, exactly as its inline publish was), and is
            // driven like any pass: a read during it is still served.
            if *publish_due {
                *publish_due = false;
                let outcome = contained_pump(
                    "publish",
                    None,
                    sign_out,
                    &mut drive!(),
                    publish_step(planes),
                )
                .await;
                let Ok(report) = outcome else {
                    return ServeEnd::Reassemble;
                };
                log_pump("publish", &report);
                if report.stale_writer {
                    tracing::info!(
                        "account runtime: reassembling — the writer was rotated under this \
                         process (publish step)"
                    );
                    return ServeEnd::Reassemble;
                }
                continue;
            }
            // A pass-bound command the last pass parked is served before the
            // loop waits for anything new, so arrival order holds across a
            // pass.
            let wake = match parked.pop_front() {
                Some(cmd) => Wake::Cmd(cmd),
                None => tokio::select! {
                    biased;

                    cmd = cmd_rx.recv() => cmd.map_or(Wake::Closed, Wake::Cmd),
                    Some(scope) = nudge_rx.recv() => Wake::Nudge(scope),
                    // The push→nudge arm: the session's push stream, mapped
                    // to the scope it wakes. A push that wakes nothing (a
                    // folder nudge, any other kind) and a lagged receiver are
                    // skipped here, so the loop only ever wakes for a scope;
                    // a closed stream disarms the arm (`Wake::PushesClosed`).
                    scope = async {
                        match pushes.as_mut() {
                            Some(rx) => loop {
                                match rx.recv().await {
                                    Ok(event) => {
                                        if let Some(scope) = nudge_scope_for_push(&event) {
                                            break Some(scope.to_string());
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                    Err(broadcast::error::RecvError::Closed) => break None,
                                }
                            },
                            None => std::future::pending().await,
                        }
                    } => scope.map_or(Wake::PushesClosed, Wake::Nudge),
                    changed = async {
                        match reconnects.as_mut() {
                            Some(rx) => rx.changed().await,
                            None => std::future::pending().await,
                        }
                    } => Wake::Reconnect(changed),
                    () = &mut backstop => {
                        backstop.set(fauna_sleep::sleep(settings.backstop_interval));
                        Wake::Tick
                    }
                    () = &mut latch_poll, if !role.is_holder() && !*grant_registered.borrow() => {
                        Wake::GrantLatch
                    }
                },
            };
            match wake {
                Wake::Closed => return ServeEnd::Closed, // every handle dropped
                // Between passes a local command is served exactly as it is
                // inside one — a tip-sealed put a pass parked included, which
                // may mint here — and a local write then arms the publish
                // step above.
                Wake::Cmd(cmd) if cmd.is_local() => {
                    match serve_local_cmd(cmd, &mut local!(false)).await {
                        Served::Done => {}
                        Served::Reassemble => return ServeEnd::Reassemble,
                        Served::Park(_) => unreachable!(
                            "serve_local_cmd parks only inside a pass — none is in flight here"
                        ),
                    }
                }
                Wake::Cmd(cmd) => {
                    match cmd {
                        Cmd::ReconcileNow { reply } => {
                            // A non-holder first re-tries the election: a free
                            // role should not make an explicit "reconcile now"
                            // answer "not my job" (the kernel arbitrates — this
                            // can never steal from a live holder). Still held
                            // elsewhere → report the role; the seed-leg role's
                            // holder runs its seed pass and reports its slots,
                            // any other runtime runs nothing.
                            retry_seed_legs(election, principal, &mut seed_legs).await;
                            if !role.is_holder() {
                                match election.try_acquire().await {
                                    ElectionOutcome::Held(lock) => {
                                        tracing::info!(
                                            "account runtime: engine singleton acquired \
                                             (reconcile-now) — pumping"
                                        );
                                        role = EngineRole::Holder { _lock: Some(lock) };
                                        cycles.set_holder(true);
                                    }
                                    ElectionOutcome::Refused | ElectionOutcome::Degraded(_) => {
                                        let skipped = PumpReport {
                                            skipped_non_holder: true,
                                            ..PumpReport::default()
                                        };
                                        if !seed_legs.is_holder() {
                                            let _ = reply.send(skipped);
                                            continue;
                                        }
                                        let Ok(report) = run_seed_pass!("seed reconcile-now")
                                        else {
                                            // As below: the pass was dropped
                                            // for a reassembly, and the caller
                                            // retries.
                                            let _ = reply.send(PumpReport {
                                                stale_writer: true,
                                                ..skipped
                                            });
                                            return ServeEnd::Reassemble;
                                        };
                                        let reason = seed_pass_reassembly!(&report);
                                        let _ = reply.send(report);
                                        if let Some(reason) = reason {
                                            tracing::info!(
                                                "account runtime: reassembling — {reason} \
                                                 (seed pass)"
                                            );
                                            return ServeEnd::Reassemble;
                                        }
                                        continue;
                                    }
                                }
                            }
                            (content_scopes, answered) =
                                derive_scopes(actor, &settings.memberships, registered, channels);
                            let facts = endpoint_facts.clone();
                            let inputs = PassInputs {
                                content_scopes: &content_scopes,
                                own_scopes: &own_scopes,
                                membership_answered: answered,
                                endpoint_facts: facts.as_ref(),
                            };
                            let outcome = contained_pump(
                                "reconcile-now",
                                Some(cycles),
                                sign_out,
                                &mut drive!(),
                                pump(store, planes, data_rpc, writer!(), &mut *legs, &inputs),
                            )
                            .await;
                            let Ok(report) = outcome else {
                                // A local write served inside this pass met a
                                // rotated writer: the pass was dropped for the
                                // reassembly, and the caller retries (the
                                // flag's documented meaning).
                                let _ = reply.send(PumpReport {
                                    stale_writer: true,
                                    ..PumpReport::default()
                                });
                                return ServeEnd::Reassemble;
                            };
                            let allow_removed_heal = removed_heal_allowed!(&report);
                            let reason =
                                reassembly_reason(&report, allow_removed_heal, !*burnt_heal_taken);
                            let burnt = walk_found_burnt(&report);
                            let _ = reply.send(report);
                            if let Some(reason) = reason {
                                *burnt_heal_taken |= burnt;
                                tracing::info!("account runtime: reassembling — {reason}");
                                return ServeEnd::Reassemble;
                            }
                        }
                        // The explicit pass barrier: answered here, between
                        // passes, after every command parked before it.
                        Cmd::Settled { reply } => {
                            let _ = reply.send(());
                        }
                        Cmd::RetireEnrollment { reply } => {
                            // Between passes by construction — commands are
                            // served one at a time on this loop — so no pass
                            // is mid-flight when the principal's sessions die
                            // with its grant. Bounded here, on the worker,
                            // so a sign-out against an unreachable nest never
                            // holds the store open past the budget.
                            //
                            // The plane leg FIRST (charter § The generation
                            // machinery → *Fleet-scope reclamation*, clause
                            // (4)): this device's own `Removed` row and the
                            // retirement of its own rows ride the session and
                            // key the grant revoke is about to end. Inside the
                            // same budget; best-effort like the revoke.
                            let retire = async {
                                // The stamp BEFORE the row: the severance
                                // below writes this machine's own `Removed`
                                // row, which a seed-holding runtime on this
                                // store — a sibling, or this one in a pass
                                // between here and its shutdown — would read
                                // as a removal and mint a successor on
                                // (`EnrollmentPass::SignedOut` owns why that
                                // must not happen).
                                let own = fauna_account_store::types::WriterId(
                                    fleet_writer.key.verifying_key().to_bytes(),
                                );
                                if let Err(e) = store.mark_writer_signed_out(&own).await {
                                    tracing::warn!(
                                        "sign-out: the store's sign-out stamp did not land \
                                         ({e:#}) — a second signed-in app on this store may \
                                         read the severance as a removal"
                                    );
                                }
                                match crate::generation_reclaim::sever_self(
                                    store,
                                    planes.fleet,
                                    fleet_writer.key,
                                )
                                .await
                                {
                                    Ok(p) => tracing::info!(
                                        retired = p.retired,
                                        deferred = p.deferred,
                                        "sign-out: severed this device on the fleet plane"
                                    ),
                                    Err(e) => tracing::warn!(
                                        "sign-out: the fleet-plane severance did not land \
                                             ({e:#}) — the enrollment stays until removed from \
                                             the devices page"
                                    ),
                                }
                                retire_enrollment(fleet_writer).await
                            };
                            let outcome = tokio::select! {
                                biased;
                                outcome = retire => outcome,
                                () = fauna_sleep::sleep(ENROLLMENT_RETIRE_BUDGET) => {
                                    EnrollmentRetirement::Deferred(format!(
                                        "the nest did not answer within {}s",
                                        ENROLLMENT_RETIRE_BUDGET.as_secs()
                                    ))
                                }
                            };
                            let _ = reply.send(outcome);
                        }
                        // The let-go (`crate::generation_let_go`): between
                        // passes, so the dead read never races a pass's own
                        // retires and re-seals.
                        Cmd::DeadGenerations { reply } => {
                            let _ = reply.send(
                                crate::generation_let_go::dead_generations(
                                    store,
                                    planes.fleet,
                                    fleet_writer.trust,
                                    fleet_writer.key,
                                )
                                .await,
                            );
                        }
                        Cmd::LetGo { generations, reply } => {
                            let _ = reply.send(
                                crate::generation_let_go::let_go(
                                    store,
                                    planes.fleet,
                                    fleet_writer.trust,
                                    fleet_writer.key,
                                    &generations,
                                )
                                .await,
                            );
                        }
                        Cmd::Shutdown { reply } => {
                            // Carried out to the host, NOT answered here: the
                            // host replies once this serve's locals — the
                            // engine-singleton lock above all — have dropped
                            // (`ServeEnd::Shutdown` owns why the ordering is
                            // the contract rather than a detail).
                            return ServeEnd::Shutdown(reply);
                        }
                        // Every local variant was taken by the guard arm above.
                        _ => unreachable!(
                            "a local command reached the pass-bound server — \
                             `Cmd::is_local` and this match disagree"
                        ),
                    }
                }

                Wake::Nudge(scope) => {
                    // Drain-and-dedup: a burst of nudges costs one walk per
                    // distinct scope.
                    let mut scopes = BTreeSet::from([scope]);
                    while let Ok(more) = nudge_rx.try_recv() {
                        scopes.insert(more);
                    }
                    // Walks are pump work. A non-holder drops the burst (after
                    // draining it, so the channel never backs up): its data
                    // arrives through the shared store, which the holder's own
                    // nudge chain keeps fresh — the `data_version` floor is
                    // how a non-holder notices, never a walk of its own.
                    if !role.is_holder() {
                        tracing::debug!("account pump: nudge dropped — not the engine singleton");
                        continue;
                    }
                    // A nudge can name a scope this replica joined since the last
                    // pass, so the set is re-derived before the lookup — otherwise
                    // a freshly joined channel's own wake would be dismissed as
                    // "untracked scope" until the backstop caught up.
                    // Walk-only: a nudge never runs the departure step, so this
                    // arm drops the affirmativeness on the floor rather than
                    // carrying it (every pump site derives its own). The set is
                    // refreshed so a just-joined scope's own wake is not
                    // dismissed, but deleting on a single-scope wake would judge
                    // memberships from a signal that says nothing about them —
                    // departures ride full passes, the same cadence rule as the
                    // seen-set producer.
                    (content_scopes, _) =
                        derive_scopes(actor, &settings.memberships, registered, channels);
                    for scope in scopes {
                        // Pump work, so it is driven and cut like any pass.
                        let label = format!("nudge {scope}");
                        let walked = drive_pass(
                            &label,
                            sign_out,
                            &mut drive!(),
                            walk_one_scope(store, planes, data_rpc, &content_scopes, &scope),
                        )
                        .await;
                        match walked {
                            Driven::Done(Err(e))
                                if fauna_account_store::store::is_stale_writer(&e) =>
                            {
                                tracing::info!(
                                    "account runtime: writer rotated under this process — \
                                     reassembling to adopt the successor (nudge {scope})"
                                );
                                return ServeEnd::Reassemble;
                            }
                            Driven::Done(Err(e)) => {
                                tracing::warn!("account pump (nudge {scope}): {e:#}")
                            }
                            Driven::Done(Ok(())) | Driven::Panicked => {}
                            Driven::Cut => break,
                            Driven::Reassemble => return ServeEnd::Reassemble,
                        }
                    }
                }

                Wake::Reconnect(changed) => {
                    match changed {
                        // A non-holder consumes the signal without pumping (its
                        // own connection's push `seq` reset changes nothing it
                        // walks); the holder re-publishes and re-walks, and the
                        // seed-leg role's holder beside it runs its seed pass.
                        Ok(()) if role.is_holder() => {
                            (content_scopes, answered) =
                                derive_scopes(actor, &settings.memberships, registered, channels);
                            let facts = endpoint_facts.clone();
                            let inputs = PassInputs {
                                content_scopes: &content_scopes,
                                own_scopes: &own_scopes,
                                membership_answered: answered,
                                endpoint_facts: facts.as_ref(),
                            };
                            let outcome = contained_pump(
                                "reconnect",
                                Some(cycles),
                                sign_out,
                                &mut drive!(),
                                pump(store, planes, data_rpc, writer!(), &mut *legs, &inputs),
                            )
                            .await;
                            let Ok(report) = outcome else {
                                return ServeEnd::Reassemble;
                            };
                            log_pump("reconnect", &report);
                            let allow_removed_heal = removed_heal_allowed!(&report);
                            if let Some(reason) =
                                reassembly_reason(&report, allow_removed_heal, !*burnt_heal_taken)
                            {
                                *burnt_heal_taken |= walk_found_burnt(&report);
                                tracing::info!("account runtime: reassembling — {reason}");
                                return ServeEnd::Reassemble;
                            }
                        }
                        Ok(()) => {
                            seed_pass_if_held!("seed reconnect");
                        }
                        Err(_) => *reconnects = None, // sender gone — disarm
                    }
                }

                Wake::PushesClosed => *pushes = None, // broker gone — disarm

                // A non-holder learns of the registration only through the
                // shared slot, where the holder's pass records it.
                Wake::GrantLatch => {
                    if slot.grant_registration_row().is_some() {
                        open_grant_gate(grant_registered);
                    } else {
                        latch_backoff.grow();
                        latch_poll.set(fauna_sleep::sleep(latch_backoff.ceiling()));
                    }
                }

                Wake::Tick => {
                    // The backstop tick doubles as the non-holder's re-election
                    // cadence (T9: no timer of its own). On a win — the previous
                    // holder exited — the first pass is the new holder's
                    // catch-up, its prologue-equivalent. The seed-leg role is
                    // re-tried on the same cadence, and its holder runs its
                    // seed pass on every tick the engine role is someone
                    // else's.
                    //
                    // It is also the removed-heal's cadence: a reassembly
                    // whose probe rotated nothing is tried again from this
                    // tick's pass, full or seed, and from no pass before it.
                    *removed_heal_taken = false;
                    retry_seed_legs(election, principal, &mut seed_legs).await;
                    let label = if role.is_holder() {
                        "ticker"
                    } else {
                        match election.try_acquire().await {
                            ElectionOutcome::Held(lock) => {
                                tracing::info!(
                                    "account runtime: engine singleton acquired \
                                     (previous holder exited) — pumping"
                                );
                                role = EngineRole::Holder { _lock: Some(lock) };
                                cycles.set_holder(true);
                                "acquired"
                            }
                            ElectionOutcome::Refused => {
                                seed_pass_if_held!("seed ticker");
                                continue;
                            }
                            // Unlike at start, a re-try degrade stays a
                            // non-holder: a refusal already proved arbitration
                            // works on this directory and a holder was live, so
                            // an I/O blip here is no license to double-pump —
                            // the next tick re-tries.
                            ElectionOutcome::Degraded(e) => {
                                tracing::warn!(
                                    "account runtime: engine-singleton re-election \
                                     degraded ({e}) — staying a plain reader/writer"
                                );
                                seed_pass_if_held!("seed ticker");
                                continue;
                            }
                        }
                    };
                    (content_scopes, answered) =
                        derive_scopes(actor, &settings.memberships, registered, channels);
                    let facts = endpoint_facts.clone();
                    let inputs = PassInputs {
                        content_scopes: &content_scopes,
                        own_scopes: &own_scopes,
                        membership_answered: answered,
                        endpoint_facts: facts.as_ref(),
                    };
                    let outcome = contained_pump(
                        label,
                        Some(cycles),
                        sign_out,
                        &mut drive!(),
                        pump(store, planes, data_rpc, writer!(), &mut *legs, &inputs),
                    )
                    .await;
                    let Ok(report) = outcome else {
                        return ServeEnd::Reassemble;
                    };
                    log_pump(label, &report);
                    let allow_removed_heal = removed_heal_allowed!(&report);
                    if let Some(reason) =
                        reassembly_reason(&report, allow_removed_heal, !*burnt_heal_taken)
                    {
                        *burnt_heal_taken |= walk_found_burnt(&report);
                        tracing::info!("account runtime: reassembling — {reason}");
                        return ServeEnd::Reassemble;
                    }
                }
            }
        }
    }
}

/// The wake the serve loop picked: a command (parked or fresh), the channel's
/// close, or one of the pump's three wake sources (a nudge arrives either
/// through the handle or through the pump's own push arm — one variant).
enum Wake {
    Cmd(Cmd),
    /// Every handle dropped.
    Closed,
    Nudge(String),
    /// The session's push broker closed — the push arm disarms.
    PushesClosed,
    Reconnect(std::result::Result<(), watch::error::RecvError>),
    Tick,
    /// A non-holder's re-read of the slot's registration latch is due.
    GrantLatch,
}

/// Which plane scope a push wakes, if any — the ONE push→nudge mapping every
/// seat runs (`account-data-plane.md` § The client-side lifecycle, the pump
/// bullet's wake source (1); `account-sync-plane.md` § Nudges and backstops).
///
/// One arm: a **scope-tagged** `fauna.sync.changed` — the tag names the plane
/// scope whose feed advanced, and exactly that scope wakes.
///
/// Every other push — an untagged `fauna.sync.changed` (a folder nudge: its
/// `folder` is the sync engine's, not a plane scope), any other kind — wakes
/// nothing.
pub fn nudge_scope_for_push(event: &PushEvent) -> Option<&str> {
    let PushEvent::SyncChanged(payload) = event else {
        return None;
    };
    payload.scope.as_deref()
}

/// The pump's walk set: the derived scopes (`scope_set`) plus whatever an app
/// registered explicitly, which a re-derivation must never drop.
///
/// `channels` carries the last membership answer across calls — see
/// [`MembershipSource`] for why a `None` reading keeps it rather than clearing
/// it. Called before every pass, so a channel joined or left between passes
/// changes the set with no notification path of its own.
///
/// The returned flag is whether the membership source **answered
/// affirmatively on this call**. It exists for the departure seam
/// ([`crate::departure`]), which may delete data and so must never act on a
/// held-over set: for the walk it makes no difference whether the answer was
/// fresh or remembered, but for a deletion it is the whole question.
pub(crate) fn derive_scopes(
    actor: Option<[u8; 32]>,
    memberships: &Option<MembershipSource>,
    registered: &[ContentScope],
    channels: &mut Vec<[u8; 32]>,
) -> (Vec<ContentScope>, bool) {
    let mut answered = false;
    if let Some(source) = memberships
        && let Some(fresh) = source()
    {
        *channels = fresh;
        answered = true;
    }
    let mut scopes = match actor {
        Some(actor) => derive_content_scopes(actor, channels).unwrap_or_else(|e| {
            // Unreachable for the kind tags in `scope_set` (compile-time
            // constants of valid shape); a future kind with a malformed tag
            // lands here rather than panicking on the store thread.
            tracing::warn!("account runtime: content-scope derivation refused ({e})");
            Vec::new()
        }),
        None => Vec::new(),
    };
    for scope in registered {
        if !scopes.contains(scope) {
            scopes.push(scope.clone());
        }
    }
    (scopes, answered)
}
