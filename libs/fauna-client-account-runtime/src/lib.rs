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

#[cfg(unix)]
pub mod conversations_door;
pub mod deployment_seeds;
pub mod holder_yield;
pub mod p2p_participation;
pub mod removed_account;
pub use removed_account::{
    EnrollmentRetirement, REMOVED_ACCOUNT_RETIRE_BUDGET, RemovedAccountInputs,
    retire_removed_account,
};
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
pub use fauna_account_plane::departed_fleet::record_departed_fleet;
pub use fauna_account_plane::owed_delivery::PredecessorSeeds;
use fauna_anon_client::AnonymousNestClient;
use fauna_client::NestClient;
use fauna_client_core::succession_delivery::{OwedNestReach, SignIn, deliver_owed_nests};
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
pub use fauna_sync_engine::account_runtime::CloudBackupExclusion;
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, DEFAULT_BACKSTOP_INTERVAL,
    LinkedNestConnector, MembershipSource, OwedNestDeliverer, RuntimePrincipal, StoreRoot,
    merge_reconnect_watches, production_credential_store, resolve_writer_key_serialized,
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
/// Fails only where the desktop root does: a **desktop** host (no container)
/// resolves [`StoreRoot::platform`], which refuses a process with no absolute
/// home (the shared no-home refusal) — the caller handles it exactly as a
/// failed assembly, every store-backed surface answering not-running. The
/// desktop backup posture stays infallible here:
/// [`CloudBackupExclusion::platform_desktop`] on a target with no established
/// posture is a shell arm that refuses at assembly — a phone shell forgetting
/// its container is the wiring bug it makes loud, at the one place every
/// consumer already handles a failed exclusion.
pub fn build_params(
    inputs: AppRuntimeInputs,
    rpc: Arc<NestClient>,
    nest_url: &str,
) -> anyhow::Result<AccountRuntimeParams<Arc<NestClient>>> {
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
            StoreRoot::platform()?,
            CloudBackupExclusion::platform_desktop(),
        ),
    };

    Ok(AccountRuntimeParams {
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
    })
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
        sign_in_with_seed(url, keypair).await
    }
}

