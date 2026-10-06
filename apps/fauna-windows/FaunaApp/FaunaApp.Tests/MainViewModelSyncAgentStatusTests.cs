using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The shell VM's global <c>sync-agent-status</c> indicator (`sync-agent.md`
/// § Local agent health) — the windows unit-level peer of linux's
/// <c>update_sync_agent_status_indicator</c>. Drives
/// <see cref="MainViewModel.PollSyncAgentStatusAsync"/> over a
/// <see cref="FakeAgentStatusProbe"/> (no live agent), asserting the state → label
/// mapping and the version/uptime text. Asserts against <see cref="Strings.Get"/>
/// (returns the key itself with no localizer registered), matching
/// <c>MainViewModelConnectionStatusTests</c>.
///
/// <para><b>What is pinned here changed with the cutover.</b> The tri-state DERIVATION —
/// "is the agent's reported version this build's" — is now shared Rust
/// (<c>fauna_client_sync::agent::agent_health_state</c>, unit-pinned there and shared with
/// linux and macOS). These tests therefore pin only what the VM still owns: the mapping
/// from state to localized label, and clearing the fields when nothing is running.
/// Re-deriving the version comparison here would pin the C# twin the cutover deleted.</para>
/// </summary>
public class MainViewModelSyncAgentStatusTests
{
    [Fact]
    public void SyncAgentStatus_DefaultsToNotRunning_BeforeAnyPoll()
    {
        var vm = new MainViewModel(new MockNestRpcClient(), new FakeAgentStatusProbe());
        Assert.Equal(Strings.Get("status/sync_agent/not_running"), vm.SyncAgentStatusText);
        Assert.Equal(string.Empty, vm.SyncAgentStatusVersionText);
        Assert.Equal(string.Empty, vm.SyncAgentStatusUptimeText);
    }

    [Fact]
    public async Task PollSyncAgentStatus_Running_ReadsRunningWithVersionAndUptime()
    {
        var probe = new FakeAgentStatusProbe
        {
            Status = FfiAgentStatusFixture.Make(
                version: FaunaFfiMethods.FaunaFfiBuildVersion(), uptimeSecs: 125),
        };
        var vm = new MainViewModel(new MockNestRpcClient(), probe);

        await vm.PollSyncAgentStatusAsync();

        Assert.Equal(Strings.Get("status/sync_agent/running"), vm.SyncAgentStatusText);
        Assert.Equal(FaunaFfiMethods.FaunaFfiBuildVersion(), vm.SyncAgentStatusVersionText);
        Assert.NotEmpty(vm.SyncAgentStatusUptimeText);
    }

    [Fact]
    public async Task PollSyncAgentStatus_RestartPending_ReadsRestartPendingAndShowsTheStaleVersion()
    {
        var probe = new FakeAgentStatusProbe
        {
            // The running agent is an older build than this client links — the shared
            // derivation's RestartPending arm. The version text must show the AGENT's
            // build, since that is the thing the user is being told to restart.
            Status = FfiAgentStatusFixture.Make(
                state: FfiAgentHealthState.RestartPending,
                version: "0.0.0-stale-before-restart", uptimeSecs: 9999),
        };
        var vm = new MainViewModel(new MockNestRpcClient(), probe);

        await vm.PollSyncAgentStatusAsync();

        Assert.Equal(Strings.Get("status/sync_agent/restart_pending"), vm.SyncAgentStatusText);
        Assert.Equal("0.0.0-stale-before-restart", vm.SyncAgentStatusVersionText);
    }

    [Fact]
    public async Task PollSyncAgentStatus_KeysPending_ReadsKeysPendingAndShowsTheVersion()
    {
        var probe = new FakeAgentStatusProbe
        {
            // The agent's own status reply says a bound set's content keys are still
            // unresolved (sync-agent.md § Local agent health) — it must not fall
            // through to the Restart pending words.
            Status = FfiAgentStatusFixture.Make(
                state: FfiAgentHealthState.KeysPending, version: "1.4.2", uptimeSecs: 42),
        };
        var vm = new MainViewModel(new MockNestRpcClient(), probe);

        await vm.PollSyncAgentStatusAsync();

        Assert.Equal(Strings.Get("status/sync_agent/keys_pending"), vm.SyncAgentStatusText);
        Assert.Equal("1.4.2", vm.SyncAgentStatusVersionText);
    }

    [Fact]
    public async Task PollSyncAgentStatus_NotEnrolled_ReadsNotEnrolledAndShowsTheVersion()
    {
        var probe = new FakeAgentStatusProbe
        {
            // The agent's own status reply says the nest holds no grant for this
            // machine any more (sync-agent.md § Local agent health) — it must not
            // fall through to the Restart pending words.
            Status = FfiAgentStatusFixture.Make(
                state: FfiAgentHealthState.NotEnrolled, version: "1.4.2", uptimeSecs: 42),
        };
        var vm = new MainViewModel(new MockNestRpcClient(), probe);

        await vm.PollSyncAgentStatusAsync();

        Assert.Equal(Strings.Get("status/sync_agent/not_enrolled"), vm.SyncAgentStatusText);
        Assert.Equal("1.4.2", vm.SyncAgentStatusVersionText);
    }

    [Fact]
    public async Task PollSyncAgentStatus_NotRunning_ClearsVersionAndUptime()
    {
        // Defaults are NotRunning with empty version/uptime — the shape the shared probe
        // returns for an unreachable agent, and for no session at all.
        var vm = new MainViewModel(new MockNestRpcClient(), new FakeAgentStatusProbe());

        await vm.PollSyncAgentStatusAsync();

        Assert.Equal(Strings.Get("status/sync_agent/not_running"), vm.SyncAgentStatusText);
        Assert.Equal(string.Empty, vm.SyncAgentStatusVersionText);
        Assert.Equal(string.Empty, vm.SyncAgentStatusUptimeText);
    }

    /// <summary>A stale version reaching the VM while the state says NotRunning must not
    /// leak into the indicator: "not running" has no version to show.</summary>
    [Fact]
    public async Task PollSyncAgentStatus_NotRunningWithAStaleVersion_StillClearsTheFields()
    {
        var probe = new FakeAgentStatusProbe
        {
            Status = FfiAgentStatusFixture.Make(
                state: FfiAgentHealthState.NotRunning, version: "0.0.0-leftover", uptimeSecs: 42),
        };
        var vm = new MainViewModel(new MockNestRpcClient(), probe);

        await vm.PollSyncAgentStatusAsync();

        Assert.Equal(Strings.Get("status/sync_agent/not_running"), vm.SyncAgentStatusText);
        Assert.Equal(string.Empty, vm.SyncAgentStatusVersionText);
        Assert.Equal(string.Empty, vm.SyncAgentStatusUptimeText);
    }
}
