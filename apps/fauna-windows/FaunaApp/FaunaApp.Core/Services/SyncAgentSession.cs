using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The app's session-scoped handle on the per-user sync agent
/// (<c>fauna-sync-agent.exe</c>) — a thin holder over the SHARED Rust
/// provisioner (<see cref="FfiSyncAgentProvisioner"/>, i.e.
/// <c>fauna_client_sync::agent::SyncAgentProvisioner</c>) plus the shared
/// pushed-event listener.
///
/// <para>
/// It replaces <c>HydrationSessionService</c> + <c>SyncAgentLauncher</c> +
/// <c>CapabilityProvisioner</c> + <c>SyncEventListener</c>: those were a
/// hand-maintained C# twin of the convergence loop, the probe-then-spawn, the
/// capability mint and the event filter that all four desktop stacks now share
/// (sync-agent.md § Consumers — "there is no per-app IPC codec", ratified
/// 2026-07-24). Nothing agent-observable changes: the wire, the agent binary and
/// the per-SID pipe name are untouched; the loop this now delegates to is the
/// very contract the C# twin was written against (probe → spawn-if-absent →
/// <c>RefreshBearer</c> → full re-provision on <c>NoCapability</c>, 30 s tick +
/// poke).
/// </para>
///
/// <para>
/// The identity seed still never leaves the app: it is consumed in-Rust to sign
/// the <c>RenewBearer</c> device grant and derive the owner <c>BackupKey</c>,
/// and only the minted capability reaches the agent (key-material-hierarchy.md
/// rule #7).
/// </para>
///
/// <para>
/// Everything is best-effort by contract, exactly as before: on-demand sync is
/// opt-in and the agent may simply not be installed, so
/// <see cref="StartAsync"/> returns <c>null</c> rather than throwing and a
/// caller treats that as a no-op that must never disrupt login.
/// </para>
/// </summary>
internal sealed class SyncAgentSession : ISyncAgentSessionHandle, IDisposable
{
    /// <summary>The device label registered on the machine's named row.</summary>
    public const string DeviceLabel = "fauna-windows";

    /// <summary>Not torn down.</summary>
    private const int TeardownNone = 0;
    /// <summary>Handle dropped, agent keeps serving (newer login / nest re-point).</summary>
    private const int TeardownStop = 1;
    /// <summary>Capability torn down (sign-out / switch / factory reset).</summary>
    private const int TeardownUnprovision = 2;

    private readonly FfiSyncAgentProvisioner _provisioner;
    private FfiSyncAgentEventListener? _listener;
    private readonly Action<string>? _onSyncComplete;

    /// <summary>Which teardown (if any) has claimed this session — one of the
    /// <c>Teardown*</c> constants. The CAUSE matters, not just the fact: see
    /// <see cref="StartAsync"/>'s post-start re-check.</summary>
    private int _teardown;

    /// <summary>
    /// Guards provisioner DISPOSAL against an in-flight <see cref="StartAsync"/>.
    ///
    /// <para>Since the session is now published before it starts, a teardown can land while
    /// <c>start()</c> is still awaiting inside Rust. Concurrent <i>calls</i> on the
    /// provisioner are fine, but disposing it drops the last handle on the Rust object
    /// while a call holds it — a use-after-free, not a caught exception. So a
    /// <see cref="Stop"/> arriving mid-start defers the dispose to the start's own
    /// completion instead.</para>
    /// </summary>
    private readonly object _disposeGate = new();
    private bool _startInFlight;
    private bool _disposeDeferred;

    private SyncAgentSession(
        string actorIdHex, FfiSyncAgentProvisioner provisioner, Action<string>? onSyncComplete)
    {
        ActorIdHex = actorIdHex;
        _provisioner = provisioner;
        _onSyncComplete = onSyncComplete;
    }

    /// <summary>
    /// The actor whose login built this session. The app-global session slot is replaced on
    /// account switch, so this is what lets a caller tell that the live agent is now
    /// another account's.
    /// </summary>
    public string ActorIdHex { get; }

    /// <summary>
    /// The live control channel, for the surfaces that read the agent rather
    /// than drive its lifecycle: the <c>sync-agent-status</c> health poll, the
    /// Settings → Sync folder bindings, and the e2e <c>data.sync</c> block.
    /// </summary>
    public IFfiSyncAgentProvisioner Channel => _provisioner;

