//! The **seed-holding app's** account-store assembly — one implementation of
//! the W3 (account-data-plane.md § Workstreams) client-side lifecycle for every native app (priority #2).
//!
//! # Why this is a crate and not per-app glue
//!
//! [`AccountRuntimeParams`] has fifteen fields, and only four of them are the
//! app's to choose. The other eleven are the same on every native app, and
//! several encode a *safety* decision whose reason is invisible at the call
//! site: the escrow trust must come from this machine's TOFU pin rather than
//! the nest's own claim, the writer-key resolve and the device-id read that
//! names the enrollment target must sit **off** the login path, `process_rpc` must
//! be the store principal's own bearer rather than the app session, and a
//! failure at any of those must leave the app without a runtime rather than refuse the
//! sign-in. tui got all eleven right at the pilot (2026-08-12); the second app
//! to write them out by hand is where they start drifting, and the third is
//! where a leg silently ships a runtime that trusts whatever nest answered.
//!
//! So the app supplies its four values and this crate performs the assembly.
//!
//! # The two phases, and why the split is load-bearing
//!
//! [`build_params`] is **synchronous and cheap** — pure struct construction. It
//! belongs on the login path, because its output is what a teardown has to be
//! able to reach (`sync-agent.md` § Control plane split: "a session is
//! published before it provisions").
//!
//! [`resolve_and_start`] does the **I/O**: the T10 credential-slot read, the
//! store open, and a blocking IPC round trip to the co-located agent. It
//! belongs on a spawned task, because a wedged agent must not hold up a
//! sign-in.
//!
//! # What stays the app's own
//!
//! * **The nest client** — the app's authenticated session.
//! * **Its own dir** — the sandboxed store container (mobile); an app with
//!   none passes `None`.
//! * **[`MembershipSource`]** — the member half of the content-scope set,
//!   which only a process hosting a live MLS engine can answer
//!   (`account-data-plane.md` § Implementation status today → *Built — W3 the
//!   own-actor scope-set derivation*). An app with no conversations session
//!   passes `None`, which reads as *cannot tell right now*.
//! * **Its own device id**, and what to do with the started handle.
//!
//! # What this crate is deliberately NOT
//!
//! It is not the **agent's** assembly. `fauna-sync-agent`'s account host is
//! seedless and MLS-free by construction ([`RuntimePrincipal::Seedless`],
//! `memberships: None`, both pinned by its own unit tests) — the T9 carve-out
//! assumes it can never grow either. Sharing an assembly between a seed-holding
//! app and that host would be sharing exactly the fields whose *absence* is the
//! agent's contract, so the two stay separate on purpose.

pub mod deployment_seeds;
pub mod p2p_participation;
// The conversations seams over the account store moved to their wasm-capable
// home on 2026-09-29 (`fauna-account-seams` — web registers the same seams
// once it hosts the runtime; `account-client-lifecycle.md` § The client-side
// lifecycle → *The trigger fired*, ruling (4)). Re-exported here so tui,
// linux and the `fauna-ffi` seat keep their `fauna_client_account_runtime::…`
// paths; a `tokio::runtime::Handle` still satisfies `wire`'s spawner. The
// devices page's `fleet_removal` door followed on 2026-09-30, when web's core
// chunk began serving it through the account port (§ *The account port*,
// decision (d)); the ATProto credential seam, `atproto_credentials`, was born
// there the same day.
pub use fauna_account_seams::{
    atproto_credentials, atproto_identity, blessed_nests, contact_overlays, conversation_seams,
    fleet_removal, folder_keys, group_reception, peer_anchors, period_keys, read_positions,
    refused_changes, store_change,
};

use std::path::PathBuf;
use std::sync::Arc;

pub use fauna_account_plane::account_driver::SeedHolder;
pub use fauna_account_plane::attested_predecessors::AttestedPredecessors;
pub use fauna_account_plane::owed_delivery::PredecessorSeeds;
use fauna_anon_client::AnonymousNestClient;
use fauna_client::NestClient;
use fauna_client_core::succession_delivery::{OwedNestReach, SignIn, deliver_owed_nests};
use fauna_core::identity::ActorKeypair;
pub use fauna_sync_engine::account_runtime::CloudBackupExclusion;
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, DEFAULT_BACKSTOP_INTERVAL,
    LinkedNestConnector, MembershipSource, OwedNestDeliverer, RuntimePrincipal, StoreRoot,
    production_credential_store, resolve_writer_key_serialized,
};
use fauna_sync_engine::linked_leg::{LinkedConnection, LinkedNestTarget};
use fauna_sync_engine::peer_leg::PeerTransportFactory;

/// A **sandboxed shell's** account-store container, paired with how it stays
/// out of the platform's cloud backup.
///
/// The two travel together because they are one fact: a shell that supplies
/// its own container (the per-app container IS the per-user root there — iOS,
/// android) is the only party that can say how that container is kept out of
/// the backup its keychain rows are kept out of, and it must say so — the
/// account store's writer key is restore-excluded on every phone, so a store
/// dir that restored without it would strand the account plane
/// (`AccountRuntimeParams::store_backup_exclusion`'s docs; `apps/common.md`
/// § Credential storage → *The shared Rust credential slots on the phones*).
/// A desktop passes no container at all and shared Rust states both the
/// per-OS root and the desktop posture.
pub struct SandboxedStoreContainer {
    /// The container the store roots under — not per-actor
    /// (`StoreRoot::store_dir` scopes underneath it).
    pub dir: PathBuf,
    /// How `dir` stays out of the platform's cloud backup — the manifest arm
    /// (android) or the shell excluder (iOS); never the desktop arm.
    pub exclusion: CloudBackupExclusion,
}

/// What the app brings to the assembly — every field a genuine per-app fact.
pub struct AppRuntimeInputs {
    /// This account's actor id, lowercase hex.
    pub actor_id_hex: String,
    /// The seed-holding principal: this identity's keypair and the attested
    /// predecessors' seeds beside it — build it with
    /// [`SeedHolder::from_registry`] over the same registry the
    /// [`Self::attested_predecessors`] walk reads. An app that cannot build
    /// one (no secret in hand) has nothing to assemble and should not call
    /// this crate at all.
    pub principal: SeedHolder,
    /// The member half of the content-scope set — this account's joined
    /// `__conv` channels, read once per pump pass off the app's live MLS
    /// session (`ConversationsSession::joined_conv_channels`).
    ///
    /// **`None` means "cannot tell right now", never "no channels"**, and the
    /// closure the app builds must honour the same distinction internally: an
    /// engine that is still loading must map to `None`, not to `Some(vec![])`.
    /// An affirmative empty answer reads as *left every channel*, and since
    /// scope departure deletes a departed scope's items
    /// (`account-data-plane.md` § Implementation status today → *Built — W3
    /// scope departure*), that reading is a data-loss bug, not a stale walk.
    pub memberships: Option<MembershipSource>,
    /// The account's **attested** succeeded-from identities — the R14 fleet
    /// view's `prior`, the succession-crossing signer allow-list
    /// (`account-data-taxonomy.md` § The generation machinery → *The source
    /// of `prior`*, ruled 2026-09-13). A genuine per-app fact because only the
    /// app's account registry knows what this device possesses: build it with
    /// `AttestedPredecessors::from_backup_keys` over
    /// `AccountRegistry::predecessor_backup_keys_by_actor` — the rows whose
    /// seeds this device holds, the same walk the retired keys come from —
    /// resolved once at the post-auth hook beside those keys, never a second
    /// walk (two resolutions of one fact is the silent divergence the shared
    /// resolver exists to prevent). The value carries each identity's
    /// delegable schedule beside its id, which is what lets the successor's
    /// walk carry that identity's delegable rows
    /// (`succession-aftermath.md` § Re-key scope). Empty for every identity
    /// that never succeeded, and fail-safe when empty on a successor (its
    /// predecessor-signed enrollments drop out of this device's view and
    /// nothing is carried; nothing is admitted).
    pub attested_predecessors: AttestedPredecessors,
    /// The peer-leg transport factory (W5.7). `None` keeps the leg
    /// structurally off, which is the correct shape for an app that has not
    /// taken the `fauna-iroh` dependency yet — the leg is a separate tranche
    /// from hosting the runtime.
    pub peer_transport: Option<PeerTransportFactory>,
    /// The per-user account-store root, for a **sandboxed shell whose
    /// container the OS supplies** — `None` on every desktop host, which is
    /// the overwhelmingly common answer.
    ///
    /// This is not a knob and no human ever chooses it
    /// (`principles.md`'s two-bucket rule): it mirrors `StoreRoot`'s own two
    /// documented constructors, and *which* one is right is a property of the
    /// target, not a preference. `None` → [`StoreRoot::platform`], the per-OS
    /// constant every desktop app and the sync agent resolve identically, so
    /// no app can re-introduce an app-namespaced root by passing the wrong
    /// dir. `Some(dir)` → [`StoreRoot::at`], for the sandboxed mobile shells
    /// where the per-app container *is* the per-user root (sandboxing keeps
    /// sharing per-app by construction) and `platform()`'s per-OS derivation
    /// would resolve a path inside the sandbox that no sibling process — the
    /// app extensions included — can reach.
    ///
    /// ⚠ **A desktop host passing `Some` is the journal-equivocation bug W6
    /// exists to prevent**, and it surfaces only after both roots have
    /// published; the tests below pin the desktop default for that reason.
    ///
    /// Since 2026-08-26 the container carries its cloud-backup exclusion with
    /// it ([`SandboxedStoreContainer`]): the shell that names the container
    /// is the shell that must say how it stays out of the backup.
    pub store_container: Option<SandboxedStoreContainer>,
}

