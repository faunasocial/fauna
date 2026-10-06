using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The unified Bridges page VM's metadata-driven settings write
/// (bridges.md § Bridge settings) — windows was the
/// last of 7 apps owing a rendered <c>number</c>-typed <c>BridgeSetting</c>
/// arm (the search-policy cap <c>limit_posts_in_search</c>). Uses
/// <see cref="BridgesViewModel.SingleBridgeId"/> to sidestep the real
/// <c>IsUnifiedBridgesPageBridge</c> filter and keep the fixture deterministic.
/// </summary>
public class BridgesViewModelTests
{
    private static BridgeInfo Bridge(string id, params BridgeSetting[] settings) => new(
        Id: id, DisplayName: id, Available: true, Linked: true,
        Identity: null, Mode: null,
        LinkModes: System.Array.Empty<BridgeLinkMode>(),
        Settings: settings);

    [Fact]
    public async Task SetSettingAsync_DispatchesTheValue()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Bridge("test-bridge") },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "test-bridge" };
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetSettingAsync("test-bridge", BridgeSettingValue.Number("limit_posts_in_search", 500));

        Assert.Equal("test-bridge", rpc.LastSetSettings?.BridgeId);
        var written = Assert.Single(rpc.LastSetSettings!.Value.Settings);
        Assert.Equal("limit_posts_in_search", written.Key);
        Assert.Equal(500, written.NumberValue);
        Assert.Null(written.BoolValue);
        Assert.Null(written.TextValue);
    }

    [Fact]
    public async Task SetSettingAsync_ReReadsAndTheClampedValueWins()
    {
        // Nest-side clamping means the value shown after a write can differ
        // from what was sent — the re-read is what shows the clamped truth
        // rather than the client's own optimistic guess.
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[]
            {
                Bridge("test-bridge", new BridgeSetting(
                    "limit_posts_in_search", "Search result cap", "number", null, null, 1000)),
            },
            PersistSetSettings = true,
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "test-bridge" };
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetSettingAsync("test-bridge", BridgeSettingValue.Number("limit_posts_in_search", 999_999_999));

        var row = vm.Bridges.Single().Settings.Single(s => s.Key == "limit_posts_in_search");
        Assert.Equal(999_999_999, row.NumberValue);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task SetSettingAsync_WriteFailure_SetsErrorMessage()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { Bridge("test-bridge") },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "test-bridge" };
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Null(vm.ErrorMessage);

        rpc.NextError = "settings write failed";
        await vm.SetSettingAsync("test-bridge", BridgeSettingValue.Number("k", 1));

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }
}
