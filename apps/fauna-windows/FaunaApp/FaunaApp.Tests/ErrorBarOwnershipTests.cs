using FaunaApp.Core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// A machine tick that finds the error chain empty must close only the text the
/// chain put up — never a gesture's direct paint (folders.md § Errors &amp; edge
/// cases: a row read retires its own error, "never a gesture's").
/// </summary>
public class ErrorBarOwnershipTests
{
    [Fact]
    public void AChainTextRetiresWhenTheChainEmpties()
    {
        var bar = new ErrorBarOwnership();
        Assert.Equal(BarAction.Show, bar.Next("lost", barOpen: false, barMessage: null));

        Assert.Equal(BarAction.Close, bar.Next(null, barOpen: true, barMessage: "lost"));
    }

    [Fact]
    public void AGesturesDirectPaintSurvivesAnEmptyChainTick()
    {
        var bar = new ErrorBarOwnership();

        // Nothing from the chain was ever shown; the gesture painted the bar itself.
        Assert.Equal(BarAction.Leave, bar.Next(null, barOpen: true, barMessage: "could not save"));
    }

    [Fact]
    public void AGestureThatOverwroteTheChainTextSurvivesTheChainRetiring()
    {
        var bar = new ErrorBarOwnership();
        bar.Next("lost", barOpen: false, barMessage: null);

        // The gesture replaced the chain's text; then the row read answers.
        Assert.Equal(BarAction.Leave, bar.Next(null, barOpen: true, barMessage: "could not save"));
        // And the next empty tick still leaves it be.
        Assert.Equal(BarAction.Leave, bar.Next(null, barOpen: true, barMessage: "could not save"));
    }

    [Fact]
    public void ABarTheUserAlreadyClosedIsNotReClosed()
    {
        var bar = new ErrorBarOwnership();
        bar.Next("lost", barOpen: false, barMessage: null);

        Assert.Equal(BarAction.Leave, bar.Next(null, barOpen: false, barMessage: "lost"));
    }
}
