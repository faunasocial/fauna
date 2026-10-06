using Xunit;
using uniffi.fauna_conversations;

namespace FaunaApp.Tests;

/// <summary>
/// FFI-conformance gate for the DM OS-toast diff decision. The exhaustive
/// decision cases (seed-silently / suppress-selected / unchanged-tick /
/// empty-pre-login) live once in shared Rust —
/// <c>libs/fauna-conversations/src/notification.rs</c> — and are unit-tested
/// there; OS notifications are now split per <c>conversations.md</c> § Where
/// logic lives (the *when/for-whom* decision is shared
/// <see cref="MessageNotificationTracker"/>, the firing is the WinUI
/// <c>NotificationService</c> glue). This one test exercises the **native** FFI
/// object end-to-end (constructing it loads + checksum-validates the real
/// <c>fauna_ffi</c> dll), proving the binding is generated and the
/// <c>client-display</c>-gated export reaches the Windows app (FlaUI can't
/// observe OS toasts — they aren't UIA elements — so this is the win-side gate).
/// </summary>
public class MessageNotificationTrackerTests
{
    private static ThreadActivity T(string id, long activityMs, uint unread, string? label = null) =>
        new(id, label ?? $"label-{id}", "", activityMs, unread);

    [Fact]
    public void Shared_tracker_seeds_then_notifies_on_new_activity()
    {
        var tracker = new MessageNotificationTracker();

        // First snapshot seeds silently (pre-existing threads aren't "new").
        // A zero launch floor: every stamp is news, which is this gate's premise.
        Assert.Empty(tracker.Diff(new[] { T("a", 100, 0) }, selected: null, launchFloorMs: 0));

        // A genuinely new message after the seed — its thread's unread count
        // rises — fires for that thread.
        var result = tracker.Diff(new[] { T("a", 150, 1, label: "Alice") }, selected: null, launchFloorMs: 0);

        Assert.Single(result);
        Assert.Equal("Alice", result[0].label);
    }
}
