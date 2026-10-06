//! The shared desktop **sync-agent control client** — the platform-neutral core
//! of the per-user `fauna-sync-agent` provisioning + folder-binding surface
//! (`sync-agent.md` § Control plane split + § Credential model; plan D3/D4).
//!
//! One implementation, every desktop consumer (priority #2): the linux GTK
//! client and fauna-tui link it directly (no FFI hop — `sync-agent.md`
//! § Consumers), and `fauna-ffi`'s `FfiSyncAgentProvisioner` is a thin UniFFI
//! adapter over it for FaunaKit/macOS and the windows app. The cross-process
//! contract every host shares (`RefreshBearerOutcome` tri-state + the pinned
//! no-capability error prefix) lives in `fauna_ipc::convergence`, not here.
//!
//! What lives here:
//!
//! * [`SyncAgentProvisioner`] — name the machine's `sync_devices` row, run the
//!   shared convergence loop over the per-user agent endpoint, and drive the
//!   folder-binding verbs (`bind_location`/`unbind_location`/`list_locations`) +
//!   `unprovision`.
//! * [`crate::register_this_machine`] — the provisioner's one nest leg: the
//!   machine's named row under its derived device id and sealed label. It mints
//!   no credential — the store principal, minted by the enrollment ceremony, is
//!   the machine's only renewal credential (`sync-agent-credentials.md`
//!   § Credential model → the RULED 2026-09-28 block).
//! * [`LocationBindingsModel`] — the pure optimistic-UI ⇄ agent-truth reconcile
//!   state machine every app folder UI needs (union semantics: a row that
//!   failed to push stays rendered and re-pushes on the next reconcile; the
//!   2026-07-19 A4 review faces (a)/(b)).
//!
//! The module (and its `fauna-ipc`/`tokio` deps) is `#[cfg(any(unix, windows))]`
//! — every desktop reaches the agent through [`AgentEndpoint`], which resolves
//! the unix socket on linux/macOS and the per-SID named pipe on windows; wasm
//! has no agent at all.

use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use fauna_core::identity::ActorKeypair;
use fauna_ipc::convergence::{
    self, DEFAULT_SPAWN_GRACE, DEFAULT_TICK_INTERVAL, ProvisioningDelegate, RefreshBearerOutcome,
};
use fauna_ipc::endpoint::AgentEndpoint;
use fauna_ipc::sync::{
    BearerToken, LocationInfo, RequestMethod, ResponsePayload, ResponseResult, SyncCapability,
};
use fauna_protocol::RpcRequester;

/// Hook: **launch the agent** when the socket probe finds it absent. Platform
/// glue — `launchctl` kick / LaunchAgent self-install on macOS, systemd user
/// unit ensure (or an e2e direct spawn) on linux. Best-effort: a failed spawn
/// just means the next tick retries.
pub trait AgentSpawner: Send + Sync {
    /// Start the agent if it is not already running. Called from the
    /// convergence loop's probe step when the socket connect fails.
    fn spawn_agent(&self);
}

/// Budget a **one-shot** agent command spends bringing the agent up before it
/// gives up — the one-shot twin of the convergence tick's
/// probe → spawn → [`DEFAULT_SPAWN_GRACE`] step (`fauna_ipc::convergence::tick`).
///
/// Only the verbs with nothing behind them to retry pay this
/// ([`ProvisionerInner::agent_request_ensuring_agent`] names them): the
/// convergence loop re-probes every [`DEFAULT_TICK_INTERVAL`], the binding model
/// re-pushes a failed bind on the next reconcile, and `PullFolderNow` has the
/// rescan tick — all of which tolerate losing one race against their own spawn.
/// A confirm modal does not: it fires once. On windows the agent is a separate
/// process whose named pipe does not exist until it has started, and
/// `sync_pipe_client`'s `ERROR_PIPE_BUSY` retry explicitly does **not** cover
/// that `ERROR_FILE_NOT_FOUND`, so without this wait the command fails outright
/// with *"agent unreachable … The system cannot find the file specified"* —
/// measured on Windows 2026-08-27 against fauna-tui, the same class as the windows
/// app's own leg.
///
/// Ten seconds is generous because it is only ever paid when the agent is
/// genuinely absent: in production the installer's HKLM `Run` key (windows) and
/// the systemd user unit (linux) mean it is already up, and a cold process start
/// on a loaded box is the slowest thing this crate waits for (the e2e harness's
/// own pre-spawn allows 20 s for it). It stays bounded, so a box where the agent
/// *cannot* start fails the command rather than hanging it.
pub const AGENT_START_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How often [`AGENT_START_WAIT`] re-probes the endpoint while the agent starts.
/// Cheap to poll: a connect to an absent socket/pipe fails immediately on both
/// transports, and the common case (the agent was already starting) costs one
/// poll.
const AGENT_START_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Connect for a one-shot command, **ensuring the agent is running first**:
/// probe → spawn-if-absent → re-probe until `budget` is spent. Yields the first
/// successful connection, or the *last* connect error once the budget runs out.
///
/// Blocking by design — every caller already runs its exchange on the tokio
/// blocking pool, because the `SyncPipeClient` frame exchange is blocking.
///
/// [`AgentSpawner::spawn_agent`] is called at most once, and is idempotent by
/// contract (`ChildSpawner::spawn_command` gates on a still-live child, and the
/// agent's own `InstanceLock` makes a duplicate exit harmlessly), so this can
/// never race the convergence loop into a second agent process.
///
/// `terminal` names the connect errors that are **answers, not misses** — a
/// refusal no amount of waiting or spawning can turn into a connection. It
/// returns immediately, without spawning. Today that is exactly the windows
/// server-identity refusal
/// ([`fauna_ipc::sync_pipe_client::is_server_identity_refusal`]): the pipe
/// namespace is machine-wide, so a local account holding our name both keeps our
/// agent from starting (its `FILE_FLAG_FIRST_PIPE_INSTANCE` create fails) and
/// answers in its place — and retrying that would spend the whole budget on
/// spawn → die → re-probe only to report *"agent unreachable"* about a machine
/// whose agent is fine.
///
/// Generic over the connect step so the retry policy is unit-testable without a
/// live socket — the endpoint's own connect is its only production argument.
fn connect_ensuring_agent<T, E>(
    mut connect: impl FnMut() -> Result<T, E>,
    spawner: &dyn AgentSpawner,
    budget: std::time::Duration,
    poll: std::time::Duration,
    terminal: impl Fn(&E) -> bool,
) -> Result<T, E> {
    let mut last = match connect() {
        Ok(connected) => return Ok(connected),
        Err(e) if terminal(&e) => return Err(e),
        Err(e) => e,
    };
    spawner.spawn_agent();
    let deadline = std::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(last);
        }
        std::thread::sleep(poll.min(remaining));
        match connect() {
            Ok(connected) => return Ok(connected),
            Err(e) if terminal(&e) => return Err(e),
            Err(e) => last = e,
        }
    }
}

/// Re-exported so an implementor of [`ProvisioningBearerSource`] can name the
/// type that trait's signature returns without taking its own dependency on
/// `fauna-ipc`. A public trait whose method type you cannot spell without an
/// unrelated crate edge is a leaky seam — every app leg either grew that edge
/// or hand-rolled the adapter beside its own import.
pub use fauna_ipc::sync::BearerToken as ProvisioningBearerToken;

/// Hook: the app's **current nest bearer**, read fresh each convergence tick and
/// pushed to the agent (`RefreshBearer`). `None` ⇒ not authenticated (the tick
/// skips).
pub trait ProvisioningBearerSource: Send + Sync {
    fn current_bearer(&self) -> impl Future<Output = Option<BearerToken>> + Send;
}

/// Hook: observe the agent-reachability false→true edge
/// ([`ProvisioningDelegate::agent_became_reachable`]) — the client's cue to
/// re-drive its folder-binding reconcile (and anything else it queued while the
/// agent was down). Called from the convergence loop's tokio task; schedule
/// onto your UI thread yourself.
pub trait ReachabilityObserver: Send + Sync {
    fn on_agent_reachable(&self);
}

/// Error from a provisioner control-channel call: the agent was unreachable,
/// answered with an error, or the exchange failed. [`Self::kind`] tells the
/// version-skew answers apart from the rest (`sync-agent.md` § Local agent
/// health): an agent that answered in a shape this build cannot read is alive,
/// and must never read as "not running".
#[derive(Debug)]
pub struct AgentControlError {
    message: String,
    kind: AgentControlErrorKind,
}

/// What kind of failure an [`AgentControlError`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentControlErrorKind {
    /// Anything else: unreachable, an agent-side error, a failed exchange, an
    /// unexpected payload.
    Other,
    /// The agent answered with a reply this build cannot decode — it is newer
    /// than this app ([`fauna_ipc::sync_pipe_client::ReplyNotUnderstood`]).
    UnreadableReply,
    /// The agent refused the verb as one it cannot name — it is older than
    /// this app ([`fauna_ipc::sync::UNSUPPORTED_METHOD_ERROR_MESSAGE`]).
    UnsupportedMethod,
}

impl AgentControlError {
    /// An [`AgentControlErrorKind::Other`] failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self::with_kind(AgentControlErrorKind::Other, message)
    }

    fn with_kind(kind: AgentControlErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
        }
    }

    /// Which failure this is.
    pub fn kind(&self) -> AgentControlErrorKind {
        self.kind
    }
}

impl std::fmt::Display for AgentControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AgentControlError {}

/// Local sync-agent process health, derived client-side from
/// [`SyncAgentProvisioner::get_service_status`] — the `sync-agent-status`
/// global shell element (`sync-agent.md` § Local agent health). Distinct from
/// [`fauna_ipc::sync::ConnectionState`], which is the agent's own
/// nest-capability state, not local-process reachability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentHealthState {
    /// The `GetServiceStatus` call itself failed — no local socket / connect
    /// error.
    NotRunning,
    /// Reachable, but the running process reports an older version than this
    /// client's own build: an update replaced the on-disk binary without the
    /// still-running agent picking it up (§ Local agent health — the
    /// per-platform reality differs; e.g. always-expected on Linux, negligible
    /// on Windows/macOS's primary channels). Also the reading when the agent
    /// answered `GetServiceStatus` in a shape this build cannot read, or
    /// refused it as unknown: something answered, and the two ends are
    /// different releases.
    RestartPending,
    /// Reachable, but the agent reports a bound set whose engine holds a
    /// content-key generation behind the set's floor, or was built keyless
    /// because custody could not be read — after a removal, the new generation
    /// has not reached custody yet. Writes to that set are held meanwhile, never
    /// sealed under the older generation. Read off the agent's own status reply
    /// ([`fauna_ipc::sync::ServiceStatusInfo::keys_pending`]; `sync-agent.md`
    /// § Local agent health → *Keys pending*).
    KeysPending,
    /// Reachable, but the agent reports that the nest holds no grant for this
    /// machine any more — it was removed from the account's devices, the nest
    /// was reset, or its enrollment never registered — so nothing syncs once
    /// the app is closed. Read off the agent's own status reply
    /// ([`fauna_ipc::sync::ServiceStatusInfo::needs_reenrollment`], which stays
    /// `true` until a renewal succeeds; `sync-agent.md` § Local agent health →
    /// *Not enrolled*).
    NotEnrolled,
    /// Reachable and running the same version as this client's own build.
    Running,
}

/// Derive [`AgentHealthState`] from a `GetServiceStatus` attempt — pass the
/// result of [`SyncAgentProvisioner::get_service_status`] as it came back. A
/// failure is *Not running* unless the agent did answer, in a shape this build
/// cannot read or as a refusal of the verb
/// ([`AgentControlErrorKind::UnreadableReply`] /
/// [`AgentControlErrorKind::UnsupportedMethod`]): that is the version-skew
/// window itself, and reads as *Restart pending*. *Keys pending* is the reply's own
/// [`keys_pending`](fauna_ipc::sync::ServiceStatusInfo::keys_pending), derived
/// agent-side where the truth is, and *Not enrolled* its
/// [`needs_reenrollment`](fauna_ipc::sync::ServiceStatusInfo::needs_reenrollment).
/// The full five-state derivation lives here so every app driving an agent
/// shares one implementation, precedence included:
/// Not running > Not enrolled > Keys pending > Restart pending > Running.
/// A pending generation outranks
/// a version mismatch because it is the one state with a security consequence
/// and a clock, and *Not enrolled* outranks both because it is the one reading
/// that waits on the user and never ends by itself while the app is closed.
pub fn agent_health_state(
    status: &Result<fauna_ipc::sync::ServiceStatusInfo, AgentControlError>,
    local_build_version: &str,
) -> AgentHealthState {
    match status {
        Err(e) => match e.kind() {
            AgentControlErrorKind::UnreadableReply | AgentControlErrorKind::UnsupportedMethod => {
                AgentHealthState::RestartPending
            }
            AgentControlErrorKind::Other => AgentHealthState::NotRunning,
        },
        Ok(s) if s.needs_reenrollment => AgentHealthState::NotEnrolled,
        Ok(s) if s.keys_pending => AgentHealthState::KeysPending,
        Ok(s) if s.version == local_build_version => AgentHealthState::Running,
        Ok(_) => AgentHealthState::RestartPending,
    }
}

/// Whether the per-user agent is currently serving **≥1 sync engine** — the
/// `data.sync.running` reading every desktop app's e2e state block reports
/// (`sync-agent.md` § Implementation status today: *"`running` = any hosted
/// engine reports `serving`"*).
///
/// One bounded **blocking** exchange on the local endpoint. An agent that is
/// unreachable, or that answers anything other than an [`ResponsePayload::Engines`]
/// payload, is simply `false` — "not running" and "cannot be asked" are the same
/// reading to a polling consumer, and neither is an error worth surfacing here.
///
/// ⚠ **NEVER call this from an e2e state provider — use
/// [`any_engine_serving_cached`].** This doc used to say the opposite ("callable
/// from a synchronous state-report path"), and that claim cost three separate
/// investigations: the state provider *is* the test agent's ack path, so a
/// round trip here is paid by every command acknowledgement. When the agent
/// connects but does not answer — a wedged agent, and a windows named pipe
/// connects to one happily — the exchange costs the full
/// [`fauna_ipc::sync_pipe_client::REQUEST_TIMEOUT`] of 6 s, which is longer than
/// `driver.call_command`'s 5 s ack budget, so *no* command could be acknowledged
/// at all. It presents as an unrelated feature's command being silently dropped,
/// never as "the state push is slow" (`e2e-conventions.md` § convention 14
/// build-out → the windows leg; the invariant is in § convention 11's entry:
/// **the e2e state provider does no blocking I/O**).
///
/// What is left for this blocking form: a caller that genuinely wants a fresh
/// answer and is not on an ack path or a UI thread.
///
/// Shared rather than per-app for the same reason [`agent_health_state`] above
/// is (priority #2): linux, fauna-tui and the windows probe each need this exact
/// derivation, and a client that re-derives it drifts — a `.all()` instead of an
/// `.any()`, or a connect error promoted to a panic, silently changes what every
/// `_wait_for_engine`-backed e2e means.
///
/// **Gated off the e2e path only by its callers**, not here: this is an ordinary
/// read of the control plane, and the `#[cfg(any(debug_assertions,
/// feature = "e2e-agent"))]` gate belongs on the state-JSON assembly that
/// consumes it (convention 15).
///
/// The I/O-free half is [`engines_are_serving`], which is where the unit tests
/// live — the socket exchange here has nothing to assert that a live agent on the
/// developer's box would not flip.
pub fn any_engine_serving() -> bool {
    probe_serving(fauna_ipc::sync_pipe_client::REQUEST_TIMEOUT)
}

/// Ceiling on the **liveness** exchange specifically, as opposed to the
/// [`fauna_ipc::sync_pipe_client::REQUEST_TIMEOUT`] (6 s) a real verb gets.
///
/// `ListEngines` is a local, in-memory answer over a socket on this same box —
/// sub-millisecond whenever the agent is answering at all. The 6 s ceiling is
/// sized for verbs that do nest-backed work (`ListFileVersions`), and inheriting
/// it here only decides how long a **wedged** agent keeps its stale reading
/// alive. Two seconds is still ~1000× the honest answer's cost, so no loaded box
/// times out a live agent, while a wedged one is re-probed promptly.
///
/// This bound is the *second* guard, not the first: the cache below is what
/// keeps the wait off the caller. A bound alone would still tax every state push
/// (2 s is no more ackable than 6 s), which is why both exist.
pub const LIVENESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// One bounded liveness exchange against a **named** endpoint. The seam the
/// tests drive: a `listening-but-silent` socket is the only way to reproduce
/// the wedged-agent case that produced the 6 s (an *absent* endpoint fails
/// `connect()` instantly on unix and proves nothing).
fn probe_serving_at(endpoint: &AgentEndpoint, timeout: std::time::Duration) -> bool {
    let Ok(client) = endpoint.connect() else {
        return false;
    };
    match client.request_with_timeout(RequestMethod::ListEngines, timeout) {
        Ok(response) => match response.result {
            ResponseResult::Ok(ResponsePayload::Engines(engines)) => engines_are_serving(&engines),
            _ => false,
        },
        Err(_) => false,
    }
}

/// [`probe_serving_at`] against this user's own agent endpoint.
fn probe_serving(timeout: std::time::Duration) -> bool {
    let Ok(endpoint) = AgentEndpoint::default_for_user() else {
        return false;
    };
    probe_serving_at(&endpoint, timeout)
}

/// A last-known-answer cache with a one-at-a-time background refresh — the
/// shape that makes a liveness read safe to call from a path that must not
/// block (an e2e state provider, a UI thread).
///
/// **Read semantics: answer now, ask for later.** [`read_and_refresh`] returns
/// the last observed value immediately and dispatches the next probe, so the
/// published answer trails the agent by at most one caller poll — sound because
/// every consumer of this reading polls (`conftest.py::_wait_for_engine`,
/// `test_sync_live_apply.py::_wait_for_sync`), and strictly *fresher* than a
/// blocking read on any run where the probe would have timed out, which
/// answered `false` six seconds late.
///
/// **Why it lives here rather than in each app** (priority #2/#4): windows and
/// macOS each independently hand-rolled this exact cache — a volatile field plus
/// an interlocked in-flight guard there, a `@MainActor` static plus a
/// `refreshing` bool there — after being bitten by the blocking form, while
/// linux and fauna-tui still called it raw and were saved only by unix
/// `connect()` failing fast on a *missing* socket. One wedged-but-listening
/// agent is all that separated them from the same bug, so the cache belongs
/// underneath all of them, once.
///
/// [`read_and_refresh`]: LivenessCache::read_and_refresh
pub struct LivenessCache {
    /// Last value a completed probe observed. `false` before the first one,
    /// which is also the truthful answer for "no agent yet".
    last: std::sync::atomic::AtomicBool,
    /// A probe is out. Keeps frequent polling from stacking socket connects.
    refreshing: std::sync::atomic::AtomicBool,
}

