using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The pure Guardian Notify counter state machine (family-safety.md § Guardian
/// Notify) — the windows twin of linux's <c>content_policy.rs::NotifyAccumulator</c>
/// and web's <c>familyNotifyBuffer.ts::NotifyBufferState</c>. These pin the SAME
/// rules those two tests pin (dedup, day-boundary reset, hourly batching, the
/// actor-change drop), so all three apps cannot drift on what Guardian Notify
/// reports.
/// </summary>
public class GuardianNotifyAccumulatorTests
{
    private const uint HourSecs = 3600;

    private static string[] Cats(params string[] categories) => categories;

    // ── The off-by-default gate ─────────────────────────────────────────────

    [Fact]
    public void Record_WhileDisabled_CountsNothing()
    {
        var acc = new GuardianNotifyAccumulator();
        // SetEnabled never called — off by default, matching linux's `on: false`.
        acc.Record("post-1", Cats("spam"), 1000, 0);

        Assert.Null(acc.TryTakeDue(1_000_000, HourSecs));
    }

    [Fact]
    public void SetEnabled_False_DropsPendingCounts()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);
        acc.Record("post-1", Cats("spam"), 1000, 0);

        acc.SetEnabled(false);

        Assert.Null(acc.TryTakeDue(1_000_000, HourSecs));
    }

    // ── Dedup + hourly batching (mirrors the linux/web reference test) ──────

    [Fact]
    public void Record_DedupsPerItemAndBatchesHourly()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);

        // The same item recorded twice counts once (re-render); a second item is +1.
        acc.Record("post-1", Cats("spam"), 1000, 120);
        acc.Record("post-1", Cats("spam"), 1005, 120);
        acc.Record("post-2", Cats("spam"), 1010, 120);

        // The first report is eager (no prior flush) and carries the delta + offset.
        var first = acc.TryTakeDue(1010, HourSecs);
        Assert.NotNull(first);
        Assert.Equal(120, first!.Value.OffsetMinutes);
        var firstEntries = first.Value.Entries;
        Assert.Single(firstEntries);
        Assert.Equal("spam", firstEntries[0].Category);
        Assert.Equal(2u, firstEntries[0].Count);

        // Drained; nothing to send again immediately.
        Assert.Null(acc.TryTakeDue(1011, HourSecs));

        // A new event within the hour accumulates but does NOT flush.
        acc.Record("post-3", Cats("spam"), 1100, 120);
        Assert.Null(acc.TryTakeDue(1100, HourSecs));

        // Once a full interval has passed, the accumulated delta flushes.
        var second = acc.TryTakeDue(1010 + HourSecs, HourSecs);
        Assert.NotNull(second);
        Assert.Equal(1u, second!.Value.Entries[0].Count);
    }

    [Fact]
    public void Record_SameLocalDay_DoesNotRecountTheSameItemCategory()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);

        acc.Record("post-1", Cats("spam"), 1000, 0);
        acc.Record("post-1", Cats("spam"), 1005, 0);

        var due = acc.TryTakeDue(1_000_000, HourSecs);
        Assert.Equal(1u, due!.Value.Entries[0].Count);
    }

    [Fact]
    public void Record_NextLocalDay_ResetsTheDedupSetAndCountsAgain()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);

        acc.Record("post-1", Cats("spam"), 1000, 0);
        // The same item on the next local day is a fresh enforcement.
        acc.Record("post-1", Cats("spam"), 1000 + 86_400, 0);

        var due = acc.TryTakeDue(1_000_000, HourSecs);
        Assert.Equal(2u, due!.Value.Entries[0].Count);
    }

    [Fact]
    public void Record_EmptyCategories_IsANoOp()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);

        acc.Record("post-1", Cats(), 1000, 0);

        Assert.Null(acc.TryTakeDue(1_000_000, HourSecs));
    }

    // ── The identity-teardown reset (security-review finding class this exists for) ──

    [Fact]
    public void Reset_DropsTheOutgoingWardsPendingCounts()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);
        acc.Record("post-1", Cats("spam"), 1000, 120);

        acc.Reset();

        // A Notify report carries no identity of its own, so counts that
        // survive are attributed to whoever is signed in when they drain.
        Assert.Null(acc.TryTakeDue(1_000_000, HourSecs));
    }

    [Fact]
    public void Reset_DropsTheDedupSet_SoTheIncomingWardIsNotUnderReported()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);
        acc.Record("post-1", Cats("spam"), 1000, 120);

        acc.Reset();
        acc.SetEnabled(true);

        // Without this the incoming ward's first enforcement on an item the
        // OUTGOING ward already saw goes silently uncounted.
        var due = acc.TryTakeDue(1000, HourSecs);
        Assert.Null(due); // nothing recorded yet after reset
        acc.Record("post-1", Cats("spam"), 1000, 120);
        due = acc.TryTakeDue(1000, HourSecs);
        Assert.Equal(1u, due!.Value.Entries[0].Count);
    }

    [Fact]
    public void Reset_ClearsTheFlushClock_SoTheIncomingWardReportsEagerly()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);
        acc.Record("post-1", Cats("spam"), 1000, 120);
        acc.TryTakeDue(1000, HourSecs); // the outgoing ward's flush sets the clock

        acc.Reset();
        acc.SetEnabled(true);

        // Carrying lastFlush across the switch would silence the incoming
        // ward's first report for up to a full interval.
        acc.Record("post-2", Cats("spam"), 1050, 120);
        var due = acc.TryTakeDue(1050, HourSecs);
        Assert.Equal(1u, due!.Value.Entries[0].Count);
    }

    [Fact]
    public void Reset_TurnsCountingOff_UntilSetEnabledIsCalledAgain()
    {
        var acc = new GuardianNotifyAccumulator();
        acc.SetEnabled(true);

        acc.Reset();
        acc.Record("post-1", Cats("spam"), 1000, 120);

        Assert.Null(acc.TryTakeDue(1_000_000, HourSecs));
    }
}
