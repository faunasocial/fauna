//! The SPA's **account runtime** — this tab's host of the account driver
//! (`account-client-lifecycle.md` § The client-side lifecycle → *The trigger
//! fired*, ruling (4)'s build decisions (c)–(f)).
//!
//! `fauna_account_plane::web_host` does the hosting; this module supplies only
//! what the SPA alone knows (decision (f)) — the session's `WsRpcClient`, the
//! keypair, the MLS engine's joined channels, the attested predecessors, the
//! origin's TOFU pin and web's own derived device id — and, once the store is
//! ready, registers the conversations seams that rest on it through the one
//! shared join (`fauna_account_seams::conversation_seams::wire_parts`), beside
//! the room seams `with_conversations` already set.
//!
//! One runtime per tab at a time, held in the same shared host every native
//! seat holds its runtime in (`fauna_account_plane::account_driver::
//! AccountRuntimeHost` — its claim and generation guard): the SPA starts it
//! once its conversations manager exists, shuts it down before another
//! account signs in, and every read — the devices page's cap notice and
//! This-device row, the e2e state keys — goes through [`handle`]. A start
//! claims its install before its first await, so a stop the SPA issues after
//! it — an account switch landing mid-assembly — supersedes it, and the
//! assembly shuts its own runtime down rather than installing the departing
//! account's; a stop takes only what is installed or assembling when it
//! runs, never a later start's runtime. A start that fails leaves the tab
//! with no account store and says so in the log, the native degrade posture:
//! every store-backed surface then fails its gesture.

use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::Arc;

use fauna_account_plane::account_driver::{
    ACCOUNT_RUNTIME_STOP_BUDGET, AccountRuntimeHost, AccountStoreHandle, DEFAULT_BACKSTOP_INTERVAL,
    EnrollmentRetirement, InstallOutcome, MembershipSource, PumpCyclesView,
    REMOVED_ACCOUNT_RETIRE_BUDGET, RuntimePrincipal, SeedHolder, StopReason, stop_one,
};
use fauna_account_plane::web_host::{self, WebRuntimeParams};
use fauna_account_seams::spawner::LocalSpawner;
use fauna_account_store::root::StoreRoot;
use fauna_client_accounts::{LocalStorageSecretStore, RemovedAccountCredentials};
use fauna_client_core::nest_trust::{LocalStoragePinStore, NestIdentityPinStore};
use fauna_client_recovery::LinkedNestDial;
use fauna_core::identity::ActorKeypair;
use wasm_bindgen::prelude::*;

/// The two objects the conversations seams register on — the manager and its
/// FaunaMls backend (`WasmConversationsManager::account_seam_parts`).
pub(crate) type SeamParts = (
    Arc<fauna_conversations::ConversationsManager>,
    Arc<fauna_conversations::backends::fauna_mls::FaunaMlsBackend>,
);

/// What this tab carries beside a runtime's handle — the host's payload,
/// claimed with the install so a teardown landing mid-assembly drops it too.
struct Running {
    /// The lowercase-hex actor id the runtime was started for.
    actor_id_hex: String,
    /// The Nests page's both-ends peer connect the runtime was started with —
    /// also the owner connection the seed-alone request opens at each linked
    /// nest ([`linked_nest_dial`]). `None` only in tests.
    peer_connect: Option<fauna_client_pair::PeerConnect>,
    /// The store-change notice as the counted level
    /// [`account_store_changed_after`] answers from, set once the store is
    /// open; dropping it with the runtime ends its relay and settles every
    /// waiter. Empty only in tests.
    store_changes: Rc<OnceCell<fauna_account_seams::store_change::StoreChangeLevel>>,
}

thread_local! {
    /// This tab's runtime host. `thread_local` because the SPA is one thread
    /// and the payload is `!Send` on wasm32; leaked so `start` can hold it
    /// across the install's await — one per tab, for the tab's life.
    static HOST: &'static AccountRuntimeHost<Running> =
        Box::leak(Box::new(AccountRuntimeHost::new()));
}

fn host() -> &'static AccountRuntimeHost<Running> {
    HOST.with(|h| *h)
}

/// This tab's account runtime, when one is running.
pub(crate) fn handle() -> Option<AccountStoreHandle> {
    host().handle()
}

/// [`handle`] as the source a store-backed surface waits on — what the
/// preference faces hand the shared `preference_surfaces`. The SPA starts the
/// runtime only once its conversations manager exists, so a page opened
/// before that waits for it; a tab that hosts no runtime at all (a second tab
/// of the same account) waits out the bound and fails the gesture.
pub(crate) fn handle_source() -> fauna_account_plane::account_driver::SeatAccountStore {
    fauna_account_plane::account_driver::SeatAccountStore::new(Arc::new(handle))
}

/// This tab's account runtime **for `actor_id_hex`** — `None` both when no
/// runtime runs and when the running one serves another account. The account
/// port's read (`crate::account_port`): a port is minted for one account and
/// answered only by that account's runtime (`account-client-lifecycle.md`
/// § The client-side lifecycle → *The account port*, decision (e)), so a page
/// machine that outlived an account switch by a tick never lands a write in
/// the next account's store.
/// The folder-key custody (`fauna.state.folder-keys`) of the account
/// `actor_id_hex`, read and written through this tab's runtime — the core
/// chunk's `PlaneFolderKeys`, the adapter every native seat wires (the folders
/// and media chunks reach the same through the account port,
/// `fauna_client_folders::port`). No runtime serving that account: a read is
/// "custody unreadable", a write waits for the assembly, then refuses.
pub(crate) fn folder_key_store(
    actor_id_hex: String,
) -> std::sync::Arc<dyn fauna_client_folders::FolderKeyStore> {
    std::sync::Arc::new(fauna_account_seams::folder_keys::PlaneFolderKeys::new(
        move || handle_for(&actor_id_hex),
    ))
}

pub(crate) fn handle_for(actor_id_hex: &str) -> Option<AccountStoreHandle> {
    host()
        .with_installed(|handle, r| {
            r.actor_id_hex
                .eq_ignore_ascii_case(actor_id_hex)
                .then(|| handle.clone())
        })
        .flatten()
}

/// The period-key custody (`fauna.state.subscriptions`) of this tab's running
/// runtime — resolved at every call, so a surface built before the runtime
/// starts reads through once it does: a read meets "not running" (never an
/// empty custody) until then, a write waits for it
/// (`fauna_account_seams::period_keys`).
pub(crate) fn period_key_store() -> fauna_client_subscriptions::SharedPeriodKeyStore {
    std::sync::Arc::new(fauna_account_seams::period_keys::PlanePeriodKeys::new(
        handle,
    ))
}

/// The account's mail custody (`fauna.state.mail`) over this tab's runtime —
/// what every mail-keyed machine the core chunk builds reads and writes the
/// MSEK and the credentials through, waiting for the runtime when the tab has
/// not started it yet (the SPA starts it only once the conversations manager
/// exists).
pub(crate) fn mail_store() -> Arc<dyn fauna_client_config::MailStore> {
    Arc::new(fauna_account_plane::account_driver::AccountMailStore::new(
        Arc::new(handle),
    ))
}

/// The succession-ledger seam (`fauna_client_config::SuccessionLedgerStore`,
/// implemented on the handle) the two review surfaces read and write through,
/// or the rejection they meet before this tab's store is up.
pub(crate) fn ledger_store() -> Result<AccountStoreHandle, JsValue> {
    handle().ok_or_else(|| JsValue::from_str("the account store is not ready yet"))
}

/// The escrow holders this browser trusts for `nest_url` — the TOFU pin this
/// origin holds for it (ruling (4)'s decision (d)), read through the same
/// "what this machine pinned, never the nest's own claim" rule as native
/// `fauna_anon_client::trust::trusted_escrow_holders`. No pin (a plaintext dev
/// nest) is fail-safe: no receipt verifies, no tip resolves, and fleet-only
/// sealing stays refused with the no-tip error.
///
/// **`test-helpers` builds only:** the e2e trust seed joins the set, read from
/// localStorage under the native variable's name in the one shared grammar
/// (`fauna_client_core::nest_trust::seeded_nest_identity`) — the
/// `fauna-client-region` seed precedent, keyed on the feature, never the
/// profile (e2e convention 15).
pub(crate) fn trusted_escrow_holders(nest_url: &str) -> Vec<[u8; 32]> {
    #[cfg_attr(not(feature = "test-helpers"), allow(unused_mut))]
    let mut holders: Vec<[u8; 32]> = LocalStoragePinStore.get(nest_url).into_iter().collect();
    #[cfg(feature = "test-helpers")]
    if let Some(id) = e2e_seeded_identity(nest_url)
        && !holders.contains(&id)
    {
        holders.push(id);
    }
    holders
}

