//! The W3 (account-data-plane.md § Workstreams) account-store runtime, **hosted by this app** —
//! `account-data-plane.md` § The account store → *The client-side lifecycle*.
//!
//! # Why the app hosts one at all, when the agent already does
//!
//! Both host one, and that is the design. `fauna-sync-agent`'s account host
//! (W5.5b) is the **app-dead backstop**: seedless, MLS-free, and deliberately
//! passing `memberships: None`. The W5.1 election (`flock` on the store's
//! `engine.lock`, taken inside `AccountStoreRuntime::start`) arbitrates — a
//! running app holds the pump role and the agent comes up beside it as a plain
//! reader/writer, taking the role back when the app exits.
//!
//! Two things follow, and neither is reachable from the agent alone:
//!
//! * **The member half of the content-scope set.** Joined `__conv` channels are
//!   read off a live MLS engine, which the bearer-only agent structurally does
//!   not link. Only this process can answer, so only this process makes the
//!   account's conversation channels get walked.
//! * **MLS-sealed outbox intents.** The T9 carve-out holds them for a process
//!   hosting the conversations engine; the agent can only ever hold them.
//!
//! # Teardown, and the supersession guard
//!
//! Assembly does real I/O (a credential-slot read, a store open, an IPC round
//! trip to the co-located agent), so it is spawned — and a sign-out or account
//! switch can therefore land *while it runs*. The shared host's generation
//! counter is what makes that safe: [`install`] and [`teardown`] both bump it,
//! and a task whose generation has moved on shuts its freshly-started runtime
//! down instead of installing it. Without that, a switch would leave the
//! previous account's runtime pumping under the new session — the shape
//! `sync-agent.md` § Control plane split calls "a signed-out account still
//! being served".
//!
//! **Invalidating that claim is not the same as stopping the store, and
//! [`teardown`] owes both.** `AccountStoreRuntime::start` opens
//! `account-store.db` on its own OS thread *before* returning, so an assembly
//! whose claim has just been invalidated may still hold the database open for
//! however long its remaining steps take — while the sign-out's erase, which is
//! synchronous, runs straight past it. So the teardown WAITS, under the shared
//! bounded budget, and erases anyway (loudly) if that budget lapses
//! (`apps/account-scoping.md` § Erasure follows scope). POSIX `unlink` removes
//! an open file, which is why this has never bitten here and why the wait is a
//! convergence obligation rather than a bug fix on this app.
//!
//! A plain **quit** deliberately does not come through [`teardown`]: the process
//! is ending, the handle drops, and the agent takes the pump role over. Only a
//! sign-out/switch/reset needs the deterministic shutdown, because there the
//! process lives on and must stop writing as the old account.

use std::rc::Rc;
use std::sync::Arc;

use fauna_client_account_runtime::{
    ACCOUNT_RUNTIME_STOP_BUDGET, AccountRuntimeHost, AppRuntimeInputs, InstallOutcome,
    ResolveContext, StopQueue, StopReason, build_params, resolve_and_start, stop_account_runtime,
    with_session_wakes,
};
use fauna_core::identity::ActorKeypair;
use fauna_sync_engine::account_runtime::AccountStoreHandle;

use crate::client::FaunaClient;

/// This process's account-runtime host — the slot and the generation guard,
/// both from the shared lifecycle (`fauna_client_account_runtime`), so the five
/// hosts of this plane cannot drift on a guard whose failure is silent.
///
/// The payload is the tokio runtime handle the deterministic stop is driven on
/// — the client's, captured **at the claim** rather than at install, so
/// [`teardown`] never builds one on the GTK main thread (`async_helper` module
/// docs, second rule) *and* still has one when the sign-out lands mid-assembly,
/// where nothing is installed to carry it. That per-host need is exactly why the
/// shared lifecycle carries a payload rather than only a handle.
static HOST: AccountRuntimeHost<tokio::runtime::Handle> = AccountRuntimeHost::new();

