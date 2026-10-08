//! The account-store runtime host — **the agent as the always-on engine
//! singleton** (W5 (account-data-plane.md § Workstreams).5b; `account-data-plane.md` § The store device principal →
//! R2 (account-data-plane.md § The ratified decisions), "replica freshness while no app runs arrives at W5 when the agent mounts
//! the store as the always-on host").
//!
//! # What this module is
//!
//! Every other host of [`AccountStoreRuntime`] is an *app* — a process that
//! holds the account's identity seed and dies with its window. This one holds
//! no seed ([`RuntimePrincipal::Seedless`], W5.5a) and outlives every app on the
//! machine, which is the whole point: an account whose apps are all closed still
//! walks its scopes, drains its outbox and publishes its endpoints.
//!
//! **This host is what gives the agent the engine role** (`account-runtime.md`
//! § Multi-instance concurrency → *The agent holds the role when present*,
//! ruled 2026-10-08). The election itself is first-come — the runtime's W5.1
//! `elect_at_start`, `flock` on `<store dir>/engine.lock` — and T9's "priority
//! is behavioral" premise did not hold: every desktop app starts its own
//! runtime at the post-auth hook that provisions the agent, so the app usually
//! won and kept the role. So the mount takes the store's **agent presence
//! lock** (`<store dir>/agent.lock`, exclusive, blocking) *before* the runtime
//! starts and holds it for the stint ([`Mounted`]); every seed-holding runtime
//! probes it — at assembly and at each re-try it does not contend while the
//! agent is present, and one that holds the role hands it over between passes
//! (legs down, its lock released, the role fact flipped). This runtime is
//! seedless, so it never probes and never takes the lock itself: it contends
//! as a non-holder until the app beside it yields, and takes the free role on
//! its tick or its `reconcile_now`. When the agent exits or crashes the kernel
//! drops both its locks and an app takes the role back at its next re-try. A
//! presence acquire that fails mounts anyway — the lock states priority,
//! `engine.lock` keeps exclusivity, and the apps then see the first-come
//! election.
//!
//! # Why it lives beside the renewal + custodian loops
//!
//! Same reason both of those do ([`crate::custodian`] is the shape this module
//! copies): they must keep running app-dead. The three are spawned together in
//! [`crate::service::run_agent`] and torn down by the same shutdown watch.
//!
//! # The store it mounts is NOT its own
//!
//! [`StoreRoot::platform`] — the one per-user root every app on this machine
//! resolves — deliberately, and never a dir derived from the agent's own
//! `--data-dir`. Giving the agent a private store dir is the two-journals-under-
//! one-writer-key shape that W6's path unification exists to prevent: both
//! stores would publish under the same [`WriterId`] and the divergence would
//! surface only as `AccountStore::ingest`'s journal-equivocation refusal, after
//! both had published. Isolation for tests and e2e rides `HOME`/`XDG_CONFIG_HOME`
//! (e2e-conventions.md § point 10), which is what `platform()` reads — so an
//! isolated launch gets an isolated store *by construction*, with no second code
//! path to drift.
//!
//! [`WriterId`]: fauna_account_store::WriterId

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, AgentPresenceLock,
    AgentPresenceLockOutcome, DEFAULT_BACKSTOP_INTERVAL, RuntimePrincipal, StoreRoot,
    production_credential_store,
};
use fauna_sync_engine::principal_bundle::load_writer_key;

use crate::state::SyncServiceState;

/// How long an idle host waits before re-reading the capability slot.
///
/// **Deliberately much shorter than [`crate::custodian`]'s 60 s, despite this
/// loop being modelled on it.** That loop pays a nest *registry read* per tick,
/// so its cadence is a real cost; this one only takes an in-memory read lock on
/// the capability slot, which is free. The tick is therefore a latency path
/// after all: it is how long after an app signs in that this machine starts
/// syncing app-dead, and the first measurement of the 60 s copy showed exactly
/// that — a provisioned agent sat idle for a full minute before mounting.
const IDLE_RECHECK_SECS: u64 = 5;

/// How often a *live* stint re-reads the capability to notice an account switch
/// or a sign-out. Same 10 s observer cadence the apps poll the store's change
/// counter on (`session.rs::ACCOUNT_STORE_POLL_INTERVAL`).
const STINT_RECHECK_SECS: u64 = 10;

/// Backoff bounds for an assembly that refused. The common cause is benign and
/// self-healing — **no signed-in app has enrolled this machine yet**, so the T10
/// slot carries no `BackupKey` and W5.5a's refusal fires (it must: a host that
/// invented one would seal rows the account's own owner could never open). The
/// cure is a human signing in, so this backs off to minutes rather than
/// retrying hot.
const ASSEMBLY_BACKOFF_START_SECS: u64 = 5;
const ASSEMBLY_BACKOFF_MAX_SECS: u64 = 300;

