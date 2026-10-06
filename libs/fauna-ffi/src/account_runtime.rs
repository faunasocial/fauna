//! The W3 (account-data-plane.md § Workstreams) account-store runtime, **hosted by the UniFFI apps** (windows,
//! macOS, iOS) — `account-data-plane.md` § The account store → *The
//! client-side lifecycle*, and the third consumer of the shared assembly
//! `fauna-client-account-runtime` after tui and linux (priority #2).
//!
//! # Why these apps host one, when the agent already does
//!
//! The same reason linux does, and the reasoning is copied from
//! `apps/fauna-linux/src/account_runtime.rs` deliberately rather than
//! restated: `fauna-sync-agent`'s account host (W5.5b) is the **app-dead
//! backstop** — seedless, MLS-free, `memberships: None` — and the W5.1
//! election (`flock` on the store's `engine.lock`, taken inside
//! `AccountStoreRuntime::start`) arbitrates between them. Two things follow
//! that the agent structurally cannot reach: the **member half of the
//! content-scope set** (joined `__conv` channels live in an MLS engine the
//! bearer-only agent does not link) and **MLS-sealed outbox intents** (the T9
//! carve-out holds them for a process hosting the conversations engine).
//!
//! iOS is the one target with no resident agent at all
//! (`apps/sync-agent.md` § Scope per platform), so there hosting in-process
//! is not merely better — it is the only host the account plane gets.
//!
//! # Why this is a separate call and not folded into `conversations_session`
//!
//! The conversations factory already carries three of the four app-owned
//! inputs (the actor secret, this device's id, and the session the membership
//! source reads), so folding the assembly into it is tempting and would even
//! remove an ordering contract. It is still wrong: an app with **no**
//! conversations session must still host an account runtime — `memberships:
//! None` is a *supported* wiring, not a degraded one — and coupling the
//! runtime's life to the conversations session would silently deny the
//! account plane to exactly those surfaces. The two planes are independent;
//! only the membership *read* crosses between them.
//!
//! What the app supplies is therefore the one thing no shared code can
//! derive: its own data dir (and, on a sandboxed shell, its container). The
//! `index_lease_device` precedent next door (`crate::index_launch`) is the
//! same shape and the same reason — app-owned state a factory is handed
//! nothing to derive.
//!
//! # Teardown, and the supersession guard
//!
//! Assembly does real I/O (a credential-slot read, a store open, an IPC round
//! trip to the co-located agent), so it is spawned — and a sign-out or account
//! switch can land *while it runs*. The shared host's generation counter is what
//! makes that safe: [`install`] and [`teardown`] both bump it, and a task whose
//! generation has moved on shuts its freshly-started runtime down instead of
//! installing it. Without that, a switch leaves the previous account's runtime
//! pumping under the new session — the shape `sync-agent.md` § Control plane
//! split calls "a signed-out account still being served".
//!
//! **That guard stops the account being SERVED; it does not stop the store
//! being OPEN, and [`teardown`] owes both.** A superseded assembly closes its
//! runtime whenever it eventually finishes, while the erase each shell runs next
//! is immediate — so the teardown also waits for an assembly in flight, under a
//! shared bounded budget (`apps/account-scoping.md` § Erasure follows scope).
//!
//! A plain **quit** deliberately does not come through [`teardown`]: the
//! process is ending, the handle drops, and (on the two platforms that have
//! one) the agent takes the pump role over. Only a sign-out/switch/reset needs
//! the deterministic shutdown, because there the process lives on and must
//! stop writing as the old account.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_account_runtime::{
    ACCOUNT_RUNTIME_STOP_BUDGET, AccountRuntimeHost, AppRuntimeInputs, InstallOutcome,
    ResolveContext, SandboxedStoreContainer, StopReason, build_params, resolve_and_start,
    stop_account_runtime, with_session_wakes,
};
use fauna_sync_engine::account_runtime::{AccountStoreHandle, MembershipSource};

/// This process's account-runtime host — the slot and the generation guard,
/// both from the shared lifecycle so linux, tui and these three apps cannot
/// drift on a guard whose failure is silent.
///
/// One host per process rather than one per [`crate::FfiNestClient`], matching
/// linux and tui: the W5.1 election is per *store*, so a second host in one
/// process would contend with the first for a role this process already holds —
/// and every UniFFI app is single-login-per-process anyway.
///
/// No payload: unlike linux, whose teardown is called from the GTK main thread
/// and must carry the tokio handle its wait is driven on, every caller here is
/// already async and awaits the stop directly.
static HOST: AccountRuntimeHost = AccountRuntimeHost::new();