/// Post-auth hook: assemble and start the account-store runtime for the
/// signed-in account. GTK main thread; all I/O is spawned.
///
/// Best-effort by construction — a failed assembly leaves every store-backed
/// surface answering "the account runtime is not running" and the account's
/// own scopes walked by the agent alone. It never fails a sign-in.
///
/// `share_seat` is this sign-in's seat slot from the Folders page's own
/// offline-share state: the share plane binds through it, so the plane and
/// the panel hold one actor-keyed endpoint between them.
pub fn install(
    fauna_client: &Rc<FaunaClient>,
    #[cfg(feature = "p2p-share")] share_seat: crate::offline_share::SessionSeat,
) {
    let Some(actor_id_hex) = fauna_client.actor_id() else {
        tracing::warn!("account runtime: no actor id — nothing to assemble");
        return;
    };
    let Ok(keypair) = ActorKeypair::from_secret_hex(fauna_client.secret_hex()) else {
        tracing::warn!("account runtime: malformed secret — nothing to assemble");
        return;
    };

    let rpc = Arc::clone(fauna_client.nest_rpc());
    let nest_url = rpc.nest_url();
    let reconnects = rpc.subscribe_reconnects();
    let pushes = rpc.subscribe_pushes();
    let rt = fauna_client.runtime_handle();

    let memberships = membership_source();

    // The fleet view's `prior`: the client's one cached registry resolution — the same
    // list `sync_agent::install` hands the agent — read HERE on the GTK thread
    // (the cache is a `RefCell`), never a writer-asserted list
    // (`account-data-taxonomy.md` § The generation machinery → *The source of
    // `prior`*).
    let attested_predecessors = fauna_client.attested_predecessors();
    // The same registry's seeds for those identities, beside this one's own:
    // what the escrow recovery opens a succession's kept wrap under
    // (`SeedHolder`).
    let principal = fauna_client_account_runtime::SeedHolder::from_registry(
        keypair,
        &crate::account_registry(),
    );

    // The share plane's seams, gathered HERE for the same reason the claim is:
    // the agent provisioner lives in a GTK-thread `thread_local`, so the task
    // below cannot reach for it. `None` is a legitimate degraded session (no
    // agent installed, no conversations session, no config dir) and leaves the
    // plane down — `crate::share_glue` module docs.
    // The same slot the store-ready edge lends the ceremony record to — kept
    // apart from the seed, which a degraded session never builds, since the
    // panel's ceremony runs with no share plane at all.
    #[cfg(feature = "p2p-share")]
    let lend_seat = share_seat.clone();
    #[cfg(feature = "p2p-share")]
    let share_seed = crate::share_glue::gather(fauna_client, share_seat);

    // The conversations session, gathered HERE for the same reason the share
    // seed is: it lives in a GTK-thread `thread_local` the task below cannot
    // reach. It carries the seams that rest on the store — the community
    // class's other read half (a room's generation wraps are addressed to
    // account-plane keypairs, `conversation-rooms.md` § The three classes →
    // *Community*) and the native rail's read positions
    // (`conversation-read-state.md` § The read-marker record). `None` is a
    // legitimate session (an engine-init refusal) and leaves community rooms
    // unopened and native threads on the launch floor.
    let conversations = crate::conversations::conv_backend::active_session();

    // The post-store-ready aftermath pass's two inputs this task cannot reach
    // for itself: the signed-in requester and the UI channel its review
    // re-read reports into (`crate::succession_aftermath::run_ledger`) — the
    // same two the deployment-seed custody leg takes at this edge.
    let ledger_nest = Arc::clone(fauna_client.nest_rpc());
    let ledger_tx = fauna_client.tx();
    // The channel the store-change watch posts its notice into
    // (`crate::store_surfaces::watch`).
    let store_change_tx = fauna_client.tx();

    // Claimed HERE, before the spawn: a claim taken inside the task would leave
    // a window in which a teardown bumps nothing and the assembly installs the
    // signed-out account's runtime anyway. The claim also registers the
    // assembly as in flight, which is what lets a sign-out landing before it
    // settles WAIT for the store it has already opened — and it carries the
    // runtime handle that wait must be driven on, which is why the payload is
    // handed over here rather than at `finish`: the teardown that needs it most
    // is the one that finds an empty slot.
    let claim = HOST.begin(rt.clone());
    rt.spawn(async move {
        let params = build_params(
            AppRuntimeInputs {
                actor_id_hex,
                principal,
                attested_predecessors,
                memberships: Some(memberships),
                // The peer leg (W5.7) is a separate tranche: it needs the
                // `fauna-iroh` dependency, which only tui carries today.
                peer_transport: None,
                // Desktop: the per-OS constant. Only a sandboxed mobile shell
                // supplies its own container — and with it, its cloud-backup
                // exclusion; the desktop posture is shared Rust's own.
                store_container: None,
            },
            rpc,
            &nest_url,
        );
        let params = with_session_wakes(params, reconnects, pushes);

        let started = resolve_and_start(
            params,
            ResolveContext {
                nest_url,
                // Which `sync_devices` row this machine is — resolved inside
                // phase 2 (it is a state-dir read) so it never reaches the
                // login path.
                own_device_id_hex: Box::new(|| crate::sync::device_id().ok().map(hex_encode)),
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
                    // The store-change notice: an open store-backed page
                    // re-reads whoever changed the store. Ends with this
                    // runtime (`account-runtime.md` § Multi-instance
                    // concurrency → *A runtime's own pump is a source of the
                    // notice too*).
                    if let Some(handle) = HOST.handle() {
                        tokio::spawn(crate::store_surfaces::watch(handle, store_change_tx));
                    }
                    // The succession ledger's post-store-ready pass: the
                    // member-item and filter-mark raises a parked ceremony
                    // owes, then the review surfaces' re-read
                    // (`fauna_client_recovery::ledger_aftermath`).
                    if let Some(handle) = HOST.handle() {
                        tokio::spawn(crate::succession_aftermath::run_ledger(
                            Arc::clone(&ledger_nest),
                            handle,
                            ledger_tx.clone(),
                        ));
                    }
                    // The deployment-seed custody leg's store-ready edge
                    // (`box-recovery.md` § The plane-era recovery floor → *(c)
                    // The writes*): the post-auth edge already landed (this
                    // install runs from it, on its authenticated client), so
                    // this edge is the second and runs the leg. Every later
                    // post-auth edge re-runs it through
                    // `FaunaClient::run_deployment_seed_custody_leg`.
                    if let Some(handle) = HOST.handle() {
                        tokio::spawn(crate::client::run_deployment_seed_custody_leg(
                            ledger_nest,
                            handle,
                            ledger_tx,
                        ));
                    }
                    // The conversations seams that rest on the store (a
                    // community room's wrap keypairs, the native rail's read
                    // positions) — the earliest honest moment, for the share
                    // plane's reason: what they serve lives behind the handle
                    // this arm just installed.
                    if let (Some(session), Some(handle)) = (conversations, HOST.handle()) {
                        fauna_client_account_runtime::conversation_seams::wire(
                            &session,
                            handle,
                            &tokio::runtime::Handle::current(),
                        );
                    }
                    // The share plane's start edge (`p2p-shared-set-build.md` § Cross-user
                    // shared-set transfer → *Built — the tui app leg*: bind at
                    // STORE-ready, not at post-auth). Every seam it pumps
                    // through — rule 7's cached brake evidence, the discovery
                    // cache's dial targets, the sink's durable write, the
                    // transfer ledger — lives behind the handle this arm just
                    // installed, so this is the earliest honest start.
                    // The offline-share seat's ceremony record is lent at the
                    // same edge — the panel may have bound long before it
                    // (`p2p.md` § Offline share initiation → *The seat's
                    // record is lent late*).
                    #[cfg(feature = "p2p-share")]
                    if let Some(handle) = HOST.handle() {
                        crate::offline_share::lend_account_record(&lend_seat, handle);
                    }
                    #[cfg(feature = "p2p-share")]
                    if let (Some(seed), Some(handle)) = (share_seed, HOST.handle()) {
                        crate::share_glue::start(seed, handle);
                    }
                }
                InstallOutcome::Superseded => tracing::info!(
                    "account runtime: superseded during assembly; shut the fresh \
                     runtime down instead of installing it"
                ),
            },
            Err(e) => tracing::warn!(
                "account runtime: assembly failed; every store-backed surface answers \
                 not-running and the agent walks this account's own scopes alone: {e:#}"
            ),
        }
    });
}

