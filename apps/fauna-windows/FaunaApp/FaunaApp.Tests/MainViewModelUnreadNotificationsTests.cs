using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Tests;

/// <summary>
/// The shell VM's APP-GLOBAL unread-notification count (notifications.md § Layout &amp;
/// flow — the <c>data.notifications.unread_count</c> state field ui.yaml declares).
/// <para>
/// This is the unit-level peer of <c>test_nest_flip_resilience[windows]</c>'s final
/// assertion. That e2e sits on the FEED, fires a nest push, and waits for the unread
/// count to move — so the count cannot live only on <c>NotificationsViewModel</c>,
/// which exists only while the notifications page is mounted. Before this, windows
/// had the snapshot setter (<c>AppDataSnapshot.SetUnreadCount</c>) and no caller at
/// all, so the declared state field answered a hard-coded 0 forever and the e2e read
/// 0→0 no matter how well the push was delivered .
/// </para>
/// <para>
/// The other half of the chain — a <c>fauna.notification</c> push raising
/// <c>Reconnected</c> — is already pinned by
/// <c>NestRpcPushDispatchTests</c>; together the two cover push → shell count.
/// </para>
/// </summary>
public class MainViewModelUnreadNotificationsTests
{
    [Fact]
    public void UnreadNotificationCount_DefaultsToZero_BeforeAnyRead()
    {
        var rpc = new MockNestRpcClient { NextUnreadCount = 4 };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());
        Assert.Equal(0, vm.UnreadNotificationCount);
    }

    [Fact]
    public async Task Refresh_ReadsTheNestCount()
    {
        var rpc = new MockNestRpcClient { NextUnreadCount = 4 };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        await vm.RefreshUnreadNotificationCountAsync();

        Assert.Equal(4, vm.UnreadNotificationCount);
    }

    [Fact]
    public async Task Load_ReadsTheCount_SoTheSurfaceIsHonestAtColdStart()
    {
        var rpc = new MockNestRpcClient { NextUnreadCount = 2 };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(2, vm.UnreadNotificationCount);
    }

    /// <summary>
    /// The arm the flip e2e actually rides: windows funnels both
    /// <c>FfiPushEvent.Notification</c> and <c>ResyncRequired</c> onto the one
    /// <c>Reconnected</c> fan-out (<c>NestRpcClient.DispatchPush</c>), so a push
    /// landing while the user is on any other page must still move this count.
    /// </summary>
    /// <remarks>No wait/poll here and none is needed: the mock's
    /// <c>NotificationsCountAsync</c> returns an already-completed task, so the
    /// fire-and-forget refresh runs inline and has committed by the time
    /// <c>RaiseReconnected</c> returns — a latency-independent assertion
    /// (testing.md convention 14), not a race we are winning.</remarks>
    [Fact]
    public void Reconnect_RefreshesTheCount()
    {
        var rpc = new MockNestRpcClient { NextUnreadCount = 7 };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());
        Assert.Equal(0, vm.UnreadNotificationCount);

        rpc.RaiseReconnected();

        Assert.Equal(7, vm.UnreadNotificationCount);
    }

    /// <summary>A failed count read is swallowed and leaves the last known value —
    /// an indicator must never surface an error banner over the visible page.</summary>
    [Fact]
    public async Task Refresh_SwallowsAFailedRead_AndKeepsTheLastValue()
    {
        var rpc = new MockNestRpcClient { NextUnreadCount = 3 };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());
        await vm.RefreshUnreadNotificationCountAsync();
        Assert.Equal(3, vm.UnreadNotificationCount);

        rpc.NextError = "nest unreachable";
        await vm.RefreshUnreadNotificationCountAsync();

        Assert.Equal(3, vm.UnreadNotificationCount);
    }
}