#[cfg(feature = "test-helpers")]
fn e2e_seeded_identity(nest_url: &str) -> Option<[u8; 32]> {
    use fauna_client_core::nest_trust::{E2E_TRUST_NEST_IDENTITY, seeded_nest_identity};
    let seed = web_sys::window()?
        .local_storage()
        .ok()
        .flatten()?
        .get_item(E2E_TRUST_NEST_IDENTITY)
        .ok()
        .flatten()?;
    seeded_nest_identity(&seed, nest_url)
}

/// Start this tab's account runtime for the signed-in account and register
/// the account-plane conversations seams on `manager` at the store-ready
/// edge.
///
/// The install is CLAIMED before the first await (the shared host's
/// `begin`), so the SPA's call order is the order the host sees: a stop
/// issued after this start — an account switch landing mid-assembly —
/// supersedes it, and the assembly then shuts its own runtime down instead of
/// installing the departing account's. A runtime still installed when this
/// one lands (an account switch whose teardown was missed) is shut down as
/// this one replaces it, so a tab never keeps two drivers. Rejects when the
/// store cannot assemble, or when a later stop or start superseded this one.
pub(crate) async fn start(
    client: fauna_rpc_wasm::WsRpcClient,
    (conversations, fauna_mls): SeamParts,
    self_secret: [u8; 32],
    nest_url: &str,
    peer_connect: fauna_client_pair::PeerConnect,
) -> Result<(), JsValue> {
    let keypair = ActorKeypair::from_secret(self_secret);
    let actor_id_hex = keypair.actor_id_hex();
    let store_changes = Rc::new(OnceCell::new());
    let claim = host().begin(Running {
        actor_id_hex: actor_id_hex.clone(),
        peer_connect: Some(peer_connect.clone()),
        store_changes: Rc::clone(&store_changes),
    });
    let registry = crate::succession::account_registry();
    let enrollment_target_device_id = registry
        .device_id_for_actor(&LocalStorageSecretStore, &actor_id_hex)
        .map_err(|e| JsValue::from_str(&format!("account runtime: device id: {e}")))?;
    let memberships: MembershipSource = {
        let fauna_mls = Arc::clone(&fauna_mls);
        Arc::new(move || Some(fauna_mls.conv_channels().into_iter().map(|c| c.0).collect()))
    };
    let ledger_client = client.clone();
    let custody_client = client.clone();
    // This identity with the seeds of the predecessors the same registry
    // attests — what the kept wrap's recovery opens under and the road's chain
    // replay signs in with (`SeedHolder`).
    let principal = SeedHolder::from_registry(keypair, &registry);
    let road_seeds = fauna_account_plane::owed_delivery::PredecessorSeeds::of(&principal);
    let params = WebRuntimeParams {
        store_root: StoreRoot::platform().map_err(|e| JsValue::from_str(&format!("{e:#}")))?,
        actor_id_hex: actor_id_hex.clone(),
        reconnects: client.subscribe_reconnects(),
        pushes: client.subscribe_pushes(),
        rpc: client,
        principal: RuntimePrincipal::SeedHolding(principal),
        memberships: Some(memberships),
        // The registry rows whose seeds this browser holds: their ids are the
        // fleet view's `prior`, their delegable schedules what the walk carries
        // a predecessor's rows under — one walk, one value.
        attested_predecessors:
            fauna_account_plane::attested_predecessors::AttestedPredecessors::from_registry(
                &registry,
                &actor_id_hex,
            ),
        trusted_escrow_holders: {
            let nest_url = nest_url.to_string();
            std::sync::Arc::new(move || trusted_escrow_holders(&nest_url))
        },
        enrollment_target_device_id,
        backstop_interval: DEFAULT_BACKSTOP_INTERVAL,
        linked_nests: Some(linked_nest_connector(peer_connect.clone())),
        // The road: the seed holder's predecessor seeds, which a chain replay
        // at an owed nest signs in with.
        owed_nests: Some(owed_nest_deliverer(road_seeds)),
    };
    let handle = web_host::start(params, Arc::new(LocalStorageSecretStore))
        .await
        .map_err(|e| {
            tracing::warn!("account runtime: not started — this tab has no account store: {e:#}");
            JsValue::from_str(&format!("account runtime: {e:#}"))
        })?;
    // The store-ready edge — never at login: the store resolves after it.
    fauna_account_seams::conversation_seams::wire_parts(
        &conversations,
        &fauna_mls,
        handle.clone(),
        &LocalSpawner,
    );
    // The store-change notice, seeded here: whatever this runtime's pump
    // changes from now on reaches the SPA's open pages. Set into the claimed
    // payload before the install, so the first reader after it finds it.
    let (level, relay) =
        fauna_account_seams::store_change::StoreChangeLevel::new(handle.clone()).await;
    wasm_bindgen_futures::spawn_local(relay);
    let _ = store_changes.set(level);
    drop(store_changes);
    if host().finish(claim, handle.clone()).await == InstallOutcome::Superseded {
        tracing::info!(
            "account runtime: superseded while assembling — a later stop or start landed first; \
             this runtime is shut down, not installed"
        );
        return Err(JsValue::from_str(
            "account runtime: superseded while assembling",
        ));
    }
    tracing::info!("account runtime: started for this tab");
    // The deployment-seed custody leg — one of its two edges;
    // `selfHealDeploymentSeedCustody` (the post-auth hook) is the other
    // (`crate::deployment_seed_custody`): runs now iff the hook already fired
    // for this account.
    crate::deployment_seed_custody::spawn_at_store_ready(custody_client, handle, &actor_id_hex);
    // The succession ledger's post-store-ready pass — one of its two edges;
    // `runSuccessionAftermath` is the other (`succession::spawn_ledger_pass`).
    crate::succession::spawn_ledger_pass(ledger_client);
    Ok(())
}

/// The secondary leg's web connector (`account-sync-plane.md` § The bind leg,
/// ruling 4): the Nests page's both-ends peer connect
/// (`crate::pairing::make_peer_connect` — a second `WsRpcClient` to the linked
/// nest under this same identity, its bearer minted on that origin's anonymous
/// socket), then the identity that connection is bound to — the origin's pin,
/// possession-verified at every login, else a possession proof over the
/// connection itself (`read_bound_identity`); never the nest's own claim. The
/// leg compares it with the pairing row's nest id before any account data
/// moves.
fn linked_nest_connector(
    connect: fauna_client_pair::PeerConnect,
) -> fauna_account_plane::account_driver::LinkedNestConnector<fauna_rpc_wasm::WsRpcClient> {
    use fauna_account_plane::linked_leg::{LinkedConnection, LinkedNestTarget};
    type Connecting = futures_util::future::LocalBoxFuture<
        'static,
        anyhow::Result<LinkedConnection<fauna_rpc_wasm::WsRpcClient>>,
    >;
    #[allow(clippy::arc_with_non_send_sync)] // wasm is single-threaded; the seam is `Arc` natively
    Arc::new(move |target: LinkedNestTarget| -> Connecting {
        let connect = connect.clone();
        Box::pin(async move {
            owner_connection(&connect, &target)
                .await
                .map_err(|e| anyhow::anyhow!(e))
        })
    })
}

/// The owner-authenticated connection to a linked nest and the identity it is
/// bound to — the one body behind the secondary leg's connector and the
/// seed-alone request's [`WasmLinkedNestDial`].
async fn owner_connection(
    connect: &fauna_client_pair::PeerConnect,
    target: &fauna_client_core::linked_nests::LinkedNestTarget,
) -> Result<fauna_client_core::linked_nests::LinkedConnection<fauna_rpc_wasm::WsRpcClient>, String>
{
    let client = connect(target.nest_url.clone())
        .await
        .map_err(|e| format!("linked nest {}: connect: {e}", target.nest_url))?;
    let pinned = LocalStoragePinStore.get(&target.nest_url);
    let bound_identity = fauna_client_core::nest_trust::read_bound_identity(&client, pinned)
        .await
        .map_err(|e| format!("linked nest {}: bound identity: {e}", target.nest_url))?;
    Ok(fauna_client_core::linked_nests::LinkedConnection {
        rpc: client,
        bound_identity,
    })
}