/// What the app brings that no shared code can derive.
pub(crate) struct HostInputs {
    /// A sandboxed shell's container, when the per-app container IS the
    /// per-user root (iOS, android) — paired with how the shell keeps it out
    /// of the platform's cloud backup. `None` on windows and macOS, where the
    /// shared assembly resolves the per-OS constant and the desktop posture
    /// itself.
    pub(crate) store_container: Option<SandboxedStoreContainer>,
    /// This machine's stable sync device id, hex — the **same** id
    /// `index_lease_device` seats at the advisory `index` lease and the
    /// Devices page rosters. `None` (a profile that has never registered one)
    /// fails the runtime start: the machine's named row is the one enrollment
    /// target (`sync-agent-credentials.md` § Credential model, the RULED
    /// 2026-09-28 block, decision 3).
    pub(crate) own_device_id_hex: Option<String>,
    /// The app's account registry, from which this seat resolves the
    /// account's **attested** succeeded-from identities IN RUST, off this
    /// session's own actor: their ids are the generation machinery's
    /// fleet-view `prior` (`account-data-taxonomy.md` § The generation
    /// machinery → *The source of `prior`*, ruled 2026-09-13) and their
    /// delegable schedules are what the walk carries a predecessor's rows
    /// under (`succession-aftermath.md` § Re-key scope). The app hands over
    /// the registry OBJECT, never ids or key bytes: the schedules derive from
    /// seeds, which must not cross a per-app FFI call, and resolving both
    /// halves off one walk here is what keeps them from disagreeing. `None`
    /// (a shell with no registry wired — a unit-test seat) attests nothing,
    /// which is fail-safe: a successor's predecessor-signed enrollments drop
    /// out of this device's fleet view and no predecessor row is carried;
    /// nothing is admitted.
    pub(crate) accounts: Option<Arc<crate::accounts_registry::FfiAccountRegistry>>,
    /// The post-store-ready aftermath pass's app-supplied halves (the sink and
    /// the registry the post-auth `run_succession_aftermath` call stashes) —
    /// the installed arm runs the pass when both are already there.
    #[cfg(feature = "recovery-aftermath")]
    pub(crate) ledger_pass: crate::succession_aftermath::LedgerPassSeams,
}

/// Post-auth hook: assemble and start the account-store runtime for the
/// signed-in account. Returns as soon as the spawn lands — every I/O-bound
/// step is inside phase 2, off the caller's login path.
///
/// Best-effort by construction, exactly as on linux: a failed assembly leaves
/// every store-backed surface answering "the account runtime is not running"
/// and the account's own scopes walked by the agent alone (or, on iOS,
/// unwalked). It never fails a sign-in.
///
/// **Requires a tokio context** — the callers are `async_runtime = "tokio"`
/// UniFFI exports, so `tokio::spawn` is available; there is no fallback
/// runtime on purpose, because a runtime this module owned would be a second
/// executor whose lifetime no app controls.
pub(crate) fn install(
    nest: Arc<NestClient>,
    inputs: HostInputs,
    memberships: MembershipSource,
    sessions: crate::caldav_client::SchedulingSessionHolder,
) -> Result<(), crate::FfiError> {
    let Some(keypair) = nest.auth().keypair() else {
        return Err(crate::FfiError::General {
            msg: "account runtime: this client holds no identity seed — nothing to assemble".into(),
        });
    };
    let keypair = fauna_core::identity::ActorKeypair::from_secret(*keypair.secret_bytes());
    let actor_id_hex = keypair.actor_id().to_hex();
    #[cfg(feature = "offline-share")]
    let actor = keypair.actor_id();

    let nest_url = nest.nest_url();
    let reconnects = nest.subscribe_reconnects();
    let pushes = nest.subscribe_pushes();

    let HostInputs {
        store_container,
        own_device_id_hex,
        accounts,
        #[cfg(feature = "recovery-aftermath")]
        ledger_pass,
    } = inputs;
    // One registry walk, off THIS session's own actor (never the registry's
    // "active" account — a bound launch may differ), read here on the
    // caller's thread before the spawn. `HostInputs::accounts` owns why it is
    // resolved in Rust.
    let attested_predecessors = accounts.as_ref().map_or_else(
        fauna_client_account_runtime::AttestedPredecessors::none,
        |accounts| {
            fauna_client_account_runtime::AttestedPredecessors::from_registry(
                accounts.registry(),
                &actor_id_hex,
            )
        },
    );
    // The same walk's seeds, beside this identity's own: what the escrow
    // recovery opens a succession's kept wrap under (`SeedHolder`).
    let principal = match accounts.as_ref() {
        Some(accounts) => {
            fauna_client_account_runtime::SeedHolder::from_registry(keypair, accounts.registry())
        }
        None => keypair.into(),
    };
    #[cfg(feature = "recovery-aftermath")]
    let ledger_nest = Arc::clone(&nest);
    #[cfg(feature = "deployment-seed")]
    let custody_nest = Arc::clone(&nest);

    // Claimed HERE, before the spawn: a claim taken inside the task would leave
    // a window in which a teardown bumps nothing and the assembly installs the
    // signed-out account's runtime anyway. The claim also registers the assembly
    // as in flight, so a sign-out landing before it settles can WAIT for the
    // store it has already opened rather than erase around it — the same door
    // windows, macOS, iOS and android all reach through `stop_account_runtime`
    // below.
    let claim = HOST.begin(());
    tokio::spawn(async move {
        let params = build_params(
            AppRuntimeInputs {
                actor_id_hex,
                principal,
                memberships: Some(memberships),
                // The registry walk above (`HostInputs::accounts`). Empty for
                // an identity that never succeeded, which is fail-safe.
                attested_predecessors,
                // The peer leg (W5.7) is a separate tranche: it needs the
                // `fauna-iroh` dependency, which only tui carries today.
                peer_transport: None,
                store_container,
            },
            Arc::clone(&nest),
            &nest_url,
        );
        let params = with_session_wakes(params, reconnects, pushes);

        let started = resolve_and_start(
            params,
            ResolveContext {
                nest_url,
                // Already resolved by the app (it owns the device row), so
                // phase 2's blocking task has nothing to read here — the
                // closure shape is the crate's contract, not a cost.
                own_device_id_hex: Box::new(move || own_device_id_hex),
            },
        )
        .await;

        match started {
            // The shared lifecycle owns both the supersession decision and the
            // shutdowns it implies (a superseded fresh runtime, or the previous
            // one this replaces) — forgetting either is invisible, which is why
            // it is not a call-site responsibility.
            Ok(handle) => match HOST.finish(claim, handle).await {
                InstallOutcome::Installed => {
                    tracing::info!("account runtime: mounted the account store for this session");
                    on_store_installed(&sessions);
                    // The succession ledger's post-store-ready pass — one of
                    // its two edges; the other is the post-auth aftermath
                    // call, whichever lands second runs it
                    // (`crate::succession_aftermath::spawn_ledger_pass`).
                    #[cfg(feature = "recovery-aftermath")]
                    crate::succession_aftermath::spawn_ledger_pass(ledger_nest, &ledger_pass);
                    // The deployment-seed custody leg's store-ready edge —
                    // the other is the app's post-auth
                    // `self_heal_deployment_seed_custody` call, which answers
                    // "retried" while no handle exists; whichever lands second
                    // runs the leg (`box-recovery.md` § The plane-era recovery
                    // floor, *(c) The writes*). `nest` is the authenticated
                    // connection this runtime was assembled over. The seat has
                    // no app warning surface, so an owed warning is logged; the
                    // app's next post-auth edge re-runs the leg and surfaces it.
                    #[cfg(feature = "deployment-seed")]
                    if let Some(handle) = HOST.handle() {
                        tokio::spawn(async move {
                            if let Some(warning) =
                                fauna_client_account_runtime::deployment_seeds::run_custody_leg(
                                    &custody_nest,
                                    &handle,
                                )
                                .await
                            {
                                tracing::warn!(
                                    "deployment-seed custody leg (store-ready edge): {warning}"
                                );
                            }
                        });
                    }
                    // The offline-share seat's ceremony record is lent at the
                    // runtime's start — the panel may have bound long before it
                    // (`p2p.md` § Offline share initiation → *The seat's record
                    // is lent late*); the lease ends with the runtime, which
                    // the teardown that empties the seat stops.
                    #[cfg(feature = "offline-share")]
                    if let Some(handle) = HOST.handle() {
                        fauna_sync_engine::offline_share::lend_account_record(
                            &crate::offline_share::session_seat_for(actor),
                            handle,
                        );
                    }
                }
                InstallOutcome::Superseded => tracing::info!(
                    "account runtime: superseded during assembly; shut the fresh runtime \
                     down instead of installing it"
                ),
            },
            Err(e) => tracing::warn!(
                "account runtime: assembly failed; every store-backed surface answers \
                 not-running and this account's own scopes go unwalked by this process: {e:#}"
            ),
        }
    });
    Ok(())
}

