//! Web's **host** of the account driver — the twin of
//! `fauna_sync_engine::account_runtime`'s native host
//! (`account-client-lifecycle.md` § The client-side lifecycle → *The trigger
//! fired*, ruling (4)'s build decision (f)).
//!
//! [`start`] mints the driver and its one [`AccountStoreHandle`], then runs
//! the assembly/serve cycle as a `spawn_local` task in the SPA's process — the
//! one-process posture `account-runtime.md` § Multi-instance concurrency gives
//! web's leg. One assembly, in the native worker's order:
//!
//! 1. the writer key, minted or loaded from the T10 slot
//!    ([`principal_bundle::mint_or_load_writer_key`]), and the account's
//!    `BackupKey` (derived from the seed, or read from the slot for a
//!    seedless caller);
//! 2. the rest of the bundle — the SAME [`PrincipalBundle`] every native
//!    machine keeps, over the caller's secret store (the SPA's
//!    `LocalStorageSecretStore`, ruling (4)'s decision (c)) and [`TabSection`];
//!    the predecessor carriage; the enrollment grant's mint when the slot has
//!    no current one;
//! 3. the succession probe, seed-holding only, over the app session
//!    ([`principal_succession::ceremony_probe`]) — a revoked writer rotates in
//!    place and assembly restarts;
//! 4. the backend — `IndexedDbBackend` under
//!    `StoreRoot::store_name(actor)` (IndexedDB's own `versionchange`
//!    transaction is the migration section, so nothing else serializes the
//!    open) — then the lost-slot heal
//!    ([`principal_succession::lost_slot_heal`]): a slot whose writer the
//!    store's stamp does not name fences onto the slot's key, or re-mints and
//!    restarts — browser storage is cleared and evicted per bucket, so
//!    localStorage can go while IndexedDB stays;
//! 5. the store open;
//! 6. the election over Web Locks ([`WebElection`] —
//!    `EngineLock::try_acquire(store_name)`, degrade-open when the API is
//!    missing: the driver's [`elect_at_start`] owns what that means; web has
//!    no sync agent, so its presence probe answers absent without asking);
//! 7. [`AccountDriver::serve`] with [`NoLegs`] — web has no iroh and hosts no
//!    custody — looping on [`ServeEnd`] as the native worker does.
//!
//! Steps 3 and 4 restart assembly exactly as the native worker's
//! `'assembly` loop does: the probe's one-rotation cap is the driver's
//! `rotated_since_serve`, the heal's one-re-mint cap is the worker's own.
//!
//! The data path is the app session (ruling (4)'s decision (e)): web's one
//! socket per actor carries the enrollment legs and the pass alike, so there
//! is no `process_rpc`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fauna_account_store::indexeddb::IndexedDbBackend;
use fauna_account_store::locks::{EngineLock, EngineLockOutcome, SeedLegLock, SeedLegLockOutcome};
use fauna_account_store::root::StoreRoot;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorId;
use fauna_protocol::{KeyedRpcRequester, PushEvent, RpcErrorClass};
use tokio::sync::{broadcast, oneshot, watch};

use crate::account_driver::{
    AccountDriver, AccountStoreHandle, Assembly, DriverConfig, ElectionOutcome, EngineElection,
    MembershipSource, NoLegs, Presence, RuntimePrincipal, ServeEnd, elect_at_start,
};
use crate::principal_bundle::{
    self, PrincipalBundle, SecretStore, SlotSection, WriterKeyProvenance,
};
use crate::principal_succession::{self, CeremonyProbe, LostSlotHeal};

/// Web's slot section: the tab's own (ruling (4)'s decision (c)). The medium
/// has no synchronous cross-tab lock — Web Locks is async and the slot seam
/// is synchronous by ruling (2) — and the one list written by a
/// read-modify-write, the staged fleet removals, is written by a human gesture
/// and completed by the one pump holder, so two tabs staging at once is a
/// human-speed race whose loser re-surfaces as an unstaged removal the user
/// redoes. The tab is single-threaded, so entering always succeeds.
#[derive(Debug, Default, Clone, Copy)]
pub struct TabSection;