impl Default for LivenessCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LivenessCache {
    pub const fn new() -> Self {
        Self {
            last: std::sync::atomic::AtomicBool::new(false),
            refreshing: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The cached answer, plus a request for a fresher one.
    ///
    /// **Never performs I/O on the calling thread** — that is the whole point.
    /// `probe` runs on a spawned thread and only when no other refresh is out;
    /// otherwise it is dropped un-run, because the in-flight one is about to
    /// answer the same question.
    ///
    /// `&'static self` because the refresh outlives the call. The one global is
    /// [`any_engine_serving_cached`]'s; tests leak their own.
    pub fn read_and_refresh<F>(&'static self, probe: F) -> bool
    where
        F: FnOnce() -> bool + Send + 'static,
    {
        use std::sync::atomic::Ordering;
        if self
            .refreshing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            // The guard is released by `Drop`, so a probe that PANICS cannot
            // wedge the cache into "forever refreshing" — which would silently
            // freeze the reading at whatever it last was.
            struct Guard(&'static std::sync::atomic::AtomicBool);
            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
            std::thread::spawn(move || {
                let _guard = Guard(&self.refreshing);
                let answer = probe();
                self.last.store(answer, Ordering::SeqCst);
            });
        }
        self.last.load(Ordering::SeqCst)
    }

    /// Whether a refresh is out. Test-only: the in-flight guard is an
    /// implementation detail no consumer branches on.
    #[cfg(test)]
    fn refresh_in_flight(&self) -> bool {
        self.refreshing.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The process-wide liveness cache behind [`any_engine_serving_cached`].
static AGENT_LIVENESS: LivenessCache = LivenessCache::new();

/// **The liveness read every state provider calls** — [`any_engine_serving`]'s
/// answer without its wait.
///
/// Returns the last known reading immediately and schedules the next probe
/// (bounded by [`LIVENESS_TIMEOUT`]); see [`LivenessCache`] for why the answer
/// trailing by one poll is sound, and [`any_engine_serving`] for the measured
/// cost of the blocking spelling on an ack path.
pub fn any_engine_serving_cached() -> bool {
    AGENT_LIVENESS.read_and_refresh(|| probe_serving(LIVENESS_TIMEOUT))
}

/// The `running` derivation over an already-fetched engine roster: **any** engine
/// serving, not all (an agent hosting one live set and one planned-but-not-started
/// set *is* running). Split out of [`any_engine_serving`] so this — the part that
/// can silently drift — is unit-testable without a socket.
pub fn engines_are_serving(engines: &[fauna_ipc::sync::EngineInfo]) -> bool {
    engines.iter().any(|e| e.serving)
}

/// What the account registry knows about this identity's ancestry, as
/// [`corpus_reseal_progress`] needs it.
///
/// Two facts, not one, because the difference between them is the whole
/// `OwedElsewhere` arm: an account that never succeeded and a successor's device
/// that holds no predecessor seed both produce **no engine record at all** (the
/// pass returns before touching the DB in either case), and only the registry
/// can tell them apart. Resolve both from the same walk the capability's retired
/// keys come from (`AccountRegistry::predecessors_of` /
/// `::predecessor_backup_keys`) rather than from two sources that can disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuccessionCorpusContext {
    /// This identity succeeded from at least one predecessor.
    pub succeeded: bool,
    /// This device holds at least one predecessor's seed material, so it is
    /// capable of doing the work rather than waiting on a device that is.
    pub holds_predecessor_material: bool,
}

/// What a [`CorpusResealProgress`] settles to once it isn't `Running`/`Failed`
/// — [`fauna_core::progress::Passage`]'s `O`. Folds the old standalone
/// `OwedElsewhere` and `Settled { resealed, owed }` arms under this one
/// `Settled` payload, per the shared type's two-tier shape: `Passage::Running`/
/// `Passage::Failed` render identically for every re-key leg; only what
/// happens once settled is per-domain.
///
/// [`Self::OwedElsewhere`] carries the one genuinely confusable reading, and it
/// is confusable in the direction that scares people: a device that never held
/// the predecessor's seed is the **ordinary** state, not damage, and a client
/// that phrased it as corruption would tell a user their files are broken at the
/// exact moment the fix is "sign in where you took your account back".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusResealOutcome {
    /// This device holds no predecessor material, so it cannot move a byte. Owed
    /// by a device that does — never damage; see the variant doc above.
    OwedElsewhere,
    /// Every hosted set's pass settled. `owed` is the total the passes examined
    /// and could not move; it retries at the next catch-up.
    Resealed { resealed: u64, owed: u64 },
}

/// i18n keys for [`CorpusResealProgress::status_line`] — `settings.recovery_kit.*`,
/// beside the four sibling aftermath lines this renders under.
const KEY_CORPUS_RESEAL_RUNNING: &str = "settings.recovery_kit.corpus_reseal_running";
const KEY_CORPUS_RESEAL_DONE: &str = "settings.recovery_kit.corpus_reseal_done";
const KEY_CORPUS_RESEAL_PARTLY_OWED: &str = "settings.recovery_kit.corpus_reseal_partly_owed";
const KEY_CORPUS_RESEAL_OWED_ELSEWHERE: &str = "settings.recovery_kit.corpus_reseal_owed_elsewhere";
const KEY_CORPUS_RESEAL_FAILED: &str = "settings.recovery_kit.corpus_reseal_failed";

impl fauna_core::progress::ProgressOutcome for CorpusResealOutcome {
    const RUNNING_KEY: &'static str = KEY_CORPUS_RESEAL_RUNNING;
    const FAILED_KEY: &'static str = KEY_CORPUS_RESEAL_FAILED;

    /// The partly-owed line is the only one carrying numbers, and deliberately:
    /// § Re-key scope asks for *progress*, and on this leg alone the corpus can
    /// be large enough that an unqualified "still working" says nothing. A
    /// settled pass with nothing moved and nothing owed renders **nothing** — it
    /// is the idempotent steady state every later catch-up reaches, and a line
    /// announcing it at every sign-in would train the user to ignore the one
    /// that matters.
    fn settled_line(&self) -> Option<fauna_core::localized::LocalizedText> {
        use fauna_core::localized::LocalizedText;
        match self {
            Self::OwedElsewhere => Some(LocalizedText::key(KEY_CORPUS_RESEAL_OWED_ELSEWHERE)),
            Self::Resealed { resealed, owed } if *owed > 0 => {
                let mut text = LocalizedText::key(KEY_CORPUS_RESEAL_PARTLY_OWED);
                text.args.insert("done".into(), resealed.to_string());
                text.args.insert("remaining".into(), owed.to_string());
                Some(text)
            }
            Self::Resealed { resealed, .. } if *resealed > 0 => {
                Some(LocalizedText::key(KEY_CORPUS_RESEAL_DONE))
            }
            Self::Resealed { .. } => None,
        }
    }

    /// § Re-key scope's *"resumed until complete"* is exactly "keep driving
    /// while this is true", and it is also the shape `sync-agent.md` bound (3)
    /// must **not** be enforced on: this is a report folded from progress
    /// records, and the bound is enforced on the drained
    /// `SyncDb::list_pending_current_root_reseal` list.
    fn still_owed(&self) -> bool {
        match self {
            Self::OwedElsewhere => true,
            Self::Resealed { owed, .. } => *owed > 0,
        }
    }
}

/// The post-succession corpus re-seal as a **progress surface** — the shared
/// projection all 7 apps render (`succession-aftermath.md` § Re-key scope: the
/// re-seal is *"started at first successor sign-in, **surfaced with progress,
/// resumed until complete**"*), and leg 5's sibling of
/// `ConfigResealProgress` / `BackupRegrantProgress` / `ReplicaResealProgress` /
/// `GrantRemintProgress` via the shared [`fauna_core::progress::Passage`].
///
/// It lives here, beside [`engines_are_serving`], for that function's exact
/// reason: on desktop the pass runs in the **agent** and the line renders in the
/// **app**, so every app folds the same `ListEngines` roster into the same
/// reading, and a client that re-derives it drifts. The producing half is
/// `fauna_sync_engine::succession_progress::CorpusResealPass`, which this crate
/// cannot name (the engine depends on *this* crate, not the reverse) — hence the
/// `fauna_ipc` mirror in between.
pub type CorpusResealProgress = fauna_core::progress::Passage<CorpusResealOutcome>;

/// Fold every hosted engine's last re-seal pass into the **one** line the user
/// sees, given what the registry knows about this identity's ancestry.
///
/// The account is one corpus spread over several sets, so the surface is one
/// line rather than one per set — a user does not think in folders, and a
/// per-set list would grow a row for a distinction they never made.
///
/// Precedence is most-actionable-first: a failure on any set outranks a pass
/// running on another (the failure is the thing they can act on), and a running
/// pass outranks the settled ones (the totals are not final yet).
///
/// Returns `None` — render nothing — in three cases, all of which are honestly
/// silent: the identity never succeeded; every set's pass settled with nothing
/// moved and nothing owed (the steady state); or the agent has not recorded a
/// pass yet, where a line would be guessing at work that may not have started.
pub fn corpus_reseal_progress(
    engines: &[fauna_ipc::sync::EngineInfo],
    context: SuccessionCorpusContext,
) -> Option<CorpusResealProgress> {
    use fauna_ipc::sync::CorpusResealInfo as Wire;
    if !context.succeeded {
        return None;
    }
    if !context.holds_predecessor_material {
        // No engine will ever record a pass here — the pass returns before
        // touching the DB with no retired keys — so this must be decided from
        // the registry or the successor is told nothing at all.
        return Some(CorpusResealProgress::Settled(
            CorpusResealOutcome::OwedElsewhere,
        ));
    }
    // A pass state a newer agent names and this build cannot read renders
    // nothing: it is no recorded pass, never a guess at one (`transport.md`
    // § Rule 3 in full).
    let records: Vec<&Wire> = engines
        .iter()
        .filter_map(|e| e.corpus_reseal.as_ref())
        .filter(|r| !matches!(r, Wire::Unknown(_)))
        .collect();
    if records.is_empty() {
        return None;
    }
    if let Some(reason) = records.iter().find_map(|r| match r {
        Wire::Failed { reason } => Some(reason.clone()),
        _ => None,
    }) {
        return Some(CorpusResealProgress::Failed(reason));
    }
    if records.iter().any(|r| matches!(r, Wire::Running)) {
        return Some(CorpusResealProgress::Running);
    }
    let (resealed, owed) = records.iter().fold((0, 0), |(r, o), record| match record {
        Wire::Settled { resealed, owed } => (r + resealed, o + owed),
        // Unreachable: both other arms returned above. Folding them as zero
        // keeps this total honest rather than panicking on a shape change.
        _ => (r, o),
    });
    Some(CorpusResealProgress::Settled(
        CorpusResealOutcome::Resealed { resealed, owed },
    ))
}

/// **The bound-(3) license** — may this device's agent be re-provisioned
/// *without* the account's retired owner keys?
///
/// `sync-agent.md` § Credential model bound (3): the keys are pushed only while
/// a re-seal is owed, and the capability is re-provisioned without them once the
/// corpus is re-sealed. Its enforcement design (§ Credential model → *Bound
/// (3)'s enforcement design*, ratified 2026-08-05) makes this a **recomputable
/// predicate, never a latch** — recomputed from a fresh `ListEngines` answer at
/// every provision, so an event that re-creates need (binding a
/// predecessor-era set, a second succession) flips it back and the keys return
/// on a following tick. That recoverability is why an automatic drop is
/// acceptable at all; it holds only because the account registry keeps the
/// retired seeds indefinitely and ruling 6 keeps every irreversible consumer off
/// this license.
///
/// **Every arm fails toward pushing.** `false` — keys stay — whenever the answer
/// is anything but a positive proof from every hosted engine:
///
/// - **An empty roster is not a license.** An agent hosting no engines has shown
///   no drain at all; treating "no counter-example" as proof is exactly the
///   vacuity ruling 5 exists to stop. It also costs nothing: a device with no
///   bound sets holds no corpus, so the keys sit unused until a set binds and
///   arms the predicate honestly.
/// - **A missing per-engine answer is not a license** — a set whose DB could not be
///   opened has nothing to say, and an engine reporting no drain answers nothing.
/// - **One half is not a license.** A drained-looking list on an engine that has
///   never folded is the fresh-device successor, whose list is empty because it
///   has no rows yet (`fauna_sync_engine::succession_drain` states this in full).
///
/// ⚠ The asymmetry that fixes the direction: a wrong "not yet" costs a retained
/// key on one device; a wrong "done" costs the corpus, permanently, for
/// everyone. This is the only place the two halves are combined, so no app can
/// get it wrong independently.
pub fn predecessor_keys_may_be_dropped(engines: &[fauna_ipc::sync::EngineInfo]) -> bool {
    !engines.is_empty()
        && engines.iter().all(|e| {
            e.reseal_drain
                .is_some_and(|d| d.folded && d.nothing_owed && d.all_at_rest_classified)
        })
}

/// [`predecessor_keys_may_be_dropped`] applied to a raw `ListEngines` reply —
/// the one step between the agent's answer and the license.
///
/// Split out of [`AgentEndpointDelegate::license_to_drop_predecessor_keys`] so
/// it is reachable from a test: what remains around it is the connect + request
/// that no unit test can drive, and every decision this leg makes lives here.
/// Anything that is not an `Engines` payload — a typed error, an unreachable answer,
/// or any other payload — is *cannot tell*, which
/// fails toward pushing exactly like an unreachable agent.
fn license_from_engines_answer(result: &ResponseResult) -> bool {
    match result {
        ResponseResult::Ok(ResponsePayload::Engines(engines)) => {
            predecessor_keys_may_be_dropped(engines)
        }
        _ => false,
    }
}

/// The capability inputs the identity-holding client supplies once at
/// construction (`sync-agent.md` § Credential model: the agent is bearer-only —
/// it gets the `BackupKey` + a bearer, never the identity seed).
pub struct AgentCapabilityInputs {
    /// The app's identity seed (32 bytes) — used **in-process only** to seal
    /// the device label and derive the actor id; never sent over the socket.
    pub identity_secret: Vec<u8>,
    /// Owner `BackupKey` (32 bytes) pushed in the provisioned capability.
    pub backup_key: Vec<u8>,
    /// Retired owner `BackupKey`s of the identities this account **succeeded
    /// from**, nearest hop first — 32 bytes each, empty for every identity that
    /// never succeeded (`sync-agent.md` § Credential model → *Retired owner keys
    /// after an identity succession*, ratified 2026-08-04).
    ///
    /// The agent is the process that opens this account's bytes on desktop, and a
    /// successor's corpus is still sealed under these; on a fresh device — the
    /// case a succession exists for — *every* chunk is. Pushed as **read**
    /// candidates only: they reach `FileDownloadKeys::predecessor_backup_keys`
    /// and no seal root consults that field.
    ///
    /// Resolve them from `AccountRegistry::predecessor_backup_keys` rather than
    /// deriving per app — that walk is the one shared resolution, and its failure
    /// mode (a dropped row) is silent.
    ///
    /// Supply the account's full retired-key list here regardless of whether the
    /// re-seal is finished: bound (3) of the ratification — omitting them once
    /// the corpus is re-sealed — is enforced *per push* by
    /// [`predecessor_keys_may_be_dropped`], not by the caller pre-filtering this
    /// field. The license is recomputed from a live agent answer every tick and
    /// must be able to put the keys **back**, which it can only do if the
    /// delegate still holds them.
    pub predecessor_backup_keys: Vec<Vec<u8>>,
    /// The **attested** actor ids of the identities this account succeeded
    /// from, nearest hop first — 32 bytes each, empty for every identity that
    /// never succeeded (`account-data-taxonomy.md` § The generation machinery →
    /// *The source of `prior`*, ruled 2026-09-13; the IPC field is
    /// `SyncCapability::predecessor_actor_ids`).
    ///
    /// The agent hosts this account's runtime seedless and holds no registry,
    /// so it cannot attest a predecessor itself: this list IS its `prior` for
    /// the fleet view — the succession-crossing signer allow-list. Resolve
    /// it from `AccountRegistry::attested_predecessor_actor_ids` (the ids of
    /// the rows whose seeds this device holds), the same walk the keys above
    /// come from, never from a replica's writer-asserted list.
    ///
    /// **Not key material, and not under bound (3):** the ids ride on every
    /// push even after the drop license omits the keys, because a
    /// predecessor-signed `Enrolled` row is never re-signed and the trust needs
    /// them for the device's life. Empty is fail-safe at the agent —
    /// predecessor-signed enrollments drop out of its view, nothing is admitted.
    pub predecessor_actor_ids: Vec<Vec<u8>>,
    /// The retired owner keys **paired with the identities they belong to**,
    /// `(actor id, key)` per hop, 32 bytes each, nearest hop first — resolve
    /// from `AccountRegistry::predecessor_backup_keys_by_actor` (the IPC field
    /// is `SyncCapability::predecessor_keys_by_actor`). The per-signer bound
    /// needs the pairing: a row signed as predecessor A is offered only A's
    /// own root and its predecessors' (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (8)(c)), and an unpaired key is
    /// never offered to such a row. Key material under bound (3) exactly like
    /// [`Self::predecessor_backup_keys`]: pushed and dropped with them. Empty
    /// keeps today's unpaired behaviour.
    pub predecessor_keys_by_actor: Vec<(Vec<u8>, Vec<u8>)>,
    /// This device's stable sync device id, hex — the same id the in-process
    /// engines used, so the nest sees one device.
    pub device_id: String,
    /// The platform device label (`fauna-linux` / `fauna-macos` / …).
    pub device_label: String,
    /// The nest URL the agent should dial (the capability carries it).
    pub nest_url: String,
}

/// The shared desktop control channel to the external `fauna-sync-agent`:
/// capability provisioning *plus* device-local folder↔set binding. See the
/// module docs; `fauna-ffi`'s `FfiSyncAgentProvisioner` wraps this 1:1.
pub struct SyncAgentProvisioner<R, B>
where
    R: RpcRequester + Clone + Send + Sync + 'static,
    B: ProvisioningBearerSource + 'static,
{
    inner: Arc<ProvisionerInner<R, B>>,
}

struct ProvisionerInner<R, B> {
    /// The identity session's authenticated connection — used once to name the
    /// machine's row. Never handed to the agent.
    nest: R,
    identity_secret: Zeroizing<Vec<u8>>,
    backup_key: Zeroizing<Vec<u8>>,
    /// Flattened retired owner keys (`len % 32 == 0`) — see
    /// [`AgentCapabilityInputs::predecessor_backup_keys`]. Flattened here (rather
    /// than kept as a `Vec<Vec<u8>>`) so one `Zeroizing` covers all of it: a
    /// vector-of-vectors leaves each inner buffer un-wiped when the outer one
    /// reallocates.
    predecessor_backup_keys: Zeroizing<Vec<u8>>,
    /// Flattened attested predecessor actor ids (`len % 32 == 0`) — see
    /// [`AgentCapabilityInputs::predecessor_actor_ids`]. Public material, so
    /// no zeroize; flattened only to match its sibling's shape on the wire.
    predecessor_actor_ids: Vec<u8>,
    /// Flattened `(actor id ‖ key)` 64-byte records — see
    /// [`AgentCapabilityInputs::predecessor_keys_by_actor`]; one `Zeroizing`
    /// for the same reason as its unpaired sibling's.
    predecessor_keys_by_actor: Zeroizing<Vec<u8>>,
    device_id: String,
    device_label: String,
    nest_url: String,
    spawner: Arc<dyn AgentSpawner>,
    bearer_source: Arc<B>,
    /// Optional agent-up edge observer, fixed at construction (every folder UI
    /// should pass one — attach-time reconcile alone races the first agent
    /// bootstrap).
    reachability: Option<Arc<dyn ReachabilityObserver>>,
    /// The per-user agent endpoint (`fauna_ipc::endpoint::AgentEndpoint` — unix
    /// socket or named pipe), resolved once at construction. Resolving once
    /// matters: the windows arm reads the harness pipe override from the
    /// environment, and a per-exchange re-read could split one session across
    /// two agents.
    endpoint: AgentEndpoint,
    /// Nudge the loop to converge immediately (sign-in, new binding, nest move).
    poke: Arc<Notify>,
    /// Stop the loop (sign-out / account-switch / drop).
    cancel: Arc<Notify>,
    /// The running convergence task, if `start()` was called. Taken + awaited by
    /// `unprovision()`.
    task: Mutex<Option<JoinHandle<()>>>,
}

impl<R, B> SyncAgentProvisioner<R, B>
where
    R: RpcRequester + Clone + Send + Sync + 'static,
    R::Error: std::fmt::Display,
    B: ProvisioningBearerSource + 'static,
{
    /// Build the provisioner. `reachability` is optional — pass an observer to
    /// re-drive binding reconciles on the agent-up edge (every folder UI should;
    /// attach-time reconcile alone races the first agent bootstrap).
    pub fn new(
        nest: R,
        inputs: AgentCapabilityInputs,
        spawner: Arc<dyn AgentSpawner>,
        bearer_source: Arc<B>,
        reachability: Option<Arc<dyn ReachabilityObserver>>,
    ) -> Result<Self, AgentControlError> {
        if inputs.identity_secret.len() != 32 {
            return Err(AgentControlError::new(
                "identity secret must be exactly 32 bytes",
            ));
        }
        if inputs.backup_key.len() != 32 {
            return Err(AgentControlError::new(
                "backup key must be exactly 32 bytes",
            ));
        }
        // Reject a malformed predecessor here rather than silently dropping it:
        // the whole leg exists to stop a readable corpus presenting as corrupt,
        // and a quietly-omitted retired key reproduces exactly that.
        if inputs.predecessor_backup_keys.iter().any(|k| k.len() != 32) {
            return Err(AgentControlError::new(
                "every predecessor backup key must be exactly 32 bytes",
            ));
        }
        // Same discipline for the attested ids: a silently-dropped id would
        // drop that predecessor's every enrollment out of the agent's fleet
        // view with nothing to observe the mistake by.
        if inputs.predecessor_actor_ids.iter().any(|id| id.len() != 32) {
            return Err(AgentControlError::new(
                "every predecessor actor id must be exactly 32 bytes",
            ));
        }
        if inputs
            .predecessor_keys_by_actor
            .iter()
            .any(|(id, key)| id.len() != 32 || key.len() != 32)
        {
            return Err(AgentControlError::new(
                "every paired predecessor id and key must be exactly 32 bytes",
            ));
        }
        let endpoint = AgentEndpoint::default_for_user()
            .map_err(|e| AgentControlError::new(format!("resolve agent endpoint: {e}")))?;
        let poke = Arc::new(Notify::new());
        Ok(Self {
            inner: Arc::new(ProvisionerInner {
                nest,
                identity_secret: Zeroizing::new(inputs.identity_secret),
                backup_key: Zeroizing::new(inputs.backup_key),
                predecessor_backup_keys: Zeroizing::new(inputs.predecessor_backup_keys.concat()),
                predecessor_actor_ids: inputs.predecessor_actor_ids.concat(),
                predecessor_keys_by_actor: Zeroizing::new(
                    inputs
                        .predecessor_keys_by_actor
                        .iter()
                        .flat_map(|(id, key)| id.iter().chain(key.iter()).copied())
                        .collect(),
                ),
                device_id: inputs.device_id,
                device_label: inputs.device_label,
                nest_url: inputs.nest_url,
                spawner,
                bearer_source,
                reachability,
                endpoint,
                poke,
                cancel: Arc::new(Notify::new()),
                task: Mutex::new(None),
            }),
        })
    }

    /// Name the machine's row (one-time) and start the convergence loop.
    /// Safe to call once per active session; a second call while running just
    /// pokes the loop.
    pub async fn start(&self) -> Result<(), AgentControlError> {
        {
            // Already running → nudge and return (idempotent).
            let running = self.inner.task.lock().unwrap().is_some();
            if running {
                // Logged, because this return is the arm that looks like a
                // successful provision from every side: the desktop hosts'
                // ENTER/BUILT/INSTALLED brackets are all emitted BEFORE
                // `start()` is awaited, so a login that only ever takes this
                // return reads in the app log as a fully provisioned session
                // that merely never registered its row.
                tracing::info!(
                    "sync-agent provisioner: already running, poked (no register this call)"
                );
                self.inner.poke.notify_one();
                return Ok(());
            }
        }

        let identity = self.inner.identity()?;
        let actor_id = identity.actor_id().0.to_vec();
        // The provisioner's one nest leg: name the machine's row. It mints
        // nothing — the store principal, minted and registered on this same
        // row by the enrollment ceremony, is the machine's only renewal
        // credential (`sync-agent-credentials.md` § Credential model → the
        // RULED 2026-09-28 block, decisions 1–2). A register never touches the
        // grant columns and a grant register never touches the label, so the
        // two passes may land in either order.
        //
        // At `info`, deliberately: it fires once per provision (never a hot
        // path), and a host passing `""` registers a row nothing can address.
        // `device_id` is a public identifier (the nest row's own key, rendered
        // on the Devices page), within the redaction rule
        // (`apps/observability.md` § Persistence & privacy).
        tracing::info!(
            "sync-agent provisioner: naming this machine's row device_id={} (empty={})",
            self.inner.device_id,
            self.inner.device_id.is_empty(),
        );
        crate::register_this_machine(
            self.inner.nest.clone(),
            &identity,
            self.inner.device_id.clone(),
            self.inner.device_label.clone(),
        )
        .await;

        let delegate = AgentEndpointDelegate {
            endpoint: self.inner.endpoint.clone(),
            spawner: Arc::clone(&self.inner.spawner),
            bearer_source: Arc::clone(&self.inner.bearer_source),
            backup_key: self.inner.backup_key.clone(),
            predecessor_backup_keys: self.inner.predecessor_backup_keys.clone(),
            predecessor_actor_ids: self.inner.predecessor_actor_ids.clone(),
            predecessor_keys_by_actor: self.inner.predecessor_keys_by_actor.clone(),
            actor_id,
            nest_url: self.inner.nest_url.clone(),
            device_id: self.inner.device_id.clone(),
            provisioned_without_predecessor_keys: Arc::new(Mutex::new(None)),
            reachability: self.inner.reachability.clone(),
        };

        let handle = tokio::spawn(convergence::run(
            delegate,
            DEFAULT_TICK_INTERVAL,
            DEFAULT_SPAWN_GRACE,
            Arc::clone(&self.inner.poke),
            Arc::clone(&self.inner.cancel),
        ));
        *self.inner.task.lock().unwrap() = Some(handle);
        Ok(())
    }

    /// Nudge the loop to converge now (sign-in, a new folder binding, a nest
    /// move).
    pub fn poke(&self) {
        self.inner.poke.notify_one();
    }

    /// Bind a local folder to a nest folder on the agent: an idempotent
    /// `AddLocation` (a fresh path takes the agent's platform default mode —
    /// on-demand on windows, always-resident elsewhere, the agent's
    /// `LocationMode::fresh_binding_default`; re-adding an existing path is a
    /// no-op that keeps its persisted mode) followed by `SetLocationFolder`. The
    /// agent reconciles engines off its own config, so a successful bind starts
    /// the set's engine agent-side.
    ///
    /// Keyed by the set's **identity**: `folder_id` is its
    /// `fauna_core::folder_keys::FolderRef` in wire form
    /// (`fauna_client_folders::engine_binding::folder_ref_for_row`), the
    /// unambiguous key the agent resolves this binding's engine key material,
    /// engine-registry entry and state DB by (account-data-plane.md
    /// § The ratified decisions); `folder` travels as the label. A caller with
    /// no ref for a row refuses the bind rather than inventing one.
    ///
    /// A bind the agent rejects is an ordinary error. The name-keyed form of
    /// this verb, and the fallback to it for an agent predating the ref, were
    /// retired 2026-09-24 (the compat-remnant sweep, `version-compatibility.md`
    /// § Dimension 2, fourth exception): a still-running pre-upgrade agent
    /// surfaces as *Restart pending* plus a bind the reconcile retries, never
    /// as a silent downgrade to the ambiguous name key.
    pub async fn bind_location(
        &self,
        path: String,
        folder: String,
        folder_id: String,
    ) -> Result<(), AgentControlError> {
        self.inner
            .agent_request(RequestMethod::AddLocation { path: path.clone() })
            .await?;
        self.inner
            .agent_request(RequestMethod::SetLocationFolder {
                path,
                folder,
                folder_id,
            })
            .await?;
        Ok(())
    }

    /// Unbind a folder on the agent (`RemoveLocation`): its engine stops and
    /// the binding is forgotten; the nest folder and the engine's state DB are
    /// untouched, so re-binding resumes instead of re-uploading.
    pub async fn unbind_location(&self, path: String) -> Result<(), AgentControlError> {
        self.inner
            .agent_request(RequestMethod::RemoveLocation { path })
            .await?;
        Ok(())
    }

    /// Set a bound folder's sync mode (`SetLocationSyncMode`): `always` keeps every
    /// file materialized, `on-demand` serves placeholders that hydrate on access.
    /// The agent owns what a mode change means per platform — on windows it
    /// decides whether the cfapi placeholder host serves that root
    /// (`file-sync.md` § On-Demand Files); linux's FUSE root is future work and
    /// macOS/iOS keep on-demand in the File Provider extension, so this is only
    /// ever *sent* by a surface that offers the toggle.
    ///
    /// Separate from [`bind_location_with_ref`](Self::bind_location_with_ref) because
    /// mode is orthogonal to the binding: re-binding must not silently reset a
    /// folder's mode, and toggling mode must not re-push a binding.
    pub async fn set_location_sync_mode(
        &self,
        path: String,
        mode: String,
    ) -> Result<(), AgentControlError> {
        self.inner
            .agent_request(RequestMethod::SetLocationSyncMode { path, mode })
            .await?;
        Ok(())
    }

    /// Nudge the agent's resident engine serving `folder` to pull remote
    /// changes **now** (`RequestMethod::PullFolderNow`), off its rescan cadence.
    /// Sent when the client receives a `PushEvent::SyncChanged` for the set.
    /// Best-effort on the agent side — an absent engine or an already-pending
    /// pull is a silent no-op there, and the rescan tick is the backstop. Per
    /// `docs/goal/behavior/file-sync.md` § Remote-change nudge.
    ///
    /// Plain [`ProvisionerInner::agent_request`], deliberately: the rescan tick
    /// named above **is** the backstop, so a nudge that misses a starting agent
    /// is delayed rather than lost — and this fires off a push event, not a
    /// gesture, so an ensure here would block a fire-and-forget path and
    /// duplicate the convergence loop's spawn on every nudge.
    ///
    /// `folder_hash` is the push's own hash address, relayed as received — the
    /// agent matches its bindings by it, so a sealed set's nudge (blank name)
    /// still reaches its engine.
    pub async fn pull_folder_now(
        &self,
        folder: String,
        folder_hash: Option<Vec<u8>>,
    ) -> Result<(), AgentControlError> {
        self.inner
            .agent_request(RequestMethod::PullFolderNow {
                folder,
                folder_hash: folder_hash.map(fauna_ipc::sync::ByteBuf::from),
            })
            .await?;
        Ok(())
    }

    /// Ask the agent to run one full pass of the account store it has mounted,
    /// now (`RequestMethod::ReconcileAccountRuntime`) — the cross-process arm
    /// of the devices page's participation switch, sent after the switch
    /// rests the device-local row, so an agent-held same-account listener
    /// drops within the gesture (`docs/goal/behavior/p2p.md` § Per-device
    /// participation → *Enforcement*, (c) Promptness). The agent answers it as
    /// a no-op when it mounts no store or does not hold the engine.
    ///
    /// Plain [`ProvisionerInner::agent_request`], deliberately: the agent
    /// pump's own backstop (and every reconnect wake) re-reads the row, so a
    /// nudge that misses a starting agent is delayed rather than lost
    /// (`sync-agent.md` § *A verb with nothing behind it to retry*) — and an
    /// ensure here would hold the gesture through an agent spawn.
    pub async fn reconcile_account_runtime(&self) -> Result<(), AgentControlError> {
        self.inner
            .agent_request(RequestMethod::ReconcileAccountRuntime)
            .await?;
        Ok(())
    }

    /// What this device's sealed **custodian store** occupies on disk
    /// (`RequestMethod::GetCustodianStore`) — the read behind
    /// `backup-orphaned-store-row` (`docs/goal/ui/backups.md` § Manage backup
    /// destinations → *Reclaim this device's copy*).
    ///
    /// The agent is asked rather than the disk because the store's location is
    /// its authority to resolve (`sync-agent.md` § Control plane split), the
    /// same reason `GetShareServeInfo` exists. The reply says only what is
    /// there; **whether it is orphaned** is the caller's call, made with the
    /// destination rows it already holds through the shared
    /// `fauna_core::data::custodian_store_is_orphaned`.
    pub async fn custodian_store(
        &self,
    ) -> Result<fauna_ipc::sync::CustodianStoreInfo, AgentControlError> {
        let payload = self
            .inner
            .agent_request(RequestMethod::GetCustodianStore)
            .await?;
        match payload {
            fauna_ipc::sync::ResponsePayload::CustodianStore(info) => Ok(info),
            other => Err(AgentControlError::new(format!(
                "GetCustodianStore expected a CustodianStore, got {other:?}"
            ))),
        }
    }

    /// This device's own custodian store as the audit pass takes it
    /// (`fauna_client_backup::audit::run_audit_pass`'s `own_custodian`): the
    /// agent's [`Self::custodian_store`] reply's standing source regressions
    /// under `device_id`, the sync device id this agent registers under.
    ///
    /// `None` when the agent does not answer — "no store was read", which the
    /// pass treats as leaving the row's record standing, never as a clean
    /// store.
    pub async fn own_custodian_store(
        &self,
        device_id: &str,
    ) -> Option<fauna_client_backup::audit::OwnCustodianStore> {
        let info = self.custodian_store().await.ok()?;
        Some(own_custodian_store(device_id, &info))
    }

    /// Free this device's whole sealed custodian store
    /// (`RequestMethod::ReclaimCustodianStore`) — the confirmed
    /// `backup-destination-reclaim-button` action.
    ///
    /// **Awaited, not fire-and-forget**, unlike the binding mutations on this
    /// seam: it is a destructive gesture behind a confirm modal, so the page
    /// repaints on what actually happened — including the
    /// [`still_hosting`](fauna_ipc::sync::CustodianReclaimOutcome::still_hosting)
    /// refusal, which means nothing was deleted because the agent's own replica
    /// had not finished stopping. The await is also what lets the caller assert
    /// state rather than timing (`testing.md` convention 14).
    ///
    /// Idempotent on the agent side: reclaiming an empty store frees nothing and
    /// succeeds.
    ///
    /// [`ProvisionerInner::agent_request_ensuring_agent`], because a confirm
    /// modal fires once: nothing re-drives this, so an unreachable agent would
    /// spend the user's gesture on an error rather than delaying it.
    pub async fn reclaim_custodian_store(
        &self,
    ) -> Result<fauna_ipc::sync::CustodianReclaimOutcome, AgentControlError> {
        let payload = self
            .inner
            .agent_request_ensuring_agent(RequestMethod::ReclaimCustodianStore)
            .await?;
        match payload {
            fauna_ipc::sync::ResponsePayload::CustodianStoreReclaimed(outcome) => Ok(outcome),
            other => Err(AgentControlError::new(format!(
                "ReclaimCustodianStore expected a CustodianStoreReclaimed, got {other:?}"
            ))),
        }
    }

    /// Start the re-seed ceremony over this device's custodian store
    /// (`RequestMethod::ReseedCustodianStore`): the confirmed
    /// `backup-destination-reseed-button` action. Returns the job's state at
    /// once, `Running` in the ordinary case; [`Self::custodian_reseed`] reads
    /// how it ends.
    ///
    /// `nest_backup_key` is the owner's seed-derived `NestBackupKey`, handed
    /// over for this one job (`owner-key-material.md` § Path A-sibling-0).
    ///
    /// [`ProvisionerInner::agent_request_ensuring_agent`], because a confirm
    /// modal fires once: nothing re-drives it.
    pub async fn reseed_custodian_store(
        &self,
        nest_backup_key: [u8; 32],
    ) -> Result<crate::reseed_wire::ReseedJob, AgentControlError> {
        let payload = self
            .inner
            .agent_request_ensuring_agent(RequestMethod::ReseedCustodianStore(
                fauna_ipc::sync::CustodianReseedRequest::new(nest_backup_key),
            ))
            .await?;
        reseed_job_from(payload, "ReseedCustodianStore")
    }

    /// The covered-folder display names this device's custodian store learned
    /// (`RequestMethod::GetCustodianFolderNames`) — what a desktop re-seed
    /// pre-creates its target sets under before starting the job.
    pub async fn custodian_folder_names(&self) -> Result<Vec<String>, AgentControlError> {
        let payload = self
            .inner
            .agent_request_ensuring_agent(RequestMethod::GetCustodianFolderNames)
            .await?;
        match payload {
            fauna_ipc::sync::ResponsePayload::CustodianFolderNames(names) => Ok(names),
            other => Err(AgentControlError::new(format!(
                "GetCustodianFolderNames expected CustodianFolderNames, got {other:?}"
            ))),
        }
    }

    /// Read the re-seed job's state (`RequestMethod::GetCustodianReseed`).
    pub async fn custodian_reseed(
        &self,
    ) -> Result<crate::reseed_wire::ReseedJob, AgentControlError> {
        let payload = self
            .inner
            .agent_request_ensuring_agent(RequestMethod::GetCustodianReseed)
            .await?;
        reseed_job_from(payload, "GetCustodianReseed")
    }

    /// **Test-only.** Run exactly one custodian pull pass on the replica the
    /// agent is already hosting, and return what it did
    /// (`RequestMethod::CustodianRunPassNow`).
    ///
    /// The await *is* the barrier: the agent replies only once the pass has
    /// pulled, sealed, stored, audited-if-due and written its check-in — so a
    /// caller asserts state and never timing (`testing.md` convention 14). It
    /// exists because the production first pass is `PERIODIC_INTERVAL` away
    /// (`CustodianPull::run_loop` mutes the interval's immediate first tick),
    /// which no test may sleep out.
    ///
    /// A device that is not hosting answers
    /// [`CustodianPassReport::hosting`] `= false` rather than erroring, so a
    /// caller polls this instead of sleeping out the host loop's registry
    /// re-read. Compiled out of release artifacts (convention 15).
    ///
    /// [`ProvisionerInner::agent_request_ensuring_agent`], because a test's poke
    /// is the purest one-shot there is: it arrives right after login, when the
    /// convergence loop has only just spawned the agent, and it has no tick
    /// behind it. On a windows host that raced the agent's own start and failed
    /// the whole test with `ERROR_FILE_NOT_FOUND` (measured 2026-08-27) — a
    /// harness-visible instance of the production shape the two verbs above hit
    /// silently.
    ///
    /// `now_offset_secs` shifts the clock this one pass runs at (`0` = the real
    /// one). It exists for the slowest cadence a pass contains — the self-audit's
    /// 24-hour debounce — so a test can reach a store's *second* audit, the
    /// first one able to observe rot that appeared after enrollment. See
    /// [`RequestMethod::CustodianRunPassNow`] for why an offset rather than an
    /// absolute `now`.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub async fn custodian_run_pass_now(
        &self,
        now_offset_secs: i64,
    ) -> Result<fauna_ipc::sync::CustodianPassReport, AgentControlError> {
        let payload = self
            .inner
            .agent_request_ensuring_agent(RequestMethod::CustodianRunPassNow { now_offset_secs })
            .await?;
        match payload {
            fauna_ipc::sync::ResponsePayload::CustodianPassReport(report) => Ok(report),
            other => Err(AgentControlError::new(format!(
                "CustodianRunPassNow expected a CustodianPassReport, got {other:?}"
            ))),
        }
    }

    /// Apply the deletes the **mass-delete floor** is holding on `folder`
    /// (`RequestMethod::ApplyHeldDeletes`) — the explicit user action behind the
    /// `folder-location-apply-deletes-button` confirm. Propagation of a held set
    /// is never automatic (`delete-propagation.md` § A wholesale-vanished folder
    /// is infrastructure failure).
    ///
    /// ⚠ **Pass the set, never the count the UI rendered.** The agent re-derives
    /// what is missing at click time, so a confirm racing a remount deletes
    /// nothing; [`fauna_ipc::sync::HeldDeletesAppliedInfo`] carries back what
    /// actually happened (`applied`, `remaining_held`, `floor_was_active`) and is
    /// what the app should repaint from — a stale rendered number is exactly what
    /// the re-derive exists to discard.
    ///
    /// [`ProvisionerInner::agent_request_ensuring_agent`], because the hold
    /// *stands* when this fails: unlike the binding mutations on this seam, which
    /// the optimistic model re-pushes on the next reconcile, nothing re-drives a
    /// refused confirm — the user must notice and click again.
    pub async fn apply_held_deletes(
        &self,
        folder: String,
    ) -> Result<fauna_ipc::sync::HeldDeletesAppliedInfo, AgentControlError> {
        match self
            .inner
            .agent_request_ensuring_agent(RequestMethod::ApplyHeldDeletes { folder })
            .await?
        {
            ResponsePayload::HeldDeletesApplied(info) => Ok(info),
            other => Err(AgentControlError::new(format!(
                "agent ApplyHeldDeletes returned an unexpected payload: {other:?}"
            ))),
        }
    }

    /// Where each bound folder's state DB and local tree live
    /// (`GetShareServeInfo`) — what the app-side share serve source opens by
    /// cross-process WAL read (`p2p.md` § Cross-user shared-set transfer,
    /// slice E). Path resolution stays the agent's one authority.
    pub async fn share_serve_info(
        &self,
    ) -> Result<Vec<fauna_ipc::sync::ShareServeFolderInfo>, AgentControlError> {
        match self
            .inner
            .agent_request(RequestMethod::GetShareServeInfo)
            .await?
        {
            ResponsePayload::ShareServeInfo(list) => Ok(list),
            other => Err(AgentControlError::new(format!(
                "agent GetShareServeInfo returned an unexpected payload: {other:?}"
            ))),
        }
    }

    /// Hand one accepted page of peer-served share rows to the resident
    /// engine serving `folder` for provisional ingest (`ShareIngest`): `rows`
    /// are canonical dag-cbor `fauna.peer.share` change encodings, bodies
    /// pre-fetched into `spool_dir` (`spool/manifests/<hex>` +
    /// `spool/chunks/<hex>`; a spool miss skips that row, never the page).
    /// The outcome carries the engine's report and the advanced per-peer pull
    /// cursor — the pump's next `since`.
    pub async fn share_ingest(
        &self,
        folder: String,
        folder_id: String,
        proven_actor_hex: String,
        rows: Vec<Vec<u8>>,
        spool_dir: String,
    ) -> Result<fauna_ipc::sync::ShareIngestOutcome, AgentControlError> {
        match self
            .inner
            .agent_request(RequestMethod::ShareIngest {
                folder,
                folder_id,
                proven_actor_hex,
                rows: rows
                    .into_iter()
                    .map(fauna_ipc::sync::ByteBuf::from)
                    .collect(),
                spool_dir,
            })
            .await?
        {
            ResponsePayload::ShareIngested(outcome) => Ok(outcome),
            other => Err(AgentControlError::new(format!(
                "agent ShareIngest returned an unexpected payload: {other:?}"
            ))),
        }
    }

    /// The agent's current sync folders (`ListLocations`) — the truth the
    /// Settings folder-binding UI reconciles against post-cutover.
    pub async fn list_locations(&self) -> Result<Vec<LocationInfo>, AgentControlError> {
        match self
            .inner
            .agent_request(RequestMethod::ListLocations)
            .await?
        {
            ResponsePayload::Locations(list) => Ok(list),
            other => Err(AgentControlError::new(format!(
                "agent ListLocations returned an unexpected payload: {other:?}"
            ))),
        }
    }

    /// The agent's hosted engines (`ListEngines`) — one row per bound folder,
    /// with its backlog and its post-succession re-seal progress.
    ///
    /// The provisioner-bound twin of the free [`any_engine_serving`], which opens
    /// its own socket because it is called from synchronous state-report paths. A
    /// caller holding a provisioner already has the endpoint, and needs the rows
    /// themselves rather than one boolean folded out of them — the aftermath's
    /// corpus line is folded from these by [`corpus_reseal_progress`].
    pub async fn list_engines(
        &self,
    ) -> Result<Vec<fauna_ipc::sync::EngineInfo>, AgentControlError> {
        match self.inner.agent_request(RequestMethod::ListEngines).await? {
            ResponsePayload::Engines(list) => Ok(list),
            other => Err(AgentControlError::new(format!(
                "agent ListEngines returned an unexpected payload: {other:?}"
            ))),
        }
    }

    /// The agent's own process health (`GetServiceStatus`) — version, uptime,
    /// capability-connection state, and sync state. Feeds the `sync-agent-status`
    /// global shell element via [`agent_health_state`] (`sync-agent.md` §
    /// Local agent health). A connect failure (agent not running) surfaces as
    /// `Err`, which callers map to [`AgentHealthState::NotRunning`].
    ///
    /// **Never [`ProvisionerInner::agent_request_ensuring_agent`]**, even though
    /// an app calls it on a gesture: a health read that starts the agent can
    /// never report [`AgentHealthState::NotRunning`] truthfully, because it would
    /// be reporting on what it just started.
    pub async fn get_service_status(
        &self,
    ) -> Result<fauna_ipc::sync::ServiceStatusInfo, AgentControlError> {
        match self
            .inner
            .agent_request(RequestMethod::GetServiceStatus)
            .await?
        {
            ResponsePayload::ServiceStatus(info) => Ok(info),
            other => Err(AgentControlError::new(format!(
                "agent GetServiceStatus returned an unexpected payload: {other:?}"
            ))),
        }
    }

    /// Stop the loop and tear down the agent's provisioned capability
    /// (`UnprovisionCapability`): the agent deletes its persisted
    /// credential-store record and stops engines. Idempotent — safe on an
    /// already-stopped provisioner. Drives sign-out and account-switch teardown.
    ///
    /// **The durable half runs first.** The push below is best-effort by
    /// necessity (the agent may be wedged, or the connect may time out under
    /// load), and what it revokes is built to outlive it: the capability
    /// persists in the secure store, the bearer self-renews app-dead off a grant
    /// with no expiry, and both resume across reboots. So this records the
    /// sign-out where the agent will find it on its own —
    /// [`fauna_ipc::sync::SignedOutMarker`] — *before* sending a message that
    /// may never arrive. That marker, not the message, is what makes
    /// `on-demand-files.md` § Multi-account × File Provider consequence 1 true
    /// when delivery fails.
    pub async fn unprovision(&self) -> Result<(), AgentControlError> {
        // Stop the convergence loop first, so it cannot re-provision after
        // teardown.
        self.inner.cancel.notify_one();
        let handle = self.inner.task.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        // The durable record of the sign-out, written before the push. Its own
        // failure modes are silent (`SecretStore::set` is infallible), which is
        // exactly why the reconcile is ordered rather than latched: a marker
        // that never lands leaves today's behavior, and one that lingers after a
        // later sign-in is outranked by that provision's stamp.
        self.record_signed_out();
        // Best-effort teardown push. A dead agent (nothing to tear down) is
        // success.
        //
        // **But it is logged either way**, because "asked and it landed" and
        // "asked and the message was lost" are the two halves of
        // `on-demand-files.md` § Multi-account × File Provider consequence 1 —
        // *is asked to unprovision, and made to reconcile if the asking fails* —
        // and swallowing the result made them indistinguishable from every
        // vantage point at once. The agent logs only on ARRIVAL, so a lost push
        // left no trace anywhere in the fleet: a windows e2e reading the agent's
        // log for the teardown line saw an empty window and could not tell a
        // failed push from an app that never asked (measured 2026-09-11 — three candidate causes, all
        // three silent, and the session had to instrument every one to find out
        // which). Still no retry: the durable marker above plus the agent's own
        // signed-out reconcile are the designed backstop, by that same §.
        let endpoint = self.inner.endpoint.clone();
        match tokio::task::spawn_blocking(move || {
            let client = endpoint.connect()?;
            client.request(RequestMethod::UnprovisionCapability)
        })
        .await
        {
            // A delivered request can still carry the agent's own refusal, so the
            // reply is inspected rather than assumed — "the write succeeded" is not
            // "the capability is gone".
            Ok(Ok(response)) => match response.result {
                ResponseResult::Ok(_) => {
                    tracing::info!("teardown push delivered: agent un-provisioned")
                }
                ResponseResult::Err(e) => tracing::warn!(
                    error = %e,
                    "teardown push reached the agent and was REFUSED — same exposure \
                     as an undelivered push"
                ),
            },
            Ok(Err(e)) => tracing::warn!(
                error = %e,
                "teardown push NOT delivered (agent unreachable, or the round trip \
                 failed) — the signed-out marker is the backstop; the agent may still \
                 be serving this account until its own reconcile"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                "teardown push task failed to run — same exposure as an undelivered push"
            ),
        }
        Ok(())
    }

    /// Write the sign-out marker into the agent's credential-store namespace —
    /// the one location both processes resolve identically (`CredentialStore::new`
    /// applies the same `FAUNA_KEYRING_APP` override on both sides, so an e2e
    /// redirect moves the capability and its marker together).
    ///
    /// Best-effort and deliberately non-fatal: a machine whose store cannot be
    /// written is no worse off than before this reconcile existed.
    fn record_signed_out(&self) {
        use fauna_client_accounts::SecretStore;
        let Ok(identity) = self.inner.identity() else {
            tracing::warn!("sign-out marker: no identity to attribute the sign-out to; skipping");
            return;
        };
        let marker = fauna_ipc::sync::SignedOutMarker::new(
            identity.actor_id().0.to_vec(),
            fauna_core::data::Timestamp::now_millis_or_zero(),
        );
        let Some(record) = marker.encode_record() else {
            tracing::warn!("sign-out marker: encode failed; skipping");
            return;
        };
        let store = fauna_credential_store::CredentialStore::new(fauna_ipc::sync::CRED_NAMESPACE);
        store.set(fauna_ipc::sync::SIGNED_OUT_KEY, &record);
        tracing::info!("sign-out recorded for the sync agent's signed-out reconcile");
    }
}

/// The agent's store reply as the audit pass takes it — the one conversion
/// from the pipe's [`fauna_ipc::sync::CustodianStoreInfo`] to
/// `fauna_client_backup::audit::OwnCustodianStore`, for every shell that reads
/// the store through an agent.
pub fn own_custodian_store(
    device_id: &str,
    info: &fauna_ipc::sync::CustodianStoreInfo,
) -> fauna_client_backup::audit::OwnCustodianStore {
    fauna_client_backup::audit::OwnCustodianStore {
        device_id: device_id.to_string(),
        source_regressions: info
            .source_regressions
            .iter()
            .map(|r| fauna_client_backup::audit::StoreSourceRegression {
                ledger: r.ledger.clone(),
                held: r.held,
                served: r.served,
                observed_at: r.observed_at,
            })
            .collect(),
    }
}

/// The two re-seed verbs answer the same payload; anything else is a peer that
/// does not speak them.
fn reseed_job_from(
    payload: fauna_ipc::sync::ResponsePayload,
    verb: &str,
) -> Result<crate::reseed_wire::ReseedJob, AgentControlError> {
    match payload {
        fauna_ipc::sync::ResponsePayload::CustodianReseed(state) => {
            Ok(crate::reseed_wire::job_from_state(state))
        }
        other => Err(AgentControlError::new(format!(
            "{verb} expected a CustodianReseed, got {other:?}"
        ))),
    }
}

/// Read this machine's persisted [`fauna_ipc::sync::SignedOutMarker`] — the
/// durable record [`SyncAgentProvisioner::unprovision`] writes before its own
/// best-effort push. `None` when nothing has signed out here, or the record
/// is unreadable (treated as absent, never as a veto — the marker's own
/// contract).
pub fn read_signed_out_marker() -> Option<fauna_ipc::sync::SignedOutMarker> {
    use fauna_client_accounts::SecretStore;
    let store = fauna_credential_store::CredentialStore::new(fauna_ipc::sync::CRED_NAMESPACE);
    let record = store.get(fauna_ipc::sync::SIGNED_OUT_KEY)?;
    fauna_ipc::sync::SignedOutMarker::decode_record(&record)
}

/// The onboarding-reconcile decision, pure: does this machine's local
/// sign-out record license unprovisioning the agent's currently-advertised
/// principal? See [`signed_out_onboarding_reconcile`] for the IO around it
/// and the ordering this guards against — a sibling app's live account B must
/// never be torn down because app A's onboarding screen remembers signing out
/// of A.
fn onboarding_reconcile_licensed(
    marker: &fauna_ipc::sync::SignedOutMarker,
    held_actor_hex: Option<&str>,
) -> bool {
    let Some(marker_actor) = marker.actor_id_array() else {
        return false;
    };
    held_actor_hex == Some(hex::encode(marker_actor).as_str())
}

/// Shape (a) of `sync-agent.md` § Credential model → *The signed-out
/// reconcile* (ratified 2026-08-12, row 239 addendum): call once when the app
/// renders **signed out** (the onboarding surface, no account) so a reachable
/// agent still serving the account that just signed out here stops
/// immediately, rather than waiting for its own renewal-loop cadence.
///
/// **The guard, not a convenience.** Needs no identity or capability of its
/// own — but an unconditional `UnprovisionCapability` would let a sibling app
/// sitting at account A's onboarding screen tear down a co-resident account
/// B's live sync (the agent's slot is single). So this only fires when the
/// agent's own [`RequestMethod::GetServiceStatus`] advertisement
/// (`store_principal_actor` — the actor the agent can currently verify itself
/// against) matches the actor this machine itself recorded signing out.
/// Every ambiguous read — no local marker, an unreachable agent, no
/// advertised actor, or a mismatched one — is a no-op and leaves the agent
/// exactly as before: the renewal loop's own `signed_out_reconcile` remains
/// the backstop within its own bound.
pub async fn signed_out_onboarding_reconcile(endpoint: &AgentEndpoint) {
    let Some(marker) = read_signed_out_marker() else {
        return;
    };
    signed_out_onboarding_reconcile_with_marker(endpoint, &marker).await;
}

/// [`signed_out_onboarding_reconcile`] with the marker supplied rather than
/// read from this process's ambient credential store — the same guard, the
/// same two exchanges, only the *source* of the marker differs. Apps call the
/// `read_signed_out_marker`-backed wrapper above; this exists because the
/// wrapper's store is resolved from process-global environment
/// (`CredentialStore::new`), which a test cannot redirect without mutating env
/// that its parallel siblings share — so the cross-process half of the
/// mechanism was reachable only in production. Splitting the ambient read from
/// the guarded act is what lets the tier_3 proof drive a REAL spawned agent
/// (`bins/fauna-sync-agent/tests/agent_process_tier3.rs`) instead of leaving
/// the round trip covered by unit tests of the predicate alone.
pub async fn signed_out_onboarding_reconcile_with_marker(
    endpoint: &AgentEndpoint,
    marker: &fauna_ipc::sync::SignedOutMarker,
) {
    let status_endpoint = endpoint.clone();
    let status = tokio::task::spawn_blocking(move || {
        let client = status_endpoint.connect()?;
        client.request(RequestMethod::GetServiceStatus)
    })
    .await;
    let Ok(Ok(response)) = status else {
        return;
    };
    let ResponseResult::Ok(ResponsePayload::ServiceStatus(info)) = response.result else {
        return;
    };
    if !onboarding_reconcile_licensed(marker, info.store_principal_actor.as_deref()) {
        return;
    }
    let unprovision_endpoint = endpoint.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let client = unprovision_endpoint.connect()?;
        client.request(RequestMethod::UnprovisionCapability)
    })
    .await;
    tracing::info!(
        "onboarding-surface signed-out reconcile un-provisioned a reachable agent \
         still serving this machine's own signed-out account"
    );
}