/// The live handle, if the assembly has landed. `None` before the assembly
/// completes, after a sign-out, or whenever it failed.
pub(crate) fn handle() -> Option<AccountStoreHandle> {
    HOST.handle()
}

/// [`handle`] as the source a store-backed surface waits on — what the
/// preference façades (`muted_keywords`, `sync_prefs`, `personalization`,
/// `task_delegation`) hand the shared `preference_surfaces`: a gesture made
/// before the assembly lands waits for it, and fails if none comes.
#[cfg_attr(
    not(any(
        feature = "feed-manager",
        feature = "muted-keywords",
        feature = "sync-prefs",
        feature = "personalization",
        feature = "task-delegation",
        feature = "conversations-session"
    )),
    allow(dead_code)
)]
pub(crate) fn handle_source() -> fauna_sync_engine::account_runtime::SeatAccountStore {
    fauna_sync_engine::account_runtime::SeatAccountStore::new(std::sync::Arc::new(handle))
}

/// The period-key custody (`fauna.state.subscriptions`) of whichever account
/// runtime is live — resolved at every call, so a surface built before the
/// assembly lands reads through once it does: a read meets "not running" (never
/// an empty custody) until then, a write waits for the assembly
/// (`fauna_account_seams::period_keys`).
#[cfg_attr(
    not(any(
        feature = "feed-manager",
        feature = "subscriptions-author",
        feature = "pairing"
    )),
    allow(dead_code)
)]
pub(crate) fn period_key_store() -> fauna_client_subscriptions::SharedPeriodKeyStore {
    std::sync::Arc::new(fauna_client_account_runtime::period_keys::PlanePeriodKeys::new(handle))
}