/// What the capability must carry before this host can assemble anything.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostInputs {
    actor_id: [u8; 32],
    actor_id_hex: String,
    nest_url: String,
    /// The `sync_devices` row this machine is known by — the capability's own
    /// `device_id`, the app's derived id, which is also what this agent
    /// advertises to co-located apps (`ServiceStatusInfo::sync_device_id`). It
    /// is the enrollment target, unconditionally (`sync-agent-credentials.md`
    /// § Credential model → the RULED 2026-09-28 block, decision 3).
    sync_device_id: String,
    /// The account's **attested** succeeded-from identities — the R14 fleet
    /// view's `prior` (`account-data-taxonomy.md` § The generation machinery →
    /// *The source of `prior`*, ruled 2026-09-13), as the identity-holding app
    /// pushed them over `SyncCapability::predecessor_actor_ids`.
    ///
    /// This process holds no account registry and so cannot attest a
    /// predecessor itself; it trusts the app over the capability pipe for this
    /// exactly as it does for `backup_key`. It deliberately does NOT read the
    /// account-plane replica it mirrors for the list (`fauna.state.succession-ledger`)
    /// — that list is writer-asserted and carried across a succession unmarked. Empty for an
    /// identity that never succeeded, which is fail-safe: predecessor-signed enrollments drop out
    /// of this host's view; nothing is admitted. Part of the mount's
    /// identity, so a push that changes the list re-mounts.
    ///
    /// The value also carries each predecessor's **delegable** schedule,
    /// derived from the retired owner keys the same capability already
    /// carries for the chunk corpus (`SyncCapability::predecessor_backup_keys`)
    /// — what lets the runtime this host mounts carry a predecessor's
    /// delegable rows app-dead (`succession-aftermath.md` § Re-key scope). No
    /// new capability field: the two lists are one registry walk's.
    attested_predecessors: fauna_sync_engine::attested_predecessors::AttestedPredecessors,
    /// Whether this mount hosts the device-principal leg. **The leg follows
    /// the refusal, not the slot** (`sync-agent-credentials.md` § Credential
    /// model → *A rejected credential asks at once*): it signs as the
    /// principal the nest refused, so while the refusal record stands the
    /// mount hosts none and the runtime rides the capability bearer, as on a
    /// machine no app has enrolled — an app's re-armed bearer brings back the
    /// capability clients, never the leg. Part of the mount's identity, so the
    /// renewal that ends the refusal re-mounts with the leg.
    device_leg: bool,
}

/// Read the provisioned capability's account identity, or `None` when there is
/// nothing to host (no capability yet, or a malformed actor id — which the
/// engines already refuse elsewhere, so this stays quiet rather than loud).
///
/// `None` too while the slot's bearer is empty
/// ([`crate::bearer::renewal_refused`]): the nest refused this capability's
/// renewal and no app has re-armed it, so there is no bearer for a mount to
/// ride. The stint comes down on its next recheck and nothing mounts until an
/// app pushes one — and then without the device-principal leg, for as long as
/// the refusal stands ([`HostInputs::device_leg`]).
async fn host_inputs(state: &SyncServiceState) -> Option<HostInputs> {
    let cap = state.capability.read().await;
    let cap = cap.as_ref()?;
    if crate::bearer::renewal_refused(cap) {
        return None;
    }
    let actor_id = cap.actor_id_array()?;
    Some(HostInputs {
        device_leg: crate::renewal::standing_refusal(state).is_none(),
        actor_id,
        actor_id_hex: hex::encode(actor_id),
        nest_url: cap.nest_url.clone(),
        sync_device_id: cap.device_id.clone(),
        attested_predecessors: attested_predecessors(
            cap.predecessor_actor_ids(),
            cap.predecessor_backup_keys(),
        ),
    })
}

/// The capability's two predecessor lists as the one value the runtime takes.
/// The retired keys are contained here: each is wrapped (and so zeroized on
/// drop) before its delegable branch is derived, and nothing keeps the key.
fn attested_predecessors(
    actor_ids: Vec<[u8; 32]>,
    retired_backup_keys: Vec<[u8; 32]>,
) -> fauna_sync_engine::attested_predecessors::AttestedPredecessors {
    let retired: Vec<fauna_core::crypto::BackupKey> = retired_backup_keys
        .into_iter()
        .map(fauna_core::crypto::BackupKey::from_bytes)
        .collect();
    fauna_sync_engine::attested_predecessors::AttestedPredecessors::from_lists(
        actor_ids
            .into_iter()
            .map(fauna_core::identity::ActorId)
            .collect(),
        retired.iter(),
    )
}