/// The live handle, if the assembly has landed. `None` before the assembly
/// completes, after a sign-out, or whenever it failed.
pub fn handle() -> Option<AccountStoreHandle> {
    HOST.handle()
}

/// [`handle`] as the source a store-backed surface waits on — what the
/// preference pages hand the shared `preference_surfaces`: a gesture made
/// before the assembly lands waits for it, and fails if none comes.
pub fn handle_source() -> fauna_sync_engine::account_runtime::SeatAccountStore {
    fauna_sync_engine::account_runtime::SeatAccountStore::new(std::sync::Arc::new(handle))
}

/// The succession-ledger seam for a machine built before the store is up (the
/// Nests page, the labeler catalog, every mail-settings machine): it resolves
/// [`handle`] at each call, waiting out an assembly still in flight
/// (`fauna_client_config::ResolvingLedgerStore`).
pub fn ledger_seam() -> std::sync::Arc<dyn fauna_client_config::SuccessionLedgerStore> {
    std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(handle))
}

/// The followed-folders seam (`fauna.state.follows`) the follow gestures and
/// the Devices + Media pages' followed-folders source read and write through —
/// resolved per call exactly as [`ledger_seam`].
pub fn follows_seam() -> std::sync::Arc<dyn fauna_client_config::FollowsStore> {
    std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(handle))
}