/// What [`resolve_and_start`] needs that the params do not already carry.
pub struct ResolveContext {
    /// The nest this account is signed in to — the device handshake's target.
    pub nest_url: String,
    /// Resolves this machine's own device id, hex — the machine's named row,
    /// the one enrollment target. `None` (or empty) fails
    /// [`resolve_and_start`]: there is no other row to enroll on.
    ///
    /// **A closure, not a value, and that is deliberate.** Reading it is real
    /// I/O on every app — a `device.db` SQLite open on tui, a state-dir read on
    /// linux — so passing the resolved string would put that read on the login
    /// path, which is exactly what this crate's phase split exists to prevent.
    /// Run on a blocking task inside phase 2.
    pub own_device_id_hex: Box<dyn FnOnce() -> Option<String> + Send>,
}

/// Phase 1 — build the params. Synchronous, no I/O; safe on the login path.
///
/// Two fields are deliberately left at their "resolve later" value and filled
/// by [`resolve_and_start`]: `process_rpc` (a credential-slot read plus a lock)
/// and `enrollment_target_device_id` (this device's own id, a DB/state-dir
/// read). Both are I/O that a sign-in must not wait on.
///
/// Infallible by design, the desktop posture included: a **desktop** host (no
/// container) resolves [`CloudBackupExclusion::platform_desktop`], which on a
/// target with no established posture is a shell arm that refuses at
/// assembly — a phone shell forgetting its container is the wiring bug it
/// makes loud, at the one place every consumer already handles a failed
/// exclusion.
pub fn build_params(
    inputs: AppRuntimeInputs,
    rpc: Arc<NestClient>,
    nest_url: &str,
) -> AccountRuntimeParams<Arc<NestClient>> {
    let AppRuntimeInputs {
        actor_id_hex,
        principal,
        memberships,
        attested_predecessors,
        peer_transport,
        store_container,
    } = inputs;
    // The secondary leg's connections authenticate as this same identity
    // (`account-sync-plane.md` § The bind leg, ruling 4): a copy of the seed
    // for the connector, taken before the principal owns it.
    let linked_nests = native_linked_nest_connector(ActorKeypair::from_secret(
        *principal.keypair().secret_bytes(),
    ));
    // …and the road's chain replay signs in as each predecessor the principal
    // holds the seed of (`identity-succession.md` § Enforcement on the home
    // nest → *Every nest the identity is linked to*, **The road**): their
    // copy, taken here for the same reason.
    let owed_nests = native_owed_nest_deliverer(PredecessorSeeds::of(&principal));

    // The UNIFIED per-user root every app and the sync agent resolve (W6
    // path unification). Never a dir derived from this app's own state
    // dir: two roots under one writer key is the journal-equivocation
    // shape W6 exists to prevent, and it would surface only after both
    // had published. A sandboxed shell supplies its container instead —
    // there the per-app container IS the per-user root, and `platform()`
    // would resolve a path no sibling process can reach — and with it the
    // one statement of how that container stays out of the cloud backup.
    let (store_root, store_backup_exclusion) = match store_container {
        Some(SandboxedStoreContainer { dir, exclusion }) => (StoreRoot::at(dir), exclusion),
        None => (
            StoreRoot::platform(),
            CloudBackupExclusion::platform_desktop(),
        ),
    };

    AccountRuntimeParams {
        store_root,
        store_backup_exclusion,
        actor_id_hex,
        rpc,
        // Resolved in phase 2 — a slot read plus a lock, off the login path.
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(principal),
        credentials: production_credential_store(),
        reconnects: None,
        pushes: None,
        backstop_interval: DEFAULT_BACKSTOP_INTERVAL,
        memberships,
        // R14 (account-data-plane.md § The ratified decisions): what this machine PINNED for its nest, never the nest's own
        // claim about itself — asking the live nest who its escrow holder is
        // would let an impostor nominate itself. No pin yet is fail-safe: no
        // receipt verifies, so no tip resolves and fleet-only sealing stays
        // refused with the precise no-tip error. Read through the one shared
        // door, which is also where the e2e's test-build trust seed lives.
        // Re-read at every pass: a rotation the app accepts re-pins the nest.
        trusted_escrow_holders: {
            let nest_url = nest_url.to_string();
            std::sync::Arc::new(move || fauna_client::trust::trusted_escrow_holders(&nest_url))
        },
        // The `prior` half of the same trust: what this device's registry
        // ATTESTS, handed through verbatim — never read off a device-local
        // replica's writer-asserted list (the field's own docs carry the ruling).
        attested_predecessors,
        peer_transport,
        // Resolved in phase 2 — reading this device's own id is a DB/state-dir
        // open, which a sign-in must not wait on.
        enrollment_target_device_id: String::new(),
        linked_nests: Some(linked_nests),
        owed_nests: Some(owed_nests),
    }
}

/// The road's native reach (`fauna_client_core::succession_delivery`): an
/// [`AnonymousNestClient`] to the owed nest's address, bound to the identity
/// read through the one shared door
/// (`fauna_anon_client::trust::connection_bound_identity`: the origin's pin,
/// else a possession proof over the connection), and a [`NestClient`] signed
/// in as a retired identity whose seed this device holds.
struct NativeOwedReach {
    seeds: PredecessorSeeds,
}

impl OwedNestReach for NativeOwedReach {
    type Anon = AnonymousNestClient;
    type Signed = Arc<NestClient>;

    async fn anonymous(&self, url: &str) -> Result<(AnonymousNestClient, [u8; 32]), String> {
        let anon = AnonymousNestClient::connect(url)
            .await
            .map_err(|e| format!("owed nest {url}: connect: {e}"))?;
        let bound = fauna_anon_client::trust::connection_bound_identity(&anon, url)
            .await
            .map_err(|e| format!("owed nest {url}: bound identity: {e}"))?;
        Ok((anon, bound))
    }

    async fn sign_in_as(&self, url: &str, actor_id: &[u8; 32]) -> SignIn<Arc<NestClient>> {
        let Some(keypair) = self.seeds.keypair_for(actor_id) else {
            return SignIn::NoSeed;
        };
        let client = NestClient::new(url.to_string(), keypair);
        match client.connect().await {
            Ok(()) => SignIn::Connected(client),
            // A refused mint flattens to "couldn't obtain a token"; the
            // typed refusal is the auth client's last one, which tells "the
            // nest holds no account for it" from "no nest answered".
            Err(e) => match client.auth().last_auth_refusal() {
                Some(refusal) => SignIn::from_error(&fauna_client::NestClientError::Rpc(refusal)),
                None => SignIn::from_error(&e),
            },
        }
    }
}

/// The road's native deliverer (`identity-succession.md` § Enforcement on the
/// home nest → *Every nest the identity is linked to*, **The road**): the
/// shared delivery body over [`NativeOwedReach`], for whichever keeping nest
/// the pass hands it. Fresh connections per delivery, dropped when it is done:
/// an owed entry is a rare thing, and the pass runs at the backstop cadence.
#[must_use]
pub fn native_owed_nest_deliverer(seeds: PredecessorSeeds) -> OwedNestDeliverer<Arc<NestClient>> {
    let reach = Arc::new(NativeOwedReach { seeds });
    Arc::new(move |keeper: Arc<NestClient>| {
        let reach = Arc::clone(&reach);
        Box::pin(async move {
            deliver_owed_nests(&keeper, &*reach)
                .await
                .map_err(|e| e.to_string())
        })
    })
}

/// The secondary leg's native connector (`account-sync-plane.md` § The bind
/// leg, ruling 4): a second [`NestClient`] to the linked nest's address under
/// this same identity — the owner-authenticated connection the Nests page's
/// both-ends link opens — and the identity **that connection** is bound to,
/// read through the one shared door
/// (`fauna_client::trust::connection_bound_identity`: the origin's pin,
/// SPKI-compared at login, else a possession proof over the connection; never
/// the nest's own `fauna.nest.info` claim). The leg compares it with the
/// pairing row's nest id before any account data moves.
///
/// One connection per linked nest per pass, dropped when the leg is done with
/// it: the leg runs at the backstop cadence, never per nudge.
#[must_use]
pub fn native_linked_nest_connector(keypair: ActorKeypair) -> LinkedNestConnector<Arc<NestClient>> {
    type Connecting = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = anyhow::Result<LinkedConnection<Arc<NestClient>>>>
                + Send,
        >,
    >;
    let keypair = Arc::new(keypair);
    Arc::new(move |target: LinkedNestTarget| -> Connecting {
        let keypair = ActorKeypair::from_secret(*keypair.secret_bytes());
        Box::pin(async move {
            let client = NestClient::new(target.nest_url.clone(), keypair);
            client
                .connect()
                .await
                .map_err(|e| anyhow::anyhow!("linked nest {}: connect: {e}", target.nest_url))?;
            let bound_identity =
                fauna_client::trust::connection_bound_identity(&client, &target.nest_url)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("linked nest {}: bound identity: {e}", target.nest_url)
                    })?;
            Ok(LinkedConnection {
                rpc: client,
                bound_identity,
            })
        })
    })
}

/// Attach the app session's two wake streams — the reconnect watch and the
/// push stream — to already-built params.
///
/// Separate from [`build_params`] only because an app may hold its session's
/// receivers somewhere the params assembly cannot reach; calling it is not
/// optional in production — without it the pump loses its push-reset signal
/// and its push→nudge arm (`fauna_sync_engine::account_runtime::nudge_scope_for_push`)
/// and falls back to the backstop cadence alone. One function for both on
/// purpose: a seat that attached the reconnect watch and forgot the push
/// stream would converge only at the backstop, and nothing else would say so.
pub fn with_session_wakes(
    mut params: AccountRuntimeParams<Arc<NestClient>>,
    reconnects: tokio::sync::watch::Receiver<u64>,
    pushes: tokio::sync::broadcast::Receiver<fauna_protocol::PushEvent>,
) -> AccountRuntimeParams<Arc<NestClient>> {
    params.reconnects = Some(reconnects);
    params.pushes = Some(pushes);
    params
}

