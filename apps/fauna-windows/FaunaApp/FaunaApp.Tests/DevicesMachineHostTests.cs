using System;
using System.Threading.Tasks;
using FaunaApp.Core;
using uniffi.fauna_devices_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The actor boundary ends every page's subscription to the devices machine.
///
/// A page listens through <see cref="DevicesMachineHost.Listen"/> and stops in
/// its own <c>OnNavigatedFrom</c> — but a root-frame navigation (a sign-out, the
/// e2e <c>reset</c>) replaces <c>MainPage</c> without ever calling
/// <c>OnNavigatedFrom</c> on the Folders/Devices page inside its content frame.
/// So every signed-in session's page stayed subscribed for the life of the
/// process, and every later devices tick re-rendered all of them on the UI
/// thread, rows rebuilt from scratch — a cost that grew with each reset until
/// UIA could no longer resolve the app's own window.
/// </summary>
public class DevicesMachineHostTests
{
    private sealed class CountingObserver : DevicesObserver
    {
        public int Ticks;
        public void OnChanged() => Ticks++;
    }

    [Fact]
    public void AnActorChangeEndsEveryPageSubscription()
    {
        DevicesMachineHost.ResetForActorChange();
        var stale = new CountingObserver();
        using var _ = DevicesMachineHost.Listen(stale);

        DevicesMachineHost.ResetForActorChange();
        DevicesMachineHost.NotifyListenersForTest();

        Assert.Equal(0, stale.Ticks);
    }

    [Fact]
    public async Task DevicesStaleResyncsTheMachineUntilTheSubscriptionEnds()
    {
        var rpc = new MockNestRpcClient();
        var resyncs = 0;
        var subscription = DevicesMachineHost.WireStaleResync(rpc, () =>
        {
            resyncs++;
            return Task.CompletedTask;
        });

        rpc.RaiseDevicesStale();
        await WaitForAsync(() => resyncs == 1);

        subscription.Dispose();
        rpc.RaiseDevicesStale();
        await Task.Delay(50);
        Assert.Equal(1, resyncs);
    }

    [Fact]
    public async Task AResyncThatFailsDoesNotStopTheNextRaise()
    {
        var rpc = new MockNestRpcClient();
        var attempts = 0;
        using var _ = DevicesMachineHost.WireStaleResync(rpc, () =>
        {
            attempts++;
            return attempts == 1 ? Task.FromException(new InvalidOperationException("nest down")) : Task.CompletedTask;
        });

        rpc.RaiseDevicesStale();
        await WaitForAsync(() => attempts == 1);
        rpc.RaiseDevicesStale();
        await WaitForAsync(() => attempts == 2);
    }

    /// <summary>The wiring runs the resync on the raising thread until its first
    /// await; this polls the observable effect (convention 14: no fixed sleeps).</summary>
    private static async Task WaitForAsync(Func<bool> done)
    {
        var deadline = DateTime.UtcNow.AddSeconds(5);
        while (!done())
        {
            Assert.True(DateTime.UtcNow < deadline, "the resync never ran");
            await Task.Delay(5);
        }
    }

    [Fact]
    public void APageThatListensAfterTheBoundaryHearsTheMachine()
    {
        DevicesMachineHost.ResetForActorChange();
        var live = new CountingObserver();
        using var subscription = DevicesMachineHost.Listen(live);

        DevicesMachineHost.NotifyListenersForTest();

        Assert.Equal(1, live.Ticks);
    }
}