impl<R, B> ProvisionerInner<R, B>
where
    R: RpcRequester + Clone + Send + Sync + 'static,
    B: ProvisioningBearerSource,
{
    /// The identity keypair from the app's seed.
    fn identity(&self) -> Result<ActorKeypair, AgentControlError> {
        let secret: [u8; 32] = self
            .identity_secret
            .as_slice()
            .try_into()
            .map_err(|_| AgentControlError::new("identity secret must be exactly 32 bytes"))?;
        Ok(ActorKeypair::from_secret(secret))
    }

    /// One bounded request/response exchange with the agent over its endpoint,
    /// on the blocking pool (the `SyncPipeClient` frame exchange is blocking).
    /// Connect failure, an agent-side `Err`, and a join panic all surface as
    /// [`AgentControlError`].
    ///
    /// **A connect failure is the answer, returned at once** — the right shape
    /// for every verb something already re-drives: the convergence tick, the
    /// status poll loop, the binding reconcile's own re-push, and a page that
    /// simply re-renders. For the acting one-shots that get exactly one attempt,
    /// see [`Self::agent_request_ensuring_agent`].
    async fn agent_request(
        &self,
        method: RequestMethod,
    ) -> Result<ResponsePayload, AgentControlError> {
        self.agent_exchange(method, None).await
    }

    /// [`Self::agent_request`] **plus the ensure step** — probe →
    /// spawn-if-absent → re-probe within [`AGENT_START_WAIT`]
    /// ([`connect_ensuring_agent`]).
    ///
    /// **For a verb that ACTS on a user (or test) gesture with nothing behind it
    /// to retry**: the user pressed *Pull now*, and a refused act is a lost
    /// gesture, not a delayed one. Deliberately *not* for the loop-driven verbs
    /// — the convergence tick re-probes in 30 s, the binding model re-pushes a
    /// failed bind on the next reconcile, and a read's caller re-renders — where
    /// the wait would buy nothing and the spawn would duplicate the loop's own.
    async fn agent_request_ensuring_agent(
        &self,
        method: RequestMethod,
    ) -> Result<ResponsePayload, AgentControlError> {
        self.agent_exchange(method, Some(Arc::clone(&self.spawner)))
            .await
    }

    /// The shared body of [`Self::agent_request`] and
    /// [`Self::agent_request_ensuring_agent`] — `ensure` carries the spawner
    /// when the caller wants the agent brought up, and is `None` otherwise.
    async fn agent_exchange(
        &self,
        method: RequestMethod,
        ensure: Option<Arc<dyn AgentSpawner>>,
    ) -> Result<ResponsePayload, AgentControlError> {
        let endpoint = self.endpoint.clone();
        let verb = method.name();
        let result = tokio::task::spawn_blocking(move || {
            let client = match ensure {
                Some(spawner) => connect_ensuring_agent(
                    || endpoint.connect(),
                    spawner.as_ref(),
                    AGENT_START_WAIT,
                    AGENT_START_POLL,
                    fauna_ipc::sync_pipe_client::is_server_identity_refusal,
                )?,
                None => endpoint.connect()?,
            };
            client.request(method)
        })
        .await;
        match result {
            Ok(exchange) => exchange_outcome(verb, exchange),
            Err(e) => Err(AgentControlError::new(format!(
                "agent {verb} task failed: {e}"
            ))),
        }
    }
}