/// Phase 2 — resolve the two I/O-bound fields and start the runtime.
///
/// A writer key that will not resolve leaves the runtime on the app session
/// (still correct, just less honest in the sessions list). This device's own
/// id is required: it
/// is the machine's named row, the one enrollment target
/// (`sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
/// block, decision 3), so a host that cannot name it fails the call — a wiring
/// bug made loud rather than an enrollment onto a row no one can address. An
/// app treats a failure here, as it does one from
/// [`AccountStoreRuntime::start`], by running without a runtime rather than
/// failing the sign-in.
pub async fn resolve_and_start(
    mut params: AccountRuntimeParams<Arc<NestClient>>,
    ctx: ResolveContext,
) -> anyhow::Result<AccountStoreHandle> {
    let actor_id_hex = params.actor_id_hex.clone();

    // W5.4b — the runtime's data path authenticates as the machine's STORE
    // PRINCIPAL: its own per-process bearer minted over
    // `fauna.auth.device_handshake` with the writer key, so the sessions list
    // stays an honest per-process record and the app's own session carries
    // only the ceremony's registration legs.
    //
    // Both arms speak, deliberately. Only the failure did until 2026-08-26,
    // which made a silent log ambiguous between the two — and the arms send
    // the data path down *different connections*, so "which one am I on?" is
    // the first question any plane diagnosis asks. The principal's client
    // waits for its grant to be registered, then reports its own outcome
    // (`spawn_connect_retry`); the fallback arm rides the app session, which
    // is connected already.
    let device_client = match resolve_process_rpc(&params.store_root, &actor_id_hex, &ctx.nest_url)
    {
        Ok(device_client) => {
            tracing::debug!(
                "store principal: the account runtime's data path rides the PRINCIPAL's own \
                 client (its first connect waits for the grant — see `spawn_connect_retry`)"
            );
            params.process_rpc = Some(Arc::clone(&device_client));
            Some(device_client)
        }
        Err(e) => {
            tracing::warn!(
                "store principal: writer key not resolvable — the account runtime rides the \
                 app session: {e:#}"
            );
            None
        }
    };

    // Which `sync_devices` row this machine enrolls on: its own id, the
    // machine's named row, unconditionally (the RULED 2026-09-28 block,
    // decision 3). Read on a blocking task — a DB/state-dir open.
    let resolve_own = ctx.own_device_id_hex;
    params.enrollment_target_device_id = tokio::task::spawn_blocking(resolve_own)
        .await
        .ok()
        .flatten()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "account runtime: this device has no own device id to enroll on — \
                 the machine's named row is the one enrollment target"
            )
        })?;

    let handle = AccountStoreRuntime::start(params).await?;
    // Spawned only now, gated on the runtime's grant-registered signal: a
    // first sign-in's principal dials nothing until the enrollment pass has
    // put its grant on the nest, instead of racing that pass into
    // `not_registered` refusals (`transport-connection.md` § The dial
    // budget). Every later launch finds the gate open from assembly.
    if let Some(device_client) = device_client {
        fauna_client::ws_device_handshake_bearer::spawn_connect_retry(
            device_client,
            fauna_client::ws_device_handshake_bearer::ConnectGate::AfterGrant(
                handle.subscribe_grant_registered(),
            ),
        );
    }
    Ok(handle)
}

/// The `account_pump_cycles` e2e state value — the completion barrier beside
/// the `account_pump_now` poke (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY` owns
/// both contracts).
///
/// Shared so two hosting apps cannot publish two shapes of one cross-app
/// contract, exactly as `fauna_conversations::state_json` is shared for the
/// receive-cycle twin (priority #2) — an app's leg is then a read, not its own
/// bookkeeping.
///
/// ```text
/// { "started": N, "completed": M, "runtime": bool, "holder": bool }
/// ```
///
/// **Four distinguishable states, and shipping only the counters made three of
/// them look identical.** That is not a hypothetical: an e2e written against
/// the counters alone (2026-08-18) asserted a poked pass and failed on a
/// perfectly healthy app, because W5.1's election had handed the role to the
/// co-located agent. Measured in one run, same code — linux `(4, 3)` on one app
/// instance, linux *and* tui frozen at `(0, 0)` on a later one.
///
/// * **key absent** — this app has no account-store leg at all. Convention 11's
///   refusal, which the consumer reports loudly; never a zero.
/// * **`runtime: false`** — the app has the leg but no assembled runtime: still
///   assembling, or the assembly failed (it is best-effort by design).
///   Counters are `0`.
/// * **`runtime: true, holder: false`** — assembled, but another co-located
///   process (ordinarily the sync agent) holds the engine-singleton role. This
///   runtime **runs no pass**, so its counters are frozen *correctly* and a
///   caller must not wait on them. Convergence arrives through the shared
///   store.
/// * **`runtime: true, holder: true`** — assembled and pumping. Only here does
///   "poke, then await a completed pass" mean anything.
///
/// A plain atomic read on both halves — convention 11's corollary: no blocking
/// I/O on the state path.
///
/// The shape itself is `fauna_sync_engine::account_runtime::PumpCyclesView`,
/// which web's host publishes too; this is its JSON value.
pub fn account_pump_cycles_json(handle: Option<&AccountStoreHandle>) -> serde_json::Value {
    serde_json::to_value(fauna_sync_engine::account_runtime::PumpCyclesView::of(
        handle,
    ))
    .unwrap_or(serde_json::Value::Null)
}

/// e2e-only: the plane's raw `fauna.state.device-set` record for
/// `device_id_hex`, as JSON — the convergence-assertion read half of the
/// devices page's removals. The reader itself is
/// [`fauna_account_plane::account_driver::e2e_readers::device_set_state`],
/// in the wasm-capable plane crate since 2026-09-30 so web's dispatcher
/// answers the same command over the same code (priority #2); this is its JSON
/// value at the old path every native dispatcher calls.
///
/// **Not a command; an on-demand READER** (async store I/O, never on the
/// per-tick state path), and **gated** unlike [`account_pump_cycles_json`]: it
/// returns real plane content, so convention 15's rule (a) treats it as a seam
/// — `docs/goal/architecture/e2e-automation-surface-gating.md` convention 15.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub async fn device_set_state_json(
    handle: Option<&AccountStoreHandle>,
    device_id_hex: &str,
) -> serde_json::Value {
    let view =
        fauna_account_plane::account_driver::e2e_readers::device_set_state(handle, device_id_hex)
            .await;
    serde_json::to_value(view).unwrap_or_else(|_| serde_json::json!({ "found": false }))
}

/// The account store's read marker for one fauna-native channel —
/// `{"found": true, "through": N}` or `{"found": false}` — the e2e's
/// durability barrier for a read (`conversation-read-state.md` § The
/// read-marker record): a test that must know a read reached the store before
/// it quits the app asks this, never a clock. Built on
/// [`AccountStoreHandle::read_markers`], the same read the conversations glue
/// delivers from. An on-demand reader, gated and dispatched exactly like
/// [`device_set_state_json`] (async store I/O, so never on the per-tick state
/// path; real account content, so convention 15's rule (a)).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub async fn read_marker_state_json(
    handle: Option<&AccountStoreHandle>,
    channel_id_hex: &str,
) -> serde_json::Value {
    let Some(handle) = handle else {
        return serde_json::json!({ "found": false });
    };
    let Ok(markers) = handle.read_markers().await else {
        return serde_json::json!({ "found": false });
    };
    match markers
        .iter()
        .find(|(channel, _)| channel.eq_ignore_ascii_case(channel_id_hex))
    {
        Some((_, through)) => serde_json::json!({ "found": true, "through": through }),
        None => serde_json::json!({ "found": false }),
    }
}

// The stop's budget, its reason and the one stop per handle live beside the
// driver (`fauna_account_plane::account_driver`'s `stop` module) since web
// hosts the runtime too and states the same reason; re-exported here, where
// every native host has always named them.
use fauna_account_plane::account_driver::stop_one;
pub use fauna_account_plane::account_driver::{ACCOUNT_RUNTIME_STOP_BUDGET, StopReason};

/// The **assembly side** of the in-flight seam: publishes the started runtime
/// the instant [`resolve_and_start`] settles, so a teardown can reach a store
/// that is open but not yet installed.
///
/// Dropping one without calling either method reads as [`Self::failed`] — the
/// assembly task died without answering, and there is nothing to stop.
pub struct AssemblySettle(tokio::sync::oneshot::Sender<Option<AccountStoreHandle>>);

impl AssemblySettle {
    /// The assembly started a runtime. Publishes a clone for the teardown;
    /// a cheap no-op when no teardown is waiting.
    pub fn started(self, handle: &AccountStoreHandle) {
        let _ = self.0.send(Some(handle.clone()));
    }

    /// The assembly failed. Resolves the wait **at once**, so a failed
    /// assembly never spends the teardown budget.
    pub fn failed(self) {
        let _ = self.0.send(None);
    }
}

/// Hand-written rather than derived: [`InstallClaim`] is `Debug` and carries
/// one of these, and neither the channel nor an `AccountStoreHandle` owes a
/// `Debug` for that to hold.
impl std::fmt::Debug for AssemblySettle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AssemblySettle")
    }
}

/// The **teardown side**: an assembly that may still be in flight.
///
/// # Why a handle slot is not enough, and this is not belt-and-braces
///
/// `AccountStoreRuntime::start` opens `account-store.db` **on its own OS
/// thread before returning**, and the started handle then has to reach the
/// app — through [`AccountRuntimeHost::finish`], or across a UI queue. Between
/// those two moments a live connection holds the database while the host's own
/// handle slot honestly reads `None`, and a teardown that only *checks* that
/// slot stops nothing and erases anyway. Measured `--app tui` on Windows,
/// 2026-09-04: sign-out landed 475 ms after the assembly minted this machine's
/// writer key, and the erase failed with `os error 32` on a root nothing had
/// torn down (`apps/account-scoping.md` § Erasure follows scope, the ⚠ *A
/// store that is still ASSEMBLING has no handle to release* note).
///
/// POSIX `unlink` removes an open file, which is why this hid on linux for as
/// long as it existed — so closing it is a **convergence** obligation on every
/// host, not a windows patch.
pub struct PendingAssembly(tokio::sync::oneshot::Receiver<Option<AccountStoreHandle>>);