/// The backup-state seam (`fauna.state.backup`, the per-source-box destination
/// list and its unattested marks) — [`ledger_seam`]'s twin, built the same way:
/// it resolves [`handle`] at each call, waiting out an assembly still in flight,
/// then answers not-ready (`fauna_client_config::ResolvingLedgerStore`).
pub fn backup_seam() -> std::sync::Arc<dyn fauna_client_config::BackupStateStore> {
    std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(handle))
}

/// The period-key custody (`fauna.state.subscriptions`) of whichever account
/// runtime is live — resolved at every call, so a surface built before the
/// assembly lands reads through once it does: a read meets "not running" (never
/// an empty custody) until then, a write waits for the assembly
/// (`fauna_account_seams::period_keys`).
pub fn period_key_store() -> fauna_client_subscriptions::SharedPeriodKeyStore {
    std::sync::Arc::new(fauna_client_account_runtime::period_keys::PlanePeriodKeys::new(handle))
}

/// The folder-key custody (`fauna.state.folder-keys`) of whichever account
/// runtime is live — resolved at every call, as [`period_key_store`]: a read
/// meets "not running" (custody unreadable — a bound set builds keyless, never
/// plaintext) until the assembly lands, a write waits for it
/// (`fauna_account_seams::folder_keys`).
pub fn folder_key_store() -> std::sync::Arc<dyn fauna_client_folders::FolderKeyStore> {
    std::sync::Arc::new(fauna_client_account_runtime::folder_keys::PlaneFolderKeys::new(handle))
}

/// The account's mail custody (`fauna.state.mail`) over this seat's runtime —
/// what every mail-keyed machine the shell builds reads and writes the MSEK
/// and the credentials through, waiting for the runtime when it is not up yet.
pub fn mail_store() -> Arc<dyn fauna_client_config::MailStore> {
    Arc::new(fauna_sync_engine::account_runtime::AccountMailStore::new(
        Arc::new(handle),
    ))
}

/// The succession-ledger seam (`fauna_client_config::SuccessionLedgerStore`,
/// implemented on the handle) the two review surfaces read and write through,
/// or the sentence they show while the account store is not up yet.
pub fn ledger_store() -> Result<AccountStoreHandle, String> {
    handle().ok_or_else(|| "the account store is not ready yet".to_string())
}

