using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The sync-agent session lifecycle (<see cref="SyncAgentSessionHost{T}"/>) — install,
/// supersession, and the two teardown causes.
///
/// <para><b>The defect these pin.</b> A sign-out landing during the ~10 s session
/// install left the agent still serving the SIGNED-OUT identity, which
/// <c>on-demand-files.md</c> § Multi-account × File Provider (consequence 1) forbids. The
/// lifecycle used to publish the session only after its whole asynchronous build returned,
/// so the teardown path found <c>null</c>, unprovisioned nothing, and returned a completed
/// task — while the build went on to provision seconds later, then discarded its own handle
/// with <c>Stop</c> ("the agent keeps serving"). Two windows, one outcome: a signed-out
/// account kept being served.</para>
///
/// <para><b>Why this is a tier_1 test and not an e2e.</b> The bug was found by
/// <c>test_sync_agent_unprovision_windows.py</c>, whose negative assert was a
/// <c>time.sleep(5.0)</c> — so it only failed when the race happened to land inside those
/// five seconds (one fail then one pass on the same tree). A wall-clock bet on a real
/// defect is the worst pairing there is: green is not evidence. Nothing about this race
/// actually needs an agent, a nest or a clock — <c>IFfiSyncAgentProvisioner</c> exposes
/// <c>Start</c>/<c>Unprovision</c>, so the interleavings can simply be <i>ordered</i> by a
/// fake and asserted exactly. Every face below is an ordering ("did it provision at all",
/// "was it unprovisioned"), never a duration, so a loaded machine gives the same verdict
/// (e2e-conventions.md § point 14).</para>
///
/// <para>Each test names the interleaving it fixes in the clock-free vocabulary the real
/// bug had: "during the build" = between <c>InstallAsync</c> being called and the build
/// completing, which the fake controls with a gate rather than a sleep.</para>
/// </summary>
public class SyncAgentSessionHostTests
{
    /// <summary>THE regression face. Sign out while the session is still building: the
    /// build must be abandoned WITHOUT ever provisioning. Before the fix it provisioned
    /// after the sign-out had already completed, and the agent kept serving a signed-out
    /// account.</summary>
    [Fact]
    public async Task SignOutDuringTheBuild_NeverProvisions()
    {
        var host = new SyncAgentSessionHost<FakeSession>();
        var session = new FakeSession();
        var build = new TaskCompletionSource<FakeSession?>();

        // Login: the build starts and does NOT complete yet — the ~10 s window, expressed
        // as an ordering rather than a wait.
        var install = host.InstallAsync(() => build.Task);

        // The user signs out mid-build. Nothing is published yet, so there is genuinely
        // nothing to unprovision — which is only correct if the build is then abandoned.
        await host.UnprovisionAsync();

        // The build finally returns, exactly as it did at 22:00:25 in the bug's log.
        build.SetResult(session);
        await install;

        Assert.False(session.Started,
            "a session superseded by sign-out must never start its convergence loop — " +
            "starting IS provisioning, and the sign-out already completed");
        Assert.Null(host.Current);
        Assert.True(session.Stopped, "its handle should still be dropped");
        Assert.False(session.Unprovisioned,
            "nothing provisioned, so there is nothing to unprovision — the point of " +
            "discarding before the start rather than after it");
    }

    /// <summary>The published-then-signed-out path: once a session IS live, sign-out must
    /// reach it and unprovision. This is the half that worked, and it must keep
    /// working.</summary>
    [Fact]
    public async Task SignOutAfterTheSessionIsLive_Unprovisions()
    {
        var host = new SyncAgentSessionHost<FakeSession>();
        var session = new FakeSession();

        await host.InstallAsync(() => Task.FromResult<FakeSession?>(session));
        Assert.True(session.Started);
        Assert.Same(session, host.Current);

        await host.UnprovisionAsync();

        Assert.True(session.Unprovisioned);
        Assert.Null(host.Current);
    }

    /// <summary>The session is reachable by teardown BEFORE it has provisioned — the
    /// invariant the whole host exists for, stated directly. If this is false, some
    /// teardown can land in a window where it silently no-ops.</summary>
    [Fact]
    public async Task TheSessionIsPublishedBeforeItStarts()
    {
        var host = new SyncAgentSessionHost<FakeSession>();
        FakeSession? visibleAtStart = null;
        var session = new FakeSession();
        // The fake reports what the host had published at the instant it was started.
        session.OnStart = () => visibleAtStart = host.Current;

        await host.InstallAsync(() => Task.FromResult<FakeSession?>(session));

        Assert.Same(session, visibleAtStart);
    }