impl PendingAssembly {
    /// Wait for the assembly to settle. `None` means there is nothing to stop
    /// — the assembly failed, or its task died without answering.
    pub async fn settled(self) -> Option<AccountStoreHandle> {
        self.0.await.ok().flatten()
    }
}

/// Mint the in-flight seam. The host — or the app itself, for a deliberately
/// host-less one like tui (`account-data-plane.md` § The account store → *The
/// client-side lifecycle*, the ⚠ *tui is a deliberate NON-consumer* note) —
/// keeps the [`PendingAssembly`]; the assembly task carries the
/// [`AssemblySettle`].
pub fn assembly_channel() -> (AssemblySettle, PendingAssembly) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    (AssemblySettle(tx), PendingAssembly(rx))
}

/// What [`stop_account_runtime`] achieved. Worth logging and worth asserting;
/// never worth branching on at a call site — the erase follows regardless.
#[derive(Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// Neither a settled runtime nor an assembly in flight — nothing this host
    /// knows of held the store.
    NothingToStop,
    /// Everything there was to stop is stopped; the scope is erasable.
    Stopped,
    /// The budget lapsed with something still holding the store. The erase
    /// proceeds anyway, and on windows it will fail with `os error 32`.
    BudgetElapsed,
}

/// Stop this account's runtime — the settled handle, the assembly still in
/// flight, or both — under one bounded budget, and **say what happened**.
///
/// `reason` decides whether the machine's enrollment is retired nest-side
/// first ([`StopReason`]); the retirement's own outcome is logged here, never
/// a reason to refuse the stop.
///
/// This is the shared half of the obligation `apps/account-scoping.md`
/// § Erasure follows scope states: *a host must be able to WAIT for an
/// in-flight assembly, under a named bounded budget, and erase anyway (loudly)
/// if the budget lapses.* Written once because the waiting logic existed only
/// in tui, hand-rolled, and the second host to write it out is where the two
/// drift on a failure that is silent on POSIX.
///
/// # Why the logging is in here rather than at each host
///
/// The same section's ⚠ *the erase must SAY what it did* corollary: a
/// surviving store dir means one thing when the success line is present
/// (something else holds the store) and quite another when it is absent (this
/// teardown never ran), and the two are indistinguishable from a test's
/// assertion alone. That diagnosis is identical on every host, so a host that
/// hand-rolled it would be a host that could forget it.
///
/// # What the caller still owns
///
/// **Whether to await this or spawn it** — a GTK main thread cannot await, an
/// already-async FFI caller should not race the drain. The same division of
/// labour as [`AccountRuntimeHost::take`], which hands back what to stop
/// rather than stopping it.
pub async fn stop_account_runtime(
    settled: Option<AccountStoreHandle>,
    pending: Option<PendingAssembly>,
    budget: std::time::Duration,
    reason: StopReason,
) -> StopOutcome {
    if settled.is_none() && pending.is_none() {
        return StopOutcome::NothingToStop;
    }
    let stop = async move {
        // The settled half first: it is the one that certainly exists.
        // `shutdown` drains the pump's in-flight pass before the store closes,
        // which is what keeps a mid-pass sign-out from leaving the outbox
        // half-drained.
        //
        // ⚠ **Two concurrent `shutdown()` calls on one runtime are not just
        // tolerated, they are the ordinary host case**, and the second one
        // still WAITS — which is the whole reason this can be written so
        // plainly. On a host, a superseded `finish` shuts the fresh runtime
        // down at the same moment this teardown, holding the published clone,
        // does. The store loop takes one `Cmd::Shutdown` and breaks; the other
        // send is either never read or refused, and in both cases the loser's
        // `await` resolves only when the loop drops its command receiver —
        // which happens *after* the store thread has exited
        // (`fauna_sync_engine::account_runtime`'s "store thread exiting" tail,
        // where the reply is deliberately sent last so the release is a causal
        // barrier). So neither caller can return while the file is still open,
        // and neither has to know which of them won.
        let mut stopped_something = false;
        if let Some(store) = settled {
            stop_one(&store, reason).await;
            stopped_something = true;
        }
        // The in-flight half. A failed assembly and a dead assembly task both
        // resolve at once, so neither spends the budget.
        if let Some(pending) = pending
            && let Some(store) = pending.settled().await
        {
            stop_one(&store, reason).await;
            stopped_something = true;
        }
        stopped_something
    };
    match tokio::time::timeout(budget, stop).await {
        Ok(false) => StopOutcome::NothingToStop,
        Ok(true) => {
            tracing::info!("[account-runtime] the account store stopped; the scope is erasable");
            StopOutcome::Stopped
        }
        Err(_) => {
            // Loud, because silence here is exactly how the windows leak went
            // unnoticed: the erase runs anyway, and the sweep reports what
            // survives.
            tracing::warn!(
                "[account-runtime] the account store did not stop within {}s — erasing \
                 anyway; a still-open store fails its remove_dir_all on windows (os error 32)",
                budget.as_secs()
            );
            StopOutcome::BudgetElapsed
        }
    }
}

/// The ordering a host that **spawns** its stop owes the work behind it: the
/// stops still in flight, and the continuations — the erase above all —
/// queued until every one of them has finished.
///
/// A host whose teardown runs on a UI thread cannot await
/// [`stop_account_runtime`] there without holding the UI for the whole
/// [`ACCOUNT_RUNTIME_STOP_BUDGET`], and cannot simply spawn it either: the
/// erase that follows a sign-out is synchronous, so a spawned stop with
/// nothing sequenced behind it is a race the erase wins
/// (`apps/account-scoping.md` § Erasure follows scope). So the host spawns
/// the stop, counts it here, and queues whatever must follow; when a stop's
/// completion comes back to the UI thread it reports [`Self::stop_finished`]
/// and runs every continuation [`Self::next_ready`] releases.
///
/// Two rules, both the reason this is shared rather than written per host:
///
/// * **Nothing runs while any stop is in flight** — including a continuation
///   whose own teardown found nothing to stop. A second sign-out landing while
///   the first one's store is still closing must not erase underneath it.
/// * **Oldest first**, so a second teardown's erase never overtakes the
///   first one's.
///
/// Pure bookkeeping, generic over the host's continuation type, so the
/// ordering is unit-testable without a runtime. Each host keeps its own
/// completion channel and its own loop that drains [`Self::next_ready`]
/// *outside* any borrow of the queue: a continuation may itself tear down,
/// and that teardown's stop must then hold back everything still queued.
pub struct StopQueue<C> {
    in_flight: usize,
    waiting: std::collections::VecDeque<C>,
}

impl<C> Default for StopQueue<C> {
    fn default() -> Self {
        Self {
            in_flight: 0,
            waiting: std::collections::VecDeque::new(),
        }
    }
}

impl<C> StopQueue<C> {
    /// A stop was spawned; nothing queued runs until it reports back.
    pub fn stop_started(&mut self) {
        self.in_flight += 1;
    }