/// Teardown (sign-out / account-switch / factory-reset / e2e-reset): stop the
/// runtime deterministically, so this process stops writing as the old account
/// before the next one signs in, then run `then` — the caller's erase, window
/// rebuild or e2e ack — on the GTK main loop once the stop has finished. A plain
/// quit does NOT come here — see the module docs.
///
/// **The stop is spawned; the continuation waits for it.** The erase that
/// follows a sign-out (`account_scope::erase_all_known_accounts`) is fully
/// synchronous, so a stop spawned with nothing sequenced behind it is a race
/// the erase wins: the sweep runs before the task has reached the assembly at
/// all (`apps/account-scoping.md` § Erasure follows scope: *a host must be able
/// to WAIT … and erase anyway (loudly)*). Until 2026-09-24 that ordering was
/// bought by blocking the GTK thread on the stop, which held the main loop for
/// the whole stop budget whenever the sign-out landed behind a prologue — the
/// window froze, and the e2e `reset` measured `kinds reset 2x5126ms`. Now the thread goes back to the main loop and
/// `then` is what waits: every caller hands this function everything that must
/// follow the stop, and nothing it runs before the call may assume the account
/// is stopped.
///
/// **The external sync agent's un-provision is part of the same stop**
/// (`crate::sync_agent::teardown`): its reply is the agent's receipt that its
/// own mount of the store is down, so the erase waits for it as well — on the
/// same spawned task, ahead of the store's stop (tui's `session::sign_out`
/// order), and never on this thread. Every actor change un-provisions:
/// the agent holds a capability for the outgoing account, and the incoming one
/// re-provisions at its post-auth.
///
/// A teardown that finds nothing to stop still waits behind any stop already
/// in flight — a second sign-out landing while the first one's store is still
/// closing must not erase underneath it — and otherwise runs `then` inline,
/// which is every teardown before an account runtime was ever assembled.
pub fn teardown(reason: StopReason, then: impl FnOnce() + 'static) {
    // The share plane's surface stops painting the outgoing account's
    // transfers immediately; its driver task ends on its own when the
    // runtime's `data_version` read errs below.
    #[cfg(feature = "p2p-share")]
    crate::share_glue::forget();
    let unprovision = crate::sync_agent::teardown();
    // `take` advances the generation even when the slot is empty — the case
    // that matters, since a sign-out landing during the first assembly has
    // nothing to take and everything to prevent — and hands back the in-flight
    // assembly to WAIT for, which invalidating the claim alone does not.
    let stopping = HOST.take(reason);
    let store = match (stopping.is_empty(), stopping.payload) {
        (false, Some(rt)) => {
            let stop = stop_account_runtime(
                stopping.settled,
                stopping.pending,
                ACCOUNT_RUNTIME_STOP_BUDGET,
                reason,
            );
            Some((rt, async move {
                // The outcome's diagnostic lines are the shared stop's own.
                let _ = stop.await;
            }))
        }
        _ => None,
    };
    stop_agent_then_store(unprovision, store);
    after_stops(Box::new(then));
}

/// Queue the one stop a teardown owes: the agent's un-provision reply first,
/// then the account store's stop, on the store's runtime — or the un-provision
/// alone, on the agent's own runtime, when no account runtime was assembled but
/// an agent was installed. Either way it is a stop the continuations wait for.
fn stop_agent_then_store(
    unprovision: Option<crate::sync_agent::Unprovision>,
    store: Option<(
        tokio::runtime::Handle,
        impl std::future::Future<Output = ()> + Send + 'static,
    )>,
) {
    match (unprovision, store) {
        (unprovision, Some((rt, stop))) => stop_then(&rt, async move {
            if let Some(unprovision) = unprovision {
                unprovision.reply.await;
            }
            stop.await;
        }),
        (Some(unprovision), None) => stop_then(&unprovision.rt, unprovision.reply),
        (None, None) => {}
    }
}

/// A continuation waiting for every in-flight stop to finish.
type AfterStop = Box<dyn FnOnce()>;

thread_local! {
    /// The stops this thread has spawned and not yet seen finish, and the
    /// continuations queued behind them — the shared ordering
    /// ([`fauna_client_account_runtime::StopQueue`], which tui drives too).
    /// GTK-main-thread only: every teardown runs there, and so does every
    /// completion (`glib::spawn_future_local`).
    static STOPS: std::cell::RefCell<StopQueue<AfterStop>> =
        std::cell::RefCell::new(StopQueue::default());
}

/// Queue `then` behind every in-flight stop and run whatever is ready.
fn after_stops(then: AfterStop) {
    STOPS.with(|q| q.borrow_mut().push(then));
    run_ready();
}

/// Run each continuation that is free to run, outside the queue's borrow: a
/// continuation may itself tear down (an account switch's rebuild signs the
/// next account in, whose own failure path tears down again), and that
/// teardown's stop then holds back everything still queued behind it.
fn run_ready() {
    while let Some(next) = STOPS.with(|q| q.borrow_mut().next_ready()) {
        crate::main_loop_meter::dispatch("account-runtime-after-stop", String::new, next);
    }
}