/// The folder-key custody (`fauna.state.folder-keys`) of whichever account
/// runtime is live — resolved at every call, as [`period_key_store`]: a read
/// meets "not running" (custody unreadable — a bound set builds keyless, never
/// plaintext) until the assembly lands, a write waits for it
/// (`fauna_account_seams::folder_keys`).
pub(crate) fn folder_key_store() -> std::sync::Arc<dyn fauna_client_folders::FolderKeyStore> {
    std::sync::Arc::new(fauna_client_account_runtime::folder_keys::PlaneFolderKeys::new(handle))
}

/// The account's mail custody (`fauna.state.mail`) over this seat's runtime —
/// what every mail-keyed machine the FFI builds reads and writes the MSEK and
/// the credentials through, waiting for the runtime when it is not up yet.
pub(crate) fn mail_store() -> std::sync::Arc<dyn fauna_client_config::MailStore> {
    std::sync::Arc::new(fauna_sync_engine::account_runtime::AccountMailStore::new(
        std::sync::Arc::new(handle),
    ))
}

/// The runtime the installed store's assembly ran on — where the conversations
/// seams start their tasks when the pair completes from the session edge,
/// which is a synchronous foreign call with no runtime of its own.
static STORE_RUNTIME: std::sync::Mutex<Option<tokio::runtime::Handle>> =
    std::sync::Mutex::new(None);

/// The store edge, run once a store is installed (on the runtime its assembly
/// ran on): start the store-change relay to the app's listener
/// (`crate::store_change` — it ends with this runtime), record that runtime,
/// then wire the conversations seams if the session is already stashed — the
/// other edge is the stash itself ([`wire_conversation_seams`]).
fn on_store_installed(sessions: &crate::caldav_client::SchedulingSessionHolder) {
    if let Some(store) = handle() {
        tokio::spawn(crate::store_change::relay(store));
    }
    *STORE_RUNTIME.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(tokio::runtime::Handle::current());
    // Before the session's seams: what they register is this store's.
    #[cfg(feature = "contact-overlays")]
    overlay_seam::store_changed();
    #[cfg(feature = "conversations-session")]
    wire_conversation_seams(sessions);
    #[cfg(not(feature = "conversations-session"))]
    let _ = sessions;
    #[cfg(feature = "contact-overlays")]
    overlay_seam::on_store_installed();
}

/// Register the conversations seams that rest on the account store — the
/// shared `conversation_seams::wire`, the same call tui and linux make — once
/// **both** a conversations session is stashed in `sessions` and a store is
/// installed (`conversation-read-state.md` § The read-marker record → *How the
/// manager reaches the plane*; `conversation-rooms.md` § The three classes →
/// *Community*).
///
/// The two land in either order ([`membership_source`]'s contract), so this
/// is called from **both** edges: [`install`]'s `Installed` arm and the
/// session stash. Each edge publishes its half under its own lock before
/// reading the other's, so at least one of them sees both halves; both may,
/// and wiring a pair twice is harmless (`conversation_seams::wire`'s docs). A
/// no-op while either half is missing.
#[cfg(feature = "conversations-session")]
pub(crate) fn wire_conversation_seams(sessions: &crate::caldav_client::SchedulingSessionHolder) {
    let Some(session) = sessions.lock().ok().and_then(|held| held.clone()) else {
        return;
    };
    let Some(store) = handle() else {
        return;
    };
    let Some(rt) = STORE_RUNTIME
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
        return;
    };
    fauna_client_account_runtime::conversation_seams::wire(&session, store, &rt);
    #[cfg(feature = "contact-overlays")]
    overlay_seam::wired_by_session(&session.manager());
}