    /// A spawned stop reported back — finished, elapsed, or lost with its
    /// runtime. Saturating: a completion that outlives a reset queue is noise,
    /// never an underflow.
    pub fn stop_finished(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Queue a continuation behind every stop now in flight. The caller then
    /// drains [`Self::next_ready`], which releases it at once when nothing is
    /// stopping — every teardown before an account runtime was ever assembled.
    pub fn push(&mut self, then: C) {
        self.waiting.push_back(then);
    }

    /// The next continuation allowed to run: only once no stop is in flight,
    /// and oldest first.
    pub fn next_ready(&mut self) -> Option<C> {
        if self.in_flight == 0 {
            self.waiting.pop_front()
        } else {
            None
        }
    }

    /// Whether a teardown is still under way: a stop in flight, or a
    /// continuation not yet run. A host refuses input to the outgoing session
    /// for exactly this long.
    pub fn is_busy(&self) -> bool {
        self.in_flight > 0 || !self.waiting.is_empty()
    }
}

/// The **install/teardown lifecycle** every seed-holding host repeats around
/// [`resolve_and_start`] — the slot the started handle lives in, the
/// generation guard that makes spawning the assembly safe, and the in-flight
/// seam a teardown waits on.
///
/// # Why this is shared and not four copies of twenty lines
///
/// Assembly does real I/O, so every host spawns it — and a sign-out or account
/// switch can therefore land *while it runs*. What must happen then is not
/// obvious and is silent when wrong: the freshly-started runtime has to be shut
/// down rather than installed, because installing it means **the signed-out
/// account is still being served** (the shape `apps/sync-agent.md` § Control
/// plane split forbids). linux and the `fauna-ffi` seat each wrote that guard
/// by hand and the second was a near-verbatim copy of the first, comments
/// included — which is exactly the drift this crate's module docs predict for
/// the eleven params, applied to the lifecycle instead. windows, macOS and iOS
/// are about to become the third, fourth and fifth.
///
/// # The payload, and why it is captured at CLAIM rather than at install
///
/// A host often needs one thing *beside* the handle, needed at teardown, and it
/// differs per host: linux stores the tokio [`Handle`] its shutdown must be
/// driven on, because its teardown is called from the GTK main thread and must
/// never build a runtime there. Hosts with nothing to carry use the default
/// `()`.
///
/// It is handed to [`Self::begin`], not to [`Self::finish`], and that moved
/// (2026-09-06) for a reason the earlier shape could not express: **the
/// teardown that most needs the payload is the one that finds an EMPTY slot.**
/// A sign-out landing mid-assembly has to wait for a store that is open but not
/// yet installed — and on linux that wait must be driven on the runtime handle,
/// which under install-time capture does not exist yet. Claim time is when the
/// host first knows both.
///
/// # What it deliberately does NOT do
///
/// [`Self::take`] returns what to stop rather than stopping it, because *how*
/// to await that shutdown is the host's own constraint — linux spawns it and
/// runs the synchronous erase that follows as a GTK main-loop continuation of
/// its completion (that thread cannot await, and must not be held for the
/// stop's budget), the FFI seat awaits it (its caller is already async and a
/// sign-out should not race the drain). Deciding that here would force one of
/// them into the wrong shape. [`stop_account_runtime`] is the shared *body* of
/// that stop; the choice of how to drive it stays at the call site.
///
/// [`Handle`]: https://docs.rs/tokio/latest/tokio/runtime/struct.Handle.html
pub struct AccountRuntimeHost<T = ()> {
    state: std::sync::Mutex<HostState<AccountStoreHandle, T>>,
}

/// The host's whole mutable state, behind **one** lock.
///
/// # Why one lock, and not a mutex per field
///
/// An install is three decisions — is my claim still current, remove the
/// in-flight registration, write the slot — and a teardown is two: advance the
/// generation, take whatever is there. Under a lock per field, a teardown could
/// land *between* an install's second and third decision and see both halves
/// empty: the registration just removed, the slot not yet written. It then
/// reported **nothing to stop**, the erase ran, and the install completed
/// immediately afterwards — leaving the signed-out account's runtime installed
/// and pumping against a store being erased (the `os error 32` shape on
/// windows, deleted-inode writes on linux). No `.await` sat in that gap, which
/// is what made it look safe; the teardown runs on a different thread (the GTK
/// main thread on linux, the FFI caller's thread in the `fauna-ffi` seat), so
/// it never needed one.
///
/// So both sequences are single transitions over this struct, and the
/// in-between state is unrepresentable rather than merely unlikely: while a
/// claim is current, this state always holds either the registration or the
/// slot for a teardown to find.
///
/// Generic over the handle type so the transitions can be tested without a
/// started [`AccountStoreHandle`], whose fields are private to its own crate
/// and which no test can construct.
struct HostState<H, T> {
    /// The installed runtime and this host's teardown payload.
    slot: Option<(H, T)>,
    /// The assembly claimed but not yet settled, and the payload its teardown
    /// will need. Present between [`AccountRuntimeHost::begin`] and whichever
    /// comes first: [`AccountRuntimeHost::finish`] or a teardown's
    /// [`AccountRuntimeHost::take`].
    pending: Option<(PendingAssembly, T)>,
    /// The newest install claim's number. `begin` and `take` both advance it;
    /// [`AccountRuntimeHost::finish`] installs only under the newest.
    generation: u64,
    /// The newest generation a **sign-out** teardown invalidated. Every claim
    /// at or below it belongs to a sign-in whose credential slot that sign-out
    /// erased, so an assembly of theirs that settles late is stopped the
    /// sign-out way ([`Self::settle_install`]). `0` — below every claim — until
    /// the first sign-out.
    signed_out_through: u64,
}

/// The two halves one teardown found — the settled runtime with its payload,
/// and the assembly still in flight with its own. [`AccountRuntimeHost::take`]
/// assembles the public [`RuntimeTeardown`] around them.
type TornDown<H, T> = (Option<(H, T)>, Option<(PendingAssembly, T)>);

/// What [`HostState::settle_install`] decided, for the caller to carry out
/// **after** releasing the lock — shutting a runtime down is async, and nothing
/// may `.await` under a `std::sync::Mutex`.
enum InstallStep<H, T> {
    /// The handle is installed. Shut down whatever it replaced, if anything.
    Installed(Option<(H, T)>),
    /// A teardown or a newer install landed first. Shut this handle down —
    /// **the way whatever superseded it would have**: a sign-out's erase took
    /// this runtime's credential slot, so its enrollment is retired on the way
    /// out; anything else left the slot in place and the machine stays
    /// enrolled ([`StopReason`]).
    Superseded(H, StopReason),
}

impl<H, T> HostState<H, T> {
    const fn empty() -> Self {
        Self {
            slot: None,
            pending: None,
            generation: 0,
            signed_out_through: 0,
        }
    }

    /// How a runtime assembled under `claim_generation` must be stopped now
    /// that it will not be installed.
    ///
    /// ⚠ **The superseded arm is a stop like any other, and a stop has a
    /// reason.** The prologue enrolls the machine *before* the assembly
    /// settles, so a sign-out landing mid-assembly supersedes a runtime that
    /// already holds a registered enrollment. Stopped plainly, that enrollment
    /// outlives the key the erase is about to destroy — a live grant nobody
    /// holds, per such sign-out (`sync-agent-credentials.md` § Implementation
    /// status today). The waiting teardown cannot be relied on
    /// for the retirement: this arm's own shutdown is queued the instant the
    /// handle is published, ahead of anything the teardown sends after waking.
    fn superseded_stop_reason(&self, claim_generation: u64) -> StopReason {
        if claim_generation <= self.signed_out_through {
            StopReason::SignOut
        } else {
            StopReason::AccountSwitch
        }
    }

    /// Claim the next install: advance the generation and register the
    /// assembly as in flight, replacing any earlier registration (whose
    /// generation has just moved on, so nothing will be installed under it).
    fn begin(&mut self, pending: PendingAssembly, payload: T) -> u64 {
        self.generation += 1;
        self.pending = Some((pending, payload));
        self.generation
    }

    /// The **whole install decision**, as one transition: the claim check, the
    /// registration's removal and the slot write — see the type docs for why
    /// they may not be three.
    fn settle_install(&mut self, claim_generation: u64, handle: H) -> InstallStep<H, T> {
        let reason = self.superseded_stop_reason(claim_generation);
        if self.generation != claim_generation {
            return InstallStep::Superseded(handle, reason);
        }
        // Unreachable while the claim is current: only `begin` and `take` clear
        // the registration, and both advance the generation in this same
        // transition. Kept as the fail-closed arm — refusing to install beats
        // inventing a payload, because an un-torn-down runtime is the one
        // outcome this type exists to prevent.
        let Some((_, payload)) = self.pending.take() else {
            return InstallStep::Superseded(handle, reason);
        };
        InstallStep::Installed(self.slot.replace((handle, payload)))
    }

    /// Everything a teardown must stop, and the generation advanced so an
    /// assembly still in flight discards itself — one transition, for the same
    /// reason [`Self::settle_install`] is one. A sign-out additionally marks
    /// every claim so far as signed out, **before** the generation moves on, so
    /// the sign-in that follows is not caught by it.
    fn tear_down(&mut self, reason: StopReason) -> TornDown<H, T> {
        if reason == StopReason::SignOut {
            self.signed_out_through = self.generation;
        }
        self.generation += 1;
        (self.slot.take(), self.pending.take())
    }
}

/// A claim on one install attempt, taken by [`AccountRuntimeHost::begin`]
/// **before** the assembly is spawned and presented back to
/// [`AccountRuntimeHost::finish`] after it returns.
///
/// Not `Copy` or constructible outside this crate on purpose: a host cannot
/// finish an install it never began, nor reuse one claim for two assemblies.
///
/// It carries the [`AssemblySettle`] half of the in-flight seam, so an
/// assembly task that **drops** its claim — the failed-assembly path, where
/// `finish` is never called — resolves a waiting teardown at once instead of
/// spending its whole budget.
#[derive(Debug)]
pub struct InstallClaim(u64, Option<AssemblySettle>);

/// What [`AccountRuntimeHost::finish`] did — worth logging, never worth
/// branching on: both arms leave the host in a correct state.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The handle is now this host's live runtime. Any previous one was shut
    /// down first.
    Installed,
    /// A teardown or a newer install landed while this assembly ran, so the
    /// fresh runtime was shut down instead of installed.
    Superseded,
}

