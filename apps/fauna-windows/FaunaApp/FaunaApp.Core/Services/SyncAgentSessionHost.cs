using System;
using System.Threading.Tasks;

namespace FaunaApp.Core.Services;

/// <summary>
/// The lifecycle face <see cref="SyncAgentSessionHost{T}"/> drives on a sync-agent
/// session. <c>SyncAgentSession</c> implements it in production; the tier_1 tests
/// implement a fake, which is what makes the install/teardown race provable with no
/// agent, no nest and no wall-clock wait.
/// </summary>
internal interface ISyncAgentSessionHandle
{
    /// <summary>
    /// Start the convergence loop. <b>This is the ONLY step that provisions the agent.</b>
    /// Building a session is agent-invisible — in shared Rust
    /// (<c>fauna_client_sync::agent</c>) <c>SyncAgentProvisioner::new</c> touches nothing
    /// on the wire, and <c>start()</c> is what names the machine's row and spawns the
    /// convergence loop that pushes <c>ProvisionCapability</c>.
    ///
    /// <para>The host's whole correctness argument rests on that split: a superseded
    /// install is discarded <i>before</i> this is called, so it never provisions at all,
    /// rather than provisioning and having to be torn down afterwards.</para>
    /// </summary>
    Task StartAsync();

    /// <summary>Drop the app's handle WITHOUT unprovisioning — the agent keeps serving.
    /// The newer-login / nest-re-point teardown.</summary>
    void Stop();

    /// <summary>Stop the loop and tear the agent's provisioned capability down — the
    /// sign-out / account-switch / factory-reset teardown.</summary>
    Task UnprovisionAsync();
}

/// <summary>
/// Owns the app's one live sync-agent session across login, account switch, sign-out and
/// nest re-point — the C# peer of linux's <c>sync_agent.rs</c> module state (its
/// <c>AGENT</c> cell plus <c>install</c>/<c>teardown</c>) and of macOS's
/// <c>FileProviderCoordinator</c> lifecycle (priority #3: one concept, one shape).
///
/// <para><b>The invariant it exists to hold: a session that has PROVISIONED is always
/// reachable by teardown.</b> Everything below follows from that one sentence.</para>
///
/// <para><b>The bug it closes (found 2026-08-05, fixed 2026-08-06).</b> The
/// lifecycle used to live inline in <c>App.xaml.cs</c>, which published the session only
/// after the whole ~10 s asynchronous build had returned — two nest round-trips, each
/// ~5 s against a connection still coming up at login. That left two windows, and a
/// sign-out landing in either one left the agent serving a signed-out identity, which
/// <c>on-demand-files.md</c> § Multi-account × File Provider (consequence 1) forbids:
/// <list type="number">
/// <item>the app's session field was <c>null</c> for the entire build, so the teardown
/// path found nothing to unprovision and returned a completed task — a silent no-op —
/// while the build went on to provision seconds later; and</item>
/// <item>the build's own supersession check ran only <i>after</i> it had provisioned, and
/// its discard arm dropped the handle without unprovisioning (<c>Stop</c>, by contract
/// "the agent keeps serving").</item>
/// </list>
/// Ordering it correctly is the entire fix: <see cref="InstallAsync"/> publishes the
/// session BEFORE starting it, under the same gate the teardowns take. A teardown that
/// already landed is seen at the publish point and the session is discarded having never
/// provisioned; a teardown that lands later finds a published session and unprovisions it
/// normally. There is no interleaving in between, because publish-and-check is one
/// critical section.</para>
///
/// <para><b>Why the two teardown causes are not conflated.</b> A session can be superseded
/// by a <i>teardown</i> (sign-out, switch-away, factory reset) or by a <i>newer login</i>,
/// and they want opposite endings: the first must leave the agent unprovisioned, the
/// second must leave the incoming login's freshly-installed capability alone — unprovision
/// there and you clobber it, which is the same class of bug from the far side. They are
/// already distinguished by which method the caller invokes (<see cref="UnprovisionAsync"/>
/// vs <see cref="Stop"/>), so no supersession "reason" has to be threaded through the
/// generation; the handle simply records which one claimed it (see
/// <c>SyncAgentSession.StartAsync</c>'s post-start re-check).</para>
///
/// <para>Note what is deliberately absent: any wall-clock wait, anywhere. Every face here
/// is an ORDERING — "published before started", "discarded when superseded" — so a slow
/// nest widens no window and the tier_1 tests give the same verdict on a loaded machine
/// (e2e-conventions.md § point 14).</para>
/// </summary>
/// <typeparam name="T">The concrete session type, so callers keep reaching the members the
/// handle interface deliberately does not carry (<c>Poke</c>, <c>Channel</c>,
/// <c>LocationControl</c>) without a cast.</typeparam>
internal sealed class SyncAgentSessionHost<T> where T : class, ISyncAgentSessionHandle
{
    private readonly object _gate = new();
    private T? _session;

    /// <summary>Bumped on every install and every teardown, so a session whose
    /// asynchronous build outlived its own login installs nothing.</summary>
    private int _generation;