/// The **private contact overlay** seam for a manager that no conversations
/// session serves (`docs/goal/ui/contacts.md` § The private overlay).
///
/// [`wire_conversation_seams`] registers the overlay seam with its siblings,
/// and needs a stashed session to do it. A seat can hold a manager and a
/// store with no session between them — every native app under plain e2e
/// (the mock rails stand in for the real ones), and android before its login
/// swaps the session's manager in — and there the watcher never started: the
/// saving process painted its own Save, and a relaunch or a sibling device's
/// edit painted nothing. The overlay needs no rail, only the store, so the
/// seat registers it on whichever manager the overlay face
/// (`crate::contact_overlays`) was last built over, from both edges: the
/// face's construction and the store's install.
///
/// One manager, one registration per store: the seat remembers what it wired
/// and under which store, so an app that builds the face on every read
/// registers once. A manager the session edge wired is recorded here too and
/// is never registered a second time.
#[cfg(feature = "contact-overlays")]
pub(crate) mod overlay_seam {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, Weak};

    use fauna_conversations::ConversationsManager;

    struct Seat {
        manager: Weak<ConversationsManager>,
        /// `(store epoch, the manager's overlay registration)` as they stood
        /// once the seam was registered; `None` while no store was there to
        /// register over. Either half moving means the registration is gone:
        /// a new store was installed, or the manager retired the seam at an
        /// identity change.
        wired: Option<(u64, u64)>,
    }

    static SEAT: Mutex<Option<Seat>> = Mutex::new(None);

    /// Moves at every store install, so a seam registered over the previous
    /// store is registered again over this one.
    static STORE_EPOCH: AtomicU64 = AtomicU64::new(0);

    fn seat() -> std::sync::MutexGuard<'static, Option<Seat>> {
        SEAT.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The face edge: `manager` is the one the app reads names from now.
    /// Registers the overlay seam on it unless this store already serves it.
    pub(crate) fn ensure(manager: &Arc<ConversationsManager>) {
        let mut seat = seat();
        let epoch = STORE_EPOCH.load(Ordering::Relaxed);
        let current = seat.as_ref().is_some_and(|s| {
            std::ptr::eq(s.manager.as_ptr(), Arc::as_ptr(manager))
                && s.wired == Some((epoch, manager.contact_overlays_generation()))
        });
        if current {
            return;
        }
        let wired = super::handle().and_then(|store| {
            let rt = super::STORE_RUNTIME
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()?;
            fauna_client_account_runtime::contact_overlays::register(manager, store, &rt);
            Some((epoch, manager.contact_overlays_generation()))
        });
        *seat = Some(Seat {
            manager: Arc::downgrade(manager),
            wired,
        });
    }

    /// A store was installed: every registration made over the one before it
    /// is stale.
    pub(super) fn store_changed() {
        STORE_EPOCH.fetch_add(1, Ordering::Relaxed);
    }

    /// The store edge, after the session's seams were wired: a new store
    /// serves whichever manager the face was last built over.
    pub(super) fn on_store_installed() {
        let manager = seat().as_ref().and_then(|s| s.manager.upgrade());
        if let Some(manager) = manager {
            ensure(&manager);
        }
    }

    /// The session edge registered the overlay seam on `manager` with its
    /// siblings (`conversation_seams::wire`): record it, so the face does not
    /// register a second one.
    #[cfg(feature = "conversations-session")]
    pub(super) fn wired_by_session(manager: &Arc<ConversationsManager>) {
        *seat() = Some(Seat {
            manager: Arc::downgrade(manager),
            wired: Some((
                STORE_EPOCH.load(Ordering::Relaxed),
                manager.contact_overlays_generation(),
            )),
        });
    }
}

/// Teardown (sign-out / account-switch / factory-reset): stop the runtime
/// deterministically, so this process stops writing as the old account before
/// the next one signs in. A plain quit does NOT come here — see the module
/// docs.
///
/// Complete by the time it returns: the FFI callers are already async, so the
/// sign-out *awaits* the stop rather than racing it — which is what makes the
/// erase each shell runs next (`account_state_erase_all_scopes` /
/// `account_state_erase_scope`) meet a closed store.
///
/// ⚠ **The wait covers an assembly still in flight, not only a settled store.**
/// `AccountStoreRuntime::start` opens the database on its own OS thread before
/// returning, so a sign-out landing mid-assembly finds an empty slot while a
/// live connection holds `account-store.db`; erasing there fails with
/// `os error 32` on windows and silently leaves a stranded store elsewhere
/// (`apps/account-scoping.md` § Erasure follows scope). The wait is bounded and
/// says so when it lapses — the erase proceeds regardless, because the user
/// asked to be signed out.
///
/// Every shell reaches this through `stop_account_runtime` — the same door
/// linux and tui use — so windows, macOS, iOS and android inherit the wait with
/// no per-shell code. What each shell still owns is the **ordering**: this call
/// must precede its `account_state_erase_*`.
pub(crate) async fn teardown(reason: StopReason) {
    // `take` advances the generation even when the slot is empty — the case
    // that matters, since a sign-out landing during the first assembly has
    // nothing to take and everything to prevent — and hands back the in-flight
    // assembly to wait for, which invalidating the claim alone does not.
    let stopping = HOST.take(reason);
    // The session's ceremony seat ends with the session: the next sign-in
    // binds a fresh listener, as tui's `sign_out` and linux's actor reset do.
    #[cfg(feature = "offline-share")]
    crate::offline_share::reset_session_seat();
    #[cfg(feature = "p2p-share")]
    crate::share_plane::forget();
    // `shutdown` drains the pump's in-flight pass before the store closes,
    // which is what keeps a mid-pass sign-out from leaving the outbox
    // half-drained; the shared stop does that for both halves under one budget.
    // `reason` is the shell's own statement of whether the credential erase
    // follows (`StopReason`): a sign-out retires the machine's enrollment
    // nest-side on the way out, a switch leaves it enrolled.
    stop_account_runtime(
        stopping.settled,
        stopping.pending,
        ACCOUNT_RUNTIME_STOP_BUDGET,
        reason,
    )
    .await;
}

/// The member half of the content-scope set, read off whatever
/// [`ConversationsSession`] this client has stashed.
///
/// **The holder is read on every invocation rather than snapshotted**, and
/// both halves of that are the seam's contract rather than style:
///
/// 1. **No session stashed answers `None`** — *cannot tell right now*, never
///    *left every channel*. An affirmative `Some(vec![])` from an app whose
///    session has not landed yet is indistinguishable from a genuine
///    departure, and scope departure **deletes** a departed scope's items — so
///    the convenient default here is a data-loss bug, not a stale walk
///    (`account-data-plane.md` § Implementation status today → *Built — W3
///    scope departure*).
/// 2. **Nothing is cached** — the pump calls this once per pass on purpose,
///    and that is what makes a join or a leave take effect with no
///    notification path, no `register_content_scope` call and no nudge.
///
/// The holder is the same late-populated stash `scheduling_session` uses, so
/// an account runtime started **before** the conversations session simply
/// answers "cannot tell" until the session lands and then starts answering —
/// which is why the two calls need no ordering contract between them.
///
/// [`ConversationsSession`]: fauna_conversations::session::ConversationsSession
#[cfg(feature = "conversations-session")]
pub(crate) fn membership_source(
    holder: crate::caldav_client::SchedulingSessionHolder,
) -> MembershipSource {
    Arc::new(move || {
        let session = holder.lock().ok()?.clone()?;
        Some(session.joined_conv_channels())
    })
}