impl<T> AccountRuntimeHost<T> {
    /// An empty host. `const` so it can be a `static` — every consumer wants
    /// exactly one per process, since the W5.1 engine election is per store
    /// and a second host in one process would contend with the first.
    pub const fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(HostState::empty()),
        }
    }

    /// The host's state, through a poisoned lock as well as a healthy one.
    ///
    /// Nothing under this lock can panic — the transitions are moves between
    /// `Option`s — so a poisoned lock means a panic *elsewhere* left the state
    /// intact, and the previous shape's "on poison, skip the operation" would
    /// turn that into the very outcome this type prevents: a teardown that
    /// takes nothing while a runtime stays installed.
    fn state(&self) -> std::sync::MutexGuard<'_, HostState<AccountStoreHandle, T>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Claim the next install and register the assembly as in flight. Call
    /// this **on the login path, before spawning the assembly** — claiming
    /// after the spawn would leave a window in which a teardown bumps nothing
    /// and the assembly installs anyway.
    ///
    /// `payload` is what this host's teardown will need beside the handle; it
    /// is captured here rather than at [`Self::finish`] because a teardown
    /// landing mid-assembly needs it precisely when nothing is installed yet
    /// (see the type docs).
    pub fn begin(&self, payload: T) -> InstallClaim {
        let (settle, pending) = assembly_channel();
        // A previous claim's registration is replaced, not merged: its
        // generation has just moved on, so `finish` will discard whatever it
        // produces and nothing is left for a teardown to wait on. Dropping its
        // receiver also resolves that assembly's own settle send as "nobody
        // waiting", which is true.
        let generation = self.state().begin(pending, payload);
        InstallClaim(generation, Some(settle))
    }

    /// Install `handle` under `claim`, or shut it down if the claim is stale.
    ///
    /// Shutting down the superseded runtime — and the previous one it replaces
    /// — happens here rather than at the call site precisely because forgetting
    /// either is invisible: the account keeps pumping and nothing reports it.
    pub async fn finish(
        &self,
        mut claim: InstallClaim,
        handle: AccountStoreHandle,
    ) -> InstallOutcome {
        // Publish to a waiting teardown FIRST, before either arm below. The
        // superseded arm's own `shutdown()` is what usually stops this runtime,
        // and `shutdown` is idempotent — so a teardown that also reaches it
        // stops it once and both report success. What the publish actually buys
        // is the *wait*: without it the teardown returns while this store is
        // still open, which on windows is the `os error 32` erase failure.
        if let Some(settle) = claim.1.take() {
            settle.started(&handle);
        }
        // ONE transition, not three: the claim check, the registration's
        // removal and the slot write happen under a single lock, so a teardown
        // cannot land between them and come away with "nothing to stop" while
        // this install completes behind it (`HostState`'s docs). The shutdowns
        // below run after the guard is dropped — nothing may `.await` under a
        // `std::sync::Mutex`.
        let step = self.state().settle_install(claim.0, handle);
        match step {
            InstallStep::Superseded(handle, reason) => {
                // The reason is the superseding teardown's, not a default: see
                // `HostState::superseded_stop_reason`.
                stop_one(&handle, reason).await;
                InstallOutcome::Superseded
            }
            InstallStep::Installed(previous) => {
                if let Some((previous, _)) = previous {
                    previous.shutdown().await;
                }
                InstallOutcome::Installed
            }
        }
    }

    /// The live handle, if an assembly has landed. `None` before it completes,
    /// after a teardown, or whenever it failed.
    pub fn handle(&self) -> Option<AccountStoreHandle> {
        self.state().slot.as_ref().map(|(h, _)| h.clone())
    }

    /// Take **everything this host has to stop** for a teardown, and advance
    /// the generation so an assembly still in flight discards itself instead of
    /// installing under the next account.
    ///
    /// **The caller must stop what it gets back** — feed it to
    /// [`stop_account_runtime`] and decide whether to await or spawn that; see
    /// the type docs for why the decision is not made here. The generation
    /// moves even when the slot is empty, which is the case that matters: a
    /// sign-out landing *during* the very first assembly has nothing to take
    /// and everything to prevent — and, since 2026-09-06, an in-flight
    /// assembly to WAIT for rather than merely to invalidate.
    ///
    /// `reason` is the same one the caller is about to hand
    /// [`stop_account_runtime`]. It is recorded here because the in-flight
    /// assembly's own [`Self::finish`] also stops that runtime, usually first,
    /// and must stop it the same way — a sign-out's retirement is otherwise
    /// lost to a plain shutdown queued ahead of it.
    pub fn take(&self, reason: StopReason) -> RuntimeTeardown<T> {
        // One transition, for [`Self::finish`]'s reason: an install that is
        // half-applied is not a state this can observe.
        let (settled, pending) = self.state().tear_down(reason);
        // The payload comes from whichever half exists. Both cannot: `finish`
        // clears the registration as it installs, and `begin` replaces it — so
        // a host is either assembling or settled, never both for one account.
        let (settled, settled_payload) = match settled {
            Some((handle, payload)) => (Some(handle), Some(payload)),
            None => (None, None),
        };
        let (pending, pending_payload) = match pending {
            Some((pending, payload)) => (Some(pending), Some(payload)),
            None => (None, None),
        };
        RuntimeTeardown {
            settled,
            pending,
            payload: settled_payload.or(pending_payload),
        }
    }
}

/// What one teardown found: the settled runtime, the assembly still in flight,
/// and the payload whichever of them registered.
///
/// A struct rather than a tuple because the interesting case is the one where
/// `settled` is `None` and `pending` is not — the mid-assembly sign-out — and a
/// tuple's second element is exactly the thing a call site forgets.
pub struct RuntimeTeardown<T> {
    /// The installed runtime, if the assembly had landed.
    pub settled: Option<AccountStoreHandle>,
    /// The assembly claimed but not yet settled — a store that may already be
    /// **open** on its own OS thread while `settled` reads `None`. Hand both to
    /// [`stop_account_runtime`].
    pub pending: Option<PendingAssembly>,
    /// This host's teardown payload — the tokio [`Handle`] on linux, `()`
    /// where there is nothing to carry. `None` only when there was nothing to
    /// stop at all.
    ///
    /// [`Handle`]: https://docs.rs/tokio/latest/tokio/runtime/struct.Handle.html
    pub payload: Option<T>,
}

impl<T> RuntimeTeardown<T> {
    /// Is there anything at all to stop? `false` is the common teardown — a
    /// sign-out with no account runtime ever assembled.
    pub fn is_empty(&self) -> bool {
        self.settled.is_none() && self.pending.is_none()
    }
}

