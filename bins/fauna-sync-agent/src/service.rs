//! FaunaSync per-user sync agent — main run loop.

use tokio::sync::broadcast;

use crate::config::{SyncConfig, SyncPaths};
use crate::state::SyncServiceState;

/// Main run loop — loads config, starts pipe server.
///
/// Previously named `run_service_loop`; renamed to `run_agent` because
/// "service loop" implied SCM/Windows-Service mode (removed in Task 3.2a).
pub async fn run_agent(pipe_name: &str, data_dir: Option<&std::path::Path>) -> anyhow::Result<()> {
    // The per-user credential store the capability persists in (sync-agent.md
    // § Credential model). Env-routed (`FAUNA_E2E_CREDENTIAL_DIR` /
    // `FAUNA_KEYRING_APP`) so e2e harnesses stay off the real keyring.
    let credentials = std::sync::Arc::new(crate::credentials::production_store());
    run_agent_with_store(pipe_name, data_dir, credentials).await
}

/// [`run_agent`] with the credential store injected — the seam that lets the
/// in-process duplicate-agent test run the full agent loop against a
/// file-backend store instead of the OS keyring (testing.md § point 10
/// applied to the agent's own tests).
pub(crate) async fn run_agent_with_store(
    pipe_name: &str,
    data_dir: Option<&std::path::Path>,
    credentials: std::sync::Arc<fauna_credential_store::CredentialStore>,
) -> anyhow::Result<()> {
    // Log the transport this build actually serves. `pipe_name` is windows-only
    // (discarded under `cfg(unix)` below), so logging it unconditionally printed
    // `pipe_name="\\.\pipe\fauna-sync"` on macOS/linux — actively misleading in
    // the one diagnostic surface a GUI-less agent has. Observed in the field on
    // the 2026-07-20 `.pkg` install, where this log was the only evidence
    // available while the agent was crash-looping.
    #[cfg(windows)]
    tracing::info!(?data_dir, pipe_name, "sync agent starting");
    #[cfg(unix)]
    tracing::info!(?data_dir, "sync agent starting");

    // Install-scoped TLS trust, BEFORE anything can dial a nest (the capability
    // restore below reconnects immediately). The agent is a read-only consumer
    // of the pins the interactive app minted — see `crate::trust`.
    crate::trust::install_consumer_pin_store(data_dir);

    // Single-instance gate — FIRST, before restoring the capability or starting
    // engines. Without it a spawner racing a slow-binding agent (the convergence
    // loop re-probes on every tick/poke) starts a second agent that syncs the
    // same DB as the same device, and `unix_transport::serve`'s unlink-before-
    // bind lets the newcomer steal the live socket. A kernel-arbitrated lock —
    // a `flock` on unix, a named mutex on windows — makes exactly one instance
    // win; the loser exits cleanly here having touched nothing. The windows
    // mutex is `Local\`-scoped (per logon session), so it does NOT exclude a
    // duplicate in a second concurrent logon session of the same user; that
    // case is caught by the machine-global `FILE_FLAG_FIRST_PIPE_INSTANCE`
    // pipe create failing, which the run-loop tail below observes and turns
    // into agent termination (see `pipe_server::InstanceLock`'s doc).
    #[cfg(unix)]
    let _instance_lock = {
        let socket_path = fauna_ipc::unix_transport::default_socket_path()?;
        match fauna_ipc::unix_transport::InstanceLock::acquire(&socket_path) {
            Ok(lock) => lock,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                tracing::info!("{e} — another instance is already serving this user");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    };

    #[cfg(windows)]
    let _instance_lock = match crate::pipe_server::InstanceLock::acquire(pipe_name) {
        Ok(lock) => lock,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            tracing::info!("{e} — another instance is already serving this user");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    // Resolve the data-root once: the `--data-dir` override (dev/CI) or the
    // production split machine layout. Threaded into state so every handler reads
    // and writes the same root this startup loads from.
    let paths = SyncPaths::new(data_dir.map(|d| d.to_path_buf()));

    // Restore the persisted capability. Loaded BEFORE the config: on macOS the
    // restored capability's actor decides the per-actor state scope, which
    // changes where the config itself lives.
    //
    // This is the *authorized* restore (`sync-agent.md` § Credential model →
    // *The signed-out reconcile*): a capability whose account signed out on this
    // machine is refused and dropped here rather than resumed. That case is
    // reachable precisely because the teardown is one best-effort message — a
    // wedged agent that missed it would otherwise revive on the next boot and
    // serve a signed-out account forever.
    let restored_cap = crate::credentials::restore_authorized_capability(&credentials);

    // Per-actor state scoping (`file-sync.md` § Multi-account × File Provider,
    // consequence 3): scope every state path to the restored capability's actor
    // (`apply_actor_scope` only scopes; nothing is adopted or copied). An
    // unprovisioned boot stays unscoped — it serves nothing until the app provisions,
    // which scopes then. macOS + windows + linux (production) — linux's switcher
    // shipped 2026-07-02, so the inheritance is now activated
    // (`account-scoping.md` § Implementation status gap ledger).
    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    if let Some(cap) = &restored_cap
        && let Some(actor) = cap.actor_id_array()
    {
        apply_actor_scope(&paths, &actor);
    }

    // Load sync config from the resolved (scoped, on macOS) data-root.
    let config = paths.load_config().unwrap_or_else(|e| {
        tracing::warn!("config load failed, using defaults: {e}");
        SyncConfig::default()
    });

    // NOTE: device.toml is NOT loaded here. Under the per-user model the WinUI app
    // provisions a SyncCapability over the pipe at login — that capability carries
    // the nest URL and acts as the "connected to nest" signal. See `state.rs` §
    // `capability` and `docs/goal/architecture/key-material-hierarchy.md` rule #7.

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (event_tx, _) = broadcast::channel::<fauna_ipc::sync::Event>(256);

    let state = SyncServiceState::new_with_credentials(
        config,
        shutdown_tx,
        event_tx.clone(),
        paths,
        Some(credentials.clone()),
    );

    {
        let config = state.config.read().await;
        tracing::info!(
            folders = config.locations.len(),
            "sync agent ready; on-demand hydration host awaits app-provisioned capability"
        );
    }

    // The on-demand surface's boot reconcile, BEFORE any engine starts
    // (`on-demand-files.md` § Linux FUSE binding, the lifecycle rule): answer the
    // availability probe the status reply reports, and on linux lazily unmount
    // every leftover mount of this agent's FUSE binding at a configured on-demand
    // location — a ghost under a mount point would otherwise take a second mount
    // on top of it. Blocking (a device open, a helper process per ghost), so off
    // the async runtime, and awaited: the engines below must find it done.
    {
        let state = state.clone();
        #[cfg(target_os = "linux")]
        let on_demand_locations: Vec<std::path::PathBuf> = state
            .config
            .read()
            .await
            .locations
            .iter()
            .filter(|l| l.mode == crate::config::LocationMode::OnDemand)
            .map(|l| std::path::PathBuf::from(&l.path))
            .collect();
        let reconciled = tokio::task::spawn_blocking(move || {
            let _ = state.on_demand.set(crate::state::probe_on_demand());
            #[cfg(target_os = "linux")]
            {
                let swept = crate::fuse_host::sweep_ghost_mounts(&on_demand_locations);
                if swept > 0 {
                    tracing::info!(swept, "unmounted leftover on-demand mounts");
                }
            }
        })
        .await;
        if let Err(e) = reconciled {
            tracing::warn!(error = %e, "the on-demand boot reconcile did not finish");
        }
    }

    // Publish the restored capability (sync-agent.md § Credential model): the
    // app-dead boot path — engines resume with no login-time push. An absent /
    // corrupt record just leaves the slot empty until the app's convergence
    // loop provisions (the pre-A2 behavior).
    if let Some(cap) = restored_cap {
        let nest_url = cap.nest_url.clone();
        // The refused state survives a restart as the capability does: the
        // record goes back into memory before the slot is published, so the
        // first status reply and the account host's first read both see it.
        crate::renewal::restore_refusal(&state);
        *state.capability.write().await = Some(cap);
        tracing::info!(nest_url = %nest_url, "capability restored from credential store");
        // The principal-support advertisement must be true from the first
        // status call, not from the first renewal: an app starting beside an
        // already-provisioned agent asks before either has renewed anything,
        // and the signed-out onboarding reconcile reads it.
        crate::renewal::refresh_store_principal_presence(&state).await;
        if let Err(e) = crate::engine_driver::reconcile_engines(&state).await {
            tracing::warn!(error = %e, "engine start deferred after capability restore");
        }
    } else {
        // No capability to serve: a refusal record left at rest describes
        // nothing, and must not be read back against a later provision.
        crate::credentials::delete_refusal(&credentials);
    }

    // Agent-side bearer renewal (same doc §): keeps the bearer fresh app-dead
    // via `fauna.auth.device_handshake`, off the renewal device key the
    // capability carries. No-op (idle recheck) while nothing is provisioned.
    let renewal_handle = tokio::spawn(crate::renewal::run(
        state.clone(),
        state.shutdown_tx.subscribe(),
    ));

    // The client-device backup custodian's desktop host (`behavior/backup-destinations.md` § Third
    // destination kind). Idles at one capability read + one registry read per
    // minute on the overwhelming majority of devices, which are not enrolled as
    // custodians; discovers its own assignment from the nest's destination
    // registry, per § Control plane split. Lives beside renewal for the same
    // reason: both must keep running app-dead.
    let custodian_handle = tokio::spawn(crate::custodian::run(
        state.clone(),
        state.shutdown_tx.subscribe(),
    ));

    // The account-store runtime host (W5 (account-data-plane.md § Workstreams).5b — `account-data-plane.md` R2 (account-data-plane.md § The ratified decisions)): this
    // process mounts the machine's shared account store with no identity seed
    // and hosts its pump app-dead, so an account whose apps are all closed still
    // walks its scopes, drains its outbox and publishes its endpoints. Beside
    // renewal + custodian for the same reason those two are here. Whether it
    // actually pumps is the runtime's own W5.1 election, not a decision here: a
    // running app keeps the role it holds, and this host takes it on the next
    // backstop tick after that app exits.
    // Answer "is there a notification sink here?" once, off the runtime: the
    // linux probe is a session-bus round trip, and the status reply that
    // reports it must stay a field read (`push_arm`).
    let sink = std::sync::Arc::clone(&state.notification_sink);
    tokio::task::spawn_blocking(move || sink.probe());

    let account_host_handle = tokio::spawn(crate::account_host::run(
        state.clone(),
        state.shutdown_tx.subscribe(),
    ));

    // The content-key edge task (`on-demand-files.md` § Shared sets on a
    // capability host → *One mechanism*): this process resolves every set's
    // content keys from its holder's custody and re-resolves at its own edges —
    // every reconcile, the fleet-scope custody nudge, an engine's refresh edge, a
    // backstop — so a rotation made on another device while every app is closed
    // still re-keys the engines here. Beside the other app-dead hosts.
    let content_keys_handle = tokio::spawn(crate::content_keys::run(
        state.clone(),
        state.shutdown_tx.subscribe(),
    ));

    // Startup janitor: sweep ghost shell sync-root registrations (folders that
    // vanished while the service was down, crashed-test leftovers). Registrations
    // are persistent across service restarts by design — this is the boot
    // reconcile that keeps that model from accreting stale Explorer entries.
    // Blocking WinRT calls, so off the async runtime; best-effort.
    #[cfg(windows)]
    tokio::task::spawn_blocking(|| {
        let swept = crate::cfapi_host::sweep_ghost_shell_registrations();
        if swept > 0 {
            tracing::info!(swept, "removed ghost shell sync-root registrations");
        }
    });

    // Start the local IPC control-plane server. Same length-prefixed dag-cbor frame
    // and the same platform-neutral `pipe_server::handle_request` everywhere; only the
    // transport differs — a per-SID named pipe on windows, a per-user unix socket
    // elsewhere (sync-agent.md § Control plane split).
    #[cfg(windows)]
    let mut ipc_handle = {
        let event_tx = event_tx.clone();
        let state = state.clone();
        let pipe_name = pipe_name.to_string();
        tokio::spawn(async move {
            crate::pipe_server::run_pipe_server(state, event_tx, shutdown_rx, &pipe_name).await
        })
    };

    #[cfg(unix)]
    let mut ipc_handle = {
        // The windows per-SID `pipe_name` has no unix analog; the socket path is
        // derived per-user ($XDG_RUNTIME_DIR / ~/Library/Application Support). A 0600
        // socket in a 0700 dir is the unix analog of the per-SID pipe DACL. If no
        // per-user runtime dir exists (a headless box without linger), fail loudly
        // rather than bind a world-reachable socket.
        let _ = pipe_name;
        let event_tx = event_tx.clone();
        let state = state.clone();
        let socket_path = fauna_ipc::unix_transport::default_socket_path()?;
        tracing::info!(socket = %socket_path.display(), "serving local IPC on unix socket");
        let handler = move |req: fauna_ipc::sync::Request| {
            let state = state.clone();
            async move { crate::pipe_server::handle_request(&req, &state).await }
        };
        tokio::spawn(async move {
            fauna_ipc::unix_transport::serve(&socket_path, handler, shutdown_rx, event_tx)
                .await
                .map_err(anyhow::Error::from)
        })
    };

    // Wait for a shutdown signal — or the IPC server ending on its own, which
    // is fatal: the IPC server is the agent's control plane, and an agent that
    // keeps running without one (renewal loop + engine reconcile, unreachable
    // by any app) is the degraded-duplicate hazard, not a service. The
    // reachable case is the cross-session duplicate: the `Local\` named mutex
    // in `InstanceLock` excludes duplicates only within one logon session,
    // while the pipe is per-user machine-wide — so a second same-user logon
    // session (RDP + console) passes the mutex and must instead be stopped by
    // its machine-global `FILE_FLAG_FIRST_PIPE_INSTANCE` pipe create failing,
    // observed here (previously that failure was swallowed in the spawn).
    // SIGTERM must take the SAME orderly path as ctrl_c (unix): it is what
    // systemd's `stop`, the e2e harness's `terminate_tree`, and any supervisor
    // send first — and the default disposition would skip the engine/cfapi
    // teardown below entirely. Observed live 2026-07-24 (macOS orphan, pid
    // 29443): the agent survived SIGTERM and needed SIGKILL while still
    // holding a live nest connection. The binary-spawn proof (SIGTERM →
    // orderly exit, bounded) rides the mass-delete-floor follow-on.
    #[cfg(unix)]
    let early_ipc_exit = {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            r = tokio::signal::ctrl_c() => {
                r?;
                tracing::info!("shutting down");
                None
            }
            _ = sigterm.recv() => {
                tracing::info!("SIGTERM — shutting down");
                None
            }
            r = &mut ipc_handle => Some(r),
        }
    };
    #[cfg(windows)]
    let early_ipc_exit = tokio::select! {
        r = tokio::signal::ctrl_c() => {
            r?;
            tracing::info!("shutting down");
            None
        }
        r = &mut ipc_handle => Some(r),
    };
    #[cfg(not(any(windows, unix)))]
    tokio::signal::ctrl_c().await?;

    // `send`, not `send_replace`, is correct here (transport.md § Connection
    // lifecycle's send_replace rule): the three
    // subscribers above (renewal, custodian, account_host) are minted once
    // at startup, before this can fire, and each returns only after it
    // itself observes this exact `true` — so by construction at least one is
    // always still alive when this sends, `send` cannot silently drop the
    // value, and nothing ever re-subscribes afterward to read a stale
    // `false`. Wakeup-only, verified by grep: `shutdown_tx.subscribe()` has
    // no other call site in this crate.
    let _ = state.shutdown_tx.send(true);

    // Stop the hydration host: dropping it cancels every engine, each of which
    // tears its own cfapi sync root down (disconnect → unregister → ctx-remove).
    // The shared `EngineHost` detaches its worker thread rather than joining, so
    // this teardown is best-effort on shutdown — harmless, since
    // `register_sync_root` uses CF_REGISTER_FLAG_UPDATE (a leftover registration
    // is re-used on the next start).
    let running = state.engines.lock().await.take();
    drop(running);

    #[cfg(any(windows, unix))]
    if let Some(join) = early_ipc_exit {
        let err = match join? {
            Ok(()) => anyhow::anyhow!("IPC server exited before shutdown was requested"),
            Err(e) => e.context("IPC server failed"),
        };
        tracing::error!("IPC control plane ended prematurely; terminating agent: {err:#}");
        let _ = renewal_handle.await;
        let _ = custodian_handle.await;
        let _ = account_host_handle.await;
        let _ = content_keys_handle.await;
        return Err(err);
    }

    #[cfg(any(windows, unix))]
    ipc_handle.await??;
    renewal_handle.await?;
    custodian_handle.await?;
    account_host_handle.await?;
    content_keys_handle.await?;

    Ok(())
}

/// Scope the shared [`SyncPaths`] to one actor's state subdir (`file-sync.md`
/// § Multi-account × File Provider, consequence 3) — nothing is adopted into
/// it: a state file sitting flat at the base is no account's. Returns whether the scope
/// changed — the caller must then reload config, since the scope changes where
/// `config.toml` lives.
///
/// Activation: **macOS + windows + linux, on every layout** — the production base
/// and a `--data-dir` override alike, since the override only moves the flat base
/// and the scope nests under it. The override was exempt (kept flat "so harnesses
/// keep their fixed layout") until 2026-09-26, when the one e2e harness that
/// passes it — windows, whose app- and tui-spawned agents are pinned to a
/// per-launch dir because the agent's default root is the machine-global
/// `%LOCALAPPDATA%` — was found sharing one binding store and one
/// `fsid-local-N.db` across every account a test module signed in: `local:<id>`
/// is unique per nest, two fresh nests both mint `local:N`, so a later account
/// inherited the earlier one's pull anchor and skipped its own nest's first
/// changes. The e2e readers glob flat-then-scoped (`helpers/sync_agent_config.py`),
/// so nothing depended on flatness; only the process-level dirs stay at the flat
/// base (the log dir, a `--data-dir` trust store). Windows switches accounts on the
/// *provision* path (it sends no Unprovision to stop engines first, unlike macOS's
/// unprovision-first teardown), so the re-scope's engine rebuild rides
/// [`crate::engine_driver::reconcile_engines`] through the scope-aware engine
/// stamp — see that module's `EngineStamp` doc. Linux's app sends
/// `UnprovisionCapability` on every session-teardown path (same as macOS), so it
/// re-scopes on the unprovision-first shape.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn apply_actor_scope(paths: &SyncPaths, actor_id: &[u8; 32]) -> bool {
    let hex = hex::encode(actor_id);
    if paths.actor_scope().as_deref() == Some(hex.as_str()) {
        return false;
    }
    let flat = paths.flat_base_dir();
    // Validate through the one shared derivation before scoping anything.
    if let Err(e) = fauna_sync_engine::db::actor_state_dir(&flat, &hex) {
        tracing::warn!(error = %e, "refusing per-actor state scope: bad actor id");
        return false;
    }
    paths.set_actor_scope(Some(hex));
    true
}