/// The account-store host task. Spawned by `run_agent` beside the renewal and
/// custodian loops; returns when `shutdown` flips.
///
/// Idles at one in-memory capability read every [`IDLE_RECHECK_SECS`] on an
/// unprovisioned box — this process runs on every desktop, and most of the time
/// there is nothing to mount.
pub async fn run(state: Arc<SyncServiceState>, mut shutdown: watch::Receiver<bool>) {
    let mut backoff = fauna_protocol::reconnect::Backoff::new(
        Duration::from_secs(ASSEMBLY_BACKOFF_START_SECS),
        Duration::from_secs(ASSEMBLY_BACKOFF_MAX_SECS),
    );
    loop {
        // Re-read the shutdown value at the top of every iteration, rather than
        // relying only on the `changed()` arm below.
        //
        // This is load-bearing, not defensive. `watch::Receiver::changed()`
        // resolves once per *transition*, and `host_stint` takes the same
        // receiver — so when shutdown fires while a mount is live, the stint's
        // own select consumes that transition. The outer `changed()` would then
        // wait for a second transition that never comes, and this task would
        // re-assemble the runtime and park forever. `run_agent` awaits this
        // handle on the orderly-exit path, so that park is an agent that never
        // exits: SIGTERM would hang instead of tearing down (the exact class
        // `tests/agent_sigterm.rs` exists to catch, which it would miss here
        // because it runs with no capability provisioned and so never mounts).
        if *shutdown.borrow() {
            return;
        }

        let delay = match host_inputs(&state).await {
            None => Duration::from_secs(IDLE_RECHECK_SECS),
            Some(inputs) => match host_stint(&state, &inputs, &mut shutdown).await {
                Ok(()) => {
                    backoff.reset();
                    Duration::from_secs(1)
                }
                Err(e) => {
                    // Expected on an unenrolled machine (no `BackupKey` in the
                    // slot yet) — `debug`, not `warn`, so a box whose user has
                    // never signed in does not fill its log with a condition
                    // only a sign-in can change.
                    tracing::debug!(
                        error = %e,
                        retry_in_secs = backoff.ceiling().as_secs(),
                        "account host: assembly unavailable; will retry"
                    );
                    let d = backoff.ceiling();
                    backoff.grow();
                    d
                }
            },
        };

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// Mount the store for one account and hold it until that stops being the
/// truth (the capability's actor changed, the capability went away, or the
/// agent is shutting down).
///
/// Returns `Ok(())` when the stint ended for a reason the caller should simply
/// re-resolve from; `Err` only when the assembly itself refused.
async fn host_stint(
    state: &SyncServiceState,
    inputs: &HostInputs,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    hold_stint(state, inputs, shutdown, || assemble(state, inputs)).await
}

/// What a stint tears down when it ends — [`Mounted`] in production.
///
/// A trait only so the tests can drive the real [`hold_stint`] (the
/// publication, the wake and the ordering of the slot's clear) with a mount
/// that needs no nest.
trait StintMount {
    /// Deterministic teardown; the stint clears
    /// [`SyncServiceState::hosted_account`] only after this returns.
    async fn tear_down(self);

    /// The account store the mount holds — published on
    /// [`SyncServiceState::mounted_store`] while the stint stands. `None` for a
    /// mount with no store (a test's).
    fn store_handle(&self) -> Option<AccountStoreHandle> {
        None
    }
}

/// The body of [`host_stint`], over any mount.
///
/// **The slot is the un-provision's receipt, so its three edges are ordered:**
///
/// 1. It is published BEFORE the capability is re-read and before assembly
///    starts. An un-provision clears the capability and then waits for the slot
///    to empty; publishing first means either this re-read already sees the
///    capability gone (and the stint never mounts), or the waiter sees the slot
///    taken and waits for the teardown below. Publishing after the re-read
///    would leave a window where the waiter finds the slot empty, replies, and
///    the mount comes up under the erase.
/// 2. The hold wakes on [`SyncServiceState::host_wake`], not only on its
///    recheck tick — the reply waits on this, so a tick would be the reply's
///    latency. A wake that arrives mid-assembly is kept by `Notify` as a permit
///    and ends the hold on its first iteration.
/// 3. It is cleared only AFTER the mount's teardown has returned, on every
///    exit path — a refused assembly included.
async fn hold_stint<M, F, Fut>(
    state: &SyncServiceState,
    inputs: &HostInputs,
    shutdown: &mut watch::Receiver<bool>,
    mount: F,
) -> anyhow::Result<()>
where
    M: StintMount,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<M>>,
{
    state.hosted_account.send_replace(Some(inputs.actor_id));
    if host_inputs(state).await.as_ref() != Some(inputs) {
        state.hosted_account.send_replace(None);
        return Ok(());
    }
    let mounted = match mount().await {
        Ok(mounted) => mounted,
        Err(e) => {
            state.hosted_account.send_replace(None);
            return Err(e);
        }
    };
    publish_store(state, mounted.store_handle());
    tracing::info!(
        actor_id = %inputs.actor_id_hex,
        "account host: mounted the account store as the app-dead runtime host"
    );

    // Hold. The runtime pumps on its own cadence (or reads plainly, if an app
    // holds the election); this loop only watches for the mount becoming wrong.
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(STINT_RECHECK_SECS)) => {}
            _ = state.host_wake.notified() => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
                continue;
            }
        }
        if host_inputs(state).await.as_ref() != Some(inputs) {
            tracing::info!(
                actor_id = %inputs.actor_id_hex,
                "account host: capability changed; tearing the mount down"
            );
            break;
        }
    }

    // The store leaves the custody reader before it closes.
    publish_store(state, None);
    mounted.tear_down().await;
    state.hosted_account.send_replace(None);
    Ok(())
}

/// Publish (or withdraw) the mounted store for the agent's custody reader,
/// then wake the content-key resolution: what custody reads changed.
fn publish_store(state: &SyncServiceState, handle: Option<AccountStoreHandle>) {
    if let Ok(mut slot) = state.mounted_store.lock() {
        *slot = handle;
    }
    state.content_keys_wake.notify_one();
}

impl StintMount for Mounted {
    fn store_handle(&self) -> Option<AccountStoreHandle> {
        Some(self.handle.clone())
    }

    async fn tear_down(self) {
        let Mounted {
            handle,
            device,
            push_arm,
            presence,
        } = self;
        push_arm.abort();
        // Deterministic teardown, never a drop: `shutdown` drains the pump's
        // in-flight pass before the store closes, which is what keeps a
        // mid-pass exit from leaving the outbox half-drained.
        handle.shutdown().await;
        // Presence goes only once the runtime — and with it `engine.lock` — is
        // gone: an app that read the agent absent any earlier would contend
        // against a holder that is still pumping.
        drop(presence);

        // The stint's own nest leg goes with it. This client is minted per stint and
        // owned by nothing else, so if the stint ends without closing it its
        // reconnect supervisor simply keeps running — a fresh one joins it on the
        // next mount, and the agent accumulates a live supervisor (and its
        // connection) per stint for as long as the process lives. Deterministic here
        // for the same reason `handle.shutdown()` is: the retry task is aborted
        // first, so it cannot hand the supervisor a fresh connection while we are
        // closing it.
        if let Some(device) = device {
            device.retry.abort();
            device.client.disconnect().await;
        }
    }
}

/// Wake a live stint to re-read the capability now — called wherever the slot
/// it keys its mount on changes.
pub(crate) fn recheck_now(state: &SyncServiceState) {
    state.host_wake.notify_one();
}

/// How long an un-provision waits for the mount to come down before it replies
/// anyway. Under the apps' pipe request ceiling
/// (`fauna_ipc::sync_pipe_client::REQUEST_TIMEOUT`, 6 s), so a slow teardown is
/// answered by this process's own refusal rather than cut off client-side — the
/// same shape the nest-backed verbs' 4 s ceiling takes. A bound, not a
/// correctness clock: the ordinary teardown finishes long before it, and an
/// elapse is reported, never taken for success.
pub(crate) const UNMOUNT_BUDGET: Duration = Duration::from_secs(4);