    /// <summary>How many installs are between "began building" and "finished starting".</summary>
    private int _installsInFlight;

    /// <summary>
    /// True while any install is still in flight — i.e. while a session could still go on
    /// to provision the agent.
    ///
    /// <para><b>This exists to be a CAUSAL BARRIER for e2e negative asserts</b>
    /// (e2e-conventions.md § point 14). "Nothing re-provisions after sign-out" is otherwise
    /// unprovable without betting on a settle window, which is exactly how row 57 survived:
    /// its e2e slept 5 s and re-read the status, so it failed only when the race happened to
    /// land inside those seconds — one fail then one pass on the same tree. Once this reads
    /// false and the capability reads cleared, no in-flight install remains that could
    /// re-provision, so the assertion is about STATE rather than elapsed time and gives the
    /// same verdict on a loaded machine.</para>
    /// </summary>
    public bool InstallInFlight
    {
        get { lock (_gate) { return _installsInFlight > 0; } }
    }

    /// <summary>
    /// The live session, or <c>null</c> when there is none — which every consumer already
    /// reads as "no local agent". Non-null from the moment the session is published, i.e.
    /// BEFORE it has provisioned; that is deliberate (it is what makes the session
    /// reachable by teardown) and safe, because every member reachable through it is
    /// either best-effort or idempotent against an agent that is not yet serving.
    /// </summary>
    public T? Current
    {
        get { lock (_gate) { return _session; } }
    }

    /// <summary>
    /// Build and install the session for a newly authenticated login, superseding whatever
    /// was here. <paramref name="build"/> must be agent-invisible (see
    /// <see cref="ISyncAgentSessionHandle.StartAsync"/>): it may be slow, and it may be
    /// abandoned, so it must not provision.
    /// </summary>
    /// <param name="build">Constructs the session; returns <c>null</c> when there is no
    /// agent to talk to, which is a normal no-op and must never disrupt login.</param>
    /// <param name="onInstalled">Ran once the session is published and before it starts —
    /// the folder-binding controller adopts its channel here. Runs inside no lock.</param>
    public async Task InstallAsync(Func<Task<T?>> build, Action<T>? onInstalled = null)
    {
        int generation;
        lock (_gate)
        {
            generation = ++_generation;
            _installsInFlight++;
            var superseded = _session;
            _session = null;
            // A newer login supersedes the previous session's handle but must NOT
            // unprovision: this very install is about to provision under the incoming
            // identity, and the agent stays serving across the seam.
            superseded?.Stop();
        }

        try
        {
            var session = await build().ConfigureAwait(false);
            if (session is null) return;

            lock (_gate)
            {
                if (_generation != generation)
                {
                    // Superseded while we were building. Nothing has provisioned yet — that
                    // is the point of doing this before StartAsync — so dropping the handle
                    // is both sufficient and correct for BOTH causes: a sign-out has already
                    // unprovisioned whatever was live, and a newer login owns what comes next.
                    session.Stop();
                    return;
                }

                _session = session;
            }

            onInstalled?.Invoke(session);
            await session.StartAsync().ConfigureAwait(false);
        }
        finally
        {
            lock (_gate) { _installsInFlight--; }
        }
    }

    /// <summary>
    /// Drop the app's handle without unprovisioning — the agent keeps serving. This is the
    /// teardown for paths that are only disposing the clients the session reads (a newer
    /// login, an e2e re-login, a nest re-point), never for sign-out.
    /// </summary>
    public void Stop()
    {
        T? session;
        lock (_gate)
        {
            _generation++;
            session = _session;
            _session = null;
        }

        session?.Stop();
    }

    /// <summary>
    /// Stop the loop and tell the agent to drop its capability — the sign-out /
    /// account-switch / factory-reset teardown (on-demand-files.md § Multi-account ×
    /// File Provider, consequence 1). Best-effort: an unreachable or never-started agent
    /// is a normal no-op.
    ///
    /// <para>The generation bump is what makes this safe against an install that is still
    /// building: that install will find itself superseded at its publish point and will
    /// never provision, so "nothing to unprovision right now" is the truth here rather
    /// than a dropped obligation.</para>
    /// </summary>
    public Task UnprovisionAsync()
    {
        T? session;
        lock (_gate)
        {
            _generation++;
            session = _session;
            _session = null;
        }

        // The one line that separates "nothing to tear down" from "the teardown
        // silently did nothing". Both read identically from the AGENT's side — an
        // absent `capability un-provisioned` line — and the e2e proof of
        // consequence 1 can only report which of the two happened if the app says
        // so here. Cheap (one line per teardown, and teardowns are rare) and it
        // pairs with `StartHydrationSession`'s ENTER/BUILT/INSTALLED bracket: a
        // `no live session` here under an ENTER with no INSTALLED is an install
        // still in flight; under neither, a `Stop()` got there first.
        Logs.ShellLog.Info(
            "App",
            session is null
                ? "[sync-agent] unprovision: no live session to tear down"
                : "[sync-agent] unprovision: tearing down the live session");

        return session is null ? Task.CompletedTask : session.UnprovisionAsync();
    }
}
