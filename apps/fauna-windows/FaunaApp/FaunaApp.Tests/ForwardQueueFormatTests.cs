using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_client_pair;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Nests-page forward-queue block's shell-side projection (docs/goal/ui/nests.md
/// § Forward queue). Pins the three render rules: the block paints only while
/// <c>queued &gt; 0</c>, the stuck half appends only when <c>stuck &gt; 0</c>, and
/// the reason is present-or-absent (never empty) and carries relay-chosen text
/// through verbatim — the XAML then paints it via a plain <c>TextBlock.Text</c>.
/// Mirrors linux's <c>forward_queue_block_*</c> tests.
/// </summary>
[Collection("StringsGlobal")]
public class ForwardQueueFormatTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["nests/forward_queue_summary"] = "{count} of your posts are waiting.",
            ["nests/forward_queue_stuck"] = "{count} of them are refused.",
            ["nests/forward_queue_last_error"] = "Last attempt failed: {error}",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public ForwardQueueFormatTests() => Strings.Initialize(new FakeLocalizer());

    [Fact]
    public void IsVisible_FalseForAbsentOrEmptyQueue()
    {
        Assert.False(ForwardQueueFormat.IsVisible(null));
        Assert.False(ForwardQueueFormat.IsVisible(new ForwardQueueStatus(0, 0, null)));
        // A stale last_error with nothing queued still paints no block.
        Assert.False(ForwardQueueFormat.IsVisible(new ForwardQueueStatus(0, 0, "boom")));
    }

    [Fact]
    public void IsVisible_TrueWhileQueued()
    {
        Assert.True(ForwardQueueFormat.IsVisible(new ForwardQueueStatus(1, 0, null)));
    }

    [Fact]
    public void Summary_OmitsStuckHalfWhenNothingStuck()
    {
        Assert.Equal("3 of your posts are waiting.",
            ForwardQueueFormat.Summary(new ForwardQueueStatus(3, 0, null)));
    }

    [Fact]
    public void Summary_AppendsStuckHalfWhenStuck()
    {
        Assert.Equal("3 of your posts are waiting. 2 of them are refused.",
            ForwardQueueFormat.Summary(new ForwardQueueStatus(3, 2, null)));
    }

    [Fact]
    public void Reason_AbsentUntilASendFailed()
    {
        Assert.Null(ForwardQueueFormat.Reason(new ForwardQueueStatus(1, 0, null)));
        Assert.Null(ForwardQueueFormat.Reason(new ForwardQueueStatus(1, 0, "")));
    }

    [Fact]
    public void Reason_CarriesRelayChosenTextVerbatim()
    {
        // The shared projection already strips control characters; whatever
        // remains (here `<b>` and the post-strip residue of an ANSI sequence)
        // must reach the TextBlock untouched — no escaping, no interpretation.
        const string hostile = "[31m<b>x</b>forbidden";
        Assert.Equal($"Last attempt failed: {hostile}",
            ForwardQueueFormat.Reason(new ForwardQueueStatus(1, 0, hostile)));
    }
}