impl<R, B> Drop for ProvisionerInner<R, B> {
    fn drop(&mut self) {
        // Belt-and-suspenders: stop a still-running loop if the object is
        // dropped without an explicit unprovision (the caller should always
        // unprovision).
        self.cancel.notify_one();
    }
}

/// One finished exchange, read as the control client's answer. The two
/// version-skew answers get their own [`AgentControlErrorKind`] — an agent
/// older than this app refusing the verb, and a reply too new to read — so no
/// caller mistakes a live agent of another release for an absent one.
fn exchange_outcome(
    verb: &str,
    exchange: std::io::Result<fauna_ipc::sync::Response>,
) -> Result<ResponsePayload, AgentControlError> {
    match exchange {
        Ok(response) => match response.result {
            ResponseResult::Ok(payload) => Ok(payload),
            ResponseResult::Err(msg) if fauna_ipc::sync::is_unsupported_method_refusal(&msg) => {
                Err(AgentControlError::with_kind(
                    AgentControlErrorKind::UnsupportedMethod,
                    format!("the sync agent is older than this app and cannot {verb}: restart it"),
                ))
            }
            ResponseResult::Err(msg) => Err(AgentControlError::new(format!(
                "agent {verb} failed: {msg}"
            ))),
        },
        Err(e) if fauna_ipc::sync_pipe_client::is_reply_not_understood(&e) => {
            Err(AgentControlError::with_kind(
                AgentControlErrorKind::UnreadableReply,
                format!("agent {verb}: {e}"),
            ))
        }
        Err(e) => Err(AgentControlError::new(format!(
            "agent unreachable for {verb}: {e}"
        ))),
    }
}

/// The shared [`ProvisioningDelegate`] the convergence loop drives — the
/// platform-neutral endpoint delegate every desktop consumer runs (FaunaKit and
/// the windows app via the `fauna-ffi` adapter, linux GTK and fauna-tui
/// directly). Holds the capability inputs + the platform hooks; every agent call
/// is bounded and runs on the tokio blocking pool (the `SyncPipeClient` frame
/// exchange is blocking on both transports).
pub struct AgentEndpointDelegate<B> {
    endpoint: AgentEndpoint,
    spawner: Arc<dyn AgentSpawner>,
    bearer_source: Arc<B>,
    backup_key: Zeroizing<Vec<u8>>,
    /// Flattened retired owner keys — carried into every capability this delegate
    /// pushes **while the bound-(3) license is unmet**, so a re-provision (a
    /// bearer refresh, a content-key generation bump) never silently drops a
    /// successor's read fallback and leaves the agent looking at a corpus it
    /// stopped being able to open. Omitted once
    /// [`predecessor_keys_may_be_dropped`] holds over a fresh `ListEngines`
    /// answer — see [`Self::license_to_drop_predecessor_keys`].
    predecessor_backup_keys: Zeroizing<Vec<u8>>,
    /// Flattened attested predecessor actor ids — carried into **every**
    /// capability this delegate pushes, licensed or not: they are the agent's
    /// fleet-view `prior`, not key material, and bound (3) reaches the keys alone
    /// ([`AgentCapabilityInputs::predecessor_actor_ids`]).
    predecessor_actor_ids: Vec<u8>,
    /// Flattened paired retired keys — carried and dropped exactly with
    /// [`Self::predecessor_backup_keys`]
    /// ([`AgentCapabilityInputs::predecessor_keys_by_actor`]).
    predecessor_keys_by_actor: Zeroizing<Vec<u8>>,
    actor_id: Vec<u8>,
    nest_url: String,
    device_id: String,
    /// Whether the last **successful** push this session omitted the retired
    /// owner keys — `None` until this session's first push, which is what makes
    /// [`needs_reprovision`] provision once per session even over an agent that
    /// holds a persisted capability (the capability carries this session's
    /// bearer and provision stamp, which the persisted one predates). After
    /// that, the bound-(3) license is recomputed from a live `ListEngines` answer
    /// each tick, and a disagreement with what the agent currently holds is a
    /// re-push — in *either* direction, so a device that drains drops the keys
    /// and a device that re-arms (a predecessor-era set binds, a second
    /// succession) gets them back. Advanced only on success, so a failed push
    /// retries. `Arc` so `full_provision`'s owned-capture future can advance it
    /// without borrowing `self` across the await.
    ///
    /// [`needs_reprovision`]: ProvisioningDelegate::needs_reprovision
    provisioned_without_predecessor_keys: Arc<Mutex<Option<bool>>>,
    reachability: Option<Arc<dyn ReachabilityObserver>>,
}

impl<B> AgentEndpointDelegate<B> {
    /// **The bound-(3) license, recomputed from a live agent answer** — whether
    /// this push may omit the account's retired owner keys
    /// (`sync-agent.md` § Credential model → *Bound (3)'s enforcement design*).
    ///
    /// Fails toward pushing at every step: a delegate that holds no retired keys
    /// answers `true` trivially (nothing to omit, so no query is worth a round
    /// trip); an agent that cannot be reached, or answers anything but
    /// `Engines`, answers `false` and the keys stay.
    async fn license_to_drop_predecessor_keys(&self) -> bool {
        if self.predecessor_backup_keys.is_empty() {
            return true;
        }
        let endpoint = self.endpoint.clone();
        let answer = tokio::task::spawn_blocking(move || {
            let client = endpoint.connect()?;
            client.request(RequestMethod::ListEngines)
        })
        .await;
        match answer {
            Ok(Ok(response)) => license_from_engines_answer(&response.result),
            // Connect failed / join error: cannot tell → keys stay.
            _ => false,
        }
    }

    /// Assemble a fresh `SyncCapability` from the stored inputs + this tick's
    /// bearer.
    ///
    /// `drop_predecessor_keys` is the caller's freshly-computed bound-(3)
    /// license ([`Self::license_to_drop_predecessor_keys`]) — passed in rather
    /// than read here, so the value that decides the capability's contents is
    /// the same one recorded as provisioned.
    fn build_capability(
        &self,
        bearer: &BearerToken,
        drop_predecessor_keys: bool,
    ) -> SyncCapability {
        let cap = SyncCapability::new(
            self.backup_key.to_vec(),
            self.actor_id.clone(),
            self.nest_url.clone(),
            self.device_id.clone(),
            BearerToken::new(bearer.token.clone(), bearer.expires_at),
        )
        // Stamped on **every** capability this delegate pushes, not just the
        // first: the signed-out reconcile orders a provision against a sign-out
        // (`fauna_ipc::sync::capability_is_signed_out`), so a re-provision after
        // signing back in must outrank the marker the sign-out left behind. A
        // stamp that only rode the initial provision would leave every later
        // one looking older than it is.
        .with_provisioned_at_ms(fauna_core::data::Timestamp::now_millis_or_zero());
        // The attested predecessor ids ride on EVERY push — they are the
        // agent's fleet-view `prior`, public material outside bound (3)'s license,
        // and a predecessor-signed enrollment row never stops needing them.
        // Empty for every identity that never succeeded, so a no-op on the
        // overwhelmingly common fleet; the flattened form was validated at
        // construction.
        let cap = cap.with_predecessor_actor_ids(self.predecessor_actor_ids.as_chunks::<32>().0);
        // Empty for every identity that never succeeded, so this is a no-op on
        // the overwhelmingly common fleet; `with_predecessor_backup_keys` takes
        // whole 32-byte keys, and the flattened form was validated at
        // construction. Bound (3): omitted entirely once every hosted engine has
        // verifiably drained, which is what ends the window the retired *seed*
        // stays load-bearing over.
        if drop_predecessor_keys {
            return cap;
        }
        let mut pairs: Vec<([u8; 32], [u8; 32])> = self
            .predecessor_keys_by_actor
            .as_chunks::<64>()
            .0
            .iter()
            .map(|record| {
                let (id, key) = record.split_at(32);
                (
                    id.try_into().expect("split at 32 of 64"),
                    key.try_into().expect("split at 32 of 64"),
                )
            })
            .collect();
        let cap = cap
            .with_predecessor_backup_keys(self.predecessor_backup_keys.as_chunks::<32>().0)
            .with_predecessor_keys_by_actor(&pairs);
        // The local copies of the paired keys are key material too.
        for (_, key) in &mut pairs {
            zeroize::Zeroize::zeroize(key);
        }
        cap
    }
}

impl<B: ProvisioningBearerSource> ProvisioningDelegate for AgentEndpointDelegate<B> {
    fn ensure_agent_running(&self) -> impl Future<Output = bool> + Send {
        let endpoint = self.endpoint.clone();
        let spawner = Arc::clone(&self.spawner);
        async move {
            let connected = tokio::task::spawn_blocking(move || endpoint.connect().is_ok())
                .await
                .unwrap_or(false);
            if connected {
                true
            } else {
                spawner.spawn_agent();
                false
            }
        }
    }

    fn bearer(&self) -> impl Future<Output = Option<BearerToken>> + Send {
        self.bearer_source.current_bearer()
    }

    fn refresh_bearer(
        &self,
        bearer: &BearerToken,
    ) -> impl Future<Output = RefreshBearerOutcome> + Send {
        let endpoint = self.endpoint.clone();
        let token = bearer.token.clone();
        let expires_at = bearer.expires_at;
        async move {
            let result = tokio::task::spawn_blocking(move || {
                let client = endpoint.connect()?;
                client.request(RequestMethod::RefreshBearer(BearerToken::new(
                    token, expires_at,
                )))
            })
            .await;
            match result {
                Ok(Ok(response)) => match response.result {
                    ResponseResult::Ok(_) => RefreshBearerOutcome::classify(Ok(())),
                    ResponseResult::Err(msg) => RefreshBearerOutcome::classify(Err(msg.as_str())),
                },
                // Connect failed / join error → treat as unreachable (retry next
                // tick).
                _ => RefreshBearerOutcome::Unreachable,
            }
        }
    }

    fn full_provision(&self, bearer: &BearerToken) -> impl Future<Output = ()> + Send {
        let endpoint = self.endpoint.clone();
        let provisioned_without_keys = Arc::clone(&self.provisioned_without_predecessor_keys);
        async move {
            // Recomputed here rather than passed down from `needs_reprovision`:
            // the `NoCapability` path provisions without ever consulting it, and
            // a license that decided the capability's contents must be the one
            // recorded as provisioned (ruling 2 — a fresh answer at every
            // provision, never a cached verdict).
            let drop_keys = self.license_to_drop_predecessor_keys().await;
            let cap = self.build_capability(bearer, drop_keys);
            let ok = matches!(
                tokio::task::spawn_blocking(move || {
                    let client = endpoint.connect()?;
                    client.request(RequestMethod::ProvisionCapability(cap))
                })
                .await,
                Ok(Ok(_))
            );
            // Advance the last-provisioned marker only on success, so a failed
            // push leaves `needs_reprovision` firing and the loop retries next
            // tick (best-effort, matching the NoCapability path).
            if ok {
                *provisioned_without_keys.lock().unwrap() = Some(drop_keys);
            }
        }
    }

    fn needs_reprovision(&self) -> impl Future<Output = bool> + Send {
        // Every sync read happens before the await — a `MutexGuard` may not be
        // held across one, and these are uncontended cell reads either way.
        let provisioned_without_keys = *self.provisioned_without_predecessor_keys.lock().unwrap();
        let holds_predecessor_keys = !self.predecessor_backup_keys.is_empty();
        async move {
            // Once per session, even over an agent holding a persisted capability
            // whose bearer is still valid (so the loop never meets the
            // NoCapability path): this session's capability carries its own
            // bearer and provision stamp, which the persisted one predates.
            let Some(provisioned_without_keys) = provisioned_without_keys else {
                return true;
            };
            // Holding no retired keys, there is nothing to add or drop, so the
            // license can never require a push — and the overwhelmingly common
            // fleet skips the agent round trip entirely.
            if !holds_predecessor_keys {
                return false;
            }
            // The bound-(3) license, recomputed every tick from a live agent
            // answer. A disagreement with what the agent currently holds is a
            // re-push in EITHER direction: a device that has drained drops the
            // retired keys, and one that re-arms — a predecessor-era set binds,
            // a second succession clears the sentinels — gets them back. This is
            // what makes the license a predicate rather than a latch, and it is
            // the only reason an automatic drop is safe (ruling 2).
            let licensed = self.license_to_drop_predecessor_keys().await;
            provisioned_without_keys != licensed
        }
    }