    /// <summary>A NEWER LOGIN superseding a build must not unprovision — the incoming
    /// login is about to install its own capability, and tearing one down here would
    /// clobber it. This is the opposite-cause face, and getting it wrong re-creates the
    /// account-switch bug from the far side.</summary>
    [Fact]
    public async Task NewerLoginDuringTheBuild_DiscardsWithoutUnprovisioning()
    {
        var host = new SyncAgentSessionHost<FakeSession>();
        var first = new FakeSession();
        var second = new FakeSession();
        var firstBuild = new TaskCompletionSource<FakeSession?>();

        var install = host.InstallAsync(() => firstBuild.Task);

        // A second login lands while the first is still building (an account switch).
        await host.InstallAsync(() => Task.FromResult<FakeSession?>(second));

        firstBuild.SetResult(first);
        await install;

        Assert.False(first.Started, "the superseded build must not provision");
        Assert.False(first.Unprovisioned,
            "and must NOT unprovision either — the newer login owns the capability now");
        Assert.True(first.Stopped);

        Assert.True(second.Started);
        Assert.Same(second, host.Current);
    }

    /// <summary>A newer login supersedes a LIVE session by dropping its handle, never by
    /// unprovisioning: the agent keeps serving across the seam.</summary>
    [Fact]
    public async Task NewerLoginAfterTheSessionIsLive_StopsWithoutUnprovisioning()
    {
        var host = new SyncAgentSessionHost<FakeSession>();
        var first = new FakeSession();
        var second = new FakeSession();

        await host.InstallAsync(() => Task.FromResult<FakeSession?>(first));
        await host.InstallAsync(() => Task.FromResult<FakeSession?>(second));

        Assert.True(first.Stopped);
        Assert.False(first.Unprovisioned);
        Assert.Same(second, host.Current);
    }

    /// <summary>A build that finds no agent at all (the normal "sync not installed" case)
    /// is a no-op that must never disrupt login.</summary>
    [Fact]
    public async Task ABuildThatYieldsNoSession_IsANoOp()
    {
        var host = new SyncAgentSessionHost<FakeSession>();

        await host.InstallAsync(() => Task.FromResult<FakeSession?>(null));

        Assert.Null(host.Current);
        // And a teardown against it stays a no-op rather than throwing.
        await host.UnprovisionAsync();
        host.Stop();
    }

    /// <summary>The nest-re-point / e2e-re-login teardown drops the handle and leaves the
    /// agent serving — the deliberate asymmetry with sign-out.</summary>
    [Fact]
    public async Task Stop_DropsTheHandleAndLeavesTheAgentServing()
    {
        var host = new SyncAgentSessionHost<FakeSession>();
        var session = new FakeSession();

        await host.InstallAsync(() => Task.FromResult<FakeSession?>(session));
        host.Stop();

        Assert.True(session.Stopped);
        Assert.False(session.Unprovisioned);
        Assert.Null(host.Current);
    }

    /// <summary>The install's <c>onInstalled</c> hook (production: the folder-binding
    /// controller adopting the session's channel) runs after publication and before the
    /// start, so a binding made during the build is pushed as early as possible.</summary>
    [Fact]
    public async Task OnInstalled_RunsAfterPublishAndBeforeStart()
    {
        var host = new SyncAgentSessionHost<FakeSession>();
        var order = new List<string>();
        var session = new FakeSession();
        session.OnStart = () => order.Add("start");

        await host.InstallAsync(
            () => Task.FromResult<FakeSession?>(session),
            onInstalled: _ =>
            {
                order.Add(host.Current is null ? "installed(unpublished)" : "installed(published)");
            });

        Assert.Equal(new[] { "installed(published)", "start" }, order);
    }

    /// <summary>
    /// A session whose only agent-visible act is <see cref="StartAsync"/>. It records the
    /// lifecycle as flags rather than timings — which is what lets these tests assert the
    /// race exactly instead of betting on a settle window.
    /// </summary>
    internal sealed class FakeSession : ISyncAgentSessionHandle
    {
        /// <summary>True once the convergence loop was started — i.e. once this session
        /// PROVISIONED the agent. The single most important assertion in this file.</summary>
        public bool Started { get; private set; }

        public bool Stopped { get; private set; }

        public bool Unprovisioned { get; private set; }

        /// <summary>Observation hook fired inside <see cref="StartAsync"/>.</summary>
        public Action? OnStart { get; set; }

        public Task StartAsync()
        {
            Started = true;
            OnStart?.Invoke();
            return Task.CompletedTask;
        }

        public void Stop() => Stopped = true;

        public Task UnprovisionAsync()
        {
            Unprovisioned = true;
            return Task.CompletedTask;
        }
    }
}