/// Wait until no stint of this process holds the account store — the
/// un-provision's receipt. `false` when the budget elapsed with a mount still
/// up.
pub(crate) async fn unmounted_within(state: &SyncServiceState, budget: Duration) -> bool {
    let mut hosted = state.hosted_account.subscribe();
    matches!(
        tokio::time::timeout(budget, hosted.wait_for(Option::is_none)).await,
        Ok(Ok(_))
    )
}

/// One mounted stint's disposables.
///
/// Split out from a bare [`AccountStoreHandle`] because the runtime is not the
/// only thing a mount starts: the device-principal client below is minted per
/// stint, is owned by nothing else, and holds a reconnect supervisor that
/// outlives the stint unless the stint closes it.
struct Mounted {
    handle: AccountStoreHandle,
    /// `None` when the writer key would not resolve — the runtime then rides the
    /// capability bearer and this stint minted no client of its own.
    device: Option<DeviceLeg>,
    /// The `ws-device` push arm posting this account's banners
    /// ([`crate::push_arm`]); aborted with the stint.
    push_arm: tokio::task::JoinHandle<()>,
    /// The agent presence lock on this account's store dir, taken before the
    /// runtime started and dropped after it shut down — what makes an app beside
    /// this stint hand it the engine role (module docs). `None` after a degraded
    /// acquire: the stint mounts anyway.
    presence: Option<AgentPresenceLock>,
}

/// The stint's own nest leg: the device-principal client and the task bringing
/// its first connection up.
struct DeviceLeg {
    client: Arc<fauna_client::NestClient>,
    retry: tokio::task::JoinHandle<()>,
}

/// Build the params and start the runtime.
///
/// The seed-shaped fields are the interesting ones, and every one of them is a
/// deliberate *absence*:
///
/// * `principal: Seedless` — the fork W5.5a built. Grant mint, fleet bootstrap
///   and the succession re-key are skipped rather than faked; each
///   belongs to a signed-in app and heals on its next assembly.
/// * `memberships: None` — the member half of the content-scope set comes from a
///   live MLS engine, and this process deliberately links none (§ Credential
///   model: bearer-only). So this replica walks its own-actor scopes only. That
///   is a real limitation, not an oversight: an app is what walks joined
///   channels, and `None` means "cannot tell", which the runtime holds the last
///   derived set through rather than reading as *left every channel*.
async fn assemble(state: &SyncServiceState, inputs: &HostInputs) -> anyhow::Result<Mounted> {
    let rpc = state
        .nest_rpc_client()
        .await
        .map_err(|e| anyhow::anyhow!("account host: nest client: {e}"))?;

    let store_root = StoreRoot::platform();
    let credentials = production_credential_store();

    // The data path authenticates as the machine's STORE PRINCIPAL, not as the
    // agent's capability bearer — T11's "one principal per machine, not one per
    // process", which is what keeps the devices list at one row per machine.
    // Best-effort exactly as tui's is: a failure here leaves the runtime on the
    // capability-bearer client (`rpc`), which is still correct, just less
    // honest in the sessions list.
    //
    // **Load-only** (`principal_bundle::load_writer_key`), never
    // `resolve_writer_key_serialized`'s mint-or-load: this agent is seedless
    // (`sync-agent.md` § Credential model, the RULED 2026-08-15 block) and a
    // consumer that merely wants to authenticate as the principal must never
    // mint one — an absent slot means no signed-in app has enrolled this
    // machine yet, and minting here would create a writer identity no nest has
    // a grant for (`principal_bundle::load_writer_key`'s own doc).
    let device = match device_leg_key(&credentials, inputs) {
        Some(writer_key) => {
            let device_client =
                fauna_client::ws_device_handshake_bearer::device_principal_nest_client(
                    &inputs.nest_url,
                    inputs.actor_id,
                    writer_key,
                    // Seedless: it cannot register itself, so the void latch
                    // waits for a signed-in app's owner-session pass — the
                    // seed pass of the app beside this agent. And the leg
                    // signs with the key the renewal loop renews with, so
                    // its refusal is the renewal's own answer heard early:
                    // report it, and the loop asks the nest at once instead
                    // of at the slot bearer's lead — the one door into the
                    // terminal state (`sync-agent-credentials.md`
                    // § Credential model → *A rejected credential asks at
                    // once*).
                    Some({
                        let void_latch =
                            fauna_sync_engine::principal_bundle::not_registered_voids_latch(
                                &inputs.actor_id_hex,
                            );
                        let slot = Arc::clone(&state.capability);
                        Arc::new(move || {
                            void_latch();
                            slot.report_principal_refused();
                        })
                    }),
                );
            // Kept, not discarded: the stint closes this leg on teardown,
            // and the retry task has to be stopped before the client is
            // disconnected or it can bring a fresh connection up behind us.
            // Ungated: the agent hosts only a principal a signed-in app has
            // already enrolled (`transport-connection.md` § The dial budget).
            let retry = fauna_client::ws_device_handshake_bearer::spawn_connect_retry(
                Arc::clone(&device_client),
                fauna_client::ws_device_handshake_bearer::ConnectGate::Open,
            );
            Some(DeviceLeg {
                client: device_client,
                retry,
            })
        }
        None => {
            // The common, expected state on a machine no app has enrolled
            // yet, and the state of one the nest refused — `debug`, not
            // `warn`: the runtime rides the capability bearer until then.
            tracing::debug!(
                "account host: no device-principal leg (no writer key in the slot \
                 yet, or its renewal stands refused) — the runtime rides the \
                 capability bearer"
            );
            None
        }
    };

    // Presence before the runtime's election, for the stint's lifetime (module
    // docs). Blocking, and bounded by construction: the only other takers are
    // the apps' momentary probes. A failure states no priority and mounts
    // anyway.
    let presence = take_presence(&store_root, &inputs.actor_id_hex);

    let reconnects = rpc.subscribe_reconnects();
    // The runtime *borrows* the stint's device client; the stint keeps the
    // owning handle so teardown can close it.
    let process_rpc = device.as_ref().map(|d| Arc::clone(&d.client));
    let params = runtime_params(
        store_root,
        crate::custodian::cloud_backup_exclusion()?,
        credentials,
        inputs,
        Arc::clone(&rpc),
        process_rpc,
        Some(reconnects),
        Some(rpc.subscribe_pushes()),
        Some(state.peer_files.binding()),
    );

    let handle = AccountStoreRuntime::start(params).await?;

    // The desktop's push wake stand-in (`common.md` § Push Notifications →
    // *Transports*): this account connection announces the machine's device
    // id — the capability's `device_id`, the same id the app subscribes its
    // `ws-device` row under — and the arm posts the frames that arrive for it
    // while no app is attached. Announced on every reconnect by the client.
    if !inputs.sync_device_id.is_empty() {
        rpc.set_push_presence(inputs.sync_device_id.clone()).await;
    }
    let push_arm = tokio::spawn(crate::push_arm::run(
        rpc.subscribe_pushes(),
        Arc::clone(&state.attached_apps),
        Arc::clone(&state.notification_sink),
    ));
    Ok(Mounted {
        handle,
        device,
        push_arm,
        presence,
    })
}