    /// <summary>The narrow folder-binding seam over <see cref="Channel"/>, for the
    /// bindings control plane (which must not be handed the whole provisioner — it could
    /// stop the agent).
    ///
    /// <para><b>The binding MODEL is not here.</b> It lives on the login-scoped
    /// <see cref="LocationBindingsController"/>, which is built synchronously at login and
    /// adopts this channel once <see cref="StartAsync"/> finally returns. Hanging the model
    /// off this session instead — as it was until 2026-08-05 — made every binding surface
    /// unavailable for as long as the start-up chain took (~10 s against a disconnected
    /// nest, longer against a slow one), and a bind attempted in that window was silently
    /// dropped. Linux draws the same line: its model is installed with the session state,
    /// and the surfaces read it through <c>sync_agent::current_locations()</c>.</para></summary>
    public ILocationControlChannel LocationControl => new AgentLocationControlChannel(_provisioner);

    /// <summary>
    /// Build the provisioner for this authenticated session. <b>Agent-invisible by
    /// contract:</b> nothing here reaches the agent — in shared Rust
    /// (<c>fauna_client_sync::agent</c>) <c>SyncAgentProvisioner::new</c> only assembles
    /// state, and it is <see cref="StartAsync"/> that names the machine's row and
    /// spawns the convergence loop which pushes <c>ProvisionCapability</c>.
    ///
    /// <para>That split is load-bearing, not incidental: it is what lets
    /// <see cref="SyncAgentSessionHost{T}"/> abandon a build that was superseded mid-flight
    /// having provisioned <i>nothing</i>. Keep any new agent traffic out of here — put it
    /// in <see cref="StartAsync"/>, or the race comes back.</para>
    /// </summary>
    /// <param name="rpc">
    /// This login's own <b>live</b> WS-RPC client — the provisioner is built on its cached,
    /// connected <c>FfiNestClient</c> (<see cref="INestRpcClient.BuildSyncAgentProvisionerAsync"/>),
    /// exactly as macOS builds its on <c>ensureNestConnected()</c>. The identity seed is read
    /// from the client's own <c>CryptoService</c> and consumed in-Rust ONLY — to sign the
    /// device label and derive the <c>BackupKey</c> — never sent to the agent.
    /// </param>
    /// <param name="actorIdHex">
    /// The public actor id of the identity <paramref name="rpc"/> is signed in as
    /// (<c>CryptoService.ActorIdHex</c>) — recorded as <see cref="ActorIdHex"/>, never sent
    /// to the agent by this argument.
    /// </param>
    /// <param name="deviceId">This device's id, as the agent's device row keys it.</param>
    /// <param name="predecessorBackupKeys">
    /// The retired owner <c>BackupKey</c>s of every identity this actor succeeded
    /// from, nearest hop first (<c>FfiAccountRegistry.PredecessorBackupKeys</c>) —
    /// resolved ONCE by the caller, post-auth, and shared with every consumer this
    /// session needs it for (sync-agent.md § Credential model → *Retired owner
    /// keys after an identity succession*). Empty when the account never
    /// succeeded, which is also the safe default a resolve failure falls back to
    /// — this constructor never re-derives or re-walks the registry itself.
    /// </param>
    /// <param name="predecessorActorIds">
    /// <paramref name="predecessorBackupKeys"/>'s attested-ids sibling
    /// (<c>FfiAccountRegistry.AttestedPredecessorActorIds</c>;
    /// <c>account-data-taxonomy.md</c> § The generation machinery → *The source
    /// of `prior`*, ruled 2026-09-13) — same caller-resolves-once contract.
    /// </param>
    /// <param name="bearerSource">
    /// A cheap SYNCHRONOUS read of the bearer the app currently holds, with its
    /// expiry (unix seconds on this device's clock, anchored at receipt — the
    /// deadline the agent's renewal loop plans on), called fresh on every
    /// convergence tick. Return <c>null</c> (or an empty token) when there is none — the tick then skips and the next one retries, which
    /// is why the caller pairs this with <see cref="Poke"/> off its TTL-refresh
    /// loop rather than blocking here. Mirrors macOS's
    /// <c>APIClientProvisioningBearerSource</c>.
    /// </param>
    /// <param name="onSyncComplete">
    /// Per-file completed-upload notification sink (the sync-complete toast).
    /// Called from the listener's background thread — marshal to the UI thread
    /// yourself. Omit to run without the listener.
    /// </param>
    /// <param name="onAgentReachable">
    /// The agent-up edge, i.e. the cue to re-drive the folder-binding reconcile
    /// (an attach-time reconcile alone races the very first spawn). Called from
    /// the convergence loop's tokio task.
    /// </param>
    /// <remarks>
    /// Its only await is the login's own connection — usually already up, since the
    /// login that calls this has been talking to the nest over the same client. No agent
    /// traffic and no RPC: just handle construction. Keep it that way — keep it fast.
    ///
    /// <para>⚠ It used to build on a one-shot <c>new FfiNestClient(nestUrl, secret)</c>
    /// that was never <c>Connect()</c>ed (that constructor does not open the socket), so
    /// the provisioner's <c>fauna.sync.register</c> waited out its deadline and failed as
    /// "the connection to the nest was lost" — the mint was decided and reached, and the
    /// named device row still never landed. Do not reintroduce a private client here.</para>
    /// </remarks>
    public static async Task<SyncAgentSession?> CreateAsync(
        INestRpcClient rpc,
        string actorIdHex,
        string deviceId,
        byte[][] predecessorBackupKeys,
        byte[][] predecessorActorIds,
        Func<(string Token, ulong ExpiresAt)?> bearerSource,
        Action<string>? onSyncComplete = null,
        Action? onAgentReachable = null)
    {
        try
        {
            var provisioner = await rpc.BuildSyncAgentProvisionerAsync(
                // Resolved by the caller (App.xaml.cs::StartHydrationSession) off
                // FfiAccountRegistry.PredecessorBackupKeys(actorId), once, post-auth
                // — mirroring linux's client.rs::predecessor_backup_keys (one cached
                // resolution shared with label_custody()) and tui's session.rs
                // (succession_predecessor_backup_keys). sync-agent.md § Credential
                // model → *Retired owner keys after an identity succession*.
                // Passed unconditionally: the shared engine (fauna-client-sync's
                // agent.rs) is the one place a stale/unused predecessor key is
                // ever dropped, never a client-side pre-filter.
                predecessorBackupKeys,
                // Passed unconditionally, same discipline as the keys above: the
                // shared agent is the one place a stale/unused attested id is
                // ever dropped, never a client-side pre-filter.
                predecessorActorIds,
                deviceId,
                DeviceLabel,
                // The platform spawner is shared Rust
                // (`fauna_client_sync::agent_spawner::WindowsDetachedSpawner`):
                // the pin/exe-dir/ProgramFiles resolution, the harness-isolation
                // argv and CREATE_NO_WINDOW all live there, so the app owns no
                // spawn code. One spawner covers production and e2e on windows —
                // the env-forwarded args do the isolation, and they compile out
                // of a release build (convention 15).
                new PlatformSpawner(),
                new BearerSource(bearerSource),
                onAgentReachable is null ? null : new ReachabilityObserver(onAgentReachable))
                .ConfigureAwait(false);

            return new SyncAgentSession(actorIdHex, provisioner, onSyncComplete);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[sync-agent] session not built: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Start the convergence loop — <b>the only step that provisions the agent</b> — and
    /// bring up the sync-complete listener.
    ///
    /// <para>Health polling and the folder UI are meaningful even if the loop itself cannot
    /// start (a reachable-but-unprovisioned agent is still a truthful "Running"), so a
    /// failure here is logged and swallowed rather than unwinding the session — the macOS
    /// ordering.</para>
    ///
    /// <para><b>Two teardown races are closed here, and they want opposite endings.</b> By
    /// the time this runs the session is already published (<see cref="SyncAgentSessionHost{T}"/>
    /// publishes before starting), so a teardown can land either just before or part-way
    /// through — and shared Rust cannot save us in the latter case: <c>unprovision()</c>
    /// cancels the convergence task and then pushes <c>UnprovisionCapability</c>, so one
    /// arriving while <c>start()</c> is still inside <c>register_this_machine</c> finds no
    /// task to cancel and is promptly undone by the loop this call goes on to spawn.
    /// <list type="bullet">
    /// <item><b>Sign-out</b> (<see cref="TeardownUnprovision"/>) — re-tear-down afterwards.
    /// <c>unprovision()</c> is idempotent, so the belt-and-braces push costs nothing when
    /// the teardown already won the race.</item>
    /// <item><b>Newer login</b> (<see cref="TeardownStop"/>) — do NOT unprovision. The
    /// incoming login owns the capability now, and tearing it down here would clobber the
    /// one it just installed: the account-switch bug from the far side.</item>
    /// </list></para>
    /// </summary>
    public async Task StartAsync()
    {
        // Superseded before we ever started: never provision. This is the cheap, common
        // case and the one row 57 was actually about.
        //
        // Logged because it is SILENT otherwise, and it is the last step of a
        // start-up chain whose every earlier step already logs: the host emits
        // ENTER/BUILT/INSTALLED before calling this, so a session superseded
        // here leaves a log that reads exactly like a provisioned one. A run
        // where every install takes this return provisions nothing and says
        // nothing — which is precisely how it presents.
        if (Volatile.Read(ref _teardown) != TeardownNone)
        {
            ShellLog.Info("App", "[sync-agent] session superseded before start — not provisioning");
            return;
        }

        lock (_disposeGate) { _startInFlight = true; }
        try
        {
            await StartInnerAsync().ConfigureAwait(false);
        }
        finally
        {
            bool disposeNow;
            lock (_disposeGate)
            {
                _startInFlight = false;
                disposeNow = _disposeDeferred;
            }

            if (disposeNow) DisposeProvisioner();
        }
    }

    private async Task StartInnerAsync()
    {
        try
        {
            await _provisioner.Start().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[sync-agent] provisioner start failed (sync degraded until relaunch): {ex.Message}");
        }

        if (Volatile.Read(ref _teardown) == TeardownUnprovision)
        {
            ShellLog.Info("App", "[sync-agent] teardown raced the start — re-unprovisioning");
            try
            {
                await _provisioner.Unprovision().ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                ShellLog.Warn("App", $"[sync-agent] re-unprovision after a raced start failed: {ex.Message}");
            }

            return;
        }

        if (_onSyncComplete is not null && Volatile.Read(ref _teardown) == TeardownNone)
        {
            try
            {
                _listener = FaunaFfiMethods.SpawnSyncEventListener(
                    new SyncCompleteObserver(_onSyncComplete));
            }
            catch (Exception ex)
            {
                ShellLog.Warn("App", $"[sync-agent] event listener not started: {ex.Message}");
            }
        }
    }

    /// <summary>
    /// Wake the convergence loop for an immediate tick — e.g. right after the
    /// TTL loop minted a fresh bearer, so the agent is not left on the stale one
    /// for up to a tick.
    /// </summary>
    public void Poke()
    {
        try { _provisioner.Poke(); } catch (Exception ex) { ShellLog.Warn("App", $"[sync-agent] poke failed: {ex.Message}"); }
    }

    /// <summary>
    /// Stop the loop and tear the agent's provisioned capability down — the
    /// sign-out / account-switch / factory-reset teardown. The loop is stopped
    /// FIRST by <c>unprovision()</c> itself, so no tick can re-provision from
    /// still-loaded crypto and undo it. Idempotent and best-effort: an
    /// unreachable or never-started agent is a normal no-op.
    /// </summary>
    public async Task UnprovisionAsync()
    {
        // Claim the session for the unprovision cause. A prior Stop() (newer login) does
        // NOT get upgraded to an unprovision — that ordering means the capability already
        // belongs to the incoming login.
        if (Interlocked.CompareExchange(ref _teardown, TeardownUnprovision, TeardownNone) != TeardownNone)
        {
            // Not a failure — but it IS the second way this teardown reaches the
            // agent as silence, and it is invisible from the agent's side (see
            // SyncAgentSessionHost.UnprovisionAsync's own note). Say which cause
            // won, so a missing `capability un-provisioned` line is diagnosable in
            // one run instead of by elimination.
            var claimed = Volatile.Read(ref _teardown);
            ShellLog.Info(
                "App",
                "[sync-agent] unprovision: teardown already claimed by "
                + (claimed == TeardownStop ? "a newer login (Stop)" : "an earlier unprovision")
                + " — not pushing");
            return;
        }

        StopListener();
        try
        {
            await _provisioner.Unprovision().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[sync-agent] unprovision failed: {ex.Message}");
        }
    }

    /// <summary>
    /// Drop this session's local handles WITHOUT unprovisioning — the teardown
    /// for paths that are only disposing the clients this session reads (an e2e
    /// re-login, a nest re-point), where the agent should keep serving.
    /// </summary>
    public void Stop()
    {
        if (Interlocked.CompareExchange(ref _teardown, TeardownStop, TeardownNone) != TeardownNone) return;
        StopListener();
        DisposeProvisioner();
    }

    /// <summary>Dispose the provisioner, or defer it if a start is still in flight (see
    /// <see cref="_disposeGate"/>).</summary>
    private void DisposeProvisioner()
    {
        lock (_disposeGate)
        {
            if (_startInFlight)
            {
                _disposeDeferred = true;
                return;
            }
        }

        try { _provisioner.Dispose(); }
        catch (Exception ex) { ShellLog.Warn("App", $"[sync-agent] provisioner dispose failed: {ex.Message}"); }
    }

    private void StopListener()
    {
        try { _listener?.Stop(); } catch { /* best-effort: the thread may already be gone */ }
        _listener = null;
    }

    public void Dispose() => Stop();

    /// <summary>
    /// Forwarder onto the shared-Rust windows spawner
    /// (<c>fauna_client_sync::agent_spawner::WindowsDetachedSpawner</c>, exported
    /// as <see cref="FfiWindowsDetachedSpawner"/>).
    ///
    /// <para>
    /// It exists ONLY because <c>uniffi-bindgen-cs</c> does not declare an
    /// exported object's conformance to a <c>with_foreign</c> trait on the
    /// generated class — Swift's generator does, which is why macOS passes
    /// <c>FfiChildAgentSpawner()</c> straight in as an <c>FfiAgentSpawner</c> and
    /// C# cannot. No spawn logic lives here: the binary resolution order, the
    /// harness-isolation argv and <c>CREATE_NO_WINDOW</c> are all Rust's, and
    /// this method body is one delegation. Delete it when the generator learns
    /// to emit the conformance.
    /// </para>
    /// </summary>
    private sealed class PlatformSpawner : FfiAgentSpawner
    {
        private readonly FfiWindowsDetachedSpawner _inner = new();

        public void SpawnAgent() => _inner.SpawnAgent();
    }

    /// <summary>
    /// The synchronous bearer read the convergence loop calls each tick. An
    /// empty token means "not authenticated right now" and skips the tick.
    /// </summary>
    private sealed class BearerSource : FfiProvisioningBearerSource
    {
        private readonly Func<(string Token, ulong ExpiresAt)?> _read;

        public BearerSource(Func<(string Token, ulong ExpiresAt)?> read) => _read = read;

        /// <summary>
        /// ⚠ An empty return here makes the shared convergence loop skip the ENTIRE tick
        /// (`fauna_ipc::convergence::tick` returns before `refresh_bearer`), so the agent is
        /// never provisioned, never starts an engine, AND the agent-reachable edge never
        /// fires. `convergence.rs` now logs the bearer present/absent state itself at every
        /// tick (`sync-agent.md` § Control plane split), so this no longer needs its
        /// own transition-tracking log — only the C#-side read failure below, which Rust
        /// never sees, still needs one.
        /// </summary>
        public FfiProvisioningBearer CurrentBearer()
        {
            try
            {
                var bearer = _read();
                return bearer is null
                    ? new FfiProvisioningBearer("", 0)
                    : new FfiProvisioningBearer(bearer.Value.Token, bearer.Value.ExpiresAt);
            }
            catch (Exception ex)
            {
                // Never let a read fault cross back into the Rust loop; an empty
                // token is the defined "skip this tick" value. Rust-side tracing
                // cannot see this — it only ever observes the empty-token result —
                // so this is the one line that still needs to log from here.
                ShellLog.Info("App", $"[sync-agent] bearer read THREW ({ex.GetType().Name}: {ex.Message}) — tick skipped");
                return new FfiProvisioningBearer("", 0);
            }
        }
    }

    private sealed class ReachabilityObserver : FfiAgentReachabilityObserver
    {
        private readonly Action _onReachable;

        public ReachabilityObserver(Action onReachable) => _onReachable = onReachable;

        public void OnAgentReachable()
        {
            try { _onReachable(); } catch { /* best-effort cue, never a fault channel */ }
        }
    }

    private sealed class SyncCompleteObserver : FfiSyncCompleteObserver
    {
        private readonly Action<string> _onSyncComplete;

        public SyncCompleteObserver(Action<string> onSyncComplete) => _onSyncComplete = onSyncComplete;

        public void OnSyncComplete(string filename)
        {
            try { _onSyncComplete(filename); } catch { /* best-effort notification */ }
        }
    }
}
