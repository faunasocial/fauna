#if P2P_SHARE
using FaunaApp.Core.Services;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The peer-transfer plane's windows leg (p2p.md § Cross-user shared-set transfer): the
/// six-id readings and the <see cref="SharePlaneModel"/> repaint contract. Shares the
/// process-global <see cref="Strings"/> localizer with the other resolution tests, hence the
/// serialized collection.
/// </summary>
[Collection("StringsGlobal")]
public class SharePlaneReadingsTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    private static LocalizedText Text(string key, params (string Name, string Value)[] args) =>
        new(key, args.ToDictionary(a => a.Name, a => a.Value));

    private static void UseLocalizer() =>
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["folders/share_serve_status_serving"] = "Serving {count} shared folder(s) to members",
            ["folders/share_transfer_state_limited"] = "Limited by {source}",
            ["features/tier_admin"] = "your admin's plan",
        }));

    [Fact]
    public void ServeStatus_ResolvesItsCount()
    {
        UseLocalizer();
        var view = new FfiSharePlaneView(
            Text("folders.share_serve_status_serving", ("count", "2")), Array.Empty<FfiShareTransfer>());

        Assert.Equal("Serving 2 shared folder(s) to members", SharePlaneReadings.ServeStatusText(view));
    }

    /// The pin the row owes: "Limited by {source}" carries an i18n KEY as its argument, so a
    /// plain (non-nested) resolve paints the raw key at the user. This goes red if the readings
    /// ever call <c>Strings.Resolve</c> instead of <c>Strings.ResolveNested</c>.
    [Fact]
    public void TransferState_LimitedBy_ResolvesItsSourceKeyNested()
    {
        UseLocalizer();
        var limited = Text("folders.share_transfer_state_limited", ("source", "features.tier_admin"));
        var view = new FfiSharePlaneView(
            Text("folders.share_serve_status_serving", ("count", "1")),
            new[] { new FfiShareTransfer(Text("n"), Text("p"), limited) });

        var row = Assert.Single(SharePlaneReadings.TransferReadings(view));

        Assert.Equal("Limited by your admin's plan", row.State);
        // The plain resolve is exactly the bug the nested one avoids.
        Assert.Equal("Limited by features.tier_admin", Strings.Resolve(limited));
    }

    [Fact]
    public void Refresh_NullView_ReadsAsNoPlane_AndAThrowingReadIsAlsoNull()
    {
        FfiSharePlaneView? cell = null;
        var model = new SharePlaneModel(() => cell);
        var changes = 0;
        model.Changed += () => changes++;

        model.Refresh();
        Assert.Null(model.View);

        cell = new FfiSharePlaneView(Text("x"), Array.Empty<FfiShareTransfer>());
        model.Refresh();
        Assert.Same(cell, model.View);

        var throwing = new SharePlaneModel(() => throw new InvalidOperationException("boom"));
        throwing.Refresh();
        Assert.Null(throwing.View);
        Assert.Equal(2, changes);
    }
}
#endif
