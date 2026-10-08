using System;
using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Settings → Account → Push notifications (<c>settings.md</c> § Push notifications): what
/// the toggle and its inline line render around the shared registration machine. The
/// machine itself (when a re-arm may subscribe, what a disable clears first) is pinned in
/// <c>fauna_client_push::registration</c>; the fake below only stores the bit the way a
/// successful or failed call leaves it, so these tests pin the VM's own half — the toggle
/// reads the STORED record, never the click, and the line's precedence.
/// </summary>
public class PushNotificationsViewModelTests
{
    /// <summary>An in-memory registration: the intent bit a real call would leave.</summary>
    private sealed class FakePushRegistration : IFfiPushRegistration
    {
        public bool OptedIn;
        public bool FailEnable;
        public bool FailDisableAfterClearing;
        public FfiPushEndpoint? Enabled;

        public Task AnnouncePresence() => Task.CompletedTask;
        public string DeviceId() => "device";
        public Task DropActorRow() => Task.CompletedTask;
        public FfiPushIntent Intent() => new(OptedIn, null);
        public Task<bool> Rearm(FfiPushEndpoint subscription) => Task.FromResult(false);

        public Task Enable(FfiPushEndpoint subscription)
        {
            if (FailEnable) return Task.FromException(new InvalidOperationException("nest refused"));
            Enabled = subscription;
            OptedIn = true;
            return Task.CompletedTask;
        }

        public Task Disable()
        {
            OptedIn = false; // the bit clears first, whatever the nest says
            return FailDisableAfterClearing
                ? Task.FromException(new InvalidOperationException("nest unreachable"))
                : Task.CompletedTask;
        }
    }

    private static PushNotificationsViewModel Vm(
        FakePushRegistration? registration, FakeAgentStatusProbe agent, Action? clearOptIn = null)
    {
        var stored = registration ?? new FakePushRegistration();
        return new PushNotificationsViewModel(
            () => Task.FromResult<IFfiPushRegistration?>(registration),
            agent,
            readIntent: () => stored.Intent(),
            clearOptIn: clearOptIn ?? (() => stored.OptedIn = false),
            localBuildVersion: () => "0.0.0-test");
    }

    private static FakeAgentStatusProbe RunningAgentWithSink(bool? sink) =>
        new() { Status = FfiAgentStatusFixture.Make(notificationSink: sink) };

    [Fact]
    public async Task A_fresh_install_reads_off_and_shows_no_line()
    {
        var vm = Vm(new FakePushRegistration(), new FakeAgentStatusProbe());
        await vm.LoadAsync();
        Assert.False(vm.OptedIn);
        Assert.Null(vm.LineText);
    }

    [Fact]
    public async Task Enabling_subscribes_a_ws_device_row_and_reads_the_stored_bit_on()
    {
        var registration = new FakePushRegistration();
        var vm = Vm(registration, RunningAgentWithSink(true));
        await vm.SetOptInAsync(true);
        Assert.True(vm.OptedIn);
        Assert.Equal("ws-device", registration.Enabled?.@transport);
        Assert.Null(vm.LineText);
    }

    [Fact]
    public async Task A_failed_enable_settles_back_off_and_says_why()
    {
        var vm = Vm(new FakePushRegistration { FailEnable = true }, RunningAgentWithSink(true));
        await vm.SetOptInAsync(true);
        Assert.False(vm.OptedIn);
        Assert.False(string.IsNullOrEmpty(vm.LineText));
    }

    [Fact]
    public async Task A_disable_the_nest_cannot_take_still_reads_off()
    {
        var registration = new FakePushRegistration { OptedIn = true, FailDisableAfterClearing = true };
        var vm = Vm(registration, RunningAgentWithSink(true));
        await vm.SetOptInAsync(false);
        Assert.False(vm.OptedIn);
        Assert.False(string.IsNullOrEmpty(vm.LineText));
    }

    [Fact]
    public async Task With_no_session_a_disable_still_clears_the_bit()
    {
        var cleared = false;
        var vm = Vm(null, new FakeAgentStatusProbe(), clearOptIn: () => cleared = true);
        await vm.SetOptInAsync(false);
        Assert.True(cleared, "off stays off even with no connection to the nest");
        Assert.False(string.IsNullOrEmpty(vm.LineText));
    }

    [Fact]
    public async Task While_opted_in_an_unreachable_agent_is_the_line()
    {
        var registration = new FakePushRegistration { OptedIn = true };
        var vm = Vm(registration, new FakeAgentStatusProbe());
        await vm.LoadAsync();
        Assert.True(vm.OptedIn);
        Assert.Equal("settings/push_notifications/agent_unreachable", vm.LineText);
    }

    [Fact]
    public async Task While_opted_in_an_agent_with_no_sink_says_so_and_a_sink_clears_the_line()
    {
        var registration = new FakePushRegistration { OptedIn = true };
        var agent = RunningAgentWithSink(false);
        var vm = Vm(registration, agent);
        await vm.LoadAsync();
        Assert.Equal("settings/push_notifications/no_sink", vm.LineText);

        agent.Status = FfiAgentStatusFixture.Make(notificationSink: true);
        await vm.RefreshLineAsync();
        Assert.Null(vm.LineText);
    }

    [Fact]
    public async Task A_toggles_failure_holds_the_line_until_a_toggle_succeeds()
    {
        var registration = new FakePushRegistration { FailEnable = true };
        var vm = Vm(registration, RunningAgentWithSink(true));
        await vm.SetOptInAsync(true);
        await vm.RefreshLineAsync();
        Assert.False(string.IsNullOrEmpty(vm.LineText));

        registration.FailEnable = false;
        await vm.SetOptInAsync(true);
        Assert.True(vm.OptedIn);
        Assert.Null(vm.LineText);
    }
}