/// Drive `stop` on the account runtime's own tokio handle and hand its
/// completion back to the GTK main loop, where it releases the continuations
/// queued behind it. The GTK thread never waits on it.
///
/// A stop whose task never reports — the runtime it was spawned on shut down
/// under it — still releases the queue, loudly: the sign-out completes and
/// erases anyway, which is the contract's own answer to a stop that cannot be
/// awaited (`apps/account-scoping.md` § Erasure follows scope).
fn stop_then(
    rt: &tokio::runtime::Handle,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) {
    STOPS.with(|q| q.borrow_mut().stop_started());
    let (done_tx, done_rx) = async_channel::bounded::<()>(1);
    rt.spawn(async move {
        stop.await;
        let _ = done_tx.send(()).await;
    });
    glib::spawn_future_local(async move {
        if done_rx.recv().await.is_err() {
            tracing::warn!(
                "[sign-out] the account store's stop task ended without reporting — its \
                 runtime is gone; continuing (and erasing) without it"
            );
        }
        STOPS.with(|q| q.borrow_mut().stop_finished());
        run_ready();
    });
}

/// The member half of the content-scope set: this account's joined `__conv`
/// channels, read off the live MLS session.
///
/// **The accessor is called on every invocation rather than snapshotted**, and
/// both halves of that are the seam's contract rather than style:
///
/// 1. **No session installed answers `None`** — *cannot tell right now*, never
///    *left every channel*. An affirmative `Some(vec![])` from a loading app is
///    indistinguishable from a genuine departure, and scope departure **deletes**
///    a departed scope's items — so the convenient default here is a data-loss
///    bug, not a stale walk (`account-data-plane.md` § Implementation status
///    today → *Built — W3 scope departure*).
/// 2. **Nothing is cached** — the pump calls this once per pass on purpose, and
///    that is what makes a join or a leave take effect with no notification
///    path, no `register_content_scope` call and no nudge.
fn membership_source() -> fauna_sync_engine::account_runtime::MembershipSource {
    Arc::new(|| {
        crate::conversations::conv_backend::active_session()
            .map(|session| session.joined_conv_channels())
    })
}

