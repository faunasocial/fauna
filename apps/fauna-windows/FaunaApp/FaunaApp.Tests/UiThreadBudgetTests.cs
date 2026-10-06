using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Pins <see cref="UiThreadBudget"/>, the detector behind the one-line
/// "the UI thread is saturated" report.
///
/// <para>Why a detector needs its own tests rather than being trusted because it is
/// simple: its whole job is to be SILENT almost always, so in normal use "it printed
/// nothing" is indistinguishable from "it cannot print". The first version of this
/// logic checked only the tick RATE, ran through a full e2e journey against a page
/// whose UI thread was demonstrably unavailable (UIA's own window resolve took 22.2 s
/// on it), and correctly printed nothing — because the ticks were few. It was the
/// missing second condition, not the page, that was healthy. These cases exist so that neither branch
/// can go quiet again without a test going red.</para>
///
/// <para>The clock is a parameter, so none of this is wall-clock-dependent
/// (e2e-conventions.md convention 14) — the windows below are exact by
/// construction.</para>
/// </summary>
public class UiThreadBudgetTests
{
    [Fact]
    public void AHealthyPage_ReportsNothing()
    {
        var budget = new UiThreadBudget();

        // Four cheap re-evaluations spread over eight seconds: windows close, but
        // nothing in them is pathological.
        Assert.Null(budget.Record(0.0, 1.0));
        Assert.Null(budget.Record(2.5, 1.0));
        Assert.Null(budget.Record(5.0, 1.0));
        Assert.Null(budget.Record(7.5, 1.0));
    }

    [Fact]
    public void AWindowDoesNotCloseBeforeItsTime()
    {
        var budget = new UiThreadBudget();
        budget.Record(0.0, 0.0);

        // 100 ticks that would trip the storm threshold five times over, but the
        // window is still open — reporting here would fire on every page arrival.
        for (var i = 1; i <= 100; i++)
            Assert.Null(budget.Record(i * 0.01, 5.0));
    }

    [Fact]
    public void ManyCheapReEvaluations_ReportAStorm()
    {
        var budget = new UiThreadBudget();
        budget.Record(0.0, 0.0);

        UiThreadBudget.Report? report = null;
        // 30 ticks inside one 2.1 s window, each trivially cheap.
        for (var i = 1; i <= 30; i++)
            report ??= budget.Record(i * 0.07, 0.5);

        Assert.NotNull(report);
        Assert.True(report!.Value.Storming);
        Assert.False(report.Value.Blocked);
        Assert.Contains("STORM", report.Value.ToString());
    }

    /// <summary>The case the rate-only detector missed: a page whose UI thread is
    /// entirely consumed by a HANDFUL of re-evaluations, because a bound source
    /// blocks. Three ticks is far below any sane storm threshold and 97% of the
    /// window is gone.</summary>
    [Fact]
    public void FewExpensiveReEvaluations_ReportBlocked()
    {
        var budget = new UiThreadBudget();
        budget.Record(0.0, 0.0);

        Assert.Null(budget.Record(1.0, 900.0));
        var report = budget.Record(3.1, 2100.0);

        Assert.NotNull(report);
        Assert.True(report!.Value.Blocked);
        Assert.False(report.Value.Storming);
        // Three: the window-opening call's own re-evaluation happened too, and is
        // counted in the first window that can actually be measured.
        Assert.Equal(3, report.Value.Ran);
        Assert.True(report.Value.BusyShare > 0.9, $"busy share was {report.Value.BusyShare}");
        Assert.Contains("BLOCKED", report.Value.ToString());
    }

    [Fact]
    public void TheWindowResets_SoOneBadWindowDoesNotReportForever()
    {
        var budget = new UiThreadBudget();
        budget.Record(0.0, 0.0);

        Assert.NotNull(budget.Record(2.1, 2000.0));   // saturated window
        Assert.Null(budget.Record(4.2, 1.0));          // the next one is quiet again
    }

    /// <summary>Guards the first-call branch: with no previous reading there is no
    /// elapsed time, and an epoch-zero window would read as infinitely busy.</summary>
    [Fact]
    public void TheFirstCallOnlyStartsTheWindow()
    {
        var budget = new UiThreadBudget();
        Assert.Null(budget.Record(1234.5, 5000.0));
    }
}