#[cfg(all(test, any(target_os = "macos", windows, target_os = "linux")))]
mod tests {
    use super::apply_actor_scope;
    use crate::config::SyncPaths;

    /// The `--data-dir` override scopes per actor exactly as the production
    /// layout does: the override only moves the flat base, and the scope nests
    /// under it. Until 2026-09-26 the override stayed flat, so the one e2e
    /// harness that passes it (windows — the app- and tui-spawned agents are
    /// pinned to a per-launch dir because the agent's default root is the
    /// machine-global `%LOCALAPPDATA%`) shared one binding store and one
    /// `fsid-local-N.db` across every account a test module signed in.
    #[test]
    fn apply_actor_scope_scopes_the_data_dir_override_too() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
        let actor = [0xabu8; 32];

        assert!(
            apply_actor_scope(&paths, &actor),
            "the override layout must scope per actor like the production one"
        );
        assert_eq!(paths.base_dir(), tmp.path().join(hex::encode(actor)));
        assert_eq!(
            paths.flat_base_dir(),
            tmp.path(),
            "the flat base stays where the override root sits"
        );

        // Idempotent for the same actor; a different actor re-scopes.
        assert!(!apply_actor_scope(&paths, &actor), "same actor: no change");
        let other = [0xcdu8; 32];
        assert!(
            apply_actor_scope(&paths, &other),
            "a different actor re-scopes"
        );
        assert_eq!(paths.base_dir(), tmp.path().join(hex::encode(other)));
    }
}
