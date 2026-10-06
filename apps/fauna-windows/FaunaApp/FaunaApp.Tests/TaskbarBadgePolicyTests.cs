using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Windows' home-screen widget — the taskbar badge (apps/windows.md § Home-screen
/// widget). The WinRT push is windows-glue; the payload decision
/// (<see cref="TaskbarBadgePolicy"/>) and the push de-duplication
/// (<see cref="TaskbarBadgeTracker"/>) are pure and pinned here. The number itself
/// is the shared <c>sum_unread</c> fold, unit-tested in <c>fauna_conversations</c>.
/// </summary>
public class TaskbarBadgePolicyTests
{
    // Zero clears the badge rather than painting a "0" — an empty inbox shows nothing
    // (linux's count-visible goes false at zero for the same reason).
    [Fact]
    public void Payload_Zero_Clears()
        => Assert.Null(TaskbarBadgePolicy.Payload(0));

    // A count is the OS numeric-badge XML, verbatim.
    [Theory]
    [InlineData(1u, "<badge value=\"1\"/>")]
    [InlineData(7u, "<badge value=\"7\"/>")]
    [InlineData(99u, "<badge value=\"99\"/>")]
    public void Payload_Count_IsTheNumericBadgeXml(uint total, string expected)
        => Assert.Equal(expected, TaskbarBadgePolicy.Payload(total));

    // Above 99 is passed through unclamped: the OS owns the glyph and renders "99+"
    // itself, so a clamp here would be a second, divergent rule.
    [Fact]
    public void Payload_Above99_IsPassedThroughUnclamped()
        => Assert.Equal("<badge value=\"100\"/>", TaskbarBadgePolicy.Payload(100));

    // The first observation always pushes, a zero included: the OS keeps a badge across
    // process exits, so a previous run's stale count must be reconciled to this
    // session's truth.
    [Fact]
    public void Tracker_FirstObservation_PushesEvenZero()
    {
        var t = new TaskbarBadgeTracker();
        Assert.Null(t.LastPushed);
        Assert.True(t.ShouldPush(0));
        Assert.Equal(0u, t.LastPushed);
    }

    // A repeat of the current total is not a push: the manager notifies on every
    // snapshot change (drafts, selection), and only a changed number is worth a
    // WinRT round trip.
    [Fact]
    public void Tracker_RepeatedTotal_DoesNotPush()
    {
        var t = new TaskbarBadgeTracker();
        Assert.True(t.ShouldPush(3));
        Assert.False(t.ShouldPush(3));
        Assert.False(t.ShouldPush(3));
        Assert.Equal(3u, t.LastPushed);
    }

    // Every change pushes, in both directions — a read that brings the count down is
    // as much a badge move as an inbound that brings it up.
    [Fact]
    public void Tracker_ChangedTotal_PushesEachChange()
    {
        var t = new TaskbarBadgeTracker();
        Assert.True(t.ShouldPush(1));
        Assert.True(t.ShouldPush(2));
        Assert.True(t.ShouldPush(0));
        Assert.True(t.ShouldPush(2));
        Assert.Equal(2u, t.LastPushed);
    }
}