/// Web's [`LinkedNestDial`] for the seed-alone request and veto at every
/// linked nest (`identity-succession.md` § Enforcement on the home nest →
/// *Every nest the identity is linked to*, clause (c);
/// `fauna_client_recovery::linked_fanout`): the owner connection is the
/// running account runtime's peer connect ([`owner_connection`]), the veto's
/// an `AnonymousWsRpcClient`, each bound-identity-checked over this tab's pin
/// store. A tab with no runtime serving the account has no owner connection to
/// open, so its request reaches the bound nest alone and the leg's owed
/// re-send carries it on once a runtime runs.
pub(crate) struct WasmLinkedNestDial {
    peer_connect: Option<fauna_client_pair::PeerConnect>,
}

/// The dial for `actor_id_hex`'s gestures in this tab.
pub(crate) fn linked_nest_dial(actor_id_hex: &str) -> WasmLinkedNestDial {
    let peer_connect = host()
        .with_installed(|_, r| {
            r.actor_id_hex
                .eq_ignore_ascii_case(actor_id_hex)
                .then(|| r.peer_connect.clone())
                .flatten()
        })
        .flatten();
    WasmLinkedNestDial { peer_connect }
}

#[async_trait::async_trait(?Send)]
impl LinkedNestDial for WasmLinkedNestDial {
    type Owner = fauna_rpc_wasm::WsRpcClient;
    type Anonymous = fauna_rpc_wasm::AnonymousWsRpcClient;

    async fn owner(
        &self,
        target: &fauna_client_core::linked_nests::LinkedNestTarget,
    ) -> Result<fauna_client_core::linked_nests::LinkedConnection<Self::Owner>, String> {
        let connect = self
            .peer_connect
            .as_ref()
            .ok_or_else(|| "no account runtime runs in this tab".to_string())?;
        owner_connection(connect, target).await
    }

    async fn anonymous(
        &self,
        target: &fauna_client_core::linked_nests::LinkedNestTarget,
    ) -> Result<fauna_client_core::linked_nests::LinkedConnection<Self::Anonymous>, String> {
        let client = fauna_rpc_wasm::AnonymousWsRpcClient::connect(&target.nest_url)
            .map_err(|e| format!("linked nest {}: connect: {e}", target.nest_url))?;
        let pinned = LocalStoragePinStore.get(&target.nest_url);
        let bound_identity = fauna_client_core::nest_trust::read_bound_identity(&client, pinned)
            .await
            .map_err(|e| format!("linked nest {}: bound identity: {e}", target.nest_url))?;
        Ok(fauna_client_core::linked_nests::LinkedConnection {
            rpc: client,
            bound_identity,
        })
    }
}

/// The road's web reach (`fauna_client_core::succession_delivery`): an
/// anonymous socket to the owed nest's address, bound to the identity read
/// off it — the origin's pin, else a possession proof over the connection,
/// as for the secondary leg — and a token session signed in as a retired
/// identity whose seed this browser holds (`crate::succession::sign_in`).
struct WebOwedReach {
    seeds: fauna_account_plane::owed_delivery::PredecessorSeeds,
}

impl fauna_client_core::succession_delivery::OwedNestReach for WebOwedReach {
    type Anon = fauna_rpc_wasm::AnonymousWsRpcClient;
    type Signed = fauna_rpc_wasm::TokenWsRpcClient;

    async fn anonymous(
        &self,
        url: &str,
    ) -> Result<(fauna_rpc_wasm::AnonymousWsRpcClient, [u8; 32]), String> {
        let anon = fauna_rpc_wasm::AnonymousWsRpcClient::connect(url)
            .map_err(|e| format!("owed nest {url}: connect: {e}"))?;
        let bound = fauna_client_core::nest_trust::read_bound_identity(
            &anon,
            LocalStoragePinStore.get(url),
        )
        .await
        .map_err(|e| format!("owed nest {url}: bound identity: {e}"))?;
        Ok((anon, bound))
    }

    async fn sign_in_as(
        &self,
        url: &str,
        actor_id: &[u8; 32],
    ) -> fauna_client_core::succession_delivery::SignIn<fauna_rpc_wasm::TokenWsRpcClient> {
        match self.seeds.keypair_for(actor_id) {
            Some(keypair) => crate::succession::sign_in(url, &keypair).await,
            None => fauna_client_core::succession_delivery::SignIn::NoSeed,
        }
    }
}

/// The road's web deliverer (`identity-succession.md` § Enforcement on the
/// home nest → *Every nest the identity is linked to*, **The road**) — the
/// native `fauna_client_account_runtime::native_owed_nest_deliverer`'s twin:
/// the shared delivery body over [`WebOwedReach`], for whichever keeping nest
/// the pass hands it.
fn owed_nest_deliverer(
    seeds: fauna_account_plane::owed_delivery::PredecessorSeeds,
) -> fauna_account_plane::account_driver::OwedNestDeliverer<fauna_rpc_wasm::WsRpcClient> {
    let reach = std::rc::Rc::new(WebOwedReach { seeds });
    #[allow(clippy::arc_with_non_send_sync)] // wasm is single-threaded; the seam is `Arc` natively
    Arc::new(move |keeper: fauna_rpc_wasm::WsRpcClient| {
        let reach = std::rc::Rc::clone(&reach);
        Box::pin(async move {
            fauna_client_core::succession_delivery::deliver_owed_nests(&keeper, &*reach)
                .await
                .map_err(|e| e.to_string())
        })
    })
}

/// Stop this tab's runtime, if one runs, for `reason` — the hosts' one stop
/// (`fauna_account_plane::account_driver::stop_one`): a sign-out retires the
/// machine's enrollment before the store closes, anything else leaves it
/// enrolled (`account-client-lifecycle.md` § The client-side lifecycle →
/// *Ruling (4), the teardown rider*). Takes what is installed AND what is
/// still assembling when it runs (the shared host's `take`), so an assembly
/// in flight is waited out and stopped, never installed after it; returns
/// once the store task has dropped its store and its Web Lock. A tab with no
/// runtime retires nothing — the native `NothingToStop` — and says so on a
/// sign-out, where it means the enrollment stays until the user removes the
/// device.
pub(crate) async fn stop(reason: StopReason) {
    let teardown = host().take(reason);
    if teardown.is_empty() {
        if reason == StopReason::SignOut {
            tracing::info!(
                "[account-runtime] sign-out: no runtime in this tab — no enrollment retired"
            );
        }
        return;
    }
    if let Some(store) = teardown.settled {
        stop_one(&store, reason).await;
    }
    if let Some(pending) = teardown.pending
        && let Some(store) = pending.settled().await
    {
        stop_one(&store, reason).await;
    }
}

/// Shut this tab's runtime down, if one runs, leaving the machine enrolled —
/// every stop that is not a sign-out ([`stop`]).
pub(crate) async fn shutdown() {
    stop(StopReason::AccountSwitch).await;
}

/// A remove-account's nest half on web: retire this browser from the account
/// `removed` holds the slots of, as a sign-out does (`account-scoping.md`
/// § Erasure follows scope → *Removing an account retires this machine from
/// it, as a sign-out does*, decisions 2 and 3) — the native
/// `fauna_client_account_runtime::retire_removed_account`'s twin over the
/// `web_host` runtime.
///
/// The removed account has no runtime in this tab (the other-tab refusal ran
/// first, and the Settings page offers removal only on an account the tab does
/// not serve), so this signs in as it over an anonymous socket with the
/// snapshot's secret (`crate::succession::sign_in`, the road's sign-in),
/// assembles the ordinary runtime over the snapshot and the account's
/// still-present store, and stops it with `shutdown_for_sign_out` at once —
/// within [`REMOVED_ACCOUNT_RETIRE_BUDGET`]. The tab's own runtime, for the
/// account it serves, is never touched. The answer is logged here and never
/// branched on: the removal's erase follows regardless.
pub(crate) async fn retire_removed_account(
    removed: Arc<RemovedAccountCredentials>,
) -> EnrollmentRetirement {
    let actor = removed.actor_id().to_owned();
    let retirement = match StoreRoot::platform() {
        Ok(root) => {
            retire_removed_over(Arc::clone(&removed), &root, async {
                sign_in_as_removed(&removed).await
            })
            .await
        }
        Err(e) => EnrollmentRetirement::Deferred(format!("{e:#}")),
    };
    tracing::info!(
        actor = %actor,
        ?retirement,
        "[account-runtime] remove-account: the machine's enrollment retirement"
    );
    retirement
}

