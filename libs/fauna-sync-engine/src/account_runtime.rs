//! The client-side account-store lifecycle's **native host** (W3
//! (account-data-plane.md § Workstreams)): one dedicated store thread per
//! (process, account), running the account driver over the SQLite store and
//! serving every other part of the app through the driver's `Send` handle.
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § The account
//! store → *The client-side lifecycle* (the six W3 rulings this module is the
//! first artifact of; ruling (2) of *The trigger fired* — the pump split — is
//! why the driver is not here), § Nudges and backstops (the pump's wake
//! sources), § Multi-instance concurrency (the engine-singleton election
//! this host takes with `engine.lock`).
//!
//! # The split (ruling (2), 2026-09-28)
//!
//! **The driver — the command service, the pass, the serve loop — is
//! `fauna_account_plane::account_driver`**, platform-generic and compiled for
//! wasm32; every public name it had here is re-exported below at its old
//! path, so `fauna_sync_engine::account_runtime::AccountStoreHandle` and its
//! siblings read unchanged. **This module is what the driver's seams name
//! native**: the store thread and its current-thread tokio runtime, the T10
//! credential slot (`principal_bundle::PrincipalSlot`, over
//! `fauna-credential-store`), the succession probe and the lost-slot heal
//! (`principal_succession`), the SQLite store, the engine-singleton file lock
//! ([`FileElection`]), and the two legs the pass runs between the fleet walk
//! and the device-endpoints step — the peer leg and the custody leg
//! ([`NativeLegs`]). Web hosts the same driver as a `spawn_local` task over
//! the IndexedDB store, with no legs.
//!
//! # Why a dedicated thread
//!
//! The sqlite [`StoreBackend`]'s AFIT futures are not `Send` — the same fact
//! that put the file-sync engines on `EngineHost`'s dedicated worker thread
//! and gave `fauna-peer-sync` its `ServeStoreHandle`. So the store connection
//! lives on one named OS thread with a current-thread runtime, and everything
//! the rest of the app does with the store crosses [`AccountStoreHandle`]'s
//! command channel. That is an in-process channel, not IPC — R2 (account-data-plane.md § The ratified decisions)'s "every
//! reader reads the store" holds; under W5's multi-process law each process
//! runs its own thread + connection over WAL.
//!
//! # The assembly/serve cycle
//!
//! One iteration of [`worker`]'s `'assembly` loop = assemble + one
//! [`AccountDriver::serve`]. A typed `StaleWriter` refusal anywhere in the
//! serve phase re-enters the loop: the writer was rotated away under this
//! process (principal succession, charter § The store device principal →
//! succession decision 4), and the heal is to re-resolve the successor from
//! the shared slot and rebuild everything that cached the old identity — the
//! store handle, both planes, the peer leg, the fleet bootstrap rows. The
//! driver's own state (the app-fed registrations, the parked commands, the
//! heal caps) survives every iteration.
//!
//! # The writer key and the T10 slot
//!
//! The store's writer identity must never change once minted — the journal's
//! equivocation refusal is keyed on it — so the first assembly for an account
//! on a machine mints an Ed25519 device keypair and persists its secret in
//! the T10 credential slot ([`CRED_NAMESPACE`], account attribute = the actor
//! id hex; charter § The store device principal owns the slot's mechanics and
//! its W5 growth). A slot that exists but cannot be read is a **hard error**,
//! never a re-mint: an unreadable value may be a recoverable key, and
//! overwriting it would abandon it for good. An EMPTY slot over a store that
//! already has a writer is a different matter since 2026-08-27 — the
//! **lost-slot self-heal** (charter § The store device principal, refinement
//! 10; `principal_succession::lost_slot_heal`): the mint proceeds as for a
//! fresh machine, and the assembly then fences the store from its stamped
//! writer onto the minted key — the old writer retired, its un-pushed tail
//! re-authored under the new one — instead of having
//! [`AccountStore::open`]'s identity check refuse the store for good. Nothing
//! is deleted: deleting the replica directory is no longer a recovery step
//! for a lost slot, and it would cost exactly the unpublished local rows the
//! heal preserves.

use std::time::Duration;

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::{
    backend::StoreBackend,
    locks::{
        EngineLock, EngineLockOutcome, MigrationLock, MigrationLockOutcome, SeedLegLock,
        SeedLegLockOutcome,
    },
    sqlite::SqliteBackend,
    store::AccountStore,
    types::WriterId,
};
use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorId;
use fauna_credential_store::CredentialStore;
use fauna_protocol::{KeyedRpcRequester, PushEvent, RpcErrorClass};
use tokio::sync::{broadcast, oneshot, watch};

use crate::attested_predecessors::AttestedPredecessors;
use crate::peer_leg::PeerLegState;
pub use crate::peer_leg::{
    DialPass, PeerLegBinding, PeerLegFactoryInputs, PeerLegPass, PeerTransportFactory,
};
use crate::principal_bundle::PrincipalSlot;
use crate::principal_succession::WriterKeyProvenance;
// The suite below drives the driver through this host, and names the plane
// types the driver's own code used to import here (`use super::*`).
/// The driver — every public name it had here, at its old path
/// (`fauna_account_plane::account_driver` owns them since the pump split).
pub use fauna_account_plane::account_driver::{
    ACCOUNT_HANDLE_WAIT, AccountDriver, AccountHandleSource, AccountMailStore, AccountStoreAccess,
    AccountStoreHandle, Assembly, CapabilitySweep, DEFAULT_BACKSTOP_INTERVAL, DriverConfig,
    ENROLLMENT_RETIRE_BUDGET, ElectionOutcome, EngineElection, EngineRole, EnrollmentPass,
    EnrollmentRefusal, EnrollmentRetirement, FleetBootstrapRows, GroupCeremonyAuthority, HostLegs,
    LegsCtx, LegsOutput, LinkedNestConnector, MembershipSource, NoLegs, OwedDeliveries,
    OwedNestDeliverer, PassTimings, PrincipalCustody, PumpCycles, PumpCyclesView, PumpReport,
    RUNTIME_ABSENT, RuntimePrincipal, SIGN_OUT_PASS_GRACE, SeatAccountStore, ServeEnd,
    TrustedHolderSource, elect_at_start, fixed_holders, fleet_bootstrap, fleet_bootstrap_rows,
    nudge_scope_for_push, push_step_error, r14_trust,
};
#[cfg(test)]
use {
    crate::device_endpoints_writer::EndpointFacts,
    crate::observation_intake::{Observation, ObservationOutcome},
    fauna_account_plane::account_driver::RUNTIME_GONE,
    fauna_account_store::types::{IntentDrainer, StateEntry},
    fauna_core::fleet_removal::{FleetRemovalRefusal, NestDeletion, PendingFleetRemoval},
    fauna_protocol::RpcRequester,
    fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE},
    fauna_protocol::merge_policy::{KIND_DEVICE_SET, LwwStamp},
    fauna_protocol::scope::ContentScope,
    std::collections::BTreeSet,
};

/// The T10 credential-slot namespace (charter § The store device principal:
/// "the slot is `libs/fauna-credential-store` under one shared namespace keyed
/// by account").
///
/// Owned by `fauna-credential-store` and re-exported here under the name every
/// writer already imports. It moved there when the sign-out erase became its
/// second consumer: the erase must sweep exactly the namespace this writes, and
/// two literals in step is the shape that let all five slots survive a sign-out
/// in the first place (`long-term-store.md` § Cleanup contract).
pub use fauna_credential_store::ACCOUNT_STORE_NAMESPACE as CRED_NAMESPACE;

/// How the store dir stays out of the platform's cloud backup — the same
/// three-arm statement the custodian store demands, re-exported because the
/// account store is its second consumer ([`AccountRuntimeParams::store_backup_exclusion`]).
pub use crate::custodian_store::CloudBackupExclusion;
/// The store's directory under the per-actor state dir — owned by the store
/// crate's `root` module since W6 (placement is the store's own concern);
/// re-exported so existing paths keep working.
pub use fauna_account_store::root::STORE_SUBDIR;
/// The per-user store root (W6 path unification): every desktop surface
/// resolves [`StoreRoot::platform`], so co-located processes share ONE store
/// dir per account — which is what makes the machine-shared writer-key slot
/// correct (the ⚠ Gap entry, `account-data-plane.md` § Implementation status
/// today).
pub use fauna_account_store::root::StoreRoot;

/// The store thread's stack. Stated, never std's 2 MiB spawn default: the whole
/// serve future is held inline in [`worker`]'s `block_on` and every pass is
/// polled from it, so an unoptimized build's poll frames run deep. Measured
/// 2026-10-01 on a debug `fauna-desktop`: a non-holder's seed pass (the
/// linked-nest leg under it) aborted the app on 2 MiB and ran on 3 MiB, with
/// that pass's future already boxed. 8 MiB is a main thread's stack, reserved
/// and not committed. The remedy `native-async-execution.md` § The rule keeps
/// for a path that stays large after boxing.
///
/// **An unoptimized build is what needs it.** The futures are small (53,504
/// bytes); the frames are not: `AccountDriver::serve`'s poll frame is 865,456
/// bytes on a debug build and 35,104 on a release one, and a debug app's full
/// pass reaches 1.96 MiB of stack, 2.09 MiB when it custodies a linked box.
/// The numbers, and how they were read, are in `account-runtime.md`
/// § Implementation status today;
/// `tests::a_pass_runs_inside_half_the_store_threads_stack` goes red if this
/// returns to the default, and so does the nest conformance suite's seed pass
/// with a linked nest under it (`conformance_account_plane_bind`).
const STORE_THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

#[cfg(any(test, feature = "test-helpers"))]
thread_local! {
    /// An address at the store thread's entry — zero on every other thread.
    /// What [`store_thread_stack_depth`] measures from.
    static STORE_THREAD_STACK_BASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The address of one of this function's own locals: where the caller's stack
/// is, to within this one small frame.
#[cfg(any(test, feature = "test-helpers"))]
#[inline(never)]
fn stack_address() -> usize {
    let here = 0u8;
    std::hint::black_box(std::ptr::from_ref(&here)).addr()
}

/// How far below the store thread's entry the caller's stack is, in bytes —
/// `None` on any thread that is not a store thread. A test double's request
/// calls it from inside its own poll, which is how a suite reads the depth a
/// pass reaches (`tests::a_pass_runs_inside_half_the_store_threads_stack`
/// here; `bins/fauna-nest`'s `conformance_account_plane_bind` for the
/// linked-nest leg). Test builds only: no ordinary build marks the thread's
/// entry.
#[cfg(any(test, feature = "test-helpers"))]
pub fn store_thread_stack_depth() -> Option<usize> {
    let base = STORE_THREAD_STACK_BASE.get();
    (base != 0).then(|| base.saturating_sub(stack_address()))
}

/// The bound those suites hold a pass's requests to: half of
/// [`STORE_THREAD_STACK_BYTES`]. Half, because a request is not the deepest
/// leaf — the store's SQLite calls, the crypto under a mint and a local
/// command served inside a pass all sit outside what a request can observe.
#[cfg(any(test, feature = "test-helpers"))]
pub const STORE_THREAD_STACK_BUDGET: usize = STORE_THREAD_STACK_BYTES / 2;

/// Production credential slot for the writer key, honoring the shared env
/// contract (`FAUNA_KEYRING_APP` override, `FAUNA_E2E_CREDENTIAL_DIR` file
/// backend) so e2e runs never touch a developer's real keyring — the sync
/// agent's `production_store` shape.
///
/// **On the phones this is the app's own secure store.** iOS and android have
/// no `fauna-credential-store` arm of their own; the slot there is the
/// platform store the app lends over the foreign seam at launch
/// (`fauna_credential_store::install_foreign_store`, reached from the apps
/// through `fauna-ffi`'s `install_platform_credential_store`), and the writer
/// key lands as a namespace-prefixed row (`fauna-account-store/<actor hex>`)
/// beside the identity it was minted for. A phone whose shell never lent its
/// store falls through to the inert arm, whose dropped write
/// [`writer_key_from_slot`]'s read-back refuses — loudly, as before 2026-08-26
/// (`apps/common.md` § Credential storage → *The shared Rust credential slots
/// on the phones*).
pub fn production_credential_store() -> CredentialStore {
    CredentialStore::new(CRED_NAMESPACE)
}

/// One reconnect watch out of two: the returned receiver changes whenever
/// either input does — the app session's watch and the data path's own
/// client's watch, when the runtime rides a store principal
/// ([`AccountRuntimeParams::process_rpc`]).
///
/// The two clients drop independently: a data-client reconnect the session
/// never saw must still wake the pump (its push `seq` reset is the data
/// path's), or a walk that failed on that drop waits for the backstop and
/// every gated read stays refused until then (`account-client-lifecycle.md`
/// § The client-side lifecycle (W3), the pump's reconnect arm).
///
/// A forwarding task, so the pump keeps its one reconnect slot. It stops when
/// the pump drops the merged receiver, or once both inputs' senders are gone
/// — and dropping the merged sender then disarms the pump's arm exactly as a
/// single closed watch does. A change on both inputs at once is coalesced
/// into one bump. Must be called inside a tokio runtime.
pub fn merge_reconnect_watches(
    mut session: watch::Receiver<u64>,
    mut data: watch::Receiver<u64>,
) -> watch::Receiver<u64> {
    let (tx, rx) = watch::channel(0u64);
    tokio::spawn(async move {
        let (mut session_open, mut data_open) = (true, true);
        while session_open || data_open {
            let changed = tokio::select! {
                () = tx.closed() => return,
                r = session.changed(), if session_open => {
                    session_open = r.is_ok();
                    r.is_ok()
                }
                r = data.changed(), if data_open => {
                    data_open = r.is_ok();
                    r.is_ok()
                }
            };
            if changed {
                // Coalesce: whichever input did not wake us may have moved too.
                if session_open {
                    session.borrow_and_update();
                }
                if data_open {
                    data.borrow_and_update();
                }
                tx.send_modify(|n| *n = n.wrapping_add(1));
            }
        }
    });
    rx
}

/// Everything [`AccountStoreRuntime::start`] needs. The caller is either a
/// seed-holding surface (R4) or the seedless agent — see
/// [`RuntimePrincipal`], which is the only assembly-visible difference.
pub struct AccountRuntimeParams<R> {
    /// Where the account store lives (W6 path unification): the per-user
    /// root the per-actor store dirs resolve under. Production desktop
    /// callers pass [`StoreRoot::platform`] — the one per-OS constant every
    /// app AND the sync agent share, which is what keeps co-located
    /// processes on ONE journal per account under the machine-shared writer
    /// key. [`StoreRoot::at`] is for sandboxed mobile shells (their
    /// container IS the per-user root) and tests.
    pub store_root: StoreRoot,
    /// How the per-actor store dir under [`store_root`](Self::store_root)
    /// stays out of the platform's **cloud** backup — stated by every caller,
    /// never defaulted, exactly as the custodian store demands of its root
    /// ([`CloudBackupExclusion`]'s own docs carry the three arms).
    ///
    /// **Why the account store must state it (2026-08-26).** Its writer key
    /// lives in the T10 credential slot, which on the phones is a
    /// `ThisDeviceOnly` keychain row (apple) / an `allowBackup=false` store
    /// (android) — deliberately excluded from device restore. A restored
    /// container that still carried the store would arrive stamped with a
    /// writer whose key did not travel; the next assembly would mint a fresh
    /// writer and the store would refuse it forever ("belongs to a different
    /// writer"), stranding the account plane on the new device with nothing
    /// on screen saying so. Excluding the store dir from the same backup the
    /// key is excluded from makes that state unrepresentable — the shape
    /// `nest/common.md` § Client-state recoverability prefers over
    /// detect-and-repair — and is android's manifest posture made explicit
    /// (`apps/common.md` § Credential storage → *The shared Rust credential
    /// slots on the phones*).
    ///
    /// Desktops state [`CloudBackupExclusion::platform_desktop`]; a sandboxed
    /// shell states its own arm beside the container it supplies. Applied on
    /// **every** assembly, after the dir exists and before anything opens it —
    /// idempotent, and what heals a dir whose exclusion a restore or copy dropped.
    pub store_backup_exclusion: CloudBackupExclusion,
    /// 64-hex actor id; validated by the state-dir floor.
    pub actor_id_hex: String,
    /// The nest requester — `Arc<NestClient>` in production. With
    /// [`process_rpc`](Self::process_rpc) present this carries only the
    /// enrollment ceremony's registration legs (they must ride an
    /// already-authenticated session — the grant they install is what the
    /// principal's own connection authenticates with); without it, everything.
    pub rpc: R,
    /// The **principal-authenticated** requester for the data path (W5.4b —
    /// charter § The store device principal): a client whose whole auth
    /// lifecycle is the store principal's per-process bearer, minted over
    /// `fauna.auth.device_handshake` with the writer key
    /// (`fauna_client::ws_device_handshake_bearer::device_principal_nest_client`,
    /// over the writer key from [`resolve_writer_key_serialized`]). When
    /// present, every plane/walk/outbox/config leg rides it — the runtime's
    /// nest leg then never authenticates as the app's own session; on a fresh
    /// machine its first requests fail until the ceremony registers the grant
    /// (ordinarily the same pass), and the pump's per-pass retry absorbs
    /// exactly that window. `None` (tests; apps not yet migrated; W6 web)
    /// keeps everything on [`rpc`](Self::rpc).
    pub process_rpc: Option<R>,
    /// Who is assembling: a seed-holding app, or the seedless agent
    /// ([`RuntimePrincipal`]).
    pub principal: RuntimePrincipal,
    /// The T10 slot the writer key persists in. Production:
    /// [`production_credential_store`]; tests: a file-backend store on a
    /// tempdir (never the OS keyring — testing.md § point 10).
    pub credentials: CredentialStore,
    /// `NestClient::subscribe_reconnects()` in production; `None` disarms the
    /// reconnect wake (tests, or a requester with no reconnect signal).
    pub reconnects: Option<watch::Receiver<u64>>,
    /// `NestClient::subscribe_pushes()` in production — the pump's own
    /// push→nudge arm (`account-data-plane.md` § The client-side lifecycle,
    /// the pump bullet's wake source (1)): every `fauna.sync.changed` the
    /// session receives is mapped by [`nudge_scope_for_push`] and fed to the
    /// nudge channel, so no app hand-wires the arm. `None` disarms it (tests
    /// drive [`AccountStoreHandle::nudge_scope`] directly, or a requester
    /// with no push source). Best-effort like every nudge: a lagged receiver
    /// skips, a closed one disarms, and the backstop ticker is the
    /// correctness path either way.
    pub pushes: Option<broadcast::Receiver<PushEvent>>,
    /// Backstop reconcile cadence — [`DEFAULT_BACKSTOP_INTERVAL`] in
    /// production; tests shrink it under a paused clock.
    pub backstop_interval: Duration,
    /// Where the member half of the content-scope set comes from
    /// ([`MembershipSource`]). `None` — the shape before any app wired one —
    /// means this replica walks its own-actor scopes only.
    pub memberships: Option<MembershipSource>,
    /// Escrow-holder identities whose receipts this account accepts — the
    /// trust half of the R14 writer door (build step 7,
    /// [`crate::generation_tip::GenerationTrust::trusted_holders`]).
    ///
    /// **The app supplies it because trust comes from the PIN, never from the
    /// nest's own say-so:** v1's holder is the user's nest, and the identity
    /// clients already pin for it is
    /// `fauna_anon_client::trust::pinned_identity(&authority_of(nest_url))` —
    /// TOFU state the connector owns, which this crate deliberately does not
    /// reach into (asking the live nest who it is would let an impostor
    /// nominate itself as its own escrow holder). A caller with no pin yet
    /// passes an empty vec, which is fail-safe: no receipt verifies as
    /// trusted, so no tip resolves and `GenerationTip` sealing stays refused
    /// with the precise no-tip error.
    ///
    /// A source, not a value: the runtime re-reads it at the start of every
    /// pass ([`TrustedHolderSource`]), so a rotation the app accepts moves
    /// the trusted holder without a restart.
    ///
    /// `root` is deliberately NOT a parameter — it is this runtime's own
    /// actor. The `prior` half is [`attested_predecessors`](Self::attested_predecessors).
    pub trusted_escrow_holders: TrustedHolderSource,
    /// The account's **attested** succeeded-from identities — the `prior` half
    /// of the R14 writer-door trust
    /// ([`crate::generation_tip::GenerationTrust::prior`]), the
    /// succession-crossing signer allow-list an enrollment cert verifies
    /// against (`account-data-taxonomy.md` § The generation machinery → *The
    /// source of `prior`*, ruled 2026-09-13).
    ///
    /// **The app supplies it because attestation is possession, and only the
    /// app's account registry knows what this device possesses:** the ids of
    /// `AccountRegistry::predecessor_backup_keys_by_actor` — the rows whose
    /// seeds this device holds, each written by this device's own ceremony or
    /// restored out of the successor's own escrow container, so a seed derives
    /// its id and the list cannot name an identity the owner never had.
    /// Until 2026-09-13 this crate read `prior_actor_ids` off the
    /// device-local `__config` replica instead — a writer-asserted list the
    /// aftermath carries across a succession with no mark plane — so an id
    /// planted in a predecessor-sealed replica became signer trust on the
    /// successor's account. That read is gone: the
    /// replica is consulted by no trust consumer, not even to intersect.
    ///
    /// The seedless host (the sync agent) holds no registry; it receives the
    /// same list from the identity-holding app over the additive
    /// `SyncCapability::predecessor_actor_ids` and passes it here. A caller
    /// with nothing attested passes an empty vec, which is fail-**safe**, not
    /// fail-open: enrollment certs signed only by a predecessor identity then
    /// fail to verify, dropping those devices out of the fleet view (and tips
    /// naming them go inadmissible) rather than admitting anything extra.
    ///
    /// **The same value carries each identity's generation-0 delegable
    /// schedule**, which the delegable scope's walk opens that identity's
    /// rows under to carry them across the succession
    /// (`succession-aftermath.md` § Re-key scope). One value with one
    /// constructor ([`AttestedPredecessors::from_backup_keys`]) so a host
    /// cannot attest an identity and hand over no schedule for it.
    pub attested_predecessors: AttestedPredecessors,
    /// The peer-leg transport factory (W5.7 — `crate::peer_leg` owns the
    /// seam's whole story): invoked by the elected engine-singleton with the
    /// assembly's own resolved writer key, once every gate is open (witness
    /// in the slot, `peer-sync` brake advertised). `None` — web, tests, apps
    /// not yet wired — keeps the peer leg structurally off. A factory rather
    /// than a built transport so a non-holder never constructs a same-NodeId
    /// endpoint at all (the module docs own the refinement record).
    pub peer_transport: Option<PeerTransportFactory>,
    /// **Which `sync_devices` row this machine is** — the hex device id the
    /// enrollment ceremony registers the store principal's grant on: the
    /// machine's **named** row, the app's own derived device id
    /// (`sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
    /// block, decision 3). One row per machine, the one the user recognises,
    /// so their delete on it structurally ends the machine.
    ///
    /// Required, and unconditional: the store principal is the machine's only
    /// renewal credential, so there is no second credential on that row an
    /// enrollment could displace and no gate to run. The id the agent presents
    /// as its `sync_device_id` is the one the app pushed, so the two halves of
    /// `this_device_row`'s *enrolled wins* rule always agree.
    pub enrollment_target_device_id: String,
    /// The secondary leg's connector (`account-sync-plane.md` § The bind leg,
    /// ruling 4): how this seed-holding runtime opens an owner-authenticated
    /// connection to each linked nest whose pairing carries the
    /// `account_replica` capability, and reads the identity that connection is
    /// bound to ([`LinkedNestConnector`]). Production seed-holding hosts build
    /// it over a second `NestClient` under the same identity
    /// (`fauna_client_account_runtime::build_params`); `None` runs no
    /// secondary leg, and the seedless agent runs none whatever it is handed.
    pub linked_nests: Option<LinkedNestConnector<R>>,
    /// The road's deliverer (`identity-succession.md` § Enforcement on the
    /// home nest → *Every nest the identity is linked to*, **The road**): how
    /// this seed-holding runtime carries a succession statement to every nest
    /// its account is owed at ([`OwedNestDeliverer`]). Production seed-holding
    /// hosts build it over an anonymous client and a sign-in as each retired
    /// identity whose seed the device holds
    /// (`fauna_client_account_runtime::build_params`); `None` delivers
    /// nothing, and the seedless agent delivers nothing whatever it is handed.
    pub owed_nests: Option<OwedNestDeliverer<R>>,
}

/// The native half of [`AccountRuntimeParams`] — what the store thread's
/// assembly needs and the driver never sees (the store's placement, the
/// credential slot, the peer transport factory, the principal the slot is
/// resolved against).
struct HostParams<R> {
    store_root: StoreRoot,
    store_backup_exclusion: CloudBackupExclusion,
    actor_id_hex: String,
    rpc: R,
    process_rpc: Option<R>,
    principal: RuntimePrincipal,
    credentials: CredentialStore,
    peer_transport: Option<PeerTransportFactory>,
    enrollment_target_device_id: String,
    linked_nests: Option<LinkedNestConnector<R>>,
    owed_nests: Option<OwedNestDeliverer<R>>,
    /// The attested predecessors — the driver's trust half, and the host's
    /// succession-rider carriage source at assembly.
    attested_predecessors: Vec<ActorId>,
}

/// Namespace-level entry point: [`Self::start`] is the whole surface.
pub struct AccountStoreRuntime;

impl AccountStoreRuntime {
    /// Assemble and start the lifecycle: spawn the store thread, open (or
    /// create) the store under the per-actor placement, mint-or-load the
    /// writer key from the T10 slot, run the pump. Returns once the thread
    /// has the store open — an unopenable store or unreadable slot errors
    /// here, not later.
    ///
    /// A start never fails on a lost engine-singleton election (module docs
    /// § The engine-singleton election): a non-holder comes up as a plain
    /// reader/writer of the shared store, and `start` reports nothing about
    /// the role — [`PumpReport::skipped_non_holder`] on a `reconcile_now` is
    /// the in-band answer for the rare caller that needs one.
    ///
    /// **The readiness barrier ends at assembly** (store open, writer key,
    /// fleet bootstrap) — deliberately, so a start never blocks on a network
    /// pass and an offline device comes up regardless. The prologue pass runs
    /// *after* ready is sent, so a caller that returns from `start()` and
    /// immediately mutates shared state **races that pass**. Callers needing
    /// the prologue behind them take the explicit barrier,
    /// [`AccountStoreHandle::settled`] (or a `reconcile_now`, which is
    /// pass-bound too). A local command's round trip — a
    /// [`AccountStoreHandle::get_preference`], say — proves nothing about the
    /// prologue: local commands are served *inside* a pass, at its yield
    /// points, which is what keeps a preference surface answering during a
    /// five-minute catch-up. Assuming a start implies a finished prologue is
    /// what made the V3 conformance test load-dependent.
    pub async fn start<R>(params: AccountRuntimeParams<R>) -> Result<AccountStoreHandle>
    where
        // Keyed, not plain: the pump's outbox drain replays intents whose
        // envelope must carry the stored intent id (`outbox::drain_outbox`).
        R: KeyedRpcRequester + Clone + Send + Sync + 'static,
        R::Error: RpcErrorClass,
    {
        let AccountRuntimeParams {
            store_root,
            store_backup_exclusion,
            actor_id_hex,
            rpc,
            process_rpc,
            principal,
            credentials,
            reconnects,
            pushes,
            backstop_interval,
            memberships,
            trusted_escrow_holders,
            attested_predecessors,
            peer_transport,
            enrollment_target_device_id,
            linked_nests,
            owed_nests,
        } = params;
        // The driver and its handle are minted HERE, before the thread: the
        // handle is what this returns, and the driver's state — the app-fed
        // registrations, the parked commands, the heal caps — outlives every
        // reassembly the thread runs.
        let (driver, handle) = AccountDriver::new(
            DriverConfig {
                actor_id_hex: actor_id_hex.clone(),
                backstop_interval,
                memberships,
                trusted_escrow_holders,
                attested_predecessors: attested_predecessors.clone(),
                enrollment_target_device_id: enrollment_target_device_id.clone(),
            },
            reconnects,
            pushes,
        );
        let host = HostParams {
            store_root,
            store_backup_exclusion,
            actor_id_hex,
            rpc,
            process_rpc,
            principal,
            credentials,
            peer_transport,
            enrollment_target_device_id,
            linked_nests,
            owed_nests,
            attested_predecessors: attested_predecessors.actor_ids().to_vec(),
        };
        let (ready_tx, ready_rx) = oneshot::channel::<Result<()>>();

        std::thread::Builder::new()
            .name("fauna-account-plane".into())
            .stack_size(STORE_THREAD_STACK_BYTES)
            .spawn(move || {
                #[cfg(any(test, feature = "test-helpers"))]
                STORE_THREAD_STACK_BASE.set(stack_address());
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("account runtime: build current-thread runtime")
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                rt.block_on(worker(host, driver, ready_tx));
            })
            .context("account runtime: spawn store thread")?;

        ready_rx
            .await
            .map_err(|_| anyhow::anyhow!("account runtime: store thread died during assembly"))??;
        Ok(handle)
    }
}

/// The sanctioned **pre-assembly** writer-key resolver (W5.4b): mint-or-load
/// the store's writer key, serialized under the store's migration/adoption
/// section exactly as the assembly itself does — the ONLY safe way to hold
/// the writer key before [`AccountStoreRuntime::start`] runs. An app calls
/// this to construct the runtime's principal-authenticated requester (the
/// writer key is the device-handshake signing key —
/// `AccountRuntimeParams::process_rpc`); W5.5's seedless agent will resolve
/// its principal the same way. The assembly then loads the same key
/// idempotently.
///
/// Never read the slot directly instead of calling this: an unserialized
/// mint-or-load is the exact race W5.3 measured (two cold processes each
/// minting a writer key, the loser's store refusing it forever).
pub fn resolve_writer_key_serialized(
    store_root: &StoreRoot,
    actor_id_hex: &str,
    credentials: &CredentialStore,
) -> Result<SigningKey> {
    let store_dir = store_root
        .store_dir(actor_id_hex)
        .context("resolve writer key: per-actor store dir")?;
    std::fs::create_dir_all(&store_dir)
        .with_context(|| format!("create store dir {}", store_dir.display()))?;
    let _section = match MigrationLock::acquire(&store_dir) {
        MigrationLockOutcome::Held(lock) => Some(lock),
        MigrationLockOutcome::Degraded(e) => {
            tracing::warn!(
                error = %e,
                "resolve writer key: migration lock unavailable — resolving \
                 unserialized (safe for the single-process case)"
            );
            None
        }
    };
    writer_key_from_slot(credentials, actor_id_hex).map(|(key, _)| key)
}

/// Mint-or-load the store's writer signing key from the T10 slot, saying
/// which of the two it did ([`WriterKeyProvenance`]) — the plane crate's one
/// resolver ([`fauna_account_plane::principal_bundle::mint_or_load_writer_key`],
/// shared with web's host), over this host's credential store.
fn writer_key_from_slot(
    credentials: &CredentialStore,
    actor_id_hex: &str,
) -> Result<(SigningKey, WriterKeyProvenance)> {
    fauna_account_plane::principal_bundle::mint_or_load_writer_key(credentials, actor_id_hex)
}

/// The native host's legs ([`HostLegs`]): the same-account peer leg
/// (`crate::peer_leg`) and the custody leg (`crate::custody_leg`), worker-owned
/// and rebuilt on every reassembly — the node identity derives from the
/// writer key, which is exactly what a rotation changed. The order inside is
/// the pump's own, unchanged by the split: the two withdrawal snapshots (so a
/// sibling's removal row merged this pass is already severing by the dial),
/// ensure-bound, the sibling dial, the custody serve refresh, the custodian
/// nest pull, the custodian dial + budget, the check-in receipts.
struct NativeLegs {
    peer: PeerLegState,
    custody: crate::custody_leg::CustodyLegState,
}

impl<B, R> HostLegs<B, R> for NativeLegs
where
    B: StoreBackend,
    R: KeyedRpcRequester + Clone + Sync,
    R::Error: RpcErrorClass,
{
    async fn run(&mut self, ctx: LegsCtx<'_, B, R>, report: &mut PumpReport) -> LegsOutput {
        let LegsCtx {
            store,
            fleet,
            trust,
            writer_key,
            slot,
            schedule,
            rpc,
            content_scopes,
            endpoint_facts,
        } = ctx;
        let peer = &mut self.peer;
        let custody = &mut self.custody;
        // The two withdrawal snapshots both peer-leg admission halves read
        // (`account-sync-plane.md` § The admission seam → *Validity and
        // severance*): revoked custody grants and removed fleet devices. Right
        // after the fleet walk, so a sibling's removal row merged THIS pass is
        // already severing by the dial below — and before the ensure step, so a
        // pass that binds reads sets derived this pass. That ordering is not
        // what keeps a first bind honest when a refresh FAILS, though: a failed
        // refresh leaves its snapshot as it was, which before any success is
        // underived, and every view over an underived snapshot refuses its arm
        // (`peer_leg::WithdrawalSnapshot`). After a success, a failure keeps the
        // last derived set in force until a later pass succeeds — the stated
        // bound, surfaced here as the step error.
        if let Err(e) = custody.refresh_revoked(store, &peer.revoked).await {
            push_step_error(report, format!("custody-revocation snapshot: {e:#}"), &e);
        }
        if let Err(e) = crate::peer_leg::refresh_removed_devices(peer, store, trust).await {
            push_step_error(report, format!("removed-device snapshot: {e:#}"), &e);
        }
        // The peer-leg ensure step (W5.7 — holder-only by placement: pump passes
        // are the holder's). Before the device-endpoints step so a first bind's
        // facts publish on the same pass; while bound it doubles as the facts
        // refresh — the interface list is read here, once per pass, so
        // the ensure and dial halves see one consistent snapshot.
        let lan_ips = fauna_peer_sync::discovery::discover_lan_candidates();
        match crate::peer_leg::ensure_bound(peer, store, writer_key, slot, rpc, &lan_ips).await {
            Ok(p) => report.peer_leg = Some(p),
            Err(e) => push_step_error(report, format!("peer leg: {e:#}"), &e),
        }
        // The dial pass (the pull half): only while bound, right after
        // the ensure step so a first-bind pass already dials, before the
        // device-endpoints step so nothing here depends on our own publish, and
        // before the seen-set step so it raises over what the dial merged in
        // on the same pass.
        // App-fed facts (the `set_endpoint_facts` Cmd — the explicit override)
        // win over the bind's self-observed ones.
        let effective_facts = endpoint_facts.or(peer.facts.as_ref());
        // T13 step 4: the same truth the fleet-only `device-endpoints` entry
        // publishes, in the shape a non-fleet peer can actually receive. Fed to
        // the listener here so an admit reply answers this pass's candidates.
        // Only when there ARE candidates: a node_id-only value would tell a peer
        // nothing it did not prove over the channel already, and staying absent
        // keeps those exchanges byte-identical to the pre-slot shape.
        let own_endpoints = effective_facts.map(|facts| {
            crate::device_endpoints_writer::endpoints_of(
                writer_key.verifying_key().to_bytes(),
                Some(facts),
            )
        });
        if let Some(server) = peer.server.as_ref() {
            server.set_own_endpoints(own_endpoints.clone());
        }
        let mut observed_endpoints: std::collections::HashMap<
            [u8; 32],
            fauna_core::device_endpoints::DeviceEndpoints,
        > = std::collections::HashMap::new();
        // Whether the nest path carries bytes this pass (`p2p.md` § The relay,
        // ruling 4): this pass's fleet walk is a nest walk that ran before the
        // legs, so its outcome is the pass's own nest reachability — passed to
        // the byte gate, never re-derived there.
        let nest = if report.fleet_walk.is_some() {
            fauna_transport::NestPath::Reachable
        } else {
            fauna_transport::NestPath::Unavailable
        };
        match crate::peer_leg::dial_pass(
            peer,
            store,
            schedule,
            trust,
            writer_key,
            slot,
            content_scopes,
            &lan_ips,
            own_endpoints.as_ref(),
            nest,
        )
        .await
        {
            Ok(p) => {
                if let Some(pass) = &p {
                    observed_endpoints.extend(pass.observed.clone());
                }
                report.peer_dial = p;
            }
            Err(e) => push_step_error(report, format!("peer dial: {e:#}"), &e),
        }
        // The custody serve refresh (W8.5 P1): the served-custodies registry,
        // after the fleet walk (custodies-held rows fresh) and after the ensure
        // step (the live server, when bound, takes the registry). Its revocation
        // snapshot (P2) refreshed earlier, beside the removed-device one.
        match custody
            .serve_refresh(store, peer.server.as_ref(), &peer.custodied_exclusions)
            .await
        {
            Ok(p) => report.custody_serve = Some(p),
            Err(e) => push_step_error(report, format!("custody serve refresh: {e:#}"), &e),
        }
        // The custodian NEST pass (W8.6's pump half): pull the accounts this
        // machine holds custody FOR from their OWNERS' nests. Deliberately NOT
        // gated on the peer transport — the always-on anchor is exactly the leg
        // for a custodian whose owner devices are asleep, unreachable, or not yet
        // known to the row. Sequenced BEFORE the dial pass so the bytes it lands
        // meet this same pass's T15 budget.
        match custody.nest_pass().await {
            Ok(p) => report.custody_nest = Some(p),
            Err(e) => push_step_error(report, format!("custody nest pull: {e:#}"), &e),
        }
        // The custodian dial pass (W8.5 P5): pull the accounts this machine holds
        // custody FOR from the owner FLEET. The dial half runs only while the leg
        // is bound — bound implies elected + enrolled + brake-off, the same gate
        // the sibling dial rides — but the pass itself runs unconditionally,
        // because it also carries T15's budget, which the nest leg's bytes need
        // just as much (`custody_leg::dial_pass`, the `transport` doc).
        {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            match custody
                .dial_pass(
                    peer.transport.as_ref(),
                    &lan_ips,
                    now_secs,
                    own_endpoints.as_ref(),
                    &peer.custodied_exclusions,
                )
                .await
            {
                Ok(p) => {
                    // The custodian's own account plane owns `custodies-held`, so
                    // the refresh goes through this runtime's own R14 door.
                    for row in &p.refreshed {
                        if let Err(e) = crate::custody_rows::put_custodies_held(
                            fleet,
                            writer_key.verifying_key().to_bytes(),
                            row,
                        )
                        .await
                        {
                            tracing::debug!("custody endpoint refresh (held row): {e:#}");
                        }
                    }
                    report.custody_dial = Some(p);
                }
                Err(e) => push_step_error(report, format!("custody dial: {e:#}"), &e),
            }
            // The check-in mint (W8.7 arc 2 — the cadence's "record" half):
            // attest each custody this pass's budget just judged, when
            // `receipt_due` says one is owed; `drive_ceremonies`' receipt arm
            // posts it over the ceremony's channel. Right after the dial pass so
            // the receipt attests THIS pass's post-eviction meter — and in the
            // pump, not the app, so an app-dead agent keeps attesting.
            if let Some(p) = report.custody_dial.as_ref()
                && !p.outcomes.is_empty()
            {
                // The ceremony record lives on this pass's own store: read
                // and joined here, on the store thread, like the held rows
                // the dial pass just refreshed.
                let records = crate::custody_leg::PassCeremonyRecords { store, fleet };
                report.custody_receipts_minted = crate::custody_leg::mint_due_receipts(
                    &records,
                    writer_key,
                    &p.outcomes,
                    fauna_core::data::Timestamp::now(),
                )
                .await;
            }
        }
        if let Some(server) = peer.server.as_ref() {
            observed_endpoints.extend(server.drain_observed_endpoints());
        }
        LegsOutput {
            bind_facts: peer.facts.clone(),
            observed_endpoints,
        }
    }
}

/// The native election ([`EngineElection`]): the kernel-arbitrated advisory
/// lock on `<store dir>/engine.lock` (`fauna_account_store::locks::EngineLock`),
/// and for the seed-leg role the one on `<store dir>/seed-legs.lock`
/// (`SeedLegLock`).
struct FileElection {
    store_dir: std::path::PathBuf,
}

impl FileElection {
    fn new(store_dir: std::path::PathBuf) -> Self {
        Self { store_dir }
    }
}

impl EngineElection for FileElection {
    type Held = EngineLock;

    async fn try_acquire(&self) -> ElectionOutcome<EngineLock> {
        match EngineLock::try_acquire(&self.store_dir) {
            EngineLockOutcome::Held(lock) => ElectionOutcome::Held(lock),
            EngineLockOutcome::Refused => ElectionOutcome::Refused,
            EngineLockOutcome::Degraded(e) => ElectionOutcome::Degraded(e.to_string()),
        }
    }

    type SeedLegs = SeedLegLock;

    async fn try_acquire_seed_legs(&self) -> ElectionOutcome<SeedLegLock> {
        match SeedLegLock::try_acquire(&self.store_dir) {
            SeedLegLockOutcome::Held(lock) => ElectionOutcome::Held(lock),
            SeedLegLockOutcome::Refused => ElectionOutcome::Refused,
            SeedLegLockOutcome::Degraded(e) => ElectionOutcome::Degraded(e.to_string()),
        }
    }
}

/// The store thread's body: assemble, hand the driver one assembly at a time
/// ([`AccountDriver::serve`]), until it says the runtime is over.
async fn worker<R>(
    params: HostParams<R>,
    mut driver: AccountDriver,
    ready_tx: oneshot::Sender<Result<()>>,
) where
    R: KeyedRpcRequester + Clone + Send + Sync + 'static,
    R::Error: RpcErrorClass,
{
    let HostParams {
        store_root,
        store_backup_exclusion,
        actor_id_hex,
        rpc,
        process_rpc,
        principal,
        credentials,
        peer_transport,
        enrollment_target_device_id,
        linked_nests,
        owed_nests,
        attested_predecessors,
    } = params;
    // The data path rides the principal's own session when the caller wired
    // one (W5.4b); `rpc` — the app session — then carries only the ceremony's
    // registration legs. Same type either way, so tests and not-yet-migrated
    // apps change nothing.
    let data_rpc = process_rpc.unwrap_or_else(|| rpc.clone());

    // ONE injected credential store, shared across reassemblies (the sealed
    // arm holds live unlocked state — sharing, never forking, is the point).
    let credentials = std::sync::Arc::new(credentials);
    // Consumed by the FIRST assembly only — a reassembly has no readiness
    // barrier to answer (the handle already exists; its next call sees the
    // healed runtime).
    let mut ready_tx = Some(ready_tx);
    // (The succession probe's one-rotation cap lives on the driver —
    // `AccountDriver::rotated_since_serve` — read here as the probe's license
    // and reset by the driver on a healthy enrollment answer.)
    // The lost-slot arm's own cap (refinement 10, bound (a)): at most ONE
    // fresh mint over a RETIRED writer found in the slot per runtime worker.
    // A slot that comes back retired after that mint is a credential store
    // not retaining writes — looping on it would be the same livelock in a
    // new coat. Atomic only because the assembly block below is a future
    // that borrows it; a relaunch re-arms it.
    let lost_slot_reminted = std::sync::atomic::AtomicBool::new(false);

    // ── The assembly/serve cycle. One iteration = assemble + serve until
    // shutdown. A typed `StaleWriter` refusal anywhere in the serve phase
    // re-enters the loop: the writer was rotated away under this process
    // (principal succession, charter § The store device principal →
    // succession decision 4), and the heal is to re-resolve the successor
    // from the shared slot and rebuild everything that cached the old
    // identity — the store handle, both planes, the peer leg, the fleet
    // bootstrap rows.
    // Answered only once the loop below has exited and its per-assembly locals
    // — the engine-singleton lock above all — have dropped. See the send after
    // `'assembly`.
    let mut shutdown_reply: Option<oneshot::Sender<()>> = None;
    'assembly: loop {
        // ── Assembly (a first-iteration failure reports through `ready_tx` and
        // ends the thread; a reassembly failure just ends the thread — loudly).
        let assembled = async {
            let store_dir = store_root
                .store_dir(&actor_id_hex)
                .context("account runtime: per-actor store dir")?;
            // The dir exists before anything below opens or locks it, and it
            // is out of the platform's cloud backup before anything is
            // written into it — a failed exclusion aborts the assembly, the
            // custodian store's rule: writing a store into a dir the platform
            // still replicates is the exact outcome the obligation prevents,
            // and nothing on the device would ever look wrong. Re-asserted on
            // every assembly (cheap, idempotent): the exclusion is a directory
            // attribute a restore or a copy can drop, and a dir that lost it
            // heals here.
            std::fs::create_dir_all(&store_dir)
                .with_context(|| format!("create store dir {}", store_dir.display()))?;
            store_backup_exclusion.apply(&store_dir).with_context(|| {
                format!(
                    "account runtime: exclude store dir {} from platform cloud backup",
                    store_dir.display()
                )
            })?;
            // Inside the store's migration/adoption section (W5.3), because
            // mint-or-load is a probe-then-act on state SHARED by every
            // co-located process: the charter's device principal is one per store
            // *replica*, not one per process. Two cold assemblies racing here
            // each read an empty slot, each mint, and the second one's store then
            // refuses it forever ("belongs to a different writer") — a store the
            // user cannot open from that app again, which is the
            // client-state-recoverability law's exact failure shape
            // (`nest/common.md` § Client-state recoverability). Measured
            // 2026-08-14 by `conformance_account_runtime` V9, which failed on
            // this before the section existed.
            //
            // Held across the mint only, then released: `SqliteBackend::open`
            // takes the same section for itself a line later, and the adoption
            // that follows is idempotent once every process resolves the same
            // key. Degrade is OPEN, matching the store's own policy.
            let (writer_key, writer_key_provenance, principal_slot, backup_key) = {
                let _section = match MigrationLock::acquire(&store_dir) {
                    MigrationLockOutcome::Held(lock) => Some(lock),
                    MigrationLockOutcome::Degraded(e) => {
                        tracing::warn!(
                            error = %e,
                            "account runtime: migration lock unavailable — resolving the writer \
                             key unserialized (safe for the single-process case)"
                        );
                        None
                    }
                };
                // Mint-or-load for a seed-holding caller — the enrollment ceremony's
                // own job. **Load-only for the seedless caller**: a Seedless
                // assembly is a *consumer* of an already-enrolled machine's
                // principal, never the ceremony, so an empty slot here means no
                // signed-in app has enrolled this machine yet — refuse rather than
                // mint a writer identity no nest has a grant for (the same
                // divergence W5.3 measured, one layer earlier: minting inside
                // `start` before the backup-key check below ran would otherwise
                // leave that orphaned identity persisted even though the refusal
                // a few lines down aborts the assembly).
                let (writer_key, writer_key_provenance) = match principal.keypair() {
                    Some(_) => writer_key_from_slot(&credentials, &actor_id_hex)?,
                    None => (
                        crate::principal_bundle::load_writer_key(&credentials, &actor_id_hex)
                            .context(
                                "account runtime: seedless assembly found no writer key in \
                                 the credential slot — this machine has never completed a \
                                 signed-in enrollment for this account, so there is nothing \
                                 to host",
                            )?,
                        WriterKeyProvenance::Loaded,
                    ),
                };
                // The account's `BackupKey`, resolved the one way this process can:
                // DERIVED where the seed is in hand, READ FROM THE SLOT where it is
                // not. W5.4a's carriage exists for precisely this consumer, so a
                // seedless host that finds nothing there must refuse to assemble —
                // guessing would seal the account's state under a key no app in the
                // fleet derives, and every such write would be silent data loss.
                let backup_key = match principal.keypair() {
                    Some(kp) => BackupKey::derive(kp.secret_bytes()),
                    None => crate::principal_bundle::load_backup_key(&credentials, &actor_id_hex)
                        .context(
                        "account runtime: seedless assembly found no backup key in the \
                         credential slot — this machine has never completed a signed-in \
                         enrollment for this account, so there is nothing to host",
                    )?,
                };
                // The rest of the T10 bundle resolves inside the SAME section
                // (W5.4a): every item is a probe-then-act on the shared slot —
                // the exact race class the writer key just paid for — so they
                // serialize with it rather than re-learning the lesson three
                // times (`principal_bundle` module docs). The seedless host passes
                // `None`: it has nothing authoritative to heal the slot WITH, and
                // its own key came out of that very slot a moment ago.
                let principal_slot = PrincipalSlot::resolve(
                    std::sync::Arc::clone(&credentials),
                    actor_id_hex.clone(),
                    store_dir.clone(),
                    &writer_key.verifying_key().to_bytes(),
                    principal.keypair().map(|_| &backup_key),
                );
                // (The succession rider's key carriage runs just AFTER this
                // section, never inside it — see there.)
                //
                // The enrollment ceremony's mint half (W5.4b), still inside the
                // section — a mint-or-load on the shared slot, the same race
                // class as everything above. Here and not in the pump because
                // this is where the seed is still in hand (the keypair moves
                // into the config client below); the nest legs (register + grant
                // register) are the pump's retryable step. Best-effort by the
                // slot's own contract: a failed mint reads as "not enrolled",
                // healed at the next seed-holding assembly.
                //
                // The seedless host never mints: the grant is root-signed, which
                // needs the seed by definition. It simply runs with whatever the
                // slot carries — an unenrolled machine's agent stays unenrolled
                // until an app signs in, which is the same "healed at the next
                // seed-holding assembly" contract, not a new failure mode.
                //
                // The heal path: a slot carrying a grant that lacks a
                // capability the ceremony mints (on capability growth, or
                // after a failed mint) is re-minted under the SAME key; the registration latch
                // is content-addressed, so the pump re-registers the new wire.
                let grant_is_current = principal_slot.device_authorization().is_some_and(|l| {
                    fauna_client_sync::principal_grant_is_current(&l.authorization)
                });
                if let (false, Some(keypair)) = (grant_is_current, principal.keypair()) {
                    let writer_pub = writer_key.verifying_key().to_bytes();
                    match fauna_client_sync::build_principal_grant(keypair, &writer_pub) {
                        Ok(wire) => {
                            if let Err(e) =
                                principal_slot.store_device_authorization(wire, &writer_pub)
                            {
                                tracing::warn!("enrollment ceremony: grant not persisted: {e:#}");
                            }
                        }
                        Err(e) => {
                            tracing::warn!("enrollment ceremony: grant mint failed: {e}");
                        }
                    }
                }
                (
                    writer_key,
                    writer_key_provenance,
                    principal_slot,
                    backup_key,
                )
            };
            // The succession rider's key carriage: a successor
            // runs under a NEW actor id, so this slot is fresh, while the
            // generation keys this device held live in each attested
            // predecessor's slot on this same machine. Carry them over —
            // idempotent — so the successor keys its pre-succession
            // generations, and its fleet walk carries the mint record of each
            // (the key in this slot is that carry's gate), which is what lets
            // it heal its siblings and re-escrow them
            // (`generation_reescrow`). Empty for every identity that never
            // succeeded, so the common path pays nothing.
            //
            // OUTSIDE the section above, deliberately: every key it records is
            // a slot read-modify-write that enters that same section itself
            // (`record_generation_key`), and the section is the store dir's
            // `migration.lock` — `flock` lives on the open file description,
            // so a second acquire from this thread while the first is held
            // blocks for ever. Inside, a successor with keys to carry never
            // reached ready (the in-process succession hang, measured on tui
            // 2026-09-30); web's host runs the carriage outside its section
            // too (`fauna_account_plane::web_host`).
            if !attested_predecessors.is_empty() {
                let carried =
                    principal_slot.carry_predecessor_generation_keys(&attested_predecessors);
                if carried > 0 {
                    tracing::info!(
                        carried,
                        "account runtime: carried predecessor-held generation keys into \
                         the successor's slot"
                    );
                }
            }
            // The succession probe — ceremony-capable
            // assemblies only, OUTSIDE the lock (it is an RPC — a nest
            // round-trip must never hold the migration section) and before
            // the store opens (a rotation restarts assembly with nothing to
            // tear down). It rides the app session exactly like the pump's
            // registration legs; its budget bounds how long an unreachable
            // nest can delay readiness.
            if let Some(kp) = principal.keypair() {
                match crate::principal_succession::ceremony_probe(
                    &rpc,
                    &credentials,
                    &actor_id_hex,
                    &store_dir,
                    kp,
                    &writer_key,
                    &principal_slot,
                    &enrollment_target_device_id,
                    !driver.rotated_since_serve,
                    // Decision 1's third trigger: the pump's finding that
                    // this machine's own device-set row reads `Removed`,
                    // carried across the reassembly it caused.
                    driver.own_row_removed(),
                )
                .await
                .context("account runtime: succession probe")?
                {
                    crate::principal_succession::CeremonyProbe::Rotated => {
                        return anyhow::Ok(None);
                    }
                    crate::principal_succession::CeremonyProbe::Registered
                    | crate::principal_succession::CeremonyProbe::Skipped => {}
                }
            }
            let writer = WriterId(writer_key.verifying_key().to_bytes());
            let backend =
                SqliteBackend::open(&store_dir).context("account runtime: open backend")?;
            // The lost-slot arm (charter § The store device principal,
            // refinement 10) and the journal-bound writer's two arms
            // (refinement 11: a key LOADED over a store with no stamped
            // writer; a stamped writer the walk found burnt) — between the
            // backend open (which takes the migration section itself) and
            // the store open (whose identity check would otherwise refuse a
            // store the slot no longer matches, for good, or adopt a key
            // whose history this journal cannot vouch for). Every host,
            // seedless included: the fence signs nothing. `Fenced` continues
            // as the held key — it IS the successor now; a restart
            // re-resolves a slot that moved.
            {
                use crate::principal_succession::LostSlotHeal;
                use std::sync::atomic::Ordering;
                match crate::principal_succession::lost_slot_heal(
                    &credentials,
                    &actor_id_hex,
                    &store_dir,
                    &backend,
                    &writer_key,
                    writer_key_provenance,
                    !lost_slot_reminted.load(Ordering::SeqCst),
                )
                .await
                .context("account runtime: lost-slot heal")?
                {
                    LostSlotHeal::Consistent | LostSlotHeal::Fenced => {}
                    // A sibling moved the slot under us: nothing was minted, so
                    // the re-mint cap must NOT be burned — otherwise a benign
                    // race spends the budget a genuine retired-writer-in-slot
                    // needs, and that one then refuses instead of healing.
                    LostSlotHeal::RestartAssembly => return anyhow::Ok(None),
                    LostSlotHeal::RemintedIntoSlot => {
                        lost_slot_reminted.store(true, Ordering::SeqCst);
                        return anyhow::Ok(None);
                    }
                }
            }
            let store = AccountStore::open(backend, &actor_id_hex, writer)
                .await
                .context("account runtime: open store")?;
            // The store is stamped with the slot's key (the open adopts it on
            // a fresh store): the mint marker is spent. From here a key
            // loaded over an UNSTAMPED store is one whose journal is gone —
            // refinement 11's inverse arm — never a mint still in flight.
            crate::principal_bundle::clear_writer_unstamped(&credentials, &actor_id_hex);
            // The engine-singleton election (W5.1) — after the store proved
            // openable (a runtime that cannot open the store must not squat on
            // the role), inside the readiness barrier so the role is settled
            // before any command is served.
            let election = FileElection::new(store_dir.clone());
            let role = elect_at_start(&election).await;
            anyhow::Ok(Some((
                store,
                writer_key,
                store_dir,
                election,
                role,
                principal_slot,
                backup_key,
            )))
        }
        .await;

        let (store, writer_key, store_dir, election, role, principal_slot, backup_key) =
            match assembled {
                Ok(Some(parts)) => parts,
                // The succession probe rotated this machine's writer: restart
                // assembly so everything resolves the successor from the slot.
                Ok(None) => {
                    driver.rotated_since_serve = true;
                    continue 'assembly;
                }
                Err(e) => {
                    match ready_tx.take() {
                        Some(tx) => {
                            let _ = tx.send(Err(e));
                        }
                        None => tracing::error!("account runtime: reassembly failed: {e:#}"),
                    }
                    return;
                }
            };
        // The peer-leg seam (W5.7): worker-owned so the node's Drop is tied to
        // this thread's life (rule 5), fed by the pump's holder-only ensure step.
        // The factory is CLONED in (it is an `Arc`): a reassembly rebuilds the
        // leg — the node identity derives from the writer key, which is exactly
        // what a rotation changed.
        let peer_leg = PeerLegState::new(
            peer_transport.clone(),
            store_dir.clone(),
            actor_id_hex.clone(),
        );
        // The custodian leg (W8.5): worker-owned like the peer leg; rebuilt
        // on reassembly for the same reason (the writer identity may have
        // rotated). The revocation snapshot reads the grant log off this
        // store, which both principals hold, so agents refresh it and serve
        // custodians like any app.
        let custody_leg = crate::custody_leg::CustodyLegState::new(
            store_root.clone(),
            ActorId(
                fauna_core::hex32::decode(&actor_id_hex).expect("assembly validated the actor id"),
            ),
            writer_key.clone(),
        );
        let mut legs = NativeLegs {
            peer: peer_leg,
            custody: custody_leg,
        };
        // Boxed: the serve's states would otherwise sit beside the assembly
        // block's in this future, held inline on the store thread's stack
        // (`native-async-execution.md` § The rule).
        let end = Box::pin(driver.serve(
            Assembly {
                store: &store,
                writer_key: &writer_key,
                backup_key: &backup_key,
                principal: &principal,
                slot: &principal_slot,
                data_rpc: &data_rpc,
                session_rpc: &rpc,
                linked_nests: linked_nests.as_ref(),
                owed_nests: owed_nests.as_ref(),
            },
            &mut legs,
            &election,
            role,
            ready_tx.take(),
        ))
        .await;
        match end {
            ServeEnd::Reassemble => continue 'assembly,
            ServeEnd::Closed => break 'assembly,
            ServeEnd::Shutdown(reply) => {
                shutdown_reply = Some(reply);
                break 'assembly;
            }
            ServeEnd::Failed => return,
        }
    } // 'assembly
    tracing::info!("account runtime: store thread exiting");
    // **Now** — after the loop's per-assembly locals have dropped, and with
    // them the engine-singleton `EngineLock` (`ServeEnd::Shutdown` owns why
    // the ordering is the contract rather than a detail).
    if let Some(reply) = shutdown_reply {
        let _ = reply.send(());
    }
}

#[cfg(test)]
mod tests {
    use fauna_client_accounts::SecretStore;
    use fauna_core::identity::ActorKeypair;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use fauna_account_store::types::WriterId;
    use fauna_core::data::ModerationConfig;
    use fauna_core::encoding::{canonical_decode, canonical_encode};
    use fauna_core::seen_set::SeenScopeSet;
    use fauna_protocol::account_state::{
        AccountStatePutReply, AccountStatePutRequest, ItemClass, KIND_STATE_PUT,
    };
    use fauna_protocol::merge_policy::{
        KIND_MODERATION, KIND_SEEN_SET, KIND_SYNC_PREFS, PREFERENCE_KEY,
    };
    use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
    use fauna_protocol::{decode_strict, encode_canonical as wire_encode};

    use super::*;
    use crate::scope_set::OWN_ACTOR_KINDS;
    use fauna_account_plane::account_driver::NotReadyReason;
    use fauna_account_store::db::actor_state_dir;
    use std::path::PathBuf;

    /// The `prior` half of the R14 writer-door trust must reach the writer door
    /// verbatim — the failure mode is silence: a wrong source yields an
    /// empty chain forever, and every enrollment a predecessor identity
    /// signed quietly stops verifying. Nothing downstream can tell that from
    /// an ordinary account, which never has priors at all.
    ///
    /// **Flipped 2026-09-13 with the ruling that `prior` is ATTESTED, never
    /// asserted** (`account-data-taxonomy.md` § The generation machinery →
    /// *The source of `prior`*): the replica read this used
    /// to pin is gone, and the pin is now the seam the assembly builds its
    /// trust through — `r14_trust` — which takes the caller's attested set
    /// and, structurally, no replica base at all. Both halves are asserted:
    /// the attested set reaches `prior` verbatim and in order, and nothing
    /// else does.
    ///
    /// Since the 2026-10-01 re-ruling the set signs nothing in the fleet view
    /// (enrollment certs verify against the root alone); the group authority
    /// view is its one reader, and the seam pinned here is still what decides
    /// whose predecessor authority it trusts.
    ///
    /// Red-verified by mutation: `prior: Vec::new()` in `r14_trust` reds the
    /// first assertion.
    #[test]
    fn the_r14_prior_is_the_attested_predecessor_set() {
        let keypair = ActorKeypair::from_secret([0x31; 32]);
        let actor_hex = keypair.actor_id().to_hex();
        let attested = [ActorId([0x77; 32]), ActorId([0x78; 32])];

        let trust = r14_trust(&actor_hex, &attested, &[[0xAA; 32]]).unwrap();
        assert_eq!(
            trust.prior, attested,
            "the attested set is the group authority view's predecessor set, verbatim and in order"
        );
        assert_eq!(trust.root, keypair.actor_id());
        assert_eq!(trust.trusted_holders.get(), vec![[0xAA; 32]]);

        // Nothing attested is fail-safe: no predecessor is trusted.
        assert!(r14_trust(&actor_hex, &[], &[]).unwrap().prior.is_empty());
        // And the one thing the seam can refuse is a root it cannot name.
        assert!(r14_trust("not-hex", &attested, &[]).is_err());
    }

    /// The blinded item key [`FakeNest::stage_forged_state_row`] puts on a
    /// fabricated row — well-formed (the walk needs 32 bytes to get as far as
    /// the arm under test) and matching no key any life ever published.
    const FORGED_ITEM_KEY: [u8; 32] = [0xF0; 32];

    /// One relayed feed row — exactly what a put carried, echoed back.
    #[derive(Clone)]
    struct FeedRow {
        /// The scope the row was written to. A real nest partitions its feed by
        /// scope, and since the runtime now walks content scopes beside the
        /// account-state one, a fake that answered every scope with every row
        /// would hand state entries to a content walk — rows it would refuse
        /// for having no CID. The filter is what keeps the fake honest.
        scope: String,
        nest_seq: i64,
        writer_id: String,
        writer_seq: i64,
        item_key: Vec<u8>,
        op: String,
        entry: Vec<u8>,
    }

    /// One `record-cid` feed row of a content scope — what the nest derives
    /// from its record mirror. Staged by tests that need a content walk to
    /// find something (the seen-set producer's fixtures).
    #[derive(Clone)]
    struct ContentRow {
        scope: String,
        seq: i64,
        digest: [u8; 32],
        op: String,
    }

    #[derive(Default)]
    struct FakeState {
        feed: Vec<FeedRow>,
        /// Every `(scope, writer, writer_seq)` a put ever recorded, live or
        /// since collapsed — the real nest refuses a coordinate it already
        /// holds (`stale_writer_seq`, refinement 11's nest half), and a fake
        /// that accepted a reused one would let a burnt journal look healthy.
        coordinates_seen: std::collections::HashSet<(String, String, i64)>,
        content_rows: Vec<ContentRow>,
        next_seq: i64,
        list_calls: usize,
        /// List calls per requested scope. A pump pass now walks the
        /// account-state scope *and* every derived content scope, so "how many
        /// times was this scope walked" is the question a walk-counting test
        /// actually means — a bare total conflates the two.
        list_calls_by_scope: std::collections::HashMap<String, usize>,
        put_calls: usize,
        /// Puts per target scope, for exactly the reason
        /// [`FakeState::list_calls_by_scope`] exists — and now for a second
        /// one: the fleet bootstrap publishes its own rows (self-enrollment,
        /// escrow target) on `state-fleet` at startup, so a bare total no
        /// longer answers "what did *this* pass publish on the scope under
        /// test". Assert on the scope you mean.
        put_calls_by_scope: std::collections::HashMap<String, usize>,
        /// Retires asked per target scope, answered or deferred.
        retire_calls_by_scope: std::collections::HashMap<String, usize>,
        /// Every keyed (outbox-drain) attempt, in arrival order: the kind and
        /// the envelope idempotency key the drain supplied — which the tests
        /// assert is the STORED intent id, the property the whole seam exists
        /// for. Attempts are recorded before the failure switches fire, so a
        /// FIFO test can also assert what was never attempted.
        keyed_requests: Vec<(String, [u8; 16])>,
        /// Device ids the W5.4b ceremony registered (`fauna.sync.register`),
        /// in arrival order — the latch pin asserts a second pass adds none.
        sync_registers: Vec<String>,
        /// Same, for `fauna.sync.device_grant.register`.
        grant_registers: Vec<String>,
        /// The `sync_devices` rows that exist for this account. Modelled
        /// because the enrollment pass now *asks* — its register-create leg
        /// fires on the nest's typed not-found rather than registering
        /// unconditionally, so a fake that accepted every grant register would
        /// silently skip the branch under test.
        rows: std::collections::BTreeSet<String>,
        /// Which row currently carries the principal's grant, if any.
        grant_on: Option<String>,
        /// The hex device KEY of every grant registered, in arrival order —
        /// the row ids above name the machine, these name the principal (the
        /// principal IS the writer key, T10; the row is the app's own id).
        grant_keys: Vec<String>,
        /// Which key the grant on [`Self::grant_on`] carries.
        granted_key: Option<String>,
        /// Device keys retired over `fauna.sync.device_grant.revoke`, in
        /// arrival order — the sign-out leg's witness. The fake models the
        /// ruled revoke (`sync-agent-credentials.md` § Credential model → the
        /// RULED 2026-09-28 block, decision 4): the named row's grant columns
        /// clear and the row itself stays.
        grant_revokes: Vec<String>,
        /// Device KEYS whose grants the "user" tombstoned per-key
        /// (`fauna.sync.devices.delete` — the revocation memory): a grant
        /// register naming one of these answers the typed
        /// `fauna.sync.device_grant_revoked` refusal exactly as the real
        /// handler's check 5 does, while OTHER keys keep registering — the
        /// per-key shape the succession revival needs (the sticky
        /// `revoke_grants` switch refuses everything, which cannot model a
        /// successor enrolling beside a dead key).
        revoked_device_ids: std::collections::BTreeSet<String>,
        /// Every `fauna.sync.device_grant.register` that arrived, answered or
        /// refused — the pump's enrollment step and the assembly's succession
        /// probe each send exactly one for a revoked key, so this counts the
        /// two together.
        grant_register_attempts: usize,
        /// [`FakeNest::flap_revoked_grant_registers`]'s turn: whether the
        /// next register of a revoked key faults instead of answering.
        flap_fault_next: bool,
        /// Every wrap an escrow deposit carried, `(generation id, wrap)` in
        /// arrival order — what the holder's `get` door serves back.
        escrow_wraps: Vec<(Vec<u8>, Vec<u8>)>,
        /// Per scope, the live-pair cap [`FakeNest::cap_at_live_rows`] set:
        /// a put that would add a live `(writer, item)` pair to a scope at
        /// its cap is refused `scope_full`, as the real handler refuses it
        /// (`record_account_state_entry`, check 3 — an update of a held
        /// pair is free).
        live_pair_caps: std::collections::HashMap<String, usize>,
        /// Puts refused `scope_full`, per scope.
        scope_full_refusals: std::collections::HashMap<String, usize>,
    }

    /// An in-memory nest answering the four kinds the runtime speaks: state
    /// put (append + per-`(writer, item)` collapse, the W2.3 rule), the
    /// state-entry feed walk (frontier-filtered, rows echoed verbatim).
    #[derive(Clone, Default)]
    struct FakeNest {
        state: Arc<Mutex<FakeState>>,
        panic_next_list: Arc<AtomicBool>,
        /// One-shot: the next `fauna.sync.changes.list` never answers — a walk
        /// that will not end, which holds the whole pass (and every command
        /// queued behind it) where it stands. `list_stalled` fires once the
        /// stalled call has arrived, so a test can land its next act INSIDE
        /// the pass rather than guess when the pass has reached it.
        stall_next_list: Arc<AtomicBool>,
        list_stalled: Arc<tokio::sync::Notify>,
        /// Ends the stall: the held `changes.list` answers (an empty page)
        /// once notified. Never notified = a pass that never ends.
        release_list: Arc<tokio::sync::Notify>,
        /// Sticky: every `fauna.sync.changes.list` walk fails in transport —
        /// a pass that ends having walked nothing (the nest unreachable for
        /// the feed just after sign-in). Refused before the walk counters, so
        /// `list_calls` keeps counting answered walks only.
        walks_unreachable: Arc<AtomicBool>,
        /// Sticky: every unkeyed request fails in transport — the nest is not
        /// there at all, so a pass ends with nothing walked and nothing read
        /// off the plane either.
        nest_unreachable: Arc<AtomicBool>,
        fail_next_put: Arc<AtomicBool>,
        /// One-shot: the next state put is RECORDED — feed row, coordinate
        /// memory, collapse — and then answered with a transport fault, the
        /// reply lost on the wire. The row the client re-sends is a replay
        /// the nest refuses `stale_writer_seq` (refinement 11 → *a refused
        /// row's relay residue*, the replay case).
        lose_next_put_reply: Arc<AtomicBool>,
        /// One-shot, keyed on `(scope, writer)`: the next state put there is RECORDED
        /// (as [`Self::lose_next_put_reply`] records it) and its reply is then
        /// held for ever — the put is on the wire and the nest holds the row
        /// when a sign-out cuts the pass around it, so the reply dies with
        /// the dropped future. `put_held` fires once the row is recorded.
        hold_next_put_reply_on: Arc<Mutex<Option<(String, String)>>>,
        put_held: Arc<tokio::sync::Notify>,
        /// One-shot: the next keyed request is refused (`is_rejection` = true)
        /// — the drain must park the intent and its scope.
        reject_next_keyed: Arc<AtomicBool>,
        /// Sticky: the nest refuses `fauna.sync.device_grant.revoke` as an
        /// unknown kind, so a sign-out's retirement defers (the generic
        /// `Err` arm) and the sign-out still completes.
        no_revoke_kind: Arc<AtomicBool>,
        /// Sticky: every `device_grant.register` answers the nest's permanent
        /// revoked-grant refusal — the tombstone a `fauna.sync.devices.delete`
        /// leaves behind (decision 4's removed-from-account state).
        revoke_grants: Arc<AtomicBool>,
        /// Sticky: a register of a revoked key answers the revoked refusal
        /// and faults in transport by turns, the refusal first. The nest
        /// cannot tell the pump's register from the probe's, and the probe's
        /// always follows a pass that was answered revoked — so under this
        /// switch every pass that reassembles meets a probe that learns
        /// nothing and rotates nothing.
        flap_revoked_grant_registers: Arc<AtomicBool>,
        /// Sticky: every `sync.register` of a NEW row answers the tier's
        /// device-cap refusal (`fauna.sync.device_limit_exceeded`, nothing
        /// written) — a re-register of a row the fake already holds still
        /// succeeds, exactly as the nest's upsert does (`devices.md` § Step 4).
        at_device_cap: Arc<AtomicBool>,
        /// Sticky: `fauna.sync.devices.list` fails in transport — the staged
        /// removals' reconcile must wait, never guess.
        roster_unreachable: Arc<AtomicBool>,
        /// One-shot: the next keyed request fails in transport — the drain
        /// must record an attempt and stop the pass.
        fail_next_keyed: Arc<AtomicBool>,
        /// Sticky: a state put at a `(scope, writer, seq)` the nest already
        /// holds under ANOTHER item is accepted, and the feed then serves
        /// both rows at that coordinate, for good. No current nest does
        /// this (refinement 11's `SeqReused` refusal, nest half); the flag is
        /// the one-leg stand-in for the live two-leg shape — a peer relaying
        /// a burnt writer's row the nest refused, beside the nest's row there
        /// (refinement 11 → *a foreign writer's second row at a held
        /// coordinate is carried*).
        accept_reused_coordinates: Arc<AtomicBool>,
        /// Sticky: every retire on the delegable scope is deferred
        /// (`not_yet_stable`), so a row stays served for good — as behind a
        /// retention gate a sibling that has not walked holds, or in
        /// [`Self::accept_reused_coordinates`]'s one-leg stand-in, where a
        /// peer relaying a burnt writer's row has no retire door. Off, the
        /// cover step (`delegable-scope-reclamation.md` part (4)) retires
        /// what a member's row carries in the pass that carries it.
        delegable_retires_deferred: Arc<AtomicBool>,
        /// Sticky: an escrow holder stands behind this nest — a deposit is
        /// answered with a receipt signed by [`escrow_holder_key`], which a
        /// runtime started with [`Fixture::params_trusting_the_holder`]
        /// trusts, so its first-need mint completes and a generation tip
        /// resolves. Off, the deposit is refused (the fixture's default).
        escrow_holder: Arc<AtomicBool>,
        /// Sticky: the holder's `get` door fails in transport while its
        /// deposit door still answers — an escrow recovery that keys nothing,
        /// so a generation only a sibling keys stays unkeyed on this device
        /// and the rows sealed under it stay unopened.
        escrow_reads_unreachable: Arc<AtomicBool>,
        /// Sticky: the holder's `get` door answers, with no wrap at all —
        /// the holder has deleted them, so a recovery that asks hears that
        /// nothing opens.
        escrow_reads_empty: Arc<AtomicBool>,
        /// The deepest a request was polled below the store thread's entry,
        /// in bytes of stack ([`FakeNest::note_stack_depth`]).
        deepest_request: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FakeNest {
        /// Called from inside each request's own poll: on a store thread,
        /// record how far below the thread's entry the stack now is.
        fn note_stack_depth(&self) {
            if let Some(depth) = store_thread_stack_depth() {
                self.deepest_request.fetch_max(depth, Ordering::SeqCst);
            }
        }

        fn deepest_request(&self) -> usize {
            self.deepest_request.load(Ordering::SeqCst)
        }

        fn forget_deepest_request(&self) {
            self.deepest_request.store(0, Ordering::SeqCst);
        }
    }

    /// The signing key of the escrow holder [`FakeNest::escrow_holder`]
    /// models.
    fn escrow_holder_key() -> SigningKey {
        SigningKey::from_bytes(&[0x4E; 32])
    }

    #[derive(Debug)]
    struct TestErr {
        msg: String,
        rejection: bool,
        /// The wire error a **typed** nest refusal carries. `None` for the two
        /// constructors above, which model a fault or an untyped refusal; the
        /// enrollment pass and the succession probe classify on the code
        /// (`is_device_grant_no_device`, `is_device_grant_revoked`), so a
        /// fixture that could not carry one could not exercise those branches
        /// at all.
        rpc: Option<fauna_protocol::RpcError>,
    }
    impl TestErr {
        /// A transport fault (disconnect/timeout) — retryable.
        fn transient(msg: impl Into<String>) -> Self {
            Self {
                msg: msg.into(),
                rejection: false,
                rpc: None,
            }
        }
        /// A server refusal — the request reached the nest and was rejected.
        fn rejection(msg: impl Into<String>) -> Self {
            Self {
                msg: msg.into(),
                rejection: true,
                rpc: None,
            }
        }
        /// A **typed** server refusal, carrying the code a real nest answers.
        fn coded(code: &str, msg: impl Into<String>) -> Self {
            let msg = msg.into();
            Self {
                msg: msg.clone(),
                rejection: true,
                rpc: Some(fauna_protocol::RpcError::new(code, msg)),
            }
        }
    }
    impl std::fmt::Display for TestErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.msg)
        }
    }
    impl std::error::Error for TestErr {}
    impl RpcErrorClass for TestErr {
        fn is_rejection(&self) -> bool {
            self.rejection
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            self.rpc.as_ref()
        }
    }

    impl FakeNest {
        fn list_calls(&self) -> usize {
            self.state.lock().unwrap().list_calls
        }
        /// Walks of one scope — see [`FakeState::list_calls_by_scope`].
        fn list_calls_for(&self, scope: &str) -> usize {
            self.state
                .lock()
                .unwrap()
                .list_calls_by_scope
                .get(scope)
                .copied()
                .unwrap_or(0)
        }
        fn put_calls(&self) -> usize {
            self.state.lock().unwrap().put_calls
        }
        /// Puts to one scope — see [`FakeState::put_calls_by_scope`].
        fn put_calls_for(&self, scope: &str) -> usize {
            self.state
                .lock()
                .unwrap()
                .put_calls_by_scope
                .get(scope)
                .copied()
                .unwrap_or(0)
        }
        /// Retires asked on one scope.
        fn retire_calls_for(&self, scope: &str) -> usize {
            self.state
                .lock()
                .unwrap()
                .retire_calls_by_scope
                .get(scope)
                .copied()
                .unwrap_or(0)
        }
        /// The live rows `scope` holds, counted by writer id (hex).
        fn live_rows_by_writer(&self, scope: &str) -> std::collections::BTreeMap<String, usize> {
            let mut census = std::collections::BTreeMap::new();
            for row in self.state.lock().unwrap().feed.iter() {
                if row.scope == scope {
                    *census.entry(row.writer_id.clone()).or_default() += 1;
                }
            }
            census
        }
        /// Cap `scope` at the live rows it holds now: from here a put that
        /// adds a pair is refused `scope_full` ([`FakeState::live_pair_caps`]).
        fn cap_at_live_rows(&self, scope: &str) {
            let mut s = self.state.lock().unwrap();
            let live = s.feed.iter().filter(|r| r.scope == scope).count();
            s.live_pair_caps.insert(scope.to_string(), live);
        }
        /// Give a capped `scope` room for `more` new pairs.
        fn widen_cap(&self, scope: &str, more: usize) {
            *self
                .state
                .lock()
                .unwrap()
                .live_pair_caps
                .get_mut(scope)
                .expect("a capped scope") += more;
        }
        /// Puts refused `scope_full` on `scope` so far.
        fn scope_full_refusals(&self, scope: &str) -> usize {
            self.state
                .lock()
                .unwrap()
                .scope_full_refusals
                .get(scope)
                .copied()
                .unwrap_or(0)
        }
        /// Every keyed (outbox-drain) attempt so far — see
        /// [`FakeState::keyed_requests`].
        fn keyed_requests(&self) -> Vec<(String, [u8; 16])> {
            self.state.lock().unwrap().keyed_requests.clone()
        }
        /// The user's `fauna.sync.devices.delete` of row `device_id`, reduced
        /// to the half the succession trigger observes: the key whose grant
        /// the row carried is tombstoned, so its next grant register answers
        /// the typed refusal.
        fn revoke_device(&self, device_id: &str) {
            let mut s = self.state.lock().unwrap();
            // The real delete removes the row and with it the grant it
            // carried — model both, or a "deleted" machine would still have
            // a registered row for the probe to find.
            s.rows.remove(device_id);
            if s.grant_on.as_deref() == Some(device_id) {
                s.grant_on = None;
                if let Some(key) = s.granted_key.take() {
                    s.revoked_device_ids.insert(key);
                }
            }
        }
        /// Stage one state-entry row on a scope's feed that no put ever
        /// carried, under an arbitrary `(writer, writer_seq)` and sealed by
        /// nobody: the shape a hostile nest — or a same-account sibling
        /// putting under another device's writer id — can serve, since
        /// `origin_writer`/`origin_seq` are feed metadata outside the seal.
        /// Deliberately NOT recorded in `coordinates_seen`: the nest is
        /// fabricating the row, not remembering a put, and the writer whose
        /// id it wears must stay free to spend the coordinate itself.
        fn stage_forged_state_row(&self, scope: &str, writer_id: &str, writer_seq: i64) {
            let mut s = self.state.lock().unwrap();
            s.next_seq += 1;
            let nest_seq = s.next_seq;
            s.feed.push(FeedRow {
                scope: scope.to_string(),
                nest_seq,
                writer_id: writer_id.to_string(),
                writer_seq,
                item_key: FORGED_ITEM_KEY.to_vec(),
                op: fauna_protocol::account_state::OP_STATE_PUT.to_string(),
                entry: b"forged by the nest: opens under no key".to_vec(),
            });
        }
        /// Stage one `record-cid` row on a content scope's feed — a record the
        /// nest already sequenced (delivery/creation happened nest-side; the
        /// walk merely observes it).
        fn stage_record(&self, scope: &str, seq: i64, digest: [u8; 32]) {
            self.state.lock().unwrap().content_rows.push(ContentRow {
                scope: scope.to_string(),
                seq,
                digest,
                op: "record-added".to_string(),
            });
        }
    }

    impl RpcRequester for FakeNest {
        type Error = TestErr;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.note_stack_depth();
            if self.nest_unreachable.load(Ordering::SeqCst) {
                return Err(TestErr::transient("the nest is unreachable"));
            }
            let req_bytes = wire_encode(&payload).expect("encode request");
            let reply_bytes: Vec<u8> = match kind {
                KIND_STATE_PUT => {
                    if self.fail_next_put.swap(false, Ordering::SeqCst) {
                        return Err(TestErr::transient("injected put failure"));
                    }
                    let req: AccountStatePutRequest =
                        decode_strict(&req_bytes).expect("decode put");
                    let (nest_seq, held) = {
                        let mut s = self.state.lock().unwrap();
                        s.put_calls += 1;
                        *s.put_calls_by_scope.entry(req.scope.clone()).or_default() += 1;
                        let coordinate = (req.scope.clone(), req.writer_id.clone(), req.writer_seq);
                        let new_pair = !s.feed.iter().any(|r| {
                            r.scope == req.scope
                                && r.writer_id == req.writer_id
                                && r.item_key == req.item_key.to_vec()
                        });
                        // The real handler's order: the coordinate checks,
                        // then the cap — which records nothing, so a refused
                        // coordinate stays free for the retry.
                        if new_pair
                            && !s.coordinates_seen.contains(&coordinate)
                            && s.live_pair_caps.get(&req.scope).is_some_and(|cap| {
                                s.feed.iter().filter(|r| r.scope == req.scope).count() >= *cap
                            })
                        {
                            *s.scope_full_refusals.entry(req.scope.clone()).or_default() += 1;
                            return Err(TestErr::coded(
                                "fauna.account.state.scope_full",
                                format!(
                                    "fauna.account.state.scope_full: scope {} is at its \
                                     live-entry cap",
                                    req.scope
                                ),
                            ));
                        }
                        if !s.coordinates_seen.insert(coordinate)
                            && !self.accept_reused_coordinates.load(Ordering::SeqCst)
                        {
                            return Err(TestErr::coded(
                                fauna_protocol::RpcError::CODE_ACCOUNT_STATE_STALE_WRITER_SEQ,
                                format!(
                                    "writer_seq {} was already recorded for writer {} in scope {} \
                                 — a reused coordinate",
                                    req.writer_seq, req.writer_id, req.scope
                                ),
                            ));
                        }
                        s.next_seq += 1;
                        let nest_seq = s.next_seq;
                        // The W2.3 collapse: a put supersedes only its own
                        // writer's predecessors for the item, within its scope.
                        s.feed.retain(|r| {
                            !(r.scope == req.scope
                                && r.writer_id == req.writer_id
                                && r.item_key == req.item_key.to_vec())
                        });
                        let held = {
                            let mut hold = self.hold_next_put_reply_on.lock().unwrap();
                            if hold.as_ref().is_some_and(|(scope, writer)| {
                                *scope == req.scope && *writer == req.writer_id
                            }) {
                                hold.take().is_some()
                            } else {
                                false
                            }
                        };
                        s.feed.push(FeedRow {
                            scope: req.scope,
                            nest_seq,
                            writer_id: req.writer_id,
                            writer_seq: req.writer_seq,
                            item_key: req.item_key.to_vec(),
                            op: req.op,
                            entry: req.entry.to_vec(),
                        });
                        (nest_seq, held)
                    };
                    if held {
                        self.put_held.notify_one();
                        return std::future::pending().await;
                    }
                    if self.lose_next_put_reply.swap(false, Ordering::SeqCst) {
                        return Err(TestErr::transient(
                            "injected lost reply — the nest recorded the row",
                        ));
                    }
                    wire_encode(&AccountStatePutReply {
                        seq: nest_seq,
                        ..Default::default()
                    })
                    .expect("encode put reply")
                    .to_vec()
                }
                // The feed's compaction (fleet-scope reclamation): drop the
                // named live row, exactly as the nest marks it superseded —
                // no retention gate here (the nest's DB tests own it).
                fauna_protocol::account_state::KIND_STATE_RETIRE => {
                    let req: fauna_protocol::account_state::AccountStateRetireRequest =
                        decode_strict(&req_bytes).expect("decode retire");
                    *self
                        .state
                        .lock()
                        .unwrap()
                        .retire_calls_by_scope
                        .entry(req.scope.clone())
                        .or_default() += 1;
                    if req.scope == ACCOUNT_STATE_SCOPE
                        && self.delegable_retires_deferred.load(Ordering::SeqCst)
                    {
                        return Err(TestErr::coded(
                            "fauna.account.state.not_yet_stable",
                            "a peer still serves the row: the retire waits",
                        ));
                    }
                    let mut s = self.state.lock().unwrap();
                    let before = s.feed.len();
                    s.feed.retain(|r| {
                        !(r.scope == req.scope
                            && r.writer_id == req.writer_id
                            && r.writer_seq == req.writer_seq
                            && r.item_key == req.item_key.to_vec())
                    });
                    wire_encode(&fauna_protocol::account_state::AccountStateRetireReply {
                        retired: s.feed.len() < before,
                        ..Default::default()
                    })
                    .expect("encode retire reply")
                    .to_vec()
                }
                "fauna.sync.changes.list" => {
                    // The bind leg's replica probe (`bind_leg::probe_bound_replica`
                    // — an all-zero `sealed_under`, which selects no row): not a
                    // walk, so no walk counter, stall or injected fault touches
                    // it; answered with this fake's one replica id.
                    if let Ok(probe) = decode_strict::<SyncChangesListRequest>(&req_bytes)
                        && probe
                            .sealed_under
                            .as_ref()
                            .is_some_and(|g| g.as_slice() == [0u8; 32])
                    {
                        return Ok(decode_strict(
                            &wire_encode(&SyncChangesListReply {
                                replica_id: Some(fauna_protocol::ByteBuf::from(vec![0xFA; 16])),
                                ..Default::default()
                            })
                            .expect("encode probe reply"),
                        )
                        .expect("decode reply"));
                    }
                    if self.panic_next_list.swap(false, Ordering::SeqCst) {
                        panic!("injected walk panic");
                    }
                    if self.stall_next_list.swap(false, Ordering::SeqCst) {
                        self.list_stalled.notify_one();
                        self.release_list.notified().await;
                    }
                    if self.walks_unreachable.load(Ordering::SeqCst) {
                        return Err(TestErr::transient("the feed is unreachable"));
                    }
                    let req: SyncChangesListRequest =
                        decode_strict(&req_bytes).expect("decode list");
                    let frontier = req.frontier.unwrap_or_default();
                    let want_scope = req.scope.clone();
                    // Two row sources, split by the requested item class: state
                    // entries for the class-2 walk, staged `record-cid` rows
                    // for a content walk (empty unless a test staged some —
                    // which is what keeps the derived own-actor set walkable
                    // against this fake).
                    let state_class = ItemClass::StateEntry.as_wire().to_string();
                    let want_state_rows = req.item_class.as_ref() == Some(&state_class);
                    let record_class = ItemClass::RecordCid.as_wire().to_string();
                    let want_record_rows = req.item_class.as_ref() == Some(&record_class);
                    let mut s = self.state.lock().unwrap();
                    s.list_calls += 1;
                    if let Some(scope) = want_scope.clone() {
                        *s.list_calls_by_scope.entry(scope).or_default() += 1;
                    }
                    let mut changes: Vec<SyncChange> = s
                        .feed
                        .iter()
                        .filter(|_| want_state_rows)
                        .filter(|r| want_scope.as_ref().is_none_or(|want| &r.scope == want))
                        .filter(|r| r.writer_seq > frontier.get(&r.writer_id).copied().unwrap_or(0))
                        .map(|r| SyncChange {
                            seq: r.nest_seq,
                            path_hash: hex::encode(&r.item_key),
                            change_type: r.op.clone(),
                            item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                            origin_writer: Some(r.writer_id.clone()),
                            origin_seq: Some(r.writer_seq),
                            entry: Some(fauna_protocol::ByteBuf::from(r.entry.clone())),
                            ..Default::default()
                        })
                        .collect();
                    changes.extend(
                        s.content_rows
                            .iter()
                            .filter(|_| want_record_rows)
                            .filter(|r| want_scope.as_ref().is_none_or(|want| &r.scope == want))
                            // A content scope has one writer, so its cursor is
                            // the scalar `since` (W2.3 ruling (a)).
                            .filter(|r| r.seq > req.since)
                            .map(|r| SyncChange {
                                seq: r.seq,
                                path_hash: hex::encode(r.digest),
                                change_type: r.op.clone(),
                                item_class: Some(ItemClass::RecordCid.as_wire().to_string()),
                                ..Default::default()
                            }),
                    );
                    wire_encode(&SyncChangesListReply {
                        changes,
                        ..Default::default()
                    })
                    .expect("encode list reply")
                    .to_vec()
                }
                "fauna.sync.devices.list" => {
                    if self.roster_unreachable.load(Ordering::SeqCst) {
                        return Err(TestErr::transient("the roster is unreachable"));
                    }
                    let s = self.state.lock().unwrap();
                    wire_encode(&fauna_protocol::sync::SyncDevicesListReply {
                        devices: s
                            .rows
                            .iter()
                            .map(|row| fauna_protocol::sync::SyncDevice {
                                device_id: row.clone(),
                                label: String::new(),
                                label_sealed: None,
                                capabilities: String::new(),
                                registered_at: 0,
                                last_seen_at: 0,
                                online: false,
                                principal: None,
                                folders: Vec::new(),
                                guardian_marked: false,
                                p2p_participation: None,
                                p2p_off_requested: false,
                                extra: Default::default(),
                            })
                            .collect(),
                        extra: Default::default(),
                    })
                    .expect("encode devices list reply")
                    .to_vec()
                }
                // The peer leg's self-arm participation report, sent on every
                // pass until one is acknowledged — accepted as reported.
                fauna_protocol::sync::KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET => {
                    let req: fauna_protocol::sync::SyncDeviceP2pParticipationSetRequest =
                        decode_strict(&req_bytes).expect("decode p2p participation set");
                    wire_encode(&fauna_protocol::sync::SyncDeviceP2pParticipationSetReply {
                        participating: Some(req.participating),
                        off_requested: false,
                        extra: Default::default(),
                    })
                    .expect("encode p2p participation set reply")
                    .to_vec()
                }
                "fauna.sync.register" => {
                    let req: fauna_protocol::sync::SyncRegisterRequest =
                        decode_strict(&req_bytes).expect("decode sync register");
                    let mut s = self.state.lock().unwrap();
                    if self.at_device_cap.load(Ordering::SeqCst) && !s.rows.contains(&req.device_id)
                    {
                        return Err(TestErr::coded(
                            fauna_protocol::RpcError::CODE_SYNC_DEVICE_LIMIT_EXCEEDED,
                            "error.sync.device_limit_exceeded",
                        ));
                    }
                    s.sync_registers.push(req.device_id.clone());
                    s.rows.insert(req.device_id.clone());
                    wire_encode(&fauna_protocol::sync::SyncRegisterReply {
                        device_id: req.device_id,
                        extra: Default::default(),
                    })
                    .expect("encode register reply")
                    .to_vec()
                }
                "fauna.sync.device_grant.register" => {
                    let req: fauna_protocol::sync::DeviceGrantRegisterRequest =
                        decode_strict(&req_bytes).expect("decode grant register");
                    let (auth_bytes, _) = req
                        .authorization
                        .clone()
                        .into_signed()
                        .expect("a signed grant");
                    let auth: fauna_core::data::DeviceAuthorization =
                        fauna_core::encoding::decode_signed_bytes(&auth_bytes)
                            .expect("decode the grant");
                    let key = fauna_core::hex32::encode(&auth.device_key);
                    let mut s = self.state.lock().unwrap();
                    s.grant_register_attempts += 1;
                    // The real handler's order: the revocation memory FIRST
                    // (it answers even with the row DELETED — what makes the
                    // succession probe's grant-first shape ghost-free), then
                    // the row check (`set_sync_device_grant` →
                    // `GrantStoreOutcome::NoDevice`: a grant on no row would
                    // be invisible to the devices UI, i.e. unrevocable). Two
                    // revocation models: the sticky `revoke_grants` switch
                    // (every key — the version-mix tests) and the per-key
                    // `revoked_device_ids` tombstones (the succession revival,
                    // where the successor must register beside a dead key).
                    if self.revoke_grants.load(Ordering::SeqCst)
                        || s.revoked_device_ids.contains(&key)
                    {
                        if self.flap_revoked_grant_registers.load(Ordering::SeqCst) {
                            let fault = s.flap_fault_next;
                            s.flap_fault_next = !fault;
                            if fault {
                                return Err(TestErr::transient(
                                    "injected grant register transport fault",
                                ));
                            }
                        }
                        return Err(TestErr::coded(
                            "fauna.sync.device_grant_revoked",
                            "this renewal grant was revoked by device deletion",
                        ));
                    }
                    if !s.rows.contains(&req.device_id) {
                        return Err(TestErr::coded(
                            "fauna.sync.not_found",
                            "device not registered — call fauna.sync.register first",
                        ));
                    }
                    s.grant_registers.push(req.device_id.clone());
                    s.grant_keys.push(key.clone());
                    s.grant_on = Some(req.device_id);
                    s.granted_key = Some(key);
                    wire_encode(&fauna_protocol::sync::DeviceGrantRegisterReply {
                        registered: true,
                        extra: Default::default(),
                    })
                    .expect("encode grant register reply")
                    .to_vec()
                }
                // No escrow holder behind this nest by default (the fixture
                // trusts none): a first-need mint's deposit is refused, so
                // the door's no-tip refusal is what the write answers. With
                // `escrow_holder` set, the holder signs the deposit's receipt.
                fauna_protocol::generation_escrow::KIND_ESCROW_PUT => {
                    if !self.escrow_holder.load(Ordering::SeqCst) {
                        return Err(TestErr::coded(
                            "fauna.protocol.unknown_kind",
                            "this nest holds no escrow",
                        ));
                    }
                    let req: fauna_protocol::generation_escrow::EscrowPutRequest =
                        decode_strict(&req_bytes).expect("decode escrow put");
                    let receipt = fauna_core::generation::sign_escrow_receipt(
                        &escrow_holder_key(),
                        req.generation_id
                            .as_slice()
                            .try_into()
                            .expect("32-byte generation id"),
                        blake3::hash(&req.wrap).into(),
                        &req.target_key,
                        7_000,
                    );
                    self.state
                        .lock()
                        .unwrap()
                        .escrow_wraps
                        .push((req.generation_id.to_vec(), req.wrap.to_vec()));
                    wire_encode(&fauna_protocol::generation_escrow::EscrowPutReply {
                        receipt: canonical_encode(&receipt).expect("encode receipt").into(),
                        extra: Default::default(),
                    })
                    .expect("encode escrow put reply")
                    .to_vec()
                }
                // The holder's read door: every wrap a deposit carried, or one
                // generation's. Refused like the deposit while no holder
                // stands behind this nest.
                fauna_protocol::generation_escrow::KIND_ESCROW_GET => {
                    if !self.escrow_holder.load(Ordering::SeqCst) {
                        return Err(TestErr::coded(
                            "fauna.protocol.unknown_kind",
                            "this nest holds no escrow",
                        ));
                    }
                    if self.escrow_reads_unreachable.load(Ordering::SeqCst) {
                        return Err(TestErr::transient(
                            "the escrow holder's read door is unreachable",
                        ));
                    }
                    let req: fauna_protocol::generation_escrow::EscrowGetRequest =
                        decode_strict(&req_bytes).expect("decode escrow get");
                    let s = self.state.lock().unwrap();
                    let empty = self.escrow_reads_empty.load(Ordering::SeqCst);
                    wire_encode(&fauna_protocol::generation_escrow::EscrowGetReply {
                        wraps: s
                            .escrow_wraps
                            .iter()
                            .filter(|_| !empty)
                            .filter(|(generation, _)| {
                                req.generation_id
                                    .as_ref()
                                    .is_none_or(|want| want.as_slice() == generation.as_slice())
                            })
                            .map(|(generation, wrap)| {
                                fauna_protocol::generation_escrow::EscrowWrapRow {
                                    generation_id: generation.clone().into(),
                                    wrap: wrap.clone().into(),
                                    deposited_at_ms: 7_000,
                                    extra: Default::default(),
                                }
                            })
                            .collect(),
                        extra: Default::default(),
                    })
                    .expect("encode escrow get reply")
                    .to_vec()
                }
                "fauna.sync.device_grant.revoke" => {
                    if self.no_revoke_kind.load(Ordering::SeqCst) {
                        return Err(TestErr::coded(
                            "fauna.protocol.unknown_kind",
                            "the revoke door refused",
                        ));
                    }
                    let req: fauna_protocol::sync::DeviceGrantRevokeRequest =
                        decode_strict(&req_bytes).expect("decode grant revoke");
                    let mut s = self.state.lock().unwrap();
                    // The real handler, reduced to what a runtime test can
                    // observe: the grant columns clear, the key is
                    // tombstoned, and the named row stays.
                    let cleared = s.grant_on.is_some();
                    s.grant_on = None;
                    s.granted_key = None;
                    s.revoked_device_ids.insert(req.device_key.clone());
                    s.grant_revokes.push(req.device_key);
                    wire_encode(&fauna_protocol::sync::DeviceGrantRevokeReply {
                        revoked: cleared,
                        sessions_revoked: 0,
                        extra: Default::default(),
                    })
                    .expect("encode grant revoke reply")
                    .to_vec()
                }
                // The full pass's capability reconcile sweep enumerates
                // before its fleet walk; this box holds no grant rows, so
                // the sweep judges an empty answer and revokes nothing.
                "fauna.capabilities.reconcile" => {
                    wire_encode(&fauna_protocol::wrapped_blob::ReconcileGrantsReply::default())
                        .expect("encode reconcile grants reply")
                        .to_vec()
                }
                // A box that never rotated serves an empty chain.
                fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND => {
                    wire_encode(&fauna_protocol::nest_rotation::RotationChainReply::default())
                        .expect("encode rotation chain reply")
                        .to_vec()
                }
                other => panic!("FakeNest: unhandled kind {other}"),
            };
            Ok(decode_strict(&reply_bytes).expect("decode reply"))
        }
    }

    /// The outbox-drain seam: record every attempt (kind + the envelope key
    /// the drain supplied — the tests assert it is the STORED intent id),
    /// then reply trivially or fire a one-shot failure switch.
    impl fauna_protocol::KeyedRpcRequester for FakeNest {
        async fn request_keyed<Req, Reply>(
            &self,
            kind: &'static str,
            idempotency_key: [u8; 16],
            payload: Req,
        ) -> Result<Reply, TestErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.note_stack_depth();
            let _ = wire_encode(&payload).expect("encode keyed request");
            self.state
                .lock()
                .unwrap()
                .keyed_requests
                .push((kind.to_string(), idempotency_key));
            if self.fail_next_keyed.swap(false, Ordering::SeqCst) {
                return Err(TestErr::transient("injected keyed transport fault"));
            }
            if self.reject_next_keyed.swap(false, Ordering::SeqCst) {
                return Err(TestErr::rejection("injected keyed rejection"));
            }
            let reply_bytes = wire_encode(&0u8).expect("encode keyed reply");
            Ok(decode_strict(&reply_bytes).expect("decode keyed reply"))
        }
    }

    /// An explicit named `sync_devices` row, for the tests that pin the
    /// target independently of the fixture's own [`named_row`].
    const NAMED_ROW: &str = "5151515151515151515151515151515151515151515151515151515151515151";

    /// The named row of the fixture device `device` — a distinct 64-hex id per
    /// device, since two devices of one account are two machines.
    fn named_row(device: &str) -> String {
        format!("{:0<64}", hex::encode(device.as_bytes()))
    }

    /// A generous ceiling for the eventually-asserts below (e2e convention
    /// 14: a named budget + deadline poll — green runs pay only the poll).
    const EVENTUALLY_BUDGET: Duration = Duration::from_secs(30);

    async fn eventually(mut pred: impl FnMut() -> bool, what: &str) {
        tokio::time::timeout(EVENTUALLY_BUDGET, async {
            while !pred() {
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("eventually({what}): not reached within budget"));
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        fake: FakeNest,
        secret: [u8; 32],
        actor_hex: String,
        base: PathBuf,
        cred_dir: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let kp = ActorKeypair::generate();
        let secret = *kp.secret_bytes();
        let actor_hex = kp.actor_id_hex();
        let base = dir.path().join("state");
        let cred_dir = dir.path().join("creds");
        std::fs::create_dir_all(&cred_dir).expect("cred dir");
        Fixture {
            _dir: dir,
            fake: FakeNest::default(),
            secret,
            actor_hex,
            base,
            cred_dir,
        }
    }

    impl Fixture {
        /// Params for one device of this account. `device` picks a distinct
        /// state + credential area, so two devices are two writers over the
        /// same fake nest — never the OS keyring (testing.md § point 10).
        fn params(&self, device: &str) -> AccountRuntimeParams<FakeNest> {
            AccountRuntimeParams {
                store_root: StoreRoot::at(self.base.join(device)),
                store_backup_exclusion: CloudBackupExclusion::NotApplicable {
                    platform: "test".into(),
                },
                actor_id_hex: self.actor_hex.clone(),
                rpc: self.fake.clone(),
                process_rpc: None,
                principal: RuntimePrincipal::SeedHolding(
                    ActorKeypair::from_secret(self.secret).into(),
                ),
                credentials: CredentialStore::with_file_backend(
                    CRED_NAMESPACE,
                    self.cred_dir.join(device),
                ),
                reconnects: None,
                pushes: None,
                backstop_interval: Duration::from_secs(3600), // disarmed unless a test shrinks it
                memberships: None,
                // `Gen0` preference kinds only here — the R14 door admits
                // those without a tip, so no holder needs trusting. The
                // production consumer path is `conformance_account_state_walk`.
                trusted_escrow_holders: fixed_holders(Vec::new()),
                attested_predecessors: Default::default(),
                linked_nests: None,
                owed_nests: None,
                peer_transport: None,
                // The machine's named row: every device of the account is its
                // own machine, so its own row (`named_row`).
                enrollment_target_device_id: named_row(device),
            }
        }

        /// [`Self::params`] with this nest's escrow holder standing and
        /// trusted — a device whose first-need mint completes, after which
        /// an admissible generation tip resolves.
        fn params_trusting_the_holder(&self, device: &str) -> AccountRuntimeParams<FakeNest> {
            self.fake.escrow_holder.store(true, Ordering::SeqCst);
            AccountRuntimeParams {
                trusted_escrow_holders: fixed_holders(vec![
                    escrow_holder_key().verifying_key().to_bytes(),
                ]),
                ..self.params(device)
            }
        }
    }

    /// **W5.4b — the enrollment ceremony's nest legs run once, then latch.**
    /// The assembly minted the grant (seed in hand, inside the migration
    /// section); the prologue's pass registers the machine row + grant
    /// through the ordinary requester; every later pass answers `Current`
    /// from the content-addressed latch — no repeat RPC. Deterministic
    /// without waits: `reconcile_now` is pass-bound, so the first one runs
    /// after the prologue and already observes the registered state.
    #[tokio::test]
    async fn the_enrollment_ceremony_registers_once_and_latches() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");

        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(
            report.enrollment,
            Some(EnrollmentPass::Current),
            "the prologue registered; the latch answers"
        );
        // The pump-cycle counters (the e2e pump barrier's shared counting):
        // this read runs AFTER an awaited command, so the prologue + that
        // pass have both counted — the counters are a plain atomic read and
        // do NOT wait for the prologue the way a pass-bound command does,
        // which is why the baseline sits here and not before the first
        // reconcile_now.
        let (started_1, completed_1) = handle.pump_cycles();
        assert!(
            started_1 >= 2 && completed_1 >= 2,
            "prologue + pass counted"
        );
        assert_eq!(started_1, completed_1, "no pass in flight between commands");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let (started_2, completed_2) = handle.pump_cycles();
        assert_eq!(started_2, started_1 + 1, "each reconcile_now counts");
        assert_eq!(completed_2, completed_1 + 1);

        let (regs, grants) = {
            let s = fx.fake.state.lock().unwrap();
            (s.sync_registers.clone(), s.grant_registers.clone())
        };
        assert_eq!(regs.len(), 1, "one machine row, registered once: {regs:?}");
        assert_eq!(
            grants, regs,
            "the grant registered under the same device id"
        );
        // The row is the machine's named row, never a row keyed by the writer
        // key: the principal IS the writer key (T10), but the machine is known
        // by one row, the app's own id (the RULED 2026-09-28 block).
        let bundle = handle.principal_bundle_status().await.expect("status");
        let auth = bundle
            .device_authorization
            .expect("the assembly minted the grant");
        assert_eq!(regs[0], named_row("a"));
        assert_ne!(regs[0], fauna_core::hex32::encode(&auth.device_key));

        handle.shutdown().await;
    }

    /// **A successor's first assembly reaches ready while it carries its
    /// predecessor's generation keys** — the in-process succession hang
    /// (measured on tui 2026-09-30: `start` neither returned nor failed, so no
    /// host's post-store-ready pass ever ran for the successor).
    ///
    /// The carriage writes each key under the slot's own section, which is
    /// the store dir's `migration.lock`; the assembly used to run it while it
    /// already held that lock for the writer-key resolve. `flock` lives on
    /// the open file description, so the second acquire in the same thread
    /// blocked on the first for ever. Only a successor with keys to carry
    /// takes the path, which is why every ordinary launch stayed green.
    ///
    /// Red-verified: with the carriage back inside the section, `start`
    /// times out here.
    #[tokio::test]
    async fn a_successor_carrying_predecessor_keys_reaches_ready() {
        use fauna_core::crypto::{BackupKey, GenerationKey};

        let fx = fixture();
        let predecessor = ActorKeypair::generate();
        let predecessor_hex = predecessor.actor_id_hex();
        // The predecessor's slot on this same machine, holding one retained
        // generation key — what a device that lived through the succession has.
        let generation = [0x5A; 32];
        let key = GenerationKey::mint();
        {
            let store_dir = StoreRoot::at(fx.base.join("a"))
                .store_dir(&predecessor_hex)
                .expect("predecessor store dir");
            let old = crate::principal_bundle::PrincipalSlot::resolve(
                Arc::new(CredentialStore::with_file_backend(
                    CRED_NAMESPACE,
                    fx.cred_dir.join("a"),
                )),
                predecessor_hex.clone(),
                store_dir,
                &[0x42; 32],
                Some(&BackupKey::derive(predecessor.secret_bytes())),
            );
            old.record_generation_key(&generation, &key);
        }

        let mut params = fx.params("a");
        params.attested_predecessors = AttestedPredecessors::from_backup_keys([(
            predecessor.actor_id(),
            &BackupKey::derive(predecessor.secret_bytes()),
        )]);
        let handle =
            tokio::time::timeout(Duration::from_secs(30), AccountStoreRuntime::start(params))
                .await
                .expect(
                    "the successor's assembly reaches ready (no self-deadlock on its own section)",
                )
                .expect("runtime");

        let bundle = handle.principal_bundle_status().await.expect("status");
        assert_eq!(
            bundle.retained_generations, 1,
            "the predecessor's key was carried into the successor's slot"
        );
        handle.shutdown().await;
    }

    /// **A successor's first gesture is applied to the list it inherited, not
    /// to an empty one** (`config-dissolution.md` § The `__config` dissolution
    /// schedule → *What replaces the bridge's two carriages*; the carry is
    /// `succession-aftermath.md` § Re-key scope's).
    ///
    /// The nest holds the predecessor's moderation row, sealed under the
    /// predecessor's schedule. The successor's runtime starts — `start`
    /// returns at assembly, before the prologue pass — and the page's add
    /// lands INSIDE that pass, which is held at its first walk so the order
    /// is the test's and not the scheduler's. The add is a read-modify-write:
    /// its read crosses the first-listing gate, so it waits for the listing
    /// that carries the predecessor's two words, and the write goes on top of
    /// them.
    ///
    /// The muted-words page's level picker: a term's level is set as a delta
    /// over the stored record (`preference_surfaces::set_muted_word_level`),
    /// the other rows keep their weights, the set's answer is what a fresh
    /// read returns, a term the list does not hold is a success no-op, and
    /// the level reads back through the shared threshold
    /// (`MutedKeyword::level`).
    #[tokio::test]
    async fn a_muted_words_level_is_set_as_a_delta_and_reads_back() {
        use crate::preference_surfaces::{add_muted_word, load_muted_words, set_muted_word_level};
        use fauna_core::scoring::{
            MUTED_KEYWORD_SHOW_LESS_WEIGHT, MUTED_KEYWORDS_PENALTY, MutedKeywordLevel,
        };

        let fx = fixture();
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start a");
        a.reconcile_now().await.expect("a enrolls");
        add_muted_word(&a, "Politics")
            .await
            .expect("a's first word");
        add_muted_word(&a, "spoiler")
            .await
            .expect("a's second word");

        let softened = set_muted_word_level(&a, "Politics", MutedKeywordLevel::ShowLess)
            .await
            .expect("the level is set");
        assert!(softened.loaded);
        assert_eq!(softened.keywords[0].keyword, "Politics");
        assert_eq!(softened.keywords[0].weight, MUTED_KEYWORD_SHOW_LESS_WEIGHT);
        assert_eq!(softened.keywords[0].level(), MutedKeywordLevel::ShowLess);
        assert_eq!(
            softened.keywords[1].level(),
            MutedKeywordLevel::Hide,
            "the other term keeps the default weight"
        );
        assert_eq!(
            load_muted_words(&a).await.expect("a reads"),
            softened,
            "the set answers with the stored record"
        );

        let unknown = set_muted_word_level(&a, "nope", MutedKeywordLevel::ShowLess)
            .await
            .expect("an unknown term is a success no-op");
        assert_eq!(unknown, softened);

        let hidden = set_muted_word_level(&a, "Politics", MutedKeywordLevel::Hide)
            .await
            .expect("the level is set back");
        assert_eq!(hidden.keywords[0].weight, MUTED_KEYWORDS_PENALTY);
        assert_eq!(hidden.keywords[0].level(), MutedKeywordLevel::Hide);
    }

    /// Red-verified: with the gate out of `load_record` the read is served
    /// inside the held pass and answers the empty store, the add stores
    /// `["third"]`, and that newer stamp then outranks the inherited list for
    /// good.
    #[tokio::test]
    async fn a_successors_first_muted_word_lands_on_the_list_it_inherited() {
        use fauna_account_plane::account_state_plane::{AccountStatePlane, ItemId};
        use fauna_account_store::sqlite::SqliteBackend;
        use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
        use fauna_core::data::ModerationConfig;
        use fauna_protocol::merge_policy::{KIND_MODERATION, LwwStamp, PREFERENCE_KEY};

        let fx = fixture();
        let predecessor = ActorKeypair::generate();
        let predecessor_key = BackupKey::derive(predecessor.secret_bytes());
        // The predecessor's device publishes its muted words, long ago.
        {
            let device = ed25519_dalek::SigningKey::from_bytes(&[0x3D; 32]);
            let writer = WriterId(device.verifying_key().to_bytes());
            let store = AccountStore::open(
                SqliteBackend::open_in_memory().expect("store"),
                &predecessor.actor_id_hex(),
                writer,
            )
            .await
            .expect("the predecessor's store");
            let schedule = AccountStateKeySchedule::derive(&predecessor_key);
            let trust = crate::generation_tip::GenerationTrust {
                root: predecessor.actor_id(),
                prior: Vec::new(),
                trusted_holders: Default::default(),
            };
            AccountStatePlane::new(
                &store,
                &fx.fake,
                &schedule,
                &device,
                &trust,
                ACCOUNT_STATE_SCOPE,
            )
            .expect("the predecessor's plane")
            .put(
                &ItemId {
                    kind: KIND_MODERATION.into(),
                    key: PREFERENCE_KEY.into(),
                },
                fauna_core::encoding::canonical_encode(&ModerationConfig {
                    muted_keywords: vec!["first".into(), "second".into()],
                    ..Default::default()
                })
                .expect("encode"),
                Some(
                    LwwStamp {
                        at_ms: 1,
                        writer: writer.0,
                    }
                    .encode()
                    .expect("stamp"),
                ),
            )
            .await
            .expect("the predecessor's row reaches the nest");
        }

        let mut params = fx.params("a");
        params.attested_predecessors =
            AttestedPredecessors::from_backup_keys([(predecessor.actor_id(), &predecessor_key)]);
        // The prologue is held at its first walk: nothing has been carried.
        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let handle = AccountStoreRuntime::start(params).await.expect("runtime");
        fx.fake.list_stalled.notified().await;
        // The page's own gesture, with no `settled` and no `reconcile_now`
        // ahead of it. While the first listing is held the add cannot answer —
        // local commands ARE served inside a pass, so this is the gate
        // waiting, not the command queue.
        let add = crate::preference_surfaces::add_muted_word(&handle, "third");
        tokio::pin!(add);
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut add)
                .await
                .is_err(),
            "the add waits for the scope's first listing"
        );
        fx.fake.release_list.notify_one();
        let snap = tokio::time::timeout(EVENTUALLY_BUDGET, add)
            .await
            .expect("the first pass ends and the add answers")
            .expect("the successor's first add");
        assert_eq!(
            snap.terms(),
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string()
            ],
            "the add was applied to the inherited list"
        );
        handle.shutdown().await;
    }

    /// What [`a_first_write_on_a_replica_that_listed_nothing`] found.
    struct UnlistedWrite {
        /// What the second device's add answered while it had listed nothing.
        while_unlisted: Result<Vec<String>>,
        /// What the page's own read — the twin every app calls — answered
        /// then.
        on_the_page: Result<Vec<String>, String>,
        /// What the same add answered once the device had listed the scope.
        retried: Vec<String>,
        /// What the first device holds once both have walked.
        on_first: Vec<String>,
        /// What the second device holds once both have walked.
        on_second: Vec<String>,
    }

    /// The scenario both never-listed tests share: a first device publishes a
    /// two-word muted list; a second device of the same account starts while
    /// `outage` holds, so its first pass ends without its listing, and the
    /// page adds one word; `outage` lifts, the second device lists, and the
    /// page retries the add.
    async fn a_first_write_on_a_replica_that_listed_nothing(
        fx: &Fixture,
        outage: &AtomicBool,
    ) -> UnlistedWrite {
        use crate::preference_surfaces::{add_muted_word, load_muted_words};

        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start a");
        a.reconcile_now().await.expect("a enrolls");
        add_muted_word(&a, "first").await.expect("a's first word");
        add_muted_word(&a, "second").await.expect("a's second word");
        a.reconcile_now().await.expect("a publishes its list");
        // A latest-wins stamp is wall-clock milliseconds, the writer id the
        // tie-break: B's write must fall in a later millisecond than A's for
        // the order to be the test's.
        tokio::time::sleep(Duration::from_millis(5)).await;

        let walks_before = fx.fake.list_calls();
        outage.store(true, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("start b");
        eventually(|| b.pump_cycles().1 > 0, "b's first pass ended").await;
        assert_eq!(
            fx.fake.list_calls(),
            walks_before,
            "b's first pass was answered no listing"
        );
        let while_unlisted =
            tokio::time::timeout(Duration::from_secs(5), add_muted_word(&b, "third"))
                .await
                .expect("the gate answers at once when no pass is in flight")
                .map(|snap| snap.terms());
        let on_the_page = crate::preference_surfaces::load_muted_words(&b)
            .await
            .map(|snap| snap.terms())
            .map_err(crate::preference_surfaces::plane_failure);

        outage.store(false, Ordering::SeqCst);
        b.reconcile_now().await.expect("b lists");
        let retried = add_muted_word(&b, "third")
            .await
            .expect("b's add, retried after its first listing")
            .terms();
        b.reconcile_now().await.expect("b publishes");
        a.reconcile_now().await.expect("a walks");
        let on_first = load_muted_words(&a).await.expect("a reads").terms();
        let on_second = load_muted_words(&b).await.expect("b reads").terms();
        a.shutdown().await;
        b.shutdown().await;
        UnlistedWrite {
            while_unlisted,
            on_the_page,
            retried,
            on_first,
            on_second,
        }
    }

    /// The refusal a read on a never-listed scope answers.
    fn refused_as_not_listed<T>(answer: &Result<T>) -> bool {
        answer
            .as_ref()
            .is_err_and(|e| e.is::<fauna_account_plane::account_driver::ScopeNotReady>())
    }

    fn all_three() -> Vec<String> {
        vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ]
    }

    /// **A replica that has never listed a scope answers no read of it, so a
    /// first write cannot destroy the account's list**
    /// (`account-client-lifecycle.md` § The client-side lifecycle → *The first
    /// listing*).
    ///
    /// The nest is unreachable while a freshly signed-in device runs its first
    /// pass, so the pass ends without its listing. The page's add is a
    /// read-edit-write, and its read is refused as not ready: nothing is put.
    /// Once the nest is back and the device has listed the scope, the retried
    /// add lands on top of the account's two words, and both devices hold all
    /// three.
    ///
    /// Red-verified against the pass-count barrier this gate replaced: the add
    /// answered `["third"]` from the empty store, and that newer stamp then
    /// outranked the account's list on both devices.
    #[tokio::test]
    async fn a_first_write_on_a_never_listed_replica_is_refused_until_its_first_listing() {
        let fx = fixture();
        let found =
            a_first_write_on_a_replica_that_listed_nothing(&fx, &fx.fake.nest_unreachable).await;
        assert!(
            refused_as_not_listed(&found.while_unlisted),
            "the add was refused as not ready, got {:?}",
            found.while_unlisted
        );
        assert_eq!(
            found.on_the_page,
            Err(fauna_i18n::strings::common::NEEDS_NEST.to_string()),
            "the page's read fails with the needs-nest reason, never a loaded empty list"
        );
        assert_eq!(
            found.retried,
            all_three(),
            "the retried add was applied to the account's list"
        );
        assert_eq!(
            found.on_first,
            all_three(),
            "the first device kept its list"
        );
        assert_eq!(found.on_second, all_three());
    }

    /// **A failed walk alone leaves the scope unlisted too** — the same
    /// scenario with the feed alone unreachable (the rest of the nest
    /// answers), and the add is refused all the same. Until closure step (5)
    /// of the dissolution schedule deleted the CAS-blob bridge, this pass
    /// still ended in the bridge's tick, whose import off the blob mirror
    /// did not make the scope listed either.
    ///
    /// Red-verified against the pass-count barrier: the add was applied to the
    /// imported list and answered all three words.
    #[tokio::test]
    async fn a_first_write_after_a_failed_walk_alone_is_refused_too() {
        let fx = fixture();
        let found =
            a_first_write_on_a_replica_that_listed_nothing(&fx, &fx.fake.walks_unreachable).await;
        assert!(
            refused_as_not_listed(&found.while_unlisted),
            "the add was refused as not ready, got {:?}",
            found.while_unlisted
        );
        assert_eq!(found.retried, all_three());
        assert_eq!(found.on_first, all_three());
        assert_eq!(found.on_second, all_three());
    }

    /// **The bound's arm: a first listing that outlasts `FIRST_PASS_WAIT`
    /// refuses the read** (*The first listing*, clause (2)). A prologue the
    /// generation machinery stretches to minutes makes this ordinary on a
    /// large account.
    ///
    /// The second device's first listing is held, and the test's clock is
    /// paused across the add, so the gate's bound is reached with no real
    /// wait. The read is refused, never answered from the store as it stands;
    /// once the listing is released and has run, the retried add lands on top
    /// of the account's list.
    ///
    /// Red-verified against the pass-count barrier: at the bound it logged and
    /// read the empty store, and the add answered `["third"]`.
    #[tokio::test]
    async fn a_first_listing_that_outlasts_the_bound_refuses_the_read() {
        use crate::preference_surfaces::add_muted_word;

        let fx = fixture();
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start a");
        a.reconcile_now().await.expect("a enrolls");
        add_muted_word(&a, "first").await.expect("a's first word");
        add_muted_word(&a, "second").await.expect("a's second word");
        a.reconcile_now().await.expect("a publishes its list");
        tokio::time::sleep(Duration::from_millis(5)).await;

        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("start b");
        fx.fake.list_stalled.notified().await;
        tokio::time::pause();
        let at_the_bound = add_muted_word(&b, "third").await.map(|snap| snap.terms());
        tokio::time::resume();
        assert!(
            refused_as_not_listed(&at_the_bound),
            "the add was refused at the bound, got {at_the_bound:?}"
        );

        fx.fake.release_list.notify_one();
        b.settled().await;
        let retried = add_muted_word(&b, "third")
            .await
            .expect("b's add, retried after its first listing")
            .terms();
        assert_eq!(retried, all_three());
        a.shutdown().await;
        b.shutdown().await;
    }

    /// **The fact is durable: a listed replica restarted while the nest is
    /// unreachable still reads and writes** (*The first listing*, clauses (1)
    /// and (2)). The launch's first read waits for this launch's first pass,
    /// which ends without a listing, and then reads the store as it stands —
    /// stale and real, never empty out of ignorance.
    ///
    /// And **a re-created store starts unlisted**: the same device with its
    /// store wiped is refused until it has listed again.
    ///
    /// Red-verified with the gate reading only this launch's listings: the
    /// offline restart's read was refused.
    #[tokio::test]
    async fn a_listed_replica_restarted_offline_reads_and_writes_and_a_wiped_one_does_not() {
        use crate::preference_surfaces::{add_muted_word, load_muted_words};

        let fx = fixture();
        let b = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("start b");
        b.reconcile_now().await.expect("b enrolls and lists");
        add_muted_word(&b, "first").await.expect("b's first word");
        b.reconcile_now().await.expect("b publishes");
        b.shutdown().await;

        fx.fake.nest_unreachable.store(true, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("restart b offline");
        let read = tokio::time::timeout(Duration::from_secs(10), load_muted_words(&b))
            .await
            .expect("the launch wait ends with the launch's first pass")
            .expect("a listed replica reads offline");
        assert_eq!(read.terms(), vec!["first".to_string()]);
        let wrote = add_muted_word(&b, "second")
            .await
            .expect("a listed replica writes offline");
        assert_eq!(
            wrote.terms(),
            vec!["first".to_string(), "second".to_string()]
        );
        b.shutdown().await;

        std::fs::remove_dir_all(fx.base.join("b")).expect("wipe b's store");
        let b = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("restart b on a re-created store");
        eventually(|| b.pump_cycles().1 > 0, "b's first pass ended").await;
        let read = load_muted_words(&b).await.map(|snap| snap.terms());
        assert!(
            refused_as_not_listed(&read),
            "a re-created store is unlisted, got {read:?}"
        );
        b.shutdown().await;
    }

    /// **The DNS management record's read crosses the gate on the fleet
    /// scope** (*The first listing*, clause (3)): the record is one
    /// whole-record latest-wins row, every write of it is a replace computed
    /// from a read, and a replace from a replica that has never listed the
    /// fleet scope is refused at that read.
    ///
    /// Red-verified before `AccountStoreHandle::dns` crossed the gate: the
    /// read answered the default record as loaded.
    #[tokio::test]
    async fn a_dns_replace_from_a_never_listed_replica_is_refused_at_its_read() {
        let fx = fixture();
        fx.fake.nest_unreachable.store(true, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("start b");
        eventually(|| b.pump_cycles().1 > 0, "b's first pass ended").await;
        let read = b.dns().await;
        assert!(
            refused_as_not_listed(&read),
            "the DNS read was refused as not ready, got {read:?}"
        );

        fx.fake.nest_unreachable.store(false, Ordering::SeqCst);
        b.reconcile_now().await.expect("b lists");
        b.dns()
            .await
            .expect("a listed replica reads its DNS record");
        b.shutdown().await;
    }

    /// **A contact overlay's save crosses the gate on the fleet scope** (*The
    /// first listing*, clause (3)): the overlay is a composite row whose
    /// nickname, notes and labels are stamped latest-wins registers, and the
    /// save's read-modify-write stamps every register it changes now. From a
    /// replica that has never listed the fleet scope, a note typed over the
    /// empty form would outrank the account's real note on every device — so
    /// the save is refused and nothing is put. Once the replica has listed, a
    /// save of another register leaves the first device's note in place.
    ///
    /// Only the second device's walks are unreachable while it is unlisted:
    /// its passes still mint it a generation tip, so its save is refused by
    /// the gate alone and never by the writer's no-tip refusal.
    ///
    /// Red-verified before `AccountStoreHandle::write_contact_overlay` crossed
    /// the gate: the never-listed replica's save was written.
    #[tokio::test]
    async fn a_contact_overlay_save_from_a_never_listed_replica_is_refused() {
        use crate::contact_overlay_rows::{OverlayWrite, OverlayWriteOutcome};
        use fauna_core::contact_overlay::OverlayChanges;

        let person = "ab".repeat(32);
        let notes = |text: &str| {
            OverlayWrite::Changes(OverlayChanges {
                notes: Some(Some(text.to_string())),
                ..Default::default()
            })
        };
        let fx = fixture();
        let a = AccountStoreRuntime::start(fx.params_trusting_the_holder("a"))
            .await
            .expect("start a");
        a.reconcile_now().await.expect("a enrolls");
        let wrote = a
            .write_contact_overlay(&person, notes("met at the conference"))
            .await
            .expect("a's note");
        assert!(
            matches!(wrote, OverlayWriteOutcome::Written(_)),
            "{wrote:?}"
        );
        a.reconcile_now().await.expect("a publishes its overlay");
        tokio::time::sleep(Duration::from_millis(5)).await;

        fx.fake.walks_unreachable.store(true, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(fx.params_trusting_the_holder("b"))
            .await
            .expect("start b");
        eventually(|| b.pump_cycles().1 > 0, "b's first pass ended").await;
        let blind = b
            .write_contact_overlay(&person, notes("typed over an empty form"))
            .await;
        assert!(
            refused_as_not_listed(&blind),
            "the save was refused as not ready, got {blind:?}"
        );

        fx.fake.walks_unreachable.store(false, Ordering::SeqCst);
        b.reconcile_now().await.expect("b lists");
        let renamed = b
            .write_contact_overlay(
                &person,
                OverlayWrite::Changes(OverlayChanges {
                    nickname: Some(Some("Sam".to_string())),
                    ..Default::default()
                }),
            )
            .await
            .expect("a listed replica saves");
        assert!(
            matches!(renamed, OverlayWriteOutcome::Written(_)),
            "{renamed:?}"
        );
        b.reconcile_now().await.expect("b publishes its nickname");
        a.reconcile_now().await.expect("a pulls b's nickname");
        for (device, handle) in [("a", &a), ("b", &b)] {
            let overlays = handle.contact_overlays().await.expect("overlays");
            let overlay = overlays.get(&person).expect("the person's overlay");
            assert_eq!(
                overlay.notes.value.as_deref(),
                Some("met at the conference"),
                "{device} kept a's note"
            );
            assert_eq!(overlay.nickname.value.as_deref(), Some("Sam"), "{device}");
        }
        a.shutdown().await;
        b.shutdown().await;
    }

    /// The source box whose backup-destination list the unopened-row pins
    /// write.
    const BACKUP_SOURCE_BOX: [u8; 32] = [0xB0; 32];

    fn backup_destination(id: &str) -> fauna_core::data::BackupDestination {
        fauna_core::data::BackupDestination {
            destination_id: id.to_string(),
            destination_nest_url: format!("https://{id}.example"),
            folder_name: "__mail".to_string(),
            ..Default::default()
        }
    }

    fn destination_ids(state: &fauna_core::backup_state::BackupState) -> Vec<String> {
        state
            .backup
            .destinations
            .iter()
            .map(|d| d.destination_id.clone())
            .collect()
    }

    fn domains(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// The scenario the unopened-row pins share: a first device mints the
    /// account's generation through the trusted holder and publishes a DNS
    /// record (two managed domains) and one box's backup-destination list
    /// (two destinations), both tip-sealed; a second device of the same
    /// account then starts and lists the fleet scope. No sibling has topped
    /// it up, so the holder's escrow wrap is the one thing that keys the
    /// first device's generation for it.
    ///
    /// With `escrow_fault` the holder's read door is unreachable while the
    /// second device passes — its deposit door still answers — so the
    /// recovery keys nothing and the first device's rows stay unopened; the
    /// fault is left standing for the caller to lift
    /// ([`the_escrow_fault_lifts_and_both_devices_walk`]).
    async fn a_second_device_that_has_listed_the_fleet_scope(
        fx: &Fixture,
        escrow_fault: bool,
    ) -> (AccountStoreHandle, AccountStoreHandle) {
        use crate::generation_escrow_recover::EscrowRecoveryPass;
        use fauna_core::data::{BackupConfig, DnsConfig};
        use fauna_protocol::merge_policy::KIND_GENERATION_MINT;

        let a = AccountStoreRuntime::start(fx.params_trusting_the_holder("a"))
            .await
            .expect("start a");
        a.reconcile_now().await.expect("a enrolls");
        assert!(
            a.write_dns(DnsConfig {
                managed_domains: domains(&["first.example", "second.example"]),
                ..Default::default()
            })
            .await
            .expect("a's DNS record"),
            "a's record was written"
        );
        a.write_backup_destinations(
            BACKUP_SOURCE_BOX,
            BackupConfig {
                destinations: vec![backup_destination("first"), backup_destination("second")],
            },
        )
        .await
        .expect("a's backup list");
        a.reconcile_now().await.expect("a publishes its rows");
        let generations = a
            .states_of_kind(KIND_GENERATION_MINT)
            .await
            .expect("a's mint rows")
            .len();
        assert_eq!(generations, 1, "the account has one generation");
        // A latest-wins stamp is wall-clock time, the writer id the
        // tie-break: b's write must fall later than a's for the order to be
        // the test's.
        tokio::time::sleep(Duration::from_millis(5)).await;

        fx.fake
            .escrow_reads_unreachable
            .store(escrow_fault, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(fx.params_trusting_the_holder("b"))
            .await
            .expect("start b");
        b.reconcile_now().await.expect("b enrolls");
        let report = b.reconcile_now().await.expect("b lists the fleet scope");
        let b_generations = b
            .states_of_kind(KIND_GENERATION_MINT)
            .await
            .expect("b's mint rows")
            .len();
        if escrow_fault {
            assert!(
                report
                    .errors
                    .iter()
                    .any(|e| e.contains("generation escrow recovery")),
                "b's recovery could not ask the holder: {:?}",
                report.errors
            );
            assert_eq!(report.generation_escrow_recovery, None, "nothing was keyed");
            assert!(
                report.fleet_walk.as_ref().is_some_and(|w| w.unopened >= 2),
                "b listed the fleet scope and left a's tip-sealed rows unopened: {:?}",
                report.fleet_walk
            );
            assert_eq!(
                b_generations,
                generations + 1,
                "b's own passes minted a second generation past the one it could not key, \
                 before any gesture"
            );
        } else {
            assert_eq!(
                report.fleet_walk.map(|w| w.unopened),
                Some(0),
                "b keyed a's generation from escrow and opened every row: {:?}",
                report.errors
            );
            assert_eq!(
                report.generation_escrow_recovery,
                Some(EscrowRecoveryPass::Current),
                "the recovery ran on an earlier pass"
            );
            assert_eq!(b_generations, generations, "b sealed under a's generation");
        }
        (a, b)
    }

    /// The escrow fault of [`a_second_device_that_has_listed_the_fleet_scope`]
    /// lifts and both devices walk until neither has anything left to merge.
    /// The second device's first pass keys the first device's generation from
    /// escrow and opens every row it had left unopened — asserted, so what
    /// the callers find afterwards is the merge's outcome and not a device
    /// that still cannot read.
    async fn the_escrow_fault_lifts_and_both_devices_walk(
        fx: &Fixture,
        a: &AccountStoreHandle,
        b: &AccountStoreHandle,
    ) {
        use crate::generation_escrow_recover::EscrowRecoveryPass;

        fx.fake
            .escrow_reads_unreachable
            .store(false, Ordering::SeqCst);
        let report = b.reconcile_now().await.expect("b publishes and recovers");
        assert_eq!(
            report.generation_escrow_recovery,
            Some(EscrowRecoveryPass::Recovered(1)),
            "b keyed a's generation from escrow: {:?}",
            report.errors
        );
        assert_eq!(
            report.fleet_rewalk.map(|w| w.unopened),
            Some(0),
            "and opened every row it had left unopened"
        );
        a.reconcile_now().await.expect("a walks");
        b.reconcile_now().await.expect("b walks again");
        a.reconcile_now().await.expect("a walks again");
    }

    /// Whom a refused read waits for, when the refusal is the gate's
    /// ([`fauna_account_plane::account_driver::ScopeNotReady`]); `None` for
    /// an answer or any other failure.
    fn refused_for<T>(answer: &Result<T>) -> Option<NotReadyReason> {
        answer
            .as_ref()
            .err()?
            .downcast_ref::<fauna_account_plane::account_driver::ScopeNotReady>()
            .map(|refusal| refusal.reason)
    }

    /// [`refused_for`] across a `StoreError` seam — the handle's
    /// `BackupStateStore` read, which production reads the backup list
    /// through: the refusal crosses as its not-ready text, which on a listed
    /// replica names the nest (the needs-nest text) or a sibling.
    fn seam_refused_for<T>(
        answer: &std::result::Result<T, fauna_client_config::StoreError>,
    ) -> Option<NotReadyReason> {
        match answer.as_ref().err()? {
            fauna_client_config::StoreError::Load(m)
                if m == fauna_client_config::LEDGER_NOT_READY =>
            {
                Some(NotReadyReason::HeldForNest)
            }
            fauna_client_config::StoreError::Load(m)
                if m == fauna_client_config::LEDGER_AWAITING_SIBLING =>
            {
                Some(NotReadyReason::HeldForSibling)
            }
            _ => None,
        }
    }

    /// The box's backup state as production reads it — through the handle's
    /// gated `BackupStateStore` seam, never the handle's own local fold.
    async fn backup_read(
        handle: &AccountStoreHandle,
    ) -> std::result::Result<fauna_core::backup_state::BackupState, fauna_client_config::StoreError>
    {
        fauna_client_config::BackupStateStore::backup_state(handle, BACKUP_SOURCE_BOX).await
    }

    /// Both devices walk until neither has anything left to merge: what the
    /// second device just wrote reaches the first, and each has keyed the
    /// other's generation by then.
    async fn both_devices_walk(a: &AccountStoreHandle, b: &AccountStoreHandle) {
        b.reconcile_now().await.expect("b publishes");
        a.reconcile_now().await.expect("a walks");
        b.reconcile_now().await.expect("b walks");
        a.reconcile_now().await.expect("a walks again");
    }

    /// **The unkeyed hold: a DNS edit on a listed replica that has not keyed
    /// the account's generation is refused at its read, and lands on the
    /// account's record once the key arrives** (`account-client-lifecycle.md`
    /// § The client-side lifecycle → *The first listing*, clause (5)).
    ///
    /// The second device has listed the fleet scope, so the first-listing
    /// gate admits it; the first device's record rests unopened under a
    /// generation the second device may still be keyed for — the holder is
    /// unasked (its read door is down) and the first device is its verified
    /// minter — so the read is refused as not ready, for want of the nest,
    /// and no edit starts. Once the fault lifts, the pass that recovers the
    /// key re-reads the scope, the read answers the account's two domains,
    /// and the retried add lands on them: three on both devices.
    ///
    /// Red-verified before the hold: the read answered the default record as
    /// loaded (the loss pin this test replaces asserted that the edit's newer
    /// stamp then destroyed the account's record on both devices).
    ///
    /// The control is
    /// [`a_dns_edit_on_a_listed_replica_that_opened_the_accounts_row_lands_on_it`]:
    /// the same scenario without the fault reads and keeps all three domains.
    #[tokio::test]
    async fn a_dns_read_held_for_an_unkeyed_generation_is_refused_until_the_key_arrives() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;

        let read = b.dns().await;
        assert_eq!(
            refused_for(&read),
            Some(NotReadyReason::HeldForNest),
            "the read is held while a's generation may still be keyed here, got {read:?}"
        );

        the_escrow_fault_lifts_and_both_devices_walk(&fx, &a, &b).await;
        let mut record = b.dns().await.expect("b reads once it keys the generation");
        assert_eq!(
            record.managed_domains,
            domains(&["first.example", "second.example"]),
            "the read answers the account's record"
        );
        record.managed_domains.insert("third.example".to_string());
        assert!(b.write_dns(record).await.expect("b's retried edit"));
        both_devices_walk(&a, &b).await;
        let all = domains(&["first.example", "second.example", "third.example"]);
        assert_eq!(a.dns().await.expect("a reads").managed_domains, all);
        assert_eq!(b.dns().await.expect("b reads").managed_domains, all);
        a.shutdown().await;
        b.shutdown().await;
    }

    /// **A device that recovers more generations from escrow than its slot's
    /// carriage holds still keys every one of them for the life of its
    /// process: it opens every row, mints nothing, and its gated reads
    /// answer** (`account-client-lifecycle.md` § Implementation status today
    /// → *the unkeyed hold*).
    ///
    /// Measured live 2026-10-05 on a remote box, on an account a test run had
    /// signed in afresh hundreds of times: the escrow recovery keyed 45
    /// generations, and each record rebuilt the bundle's in-memory view from
    /// the carriage trimmed to about fifteen, so the process kept only those
    /// and the newest. The rows under the rest stayed unopened, the hold
    /// stood for want of the nest, and every mail, backup and DNS read was
    /// refused as not ready; with the tip among the lost keys the pass could
    /// not seal either, and the next tip-sealed write minted one generation
    /// more, so the set grew with every launch.
    ///
    /// The account here gets its generations the way that run did: devices
    /// that could not ask the holder each mint their own. Red before the fix
    /// on the unopened rows of the re-walk and on the held DNS read.
    #[tokio::test]
    async fn a_device_that_recovers_more_generations_than_its_carriage_holds_keys_them_all() {
        use fauna_protocol::merge_policy::KIND_GENERATION_MINT;

        const MINTERS: usize = 20;
        let fx = fixture();
        fx.fake
            .escrow_reads_unreachable
            .store(true, Ordering::SeqCst);
        for n in 0..MINTERS {
            let minter =
                AccountStoreRuntime::start(fx.params_trusting_the_holder(&format!("m{n}")))
                    .await
                    .expect("start a minter");
            minter.reconcile_now().await.expect("the minter enrolls");
            minter.reconcile_now().await.expect("the minter mints");
            minter.shutdown().await;
        }
        fx.fake
            .escrow_reads_unreachable
            .store(false, Ordering::SeqCst);

        let fresh = AccountStoreRuntime::start(fx.params_trusting_the_holder("fresh"))
            .await
            .expect("start the fresh device");
        fresh
            .reconcile_now()
            .await
            .expect("the fresh device enrolls");
        let generations = fresh
            .states_of_kind(KIND_GENERATION_MINT)
            .await
            .expect("mint rows")
            .len();
        // The carriage holds fifteen
        // (`the_retained_carriage_is_bounded_by_the_credential_item_cap`).
        assert!(
            generations > 16,
            "the account carries more generations than the carriage holds: {generations}"
        );
        let report = fresh.reconcile_now().await.expect("the fresh device walks");
        assert_eq!(
            report.fleet_walk.as_ref().map(|w| w.unopened),
            Some(0),
            "every row opens under the keys the recovery obtained: {:?}",
            report.errors
        );
        assert_eq!(
            fresh
                .states_of_kind(KIND_GENERATION_MINT)
                .await
                .expect("mint rows")
                .len(),
            generations,
            "the fresh device keys the tip and mints nothing"
        );
        let read = fresh.dns().await;
        assert_eq!(
            refused_for(&read),
            None,
            "the DNS read answers, got {read:?}"
        );
        fresh.shutdown().await;
    }

    /// **The control: with the holder's read door answering, the same edit
    /// lands on top of the account's record.** The second device's pass keys
    /// the first device's generation from escrow and re-presents the rows it
    /// had left unopened, so the read answers the account's two domains and
    /// the edit adds a third — the fault is the whole difference between this
    /// test and
    /// [`a_dns_read_held_for_an_unkeyed_generation_is_refused_until_the_key_arrives`].
    #[tokio::test]
    async fn a_dns_edit_on_a_listed_replica_that_opened_the_accounts_row_lands_on_it() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, false).await;

        let mut record = b.dns().await.expect("b reads");
        assert_eq!(
            record.managed_domains,
            domains(&["first.example", "second.example"]),
            "the read answered the account's record"
        );
        record.managed_domains.insert("third.example".to_string());
        assert!(b.write_dns(record).await.expect("b's edit"));

        b.reconcile_now().await.expect("b publishes");
        a.reconcile_now().await.expect("a walks");
        let all = domains(&["first.example", "second.example", "third.example"]);
        assert_eq!(a.dns().await.expect("a reads").managed_domains, all);
        assert_eq!(b.dns().await.expect("b reads").managed_domains, all);
        a.shutdown().await;
        b.shutdown().await;
    }

    /// **The same hold on the backup list: adding a destination on a listed
    /// replica that has not keyed the account's generation is refused at its
    /// read, and lands on the box's list once the key arrives** — the
    /// scenario and the reading of
    /// [`a_dns_read_held_for_an_unkeyed_generation_is_refused_until_the_key_arrives`],
    /// on `fauna.state.backup`'s `destinations/<source nest>` row, which is
    /// latest-wins on its own embedded stamp.
    ///
    /// Red-verified before the hold: the read answered an empty list as
    /// loaded.
    #[tokio::test]
    async fn a_backup_read_held_for_an_unkeyed_generation_is_refused_until_the_key_arrives() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;

        let read = backup_read(&b).await;
        assert_eq!(
            seam_refused_for(&read),
            Some(NotReadyReason::HeldForNest),
            "the read is held while a's generation may still be keyed here, got {read:?}"
        );

        the_escrow_fault_lifts_and_both_devices_walk(&fx, &a, &b).await;
        let mut list = backup_read(&b)
            .await
            .expect("b reads once it keys the generation")
            .backup;
        list.destinations.push(backup_destination("third"));
        b.write_backup_destinations(BACKUP_SOURCE_BOX, list)
            .await
            .expect("b's retried edit");
        both_devices_walk(&a, &b).await;
        let all = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];
        assert_eq!(
            destination_ids(&a.backup_state(BACKUP_SOURCE_BOX).await.expect("a reads")),
            all
        );
        assert_eq!(
            destination_ids(&b.backup_state(BACKUP_SOURCE_BOX).await.expect("b reads")),
            all
        );
        a.shutdown().await;
        b.shutdown().await;
    }

    /// The holder's read door comes back answering with no wrap at all, and
    /// the second device's next pass asks it: the recovery hears that
    /// nothing opens, keys nothing and re-walks nothing.
    async fn the_holder_answers_with_no_wrap(fx: &Fixture, b: &AccountStoreHandle) {
        use crate::generation_escrow_recover::EscrowRecoveryPass;

        fx.fake.escrow_reads_empty.store(true, Ordering::SeqCst);
        fx.fake
            .escrow_reads_unreachable
            .store(false, Ordering::SeqCst);
        let report = b.reconcile_now().await.expect("b asks the holder");
        assert_eq!(
            report.generation_escrow_recovery,
            Some(EscrowRecoveryPass::Current),
            "the holder answered and nothing opened: {:?}",
            report.errors
        );
        assert_eq!(report.fleet_rewalk, None, "nothing was keyed to re-read");
    }

    /// **A holder's empty answer leaves the minter's hold standing, and the
    /// refusal names the sibling** (clause (5), source (c)). With the holder
    /// answered, only the first device — the generation's verified minter,
    /// still enrolled — can hand this one the key, so both tip-sealed reads
    /// stay refused, now with the reason that says another of the account's
    /// devices holds what this one needs.
    ///
    /// Red-verified before the hold: both reads answered the store as it
    /// stands.
    #[tokio::test]
    async fn a_holder_that_answers_empty_leaves_the_hold_on_the_minter() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;
        the_holder_answers_with_no_wrap(&fx, &b).await;

        let read = b.dns().await;
        assert_eq!(
            refused_for(&read),
            Some(NotReadyReason::HeldForSibling),
            "the minter is the one source left, got {read:?}"
        );
        let read = backup_read(&b).await;
        assert_eq!(
            seam_refused_for(&read),
            Some(NotReadyReason::HeldForSibling)
        );
        a.shutdown().await;
        b.shutdown().await;
    }

    /// **Removing the minter ends a hold the holder has answered** (clause
    /// (5), *How a hold ends*, (c)): with the holder's empty answer recorded
    /// and the first device removed on the Devices page, no living device
    /// and no holder can open those rows, so the read answers what the store
    /// holds.
    #[tokio::test]
    async fn removing_the_minter_after_the_holder_answers_empty_ends_the_hold() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;
        the_holder_answers_with_no_wrap(&fx, &b).await;
        let minter = fleet_id_of(&a).await;
        a.shutdown().await;

        remove_and_publish(&b, minter).await;
        assert_eq!(
            b.dns().await.expect("no source of the key stands"),
            fauna_core::data::DnsConfig::default(),
            "the read answers the store as it stands"
        );
        b.shutdown().await;
    }

    /// **The hold is durable** (clause (5), *The fact*): a held device
    /// relaunched with its nest unreachable runs no listing and asks no
    /// holder, and its read is still refused — the generation it may yet be
    /// keyed for was recorded by the listing that left its rows unopened.
    ///
    /// Red-verified before the hold: the relaunched read answered the
    /// default record.
    #[tokio::test]
    async fn a_held_replica_relaunched_offline_stays_held() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;
        b.shutdown().await;

        fx.fake.nest_unreachable.store(true, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(fx.params_trusting_the_holder("b"))
            .await
            .expect("relaunch b");
        eventually(|| b.pump_cycles().1 > 0, "b's first pass ended").await;
        let read = b.dns().await;
        assert_eq!(
            refused_for(&read),
            Some(NotReadyReason::HeldForNest),
            "the relaunched read is held, got {read:?}"
        );
        a.shutdown().await;
        b.shutdown().await;
    }

    /// **The holder's answer is durable** (clause (5), *The fact*, the
    /// answered-empty bit): once the holder has answered with no wrap that
    /// opens and the minter is removed, a relaunch with the nest unreachable
    /// — no listing, no question to the holder — answers the read. Without
    /// the bit, the receipt would hold this seed-holding device again until
    /// its runtime could ask once more.
    #[tokio::test]
    async fn a_replica_relaunched_offline_after_the_holder_answered_empty_is_not_held() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;
        the_holder_answers_with_no_wrap(&fx, &b).await;
        let minter = fleet_id_of(&a).await;
        a.shutdown().await;
        remove_and_publish(&b, minter).await;
        b.shutdown().await;

        fx.fake.nest_unreachable.store(true, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(fx.params_trusting_the_holder("b"))
            .await
            .expect("relaunch b");
        eventually(|| b.pump_cycles().1 > 0, "b's first pass ended").await;
        assert_eq!(
            b.dns().await.expect("the holder's answer is remembered"),
            fauna_core::data::DnsConfig::default()
        );
        b.shutdown().await;
    }

    /// **What a hold leaves alone** (clause (5), *The gate*): while a
    /// generation holds, a per-item tip-sealed put lands, a preference read
    /// answers, and the device's own pass still runs its writers — the
    /// device-endpoints step among them. Only a gated read of a tip-sealed
    /// kind waits.
    #[tokio::test]
    async fn a_hold_refuses_only_the_gated_tip_sealed_reads() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;
        assert!(refused_for(&b.dns().await).is_some(), "b is held");
        // The contact overlay's save re-stamps its registers from a
        // store-thread read of a tip-sealed kind, so it crosses the hold.
        let save = b
            .write_contact_overlay(
                &"ab".repeat(32),
                crate::contact_overlay_rows::OverlayWrite::Changes(
                    fauna_core::contact_overlay::OverlayChanges {
                        notes: Some(Some("typed over an empty form".to_string())),
                        ..Default::default()
                    },
                ),
            )
            .await;
        assert_eq!(
            refused_for(&save),
            Some(NotReadyReason::HeldForNest),
            "the overlay's save is held, got {save:?}"
        );

        assert!(
            b.put_follow(fauna_core::data::FollowedFolder {
                owner_actor_id: "ab".repeat(32),
                folder_id: 7,
                display_name: "a folder".to_string(),
                ..Default::default()
            })
            .await
            .expect("a per-item put lands while held"),
            "the follow was written"
        );
        crate::preference_surfaces::load_muted_words(&b)
            .await
            .expect("a preference read answers while held");
        let report = b.reconcile_now().await.expect("b's pass");
        assert!(
            report.device_endpoints.is_some(),
            "the device-endpoints step ran: {:?}",
            report.errors
        );
        assert!(
            !report.errors.iter().any(|e| e.contains("endpoints")),
            "and wrote without error: {:?}",
            report.errors
        );
        assert!(refused_for(&b.dns().await).is_some(), "b is still held");
        a.shutdown().await;
        b.shutdown().await;
    }

    /// The machine's seedless host over the second device's store, once that
    /// device's own runtime has stopped: it loads the principal the sign-in
    /// enrolled and asks no holder.
    async fn the_seedless_host_of(fx: &Fixture, device: &str) -> AccountStoreHandle {
        let agent = AccountStoreRuntime::start(AccountRuntimeParams {
            principal: RuntimePrincipal::Seedless,
            ..fx.params_trusting_the_holder(device)
        })
        .await
        .expect("the seedless host");
        eventually(|| agent.pump_cycles().1 > 0, "the host's first pass ended").await;
        agent
    }

    /// **A seedless runtime is held by a sibling** (clause (5), bound (ii)):
    /// it never asks the holder, so the receipt holds nothing for it, but
    /// the first device — the generation's verified minter — still can hand
    /// the machine the key, so the read is refused with the sibling reason.
    #[tokio::test]
    async fn a_seedless_runtime_is_held_by_a_sibling_that_minted_the_generation() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;
        b.shutdown().await;

        let agent = the_seedless_host_of(&fx, "b").await;
        let read = agent.dns().await;
        assert_eq!(
            refused_for(&read),
            Some(NotReadyReason::HeldForSibling),
            "the minter holds the seedless host, got {read:?}"
        );
        a.shutdown().await;
        agent.shutdown().await;
    }

    /// **A seedless runtime is never held by the receipt** (clause (5),
    /// bound (ii)): with the minter removed, the seed-holding device is still
    /// held — its runtime would ask the holder — but the machine's seedless
    /// host, which never asks, reads the store as it stands.
    #[tokio::test]
    async fn a_seedless_runtime_is_not_held_by_the_receipt_alone() {
        let fx = fixture();
        let (a, b) = a_second_device_that_has_listed_the_fleet_scope(&fx, true).await;
        let minter = fleet_id_of(&a).await;
        a.shutdown().await;
        remove_and_publish(&b, minter).await;
        let read = b.dns().await;
        assert_eq!(
            refused_for(&read),
            Some(NotReadyReason::HeldForNest),
            "the seed-holding device is held by the holder it has not asked, got {read:?}"
        );
        b.shutdown().await;

        let agent = the_seedless_host_of(&fx, "b").await;
        assert_eq!(
            agent.dns().await.expect("the seedless host is not held"),
            fauna_core::data::DnsConfig::default()
        );
        agent.shutdown().await;
    }

    /// **The sign-out leg: a sign-out's stop retires the enrollment nest-side,
    /// clearing the named row's grant and keeping the row**
    /// (`sync-agent-credentials.md` § Credential model → *The signed-out
    /// reconcile*, the nest-side leg, built 2026-09-14; the RULED 2026-09-28
    /// block, decision 4). Before 2026-09-14 a sign-out erased the writer key
    /// locally and told the nest nothing, so a live grant nobody held survived
    /// every sign-in cycle. The row itself stays — it carries the user's name
    /// for the machine, and the next sign-in enrolls onto it again.
    ///
    /// Red-verified: without the `RetireEnrollment` arm the fake records no
    /// revoke and the grant survives the stop.
    #[tokio::test]
    async fn a_sign_out_stop_retires_the_enrollment_and_keeps_the_named_row() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        handle.reconcile_now().await.expect("pass");
        let writer = enrolled_writer_hex(&handle).await;
        assert_eq!(
            fx.fake.state.lock().unwrap().grant_on.as_deref(),
            Some(named_row("a").as_str()),
            "the ceremony registered the grant on the named row"
        );

        let outcome = handle.shutdown_for_sign_out().await;
        assert_eq!(
            outcome,
            EnrollmentRetirement::Retired {
                cleared: true,
                sessions_revoked: 0
            },
            "the nest confirmed the retirement of a live grant"
        );
        {
            let s = fx.fake.state.lock().unwrap();
            assert_eq!(
                s.grant_revokes,
                vec![writer],
                "exactly one retirement, naming the writer key — the principal IS the key"
            );
            assert!(
                s.rows.contains(&named_row("a")),
                "the named row stays: a revoke clears the grant, never the row"
            );
            assert!(s.grant_on.is_none(), "no row carries the grant any more");
        }
        // The stop is the ordinary shutdown after the retirement.
        handle.closed().await;
    }

    /// **A sign-out landing INSIDE a pass still retires the enrollment.**
    /// Commands are served between passes, and a fresh sign-in's prologue is
    /// the longest pass there is — it recovers every generation key the
    /// account ever minted from escrow, which by the middle of a whole-suite
    /// sweep is 50–70 of them. A sign-out that queued its retirement behind
    /// that pass waited out the host's stop budget, the erase then took the
    /// writer key with its grant still live, and nothing could ever retire
    /// it: 41 of the 42 stranded rows the 2026-09-20 `--app linux` sweep
    /// left before the device cap were exactly this.
    ///
    /// The prologue here never ends (its first walk is held), so the only way
    /// the sign-out returns at all is by cutting the pass
    /// ([`SIGN_OUT_PASS_GRACE`]). Red-verified: without the preemption the
    /// retirement waits on the held walk for ever.
    #[tokio::test]
    async fn a_sign_out_landing_in_a_pass_that_will_not_end_still_retires_the_enrollment() {
        let fx = fixture();
        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        // The prologue registered the grant, then reached its walk and is
        // held there — the sign-out below lands inside it.
        fx.fake.list_stalled.notified().await;

        let outcome = tokio::time::timeout(EVENTUALLY_BUDGET, handle.shutdown_for_sign_out())
            .await
            .expect(
                "the sign-out's retirement waited on a pass that will not end — in a host \
                 that is a lapsed stop budget, an erase that takes the writer key, and a \
                 grant nobody can ever retire",
            );
        assert_eq!(
            outcome,
            EnrollmentRetirement::Retired {
                cleared: true,
                sessions_revoked: 0
            },
            "the retirement ran once the pass was cut, on the same key and session"
        );
        let s = fx.fake.state.lock().unwrap();
        assert_eq!(s.grant_revokes.len(), 1, "exactly one retirement");
        assert!(s.grant_on.is_none(), "no row carries the grant any more");
        assert!(
            s.rows.contains(&named_row("a")),
            "the named row stays: a revoke clears the grant, never the row"
        );
    }

    /// **A sign-out that cuts a pass mid-put still lands this device's own
    /// `Removed` row** (`account-data-taxonomy.md` § The generation machinery
    /// → *Fleet-scope reclamation*, clause (4)). The cut drops the pass where
    /// it stands, and when that is inside a fleet-scope put the nest already
    /// holds the row but the reply dies with the future: the own slot stays
    /// below it. The severance's ordered own publish then re-sends that row
    /// first, the nest refuses the replay `stale_writer_seq`, and — before
    /// the severance was the writer's last word — its `Removed` row queued
    /// behind the refusal and never left: the 2026-09-22 `--app linux`
    /// sweeps' `sign-out: the fleet-plane severance did not land`, one named
    /// device left un-severed on the fleet plane per such sign-out.
    ///
    /// The cut must be the retirement's own: a pass queued ahead of it walks
    /// the replay's self-echo first and heals the slot, which is why the
    /// prologue (held at its first walk to arm the hold) is the cut pass here
    /// and nothing else is queued. Red-verified: with `sever_self` on the
    /// ordinary ordered put, no coordinate above the held one is ever sent.
    #[tokio::test]
    async fn a_sign_out_cutting_a_fleet_put_mid_flight_still_severs_the_device() {
        let fx = fixture();
        let mut b_params = fx.params("b");
        b_params.enrollment_target_device_id = NAMED_ROW.to_string();
        let b = AccountStoreRuntime::start(b_params).await.expect("start b");
        b.reconcile_now().await.expect("b enrolls");
        b.reconcile_now().await.expect("b states its row");
        let fleet_writers = |fake: &FakeNest| -> BTreeSet<String> {
            let s = fake.state.lock().unwrap();
            s.feed
                .iter()
                .filter(|r| r.scope == ACCOUNT_STATE_FLEET_SCOPE)
                .map(|r| r.writer_id.clone())
                .collect()
        };
        let b_writers = fleet_writers(&fx.fake);

        // `a`'s prologue is held at its first walk, so the put hold below is
        // armed at a known point: after the fleet bootstrap (inside `start`,
        // not a pass — no sign-out cuts it), before the prologue's own
        // fleet-scope publish.
        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start a");
        fx.fake.list_stalled.notified().await;
        // `a`'s writer: the one its fleet bootstrap added.
        let a_writer = fleet_writers(&fx.fake)
            .difference(&b_writers)
            .next()
            .expect("a's bootstrap published on the fleet scope")
            .clone();
        // The prologue's next fleet-scope put: the nest records the row and
        // holds the reply — the sign-out below lands inside that put.
        *fx.fake.hold_next_put_reply_on.lock().unwrap() =
            Some((ACCOUNT_STATE_FLEET_SCOPE.to_string(), a_writer.clone()));
        fx.fake.release_list.notify_one();
        tokio::time::timeout(EVENTUALLY_BUDGET, fx.fake.put_held.notified())
            .await
            .expect("the prologue put a fleet-scope row");
        let held_seq = {
            let s = fx.fake.state.lock().unwrap();
            s.feed
                .iter()
                .rev()
                .find(|r| r.scope == ACCOUNT_STATE_FLEET_SCOPE && r.writer_id == a_writer)
                .expect("the held put is on the nest's feed")
                .writer_seq
        };

        let outcome = tokio::time::timeout(EVENTUALLY_BUDGET, a.shutdown_for_sign_out())
            .await
            .expect("the sign-out cut the held pass");
        assert!(
            matches!(outcome, EnrollmentRetirement::Retired { cleared: true, .. }),
            "the grant retirement ran after the severance: {outcome:?}"
        );
        // Read off the coordinates the nest recorded, not its live feed: a
        // removed writer's rows are anyone's to compact, and `b`'s own passes
        // may already have retired the `Removed` row's supersession chain.
        assert!(
            fx.fake
                .state
                .lock()
                .unwrap()
                .coordinates_seen
                .iter()
                .any(|(scope, writer, seq)| {
                    scope == ACCOUNT_STATE_FLEET_SCOPE && *writer == a_writer && *seq > held_seq
                }),
            "the severance put a row above the held coordinate — the refused replay did \
             not hold it back"
        );

        // A sibling's view: `a`'s cell is `Removed`, or already compacted
        // away (a removed writer's rows are anyone's to retire, and a retired
        // item is forgotten) — never the `Enrolled` row a lost severance
        // leaves standing.
        b.reconcile_now().await.expect("b walks a's severance");
        let rows = b.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        let still_enrolled = rows.iter().any(|e| {
            !e.tombstone
                && e.key == a_writer
                && matches!(
                    canonical_decode(&e.value).expect("decode the row"),
                    fauna_core::generation::DeviceSetRecord::Enrolled { .. }
                )
        });
        assert!(
            !still_enrolled,
            "b still reads a as Enrolled — the named device was left un-severed on the \
             fleet plane"
        );
    }

    /// **A local read answers inside a pass, never behind it** (charter § The
    /// client-side lifecycle, the pump bullet → *Commands and passes*). The
    /// 2026-09-22 `--app linux` sweep measured a fresh sign-in's prologue —
    /// a full catch-up on a fresh replica — past five minutes, and every
    /// preference surface read empty behind it because commands were served
    /// only between passes. The prologue here never ends (its first walk is
    /// held), so the read below can only answer by being served at that
    /// await point. Red-verified with every command pass-bound: the read
    /// waits on the held walk for ever.
    #[tokio::test]
    async fn a_preference_read_answers_while_the_prologue_is_held_on_the_network() {
        let fx = fixture();
        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        fx.fake.list_stalled.notified().await;
        let in_flight = handle.pump_cycles();
        assert!(in_flight.0 > in_flight.1, "the prologue is in flight");

        let read = tokio::time::timeout(EVENTUALLY_BUDGET, handle.get_preference(KIND_MODERATION))
            .await
            .expect("a local read waited on a pass that will not end")
            .expect("read");
        assert!(read.is_none(), "nothing local yet");
        assert_eq!(
            handle.pump_cycles(),
            in_flight,
            "the pass is still in flight — the read did not wait for it"
        );
        // The held pass ends only by a sign-out's cut (a plain shutdown is
        // pass-bound and would let it finish, which it never does).
        handle.shutdown_for_sign_out().await;
    }

    /// **A local write lands inside a pass and its network legs run after
    /// it** (the pump bullet's wake source (4)): `put_preference` answers
    /// with the row durable and stamped while the pass is held, sends
    /// nothing itself, and the publish step — the ordered own publish —
    /// runs as soon as the pass ends, before any command
    /// sent after it. Red-verified with every command pass-bound: the write
    /// waits on the held walk for ever.
    #[tokio::test]
    async fn a_preference_write_lands_while_a_pass_is_held_and_publishes_after_it() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        // The prologue behind us: the enrollment is registered, so the
        // publish below is accepted.
        handle.settled().await;
        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let held = tokio::spawn({
            let handle = handle.clone();
            async move { handle.reconcile_now().await }
        });
        fx.fake.list_stalled.notified().await;
        let puts_before = fx.fake.put_calls();
        let value = moderation_value(&["w"]);

        let stamp = tokio::time::timeout(
            EVENTUALLY_BUDGET,
            handle.put_preference(KIND_MODERATION, value.clone()),
        )
        .await
        .expect("a local write waited on a pass that will not end")
        .expect("put");
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the row is durable inside the pass");
        assert_eq!(entry.value, value);
        assert_eq!(
            fx.fake.put_calls(),
            puts_before,
            "nothing was sent: the network legs wait for the pass"
        );
        let (started, completed) = handle.pump_cycles();
        assert!(started > completed, "the pass is still in flight");

        fx.fake.release_list.notify_one();
        let report = held.await.expect("join").expect("the held pass ends");
        assert!(!report.cut_by_sign_out);
        // The publish step runs at the loop's top, ahead of anything sent
        // after the pass — this barrier included.
        handle.settled().await;
        assert!(
            fx.fake.put_calls() > puts_before,
            "the publish step sent the row once the pass had ended"
        );
        let published =
            fx.fake
                .state
                .lock()
                .unwrap()
                .coordinates_seen
                .iter()
                .any(|(scope, writer, _)| {
                    scope == ACCOUNT_STATE_SCOPE
                        && *writer == fauna_core::hex32::encode(&stamp.writer)
                });
        assert!(published, "the row is on the nest under this writer");
        handle.shutdown().await;
    }

    /// **`settled` is the pass barrier**: parked behind the pass in flight
    /// while a local read sent after it is served inside that pass, and
    /// answered only once the pass has ended.
    #[tokio::test]
    async fn settled_answers_only_once_the_pass_in_flight_has_ended() {
        let fx = fixture();
        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        fx.fake.list_stalled.notified().await;
        let settled = tokio::spawn({
            let handle = handle.clone();
            async move { handle.settled().await }
        });
        // Served inside the held prologue, at the stalled walk's await point;
        // the barrier sent before it is still parked.
        handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("a read inside the pass");
        assert!(
            !settled.is_finished(),
            "the barrier waits for the pass in flight"
        );

        fx.fake.release_list.notify_one();
        settled.await.expect("join");
        let (started, completed) = handle.pump_cycles();
        assert_eq!(
            started, completed,
            "no pass is in flight once settled answers"
        );
        assert!(completed >= 1, "the prologue counted");
        handle.shutdown().await;
    }

    /// A full pass on `handle`, held at its walk's first page until
    /// [`release_the_pass`] — the stand-in for a fresh sign-in's minutes-long
    /// catch-up. Called between passes (after the prologue), so every command
    /// sent while it is held is served inside it or parked behind it.
    async fn hold_a_pass(
        fx: &Fixture,
        handle: &AccountStoreHandle,
    ) -> tokio::task::JoinHandle<Result<PumpReport>> {
        handle.settled().await;
        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let held = tokio::spawn({
            let handle = handle.clone();
            async move { handle.reconcile_now().await }
        });
        fx.fake.list_stalled.notified().await;
        held
    }

    /// Let the pass [`hold_a_pass`] held run out, then wait past the publish
    /// step it owes — the step runs at the loop's top, ahead of the barrier.
    async fn release_the_pass(
        fx: &Fixture,
        handle: &AccountStoreHandle,
        held: tokio::task::JoinHandle<Result<PumpReport>>,
    ) {
        fx.fake.release_list.notify_one();
        let report = held.await.expect("join").expect("the held pass ends");
        assert!(!report.cut_by_sign_out);
        handle.settled().await;
    }

    /// A command's answer inside a held pass — a deadlock guard, never a
    /// timing assertion: a command parked behind the pass never answers.
    async fn inside_the_pass<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(EVENTUALLY_BUDGET, fut)
            .await
            .unwrap_or_else(|_| panic!("{what} waited on a pass that will not end"))
    }

    /// **A read-marker raise lands inside a pass and publishes after it**
    /// (`Cmd::is_local`, the read-marker verdict): the join reads and writes
    /// the stored marker with no yield between, so no walk page can merge
    /// between the two, and the publish is the publish step's. Red-verified
    /// with the raise pass-bound: it waits on the held walk for ever.
    #[tokio::test]
    async fn a_read_marker_raise_lands_while_a_pass_is_held_and_publishes_after_it() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        let channel = "cd".repeat(32);
        let held = hold_a_pass(&fx, &handle).await;
        let puts_before = fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE);

        assert!(
            inside_the_pass("the raise", handle.raise_read_marker(&channel, 5))
                .await
                .expect("raise")
        );
        assert_eq!(
            handle.read_markers().await.unwrap(),
            vec![(channel.clone(), 5)]
        );
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
            puts_before,
            "nothing was sent: the network legs wait for the pass"
        );

        release_the_pass(&fx, &handle, held).await;
        assert!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE) > puts_before,
            "the publish step sent the marker once the pass had ended"
        );
        handle.shutdown().await;
    }

    /// **An observation lands inside a pass and publishes after it**
    /// (`Cmd::is_local`, the observation verdict): the coordinate resolution
    /// reads what the walk has merged so far — a record the walk has not
    /// carried yet answers `Unresolved` exactly as it does between passes —
    /// and the seen-set read-modify-write runs with no yield between its read
    /// and its write. Red-verified with the observation pass-bound.
    #[tokio::test]
    async fn an_observation_lands_while_a_pass_is_held_and_publishes_after_it() {
        let fx = fixture();
        let channel = [0x78; 32];
        let scope = conv_scope(channel);
        let mut params = fx.params("a");
        params.memberships = Some(Memberships::knows(&[channel]).source());
        fx.fake.stage_record(&scope, 1, [0xD3; 32]);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle
            .reconcile_now()
            .await
            .expect("the walk carries the record");
        let held = hold_a_pass(&fx, &handle).await;
        let puts_before = fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE);

        let observation = Observation {
            scope: ContentScope::new(crate::scope_set::CONV_KIND, channel).expect("scope"),
            record: fauna_core::data::ContentHash::from_digest_dag_cbor([0xD3; 32]),
        };
        assert_eq!(
            inside_the_pass("the observation", handle.record_observation(observation))
                .await
                .expect("record"),
            ObservationOutcome::Recorded,
        );
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
            puts_before,
            "nothing was sent: the network legs wait for the pass"
        );

        release_the_pass(&fx, &handle, held).await;
        assert!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE) > puts_before,
            "the publish step sent the seen-set entry once the pass had ended"
        );
        handle.shutdown().await;
    }

    /// **The devices page's whole removal runs inside a pass, and its
    /// `Removed` row publishes after it** (`Cmd::is_local`, the fleet-removal
    /// verdict; clause (4), *The completion rule*): resolve, stage and the
    /// settle (which writes the `Removed` row) all answer while a pass is held — the
    /// reconcile they share the slot with has no yield point of its own, so
    /// it reads the slot before or after a command, never across one — and
    /// the fleet row waits for the publish step, which now publishes the
    /// fleet plane too. Red-verified with the quartet pass-bound: the resolve
    /// waits on the held walk for ever.
    #[tokio::test]
    async fn a_device_removal_runs_whole_while_a_pass_is_held_and_publishes_after_it() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;
        let held = hold_a_pass(&fx, &a).await;
        let fleet_puts_before = fx.fake.put_calls_for(ACCOUNT_STATE_FLEET_SCOPE);

        let targets = inside_the_pass(
            "the resolve",
            a.resolve_fleet_removal(NAMED_ROW, Some(sibling)),
        )
        .await
        .expect("resolve");
        assert_eq!(targets, vec![sibling]);
        let removal = PendingFleetRemoval {
            row: NAMED_ROW.to_string(),
            targets,
        };
        inside_the_pass("the stage", a.stage_fleet_removal(removal.clone()))
            .await
            .expect("stage");
        fx.fake.revoke_device(NAMED_ROW);
        inside_the_pass(
            "the settle",
            a.settle_fleet_removal(removal, NestDeletion::Gone),
        )
        .await
        .expect("settle");
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(
            removed_by_at(&rows, &sibling),
            Some(me),
            "durable inside the pass"
        );
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_FLEET_SCOPE),
            fleet_puts_before,
            "nothing was sent: the network legs wait for the pass"
        );

        release_the_pass(&fx, &a, held).await;
        assert!(
            fx.fake.put_calls_for(ACCOUNT_STATE_FLEET_SCOPE) > fleet_puts_before,
            "the publish step sent the Removed row once the pass had ended"
        );
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals, None, "nothing is left staged");

        a.shutdown().await;
        b.shutdown().await;
    }

    /// **The endpoint facts and a ceremony's group adoption answer inside a
    /// pass** (`Cmd::is_local`, their verdicts): the pass reads a snapshot of
    /// the facts taken at its start, so a set during it lands for the next
    /// one; the adoption is store transactions on a group scope no pass
    /// walks. Red-verified with both pass-bound.
    #[tokio::test]
    async fn endpoint_facts_and_a_group_adoption_answer_while_a_pass_is_held() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        let held = hold_a_pass(&fx, &handle).await;

        inside_the_pass(
            "the facts",
            handle.set_endpoint_facts(EndpointFacts {
                relay_url: Some("https://relay.example/".into()),
                ..Default::default()
            }),
        )
        .await
        .expect("set facts");
        let root = fauna_core::group_generation::GroupHeldRootRecord {
            scope_id: [0x3A; 32],
            root: vec![0x3B; 32].into(),
            held_since_ms: 1,
        };
        let report = inside_the_pass("the adoption", handle.adopt_group_rows(root, Vec::new()))
            .await
            .expect("adopt");
        assert_eq!(report, crate::group_state_plane::AdoptReport::default());

        release_the_pass(&fx, &handle, held).await;
        handle.shutdown().await;
    }

    /// **The share pump's ledger persist answers inside an account pass**
    /// (`Cmd::is_local`, the ledger verdict): one meta-table write, whose
    /// load-at-start, persist-at-end cycle is the share pump's own — no
    /// account pass holds the ledger. Red-verified with the write pass-bound.
    #[cfg(feature = "p2p-share")]
    #[tokio::test]
    async fn the_share_transfer_ledger_persists_while_a_pass_is_held() {
        use crate::share_pump::ShareStoreDoors as _;
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        let held = hold_a_pass(&fx, &handle).await;

        let mut ledger = crate::share_pump::TransferUsageLedger::default();
        ledger.seen_peers.insert("ab".repeat(32), 3);
        inside_the_pass(
            "the ledger persist",
            handle.put_share_transfer_ledger(ledger.clone()),
        )
        .await
        .expect("persist");
        assert_eq!(
            handle.share_transfer_ledger().await.expect("read"),
            Some(ledger)
        );

        release_the_pass(&fx, &handle, held).await;
        handle.shutdown().await;
    }

    /// **A tip-sealed door put that would mint waits for the pass instead of
    /// minting inside it** (`Cmd::is_local`, the door-put verdict): the local
    /// half of a tip-sealed put is store work only while an admissible tip
    /// resolves; with none, its door runs the first-need mint — an escrow
    /// deposit and a publish, network the pass must not be suspended behind
    /// and a lock a pass may hold. This fixture trusts no escrow holder, so
    /// no tip ever resolves: all five puts park while the pass is held — a
    /// read sent after them answers first — and each answers the door's
    /// no-tip refusal once the pass has ended. Red-verified by serving them
    /// without the tip check: each answers inside the held pass.
    #[tokio::test]
    async fn a_tip_sealed_put_that_would_mint_waits_for_the_pass_instead() {
        use futures_util::FutureExt as _;
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        let held = hold_a_pass(&fx, &handle).await;

        let mut puts: Vec<
            std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64>> + Send>>,
        > = vec![
            Box::pin({
                let h = handle.clone();
                async move { h.put_custodian_endpoints(Default::default()).await }
            }),
            Box::pin({
                let h = handle.clone();
                async move { h.put_custodies_held(Default::default()).await }
            }),
            Box::pin({
                let h = handle.clone();
                async move { h.put_share_endpoints(Default::default()).await }
            }),
            Box::pin({
                let h = handle.clone();
                async move {
                    h.put_group_held_root(fauna_core::group_generation::GroupHeldRootRecord {
                        scope_id: [0x3C; 32],
                        root: vec![0x3D; 32].into(),
                        held_since_ms: 1,
                    })
                    .await
                }
            }),
            Box::pin({
                let h = handle.clone();
                async move {
                    h.put_group_reception_key(
                        fauna_core::group_generation::GroupReceptionKeyRecord::mint(1),
                    )
                    .await
                }
            }),
        ];
        // One poll each: the command is on the channel, the answer awaited.
        for put in &mut puts {
            assert!(
                put.as_mut().now_or_never().is_none(),
                "sent, not yet answered"
            );
        }
        // Served in arrival order on the store thread: when this read has
        // answered, every put ahead of it was answered or parked.
        inside_the_pass("the read", handle.get_preference(KIND_MODERATION))
            .await
            .expect("read");
        for (i, put) in puts.iter_mut().enumerate() {
            assert!(
                put.as_mut().now_or_never().is_none(),
                "put {i} answered inside the pass — it would have minted there"
            );
        }

        fx.fake.release_list.notify_one();
        held.await.expect("join").expect("the held pass ends");
        for (i, put) in puts.into_iter().enumerate() {
            put.await
                .expect_err(&format!("put {i}: no tip resolves and none can mint here"));
        }
        handle.shutdown().await;
    }

    /// **A tip-sealed door put answers inside a held pass once a generation
    /// tip resolves, and publishes after it** (`Cmd::is_local`, the door-put
    /// verdict — the other half of the test above): with a trusted holder
    /// behind the nest, a put between passes runs the first-need mint, and
    /// from then on the door admits the kind under the resolved tip, so a
    /// put sent while a pass is held is store work only — it answers inside
    /// the pass with nothing sent, and the publish step carries its row to
    /// the nest once the pass has ended. Red-verified by parking every
    /// tip-sealed put while a pass is in flight: the put waits on the held
    /// walk for ever.
    #[tokio::test]
    async fn a_tip_sealed_put_lands_while_a_pass_is_held_once_a_tip_resolves() {
        use fauna_core::group_generation::GroupReceptionKeyRecord;
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params_trusting_the_holder("a"))
            .await
            .expect("runtime");
        // Between passes: the first put's door mints generation 0, and the
        // tip resolves from here on.
        handle
            .put_group_reception_key(GroupReceptionKeyRecord::mint(1))
            .await
            .expect("the first put mints through the trusted holder");

        let held = hold_a_pass(&fx, &handle).await;
        let fleet_puts_before = fx.fake.put_calls_for(ACCOUNT_STATE_FLEET_SCOPE);
        let second = GroupReceptionKeyRecord::mint(2);
        inside_the_pass(
            "the tip-sealed put",
            handle.put_group_reception_key(second.clone()),
        )
        .await
        .expect("the put answers under the resolved tip");
        assert!(
            handle
                .group_reception_keys()
                .await
                .expect("read")
                .contains(&second),
            "durable inside the pass"
        );
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_FLEET_SCOPE),
            fleet_puts_before,
            "nothing was sent: the network legs wait for the pass"
        );

        release_the_pass(&fx, &handle, held).await;
        assert!(
            fx.fake.put_calls_for(ACCOUNT_STATE_FLEET_SCOPE) > fleet_puts_before,
            "the publish step sent the reception key's row once the pass had ended"
        );
        handle.shutdown().await;
    }

    /// This device's fleet id — the principal IS the writer key (T10).
    async fn fleet_id_of(handle: &AccountStoreHandle) -> [u8; 32] {
        handle
            .principal_bundle_status()
            .await
            .expect("status")
            .device_authorization
            .expect("the prologue minted the grant")
            .device_key
    }

    /// Two enrolled devices, `a` having merged `b`'s rows: `b` passes first so
    /// its enrollment and its row statement are published, then `a` walks.
    async fn two_enrolled_devices(fx: &Fixture) -> (AccountStoreHandle, AccountStoreHandle) {
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start a");
        let mut b_params = fx.params("b");
        b_params.enrollment_target_device_id = NAMED_ROW.to_string();
        let b = AccountStoreRuntime::start(b_params).await.expect("start b");
        // Twice: the pass that first registers the grant sets the latch, and
        // the statement rides the next device-endpoints publish.
        b.reconcile_now().await.expect("b enrolls");
        b.reconcile_now().await.expect("b states its row");
        a.reconcile_now().await.expect("a merges b");
        (a, b)
    }

    fn removed_by_at(rows: &[StateEntry], id: &[u8; 32]) -> Option<[u8; 32]> {
        let row = rows
            .iter()
            .find(|e| !e.tombstone && e.key == fauna_core::hex32::encode(id))?;
        match canonical_decode(&row.value).expect("decode the row") {
            fauna_core::generation::DeviceSetRecord::Removed { removed_by, .. } => Some(removed_by),
            fauna_core::generation::DeviceSetRecord::Enrolled { .. } => None,
        }
    }

    /// **The read-marker door is a monotone raise, and what it writes reaches
    /// a sibling on the next passes** (`conversation-read-state.md` § The
    /// read-marker record → *Who writes it*): a raise the stored marker covers
    /// writes nothing, nothing lowers it, and the keyed read serves the
    /// position a sibling's walk merged. The sibling's re-read is keyed off
    /// its change generation ([`AccountStoreHandle::changed_after`]) — the
    /// own-pump source of the store-change watch the conversations glue
    /// waits on — which the walk that merged the raise moves.
    #[tokio::test]
    async fn the_read_marker_door_raises_monotonically_and_reaches_a_sibling() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let channel = "ab".repeat(32);

        assert!(a.raise_read_marker(&channel, 5).await.expect("raise"));
        assert!(
            !a.raise_read_marker(&channel, 5).await.expect("raise"),
            "a raise the marker covers writes nothing"
        );
        assert!(
            !a.raise_read_marker(&channel, 3).await.expect("raise"),
            "nothing lowers a marker"
        );
        assert_eq!(a.read_markers().await.unwrap(), vec![(channel.clone(), 5)]);

        a.reconcile_now().await.expect("A publishes");
        let before = b.change_generation();
        let waiter = tokio::spawn({
            let b = b.clone();
            async move { b.changed_after(before).await }
        });
        b.reconcile_now().await.expect("B walks");
        assert!(
            waiter.await.expect("join") > before,
            "the walk that merged the raise moved the change generation"
        );
        assert_eq!(
            b.read_markers().await.unwrap(),
            vec![(channel.clone(), 5)],
            "B serves the position A wrote"
        );
        assert!(
            !b.raise_read_marker(&channel, 4).await.expect("raise"),
            "B's lower read is already covered by A's"
        );
        a.shutdown().await;
        b.shutdown().await;
    }

    /// **The honest writer behind the live removal doors refuses this device's
    /// own id and any id the fleet view does not verify, and a retried
    /// completion is a no-op** (`docs/goal/behavior/devices.md` § Removing a
    /// Device; `docs/goal/architecture/account-data-taxonomy.md` § The
    /// generation machinery → *Fleet-scope reclamation*, clause (4)).
    /// `Removed` is absorbing and excludes unconditionally at every reader, so
    /// `write_removed` — reached in production through `settle` (`Gone`),
    /// `complete_pending` and `remove_fleet_member` — refuses its own device
    /// and any non-member. Intents naming this device and a stranger
    /// (`stage_fleet_removal` stores targets unvalidated) are settled `Gone`
    /// and refused; one naming the real sibling lands, attributed to the
    /// settling device. (The member
    /// door's own refusals sit at `resolve_member_removal`, pinned by
    /// `the_member_door_lists_the_unaccounted_and_removes_by_key`, so it does
    /// not witness these arms.)
    ///
    /// Red-verified: drop `write_removed`'s own-device arm and the first
    /// settle succeeds, leaving a `Removed` row at this device's own cell;
    /// drop the membership arm and the stranger's settle succeeds, filing a
    /// row at its cell; drop the already-removed no-op and the retried settle
    /// errors.
    #[tokio::test]
    async fn the_honest_writer_refuses_itself_and_strangers_through_the_settle_door() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;
        let stranger = [7u8; 32];
        let removal = |targets: Vec<[u8; 32]>| PendingFleetRemoval {
            row: NAMED_ROW.to_string(),
            targets,
        };
        fx.fake.revoke_device(NAMED_ROW);

        // `settle` writes in order and stops at the first refusal, so each
        // refused target gets its own intent (a re-stage replaces by row).
        for (what, target) in [("this device", me), ("a stranger", stranger)] {
            let intent = removal(vec![target]);
            a.stage_fleet_removal(intent.clone()).await.expect("stage");
            a.settle_fleet_removal(intent, NestDeletion::Gone)
                .await
                .expect_err(&format!("never {what}"));
        }
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(
            removed_by_at(&rows, &me),
            None,
            "no Removed at our own cell — leaving is sign-out's path"
        );
        assert!(
            !rows
                .iter()
                .any(|e| e.key == fauna_core::hex32::encode(&stranger)),
            "no row at the stranger's cell — an id the fleet view does not verify"
        );

        let intent = removal(vec![sibling]);
        a.stage_fleet_removal(intent.clone()).await.expect("stage");
        a.settle_fleet_removal(intent.clone(), NestDeletion::Gone)
            .await
            .expect("the verified member is removed");
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(
            removed_by_at(&rows, &sibling),
            Some(me),
            "attributed to the settling device, not the target"
        );
        a.settle_fleet_removal(intent, NestDeletion::Gone)
            .await
            .expect("already removed — a retried gesture is idempotent");

        a.shutdown().await;
        b.shutdown().await;
    }

    /// **The member door lists exactly the members no roster row accounts for,
    /// and removes one by its key with nothing else consulted** (clause (4),
    /// *A disagreement is the user's to settle*). With no roster the sibling
    /// is listed, carrying its own asserted enrollment instant; a roster whose
    /// row claims it (a pre-binding member, as this fixture's are — no
    /// statement publishes here) accounts for it and the list empties; the
    /// removal refuses this device and a stranger, writes the sibling's
    /// `Removed` row attributed to the caller, and is idempotent — after which
    /// the sibling is neither listed nor a wrap target.
    ///
    /// Red-verified: list every member and the accounted-for arm fails; skip
    /// `resolve_member_removal` and the own-device refusal fails.
    #[tokio::test]
    async fn the_member_door_lists_the_unaccounted_and_removes_by_key() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;

        let view = a
            .unaccounted_fleet_members(Vec::new())
            .await
            .expect("read the member door");
        assert_eq!(
            view.me, me,
            "the page's own fingerprint is this device's fleet id"
        );
        assert_eq!(
            view.unaccounted
                .iter()
                .map(|m| m.device_id)
                .collect::<Vec<_>>(),
            vec![sibling],
            "with no roster row, the sibling is unaccounted for; this device is never listed"
        );
        assert!(
            view.unaccounted[0].enrolled_at_ms > 0,
            "the card carries the member's asserted enrollment instant"
        );
        let view = a
            .unaccounted_fleet_members(vec![(NAMED_ROW.to_string(), Some(sibling))])
            .await
            .expect("read the member door");
        assert!(
            view.unaccounted.is_empty(),
            "a row whose claim resolves to the sibling alone accounts for it"
        );

        assert_eq!(
            a.remove_fleet_member(me).await,
            Err(FleetRemovalRefusal::OwnDevice),
            "never this device — leaving is sign-out's path"
        );
        assert_eq!(
            a.remove_fleet_member([7u8; 32]).await,
            Err(FleetRemovalRefusal::NotAMember),
            "never an id the fleet view does not verify"
        );
        a.remove_fleet_member(sibling)
            .await
            .expect("the one leg: the Removed row");
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(
            removed_by_at(&rows, &sibling),
            Some(me),
            "attributed to the device that removed it"
        );
        a.remove_fleet_member(sibling)
            .await
            .expect("already removed — nothing to write, no refusal");
        let view = a
            .unaccounted_fleet_members(Vec::new())
            .await
            .expect("read the member door");
        assert!(
            view.unaccounted.is_empty(),
            "a removed member is no wrap target, so it is off the list"
        );

        a.shutdown().await;
        b.shutdown().await;
    }

    /// **The resolve door answers from this replica's own fleet view, never
    /// the nest's claim alone** (clause (4), *The removal target*): it refuses
    /// this device — by claimed principal or by its own enrolled row — and any
    /// id the view does not verify, and accepts a verified sibling that has
    /// stated no row (this fixture trusts no escrow holder, so no generation
    /// mints and no device-endpoints entry publishes: every member here is the
    /// pre-binding shape). The statement's own journey — sealed, through a real
    /// nest, into a sibling's facts — is pinned where generations run:
    /// `conformance_account_state_walk`'s
    /// `a_members_row_statement_reaches_a_siblings_removal_facts`.
    ///
    /// Red-verified: answer `Ok(claimed)` without consulting the facts and the
    /// three refusals all fail.
    #[tokio::test]
    async fn resolve_fleet_removal_refuses_this_device_and_strangers() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;
        let own_row = a
            .enrolled_device_row()
            .await
            .expect("read")
            .expect("a enrolled");

        assert_eq!(
            a.resolve_fleet_removal(NAMED_ROW, Some(sibling)).await,
            Ok(vec![sibling]),
            "a verified member that has stated no row"
        );
        assert_eq!(
            a.resolve_fleet_removal(NAMED_ROW, Some(me)).await,
            Err(FleetRemovalRefusal::OwnDevice),
            "the nest aims the user's removal at the caller"
        );
        assert_eq!(
            a.resolve_fleet_removal(NAMED_ROW, Some([7u8; 32])).await,
            Err(FleetRemovalRefusal::NotAMember)
        );
        assert_eq!(
            a.resolve_fleet_removal(own_row, Some(sibling)).await,
            Err(FleetRemovalRefusal::OwnDevice),
            "this device's own enrolled row, whatever principal it carries"
        );
        assert_eq!(a.resolve_fleet_removal(NAMED_ROW, None).await, Ok(vec![]));

        a.shutdown().await;
        b.shutdown().await;
    }

    /// **A removal the page could not finish is finished by the runtime, with
    /// no user gesture** (clause (4), *The completion rule*; the review
    /// probe's scenario, green). The intent is staged before the nest
    /// deletion; then everything that can go wrong between the two legs does —
    /// the page never settles (a crash, a runtime that was down, a failed
    /// write all look the same from here). The nest deleted the row. The next
    /// full pass reads the roster, finds the row gone, journals `Removed` for
    /// the staged id and clears the intent.
    ///
    /// Red-verified: drop the pump's reconcile step and `b` is never removed.
    #[tokio::test]
    async fn a_staged_removal_the_page_never_settled_completes_on_the_next_pass() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;

        a.stage_fleet_removal(PendingFleetRemoval {
            row: NAMED_ROW.to_string(),
            targets: vec![sibling],
        })
        .await
        .expect("stage before the nest deletion");
        // The nest's half happened; the page's settle never did.
        fx.fake.revoke_device(NAMED_ROW);

        // Offline first: the roster cannot be read, so the intent WAITS — it
        // is neither completed on a guess nor dropped.
        fx.fake.roster_unreachable.store(true, Ordering::SeqCst);
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(
            report.fleet_removals.map(|p| (p.completed, p.waiting)),
            Some((0, 1))
        );
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(removed_by_at(&rows, &sibling), None);

        fx.fake.roster_unreachable.store(false, Ordering::SeqCst);
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals.map(|p| p.completed), Some(1));
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(
            removed_by_at(&rows, &sibling),
            Some(me),
            "the Removed row landed with no user gesture"
        );
        // Finished means finished: nothing staged, so the next pass reads no roster.
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals, None);

        a.shutdown().await;
        b.shutdown().await;
    }

    /// **A deletion the nest definitively refused writes no `Removed` row** —
    /// by the page's own settle (`Kept`), and equally by the reconcile when
    /// the page never settled and the roster still holds the row. `Removed` is
    /// absorbing: a device the user was told could not be removed must not be
    /// excluded from the fleet anyway. The reconcile's half only drops the
    /// intent once the row has stayed on the roster a whole in-flight bound
    /// after a pass first sighted it there; inside the bound it records the
    /// sighting and waits, since the deletion may still land.
    ///
    /// Red-verified: make `Kept` write like `Gone` and the page's half fails;
    /// drop the intent at the first sighting and the reconcile's wait fails.
    #[tokio::test]
    async fn a_refused_nest_deletion_clears_the_intent_and_writes_nothing() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let sibling = fleet_id_of(&b).await;
        let removal = PendingFleetRemoval {
            row: NAMED_ROW.to_string(),
            targets: vec![sibling],
        };

        // The page's own settle.
        a.stage_fleet_removal(removal.clone()).await.expect("stage");
        a.settle_fleet_removal(removal.clone(), NestDeletion::Kept)
            .await
            .expect("settle");
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals, None, "the intent is cleared");

        // The reconcile's: staged, never settled, and the roster holds the row.
        // Inside the bound that is a sighting and a wait.
        a.stage_fleet_removal(removal.clone()).await.expect("stage");
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(
            report.fleet_removals.map(|p| (p.abandoned, p.waiting)),
            Some((0, 1))
        );
        let neighbour = slot_neighbour(&fx);
        let slot = neighbour
            .get(&pending_removals_attr(&fx))
            .expect("still staged");
        let sighted = fauna_core::fleet_removal::decode_pending(&slot);
        assert!(
            matches!(sighted.as_slice(), [s] if s.present_since_ms.is_some()),
            "the pass recorded its sighting: {sighted:?}"
        );
        // The row outlasts the bound since that sighting: no deletion is in
        // flight, and none happened.
        let long_ago = fauna_core::data::Timestamp::now_millis_or_zero()
            - fauna_core::fleet_removal::DELETION_IN_FLIGHT_BOUND_MS;
        neighbour.set(
            &pending_removals_attr(&fx),
            &fauna_core::fleet_removal::encode_pending(&[
                fauna_core::fleet_removal::StagedFleetRemoval {
                    removal,
                    present_since_ms: Some(long_ago),
                },
            ]),
        );
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals.map(|p| p.abandoned), Some(1));
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals, None, "the intent is dropped");

        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(removed_by_at(&rows, &sibling), None, "b is still a member");

        a.shutdown().await;
        b.shutdown().await;
    }

    /// The ordinary path: the page settles `Gone` right after the nest
    /// deletion, the row lands at once, and nothing is left staged.
    #[tokio::test]
    async fn a_settled_removal_lands_at_once_and_leaves_nothing_staged() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;
        let removal = PendingFleetRemoval {
            row: NAMED_ROW.to_string(),
            targets: vec![sibling],
        };

        a.stage_fleet_removal(removal.clone()).await.expect("stage");
        fx.fake.revoke_device(NAMED_ROW);
        a.settle_fleet_removal(removal, NestDeletion::Gone)
            .await
            .expect("settle");

        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(removed_by_at(&rows, &sibling), Some(me));
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals, None);

        a.shutdown().await;
        b.shutdown().await;
    }

    /// The credential store of another process sharing device `a`'s slot —
    /// on a desktop the co-located agent, whose pump passes on its own clock
    /// (reading the slot on its own schedule).
    fn slot_neighbour(fx: &Fixture) -> CredentialStore {
        CredentialStore::with_file_backend(CRED_NAMESPACE, fx.cred_dir.join("a"))
    }

    fn pending_removals_attr(fx: &Fixture) -> String {
        format!("{}/pending-fleet-removals", fx.actor_hex)
    }

    /// **A pass that runs while the deletion is in flight does not drop the
    /// intent** (clause (4), *The completion rule*). The page staged, its `fauna.sync.devices.delete` is still
    /// on the wire, and a pass reads a roster that still holds the row — then
    /// the deletion lands and the page never settles. The pass must not read
    /// "row present" as "never happened": the removal completes on the pass
    /// after the deletion, with no user gesture.
    ///
    /// Red-verified: restore the drop-on-a-present-row reconcile and the
    /// first pass abandons the intent, the second finds nothing staged, and
    /// `b` stays a member.
    #[tokio::test]
    async fn a_pass_during_the_deletions_flight_keeps_the_removal_it_then_completes() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;

        a.stage_fleet_removal(PendingFleetRemoval {
            row: NAMED_ROW.to_string(),
            targets: vec![sibling],
        })
        .await
        .expect("stage before the nest deletion");
        // The delete is in flight: the nest still lists the row.
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(
            report.fleet_removals,
            Some(crate::fleet_removal::FleetRemovalPass {
                completed: 0,
                abandoned: 0,
                waiting: 1,
            }),
            "a row still present inside the in-flight bound is a wait, never a drop"
        );
        // The deletion lands; the page never settles (killed, runtime gone,
        // or its reply lost).
        fx.fake.revoke_device(NAMED_ROW);

        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals.map(|p| p.completed), Some(1));
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(
            removed_by_at(&rows, &sibling),
            Some(me),
            "the removal the user asked for lands once its deletion does"
        );

        a.shutdown().await;
        b.shutdown().await;
    }

    /// **`settle(Unknown)` leaves the intent staged** — a transport failure
    /// after the nest acted (the reply lost) is the case the staged copy
    /// exists for, so the next pass finds the row gone and finishes it.
    ///
    /// Red-verified: let `Unknown` fall through to the
    /// clear and nothing is staged for the pass to finish.
    #[tokio::test]
    async fn an_unknown_deletion_outcome_leaves_the_removal_for_the_reconcile() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;
        let removal = PendingFleetRemoval {
            row: NAMED_ROW.to_string(),
            targets: vec![sibling],
        };

        a.stage_fleet_removal(removal.clone()).await.expect("stage");
        // The nest deleted the row; its reply never reached the page.
        fx.fake.revoke_device(NAMED_ROW);
        a.settle_fleet_removal(removal, NestDeletion::Unknown)
            .await
            .expect("an unknown outcome settles nothing, and says so without error");
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(removed_by_at(&rows, &sibling), None, "nothing written yet");

        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(
            report.fleet_removals.map(|p| p.completed),
            Some(1),
            "the intent was still staged, so the pass finished it"
        );
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(removed_by_at(&rows, &sibling), Some(me));

        a.shutdown().await;
        b.shutdown().await;
    }

    /// **`settle(Gone)` finishes a removal whose staged copy a racing pass
    /// already dropped** — by re-staging before it writes, so a write that
    /// fails part-way still leaves the reconcile something to finish. The
    /// drop is made by a process sharing the slot (a pass whose in-flight
    /// bound ran out under a deletion slower than it).
    /// Two ids on the row (a re-minted principal), and the second write fails
    /// part-way: the `Removed` row is a local write (`fleet_removal` module
    /// docs), so the failure is the door's own — here the second id is a
    /// device this replica has not walked yet, so its fleet view does not
    /// verify it. Without the re-stage that would strand the second id for
    /// good; with it the next pass, having walked, finishes both.
    ///
    /// Red-verified: drop the re-stage and, the failed
    /// settle having cleared nothing that was there, the pass finds nothing
    /// staged.
    #[tokio::test]
    async fn a_gone_settle_re_stages_a_removal_a_racing_pass_dropped() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let me = fleet_id_of(&a).await;
        let sibling = fleet_id_of(&b).await;
        let c = AccountStoreRuntime::start(fx.params("c"))
            .await
            .expect("start c");
        c.reconcile_now().await.expect("c publishes its enrollment");
        let unwalked = fleet_id_of(&c).await;
        let removal = PendingFleetRemoval {
            row: NAMED_ROW.to_string(),
            targets: vec![sibling, unwalked],
        };

        a.stage_fleet_removal(removal.clone()).await.expect("stage");
        // The racing drop, from the process beside this one.
        slot_neighbour(&fx).delete(&pending_removals_attr(&fx));
        fx.fake.revoke_device(NAMED_ROW);
        a.settle_fleet_removal(removal, NestDeletion::Gone)
            .await
            .expect_err("the second id is not yet a member this replica verifies");

        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(
            report.fleet_removals.map(|p| p.completed),
            Some(1),
            "the settle re-staged the removal before writing, so the pass finished it"
        );
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert_eq!(removed_by_at(&rows, &sibling), Some(me));
        assert_eq!(removed_by_at(&rows, &unwalked), Some(me));
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.fleet_removals, None, "and nothing is left staged");

        a.shutdown().await;
        b.shutdown().await;
        c.shutdown().await;
    }

    /// The plain stop — an account switch, or the superseded assembly a
    /// sign-out landing mid-assembly shuts down — **keeps the machine
    /// enrolled**: the slot survives a switch, so the key it holds will be
    /// loaded again, and a tombstoned key would turn the switch back into a
    /// removed-from-account state and a successor mint.
    #[tokio::test]
    async fn a_plain_shutdown_unlike_a_sign_out_keeps_the_machine_enrolled() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        handle.reconcile_now().await.expect("pass");

        handle.shutdown().await;

        let s = fx.fake.state.lock().unwrap();
        assert!(
            s.grant_revokes.is_empty(),
            "a switch retires nothing: {:?}",
            s.grant_revokes
        );
        assert_eq!(
            s.grant_on.as_deref(),
            Some(named_row("a").as_str()),
            "the named row — and the credential it carries — survive a switch"
        );
    }

    /// **When the revoke cannot land (faked here as a coded refusal; any
    /// transport failure or budget overrun takes the same `Err` arm), the
    /// sign-out still completes.** The retirement
    /// defers (the grant may survive on the named row) and the
    /// store stops regardless: the user asked to be signed out.
    #[tokio::test]
    async fn a_sign_out_against_a_nest_without_the_kind_defers_and_still_stops() {
        let fx = fixture();
        fx.fake.no_revoke_kind.store(true, Ordering::SeqCst);
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        handle.reconcile_now().await.expect("pass");

        let outcome = handle.shutdown_for_sign_out().await;
        assert!(
            matches!(outcome, EnrollmentRetirement::Deferred(_)),
            "a failed revoke defers the retirement rather than failing the sign-out: {outcome:?}"
        );
        assert!(
            fx.fake.state.lock().unwrap().grant_on.is_some(),
            "the grant simply persists when the revoke fails"
        );
        handle.closed().await;
        fx.fake.no_revoke_kind.store(false, Ordering::SeqCst);
    }

    /// **The election role is published, and published before `start` returns.**
    ///
    /// The counters alone are not interpretable: a non-holder runs no pass, so
    /// frozen counters are correct there and a reader cannot tell that from
    /// "no runtime" or "no pass yet". This is the half that disambiguates them
    /// ([`PumpCycles`] owns the argument), and the ordering matters as much as
    /// the value — a caller that reads the role the instant `start` resolves
    /// must not see the `false` default still standing.
    #[tokio::test]
    async fn a_lone_runtime_publishes_the_holder_role_before_start_returns() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        assert!(
            handle.is_engine_holder(),
            "a runtime alone on its store dir won the election, and said so \
             before start() resolved — a false here is either a lost election \
             or the publish landing after the readiness barrier"
        );
        handle.shutdown().await;
    }

    /// **`shutdown()` CLOSES THE DATABASE, and the store dir is removable once
    /// it returns** — the property sign-out's erase rests on, and the one
    /// nothing asserted until a windows leak found it the expensive way.
    ///
    /// Every app's sign-out awaits this shutdown and then `remove_dir_all`s
    /// the actor's scope (`fauna_account_store::db::erase_actor_state`). On
    /// POSIX that sweep succeeds whether or not the database was ever closed —
    /// `unlink` removes an open file — so the erase's *real* precondition went
    /// unwitnessed on the only platforms it was ever run on. On Windows an
    /// open file cannot be deleted at all (`os error 32`), so the same sweep
    /// aborts on the first still-held child and leaves the signed-out user's
    /// account store on disk for the next sign-in to re-adopt
    /// (`account-scoping.md` § Erasure follows scope → the ⚠ *An OPEN store is
    /// an unerasable store, and only Windows says so* note).
    ///
    /// The load-bearing assertion is therefore the **sidecar** one, not the
    /// removal: SQLite deletes `-wal`/`-shm` when the last connection to a
    /// database closes cleanly, so their absence witnesses the close itself on
    /// every platform — where `remove_dir_all` succeeding proves nothing on
    /// POSIX. Their presence while the runtime is live is asserted first, so a
    /// store that never opened a WAL at all cannot make this pass vacuously.
    #[tokio::test]
    async fn shutdown_closes_the_db_and_leaves_the_store_dir_removable() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        // A real write, so the WAL exists and the close has something to do.
        handle
            .put_preference(KIND_MODERATION, moderation_value(&["x"]))
            .await
            .expect("write");
        let store_dir = StoreRoot::at(fx.base.join("a"))
            .store_dir(&fx.actor_hex)
            .expect("store dir");
        let db = store_dir.join(fauna_account_store::sqlite::ACCOUNT_STORE_DB_FILENAME);
        let wal = store_dir.join(format!(
            "{}-wal",
            fauna_account_store::sqlite::ACCOUNT_STORE_DB_FILENAME
        ));
        let shm = store_dir.join(format!(
            "{}-shm",
            fauna_account_store::sqlite::ACCOUNT_STORE_DB_FILENAME
        ));
        assert!(db.is_file(), "precondition: the db exists while live");
        assert!(
            wal.is_file(),
            "precondition: a live WAL-mode store has its -wal sidecar, so its \
             absence below is evidence of a close rather than of nothing having \
             happened (checked at {})",
            wal.display()
        );

        handle.shutdown().await;

        assert!(
            !wal.exists() && !shm.exists(),
            "shutdown() returned with the database still OPEN: SQLite removes \
             -wal/-shm only when the last connection closes cleanly, and these \
             survive it — {} (-wal present: {}, -shm present: {}). Sign-out \
             awaits exactly this call and then erases the scope, so a store \
             still open here is a signed-out user's data left on disk wherever \
             an open file cannot be deleted (windows, os error 32)",
            store_dir.display(),
            wal.exists(),
            shm.exists()
        );
        std::fs::remove_dir_all(&store_dir).unwrap_or_else(|e| {
            panic!(
                "the erase every app runs after shutdown() could not remove the \
                 store dir {}: {e}",
                store_dir.display()
            )
        });
    }

    /// **`closed()` is the shutdown, as an event.** It pends while the runtime
    /// runs and resolves once `shutdown()` has let the store thread exit — the
    /// signal a task serving this account (`share_glue::run`) selects on so it
    /// ends at the teardown rather than at its next cadence tick.
    #[tokio::test]
    async fn closed_resolves_at_shutdown_and_not_before() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        let watcher = handle.clone();
        // A live runtime: `closed()` must still be pending. Probed through a
        // zero-budget race rather than a sleep — the question is "is it
        // resolved now", not "does it resolve later".
        tokio::select! {
            biased;
            _ = watcher.closed() => panic!("closed() resolved on a live runtime"),
            _ = std::future::ready(()) => {}
        }
        handle.shutdown().await;
        watcher.closed().await;
    }

    /// **A second runtime over ONE store is a non-holder, and a poke moves
    /// nothing** — the property the cross-app e2e's non-holder branch rests on.
    ///
    /// This is the state that made an e2e written against the counters alone
    /// fail on two healthy apps (2026-08-18): on a real desktop the co-located
    /// sync agent is this second runtime, and its counters sit at `(0, 0)`
    /// forever by design. `flock` is per open file description, so two
    /// runtimes in ONE process reproduce it exactly — the same arbitration a
    /// second process gets.
    ///
    /// Both halves are load-bearing. The role tells a caller not to wait; the
    /// frozen counters are the proof it genuinely did not pump, which is what
    /// keeps two co-located processes from both driving one store.
    #[tokio::test]
    async fn a_second_runtime_over_one_store_is_a_non_holder_and_runs_no_pass() {
        let fx = fixture();
        let first = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("first runtime");
        first.reconcile_now().await.expect("pass");
        assert!(first.is_engine_holder(), "the first runtime took the lock");

        // Same params — same store dir, same credential slot: the shape a
        // co-located app + agent pair have on a real machine.
        let second = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("second runtime");
        assert!(
            !second.is_engine_holder(),
            "the lock is held; a second holder would mean two processes \
             pumping one store, which is what W5.1 exists to prevent"
        );

        let before = second.pump_cycles();
        let report = second
            .reconcile_now()
            .await
            .expect("role answer, not a pass");
        assert!(
            report.skipped_non_holder,
            "the in-band role answer, so a caller learns it without the state key"
        );
        assert_eq!(
            second.pump_cycles(),
            before,
            "a non-holder must run NO pass even when explicitly poked"
        );
        assert!(
            !second.is_engine_holder(),
            "still not the holder after the re-try"
        );

        second.shutdown().await;
        first.shutdown().await;
    }

    /// **The store principal's connect gate** (`transport-connection.md` § The
    /// dial budget) — the signal the principal's first
    /// `fauna.auth.device_handshake` waits on, so a first sign-in never mints
    /// into a `not_registered` refusal. Closed while the nest has not
    /// registered the grant (a device-cap refusal holds it off here), opened
    /// by the pass that lands the registration, opened for a co-located
    /// non-holder — which runs no pass — from the slot latch the holder's pass
    /// wrote, and open from assembly on every relaunch.
    #[tokio::test]
    async fn the_grant_registration_opens_the_principals_connect_gate() {
        let fx = fixture();
        fx.fake.at_device_cap.store(true, Ordering::SeqCst);
        let params = || fx.params("a");
        let holder = AccountStoreRuntime::start(params()).await.expect("runtime");
        let gate = holder.subscribe_grant_registered();
        holder.settled().await;
        assert!(
            !*gate.borrow(),
            "the prologue's refused register opened the gate — the principal would mint \
             into `not_registered`"
        );

        // The co-located process beside it: same store, same slot.
        let beside = AccountStoreRuntime::start(params())
            .await
            .expect("second runtime");
        assert!(!beside.is_engine_holder());
        let mut beside_gate = beside.subscribe_grant_registered();
        assert!(!*beside_gate.borrow());

        fx.fake.at_device_cap.store(false, Ordering::SeqCst);
        let report = holder.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Registered));
        assert!(
            *gate.borrow(),
            "the pass that put the grant on the nest opens the gate"
        );
        tokio::time::timeout(Duration::from_secs(10), beside_gate.wait_for(|open| *open))
            .await
            .expect("a non-holder's gate opens from the latch the holder wrote")
            .expect("the runtime is alive");

        beside.shutdown().await;
        holder.shutdown().await;
        let relaunch = AccountStoreRuntime::start(params())
            .await
            .expect("relaunch");
        assert!(
            *relaunch.subscribe_grant_registered().borrow(),
            "a relaunch starts open: its slot already records the registration"
        );
        relaunch.shutdown().await;
    }

    /// **A fresh machine enrolls straight onto its named row, and no row keyed
    /// by the writer key ever appears** (`sync-agent-credentials.md`
    /// § Credential model → the RULED 2026-09-28 block, decision 3: the
    /// target is the app's own id, unconditionally — on an agent-hosting host
    /// too). The row is register-created because the nest says it does not
    /// exist, and the grant lands on it: one row from the first pass.
    #[tokio::test]
    async fn a_fresh_enrollment_registers_the_named_row_and_never_a_writer_key_row() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");

        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));

        let (regs, grants) = {
            let s = fx.fake.state.lock().unwrap();
            (s.sync_registers.clone(), s.grant_registers.clone())
        };
        assert_eq!(
            regs,
            vec![named_row("a")],
            "the named row, register-created once"
        );
        assert_eq!(grants, vec![named_row("a")], "and it carries the grant");

        // The writer key names no row: the retired placeholder shape.
        let writer = enrolled_writer_hex(&handle).await;
        assert!(
            !regs.contains(&writer) && !grants.contains(&writer),
            "no hex(writer_pub) row was ever registered"
        );

        handle.shutdown().await;
    }

    /// **Row 176 — the This-device badge's read door reports the row this
    /// machine ENROLLED on**, read from the registration latch rather than
    /// computed from any key this machine holds. Held here by giving the
    /// runtime a target its own writer key could never produce, so a door that
    /// derived the row from the key would red.
    #[tokio::test]
    async fn the_enrolled_row_door_reports_the_latched_row_never_a_key_derived_one() {
        let fx = fixture();
        let mut params = fx.params("a");
        params.enrollment_target_device_id = NAMED_ROW.to_string();
        let handle = AccountStoreRuntime::start(params).await.expect("runtime");
        handle.reconcile_now().await.expect("pass");

        assert_eq!(
            handle.enrolled_device_row().await.expect("the read door"),
            Some(NAMED_ROW.to_string()),
            "the badge must mark the row the grant registered on"
        );

        // And it is genuinely the *enrolled* row, never one derived from the
        // writer key.
        let bundle = handle.principal_bundle_status().await.expect("status");
        let writer = fauna_core::hex32::encode(
            &bundle
                .device_authorization
                .expect("the assembly minted the grant")
                .device_key,
        );
        assert_ne!(
            handle.enrolled_device_row().await.expect("the read door"),
            Some(writer),
            "the door reports the latch's row, never a key-derived one"
        );

        handle.shutdown().await;
    }

    /// **Row 47 decision 4 — a removed machine says so, loudly, and does not
    /// latch.** The nest's revocation memory refuses the principal's key
    /// permanently, so the pass reports `RemovedFromAccount` rather than
    /// treating the refusal as an ordinary retryable error and rather than
    /// recording a registration that never happened. Latching here is what
    /// would turn a recoverable removal into the silent death decision 4 names.
    #[tokio::test]
    async fn a_revoked_principal_reports_removed_from_account_and_does_not_latch() {
        let fx = fixture();
        fx.fake.revoke_grants.store(true, Ordering::SeqCst);
        let mut params = fx.params("a");
        params.enrollment_target_device_id = NAMED_ROW.to_string();
        let handle = AccountStoreRuntime::start(params).await.expect("runtime");

        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(
            report.enrollment,
            Some(EnrollmentPass::RemovedFromAccount),
            "the tombstone is a removal, not a malformation to retry"
        );
        assert!(
            fx.fake.state.lock().unwrap().grant_registers.is_empty(),
            "nothing registered"
        );

        // Un-revoke: the pass must be free to succeed, which it could not be
        // had the refusal latched.
        fx.fake.revoke_grants.store(false, Ordering::SeqCst);
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Registered));

        handle.shutdown().await;
    }

    /// `remover` writes `removed`'s fleet `Removed` row and publishes it:
    /// the publish step sends the row, and the barrier behind it is what
    /// says it has. No pass of the remover's is needed for the removed
    /// device to be served the row.
    async fn remove_and_publish(remover: &AccountStoreHandle, removed: [u8; 32]) {
        remover
            .remove_fleet_member(removed)
            .await
            .expect("a sibling writes the Removed row");
        remover.settled().await;
    }

    /// **The third trigger's read — a machine's own `Removed` device-set row
    /// is read ahead of the registration latch** (`account-replica-posture.md`
    /// § The store device principal → *Principal succession after a device
    /// delete*, decision 1, the third trigger). The nest revokes nothing here
    /// and the latch still matches the named row, which is the whole case: a
    /// removal written on the fleet plane sends the nest nothing, so the
    /// latch went on answering `Current` and the removed machine never
    /// noticed. Held on the seedless host, where the answer is the end of it:
    /// loud, no register, no rotation.
    ///
    /// Red-verified: with the latch consulted first, the pass answers
    /// `Current`.
    #[tokio::test]
    async fn a_latched_machine_whose_own_row_reads_removed_answers_removed_with_no_rpc() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let removed = fleet_id_of(&b).await;
        b.shutdown().await;
        // The machine's seedless host: it loads the principal the sign-in
        // enrolled and can mint nothing.
        let agent = AccountStoreRuntime::start(AccountRuntimeParams {
            principal: RuntimePrincipal::Seedless,
            enrollment_target_device_id: NAMED_ROW.to_string(),
            ..fx.params("b")
        })
        .await
        .expect("the seedless host");
        let report = agent.reconcile_now().await.expect("pass");
        assert_eq!(
            report.enrollment,
            Some(EnrollmentPass::Current),
            "latched on its named row before the removal"
        );

        remove_and_publish(&a, removed).await;
        agent.reconcile_now().await.expect("the walk merges it");
        let registers = || {
            let s = fx.fake.state.lock().unwrap();
            (s.sync_registers.len(), s.grant_registers.len())
        };
        let before = registers();
        let report = agent.reconcile_now().await.expect("pass");
        assert_eq!(
            report.enrollment,
            Some(EnrollmentPass::RemovedFromAccount),
            "the own row is read ahead of the latch"
        );
        assert_eq!(registers(), before, "and nothing was asked of the nest");
        assert_eq!(
            fleet_id_of(&agent).await,
            removed,
            "a seedless host rotates nothing"
        );

        a.shutdown().await;
        agent.shutdown().await;
    }

    /// **The third trigger — a seed-holding machine whose own device-set row
    /// reads `Removed` mints its successor, with no answer from the nest**
    /// (decision 1, the third trigger). The nest here keeps the removed
    /// principal's grant, as it does for a guardian-marked row, a removal by
    /// key and a rebuilt box: the revoked answer the first trigger rotates on
    /// never comes. The pass answers `RemovedFromAccount`, the worker
    /// reassembles carrying the finding, the assembly rotates on it, and the
    /// successor enrolls on the machine's own named row as a new fleet device.
    ///
    /// Red-verified: with the probe deaf to the finding it re-registers the
    /// removed key, the next pass answers `RemovedFromAccount` again, and the
    /// worker reassembles on every pass without ever rotating — the case does
    /// not end. The rotation is what spends the cap.
    #[tokio::test]
    async fn a_seed_holder_whose_own_row_reads_removed_mints_a_successor_unasked() {
        let fx = fixture();
        let (a, b) = two_enrolled_devices(&fx).await;
        let removed = fleet_id_of(&b).await;
        remove_and_publish(&a, removed).await;

        let mut verdicts = Vec::new();
        for _ in 0..4 {
            verdicts.push(b.reconcile_now().await.expect("pass").enrollment);
        }
        let successor = fleet_id_of(&b).await;
        assert_ne!(
            successor, removed,
            "the machine holds a fresh principal: {verdicts:?}"
        );
        assert!(
            verdicts.contains(&Some(EnrollmentPass::RemovedFromAccount))
                && verdicts.last() == Some(&Some(EnrollmentPass::Current)),
            "removed, then healthy again under the successor: {verdicts:?}"
        );
        {
            let s = fx.fake.state.lock().unwrap();
            assert_eq!(
                (s.grant_on.as_deref(), s.granted_key.clone()),
                (Some(NAMED_ROW), Some(fauna_core::hex32::encode(&successor))),
                "the successor's grant is on the machine's own named row"
            );
        }
        a.reconcile_now().await.expect("a walks the successor");
        let rows = a.states_of_kind(KIND_DEVICE_SET).await.expect("read");
        assert!(
            rows.iter()
                .any(|e| !e.tombstone && e.key == fauna_core::hex32::encode(&successor))
                && removed_by_at(&rows, &successor).is_none()
                && removed_by_at(&rows, &removed).is_some(),
            "the sibling reads the successor enrolled and the predecessor still removed"
        );

        a.shutdown().await;
        b.shutdown().await;
    }

    /// **A sign-out is not a removal — its own `Removed` row never mints a
    /// successor** (decision 1, the third trigger → *A sign-out is not a
    /// removal*). A sign-out writes this machine's own `Removed` row, the
    /// very row the third trigger rotates on, and a second seed-holding
    /// runtime on the same store dir reads it before the erase that follows.
    /// Were it to rotate, the signed-out machine would be a fleet member
    /// again under a key the erase is about to destroy, with a live grant on
    /// its row that nothing could retire. The sign-out stamps the store
    /// before it writes the row, and every runtime on that store answers
    /// `SignedOut`: nothing registers, nothing rotates.
    ///
    /// Then the other half: a host start on the surviving store — the erase
    /// never landed and the user signs in again — clears the stamp and heals
    /// as any removed seed holder does.
    ///
    /// Red-verified: with the stamp unread, the second runtime answers
    /// `RemovedFromAccount`, reassembles and is `Current` again under a
    /// fresh principal.
    #[tokio::test]
    async fn a_sign_out_beside_a_second_seed_holder_is_not_undone() {
        let fx = fixture();
        let first = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the first app");
        first.reconcile_now().await.expect("it enrolls");
        first.reconcile_now().await.expect("and states its row");
        let second = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the second app, on the same store dir");
        let report = second.reconcile_now().await.expect("the second app");
        assert!(report.skipped_non_holder, "{report:?}");
        let principal = fleet_id_of(&first).await;

        let outcome = first.shutdown_for_sign_out().await;
        assert!(
            matches!(outcome, EnrollmentRetirement::Retired { cleared: true, .. }),
            "{outcome:?}"
        );
        let grant_registers = fx.fake.state.lock().unwrap().grant_registers.len();

        // The second app takes both roles and runs full passes over the
        // signed-out store.
        let mut verdicts = Vec::new();
        for _ in 0..4 {
            let report = second.reconcile_now().await.expect("the second app's pass");
            assert!(!report.skipped_non_holder, "{report:?}");
            verdicts.push(report.enrollment);
        }
        assert!(
            verdicts
                .iter()
                .all(|v| *v == Some(EnrollmentPass::SignedOut)),
            "every pass reads the sign-out, never a removal: {verdicts:?}"
        );
        assert_eq!(
            crate::principal_bundle::load_writer_key(&fx.params("a").credentials, &fx.actor_hex)
                .expect("the slot still holds a writer key")
                .verifying_key()
                .to_bytes(),
            principal,
            "no successor was minted into the slot"
        );
        {
            let s = fx.fake.state.lock().unwrap();
            assert!(
                s.grant_on.is_none() && s.grant_registers.len() == grant_registers,
                "and nothing was registered on the row the sign-out cleared: {:?}",
                s.grant_registers
            );
        }
        second.shutdown().await;

        // The erase never landed, and the user signs in again.
        let again = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the next sign-in");
        let mut verdicts = Vec::new();
        for _ in 0..4 {
            verdicts.push(again.reconcile_now().await.expect("pass").enrollment);
        }
        assert_ne!(
            fleet_id_of(&again).await,
            principal,
            "a sign-in outranks the sign-out before it: {verdicts:?}"
        );
        assert_eq!(
            verdicts.last(),
            Some(&Some(EnrollmentPass::Current)),
            "{verdicts:?}"
        );
        again.shutdown().await;
    }

    /// **The tier device cap is a standing, remediable refusal — reported,
    /// recorded for the Devices page, never latched, and cleared by the next
    /// accepted register.** `devices.md` § Step 4 asks the app to render it;
    /// before 2026-09-15 the pass folded it into `report.errors`, which no log
    /// printed and no page read, so a capped account's third device stayed
    /// silently unenrolled. The slot record is what an app whose co-located
    /// agent holds the pump reads — hence asserted through the handle, the
    /// page's own door, and re-read after the cap lifts.
    /// **A cap-refused pass ends at the enrollment step, so the cap's own
    /// remedy is served at once.** The refusal means the principal's grant is
    /// registered nowhere, so the data path it authenticates cannot come up
    /// and every later network leg would wait out its whole deadline. The
    /// pass-bound fleet-removal commands park behind that pass — and removing
    /// a device from the Devices page is the remedy the refusal names.
    /// Measured 2026-09-24 on a pump-holding windows seat: the removal's
    /// target resolve was still parked 30 s after the click, so the nest row
    /// was never deleted. Modelled here as a
    /// walk that never answers: a pass that reaches the network legs hangs,
    /// one that ends at the refusal does not.
    #[tokio::test]
    async fn a_device_cap_refusal_ends_the_pass_so_a_removal_is_served_at_once() {
        let fx = fixture();
        fx.fake.at_device_cap.store(true, Ordering::SeqCst);
        fx.fake.stall_next_list.store(true, Ordering::SeqCst);
        let mut params = fx.params("a");
        params.enrollment_target_device_id = NAMED_ROW.to_string();
        let handle = AccountStoreRuntime::start(params).await.expect("runtime");

        let budget = std::time::Duration::from_secs(10);
        let report = tokio::time::timeout(budget, handle.reconcile_now())
            .await
            .expect("a cap-refused pass must not reach the network legs")
            .expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::DeviceLimitExceeded));
        assert_eq!(
            fx.fake.state.lock().unwrap().list_calls,
            0,
            "no walk runs on a data path that cannot authenticate"
        );
        let targets =
            tokio::time::timeout(budget, handle.resolve_fleet_removal("the-only-slot", None))
                .await
                .expect("the removal's pass-bound resolve is served, not parked")
                .expect("a row naming no member resolves");
        assert!(
            targets.is_empty(),
            "a principal-less row is the nest deletion alone"
        );

        // The slot frees: the next pass registers and runs in full again.
        fx.fake.at_device_cap.store(false, Ordering::SeqCst);
        fx.fake.release_list.notify_one();
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Registered));
        assert!(
            fx.fake.state.lock().unwrap().list_calls > 0,
            "the walks resume"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn a_device_cap_refusal_is_recorded_for_the_page_and_clears_when_a_slot_frees() {
        let fx = fixture();
        fx.fake.at_device_cap.store(true, Ordering::SeqCst);
        let mut params = fx.params("a");
        params.enrollment_target_device_id = NAMED_ROW.to_string();
        let handle = AccountStoreRuntime::start(params).await.expect("runtime");

        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(
            report.enrollment,
            Some(EnrollmentPass::DeviceLimitExceeded),
            "the cap is its own verdict, not an anonymous step error"
        );
        assert!(
            fx.fake.state.lock().unwrap().grant_registers.is_empty(),
            "nothing registered past the cap"
        );
        assert_eq!(
            handle.enrolled_device_row().await.expect("read"),
            None,
            "a refused register must not latch a row that does not exist"
        );
        assert_eq!(
            handle.enrollment_refusal().await.expect("read"),
            Some(EnrollmentRefusal::DeviceLimitExceeded),
            "the refusal is recorded where the Devices page reads it"
        );
        assert_eq!(
            EnrollmentRefusal::DeviceLimitExceeded.notice(),
            fauna_protocol::RpcError::new(
                fauna_protocol::RpcError::CODE_SYNC_DEVICE_LIMIT_EXCEEDED,
                "error.sync.device_limit_exceeded",
            )
            .localized(),
            "the page's sentence and the wire refusal's rendering are one string"
        );

        // A slot frees (a device removed, or a bigger tier): the ordinary
        // next pass registers, and the standing notice comes down with it.
        fx.fake.at_device_cap.store(false, Ordering::SeqCst);
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Registered));
        assert_eq!(
            handle.enrolled_device_row().await.expect("read").as_deref(),
            Some(NAMED_ROW)
        );
        assert_eq!(handle.enrollment_refusal().await.expect("read"), None);

        handle.shutdown().await;
    }

    /// **W5.4b — the data path rides the principal's session; the ceremony
    /// rides the app's.** With `process_rpc` wired, every plane/walk/config
    /// leg lands on the process requester, and ONLY the two registration
    /// legs land on the app-session requester — the split that retires the
    /// W3-era "app's own session" nest leg. Two independent fakes make the
    /// split exact: a leg on the wrong requester is a hard count, not a
    /// timing question.
    #[tokio::test]
    async fn the_data_path_rides_the_process_rpc_and_the_ceremony_the_session() {
        let fx = fixture();
        let process_fake = FakeNest::default();
        let mut params = fx.params("a");
        params.process_rpc = Some(process_fake.clone());
        let handle = AccountStoreRuntime::start(params).await.expect("runtime");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));

        let (regs, grants, session_lists) = {
            let s = fx.fake.state.lock().unwrap();
            (
                s.sync_registers.clone(),
                s.grant_registers.clone(),
                s.list_calls,
            )
        };
        assert_eq!(regs.len(), 1, "the ceremony rode the app session: {regs:?}");
        assert_eq!(grants.len(), 1);
        assert_eq!(session_lists, 0, "a walk leg leaked onto the app session");
        let (process_lists, process_regs) = {
            let s = process_fake.state.lock().unwrap();
            (s.list_calls, s.sync_registers.clone())
        };
        assert!(process_lists > 0, "the walk rides the process requester");
        assert!(
            process_regs.is_empty(),
            "a ceremony leg leaked onto the process requester: {process_regs:?}"
        );

        handle.shutdown().await;
    }

    fn moderation_value(words: &[&str]) -> Vec<u8> {
        canonical_encode(&ModerationConfig {
            muted_keywords: words
                .iter()
                .map(|w| fauna_core::data::MutedKeyword::from(*w))
                .collect(),
            ..Default::default()
        })
        .expect("encode moderation")
    }

    /// A preference put lands locally, reads back, and publishes on the plane
    /// — and nothing is written anywhere else.
    #[tokio::test]
    async fn put_preference_lands_locally_reads_back_and_publishes() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        let value = moderation_value(&["spoilers"]);
        handle
            .put_preference(KIND_MODERATION, value.clone())
            .await
            .expect("put");

        // Local read serves the entry.
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("entry present");
        assert_eq!(entry.value, value);
        assert_eq!(entry.key, PREFERENCE_KEY);
        // Behind the pass barrier: the publish step the write armed has run.
        handle.settled().await;
        assert!(fx.fake.put_calls() >= 1, "the row was published");
        handle.shutdown().await;
    }

    // ── A row refused for room is parked (`account-replica-posture.md` § The
    // store device principal, refinement 11 → *A row refused for room is
    // parked*) — the delegable scope at its cap, over the fake's live-pair
    // cap. Each test reads what the 2026-10-01 measurement found on the real
    // handlers: a new item at a full scope, and the device's other writes
    // behind it.

    /// A channel id for these tests' read markers.
    fn channel(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    /// The pass's publish errors, if any — a parked row is none.
    fn publish_errors(report: &PumpReport) -> Vec<&String> {
        report
            .errors
            .iter()
            .filter(|e| e.contains("publish_pending"))
            .collect()
    }

    /// Device `a` with one marker and the moderation record published, the
    /// delegable scope then capped at the rows it holds.
    async fn device_at_a_full_scope(fx: &Fixture) -> AccountStoreHandle {
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        a.put_preference(KIND_MODERATION, moderation_value(&["one"]))
            .await
            .expect("put");
        assert!(a.raise_read_marker(&channel(1), 1).await.expect("raise"));
        a.settled().await;
        let report = a.reconcile_now().await.expect("pass");
        assert!(publish_errors(&report).is_empty(), "{:?}", report.errors);
        fx.fake.cap_at_live_rows(ACCOUNT_STATE_SCOPE);
        a
    }

    /// A fresh device's reading of the account: its markers and its
    /// moderation record.
    async fn fresh_device_reads(fx: &Fixture, device: &str) -> (Vec<(String, u64)>, Vec<u8>) {
        let fresh = AccountStoreRuntime::start(fx.params(device))
            .await
            .expect("fresh device");
        fresh.reconcile_now().await.expect("pass");
        let markers = fresh.read_markers().await.expect("markers");
        let moderation = fresh
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .map(|e| e.value)
            .unwrap_or_default();
        fresh.shutdown().await;
        (markers, moderation)
    }

    /// The 2026-10-01 measured flow, healed: at a full scope a new item's row
    /// parks and the publish goes on, so a rewrite of an item the nest
    /// already holds a row of lands in the same pass and a fresh device reads
    /// it; from then on a pass costs exactly one refused put. Red before the
    /// build: the rewrite stayed on the device behind the refused marker.
    #[tokio::test]
    async fn at_a_full_scope_a_new_item_parks_and_the_devices_other_writes_land() {
        let fx = fixture();
        let a = device_at_a_full_scope(&fx).await;

        assert!(a.raise_read_marker(&channel(2), 1).await.expect("raise"));
        a.put_preference(KIND_MODERATION, moderation_value(&["two"]))
            .await
            .expect("the rewrite is durable locally");
        a.settled().await;
        let report = a.reconcile_now().await.expect("pass");
        assert!(publish_errors(&report).is_empty(), "{:?}", report.errors);
        assert_eq!(report.parked, Some(1), "the new marker is parked");

        let (markers, moderation) = fresh_device_reads(&fx, "fresh").await;
        assert_eq!(
            moderation,
            moderation_value(&["two"]),
            "the rewrite reached the nest past the parked row"
        );
        assert_eq!(
            markers,
            vec![(channel(1), 1)],
            "the parked marker has no room"
        );

        for pass in 0..2 {
            let refused = fx.fake.scope_full_refusals(ACCOUNT_STATE_SCOPE);
            let report = a.reconcile_now().await.expect("pass");
            assert_eq!(
                fx.fake.scope_full_refusals(ACCOUNT_STATE_SCOPE),
                refused + 1,
                "pass {pass}: a full scope costs one refused put a pass"
            );
            assert_eq!(report.parked, Some(1));
            assert!(publish_errors(&report).is_empty(), "{:?}", report.errors);
        }
        a.shutdown().await;
    }

    /// Room appears (a retire elsewhere freed a pair): the next pass's retry
    /// sends the parked row with no further write of the device's, and the
    /// list empties.
    #[tokio::test]
    async fn a_parked_row_lands_by_itself_once_the_scope_has_room() {
        let fx = fixture();
        let a = device_at_a_full_scope(&fx).await;
        assert!(a.raise_read_marker(&channel(2), 7).await.expect("raise"));
        a.settled().await;
        assert_eq!(a.reconcile_now().await.expect("pass").parked, Some(1));

        fx.fake.widen_cap(ACCOUNT_STATE_SCOPE, 1);
        let refused = fx.fake.scope_full_refusals(ACCOUNT_STATE_SCOPE);
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.parked, Some(0), "{:?}", report.errors);
        assert_eq!(fx.fake.scope_full_refusals(ACCOUNT_STATE_SCOPE), refused);
        let (markers, _) = fresh_device_reads(&fx, "fresh").await;
        assert_eq!(markers, vec![(channel(1), 1), (channel(2), 7)]);
        a.shutdown().await;
    }

    /// The retry order: a preference record's parked row goes before the
    /// marker rows parked ahead of it, so the first room the scope gets is
    /// the user's setting.
    #[tokio::test]
    async fn a_parked_preference_row_is_retried_before_older_parked_rows() {
        let fx = fixture();
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        assert!(a.raise_read_marker(&channel(1), 1).await.expect("raise"));
        a.settled().await;
        a.reconcile_now().await.expect("pass");
        fx.fake.cap_at_live_rows(ACCOUNT_STATE_SCOPE);

        assert!(a.raise_read_marker(&channel(2), 1).await.expect("raise"));
        assert!(a.raise_read_marker(&channel(3), 1).await.expect("raise"));
        a.put_preference(KIND_MODERATION, moderation_value(&["first"]))
            .await
            .expect("put");
        a.settled().await;
        assert_eq!(a.reconcile_now().await.expect("pass").parked, Some(3));

        fx.fake.widen_cap(ACCOUNT_STATE_SCOPE, 1);
        assert_eq!(a.reconcile_now().await.expect("pass").parked, Some(2));
        let (markers, moderation) = fresh_device_reads(&fx, "fresh").await;
        assert_eq!(
            moderation,
            moderation_value(&["first"]),
            "the preference took the room"
        );
        assert_eq!(markers, vec![(channel(1), 1)], "the markers wait");
        a.shutdown().await;
    }

    /// A parked row the entry has since moved past leaves the list: the
    /// later row carries the value, and is the one owed.
    #[tokio::test]
    async fn a_superseded_parked_row_is_dropped_and_the_later_row_is_owed() {
        let fx = fixture();
        let a = device_at_a_full_scope(&fx).await;
        assert!(a.raise_read_marker(&channel(2), 1).await.expect("raise"));
        a.settled().await;
        assert_eq!(a.reconcile_now().await.expect("pass").parked, Some(1));

        assert!(a.raise_read_marker(&channel(2), 2).await.expect("raise"));
        a.settled().await;
        assert_eq!(
            a.reconcile_now().await.expect("pass").parked,
            Some(1),
            "the first row left the list; the later one is parked in its place"
        );

        fx.fake.widen_cap(ACCOUNT_STATE_SCOPE, 1);
        assert_eq!(a.reconcile_now().await.expect("pass").parked, Some(0));
        let (markers, _) = fresh_device_reads(&fx, "fresh").await;
        assert_eq!(markers, vec![(channel(1), 1), (channel(2), 2)]);
        a.shutdown().await;
    }

    /// The heal: a rotation re-journals the predecessor's parked row with
    /// its un-pushed tail, so the successor owes it — parked under its own
    /// writer while the scope stays full — and sends it once the scope has
    /// room. The scope stays full across the revival so the successor's diff
    /// cannot land the predecessor's relay copy first and hide the heal. Red
    /// before the build: the parked row sat at or below the predecessor's
    /// slot, outside the tail re-author's bound, and the successor owed
    /// nothing.
    #[tokio::test]
    async fn a_rotation_re_journals_a_parked_row_under_the_successor() {
        let fx = fixture();
        {
            let a = device_at_a_full_scope(&fx).await;
            assert!(a.raise_read_marker(&channel(2), 4).await.expect("raise"));
            a.settled().await;
            assert_eq!(a.reconcile_now().await.expect("pass").parked, Some(1));
            a.shutdown().await;
        }
        // The user deletes the machine while nothing runs on it; the next
        // sign-in mints a successor (the revival ceremony).
        fx.fake.revoke_device(&named_row("a"));
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("revival sign-in");
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(
            report.parked,
            Some(1),
            "the successor owes the re-journaled marker: {:?}",
            report.errors
        );

        fx.fake.widen_cap(ACCOUNT_STATE_SCOPE, 1);
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(report.parked, Some(0), "{:?}", report.errors);
        let (markers, _) = fresh_device_reads(&fx, "fresh").await;
        assert!(
            markers.contains(&(channel(2), 4)),
            "the parked marker rode the rotation: {markers:?}"
        );
        a.shutdown().await;
    }

    // ── A departed scope's rows leave the nest
    // (`delegable-scope-reclamation.md` § Delegable-scope reclamation, parts
    // (6) and (7)) — over the fake's live feed. Each device's membership
    // source is the test's to move; a channel's two items are its read marker
    // and its seen-set entry.

    /// Device `device` of `fx`'s account, its membership source `memberships`.
    async fn device_in(
        fx: &Fixture,
        device: &str,
        memberships: &Memberships,
    ) -> AccountStoreHandle {
        let mut params = fx.params(device);
        params.memberships = Some(memberships.source());
        AccountStoreRuntime::start(params).await.expect("runtime")
    }

    /// The channel `ch` with one record on its feed, for an observation to
    /// name ([`observed`]).
    fn stage_channel(fx: &Fixture, ch: u8) {
        fx.fake
            .stage_record(&conv_scope([ch; 32]), 1, [0xD0 ^ ch; 32]);
    }

    /// The observation of the record [`stage_channel`] staged — what writes
    /// the channel's seen-set entry.
    fn observed(ch: u8) -> Observation {
        Observation {
            scope: ContentScope::new(crate::scope_set::CONV_KIND, [ch; 32]).expect("scope"),
            record: fauna_core::data::ContentHash::from_digest_dag_cbor([0xD0 ^ ch; 32]),
        }
    }

    /// `rounds` passes of each device, in order.
    async fn rounds(devices: &[&AccountStoreHandle], rounds: usize) {
        for _ in 0..rounds {
            for d in devices {
                d.reconcile_now().await.expect("pass");
            }
        }
    }

    fn census(fx: &Fixture) -> std::collections::BTreeMap<String, usize> {
        fx.fake.live_rows_by_writer(ACCOUNT_STATE_SCOPE)
    }

    /// Device A leaves a channel: its own rows of the channel's two items are
    /// retired, B's row stands, and B writes the item back that only A's row
    /// carried. Over ten further rounds neither device puts or retires
    /// anything: they do not undo each other. Red-verified by letting A
    /// retire a member's row (B's marker went, and B wrote it back).
    #[tokio::test]
    async fn leaving_a_channel_retires_this_devices_rows_and_a_members_row_stands() {
        let fx = fixture();
        let ch = 0x31;
        stage_channel(&fx, ch);
        let a_in = Memberships::knows(&[[ch; 32]]);
        let b_in = Memberships::knows(&[[ch; 32]]);
        let a = device_in(&fx, "a", &a_in).await;
        let b = device_in(&fx, "b", &b_in).await;
        rounds(&[&a, &b], 2).await;
        assert_eq!(
            a.record_observation(observed(ch)).await.expect("observe"),
            ObservationOutcome::Recorded
        );
        assert!(a.raise_read_marker(&channel(ch), 3).await.expect("raise"));
        rounds(&[&a, &b], 2).await;
        assert!(b.raise_read_marker(&channel(ch), 9).await.expect("raise"));
        rounds(&[&b, &a], 3).await;
        let (a_w, b_w) = (enrolled_writer_hex(&a).await, enrolled_writer_hex(&b).await);
        assert_eq!(
            census(&fx),
            [(a_w.clone(), 1), (b_w.clone(), 1)].into(),
            "A's seen-set row and B's marker row, one row per item"
        );

        a_in.set(&[]);
        let report = a.reconcile_now().await.expect("pass");
        assert_eq!(
            report.cover_reclaim.as_ref().map(|c| c.departed),
            Some(1),
            "{report:?}"
        );
        assert_eq!(
            census(&fx),
            [(b_w.clone(), 1)].into(),
            "A's row is gone and the member's row stands"
        );
        b.reconcile_now().await.expect("pass");
        assert_eq!(
            census(&fx),
            [(b_w.clone(), 2)].into(),
            "B, still in the channel, wrote back the item only A's row carried"
        );

        let (puts, retires) = (
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
            fx.fake.retire_calls_for(ACCOUNT_STATE_SCOPE),
        );
        rounds(&[&a, &b], 10).await;
        assert_eq!(
            (
                fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
                fx.fake.retire_calls_for(ACCOUNT_STATE_SCOPE)
            ),
            (puts, retires),
            "no put and no retire: the two devices do not undo each other"
        );
        assert_eq!(census(&fx), [(b_w, 2)].into());
        assert!(
            a.read_markers()
                .await
                .expect("markers")
                .contains(&(channel(ch), 9)),
            "A keeps the entry"
        );

        // B leaves too and retires its rows. A still holds its relay copies
        // of them, which the nest no longer lists; its diff withholds them
        // as a departed scope's rows rather than push them back.
        b_in.set(&[]);
        b.reconcile_now().await.expect("pass");
        assert_eq!(census(&fx), Default::default(), "both devices left");
        let puts = fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE);
        let report = a.reconcile_now().await.expect("pass");
        assert!(
            report.publish_diff.is_some_and(|d| d.departed == 2),
            "{:?}",
            report.publish_diff
        );
        rounds(&[&a, &b], 3).await;
        assert_eq!(fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE), puts);
        assert_eq!(census(&fx), Default::default());
        a.shutdown().await;
        b.shutdown().await;
    }

    /// A device that never subscribed to a scope retires nothing of it and
    /// writes nothing of it, whatever its answer omits.
    #[tokio::test]
    async fn a_device_that_never_subscribed_to_a_scope_retires_nothing() {
        let fx = fixture();
        let ch = 0x32;
        let a_in = Memberships::knows(&[[ch; 32]]);
        let b_in = Memberships::knows(&[]);
        let a = device_in(&fx, "a", &a_in).await;
        let b = device_in(&fx, "b", &b_in).await;
        rounds(&[&a, &b], 2).await;
        assert!(a.raise_read_marker(&channel(ch), 4).await.expect("raise"));
        rounds(&[&a, &b], 4).await;
        let a_w = enrolled_writer_hex(&a).await;
        assert_eq!(census(&fx), [(a_w, 1)].into());
        let report = b.reconcile_now().await.expect("pass");
        assert_eq!(
            report.cover_reclaim.as_ref().map(|c| c.departed),
            Some(0),
            "{report:?}"
        );
        assert!(
            b.read_markers()
                .await
                .expect("markers")
                .contains(&(channel(ch), 4)),
            "B walked the marker in"
        );
        a.shutdown().await;
        b.shutdown().await;
    }

    /// A device with no membership answer hands over a removed device's
    /// preference record, and no marker: part (7) keeps a member scope's
    /// item to a device that is in the scope.
    #[tokio::test]
    async fn a_device_with_no_membership_answer_hands_over_a_preference_and_no_marker() {
        let fx = fixture();
        let ch = 0x33;
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        let r = AccountStoreRuntime::start(fx.params("r"))
            .await
            .expect("runtime");
        rounds(&[&a, &r], 2).await;
        r.put_preference(KIND_MODERATION, moderation_value(&["kept"]))
            .await
            .expect("put");
        assert!(r.raise_read_marker(&channel(ch), 2).await.expect("raise"));
        rounds(&[&r, &a], 3).await;
        let (a_w, r_w) = (enrolled_writer_hex(&a).await, enrolled_writer_hex(&r).await);
        assert_eq!(census(&fx), [(r_w.clone(), 2)].into());
        r.shutdown().await;
        let r_id: [u8; 32] = fauna_core::hex32::decode(&r_w).expect("writer id");
        a.remove_fleet_member(r_id).await.expect("A removes R");
        rounds(&[&a], 4).await;

        assert_eq!(
            census(&fx),
            [(a_w, 1), (r_w, 1)].into(),
            "A carries the record; the marker stays R's, unretired and not \
             handed over"
        );
        let (markers, moderation) = fresh_device_reads(&fx, "fresh").await;
        assert_eq!(moderation, moderation_value(&["kept"]));
        assert_eq!(markers, vec![(channel(ch), 2)]);
        a.shutdown().await;
    }

    /// Three devices: R, removed, wrote the only row of a channel's marker;
    /// A has left the channel and B is still in it. A retires R's row on the
    /// departed licence (R is no member), and B writes the marker back under
    /// its own id.
    #[tokio::test]
    async fn a_removed_devices_row_of_a_departed_scope_is_retired_by_the_device_that_left() {
        let fx = fixture();
        let ch = 0x34;
        let a_in = Memberships::knows(&[[ch; 32]]);
        let b_in = Memberships::knows(&[[ch; 32]]);
        let r_in = Memberships::knows(&[[ch; 32]]);
        let a = device_in(&fx, "a", &a_in).await;
        let b = device_in(&fx, "b", &b_in).await;
        let r = device_in(&fx, "r", &r_in).await;
        rounds(&[&a, &b, &r], 2).await;
        assert!(r.raise_read_marker(&channel(ch), 6).await.expect("raise"));
        rounds(&[&r, &a, &b], 3).await;
        let (b_w, r_w) = (enrolled_writer_hex(&b).await, enrolled_writer_hex(&r).await);
        assert_eq!(census(&fx), [(r_w.clone(), 1)].into());

        a_in.set(&[]);
        a.reconcile_now().await.expect("pass");
        assert_eq!(
            census(&fx),
            [(r_w.clone(), 1)].into(),
            "R is a member yet: its row stands"
        );
        r.shutdown().await;
        let r_id: [u8; 32] = fauna_core::hex32::decode(&r_w).expect("writer id");
        a.remove_fleet_member(r_id).await.expect("A removes R");
        let mut departed = 0;
        for _ in 0..3 {
            let report = a.reconcile_now().await.expect("pass");
            departed += report.cover_reclaim.map_or(0, |c| c.departed);
        }
        assert_eq!(departed, 1, "A retired R's row on the departed licence");
        assert_eq!(census(&fx), Default::default());
        rounds(&[&b], 2).await;
        assert_eq!(census(&fx), [(b_w, 1)].into(), "B wrote the marker back");
        let (markers, _) = fresh_device_reads(&fx, "fresh").await;
        assert_eq!(markers, vec![(channel(ch), 6)]);
        a.shutdown().await;
        b.shutdown().await;
    }

    /// A re-join writes the marker back at its old position: the device that
    /// left kept the entry, and its first pass in the channel again finds no
    /// row of it at the nest.
    #[tokio::test]
    async fn a_re_join_writes_the_marker_back_at_its_old_position() {
        let fx = fixture();
        let ch = 0x35;
        stage_channel(&fx, ch);
        let a_in = Memberships::knows(&[[ch; 32]]);
        let a = device_in(&fx, "a", &a_in).await;
        rounds(&[&a], 2).await;
        assert!(a.raise_read_marker(&channel(ch), 5).await.expect("raise"));
        assert_eq!(
            a.record_observation(observed(ch)).await.expect("observe"),
            ObservationOutcome::Recorded
        );
        rounds(&[&a], 2).await;
        let a_w = enrolled_writer_hex(&a).await;
        assert_eq!(census(&fx), [(a_w.clone(), 2)].into());

        a_in.set(&[]);
        rounds(&[&a], 2).await;
        assert_eq!(census(&fx), Default::default(), "both rows left the nest");
        // A late raise while out of the channel is durable locally and sends
        // nothing.
        let puts = fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE);
        assert!(a.raise_read_marker(&channel(ch), 8).await.expect("raise"));
        rounds(&[&a], 3).await;
        assert_eq!(fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE), puts);
        let (markers, _) = fresh_device_reads(&fx, "fresh").await;
        assert!(markers.is_empty(), "{markers:?}");

        a_in.set(&[[ch; 32]]);
        rounds(&[&a], 2).await;
        assert_eq!(census(&fx), [(a_w, 2)].into(), "both items written back");
        let (markers, _) = fresh_device_reads(&fx, "fresh-2").await;
        assert_eq!(markers, vec![(channel(ch), 8)], "the kept position");
        a.shutdown().await;
    }

    /// **Row 50 leg 1 — the stale-writer heal (succession decision 4).** A
    /// successor rotation lands under a LIVE runtime, exactly as the
    /// succession ceremony in another process will do it: slot re-keyed
    /// first, then the store's writer meta re-stamped (the fence — the ruled
    /// ordering). The live runtime's next local append refuses TYPED (the
    /// caller sees the `StaleWriter` chain and can retry), the worker
    /// reassembles from the shared slot, and the retried write lands under
    /// the successor — no interleaved logs, no dead thread. Sequenced by
    /// the pass barrier, not by timing (convention 14): the prologue and the
    /// pre-rotation write's publish step both write the store, so without
    /// `settled()` one of them can meet the rotation first, reassemble, and
    /// hand the "stale" put an already-healed runtime. With the backstop
    /// disarmed nothing runs between the barrier and the stale put. The
    /// transaction-level interleaving is the store crate's own guard test.
    #[tokio::test]
    async fn a_live_runtime_heals_typed_when_the_writer_rotates_under_it() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("runtime");
        handle
            .put_preference(KIND_MODERATION, moderation_value(&["pre"]))
            .await
            .expect("pre-rotation write");
        // The prologue and the publish step the write armed are behind the
        // fence before the rotation lands.
        handle.settled().await;

        // The "successor ceremony" from outside the runtime: mint, re-key
        // the slot, then re-stamp the store meta.
        let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, fx.cred_dir.join("a"));
        let old_hex = creds
            .get(&fx.actor_hex)
            .expect("the assembly minted a writer into the slot");
        let old_pub = SigningKey::from_bytes(&fauna_core::hex32::decode(&old_hex).unwrap())
            .verifying_key()
            .to_bytes();
        let successor = SigningKey::from_bytes(&[0x5C; 32]);
        creds.set(
            &fx.actor_hex,
            &fauna_core::hex32::encode(&successor.to_bytes()),
        );
        let store_dir = StoreRoot::at(fx.base.join("a"))
            .store_dir(&fx.actor_hex)
            .expect("store dir");
        let backend = SqliteBackend::open(&store_dir).expect("second connection");
        fauna_account_store::store::rotate_writer_identity(
            &backend,
            &WriterId(old_pub),
            &WriterId(successor.verifying_key().to_bytes()),
        )
        .await
        .expect("the fence");
        drop(backend);

        // Refused typed…
        let err = handle
            .put_preference(KIND_MODERATION, moderation_value(&["stale"]))
            .await
            .expect_err("the stale writer must be refused");
        assert!(
            fauna_account_store::store::is_stale_writer(&err),
            "typed StaleWriter expected in the chain, got: {err:#}"
        );

        // …and healed: the same handle's retry lands under the successor
        // (the worker reassembled from the slot before serving it).
        let healed = moderation_value(&["healed"]);
        handle
            .put_preference(KIND_MODERATION, healed.clone())
            .await
            .expect("the reassembled runtime serves the retry");
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("entry present");
        assert_eq!(entry.value, healed);
        handle.shutdown().await;
    }

    /// **Row 50 decisions 1–3, end-to-end at the runtime level.** A machine
    /// enrolls, writes a preference the nest never receives (the un-pushed
    /// tail — the publish leg one-shot-fails while the local row lands
    /// durably), quits; the user deletes the device (the fake tombstones the
    /// key exactly as the real handler's check 5 does); the next seed-holding
    /// sign-in's ceremony probe meets the typed `grant_revoked` answer, mints
    /// a successor (fresh writer + fresh `DeviceAuthorization`, store meta
    /// re-stamped), restarts assembly, and the first pump pass re-authors the
    /// tail under the successor and publishes it — the no-data-loss proof.
    /// Deterministic: every step is sequential (shutdown before delete,
    /// assembly before the read), no race window needed (convention 14).
    #[tokio::test]
    async fn a_deleted_machine_revives_as_a_successor_with_its_unpushed_tail() {
        let fx = fixture();
        let value = moderation_value(&["survives-the-delete"]);
        let old_device_id;
        {
            let handle = AccountStoreRuntime::start(fx.params("a"))
                .await
                .expect("first enrollment");
            // Converge enrollment (probe + prologue both had their chance).
            let report = handle.reconcile_now().await.expect("pass");
            assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
            // The un-pushed write: the publish leg fails once, the local row
            // is durable-before-error by contract.
            fx.fake.fail_next_put.store(true, Ordering::SeqCst);
            let _ = handle.put_preference(KIND_MODERATION, value.clone()).await;
            let entry = handle
                .get_preference(KIND_MODERATION)
                .await
                .expect("get")
                .expect("the local row is durable despite the failed publish");
            assert_eq!(entry.value, value);
            old_device_id = fauna_core::hex32::encode(
                &handle
                    .principal_bundle_status()
                    .await
                    .expect("status")
                    .device_authorization
                    .expect("enrolled")
                    .device_key,
            );
            handle.shutdown().await;
        }

        // The user's delete, while no process runs on the machine.
        fx.fake.revoke_device(&named_row("a"));
        // Scope-level, not the total: the successor's fleet bootstrap also
        // publishes (state-fleet rows) — only the re-authored tail moves the
        // account-state ("state") counter.
        let state_puts_before = fx.fake.put_calls_for("state");
        let (row_registers_before, old_key_grants_before) = {
            let s = fx.fake.state.lock().unwrap();
            (
                s.sync_registers.len(),
                s.grant_keys.iter().filter(|k| **k == old_device_id).count(),
            )
        };

        // The next sign-in: probe → typed refusal → rotation → reassembly.
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("revival sign-in");
        let report = handle.reconcile_now().await.expect("pass");
        // The successor registered (probe latch) — never the dead key again.
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let new_device_id = fauna_core::hex32::encode(
            &handle
                .principal_bundle_status()
                .await
                .expect("status")
                .device_authorization
                .expect("the successor is enrolled")
                .device_key,
        );
        assert_ne!(
            new_device_id, old_device_id,
            "revival must mint a SUCCESSOR, never resurrect the dead key"
        );
        {
            let s = fx.fake.state.lock().unwrap();
            assert!(
                s.grant_keys.contains(&new_device_id),
                "the successor's grant registered: {:?}",
                s.grant_keys
            );
            assert_eq!(
                s.grant_keys.iter().filter(|k| **k == old_device_id).count(),
                old_key_grants_before,
                "the dead key's grant must not have re-attached post-delete"
            );
            // Ghost-free: the probe is grant-first precisely so the DEAD key
            // never re-creates the row the user deleted. The machine's named
            // row comes back exactly once — the successor's own enrollment —
            // and it carries the successor, never the dead key.
            assert_eq!(
                s.sync_registers.len(),
                row_registers_before + 1,
                "only the successor's enrollment re-registers the named row"
            );
            assert_eq!(
                s.granted_key.as_deref(),
                Some(new_device_id.as_str()),
                "the re-created row carries the successor's grant"
            );
        }
        // The un-pushed value survived the succession locally…
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the tail survived");
        assert_eq!(entry.value, value, "the no-data-loss proof");
        // …and actually reached the nest this time, re-authored under the
        // successor (the re-author runs in the prologue; its publish is what
        // moves the account-state put counter past the pre-revival count),
        // with the nest's feed row carrying the SUCCESSOR's writer id.
        assert!(
            fx.fake.put_calls_for("state") > state_puts_before,
            "the re-authored tail must publish after revival"
        );
        {
            let s = fx.fake.state.lock().unwrap();
            assert!(
                s.feed
                    .iter()
                    .any(|r| r.scope == "state" && r.writer_id == new_device_id),
                "the nest must hold the re-authored row under the successor writer"
            );
        }
        handle.shutdown().await;
    }

    /// **A machine deleted while its app sits beside a seedless engine holder
    /// revives from the app's seed pass** (`account-runtime.md` § Multi-instance
    /// concurrency → *The seed-leg role*, part 4, *Enrollment*: a
    /// removed-from-account answer reassembles the seed-holding runtime exactly
    /// as it does in a pass). The agent pumps and holds no seed; the app never
    /// pumps. The user deletes the device, and the device handshake's
    /// `not_registered` answer voids the latch. The app's seed pass then runs
    /// the registration, is answered that the key was revoked, and the runtime
    /// reassembles: the ceremony mints a successor and registers it.
    ///
    /// Red-verified: with a seed pass's report read for a stale writer alone,
    /// every seed pass answers `RemovedFromAccount` and the dead key stays the
    /// machine's principal.
    #[tokio::test]
    async fn a_machine_deleted_beside_a_seedless_holder_revives_from_the_seed_pass() {
        let fx = fixture();
        // The sign-in that enrolls the machine, before the agent comes up.
        let sign_in = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("first enrollment");
        sign_in.reconcile_now().await.expect("pass");
        let old_device_id = enrolled_writer_hex(&sign_in).await;
        sign_in.shutdown().await;

        let agent = AccountStoreRuntime::start(AccountRuntimeParams {
            principal: RuntimePrincipal::Seedless,
            ..fx.params("a")
        })
        .await
        .expect("the seedless agent");
        agent.reconcile_now().await.expect("the agent's pass");
        assert!(agent.is_engine_holder(), "the agent pumps");
        let app = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the app beside it");
        let report = app.reconcile_now().await.expect("the app's seed pass");
        assert!(report.skipped_non_holder && !app.is_engine_holder());
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));

        // The user's delete, and what the machine's next device handshake
        // learns of it.
        fx.fake.revoke_device(&named_row("a"));
        crate::principal_bundle::not_registered_voids_latch_in(
            fx.params("a").credentials,
            &fx.actor_hex,
        )();

        let report = app.reconcile_now().await.expect("the app's seed pass");
        assert!(report.skipped_non_holder, "{report:?}");
        assert_eq!(
            report.enrollment,
            Some(EnrollmentPass::RemovedFromAccount),
            "the seed pass ran the registration and met the revocation"
        );
        // The reassembly follows the reply; the next command is served by the
        // reassembled runtime.
        let report = app.reconcile_now().await.expect("the app's seed pass");
        assert!(
            report.skipped_non_holder && !app.is_engine_holder(),
            "the revived app still never pumps: {report:?}"
        );
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let new_device_id = enrolled_writer_hex(&app).await;
        assert_ne!(
            new_device_id, old_device_id,
            "the ceremony minted a successor principal"
        );
        {
            let s = fx.fake.state.lock().unwrap();
            assert_eq!(
                s.granted_key.as_deref(),
                Some(new_device_id.as_str()),
                "and the machine's row carries the successor's grant"
            );
        }

        app.shutdown().await;
        agent.shutdown().await;
    }

    /// **A removed-heal reassembly whose probe rotated nothing is not repeated
    /// before the backstop tick** (`account-replica-posture.md` § The store
    /// device principal → *Principal succession after a device delete*,
    /// refinement 7). The user deletes the device; the pump's register is
    /// answered revoked and the worker reassembles, but the assembly's probe
    /// meets a transport fault and skips. The rotation cap is not spent — no
    /// rotation ran — and the new assembly's prologue is answered revoked
    /// again. That second answer must not reassemble: the runtime serves,
    /// loud, and tries the heal again at the backstop tick, where a probe
    /// that is answered mints the successor.
    ///
    /// Red-verified: with the reassembly gated by the rotation cap alone, the
    /// worker reassembles on every prologue with no wait in the loop and the
    /// runtime never serves another command.
    #[tokio::test]
    async fn a_removed_heal_whose_probe_rotated_nothing_waits_for_the_backstop_tick() {
        const BACKSTOP: Duration = Duration::from_millis(200);
        // A pass sends one register; a pass that reassembles adds the probe's
        // and the prologue's.
        const REGISTERS_PER_HEAL: usize = 3;
        const RECONCILES: usize = 3;

        let fx = fixture();
        let handle = AccountStoreRuntime::start(AccountRuntimeParams {
            backstop_interval: BACKSTOP,
            ..fx.params("a")
        })
        .await
        .expect("first enrollment");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let old_device_id = enrolled_writer_hex(&handle).await;

        // The flap first, so no pass can meet the tombstone without it. Then
        // the user's delete, and what the machine's next device handshake
        // learns of it.
        fx.fake
            .flap_revoked_grant_registers
            .store(true, Ordering::SeqCst);
        let attempts = || fx.fake.state.lock().unwrap().grant_register_attempts;
        let attempts_before = attempts();
        let since = std::time::Instant::now();
        fx.fake.revoke_device(&named_row("a"));
        crate::principal_bundle::not_registered_voids_latch_in(
            fx.params("a").credentials,
            &fx.actor_hex,
        )();

        // A pass-bound command is served between passes, by a runtime that
        // has stopped reassembling.
        for n in 1..=RECONCILES {
            tokio::time::timeout(EVENTUALLY_BUDGET, handle.reconcile_now())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "reconcile {n} was never served — the worker is reassembling on \
                         every prologue: {} grant registers since the delete",
                        attempts() - attempts_before
                    )
                })
                .expect("pass");
        }
        // One heal when the answer first arrives, then at most one per
        // backstop tick: the count read first, the clock after, so the ticks
        // counted are at least the ticks that had fired.
        let sent = attempts() - attempts_before;
        let ticks = (since.elapsed().as_millis() / BACKSTOP.as_millis()) as usize + 1;
        assert!(
            sent >= REGISTERS_PER_HEAL,
            "the removal reassembled the runtime once: {sent} grant registers"
        );
        assert!(
            sent <= REGISTERS_PER_HEAL * (1 + ticks) + RECONCILES,
            "a heal that rotated nothing is repeated at the backstop cadence, never \
             sooner: {sent} grant registers over {ticks} ticks"
        );
        assert_eq!(
            enrolled_writer_hex(&handle).await,
            old_device_id,
            "no probe was answered, so nothing rotated"
        );

        // The nest answers the probe: the next tick's pass reassembles and
        // the ceremony mints the successor.
        fx.fake
            .flap_revoked_grant_registers
            .store(false, Ordering::SeqCst);
        eventually(
            || {
                let s = fx.fake.state.lock().unwrap();
                s.granted_key
                    .as_deref()
                    .is_some_and(|key| key != old_device_id)
            },
            "the successor's grant registered",
        )
        .await;
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        assert_ne!(enrolled_writer_hex(&handle).await, old_device_id);

        handle.shutdown().await;
    }

    /// The enrolled writer's public key, hex — what the nest's feed rows and
    /// device rows name this machine by.
    async fn enrolled_writer_hex(handle: &AccountStoreHandle) -> String {
        fauna_core::hex32::encode(
            &handle
                .principal_bundle_status()
                .await
                .expect("status")
                .device_authorization
                .expect("enrolled")
                .device_key,
        )
    }

    /// One enrolled device with one un-pushed local row, shut down — the
    /// starting state of every lost-slot scenario below. Returns the writer
    /// hex and the slot's writer secret (hex) as they were before the loss.
    async fn enroll_with_an_unpushed_row(fx: &Fixture, value: &[u8]) -> (String, String) {
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("first enrollment");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        // The un-pushed write: the publish leg fails once, the local row is
        // durable-before-error by contract.
        fx.fake.fail_next_put.store(true, Ordering::SeqCst);
        let _ = handle.put_preference(KIND_MODERATION, value.to_vec()).await;
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the local row is durable despite the failed publish");
        assert_eq!(entry.value, value);
        let writer_hex = enrolled_writer_hex(&handle).await;
        handle.shutdown().await;
        let secret_hex = CredentialStore::with_file_backend(CRED_NAMESPACE, fx.cred_dir.join("a"))
            .get(&fx.actor_hex)
            .expect("the T10 slot holds the writer secret");
        (writer_hex, secret_hex)
    }

    /// The slot is lost while the store dir survives: the whole T10
    /// namespace goes (a reset login keychain / Credential Manager / Secret
    /// Service collection), the
    /// store dir — stamped with the old writer, holding the un-pushed row —
    /// stays exactly as it was.
    fn lose_the_slot(fx: &Fixture) {
        let slot_dir = fx.cred_dir.join("a");
        std::fs::remove_dir_all(&slot_dir).expect("wipe the slot");
        std::fs::create_dir_all(&slot_dir).expect("an empty slot dir");
    }

    /// Device `a`'s account-store dir — the journal, under the W6 root.
    fn store_dir_of(fx: &Fixture, device: &str) -> PathBuf {
        StoreRoot::at(fx.base.join(device))
            .store_dir(&fx.actor_hex)
            .expect("store dir")
    }

    /// One life of device `a`: assemble, put one moderation value, report
    /// the writer it enrolled as, shut down.
    async fn a_life_that_puts(fx: &Fixture, value: &[u8]) -> String {
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("assemble");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        handle
            .put_preference(KIND_MODERATION, value.to_vec())
            .await
            .expect("the local write lands");
        // Its publish step runs before the shutdown is served (a reused
        // coordinate would be refused there and left unsent).
        handle.settled().await;
        let writer = enrolled_writer_hex(&handle).await;
        handle.shutdown().await;
        writer
    }

    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).expect("mkdir");
        for entry in std::fs::read_dir(src).expect("read dir") {
            let entry = entry.expect("entry");
            let target = dst.join(entry.file_name());
            if entry.file_type().expect("type").is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy");
            }
        }
    }

    /// **The journal-bound writer, inverse arm (charter § The store device
    /// principal, refinement 11).** The slot keeps the writer key while the
    /// store dir is deleted — a config dir wiped by hand, or any carry that
    /// restores the slot without the replica. A fresh journal would re-issue seqs
    /// 1.. under the surviving key: the nest refuses the ones whose item has
    /// a head (`stale_writer_seq`, wedging `publish_pending` for good) and
    /// ACCEPTS the rest at coordinates the previous life already used, which
    /// every other replica then meets as journal equivocation. The ruling: a
    /// key loaded over a store with no stamped writer is abandoned on the
    /// spot — a fresh writer is minted, the machine is a new fleet device,
    /// and a second replica walks both lives as ordinary history.
    #[tokio::test]
    async fn a_surviving_slot_over_a_fresh_store_retires_the_key_and_a_second_replica_walks_both_lives()
     {
        let fx = fixture();
        let first_value = moderation_value(&["life-one"]);
        let first_writer = a_life_that_puts(&fx, &first_value).await;
        let first_life_coordinates: Vec<(String, i64)> = {
            let s = fx.fake.state.lock().unwrap();
            s.coordinates_seen
                .iter()
                .filter(|(scope, w, _)| scope == ACCOUNT_STATE_SCOPE && *w == first_writer)
                .map(|(_, w, seq)| (w.clone(), *seq))
                .collect()
        };
        assert!(
            !first_life_coordinates.is_empty(),
            "fixture: the first life must have published under its writer"
        );

        // The journal is gone; the slot is not.
        std::fs::remove_dir_all(store_dir_of(&fx, "a")).expect("delete the store dir");

        let second_value = moderation_value(&["life-two"]);
        let second_writer = a_life_that_puts(&fx, &second_value).await;
        assert_ne!(
            second_writer, first_writer,
            "the key loaded over a fresh store is abandoned, never put to work: a fresh \
             writer is minted and this machine enrolls as a new fleet device"
        );
        {
            let s = fx.fake.state.lock().unwrap();
            assert!(
                s.feed
                    .iter()
                    .any(|r| r.scope == ACCOUNT_STATE_SCOPE && r.writer_id == second_writer),
                "the second life's row reached the nest under the fresh writer"
            );
            assert!(
                s.feed
                    .iter()
                    .any(|r| r.scope == ACCOUNT_STATE_SCOPE && r.writer_id == first_writer),
                "the first life's row is still history on the feed — nothing is re-stamped"
            );
        }

        // A second replica walks both lives: no equivocation, and the newer
        // write wins by its own stamp — the previous life's rows are ordinary
        // history under the plane's own merge rule, never privileged or
        // refused by writer identity.
        let other = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("a second replica assembles");
        let report = other
            .reconcile_now()
            .await
            .expect("the walk over both lives completes");
        assert!(
            report.errors.is_empty(),
            "a second replica must walk both lives cleanly: {:?}",
            report.errors
        );
        let walk = report.walk.expect("the account-state scope walked");
        assert_eq!(
            walk.own_burnt, 0,
            "another replica's history is never 'burnt' here"
        );
        let entry = other
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the second replica converged on the preference");
        assert_eq!(
            entry.value, second_value,
            "the newer life's write wins by its LWW stamp, not by writer identity"
        );
        other.shutdown().await;
    }

    /// **The inverse arm must not fire on a mint still in flight.** A crash
    /// between the slot mint and the first store open — and, the same shape
    /// concurrently, a sibling assembly on the same dir (the app beside its
    /// agent, V9/V10 in `conformance_account_runtime.rs`) loading the key its
    /// sibling minted before that sibling's open stamped the store — is a
    /// LOADED key over an unstamped store whose journal was never lost. The
    /// slot's unstamped-mint marker tells the two apart: the next assembly
    /// adopts the key, as a first open does, and mints nothing.
    #[tokio::test]
    async fn a_crash_between_the_mint_and_the_first_open_adopts_the_key_instead_of_retiring_it() {
        let fx = fixture();
        let minted = {
            let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, fx.cred_dir.join("a"));
            let key = resolve_writer_key_serialized(
                &StoreRoot::at(fx.base.join("a")),
                &fx.actor_hex,
                &creds,
            )
            .expect("the mint into an empty slot — the process dies before any open");
            fauna_core::hex32::encode(&key.verifying_key().to_bytes())
        };

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the next assembly adopts the minted key");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        assert_eq!(
            enrolled_writer_hex(&handle).await,
            minted,
            "a key no store ever stamped never published: it is adopted, never retired"
        );
        handle.shutdown().await;
        // …and once a store is stamped with it, the marker is spent: the
        // next launch over a DELETED store dir does retire it.
        let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, fx.cred_dir.join("a"));
        assert!(
            !crate::principal_bundle::writer_unstamped(&creds, &fx.actor_hex),
            "the open spends the unstamped-mint marker"
        );
    }

    /// **The journal-bound writer, the burnt-journal heal (refinement 11).**
    /// The store dir is restored from an older backup while the slot kept
    /// the key: stamped writer == slot key, so nothing disagrees at
    /// assembly, but the feed holds rows under this writer above everything
    /// the restored journal holds — coordinates the pre-backup life used,
    /// which the next local put would reuse. The walk refuses them as the
    /// burnt signature (never ingests them, which would seed the counter
    /// past them by accident), stamps the durable marker, the pump
    /// reassembles, and the heal arm rotates the burnt writer onto a fresh
    /// mint through the ordinary fence. After it the old life's later row
    /// lands as retired-own history, and a new put publishes under the
    /// successor at a coordinate no life has used.
    #[tokio::test]
    async fn a_journal_restored_from_an_older_backup_is_found_burnt_by_the_walk_and_rotated() {
        let fx = fixture();
        let dir = store_dir_of(&fx, "a");
        let backup = fx.base.join("a-backup");

        let older_value = moderation_value(&["before-the-backup"]);
        let first_writer = a_life_that_puts(&fx, &older_value).await;
        copy_dir_recursive(&dir, &backup);
        let newer_value = moderation_value(&["after-the-backup"]);
        assert_eq!(
            a_life_that_puts(&fx, &newer_value).await,
            first_writer,
            "fixture: a consistent restart keeps its writer"
        );

        // The restore: an older journal under the same key.
        std::fs::remove_dir_all(&dir).expect("drop the current journal");
        copy_dir_recursive(&backup, &dir);

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the restored store assembles — the stamp agrees with the slot");
        // The prologue's walk meets the pre-backup life's later row; the
        // reassembly and the heal ride the passes a pass-bound command
        // waits for. Two passes bound it: one to detect, one on the healed
        // writer.
        handle.reconcile_now().await.expect("pass");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let successor = enrolled_writer_hex(&handle).await;
        assert_ne!(
            successor, first_writer,
            "the burnt writer is rotated away: the machine authors as a fresh writer"
        );
        // The pre-backup life's later row is this machine's RETIRED history
        // above what the restored journal held — new history, it lands.
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the preference is present");
        assert_eq!(
            entry.value, newer_value,
            "the old life's later write landed once its writer was retired (the \
             live-predecessor bound: a retired own writer's later rows are new history)"
        );
        // A new put publishes under the successor, at a coordinate no life
        // has used — the fake refuses a reused one, which would fail this put.
        let latest_value = moderation_value(&["after-the-heal"]);
        handle
            .put_preference(KIND_MODERATION, latest_value.clone())
            .await
            .expect("a put under the successor lands");
        handle.settled().await;
        {
            let s = fx.fake.state.lock().unwrap();
            assert!(
                s.feed
                    .iter()
                    .any(|r| r.scope == ACCOUNT_STATE_SCOPE && r.writer_id == successor),
                "the nest holds the successor's row"
            );
        }
        handle.shutdown().await;

        let reopened = SqliteBackend::open(&dir).expect("reopen the backend");
        let (stamped, burnt) = fauna_account_store::store::stamped_and_burnt_writer(&reopened)
            .await
            .expect("meta");
        assert_eq!(
            stamped.map(|w| w.to_hex()),
            Some(successor),
            "the store is stamped with the successor"
        );
        assert_eq!(
            burnt, None,
            "the burnt verdict is inert once the stamp moved on"
        );
        assert!(
            fauna_account_store::store::retired_writers(&reopened)
                .await
                .expect("retired")
                .iter()
                .any(|w| w.to_hex() == first_writer),
            "the burnt writer joined the retired memory"
        );
    }

    /// The account-state frontier slot a store dir holds for `writer` — for
    /// the CURRENT writer this doubles as the published-to-the-nest
    /// high-water, which is why nothing a walk refuses may advance it.
    async fn frontier_seq_of(dir: &std::path::Path, writer: &WriterId) -> u64 {
        SqliteBackend::open(dir)
            .expect("reopen")
            .frontier(ACCOUNT_STATE_SCOPE)
            .await
            .expect("frontier")
            .into_iter()
            .find(|(w, _)| w == writer)
            .map_or(0, |(_, seq)| seq)
    }

    /// The writer a hex id names — the `WriterId` the store-level helpers
    /// take, from the hex the runtime helpers report.
    fn writer_id_of(hex_writer: &str) -> WriterId {
        WriterId(
            hex::decode(hex_writer)
                .expect("writer hex")
                .try_into()
                .expect("32 bytes"),
        )
    }

    /// **A row the nest made up does not burn the writer — the
    /// above-the-journal arm (refinement 11 arm (b)).** `origin_writer` and
    /// `origin_seq` are the NEST's word about a row, not the row's own: they
    /// ride the feed metadata, outside the seal. This arm used to be believed
    /// on those coordinates alone, so a nest — or a same-account sibling
    /// putting under this writer's id — could serve garbage bytes above
    /// everything the journal holds and have the walk stamp the writer BURNT:
    /// a reassembly and a fresh enrollment once per runtime worker, once more
    /// per relaunch, for free. The arm opens the envelope first now. A row
    /// that opens under this writer's in-seal signature is a previous life's
    /// and still burns (the backup-restore pin above); one that opens under no
    /// key is accounted `unopened` and nothing else happens — no burn, no
    /// rotation, and no relay row, since relaying it would assert the forgery
    /// to every peer as this writer's own word.
    #[tokio::test]
    async fn a_forged_unopenable_own_row_above_the_journal_is_unopened_not_burnt() {
        let fx = fixture();
        let dir = store_dir_of(&fx, "a");
        let first_writer = a_life_that_puts(&fx, &moderation_value(&["the-only-life"])).await;

        let forged_seq = state_coordinates_of(&fx, &first_writer)
            .iter()
            .copied()
            .max()
            .expect("fixture: the life published at least one account-state row")
            + 10;
        fx.fake
            .stage_forged_state_row(ACCOUNT_STATE_SCOPE, &first_writer, forged_seq);

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("assemble");
        // Three passes. The burn landed in the assembly prologue's own walk,
        // so one pass could not tell a stable verdict from a once-only one —
        // and every pass re-presents the row, because nothing may advance the
        // frontier past a row this replica does not hold.
        for pass in 0..3 {
            let report = handle.reconcile_now().await.expect("pass");
            let walk = report.walk.expect("the account-state scope walked");
            assert_eq!(
                walk.own_burnt, 0,
                "pass {pass}: a row nobody sealed is no evidence of a burnt journal"
            );
            assert!(
                walk.unopened >= 1,
                "pass {pass}: the forged row is accounted unopened, which is all it \
                 deserves: {walk:?}"
            );
            assert_eq!(
                enrolled_writer_hex(&handle).await,
                first_writer,
                "pass {pass}: the writer is not rotated by a row the nest made up"
            );
        }
        handle.shutdown().await;

        let writer = writer_id_of(&first_writer);
        let forged = u64::try_from(forged_seq).expect("seq");
        assert!(
            relay_item_keys_at(&dir, &writer, forged).await.is_empty(),
            "the forgery is not recorded as this replica's relay word at the coordinate"
        );
        assert!(
            frontier_seq_of(&dir, &writer).await < forged,
            "the own slot is the published high-water: nothing may advance it past a row \
             this replica does not hold"
        );
        assert!(
            !relay_item_keys_at(
                &dir,
                &writer,
                *journal_rows_of(&dir, &writer)
                    .await
                    .iter()
                    .map(|row| row.seq)
                    .max()
                    .as_ref()
                    .expect("fixture: the life journaled a row"),
            )
            .await
            .is_empty(),
            "and the genuine row's own relay copy is untouched"
        );
    }

    /// **The same, at a coordinate the journal holds NOTHING at (refinement
    /// 11 arm (b)).** The fourth refusal of the same signature, and the one
    /// reachable without the forger having to outrun the journal: a writer's
    /// seq counter is cross-scope (`next_local_seq` reads `max_writer_seq`
    /// unfiltered) while `max_held_seq` is per-scope, so every seq this writer
    /// spent on the sibling fleet scope is a gap in the account-state
    /// journal — below its own high-water, and legitimately empty. A pump pass
    /// walks from a ZERO frontier (`AccountStatePlane::reconcile`), so a row
    /// planted in such a gap is served on every pass, not once. It too is
    /// decided on nest-asserted coordinates unless the envelope is opened
    /// first.
    #[tokio::test]
    async fn a_forged_unopenable_own_row_at_an_unheld_coordinate_is_unopened_not_burnt() {
        let fx = fixture();
        let dir = store_dir_of(&fx, "a");
        let first_writer = a_life_that_puts(&fx, &moderation_value(&["the-only-life"])).await;
        let writer = writer_id_of(&first_writer);

        let held: Vec<u64> = journal_rows_of(&dir, &writer)
            .await
            .iter()
            .map(|row| row.seq)
            .collect();
        let top = *held
            .iter()
            .max()
            .expect("fixture: the life journaled at least one account-state row");
        let forged_seq = (1..top).find(|seq| !held.contains(seq)).unwrap_or_else(|| {
            panic!(
                "fixture: the fleet scope spends a seq below the account scope's high-water, \
                     leaving a gap to plant in — account-state journal holds {held:?}"
            )
        });
        fx.fake.stage_forged_state_row(
            ACCOUNT_STATE_SCOPE,
            &first_writer,
            i64::try_from(forged_seq).expect("seq"),
        );

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("assemble");
        for pass in 0..3 {
            let report = handle.reconcile_now().await.expect("pass");
            let walk = report.walk.expect("the account-state scope walked");
            assert_eq!(
                walk.own_burnt, 0,
                "pass {pass}: a gap the fleet scope explains is not a burnt journal"
            );
            assert!(
                walk.unopened >= 1,
                "pass {pass}: the forged row is accounted unopened: {walk:?}"
            );
            assert_eq!(
                enrolled_writer_hex(&handle).await,
                first_writer,
                "pass {pass}: the writer is not rotated by a row the nest made up"
            );
        }
        handle.shutdown().await;

        assert!(
            relay_item_keys_at(&dir, &writer, forged_seq)
                .await
                .is_empty(),
            "the forgery is not recorded as this replica's relay word at the coordinate"
        );
        assert!(
            frontier_seq_of(&dir, &writer).await >= top,
            "the own slot still names everything this replica published — a refusal in a gap \
             below it neither advances nor regresses the published high-water"
        );
        assert!(
            !relay_item_keys_at(&dir, &writer, top).await.is_empty(),
            "and the genuine row's own relay copy is untouched"
        );
    }

    /// **The same-item burnt coordinate, detected through the entry
    /// (refinement 11).** The commonest restore shape, and the one neither
    /// older burnt arm can see: a store dir restored from an older backup
    /// whose next writes hit exactly the keys the previous life wrote after
    /// the backup — here one key, moderation, on both lives. Every
    /// coordinate the feed serves is then held, and held under the SAME
    /// item, so the row is not above the journal and not under a different
    /// item. The journal row carries no value to compare with; the ENTRY
    /// does, for as long as the row is its latest write, and the burnt
    /// life's last write is exactly that. The walk compares the served
    /// plaintext with the entry's own, finds other content, and refuses the
    /// row as the burnt signature. Undetected, the replica is wedged: the
    /// nest refuses the reused coordinate forever and every later local
    /// write drains behind it, so nothing this machine writes ever reaches
    /// the fleet again.
    #[tokio::test]
    async fn a_burnt_life_that_rewrote_only_the_old_lifes_keys_is_found_burnt_through_the_entry() {
        let fx = fixture();
        let dir = store_dir_of(&fx, "a");
        let backup = fx.base.join("a-backup");

        let first_writer = a_life_that_puts(&fx, &moderation_value(&["before-the-backup"])).await;
        copy_dir_recursive(&dir, &backup);
        let before = state_coordinates_of(&fx, &first_writer);

        // The pre-backup life goes on, re-writing the key it already wrote.
        let old_stamp = {
            let handle = AccountStoreRuntime::start(fx.params("a"))
                .await
                .expect("assemble");
            let report = handle.reconcile_now().await.expect("pass");
            assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
            let stamp = handle
                .put_preference(KIND_MODERATION, moderation_value(&["the-old-life"]))
                .await
                .expect("the moderation put publishes");
            assert_eq!(enrolled_writer_hex(&handle).await, first_writer);
            handle.shutdown().await;
            stamp
        };
        let fresh: Vec<i64> = state_coordinates_of(&fx, &first_writer)
            .difference(&before)
            .copied()
            .collect();
        assert_eq!(
            fresh.len(),
            1,
            "fixture: the old life published exactly one row after the backup: {fresh:?}"
        );
        let coordinate = fresh[0];
        assert_eq!(
            feed_dump(&fx, &first_writer)
                .iter()
                .map(|(seq, _)| *seq)
                .max(),
            Some(coordinate),
            "fixture: that row is the TOP of the feed under this writer — below it the \
             above-everything-the-journal-holds arm fires first and this test proves nothing: {:?}",
            feed_dump(&fx, &first_writer)
        );
        // The blinded key the feed serves there. It is derived per
        // `(kind, key)` and is writer-independent, so the successor's
        // re-published moderation row carries the same one — which is how
        // the end of this test proves the coordinate was served under the
        // very item the burnt journal holds there.
        let served_item_key = {
            let keys = feed_item_keys_at(&fx, &first_writer, coordinate);
            assert_eq!(
                keys.len(),
                1,
                "fixture: one item at the coordinate: {keys:?}"
            );
            keys.into_iter().next().expect("one item key")
        };

        // The restore, then the burnt life, offline: one moderation write,
        // landing at the very coordinate the old life spent on the same key,
        // with a later stamp so its value wins once it is carried.
        std::fs::remove_dir_all(&dir).expect("drop the current journal");
        copy_dir_recursive(&backup, &dir);
        let burnt_value = moderation_value(&["the-burnt-life"]);
        {
            let backend = SqliteBackend::open(&dir).expect("open the restored backend");
            let writer = fauna_account_store::store::stamped_writer(&backend)
                .await
                .expect("meta")
                .expect("the restored store is stamped");
            assert_eq!(writer.to_hex(), first_writer);
            let store = AccountStore::open(backend, &fx.actor_hex, writer)
                .await
                .expect("open as the surviving writer");
            let merge_meta = LwwStamp {
                at_ms: old_stamp.at_ms + 1,
                writer: writer.0,
            }
            .encode()
            .expect("stamp");
            let mut writes = 0usize;
            let mut seq = 0;
            while i64::try_from(seq).unwrap() < coordinate {
                seq = store
                    .put_state(StateEntry {
                        kind: KIND_MODERATION.into(),
                        key: PREFERENCE_KEY.into(),
                        scope: ACCOUNT_STATE_SCOPE.into(),
                        value: burnt_value.clone(),
                        merge_meta: Some(merge_meta.clone()),
                        entry_version: 0,
                        tombstone: false,
                    })
                    .await
                    .expect("the burnt life journals")
                    .1;
                writes += 1;
            }
            assert_eq!(
                i64::try_from(seq).unwrap(),
                coordinate,
                "fixture: the burnt life's LAST row sits at the old life's coordinate — it is \
                 the entry's latest write, which is what the arm under test compares"
            );
            assert_eq!(
                writes, 1,
                "fixture: it reused exactly that one coordinate; every other coordinate the \
                 journal holds is the backup's own row, under its own item"
            );
        }

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the restored store assembles — the stamp agrees with the slot");
        // The assembly prologue's own walk already meets the row, so the
        // detect-and-heal can land before the first nudged pass; these three
        // are the healed writer's, and they must be quiet and stable.
        let mut burnt_after_heal = 0usize;
        let mut passes = Vec::new();
        for _ in 0..3 {
            let report = handle.reconcile_now().await.expect("pass");
            burnt_after_heal += report.walk.as_ref().map_or(0, |walk| walk.own_burnt);
            passes.push(format!("{:?} errors={:?}", report.walk, report.errors));
        }
        // `mark_writer_burnt` is reachable from nowhere but the walk's
        // `refuse_burnt`, so the rotation IS the detection: without the
        // content compare this replica keeps authoring as `first_writer`.
        let successor = enrolled_writer_hex(&handle).await;
        assert_ne!(
            successor,
            first_writer,
            "a served row at a held coordinate under the same item with OTHER content is the \
             burnt signature — the entry is what carries the value to compare, and the burnt \
             writer must be rotated away. passes={passes:#?} feed={:?} coordinate={coordinate}",
            feed_dump(&fx, &first_writer)
        );
        assert_eq!(
            burnt_after_heal, 0,
            "the healed writer's own rows are echoes — the arm does not re-burn every pass: \
             {passes:#?}"
        );
        let healed = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("moderation is present");
        assert_eq!(
            healed.value, burnt_value,
            "the burnt life's later write survives the heal"
        );
        // WHICH arm fired. The successor re-publishes the same moderation
        // entry, and the blind is per `(kind, key)`, so its item key on the
        // feed is the very one the old life's row at `coordinate` carries:
        // the coordinate was held (the fixture's `writes == 1`), served
        // under the item the journal holds there, and the served row was the
        // top of the feed — neither the above-the-journal arm nor the
        // different-item arm could have fired. Only the content compare is
        // left.
        {
            let s = fx.fake.state.lock().unwrap();
            let successor_keys: Vec<&Vec<u8>> = s
                .feed
                .iter()
                .filter(|r| r.scope == ACCOUNT_STATE_SCOPE && r.writer_id == successor)
                .map(|r| &r.item_key)
                .collect();
            assert!(
                successor_keys.contains(&&served_item_key),
                "the burnt coordinate was served under the SAME item the journal holds there — \
                 the successor re-publishes it under the same blinded key: {successor_keys:?}"
            );
        }
        handle.shutdown().await;

        // The payoff: the value reaches the fleet. Undetected it never could
        // — the nest refuses the reused coordinate, and since every local
        // write drains through `publish_pending` in journal order, the
        // refused row holds every later one behind it forever.
        let other = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("a second replica assembles");
        let report = other.reconcile_now().await.expect("pass");
        assert!(
            report.errors.is_empty(),
            "the second replica walks every life cleanly: {:?}",
            report.errors
        );
        let converged = other
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the second replica holds moderation");
        assert_eq!(
            converged.value, burnt_value,
            "the restored replica's write reached the fleet — the publish wedge is gone"
        );
        other.shutdown().await;

        let reopened = SqliteBackend::open(&dir).expect("reopen the backend");
        let (stamped, burnt) = fauna_account_store::store::stamped_and_burnt_writer(&reopened)
            .await
            .expect("meta");
        assert_eq!(
            stamped.map(|w| w.to_hex()),
            Some(successor),
            "the store is stamped with the successor"
        );
        assert_eq!(
            burnt, None,
            "the burnt verdict is inert once the stamp moved on"
        );
    }

    /// **The same-item arm's false-positive surface, pinned.** The arm above
    /// reads a HEALTHY replica's own rows too: a full pass walks from a zero
    /// watermark, so every row this replica ever published comes back, each
    /// at a held coordinate under the same item. Each stays an echo because
    /// every path that rewrites an entry bumps `entry_version` (`put_state`,
    /// `ingest_state`, a tombstone), so a row that is still the entry's
    /// latest write sealed exactly the plaintext the entry holds now.
    /// Repeated writes to ONE item are the shape that matters: the nest
    /// collapses per `(item, writer)`, so the feed serves the latest
    /// coordinate — which is precisely the row the compare reads.
    #[tokio::test]
    async fn a_healthy_replicas_own_rows_are_echoes_not_burnt_coordinates() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("assemble");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let writer = enrolled_writer_hex(&handle).await;

        for words in [
            &["one"][..],
            &["one", "two"][..],
            &["one", "two", "three"][..],
        ] {
            handle
                .put_preference(KIND_MODERATION, moderation_value(words))
                .await
                .expect("the put publishes");
        }
        crate::preference_surfaces::save_sync_prefs(&handle, Some("latest_wins_always"))
            .await
            .expect("the sync-prefs save publishes");

        for pass in 1..=2 {
            let report = handle.reconcile_now().await.expect("pass");
            assert!(
                report.errors.is_empty(),
                "pass {pass}: a healthy replica walks its own rows cleanly: {:?}",
                report.errors
            );
            let walk = report.walk.expect("the account-state scope walked");
            assert_eq!(
                walk.own_burnt, 0,
                "pass {pass}: a healthy replica's own rows are echoes, never burnt: {walk:?}"
            );
            assert!(
                walk.self_echo >= 2,
                "pass {pass}: the pass walked this replica's own rows back — without that the \
                 zero above proves nothing: {walk:?}"
            );
        }
        assert_eq!(
            enrolled_writer_hex(&handle).await,
            writer,
            "nothing rotated: no burn was seen"
        );
        handle.shutdown().await;
    }

    /// Every coordinate the fake nest recorded under `writer` on the
    /// account-state scope — what one life published there.
    fn state_coordinates_of(fx: &Fixture, writer: &str) -> std::collections::BTreeSet<i64> {
        fx.fake
            .state
            .lock()
            .unwrap()
            .coordinates_seen
            .iter()
            .filter(|(scope, w, _)| scope == ACCOUNT_STATE_SCOPE && w == writer)
            .map(|(_, _, seq)| *seq)
            .collect()
    }

    /// The item keys the fake's feed serves at one `(writer, seq)` of the
    /// account-state scope — more than one only under
    /// `accept_reused_coordinates`, which accepts a reused coordinate under
    /// another item.
    fn feed_item_keys_at(fx: &Fixture, writer: &str, seq: i64) -> Vec<Vec<u8>> {
        fx.fake
            .state
            .lock()
            .unwrap()
            .feed
            .iter()
            .filter(|r| {
                r.scope == ACCOUNT_STATE_SCOPE && r.writer_id == writer && r.writer_seq == seq
            })
            .map(|r| r.item_key.clone())
            .collect()
    }

    /// Every account-state feed row the fake serves under `writer`, as
    /// `(seq, first two item-key bytes)` — the failure message of a
    /// coordinate assertion.
    fn feed_dump(fx: &Fixture, writer: &str) -> Vec<(i64, [u8; 2])> {
        fx.fake
            .state
            .lock()
            .unwrap()
            .feed
            .iter()
            .filter(|r| r.scope == ACCOUNT_STATE_SCOPE && r.writer_id == writer)
            .map(|r| (r.writer_seq, [r.item_key[0], r.item_key[1]]))
            .collect()
    }

    /// The relay rows a store dir holds for `writer` at `seq` in the
    /// account-state scope, as item keys.
    async fn relay_item_keys_at(
        dir: &std::path::Path,
        writer: &WriterId,
        seq: u64,
    ) -> Vec<Vec<u8>> {
        SqliteBackend::open(dir)
            .expect("reopen")
            .relay_rows(
                ACCOUNT_STATE_SCOPE,
                ItemClass::StateEntry.as_wire(),
                &[],
                u32::MAX,
            )
            .await
            .expect("relay rows")
            .into_iter()
            .filter(|r| r.writer == *writer && r.writer_seq == seq)
            .map(|r| r.item_key)
            .collect()
    }

    /// Every account-state journal row a store dir holds under `writer`.
    async fn journal_rows_of(
        dir: &std::path::Path,
        writer: &WriterId,
    ) -> Vec<fauna_account_store::types::JournalRow> {
        SqliteBackend::open(dir)
            .expect("reopen")
            .rows_for_scope(ACCOUNT_STATE_SCOPE, writer, 0, u32::MAX)
            .await
            .expect("rows")
    }

    /// The pre-backup life of device `a`: it publishes one moderation value
    /// and a first sync-prefs value, and reports the writer, the moderation
    /// stamp and the item key the feed serves moderation under.
    ///
    /// The sync-prefs save gives the restored replica a second kind's row of
    /// the old life. (It was added for the CAS-blob bridge, deleted at closure
    /// step (5) of the dissolution schedule: with no plane value the bridge
    /// imported the blob's later value under the still-current burnt writer
    /// before the heal, and that publish dissolved the shadow on its own.
    /// What these scenarios pin is the plane's own path, now the only one.)
    async fn a_life_that_puts_stamped(fx: &Fixture, value: &[u8]) -> (String, LwwStamp, Vec<u8>) {
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("assemble");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let writer = enrolled_writer_hex(&handle).await;
        let before = state_coordinates_of(fx, &writer);
        let stamp = handle
            .put_preference(KIND_MODERATION, value.to_vec())
            .await
            .expect("the local write lands");
        // The publish step runs after the write answers; the nest is read
        // behind the pass barrier. A reused coordinate would be refused there.
        handle.settled().await;
        let fresh: Vec<i64> = state_coordinates_of(fx, &writer)
            .difference(&before)
            .copied()
            .collect();
        assert_eq!(fresh.len(), 1, "fixture: one moderation row: {fresh:?}");
        let keys = feed_item_keys_at(fx, &writer, fresh[0]);
        assert_eq!(keys.len(), 1, "fixture: one item at the coordinate");
        crate::preference_surfaces::save_sync_prefs(&handle, Some("auto"))
            .await
            .expect("the first sync-prefs save publishes");
        handle.shutdown().await;
        (writer, stamp, keys[0].clone())
    }

    /// The pre-backup life's one save after the backup: sync prefs, at the
    /// coordinate the burnt life will reuse. Returns that coordinate, the
    /// item key the feed serves there, and the saved entry.
    async fn the_old_life_saves_sync_prefs(
        fx: &Fixture,
        first_writer: &str,
    ) -> (i64, Vec<u8>, fauna_account_store::types::StateEntry) {
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("assemble");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let before = state_coordinates_of(fx, first_writer);
        crate::preference_surfaces::save_sync_prefs(&handle, Some("latest_wins_always"))
            .await
            .expect("the sync-prefs save lands");
        handle.settled().await;
        let fresh: Vec<i64> = state_coordinates_of(fx, first_writer)
            .difference(&before)
            .copied()
            .collect();
        assert_eq!(
            fresh.len(),
            1,
            "fixture: the save published one row: {fresh:?}"
        );
        let keys = feed_item_keys_at(fx, first_writer, fresh[0]);
        assert_eq!(keys.len(), 1, "fixture: one item at the coordinate");
        let saved = handle
            .get_preference(KIND_SYNC_PREFS)
            .await
            .expect("get")
            .expect("saved");
        assert_eq!(enrolled_writer_hex(&handle).await, first_writer);
        handle.shutdown().await;
        (fresh[0], keys[0].clone(), saved)
    }

    /// The burnt life, forged over a restored backup: moderation journaled
    /// under the surviving key up to `through` (the old life's sync-prefs
    /// coordinate) with a stamp later than the old life's, a relay row
    /// recorded for it as the plane records one before every send. Nothing
    /// is published. Returns the writer.
    async fn a_burnt_life_journals_moderation_through(
        dir: &std::path::Path,
        fx: &Fixture,
        first_writer: &str,
        old_stamp: &LwwStamp,
        value: &[u8],
        moderation_key: &[u8],
        through: i64,
    ) -> WriterId {
        let backend = SqliteBackend::open(dir).expect("open the restored backend");
        let writer = fauna_account_store::store::stamped_writer(&backend)
            .await
            .expect("meta")
            .expect("the restored store is stamped");
        assert_eq!(writer.to_hex(), first_writer);
        let store = AccountStore::open(backend, &fx.actor_hex, writer)
            .await
            .expect("open as the surviving writer");
        let merge_meta = LwwStamp {
            at_ms: old_stamp.at_ms + 1,
            writer: writer.0,
        }
        .encode()
        .expect("stamp");
        let mut seq = 0;
        while i64::try_from(seq).unwrap() < through {
            seq = store
                .put_state(StateEntry {
                    kind: KIND_MODERATION.into(),
                    key: PREFERENCE_KEY.into(),
                    scope: ACCOUNT_STATE_SCOPE.into(),
                    value: value.to_vec(),
                    merge_meta: Some(merge_meta.clone()),
                    entry_version: 0,
                    tombstone: false,
                })
                .await
                .expect("the burnt life journals")
                .1;
            store
                .record_relay_row(&fauna_account_store::types::RelayRow {
                    scope: ACCOUNT_STATE_SCOPE.into(),
                    item_class: ItemClass::StateEntry.as_wire().to_string(),
                    writer,
                    writer_seq: seq,
                    item_key: moderation_key.to_vec(),
                    op: "state-put".into(),
                    entry: Some(b"sealed by the burnt life".to_vec()),
                    feed_seq: None,
                })
                .await
                .expect("the relay row the plane records before a send");
        }
        assert_eq!(
            i64::try_from(seq).unwrap(),
            through,
            "fixture: the burnt life's row sits at the old life's sync-prefs coordinate"
        );
        writer
    }

    /// Three passes on the restored store (detect, heal, walk healed), then
    /// the assertions every carry scenario shares: the writer rotated, the
    /// healed walk found nothing burnt and carried at least `carried` rows,
    /// the shadowed sync-prefs row converged and the burnt life's own
    /// moderation write survived. Returns the store dir's successor writer
    /// and the count of its journal rows, for the stability check.
    async fn heal_and_check_carried(
        fx: &Fixture,
        dir: &std::path::Path,
        first_writer: &str,
        old_sync: &fauna_account_store::types::StateEntry,
        burnt_value: &[u8],
        carried: usize,
    ) -> (WriterId, usize) {
        // The carry is this family's subject, every pass: the burnt rows stay
        // served, as behind a gate an unwalked sibling holds. Their retire
        // behind a member's cover is the cover step's, proved on the real
        // nest (`conformance_account_runtime`).
        fx.fake
            .delegable_retires_deferred
            .store(true, Ordering::SeqCst);
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the restored store assembles");
        // Detect (the prologue marks the burn), heal (the next pass
        // reassembles and rotates), then one pass on the healed writer.
        handle.reconcile_now().await.expect("pass");
        handle.reconcile_now().await.expect("pass");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        assert_ne!(
            enrolled_writer_hex(&handle).await,
            first_writer,
            "the burnt writer is rotated away"
        );
        let walk = report.walk.expect("the account-state scope walked");
        assert_eq!(
            walk.own_burnt, 0,
            "the healed writer's walk finds nothing burnt"
        );
        assert!(
            walk.retired_carried >= carried,
            "the retired burnt writer's rows at held coordinates are carried: {walk:?}"
        );
        let healed_sync = handle
            .get_preference(KIND_SYNC_PREFS)
            .await
            .expect("get")
            .expect("sync prefs reached the healed replica");
        assert_eq!(
            healed_sync.value, old_sync.value,
            "the shadowed sync-prefs row converged"
        );
        assert_eq!(
            handle
                .get_preference(KIND_MODERATION)
                .await
                .expect("get")
                .expect("moderation is present")
                .value,
            burnt_value,
            "the burnt life's own write survives the heal"
        );
        handle.shutdown().await;
        let successor =
            fauna_account_store::store::stamped_writer(&SqliteBackend::open(dir).expect("reopen"))
                .await
                .expect("meta")
                .expect("stamped with the successor");
        assert_ne!(successor.to_hex(), first_writer);
        let rows = journal_rows_of(dir, &successor).await.len();
        (successor, rows)
    }

    /// Three more passes on the healed store: every one carries again (the
    /// full reconcile re-presents the rows) and none lands a successor row
    /// — the carry is idempotent, so nothing is re-put.
    async fn check_carry_is_stable(
        fx: &Fixture,
        dir: &std::path::Path,
        successor: &WriterId,
        rows_after_carry: usize,
        carried: usize,
    ) {
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("reassemble");
        for _ in 0..3 {
            let report = handle.reconcile_now().await.expect("pass");
            assert!(
                report.errors.is_empty(),
                "a clean pass: {:?}",
                report.errors
            );
            let walk = report.walk.expect("walked");
            assert!(walk.retired_carried >= carried, "carried again: {walk:?}");
        }
        handle.shutdown().await;
        assert_eq!(
            journal_rows_of(dir, successor).await.len(),
            rows_after_carry,
            "no repeated re-put: the successor's log is stable across passes"
        );
    }

    /// **The burnt residue below the bound, carried** (refinement 11 → *the
    /// retired burnt writer's rows are carried, never echoed*). A store
    /// restored from backup in residue shape (i): the burnt life journaled moderation up to
    /// the coordinate the old life had spent on a sync-prefs save, its relay
    /// row recorded as the plane records one before a send, and its slot
    /// then raised past those rows the way an out-of-order inline
    /// publish would — forged, because no current build can (own rows publish
    /// in order now). The rows sit below the re-author's bound, so the heal
    /// neither re-puts nor compacts them, and the echo bound used to drop the
    /// fleet's sync-prefs row at that coordinate for good. Now a retired
    /// burnt writer's row at a held coordinate is carried: the fleet's value
    /// merges through the ordinary apply, the burnt life's relay row at that
    /// coordinate is retired for the fleet's, the journal is left alone, and
    /// a later pass finds nothing to re-put.
    #[tokio::test]
    async fn a_burnt_row_below_a_raised_slot_is_carried_and_the_healed_replica_converges() {
        let fx = fixture();
        let dir = store_dir_of(&fx, "a");
        let backup = fx.base.join("a-backup");

        let (first_writer, old_stamp, moderation_key) =
            a_life_that_puts_stamped(&fx, &moderation_value(&["before-the-backup"])).await;
        copy_dir_recursive(&dir, &backup);
        let (sync_prefs_coordinate, sync_prefs_key, old_sync) =
            the_old_life_saves_sync_prefs(&fx, &first_writer).await;
        assert_ne!(moderation_key, sync_prefs_key);

        std::fs::remove_dir_all(&dir).expect("drop the current journal");
        copy_dir_recursive(&backup, &dir);
        let burnt_value = moderation_value(&["the-burnt-life"]);
        let burnt_writer = a_burnt_life_journals_moderation_through(
            &dir,
            &fx,
            &first_writer,
            &old_stamp,
            &burnt_value,
            &moderation_key,
            sync_prefs_coordinate,
        )
        .await;
        // The slot raised past the never-accepted rows: what an
        // out-of-order inline publish would do (residue shape (i)).
        AccountStore::open(
            SqliteBackend::open(&dir).expect("reopen"),
            &fx.actor_hex,
            burnt_writer,
        )
        .await
        .expect("open")
        .advance_frontier(
            ACCOUNT_STATE_SCOPE,
            &burnt_writer,
            u64::try_from(sync_prefs_coordinate).unwrap(),
        )
        .await
        .expect("the slot is raised");

        let (successor, rows_after_carry) =
            heal_and_check_carried(&fx, &dir, &first_writer, &old_sync, &burnt_value, 1).await;

        // The relay plane at the shadowed coordinate serves the fleet's row,
        // not the burnt life's …
        let coordinate = u64::try_from(sync_prefs_coordinate).unwrap();
        assert_eq!(
            relay_item_keys_at(&dir, &burnt_writer, coordinate).await,
            vec![sync_prefs_key.clone()],
            "the burnt life's relay row at the coordinate is retired for the fleet's \
             (moderation key {:?}, sync-prefs key {:?}, feed {:?}, journal {:?})",
            &moderation_key[..2],
            &sync_prefs_key[..2],
            feed_dump(&fx, &first_writer),
            journal_rows_of(&dir, &burnt_writer)
                .await
                .iter()
                .map(|r| (r.seq, r.item.clone()))
                .collect::<Vec<_>>()
        );
        // … while the journal is left alone: the retired coordinate still
        // holds the burnt life's moderation row.
        let held = journal_rows_of(&dir, &burnt_writer)
            .await
            .into_iter()
            .find(|r| r.seq == coordinate)
            .expect("the retired coordinate is still held");
        assert!(
            matches!(&held.item, fauna_account_store::types::ItemRef::StateKey { kind, .. } if kind == KIND_MODERATION),
            "the burnt row is not rewritten: {held:?}"
        );
        check_carry_is_stable(&fx, &dir, &successor, rows_after_carry, 1).await;
    }

    /// **A refused row's relay residue — the burn** (`account-replica-posture.md`
    /// § The store device principal, refinement 11 → *a refused row's relay
    /// residue*). The backup-restore burnt life journals moderation up to the
    /// coordinate the old life spent on a sync-prefs save; the prologue's
    /// publish sends its rows in order and the nest — as a current nest does
    /// — refuses the reused coordinate `stale_writer_seq`. Before the heal
    /// runs, the relay row at that coordinate is already retired: the
    /// refusal is the nest's final word, and a peer walking this replica in
    /// the window before the rotation meets nothing there rather than the
    /// burnt life's row. After the heal, every relay row under the retired
    /// writer is one the nest serves at the same `(seq, item)` — a peer
    /// meets no row the nest refused — and the burnt value re-authored under
    /// the successor is on the relay plane as the successor's word.
    #[tokio::test]
    async fn a_refused_rows_relay_row_is_retired_and_a_peer_meets_only_what_the_nest_serves() {
        let fx = fixture();
        let dir = store_dir_of(&fx, "a");
        let backup = fx.base.join("a-backup");

        let (first_writer, old_stamp, moderation_key) =
            a_life_that_puts_stamped(&fx, &moderation_value(&["before-the-backup"])).await;
        copy_dir_recursive(&dir, &backup);
        let (sync_prefs_coordinate, sync_prefs_key, old_sync) =
            the_old_life_saves_sync_prefs(&fx, &first_writer).await;
        assert_ne!(moderation_key, sync_prefs_key);

        std::fs::remove_dir_all(&dir).expect("drop the current journal");
        copy_dir_recursive(&backup, &dir);
        let burnt_value = moderation_value(&["the-burnt-life"]);
        let burnt_writer = a_burnt_life_journals_moderation_through(
            &dir,
            &fx,
            &first_writer,
            &old_stamp,
            &burnt_value,
            &moderation_key,
            sync_prefs_coordinate,
        )
        .await;
        let coordinate = u64::try_from(sync_prefs_coordinate).unwrap();
        assert_eq!(
            relay_item_keys_at(&dir, &burnt_writer, coordinate).await,
            vec![moderation_key.clone()],
            "fixture: the relay row the plane records before a send is in place"
        );

        // The prologue publishes the burnt rows in order and the nest refuses
        // the first reused coordinate for good — the refusal arm retires that
        // relay row on the spot — then the walk meets the fleet's row under
        // another item and marks the burn. The window between the refusal
        // and the rotation is not observable through the runtime (the heal
        // rides the command queue a shutdown drains), so the arm itself is
        // pinned by the replay test, which shares it; here the assertions are
        // the peer-facing outcome once the heal has run.
        let (successor, rows_after_carry) =
            heal_and_check_carried(&fx, &dir, &first_writer, &old_sync, &burnt_value, 1).await;

        // What a peer walking this replica's relay plane meets under the
        // retired writer is exactly what the nest serves under it.
        let relay = relay_rows_under(&dir, &burnt_writer).await;
        let feed = feed_rows_under(&fx, &first_writer);
        assert_eq!(
            relay,
            feed,
            "a peer meets no row the nest refused (journal {:?})",
            journal_rows_of(&dir, &burnt_writer)
                .await
                .iter()
                .map(|r| (r.seq, r.item.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            relay_item_keys_at(&dir, &burnt_writer, coordinate).await,
            vec![sync_prefs_key.clone()],
            "at the refused coordinate the relay plane serves the fleet's item alone"
        );
        assert!(
            relay_rows_under(&dir, &successor)
                .await
                .iter()
                .any(|(_, item)| *item == moderation_key),
            "the burnt value re-authored under the successor is on the relay plane as the \
             successor's word"
        );
        check_carry_is_stable(&fx, &dir, &successor, rows_after_carry, 1).await;
    }

    /// A coordinate the lenient fake serves under two items, for good — the
    /// one-leg stand-in for a nest row plus a peer-relayed burnt row: what
    /// [`a_double_served_coordinate`] builds.
    struct DoubleServed {
        /// Device `a`'s store dir: the healed replica, whose retired burnt
        /// writer authored the second row.
        dir: PathBuf,
        /// The burnt life's writer, hex — the writer BOTH rows sit under.
        first_writer: String,
        burnt_writer: WriterId,
        /// The doubly-served `writer_seq`.
        coordinate: i64,
        /// The old life's sync-prefs value — the fleet's row at the coordinate.
        old_sync: fauna_account_store::types::StateEntry,
        /// The burnt life's moderation value — the other row there.
        burnt_value: Vec<u8>,
        moderation_key: Vec<u8>,
        sync_prefs_key: Vec<u8>,
        /// The healed replica's successor writer and its journal length
        /// after the carry — `check_carry_is_stable`'s baseline.
        successor: WriterId,
        rows_after_carry: usize,
    }

    /// Build the double-served coordinate: the backup-restore burnt life
    /// journals moderation at the coordinate the old life spent on a
    /// sync-prefs save, the lenient fake accepts the burnt row there under
    /// the other item, and the heal on device `a` carries both — leaving the
    /// fake serving two items at one `(scope, writer, seq)` under the retired
    /// writer, as a nest and a peer relaying the burnt row do between them.
    async fn a_double_served_coordinate(fx: &Fixture) -> DoubleServed {
        let dir = store_dir_of(fx, "a");
        let backup = fx.base.join("a-backup");

        let (first_writer, old_stamp, moderation_key) =
            a_life_that_puts_stamped(fx, &moderation_value(&["before-the-backup"])).await;
        copy_dir_recursive(&dir, &backup);
        // The old life's ONE save after the backup: its moderation head stays
        // below the sync-prefs coordinate, which is what lets the lenient fake
        // accept the burnt life's moderation row there.
        let (sync_prefs_coordinate, sync_prefs_key, old_sync) =
            the_old_life_saves_sync_prefs(fx, &first_writer).await;
        assert_ne!(moderation_key, sync_prefs_key);

        std::fs::remove_dir_all(&dir).expect("drop the current journal");
        copy_dir_recursive(&backup, &dir);
        let burnt_value = moderation_value(&["the-burnt-life"]);
        // Unsent: the slot stays at the backup's high-water, so the prologue's
        // publish_pending sends the burnt row — and the lenient fake accepts
        // it at the old life's coordinate, under the other item.
        let burnt_writer = a_burnt_life_journals_moderation_through(
            &dir,
            fx,
            &first_writer,
            &old_stamp,
            &burnt_value,
            &moderation_key,
            sync_prefs_coordinate,
        )
        .await;
        fx.fake
            .accept_reused_coordinates
            .store(true, Ordering::SeqCst);

        let (successor, rows_after_carry) =
            heal_and_check_carried(fx, &dir, &first_writer, &old_sync, &burnt_value, 2).await;
        let mut keys = feed_item_keys_at(fx, &first_writer, sync_prefs_coordinate);
        keys.sort();
        let mut both = vec![moderation_key.clone(), sync_prefs_key.clone()];
        both.sort();
        assert_eq!(
            keys,
            both,
            "fixture: the lenient nest serves both items at the coordinate (moderation key \
             {:?}, sync-prefs key {:?}, feed {:?}, journal {:?})",
            &both[0][..2],
            &both[1][..2],
            feed_dump(fx, &first_writer),
            journal_rows_of(&dir, &burnt_writer)
                .await
                .iter()
                .map(|r| (r.seq, r.item.clone()))
                .collect::<Vec<_>>()
        );
        DoubleServed {
            dir,
            first_writer,
            burnt_writer,
            coordinate: sync_prefs_coordinate,
            old_sync,
            burnt_value,
            moderation_key,
            sync_prefs_key,
            successor,
            rows_after_carry,
        }
    }

    /// **A double-accepted coordinate is carried stably** — a review
    /// correction on the change that ruled this arm: where the feed serves both rows at a reused coordinate (a
    /// peer still relaying the burnt life's row beside the nest's), a
    /// compact-and-ingest at a shadowed coordinate would re-put a fresh
    /// successor row on every walk.
    /// The carry writes nothing at the retired coordinate: after the first
    /// carried pass the healed journal is stable and no own row lands. The
    /// second replica's walk of such a coordinate is the sibling test below.
    #[tokio::test]
    async fn a_double_accepted_coordinate_is_carried_without_a_repeated_re_put() {
        let fx = fixture();
        let d = a_double_served_coordinate(&fx).await;
        let coordinate = u64::try_from(d.coordinate).unwrap();
        assert!(
            journal_rows_of(&d.dir, &d.burnt_writer)
                .await
                .iter()
                .any(|r| r.seq == coordinate),
            "the retired coordinate is still held"
        );
        check_carry_is_stable(&fx, &d.dir, &d.successor, d.rows_after_carry, 2).await;
    }

    /// **The double-served coordinate at every OTHER replica** (refinement
    /// 11 → *a foreign writer's second row at a held coordinate is carried*). A feed can serve two items at one
    /// `(scope, writer, seq)` for good — a nest's row plus a peer's relay of
    /// a burnt writer's row the nest refused — and every replica
    /// but the healed one meets them as a FOREIGN writer's rows: the first
    /// ingests, and the second used to abort the walk's page as journal
    /// equivocation. The incremental walk self-cleared (the first row's
    /// frontier advance gates both rows off the next page), but the full
    /// pass's zero-frontier reconcile met it again every pass, forever — an
    /// `errors` entry and no walk, per pass. Now the second row is carried:
    /// merged through the ordinary apply, journaled at no coordinate, counted
    /// [`WalkReport::double_served`], both relay rows kept (a peer is served
    /// what the nest serves), and the walk pages on past it.
    #[tokio::test]
    async fn a_second_replica_walks_a_double_served_coordinate_cleanly() {
        let fx = fixture();
        let d = a_double_served_coordinate(&fx).await;
        let first_writer =
            WriterId(fauna_core::hex32::decode(&d.first_writer).expect("writer hex"));
        let coordinate = u64::try_from(d.coordinate).unwrap();
        let dir_b = store_dir_of(&fx, "b");

        let other = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("a second replica assembles");
        let own_writer = WriterId(
            fauna_core::hex32::decode(&enrolled_writer_hex(&other).await).expect("writer hex"),
        );
        let mut own_rows_after_first_pass = None;
        for pass in 1..=3 {
            let report = other.reconcile_now().await.expect("pass");
            assert!(
                report.errors.is_empty(),
                "pass {pass}: a second replica walks the double-served coordinate cleanly: {:?}",
                report.errors
            );
            let walk = report.walk.expect("the account-state scope walked");
            assert_eq!(
                walk.own_burnt, 0,
                "pass {pass}: another replica's history is never 'burnt' here"
            );
            assert_eq!(
                walk.double_served, 1,
                "pass {pass}: the second row at the coordinate is carried, every pass: {walk:?}"
            );
            let own_rows = journal_rows_of(&dir_b, &own_writer).await.len();
            match own_rows_after_first_pass {
                None => own_rows_after_first_pass = Some(own_rows),
                Some(after_first) => assert_eq!(
                    own_rows, after_first,
                    "pass {pass}: the carry is idempotent — no repeated own row"
                ),
            }
        }

        // Both items' values converge on what the fleet holds.
        assert_eq!(
            other
                .get_preference(KIND_SYNC_PREFS)
                .await
                .expect("get")
                .expect("sync prefs reached the second replica")
                .value,
            d.old_sync.value,
            "the fleet's sync-prefs row at the coordinate is applied"
        );
        assert_eq!(
            other
                .get_preference(KIND_MODERATION)
                .await
                .expect("get")
                .expect("moderation reached the second replica")
                .value,
            d.burnt_value,
            "the burnt life's moderation row at the same coordinate is carried"
        );
        other.shutdown().await;

        // The journal holds ONE row at the coordinate — the first served —
        // and nothing was written there for the second.
        let at_coordinate: Vec<_> = journal_rows_of(&dir_b, &first_writer)
            .await
            .into_iter()
            .filter(|r| r.seq == coordinate)
            .collect();
        assert_eq!(
            at_coordinate.len(),
            1,
            "one journal row at the double-served coordinate: {at_coordinate:?}"
        );
        // The relay plane keeps BOTH rows: a peer is served exactly what the
        // nest serves, and meets the same shape with the same arm.
        let mut relayed = relay_item_keys_at(&dir_b, &first_writer, coordinate).await;
        relayed.sort();
        let mut both = vec![d.moderation_key.clone(), d.sync_prefs_key.clone()];
        both.sort();
        assert_eq!(relayed, both, "both duplicates stay relayable onward");
        // The walk paged past the coordinate: the healed replica's successor
        // rows, served after the duplicates, landed as ordinary foreign rows.
        assert!(
            !journal_rows_of(&dir_b, &d.successor).await.is_empty(),
            "the walk kept paging past the double-served coordinate"
        );
    }

    /// **The burnt-journal residue, freed (refinement 11).** The sibling of
    /// the backup-restore heal above where the burnt life WROTE: offline, it
    /// journaled a moderation value at the very coordinate the pre-backup
    /// life had used for a DIFFERENT key (a sync-prefs save), a key that life
    /// also wrote. Once the heal retires the writer, the walk treats its rows
    /// at or below what the journal holds as this machine's own echoes, so
    /// the burnt row would shadow the fleet's sync-prefs row for good. The
    /// re-author carries the burnt value to the successor and compacts the
    /// retired writer's never-published rows, which frees the coordinate:
    /// the fleet's row walks in as retired history, and both replicas agree
    /// on both keys.
    #[tokio::test]
    async fn a_burnt_lifes_row_at_a_coordinate_the_old_life_used_for_another_key_is_compacted_and_both_replicas_converge()
     {
        let fx = fixture();
        let dir = store_dir_of(&fx, "a");
        let backup = fx.base.join("a-backup");

        let first_writer = a_life_that_puts(&fx, &moderation_value(&["before-the-backup"])).await;
        copy_dir_recursive(&dir, &backup);

        // The pre-backup life goes on: a sync-prefs save, then a moderation put.
        let (sync_prefs_coordinate, old_stamp) = {
            let handle = AccountStoreRuntime::start(fx.params("a"))
                .await
                .expect("assemble");
            let report = handle.reconcile_now().await.expect("pass");
            assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
            let before = state_coordinates_of(&fx, &first_writer);
            crate::preference_surfaces::save_sync_prefs(&handle, Some("latest_wins_always"))
                .await
                .expect("the sync-prefs save lands");
            handle.settled().await;
            let fresh: Vec<i64> = state_coordinates_of(&fx, &first_writer)
                .difference(&before)
                .copied()
                .collect();
            assert_eq!(
                fresh.len(),
                1,
                "fixture: the save published one row: {fresh:?}"
            );
            let stamp = handle
                .put_preference(KIND_MODERATION, moderation_value(&["the-old-life"]))
                .await
                .expect("the moderation put lands");
            // Its publish step runs before the shutdown is served.
            handle.settled().await;
            assert_eq!(enrolled_writer_hex(&handle).await, first_writer);
            handle.shutdown().await;
            (fresh[0], stamp)
        };

        // The restore, then the burnt life, offline: it journals moderation
        // under the surviving key until it reaches the coordinate the old
        // life spent on sync prefs. Its stamp is later than the old life's
        // moderation stamp, so its value wins everywhere once it is carried.
        std::fs::remove_dir_all(&dir).expect("drop the current journal");
        copy_dir_recursive(&backup, &dir);
        let burnt_value = moderation_value(&["the-burnt-life"]);
        let burnt_writer = {
            let backend = SqliteBackend::open(&dir).expect("open the restored backend");
            let writer = fauna_account_store::store::stamped_writer(&backend)
                .await
                .expect("meta")
                .expect("the restored store is stamped");
            assert_eq!(writer.to_hex(), first_writer);
            let store = AccountStore::open(backend, &fx.actor_hex, writer)
                .await
                .expect("open as the surviving writer");
            let merge_meta = LwwStamp {
                at_ms: old_stamp.at_ms + 1,
                writer: writer.0,
            }
            .encode()
            .expect("stamp");
            let mut seq = 0;
            while i64::try_from(seq).unwrap() < sync_prefs_coordinate {
                seq = store
                    .put_state(StateEntry {
                        kind: KIND_MODERATION.into(),
                        key: PREFERENCE_KEY.into(),
                        scope: ACCOUNT_STATE_SCOPE.into(),
                        value: burnt_value.clone(),
                        merge_meta: Some(merge_meta.clone()),
                        entry_version: 0,
                        tombstone: false,
                    })
                    .await
                    .expect("the burnt life journals")
                    .1;
            }
            assert_eq!(
                i64::try_from(seq).unwrap(),
                sync_prefs_coordinate,
                "fixture: the burnt life's row sits at the old life's sync-prefs coordinate"
            );
            writer
        };

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the restored store assembles");
        // Detect, heal, then one pass on the healed writer that walks it all.
        handle.reconcile_now().await.expect("pass");
        handle.reconcile_now().await.expect("pass");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        assert_ne!(
            enrolled_writer_hex(&handle).await,
            first_writer,
            "the burnt writer is rotated away"
        );
        assert_eq!(
            report
                .walk
                .expect("the account-state scope walked")
                .own_burnt,
            0,
            "the healed writer's walk finds nothing burnt"
        );
        // The views alone cannot tell whether the compaction ran; the
        // coordinate check at the end is the discriminating one.
        let healed_sync = handle
            .get_preference(KIND_SYNC_PREFS)
            .await
            .expect("get")
            .expect("sync prefs are present at the healed replica");
        let healed_moderation = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("moderation is present");
        assert_eq!(
            healed_moderation.value, burnt_value,
            "the burnt life's later write survives the heal"
        );
        handle.shutdown().await;

        let other = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("a second replica assembles");
        let report = other.reconcile_now().await.expect("pass");
        assert!(
            report.errors.is_empty(),
            "the second replica walks every life cleanly: {:?}",
            report.errors
        );
        let other_sync = other
            .get_preference(KIND_SYNC_PREFS)
            .await
            .expect("get")
            .expect("the second replica holds sync prefs");
        let other_moderation = other
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the second replica holds moderation");
        assert_eq!(healed_sync.value, other_sync.value, "sync prefs converge");
        assert_eq!(
            healed_moderation.value, other_moderation.value,
            "moderation converges"
        );
        other.shutdown().await;

        // No retired-writer row lands at a coordinate the healed journal holds
        // under a different item: every row of the burnt writer the second
        // replica ingested off the feed sits at the same item there.
        let rows_under = |store_dir: PathBuf| async move {
            SqliteBackend::open(&store_dir)
                .expect("reopen")
                .rows_for_scope(ACCOUNT_STATE_SCOPE, &burnt_writer, 0, u32::MAX)
                .await
                .expect("rows")
        };
        let item = |row: &fauna_account_store::types::JournalRow| match &row.item {
            fauna_account_store::types::ItemRef::StateKey { kind, key, .. } => {
                Some((kind.clone(), key.clone()))
            }
            fauna_account_store::types::ItemRef::Cid(_) => None,
        };
        let healed_rows = rows_under(dir.clone()).await;
        let fleet_rows = rows_under(store_dir_of(&fx, "b")).await;
        assert!(
            fleet_rows
                .iter()
                .any(|r| i64::try_from(r.seq).unwrap() == sync_prefs_coordinate),
            "fixture: the fleet serves the old life's sync-prefs row"
        );
        for fleet in &fleet_rows {
            let healed = healed_rows.iter().find(|r| r.seq == fleet.seq);
            assert_eq!(
                healed.map(item),
                Some(item(fleet)),
                "the healed journal holds seq {} of the retired writer under a different \
                 item than the fleet, or not at all",
                fleet.seq
            );
        }
    }

    /// **The lost-slot self-heal (charter § The store device principal →
    /// succession, the lost-slot trigger).** A slot lost while the store dir
    /// survives used to strand the machine silently: the next assembly
    /// minted a fresh writer into the empty slot, the store's identity check
    /// refused it ("belongs to a different writer"), and the app ran with no
    /// account runtime and nothing on screen saying so — every later
    /// launch found the slot FULL of that doomed key, so the strand was
    /// permanent. The heal is the BUILT succession rotation fired on the
    /// shape itself: the fresh key becomes the successor (fence: the old
    /// writer retired, the pending-re-author marker stamped), the store
    /// opens, and the pump's tail walk re-authors the un-pushed row under
    /// the successor — nothing deleted, no UI, no seed needed for the fence.
    #[tokio::test]
    async fn a_lost_slot_over_a_stamped_store_self_heals_into_a_successor_with_its_unpushed_tail() {
        let fx = fixture();
        let value = moderation_value(&["survives-the-lost-slot"]);
        let (old_writer, _) = enroll_with_an_unpushed_row(&fx, &value).await;
        lose_the_slot(&fx);
        let state_puts_before = fx.fake.put_calls_for("state");

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the next assembly must heal the lost slot, never strand");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let new_writer = enrolled_writer_hex(&handle).await;
        assert_ne!(
            new_writer, old_writer,
            "the key minted into the empty slot is the SUCCESSOR — the old writer is retired, \
             never resurrected"
        );
        // The un-pushed row survived locally…
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the tail survived");
        assert_eq!(entry.value, value, "the no-row-loss proof");
        // …and reached the nest re-authored under the successor.
        assert!(
            fx.fake.put_calls_for("state") > state_puts_before,
            "the re-authored tail must publish after the heal"
        );
        {
            let s = fx.fake.state.lock().unwrap();
            assert!(
                s.feed
                    .iter()
                    .any(|r| r.scope == "state" && r.writer_id == new_writer),
                "the nest must hold the re-authored row under the successor writer"
            );
            assert!(
                s.grant_keys.contains(&new_writer),
                "the successor's grant registered: {:?}",
                s.grant_keys
            );
        }
        handle.shutdown().await;
    }

    /// An install a pre-heal build already stranded — the doomed fresh
    /// writer sits in the slot, the store still names the old one — is the
    /// same shape one launch later, and it is what every machine that hit
    /// the strand before the heal shipped looks like today. The heal adopts
    /// the slot's key as the successor WITHOUT minting a third identity: the
    /// fence retires the stamped writer in favour of the key the slot holds.
    #[tokio::test]
    async fn an_install_already_stranded_by_a_doomed_fresh_writer_heals_at_the_next_assembly() {
        let fx = fixture();
        let value = moderation_value(&["survives-the-strand"]);
        let (old_writer, _) = enroll_with_an_unpushed_row(&fx, &value).await;
        lose_the_slot(&fx);
        // The pre-heal launch: a fresh writer minted into the empty slot (the
        // pre-assembly resolver every app host runs first), then the store
        // refused it and the machine ran stranded. Reproduced through the
        // production resolver, not by hand-writing the slot.
        let doomed = {
            let creds = CredentialStore::with_file_backend(CRED_NAMESPACE, fx.cred_dir.join("a"));
            let key = resolve_writer_key_serialized(
                &StoreRoot::at(fx.base.join("a")),
                &fx.actor_hex,
                &creds,
            )
            .expect("the mint into an empty slot");
            fauna_core::hex32::encode(&key.verifying_key().to_bytes())
        };
        assert_ne!(doomed, old_writer);
        let state_puts_before = fx.fake.put_calls_for("state");

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the stranded install must heal at the next assembly");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        assert_eq!(
            enrolled_writer_hex(&handle).await,
            doomed,
            "the slot's key IS the successor — no third identity is minted"
        );
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the tail survived the strand");
        assert_eq!(entry.value, value);
        assert!(fx.fake.put_calls_for("state") > state_puts_before);
        assert!(
            fx.fake
                .state
                .lock()
                .unwrap()
                .feed
                .iter()
                .any(|r| r.scope == "state" && r.writer_id == doomed),
            "the re-authored row publishes under the adopted successor"
        );
        handle.shutdown().await;
    }

    /// A slot restored to a RETIRED writer (a credential backup older than a
    /// rotation, landed over a current store dir) must never put the dead
    /// key back to work — the ruling's "abandoned, never re-used", and the
    /// nest may hold its tombstone. The heal mints a fresh successor into
    /// the slot instead and fences the store onto it.
    #[tokio::test]
    async fn a_slot_restored_to_a_retired_writer_is_never_reused_the_heal_mints_a_fresh_successor()
    {
        let fx = fixture();
        let value = moderation_value(&["outlives-two-rotations"]);
        let (old_writer, old_secret_hex) = enroll_with_an_unpushed_row(&fx, &value).await;
        lose_the_slot(&fx);
        // The first heal: the old writer is retired, the slot holds successor X.
        let successor = {
            let handle = AccountStoreRuntime::start(fx.params("a"))
                .await
                .expect("the lost-slot heal");
            handle.reconcile_now().await.expect("pass");
            let hex = enrolled_writer_hex(&handle).await;
            handle.shutdown().await;
            hex
        };
        assert_ne!(successor, old_writer);
        // The old backup lands: the slot names the RETIRED writer again.
        CredentialStore::with_file_backend(CRED_NAMESPACE, fx.cred_dir.join("a"))
            .set(&fx.actor_hex, &old_secret_hex);

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("a retired key in the slot heals by a FRESH mint");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let fresh = enrolled_writer_hex(&handle).await;
        assert_ne!(
            fresh, old_writer,
            "the retired writer is never put back to work"
        );
        assert_ne!(fresh, successor, "the store moved on to a fresh successor");
        let entry = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("the row is still here");
        assert_eq!(entry.value, value);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn two_devices_converge_through_the_pump() {
        let fx = fixture();
        let a = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start a");
        let b = AccountStoreRuntime::start(fx.params("b"))
            .await
            .expect("start b");

        let value = moderation_value(&["spoilers", "endgame"]);
        a.put_preference(KIND_MODERATION, value.clone())
            .await
            .expect("put");

        let report = b.reconcile_now().await.expect("reconcile");
        assert!(report.errors.is_empty(), "clean pass: {:?}", report.errors);
        let entry = b
            .get_preference(KIND_MODERATION)
            .await
            .expect("get")
            .expect("b sees a's value");
        assert_eq!(entry.value, value);
        a.shutdown().await;
        b.shutdown().await;
    }

    #[tokio::test]
    async fn failed_publish_is_recovered_by_the_next_pass() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        // The pass barrier: the prologue pass publishes this device's reach
        // row, and the injected failure must land on the preference put's
        // publish step below, not on that.
        handle.settled().await;

        fx.fake.fail_next_put.store(true, Ordering::SeqCst);
        // The write is the local row: it answers, and the publish step that
        // follows it meets the injected failure (a network failure is never
        // the write's answer — the pump bullet's wake source (4)).
        handle
            .put_preference(KIND_MODERATION, moderation_value(&["w"]))
            .await
            .expect("the local write lands");
        handle.settled().await;
        assert!(
            !fx.fake.fail_next_put.load(Ordering::SeqCst),
            "the publish step ran and met the injected failure"
        );

        // The local write landed first (the plane's law) …
        assert!(
            handle
                .get_preference(KIND_MODERATION)
                .await
                .expect("get")
                .is_some(),
            "local row survived the failed publish"
        );
        // … and the next full pass replays it.
        let before = fx.fake.put_calls();
        let report = handle.reconcile_now().await.expect("reconcile");
        assert_eq!(
            report.published,
            Some(1),
            "publish_pending replayed the row"
        );
        assert!(fx.fake.put_calls() > before);
        handle.shutdown().await;
        // The outage law: the relay row recorded before the failed send was
        // never retired (a transport fault is not a refusal), and the replay
        // re-sealed onto the same coordinate — one live relay row.
        let dir = store_dir_of(&fx, "a");
        let writer =
            fauna_account_store::store::stamped_writer(&SqliteBackend::open(&dir).unwrap())
                .await
                .expect("meta")
                .expect("stamped");
        assert_eq!(
            relay_rows_under(&dir, &writer).await.len(),
            1,
            "an outage-pending row keeps its relay row"
        );
    }

    /// Every account-state relay row a store dir holds under `writer`, as
    /// `(writer_seq, item_key)` — what a peer walking this replica's relay
    /// plane is served.
    async fn relay_rows_under(dir: &std::path::Path, writer: &WriterId) -> Vec<(u64, Vec<u8>)> {
        let mut rows: Vec<(u64, Vec<u8>)> = SqliteBackend::open(dir)
            .expect("reopen")
            .relay_rows(
                ACCOUNT_STATE_SCOPE,
                ItemClass::StateEntry.as_wire(),
                &[],
                u32::MAX,
            )
            .await
            .expect("relay rows")
            .into_iter()
            .filter(|r| r.writer == *writer)
            .map(|r| (r.writer_seq, r.item_key))
            .collect();
        rows.sort();
        rows
    }

    /// Every account-state feed row the fake serves under `writer`, as
    /// `(writer_seq, item_key)` — what the nest holds live.
    fn feed_rows_under(fx: &Fixture, writer: &str) -> Vec<(u64, Vec<u8>)> {
        let mut rows: Vec<(u64, Vec<u8>)> = fx
            .fake
            .state
            .lock()
            .unwrap()
            .feed
            .iter()
            .filter(|r| r.scope == ACCOUNT_STATE_SCOPE && r.writer_id == writer)
            .map(|r| (u64::try_from(r.writer_seq).unwrap(), r.item_key.clone()))
            .collect();
        rows.sort();
        rows
    }

    /// **A refused row's relay residue — the replay** (`account-replica-posture.md`
    /// § The store device principal, refinement 11 → *a refused row's relay
    /// residue*). The nest recorded a put but its reply was lost, so the put
    /// failed as a transport fault and the relay row recorded before the
    /// send stayed (the outage law). The next own write re-sends that row
    /// first (the ordered own publish) and the nest refuses the replay
    /// `stale_writer_seq` — its final word on the coordinate — which retires
    /// the relay row: at that moment a peer is served nothing for the item
    /// rather than a row this replica cannot vouch for. The walk then serves
    /// the row back (it sits above the un-advanced slot): the own echo is the
    /// proof the nest holds it, re-records the relay row from the served
    /// envelope and advances the slot, and the write behind it publishes.
    #[tokio::test]
    async fn a_lost_reply_is_refused_as_a_replay_and_the_own_echo_restores_the_relay_row() {
        let fx = fixture();
        let dir = store_dir_of(&fx, "a");
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        let writer_hex = enrolled_writer_hex(&handle).await;
        // The prologue behind us before the fake is armed: a local read is
        // served inside it, and the flag would otherwise take the prologue's
        // next fleet-scope put rather than the moderation row below.
        handle.settled().await;

        fx.fake.lose_next_put_reply.store(true, Ordering::SeqCst);
        let moderation = moderation_value(&["held-by-the-nest"]);
        // The write is the local row; its publish step meets the lost reply
        // (a transport fault is never the write's answer).
        handle
            .put_preference(KIND_MODERATION, moderation.clone())
            .await
            .expect("the local write lands");
        handle.settled().await;
        let feed = feed_rows_under(&fx, &writer_hex);
        assert_eq!(
            feed.len(),
            1,
            "fixture: the nest recorded the row: {feed:?}"
        );
        let (coordinate, moderation_key) = feed[0].clone();

        // A second item's write lands locally, and its publish step re-sends
        // the moderation row ahead of it (journal order) and meets the replay
        // refusal — the nest's final word on the coordinate: its own row is
        // left unsent behind it, and the moderation relay row is retired
        // (asserted below, once the store is closed).
        crate::preference_surfaces::save_sync_prefs(&handle, Some("auto"))
            .await
            .expect("the second local write lands");
        handle.settled().await;
        handle.shutdown().await;
        let writer = WriterId(fauna_core::hex32::decode(&writer_hex).unwrap());
        assert_eq!(
            relay_rows_under(&dir, &writer).await,
            Vec::<(u64, Vec<u8>)>::new(),
            "the refused replay's relay row is retired (and the row behind it was never sent)"
        );

        // The next assembly's prologue re-sends (refused again, nothing left
        // to retire), then walks: the own echo restores the relay row and
        // advances the slot, and the pass after publishes the sync-prefs row.
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("reassemble");
        let report = handle.reconcile_now().await.expect("pass");
        assert!(
            report.errors.is_empty(),
            "a clean pass once the echo settled the replay: {:?}",
            report.errors
        );
        assert_eq!(
            handle
                .get_preference(KIND_MODERATION)
                .await
                .expect("get")
                .expect("present")
                .value,
            moderation,
            "the local row was never touched"
        );
        handle.shutdown().await;
        let relay = relay_rows_under(&dir, &writer).await;
        assert_eq!(
            relay,
            feed_rows_under(&fx, &writer_hex),
            "the relay plane holds exactly what the nest holds: the echoed moderation row \
             and the sync-prefs row the unblocked pass published"
        );
        assert!(
            relay.contains(&(coordinate, moderation_key)),
            "the echo re-recorded the moderation row at its coordinate: {relay:?}"
        );
    }

    /// **Own rows publish in journal order** (`account-replica-posture.md`
    /// § The store device principal, refinement 11 → *the ordered own
    /// publish*). An inline put used to seal and send only its own row, and
    /// the own frontier slot is MAX-merge: a put accepted while an earlier
    /// own row was still unsent raised the slot past that row, and
    /// `publish_pending` — which scans above the slot — never sent it.
    /// Reachable on every reconnect (the pump serves queued commands before
    /// the reconnect pass), and the very mechanism behind refinement 11's
    /// residue (i). A put's publish step now publishes every unsent own row
    /// before its own, so the slot is always the contiguous attempted
    /// prefix. The step runs right after the put answers, so the nest is
    /// read behind the pass barrier (`settled`).
    #[tokio::test]
    async fn an_inline_put_publishes_the_unsent_own_rows_before_its_own() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        let report = handle.reconcile_now().await.expect("pass");
        assert_eq!(report.enrollment, Some(EnrollmentPass::Current));
        let writer = enrolled_writer_hex(&handle).await;
        let before = state_coordinates_of(&fx, &writer);

        // The unsent row: the local row is durable, and its publish step's
        // leg fails.
        fx.fake.fail_next_put.store(true, Ordering::SeqCst);
        handle
            .put_preference(KIND_MODERATION, moderation_value(&["unsent"]))
            .await
            .expect("the local write lands");
        handle.settled().await;
        // The next put is accepted — and carries the unsent row before itself.
        crate::preference_surfaces::save_sync_prefs(&handle, Some("latest_wins_always"))
            .await
            .expect("the save lands");
        handle.settled().await;
        let fresh: Vec<i64> = state_coordinates_of(&fx, &writer)
            .difference(&before)
            .copied()
            .collect();
        assert_eq!(
            fresh.len(),
            2,
            "both own rows reached the nest, the unsent one first: {fresh:?}"
        );
        assert_eq!(
            handle.reconcile_now().await.expect("pass").published,
            Some(0),
            "nothing was stranded below the slot for the pass to replay"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn nudges_coalesce_to_one_walk() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        // Quiesce: one barrier pass so the prologue's calls are behind us.
        handle.reconcile_now().await.expect("barrier");

        // Counted per scope: a nudge for the account-state scope walks exactly
        // that scope, while the closing barrier is a full pass (state + every
        // derived content scope), so a bare list-call total would fold the
        // barrier's content walks into the number under test.
        let before = fx.fake.list_calls_for(ACCOUNT_STATE_SCOPE);
        for _ in 0..5 {
            handle.nudge_scope(ACCOUNT_STATE_SCOPE);
        }
        // The burst lands as one drained batch → one walk (one list call,
        // empty feed = single page). Eventually-assert, then a barrier, then
        // the exact count.
        eventually(
            || fx.fake.list_calls_for(ACCOUNT_STATE_SCOPE) > before,
            "nudge walk ran",
        )
        .await;
        let report = handle.reconcile_now().await.expect("barrier");
        assert!(report.errors.is_empty());
        // Minus the barrier's own state walk.
        let nudge_walks = fx.fake.list_calls_for(ACCOUNT_STATE_SCOPE) - before - 1;
        assert_eq!(nudge_walks, 1, "five nudges coalesced into one walk");
        handle.shutdown().await;
    }

    fn sync_changed(folder: &str, scope: Option<&str>) -> PushEvent {
        PushEvent::SyncChanged(fauna_protocol::push_events::SyncChangedPayload {
            folder: folder.into(),
            scope: scope.map(str::to_string),
            ..Default::default()
        })
    }

    /// The one push→nudge mapping: a scope tag wakes its scope; every other
    /// push wakes nothing.
    #[test]
    fn nudge_scope_for_push_maps_the_tag_then_nothing() {
        assert_eq!(
            nudge_scope_for_push(&sync_changed("__state", Some(ACCOUNT_STATE_SCOPE))),
            Some(ACCOUNT_STATE_SCOPE)
        );
        assert_eq!(
            nudge_scope_for_push(&sync_changed("__state", Some(ACCOUNT_STATE_FLEET_SCOPE))),
            Some(ACCOUNT_STATE_FLEET_SCOPE)
        );
        assert_eq!(
            nudge_scope_for_push(&sync_changed("Holiday Photos", None)),
            None,
            "a folder nudge is the sync engine's, not a plane scope"
        );
        assert_eq!(
            nudge_scope_for_push(&PushEvent::ResyncRequired(
                fauna_protocol::push_events::ResyncRequiredPayload {
                    dropped_count: 1,
                    extra: Default::default(),
                }
            )),
            None
        );
    }

    /// The pump's own push arm: a `fauna.sync.changed` on the session's push
    /// stream walks the scope it maps to, with no app glue in between — and a
    /// push that maps to nothing walks nothing.
    #[tokio::test]
    async fn the_push_arm_feeds_the_nudge_channel() {
        let fx = fixture();
        let (push_tx, push_rx) = broadcast::channel::<PushEvent>(8);
        let mut params = fx.params("a");
        params.pushes = Some(push_rx);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("barrier");

        // A folder nudge wakes nothing: no walk, and the arm stays armed.
        let before = fx.fake.list_calls_for(ACCOUNT_STATE_SCOPE);
        push_tx
            .send(sync_changed("Holiday Photos", None))
            .expect("subscriber alive");

        // The scope-tagged form walks its scope.
        push_tx
            .send(sync_changed("__state", Some(ACCOUNT_STATE_SCOPE)))
            .expect("subscriber alive");
        eventually(
            || fx.fake.list_calls_for(ACCOUNT_STATE_SCOPE) > before,
            "the scope-tagged push walked its scope",
        )
        .await;
        let report = handle.reconcile_now().await.expect("barrier");
        assert!(report.errors.is_empty());
        // Exactly one nudge walk (minus the barrier's own): the folder push
        // contributed none.
        assert_eq!(
            fx.fake.list_calls_for(ACCOUNT_STATE_SCOPE) - before - 1,
            1,
            "one walk for the tagged push, none for the folder push"
        );

        // The broker going away disarms the arm and leaves the pump serving.
        drop(push_tx);
        let report = handle.reconcile_now().await.expect("pump still serves");
        assert!(report.errors.is_empty());
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn poisoned_pump_pass_is_contained() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        // Quiesce past the prologue so the flag poisons the intended pass.
        handle.reconcile_now().await.expect("barrier");

        fx.fake.panic_next_list.store(true, Ordering::SeqCst);
        let report = handle.reconcile_now().await.expect("runtime survives");
        assert!(
            report.errors.iter().any(|e| e.contains("panicked")),
            "the poisoned pass is reported: {:?}",
            report.errors
        );
        // The thread lives; the next pass is clean.
        let report = handle.reconcile_now().await.expect("still serving");
        assert!(
            report.errors.is_empty(),
            "clean after poison: {:?}",
            report.errors
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn backstop_ticker_pumps_without_a_nudge() {
        let fx = fixture();
        let mut params = fx.params("a");
        params.backstop_interval = Duration::from_millis(50);
        let handle = AccountStoreRuntime::start(params).await.expect("start");

        let before = fx.fake.list_calls();
        eventually(|| fx.fake.list_calls() > before, "ticker pumped").await;
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn reconnect_bump_pumps() {
        let fx = fixture();
        let (tx, rx) = watch::channel(0u64);
        let mut params = fx.params("a");
        params.reconnects = Some(rx);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("barrier");

        let before = fx.fake.list_calls();
        tx.send(1).expect("bump");
        eventually(|| fx.fake.list_calls() > before, "reconnect pumped").await;
        handle.shutdown().await;
    }

    /// **A first walk that failed on the data client's drop is retried on
    /// that client's reconnect, not at the backstop**
    /// (`account-client-lifecycle.md` § The client-side lifecycle (W3), the
    /// pump's reconnect wake). The runtime's data path rides the store
    /// principal's own client, which can drop and reconnect while the app
    /// session stays up; its reconnect must wake the pump too, or every gated
    /// read stays refused as not ready for the whole backstop interval.
    ///
    /// The session's watch never moves here; only the data client's does.
    #[tokio::test]
    async fn a_failed_first_walk_recovers_on_the_data_clients_reconnect() {
        use crate::preference_surfaces::load_muted_words;

        let fx = fixture();
        let (_session_tx, session_rx) = watch::channel(0u64);
        let (data_tx, data_rx) = watch::channel(0u64);
        let mut params = fx.params("b");
        params.backstop_interval = DEFAULT_BACKSTOP_INTERVAL;
        params.reconnects = Some(merge_reconnect_watches(session_rx, data_rx));

        fx.fake.walks_unreachable.store(true, Ordering::SeqCst);
        let b = AccountStoreRuntime::start(params).await.expect("start b");
        eventually(|| b.pump_cycles().1 > 0, "b's first pass ended").await;
        assert!(
            refused_as_not_listed(&load_muted_words(&b).await),
            "the failed walk left the scope unlisted"
        );

        // The data client is back; the session never dropped.
        fx.fake.walks_unreachable.store(false, Ordering::SeqCst);
        data_tx.send_replace(1); // the data client reconnects
        let answered = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let read = load_muted_words(&b).await;
                if refused_as_not_listed(&read) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                break read.expect("the read answers once the scope is listed");
            }
        })
        .await;
        assert!(
            answered.is_ok(),
            "a gated read answers within seconds of the data client's reconnect, \
             not at the {DEFAULT_BACKSTOP_INTERVAL:?} backstop"
        );
        b.shutdown().await;
    }

    #[tokio::test]
    async fn writer_key_is_stable_across_restarts_and_a_corrupt_slot_refuses() {
        let fx = fixture();
        // First start mints; a put pins the writer id into the store + feed.
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("first start");
        handle
            .put_preference(KIND_MODERATION, moderation_value(&["w"]))
            .await
            .expect("put");
        handle.shutdown().await;

        // Second start loads the same key — a re-mint would be refused by the
        // store's identity check, so plain success proves stability.
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("second start");
        assert!(
            handle
                .get_preference(KIND_MODERATION)
                .await
                .expect("get")
                .is_some()
        );
        handle.shutdown().await;

        // A corrupt slot is a hard, named error — never a silent re-mint.
        let params = fx.params("a");
        params.credentials.set(&fx.actor_hex, "not hex at all");
        let err = AccountStoreRuntime::start(params)
            .await
            .expect_err("refuses");
        assert!(
            err.to_string().contains("unreadable writer key"),
            "err names the slot: {err:#}"
        );
    }

    /// ** leg 2 — a Seedless assembly over an empty slot mints
    /// nothing, even though it still refuses.** Before this fix, `start`
    /// called the mint-or-load `writer_key_from_slot` unconditionally, ahead
    /// of the seedless backup-key refusal a few lines later — so a Seedless
    /// host with no enrolled principal would mint a fresh writer identity
    /// into the slot and THEN fail, leaving that orphaned identity behind
    /// (one no nest has ever granted, and a second writer for a store no app
    /// has enrolled — the exact divergence W5.3 measured, reached one layer
    /// earlier). The assertion covers the WHOLE assembly, `start` included,
    /// not just the internal resolver, because the orphaned write is a
    /// `start`-level side effect.
    ///
    /// Mutation: restore the unconditional `writer_key_from_slot(&credentials,
    /// &actor_id_hex)?` in place of the `principal.keypair()` branch and this
    /// reds — the slot gains a writer key even though assembly still fails.
    #[tokio::test]
    async fn a_seedless_assembly_over_an_empty_slot_mints_nothing() {
        let fx = fixture();
        let mut params = fx.params("a");
        params.principal = RuntimePrincipal::Seedless;
        let actor_hex = fx.actor_hex.clone();
        let cred_dir = fx.cred_dir.join("a");

        let err = AccountStoreRuntime::start(params)
            .await
            .expect_err("a seedless host with no enrolled principal must refuse");
        assert!(
            err.to_string().contains("found no writer key"),
            "the seedless refusal must name the missing WRITER KEY — reaching the \
             pre-existing backup-key refusal instead would mean the mint above it \
             still ran first: {err:#}"
        );

        let check = CredentialStore::with_file_backend(CRED_NAMESPACE, cred_dir);
        assert!(
            check.get(&actor_hex).is_none(),
            "a refused seedless assembly must leave the slot exactly as empty as it \
             found it — no orphaned writer identity"
        );
    }

    /// The own-actor half of the scope-set derivation, end-to-end through the
    /// real runtime: a started runtime walks this account's own-actor content
    /// scopes without any app registering a thing (charter § Feeds and cursors
    /// → *Scope partition*: "own-actor scopes (mail, calendar, card, own
    /// posts)"). Before the derivation the pilot registered none, so the pump's
    /// content-walk list was empty on every pass.
    #[tokio::test]
    async fn a_started_runtime_walks_its_own_actor_content_scopes() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        let report = handle.reconcile_now().await.expect("reconcile");
        let walked: Vec<&str> = report
            .content_walks
            .iter()
            .map(|w| w.scope.as_str())
            .collect();
        assert_eq!(
            walked.len(),
            OWN_ACTOR_KINDS.len(),
            "one walk per own-actor kind, got {walked:?}"
        );

        // Each is the canonical string for (kind, this account's actor) — the
        // grammar's own constructor, so the assertion cannot drift from it.
        let actor = fauna_core::hex32::decode(&fx.actor_hex).expect("actor bytes");
        for kind in OWN_ACTOR_KINDS {
            let want = ContentScope::new(kind, actor).expect("scope").to_string();
            assert!(
                walked.contains(&want.as_str()),
                "{want} walked, got {walked:?}"
            );
        }
        assert!(
            report.errors.is_empty(),
            "a clean pass over the derived set: {:?}",
            report.errors
        );
        handle.shutdown().await;
    }

    /// The auto-in-set producer (charter § The replica boundary — R1 + the T1
    /// producer decomposition): every item of an own-actor scope is creation-
    /// or delivery-class, in-set with **no render event**, so a pump pass that
    /// walked the scope raises the scope's seen-set watermark to the accounted
    /// frontier and publishes the entry.
    #[tokio::test]
    async fn a_pump_pass_records_own_actor_deliveries_in_the_seen_set() {
        let fx = fixture();
        // Two mail records the nest already sequenced (delivery happened
        // nest-side) — staged before the runtime's first pass.
        let actor = fauna_core::hex32::decode(&fx.actor_hex).expect("actor bytes");
        let mail_scope = ContentScope::new("mail", actor).expect("scope").to_string();
        fx.fake.stage_record(&mail_scope, 1, [0xAB; 32]);
        fx.fake.stage_record(&mail_scope, 2, [0xCD; 32]);

        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        let report = handle.reconcile_now().await.expect("pass");
        assert!(report.errors.is_empty(), "clean pass: {:?}", report.errors);

        // The producer wrote the scope's entry: watermark = the accounted
        // frontier, watermark-only (auto-in-set never itemizes).
        let entries = handle.states_of_kind(KIND_SEEN_SET).await.expect("states");
        let entry = entries
            .iter()
            .find(|e| e.key == mail_scope)
            .unwrap_or_else(|| {
                panic!(
                    "a seen-set entry for {mail_scope}, got keys {:?}",
                    entries.iter().map(|e| &e.key).collect::<Vec<_>>()
                )
            });
        let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode seen-set");
        assert_eq!(
            set.watermark_of(&WriterId::NEST_SEQUENCER.0),
            2,
            "the watermark is the accounted frontier"
        );
        assert!(set.refs.is_empty(), "auto-in-set is watermark-only");
        // Counted per scope, not in total: the fleet bootstrap publishes its
        // own rows on `state-fleet` at startup, and this test is about what the
        // seen-set producer put on the delegable state scope.
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
            1,
            "the entry was published to the plane"
        );

        // The no-echo half: a second pass with nothing new neither raises nor
        // re-publishes (raise_watermark refuses a non-raise, so no fresh put).
        let report = handle.reconcile_now().await.expect("second pass");
        assert!(report.errors.is_empty(), "clean pass: {:?}", report.errors);
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
            1,
            "a converged pass publishes nothing"
        );

        // New delivery between passes → the watermark follows the frontier.
        fx.fake.stage_record(&mail_scope, 3, [0xEF; 32]);
        handle.reconcile_now().await.expect("third pass");
        let entries = handle.states_of_kind(KIND_SEEN_SET).await.expect("states");
        let entry = entries.iter().find(|e| e.key == mail_scope).expect("entry");
        let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode seen-set");
        assert_eq!(set.watermark_of(&WriterId::NEST_SEQUENCER.0), 3);
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
            2,
            "the raise published once more"
        );
        handle.shutdown().await;
    }

    /// A membership source the test drives: the channel list, and whether the
    /// source can currently answer at all (`None` — see [`MembershipSource`]).
    #[derive(Clone, Default)]
    struct Memberships(Arc<Mutex<Option<Vec<[u8; 32]>>>>);

    impl Memberships {
        fn knows(channels: &[[u8; 32]]) -> Self {
            Self(Arc::new(Mutex::new(Some(channels.to_vec()))))
        }
        fn set(&self, channels: &[[u8; 32]]) {
            *self.0.lock().unwrap() = Some(channels.to_vec());
        }
        /// The loading-engine state: cannot answer yet.
        fn cannot_tell(&self) {
            *self.0.lock().unwrap() = None;
        }
        fn source(&self) -> MembershipSource {
            let cell = Arc::clone(&self.0);
            Arc::new(move || cell.lock().unwrap().clone())
        }
    }

    fn conv_scope(channel: [u8; 32]) -> String {
        ContentScope::new(crate::scope_set::CONV_KIND, channel)
            .expect("conv scope")
            .to_string()
    }

    fn walked(report: &PumpReport) -> Vec<String> {
        report
            .content_walks
            .iter()
            .map(|w| w.scope.clone())
            .collect()
    }

    /// The member half end-to-end: joined channels become walked `conv` scopes,
    /// a join between passes is picked up with no notification path, and a
    /// leave stops the walk (charter § Feeds and cursors → *Scope partition*:
    /// the set "changes as the account joins and leaves things").
    #[tokio::test]
    async fn joined_channels_are_walked_and_the_set_follows_joins_and_leaves() {
        let fx = fixture();
        let first = [0x11; 32];
        let second = [0x22; 32];
        let memberships = Memberships::knows(&[first]);
        let mut params = fx.params("a");
        params.memberships = Some(memberships.source());
        let handle = AccountStoreRuntime::start(params).await.expect("start");

        let report = handle.reconcile_now().await.expect("reconcile");
        assert!(
            walked(&report).contains(&conv_scope(first)),
            "joined channel walked"
        );
        assert_eq!(
            report.content_walks.len(),
            OWN_ACTOR_KINDS.len() + 1,
            "own-actor scopes plus the one channel"
        );

        // A join between passes — no register call, no nudge, just a different
        // answer from the source.
        memberships.set(&[first, second]);
        let report = handle.reconcile_now().await.expect("reconcile");
        let scopes = walked(&report);
        assert!(scopes.contains(&conv_scope(first)) && scopes.contains(&conv_scope(second)));

        // A leave ends the subscription: the scope stops being walked. (Dropping
        // that scope's *items* is T2 transition 3 — a deletion path this does
        // not touch.)
        memberships.set(&[second]);
        let scopes = walked(&handle.reconcile_now().await.expect("reconcile"));
        assert!(
            !scopes.contains(&conv_scope(first)),
            "left channel not walked"
        );
        assert!(
            scopes.contains(&conv_scope(second)),
            "remaining channel still walked"
        );
        handle.shutdown().await;
    }

    /// A source that cannot answer yet must not read as "left every channel" —
    /// the runtime keeps the last set it trusted.
    #[tokio::test]
    async fn an_unanswerable_membership_source_keeps_the_last_known_set() {
        let fx = fixture();
        let channel = [0x33; 32];
        let memberships = Memberships::knows(&[channel]);
        let mut params = fx.params("a");
        params.memberships = Some(memberships.source());
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        assert!(
            walked(&handle.reconcile_now().await.expect("reconcile"))
                .contains(&conv_scope(channel))
        );

        memberships.cannot_tell();
        let scopes = walked(&handle.reconcile_now().await.expect("reconcile"));
        assert!(
            scopes.contains(&conv_scope(channel)),
            "an unknown answer holds the set, it does not clear it: {scopes:?}"
        );
        handle.shutdown().await;
    }

    // ── Scope departure — T2 transition 3 (crate::departure) ────────────────
    //
    // Driven through the real runtime, because the whole risk lives in the
    // wiring: the store's `drop_scope` is proven in its own crate, and what
    // these pin is *when* the runtime is allowed to call it.

    /// A second connection to the runtime's own store — the store is
    /// multi-connection-safe by charter (WAL, § Multi-instance concurrency),
    /// so a test can read what the pump wrote without stopping it.
    /// Opened under the runtime's **own** writer id, read from the same
    /// credential slot: a store is bound to one replica identity (charter
    /// § The store device principal), so an observer is that replica or
    /// nothing.
    async fn store_of(fx: &Fixture, device: &str) -> AccountStore<SqliteBackend> {
        let dir = actor_state_dir(&fx.base.join(device), &fx.actor_hex)
            .expect("state dir")
            .join(STORE_SUBDIR);
        let credentials =
            CredentialStore::with_file_backend(CRED_NAMESPACE, fx.cred_dir.join(device));
        let (key, _) = writer_key_from_slot(&credentials, &fx.actor_hex).expect("writer key");
        AccountStore::open(
            SqliteBackend::open(&dir).expect("open store"),
            &fx.actor_hex,
            WriterId(key.verifying_key().to_bytes()),
        )
        .await
        .expect("store")
    }

    /// How many index rows the replica holds for a scope.
    async fn held(store: &AccountStore<SqliteBackend>, scope: &str) -> usize {
        store
            .records_in_scope(scope, None, 100)
            .await
            .expect("records")
            .len()
    }

    /// **The transition, end to end.** A channel's content is walked in, the
    /// account leaves between passes, and that scope's items leave the replica
    /// — "the data belonged to the membership" (charter T2 transition 3).
    ///
    /// Red-verified by returning early from `drop_departed_scopes` before the
    /// diff: the rows stay and this fails on the first assert.
    #[tokio::test]
    async fn leaving_a_channel_drops_that_scopes_items_from_the_replica() {
        let fx = fixture();
        let leaving = [0x11; 32];
        let staying = [0x22; 32];
        let memberships = Memberships::knows(&[leaving, staying]);
        let mut params = fx.params("a");
        params.memberships = Some(memberships.source());
        // Both channels have content the nest already sequenced.
        fx.fake.stage_record(&conv_scope(leaving), 1, [0xA1; 32]);
        fx.fake.stage_record(&conv_scope(staying), 1, [0xB2; 32]);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("reconcile");

        let store = store_of(&fx, "a").await;
        assert_eq!(held(&store, &conv_scope(leaving)).await, 1, "walked in");
        assert_eq!(held(&store, &conv_scope(staying)).await, 1);

        memberships.set(&[staying]); // the leave
        let report = handle.reconcile_now().await.expect("reconcile");

        let dropped = &report.departures.as_ref().expect("departure pass").dropped;
        assert_eq!(dropped.len(), 1, "exactly the left channel: {dropped:?}");
        assert_eq!(dropped[0].0, conv_scope(leaving));
        assert_eq!(
            held(&store, &conv_scope(leaving)).await,
            0,
            "the departed scope's index rows are gone"
        );
        assert!(
            store
                .frontier(&conv_scope(leaving))
                .await
                .expect("frontier")
                .is_empty(),
            "and its frontier vector with them"
        );
        assert_eq!(
            held(&store, &conv_scope(staying)).await,
            1,
            "the channel still joined is untouched"
        );
        handle.shutdown().await;
    }

    /// **Refusal 1 — the one that would delete a user's channels.** A source
    /// that cannot answer holds the walk set (pinned above); here it must also
    /// hold the *knife*. Nothing about a loading MLS engine says a membership
    /// ended.
    #[tokio::test]
    async fn an_unanswerable_membership_source_drops_nothing() {
        let fx = fixture();
        let channel = [0x33; 32];
        let memberships = Memberships::knows(&[channel]);
        let mut params = fx.params("a");
        params.memberships = Some(memberships.source());
        fx.fake.stage_record(&conv_scope(channel), 1, [0xC3; 32]);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("reconcile");
        let store = store_of(&fx, "a").await;
        assert_eq!(held(&store, &conv_scope(channel)).await, 1);

        memberships.cannot_tell();
        let report = handle.reconcile_now().await.expect("reconcile");

        let departures = report.departures.as_ref().expect("departure pass");
        assert!(
            departures.dropped.is_empty(),
            "a `None` answer is not a departure: {departures:?}"
        );
        assert_eq!(
            departures.skipped,
            Some("no affirmative membership answer this pass"),
            "and the pass says why it refused rather than looking clean"
        );
        assert_eq!(
            held(&store, &conv_scope(channel)).await,
            1,
            "the channel's content survives"
        );
        handle.shutdown().await;
    }

    /// **Refusal 2 — the six-app state.** An app that wires no
    /// `MembershipSource` derives no `conv` scope at all. If that read as authoritative, opening one of
    /// those apps would delete every channel's local content; it cannot,
    /// because a source that does not exist cannot answer affirmatively.
    #[tokio::test]
    async fn a_replica_with_no_membership_source_drops_nothing() {
        let fx = fixture();
        let channel = [0x44; 32];
        let scope = conv_scope(channel);
        // Give the runtime the scope explicitly the one way such an app can:
        // registration. Content walks in, and the account never says it left.
        fx.fake.stage_record(&scope, 1, [0xD4; 32]);
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start"); // memberships: None — the six-app shape
        handle
            .register_content_scope(
                ContentScope::new(crate::scope_set::CONV_KIND, channel).unwrap(),
            )
            .await
            .expect("register");
        handle.reconcile_now().await.expect("reconcile");
        let store = store_of(&fx, "a").await;
        assert_eq!(held(&store, &scope).await, 1);

        let report = handle.reconcile_now().await.expect("reconcile");
        assert!(
            report
                .departures
                .as_ref()
                .expect("departure pass")
                .dropped
                .is_empty()
        );
        assert_eq!(held(&store, &scope).await, 1, "still held");
        handle.shutdown().await;
    }

    /// **The seen-set survives the departure, through the live runtime.** The
    /// grow-only law is T2 transition 4: membership never shrinks, so the
    /// account's record of having observed those items outlives its access to
    /// them. (The store proves the structural half; this proves the wiring
    /// does not add a helpful extra delete.)
    #[tokio::test]
    async fn the_seen_set_entry_for_a_left_channel_outlives_its_content() {
        let fx = fixture();
        let channel = [0x55; 32];
        let scope = conv_scope(channel);
        let memberships = Memberships::knows(&[channel]);
        let mut params = fx.params("a");
        params.memberships = Some(memberships.source());
        fx.fake.stage_record(&scope, 1, [0xE5; 32]);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("reconcile");

        // What the T1 browse trigger will write once it ships: an itemized
        // entry keyed by the browse scope, living on the class-2 rail.
        let store = store_of(&fx, "a").await;
        let mut set = SeenScopeSet::new();
        set.raise_watermark(WriterId::NEST_SEQUENCER.0, 1);
        store
            .put_state(StateEntry {
                kind: KIND_SEEN_SET.to_string(),
                key: scope.clone(),
                scope: ACCOUNT_STATE_SCOPE.to_string(),
                value: canonical_encode(&set).expect("encode"),
                merge_meta: None,
                entry_version: 1,
                tombstone: false,
            })
            .await
            .expect("seed seen-set");

        memberships.set(&[]);
        handle.reconcile_now().await.expect("reconcile");

        assert_eq!(held(&store, &scope).await, 0, "content gone");
        let kept = store
            .state(KIND_SEEN_SET, &scope)
            .await
            .expect("state")
            .expect("the observation history is the account's, not the channel's");
        let kept: SeenScopeSet = canonical_decode(&kept.value).expect("decode");
        assert_eq!(
            kept.watermark_of(&WriterId::NEST_SEQUENCER.0),
            1,
            "and it did not shrink"
        );
        handle.shutdown().await;
    }

    /// **The T1 reporting seam, through the handle an app actually holds.** A
    /// browse record whose body an app says it displayed lands *itemized* — the
    /// one coordinate, never a watermark over the prefix a reader did not earn
    /// (charter § The replica boundary → T1).
    ///
    /// The second half is the property that lets a shell report its whole
    /// visible set every frame instead of keeping a "what did I already report"
    /// mirror: a repeat is [`ObservationOutcome::AlreadyIn`] and publishes
    /// nothing.
    #[tokio::test]
    async fn a_reported_observation_lands_that_records_coordinate_itemized() {
        let fx = fixture();
        let channel = [0x77; 32];
        let scope = conv_scope(channel);
        let mut params = fx.params("a");
        params.memberships = Some(Memberships::knows(&[channel]).source());
        // The record the app will claim to have displayed, and a second one it
        // never shows — the discriminating half of "itemized, not watermarked".
        fx.fake.stage_record(&scope, 1, [0xD1; 32]);
        fx.fake.stage_record(&scope, 2, [0xD2; 32]);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("reconcile");
        let puts_after_walk = fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE);

        let observation = Observation {
            scope: ContentScope::new(crate::scope_set::CONV_KIND, channel).expect("scope"),
            record: fauna_core::data::ContentHash::from_digest_dag_cbor([0xD2; 32]),
        };
        assert_eq!(
            handle
                .record_observation(observation.clone())
                .await
                .expect("record"),
            ObservationOutcome::Recorded,
        );

        let store = store_of(&fx, "a").await;
        let entry = store
            .state(KIND_SEEN_SET, &scope)
            .await
            .expect("state")
            .expect("the observation published this browse scope's entry");
        let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode");
        assert_eq!(
            set.watermark_of(&WriterId::NEST_SEQUENCER.0),
            0,
            "browse content earns no watermark — reading one message is not \
             reading the prefix below it",
        );
        assert!(
            set.contains(&WriterId::NEST_SEQUENCER.0, 2),
            "the displayed record's own coordinate is in-set: {set:?}",
        );
        assert!(
            !set.contains(&WriterId::NEST_SEQUENCER.0, 1),
            "the record that was never displayed stays out: {set:?}",
        );
        // The write is local; the publish step it armed runs ahead of this
        // barrier.
        handle.settled().await;
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
            puts_after_walk + 1,
            "the entry was published to the plane",
        );

        // Re-render of the same message: dedup against the merged entry, no put.
        assert_eq!(
            handle
                .record_observation(observation)
                .await
                .expect("re-record"),
            ObservationOutcome::AlreadyIn,
        );
        assert_eq!(
            fx.fake.put_calls_for(ACCOUNT_STATE_SCOPE),
            puts_after_walk + 1,
            "an already-in observation publishes nothing",
        );
        handle.shutdown().await;
    }

    /// **Two frames' reports overlap on one scope, and both bodies stay
    /// in-set.** The pin asked
    /// for once a real reporting caller existed — it now does, and it is exactly
    /// the concurrent shape that pin predicted.
    ///
    /// tui reports fire-and-forget: `observation::report` spawns a task per
    /// frame (`apps/fauna-tui/src/observation.rs`) and the paint loop calls it
    /// every frame, so a frame can paint while the previous frame's report is
    /// still in flight — overlapping same-scope reports are the steady state
    /// there, not an edge case.
    ///
    /// The intake is a read-modify-write over that scope's entry
    /// (`observation_intake::record_observation` reads `store.state`, inserts
    /// the ref, and puts the value it computed *verbatim*), so two concurrent
    /// same-scope observations would clobber: the second put carries an entry
    /// read before the first landed, and one body a reader genuinely saw falls
    /// out of the set. What prevents it is that every production observation
    /// funnels through `Cmd::RecordObservation` on the runtime's single store
    /// thread, which awaits each intake call whole before taking the next
    /// command. This test is what makes that load-bearing rather than
    /// incidental: parallelize the command loop, or let a caller reach the
    /// intake without crossing this handle, and it reds.
    ///
    /// The assertion is deliberately on the **local** entry rather than a
    /// post-merge fleet view. `SeenScopeSet` is grow-only and each put is
    /// published at its own `writer_seq`, so the merge seam unions the two rows
    /// and hides the clobber — a test that merged first would stay green
    /// through precisely the defect it exists to catch.
    #[tokio::test]
    async fn overlapping_frame_reports_for_one_scope_both_survive_locally() {
        let fx = fixture();
        let channel = [0x5C; 32];
        let scope = conv_scope(channel);
        let mut params = fx.params("a");
        params.memberships = Some(Memberships::knows(&[channel]).source());
        // Two distinct bodies in the one scope — the discriminating pair: a
        // clobber keeps whichever put landed second and drops the other.
        fx.fake.stage_record(&scope, 1, [0xE1; 32]);
        fx.fake.stage_record(&scope, 2, [0xE2; 32]);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("reconcile");

        let observation_of = |digest: [u8; 32]| Observation {
            scope: ContentScope::new(crate::scope_set::CONV_KIND, channel).expect("scope"),
            record: fauna_core::data::ContentHash::from_digest_dag_cbor(digest),
        };
        // Two tasks, as the shell spawns them — not two awaits on one task.
        let first = {
            let handle = handle.clone();
            let observation = observation_of([0xE1; 32]);
            tokio::spawn(async move { handle.record_observation(observation).await })
        };
        let second = {
            let handle = handle.clone();
            let observation = observation_of([0xE2; 32]);
            tokio::spawn(async move { handle.record_observation(observation).await })
        };
        assert_eq!(
            first.await.expect("first task").expect("first record"),
            ObservationOutcome::Recorded,
        );
        assert_eq!(
            second.await.expect("second task").expect("second record"),
            ObservationOutcome::Recorded,
        );

        let store = store_of(&fx, "a").await;
        let entry = store
            .state(KIND_SEEN_SET, &scope)
            .await
            .expect("state")
            .expect("the reports published this browse scope's entry");
        let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode");
        assert!(
            set.contains(&WriterId::NEST_SEQUENCER.0, 1),
            "the first frame's body survived the second frame's report: {set:?}",
        );
        assert!(
            set.contains(&WriterId::NEST_SEQUENCER.0, 2),
            "the second frame's body is in-set: {set:?}",
        );
        assert_eq!(
            set.watermark_of(&WriterId::NEST_SEQUENCER.0),
            0,
            "concurrency changes nothing about itemization — still no watermark",
        );
        handle.shutdown().await;
    }

    /// A report this replica cannot place is **dropped, never queued** — and
    /// re-recordable once the walk catches up (charter § The replica boundary →
    /// T1: "dropped and re-recorded on a later render ... never CID-keyed
    /// pending state"). The hydration path can outrun the feed walk, and the
    /// grow-only set makes the retry free.
    #[tokio::test]
    async fn an_observation_the_walk_has_not_carried_yet_writes_nothing_and_retries() {
        let fx = fixture();
        let channel = [0x88; 32];
        let scope = conv_scope(channel);
        let mut params = fx.params("a");
        params.memberships = Some(Memberships::knows(&[channel]).source());
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("reconcile");

        let observation = Observation {
            scope: ContentScope::new(crate::scope_set::CONV_KIND, channel).expect("scope"),
            record: fauna_core::data::ContentHash::from_digest_dag_cbor([0xD9; 32]),
        };
        assert_eq!(
            handle
                .record_observation(observation.clone())
                .await
                .expect("record"),
            ObservationOutcome::Unresolved,
        );
        let store = store_of(&fx, "a").await;
        assert!(
            store
                .state(KIND_SEEN_SET, &scope)
                .await
                .expect("state")
                .is_none(),
            "an unresolvable observation writes no entry at all",
        );

        // The walk catches up; the next render's report resolves.
        fx.fake.stage_record(&scope, 4, [0xD9; 32]);
        handle.reconcile_now().await.expect("reconcile");
        assert_eq!(
            handle.record_observation(observation).await.expect("retry"),
            ObservationOutcome::Recorded,
        );
        let entry = store
            .state(KIND_SEEN_SET, &scope)
            .await
            .expect("state")
            .expect("the retry landed");
        let set: SeenScopeSet = canonical_decode(&entry.value).expect("decode");
        assert!(set.contains(&WriterId::NEST_SEQUENCER.0, 4), "{set:?}");
        handle.shutdown().await;
    }

    /// **A re-join re-bootstraps with no re-join code.** The frontier row left
    /// with everything else, so the next walk of that scope starts at cursor
    /// zero — the same path a fresh replica takes.
    #[tokio::test]
    async fn a_re_join_re_walks_the_scope_from_zero() {
        let fx = fixture();
        let channel = [0x66; 32];
        let scope = conv_scope(channel);
        let memberships = Memberships::knows(&[channel]);
        let mut params = fx.params("a");
        params.memberships = Some(memberships.source());
        fx.fake.stage_record(&scope, 1, [0xF6; 32]);
        let handle = AccountStoreRuntime::start(params).await.expect("start");
        handle.reconcile_now().await.expect("reconcile");
        let store = store_of(&fx, "a").await;
        assert_eq!(held(&store, &scope).await, 1);

        memberships.set(&[]);
        handle.reconcile_now().await.expect("reconcile");
        assert_eq!(held(&store, &scope).await, 0, "dropped");

        // Re-join. The nest still holds the row at seq 1; a replica that kept
        // its frontier would ask for rows *after* 1 and re-materialize
        // nothing.
        memberships.set(&[channel]);
        handle.reconcile_now().await.expect("reconcile");
        assert_eq!(
            held(&store, &scope).await,
            1,
            "the re-joined scope re-walked its whole feed"
        );
        handle.shutdown().await;
    }

    /// An explicitly registered scope survives every re-derivation — the two
    /// sources of scopes are unioned, never one overwriting the other.
    #[tokio::test]
    async fn a_registered_scope_survives_re_derivation() {
        let fx = fixture();
        let memberships = Memberships::knows(&[]);
        let mut params = fx.params("a");
        params.memberships = Some(memberships.source());
        let handle = AccountStoreRuntime::start(params).await.expect("start");

        let extra = ContentScope::new("post", [0x44; 32]).expect("scope");
        handle
            .register_content_scope(extra.clone())
            .await
            .expect("register");
        memberships.set(&[[0x55; 32]]); // force a re-derivation with new input
        let scopes = walked(&handle.reconcile_now().await.expect("reconcile"));
        assert!(
            scopes.contains(&extra.to_string()),
            "the registered scope is still walked: {scopes:?}"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_is_idempotent_and_calls_after_it_error() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        handle.shutdown().await;
        handle.shutdown().await; // second call: no hang, no panic
        let err = handle
            .get_preference(KIND_MODERATION)
            .await
            .expect_err("gone");
        assert!(err.to_string().contains(RUNTIME_GONE));
    }

    // ── The preference surfaces (crate::preference_surfaces) ─────────────────
    //
    // Tested here rather than in their own module because the runtime's
    // `FakeNest` is the double they need — a live pump over a stateful nest —
    // and it is deliberately private to this test module.

    /// A save stores the normalized list and hands it back, and a re-read
    /// agrees.
    #[tokio::test]
    async fn a_store_save_stores_the_normalized_list() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        let snap = crate::preference_surfaces::save_muted_words(
            &handle,
            vec!["  Lottery ".into(), "lottery".into(), "  ".into()],
        )
        .await
        .expect("store save");
        assert!(snap.loaded, "a completed save leaves the page loaded");
        assert_eq!(
            snap.terms(),
            vec!["Lottery".to_string()],
            "the one shared normalizer: trimmed, blank-dropped, case-deduped"
        );

        // The store's own re-read agrees.
        let store_view = crate::preference_surfaces::load_muted_words(&handle)
            .await
            .expect("store read");
        assert_eq!(store_view.terms(), snap.terms());
        assert!(store_view.loaded);
        handle.shutdown().await;
    }

    /// A replica whose store holds no entry answers a *loaded* empty page (a
    /// `None` entry is an answer, never a still-loading state).
    #[tokio::test]
    async fn a_store_read_with_no_entry_is_a_loaded_empty_page() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        let snap = crate::preference_surfaces::load_muted_words(&handle)
            .await
            .expect("read");
        assert!(snap.loaded, "the store answered");
        assert!(snap.terms().is_empty());
        assert!(snap.shows_empty_state());
        handle.shutdown().await;
    }

    /// A store save with an existing entry replaces the list (whole-record
    /// LWW, the moderation kind's merge policy) and keeps serving the store's
    /// own read path — two saves, second wins.
    #[tokio::test]
    async fn a_second_store_save_replaces_the_list() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        crate::preference_surfaces::save_muted_words(&handle, vec!["first".into()])
            .await
            .expect("first save");
        let snap = crate::preference_surfaces::save_muted_words(&handle, vec!["second".into()])
            .await
            .expect("second save");
        assert_eq!(snap.terms(), vec!["second".to_string()]);
        let reread = crate::preference_surfaces::load_muted_words(&handle)
            .await
            .expect("reread");
        assert_eq!(reread.terms(), vec!["second".to_string()]);
        handle.shutdown().await;
    }

    /// **A preference gesture made before the runtime is up waits for it**
    /// (`config-dissolution.md` § The `__config` dissolution schedule → *What
    /// replaces the bridge's two carriages*, case (a)). The page's surface is
    /// handed the seat's store, whose source answers no handle yet, so the add
    /// is left waiting; the runtime then assembles and the seat's slot fills,
    /// and the add lands on the account store. Nothing is written outside
    /// the account store at any point.
    #[tokio::test]
    async fn a_preference_gesture_made_before_the_runtime_is_up_lands_once_it_is() {
        use crate::preference_surfaces as ps;
        let fx = fixture();
        let slot: Arc<std::sync::Mutex<Option<AccountStoreHandle>>> = Default::default();
        let seat = SeatAccountStore::new({
            let slot = Arc::clone(&slot);
            Arc::new(move || slot.lock().unwrap().clone())
        });

        let add = tokio::spawn({
            let seat = seat.clone();
            async move { ps::add_muted_word(&seat, "early").await }
        });
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        assert!(
            !add.is_finished(),
            "the gesture waits while the seat has no runtime"
        );
        *slot.lock().unwrap() = Some(handle.clone());

        let snap = add
            .await
            .expect("the gesture's task")
            .expect("the add lands once the runtime is up");
        assert_eq!(snap.terms(), vec!["early".to_string()]);
        assert_eq!(
            ps::load_muted_words(&handle)
                .await
                .expect("store read")
                .terms(),
            vec!["early".to_string()],
            "the account store took the add"
        );
        handle.settled().await;
        handle.shutdown().await;
    }

    /// **…and fails when none comes.** With no runtime for the whole bound, a
    /// read and a write each wait it out and answer that the runtime is not
    /// running. `start_paused` turns the bound into an instant of virtual
    /// time.
    #[tokio::test(start_paused = true)]
    async fn a_preference_gesture_with_no_runtime_waits_out_the_bound_and_fails() {
        use crate::preference_surfaces as ps;
        let seat = SeatAccountStore::new(Arc::new(|| None));

        let started = tokio::time::Instant::now();
        let err = ps::save_muted_words(&seat, vec!["never".into()])
            .await
            .expect_err("a save with no account store");
        assert!(err.to_string().contains(RUNTIME_ABSENT), "{err:#}");
        assert!(
            started.elapsed() >= ACCOUNT_HANDLE_WAIT,
            "the save waited the bound out: {:?}",
            started.elapsed()
        );

        let err = ps::load_muted_words(&seat)
            .await
            .expect_err("a read with no account store");
        assert!(err.to_string().contains(RUNTIME_ABSENT), "{err:#}");
        let err = ps::load_sync_prefs(&seat)
            .await
            .expect_err("a sync-prefs read with no account store");
        assert!(err.to_string().contains(RUNTIME_ABSENT), "{err:#}");
    }

    /// A sync-prefs save stores the normalized policy, a re-read agrees, and
    /// a clear is a real value.
    #[tokio::test]
    async fn a_store_sync_prefs_save_stores_the_normalized_policy() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        // An out-of-catalog value degrades to the canonical default — the
        // shared `preference_records::set_default_conflict_policy`.
        let stored =
            crate::preference_surfaces::save_sync_prefs(&handle, Some("definitely-not-a-policy"))
                .await
                .expect("store save");
        assert_eq!(stored.as_deref(), Some("auto"));

        let stored =
            crate::preference_surfaces::save_sync_prefs(&handle, Some("latest_wins_always"))
                .await
                .expect("second save");
        assert_eq!(stored.as_deref(), Some("latest_wins_always"));
        assert_eq!(
            crate::preference_surfaces::load_sync_prefs(&handle)
                .await
                .expect("store read"),
            stored,
            "the store's own re-read agrees"
        );

        // Clearing is a write — a `None` is a real value, not "leave it alone".
        assert_eq!(
            crate::preference_surfaces::save_sync_prefs(&handle, None)
                .await
                .expect("clear"),
            None
        );
        handle.shutdown().await;
    }

    /// A replica with no sync-prefs entry answers `None` — no preference
    /// recorded, so new sets take the nest column default. Not an error, and
    /// not a silent `"auto"` (which would be a *recorded* preference).
    #[tokio::test]
    async fn a_store_sync_prefs_read_with_no_entry_is_no_preference() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        assert_eq!(
            crate::preference_surfaces::load_sync_prefs(&handle)
                .await
                .expect("read"),
            None
        );
        handle.shutdown().await;
    }

    /// The **personalization registry** and the **delegation pins** — the two
    /// clusters whose surfaces compose their sub-record with nest-side facts,
    /// so only the record half lives on the plane: each write reads back.
    #[tokio::test]
    async fn store_writes_of_the_registry_and_the_pins_read_back() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        // Personalization: mint through the plane using the shared registry
        // mutator.
        let (registry, minted) = crate::preference_surfaces::update_personalization(&handle, |r| {
            fauna_client_config::preference_records::add_trained_factor(r, " Cats ")
        })
        .await
        .expect("plane create");
        let minted = minted.expect("a fresh registry is under the cap");
        assert_eq!(minted.name, "Cats", "the shared mutator trimmed it");
        assert_eq!(registry.trained_factors.len(), 1);

        // Delegation: pin a kind through the plane using the sub-record's own
        // shared `set_pin`.
        let pin = fauna_core::data::ParticipantRef::Device {
            device_id: "aa".repeat(32),
        };
        let (pins, ()) = crate::preference_surfaces::update_delegation(&handle, |d| {
            d.set_pin(fauna_core::delegation::KIND_INDEX, Some(pin.clone()))
        })
        .await
        .expect("plane pin");
        assert_eq!(pins.assignments.len(), 1);

        // Each plane read agrees with what was stored.
        assert_eq!(
            crate::preference_surfaces::load_personalization(&handle)
                .await
                .expect("registry read"),
            registry
        );
        assert_eq!(
            crate::preference_surfaces::load_delegation(&handle)
                .await
                .expect("pins read"),
            pins
        );
        handle.shutdown().await;
    }

    /// The generic core's byte-level skip: a save that stores the value already
    /// there mints **no** new stamp. A no-op write is not merely wasted work —
    /// its fresh `LwwStamp` would outrank a concurrent sibling's real edit and
    /// lose it, which is why the skip is decided on the encoded bytes rather
    /// than on a mutator's own "changed" report.
    #[tokio::test]
    async fn a_save_that_changes_nothing_mints_no_new_stamp() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        crate::preference_surfaces::save_sync_prefs(&handle, Some("auto"))
            .await
            .expect("first save");
        let first = handle
            .get_preference(KIND_SYNC_PREFS)
            .await
            .expect("read")
            .expect("entry exists")
            .entry_version;

        crate::preference_surfaces::save_sync_prefs(&handle, Some("auto"))
            .await
            .expect("identical save");
        let second = handle
            .get_preference(KIND_SYNC_PREFS)
            .await
            .expect("read")
            .expect("entry exists")
            .entry_version;
        assert_eq!(first, second, "an identical save wrote nothing");

        // The guard is byte-scoped, not write-scoped: a real change still writes.
        crate::preference_surfaces::save_sync_prefs(&handle, Some("latest_wins_always"))
            .await
            .expect("real change");
        let third = handle
            .get_preference(KIND_SYNC_PREFS)
            .await
            .expect("read")
            .expect("entry exists")
            .entry_version;
        assert_ne!(second, third, "a real change still mints a stamp");
        handle.shutdown().await;
    }

    // ── The outbox (W4, charter § The offline-mutation contract) ─────────────

    /// An `OfflineQueued` kind, real and registered (the enqueue door checks).
    const QUEUED_KIND: &str = "fauna.subscriptions.requests.approve";

    /// The core W4 property over the real runtime: an enqueued intent is
    /// durable, drains exactly once on the next full pass, and the request's
    /// envelope key IS the stored intent id — the whole reason the keyed
    /// seam exists (the nest's durable idempotency table will key on it).
    #[tokio::test]
    async fn an_enqueued_intent_drains_on_the_next_pass_with_its_stored_key() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        let payload = wire_encode(&7u8).expect("payload").to_vec();
        let id = handle
            .enqueue_intent(QUEUED_KIND, "state", payload, IntentDrainer::Rpc)
            .await
            .expect("enqueue");

        let report = handle.reconcile_now().await.expect("pass");
        let outbox = report.outbox.expect("outbox step ran");
        assert_eq!(outbox.drained, 1, "errors: {:?}", report.errors);
        assert_eq!(
            fx.fake.keyed_requests(),
            vec![(QUEUED_KIND.to_string(), id)],
            "the envelope carried the STORED intent id"
        );

        // Drained = deleted: a second pass replays nothing.
        let report = handle.reconcile_now().await.expect("second pass");
        assert_eq!(report.outbox.expect("outbox step").drained, 0);
        assert_eq!(fx.fake.keyed_requests().len(), 1, "no replay after ack");
        handle.shutdown().await;
    }

    /// The phase-0 boundary at the door: an `OnlineOnly` kind must not queue
    /// (and an unregistered kind cannot).
    #[tokio::test]
    async fn the_enqueue_door_refuses_non_queued_kinds() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");
        let err = handle
            .enqueue_intent(
                "fauna.bridges.add_local_domain", // OnlineOnly by charter
                "state",
                vec![],
                IntentDrainer::Rpc,
            )
            .await
            .expect_err("refused");
        assert!(err.to_string().contains("not OfflineQueued"), "{err}");
        let err = handle
            .enqueue_intent("fauna.no.such.kind", "state", vec![], IntentDrainer::Rpc)
            .await
            .expect_err("refused");
        assert!(err.to_string().contains("not a registered kind"), "{err}");
        handle.shutdown().await;
    }

    /// A rejection parks the intent AND its scope's remainder — FIFO is never
    /// reordered around a failure — and the park is durable across passes.
    #[tokio::test]
    async fn a_rejection_parks_the_intent_and_holds_its_scope() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        let payload = wire_encode(&7u8).expect("payload").to_vec();
        handle
            .enqueue_intent(QUEUED_KIND, "state", payload.clone(), IntentDrainer::Rpc)
            .await
            .expect("first");
        handle
            .enqueue_intent(QUEUED_KIND, "state", payload, IntentDrainer::Rpc)
            .await
            .expect("second");

        fx.fake.reject_next_keyed.store(true, Ordering::SeqCst);
        let report = handle.reconcile_now().await.expect("pass");
        let outbox = report.outbox.expect("outbox step");
        assert_eq!(outbox.newly_parked, 1);
        assert_eq!(outbox.held_behind_park, 1, "the second never attempted");
        assert_eq!(outbox.drained, 0);
        assert_eq!(fx.fake.keyed_requests().len(), 1);

        // The park persists: the next pass attempts NOTHING on that scope,
        // and both intents are still durable (user action is the only exit).
        let report = handle.reconcile_now().await.expect("next pass");
        let outbox = report.outbox.expect("outbox step");
        assert_eq!(outbox.drained, 0);
        assert_eq!(outbox.held_behind_park, 1);
        assert_eq!(fx.fake.keyed_requests().len(), 1, "no new attempts");
        handle.shutdown().await;
    }

    /// A transport fault stops the pass (the connection is gone) and the next
    /// pass replays the SAME intent id — the at-least-once posture whose
    /// dedup phase 3's durable table supplies.
    #[tokio::test]
    async fn a_transport_fault_retries_next_pass_with_the_same_key() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        let payload = wire_encode(&7u8).expect("payload").to_vec();
        let id = handle
            .enqueue_intent(QUEUED_KIND, "state", payload, IntentDrainer::Rpc)
            .await
            .expect("enqueue");

        fx.fake.fail_next_keyed.store(true, Ordering::SeqCst);
        let report = handle.reconcile_now().await.expect("faulted pass");
        let outbox = report.outbox.expect("outbox step");
        assert_eq!(outbox.retried, 1);
        assert_eq!(outbox.drained, 0);

        let report = handle.reconcile_now().await.expect("retry pass");
        assert_eq!(report.outbox.expect("outbox step").drained, 1);
        let attempts = fx.fake.keyed_requests();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].1, id);
        assert_eq!(attempts[1].1, id, "the retry re-presents the same key");
        handle.shutdown().await;
    }

    /// An `Mls` intent is never sent by the generic drain — it waits for the
    /// gated MLS send path (T6's consumer), listed but untouched.
    #[tokio::test]
    async fn an_mls_intent_is_held_not_sent() {
        let fx = fixture();
        let handle = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("start");

        // A conversations send is the MLS composer's kind; enqueue it with
        // the Mls drainer exactly as that composer will.
        let payload = wire_encode(&7u8).expect("payload").to_vec();
        handle
            .enqueue_intent(
                "fauna.conversations.channel.send",
                "conv-scope",
                payload,
                IntentDrainer::Mls,
            )
            .await
            .expect("enqueue");

        let report = handle.reconcile_now().await.expect("pass");
        let outbox = report.outbox.expect("outbox step");
        assert_eq!(outbox.held_for_mls, 1);
        assert_eq!(outbox.drained, 0);
        assert!(fx.fake.keyed_requests().is_empty(), "nothing sent");
        handle.shutdown().await;
    }

    // -----------------------------------------------------------------------
    // The T10 slot over the phones' arm — the app's store lent over the
    // foreign seam (`apps/common.md` § Credential storage → *The shared Rust
    // credential slots on the phones*, 2026-08-26). The production shape iOS
    // and android reach; every desktop test above rides the file arm.
    // -----------------------------------------------------------------------

    /// The Swift Keychain / android `EncryptedSharedPreferences` stand-in
    /// behind the seam: a flat logical-key map.
    #[derive(Default)]
    struct PlatformStore(std::sync::Mutex<std::collections::BTreeMap<String, String>>);

    impl SecretStore for PlatformStore {
        fn get(&self, key: &str) -> Option<String> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
        }
        fn delete(&self, key: &str) {
            self.0.lock().unwrap().remove(key);
        }
    }

    /// The platform store as `no_keyring` presents it — and as a locked
    /// keychain or a denied ACL does: every write silently swallowed.
    struct DroppingPlatformStore;

    impl SecretStore for DroppingPlatformStore {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
        fn set(&self, _key: &str, _value: &str) {}
        fn delete(&self, _key: &str) {}
    }

    /// The writer key mints into the lent store, reads back, and every later
    /// assembly loads the SAME key — the "one writer per machine forever"
    /// property the journal's equivocation refusal depends on, now holding on
    /// a phone. The row the platform store sees is namespace-prefixed, so it
    /// can never collide with the agent slot's item for the same actor.
    #[test]
    fn the_writer_key_persists_and_reloads_over_the_phones_lent_store() {
        let platform = Arc::new(PlatformStore::default());
        let creds = CredentialStore::with_foreign_backend(
            CRED_NAMESPACE,
            Arc::clone(&platform) as Arc<dyn SecretStore>,
        );
        let actor = fauna_core::hex32::encode(&[0x42; 32]);

        let (minted, minted_from) =
            writer_key_from_slot(&creds, &actor).expect("the first assembly mints");
        let (loaded, loaded_from) =
            writer_key_from_slot(&creds, &actor).expect("the second assembly loads");
        assert_eq!(
            (minted_from, loaded_from),
            (WriterKeyProvenance::Minted, WriterKeyProvenance::Loaded),
            "the resolver says which of the two it did — the journal-bound writer's arm \
             keys on it"
        );
        assert_eq!(
            minted.to_bytes(),
            loaded.to_bytes(),
            "a second assembly must load the minted key, never re-mint"
        );

        let row = format!("{CRED_NAMESPACE}/{actor}");
        let stored = platform
            .get(&row)
            .expect("the platform store holds the writer key under the prefixed row");
        assert_eq!(
            fauna_core::hex32::decode(&stored).unwrap(),
            minted.to_bytes(),
            "the stored hex IS the minted secret"
        );
        // Two rows, both prefixed — the writer key and the unstamped-mint
        // marker the mint sets beside it (refinement 11's provenance memory,
        // spent by the first store open) — and no unprefixed twin.
        let rows: Vec<String> = platform.0.lock().unwrap().keys().cloned().collect();
        assert_eq!(
            rows.len(),
            2,
            "the key and its mint marker, nothing else: {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|k| k.starts_with(&format!("{CRED_NAMESPACE}/"))),
            "every row rides the namespace prefix — no unprefixed twin: {rows:?}"
        );
        assert!(
            rows.contains(&format!("{CRED_NAMESPACE}/{actor}/writer-unstamped")),
            "the mint marks its key unstamped: {rows:?}"
        );
    }

    /// A phone whose shell never lent its store — or whose platform store
    /// swallowed the write — still refuses at the read-back rather than
    /// opening the store with a key that would re-mint on the next launch
    /// (the stranding shape `account-scoping.md` § Erasure follows scope
    /// records). The refusal is the same text the iOS log carried on
    /// 2026-08-26, which is what makes the log diagnosable.
    #[test]
    fn a_lent_store_that_drops_the_write_is_refused_at_the_read_back() {
        let creds =
            CredentialStore::with_foreign_backend(CRED_NAMESPACE, Arc::new(DroppingPlatformStore));
        let actor = fauna_core::hex32::encode(&[0x43; 32]);
        let err = writer_key_from_slot(&creds, &actor)
            .expect_err("a dropped write must not open the store");
        assert!(
            err.to_string().contains("did not retain the writer key"),
            "the refusal names the dropped write: {err:#}"
        );
    }

    // -----------------------------------------------------------------------
    // The store dir's cloud-backup exclusion — stated by every assembly,
    // applied before anything opens the dir (`AccountRuntimeParams::
    // store_backup_exclusion`, 2026-08-26).
    // -----------------------------------------------------------------------

    /// The shell arm runs against the per-actor store dir — created first, so
    /// apple's `setResourceValue` has something to mark — on every assembly.
    #[tokio::test]
    async fn the_shell_exclusion_runs_against_the_created_store_dir() {
        let fx = fixture();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
        let sink = Arc::clone(&seen);
        let mut params = fx.params("a");
        params.store_backup_exclusion =
            CloudBackupExclusion::ExcludedByShell(Arc::new(move |p: &std::path::Path| {
                assert!(
                    p.is_dir(),
                    "the shell was handed a store dir that does not exist"
                );
                sink.lock().unwrap().push(p.to_path_buf());
                Ok(())
            }));
        let handle = AccountStoreRuntime::start(params).await.expect("runtime");
        let store_dir = StoreRoot::at(fx.base.join("a"))
            .store_dir(&fx.actor_hex)
            .expect("store dir");
        assert_eq!(
            &*seen.lock().unwrap(),
            &[store_dir],
            "the exclusion targets exactly the per-actor store dir, once per assembly"
        );
        handle.shutdown().await;
    }

    /// **Mutation-verified rule.** A failed exclusion aborts the assembly
    /// rather than opening the store anyway: a store written into a dir the
    /// platform still replicates is the outcome the obligation exists to
    /// prevent, and nothing on the device would ever look wrong.
    #[tokio::test]
    async fn a_failed_exclusion_refuses_to_assemble() {
        let fx = fixture();
        let mut params = fx.params("a");
        params.store_backup_exclusion =
            CloudBackupExclusion::ExcludedByShell(Arc::new(|_p: &std::path::Path| {
                anyhow::bail!("setResourceValue refused")
            }));
        let err = AccountStoreRuntime::start(params)
            .await
            .expect_err("an unexcluded store dir must not be opened");
        let text = format!("{err:#}");
        assert!(
            text.contains("cloud backup") && text.contains("setResourceValue refused"),
            "the failure names the obligation and the cause: {text}"
        );
    }

    /// The self-signed enrollment ruling's heal:
    /// `fleet_bootstrap` re-publishes this device's enrollment signed
    /// whenever the merged row at its own cell is not its own verifying one
    /// — an unsigned row or a `BackupKey` holder's forgery carrying its
    /// cert — and leaves a verifying own row
    /// and a `Removed` untouched (write-if-absent, where "present" means
    /// ours-and-signed).
    #[tokio::test]
    async fn fleet_bootstrap_republishes_the_enrollment_signed_over_an_unsigned_or_forged_row() {
        use crate::generation_fixture_test_support::{
            Fixture, THEM, US, device_id_of, fixture, machinery_row, root, unsigned_enrollment_row,
        };
        use fauna_account_store::types::StateEntry;
        use fauna_core::generation::{DeviceSetRecord, derive_device_xwing_keypair};
        use fauna_protocol::merge_policy::KIND_DEVICE_SET;

        async fn merged(f: &Fixture) -> StateEntry {
            f.store
                .state(
                    KIND_DEVICE_SET,
                    &fauna_core::hex32::encode(&device_id_of(US)),
                )
                .await
                .unwrap()
                .expect("a device-set row at our own cell")
        }
        fn record_of(entry: &StateEntry) -> DeviceSetRecord {
            canonical_decode(&entry.value).unwrap()
        }
        fn kem_of(entry: &StateEntry) -> Vec<u8> {
            match record_of(entry) {
                DeviceSetRecord::Enrolled { xwing_pubkey, .. } => xwing_pubkey,
                DeviceSetRecord::Removed { .. } => Vec::new(),
            }
        }

        let f = fixture().await;
        let me = device_id_of(US);
        let my_kem = derive_device_xwing_keypair(&US).public.to_bytes().to_vec();
        let rows = || fleet_bootstrap_rows(&root(), &f.writer_key).expect("bootstrap rows");

        // A fresh replica: written once, signed, our own KEM.
        fleet_bootstrap(&f.store, &f.plane(), Some(rows())).await;
        let first = merged(&f).await;
        assert!(record_of(&first).self_verifies_at(&me));
        assert_eq!(kem_of(&first), my_kem);
        // Idempotent: a verifying own row is left alone.
        fleet_bootstrap(&f.store, &f.plane(), Some(rows())).await;
        assert_eq!(
            merged(&f).await.entry_version,
            first.entry_version,
            "no re-publish over our own signed row"
        );

        // An unsigned row at our cell: re-published over, signed.
        f.put(unsigned_enrollment_row(US)).await;
        assert!(!record_of(&merged(&f).await).self_verifies_at(&me));
        fleet_bootstrap(&f.store, &f.plane(), Some(rows())).await;
        let upgraded = merged(&f).await;
        assert!(
            record_of(&upgraded).self_verifies_at(&me),
            "the unsigned row is replaced by our signed one"
        );

        // A forgery at our cell — our cert verbatim, THEM's KEM key, the
        // shape: re-published over, and the merged KEM is ours again.
        let DeviceSetRecord::Enrolled { authorization, .. } = record_of(&upgraded) else {
            unreachable!()
        };
        f.put(machinery_row(
            KIND_DEVICE_SET,
            fauna_core::hex32::encode(&me),
            &DeviceSetRecord::Enrolled {
                xwing_pubkey: derive_device_xwing_keypair(&THEM)
                    .public
                    .to_bytes()
                    .to_vec(),
                authorization,
                enrolled_at_ms: 5_000,
                device_sig: Vec::new(),
            },
        ))
        .await;
        assert_ne!(kem_of(&merged(&f).await), my_kem, "the forgery is in place");
        fleet_bootstrap(&f.store, &f.plane(), Some(rows())).await;
        let healed = merged(&f).await;
        assert!(record_of(&healed).self_verifies_at(&me));
        assert_eq!(
            kem_of(&healed),
            my_kem,
            "the forgery is displaced by our own signed row"
        );

        // A removed self never re-announces.
        f.put(machinery_row(
            KIND_DEVICE_SET,
            fauna_core::hex32::encode(&me),
            &DeviceSetRecord::Removed {
                removed_at_ms: 9_000,
                removed_by: device_id_of(THEM),
            },
        ))
        .await;
        let removed = merged(&f).await;
        fleet_bootstrap(&f.store, &f.plane(), Some(rows())).await;
        let after = merged(&f).await;
        assert_eq!(after.entry_version, removed.entry_version);
        assert!(matches!(record_of(&after), DeviceSetRecord::Removed { .. }));
    }

    /// **The bootstrap sends nothing: it runs inside `start()`'s readiness
    /// barrier, which a sign-out cannot cut.** Until `ready` the host holds
    /// no handle, so a sign-out landing then waits the whole assembly out
    /// under `ACCOUNT_RUNTIME_STOP_BUDGET` — and a nest leg in here, slow
    /// while the principal's first connect still waits for its grant, spent
    /// that budget by itself (measured on Windows 2026-09-30: each of the two
    /// puts took ~5 s to fail, and the erase found `account-store.db` still
    /// open). The rows publish on the prologue's publish step, where a
    /// sign-out cuts the pass. Red-verified against the publishing `put`:
    /// the bootstrap waits on this nest for ever.
    #[tokio::test]
    async fn fleet_bootstrap_writes_locally_and_never_waits_on_the_nest() {
        use crate::generation_fixture_test_support::{US, device_id_of, fixture, root};
        use fauna_account_plane::account_state_plane::AccountStatePlane;

        /// A nest that never answers.
        #[derive(Clone)]
        struct Silent;
        impl RpcRequester for Silent {
            type Error = std::convert::Infallible;
            async fn request<Req, Reply>(
                &self,
                _kind: &'static str,
                _payload: Req,
            ) -> Result<Reply, Self::Error>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                std::future::pending().await
            }
        }

        let f = fixture().await;
        let plane = AccountStatePlane::new(
            &f.store,
            &Silent,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .expect("publishing plane");
        let rows = fleet_bootstrap_rows(&root(), &f.writer_key).expect("bootstrap rows");
        tokio::time::timeout(
            Duration::from_secs(2),
            fleet_bootstrap(&f.store, &plane, Some(rows)),
        )
        .await
        .expect("the bootstrap must not wait on the nest");
        let own = f
            .store
            .state(
                KIND_DEVICE_SET,
                &fauna_core::hex32::encode(&device_id_of(US)),
            )
            .await
            .unwrap();
        assert!(own.is_some(), "the enrollment is durable locally");
    }

    /// A value of any type, for a closure that is never called
    /// ([`future_size_of`]).
    fn never<T>() -> T {
        unreachable!("a future-size probe is typed, never run")
    }

    /// The size of the future `make` would return. `make` is not called: the
    /// size is its return type's, so a probe needs no store, driver or nest.
    fn future_size_of<F>(_make: impl FnOnce() -> F) -> usize {
        std::mem::size_of::<F>()
    }

    /// The largest future the store thread may hold inline. Measured 2026-10-01
    /// (aarch64 linux, a debug build): [`worker`] 53,504 bytes, the serve inside
    /// it 48,544.
    const STORE_THREAD_FUTURE_BYTES_MAX: usize = 64 * 1024;

    /// **The store thread's futures stay small** (`native-async-execution.md`
    /// § The rule: a future-size assertion, not a stack-overflow test). The
    /// thread holds [`worker`]'s future inline in its `block_on`, and the
    /// serve's inside that. An unoptimized build pays for that size several
    /// times over in stack: `block_on` moves the future by value through three
    /// frames (262,592 bytes of them for these 53,504, read off a debug app).
    ///
    /// ⚠ **This catches a future that grows, and nothing else.** A future's
    /// size is the largest of its states; a poll frame on an unoptimized build
    /// is the sum of its temporaries. Un-boxing the seed pass leaves both
    /// sizes here where they are (measured: the serve 48,544 bytes either way)
    /// and puts 334,624 bytes more stack under a full pass. The depth is
    /// [`a_pass_runs_inside_half_the_store_threads_stack`]'s to watch.
    ///
    /// Red-verified by lowering the bound to 32 KiB.
    #[test]
    fn the_store_threads_futures_stay_small() {
        let worker_bytes = future_size_of(|| worker::<FakeNest>(never(), never(), never()));
        let serve_bytes = future_size_of(|| {
            never::<&'static mut AccountDriver>().serve(
                never::<Assembly<'static, SqliteBackend, FakeNest>>(),
                never::<&'static mut NativeLegs>(),
                never::<&'static FileElection>(),
                never(),
                never(),
            )
        });
        for (what, bytes) in [("worker", worker_bytes), ("serve", serve_bytes)] {
            assert!(
                bytes <= STORE_THREAD_FUTURE_BYTES_MAX,
                "the store thread's {what} future is {bytes} bytes — expected <= \
                 {STORE_THREAD_FUTURE_BYTES_MAX}. It is held inline on the store thread's \
                 stack; box the future that grew at its call site (the seed pass's \
                 `Box::pin` in `AccountDriver::serve` is the shape) before raising the bound. \
                 Owner: docs/goal/architecture/apps/native-async-execution.md § The rule."
            );
        }
    }

    /// **A pass runs inside half the store thread's stack**, measured where it
    /// is deepest in a nest request: [`FakeNest`] records how far below the
    /// thread's entry each request is polled.
    ///
    /// The future-size assertion above cannot see this. On an unoptimized
    /// build the stack goes to poll frames, not to the future: `serve`'s poll
    /// frame alone is 865,456 bytes, `serve_local_cmd`'s 584,736 and `pump`'s
    /// 365,712 (read off a debug `fauna-desktop`'s prologues, 2026-10-01), each the sum
    /// of every temporary in a long `async fn`, because an unoptimized build
    /// gives each its own slot. So this drives the real thread through a full
    /// pass and a seed pass and reads the depth. Measured 2026-10-01 on a
    /// debug build (aarch64 linux): a full pass 1,834,320 bytes deep, a seed
    /// pass 1,268,320 — with no linked nest, which the fake does not serve:
    /// that leg is asserted against the same bound in `bins/fauna-nest`'s
    /// `conformance_account_plane_bind`, over real in-process nests
    /// ([`store_thread_stack_depth`]).
    ///
    /// Half the stack, because a request is not the deepest leaf: the store's
    /// SQLite calls, the crypto under a mint and a local command served
    /// inside a pass (which runs under `drive_pass`, beside the pass rather
    /// than below a request) all sit outside what a request can observe. A build whose passes reach half the stack at their
    /// requests has no stated headroom for those. **Returning
    /// [`STORE_THREAD_STACK_BYTES`] to std's 2 MiB default turns this red on a
    /// debug build**: this is the constant's own regression test.
    ///
    /// Red-verified with the constant at 3 MiB (a full pass, 1,834,320 bytes
    /// against 1,572,864).
    #[tokio::test]
    async fn a_pass_runs_inside_half_the_store_threads_stack() {
        const BUDGET: usize = STORE_THREAD_STACK_BUDGET;
        let within = |what: &str, depth: usize| {
            assert!(
                depth > 0,
                "{what}: no nest request was polled on the store thread — the probe is dark"
            );
            assert!(
                depth <= BUDGET,
                "{what}: a nest request is polled {depth} bytes below the store thread's \
                 entry — expected <= {BUDGET}, half of STORE_THREAD_STACK_BYTES. The poll \
                 frames above it grew (an unoptimized build sums every temporary of a long \
                 `async fn` into its frame): box the future that grew, or split the \
                 function, before raising the stack. Owner: \
                 docs/goal/architecture/apps/native-async-execution.md § The rule."
            );
        };

        // An engine holder: the prologue, a preference write and its publish
        // step, a full pass.
        let fx = fixture();
        let holder = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the engine holder");
        holder.reconcile_now().await.expect("pass");
        holder
            .put_preference(KIND_MODERATION, moderation_value(&["probe"]))
            .await
            .expect("a preference write");
        holder.reconcile_now().await.expect("pass");
        holder.shutdown().await;
        within("a full pass", fx.fake.deepest_request());

        // A seed-leg holder beside a seedless engine holder: the seed pass.
        let fx = fixture();
        let sign_in = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("first enrollment");
        sign_in.reconcile_now().await.expect("pass");
        sign_in.shutdown().await;
        let agent = AccountStoreRuntime::start(AccountRuntimeParams {
            principal: RuntimePrincipal::Seedless,
            ..fx.params("a")
        })
        .await
        .expect("the seedless agent");
        agent.reconcile_now().await.expect("the agent's pass");
        // The agent idles from here (its backstop is disarmed), so what the
        // probe sees next is the app's.
        fx.fake.forget_deepest_request();
        let app = AccountStoreRuntime::start(fx.params("a"))
            .await
            .expect("the app beside it");
        let report = app.reconcile_now().await.expect("the app's seed pass");
        assert!(report.skipped_non_holder && !app.is_engine_holder());
        app.shutdown().await;
        agent.shutdown().await;
        within("a seed pass", fx.fake.deepest_request());
    }
}