/// Take the agent presence lock on `actor_id_hex`'s store dir under
/// `store_root` — `None`, logged, when it cannot be taken (the stint mounts
/// anyway; the apps then see the first-come election).
fn take_presence(store_root: &StoreRoot, actor_id_hex: &str) -> Option<AgentPresenceLock> {
    let store_dir = match store_root.store_dir(actor_id_hex) {
        Ok(dir) => dir,
        Err(e) => {
            tracing::warn!(
                "account host: no store dir for the presence lock ({e:#}) — mounting without \
                 it; an app beside this agent keeps the first-come election"
            );
            return None;
        }
    };
    match AgentPresenceLock::acquire(&store_dir) {
        AgentPresenceLockOutcome::Held(lock) => Some(lock),
        AgentPresenceLockOutcome::Degraded(e) => {
            tracing::warn!(
                "account host: agent presence lock unavailable ({e}) — mounting without it; \
                 an app beside this agent keeps the first-come election"
            );
            None
        }
    }
}

/// The key the stint's device-principal leg signs with — or `None` when the
/// stint hosts no leg: the nest refused this principal
/// ([`HostInputs::device_leg`]), or no app has enrolled the machine yet.
fn device_leg_key(
    credentials: &fauna_credential_store::CredentialStore,
    inputs: &HostInputs,
) -> Option<ed25519_dalek::SigningKey> {
    if !inputs.device_leg {
        return None;
    }
    load_writer_key(credentials, &inputs.actor_id_hex)
}