/// The sign-in as the removed account: an anonymous handshake with the
/// snapshot's secret, then a one-shot bearer session on the nest it was bound
/// to.
async fn sign_in_as_removed(
    removed: &RemovedAccountCredentials,
) -> Result<fauna_rpc_wasm::TokenWsRpcClient, String> {
    use fauna_client_core::succession_delivery::SignIn;
    let secret = removed
        .secret_hex()
        .ok_or("the removal took no identity secret to sign in with")?;
    let nest_url = removed
        .nest_url()
        .ok_or("the removed account was bound to no nest")?;
    let keypair = ActorKeypair::from_secret_hex(&secret)
        .map_err(|e| format!("the removed account's secret: {e}"))?;
    match crate::succession::sign_in(&nest_url, &keypair).await {
        SignIn::Connected(client) => Ok(client),
        SignIn::NoAccount => Err("the nest holds no account for the removed identity".into()),
        SignIn::Failed(e) => Err(format!("sign-in: {e}")),
        SignIn::NoSeed => Err("sign-in: no seed".into()),
    }
}

/// [`retire_removed_account`] over any session and store root — the seam a
/// test drives with a fake nest. `connect` is the sign-in; it and the assembly
/// share the retirement budget less the stop's own.
async fn retire_removed_over<R, C>(
    removed: Arc<RemovedAccountCredentials>,
    root: &StoreRoot,
    connect: C,
) -> EnrollmentRetirement
where
    R: fauna_protocol::KeyedRpcRequester + Clone + 'static,
    R::Error: fauna_protocol::RpcErrorClass,
    C: std::future::Future<Output = Result<R, String>>,
{
    if !removed.holds_device_authorization() {
        return EnrollmentRetirement::NotEnrolled;
    }
    let assembly_budget = REMOVED_ACCOUNT_RETIRE_BUDGET - ACCOUNT_RUNTIME_STOP_BUDGET;
    let handle = match within(assembly_budget, assemble_removed(&removed, root, connect)).await {
        Some(Ok(handle)) => handle,
        Some(Err(e)) => return EnrollmentRetirement::Deferred(e),
        None => {
            return EnrollmentRetirement::Deferred(format!(
                "the nest did not answer within {}s",
                assembly_budget.as_secs()
            ));
        }
    };
    let retirement = within(ACCOUNT_RUNTIME_STOP_BUDGET, handle.shutdown_for_sign_out())
        .await
        // Dropping the handle below ends the runtime's store task all the same.
        .unwrap_or_else(|| {
            EnrollmentRetirement::Deferred(format!(
                "the stop did not finish within {}s",
                ACCOUNT_RUNTIME_STOP_BUDGET.as_secs()
            ))
        });
    drop(handle);
    retirement
}

/// The sign-in and the runtime over the snapshot: every slot the assembly
/// reads or writes is the snapshot's, so nothing reaches the registry the
/// removal just left (decision 1).
async fn assemble_removed<R, C>(
    removed: &Arc<RemovedAccountCredentials>,
    root: &StoreRoot,
    connect: C,
) -> Result<AccountStoreHandle, String>
where
    R: fauna_protocol::KeyedRpcRequester + Clone + 'static,
    R::Error: fauna_protocol::RpcErrorClass,
    C: std::future::Future<Output = Result<R, String>>,
{
    let actor_id_hex = removed.actor_id().to_owned();
    let secret = removed
        .secret_hex()
        .ok_or("the removal took no identity secret to sign in with")?;
    let keypair = ActorKeypair::from_secret_hex(&secret)
        .map_err(|e| format!("the removed account's secret: {e}"))?;
    // The named row: the account's persisted id, carried in the snapshot, else
    // the one derived from this browser's install secret — read through a
    // registry over the snapshot, so a derived id is persisted into memory and
    // never back into the registry.
    let enrollment_target_device_id =
        fauna_client_accounts::AccountRegistry::new(Arc::clone(removed) as _)
            .device_id_for_actor(&LocalStorageSecretStore, &actor_id_hex)
            .map_err(|e| format!("device id: {e}"))?;
    let rpc = connect.await?;
    let nest_url = removed.nest_url().unwrap_or_default();
    let params = WebRuntimeParams {
        store_root: root.clone(),
        actor_id_hex,
        rpc,
        principal: RuntimePrincipal::SeedHolding(keypair.into()),
        // Nothing this runtime does reads them: it is stopped as soon as it is
        // ready, and its stop runs no pass to walk channels, carry
        // predecessors, bind a linked nest or deliver a road.
        memberships: None,
        attested_predecessors: Default::default(),
        trusted_escrow_holders: std::sync::Arc::new(move || trusted_escrow_holders(&nest_url)),
        enrollment_target_device_id,
        backstop_interval: DEFAULT_BACKSTOP_INTERVAL,
        reconnects: None,
        pushes: None,
        linked_nests: None,
        owed_nests: None,
    };
    web_host::start(params, Arc::clone(removed))
        .await
        .map_err(|e| format!("{e:#}"))
}

/// `fut`, or `None` once `budget` has passed — wasm has no `tokio::time`.
async fn within<T>(
    budget: std::time::Duration,
    fut: impl std::future::Future<Output = T>,
) -> Option<T> {
    use futures_util::future::{Either, select};
    let fut = std::pin::pin!(fut);
    let timer = std::pin::pin!(fauna_sleep::sleep(budget));
    match select(fut, timer).await {
        Either::Left((value, _)) => Some(value),
        Either::Right(_) => None,
    }
}

/// The succession-ledger seam every grant-minting machine this chunk builds
/// records through (the Nests page, the mail-settings machines): this tab's
/// handle, resolved per call and waited for while the runtime is still
/// starting (`fauna_client_config::ResolvingLedgerStore`).
pub(crate) fn ledger_seam() -> std::sync::Arc<dyn fauna_client_config::SuccessionLedgerStore> {
    std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(handle))
}

/// The backup-destination seam — `ledger_seam`'s twin over the same handle.
/// Every read and write of the per-box `fauna.state.backup` list and its
/// unattested marks (the Backups page, the folders page's destination places,
/// the Nests page's backup trust rows) goes through it, resolved per call and
/// waited for while the runtime is still starting
/// (`fauna_client_config::ResolvingLedgerStore`).
pub(crate) fn backup_seam() -> std::sync::Arc<dyn fauna_client_config::BackupStateStore> {
    std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(handle))
}

/// Install `handle` as this tab's runtime for `actor_id_hex` through the
/// host's own claim and install — what [`start`] does once a store is open —
/// for tests that assemble the store themselves.
#[cfg(test)]
async fn install_for_test(actor_id_hex: String, handle: AccountStoreHandle) {
    let claim = host().begin(Running {
        actor_id_hex,
        peer_connect: None,
        store_changes: Rc::new(OnceCell::new()),
    });
    assert_eq!(
        host().finish(claim, handle).await,
        InstallOutcome::Installed
    );
}

/// Shut this tab's account runtime down and leave the machine enrolled (an
/// account switch, a re-pinned tab, a superseded start); resolves once the
/// store and its Web Lock are released. A no-op when none runs.
#[wasm_bindgen(js_name = accountRuntimeShutdown, unchecked_return_type = "Promise<void>")]
pub fn account_runtime_shutdown() -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async {
        shutdown().await;
        Ok(JsValue::UNDEFINED)
    })
}

/// The **sign-out** stop: retire this browser's enrollment over the session
/// the tab still holds, then shut the runtime down
/// (`AccountStoreHandle::shutdown_for_sign_out`). The credential wipe and the
/// store erase follow it; the caller bounds the wait
/// ([`account_runtime_stop_budget_ms`]). A no-op when no runtime runs.
#[wasm_bindgen(
    js_name = accountRuntimeShutdownForSignOut,
    unchecked_return_type = "Promise<void>"
)]
pub fn account_runtime_shutdown_for_sign_out() -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async {
        stop(StopReason::SignOut).await;
        Ok(JsValue::UNDEFINED)
    })
}