    fn agent_became_reachable(&self) -> impl Future<Output = ()> + Send {
        let observer = self.reachability.clone();
        async move {
            if let Some(observer) = observer {
                observer.on_agent_reachable();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// LocationBindingsModel — the pure optimistic-UI ⇄ agent-truth reconcile core.
// ---------------------------------------------------------------------------

/// Where a rendered binding row stands relative to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
    /// The agent has it (seen in `ListLocations`, or a bind push succeeded).
    Confirmed,
    /// Local truth not yet on the agent: a user add whose push hasn't
    /// succeeded. Stays **rendered** and
    /// re-pushes on every reconcile until confirmed (union semantics — a failed
    /// push must never vanish from the list).
    PendingBind,
    /// A local remove not yet pushed: hidden from render, re-pushes
    /// `RemoveLocation` on every reconcile until the agent no longer lists it.
    PendingUnbind,
}

/// One folder↔folder row in the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingRow {
    pub path: String,
    pub folder: String,
    /// The bound set's `fauna_core::folder_keys::FolderRef` in wire form — the
    /// unambiguous identity the agent keys the engine, its key material and its
    /// state DB by (set names are unique only per owner), and the
    /// key this model joins agent rows and engine rows by. `folder` is a label.
    pub folder_id: String,
    pub state: BindingState,
    /// The bound set's write grant was revoked by the nest that owns it, so the
    /// agent has **parked** this binding: no engine runs for it and the folder is
    /// no longer tracked (`file-sync.md` § Multi-writer shared sets — D4). Local
    /// files are untouched; the row stays rendered precisely so the client can
    /// say so (`folder-access-revoked-warning`) instead of the folder silently
    /// going quiet.
    ///
    /// Mirrored from [`LocationInfo::access_revoked`] on every reconcile — the
    /// agent is the only writer, since only a live engine meets the refusal. An
    /// optimistic local row (a fresh add) starts `false`: it has
    /// no agent-side truth yet, and a re-bind is exactly what clears a park.
    pub access_revoked: bool,
    /// How many deletes the **mass-delete floor** is holding on this row's set —
    /// the count the *"your folder emptied — N deletions held"* affordance
    /// renders (`folder-location-deletes-held`;
    /// `delete-propagation.md` § A wholesale-vanished folder is infrastructure
    /// failure). `0` is the overwhelmingly common reading and means *do not warn*.
    ///
    /// Mirrored from [`fauna_ipc::sync::EngineInfo::deletes_held`] by
    /// [`LocationBindingsModel::fold_engine_holds`], which is a **separate** fold
    /// from [`LocationBindingsModel::reconcile`]: the hold is reported per *set*
    /// on the `ListEngines` roster, while a row is per *bound path*, and the two
    /// answers arrive on different cadences. A binding reconcile therefore never
    /// touches this field — it knows nothing about holds, and blanking it between
    /// status polls would make the surface flicker.
    ///
    /// ⚠ **Derived, never stored**, with exactly the agent's own lifetime: a set
    /// missing from the roster (engine stopped, agent restarted, agent too old to
    /// report) reads `0` rather than keeping the last number seen — the
    /// affordance must never be handed a count no live engine stands behind.
    pub deletes_held: u64,
    /// How many deletes the delete rail withheld on this row's set because part
    /// of it **could not be read** — the count `folder-location-unreadable`
    /// renders (`delete-propagation.md` § Unreadable is not absent). `0` means
    /// *do not warn*.
    ///
    /// Mirrored from [`fauna_ipc::sync::EngineInfo::deletes_skipped_unreadable`]
    /// by the same roster fold as [`Self::deletes_held`]
    /// ([`LocationBindingsModel::fold_engine_holds`]), under the same
    /// derived-never-stored contract: a set missing from the roster reads `0`.
    /// A separate field, never added into the hold: a hold offers *apply*, and
    /// an unreadable path has nothing to apply.
    pub deletes_skipped_unreadable: u64,
    /// The binding's sync mode (`"always"` | `"on-demand"`) — the reading
    /// `folder-location-mode-toggle` renders (`on-demand-files.md` § On-Demand
    /// Files → *The choice is the user's*). **Agent truth**: mirrored from
    /// [`LocationInfo::mode`] on every [`LocationBindingsModel::reconcile`], so
    /// the switch re-reads the sync host's record rather than view state. An
    /// optimistic row the agent has not listed yet starts at
    /// [`fauna_ipc::sync::fresh_binding_mode`] — the mode the agent's
    /// `AddLocation` will give it — so it does not flicker between the bind and
    /// the reconcile. Orthogonal to the binding: a mode change is pushed by
    /// `SetLocationSyncMode`, never by the model.
    pub mode: String,
    /// Why this on-demand row's root runs without its placeholder surface —
    /// [`LocationInfo::on_demand_mount_error`]'s code, mirrored by
    /// [`LocationBindingsModel::reconcile`] and by the status-tick watch
    /// ([`LocationBindingsModel::fold_parks`]): the mount is attempted after
    /// the flip's reply, so no user gesture produces the reading. `None`
    /// renders nothing; [`LocationBindingsModel::mode_toggle`] turns a code
    /// into the row's [`OnDemandNotice`].
    pub mount_error: Option<String>,
}

/// What a bound row's `folder-location-mode-toggle` says beside the switch
/// (`on-demand-files.md` § Linux FUSE binding, the lifecycle rule). Each app
/// maps a variant to its own localized line; the mapping from the agent's
/// codes is here so no app carries a copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDemandNotice {
    /// The host has no mount helper — the `fuse3` package is not installed.
    NeedsFuse3,
    /// The host has no usable FUSE device (a kernel or sandbox without one).
    NoFuseDevice,
    /// The agent cannot serve on-demand here for a reason this build has no
    /// line for (a code from a newer agent).
    Unavailable,
    /// This location's mount was refused — the folder sits where the mount
    /// helper may not mount. The remedy is a folder under the user's home.
    MountRefused,
    /// This location's mount failed for another reason.
    MountFailed,
}

impl OnDemandNotice {
    /// Every variant, for the apps' "each line resolves" tests.
    pub const ALL: [Self; 5] = [
        Self::NeedsFuse3,
        Self::NoFuseDevice,
        Self::Unavailable,
        Self::MountRefused,
        Self::MountFailed,
    ];

    /// The notice's line in the shared string table (`i18n/strings/en.yaml`,
    /// `devices.sync_locations`), as the key `fauna_i18n::strings::lookup`
    /// resolves — so the code→line mapping is written once for every app.
    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::NeedsFuse3 => "devices.sync_locations.on_demand_needs_fuse3",
            Self::NoFuseDevice => "devices.sync_locations.on_demand_no_fuse_device",
            Self::Unavailable => "devices.sync_locations.on_demand_unavailable",
            Self::MountRefused => "devices.sync_locations.on_demand_mount_refused",
            Self::MountFailed => "devices.sync_locations.on_demand_mount_failed",
        }
    }
}

/// How one bound row's `folder-location-mode-toggle` renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeToggle {
    /// The row's mode, agent truth ([`BindingRow::mode`]).
    pub on_demand: bool,
    /// Whether the switch takes a flip. `false` only where turning on-demand
    /// ON cannot be served; a row already on-demand always keeps a live switch,
    /// since turning it off is the way out.
    pub enabled: bool,
    /// The line to render with the switch, if any.
    pub notice: Option<OnDemandNotice>,
}

/// Whether this host's agent can serve an on-demand binding — the reading of
/// [`fauna_ipc::sync::ServiceStatusInfo::on_demand_available`] the model keeps
/// between status ticks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OnDemandSurface {
    /// No status reply yet: the platform's
    /// own default stands — a switch where the agent hosts a placeholder
    /// surface (windows, linux), none elsewhere.
    #[default]
    Unknown,
    /// The agent can mount a placeholder root.
    Available,
    /// It cannot, for the carried `ON_DEMAND_REASON_*` code.
    Unavailable(String),
    /// On-demand is not the agent's on this platform at all (macOS: the File
    /// Provider extension's) — no switch is rendered.
    NotAgentHosted,
}

impl OnDemandSurface {
    /// The surface a status reply reports.
    pub fn from_status(status: &fauna_ipc::sync::ServiceStatusInfo) -> Self {
        match (
            status.on_demand_available,
            status.on_demand_unavailable_reason.as_deref(),
        ) {
            (Some(true), _) => Self::Available,
            (Some(false), Some(fauna_ipc::sync::ON_DEMAND_REASON_NOT_AGENT_HOSTED)) => {
                Self::NotAgentHosted
            }
            (Some(false), reason) => Self::Unavailable(reason.unwrap_or_default().to_string()),
            (None, _) => Self::Unknown,
        }
    }
}

/// One binding a reconcile pass wants pushed — the arguments of
/// [`AgentControl::bind_location`], as a named struct rather than a tuple so the
/// three `String`s cannot be transposed silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBind {
    pub path: String,
    pub folder: String,
    /// The set's `FolderRef` in wire form.
    pub folder_id: String,
}

impl PendingBind {
    fn from_row(row: &BindingRow) -> Self {
        Self {
            path: row.path.clone(),
            folder: row.folder.clone(),
            folder_id: row.folder_id.clone(),
        }
    }
}

/// What a reconcile pass wants pushed to the agent.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileActions {
    /// The bindings to push via `bind_location`.
    pub to_bind: Vec<PendingBind>,
    /// Paths to `unbind_location`.
    pub to_unbind: Vec<String>,
}

/// The pure folder-binding state machine: optimistic local rows reconciled
/// against the agent's `ListLocations` truth. No IO — the caller drives the
/// socket verbs and reports outcomes back ([`Self::confirm_bind`] /
/// [`Self::confirm_unbind`]; a failed push simply stays pending for the next
/// reconcile). Designed to be driven at attach, after each user mutation, and on
/// every [`ReachabilityObserver::on_agent_reachable`] edge — never only once.
#[derive(Debug, Default)]
pub struct LocationBindingsModel {
    rows: Vec<BindingRow>,
    /// The host's on-demand surface, as the last status reply reported it
    /// ([`Self::fold_service_status`]).
    on_demand: OnDemandSurface,
}

impl LocationBindingsModel {
    /// The rows the UI renders: everything except pending unbinds. A
    /// [`BindingState::PendingBind`] row renders — an add made while the agent
    /// is down must stay visible (face (b) of the A4
    /// review finding).
    pub fn rendered(&self) -> Vec<BindingRow> {
        self.rows
            .iter()
            .filter(|r| r.state != BindingState::PendingUnbind)
            .cloned()
            .collect()
    }

    /// Optimistic user add, keyed by the set's identity — every app surface
    /// picks the set from a list, so it holds the summary (or foreign record)
    /// the ref comes from (`folder_ref_for_row`). Replaces any existing row for
    /// the same path; returns the binding to push (the caller then
    /// [`Self::confirm_bind`]s on success).
    pub fn add(&mut self, path: String, folder: String, folder_id: String) -> PendingBind {
        // Re-adding a path the agent already holds keeps its persisted mode
        // (`AddLocation` is a no-op for a known path), so the replaced row's
        // reading carries over; only a genuinely new path takes the default.
        let mode = self
            .rows
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.mode.clone())
            .unwrap_or_else(|| fauna_ipc::sync::fresh_binding_mode().to_string());
        self.rows.retain(|r| r.path != path);
        self.rows.push(BindingRow {
            path: path.clone(),
            folder: folder.clone(),
            folder_id: folder_id.clone(),
            state: BindingState::PendingBind,
            // A user (re-)bind is the recovery path out of a park: clear it
            // optimistically, and let the next reconcile carry the agent's answer.
            access_revoked: false,
            // A re-bind is also the "reconnect the folder" arm of the hold
            // affordance, and the row it replaces may have been rendering one.
            // Start clean and let the next roster carry the engine's answer —
            // the alternative offers to apply deletions against a folder the
            // user just pointed somewhere else.
            deletes_held: 0,
            deletes_skipped_unreadable: 0,
            mode,
            // A (re-)bind restarts the root; the next agent row carries its answer.
            mount_error: None,
        });
        PendingBind {
            path,
            folder,
            folder_id,
        }
    }

    /// Optimistic user remove (by folder — the linux UI's row key). A
    /// confirmed row flips to [`BindingState::PendingUnbind`] (push
    /// `RemoveLocation`; keep re-pushing until the agent no longer lists it);
    /// a never-pushed [`BindingState::PendingBind`] row still flips (the bind
    /// may have raced through — `RemoveLocation` on an unknown path is
    /// harmless). Returns the paths to unbind.
    pub fn remove_by_set(&mut self, folder: &str) -> Vec<String> {
        let mut paths = Vec::new();
        for row in &mut self.rows {
            if row.folder == folder && row.state != BindingState::PendingUnbind {
                row.state = BindingState::PendingUnbind;
                paths.push(row.path.clone());
            }
        }
        paths
    }

    /// Optimistic user remove **by path** — the windows UI's row key, where the
    /// per-row `folder-location-remove-button` acts on one folder. State transitions
    /// are [`remove_by_set`](Self::remove_by_set)'s exactly; only the selector
    /// differs.
    ///
    /// Path is the model's real identity ([`add`](Self::add) replaces by it, and
    /// [`reconcile`](Self::reconcile) keys the agent's rows by it), so this is the
    /// narrower of the two: two folders bound to the *same* set are one
    /// `remove_by_set` and two distinct `remove_by_path`s. A per-path UI must use
    /// this one, or removing one row silently unbinds its sibling.
    pub fn remove_by_path(&mut self, path: &str) -> Vec<String> {
        let mut paths = Vec::new();
        for row in &mut self.rows {
            if row.path == path && row.state != BindingState::PendingUnbind {
                row.state = BindingState::PendingUnbind;
                paths.push(row.path.clone());
            }
        }
        paths
    }

    /// A bind push succeeded.
    pub fn confirm_bind(&mut self, path: &str) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.path == path)
            && row.state == BindingState::PendingBind
        {
            row.state = BindingState::Confirmed;
        }
    }

    /// An unbind push succeeded (or the agent stopped listing the row).
    pub fn confirm_unbind(&mut self, path: &str) {
        self.rows
            .retain(|r| !(r.path == path && r.state == BindingState::PendingUnbind));
    }

    /// Reconcile against the agent's `ListLocations` truth. Updates row
    /// states and returns what to push:
    ///
    /// * agent has a row we hold pending-bind → confirmed (a raced push
    ///   landed);
    /// * agent lacks a pending-bind row → `to_bind` (adds-while-down, failed
    ///   earlier pushes);
    /// * agent still lists a pending-unbind row → `to_unbind` (re-push);
    /// * agent no longer lists a pending-unbind row → row dropped (done);
    /// * agent has a **bound** row we don't hold at all → adopted as confirmed
    ///   (another control surface — fauna-tui, another session — bound it);
    /// * a confirmed row the agent no longer lists → back to pending-bind, so
    ///   the union re-pushes rather than silently dropping it (an agent config
    ///   reset must not erase the user's bindings).
    ///
    /// "Has a row" means the same path bound to the same **set** — matched by
    /// `folder_id`, never by the label two sets can share.
    pub fn reconcile(&mut self, agent_rows: &[LocationInfo]) -> ReconcileActions {
        let mut actions = ReconcileActions::default();
        let agent_by_path: std::collections::HashMap<&str, &LocationInfo> =
            agent_rows.iter().map(|r| (r.path.as_str(), r)).collect();

        self.rows.retain_mut(|row| match row.state {
            BindingState::PendingUnbind => {
                if agent_by_path.contains_key(row.path.as_str()) {
                    actions.to_unbind.push(row.path.clone());
                    true
                } else {
                    false // unbind complete — drop the row
                }
            }
            BindingState::PendingBind => {
                match agent_by_path.get(row.path.as_str()) {
                    Some(agent) if agent.folder_id.as_deref() == Some(row.folder_id.as_str()) => {
                        row.state = BindingState::Confirmed;
                        row.access_revoked = agent.access_revoked;
                        row.mode = agent.mode.clone();
                        row.mount_error = agent.on_demand_mount_error.clone();
                    }
                    _ => {
                        actions.to_bind.push(PendingBind::from_row(row));
                    }
                }
                true
            }
            BindingState::Confirmed => {
                match agent_by_path.get(row.path.as_str()) {
                    Some(agent) if agent.folder_id.as_deref() == Some(row.folder_id.as_str()) => {
                        // The agent is the authority on the park: mirror it in
                        // BOTH directions, so a re-bind that cleared it agent-side
                        // clears the client's rendering too. Same for the mode: a
                        // flip made from another surface is mirrored here.
                        row.access_revoked = agent.access_revoked;
                        row.mode = agent.mode.clone();
                        row.mount_error = agent.on_demand_mount_error.clone();
                    }
                    _ => {
                        // The agent lost it (config reset / a divergent set) —
                        // re-push rather than silently dropping the user's row.
                        row.state = BindingState::PendingBind;
                        actions.to_bind.push(PendingBind::from_row(row));
                    }
                }
                true
            }
        });

        // Adopt agent-side bound rows we don't hold (bound from another control
        // surface), keyed by the ref the agent reports. Unbound agent rows
        // (no binding) aren't renderable bindings — ignore them.
        for agent in agent_rows {
            let (Some(folder), Some(folder_id)) = (agent.folder.clone(), agent.folder_id.clone())
            else {
                continue;
            };
            if !self.rows.iter().any(|r| r.path == agent.path) {
                self.rows.push(BindingRow {
                    path: agent.path.clone(),
                    folder,
                    folder_id,
                    state: BindingState::Confirmed,
                    access_revoked: agent.access_revoked,
                    // `ListLocations` carries no hold; the next `fold_engine_holds`
                    // is where an adopted row learns its set's.
                    deletes_held: 0,
                    deletes_skipped_unreadable: 0,
                    mode: agent.mode.clone(),
                    mount_error: agent.on_demand_mount_error.clone(),
                });
            }
        }

        actions
    }

    /// Fold the agent's `ListEngines` roster onto the rendered rows, mirroring
    /// each set's [`fauna_ipc::sync::EngineInfo::deletes_held`] onto every row
    /// bound to it — the link the *"your folder emptied — N deletions held"*
    /// affordance is drawn from (`delete-propagation.md` § A wholesale-vanished
    /// folder is infrastructure failure).
    ///
    /// A **join, not a copy**: the hold is reported per set and rendered per
    /// bound path, so two folders bound to one set both carry its hold. The
    /// join key is the set's `folder_id` — two same-named sets hold apart. Kept
    /// separate from [`Self::reconcile`] because the two answers come from
    /// different verbs on different cadences — bindings reconcile at every user
    /// mutation, the roster arrives on the status poll — and because a caller
    /// that never asks for engines is entitled to a model that simply reports no
    /// hold rather than one that guesses.
    ///
    /// Deliberately a **mirror**: a set absent from the roster reads `0`. That is
    /// the whole derived-never-stored contract at the client end — a stopped
    /// engine, a restarted agent and an agent too old to report the field are all
    /// "no live engine stands behind a count", which is exactly the number the
    /// affordance must never be handed.
    ///
    /// The same pass mirrors the set's
    /// [`fauna_ipc::sync::EngineInfo::deletes_skipped_unreadable`] onto
    /// [`BindingRow::deletes_skipped_unreadable`] (`folder-location-unreadable`,
    /// `delete-propagation.md` § Unreadable is not absent): same roster, same
    /// join, same mirror rule, so every app that already folds holds gets the
    /// unreadable count without a second call.
    pub fn fold_engine_holds(&mut self, engines: &[fauna_ipc::sync::EngineInfo]) {
        let by_set: std::collections::HashMap<&str, (u64, u64)> = engines
            .iter()
            .map(|e| {
                (
                    e.folder_id.as_str(),
                    (e.deletes_held, e.deletes_skipped_unreadable),
                )
            })
            .collect();
        for row in &mut self.rows {
            let (held, unreadable) = by_set
                .get(row.folder_id.as_str())
                .copied()
                .unwrap_or_default();
            row.deletes_held = held;
            row.deletes_skipped_unreadable = unreadable;
        }
    }

    /// Mirror the agent's **park** (`LocationInfo::access_revoked`) onto the rows
    /// this model already holds — the watch half of D4 revocation, beside
    /// [`Self::fold_engine_holds`] and for the same reason: the park is derived
    /// inside the agent (the owning nest refused a write), so no user gesture
    /// and no reachability edge produces it, and [`Self::reconcile`] alone left
    /// a folder the nest had just refused painting as syncing until the user's
    /// next mutation (`file-sync.md` § Multi-writer shared sets → *Revocation*).
    ///
    /// A mirror of that one level, never a reconcile: it pushes nothing, adopts
    /// nothing and changes no row's state. A row the agent does not report, or
    /// reports bound to another set, keeps what it had for the next reconcile
    /// to settle. Both directions, like the reconcile's own mirror — a re-bind
    /// the agent cleared un-parks the row here too.
    ///
    /// The same watch carries [`BindingRow::mount_error`], which is the same
    /// kind of level: an on-demand root's mount is attempted after the flip's
    /// reply, so only a later `ListLocations` can say it failed (or that a
    /// retry stood).
    pub fn fold_parks(&mut self, agent_rows: &[LocationInfo]) {
        for row in &mut self.rows {
            if let Some(agent) = agent_rows.iter().find(|a| {
                a.path == row.path && a.folder_id.as_deref() == Some(row.folder_id.as_str())
            }) {
                row.access_revoked = agent.access_revoked;
                row.mount_error = agent.on_demand_mount_error.clone();
            }
        }
    }

    /// Keep the host's on-demand surface from a `GetServiceStatus` reply.
    /// Returns whether it changed — the caller repaints its bound rows then,
    /// since every row's [`Self::mode_toggle`] reads it.
    pub fn fold_service_status(&mut self, status: &fauna_ipc::sync::ServiceStatusInfo) -> bool {
        let surface = OnDemandSurface::from_status(status);
        let changed = self.on_demand != surface;
        self.on_demand = surface;
        changed
    }

    /// How `row`'s `folder-location-mode-toggle` renders on this host, or
    /// `None` where no switch is rendered at all. The ONE availability rule
    /// for the switch, shared by every app that drives the agent
    /// (`on-demand-files.md` § Linux FUSE binding, the lifecycle rule):
    ///
    /// * a host whose agent can mount → a live switch, plus this row's own
    ///   mount failure when it is on-demand and its root runs unmounted;
    /// * a host whose agent cannot → the switch with the host's reason,
    ///   disabled for a row that would be turning on-demand ON and live for a
    ///   row already on-demand (turning it off is the way out);
    /// * a platform where on-demand is not the agent's → no switch.
    pub fn mode_toggle(&self, row: &BindingRow) -> Option<ModeToggle> {
        let on_demand = row.mode == "on-demand";
        match &self.on_demand {
            OnDemandSurface::NotAgentHosted => None,
            OnDemandSurface::Unknown if !cfg!(any(windows, target_os = "linux")) => None,
            OnDemandSurface::Unknown | OnDemandSurface::Available => Some(ModeToggle {
                on_demand,
                enabled: true,
                notice: on_demand
                    .then_some(row.mount_error.as_deref())
                    .flatten()
                    .map(|code| match code {
                        fauna_ipc::sync::ON_DEMAND_MOUNT_REFUSED => OnDemandNotice::MountRefused,
                        _ => OnDemandNotice::MountFailed,
                    }),
            }),
            OnDemandSurface::Unavailable(reason) => Some(ModeToggle {
                on_demand,
                enabled: on_demand,
                notice: Some(match reason.as_str() {
                    fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSERMOUNT => OnDemandNotice::NeedsFuse3,
                    fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSE_DEVICE => {
                        OnDemandNotice::NoFuseDevice
                    }
                    _ => OnDemandNotice::Unavailable,
                }),
            }),
        }
    }

    /// Repaint **one** set's hold — what an
    /// [`AgentControl::apply_held_deletes`](crate::agent::AgentControl::apply_held_deletes)
    /// reply carries back in `HeldDeletesAppliedInfo::remaining_held`.
    ///
    /// Kept distinct from [`Self::fold_engine_holds`], which is a whole-roster
    /// mirror and would blank every other set: an apply speaks for its own set
    /// and nothing else. And the reply is the *right* source here — the agent's
    /// progress drain is asynchronous, so a `ListEngines` issued immediately
    /// after the click can still answer the pre-apply number, which is precisely
    /// the stale count the affordance must never show.
    pub fn set_engine_hold(&mut self, folder: &str, held: u64) {
        for row in self.rows.iter_mut().filter(|r| r.folder == folder) {
            row.deletes_held = held;
        }
    }
}