impl SlotSection for TabSection {
    type Guard = ();

    fn enter(&self, _degraded: &str) -> Option<()> {
        Some(())
    }
}

/// Web's credential slot — the one bundle over the caller's secret store.
pub type WebSlot<S> = PrincipalBundle<S, TabSection>;

/// Web's engine-singleton election ([`EngineElection`]): the Web Locks
/// request `EngineLock::try_acquire` makes for the store's name, answered at
/// once (`ifAvailable`) — another tab, or another runtime in this one, holds
/// it or it does not. The seed-leg role is the same request under its own
/// name (`SeedLegLock`). There is no sync agent on web, so the presence
/// question is answered `Absent` without asking (`account-runtime.md`
/// § Multi-instance concurrency → *The agent holds the role when present*,
/// part 1) and the first-come election among tabs stands.
pub struct WebElection {
    store_name: String,
}

impl EngineElection for WebElection {
    type Held = EngineLock;

    async fn try_acquire(&self) -> ElectionOutcome<EngineLock> {
        match EngineLock::try_acquire(&self.store_name).await {
            EngineLockOutcome::Held(lock) => ElectionOutcome::Held(lock),
            EngineLockOutcome::Refused => ElectionOutcome::Refused,
            EngineLockOutcome::Degraded(e) => ElectionOutcome::Degraded(e.to_string()),
        }
    }

    type SeedLegs = SeedLegLock;

    async fn try_acquire_seed_legs(&self) -> ElectionOutcome<SeedLegLock> {
        match SeedLegLock::try_acquire(&self.store_name).await {
            SeedLegLockOutcome::Held(lock) => ElectionOutcome::Held(lock),
            SeedLegLockOutcome::Refused => ElectionOutcome::Refused,
            SeedLegLockOutcome::Degraded(e) => ElectionOutcome::Degraded(e.to_string()),
        }
    }

    async fn agent_present(&self) -> Presence {
        Presence::Absent
    }
}

/// What only the SPA knows (ruling (4)'s decision (f)); everything else the
/// host resolves itself.
pub struct WebRuntimeParams<R> {
    /// Where the account's stores live — [`StoreRoot::platform`] in the SPA;
    /// a test isolates its own root.
    pub store_root: StoreRoot,
    /// 64-hex actor id.
    pub actor_id_hex: String,
    /// The app session — the enrollment legs and the data path alike.
    pub rpc: R,
    /// The signed-in identity. Every web tab holds the seed, so this is
    /// [`RuntimePrincipal::SeedHolding`] in production.
    pub principal: RuntimePrincipal,
    /// The member half of the content-scope set — the MLS engine's joined
    /// conversation channels.
    pub memberships: Option<MembershipSource>,
    /// The account's attested succeeded-from identities, with the delegable
    /// schedule each one's rows are carried under.
    pub attested_predecessors: crate::attested_predecessors::AttestedPredecessors,
    /// The escrow holders this browser pinned — the serving origin's TOFU pin
    /// (ruling (4)'s decision (d)), re-read at every pass.
    pub trusted_escrow_holders: crate::account_driver::TrustedHolderSource,
    /// Web's own derived device id — the named row the enrollment registers
    /// the principal's grant on (decision (e)).
    pub enrollment_target_device_id: String,
    /// The backstop reconcile cadence
    /// ([`crate::account_driver::DEFAULT_BACKSTOP_INTERVAL`] in production).
    pub backstop_interval: Duration,
    /// The session's reconnect counter; `None` disarms the reconnect wake
    /// (the backstop stays the correctness path).
    pub reconnects: Option<watch::Receiver<u64>>,
    /// The session's push stream; `None` disarms the push→nudge arm.
    pub pushes: Option<broadcast::Receiver<PushEvent>>,
    /// The secondary leg's connector (`account-sync-plane.md` § The bind leg,
    /// ruling 4) — in the SPA a second `WsRpcClient` to each linked nest, its
    /// bearer minted on that origin's anonymous socket, and the identity the
    /// connection is bound to. `None` runs no secondary leg.
    pub linked_nests: Option<crate::account_driver::LinkedNestConnector<R>>,
    /// The road's deliverer (`crate::owed_delivery`) — in the SPA an
    /// anonymous socket to each owed nest and a bearer-minted sign-in as each
    /// retired identity whose seed the tab holds. `None` delivers nothing.
    pub owed_nests: Option<crate::account_driver::OwedNestDeliverer<R>>,
}

