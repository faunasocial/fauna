using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The process-lifetime Guardian Notify holder (family-safety.md § Guardian
/// Notify) — the static driver around <see cref="GuardianNotifyAccumulator"/>
/// that stamps wall-clock time/offset and converts to the FFI wire shape.
/// Sibling of <c>ContentPolicyCacheTests</c>; serialized the same way since
/// xUnit parallelizes by class and this is process-global static state.
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class GuardianNotifyCacheTests : IDisposable
{
    public GuardianNotifyCacheTests() => GuardianNotifyCache.Reset();
    public void Dispose() => GuardianNotifyCache.Reset();

    [Fact]
    public void TryTakeDue_NothingRecorded_ReturnsNull()
    {
        Assert.Null(GuardianNotifyCache.TryTakeDue());
    }

    [Fact]
    public void TryTakeDue_WhileDisabled_ReturnsNull()
    {
        // SetEnabled never called — off by default.
        GuardianNotifyCache.Record("post-1", ["spam"]);

        Assert.Null(GuardianNotifyCache.TryTakeDue());
    }

    [Fact]
    public void Record_ThenTryTakeDue_ReportsTheCategoryAndCount()
    {
        GuardianNotifyCache.SetEnabled(true);
        GuardianNotifyCache.Record("post-1", ["spam"]);

        var due = GuardianNotifyCache.TryTakeDue();

        Assert.NotNull(due);
        Assert.Single(due!.Value.Entries);
        Assert.Equal("spam", due.Value.Entries[0].@category);
        Assert.Equal(1u, due.Value.Entries[0].@count);
    }

    [Fact]
    public void TryTakeDue_DrainsPending_SoASecondImmediateCallReturnsNull()
    {
        GuardianNotifyCache.SetEnabled(true);
        GuardianNotifyCache.Record("post-1", ["spam"]);

        GuardianNotifyCache.TryTakeDue();

        Assert.Null(GuardianNotifyCache.TryTakeDue());
    }

    [Fact]
    public void Record_EmptyCategories_IsANoOp()
    {
        GuardianNotifyCache.SetEnabled(true);
        GuardianNotifyCache.Record("post-1", []);

        Assert.Null(GuardianNotifyCache.TryTakeDue());
    }

    [Fact]
    public void Reset_DropsPendingCountsAndTurnsCountingOff()
    {
        GuardianNotifyCache.SetEnabled(true);
        GuardianNotifyCache.Record("post-1", ["spam"]);

        GuardianNotifyCache.Reset();

        Assert.Null(GuardianNotifyCache.TryTakeDue());
        GuardianNotifyCache.Record("post-2", ["spam"]);
        Assert.Null(GuardianNotifyCache.TryTakeDue());
    }
}