/// The hosts' one stop budget, in milliseconds
/// (`ACCOUNT_RUNTIME_STOP_BUDGET`) — how long a sign-out waits for the
/// runtime's stop, an in-flight start included, before it erases anyway.
#[wasm_bindgen(js_name = accountRuntimeStopBudgetMs)]
pub fn account_runtime_stop_budget_ms() -> u32 {
    ACCOUNT_RUNTIME_STOP_BUDGET.as_millis() as u32
}

/// The standing refusal of this browser's enrollment, as the one rendered
/// sentence (`EnrollmentRefusal::notice` — the SAME string the wire code
/// localizes to), or `undefined` — the devices page paints it on
/// `error-message` on every hydrate (`ui/devices.md` § State & data shape).
/// The twin of `FfiNestClient::account_enrollment_notice`.
#[wasm_bindgen(
    js_name = accountEnrollmentNotice,
    unchecked_return_type = "Promise<string | undefined>"
)]
pub fn account_enrollment_notice() -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async {
        let Some(handle) = handle() else {
            return Ok(JsValue::UNDEFINED);
        };
        Ok(match handle.enrollment_refusal().await {
            Ok(Some(refusal)) => JsValue::from_str(&refusal.notice()),
            Ok(None) => JsValue::UNDEFINED,
            Err(e) => {
                tracing::debug!("account enrollment notice: {e}");
                JsValue::UNDEFINED
            }
        })
    })
}

/// The `sync_devices` row this browser's enrollment registered on, or
/// `undefined` before it has — the devices page's This-device marker reads it
/// (`ui/devices.md` § This-device marker; the handle's `enrolled_device_row`).
#[wasm_bindgen(
    js_name = accountEnrolledDeviceRow,
    unchecked_return_type = "Promise<string | undefined>"
)]
pub fn account_enrolled_device_row() -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async {
        let Some(handle) = handle() else {
            return Ok(JsValue::UNDEFINED);
        };
        Ok(match handle.enrolled_device_row().await {
            Ok(Some(row)) => JsValue::from_str(&row),
            Ok(None) => JsValue::UNDEFINED,
            Err(e) => {
                tracing::debug!("account enrolled device row: {e}");
                JsValue::UNDEFINED
            }
        })
    })
}

/// The **store-change notice** for this tab's open pages
/// (`account-runtime.md` § Multi-instance concurrency → *A runtime's own pump
/// is a source of the notice too*, parts 4 and 5): resolves with the notice
/// count once it differs from `seen` — "the account store may have changed",
/// payload-free — and the caller re-arms with the count it was handed (`0`
/// first). Resolves `undefined` when this tab hosts no runtime, and at once
/// when the runtime stops while the caller waits; it never rejects. The
/// shared `StoreChangeLevel` over the one watch — the twin of `fauna-ffi`'s
/// `FfiStoreChangeListener`.
#[wasm_bindgen(
    js_name = accountStoreChangedAfter,
    unchecked_return_type = "Promise<number | undefined>"
)]
pub fn account_store_changed_after(seen: u32) -> js_sys::Promise {
    let waiter = host()
        .with_installed(|_, r| r.store_changes.get().map(|level| level.changed_after(seen)))
        .flatten();
    wasm_bindgen_futures::future_to_promise(async move {
        Ok(match waiter {
            Some(waiter) => waiter.await.map_or(JsValue::UNDEFINED, JsValue::from),
            None => JsValue::UNDEFINED,
        })
    })
}

/// The `account_pump_cycles` e2e state value
/// (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`) — the shared
/// `PumpCyclesView` shape every hosting app publishes, `runtime: false` while
/// no runtime runs. A plain atomic read, legal on the state path.
#[wasm_bindgen(js_name = accountPumpCyclesJson)]
pub fn account_pump_cycles_json() -> String {
    serde_json::to_string(&PumpCyclesView::of(handle().as_ref())).unwrap_or_default()
}

/// e2e-only: the `device_set_state` command's body — this browser's own
/// account-runtime read of `device_id_hex`'s `fauna.state.device-set` plane
/// row, as the JSON every hosting app answers (`{"found": false}` with no
/// runtime). The SAME reader the native dispatchers call
/// (`fauna_account_plane::account_driver::e2e_readers::device_set_state`),
/// never a twin. Keyed on `test-helpers` alone (convention 15 rule (b)), so a
/// production bundle carries no such export.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = accountDeviceSetStateJsonForTest, unchecked_return_type = "Promise<string>")]
pub fn account_device_set_state_json(device_id_hex: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let view = fauna_account_plane::account_driver::e2e_readers::device_set_state(
            handle().as_ref(),
            &device_id_hex,
        )
        .await;
        Ok(JsValue::from_str(
            &serde_json::to_string(&view).unwrap_or_else(|_| r#"{"found":false}"#.into()),
        ))
    })
}

/// e2e-only: the `read_marker_state` command's body — this browser's account
/// store read marker for `channel_id_hex`, as the JSON every hosting app
/// answers (`{"found": false}` with no runtime). The SAME reader the native
/// dispatchers call
/// (`fauna_account_plane::account_driver::e2e_readers::read_marker_state`),
/// never a twin; gated like [`account_device_set_state_json`].
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = accountReadMarkerStateJsonForTest, unchecked_return_type = "Promise<string>")]
pub fn account_read_marker_state_json(channel_id_hex: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let view = fauna_account_plane::account_driver::e2e_readers::read_marker_state(
            handle().as_ref(),
            &channel_id_hex,
        )
        .await;
        Ok(JsValue::from_str(
            &serde_json::to_string(&view).unwrap_or_else(|_| r#"{"found":false}"#.into()),
        ))
    })
}

/// Run one full account-pump pass now (`fauna_e2e_agent::ACCOUNT_PUMP_NOW`):
/// `AccountStoreHandle::reconcile_now`, the ticker's own work on demand.
/// Resolves when the pass has run; a no-op when no runtime runs.
#[wasm_bindgen(js_name = accountPumpNow, unchecked_return_type = "Promise<void>")]
pub fn account_pump_now() -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async {
        if let Some(handle) = handle()
            && let Err(e) = handle.reconcile_now().await
        {
            tracing::warn!("account pump now: {e:#}");
        }
        Ok(JsValue::UNDEFINED)
    })
}