fn hex_encode(bytes: [u8; 32]) -> String {
    fauna_core::hex32::encode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no conversations session installed the source answers **`None`**.
    ///
    /// The mutation this goes red against is the tempting one — mapping the
    /// absent session to an empty vec, "so the pump gets a definite answer".
    /// That answer means *this account left every channel*, and the departure
    /// pass acts on it by deleting every joined channel's content from this
    /// device. There is no louder failure downstream to catch it: the walk
    /// simply stops and the rows go.
    #[test]
    fn no_session_cannot_tell_rather_than_answering_empty() {
        let source = membership_source();
        assert_eq!(
            source(),
            None,
            "an app with no session must answer `cannot tell`, never `left every channel`"
        );
    }

    /// Teardown clears the slot, so a post-sign-out reader cannot be handed the
    /// outgoing account's store handle — the in-memory half of the switch
    /// isolation contract `actor_scope` owns.
    #[test]
    fn teardown_leaves_no_handle_behind() {
        let ran = Rc::new(std::cell::Cell::new(false));
        let ran_in_then = Rc::clone(&ran);
        teardown(StopReason::AccountSwitch, move || ran_in_then.set(true));
        assert!(handle().is_none());
        // Nothing was assembled, so nothing is stopping: the continuation runs
        // inline, exactly as every teardown did before a runtime existed.
        assert!(
            ran.get(),
            "a teardown with nothing to stop ran its continuation"
        );
    }

    /// The stop does not hold the calling (GTK) thread, and nothing queued
    /// behind it — the erase, a second sign-out's erase — runs until it ends.
    ///
    /// Latency-independent by construction: the stop is gated on a channel the
    /// test itself releases, so "the call returned while the stop was still
    /// running" is a fact of ordering, not of how long anything took. Red
    /// against both halves of the old trade-off: a blocked stop never returns
    /// here (the gate is released only after it), and a spawned stop with the
    /// continuation run alongside it erases before the stop ends.
    #[test]
    fn the_stop_hands_the_thread_back_and_the_erase_waits_for_it() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let order = Rc::new(std::cell::RefCell::new(Vec::<&str>::new()));
            let (release, gate) = async_channel::bounded::<()>(1);
            stop_then(rt.handle(), async move {
                let _ = gate.recv().await;
            });
            let first = Rc::clone(&order);
            after_stops(Box::new(move || first.borrow_mut().push("sign-out erase")));
            // A second teardown landing mid-stop finds nothing to stop — and
            // must still not erase underneath the first one's store.
            let second = Rc::clone(&order);
            after_stops(Box::new(move || second.borrow_mut().push("second erase")));

            while ctx.iteration(false) {}
            assert!(
                order.borrow().is_empty(),
                "a continuation ran while the stop was still in flight: {:?}",
                order.borrow()
            );

            release
                .send_blocking(())
                .expect("the stop is still waiting on its gate");
            while order.borrow().len() < 2 {
                ctx.iteration(true);
            }
            assert_eq!(*order.borrow(), ["sign-out erase", "second erase"]);
        })
        .expect("a fresh main context is free to acquire");
    }

    /// The sync agent's un-provision holds the erase too — without holding the
    /// calling (GTK) thread — and runs ahead of the store's stop, on the same
    /// task; with no account runtime assembled it is a stop of its own.
    ///
    /// Gated like its twin above, so it is ordering, not timing. Red against
    /// the old shape (the un-provision blocked inside the continuation: the
    /// gate is released only after the teardown returns, so it never would),
    /// against an un-provision spawned and forgotten (the erase runs while it
    /// is gated), and against the store stopping first.
    #[test]
    fn a_gated_unprovision_holds_the_erase_and_precedes_the_store_stop() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let steps = Arc::new(std::sync::Mutex::new(Vec::<&str>::new()));
            let gated_unprovision = |steps: &Arc<std::sync::Mutex<Vec<&'static str>>>| {
                let (release, gate) = async_channel::bounded::<()>(1);
                let steps = Arc::clone(steps);
                let unprovision = crate::sync_agent::Unprovision {
                    rt: rt.handle().clone(),
                    reply: Box::pin(async move {
                        let _ = gate.recv().await;
                        steps.lock().unwrap().push("agent un-provisioned");
                    }),
                };
                (release, unprovision)
            };
            let erase = |label: &'static str| {
                let steps = Arc::clone(&steps);
                Box::new(move || steps.lock().unwrap().push(label)) as AfterStop
            };

            // An agent and an account runtime.
            let (release, unprovision) = gated_unprovision(&steps);
            let store_steps = Arc::clone(&steps);
            stop_agent_then_store(
                Some(unprovision),
                Some((rt.handle().clone(), async move {
                    store_steps.lock().unwrap().push("store stopped");
                })),
            );
            after_stops(erase("sign-out erase"));
            while ctx.iteration(false) {}
            assert!(
                steps.lock().unwrap().is_empty(),
                "the store stopped or the erase ran while the agent had not answered: {:?}",
                steps.lock().unwrap()
            );
            release
                .send_blocking(())
                .expect("the un-provision is gated");
            while steps.lock().unwrap().len() < 3 {
                ctx.iteration(true);
            }
            assert_eq!(
                *steps.lock().unwrap(),
                ["agent un-provisioned", "store stopped", "sign-out erase"]
            );

            // An agent installed but no account runtime assembled.
            steps.lock().unwrap().clear();
            let (release, unprovision) = gated_unprovision(&steps);
            stop_agent_then_store(
                Some(unprovision),
                None::<(tokio::runtime::Handle, std::future::Ready<()>)>,
            );
            after_stops(erase("erase"));
            while ctx.iteration(false) {}
            assert!(
                steps.lock().unwrap().is_empty(),
                "the erase ran while the agent had not answered"
            );
            release
                .send_blocking(())
                .expect("the un-provision is gated");
            while steps.lock().unwrap().len() < 2 {
                ctx.iteration(true);
            }
            assert_eq!(*steps.lock().unwrap(), ["agent un-provisioned", "erase"]);
        })
        .expect("a fresh main context is free to acquire");
    }

    /// A stop whose runtime is gone before it reports still releases the queue:
    /// the sign-out erases anyway rather than stranding the user on a window
    /// that never finishes signing out.
    #[test]
    fn a_stop_whose_runtime_is_gone_still_releases_the_erase() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let handle = rt.handle().clone();
        drop(rt);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let erased = Rc::new(std::cell::Cell::new(false));
            let erased_in_then = Rc::clone(&erased);
            stop_then(&handle, std::future::pending());
            after_stops(Box::new(move || erased_in_then.set(true)));
            while !erased.get() {
                ctx.iteration(true);
            }
        })
        .expect("a fresh main context is free to acquire");
    }
}
