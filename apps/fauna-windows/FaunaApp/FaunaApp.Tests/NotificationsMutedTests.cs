using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The notifications page's muted-word collapse (`notifications.md` § Muted words):
/// the view-model asks the shared <c>notification_muted_for</c> over each row against
/// the process-held list the conversation collapse reads (<see cref="MutedKeywordsCache"/>),
/// and a reveal is session-local. Calls the REAL FFI export — the decision (rules 1, 2
/// and 5) is shared Rust's and is pinned there; these pin the app glue around it.
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class NotificationsMutedTests : IDisposable
{
    // `MutedKeywordLevel::Hide`'s weight (`fauna_core::scoring::MUTED_KEYWORDS_PENALTY`).
    private const long Hide = -1000;
    private const string Term = "mutedterm";

    public NotificationsMutedTests() => MutedKeywordsCache.Reset();
    public void Dispose() => MutedKeywordsCache.Reset();

    private static FfiNotifItem Row(long id, string type, string key, string arg, string value) =>
        new FfiNotifItem(id, type, "fauna", null, null, null, $"{type} row", false, 1000 + id,
            new LocalizedText(key, new Dictionary<string, string> { [arg] = value }));

    private static MockNestRpcClient SeededRpc() => new()
    {
        NextNotifications = FfiNotifListReplyFixture.Make(notifications: new[]
        {
            Row(1, "knock", "notifications.row_knock", "message", $"hello, about {Term.ToUpperInvariant()}"),
            Row(2, "like", "notifications.row_like", "sender", "clean-sender"),
            Row(3, "security.notice", "notifications.row_security_archive_exported", "ip", $"203.0.113.7 {Term}"),
        }),
        NextUnreadCount = 3,
    };

    [Fact]
    public async Task Load_CollapsesTheRowWhoseRelayedTextCarriesAHideWord_NeverASecurityNotice()
    {
        MutedKeywordsCache.SetKeywords(new[] { new MutedKeyword(Term, Hide) });
        var vm = new NotificationsViewModel(SeededRpc());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(3, vm.Notifications.Count);
        Assert.True(vm.Notifications[0].IsMuted);
        Assert.False(vm.Notifications[1].IsMuted);
        Assert.False(vm.Notifications[2].IsMuted); // rule 5
        // Rule 4: the badge is the nest's count, the collapsed row included.
        Assert.Equal(3, vm.UnreadCount);
    }

    [Fact]
    public async Task Load_EmptyList_CollapsesNothing()
    {
        var vm = new NotificationsViewModel(SeededRpc());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.All(vm.Notifications, n => Assert.False(n.IsMuted));
    }

    [Fact]
    public async Task Reveal_SwapsTheFullRowInAtTheSameIndex_ForTheSession()
    {
        MutedKeywordsCache.SetKeywords(new[] { new MutedKeyword(Term, Hide) });
        var rpc = SeededRpc();
        var vm = new NotificationsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.RevealMuted(vm.Notifications[0]);

        Assert.Equal(3, vm.Notifications.Count);
        Assert.Equal(1, vm.Notifications[0].Id);
        Assert.False(vm.Notifications[0].IsMuted);

        // Session-local, not page-local: a page re-mount (a fresh view-model) keeps
        // the reveal, while the mute itself stays in the list.
        var remounted = new NotificationsViewModel(rpc);
        await remounted.LoadCommand.ExecuteAsync(null);
        Assert.False(remounted.Notifications[0].IsMuted);
        Assert.Single(MutedKeywordsCache.Keywords);
    }
}
