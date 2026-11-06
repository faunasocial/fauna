using FaunaApp.Core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The per-(read, set) slots behind a folder row's background-read errors
/// (folders.md § Errors &amp; edge cases — "a folder row's own reads retire their
/// own error"): a read's good answer clears exactly its own text.
/// </summary>
public class RowReadErrorsTests
{
    [Fact]
    public void AGoodAnswerRetiresExactlyItsOwnSlot()
    {
        var errors = new RowReadErrors();
        errors.Set(RowRead.DeviceActivity, "docs", "activity down");
        errors.Set(RowRead.PlaceRoster, "docs", "roster down");

        errors.Clear(RowRead.DeviceActivity, "docs");

        // The sibling read's text stands.
        Assert.Equal("roster down", errors.Text);
        errors.Clear(RowRead.PlaceRoster, "docs");
        Assert.Null(errors.Text);
    }

    [Fact]
    public void ASlotIsKeyedBySetAsWellAsRead()
    {
        var errors = new RowReadErrors();
        errors.Set(RowRead.ActorMembers, "docs", "a");
        errors.Set(RowRead.ActorMembers, "photos", "b");

        errors.Clear(RowRead.ActorMembers, "docs");

        Assert.Equal("b", errors.Text);
    }

    [Fact]
    public void ARepeatedFailureReplacesItsOwnTextWithoutAddingASlot()
    {
        var errors = new RowReadErrors();
        errors.Set(RowRead.DestinationPlaces, "docs", "first");
        errors.Set(RowRead.DestinationPlaces, "docs", "second");

        Assert.Equal("second", errors.Text);
        errors.Clear(RowRead.DestinationPlaces, "docs");
        Assert.Null(errors.Text);
    }

    [Fact]
    public void ARowThatClosesDropsEverySlotOfItsSet()
    {
        var errors = new RowReadErrors();
        errors.Set(RowRead.DeviceActivity, "docs", "a");
        errors.Set(RowRead.PlaceRoster, "docs", "b");
        errors.Set(RowRead.PlaceRoster, "photos", "c");

        errors.ClearSet("docs");

        Assert.Equal("c", errors.Text);
        errors.ClearAll();
        Assert.Null(errors.Text);
    }

    [Fact]
    public void ClearingAnAbsentSlotIsANoOp()
    {
        var errors = new RowReadErrors();
        errors.Clear(RowRead.ActorMembers, "docs");
        Assert.Null(errors.Text);
    }
}