/// Start the account runtime for one account in this tab. Returns once the
/// first assembly has signalled ready (the store is open, the writer resolved,
/// the fleet bootstrap written) — never on a network pass, so an offline tab
/// comes up. The caller keeps the handle for the account's session and calls
/// [`AccountStoreHandle::shutdown`] before another account signs in.
pub async fn start<S, R>(
    params: WebRuntimeParams<R>,
    credentials: Arc<S>,
) -> Result<AccountStoreHandle>
where
    S: SecretStore + 'static,
    R: KeyedRpcRequester + Clone + 'static,
    R::Error: RpcErrorClass,
{
    fauna_core::hex32::decode(&params.actor_id_hex)
        .map_err(|e| anyhow::anyhow!("account runtime: actor id: {e}"))?;
    let (driver, handle) = AccountDriver::new(
        DriverConfig {
            actor_id_hex: params.actor_id_hex.clone(),
            backstop_interval: params.backstop_interval,
            memberships: params.memberships.clone(),
            trusted_escrow_holders: params.trusted_escrow_holders.clone(),
            attested_predecessors: params.attested_predecessors.clone(),
            enrollment_target_device_id: params.enrollment_target_device_id.clone(),
        },
        params.reconnects.clone(),
        params.pushes.as_ref().map(broadcast::Receiver::resubscribe),
    );
    let (ready_tx, ready_rx) = oneshot::channel();
    wasm_bindgen_futures::spawn_local(worker(params, credentials, driver, ready_tx));
    ready_rx
        .await
        .map_err(|_| anyhow::anyhow!("account runtime: the store task ended during assembly"))??;
    Ok(handle)
}

/// One assembly's resolved parts.
struct Assembled<S: SecretStore + 'static> {
    store: AccountStore<IndexedDbBackend>,
    writer_key: ed25519_dalek::SigningKey,
    backup_key: BackupKey,
    slot: WebSlot<S>,
    election: WebElection,
}

/// The writer key, where it came from, the backup key and the bundle.
type ResolvedSlot<S> = (
    ed25519_dalek::SigningKey,
    WriterKeyProvenance,
    BackupKey,
    WebSlot<S>,
);

/// Resolve the writer key, the backup key and the bundle — the native
/// assembly's section body, in its order.
fn resolve_slot<S: SecretStore + 'static>(
    credentials: &Arc<S>,
    actor_id_hex: &str,
    principal: &RuntimePrincipal,
    attested_predecessors: &[ActorId],
) -> Result<ResolvedSlot<S>> {
    let (writer_key, provenance) = match principal.keypair() {
        Some(_) => {
            let (key, provenance) =
                principal_bundle::mint_or_load_writer_key(&**credentials, actor_id_hex)?;
            if provenance == WriterKeyProvenance::Loaded {
                tracing::debug!("account runtime: loaded this browser's store writer key");
            }
            (key, provenance)
        }
        None => (
            principal_bundle::load_writer_key(&**credentials, actor_id_hex).context(
                "account runtime: seedless assembly found no writer key in the credential slot",
            )?,
            WriterKeyProvenance::Loaded,
        ),
    };
    let backup_key = match principal.keypair() {
        Some(kp) => BackupKey::derive(kp.secret_bytes()),
        None => principal_bundle::load_backup_key(&**credentials, actor_id_hex).context(
            "account runtime: seedless assembly found no backup key in the credential slot",
        )?,
    };
    let writer_pub = writer_key.verifying_key().to_bytes();
    let slot = WebSlot::resolve(
        Arc::clone(credentials),
        actor_id_hex.to_owned(),
        TabSection,
        &writer_pub,
        principal.keypair().map(|_| &backup_key),
    );
    if !attested_predecessors.is_empty() {
        let carried = slot.carry_predecessor_generation_keys(attested_predecessors);
        if carried > 0 {
            tracing::info!(
                carried,
                "account runtime: carried predecessor-held generation keys into the \
                 successor's slot"
            );
        }
    }
    // The enrollment ceremony's mint half, where the seed is in hand;
    // the nest legs are the pump's retryable step. Best-effort by the slot's
    // own contract: a failed mint reads as "not enrolled", healed at the next
    // assembly.
    let grant_is_current = slot
        .device_authorization()
        .is_some_and(|l| fauna_client_sync::principal_grant_is_current(&l.authorization));
    if let (false, Some(keypair)) = (grant_is_current, principal.keypair()) {
        match fauna_client_sync::build_principal_grant(keypair, &writer_pub) {
            Ok(wire) => {
                if let Err(e) = slot.store_device_authorization(wire, &writer_pub) {
                    tracing::warn!("enrollment ceremony: grant not persisted: {e:#}");
                }
            }
            Err(e) => tracing::warn!("enrollment ceremony: grant mint failed: {e}"),
        }
    }
    Ok((writer_key, provenance, backup_key, slot))
}