/// The liveness cache's own tests — the non-blocking contract, kept in their
/// own module because they are the only tests here that stand up a real socket.
///
/// ⚠ **Every one of these is written to be killed by the cache-removal mutant**
/// (make [`LivenessCache::read_and_refresh`] call its probe inline). That mutant
/// has to be *runnable*, which is why the parked probes below all release
/// themselves on a bounded fallback: a mutant that reintroduces the block must
/// **fail** the assertion, never hang the suite into a timeout that names a
/// different bug.
#[cfg(test)]
mod liveness_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Long enough that a mutant which blocks on it is unmistakably blocking,
    /// short enough that its test still ends.
    const PARK: Duration = Duration::from_secs(3);
    /// A generous ceiling for "the probe thread got going at all" (convention
    /// 14: budgets are ceilings a green run never pays, not expected latencies).
    const START_BUDGET: Duration = Duration::from_secs(30);

    fn fresh_cache() -> &'static LivenessCache {
        Box::leak(Box::new(LivenessCache::new()))
    }

    /// **The pin this whole track exists for.** A read must answer from the
    /// cached field while its refresh is still out on the wire — that is the
    /// difference between `provider=0ms` and the measured `provider=6007ms`
    /// that made every windows command unackable.
    #[test]
    fn the_cached_read_answers_while_its_probe_is_still_in_flight() {
        let cache = fresh_cache();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let answer = cache.read_and_refresh(move || {
            entered_tx.send(()).expect("test still listening");
            // Bounded so the cache-removal mutant FAILS rather than hangs.
            let _ = release_rx.recv_timeout(PARK);
            true
        });

        assert!(
            !answer,
            "the read must answer from the cache, not wait for the probe — a \
             state provider that waits is the 6 s ack-path block this cache removes"
        );
        // Non-vacuity: prove the probe really was dispatched, so the `false`
        // above is "cached answer while a refresh runs" and not "no refresh".
        entered_rx
            .recv_timeout(START_BUDGET)
            .expect("the read must dispatch a refresh");
        assert!(
            cache.refresh_in_flight(),
            "the parked probe must still be counted in flight"
        );
        let _ = release_tx.send(());
    }

    /// The cache is a *converging* one, not a stale one: the answer the probe
    /// found becomes the answer a later read gives.
    #[test]
    fn a_refresh_converges_the_cached_answer() {
        let cache = fresh_cache();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        // The probe is parked so that "no answer has landed yet" is a *causal*
        // fact and not a race this thread has to win. An unparked probe may
        // store its answer before the calling thread reaches the `last.load()`
        // that `read_and_refresh` ends on, and a loaded box loses that race —
        // which is how this test redded the merge gate on 2026-08-15 while the
        // cache it covers was correct. Bounded, so the cache-removal mutant
        // (probe called inline) FAILS the assert rather than hanging.
        let first = cache.read_and_refresh(move || {
            let _ = release_rx.recv_timeout(PARK);
            true
        });
        assert!(!first, "the first read predates any answer");
        let _ = release_tx.send(());

        // Deadline poll against a generous ceiling — no wall-clock assertion.
        let deadline = Instant::now() + START_BUDGET;
        while !cache.read_and_refresh(|| true) {
            assert!(
                Instant::now() < deadline,
                "the cached answer never converged on the probe's"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Frequent state polling must not stack connects: at most one refresh is
    /// ever out. (Windows and macOS each hand-rolled this guard; it belongs
    /// here once — priority #2.)
    #[test]
    fn only_one_refresh_is_ever_in_flight() {
        let cache = fresh_cache();
        let starts = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let probe = {
            let starts = Arc::clone(&starts);
            move || {
                starts.fetch_add(1, Ordering::SeqCst);
                let _ = entered_tx.send(());
                let _ = release_rx.recv_timeout(PARK);
                true
            }
        };
        cache.read_and_refresh(probe);
        entered_rx
            .recv_timeout(START_BUDGET)
            .expect("the first read must dispatch a refresh");

        // Every read while that one is parked must decline to start another.
        for _ in 0..20 {
            let starts = Arc::clone(&starts);
            cache.read_and_refresh(move || {
                starts.fetch_add(1, Ordering::SeqCst);
                true
            });
        }
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "a refresh already in flight must absorb every further read"
        );
        let _ = release_tx.send(());
    }

    /// A probe that panics must not wedge the cache into "forever refreshing".
    #[test]
    fn a_panicking_probe_releases_the_in_flight_guard() {
        let cache = fresh_cache();
        cache.read_and_refresh(|| panic!("probe blew up"));

        let deadline = Instant::now() + START_BUDGET;
        while cache.refresh_in_flight() {
            assert!(
                Instant::now() < deadline,
                "a panicking probe left the guard set — no refresh can ever run again"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // And the cache is usable again.
        let (entered_tx, entered_rx) = mpsc::channel();
        cache.read_and_refresh(move || {
            let _ = entered_tx.send(());
            true
        });
        entered_rx
            .recv_timeout(START_BUDGET)
            .expect("the cache must still dispatch after a panicking probe");
    }

    /// **The condition that produced the 6 s, reproduced.** An endpoint that
    /// *accepts and then says nothing* is the windows named-pipe case; "no
    /// endpoint at all" is the unix case, which fails `connect()` instantly and
    /// would have missed this bug entirely (the grading note on row 269).
    #[cfg(unix)]
    #[test]
    fn a_listening_but_silent_agent_costs_the_liveness_bound_not_the_request_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            // Accept, then answer nothing at all — the wedged-agent state.
            let _held = listener.accept();
            let _ = hold_rx.recv_timeout(Duration::from_secs(30));
        });

        let start = Instant::now();
        let serving = probe_serving_at(&AgentEndpoint::Unix(path), LIVENESS_TIMEOUT);
        let elapsed = start.elapsed();

        assert!(!serving, "a silent agent is not serving");
        assert!(
            elapsed < fauna_ipc::sync_pipe_client::REQUEST_TIMEOUT,
            "the liveness probe must use its own short bound, not the {:?} \
             request ceiling — took {elapsed:?}",
            fauna_ipc::sync_pipe_client::REQUEST_TIMEOUT
        );
        let _ = hold_tx.send(());
    }

    /// The same silent endpoint behind the cache: the read is immediate, and
    /// the answer stays `false` rather than becoming `true` by default.
    #[cfg(unix)]
    #[test]
    fn a_silent_agent_never_flips_the_cached_answer_to_serving() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _held = listener.accept();
            let _ = hold_rx.recv_timeout(Duration::from_secs(30));
        });

        let cache = fresh_cache();
        let endpoint = AgentEndpoint::Unix(path);
        let started = Instant::now();
        let answer = cache.read_and_refresh(move || probe_serving_at(&endpoint, LIVENESS_TIMEOUT));
        assert!(!answer);
        assert!(
            started.elapsed() < LIVENESS_TIMEOUT,
            "the read waited on the probe it dispatched"
        );
        let _ = hold_tx.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopSpawner;
    impl AgentSpawner for NoopSpawner {
        fn spawn_agent(&self) {}
    }
    struct EmptyBearer;
    impl ProvisioningBearerSource for EmptyBearer {
        async fn current_bearer(&self) -> Option<BearerToken> {
            None
        }
    }

    /// An endpoint nothing serves. These tests exercise pure capability
    /// assembly + the re-provision decision — never a real exchange — so the
    /// only requirement is that it is well-formed on this platform.
    fn unreachable_endpoint() -> AgentEndpoint {
        #[cfg(unix)]
        {
            AgentEndpoint::Unix(std::path::PathBuf::from(
                "/nonexistent/fauna-sync-agent.sock",
            ))
        }
        #[cfg(windows)]
        {
            AgentEndpoint::Pipe(r"\\.\pipe\fauna-sync-nonexistent-test".to_string())
        }
    }

    fn delegate() -> AgentEndpointDelegate<EmptyBearer> {
        AgentEndpointDelegate {
            endpoint: unreachable_endpoint(),
            spawner: Arc::new(NoopSpawner),
            bearer_source: Arc::new(EmptyBearer),
            backup_key: Zeroizing::new(vec![7u8; 32]),
            predecessor_backup_keys: Zeroizing::new(Vec::new()),
            predecessor_actor_ids: Vec::new(),
            predecessor_keys_by_actor: Zeroizing::new(Vec::new()),
            actor_id: vec![9u8; 32],
            nest_url: "https://nest.example".to_string(),
            provisioned_without_predecessor_keys: Arc::new(Mutex::new(None)),
            device_id: "dev-123".to_string(),
            reachability: None,
        }
    }

    /// The capability carries the provision's inputs and **no renewal key** —
    /// the store principal, read by the agent load-only from the shared slot,
    /// is the machine's only renewal credential (`sync-agent-credentials.md`
    /// § Credential model → the RULED 2026-09-28 block, decision 1).
    #[test]
    fn build_capability_carries_the_inputs_and_no_renewal_key() {
        let d = delegate();
        let cap = d.build_capability(&BearerToken::new("tok".into(), 1234), false);
        assert_eq!(cap.nest_url, "https://nest.example");
        assert_eq!(cap.device_id, "dev-123");
        assert_eq!(cap.bearer.token, "tok");
        assert_eq!(cap.bearer.expires_at, 1234);
        // The wire fields are pinned in `fauna_ipc::sync`'s own test; here,
        // that the provision fills exactly those and nothing beside them.
        let fields: std::collections::BTreeMap<String, serde::de::IgnoredAny> =
            fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&cap).expect("encode"))
                .expect("decode");
        assert_eq!(
            fields.len(),
            8,
            "a provision pushes the pinned capability fields and no second credential: {:?}",
            fields.keys().collect::<Vec<_>>()
        );
    }

    /// A delegate that DOES hold retired owner keys — the successor's device,
    /// the only shape bound (3) has anything to say about.
    fn delegate_with_predecessor_keys() -> AgentEndpointDelegate<EmptyBearer> {
        AgentEndpointDelegate {
            predecessor_backup_keys: Zeroizing::new(vec![5u8; 32]),
            predecessor_actor_ids: vec![6u8; 32],
            predecessor_keys_by_actor: Zeroizing::new([vec![6u8; 32], vec![5u8; 32]].concat()),
            ..delegate()
        }
    }

    /// **The attested predecessor ids are outside bound (3)** (`account-data-
    /// taxonomy.md` § The generation machinery → *The source of `prior`*): a
    /// push that drops the retired KEYS under the license still carries the
    /// IDS, because they are the agent's fleet-view `prior` and a predecessor-signed
    /// enrollment row never stops needing them. Mutation: make the id attach
    /// follow the `drop_predecessor_keys` early return → this reds.
    #[test]
    fn the_attested_predecessor_ids_survive_the_key_drop_license() {
        let d = delegate_with_predecessor_keys();
        let bearer = BearerToken::new("tok".into(), 4_000_000_000);
        let licensed = d.build_capability(&bearer, true);
        assert!(
            licensed.predecessor_backup_keys().is_empty(),
            "the license omits the keys — that half is bound (3)'s"
        );
        assert_eq!(
            licensed.predecessor_actor_ids(),
            vec![[6u8; 32]],
            "the ids are not key material and ride on every push"
        );
        let unlicensed = d.build_capability(&bearer, false);
        assert_eq!(unlicensed.predecessor_actor_ids(), vec![[6u8; 32]]);
        assert_eq!(unlicensed.predecessor_backup_keys(), vec![[5u8; 32]]);
    }

    /// Once per session, whatever the agent already holds: a persisted
    /// capability with a valid bearer never meets the NoCapability path, yet
    /// this session's bearer and provision stamp must reach the agent.
    /// Then it settles — no retired keys, nothing for the license to move.
    #[tokio::test]
    async fn needs_reprovision_fires_once_per_session_then_settles() {
        let d = delegate();
        assert!(
            d.needs_reprovision().await,
            "never provisioned this session ⇒ provision"
        );
        *d.provisioned_without_predecessor_keys.lock().unwrap() = Some(false);
        assert!(!d.needs_reprovision().await, "provisioned ⇒ settled");
    }

    #[tokio::test]
    async fn ensure_agent_running_spawns_when_socket_absent() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct RecordingSpawner(Arc<AtomicBool>);
        impl AgentSpawner for RecordingSpawner {
            fn spawn_agent(&self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let spawned = Arc::new(AtomicBool::new(false));
        let mut d = delegate();
        d.spawner = Arc::new(RecordingSpawner(spawned.clone()));
        let running = d.ensure_agent_running().await;
        assert!(!running, "an absent socket ⇒ not already running");
        assert!(
            spawned.load(Ordering::SeqCst),
            "an absent socket must trigger a spawn"
        );
    }

    // ── A retry-less verb ensures the agent is up first ─────────────────────
    //
    // The windows failure this closes: a verb with nothing behind it to retry
    // (`ApplyHeldDeletes`, `ReclaimCustodianStore`, `CustodianRunPassNow`)
    // issued while the agent is still starting reaches a named pipe that does
    // not exist yet and fails outright, because `sync_pipe_client`'s
    // `ERROR_PIPE_BUSY` retry explicitly does not cover `ERROR_FILE_NOT_FOUND`.
    // Unlike a convergence tick, a confirm modal has no next attempt.
    //
    // Every assertion below is on ATTEMPT and SPAWN COUNTS, never on elapsed
    // time (`testing.md` convention 14): the give-up case runs a zero budget, so
    // it is exact rather than merely fast.

    /// A spawner that counts its calls — the contract is "at most once".
    struct CountingSpawner(Arc<std::sync::atomic::AtomicUsize>);

    impl AgentSpawner for CountingSpawner {
        fn spawn_agent(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A connect step that fails `failures` times, then answers.
    fn connect_failing(
        failures: usize,
    ) -> (
        impl FnMut() -> Result<&'static str, &'static str>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&attempts);
        let connect = move || {
            let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < failures {
                Err("ERROR_FILE_NOT_FOUND")
            } else {
                Ok("connected")
            }
        };
        (connect, attempts)
    }

    #[test]
    fn a_reachable_agent_is_never_spawned_by_an_ensuring_verb() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let spawns = Arc::new(AtomicUsize::new(0));
        let (connect, attempts) = connect_failing(0);
        let got = connect_ensuring_agent(
            connect,
            &CountingSpawner(Arc::clone(&spawns)),
            AGENT_START_WAIT,
            std::time::Duration::from_millis(1),
            // No terminal errors here: an absent endpoint is the retryable kind.
            |_| false,
        );
        assert_eq!(got, Ok("connected"));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "an agent that answers costs exactly one connect"
        );
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            0,
            "a reachable agent must never be spawned again — that is the \
             convergence loop's probe contract, and a second process is exactly \
             what ChildSpawner's live-child gate exists to avoid"
        );
    }

    #[test]
    fn an_ensuring_verb_spawns_the_agent_and_retries_until_it_answers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let spawns = Arc::new(AtomicUsize::new(0));
        let (connect, attempts) = connect_failing(3);
        let got = connect_ensuring_agent(
            connect,
            &CountingSpawner(Arc::clone(&spawns)),
            AGENT_START_WAIT,
            std::time::Duration::from_millis(1),
            // No terminal errors here: an absent endpoint is the retryable kind.
            |_| false,
        );
        assert_eq!(
            got,
            Ok("connected"),
            "the agent came up mid-wait, so the gesture must land — this is \
             the whole windows failure: the first connect loses the race against \
             the agent's own start"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            4,
            "one failed probe, then a re-probe per poll until it answered"
        );
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "spawn once, then wait it out — never once per poll"
        );
    }

    #[test]
    fn an_ensuring_verb_gives_up_with_the_last_error_when_the_budget_is_spent() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let spawns = Arc::new(AtomicUsize::new(0));
        let (connect, attempts) = connect_failing(usize::MAX);
        let got = connect_ensuring_agent(
            connect,
            &CountingSpawner(Arc::clone(&spawns)),
            // A spent budget: the wait is bounded, so a box where the agent
            // cannot start fails the command instead of hanging it.
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(1),
            |_| false,
        );
        assert_eq!(got, Err("ERROR_FILE_NOT_FOUND"));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a spent budget buys no re-probe past the first"
        );
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "the spawn is still worth attempting — the convergence loop's next \
             tick is what picks the agent up afterwards"
        );
    }

    /// A refused *server identity* is terminal — the one connect failure this
    /// wait must not treat as "the agent isn't up yet".
    ///
    /// On windows the pipe namespace is machine-wide, so another local account
    /// can hold `\\.\pipe\fauna-sync.<our SID>` while our agent is down — and
    /// while it does, our agent's `FILE_FLAG_FIRST_PIPE_INSTANCE` create fails
    /// and it exits. Retrying there is not merely useless, it is actively
    /// misleading: the loop would spend its whole 10 s budget on
    /// spawn → die → re-probe and then report the one diagnosis that is
    /// certainly wrong — *"agent unreachable"* — for a machine whose agent is
    /// fine and whose name has been taken.
    ///
    /// Asserted on attempt and spawn counts, never elapsed time
    /// (`testing.md` convention 14).
    #[test]
    fn a_refused_server_identity_is_terminal_and_never_spawned_past() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let spawns = Arc::new(AtomicUsize::new(0));
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&attempts);
        let connect = move || -> Result<&'static str, &'static str> {
            seen.fetch_add(1, Ordering::SeqCst);
            Err("REFUSED")
        };

        let got = connect_ensuring_agent(
            connect,
            &CountingSpawner(Arc::clone(&spawns)),
            // A budget big enough that a retrying implementation would take it:
            // the assertions below are what fails, not a stopwatch.
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(1),
            |e: &&'static str| *e == "REFUSED",
        );

        assert_eq!(got, Err("REFUSED"));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a refusal is an answer, not a miss — it must cost exactly one connect"
        );
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            0,
            "spawning against a squatted name starts an agent that immediately \
             dies on its own first-instance create"
        );
    }

    fn status_with_version(version: &str) -> fauna_ipc::sync::ServiceStatusInfo {
        fauna_ipc::sync::ServiceStatusInfo {
            version: version.to_string(),
            uptime_secs: 42,
            connection: fauna_ipc::sync::ConnectionState::Connected,
            sync: fauna_ipc::sync::SyncStatusInfo {
                connected: true,
                syncing: false,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn agent_health_state_is_running_when_versions_match() {
        let status = status_with_version("1.4.2");
        assert_eq!(
            agent_health_state(&Ok(status), "1.4.2"),
            AgentHealthState::Running
        );
    }

    #[test]
    fn agent_health_state_is_restart_pending_when_versions_differ() {
        let status = status_with_version("1.4.1");
        assert_eq!(
            agent_health_state(&Ok(status), "1.4.2"),
            AgentHealthState::RestartPending,
            "an update replaced the on-disk binary but the running agent hasn't picked it up"
        );
    }

    #[test]
    fn agent_health_state_is_not_running_when_the_call_failed() {
        assert_eq!(
            agent_health_state(&Err(AgentControlError::new("unreachable")), "1.4.2"),
            AgentHealthState::NotRunning,
            "a failed/unreachable GetServiceStatus call must never read as a stale version"
        );
    }

    /// tier_1: in the version-skew window something DID answer — a reply too
    /// new to read, or a refusal of the verb by an older agent — so the
    /// reading is *Restart pending*, never *Not running* (`sync-agent.md`
    /// § Local agent health).
    #[test]
    fn agent_health_state_is_restart_pending_when_the_agent_answered_unreadably() {
        let unreadable = exchange_outcome(
            "GetServiceStatus",
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                fauna_ipc::sync_pipe_client::ReplyNotUnderstood { id: 1 },
            )),
        )
        .unwrap_err();
        assert_eq!(unreadable.kind(), AgentControlErrorKind::UnreadableReply);
        assert_eq!(
            agent_health_state(&Err(unreadable), "1.4.2"),
            AgentHealthState::RestartPending
        );

        let refused = exchange_outcome(
            "GetServiceStatus",
            Ok(fauna_ipc::sync::Response {
                id: 1,
                result: ResponseResult::Err(
                    fauna_ipc::sync::UNSUPPORTED_METHOD_ERROR_MESSAGE.into(),
                ),
            }),
        )
        .unwrap_err();
        assert_eq!(refused.kind(), AgentControlErrorKind::UnsupportedMethod);
        assert_eq!(
            agent_health_state(&Err(refused), "1.4.2"),
            AgentHealthState::RestartPending
        );

        let failed = exchange_outcome(
            "GetServiceStatus",
            Ok(fauna_ipc::sync::Response {
                id: 1,
                result: ResponseResult::Err("boom".into()),
            }),
        )
        .unwrap_err();
        assert_eq!(failed.kind(), AgentControlErrorKind::Other);
    }

    /// tier_1, end to end over a real socket: an agent newer than this app
    /// answers `GetServiceStatus` in a shape this build cannot read. The call
    /// fails at once, typed, and the health reading is *Restart pending*.
    #[cfg(unix)]
    #[test]
    fn a_newer_agents_unreadable_status_reply_reads_as_restart_pending() {
        use std::time::{Duration, Instant};
        #[derive(serde::Serialize)]
        enum NewerPayload {
            AddedInANewerAgent { n: u32 },
        }
        #[derive(serde::Serialize)]
        enum NewerResult {
            Ok(NewerPayload),
        }
        #[derive(serde::Serialize)]
        struct NewerResponse {
            id: u64,
            result: NewerResult,
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        std::thread::spawn(move || {
            use std::io::Write;
            let (mut stream, _) = listener.accept().expect("accept");
            let payload = fauna_ipc::sync_pipe_client::read_frame(&mut stream).expect("a request");
            let req: fauna_ipc::sync::Request =
                fauna_ipc::decode_payload(&payload).expect("decodes");
            let reply = fauna_ipc::encode_frame(&NewerResponse {
                id: req.id,
                result: NewerResult::Ok(NewerPayload::AddedInANewerAgent { n: 1 }),
            })
            .unwrap();
            stream.write_all(&reply).unwrap();
            // Hold the connection open: a failure must come from the reply,
            // not from the socket closing.
            std::thread::sleep(Duration::from_secs(10));
        });

        let started = Instant::now();
        let client = AgentEndpoint::Unix(path).connect().expect("connect");
        let outcome = exchange_outcome(
            "GetServiceStatus",
            client.request(RequestMethod::GetServiceStatus),
        )
        .map(|_| unreachable!("the reply cannot be read"));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "failed at once, not on the request timeout"
        );
        assert_eq!(
            agent_health_state(&outcome, "1.4.2"),
            AgentHealthState::RestartPending
        );
    }

    /// The agent's own *Keys pending* outranks a version mismatch — it is the one
    /// state with a security consequence and a clock — and is itself outranked
    /// only by an agent that cannot be reached at all (`sync-agent.md` § Local
    /// agent health: Not running > Keys pending > Restart pending > Running).
    /// The input is the reply's field: the agent derives it where the truth is.
    #[test]
    fn agent_health_state_is_keys_pending_over_running_and_restart_pending() {
        let pending = |version: &str| fauna_ipc::sync::ServiceStatusInfo {
            keys_pending: true,
            ..status_with_version(version)
        };
        assert_eq!(
            agent_health_state(&Ok(pending("1.4.2")), "1.4.2"),
            AgentHealthState::KeysPending
        );
        assert_eq!(
            agent_health_state(&Ok(pending("1.4.1")), "1.4.2"),
            AgentHealthState::KeysPending,
            "a pending generation must not hide behind the weeks-long linux Restart pending"
        );
        assert_eq!(
            agent_health_state(&Err(AgentControlError::new("unreachable")), "1.4.2"),
            AgentHealthState::NotRunning,
            "an unreachable agent is the root cause — nothing can be read from it"
        );
    }

    /// *Not enrolled* outranks every other reading of a reachable agent
    /// (`sync-agent.md` § Local agent health: Not running > Not enrolled >
    /// Keys pending > Restart pending > Running): it is the one that waits on
    /// the user. Read off the reply's `needs_reenrollment`.
    #[test]
    fn agent_health_state_is_not_enrolled_over_keys_pending_and_restart_pending() {
        let refused = fauna_ipc::sync::ServiceStatusInfo {
            needs_reenrollment: true,
            keys_pending: true,
            ..status_with_version("1.4.1")
        };
        assert_eq!(
            agent_health_state(&Ok(refused), "1.4.2"),
            AgentHealthState::NotEnrolled
        );
        assert_eq!(
            agent_health_state(
                &Ok(fauna_ipc::sync::ServiceStatusInfo {
                    needs_reenrollment: true,
                    ..status_with_version("1.4.2")
                }),
                "1.4.2"
            ),
            AgentHealthState::NotEnrolled
        );
    }

    fn engine(folder: &str, serving: bool) -> fauna_ipc::sync::EngineInfo {
        fauna_ipc::sync::EngineInfo {
            folder: folder.to_string(),
            mode: "always".to_string(),
            serving,
            ..Default::default()
        }
    }

    #[test]
    fn no_engines_is_not_serving() {
        assert!(
            !engines_are_serving(&[]),
            "an agent hosting no engines is not running: `data.sync.running` must \
             stay false, or every _wait_for_engine fixture passes before its bind lands"
        );
    }

    #[test]
    fn a_planned_but_unstarted_engine_is_not_serving() {
        assert!(
            !engines_are_serving(&[engine("set-a", false)]),
            "an engine the agent knows about but has not started must not read as running"
        );
    }

    #[test]
    fn any_one_serving_engine_is_serving() {
        assert!(
            engines_are_serving(&[engine("set-a", false), engine("set-b", true)]),
            "ANY serving engine means the agent is running — an `.all()` here would \
             report a healthy multi-set agent as down whenever one set is still starting"
        );
    }

    // ── The post-succession corpus re-seal's progress fold (leg 5 of the
    //    aftermath; `succession-aftermath.md` § Re-key scope) ────────────────

    fn with_reseal(
        folder: &str,
        reseal: fauna_ipc::sync::CorpusResealInfo,
    ) -> fauna_ipc::sync::EngineInfo {
        fauna_ipc::sync::EngineInfo {
            corpus_reseal: Some(reseal),
            ..engine(folder, true)
        }
    }

    const SUCCESSOR: SuccessionCorpusContext = SuccessionCorpusContext {
        succeeded: true,
        holds_predecessor_material: true,
    };

    /// The overwhelmingly common fleet. An identity that never succeeded must
    /// render nothing **whatever the engines say** — every row's
    /// `current_root_sealed` starts at 0, so an implementation that keyed the
    /// line off the engines alone would show an aftermath line to every user who
    /// never had an aftermath.
    #[test]
    fn an_identity_that_never_succeeded_renders_nothing() {
        let engines = [with_reseal(
            "set-a",
            fauna_ipc::sync::CorpusResealInfo::Settled {
                resealed: 3,
                owed: 7,
            },
        )];
        assert_eq!(
            corpus_reseal_progress(
                &engines,
                SuccessionCorpusContext {
                    succeeded: false,
                    holds_predecessor_material: false,
                }
            ),
            None
        );
    }

    /// The arm no engine can report, which is exactly why the registry facts are
    /// arguments: with no retired keys the pass returns before touching the DB,
    /// so this device records nothing and would otherwise be told nothing —
    /// while its corpus really is still sealed to the retired identity.
    #[test]
    fn a_successor_holding_no_predecessor_material_is_owed_elsewhere() {
        assert_eq!(
            corpus_reseal_progress(
                &[engine("set-a", true)],
                SuccessionCorpusContext {
                    succeeded: true,
                    holds_predecessor_material: false,
                }
            ),
            Some(CorpusResealProgress::Settled(
                CorpusResealOutcome::OwedElsewhere
            ))
        );
    }

    // ---- bound (3)'s license: `predecessor_keys_may_be_dropped` ----

    /// An engine row carrying a drain answer.
    fn with_drain(folder: &str, folded: bool, nothing_owed: bool) -> fauna_ipc::sync::EngineInfo {
        drain_row(folder, folded, nothing_owed, true)
    }

    fn drain_row(
        folder: &str,
        folded: bool,
        nothing_owed: bool,
        all_at_rest_classified: bool,
    ) -> fauna_ipc::sync::EngineInfo {
        fauna_ipc::sync::EngineInfo {
            reseal_drain: Some(fauna_ipc::sync::ResealDrainInfo {
                folded,
                nothing_owed,
                all_at_rest_classified,
            }),
            ..engine(folder, true)
        }
    }

    /// at the license layer: a roster that looks drained but contains
    /// an engine which could not classify all its at-rest rows is not a license.
    #[test]
    fn an_engine_with_unclassified_at_rest_rows_is_not_a_license() {
        assert!(!predecessor_keys_may_be_dropped(&[drain_row(
            "set-a", true, true, false
        )]));
    }

    #[test]
    fn every_engine_drained_licenses_the_drop() {
        assert!(predecessor_keys_may_be_dropped(&[
            with_drain("set-a", true, true),
            with_drain("set-b", true, true),
        ]));
    }

    /// The vacuity guard: "no engine reported a problem" is not "every engine
    /// proved it drained". An agent hosting nothing has shown nothing.
    #[test]
    fn an_empty_roster_is_not_a_license() {
        assert!(!predecessor_keys_may_be_dropped(&[]));
    }

    /// An engine with no reported drain answer — a set whose DB could not be
    /// opened — has none. That reads as "cannot tell" — the keys stay.
    #[test]
    fn an_engine_with_no_drain_answer_is_not_a_license() {
        assert!(!predecessor_keys_may_be_dropped(&[engine("set-a", true)]));
        assert!(
            !predecessor_keys_may_be_dropped(&[
                with_drain("set-a", true, true),
                engine("set-b", true)
            ]),
            "one silent engine must veto the whole roster"
        );
    }

    /// The family, at the license layer: a fresh-device successor's list is
    /// empty because it has no rows yet, not because anything was re-sealed.
    #[test]
    fn an_unfolded_engine_is_not_a_license_even_with_an_empty_list() {
        assert!(!predecessor_keys_may_be_dropped(&[with_drain(
            "set-a", false, true
        )]));
    }

    #[test]
    fn a_folded_engine_that_still_owes_is_not_a_license() {
        assert!(!predecessor_keys_may_be_dropped(&[with_drain(
            "set-a", true, false
        )]));
    }

    /// One re-armed set is enough to bring the keys back — the predicate is over
    /// the whole roster, and the successor's corpus is one corpus.
    #[test]
    fn a_single_undrained_engine_vetoes_a_drained_roster() {
        assert!(!predecessor_keys_may_be_dropped(&[
            with_drain("set-a", true, true),
            with_drain("set-b", true, false),
            with_drain("set-c", true, true),
        ]));
    }

    /// The seam between the agent's reply and the license. Pinned separately
    /// because the connect + request around it cannot be driven from a unit
    /// test — without this, a delegate that ignored the roster entirely and
    /// answered `true` passed every predicate test above (found by mutation).
    #[test]
    fn the_engines_answer_decides_the_license() {
        let drained = vec![with_drain("set-a", true, true)];
        assert!(license_from_engines_answer(&ResponseResult::Ok(
            ResponsePayload::Engines(drained)
        )));
        let owing = vec![with_drain("set-a", true, false)];
        assert!(!license_from_engines_answer(&ResponseResult::Ok(
            ResponsePayload::Engines(owing)
        )));
    }

    /// A typed error answering `ListEngines`;
    /// any non-`Engines` payload is equally uninformative. Both are "cannot
    /// tell", which must fail toward pushing.
    #[test]
    fn a_non_engines_answer_never_licenses_the_drop() {
        assert!(!license_from_engines_answer(&ResponseResult::Err(
            "unknown method".to_string()
        )));
        assert!(!license_from_engines_answer(&ResponseResult::Ok(
            ResponsePayload::Empty
        )));
    }

    /// A delegate holding retired keys must not drop them on an agent it cannot
    /// reach — the connect fails, and the license fails closed with it.
    #[tokio::test]
    async fn an_unreachable_agent_never_licenses_the_drop() {
        let d = delegate_with_predecessor_keys();
        assert!(!d.license_to_drop_predecessor_keys().await);
    }

    /// Holding no retired keys there is nothing to omit, so the license is
    /// trivially granted and no round trip is spent — this is the whole fleet.
    #[tokio::test]
    async fn a_delegate_with_no_retired_keys_needs_no_agent_answer() {
        let d = delegate();
        assert!(
            d.license_to_drop_predecessor_keys().await,
            "the endpoint is unreachable, so a true answer proves no query was made"
        );
    }

    /// The license must actually reach the pushed capability — both directions.
    #[test]
    fn build_capability_honours_the_license() {
        let d = delegate_with_predecessor_keys();
        let bearer = BearerToken::new("tok".into(), 4_000_000_000);
        let kept = d.build_capability(&bearer, false);
        assert_eq!(
            kept.predecessor_backup_keys().len(),
            1,
            "unlicensed ⇒ the retired keys ride the capability"
        );
        let dropped = d.build_capability(&bearer, true);
        assert!(
            dropped.predecessor_backup_keys().is_empty(),
            "licensed ⇒ bound (3) omits them"
        );
    }

    /// The license is a predicate, not a latch: a disagreement with what the
    /// agent currently holds re-provisions in EITHER direction. Here the agent
    /// is unreachable (license `false`) while the last push omitted the keys, so
    /// the keys must be pushed back.
    #[tokio::test]
    async fn a_license_disagreement_re_provisions() {
        let d = delegate_with_predecessor_keys();
        *d.provisioned_without_predecessor_keys.lock().unwrap() = Some(true);
        assert!(
            d.needs_reprovision().await,
            "the agent can no longer show a drain ⇒ the keys must come back"
        );
        *d.provisioned_without_predecessor_keys.lock().unwrap() = Some(false);
        assert!(!d.needs_reprovision().await, "agreement ⇒ nothing to push");
    }

    /// The two surfaces are independent by construction: a settled pass that
    /// still owes must not license anything, and — the direction that matters —
    /// a pass reporting `owed: 0` is a *report*, never the license
    /// (`sync-agent.md` A8: enforced on the drained list, never on a count).
    #[test]
    fn a_settled_pass_reporting_nothing_owed_is_not_itself_a_license() {
        let engines = [with_reseal(
            "set-a",
            fauna_ipc::sync::CorpusResealInfo::Settled {
                resealed: 900,
                owed: 0,
            },
        )];
        assert_eq!(
            corpus_reseal_progress(&engines, SUCCESSOR),
            Some(CorpusResealProgress::Settled(
                CorpusResealOutcome::Resealed {
                    resealed: 900,
                    owed: 0
                }
            )),
            "the report still renders"
        );
        assert!(
            !predecessor_keys_may_be_dropped(&engines),
            "but a count-shaped report licenses nothing — the row carries no drain answer"
        );
    }

    /// Before any pass has recorded anything there is nothing to say — a line
    /// here would be guessing at work that may not have started.
    #[test]
    fn a_successor_whose_agent_has_recorded_no_pass_yet_renders_nothing() {
        assert_eq!(
            corpus_reseal_progress(&[engine("set-a", true)], SUCCESSOR),
            None
        );
    }

    /// tier_1: a pass state a newer agent names renders nothing — it is no
    /// recorded pass — and the `ListEngines` row it rides in still licenses
    /// exactly what its drain answer says (`transport.md` § Rule 3 in full).
    #[test]
    fn an_unknown_reseal_state_renders_nothing_and_leaves_the_licence_to_the_drain() {
        #[derive(serde::Serialize)]
        enum Newer {
            AddedInANewerAgent { n: u32 },
        }
        let frame = fauna_ipc::encode_frame(&Newer::AddedInANewerAgent { n: 1 }).unwrap();
        let unknown: fauna_ipc::sync::CorpusResealInfo =
            fauna_ipc::decode_payload(&frame[4..]).expect("the unknown arm takes it");
        let row = fauna_ipc::sync::EngineInfo {
            reseal_drain: Some(fauna_ipc::sync::ResealDrainInfo {
                folded: true,
                nothing_owed: true,
                all_at_rest_classified: true,
            }),
            ..with_reseal("set-a", unknown)
        };
        assert_eq!(
            corpus_reseal_progress(std::slice::from_ref(&row), SUCCESSOR),
            None,
            "an unreadable pass is no pass, never a guess at one"
        );
        let settled = with_reseal(
            "set-b",
            fauna_ipc::sync::CorpusResealInfo::Settled {
                resealed: 2,
                owed: 1,
            },
        );
        assert_eq!(
            corpus_reseal_progress(&[row.clone(), settled], SUCCESSOR),
            Some(CorpusResealProgress::Settled(
                CorpusResealOutcome::Resealed {
                    resealed: 2,
                    owed: 1
                }
            )),
            "the readable rows still fold"
        );
        assert!(license_from_engines_answer(&ResponseResult::Ok(
            ResponsePayload::Engines(vec![row])
        )));
    }

    /// The account is one corpus over several sets, so the totals add up — a
    /// fold that reported only the first set would understate the work on every
    /// multi-set account.
    #[test]
    fn settled_passes_sum_across_every_hosted_set() {
        let engines = [
            with_reseal(
                "set-a",
                fauna_ipc::sync::CorpusResealInfo::Settled {
                    resealed: 4,
                    owed: 1,
                },
            ),
            with_reseal(
                "set-b",
                fauna_ipc::sync::CorpusResealInfo::Settled {
                    resealed: 6,
                    owed: 2,
                },
            ),
        ];
        assert_eq!(
            corpus_reseal_progress(&engines, SUCCESSOR),
            Some(CorpusResealProgress::Settled(
                CorpusResealOutcome::Resealed {
                    resealed: 10,
                    owed: 3
                }
            ))
        );
    }

    /// Precedence, both edges. A failure outranks a running pass (it is the one
    /// the user can act on) and a running pass outranks settled totals (they are
    /// not final yet) — asserted with the *settled* set listed first each time,
    /// so an implementation that simply took the first record fails.
    #[test]
    fn a_failure_outranks_running_which_outranks_settled() {
        let settled = with_reseal(
            "set-a",
            fauna_ipc::sync::CorpusResealInfo::Settled {
                resealed: 9,
                owed: 0,
            },
        );
        let running = with_reseal("set-b", fauna_ipc::sync::CorpusResealInfo::Running);
        let failed = with_reseal(
            "set-c",
            fauna_ipc::sync::CorpusResealInfo::Failed {
                reason: "nest unreachable".into(),
            },
        );
        assert_eq!(
            corpus_reseal_progress(&[settled.clone(), running.clone()], SUCCESSOR),
            Some(CorpusResealProgress::Running)
        );
        assert_eq!(
            corpus_reseal_progress(&[settled, running, failed], SUCCESSOR),
            Some(CorpusResealProgress::Failed("nest unreachable".into()))
        );
    }

    /// The idempotent steady state every later catch-up reaches: nothing moved,
    /// nothing owed. Silent, or the user learns to ignore the line that matters.
    #[test]
    fn a_settled_pass_with_nothing_moved_and_nothing_owed_is_silent() {
        let p = CorpusResealProgress::Settled(CorpusResealOutcome::Resealed {
            resealed: 0,
            owed: 0,
        });
        assert_eq!(p.status_line(), None);
        assert!(!p.still_owed());
    }

    /// The partly-owed line is the only one carrying numbers, and it must carry
    /// **both** — "some are still moving" without a magnitude is not progress.
    #[test]
    fn the_partly_owed_line_names_how_much_moved_and_how_much_is_left() {
        let line = CorpusResealProgress::Settled(CorpusResealOutcome::Resealed {
            resealed: 900,
            owed: 3,
        })
        .status_line()
        .expect("a pass with entries still owed must render");
        assert_eq!(line.key, "settings.recovery_kit.corpus_reseal_partly_owed");
        assert_eq!(line.args.get("done"), Some(&"900".to_string()));
        assert_eq!(line.args.get("remaining"), Some(&"3".to_string()));
    }

    /// Every arm that is not "finished with nothing left" keeps the caller
    /// driving — § Re-key scope's "resumed until complete" is exactly this
    /// predicate, and `OwedElsewhere` belongs in it (another device owes it).
    #[test]
    fn every_unfinished_arm_is_still_owed() {
        assert!(CorpusResealProgress::Running.still_owed());
        assert!(CorpusResealProgress::Settled(CorpusResealOutcome::OwedElsewhere).still_owed());
        assert!(CorpusResealProgress::Failed("x".into()).still_owed());
        assert!(
            CorpusResealProgress::Settled(CorpusResealOutcome::Resealed {
                resealed: 1,
                owed: 1
            })
            .still_owed()
        );
        assert!(
            !CorpusResealProgress::Settled(CorpusResealOutcome::Resealed {
                resealed: 1,
                owed: 0
            })
            .still_owed()
        );
    }

    /// The `OwedElsewhere` copy is the one that could frighten a user whose data
    /// is fine, so it is pinned to its own key rather than sharing the failure
    /// one (`settings.recovery_kit.corpus_reseal_owed_elsewhere` names the fix;
    /// the failed key names an outage).
    #[test]
    fn owed_elsewhere_and_failed_are_different_lines() {
        assert_eq!(
            CorpusResealProgress::Settled(CorpusResealOutcome::OwedElsewhere)
                .status_line()
                .expect("owed-elsewhere renders")
                .key,
            "settings.recovery_kit.corpus_reseal_owed_elsewhere"
        );
        let failed = CorpusResealProgress::Failed("boom".into())
            .status_line()
            .expect("failed renders");
        assert_eq!(failed.key, "settings.recovery_kit.corpus_reseal_failed");
        assert_eq!(failed.args.get("reason"), Some(&"boom".to_string()));
    }

    #[tokio::test]
    async fn refresh_bearer_is_unreachable_when_socket_absent() {
        let d = delegate();
        let outcome = d
            .refresh_bearer(&BearerToken::new("tok".into(), 4_000_000_000))
            .await;
        assert_eq!(outcome, RefreshBearerOutcome::Unreachable);
    }

    #[tokio::test]
    async fn reachable_edge_forwards_to_the_observer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counting(Arc<AtomicUsize>);
        impl ReachabilityObserver for Counting {
            fn on_agent_reachable(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let mut d = delegate();
        d.reachability = Some(Arc::new(Counting(count.clone())));
        d.agent_became_reachable().await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// A stable, per-name `FolderRef` wire form for these fixtures: one set per
    /// name unless a test builds a same-named twin by hand.
    fn set_ref(folder: &str) -> String {
        let id = folder.bytes().fold(0i64, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(i64::from(b))
        });
        fauna_core::folder_keys::FolderRef::Local(id).to_wire()
    }

    /// A model holding one pending-bind row per `(path, folder)`, each bound to
    /// that name's [`set_ref`].
    fn with_rows(rows: &[(&str, &str)]) -> LocationBindingsModel {
        let mut model = LocationBindingsModel::default();
        for (path, folder) in rows {
            model.add(path.to_string(), folder.to_string(), set_ref(folder));
        }
        model
    }

    fn agent_row(path: &str, folder: Option<&str>) -> LocationInfo {
        LocationInfo {
            path: path.to_string(),
            file_count: 0,
            total_bytes: 0,
            status: fauna_ipc::sync::LocationStatus::Synced,
            folder: folder.map(str::to_string),
            folder_id: folder.map(set_ref),
            mode: "always".to_string(),
            ..Default::default()
        }
    }

    fn status(available: Option<bool>, reason: Option<&str>) -> fauna_ipc::sync::ServiceStatusInfo {
        fauna_ipc::sync::ServiceStatusInfo {
            on_demand_available: available,
            on_demand_unavailable_reason: reason.map(str::to_string),
            ..Default::default()
        }
    }

    /// One confirmed row at `mode`, with the agent reporting `mount_error`.
    fn model_with_row(mode: &str, mount_error: Option<&str>) -> LocationBindingsModel {
        let mut model = LocationBindingsModel::default();
        model.reconcile(&[LocationInfo {
            mode: mode.to_string(),
            on_demand_mount_error: mount_error.map(str::to_string),
            ..agent_row("/home/a/Docs", Some("docs"))
        }]);
        model
    }

    /// The Availability flow's app half: a host without the mount helper
    /// renders the switch disabled WITH the reason, never hidden and never a
    /// silent no-op (`on-demand-files.md` § Linux FUSE binding).
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn an_unavailable_host_disables_the_switch_with_its_reason() {
        for (code, notice) in [
            (
                fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSERMOUNT,
                OnDemandNotice::NeedsFuse3,
            ),
            (
                fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSE_DEVICE,
                OnDemandNotice::NoFuseDevice,
            ),
            ("a-code-from-a-newer-agent", OnDemandNotice::Unavailable),
        ] {
            let mut model = model_with_row("always", None);
            assert!(model.fold_service_status(&status(Some(false), Some(code))));
            let row = &model.rendered()[0];
            assert_eq!(
                model.mode_toggle(row),
                Some(ModeToggle {
                    on_demand: false,
                    enabled: false,
                    notice: Some(notice),
                }),
                "{code}"
            );
        }
    }

    /// A row already on-demand on a host that lost its helper keeps a live
    /// switch: turning on-demand off is the way out.
    #[test]
    fn an_on_demand_row_on_an_unavailable_host_can_still_be_turned_off() {
        let mut model = model_with_row("on-demand", None);
        model.fold_service_status(&status(
            Some(false),
            Some(fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSERMOUNT),
        ));
        let toggle = model.mode_toggle(&model.rendered()[0]).unwrap();
        assert!(toggle.on_demand && toggle.enabled);
        assert_eq!(toggle.notice, Some(OnDemandNotice::NeedsFuse3));
    }

    /// The per-location refusal reaches the binding's row: a root whose mount
    /// was refused says so, on that row, and only while it is on-demand.
    #[test]
    fn a_refused_mount_is_carried_to_its_own_row() {
        let mut model = model_with_row("on-demand", Some(fauna_ipc::sync::ON_DEMAND_MOUNT_REFUSED));
        assert!(!model.fold_service_status(&status(None, None)));
        model.fold_service_status(&status(Some(true), None));
        assert_eq!(
            model.mode_toggle(&model.rendered()[0]),
            Some(ModeToggle {
                on_demand: true,
                enabled: true,
                notice: Some(OnDemandNotice::MountRefused),
            })
        );

        // Any other code reads as the generic failure.
        let mut model = model_with_row("on-demand", Some("a-newer-code"));
        model.fold_service_status(&status(Some(true), None));
        assert_eq!(
            model.mode_toggle(&model.rendered()[0]).unwrap().notice,
            Some(OnDemandNotice::MountFailed)
        );

        // The watch mirrors it both ways: a later listing without the error
        // (the user flipped back, or a retry mounted) clears the line.
        let mut model = model_with_row("on-demand", None);
        model.fold_service_status(&status(Some(true), None));
        model.fold_parks(&[LocationInfo {
            mode: "on-demand".to_string(),
            on_demand_mount_error: Some(fauna_ipc::sync::ON_DEMAND_MOUNT_FAILED.to_string()),
            ..agent_row("/home/a/Docs", Some("docs"))
        }]);
        assert_eq!(
            model.mode_toggle(&model.rendered()[0]).unwrap().notice,
            Some(OnDemandNotice::MountFailed)
        );
        model.fold_parks(&[agent_row("/home/a/Docs", Some("docs"))]);
        assert_eq!(model.rendered()[0].mount_error, None);
    }

    /// macOS: on-demand is the File Provider extension's, so the agent's
    /// "not hosted here" answer renders no switch at all — never a disabled one.
    #[test]
    fn a_platform_whose_agent_hosts_no_surface_renders_no_switch() {
        let mut model = model_with_row("always", None);
        model.fold_service_status(&status(
            Some(false),
            Some(fauna_ipc::sync::ON_DEMAND_REASON_NOT_AGENT_HOSTED),
        ));
        assert_eq!(model.mode_toggle(&model.rendered()[0]), None);
    }

    /// `folder-location-mode-toggle` reads the row's mode off the model, so the
    /// model must carry the AGENT's answer (`on-demand-files.md` § On-Demand
    /// Files → *The choice is the user's*: the page re-reads the sync host's
    /// record, never view state). An optimistic row the agent has not listed
    /// yet renders the mode the agent's `AddLocation` WILL give it, so the
    /// switch does not flicker between the bind and the reconcile; every
    /// reconcile then mirrors the agent's mode, in both directions.
    #[test]
    fn a_row_carries_the_agents_mode_and_starts_at_the_fresh_binding_default() {
        let mut model = with_rows(&[("/home/a/Docs", "docs")]);
        assert_eq!(
            model.rendered()[0].mode,
            fauna_ipc::sync::fresh_binding_mode(),
            "an unseen row renders the platform's fresh-binding default"
        );

        let mut on_demand = agent_row("/home/a/Docs", Some("docs"));
        on_demand.mode = "on-demand".to_string();
        model.reconcile(&[on_demand]);
        assert_eq!(model.rendered()[0].mode, "on-demand");

        // Confirmed row: a flip made agent-side is mirrored on the next pass.
        model.reconcile(&[agent_row("/home/a/Docs", Some("docs"))]);
        assert_eq!(model.rendered()[0].mode, "always");
    }

    /// A binding made from another control surface (the windows app, another
    /// tui) is adopted with the agent's mode, not a guessed default.
    #[test]
    fn an_adopted_row_takes_the_agents_mode() {
        let mut model = LocationBindingsModel::default();
        let mut row = agent_row("/home/a/Pix", Some("pix"));
        row.mode = "on-demand".to_string();
        model.reconcile(&[row]);
        assert_eq!(model.rendered()[0].mode, "on-demand");
    }

    // Pin (i): agent unreachable at attach → pending rows stay rendered; the
    // reachable-edge reconcile pushes them.
    #[test]
    fn pending_rows_survive_an_unreachable_attach_and_push_on_the_edge() {
        let mut model = with_rows(&[("/home/a/Docs", "docs")]);
        // Attach-time reconcile never ran (agent down) — the row still renders.
        assert_eq!(model.rendered().len(), 1);

        // Edge fires → reconcile against an empty (fresh) agent: push the bind.
        let actions = model.reconcile(&[]);
        assert_eq!(
            actions.to_bind,
            vec![PendingBind {
                path: "/home/a/Docs".to_string(),
                folder: "docs".to_string(),
                folder_id: set_ref("docs"),
            }]
        );
        assert!(actions.to_unbind.is_empty());
        // Still rendered while pending.
        assert_eq!(model.rendered().len(), 1);

        // Push succeeded → confirmed; the next reconcile is a no-op.
        model.confirm_bind("/home/a/Docs");
        let actions = model.reconcile(&[agent_row("/home/a/Docs", Some("docs"))]);
        assert_eq!(actions, ReconcileActions::default());
        assert_eq!(model.rendered()[0].state, BindingState::Confirmed);
    }

    // Pin (ii): a row whose bind push failed stays rendered (face (b)) and
    // re-pushes on the next reconcile.
    #[test]
    fn failed_bind_row_stays_rendered_and_repushes() {
        let mut model = LocationBindingsModel::default();
        model.add("/home/a/Pix".to_string(), "pix".to_string(), set_ref("pix"));
        // Push failed (no confirm_bind call). Still rendered.
        assert_eq!(model.rendered().len(), 1);
        // Next reconcile re-pushes.
        let actions = model.reconcile(&[]);
        assert_eq!(
            actions.to_bind,
            vec![PendingBind {
                path: "/home/a/Pix".to_string(),
                folder: "pix".to_string(),
                folder_id: set_ref("pix"),
            }]
        );
        assert_eq!(model.rendered().len(), 1);
    }

    // Pin (iii): add-while-down, then the edge → pushed.
    #[test]
    fn add_while_agent_down_pushes_on_the_edge() {
        let mut model = LocationBindingsModel::default();
        model.add(
            "/home/a/Music".to_string(),
            "music".to_string(),
            set_ref("music"),
        );
        let actions = model.reconcile(&[]);
        assert_eq!(
            actions.to_bind,
            vec![PendingBind {
                path: "/home/a/Music".to_string(),
                folder: "music".to_string(),
                folder_id: set_ref("music"),
            }]
        );
    }

    #[test]
    fn a_pending_row_the_agent_already_holds_confirms_without_pushing() {
        let mut model = with_rows(&[("/home/a/Docs", "docs")]);
        let actions = model.reconcile(&[agent_row("/home/a/Docs", Some("docs"))]);
        assert_eq!(actions, ReconcileActions::default());
        assert_eq!(model.rendered()[0].state, BindingState::Confirmed);
    }

    #[test]
    fn remove_while_agent_down_repushes_unbind_until_agent_drops_it() {
        let mut model = with_rows(&[("/home/a/Docs", "docs")]);
        model.confirm_bind("/home/a/Docs");

        let paths = model.remove_by_set("docs");
        assert_eq!(paths, vec!["/home/a/Docs".to_string()]);
        // Hidden from render immediately (optimistic remove).
        assert!(model.rendered().is_empty());

        // Agent still lists it (the unbind push failed) → re-push.
        let actions = model.reconcile(&[agent_row("/home/a/Docs", Some("docs"))]);
        assert_eq!(actions.to_unbind, vec!["/home/a/Docs".to_string()]);

        // Agent dropped it → row gone for good, nothing further to push.
        let actions = model.reconcile(&[]);
        assert_eq!(actions, ReconcileActions::default());
        assert!(model.rendered().is_empty());
    }

    /// The reason `remove_by_path` exists: windows' per-row remove button acts on
    /// ONE folder, and two folders may share a folder. `remove_by_set` would
    /// take both — a silent unbind of a row the user never touched.
    #[test]
    fn remove_by_path_takes_only_that_row_when_two_folders_share_a_set() {
        let mut model = with_rows(&[("/home/a/Docs", "shared"), ("/home/a/More", "shared")]);
        model.confirm_bind("/home/a/Docs");
        model.confirm_bind("/home/a/More");

        let paths = model.remove_by_path("/home/a/Docs");
        assert_eq!(paths, vec!["/home/a/Docs".to_string()]);

        let rendered = model.rendered();
        assert_eq!(rendered.len(), 1, "the sibling row must survive");
        assert_eq!(rendered[0].path, "/home/a/More");

        // Same re-push semantics as remove_by_set: the agent still lists it.
        let actions = model.reconcile(&[
            agent_row("/home/a/Docs", Some("shared")),
            agent_row("/home/a/More", Some("shared")),
        ]);
        assert_eq!(actions.to_unbind, vec!["/home/a/Docs".to_string()]);

        // ...and by contrast, remove_by_set would have taken both.
        let mut both = with_rows(&[("/home/a/Docs", "shared"), ("/home/a/More", "shared")]);
        assert_eq!(both.remove_by_set("shared").len(), 2);
    }

    /// The join is the set's identity, never its label: an agent row wearing
    /// the same NAME but bound to a different set is not this row's binding —
    /// confirming it would render a folder as synced to a set it is not synced
    /// to — so the row re-pushes.
    #[test]
    fn a_same_named_agent_row_bound_to_another_set_does_not_confirm() {
        let mut model = with_rows(&[("/home/a/Docs", "docs")]);
        let other_docs = LocationInfo {
            folder_id: Some(fauna_core::folder_keys::FolderRef::Local(-5).to_wire()),
            ..agent_row("/home/a/Docs", Some("docs"))
        };
        let actions = model.reconcile(&[other_docs]);
        assert_eq!(
            actions.to_bind,
            vec![PendingBind {
                path: "/home/a/Docs".to_string(),
                folder: "docs".to_string(),
                folder_id: set_ref("docs"),
            }]
        );
        assert_eq!(model.rendered()[0].state, BindingState::PendingBind);
    }

    #[test]
    fn agent_side_rows_from_other_surfaces_are_adopted() {
        let mut model = LocationBindingsModel::default();
        let actions = model.reconcile(&[
            agent_row("/home/a/Tui", Some("tui-set")),
            // An added-but-unbound folder is not a renderable binding.
            agent_row("/home/a/Unbound", None),
        ]);
        assert_eq!(actions, ReconcileActions::default());
        let rendered = model.rendered();
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].folder, "tui-set");
        assert_eq!(
            rendered[0].folder_id,
            set_ref("tui-set"),
            "an adopted row is keyed by the ref the agent reports"
        );
        assert_eq!(rendered[0].state, BindingState::Confirmed);
    }

    #[test]
    fn a_confirmed_row_the_agent_lost_repushes_instead_of_vanishing() {
        let mut model = with_rows(&[("/home/a/Docs", "docs")]);
        model.confirm_bind("/home/a/Docs");
        // Agent config reset: it no longer lists the row. The union re-pushes.
        let actions = model.reconcile(&[]);
        assert_eq!(
            actions.to_bind,
            vec![PendingBind {
                path: "/home/a/Docs".to_string(),
                folder: "docs".to_string(),
                folder_id: set_ref("docs"),
            }]
        );
        assert_eq!(model.rendered().len(), 1);
    }

    fn parked_agent_row(path: &str, folder: &str) -> LocationInfo {
        LocationInfo {
            access_revoked: true,
            ..agent_row(path, Some(folder))
        }
    }

    /// D4: the agent's park must reach the rendered row, in **both** directions.
    ///
    /// This is the link the client's revocation warning is drawn from
    /// (`folder-access-revoked-warning`), and it is exactly the kind of seam
    /// that goes quiet: the model would happily keep serving a stale `false`
    /// forever, so the folder would look healthy while the agent had already
    /// stopped syncing it — the silent un-sync the whole feature exists to
    /// prevent. The clear direction matters just as much: after a re-bind
    /// restores the grant, a sticky warning would tell the user their working
    /// folder is dead when it is not.
    #[test]
    fn a_parked_binding_is_mirrored_onto_the_rendered_row_and_cleared_again() {
        let mut model = with_rows(&[("/home/a/Shared", "photos")]);

        // Confirm the row first, so the park lands on an established binding
        // rather than riding in on the initial adopt.
        model.reconcile(&[agent_row("/home/a/Shared", Some("photos"))]);
        assert!(
            !model.rendered()[0].access_revoked,
            "a live binding renders unparked"
        );

        model.reconcile(&[parked_agent_row("/home/a/Shared", "photos")]);
        assert!(
            model.rendered()[0].access_revoked,
            "the agent parked this binding; the rendered row must say so"
        );

        model.reconcile(&[agent_row("/home/a/Shared", Some("photos"))]);
        assert!(
            !model.rendered()[0].access_revoked,
            "a re-granted binding clears — the warning must not outlive the revocation"
        );
    }

    /// A binding first *seen* parked (the agent was already in that state when
    /// this client attached — e.g. the app restarted after the demotion) adopts
    /// the park too, not just one observed transitioning.
    #[test]
    fn a_binding_adopted_while_already_parked_renders_parked() {
        let mut model = LocationBindingsModel::default();
        model.reconcile(&[parked_agent_row("/home/a/Shared", "photos")]);
        let rows = model.rendered();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].access_revoked,
            "adopting an agent row must carry its park, or a restart hides the revocation"
        );
    }

    /// The park is a level the agent derives on its own — the nest refused a
    /// write — so the status poll must be able to watch it without a reconcile.
    /// Measured 2026-09-22 by the first app-driven witness of revocation: the
    /// agent had parked the binding (`access_revoked = true` in its config) and
    /// the member's row never said so, because the reconcile that mirrors the
    /// park runs only on a mutation or a reachability edge.
    #[test]
    fn fold_parks_mirrors_the_park_without_reconciling_anything() {
        let mut model = with_rows(&[("/home/a/Shared", "photos")]);
        model.reconcile(&[agent_row("/home/a/Shared", Some("photos"))]);
        let before = model.rendered();

        model.fold_parks(&[parked_agent_row("/home/a/Shared", "photos")]);
        let parked = model.rendered();
        assert!(
            parked[0].access_revoked,
            "the watch must carry the park onto the row"
        );
        assert_eq!(
            BindingRow {
                access_revoked: false,
                ..parked[0].clone()
            },
            before[0],
            "the park is the only thing a watch may change"
        );

        model.fold_parks(&[agent_row("/home/a/Shared", Some("photos"))]);
        assert!(
            !model.rendered()[0].access_revoked,
            "a re-bind's clear is mirrored too"
        );

        // A row the agent reports under another set, and a row it does not
        // report at all, are the reconcile's to settle — the watch leaves them.
        model.fold_parks(&[parked_agent_row("/home/a/Shared", "other")]);
        assert!(!model.rendered()[0].access_revoked);
        model.fold_parks(&[parked_agent_row("/home/a/Elsewhere", "photos")]);
        assert_eq!(model.rendered().len(), 1, "a watch adopts nothing");
        assert!(!model.rendered()[0].access_revoked);
    }

    fn engine_row(folder: &str, deletes_held: u64) -> fauna_ipc::sync::EngineInfo {
        fauna_ipc::sync::EngineInfo {
            folder: folder.to_string(),
            folder_id: set_ref(folder),
            deletes_held,
            ..Default::default()
        }
    }

    /// The mass-delete floor's hold reaches the rendered binding row — the one
    /// link the *"your folder emptied — N deletions held"* affordance is drawn
    /// from (`delete-propagation.md` § A wholesale-vanished folder …).
    ///
    /// The hold is reported per **set** (`EngineInfo.folder`) and rendered per
    /// **bound path**, so the fold is a join, not a copy: two folders bound to
    /// the same set both carry its hold, and a set with no engine row carries
    /// none.
    #[test]
    fn an_engine_hold_reaches_every_row_bound_to_that_set() {
        let mut model = with_rows(&[
            ("/home/a/Docs", "docs"),
            ("/mnt/ext/Docs", "docs"),
            ("/home/a/Pics", "photos"),
        ]);
        model.reconcile(&[
            agent_row("/home/a/Docs", Some("docs")),
            agent_row("/mnt/ext/Docs", Some("docs")),
            agent_row("/home/a/Pics", Some("photos")),
        ]);

        model.fold_engine_holds(&[engine_row("docs", 12), engine_row("photos", 0)]);

        let rows = model.rendered();
        let held = |path: &str| {
            rows.iter()
                .find(|r| r.path == path)
                .unwrap_or_else(|| panic!("no row for {path}"))
                .deletes_held
        };
        assert_eq!(
            held("/home/a/Docs"),
            12,
            "the set's hold reaches its binding"
        );
        assert_eq!(
            held("/mnt/ext/Docs"),
            12,
            "a second folder bound to the same set carries the same hold — the fold \
             is keyed by SET, not by path"
        );
        assert_eq!(
            held("/home/a/Pics"),
            0,
            "a set that holds nothing must not bleed its sibling's hold"
        );
    }

    /// The unreadable-path count rides the same roster fold as the hold: a
    /// join by set onto every bound row, never added into the hold, a zero
    /// clears it, and a set absent from the roster reads 0
    /// (`delete-propagation.md` § Unreadable is not absent).
    #[test]
    fn an_unreadable_count_reaches_every_row_bound_to_that_set_and_mirrors() {
        let mut model = with_rows(&[
            ("/home/a/Pics", "photos"),
            ("/mnt/nas/Pics", "photos"),
            ("/home/a/Docs", "docs"),
        ]);
        model.reconcile(&[
            agent_row("/home/a/Pics", Some("photos")),
            agent_row("/mnt/nas/Pics", Some("photos")),
            agent_row("/home/a/Docs", Some("docs")),
        ]);
        let unreadable = |model: &LocationBindingsModel, path: &str| {
            model
                .rendered()
                .iter()
                .find(|r| r.path == path)
                .unwrap_or_else(|| panic!("no row for {path}"))
                .deletes_skipped_unreadable
        };

        let photos = fauna_ipc::sync::EngineInfo {
            deletes_skipped_unreadable: 5,
            ..engine_row("photos", 0)
        };
        model.fold_engine_holds(&[photos, engine_row("docs", 0)]);
        assert_eq!(unreadable(&model, "/home/a/Pics"), 5);
        assert_eq!(
            unreadable(&model, "/mnt/nas/Pics"),
            5,
            "keyed by SET: a second folder bound to it carries the count too"
        );
        assert_eq!(unreadable(&model, "/home/a/Docs"), 0, "no bleed");
        assert!(
            model.rendered().iter().all(|r| r.deletes_held == 0),
            "an unreadable path is never a hold — it offers nothing to apply"
        );

        model.fold_engine_holds(&[engine_row("photos", 0), engine_row("docs", 0)]);
        assert_eq!(unreadable(&model, "/home/a/Pics"), 0, "a zero clears it");

        let photos = fauna_ipc::sync::EngineInfo {
            deletes_skipped_unreadable: 5,
            ..engine_row("photos", 0)
        };
        model.fold_engine_holds(&[photos]);
        model.fold_engine_holds(&[]);
        assert_eq!(
            unreadable(&model, "/home/a/Pics"),
            0,
            "a set absent from the roster reads 0 — no live engine stands behind a count"
        );
    }

    /// A zero is the only thing that ever retracts a displayed hold
    /// (`delete-propagation.md` § Implementation status today, 2026-08-10, point
    /// 3), so the fold must be a **mirror**, never an accumulate-and-keep: the
    /// remounted drive's next pass clears the surface.
    #[test]
    fn a_zero_report_clears_a_displayed_hold() {
        let mut model = with_rows(&[("/mnt/ext/Docs", "docs")]);
        model.reconcile(&[agent_row("/mnt/ext/Docs", Some("docs"))]);

        model.fold_engine_holds(&[engine_row("docs", 9)]);
        assert_eq!(model.rendered()[0].deletes_held, 9);

        model.fold_engine_holds(&[engine_row("docs", 0)]);
        assert_eq!(
            model.rendered()[0].deletes_held,
            0,
            "the drive came back; a sticky hold would offer to delete files that are present"
        );
    }

    /// An engine that stopped (or an agent that restarted, or one too old to
    /// report at all) leaves **no** row in the roster — and the reading of an
    /// absent row is `0`, not "keep the last number we saw". This is the
    /// derived-never-stored lifetime made visible: the affordance must never be
    /// handed a count no live engine stands behind.
    #[test]
    fn a_set_absent_from_the_roster_reads_as_no_hold() {
        let mut model = with_rows(&[("/mnt/ext/Docs", "docs")]);
        model.reconcile(&[agent_row("/mnt/ext/Docs", Some("docs"))]);
        model.fold_engine_holds(&[engine_row("docs", 4)]);
        assert_eq!(model.rendered()[0].deletes_held, 4);

        // The engine stopped: the agent's drain removed its entry, so ListEngines
        // no longer carries the set.
        model.fold_engine_holds(&[]);
        assert_eq!(
            model.rendered()[0].deletes_held,
            0,
            "no live engine stands behind the old count — an absent row reads as no hold"
        );
    }

    /// The apply reply's own count lands on the surface without waiting for the
    /// next roster. `HeldDeletesAppliedInfo::remaining_held` is the *post-apply*
    /// truth, where a `ListEngines` issued immediately after the click can still
    /// read the pre-apply number — the agent's progress drain is asynchronous —
    /// so repainting from the reply is what makes the surface honest at the one
    /// moment the user is looking at it.
    #[test]
    fn the_apply_reply_repaints_only_its_own_set() {
        let mut model = with_rows(&[("/mnt/ext/Docs", "docs"), ("/home/a/Pics", "photos")]);
        model.reconcile(&[
            agent_row("/mnt/ext/Docs", Some("docs")),
            agent_row("/home/a/Pics", Some("photos")),
        ]);
        model.fold_engine_holds(&[engine_row("docs", 6), engine_row("photos", 3)]);

        model.set_engine_hold("docs", 0);

        let rows = model.rendered();
        let held = |path: &str| rows.iter().find(|r| r.path == path).unwrap().deletes_held;
        assert_eq!(
            held("/mnt/ext/Docs"),
            0,
            "the applied set's hold is cleared"
        );
        assert_eq!(
            held("/home/a/Pics"),
            3,
            "a single set's apply must not blank every other set's hold"
        );
    }

    /// A partial apply (a crash mid-run, or a floor that re-engaged) leaves a
    /// remainder, and the surface must keep offering the verb for it rather than
    /// reading "done".
    #[test]
    fn a_partial_apply_keeps_the_remainder_on_the_surface() {
        let mut model = with_rows(&[("/mnt/ext/Docs", "docs")]);
        model.reconcile(&[agent_row("/mnt/ext/Docs", Some("docs"))]);
        model.fold_engine_holds(&[engine_row("docs", 10)]);

        model.set_engine_hold("docs", 4);
        assert_eq!(model.rendered()[0].deletes_held, 4);
    }

    /// A hold must survive an ordinary binding reconcile — the two folds run on
    /// different cadences (locations at every mutation, engines at the status
    /// poll), so a `reconcile` that reset the hold would blank the surface
    /// between polls and make it flicker.
    #[test]
    fn a_binding_reconcile_does_not_blank_a_held_row() {
        let mut model = with_rows(&[("/mnt/ext/Docs", "docs")]);
        model.reconcile(&[agent_row("/mnt/ext/Docs", Some("docs"))]);
        model.fold_engine_holds(&[engine_row("docs", 7)]);

        model.reconcile(&[agent_row("/mnt/ext/Docs", Some("docs"))]);
        assert_eq!(
            model.rendered()[0].deletes_held,
            7,
            "the locations reconcile knows nothing about holds; it must not clear one"
        );
    }
}

