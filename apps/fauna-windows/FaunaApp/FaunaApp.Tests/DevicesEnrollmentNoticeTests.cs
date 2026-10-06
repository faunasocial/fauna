using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Settings → Devices page's standing enrollment notice (ui/devices.md
/// § State &amp; data shape + § Errors &amp; edge cases): the sentence the account
/// runtime's credential slot records while the nest refuses to enroll this
/// machine (the tier device cap, behavior/devices.md § Step 4), read on every
/// hydrate through <c>INestRpcClient.AccountEnrollmentNoticeAsync</c>. The page
/// itself is WinUI and not reachable from this assembly, so its two rules live in
/// <see cref="DevicesEnrollmentNotice"/> and are pinned here: the LOAD's
/// keep-vs-clear distinction (the same one FaunaKit's
/// <c>DevicesEnrollmentNoticeTests</c> pins on apple) and the RENDER's
/// precedence. End to end: <c>tests/e2e-unified/tests/test_device_cap_refusal.py</c>.
/// </summary>
public class DevicesEnrollmentNoticeTests
{
    // Already localized by shared Rust — the state holder must never parse or
    // compose it, so any sentence stands in.
    private const string CapSentence = "Your account has reached its device limit.";

    [Fact]
    public async Task LoadAsync_HoldsTheSentenceTheSlotRecords()
    {
        var rpc = new MockNestRpcClient { NextEnrollmentNotice = CapSentence };
        var notice = new DevicesEnrollmentNotice();

        await notice.LoadAsync(rpc);

        Assert.Contains("AccountEnrollmentNotice", rpc.Calls);
        Assert.Equal(CapSentence, notice.Notice);
    }

    /// <summary>The slot no longer recording the refusal is the ONLY thing that
    /// takes the notice down (ui/devices.md § Errors &amp; edge cases): a
    /// successful read that answers "nothing stands" must clear it, not be folded
    /// into "keep the old value" — the apple lesson (SE-0230) this leg inherits.</summary>
    [Fact]
    public async Task LoadAsync_ASuccessfulNull_ClearsAStandingNotice()
    {
        var rpc = new MockNestRpcClient { NextEnrollmentNotice = CapSentence };
        var notice = new DevicesEnrollmentNotice();
        await notice.LoadAsync(rpc);

        rpc.NextEnrollmentNotice = null;
        await notice.LoadAsync(rpc);

        Assert.Null(notice.Notice);
    }

    /// <summary>A transient FFI failure is not "the refusal cleared": the notice
    /// must not flicker off, and the load must not throw into the page's hydrate.</summary>
    [Fact]
    public async Task LoadAsync_AnException_KeepsTheStandingNotice_AndDoesNotThrow()
    {
        var rpc = new MockNestRpcClient { NextEnrollmentNotice = CapSentence };
        var notice = new DevicesEnrollmentNotice();
        await notice.LoadAsync(rpc);

        rpc.NextError = "ffi hiccup";
        await notice.LoadAsync(rpc);

        Assert.Equal(CapSentence, notice.Notice);
    }

    /// <summary>A roster gesture's own error is the more specific news and wins
    /// while it stands (ui/devices.md § Errors &amp; edge cases).</summary>
    [Fact]
    public async Task MessageFor_AGestureError_WinsOverTheNotice()
    {
        var rpc = new MockNestRpcClient { NextEnrollmentNotice = CapSentence };
        var notice = new DevicesEnrollmentNotice();
        await notice.LoadAsync(rpc);

        Assert.Equal("Could not remove the device.", notice.MessageFor("Could not remove the device."));
    }

    /// <summary>With no gesture error the notice is the fallback — which is what
    /// keeps a snapshot repaint (every observer tick) from wiping it.</summary>
    [Fact]
    public async Task MessageFor_NoGestureError_FallsBackToTheNotice()
    {
        var rpc = new MockNestRpcClient { NextEnrollmentNotice = CapSentence };
        var notice = new DevicesEnrollmentNotice();
        await notice.LoadAsync(rpc);

        Assert.Equal(CapSentence, notice.MessageFor(null));
    }

    [Fact]
    public void MessageFor_NothingStanding_IsNull()
    {
        var notice = new DevicesEnrollmentNotice();

        Assert.Null(notice.MessageFor(null));
    }
}