/// What one assembly needs beyond the slot — the native worker's locals.
struct AssemblyInputs<'a, R> {
    store_root: &'a StoreRoot,
    actor_id_hex: &'a str,
    rpc: &'a R,
    principal: &'a RuntimePrincipal,
    attested_predecessors: &'a [ActorId],
    enrollment_target_device_id: &'a str,
    /// The probe's rotation license — `!driver.rotated_since_serve`.
    allow_rotation: bool,
    /// The third trigger's evidence — `driver.own_row_removed()`: the writer
    /// whose own device-set row the pump read `Removed`.
    own_row_removed: Option<[u8; 32]>,
    /// The heal's re-mint license — the worker's one-per-run cap.
    allow_remint: bool,
}

/// Why an assembly handed back no parts: the slot moved, and the worker
/// restarts assembly to resolve it afresh.
enum Restart {
    /// The probe rotated, or the heal found the slot moved under a sibling —
    /// nothing counted against the re-mint cap.
    Plain,
    /// The heal minted a fresh writer into the slot — counted against the cap.
    Reminted,
}

/// One assembly, in the native worker's order. `Err(Restart)` is the native
/// `Ok(None)`: the slot moved, and the worker loops.
async fn assemble<S, R>(
    inputs: &AssemblyInputs<'_, R>,
    credentials: &Arc<S>,
) -> Result<std::result::Result<Assembled<S>, Restart>>
where
    S: SecretStore + 'static,
    R: KeyedRpcRequester + Clone + 'static,
    R::Error: RpcErrorClass,
{
    let actor_id_hex = inputs.actor_id_hex;
    let store_name = inputs
        .store_root
        .store_name(actor_id_hex)
        .context("account runtime: store name")?;
    let (writer_key, provenance, backup_key, slot) = resolve_slot(
        credentials,
        actor_id_hex,
        inputs.principal,
        inputs.attested_predecessors,
    )?;
    // The succession probe — seed-holding assemblies
    // only, outside any section (it is an RPC) and before the store opens (a
    // rotation restarts assembly with nothing to tear down). It rides the app
    // session, exactly like the pump's registration legs.
    if let Some(kp) = inputs.principal.keypair() {
        let probe = principal_succession::ceremony_probe(
            inputs.rpc,
            credentials,
            actor_id_hex,
            &TabSection,
            || IndexedDbBackend::open(&store_name),
            kp,
            &writer_key,
            &slot,
            inputs.enrollment_target_device_id,
            inputs.allow_rotation,
            inputs.own_row_removed,
        )
        .await
        .context("account runtime: succession probe")?;
        if let CeremonyProbe::Rotated = probe {
            return Ok(Err(Restart::Plain));
        }
    }
    let backend = IndexedDbBackend::open(&store_name)
        .await
        .context("account runtime: open backend")?;
    // The lost-slot arm (refinement 10) and the journal-bound writer's two
    // arms (refinement 11) — between the backend open and the store open,
    // whose identity check would otherwise refuse a store the slot no longer
    // matches, for good. `Fenced` continues as the held key: it IS the
    // successor now.
    match principal_succession::lost_slot_heal(
        &**credentials,
        actor_id_hex,
        &TabSection,
        &backend,
        &writer_key,
        provenance,
        inputs.allow_remint,
    )
    .await
    .context("account runtime: lost-slot heal")?
    {
        LostSlotHeal::Consistent | LostSlotHeal::Fenced => {}
        // A sibling moved the slot under us: nothing was minted, so the
        // re-mint cap is NOT burned.
        LostSlotHeal::RestartAssembly => return Ok(Err(Restart::Plain)),
        LostSlotHeal::RemintedIntoSlot => return Ok(Err(Restart::Reminted)),
    }
    let writer = WriterId(writer_key.verifying_key().to_bytes());
    let store = AccountStore::open(backend, actor_id_hex, writer)
        .await
        .context("account runtime: open store")?;
    // The store is stamped with the slot's key: the mint marker is spent.
    principal_bundle::clear_writer_unstamped(&**credentials, actor_id_hex);
    Ok(Ok(Assembled {
        store,
        writer_key,
        backup_key,
        slot,
        election: WebElection { store_name },
    }))
}