/// Sign in to `url` as the identity whose seed is `keypair`, over a fresh
/// [`NestClient`] of its own — no app session, no registry. The road's
/// sign-in as a retired identity the device holds the seed of, and a
/// remove-account's sign-in as the account it just removed
/// ([`retire_removed_account`]): both reach a nest as an identity no live
/// session of this app is signed in as.
pub async fn sign_in_with_seed(url: &str, keypair: ActorKeypair) -> SignIn<Arc<NestClient>> {
    let client = NestClient::new(url.to_string(), keypair);
    match client.connect().await {
        Ok(()) => SignIn::Connected(client),
        // A refused mint flattens to "couldn't obtain a token"; the typed
        // refusal is the auth client's last one, which tells "the nest holds
        // no account for it" from "no nest answered".
        Err(e) => match client.auth().last_auth_refusal() {
            Some(refusal) => SignIn::from_error(&fauna_client::NestClientError::Rpc(refusal)),
            None => SignIn::from_error(&e),
        },
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
///
/// The session's watch only: when the data path rides the store principal's
/// own client, [`resolve_and_start`] merges that client's reconnect watch in
/// itself, so no host passes it.
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
    params: AccountRuntimeParams<Arc<NestClient>>,
    ctx: ResolveContext,
) -> anyhow::Result<AccountStoreHandle> {
    let handle = assemble(params, ctx, production_credential_store()).await?;
    // The yield's nudge: on a desktop whose agent mounts beside this runtime,
    // the role this runtime hands over is taken by the agent now, not at its
    // backstop tick (`holder_yield`). Ends with the runtime.
    holder_yield::spawn_holder_yield_nudge(
        handle.subscribe_engine_role(),
        p2p_participation::EngineHolderNudgeSlot::seat(),
    );
    // And beside the agent, a key this runtime's seed pass recovers from
    // escrow asks the agent for the pass that re-presents the rows it opens
    // (`holder_yield::spawn_holder_pass_owed_nudge`). Ends with the runtime.
    holder_yield::spawn_holder_pass_owed_nudge(
        handle.subscribe_holder_pass_owed(),
        p2p_participation::EngineHolderNudgeSlot::seat(),
    );
    // The awaited walk's reach: a non-holder's `walk_fleet` (the mail
    // refresh's walk leg) is walked by the holder this slot reaches — the
    // agent, once its provisioner is published (`account-runtime.md`
    // § Multi-instance concurrency → *A non-holder's awaited fleet walk
    // reaches the holder*). A process with no agent reaches none and keeps
    // the role refusal.
    handle.seat_holder_walk(Arc::new(p2p_participation::EngineHolderNudgeSlot::seat()));
    Ok(handle)
}

/// Phase 2's assembly, shared by the live seat ([`resolve_and_start`], which
/// adds the agent nudges a serving runtime owes) and a remove-account's nest
/// half ([`retire_removed_account`], which stops the runtime at once and owes
/// the agent nothing). Every slot it reads or writes is `params.credentials`'
/// — the store principal's writer key included — and `latch_credentials` is
/// that same namespace again, for the principal client's `not_registered`
/// hook to void the latch in (a [`CredentialStore`] is not `Clone`).
async fn assemble(
    mut params: AccountRuntimeParams<Arc<NestClient>>,
    ctx: ResolveContext,
    latch_credentials: CredentialStore,
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
    let device_client = match resolve_process_rpc(
        &params.store_root,
        &actor_id_hex,
        &ctx.nest_url,
        &params.credentials,
        latch_credentials,
    ) {
        Ok(device_client) => {
            tracing::debug!(
                "store principal: the account runtime's data path rides the PRINCIPAL's own \
                 client (its first connect waits for the grant — see `spawn_connect_retry`)"
            );
            params.process_rpc = Some(Arc::clone(&device_client));
            // The data path's own client drops and reconnects apart from the
            // session's, so its reconnect wakes the pump too — merged here,
            // so no host wires it (`merge_reconnect_watches`).
            let data_reconnects = device_client.subscribe_reconnects();
            params.reconnects = Some(match params.reconnects.take() {
                Some(session) => merge_reconnect_watches(session, data_reconnects),
                None => data_reconnects,
            });
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

    // The data path's own connection is one of this device's live connections
    // too, so it announces the same device the app session announces
    // (`common.md` § Registration → *Every connection announces*): one silent
    // connection would quiet every relay row of the actor — the user's other
    // devices — for as long as the app runs (§ Dispatch Logic, the
    // compatibility rider). The machine's own id is the id the app's push row
    // and presence use. Set before the first connect, which reads the slot.
    if let Some(device_client) = &device_client {
        device_client
            .set_push_presence(params.enrollment_target_device_id.clone())
            .await;
    }

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
/// read-marker record). The reader itself is
/// [`fauna_account_plane::account_driver::e2e_readers::read_marker_state`], in
/// the wasm-capable plane crate so web's dispatcher answers the same command
/// over the same code (priority #2); this is its JSON value at the old path
/// every native dispatcher calls. An on-demand reader, gated and dispatched
/// exactly like [`device_set_state_json`] (async store I/O, so never on the
/// per-tick state path; real account content, so convention 15's rule (a)).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub async fn read_marker_state_json(
    handle: Option<&AccountStoreHandle>,
    channel_id_hex: &str,
) -> serde_json::Value {
    let view =
        fauna_account_plane::account_driver::e2e_readers::read_marker_state(handle, channel_id_hex)
            .await;
    serde_json::to_value(view).unwrap_or_else(|_| serde_json::json!({ "found": false }))
}

// The stop's budget, its reason and the one stop per handle live beside the
// driver (`fauna_account_plane::account_driver`'s `stop` module) since web
// hosts the runtime too and states the same reason; re-exported here, where
// every native host has always named them.
use fauna_account_plane::account_driver::stop_one;
pub use fauna_account_plane::account_driver::{ACCOUNT_RUNTIME_STOP_BUDGET, StopReason};
// The install/teardown lifecycle lives beside the driver
// (`fauna_account_plane::account_driver`'s `host` module) since the browser
// tab hosts the runtime through the same claim and generation guard;
// re-exported here at the paths every native host has always named.
pub use fauna_account_plane::account_driver::{
    AccountRuntimeHost, AssemblySettle, InstallClaim, InstallOutcome, PendingAssembly,
    RuntimeTeardown, assembly_channel,
};

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

/// Mint the store principal's own nest client. Its connect retry is the
/// caller's to spawn, once the runtime that gates it exists.
///
/// Split out so the degrade path above reads as one `match` — and so the
/// hex-decode and the slot read, which fail for entirely different reasons,
/// surface as one "not resolvable" verdict at the single call site that cares.
///
/// The writer key is read from `credentials` — the runtime's own
/// `params.credentials`, never a namespace of its own choosing: over a
/// remove-account's snapshot, a read of the production slot would find it
/// already deleted and mint a fresh key into the registry's namespace, the
/// ⚠ *async writer* case `account-scoping.md` § Erasure follows scope refuses.
/// The client's per-mint read, load-only, goes through `latch_credentials` —
/// that same namespace again, so it never names a slot of its own choosing
/// either.
fn resolve_process_rpc(
    store_root: &StoreRoot,
    actor_id_hex: &str,
    nest_url: &str,
    credentials: &CredentialStore,
    latch_credentials: CredentialStore,
) -> anyhow::Result<Arc<NestClient>> {
    let actor_bytes =
        fauna_core::hex32::decode(actor_id_hex).map_err(|e| anyhow::anyhow!("actor id: {e}"))?;
    // Resolved here for the mint a first sign-in owes and the early "not
    // resolvable" verdict — but never captured: the assembly may rotate this
    // writer before its first pass (a key loaded over a store with no stamped
    // writer, a lost slot, a burnt journal, a succession), and the enrollment
    // pass then registers the SUCCESSOR's grant. So the client reads the slot
    // at every mint (`PrincipalKeySource`), load-only, and signs with whatever
    // writer the machine has now.
    resolve_writer_key_serialized(store_root, actor_id_hex, credentials)?;
    let slot = Arc::new(latch_credentials);
    let writer_key = {
        let slot = Arc::clone(&slot);
        let actor_id_hex = actor_id_hex.to_string();
        fauna_client::ws_device_handshake_bearer::PrincipalKeySource::reading(move || {
            fauna_sync_engine::principal_bundle::load_writer_key(&slot, &actor_id_hex)
        })
    };
    Ok(
        fauna_client::ws_device_handshake_bearer::device_principal_nest_client(
            nest_url,
            actor_bytes,
            writer_key,
            // A second nest or a rebuilt box answers `not_registered`: the
            // latch is void, and the runtime's next pass re-registers.
            Some(
                fauna_sync_engine::principal_bundle::not_registered_voids_latch_in(
                    slot,
                    actor_id_hex,
                ),
            ),
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
        let params = build_params(inputs(), rpc(), "https://nest.example").unwrap();
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
        let params = build_params(inputs(), rpc(), "https://nest.example").unwrap();
        assert_eq!(
            params.store_root.base(),
            StoreRoot::platform().unwrap().base(),
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
        let params = build_params(i, rpc(), "https://nest.example").unwrap();
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
        let params = build_params(inputs(), rpc(), "https://nest.example").unwrap();
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
        let params = build_params(inputs(), rpc(), "https://nest.example").unwrap();
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
        let params = build_params(inputs(), rpc(), "https://nest.example").unwrap();
        assert!(
            params.memberships.is_none(),
            "an app with no conversations session cannot tell — it must not answer empty"
        );

        let mut i = inputs();
        i.memberships = Some(Arc::new(|| Some(vec![[9u8; 32]])));
        let params = build_params(i, rpc(), "https://nest.example").unwrap();
        let source = params.memberships.expect("wired");
        assert_eq!(source(), Some(vec![[9u8; 32]]));
    }

    /// The peer leg is off unless the app wired a factory — hosting the
    /// runtime and taking the `fauna-iroh` dependency are separate tranches.
    #[test]
    fn the_peer_leg_is_structurally_off_without_a_factory() {
        let params = build_params(inputs(), rpc(), "https://nest.example").unwrap();
        assert!(params.peer_transport.is_none());
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

        // The assembly failed: its task drops the claim, whose settle half
        // then reads as `failed`.
        drop(claim);
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

    /// The reconnect wake is the pump's push-reset signal; without it a pump
    /// falls back to the backstop cadence alone, which is a latency
    /// regression no test downstream would name.
    #[test]
    fn the_session_wakes_attach_together() {
        let params = build_params(inputs(), rpc(), "https://nest.example").unwrap();
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