/// The mechanism proof of ruling (4)'s decision (g): the driver hosted by
/// `web_host` over the REAL IndexedDB backend, with `NoLegs` and a fake
/// requester, the seams registered through `wire_parts` on a real manager,
/// and a group-reception keypair put and read back through the seam after the
/// store is shut down and reassembled — the community class's key survives a
/// browser replica's reload without a nest. Founding and accepting on web are
/// the e2e's.
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use ed25519_dalek::SigningKey;
    use fauna_account_seams::group_reception::AccountGroupReceptionKeys;
    use fauna_client_accounts::SecretStore;
    use fauna_conversations::ConversationsManager;
    use fauna_conversations::backend::{GroupReceptionKeys, SelfAddress};
    use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
    use fauna_conversations::backends::mock::InertConversationsRpc;
    use fauna_core::group_generation::GroupReceptionKeyRecord;
    use fauna_mls::engine::MlsEngine;
    use fauna_protocol::generation_escrow::{EscrowPutReply, EscrowPutRequest, KIND_ESCROW_PUT};
    use fauna_protocol::{RpcErrorClass, RpcRequester, decode_strict, encode_canonical};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// The slot, in memory: the subject here is the store, not the secret
    /// store, and a test must not write this origin's real localStorage.
    #[derive(Default)]
    struct MemSecrets(Mutex<HashMap<String, String>>);

    impl SecretStore for MemSecrets {
        fn get(&self, key: &str) -> Option<String> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_owned());
        }
        fn delete(&self, key: &str) {
            self.0.lock().unwrap().remove(key);
        }
    }

    #[derive(Debug)]
    struct NoNest(&'static str);

    impl std::fmt::Display for NoNest {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "no nest in this test: {}", self.0)
        }
    }

    impl RpcErrorClass for NoNest {
        fn is_rejection(&self) -> bool {
            false
        }
    }

    /// The escrow holder this test's account trusts — standing in for the
    /// origin's pinned nest.
    fn holder_key() -> SigningKey {
        SigningKey::from_bytes(&[0x4E; 32])
    }

    /// Answers `fauna.nest.info` and the first-need mint's escrow deposit
    /// (a receipt signed by [`holder_key`], so a generation tip resolves and
    /// the fleet-only reception-key kind can seal); every leg that would move
    /// data to a nest fails — the plane's own puts included: a mint whose
    /// holder answered stands on its local rows — so everything the test
    /// reads comes off the browser's own replica. Every kind asked for is
    /// noted, in order, so a test can see what a stop told the nest.
    ///
    /// [`Self::listing`] also answers the plane's walks with an empty page —
    /// a nest holding none of the account's rows yet — so the replica has
    /// listed and a save gated on that listing can be written.
    #[derive(Clone, Default)]
    struct EscrowOnly(Arc<Mutex<Vec<&'static str>>>, bool);

    impl EscrowOnly {
        fn listing() -> Self {
            Self(Arc::default(), true)
        }

        fn asked(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().clone()
        }
    }

    impl RpcRequester for EscrowOnly {
        type Error = NoNest;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, NoNest>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.0.lock().unwrap().push(kind);
            let reply = match kind {
                "fauna.nest.info" => {
                    encode_canonical(&fauna_protocol::discovery::NestInfoReply::default())
                }
                KIND_ESCROW_PUT => {
                    let req: EscrowPutRequest =
                        decode_strict(&encode_canonical(&payload).expect("encode request"))
                            .expect("decode escrow put");
                    let receipt = fauna_core::generation::sign_escrow_receipt(
                        &holder_key(),
                        req.generation_id
                            .as_slice()
                            .try_into()
                            .expect("32-byte generation id"),
                        blake3::hash(&req.wrap).into(),
                        &req.target_key,
                        7_000,
                    );
                    encode_canonical(&EscrowPutReply {
                        receipt: fauna_core::encoding::canonical_encode(&receipt)
                            .expect("encode receipt")
                            .to_vec()
                            .into(),
                        extra: Default::default(),
                    })
                }
                "fauna.sync.changes.list" if self.1 => {
                    encode_canonical(&fauna_protocol::sync::SyncChangesListReply::default())
                }
                _ => return Err(NoNest(kind)),
            };
            Ok(decode_strict(&reply.expect("encode reply")).expect("reply decodes"))
        }
    }

    impl fauna_protocol::KeyedRpcRequester for EscrowOnly {
        async fn request_keyed<Req, Reply>(
            &self,
            kind: &'static str,
            _idempotency_key: [u8; 16],
            payload: Req,
        ) -> Result<Reply, NoNest>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.request(kind, payload).await
        }
    }

    const SEED: [u8; 32] = [0x21; 32];

    fn params(root: &StoreRoot) -> WebRuntimeParams<EscrowOnly> {
        params_over(root, EscrowOnly::default())
    }

    fn params_over(root: &StoreRoot, rpc: EscrowOnly) -> WebRuntimeParams<EscrowOnly> {
        let keypair = ActorKeypair::from_secret(SEED);
        WebRuntimeParams {
            store_root: root.clone(),
            actor_id_hex: keypair.actor_id_hex(),
            rpc,
            principal: RuntimePrincipal::SeedHolding(keypair.into()),
            memberships: None,
            attested_predecessors: Default::default(),
            trusted_escrow_holders: fauna_account_plane::account_driver::fixed_holders(vec![
                holder_key().verifying_key().to_bytes(),
            ]),
            enrollment_target_device_id: "ab".repeat(16),
            backstop_interval: DEFAULT_BACKSTOP_INTERVAL,
            reconnects: None,
            pushes: None,
            linked_nests: None,
            owed_nests: None,
        }
    }

    #[wasm_bindgen_test]
    async fn a_reception_key_round_trips_through_the_browser_replica_without_a_nest() {
        // The panic hook and the console layer: a failure here names itself
        // (e2e convention 6) instead of dying as a bare wasm trap.
        crate::logs::install_logging();
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("randomness");
        let root = StoreRoot::at(format!("fauna-test-web-host-{}", hex::encode(nonce)));
        let secrets = Arc::new(MemSecrets::default());

        let handle = web_host::start(params(&root), Arc::clone(&secrets))
            .await
            .expect("the runtime assembles over IndexedDB");

        // The seams, through the one shared join, on a real manager.
        let keypair = ActorKeypair::from_secret(SEED);
        let manager = ConversationsManager::new();
        let backend = Arc::new(FaunaMlsBackend::new_shared(
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(SEED)).expect("engine")),
            Arc::new(InertConversationsRpc),
            SelfAddress::new(""),
            keypair.actor_id(),
        ));
        manager.register_backend(backend.clone());
        fauna_account_seams::conversation_seams::wire_parts(
            &manager,
            &backend,
            handle.clone(),
            &LocalSpawner,
        );

        let record = GroupReceptionKeyRecord::mint(1);
        let keys = AccountGroupReceptionKeys::new(handle.clone());
        assert!(
            keys.put_reception_key(record.clone()).await,
            "the put mints generation 0 through the trusted holder and is durable"
        );
        assert!(
            keys.reception_keys().await.contains(&record),
            "read back live"
        );
        handle.shutdown().await;

        // A reload: a fresh driver over the same IndexedDB database and slot.
        let handle = web_host::start(params(&root), secrets)
            .await
            .expect("the runtime reassembles over the same store");
        let keys = AccountGroupReceptionKeys::new(handle.clone());
        assert!(
            keys.reception_keys().await.contains(&record),
            "the reception key survived the store's reload"
        );
        handle.shutdown().await;
    }

    /// **Refinement 10 on web — the lost-slot arm.** Browser storage is
    /// cleared and evicted per bucket, so the slot (localStorage) can go while
    /// the store (IndexedDB) stays. The next assembly mints a fresh writer
    /// into the empty slot; the heal between the backend open and the store
    /// open fences the stamped writer onto it, so the store reopens under
    /// the fresh key with its rows intact — where it used to refuse the store
    /// ("belongs to a different writer") and leave the tab with no runtime.
    #[wasm_bindgen_test]
    async fn a_lost_slot_over_a_kept_store_heals_onto_a_fresh_writer() {
        use fauna_account_store::indexeddb::IndexedDbBackend;
        use fauna_account_store::store::{retired_writers, stamped_and_burnt_writer};

        crate::logs::install_logging();
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("randomness");
        let root = StoreRoot::at(format!("fauna-test-lost-slot-{}", hex::encode(nonce)));
        let secrets = Arc::new(MemSecrets::default());
        let actor_id_hex = ActorKeypair::from_secret(SEED).actor_id_hex();

        let handle = web_host::start(params(&root), Arc::clone(&secrets))
            .await
            .expect("the runtime assembles over IndexedDB");
        let record = GroupReceptionKeyRecord::mint(1);
        assert!(
            AccountGroupReceptionKeys::new(handle.clone())
                .put_reception_key(record.clone())
                .await,
            "a row lands in the store before the slot is lost"
        );
        handle.shutdown().await;
        let lost = secrets.get(&actor_id_hex).expect("the slot held a writer");

        // The slot is lost; the store is kept.
        secrets.delete(&actor_id_hex);

        let handle = web_host::start(params(&root), Arc::clone(&secrets))
            .await
            .expect("the heal reopens the kept store under a fresh writer");
        assert!(
            AccountGroupReceptionKeys::new(handle.clone())
                .reception_keys()
                .await
                .contains(&record),
            "the kept store's rows survive the fence"
        );
        handle.shutdown().await;

        let fresh = secrets
            .get(&actor_id_hex)
            .expect("the heal left a writer in the slot");
        assert_ne!(
            fresh, lost,
            "an empty slot mints; the lost key is not guessed back"
        );
        let fresh_pub =
            fauna_account_plane::principal_bundle::load_writer_key(&*secrets, &actor_id_hex)
                .expect("the fresh writer reads back")
                .verifying_key()
                .to_bytes();
        let lost_pub = SigningKey::from_bytes(
            &hex::decode(&lost)
                .expect("hex")
                .try_into()
                .expect("32 bytes"),
        )
        .verifying_key()
        .to_bytes();
        let backend = IndexedDbBackend::open(&root.store_name(&actor_id_hex).expect("store name"))
            .await
            .expect("reopen the backend");
        let (stamped, _) = stamped_and_burnt_writer(&backend)
            .await
            .expect("read the stamp");
        assert_eq!(
            stamped.map(|w| w.0),
            Some(fresh_pub),
            "the store is stamped with the slot's fresh writer"
        );
        assert!(
            retired_writers(&backend)
                .await
                .expect("read the retired memory")
                .iter()
                .any(|w| w.0 == lost_pub),
            "the lost writer is retired, never put back to work"
        );
    }

    /// **A pass beside local commands still writes its own rows.** The driver
    /// serves a local command at a pass's yield point and does not poll the
    /// pass while it does, so over IndexedDB a read-then-write transaction the
    /// pass had open used to commit request-less under it: every pass of a
    /// busy tab reported `publish_pending (fleet): … relay plane: IndexedDB
    /// put: TransactionInactiveError`, and the tab's own rows never reached
    /// its relay plane. One unpublished own row per plane — a preference on
    /// the delegable plane, the assembly's own bootstrap rows on the fleet
    /// plane; this requester has no nest to send either to — then local
    /// reads arriving all through the publish step and the pass: the report
    /// names no store failure, and both planes' rows are in the relay plane.
    #[wasm_bindgen_test]
    async fn a_pass_beside_local_commands_records_both_planes_own_rows() {
        use fauna_account_store::backend::StoreBackend;
        use fauna_account_store::indexeddb::IndexedDbBackend;
        use fauna_protocol::merge_policy::KIND_MODERATION;

        crate::logs::install_logging();
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("randomness");
        let root = StoreRoot::at(format!("fauna-test-busy-pass-{}", hex::encode(nonce)));
        let actor_id_hex = ActorKeypair::from_secret(SEED).actor_id_hex();
        let handle = web_host::start(params(&root), Arc::new(MemSecrets::default()))
            .await
            .expect("the runtime assembles over IndexedDB");

        let moderation =
            fauna_core::encoding::canonical_encode(&fauna_core::data::ModerationConfig {
                muted_keywords: vec![fauna_core::data::MutedKeyword::new("muted")],
                ..Default::default()
            })
            .expect("encode moderation")
            .to_vec();
        handle
            .put_preference(KIND_MODERATION, moderation)
            .await
            .expect("the delegable plane's own row is written");

        let reads = async {
            for _ in 0..100 {
                handle
                    .get_preference(KIND_MODERATION)
                    .await
                    .expect("a local read is served inside the pass");
            }
        };
        let (report, ()) = futures_util::future::join(handle.reconcile_now(), reads).await;
        let report = report.expect("the pass ran");
        let store_failures: Vec<&String> = report
            .errors
            .iter()
            .filter(|e| e.contains("IndexedDB"))
            .collect();
        assert!(
            store_failures.is_empty(),
            "a pass's transactions do not depend on when the driver polls it: {store_failures:#?}"
        );
        handle.shutdown().await;

        let backend = IndexedDbBackend::open(&root.store_name(&actor_id_hex).expect("store name"))
            .await
            .expect("reopen the backend");
        let held = backend
            .relay_meter(&[])
            .await
            .expect("read the relay plane");
        let scopes: std::collections::BTreeSet<&str> = held
            .iter()
            .filter(|family| family.rows > 0)
            .map(|family| family.scope.as_str())
            .collect();
        assert_eq!(
            scopes.len(),
            2,
            "each plane's own row is recorded for the peer leg: {held:#?}"
        );
    }

    /// **The teardown rider and the erase behind it.** A sign-out's stop asks
    /// the nest to retire this browser's enrollment
    /// (`fauna.sync.device_grant.revoke`, over the session the tab still
    /// holds) before the store closes, where the plain stop asks nothing; and
    /// once the stop has returned, the account-scope erase leaves no store of
    /// that name in the origin — a delete that would wait for ever behind a
    /// store the stop had left open.
    #[wasm_bindgen_test]
    async fn a_sign_out_stop_asks_the_nest_to_retire_and_the_erase_leaves_no_store() {
        use fauna_account_store::indexeddb::IndexedDbBackend;

        const REVOKE: &str = "fauna.sync.device_grant.revoke";

        crate::logs::install_logging();
        let actor_id_hex = ActorKeypair::from_secret(SEED).actor_id_hex();
        let install = |handle: AccountStoreHandle| install_for_test(actor_id_hex.clone(), handle);
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("randomness");
        let root = StoreRoot::at(format!("fauna-test-sign-out-{}", hex::encode(nonce)));
        let secrets = Arc::new(MemSecrets::default());

        // The plain stop first: the machine stays enrolled, the nest is told
        // nothing.
        shutdown().await;
        let rpc = EscrowOnly::default();
        install(
            web_host::start(params_over(&root, rpc.clone()), Arc::clone(&secrets))
                .await
                .expect("the runtime assembles over IndexedDB"),
        )
        .await;
        stop(StopReason::AccountSwitch).await;
        assert!(handle().is_none(), "the stop took the tab's runtime");
        assert!(
            !rpc.asked().contains(&REVOKE),
            "a plain stop leaves the machine enrolled"
        );

        // The sign-out stop, over the same store and slot.
        let rpc = EscrowOnly::default();
        install(
            web_host::start(params_over(&root, rpc.clone()), Arc::clone(&secrets))
                .await
                .expect("the runtime reassembles over the same store"),
        )
        .await;
        stop(StopReason::SignOut).await;
        assert!(handle().is_none(), "the stop took the tab's runtime");
        assert!(
            rpc.asked().contains(&REVOKE),
            "the sign-out stop asks the nest to retire the enrollment; it asked {:?}",
            rpc.asked()
        );

        crate::account_scope::erase_actor_scope_under(&root, &actor_id_hex)
            .await
            .expect("the erase runs once the stop has closed the store");
        let name = root.store_name(&actor_id_hex).expect("store name");
        assert!(
            IndexedDbBackend::open_existing(&name)
                .await
                .expect("probe the origin")
                .is_none(),
            "the erase left no account store behind"
        );
    }

    /// **A remove-account retires this browser from the removed account**
    /// (`account-scoping.md` § Erasure follows scope → *Removing an account
    /// retires this machine from it, as a sign-out does*): the nest half rides
    /// the registry removal's snapshot, assembles over the account's store
    /// with none of this tab's runtime involved, and asks the nest for the
    /// sign-out's `fauna.sync.device_grant.revoke` — while nothing it writes
    /// reaches the registry the removal left, and the erase behind it leaves
    /// no store. An account this browser never enrolled on dials nothing.
    #[wasm_bindgen_test]
    async fn a_removal_asks_the_nest_to_retire_the_removed_account_and_writes_nothing_back() {
        use fauna_account_store::indexeddb::IndexedDbBackend;
        use fauna_client_accounts::AccountRegistry;

        const REVOKE: &str = "fauna.sync.device_grant.revoke";

        crate::logs::install_logging();
        shutdown().await;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("randomness");
        let root = StoreRoot::at(format!("fauna-test-removal-{}", hex::encode(nonce)));
        let secrets = Arc::new(MemSecrets::default());
        let registry = AccountRegistry::new(Arc::clone(&secrets) as _);
        let actor_id_hex = registry
            .add_account(
                &hex::encode(SEED),
                Some("https://removed.example"),
                Some(&"cd".repeat(32)),
            )
            .expect("the account is added");

        // Never enrolled: nothing to retire, nothing dialled.
        let never = {
            let other = AccountRegistry::new(Arc::new(MemSecrets::default()) as _);
            let actor = other
                .add_account(&"22".repeat(32), Some("https://never.example"), None)
                .expect("the account is added");
            Arc::new(
                other
                    .remove_with_snapshot(&actor)
                    .expect("the registry removes it"),
            )
        };
        let dialled = EscrowOnly::default();
        assert_eq!(
            retire_removed_over(Arc::clone(&never), &root, {
                let dialled = dialled.clone();
                async move { Ok(dialled) }
            })
            .await,
            EnrollmentRetirement::NotEnrolled
        );
        assert!(
            dialled.asked().is_empty(),
            "an unenrolled removal dials nothing"
        );

        // Enrolled: the account ran a runtime of its own over the registry's
        // slots, which minted its device authorization.
        let enrolled = web_host::start(
            params_over(&root, EscrowOnly::default()),
            Arc::clone(&secrets),
        )
        .await
        .expect("the account's runtime assembles over IndexedDB");
        enrolled.shutdown().await;
        drop(enrolled);
        secrets.set(&format!("{actor_id_hex}/device-auth"), "an authorization");

        let removed = Arc::new(
            registry
                .remove_with_snapshot(&actor_id_hex)
                .expect("the registry removes it"),
        );
        assert!(removed.holds_device_authorization());
        let rpc = EscrowOnly::default();
        let _ = retire_removed_over(Arc::clone(&removed), &root, {
            let rpc = rpc.clone();
            async move { Ok(rpc) }
        })
        .await;
        assert!(
            rpc.asked().contains(&REVOKE),
            "the removal's nest half asks the nest to retire the enrollment; it asked {:?}",
            rpc.asked()
        );
        assert!(handle().is_none(), "the tab's own runtime is never touched");
        let left: Vec<String> = secrets
            .0
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.contains(&actor_id_hex))
            .cloned()
            .collect();
        assert!(
            left.is_empty(),
            "the nest half wrote the removed account's slots back: {left:?}"
        );

        crate::account_scope::erase_actor_scope_under(&root, &actor_id_hex)
            .await
            .expect("the erase runs once the nest half has closed the store");
        let name = root.store_name(&actor_id_hex).expect("store name");
        assert!(
            IndexedDbBackend::open_existing(&name)
                .await
                .expect("probe the origin")
                .is_none(),
            "the erase left no account store behind"
        );
    }

    // ── The account port (decision (i)): the core chunk's half over the REAL
    // `web_host` runtime. The consumer chunk's forwarder is the same
    // `PortFleetRemoval` the folders chunk wires; only the JS hop between the
    // two chunks is replaced by a direct call into `account_port::dispatch`.

    /// The folders chunk's forwarder, with the SPA's `sharedAccountPort` hop
    /// replaced by a direct call into the core chunk's dispatch.
    struct CoreChunk(String);

    #[async_trait::async_trait(?Send)]
    impl fauna_account_port::PortTransport for CoreChunk {
        async fn call(
            &self,
            door: &'static str,
            payload: Vec<u8>,
        ) -> Result<Vec<u8>, fauna_account_port::PortFault> {
            crate::account_port::dispatch(self.0.clone(), door, &payload).await
        }
    }

    fn port_for(actor_id_hex: String) -> fauna_devices_machine::port::PortFleetRemoval<CoreChunk> {
        fauna_devices_machine::port::PortFleetRemoval::new(CoreChunk(actor_id_hex))
    }

    /// Start a runtime for [`SEED`]'s account over a fresh store and install
    /// it as this tab's, as `start` does; answers the store's writer — this
    /// browser's own fleet id.
    async fn install_seed_runtime(tag: &str) -> [u8; 32] {
        crate::logs::install_logging();
        shutdown().await;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("randomness");
        let root = StoreRoot::at(format!(
            "fauna-test-account-port-{tag}-{}",
            hex::encode(nonce)
        ));
        let secrets = Arc::new(MemSecrets::default());
        let handle = web_host::start(params(&root), Arc::clone(&secrets))
            .await
            .expect("the runtime assembles over IndexedDB");
        let actor_id_hex = ActorKeypair::from_secret(SEED).actor_id_hex();
        let me = fauna_account_plane::principal_bundle::load_writer_key(&*secrets, &actor_id_hex)
            .expect("the writer reads back")
            .verifying_key()
            .to_bytes();
        install_for_test(actor_id_hex, handle).await;
        me
    }

    /// No runtime in the tab: the member read and the removal are refused —
    /// never answered "nobody", which would let the nest deletion run alone.
    #[wasm_bindgen_test]
    async fn the_port_refuses_when_no_runtime_runs() {
        use fauna_devices_machine::{FleetRemoval, FleetRemovalRefusal};
        shutdown().await;
        let port = port_for(ActorKeypair::from_secret(SEED).actor_id_hex());
        assert!(port.fleet_members(Vec::new()).await.is_err());
        assert!(matches!(
            port.resolve_removal("aa", None).await,
            Err(FleetRemovalRefusal::Unavailable(_))
        ));
    }

    /// A port minted for another account is refused by this account's
    /// running runtime (decision (e)).
    #[wasm_bindgen_test]
    async fn the_port_refuses_another_accounts_call() {
        use fauna_devices_machine::{FleetRemoval, FleetRemovalRefusal};
        install_seed_runtime("foreign").await;
        let other = ActorKeypair::from_secret([0x22; 32]).actor_id_hex();
        let port = port_for(other);
        assert!(port.fleet_members(Vec::new()).await.is_err());
        assert!(matches!(
            port.remove_member([0x33; 32]).await,
            Err(FleetRemovalRefusal::Unavailable(_))
        ));
        shutdown().await;
    }

    /// The running account's port is served: the member read answers this
    /// browser's own fleet id — and a door no seam knows is refused by name.
    #[wasm_bindgen_test]
    async fn the_running_accounts_member_read_answers_this_browsers_fleet_id() {
        use fauna_devices_machine::FleetRemoval;
        let me = install_seed_runtime("served").await;
        let actor_id_hex = ActorKeypair::from_secret(SEED).actor_id_hex();
        let view = port_for(actor_id_hex.clone())
            .fleet_members(Vec::new())
            .await
            .expect("the running account's member read is served");
        assert_eq!(view.me, me);
        assert_eq!(
            crate::account_port::dispatch(actor_id_hex, "no_such.door", &[]).await,
            Err(fauna_account_port::PortFault::UnknownDoor(
                "no_such.door".into()
            ))
        );
        shutdown().await;
    }

    /// **The private contact overlay on web** (`contacts.md` § The private
    /// overlay → *Substrate*): the Profile's Save, through the wasm face over
    /// the real web host, writes the nickname and label; the projection the
    /// overlay seam feeds reads them back, and the face's names follow. A face
    /// with no manager — a tab hosting no runtime — refuses the same Save.
    #[wasm_bindgen_test]
    async fn a_private_overlay_save_reads_back_through_the_projection() {
        use crate::contact_overlays::WasmContactOverlays;

        crate::logs::install_logging();
        shutdown().await;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("randomness");
        let root = StoreRoot::at(format!("fauna-test-overlay-{}", hex::encode(nonce)));
        let handle = web_host::start(
            params_over(&root, EscrowOnly::listing()),
            Arc::new(MemSecrets::default()),
        )
        .await
        .expect("the runtime assembles over IndexedDB");
        handle.reconcile_now().await.expect("the replica lists");
        let manager = ConversationsManager::new();
        let backend = Arc::new(FaunaMlsBackend::new_shared(
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(SEED)).expect("engine")),
            Arc::new(InertConversationsRpc),
            SelfAddress::new(""),
            ActorKeypair::from_secret(SEED).actor_id(),
        ));
        manager.register_backend(backend.clone());
        fauna_account_seams::conversation_seams::wire_parts(
            &manager,
            &backend,
            handle.clone(),
            &LocalSpawner,
        );
        install_for_test(ActorKeypair::from_secret(SEED).actor_id_hex(), handle).await;

        let mum = "ab".repeat(32);
        let answer_kind = |answer: &JsValue| {
            js_sys::Reflect::get(answer, &"kind".into())
                .expect("an answer kind")
                .as_string()
                .expect("a string kind")
        };

        let homeless = WasmContactOverlays::without_runtime().editor(mum.clone());
        homeless.set_nickname("Mum".into());
        let refused = wasm_bindgen_futures::JsFuture::from(homeless.save())
            .await
            .expect("the save answers");
        assert_eq!(answer_kind(&refused), "notReady");
        assert!(
            homeless.is_staged(),
            "a refused save keeps the staged edits"
        );

        let face = WasmContactOverlays::over(Arc::clone(&manager));
        let before = face.revision();
        let editor = face.editor(mum.clone());
        editor.set_nickname("Mum".into());
        assert!(editor.add_label("Family").expect("staged").is_undefined());
        let saved = wasm_bindgen_futures::JsFuture::from(editor.save())
            .await
            .expect("the save answers");
        assert_eq!(answer_kind(&saved), "saved", "{saved:?}");
        assert!(!editor.is_staged(), "a saved staging is dropped");

        assert!(face.revision() > before, "the projection moved");
        assert_eq!(face.labels_line(&mum).as_deref(), Some("Family"));
        let label = face
            .peer_label(None, Some("alice".into()), &mum)
            .expect("a label");
        let field = |name: &str| {
            js_sys::Reflect::get(&label, &name.into())
                .expect("a field")
                .as_string()
        };
        assert_eq!(field("primary").as_deref(), Some("Mum"));
        assert_eq!(field("public").as_deref(), Some("alice"));
        assert!(face.matches_filter("fam", Some("alice".into()), None, &mum));
        shutdown().await;
    }
}