/// The store task's body: assemble, hand the driver one assembly at a time,
/// until it says the runtime is over.
async fn worker<S, R>(
    params: WebRuntimeParams<R>,
    credentials: Arc<S>,
    mut driver: AccountDriver,
    ready_tx: oneshot::Sender<Result<()>>,
) where
    S: SecretStore + 'static,
    R: KeyedRpcRequester + Clone + 'static,
    R::Error: RpcErrorClass,
{
    let WebRuntimeParams {
        store_root,
        actor_id_hex,
        rpc,
        principal,
        attested_predecessors,
        enrollment_target_device_id,
        linked_nests,
        owed_nests,
        ..
    } = params;
    let mut ready_tx = Some(ready_tx);
    let mut shutdown_reply: Option<oneshot::Sender<()>> = None;
    // The lost-slot arm's own cap (refinement 10, bound (a)): at most ONE
    // fresh mint per runtime worker. A slot that comes back needing another
    // is a secret store not retaining writes; a reload re-arms it.
    let mut lost_slot_reminted = false;
    'assembly: loop {
        let inputs = AssemblyInputs {
            store_root: &store_root,
            actor_id_hex: &actor_id_hex,
            rpc: &rpc,
            principal: &principal,
            attested_predecessors: attested_predecessors.actor_ids(),
            enrollment_target_device_id: &enrollment_target_device_id,
            allow_rotation: !driver.rotated_since_serve,
            own_row_removed: driver.own_row_removed(),
            allow_remint: !lost_slot_reminted,
        };
        let parts = match assemble(&inputs, &credentials).await {
            Ok(Ok(parts)) => parts,
            // The slot moved: restart so everything resolves it afresh. The
            // native worker sets the one-rotation cap on every restart, and
            // so does this one.
            Ok(Err(restart)) => {
                if let Restart::Reminted = restart {
                    lost_slot_reminted = true;
                }
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
        // Inside the readiness barrier, after the store proved openable: a
        // runtime that cannot open the store must not squat on the role.
        let role = elect_at_start(&parts.election, &principal).await;
        let end = driver
            .serve(
                Assembly {
                    store: &parts.store,
                    writer_key: &parts.writer_key,
                    backup_key: &parts.backup_key,
                    principal: &principal,
                    slot: &parts.slot,
                    data_rpc: &rpc,
                    session_rpc: &rpc,
                    linked_nests: linked_nests.as_ref(),
                    owed_nests: owed_nests.as_ref(),
                },
                &mut NoLegs,
                &parts.election,
                role,
                ready_tx.take(),
            )
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
    }
    tracing::info!("account runtime: store task exiting");
    // After the assembly's locals — the Web Lock above all — have dropped
    // (`ServeEnd::Shutdown` owns why the ordering is the contract).
    if let Some(reply) = shutdown_reply {
        let _ = reply.send(());
    }
}