/// Assemble the params — split out from [`assemble`] so the seed-shaped
/// choices are assertable without a live nest connection. Generic over the
/// requester for the same reason.
#[allow(clippy::too_many_arguments)] // builds an AccountRuntimeParams from its fields; a struct would just relocate them
fn runtime_params<R>(
    store_root: StoreRoot,
    store_backup_exclusion: fauna_sync_engine::account_runtime::CloudBackupExclusion,
    credentials: fauna_credential_store::CredentialStore,
    inputs: &HostInputs,
    rpc: R,
    process_rpc: Option<R>,
    reconnects: Option<watch::Receiver<u64>>,
    pushes: Option<tokio::sync::broadcast::Receiver<fauna_protocol::PushEvent>>,
    file_sync: Option<fauna_sync_engine::account_runtime::PeerFileSync>,
) -> AccountRuntimeParams<R> {
    AccountRuntimeParams {
        store_root,
        // The desktop posture this binary already states for its custodian
        // store — one statement, two stores (`custodian::cloud_backup_exclusion`).
        store_backup_exclusion,
        actor_id_hex: inputs.actor_id_hex.clone(),
        rpc,
        process_rpc,
        principal: RuntimePrincipal::Seedless,
        credentials,
        reconnects,
        pushes,
        backstop_interval: DEFAULT_BACKSTOP_INTERVAL,
        memberships: None,
        // The R14 escrow-holder trust: what this machine PINNED for its nest,
        // never what the nest says about itself. The agent reads the same TOFU
        // store the apps do (`crate::trust::install_consumer_pin_store` ran at
        // startup, so the pin an app minted is visible here). No pin yet is
        // fail-safe: no receipt verifies, so no tip resolves and fleet-only
        // sealing stays refused with the precise no-tip error.
        //
        // Read through the ONE shared door rather than open-coding
        // `pinned_identity(authority_of(..))` — identical in a release build,
        // but the door is also where the e2e's test-build trust seed lives
        // (`FAUNA_E2E_TRUST_NEST_IDENTITY`). This agent is the account-runtime
        // host for every app EXCEPT tui (the only other assembly site), so
        // before this the seed reached tui alone and the R14 plane stayed
        // dormant in the other six apps' e2e — the reason the door's contract
        // now says every consumer reads it here.
        // Re-read at every pass: a rotation the app accepts re-pins the nest.
        trusted_escrow_holders: {
            let nest_url = inputs.nest_url.clone();
            std::sync::Arc::new(move || fauna_client::trust::trusted_escrow_holders(&nest_url))
        },
        // The `prior` half of the same trust: what the identity-holding app
        // ATTESTED over the capability pipe (`HostInputs::attested_predecessors`
        // owns why this process cannot attest a predecessor itself, and why
        // the account-plane replica it mirrors is not read for it).
        attested_predecessors: inputs.attested_predecessors.clone(),
        // The peer-leg transport factory: the agent is the machine's
        // steady-state engine-singleton, so this is what makes the listener
        // AND the dial pass run app-dead. Same gates as everywhere — the
        // runtime invokes it only elected + enrolled + brake-off, with its own
        // resolved writer key (R5 — the endpoint IS the store principal).
        // The seedless agent runs no secondary leg
        // (`account-sync-plane.md` § The bind leg, ruling 4): the signed-in
        // app beside it runs it in its seed pass, re-issuing the retires this
        // host sends from the store's retire record (ruling 5).
        linked_nests: None,
        // Nor the road: a succession is delivered by a seed holder, which can
        // sign in as a retired identity where a chain must be replayed.
        owed_nests: None,
        //
        // The binding also carries this host's file-sync engines onto the leg
        // (`crate::peer_files`): the agent holds the bodies, so it is the
        // process that serves a sibling's chunk want and asks a sibling first.
        peer_transport: Some(std::sync::Arc::new(
            move |inputs: fauna_sync_engine::account_runtime::PeerLegFactoryInputs| {
                let file_sync = file_sync.clone();
                Box::pin(async move {
                    let (transport, bound_addrs) = fauna_iroh::peer_leg_transport(
                        inputs.writer_key.to_bytes(),
                        inputs.relay_url.as_deref(),
                    )
                    .await?;
                    Ok(fauna_sync_engine::account_runtime::PeerLegBinding {
                        transport,
                        bound_addrs,
                        file_sync,
                    })
                })
            },
        )),
        // The machine's named row (`HostInputs::sync_device_id`). An empty
        // one — a capability with no device id, a host wiring bug — fails the
        // enrollment pass loudly rather than naming a row no one can address.
        enrollment_target_device_id: inputs.sync_device_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_inputs() -> HostInputs {
        let actor_id = [7u8; 32];
        HostInputs {
            actor_id,
            actor_id_hex: hex::encode(actor_id),
            nest_url: "https://nest.example".to_string(),
            sync_device_id: TEST_DEVICE_ID.to_string(),
            attested_predecessors: attested_predecessors(vec![[0x77u8; 32]], vec![[0x55u8; 32]]),
            device_leg: true,
        }
    }

    /// **The leg follows the refusal, not the slot**
    /// (`sync-agent-credentials.md` § Credential model → *A rejected
    /// credential asks at once*). A refused slot mounts nothing; once an app
    /// re-arms the bearer the mount comes back on the capability bearer — and
    /// hosts no device-principal leg while the refusal record stands, though
    /// the refused principal's key still sits in the slot. Before this the
    /// host keyed the leg on the bearer, so an app's `RefreshBearer` brought
    /// the leg back to redial the refused principal at its 30 s ceiling.
    #[tokio::test]
    async fn a_re_armed_slot_under_a_standing_refusal_mounts_no_device_leg() {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
        );
        let actor = [7u8; 32];
        *state.capability.write().await = Some(fauna_ipc::sync::SyncCapability::new(
            vec![0xABu8; 32],
            actor.to_vec(),
            "https://nest.example".into(),
            TEST_DEVICE_ID.into(),
            fauna_ipc::sync::BearerToken::new("tok".into(), u64::MAX),
        ));
        // The principal's key is in the slot throughout (a tempdir slot,
        // never the OS keyring).
        let dir = tempfile::tempdir().expect("tempdir");
        let slot = fauna_credential_store::CredentialStore::with_file_backend(
            fauna_sync_engine::account_runtime::CRED_NAMESPACE,
            dir.path().to_path_buf(),
        );
        {
            use fauna_client_accounts::SecretStore;
            slot.set(&hex::encode(actor), &hex::encode([0xc3u8; 32]));
        }

        let enrolled = host_inputs(&state).await.expect("a capability mounts");
        assert!(enrolled.device_leg);
        assert!(device_leg_key(&slot, &enrolled).is_some());

        // The nest refuses the renewal: the bearer is dropped, nothing mounts.
        *state.renewal_refusal.write().unwrap() = Some(crate::credentials::RefusalRecord {
            nest_url: "https://nest.example".into(),
            principal_key: hex::encode([0xc3u8; 32]),
            grant_registered: true,
        });
        crate::bearer::mark_renewal_refused(state.capability.write().await.as_mut().unwrap());
        assert!(host_inputs(&state).await.is_none());

        // An app re-arms the bearer: the mount is back, the leg is not.
        state.capability.write().await.as_mut().unwrap().bearer =
            fauna_ipc::sync::BearerToken::new("app-tok".into(), u64::MAX);
        let re_armed = host_inputs(&state)
            .await
            .expect("a re-armed slot mounts on the capability bearer");
        assert!(!re_armed.device_leg);
        assert!(
            device_leg_key(&slot, &re_armed).is_none(),
            "the refused principal's leg must not be hosted, key present or not"
        );
        assert_ne!(
            re_armed, enrolled,
            "the leg is part of the mount's identity, so ending the refusal re-mounts"
        );

        // A renewal succeeds: the record goes, and the leg with it comes back.
        *state.renewal_refusal.write().unwrap() = None;
        assert_eq!(host_inputs(&state).await, Some(enrolled));
    }

    /// **The app's attested predecessor set reaches the runtime's R14 trust**
    /// (`account-data-taxonomy.md` § The generation machinery → *The source of
    /// `prior`*): the ids the capability carried are the params' `prior`
    /// half, verbatim. Dropping them would fail nothing loudly — a successor's
    /// predecessor-signed enrollments would simply stop verifying at the agent
    /// while the app beside it still verified them, and the two views would
    /// disagree forever with nothing on screen saying so. Mutation:
    /// `attested_predecessors: Vec::new()` in `runtime_params` → this reds.
    #[test]
    fn the_agent_hands_the_apps_attested_predecessors_to_the_r14_trust() {
        let params = params_under_test(StoreRoot::at("/shared/root"));
        assert_eq!(
            params.attested_predecessors.actor_ids(),
            [fauna_core::identity::ActorId([0x77u8; 32])],
            "the capability's predecessor_actor_ids ARE this host's prior — it holds no \
             registry to attest one itself, and must not read the replica for it"
        );
        // ...and the retired key beside them is the walk's carry schedule.
        // Mutation: `AttestedPredecessors::none()` in `host_inputs` → this reds.
        let schedules = params.attested_predecessors.delegable_schedules();
        assert_eq!(schedules.len(), 1);
        assert_eq!(
            **schedules[0].seal_root(),
            **fauna_core::crypto::DelegableSchedule::derive(
                &fauna_core::crypto::BackupKey::from_bytes([0x55u8; 32])
            )
            .seal_root(),
            "the predecessor's delegable schedule derives from the retired key the \
             capability already carries"
        );
    }

    /// The same, from the capability itself: `host_inputs`' decode of the
    /// flattened ids, in order, and empty for a capability that carries none.
    #[test]
    fn host_inputs_decode_the_capabilitys_attested_predecessor_ids() {
        let cap = fauna_ipc::sync::SyncCapability::new(
            vec![0xABu8; 32],
            vec![7u8; 32],
            "https://nest.example".into(),
            TEST_DEVICE_ID.into(),
            fauna_ipc::sync::BearerToken::new("tok".into(), 1),
        )
        .with_predecessor_actor_ids(&[[0x77u8; 32], [0x78u8; 32]])
        .with_predecessor_backup_keys(&[[0x55u8; 32], [0x56u8; 32]]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
        );
        rt.block_on(async {
            *state.capability.write().await = Some(cap);
            let inputs = host_inputs(&state).await.expect("a capability mounts");
            assert_eq!(
                inputs.attested_predecessors,
                attested_predecessors(
                    vec![[0x77u8; 32], [0x78u8; 32]],
                    vec![[0x55u8; 32], [0x56u8; 32]],
                ),
                "the ids in order, and one delegable schedule per retired key"
            );
        });
    }

    /// The machine row this agent's capability names — asserted below to reach
    /// the runtime params, because a host that dropped it would leave every
    /// app-dead enrollment with no row to register on.
    const TEST_DEVICE_ID: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    /// The params this host actually builds, over the real construction path.
    fn params_under_test(store_root: StoreRoot) -> AccountRuntimeParams<()> {
        runtime_params(
            store_root,
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
            production_credential_store(),
            &test_inputs(),
            (),
            None,
            None,
            None,
            None,
        )
    }

    /// The machine's row reaches the runtime, so an **app-dead** enrollment
    /// lands on the row the user recognises (`sync-agent-credentials.md`
    /// § Credential model → the RULED 2026-09-28 block, decision 3).
    #[test]
    fn the_agent_targets_the_machines_own_device_row() {
        let params = params_under_test(StoreRoot::at("/shared/root"));
        assert_eq!(
            params.enrollment_target_device_id, TEST_DEVICE_ID,
            "the capability's device_id IS the machine's named row"
        );
    }

    /// **The MLS carve-out, asserted at the agent** (W5.5b's unit pin).
    ///
    /// The carve-out is T9's: the engine singleton drains only intents whose
    /// seal needs store-held keys, and an MLS-sealed intent must drain from a
    /// process hosting the conversations engine — **never the bearer-only
    /// agent**, which holds no MLS state and so could only fail or forge. The
    /// *mechanism* is already proven where it lives, against a real runtime
    /// with a real fake requester (`fauna_sync_engine::account_runtime`'s
    /// `an_mls_intent_is_held_not_sent`: `held_for_mls == 1`, `drained == 0`,
    /// nothing sent). Re-asserting it here would only restate it.
    ///
    /// What is *this crate's* to prove is the precondition that makes the
    /// carve-out load-bearing rather than incidental: **this process assembles
    /// an MLS-free, seedless host.** Both fields below are the ones a future
    /// change would have to flip to make the agent capable of draining an MLS
    /// intent, so this is the assertion that goes red first.
    #[test]
    fn the_agent_assembles_an_mls_free_seedless_host() {
        let params = params_under_test(StoreRoot::at("/shared/root"));

        assert!(
            matches!(params.principal, RuntimePrincipal::Seedless),
            "the agent holds no identity seed (sync-agent.md § Credential \
             model, key-material-hierarchy rules #6/#7); a SeedHolding agent \
             would be the seed reaching a process that must never see it"
        );
        assert!(
            params.memberships.is_none(),
            "the member half of the content-scope set comes from a live MLS \
             engine, and this process links none — a Some(..) here would mean \
             the agent had grown one, which is exactly the state the T9 \
             carve-out assumes it can never be in"
        );
    }

    // ── The un-provision reply is the unmount receipt ──
    //
    // These drive the real `hold_stint` — the slot's publication, the wake and
    // the clear-after-teardown ordering — with a mount that needs no nest.

    /// A mount that needs no nest. Its teardown announces that it has begun and
    /// then waits on `gate` before it finishes — so a test can hold the teardown
    /// open and prove the reply waited for it, rather than hoping the teardown
    /// happened to win a race.
    struct FakeMount(Arc<Teardown>);

    #[derive(Default)]
    struct Teardown {
        started: tokio::sync::Notify,
        gate: tokio::sync::Notify,
        done: std::sync::atomic::AtomicBool,
    }

    impl Teardown {
        /// A teardown that finishes as soon as it runs.
        fn open() -> Arc<Self> {
            let t = Arc::new(Self::default());
            t.gate.notify_one();
            t
        }

        fn is_done(&self) -> bool {
            self.done.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl StintMount for FakeMount {
        async fn tear_down(self) {
            self.0.started.notify_one();
            self.0.gate.notified().await;
            self.0.done.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn provisioned_state() -> Arc<SyncServiceState> {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
        )
    }

    fn account_capability() -> fauna_ipc::sync::SyncCapability {
        fauna_ipc::sync::SyncCapability::new(
            vec![1u8; 32],
            vec![7u8; 32],
            "https://nest.example".into(),
            TEST_DEVICE_ID.into(),
            fauna_ipc::sync::BearerToken::new("tok".into(), 1),
        )
    }

    /// Run one stint over a [`FakeMount`]. `entered` fires when assembly starts,
    /// `assembled` once the mount exists; the mount comes up only once `proceed`
    /// fires, so a test can hold it mid-assembly.
    fn spawn_stint(
        state: &Arc<SyncServiceState>,
        teardown: &Arc<Teardown>,
        proceed: tokio::sync::oneshot::Receiver<()>,
    ) -> (
        tokio::task::JoinHandle<anyhow::Result<()>>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::watch::Sender<bool>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (assembled_tx, assembled_rx) = tokio::sync::oneshot::channel();
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let state = Arc::clone(state);
        let teardown = Arc::clone(teardown);
        let stint = tokio::spawn(async move {
            let inputs = host_inputs(&state).await.expect("a provisioned account");
            hold_stint(&state, &inputs, &mut shutdown_rx, || async move {
                let _ = entered_tx.send(());
                let _ = proceed.await;
                let _ = assembled_tx.send(());
                Ok(FakeMount(teardown))
            })
            .await
        });
        (stint, entered_rx, assembled_rx, shutdown_tx)
    }

    /// Un-provision on its own task, reporting whether the teardown had finished
    /// at the instant the reply existed.
    fn spawn_unprovision(
        state: &Arc<SyncServiceState>,
        teardown: &Arc<Teardown>,
    ) -> tokio::task::JoinHandle<(bool, bool)> {
        let state = Arc::clone(state);
        let teardown = Arc::clone(teardown);
        tokio::spawn(async move {
            let unmounted = crate::pipe_server::unprovision_now(&state).await;
            (unmounted, teardown.is_done())
        })
    }

    /// A hang guard only — every assertion below is on state. A stint that
    /// outlives the un-provision would otherwise hang the test instead of
    /// failing it.
    async fn stint_ends(stint: tokio::task::JoinHandle<anyhow::Result<()>>) {
        tokio::time::timeout(Duration::from_secs(60), stint)
            .await
            .expect("the stint outlived the un-provision")
            .expect("stint task")
            .expect("stint result");
    }

    /// ⚠ **The receipt.** The un-provision wakes the stint, and its reply comes
    /// only after the mount's teardown has FINISHED — held open here until the
    /// test lets it go. The app erases the store on that reply; before this the
    /// reply went out at once and the stint noticed only at its 10 s recheck.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_unprovision_reply_arrives_only_after_the_mount_is_down() {
        let state = provisioned_state();
        *state.capability.write().await = Some(account_capability());
        let teardown = Arc::new(Teardown::default());
        let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel();
        let (stint, _entered, assembled, _shutdown) = spawn_stint(&state, &teardown, proceed_rx);
        proceed_tx.send(()).unwrap();
        assembled.await.expect("the stint mounted");
        assert_eq!(*state.hosted_account.borrow(), Some([7u8; 32]));

        let unprovision = spawn_unprovision(&state, &teardown);
        // The wake reached the stint: its teardown has begun, and is held.
        teardown.started.notified().await;
        teardown.gate.notify_one();

        let (unmounted, done_at_reply) = unprovision.await.unwrap();
        assert!(unmounted, "the un-provision reports the mount down");
        assert!(
            done_at_reply,
            "the reply must follow the mount's teardown, not precede it"
        );
        assert_eq!(*state.hosted_account.borrow(), None);
        stint_ends(stint).await;
    }

    /// An un-provision that lands while the store is still being ASSEMBLED
    /// waits for the mount that assembly produces to come down too — the slot is
    /// published before assembly, and the wake is kept as a permit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unprovision_during_assembly_waits_for_that_mount_to_come_down() {
        let state = provisioned_state();
        *state.capability.write().await = Some(account_capability());
        let teardown = Arc::new(Teardown::default());
        let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel();
        let (stint, entered, _assembled, _shutdown) = spawn_stint(&state, &teardown, proceed_rx);
        entered.await.expect("the stint is parked in assembly");

        let unprovision = spawn_unprovision(&state, &teardown);
        // The capability is gone before the assembly finishes.
        while state.capability.read().await.is_some() {
            tokio::task::yield_now().await;
        }
        proceed_tx.send(()).unwrap();
        // The mount the assembly produced is being torn down, and is held.
        teardown.started.notified().await;
        teardown.gate.notify_one();

        let (unmounted, done_at_reply) = unprovision.await.unwrap();
        assert!(unmounted, "the receipt still arrives");
        assert!(
            done_at_reply,
            "the mount the in-flight assembly produced came down before the reply"
        );
        stint_ends(stint).await;
    }

    /// The second arm of the defect: un-provision → re-provision of the SAME
    /// account. Its inputs compare equal, so a stint that only re-read the
    /// capability on a tick survived both and kept running out of the store the
    /// app erased in between. Now the old stint has ended by the reply, and the
    /// re-provision mounts afresh.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_same_account_reprovision_gets_a_fresh_mount_not_the_old_stint() {
        let state = provisioned_state();
        *state.capability.write().await = Some(account_capability());
        let teardown = Teardown::open();
        let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel();
        let (stint, _entered, assembled, _shutdown) = spawn_stint(&state, &teardown, proceed_rx);
        proceed_tx.send(()).unwrap();
        assembled.await.unwrap();

        assert!(crate::pipe_server::unprovision_now(&state).await);
        *state.capability.write().await = Some(account_capability());
        stint_ends(stint).await;
        assert!(teardown.is_done());

        let fresh = Teardown::open();
        let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel();
        let (fresh_stint, _entered, fresh_assembled, shutdown) =
            spawn_stint(&state, &fresh, proceed_rx);
        proceed_tx.send(()).unwrap();
        fresh_assembled
            .await
            .expect("the re-provision assembles a fresh mount");
        shutdown.send(true).unwrap();
        stint_ends(fresh_stint).await;
        assert!(fresh.is_done());
    }
}