impl<T> Default for AccountRuntimeHost<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Mint the store principal's own nest client. Its connect retry is the
/// caller's to spawn, once the runtime that gates it exists.
///
/// Split out so the degrade path above reads as one `match` — and so the
/// hex-decode and the slot read, which fail for entirely different reasons,
/// surface as one "not resolvable" verdict at the single call site that cares.
fn resolve_process_rpc(
    store_root: &StoreRoot,
    actor_id_hex: &str,
    nest_url: &str,
) -> anyhow::Result<Arc<NestClient>> {
    let actor_bytes =
        fauna_core::hex32::decode(actor_id_hex).map_err(|e| anyhow::anyhow!("actor id: {e}"))?;
    let writer_key =
        resolve_writer_key_serialized(store_root, actor_id_hex, &production_credential_store())?;
    Ok(
        fauna_client::ws_device_handshake_bearer::device_principal_nest_client(
            nest_url,
            actor_bytes,
            writer_key,
            // A second nest or a rebuilt box answers `not_registered`: the
            // latch is void, and the runtime's next pass re-registers.
            Some(fauna_sync_engine::principal_bundle::not_registered_voids_latch(actor_id_hex)),
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const ACTOR: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SECRET: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn inputs() -> AppRuntimeInputs {
        AppRuntimeInputs {
            actor_id_hex: ACTOR.to_string(),
            principal: ActorKeypair::from_secret_hex(SECRET)
                .expect("keypair")
                .into(),
            memberships: None,
            attested_predecessors: Default::default(),
            peer_transport: None,
            store_container: None,
        }
    }

    fn rpc() -> Arc<NestClient> {
        NestClient::new(
            "https://nest.example".to_string(),
            ActorKeypair::from_secret_hex(SECRET).expect("keypair"),
        )
    }

    /// The two fields phase 2 owns start **unset**, which is what keeps their
    /// I/O off the login path. An app that filled either one synchronously
    /// would put a credential-slot read (and, worse, a blocking IPC round trip
    /// to a possibly-wedged agent) in front of its own sign-in.
    #[test]
    fn the_io_bound_fields_are_left_for_phase_two() {
        let params = build_params(inputs(), rpc(), "https://nest.example");
        assert!(
            params.process_rpc.is_none(),
            "the store principal's bearer costs a credential-slot read; phase 2 owns it"
        );
        assert!(
            params.enrollment_target_device_id.is_empty(),
            "the enrollment target costs a DB/state-dir read; phase 2 owns it"
        );
    }

    /// The store root is the SHARED per-user root, never the app's own dir.
    ///
    /// Two roots publishing under one writer key is the journal-equivocation
    /// shape W6 exists to prevent, and it surfaces only after both have
    /// published — so this assertion goes red before the damage, which no
    /// integration test can do.
    #[test]
    fn the_store_root_is_the_shared_per_user_root() {
        let params = build_params(inputs(), rpc(), "https://nest.example");
        assert_eq!(
            params.store_root.base(),
            StoreRoot::platform().base(),
            "the unified per-user root every app and the agent open (W6)"
        );
        assert_ne!(
            params.store_root.base(),
            StoreRoot::at("/app/dir").base(),
            "an app's own state dir is never a store root"
        );
    }

    /// A sandboxed shell's container becomes the root verbatim — the
    /// `StoreRoot::at` half of the same two-constructor split.
    ///
    /// The mistake this goes red against is deriving the mobile root the
    /// desktop way: on iOS `platform()` takes the unix branch and resolves
    /// `$HOME/.config/fauna/sync` *inside the sandbox*, which looks fine (it
    /// is writable, and a store opens there) while being unreachable by the
    /// app extensions that share the container — so the failure is a silently
    /// unshared store, not an error.
    #[test]
    fn a_sandboxed_shell_roots_the_store_at_its_own_container() {
        let mut i = inputs();
        i.store_container = Some(SandboxedStoreContainer {
            dir: PathBuf::from("/containers/group.social.fauna.shared"),
            exclusion: CloudBackupExclusion::DeclaredInManifest {
                declaration: "test manifest".into(),
            },
        });
        let params = build_params(i, rpc(), "https://nest.example");
        assert_eq!(
            params.store_root.base(),
            Path::new("/containers/group.social.fauna.shared"),
            "the container the shell supplies IS the per-user root there"
        );
        // Still not the app's own state dir — the W6 rule is unchanged, only
        // the base it resolves from.
        assert_ne!(params.store_root.base(), Path::new("/app/dir"));
        // And the shell's own exclusion is the one the runtime applies — never
        // the desktop posture, which a sandboxed shell may not claim.
        assert!(
            matches!(
                &params.store_backup_exclusion,
                CloudBackupExclusion::DeclaredInManifest { declaration } if declaration == "test manifest"
            ),
            "the container's exclusion travels with it: {:?}",
            params.store_backup_exclusion
        );
    }

    /// A desktop host states the desktop posture — resolved by shared Rust,
    /// never by the shell — and on the three established desktops that is a
    /// named `NotApplicable`, so the runtime applies nothing and refuses
    /// nothing.
    #[test]
    fn a_desktop_host_carries_the_desktop_posture() {
        let params = build_params(inputs(), rpc(), "https://nest.example");
        assert!(
            matches!(
                &params.store_backup_exclusion,
                CloudBackupExclusion::NotApplicable { platform } if !platform.trim().is_empty()
            ),
            "the desktop posture names its platform: {:?}",
            params.store_backup_exclusion
        );
    }

    /// A seed-holding app is `SeedHolding`. The agent's host is the seedless
    /// one, and the two assemblies stay separate precisely so a change here
    /// can never reach it.
    #[test]
    fn an_app_assembles_a_seed_holding_host() {
        let params = build_params(inputs(), rpc(), "https://nest.example");
        assert!(matches!(params.principal, RuntimePrincipal::SeedHolding(_)));
    }

    /// A membership source passes through untouched, and its absence stays an
    /// absence — never an empty answer.
    ///
    /// `Some(vec![])` from this seam reads as *left every channel*, and scope
    /// departure deletes a departed scope's items, so a helpful "default to
    /// empty" here would be a data-loss bug rather than a stale walk.
    #[test]
    fn the_membership_source_passes_through_and_absence_stays_absence() {
        let params = build_params(inputs(), rpc(), "https://nest.example");
        assert!(
            params.memberships.is_none(),
            "an app with no conversations session cannot tell — it must not answer empty"
        );

        let mut i = inputs();
        i.memberships = Some(Arc::new(|| Some(vec![[9u8; 32]])));
        let params = build_params(i, rpc(), "https://nest.example");
        let source = params.memberships.expect("wired");
        assert_eq!(source(), Some(vec![[9u8; 32]]));
    }

    /// The peer leg is off unless the app wired a factory — hosting the
    /// runtime and taking the `fauna-iroh` dependency are separate tranches.
    #[test]
    fn the_peer_leg_is_structurally_off_without_a_factory() {
        let params = build_params(inputs(), rpc(), "https://nest.example");
        assert!(params.peer_transport.is_none());
    }

    // ── the install/teardown transitions ────────────────────────────────
    //
    // These drive `HostState` rather than the host, because the install half
    // needs a handle and a started `AccountStoreHandle` cannot be constructed
    // in a test (its fields are private to `fauna-sync-engine`). The state is
    // generic over the handle exactly so this half is reachable; the host is a
    // lock around it and one `.await` per shutdown.

    /// A stand-in for the started runtime. The transitions never call anything
    /// on it — they only move it between the registration, the slot and the
    /// teardown, which is the whole of what the lock protects.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeHandle(u8);

    fn claimed(state: &mut HostState<FakeHandle, ()>) -> (u64, AssemblySettle) {
        let (settle, pending) = assembly_channel();
        (state.begin(pending, ()), settle)
    }

    /// ⚠ **The state a teardown must never observe: nothing to stop while an
    /// install is still going to happen.**
    ///
    /// The defect this pins: with a lock per field, an
    /// install was three
    /// decisions — check the claim, remove the registration, write the slot —
    /// and a teardown landing between the last two found the registration gone
    /// and the slot empty. It reported `NothingToStop`, the erase ran, and the
    /// install completed behind it: the signed-out account's runtime installed
    /// and pumping against a store being erased. No `.await` sat in that gap,
    /// which is what made it read as safe; the teardown runs on another thread.
    ///
    /// The fix makes the install ONE transition, so the gap state is
    /// unrepresentable rather than unlikely. This walks the protocol and
    /// asserts that after every step, while the claim is current, the state
    /// still holds something for a teardown to take.
    #[test]
    fn a_current_claim_always_leaves_a_teardown_something_to_take() {
        let mut state = HostState::<FakeHandle, ()>::empty();
        let has_something_to_stop =
            |st: &HostState<FakeHandle, ()>| st.slot.is_some() || st.pending.is_some();

        let (claim, _settle) = claimed(&mut state);
        assert!(
            has_something_to_stop(&state),
            "the registration is what a mid-assembly teardown waits on"
        );

        let step = state.settle_install(claim, FakeHandle(1));
        assert!(
            matches!(step, InstallStep::Installed(None)),
            "the first install replaces nothing"
        );
        assert!(
            has_something_to_stop(&state),
            "the install moved the account from the registration to the slot in ONE \
             transition — a teardown sees one or the other, never neither"
        );
        assert!(
            state.pending.is_none() && state.slot.is_some(),
            "and it is the slot's turn now: {:?}",
            state.slot.map(|(h, ())| h)
        );
    }

    /// The two orders a teardown and an install can genuinely land in — and
    /// neither leaves a runtime installed behind a teardown that took nothing.
    #[test]
    fn a_teardown_either_takes_the_runtime_or_stops_it_being_installed() {
        // Teardown first: the claim is stale, so the assembly's handle is
        // handed back to be shut down and nothing is installed.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let (settled, pending) = state.tear_down(StopReason::AccountSwitch);
        assert!(settled.is_none(), "nothing had been installed yet");
        assert!(pending.is_some(), "but the assembly was in flight");
        assert!(
            matches!(
                state.settle_install(claim, FakeHandle(1)),
                InstallStep::Superseded(FakeHandle(1), _)
            ),
            "the signed-out account's runtime must be shut down, not installed"
        );
        assert!(state.slot.is_none(), "and it must not be in the slot");

        // Install first: the teardown takes the installed runtime, so the stop
        // has something to wait on and nothing stays behind.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let _ = state.settle_install(claim, FakeHandle(2));
        let (settled, pending) = state.tear_down(StopReason::AccountSwitch);
        assert_eq!(
            settled.map(|(h, ())| h),
            Some(FakeHandle(2)),
            "the teardown takes the runtime the install landed"
        );
        assert!(pending.is_none(), "the registration went when it installed");
        assert!(state.slot.is_none());
    }

    /// A live claim survives to its install, and a second `begin` invalidates
    /// the first — the account-switch case, where the older assembly must
    /// discard its runtime rather than install it under the newer login.
    ///
    /// Asserted through the install step itself rather than a predicate beside
    /// it: the check is now part of the one transition, and a mirror of it in
    /// the host could drift from the one the install actually gates on.
    #[test]
    fn a_newer_install_invalidates_the_older_claim() {
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (first, _first_settle) = claimed(&mut state);
        let (second, _second_settle) = claimed(&mut state);

        assert!(
            matches!(
                state.settle_install(first, FakeHandle(1)),
                InstallStep::Superseded(..)
            ),
            "the older assembly must shut its runtime down, not install it under the \
             newer login"
        );
        assert!(
            matches!(
                state.settle_install(second, FakeHandle(2)),
                InstallStep::Installed(_)
            ),
            "the newest claim is the one that installs"
        );
    }

    /// ⚠ **A runtime superseded by a SIGN-OUT is stopped the sign-out way** —
    /// its enrollment retired — and one superseded by anything else is not.
    ///
    /// The defect this pins, measured `--app linux` 2026-09-20: the prologue
    /// registers the machine's enrollment *before* the assembly settles, so a
    /// sign-out landing mid-assembly supersedes a runtime that already holds a
    /// registered grant. The superseded arm shut it down plainly, and that
    /// shutdown is queued the instant the handle is published — ahead of the
    /// waiting teardown's retirement, which then read `account runtime is shut
    /// down`. One stranded `sync_devices` row per such sign-out, to the tier's
    /// device cap.
    #[test]
    fn a_sign_out_superseding_an_assembly_has_its_runtime_retire_on_the_way_out() {
        // The sign-out lands mid-assembly: the late install must retire.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::SignOut);
        assert!(
            matches!(
                state.settle_install(claim, FakeHandle(1)),
                InstallStep::Superseded(FakeHandle(1), StopReason::SignOut)
            ),
            "the erase that follows a sign-out destroys this runtime's key, so the \
             enrollment it registered must be retired before it stops"
        );

        // ...and STILL must when the next sign-in has claimed before the old
        // assembly settles: the slot it enrolled under is just as erased.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (old, _old_settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::SignOut);
        let (new, _new_settle) = claimed(&mut state);
        assert!(matches!(
            state.settle_install(old, FakeHandle(1)),
            InstallStep::Superseded(_, StopReason::SignOut)
        ));
        assert!(
            matches!(
                state.settle_install(new, FakeHandle(2)),
                InstallStep::Installed(None)
            ),
            "the sign-in after the sign-out is not caught by it"
        );
        // Nor is that sign-in's own assembly, when a switch supersedes it: its
        // slot postdates the erase and survives the switch.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let _ = claimed(&mut state);
        let _ = state.tear_down(StopReason::SignOut);
        let (after, _after_settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::AccountSwitch);
        assert!(
            matches!(
                state.settle_install(after, FakeHandle(3)),
                InstallStep::Superseded(_, StopReason::AccountSwitch)
            ),
            "a sign-out marks the claims BEFORE it, never the sign-in after it"
        );

        // A switch keeps the slot: retiring here would turn the next sign-in as
        // this account into a removed-from-account state.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::AccountSwitch);
        assert!(matches!(
            state.settle_install(claim, FakeHandle(1)),
            InstallStep::Superseded(_, StopReason::AccountSwitch)
        ));

        // So does a newer login replacing an older assembly with no teardown.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (first, _first_settle) = claimed(&mut state);
        let (_second, _second_settle) = claimed(&mut state);
        assert!(matches!(
            state.settle_install(first, FakeHandle(1)),
            InstallStep::Superseded(_, StopReason::AccountSwitch)
        ));
    }

    /// ⚠ **A teardown advances the generation even with an EMPTY slot**, and
    /// that is the case the guard exists for.
    ///
    /// A sign-out landing *during the very first assembly* has no handle to
    /// take — so an implementation that only bumped when it took something
    /// would leave this claim current, and the assembly would install the
    /// signed-out account's runtime seconds later, with the app showing a
    /// logged-out UI over a live pump. There is no louder failure downstream:
    /// the runtime is healthy, it is just serving an account nobody is signed
    /// in to.
    #[test]
    fn a_teardown_invalidates_an_in_flight_claim_even_with_nothing_installed() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        let _claim = host.begin(());
        assert!(
            host.take(StopReason::AccountSwitch).settled.is_none(),
            "nothing installed yet"
        );

        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::AccountSwitch);
        assert_ne!(
            state.generation, claim,
            "the teardown must advance the generation itself — asserted here rather \
             than only through the install below, which would refuse this claim \
             anyway on the fail-closed arm (its registration is gone) and so would \
             pass even if the generation had never moved"
        );
        assert!(
            matches!(
                state.settle_install(claim, FakeHandle(1)),
                InstallStep::Superseded(..)
            ),
            "a sign-out during the first assembly must still supersede it"
        );
    }

    /// ⚠ **The same teardown must also come away with something to WAIT on**,
    /// which invalidation alone does not give it.
    ///
    /// Invalidating the claim stops the signed-out account from being *served*;
    /// it does nothing about the store the assembly already has **open on its
    /// own OS thread**, and the erase that follows a sign-out is synchronous.
    /// That gap is the measured `os error 32` (`PendingAssembly`'s docs), so
    /// this pins the half the generation guard structurally cannot cover.
    #[test]
    fn a_teardown_during_the_first_assembly_comes_away_with_the_in_flight_half() {
        let host: AccountRuntimeHost<u8> = AccountRuntimeHost::new();
        let _claim = host.begin(7);
        let teardown = host.take(StopReason::AccountSwitch);
        assert!(teardown.settled.is_none(), "nothing installed yet");
        assert!(
            teardown.pending.is_some(),
            "a sign-out mid-assembly must be able to wait for the store the \
             assembly already opened — an empty slot is not 'nothing to stop'"
        );
        assert!(!teardown.is_empty());
        assert_eq!(
            teardown.payload,
            Some(7),
            "the payload a teardown needs most is the one whose slot is empty: \
             on linux it is the runtime handle the wait is driven on"
        );
    }

    /// A second teardown finds the seam emptied — the wait is handed over once,
    /// not re-served to whoever asks next.
    #[test]
    fn the_in_flight_half_is_handed_over_exactly_once() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        let _claim = host.begin(());
        assert!(host.take(StopReason::AccountSwitch).pending.is_some());
        let second = host.take(StopReason::AccountSwitch);
        assert!(second.pending.is_none());
        assert!(second.is_empty());
    }

    /// A failed assembly — the task drops its claim without ever calling
    /// `finish` — must resolve the wait AT ONCE, not at the budget.
    ///
    /// The budget here is deliberately enormous: the only way this test fails
    /// is by hanging, which is the exact defect (a teardown that spends five
    /// seconds waiting for an assembly that died) and cannot be reached by a
    /// slow machine.
    #[tokio::test]
    async fn a_failed_assembly_is_not_waited_on() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        let claim = host.begin(());
        let teardown = host.take(StopReason::AccountSwitch);
        drop(claim); // the assembly returned `Err` and its task ended

        assert_eq!(
            stop_account_runtime(
                teardown.settled,
                teardown.pending,
                std::time::Duration::from_secs(3600),
                StopReason::SignOut,
            )
            .await,
            StopOutcome::NothingToStop,
        );
    }

    /// The assembly is genuinely **awaited**: the stop does not resolve while
    /// the assembly is still in flight.
    ///
    /// The barrier is causal, not a sleep (e2e convention 14): on a
    /// current-thread runtime a `yield_now` runs the spawned stop to its await
    /// point, and nothing in the process can complete it until this test
    /// settles the assembly — so `is_finished()` is a fact, not a race.
    #[tokio::test]
    async fn an_assembly_still_in_flight_is_awaited() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        let claim = host.begin(());
        let teardown = host.take(StopReason::AccountSwitch);
        let stop = tokio::spawn(stop_account_runtime(
            teardown.settled,
            teardown.pending,
            std::time::Duration::from_secs(3600),
            StopReason::SignOut,
        ));
        tokio::task::yield_now().await;
        assert!(
            !stop.is_finished(),
            "a teardown that returns while the assembly still holds the store \
             is the whole defect: the erase then runs against an open file"
        );

        claim.1.expect("the claim carries the settle").failed();
        assert_eq!(
            stop.await.expect("the stop task"),
            StopOutcome::NothingToStop
        );
    }

    /// The budget elapses **loudly** rather than waiting forever: an assembly
    /// that never settles must not hold a sign-out open.
    ///
    /// Latency-independent in the direction that matters — the claim is held
    /// for the whole test, so the wait can only ever time out; a slow machine
    /// cannot make this pass spuriously.
    #[tokio::test]
    async fn the_budget_elapses_rather_than_holding_the_sign_out_open() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        let claim = host.begin(());
        let teardown = host.take(StopReason::AccountSwitch);

        assert_eq!(
            stop_account_runtime(
                teardown.settled,
                teardown.pending,
                std::time::Duration::from_millis(10),
                StopReason::SignOut,
            )
            .await,
            StopOutcome::BudgetElapsed,
            "the user asked to be signed out; the erase proceeds and says so"
        );
        drop(claim);
    }

    /// A host that never assembled anything reports nothing to stop — the
    /// common sign-out, and the one where a spurious wait would be pure
    /// latency.
    #[tokio::test]
    async fn a_host_that_never_assembled_has_nothing_to_stop() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        let teardown = host.take(StopReason::AccountSwitch);
        assert!(teardown.is_empty());
        assert_eq!(
            stop_account_runtime(
                teardown.settled,
                teardown.pending,
                ACCOUNT_RUNTIME_STOP_BUDGET,
                StopReason::SignOut,
            )
            .await,
            StopOutcome::NothingToStop,
        );
    }

    /// An untouched host serves no handle, and a teardown on one is a no-op
    /// rather than a panic — every host's teardown runs on paths where no
    /// runtime was ever assembled (a failed assembly, a sign-out before login
    /// completed).
    #[test]
    fn an_empty_host_serves_nothing_and_tears_down_cleanly() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        assert!(host.handle().is_none());
        assert!(host.take(StopReason::AccountSwitch).is_empty());
        assert!(host.handle().is_none());
    }

    /// The reconnect wake is the pump's push-reset signal; without it a pump
    /// falls back to the backstop cadence alone, which is a latency
    /// regression no test downstream would name.
    #[test]
    fn the_session_wakes_attach_together() {
        let params = build_params(inputs(), rpc(), "https://nest.example");
        assert!(
            params.reconnects.is_none() && params.pushes.is_none(),
            "unset until the app attaches them"
        );
        let (_tx, rx) = tokio::sync::watch::channel(0u64);
        let (_push_tx, push_rx) = tokio::sync::broadcast::channel(1);
        let params = with_session_wakes(params, rx, push_rx);
        assert!(params.reconnects.is_some());
        assert!(
            params.pushes.is_some(),
            "the push→nudge arm is armed by the same call as the reconnect wake"
        );
    }

    // ── device_set_state_json ─────────────────────────────────

    /// No handle (no account runtime for this actor) is a plain `found:
    /// false` — never a panic, and never a network attempt.
    #[tokio::test]
    async fn device_set_state_with_no_handle_reports_not_found() {
        assert_eq!(
            device_set_state_json(None, "aa".repeat(32).as_str()).await,
            serde_json::json!({ "found": false })
        );
    }

    /// The JSON every dispatcher hands the e2e harness is the shared view's
    /// serialization — pinned here because `helpers/fleet.py` reads these
    /// exact keys on every app (the decode itself is tested beside the reader,
    /// in `fauna-account-plane`).
    #[test]
    fn the_device_set_view_serializes_to_the_harness_shape() {
        use fauna_account_plane::account_driver::e2e_readers::DeviceSetStateView;
        let removed_by = fauna_core::hex32::encode(&[7u8; 32]);
        let removed = DeviceSetStateView {
            found: true,
            state: Some("removed"),
            removed_at_ms: Some(42),
            removed_by: Some(removed_by.clone()),
            enrolled_at_ms: None,
        };
        assert_eq!(
            serde_json::to_value(removed).unwrap(),
            serde_json::json!({
                "found": true,
                "state": "removed",
                "removed_at_ms": 42,
                "removed_by": removed_by,
            })
        );
        let enrolled = DeviceSetStateView {
            found: true,
            state: Some("enrolled"),
            removed_at_ms: None,
            removed_by: None,
            enrolled_at_ms: Some(7),
        };
        assert_eq!(
            serde_json::to_value(enrolled).unwrap(),
            serde_json::json!({ "found": true, "state": "enrolled", "enrolled_at_ms": 7 })
        );
    }

    /// The two ordering rules a spawned stop owes what follows it: nothing
    /// queued runs while any stop is in flight — a second teardown that found
    /// nothing to stop included — and what is queued runs oldest first.
    ///
    /// Red against the per-teardown reading ("run my continuation once MY stop
    /// ends"): the second continuation below has no stop of its own and would
    /// run at once, erasing underneath the first teardown's still-open store.
    #[test]
    fn the_stop_queue_holds_every_continuation_until_the_last_stop_ends() {
        let mut q = StopQueue::<&str>::default();
        assert!(!q.is_busy(), "a fresh queue is idle");

        // Nothing stopping: a teardown's continuation is released at once.
        q.push("inline");
        assert_eq!(q.next_ready(), Some("inline"));
        assert!(!q.is_busy());

        q.stop_started();
        q.push("first erase");
        // A second teardown landing mid-stop, with nothing of its own to stop.
        q.push("second erase");
        assert!(q.is_busy());
        assert_eq!(q.next_ready(), None, "a continuation ran mid-stop");

        // Two stops in flight: the first to finish releases nothing.
        q.stop_started();
        q.stop_finished();
        assert_eq!(q.next_ready(), None, "one stop is still in flight");

        q.stop_finished();
        assert_eq!(q.next_ready(), Some("first erase"));
        assert_eq!(q.next_ready(), Some("second erase"));
        assert_eq!(q.next_ready(), None);
        assert!(!q.is_busy());

        // A stray completion never underflows into "stopping forever".
        q.stop_finished();
        assert!(!q.is_busy());
    }
}