#[cfg(test)]
mod onboarding_reconcile_tests {
    use super::*;

    const ACTOR_A: [u8; 32] = [0xAA; 32];
    const ACTOR_B: [u8; 32] = [0xBB; 32];

    fn marker_for(actor: [u8; 32]) -> fauna_ipc::sync::SignedOutMarker {
        fauna_ipc::sync::SignedOutMarker::new(actor.to_vec(), 2_000)
    }

    /// The case the reconcile exists for: the agent still advertises the
    /// exact account this machine recorded signing out of.
    #[test]
    fn a_matching_advertised_actor_licenses_the_unprovision() {
        let marker = marker_for(ACTOR_A);
        assert!(onboarding_reconcile_licensed(
            &marker,
            Some(hex::encode(ACTOR_A).as_str())
        ));
    }

    /// The agent's slot is single, so a sibling app sitting at account A's
    /// onboarding screen must not tear down account B's live sync just
    /// because the agent happens to be reachable and serving someone.
    #[test]
    fn an_advertised_actor_for_another_account_is_not_licensed() {
        let marker = marker_for(ACTOR_A);
        assert!(!onboarding_reconcile_licensed(
            &marker,
            Some(hex::encode(ACTOR_B).as_str())
        ));
    }

    /// No advertisement — an idle agent, or one whose principal grant is not
    /// registered yet — is an ambiguous read, and
    /// every ambiguous read leaves the agent exactly as it was: the periodic
    /// renewal-loop reconcile remains the backstop.
    #[test]
    fn no_advertised_actor_is_not_licensed() {
        let marker = marker_for(ACTOR_A);
        assert!(!onboarding_reconcile_licensed(&marker, None));
    }

    /// A record that names no account must not license anything — the same
    /// fail-closed rule [`fauna_ipc::sync::capability_is_signed_out`] applies
    /// to a malformed marker.
    #[test]
    fn a_malformed_marker_actor_id_is_not_licensed() {
        let marker = fauna_ipc::sync::SignedOutMarker::new(vec![0xAA; 8], 2_000);
        assert!(!onboarding_reconcile_licensed(
            &marker,
            Some(hex::encode(ACTOR_A).as_str())
        ));
    }
}
