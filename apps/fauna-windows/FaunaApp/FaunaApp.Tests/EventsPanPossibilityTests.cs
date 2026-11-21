using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Events page's pan pair offered only where it can move
/// (<c>docs/goal/ui/events.md</c> § User actions; <c>docs/goal/ui/README.md</c>
/// § Offer only what is possible, rules 2 and 6): <see cref="EventsViewModel.PanPossibility"/>
/// reads the shared <c>calendar_pan_possibility</c> over the REAL native
/// <c>fauna_ffi</c> dll — blocked in agenda with <c>events.pan_none_in_agenda</c>,
/// possible in month, week and day — and <c>NavigateCommand</c> asks the same answer
/// and refuses with its reason on <see cref="ViewModelBase.ErrorMessage"/>, moving
/// nothing.
///
/// <para>Joins the <c>StringsGlobal</c> collection because every expected reason is
/// resolved through <see cref="Strings"/> at assertion time, so the cases hold
/// whichever localizer the process carries.</para>
/// </summary>
[Collection("StringsGlobal")]
public class EventsPanPossibilityTests
{
    private static readonly DateTimeOffset Anchor = new(2026, 3, 17, 0, 0, 0, TimeSpan.Zero);

    private static string AgendaReason() =>
        Strings.Resolve(new LocalizedText("events.pan_none_in_agenda", new Dictionary<string, string>()));

    // xUnit needs public test methods, which cannot take the UniFFI-internal enum,
    // so the theories name the mode by its shared wire word.
    private static FfiCalendarViewMode ModeFromWire(string wire) =>
        FaunaFfiMethods.CalendarViewModes()
            .First(m => FaunaFfiMethods.CalendarViewModeWire(m) == wire);

    private static EventsViewModel At(string modeWire) => new(new MockNestRpcClient())
    {
        CurrentMonth = Anchor,
        CurrentDay = Anchor,
        ViewMode = ModeFromWire(modeWire),
    };

    [Fact]
    public void InAgenda_ThePairIsShownDisabled_WithTheSharedReason()
    {
        var pan = At("agenda").PanPossibility();

        Assert.True(pan.Shown);
        Assert.False(pan.Enabled);
        Assert.Equal(AgendaReason(), pan.Reason);
    }

    [Theory]
    [InlineData("month")]
    [InlineData("week")]
    [InlineData("day")]
    public void InARangedView_ThePairIsLive_WithNoReason(string modeWire)
    {
        var pan = At(modeWire).PanPossibility();

        Assert.True(pan.IsPossible);
        Assert.Null(pan.Reason);
    }

    /// <summary>Rule 6: a press that reaches the action in agenda is refused with the
    /// reason the pair paints — not the silent return it used to be — and nothing
    /// moves: neither anchor, nor the label.</summary>
    [Theory]
    [InlineData(1)]
    [InlineData(-1)]
    public async Task APressInAgenda_IsRefusedWithTheReason_AndMovesNothing(int direction)
    {
        var vm = At("agenda");
        var labelBefore = vm.MonthYearDisplay;

        await vm.NavigateCommand.ExecuteAsync(direction);

        Assert.Equal(AgendaReason(), vm.ErrorMessage);
        Assert.Equal(Anchor, vm.CurrentDay);
        Assert.Equal(Anchor, vm.CurrentMonth);
        Assert.Equal(labelBefore, vm.MonthYearDisplay);
    }

    [Fact]
    public async Task SwitchingToARangedView_LetsThePanMove_AndClearsTheRefusal()
    {
        var vm = At("agenda");
        await vm.NavigateCommand.ExecuteAsync(1);
        Assert.NotNull(vm.ErrorMessage);

        vm.ViewMode = ModeFromWire("day");
        Assert.True(vm.PanPossibility().IsPossible);
        await vm.NavigateCommand.ExecuteAsync(1);

        Assert.Null(vm.ErrorMessage);
        Assert.Equal("2026-03-18", vm.CurrentDay.ToString("yyyy-MM-dd"));
    }
}