/// A build with no conversations rail cannot tell, ever — the same `None` the
/// desktop apps answer before their session lands, not an empty answer.
#[cfg(not(feature = "conversations-session"))]
pub(crate) fn membership_source(
    _holder: crate::caldav_client::SchedulingSessionHolder,
) -> MembershipSource {
    Arc::new(|| None)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A turn on this process's one [`HOST`] with nothing installed — what a
    /// test anywhere in the crate holds for its whole body when it reads
    /// [`handle`] as absent. Without the turn it races the tests below that
    /// install a store, and sees theirs.
    pub(crate) async fn host_with_no_runtime() -> tokio::sync::MutexGuard<'static, ()> {
        let turn = HOST_TESTS.lock().await;
        teardown(StopReason::AccountSwitch).await;
        turn
    }

    /// With no session stashed the source answers **`None`**.
    ///
    /// The mutation this goes red against is the tempting one — mapping the
    /// absent session to an empty vec, "so the pump gets a definite answer".
    /// That answer means *this account left every channel*, and the departure
    /// pass acts on it by deleting every joined channel's content from this
    /// device. There is no louder failure downstream to catch it: the walk
    /// simply stops and the rows go.
    #[test]
    fn no_session_cannot_tell_rather_than_answering_empty() {
        let holder: crate::caldav_client::SchedulingSessionHolder = Arc::new(Mutex::new(None));
        let source = membership_source(holder);
        assert_eq!(
            source(),
            None,
            "an app with no stashed session must answer `cannot tell`, never `left every \
             channel`"
        );
    }

    /// Teardown clears the slot, so a post-sign-out reader cannot be handed
    /// the outgoing account's store handle — the in-memory half of the switch
    /// isolation contract.
    #[tokio::test]
    async fn teardown_leaves_no_handle_behind() {
        let _host = HOST_TESTS.lock().await;
        teardown(StopReason::AccountSwitch).await;
        assert!(handle().is_none());
    }

    /// The tests that install into, or tear down, this process's one [`HOST`]
    /// take turns.
    static HOST_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[cfg(feature = "conversations-session")]
    mod conversation_seams {
        use std::path::Path;
        use std::time::Duration;

        use fauna_conversations::message::{MessageBadges, MessageId, MessageSnapshot};
        use fauna_conversations::store::history::ChannelHistorySlice;
        use fauna_conversations::thread::{ThreadFlavor, ThreadId};
        use fauna_conversations::{ConversationsManager, ConversationsSession};
        use fauna_sync_engine::account_runtime::{
            AccountRuntimeParams, AccountStoreRuntime, CRED_NAMESPACE, CloudBackupExclusion,
            RuntimePrincipal, StoreRoot,
        };

        use super::*;

        const CHANNEL: &str = "ffeeffeeffeeffeeffeeffeeffeeffeeffeeffeeffeeffeeffeeffeeffeeffee";

        /// Every nest call fails as a transport fault: the store needs no nest
        /// to be installed and to serve its (empty) read markers.
        #[derive(Clone)]
        struct NoNest;

        #[derive(Debug)]
        struct Offline;

        impl std::fmt::Display for Offline {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("no nest in this test")
            }
        }

        impl fauna_protocol::RpcErrorClass for Offline {
            fn is_rejection(&self) -> bool {
                false
            }
        }

        impl fauna_protocol::RpcRequester for NoNest {
            type Error = Offline;

            async fn request<Req, Reply>(
                &self,
                _kind: &'static str,
                _p: Req,
            ) -> Result<Reply, Offline>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                Err(Offline)
            }
        }

        impl fauna_protocol::KeyedRpcRequester for NoNest {
            async fn request_keyed<Req, Reply>(
                &self,
                _kind: &'static str,
                _key: [u8; 16],
                _p: Req,
            ) -> Result<Reply, Offline>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                Err(Offline)
            }
        }

        fn root() -> fauna_core::identity::ActorKeypair {
            fauna_core::identity::ActorKeypair::from_secret([0x3C; 32])
        }

        pub(super) async fn a_store(base: &Path) -> AccountStoreHandle {
            AccountStoreRuntime::start(AccountRuntimeParams {
                store_backup_exclusion: CloudBackupExclusion::NotApplicable {
                    platform: "test".into(),
                },
                store_root: StoreRoot::at(base.join("state")),
                actor_id_hex: root().actor_id_hex(),
                rpc: NoNest,
                process_rpc: None,
                principal: RuntimePrincipal::SeedHolding(root().into()),
                credentials: fauna_credential_store::CredentialStore::with_file_backend(
                    CRED_NAMESPACE,
                    base.join("creds"),
                ),
                reconnects: None,
                pushes: None,
                backstop_interval: Duration::from_secs(3600),
                memberships: None,
                trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(
                    Vec::new(),
                ),
                attested_predecessors: Default::default(),
                linked_nests: None,
                owed_nests: None,
                peer_transport: None,
                // The machine's named row — never read here (no nest answers enrollment).
                enrollment_target_device_id: "ab".repeat(32),
            })
            .await
            .expect("the store starts with no nest")
        }

        /// A session over a manager holding one native channel whose three
        /// messages predate the launch floor — so they are unread only once a
        /// registered seam has delivered positions (none stored: position 0).
        fn a_session() -> (Arc<ConversationsSession>, ThreadId) {
            let manager = ConversationsManager::new();
            let message = |seq: u64| MessageSnapshot {
                message_id: MessageId(format!("conv:{CHANNEL}:{seq}")),
                sender: fauna_conversations::address::TypedAddress::Email {
                    email_address: "peer@host.test".into(),
                },
                sender_display: String::new(),
                body: String::new(),
                document: fauna_core::render::RenderDocument::default(),
                timestamp_ms: seq as i64,
                subject_line: None,
                badges: MessageBadges::default(),
                reply_to: None,
                reactions: vec![],
                deleted: false,
                is_own: false,
                legal_takedown_ref: None,
                labels: vec![],
                plane_ref: None,
                can_delete: false,
            };
            let thread = manager.restore_channel_slice(&ChannelHistorySlice {
                channel_id_hex: CHANNEL.to_string(),
                label: "peer".to_string(),
                flavor: ThreadFlavor::OneToOne,
                participants: vec![],
                messages: (1..=3).map(message).collect(),
                ..Default::default()
            });
            let engine = Arc::new(
                fauna_mls::engine::MlsEngine::new_in_memory(root()).expect("in-memory engine"),
            );
            let self_actor = engine.identity_actor_id();
            let session = ConversationsSession::from_manager(
                manager,
                engine,
                Arc::new(fauna_conversations::backends::mock::InertConversationsRpc),
                "me@host.test".to_string(),
                self_actor,
                None,
            );
            (session, thread)
        }

        fn unread(session: &ConversationsSession, thread: &ThreadId) -> u32 {
            session
                .manager()
                .snapshot()
                .threads
                .iter()
                .find(|t| &t.thread_id == thread)
                .expect("the channel's thread")
                .unread_count
        }

        /// Deadline-poll for the seam's first delivery — one generous budget
        /// a green run pays a tick of (convention 14).
        async fn delivered(session: &ConversationsSession, thread: &ThreadId) -> bool {
            tokio::time::timeout(Duration::from_secs(30), async {
                while unread(session, thread) != 3 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .is_ok()
        }

        /// The store edge exactly as [`install`] runs it.
        pub(super) async fn install_store(
            store: AccountStoreHandle,
            sessions: &SchedulingSessionHolder,
        ) {
            let claim = HOST.begin(());
            assert!(matches!(
                HOST.finish(claim, store).await,
                InstallOutcome::Installed
            ));
            on_store_installed(sessions);
        }

        /// The session edge exactly as `FfiNestClient`'s session factory runs
        /// it: stash, then wire.
        fn stash_session(sessions: &SchedulingSessionHolder, session: &Arc<ConversationsSession>) {
            *sessions.lock().unwrap() = Some(Arc::clone(session));
            wire_conversation_seams(sessions);
        }

        type SchedulingSessionHolder = crate::caldav_client::SchedulingSessionHolder;

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_session_stashed_after_the_store_is_installed_gets_the_seams() {
            let _host = HOST_TESTS.lock().await;
            teardown(StopReason::AccountSwitch).await;
            let tmp = tempfile::tempdir().unwrap();
            let sessions: SchedulingSessionHolder = Arc::new(Mutex::new(None));

            install_store(a_store(tmp.path()).await, &sessions).await;
            let (session, thread) = a_session();
            assert_eq!(unread(&session, &thread), 0, "no seam: the floor decides");
            stash_session(&sessions, &session);
            assert!(
                delivered(&session, &thread).await,
                "the session edge wired the read-position seam"
            );
            teardown(StopReason::AccountSwitch).await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_store_installed_after_the_session_is_stashed_gets_the_seams() {
            let _host = HOST_TESTS.lock().await;
            teardown(StopReason::AccountSwitch).await;
            let tmp = tempfile::tempdir().unwrap();
            let sessions: SchedulingSessionHolder = Arc::new(Mutex::new(None));

            let (session, thread) = a_session();
            stash_session(&sessions, &session);
            assert_eq!(unread(&session, &thread), 0, "no store yet: nothing wired");
            install_store(a_store(tmp.path()).await, &sessions).await;
            assert!(
                delivered(&session, &thread).await,
                "the store edge wired the read-position seam"
            );
            teardown(StopReason::AccountSwitch).await;
        }
    }

    /// The overlay seam of a seat with no conversations session
    /// ([`overlay_seam`]): the manager the face was built over is registered
    /// once per store, from whichever edge completes the pair.
    #[cfg(all(feature = "conversations-session", feature = "contact-overlays"))]
    mod overlay_seam_edges {
        use fauna_conversations::ConversationsManager;

        use super::conversation_seams::{a_store, install_store};
        use super::*;

        type SchedulingSessionHolder = crate::caldav_client::SchedulingSessionHolder;

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_bare_manager_is_registered_when_the_store_lands_and_only_once() {
            let _host = HOST_TESTS.lock().await;
            teardown(StopReason::AccountSwitch).await;
            let tmp = tempfile::tempdir().unwrap();
            let sessions: SchedulingSessionHolder = Arc::new(Mutex::new(None));

            let manager = ConversationsManager::new();
            let unregistered = manager.contact_overlays_generation();
            overlay_seam::ensure(&manager);
            assert_eq!(
                manager.contact_overlays_generation(),
                unregistered,
                "no store yet: nothing to register over"
            );

            install_store(a_store(tmp.path()).await, &sessions).await;
            let registered = manager.contact_overlays_generation();
            assert_ne!(
                registered, unregistered,
                "the store edge registered the overlay seam with no session stashed"
            );

            overlay_seam::ensure(&manager);
            assert_eq!(
                manager.contact_overlays_generation(),
                registered,
                "a face built on every read registers nothing more"
            );
            teardown(StopReason::AccountSwitch).await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_face_built_after_the_store_registers_and_a_new_store_registers_again() {
            let _host = HOST_TESTS.lock().await;
            teardown(StopReason::AccountSwitch).await;
            let tmp = tempfile::tempdir().unwrap();
            let sessions: SchedulingSessionHolder = Arc::new(Mutex::new(None));

            install_store(a_store(&tmp.path().join("a")).await, &sessions).await;
            let manager = ConversationsManager::new();
            let unregistered = manager.contact_overlays_generation();
            overlay_seam::ensure(&manager);
            let registered = manager.contact_overlays_generation();
            assert_ne!(registered, unregistered, "the face edge registered it");

            // An identity change retires the seam; the next read registers
            // the incoming account's.
            manager.clear_for_identity_change();
            let retired = manager.contact_overlays_generation();
            overlay_seam::ensure(&manager);
            assert_ne!(manager.contact_overlays_generation(), retired);

            // A new store with no identity change on the manager (a seat that
            // signed out and in again) is registered over as well.
            teardown(StopReason::AccountSwitch).await;
            let before = manager.contact_overlays_generation();
            install_store(a_store(&tmp.path().join("b")).await, &sessions).await;
            assert_ne!(manager.contact_overlays_generation(), before);
            teardown(StopReason::AccountSwitch).await;
        }
    }

    /// The store edge's relay to the app's store-change listener
    /// (`crate::store_change`), over a real store.
    #[cfg(feature = "conversations-session")]
    mod store_change_relay {
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        use super::conversation_seams::{a_store, install_store};
        use super::*;
        use crate::store_change::tests::{Counting, SLOT_TESTS};

        /// A real store's change reaches the registered listener through the
        /// edge `install` runs, with no nest. Red if the store edge stops
        /// spawning the relay, or the relay stops calling the listener.
        ///
        /// Which source wakes it is not pinned here (the watch's own proofs
        /// do that, `conformance_account_runtime.rs`): the holder's first
        /// runs usually move its change generation at once, and the sibling's
        /// commits are the floor source behind it — it keeps committing until
        /// the listener hears, because a commit landing before the relay
        /// seeded its floor reading is (correctly) not a change. One generous
        /// budget (convention 14).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_store_change_reaches_the_registered_listener() {
            let _host = HOST_TESTS.lock().await;
            let _slot = SLOT_TESTS.lock().await;
            teardown(StopReason::AccountSwitch).await;
            let tmp = tempfile::tempdir().unwrap();
            let sessions: crate::caldav_client::SchedulingSessionHolder =
                Arc::new(Mutex::new(None));

            let heard = Arc::new(Counting::default());
            crate::store_change::set_store_change_listener(Some(heard.clone()));
            install_store(a_store(tmp.path()).await, &sessions).await;

            let sibling = a_store(tmp.path()).await;
            let delivered = tokio::time::timeout(Duration::from_secs(120), async {
                let mut n = 0u8;
                while heard.0.load(Ordering::SeqCst) == 0 {
                    sibling
                        .put_preference(fauna_protocol::merge_policy::KIND_MODERATION, vec![n])
                        .await
                        .expect("put on the sibling");
                    n = n.wrapping_add(1);
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            })
            .await
            .is_ok();
            assert!(
                delivered,
                "no store change ever reached the registered store-change listener"
            );

            sibling.shutdown().await;
            teardown(StopReason::AccountSwitch).await;
            crate::store_change::set_store_change_listener(None);
        }
    }

    // The supersession guard's own semantics — that a teardown invalidates an
    // in-flight claim, empty slot included — are pinned in the shared
    // lifecycle (`fauna_client_account_runtime`'s
    // `a_teardown_invalidates_an_in_flight_claim_even_with_nothing_installed`
    // and `a_newer_install_invalidates_the_older_claim`). That is the point of
    // hosting on it rather than re-deriving it here: one guard, one set of
    // pins, five hosts.
}
